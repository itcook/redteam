use breakpoints::cli::{parse_format, resolve_models, run_code_review, run_review, ReviewArgs};
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

            let fmt = parse_format(&format);
            let args = ReviewArgs {
                mode, base: base.clone(), scope, spec, focus, lenses, models, models_all,
                no_correlate, block_threshold, conf_floor, background, format, targets,
            };

            let (planned, _replicate) = resolve_models(args.models.as_deref(), args.models_all, &cfg);
            if planned.is_empty() {
                eprintln!("no provider available, please configure [providers.*] first");
                std::process::exit(2);
            }
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

            // 按 mode 构造通用 ProviderRegistry：code 填 ToolLoop 实例、doc 填 Direct 实例。
            let mut registry = breakpoints::provider::registry::ProviderRegistry::new();
            for pm in &planned {
                let pc = match cfg.providers.get(&pm.name) {
                    Some(p) => p.clone(),
                    None => {
                        eprintln!("error: model '{}' has no [providers.{}] config", pm.name, pm.name);
                        std::process::exit(2);
                    }
                };
                let key = cfg.resolve_key(&pm.name).unwrap_or_default();
                match detected {
                    Mode::Code => {
                        let backend = match backend_from_config(&pc, key) {
                            Ok(b) => b,
                            Err(e) => {
                                eprintln!("error: {e}");
                                std::process::exit(2);
                            }
                        };
                        registry.insert(pm.name.clone(), Box::new(ToolLoopProvider::new(backend, cwd.clone())));
                    }
                    Mode::Doc => {
                        registry.insert(pm.name.clone(), Box::new(DirectProvider::from_config(&pc, key)));
                    }
                }
            }

            let result = match detected {
                Mode::Code => {
                    let changes = match breakpoints::evidence::git::changed_files(
                        base.as_deref(),
                        scope_enum == Scope::Branch,
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("error: {e}");
                            std::process::exit(2);
                        }
                    };
                    run_code_review(&args, &registry, &cfg, changes, &cwd).await
                }
                Mode::Doc => run_review(args, &registry, &cfg).await,
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
