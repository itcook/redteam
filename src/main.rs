use redteam::cli::{parse_format, resolve_models, ReviewArgs};
use redteam::config::Config;
use redteam::model::Prober as _;
use redteam::domain::Mode;
use redteam::evidence::{detect_mode, EvidenceOpts, Scope};
use redteam::output::render;
use redteam::provider::backends::backend_from_config;
use redteam::provider::registry::ProviderRegistry;
use redteam::provider::tool_loop::ToolLoopProvider;
use clap::{Parser, Subcommand};
use std::path::Path;

#[derive(Parser)]
#[command(name = "redteam")]
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
        #[arg(long)]
        background: bool,
        #[arg(long, default_value = "json")]
        format: String,
        /// R2.3：补跑指定作业的差集 cell（不重收集证据），前台同步执行；忽略其余 review flag
        /// （原始 mode/targets/models 等从该作业落盘的 args.json 里重建）。
        #[arg(long)]
        resume: Option<String>,
        /// R8.2：既往驳回清单——目录（内含 `<id>.md`）或 16-hex id 逗号列表；命中的 finding
        /// 豁免 verdict blocking，报告标出 previously_dismissed + run.dismissed_reappeared。
        #[arg(long)]
        dismissed: Option<String>,
        targets: Vec<String>,
    },
    /// 查询作业状态：无 id 列当前 workspace 全部；有 id 打印该作业 JSON。
    /// `--workspaces` 聚合所有 workspace，`--workspaces <substr>` 按路径子串筛。
    Status {
        job_id: Option<String>,
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        workspaces: Option<String>,
        /// 输出格式：json（默认，机读契约）| table（人读）
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// 取回已完成作业的评审结果（读后标记已消费）。仅作业处于终态 `done` 时可取；未终态返回退出码 2。
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
    /// 幂等创建项目级 .redteam/ 骨架（config.toml 模板、.gitignore、dismissed/、rounds/）。
    Init,
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

/// R1.2：worker 总时限——到点仍未完成的作业自我中止为 TimedOut（保留部分报告，见 cli::run_job）。
const JOB_DEADLINE_SECS: u64 = 40 * 60;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 生成进程内唯一 job-id（pid+secs+cwd 的 fnv 前 12 位）。
fn gen_job_id(cwd: &std::path::Path) -> String {
    let seed = format!("{}-{}", std::process::id(), now_secs());
    let h = redteam::hash::fnv1a64_hex(&[&seed, &cwd.to_string_lossy()]);
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

/// 加载配置；解析失败时打 stderr 警告（而非静默回落空配置——否则配置写错一个字段
/// 就「无任何输出」无从诊断）后回落默认。
fn load_config_warned() -> Config {
    match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: 配置文件解析失败，回落默认/空配置：{e}");
            Config::default()
        }
    }
}

/// 机会式 prune：加载配置，清过期作业。
fn prune_here(ws: &std::path::Path) {
    let cfg = load_config_warned();
    redteam::state::store::prune(
        ws,
        redteam::state::store::parse_ttl(&cfg.review.state_ttl),
        now_secs(),
    );
}

