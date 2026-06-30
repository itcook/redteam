use breakpoints::cli::{parse_format, resolve_models, ReviewArgs};
use breakpoints::config::Config;
use breakpoints::domain::Mode;
use breakpoints::evidence::{detect_mode, EvidenceOpts, Scope};
use breakpoints::output::render;
use breakpoints::provider::backends::backend_from_config;
use breakpoints::provider::registry::ProviderRegistry;
use breakpoints::provider::tool_loop::ToolLoopProvider;
use clap::{Parser, Subcommand};
use std::path::Path;

#[derive(Parser)]
#[command(name = "brk")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Cmd {
    Review {
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long, default_value = "working-tree")]
        scope: String,
        #[arg(long)]
        spec: Vec<String>,
        #[arg(long)]
        focus: Option<String>,
        #[arg(long)]
        lenses: Option<String>,
        #[arg(long)]
        models: Option<String>,
        #[arg(long)]
        models_all: bool,
        #[arg(long)]
        no_correlate: bool,
        #[arg(long)]
        block_threshold: Option<String>,
        #[arg(long)]
        conf_floor: Option<f32>,
        #[arg(long, default_value_t = true)]
        wait: bool,
        #[arg(long)]
        background: bool,
        #[arg(long, default_value = "json")]
        format: String,
        targets: Vec<String>,
    },
    /// 查询作业状态：无 id 列全部，有 id 打印该作业 JSON。
    Status { job_id: Option<String> },
    /// 取回已完成作业的评审结果（读后标记已消费）。
    Result {
        job_id: String,
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// 取消 running/pending 作业（发 SIGTERM）。
    Cancel { job_id: String },
    /// 隐藏：后台 worker（由 `review --background` re-exec 拉起，用户不直接调）。
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        job_id: String,
    },
    /// 配置/查询 provider 与 model。
    Model {
        #[command(subcommand)]
        action: ModelCmd,
    },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// 配置一个 provider/model（flag 驱动）。
    Setup {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        priority: Option<i32>,
        /// key 去处：env(默认，打印 export) | secrets(写 secrets.toml chmod 600)
        #[arg(long, default_value = "env")]
        key_store: String,
    },
    /// 列已配置 provider + 可用性 + 配置位置。
    List {
        #[arg(long)]
        remote: Option<String>,
    },
    /// 轻量可用性预检（退出码全可用 0 / 有不可用非 0）。
    Check,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 生成进程内唯一 job-id（pid+secs+cwd 的 fnv 前 12 位）。
fn gen_job_id(cwd: &std::path::Path) -> String {
    let seed = format!("{}-{}", std::process::id(), now_secs());
    let h = breakpoints::hash::fnv1a64_hex(&[&seed, &cwd.to_string_lossy()]);
    h[..12].to_string()
}

/// spawn detached worker：re-exec 自身 `worker --job-id <id>`，setsid 新会话、stdio→null。
/// 返回子进程 pid。
fn spawn_detached_worker(job_id: &str) -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("worker").arg("--job-id").arg(job_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid 仅调一个 async-signal-safe 系统调用，新建会话脱离 controlling tty。
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                // setsid 失败不致命（已 fork 的子进程），继续；进程组隔离仍由 fork 提供。
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok(child.id())
}

/// 把作业列表渲染成每行一条的摘要（status 命令无 id 时用）。
fn render_job_list(jobs: &[breakpoints::state::job::JobState]) -> String {
    let mut lines: Vec<String> = jobs.iter().map(|s| {
        let mode = s.mode.map(|m| format!("{m:?}")).unwrap_or_else(|| "?".into());
        format!("{}  {:?}  {}  {}/{}", s.job_id, s.status, mode, s.progress.done, s.progress.total)
    }).collect();
    lines.sort();
    lines.join("\n")
}

/// 机会式 prune：加载配置，清过期作业。
fn prune_here(ws: &std::path::Path) {
    let cfg = Config::load().unwrap_or_default();
    breakpoints::state::store::prune(
        ws,
        breakpoints::state::store::parse_ttl(&cfg.review.state_ttl),
        now_secs(),
    );
}

