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
    /// 隐藏：后台 worker（由 `review --background` re-exec 拉起，用户不直接调）。
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        job_id: String,
    },
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

/// 探测 mode + 构造 ProviderRegistry（前台与 worker 共用，避免重复逻辑）。
fn build_registry(args: &ReviewArgs, cfg: &Config, cwd: &Path) -> (Mode, ProviderRegistry) {
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
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let (planned, _replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    if planned.is_empty() {
        eprintln!("no provider available, please configure [providers.*] first");
        std::process::exit(2);
    }

    let mut registry = ProviderRegistry::new();
    for pm in &planned {
        let pc = match cfg.providers.get(&pm.name) {
            Some(p) => p.clone(),
            None => {
                eprintln!("error: model '{}' has no [providers.{}] config", pm.name, pm.name);
                std::process::exit(2);
            }
        };
        let key = cfg.resolve_key(&pm.name).unwrap_or_default();
        let backend = match backend_from_config(&pc, key) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        };
        registry.insert(pm.name.clone(), Box::new(ToolLoopProvider::new(backend, cwd.to_path_buf())));
    }

    (detected, registry)
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

            // 前台路径：构造 registry 后调 run_review_dispatch。
            let (_detected, registry) = build_registry(&args, &cfg, &cwd);

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
            let (_detected, registry) = build_registry(&args, &cfg, &cwd);
            let started = breakpoints::state::job::read_state(&ws, &job_id)
                .map(|s| s.started_at)
                .unwrap_or_else(now_secs);
            let code = breakpoints::cli::run_job(&ws, &job_id, &args, &registry, &cfg, &cwd, started).await;
            std::process::exit(code);
        }
    };
    std::process::exit(code);
}