/// Fix 1（final-review）：`redteam result` 在任一终态（Done|Failed|TimedOut|Cancelled）都应放行——
/// 失败/超时路径经 `write_partial_result` 落了带 `verdict: incomplete` 的 result.json（ship 协议
/// §2.4 依赖取到它），不该被 Failed/TimedOut 一律拦成 "not done"。门槛统一为「终态 + 文件存在」；
/// 非终态（Pending/Running）或文件缺失，行为不变（沿用既有报错文案）。
/// 抽成纯函数便于单测，避免每条分支都要拉起子进程。
fn resolve_result(
    ws: &std::path::Path,
    job_id: &str,
    format: &str,
    st: &redteam::state::job::JobState,
) -> Result<String, (String, i32)> {
    use redteam::state::job::JobStatus;
    let terminal = matches!(
        st.status,
        JobStatus::Done | JobStatus::Failed | JobStatus::TimedOut | JobStatus::Cancelled
    );
    if !terminal {
        return Err((format!("job {job_id} not done (status: {:?})", st.status), 2));
    }
    let content = if format == "md" {
        redteam::state::store::read_result_md(ws, job_id)
            .or_else(|| redteam::state::store::read_result(ws, job_id))
    } else {
        redteam::state::store::read_result(ws, job_id)
    };
    match content {
        Some(raw) => Ok(raw),
        None if st.status == JobStatus::Done => {
            Err((format!("result expired or missing for {job_id}"), 2))
        }
        None => Err((format!("job {job_id} not done (status: {:?})", st.status), 2)),
    }
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
            background,
            format,
            resume,
            dismissed,
            targets,
        } => {
            let cfg = load_config_warned();
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

            let fmt = parse_format(&format);

            // R2.3：--resume <job_id> 补跑差集 cell，前台同步；忽略本次调用的其余 flag——
            // mode/targets/models 等从该作业落盘的 args.json 重建（run_resume 内部处理）。
            if let Some(job_id) = resume {
                let ws = redteam::state::store::workspace_bucket(&cwd);
                prune_here(&ws);
                let orig_args_json = match redteam::state::store::read_args_raw(&ws, &job_id) {
                    Some(j) => j,
                    None => {
                        eprintln!("error: no args for job {job_id} (resume requires a job that ran via --background)");
                        std::process::exit(2);
                    }
                };
                let orig_args: ReviewArgs = match serde_json::from_str(&orig_args_json) {
                    Ok(a) => a,
                    Err(e) => { eprintln!("error: bad args for job {job_id}: {e}"); std::process::exit(2); }
                };
                let (_detected, registry) = match build_registry(&orig_args, &cfg, &cwd) {
                    Ok(x) => x,
                    Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
                };
                let (report, code) =
                    redteam::cli::run_resume(&ws, &job_id, &registry, &cfg, &cwd).await;
                println!("{}", render(&report, fmt));
                std::process::exit(code);
            }

            let args = ReviewArgs {
                mode, base: base.clone(), scope, spec, focus, lenses, models, models_all,
                no_correlate, block_threshold, conf_floor, background, format, targets,
                resume: None, dismissed,
            };

            // 后台：写 pending + args 文件 → spawn detached worker → 打印 job-id 退出。
            if background {
                let ws = redteam::state::store::workspace_bucket(&cwd);
                let job_id = gen_job_id(&cwd);
                // 机会式 prune（与前台一致，见 Task 8 也在前台跑）
                redteam::state::store::prune(
                    &ws,
                    redteam::state::store::parse_ttl(&cfg.review.state_ttl),
                    now_secs(),
                );
                let args_json = serde_json::to_string(&args).unwrap_or_default();
                if redteam::state::store::write_args_raw(&ws, &job_id, &args_json).is_err() {
                    eprintln!("error: failed to write job args");
                    std::process::exit(2);
                }
                let mut st = redteam::state::job::JobState::new(&job_id, now_secs());
                st.workspace = Some(
                    std::fs::canonicalize(&cwd)
                        .unwrap_or_else(|_| cwd.clone())
                        .to_string_lossy()
                        .into_owned(),
                );
                let _ = redteam::state::job::write_state(&ws, &st);
                match spawn_detached_worker(&job_id) {
                    Ok(pid) => {
                        st.pid = Some(pid);
                        let _ = redteam::state::job::write_state(&ws, &st);
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
            prune_here(&redteam::state::store::workspace_bucket(&cwd));
            let (_detected, registry) = match build_registry(&args, &cfg, &cwd) {
                Ok(x) => x,
                Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
            };

            // 前台直调无 job 上下文，sink=None（与 run_review/run_code_review 直调路径一致）。
            let result = redteam::cli::run_review_dispatch(&args, &registry, &cfg, &cwd, None).await;

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
            let cfg = load_config_warned();
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = redteam::state::store::workspace_bucket(&cwd);
            let args_json = match redteam::state::store::read_args_raw(&ws, &job_id) {
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
                    let mut st = redteam::state::job::read_state(&ws, &job_id)
                        .unwrap_or_else(|| redteam::state::job::JobState::new(&job_id, 0));
                    st.status = redteam::state::job::JobStatus::Failed;
                    st.error = Some(e);
                    st.exit_code = Some(2);
                    let _ = redteam::state::job::write_state(&ws, &st);
                    std::process::exit(2);
                }
            };
            let started = redteam::state::job::read_state(&ws, &job_id)
                .map(|s| s.started_at)
                .unwrap_or_else(now_secs);
            // R1.2：worker 起手设 deadline_at（若尚未设置——例如状态文件是旧格式或已被设过）。
            if let Some(mut st) = redteam::state::job::read_state(&ws, &job_id) {
                if st.deadline_at.is_none() {
                    st.deadline_at = Some(started + JOB_DEADLINE_SECS);
                    let _ = redteam::state::job::write_state(&ws, &st);
                }
            }
            let code = redteam::cli::run_job(&ws, &job_id, &args, &registry, &cfg, &cwd, started).await;
            std::process::exit(code);
        }
        Cmd::Status { job_id, workspaces, format } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = redteam::state::store::workspace_bucket(&cwd);
            prune_here(&ws); // 机会式 prune（仅当前 workspace，见决策⑥）
            // 两分支统一收集（列表 / 单 job），再按 format 分流；json 输出形状与改动前逐字节一致。
            let jobs: Option<Vec<redteam::state::job::JobState>> = match &job_id {
                None => Some(redteam::state::job::select_jobs(
                    &ws,
                    &redteam::state::store::state_root(),
                    workspaces.as_deref(),
                )),
                Some(id) => redteam::state::job::read_state(&ws, id).map(|st| vec![st]),
            };
            match jobs {
                None => { eprintln!("no such job: {}", job_id.unwrap_or_default()); 2 }
                Some(js) => {
                    if format == "table" {
                        println!("{}", render_status_table(&js, now_secs()));
                    } else if job_id.is_none() {
                        println!("{}", serde_json::to_string_pretty(&js).unwrap_or_else(|_| "[]".into()));
                    } else {
                        println!("{}", serde_json::to_string_pretty(&js[0]).unwrap_or_default());
                    }
                    0
                }
            }
        }
        Cmd::Result { job_id, format } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = redteam::state::store::workspace_bucket(&cwd);
            prune_here(&ws);
            let st = redteam::state::job::read_state(&ws, &job_id);
            match st {
                Some(s) => match resolve_result(&ws, &job_id, &format, &s) {
                    Ok(raw) => {
                        println!("{raw}");
                        let _ = redteam::state::store::mark_consumed(&ws, &job_id);
                        s.exit_code.unwrap_or(0)
                    }
                    Err((msg, code)) => { eprintln!("{msg}"); code }
                },
                None => { eprintln!("no such job: {job_id}"); 2 }
            }
        }
        Cmd::Cancel { job_id } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = redteam::state::store::workspace_bucket(&cwd);
            match redteam::state::job::read_state(&ws, &job_id) {
                Some(mut s) if matches!(s.status, redteam::state::job::JobStatus::Running | redteam::state::job::JobStatus::Pending) => {
                    if let Some(pid) = s.pid {
                        // SAFETY: kill 单系统调用；pid 来自本 workspace 自己写的状态。
                        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM); }
                    }
                    s.status = redteam::state::job::JobStatus::Cancelled;
                    let _ = redteam::state::job::write_state(&ws, &s);
                    println!("cancelled {job_id}");
                    0
                }
                Some(s) => { eprintln!("job {job_id} not cancellable (status: {:?})", s.status); 2 }
                None => { eprintln!("no such job: {job_id}"); 2 }
            }
        }
        Cmd::Model { action } => match action {
            ModelCmd::Check => {
                let cfg = load_config_warned();
                let prober = redteam::model::probe::HttpProber::new();
                let avs = redteam::model::check_all(&cfg, &prober).await;
                for a in &avs {
                    println!("{}  {}", a.provider, a.status_str());
                }
                redteam::model::check_exit_code(&avs)
            }
            ModelCmd::Setup { provider, kind, base_url, model, key, priority, key_store } => {
                let k = redteam::model::derive_kind(&provider, kind.as_deref(), base_url.as_deref());
                // 写入前守卫：kind/base_url 不合法则提示并拒绝写入，不污染配置。
                if let Err(e) = redteam::model::validate_setup(&k, base_url.as_deref()) {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
                let model = match model {
                    Some(m) => m,
                    None => { eprintln!("error: --model is required"); std::process::exit(2); }
                };
                let api_key_env = redteam::model::derive_api_key_env(&provider);
                let prio = priority.unwrap_or(0);
                if let Err(e) = redteam::config::write_provider(
                    &provider, &k, base_url.as_deref(), &model, prio, Some(&api_key_env),
                ) {
                    eprintln!("error: failed to write provider config: {e}");
                    std::process::exit(2);
                }
                // key 去处：env(默认，打印 export) / secrets(写 secrets.toml)
                match key_store.as_str() {
                    "secrets" => match key {
                        Some(k) => {
                            if let Err(e) = redteam::config::secrets::write_secret(&provider, &k) {
                                eprintln!("error: failed to write secret: {e}");
                                std::process::exit(2);
                            }
                            println!("wrote key to {} (chmod 600)", redteam::config::secrets::secrets_path().display());
                        }
                        None => { eprintln!("error: --key-store secrets requires --key"); std::process::exit(2); }
                    },
                    _ => {
                        // env 默认：主配置只记 api_key_env，提示用户设 env。
                        match key {
                            Some(k) => println!("add to your shell profile:\n  export {api_key_env}={k}"),
                            None => println!("set the API key in your environment:\n  export {api_key_env}=<your-key>"),
                        }
                    }
                }
                println!("configured provider '{provider}' (kind={k}, model={model}, api_key_env={api_key_env})");
                // 完成后自动跑一次 check
                let cfg = load_config_warned();
                let prober = redteam::model::probe::HttpProber::new();
                let avs = redteam::model::check_all(&cfg, &prober).await;
                for a in &avs {
                    println!("{}  {}", a.provider, a.status_str());
                }
                0
            }
            ModelCmd::List { remote } => {
                let cfg = load_config_warned();
                let prober = redteam::model::probe::HttpProber::new();
                match remote {
                    Some(prov) => {
                        let pc = match cfg.providers.get(&prov) {
                            Some(p) => p,
                            None => { eprintln!("no such provider: {prov}"); std::process::exit(2); }
                        };
                        let base = redteam::model::effective_base(pc);
                        let key = cfg.resolve_key(&prov);
                        match prober.list_models(&base, key.as_deref()).await {
                            Ok(models) => {
                                for m in models { println!("{m}"); }
                                0
                            }
                            Err(e) => { eprintln!("error querying {base}/models: {e}"); 2 }
                        }
                    }
                    None => {
                        let avs = redteam::model::check_all(&cfg, &prober).await;
                        let paths: Vec<String> = redteam::config::config_paths()
                            .iter().map(|p| p.display().to_string()).collect();
                        println!("{}", redteam::model::render_model_list(&cfg, &avs, &paths));
                        0
                    }
                }
            }
        },
        Cmd::Init => {
            match redteam::project::init_project(std::path::Path::new(".")) {
                Ok(c) if c.is_empty() => println!(".redteam/ already complete"),
                Ok(c) => c.iter().for_each(|p| println!("created {p}")),
                Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
            }
            0
        }
    };
    std::process::exit(code);
}

