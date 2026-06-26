use breakpoints::cli::{check_multi_model, parse_format, run_code_review, run_review, ReviewArgs};
use breakpoints::config::Config;
use breakpoints::domain::Mode;
use breakpoints::evidence::{detect_mode, EvidenceOpts, Scope};
use breakpoints::output::render;
use breakpoints::provider::backends::backend_from_config;
use breakpoints::provider::direct::DirectProvider;
use breakpoints::provider::tool_loop::ToolLoopProvider;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "brk")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
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
            no_correlate,
            block_threshold,
            conf_floor,
            wait: _wait, // 默认前台同步路径；run_review 不持有此字段（与 --background 对称）
            background,
            format,
            targets,
        } => {
            let cfg = Config::load().unwrap_or_default();

            if let Err(e) = check_multi_model(&models) {
                eprintln!("error: {e}");
                std::process::exit(2);
            }

            // 探测 mode 以选 provider 形态。
            let scope_enum = if scope == "branch" { Scope::Branch } else { Scope::WorkingTree };
            let mode_opt = mode.as_deref().map(|m| if m == "code" { Mode::Code } else { Mode::Doc });
            let eopts = EvidenceOpts {
                mode: mode_opt,
                base: base.clone(),
                scope: scope_enum,
                spec: spec.clone(),
                targets: targets.clone(),
            };
            let detected = match detect_mode(&eopts) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            };

            // lead provider 选取（与既有逻辑一致）。
            let lead = models
                .as_deref()
                .and_then(|s| s.split(',').next())
                .map(|s| s.trim().to_string())
                .or_else(|| cfg.review.default_models.first().cloned())
                .or_else(|| {
                    let mut v: Vec<_> = cfg.providers.iter().collect();
                    v.sort_by_key(|p| std::cmp::Reverse(p.1.priority));
                    v.first().map(|(k, _)| (*k).clone())
                });
            let lead_pc = match lead.as_deref().and_then(|n| cfg.providers.get(n).map(|pc| (n, pc))) {
                Some((n, pc)) => {
                    let key = cfg.resolve_key(n).unwrap_or_default();
                    (n.to_string(), pc.clone(), key)
                }
                None => {
                    eprintln!("no provider available, please configure [providers.*] first");
                    std::process::exit(2);
                }
            };

            let fmt = parse_format(&format);
            let args = ReviewArgs {
                mode, base: base.clone(), scope, spec, focus, lenses, models,
                no_correlate, block_threshold, conf_floor, background, format, targets,
            };

            let result = match detected {
                Mode::Code => {
                    // code：工具循环 + bundle 流水线
                    let backend = match backend_from_config(&lead_pc.1, lead_pc.2) {
                        Ok(b) => b,
                        Err(e) => {
                            eprintln!("error: {e}");
                            std::process::exit(2);
                        }
                    };
                    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let provider = ToolLoopProvider::new(backend, cwd.clone());
                    let changes = match breakpoints::evidence::git::changed_files(base.as_deref(), scope_enum == Scope::Branch) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("error: {e}");
                            std::process::exit(2);
                        }
                    };
                    run_code_review(&args, &provider, &cfg, changes, &cwd).await
                }
                Mode::Doc => {
                    // doc：维持 Phase-1 单发 lens×model（留到 S4）
                    let provider = DirectProvider::from_config(&lead_pc.1, lead_pc.2);
                    run_review(args, &provider, &cfg).await
                }
            };

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
    };
    std::process::exit(code);
}
