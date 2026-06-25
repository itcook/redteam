use breakpoints::cli::{check_multi_model, parse_format, run_review, ReviewArgs};
use breakpoints::config::Config;
use breakpoints::output::render;
use breakpoints::provider::direct::DirectProvider;
use breakpoints::provider::ReviewProvider;
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

            // #1 MVP 多模型保护：显式 --models 传入 >1 个不同 provider 名时明确拒绝。
            // run_review 层也做同样检查（可测），main 层提前 exit 避免构造无用 provider。
            if let Err(e) = check_multi_model(&models) {
                eprintln!("error: {e}");
                std::process::exit(2);
            }

            // 选 lead provider 构造真实 DirectProvider。
            // 优先级：--models 首个 → review.default_models 首个 → 按 priority 降序的首个已配置 provider。
            // MVP：单实例；按 model 路由到多 provider 实例的注册表是 Plan 2 的 ProviderRegistry。
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
            let provider: Box<dyn ReviewProvider> = match lead
                .as_deref()
                .and_then(|n| cfg.providers.get(n).map(|pc| (n, pc)))
            {
                Some((n, pc)) => {
                    let key = cfg.resolve_key(n).unwrap_or_default();
                    Box::new(DirectProvider::from_config(pc, key))
                }
                None => {
                    eprintln!("no provider available, please configure [providers.*] first");
                    std::process::exit(2);
                }
            };
            let fmt = parse_format(&format);
            let args = ReviewArgs {
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
                background,
                format,
                targets,
            };
            match run_review(args, provider.as_ref(), &cfg).await {
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