/// M5 批2：status 的人读时长（42s / 7m / 1h03m）。
fn fmt_dur(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// serde 视角的枚举字串（与 status JSON 契约逐字一致，如 timed_out）。
fn serde_str<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|x| x.as_str().map(String::from))
        .unwrap_or_default()
}

/// M5 批2：`status --format table` 人读视图（opt-in；默认 JSON 数组契约不动）。
/// 空格对齐、无框线——窄终端可 wrap、输出可 grep。now 注入以便测试。
fn render_status_table(jobs: &[redteam::state::job::JobState], now: u64) -> String {
    const HEADER: [&str; 8] = ["JOB", "STATUS", "PHASE", "MODE", "PROG", "AGE", "IDLE", "WORKSPACE"];
    let rows: Vec<[String; 8]> = jobs
        .iter()
        .map(|j| {
            [
                j.job_id.clone(),
                serde_str(&j.status),
                j.phase.as_ref().map(serde_str).unwrap_or_else(|| "-".into()),
                j.mode.as_ref().map(serde_str).unwrap_or_else(|| "-".into()),
                if j.progress.total == 0 { "-".into() } else { format!("{}/{}", j.progress.done, j.progress.total) },
                fmt_dur(now.saturating_sub(j.started_at)),
                if j.last_active == 0 { "-".into() } else { fmt_dur(now.saturating_sub(j.last_active)) },
                j.workspace.clone().unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..HEADER.len())
        .map(|c| rows.iter().map(|r| r[c].len()).chain([HEADER[c].len()]).max().unwrap_or(0))
        .collect();
    let fmt_row = |cells: &[&str]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(c, s)| format!("{:w$}", s, w = widths[c]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = vec![fmt_row(&HEADER)];
    out.extend(rows.iter().map(|r| fmt_row(&r.iter().map(String::as_str).collect::<Vec<_>>())));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M5 批2：status --format table 的人读渲染（默认 json 契约不动）。
    #[test]
    fn render_status_table_shapes_columns() {
        use redteam::state::job::{JobState, JobStatus, Phase, Progress};
        let mut a = JobState::new("abc123def456", 1000);
        a.status = JobStatus::Running;
        a.phase = Some(Phase::Fanout);
        a.mode = Some(redteam::domain::input::Mode::Code);
        a.progress = Progress { done: 3, total: 10 };
        a.last_active = 4500;
        a.workspace = Some("/repos/sites".into());
        let b = JobState::new("fff000fff000", 4000); // pending：phase/mode/workspace 全 None、last_active=0
        let out = render_status_table(&[a, b], 4600);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains("JOB") && lines[0].contains("STATUS") && lines[0].contains("PROG")
            && lines[0].contains("AGE") && lines[0].contains("IDLE"), "header: {}", lines[0]);
        assert!(lines[1].contains("abc123def456") && lines[1].contains("running")
            && lines[1].contains("fanout") && lines[1].contains("code")
            && lines[1].contains("3/10") && lines[1].contains("1h00m") // age 3600s
            && !lines[1].contains("100s") && lines[1].contains("/repos/sites"), "row a: {}", lines[1]);
        // idle 100s → "1m"（分钟粒度截断）
        assert!(lines[1].contains("1m"), "row a idle: {}", lines[1]);
        assert!(lines[2].contains("fff000fff000") && lines[2].contains("pending")
            && lines[2].contains("-"), "row b folds None to '-': {}", lines[2]);
    }

    #[test]
    fn fmt_dur_tiers() {
        assert_eq!(fmt_dur(42), "42s");
        assert_eq!(fmt_dur(60), "1m");
        assert_eq!(fmt_dur(3599), "59m");
        assert_eq!(fmt_dur(3780), "1h03m");
    }

    /// C1：无 provider 配置的 Config + doc 模式 ReviewArgs → build_registry 返回 Err。
    #[test]
    fn build_registry_no_provider_returns_err() {
        use redteam::cli::ReviewArgs;
        use redteam::config::Config;

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
            resume: None,
            dismissed: None,
        };
        let result = build_registry(&args, &cfg, cwd);
        assert!(result.is_err(), "无 provider 时 build_registry 应返回 Err");
        let msg = result.err().unwrap();
        assert!(
            msg.contains("no provider"),
            "错误消息应提示 provider 问题，实际: {msg}"
        );
    }

    /// Fix 1：Failed + result.json 存在 → 取回成功（partial report，ship 协议 §2.4 依赖此路径）。
    #[test]
    fn resolve_result_failed_with_partial_report_succeeds() {
        use redteam::state::job::{JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        redteam::state::store::write_result(ws, "j1", r#"{"verdict":"incomplete"}"#).unwrap();
        let mut st = JobState::new("j1", 0);
        st.status = JobStatus::Failed;
        st.exit_code = Some(2);
        let out = resolve_result(ws, "j1", "json", &st);
        assert_eq!(out, Ok(r#"{"verdict":"incomplete"}"#.to_string()));
    }

    /// Fix 1：Failed + 无 result.json → 仍报错（未曾 checkpoint 过任何 cell 的早期崩溃）。
    #[test]
    fn resolve_result_failed_without_result_file_errs() {
        use redteam::state::job::{JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j2", 0);
        st.status = JobStatus::Failed;
        let out = resolve_result(ws, "j2", "json", &st);
        assert!(out.is_err());
        assert_eq!(out.unwrap_err().1, 2);
    }

    /// 非终态（Running）不受影响：即便 result.json 意外存在也不放行。
    #[test]
    fn resolve_result_running_stays_not_done() {
        use redteam::state::job::{JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        redteam::state::store::write_result(ws, "j3", "{}").unwrap();
        let mut st = JobState::new("j3", 0);
        st.status = JobStatus::Running;
        let out = resolve_result(ws, "j3", "json", &st);
        let (msg, code) = out.unwrap_err();
        assert!(msg.contains("not done"), "msg: {msg}");
        assert_eq!(code, 2);
    }
}