/// 探测 mode + 构造 ProviderRegistry（前台与 worker 共用，避免重复逻辑）。
/// 返回 Err(消息) 代替直接 process::exit，调用方决定如何处理。
fn build_registry(args: &ReviewArgs, cfg: &Config, cwd: &Path) -> Result<(Mode, ProviderRegistry), String> {
    let scope_enum = if args.scope == "branch" { Scope::Branch } else { Scope::WorkingTree };
    let mode_opt = args.mode.as_deref().map(|m| if m == "code" { Mode::Code } else { Mode::Doc });
    let eopts = EvidenceOpts {
        mode: mode_opt,
        base: args.base.clone(),
        scope: scope_enum,
        spec: args.spec.clone(),
        targets: args.targets.clone(),
    };
    let detected = match detect_mode(&eopts) {
        Ok(m) => m,
        Err(e) => {
            return Err(format!("{e}"));
        }
    };

    let (planned, _replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    if planned.is_empty() {
        return Err("no provider available, please configure [providers.*] first".to_string());
    }

    let mut registry = ProviderRegistry::new();
    for pm in &planned {
        let pc = match cfg.providers.get(&pm.name) {
            Some(p) => p.clone(),
            None => {
                return Err(format!("model '{}' has no [providers.{}] config", pm.name, pm.name));
            }
        };
        let key = cfg.resolve_key(&pm.name).unwrap_or_default();
        let backend = match backend_from_config(&pc, key) {
            Ok(b) => b,
            Err(e) => {
                return Err(format!("{e}"));
            }
        };
        registry.insert(pm.name.clone(), Box::new(ToolLoopProvider::new(backend, cwd.to_path_buf())));
    }

    Ok((detected, registry))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Review {
            mode,
            base,
            scope,
            spec,
            focus,
            lenses,
            models,
            models_all,
            no_correlate,
            block_threshold,
            conf_floor,
            wait: _wait, // 默认前台同步路径；run_review 不持有此字段（与 --background 对称）
            background,
            format,
            targets,
        } => {
            let cfg = Config::load().unwrap_or_default();
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

            let fmt = parse_format(&format);
            let args = ReviewArgs {
                mode, base: base.clone(), scope, spec, focus, lenses, models, models_all,
                no_correlate, block_threshold, conf_floor, background, format, targets,
            };

            // 后台：写 pending + args 文件 → spawn detached worker → 打印 job-id 退出。
            if background {
                let ws = breakpoints::state::store::workspace_bucket(&cwd);
                let job_id = gen_job_id(&cwd);
                // 机会式 prune（与前台一致，见 Task 8 也在前台跑）
                breakpoints::state::store::prune(
                    &ws,
                    breakpoints::state::store::parse_ttl(&cfg.review.state_ttl),
                    now_secs(),
                );
                let args_json = serde_json::to_string(&args).unwrap_or_default();
                if breakpoints::state::store::write_args_raw(&ws, &job_id, &args_json).is_err() {
                    eprintln!("error: failed to write job args");
                    std::process::exit(2);
                }
                let mut st = breakpoints::state::job::JobState::new(&job_id, now_secs());
                let _ = breakpoints::state::job::write_state(&ws, &st);
                match spawn_detached_worker(&job_id) {
                    Ok(pid) => {
                        st.pid = Some(pid);
                        let _ = breakpoints::state::job::write_state(&ws, &st);
                        println!("{job_id}");
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("error: failed to spawn worker: {e}");
                        std::process::exit(2);
                    }
                }
            }

            // 前台路径：机会式 prune + 构造 registry 后调 run_review_dispatch。
            prune_here(&breakpoints::state::store::workspace_bucket(&cwd));
            let (_detected, registry) = match build_registry(&args, &cfg, &cwd) {
                Ok(x) => x,
                Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
            };

            let result = breakpoints::cli::run_review_dispatch(&args, &registry, &cfg, &cwd).await;

            match result {
                Ok((report, code)) => {
                    println!("{}", render(&report, fmt));
                    code
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    2
                }
            }
        }
        Cmd::Worker { job_id } => {
            let cfg = Config::load().unwrap_or_default();
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = breakpoints::state::store::workspace_bucket(&cwd);
            let args_json = match breakpoints::state::store::read_args_raw(&ws, &job_id) {
                Some(j) => j,
                None => {
                    eprintln!("error: no args for job {job_id}");
                    std::process::exit(2);
                }
            };
            let args: ReviewArgs = match serde_json::from_str(&args_json) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("error: bad args: {e}");
                    std::process::exit(2);
                }
            };
            // 建 registry（复用 build_registry：探测 mode + resolve_models + 构造 ProviderRegistry）
            let (_detected, registry) = match build_registry(&args, &cfg, &cwd) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("error: {e}");
                    // 写 Failed 终态，让 status 命令能看到错误原因。
                    let mut st = breakpoints::state::job::read_state(&ws, &job_id)
                        .unwrap_or_else(|| breakpoints::state::job::JobState::new(&job_id, 0));
                    st.status = breakpoints::state::job::JobStatus::Failed;
                    st.error = Some(e);
                    st.exit_code = Some(2);
                    let _ = breakpoints::state::job::write_state(&ws, &st);
                    std::process::exit(2);
                }
            };
            let started = breakpoints::state::job::read_state(&ws, &job_id)
                .map(|s| s.started_at)
                .unwrap_or_else(now_secs);
            let code = breakpoints::cli::run_job(&ws, &job_id, &args, &registry, &cfg, &cwd, started).await;
            std::process::exit(code);
        }
        Cmd::Status { job_id } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = breakpoints::state::store::workspace_bucket(&cwd);
            prune_here(&ws); // 机会式 prune
            match job_id {
                None => {
                    let jobs = breakpoints::state::job::list_states(&ws);
                    println!("{}", render_job_list(&jobs));
                    0
                }
                Some(id) => match breakpoints::state::job::read_state(&ws, &id) {
                    Some(st) => { println!("{}", serde_json::to_string_pretty(&st).unwrap_or_default()); 0 }
                    None => { eprintln!("no such job: {id}"); 2 }
                },
            }
        }
        Cmd::Result { job_id, format } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = breakpoints::state::store::workspace_bucket(&cwd);
            prune_here(&ws);
            let st = breakpoints::state::job::read_state(&ws, &job_id);
            match st {
                Some(s) if matches!(s.status, breakpoints::state::job::JobStatus::Done) => {
                    // 按 format 直读对应渲染文件（worker 已同落 json+md，无需反序列化）。
                    let content = if format == "md" {
                        breakpoints::state::store::read_result_md(&ws, &job_id)
                            .or_else(|| breakpoints::state::store::read_result(&ws, &job_id))
                    } else {
                        breakpoints::state::store::read_result(&ws, &job_id)
                    };
                    match content {
                        Some(raw) => {
                            println!("{raw}");
                            let _ = breakpoints::state::store::mark_consumed(&ws, &job_id);
                            s.exit_code.unwrap_or(0)
                        }
                        None => { eprintln!("result expired or missing for {job_id}"); 2 }
                    }
                }
                Some(s) => { eprintln!("job {job_id} not done (status: {:?})", s.status); 2 }
                None => { eprintln!("no such job: {job_id}"); 2 }
            }
        }
        Cmd::Cancel { job_id } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = breakpoints::state::store::workspace_bucket(&cwd);
            match breakpoints::state::job::read_state(&ws, &job_id) {
                Some(mut s) if matches!(s.status, breakpoints::state::job::JobStatus::Running | breakpoints::state::job::JobStatus::Pending) => {
                    if let Some(pid) = s.pid {
                        // SAFETY: kill 单系统调用；pid 来自本 workspace 自己写的状态。
                        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM); }
                    }
                    s.status = breakpoints::state::job::JobStatus::Cancelled;
                    let _ = breakpoints::state::job::write_state(&ws, &s);
                    println!("cancelled {job_id}");
                    0
                }
                Some(s) => { eprintln!("job {job_id} not cancellable (status: {:?})", s.status); 2 }
                None => { eprintln!("no such job: {job_id}"); 2 }
            }
        }
        Cmd::Model { action } => match action {
            ModelCmd::Check => {
                let cfg = Config::load().unwrap_or_default();
                let prober = breakpoints::model::probe::HttpProber::new();
                let avs = breakpoints::model::check_all(&cfg, &prober).await;
                for a in &avs {
                    println!("{}  {}", a.provider, a.status_str());
                }
                breakpoints::model::check_exit_code(&avs)
            }
            ModelCmd::Setup { .. } => { eprintln!("model setup: implemented in next task"); 2 }
            ModelCmd::List { .. } => { eprintln!("model list: implemented in next task"); 2 }
        },
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use breakpoints::state::job::{JobState, JobStatus};
    #[test]
    fn render_job_list_one_line_per_job_sorted() {
        let mut a = JobState::new("aaa", 0); a.status = JobStatus::Done;
        let mut b = JobState::new("bbb", 0); b.status = JobStatus::Running;
        let out = render_job_list(&[b, a]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("aaa"), "sorted by id");
        assert!(lines[0].contains("Done") && lines[1].contains("Running"));
    }

    /// C1：无 provider 配置的 Config + doc 模式 ReviewArgs → build_registry 返回 Err。
    #[test]
    fn build_registry_no_provider_returns_err() {
        use breakpoints::cli::ReviewArgs;
        use breakpoints::config::Config;

        // 空配置：无 [providers.*]，resolve_models 会产出空列表
        let cfg = Config::default();
        let cwd = std::path::Path::new(".");
        let args = ReviewArgs {
            mode: Some("doc".into()),
            base: None,
            scope: "working-tree".into(),
            spec: vec![],
            focus: None,
            lenses: None,
            models: None,
            models_all: false,
            no_correlate: false,
            block_threshold: None,
            conf_floor: None,
            background: false,
            format: "json".into(),
            targets: vec![],
        };
        let result = build_registry(&args, &cfg, cwd);
        assert!(result.is_err(), "无 provider 时 build_registry 应返回 Err");
        let msg = result.err().unwrap();
        assert!(
            msg.contains("no provider"),
            "错误消息应提示 provider 问题，实际: {msg}"
        );
    }
}
