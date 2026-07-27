use redteam::cli::{parse_format, resolve_models, ReviewArgs};
use redteam::config::Config;
use redteam::model::Prober as _;
use redteam::output::render;
use redteam::provider::backends::backend_from_config;
use redteam::provider::registry::ProviderRegistry;
use redteam::provider::tool_loop::ToolLoopProvider;
use clap::{Parser, Subcommand};
use std::path::Path;

#[derive(Parser)]
// args_override_self：同名 flag 重复时后者胜（而非报错）——ship 等编排方把自己管理的
// flag 追加在用户参数之后即权威生效，无需依赖 prompt 层把用户重复参数「摘干净」
// （r5 dogfood 1c72b790：LLM 摘参不可靠，机制化消灭「重复即崩」类）。
#[command(name = "redteam", version, about = "模型无关的对抗式代码评审器", args_override_self = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// 发起对抗式评审：多模型扇出 + 工具检索 + 定向核实，输出 verdict/findings。
    /// 退出码：reject→10、approve 但有降级/不完整→2、干净 approve→0。
    Review {
        /// diff 基线（分支名/commit）；缺省按 --scope 收集当前改动。
        #[arg(long)]
        base: Option<String>,
        /// 证据范围：working-tree（未提交改动）| branch（当前分支相对基线的全部提交）。
        /// 缺省：给了 --base 用 branch，否则 working-tree。
        #[arg(long, value_parser = ["working-tree", "branch"])]
        scope: Option<String>,
        /// 对照的设计/规格文档路径（可多次）——注入评审上下文，供 [L0] 检查实现是否偏离设计意图
        /// （评审对象仍是代码、spec 仅作参照；总注入 ≤64KB 超则截断；密钥/凭据类文件拒绝注入防泄漏）。
        #[arg(long)]
        spec: Vec<String>,
        /// 用户聚焦提示，注入评审 prompt 引导注意力（如「重点看并发安全」）。
        #[arg(long)]
        focus: Option<String>,
        /// 逗号分隔的 model 名，每个各评一遍（复评提升 agreement 信号，计费随模型数翻倍）；
        /// 缺省用配置的 default_models，全空时才按 priority 组候补链取一。
        #[arg(long)]
        models: Option<String>,
        /// 所有已配置 model 各评一遍（复评提升 agreement 信号），而非候补链取一。
        #[arg(long)]
        models_all: bool,
        /// verdict 阻断阈值：low | medium（默认）| high | critical——达到即 reject。
        #[arg(long, value_parser = ["low", "medium", "high", "critical"])]
        block_threshold: Option<String>,
        /// finding 置信度下限（默认 0.5）：低于此值不参与 reject 判定，但仍保留在报告中。
        #[arg(long)]
        conf_floor: Option<f32>,
        /// 工具档位：off（纯无工具）| two-stage（默认：无工具评审；二阶段补查机制后续批次交付，
        /// 当前等效 off）| on（对话式工具，诊断/对照档）。缺省读 config [review].tools。
        #[arg(long, value_parser = ["off", "two-stage", "on"])]
        tools: Option<String>,
        /// 后台作业模式：立即返回 job id，用 status/result 跟进（40min deadline 兜底）。
        #[arg(long)]
        background: bool,
        /// 输出格式：json（默认）| md。
        #[arg(long, default_value = "json", value_parser = ["json", "md"])]
        format: String,
        /// R2.3：补跑指定作业的差集 cell（不重收集证据），前台同步执行；忽略其余 review flag
        /// （原始 base/models 等从该作业落盘的 args.json 里重建）。
        #[arg(long)]
        resume: Option<String>,
        /// 台账来源覆盖：db 文件路径 或 16-hex id 逗号列表（csv 仅豁免）。缺省 zero-config：
        /// 项目存在 .redteam/proj.db 即自动加载（豁免 + refute 判例裁决），run.ledger 披露。
        #[arg(long)]
        dismissed: Option<String>,
        /// 本次评审完全关闭台账（豁免/预筛/判例注入全停）。
        #[arg(long)]
        no_dismissed: bool,
        /// 增量轮（M13）：以指定作业的评审为基线，只审其 stamped HEAD 以来已提交的 fix delta，
        /// 未动代码的 findings 重锚定携带。仅 branch scope（须 --base）；上轮须干净
        /// 全量（非 incomplete、无 failed_models、经 --background 持久化）。守卫违例显式报错，
        /// 不静默降级全量。
        #[arg(long)]
        incremental_from: Option<String>,
    },
    /// 查询作业状态：无 id 列本项目全部；有 id 打印该作业 JSON。
    Status {
        job_id: Option<String>,
        /// 输出格式：json（默认，机读契约）| table（人读）
        #[arg(long, default_value = "json", value_parser = ["json", "table"])]
        format: String,
        /// 输出该 job 的 cell 级明细（JSON 数组：duration_ms/tokens/attempts/warn，调试用）。
        #[arg(long)]
        cells: bool,
    },
    /// 取回作业的评审结果（持久存储于 .redteam/proj.db，永不过期、可重复取）。
    /// 任一终态可取（failed/timed_out 有部分报告也放行）；未终态返回退出码 2。
    Result {
        job_id: String,
        /// 输出格式：json（默认）| md。
        #[arg(long, default_value = "json", value_parser = ["json", "md"])]
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
    /// 驳回判例台账（.redteam/proj.db）：建卡/查卡/状态迁移/锚失效回收。
    Dismiss {
        #[command(subcommand)]
        action: DismissCmd,
    },
    /// 销案：把「属实→已修复」的 finding 记入 resolve 台账（proj.db 独立表，不混 dismissed
    /// ——已修复≠误报，不进豁免/预筛）。增量轮（review --incremental-from）据此不再携带该
    /// finding；quote 未随修复消失的会注入下轮「顺带验证」防假修复。
    Resolve {
        /// finding id（16 位 hex，来自评审报告）。
        id: String,
        /// 评审报告 JSON 路径——按 id 自动抄 title/file/quote/severity 快照。
        #[arg(long)]
        from: Option<String>,
        /// 销案发生轮次（如 r2），可选。
        #[arg(long, default_value = "")]
        round: String,
        /// 修法一句话（可选，进台账与 history 备审计）；再销案时缺省沿用卡上原理由。
        #[arg(long, default_value = "")]
        reason: String,
        /// 快照字段（无 --from 时必须全给；与 --from 并用时覆盖抄写值）。
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        quote: Option<String>,
        #[arg(long)]
        severity: Option<String>,
    },
    /// 幂等创建项目级 .redteam/ 骨架（config.toml 模板、.gitignore、rounds/、proj.db）。
    Init,
}

#[derive(Subcommand)]
enum DismissCmd {
    /// 建卡（记一条人裁驳回判例）。快照四件套优先从 --from 报告按 id 自动抄写。
    Add {
        /// finding id（16 位 hex，来自评审报告）。
        id: String,
        /// 驳回理由（判例核心，refute 判例裁决与人读都依赖它）。
        #[arg(long)]
        reason: String,
        /// 评审报告 JSON 路径（redteam result 输出）——按 id 自动抄 title/file/quote/severity。
        #[arg(long)]
        from: Option<String>,
        /// 驳回发生轮次（如 r2），可选。
        #[arg(long, default_value = "")]
        round: String,
        /// 快照字段（无 --from 时必须全给；与 --from 并用时覆盖抄写值）。
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        quote: Option<String>,
        #[arg(long)]
        severity: Option<String>,
    },
    /// 列卡（默认全状态）。
    List {
        /// 按状态过滤：dismissed | re-upheld | human-confirmed | withdrawn | stale。
        #[arg(long, value_parser = redteam::db::STATUSES.to_vec())]
        status: Option<String>,
        /// 输出格式：table（默认，人读）| json。
        #[arg(long, default_value = "table", value_parser = ["table", "json"])]
        format: String,
    },
    /// 单卡详情 + 全部历史事件。
    Show { id: String },
    /// 状态迁移（协议动作：re-upheld=复裁仍误报 / human-confirmed=人裁终局 /
    /// withdrawn=改判属实 / dismissed=回滚）。历史全留痕；不做状态机硬约束。
    SetStatus {
        id: String,
        #[arg(value_parser = redteam::db::STATUSES.to_vec())]
        status: String,
        /// 迁移发生轮次（如 r3），可选。
        #[arg(long, default_value = "")]
        round: String,
        /// 新理由；缺省沿用卡上原理由。
        #[arg(long)]
        reason: Option<String>,
    },
    /// 锚失效回收：quote 在当前代码中已不存在的活跃卡 → 标 stale（human-confirmed 豁免）。
    /// 默认 dry-run 只打印清单；--apply 才落账。
    Gc {
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// 配置一个 provider/model（flag 驱动）。
    Setup {
        /// provider 名（配置块键名，如 DeepSeek、Kimi）。
        #[arg(long)]
        provider: String,
        /// 协议类型：openai | anthropic（含 claude/gpt/gemini 等别名）。
        #[arg(long)]
        kind: Option<String>,
        /// API 端点 base URL（anthropic 网关须带 /v1）。
        #[arg(long)]
        base_url: Option<String>,
        /// model id（如 deepseek-v4-pro）。
        #[arg(long)]
        model: Option<String>,
        /// API key（配合 --key-store 决定去处；不落 config.toml）。
        #[arg(long)]
        key: Option<String>,
        /// 候补链优先级（大者先选）。
        #[arg(long)]
        priority: Option<i32>,
        /// key 去处：env(默认，打印 export) | secrets(写 secrets.toml chmod 600)
        #[arg(long, default_value = "env", value_parser = ["env", "secrets"])]
        key_store: String,
    },
    /// 列已配置 provider + 可用性 + 配置位置。
    List {
        /// 对指定 provider 实探上游 /models 列表（带 key 探测，抓 401 假阳性）。
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

/// Fix 1（final-review）：`redteam result` 在任一终态（Done|Failed|TimedOut|Cancelled）都应放行——
/// 失败/超时路径经 `write_partial_result` 落了带 `verdict: incomplete` 的 result 记录（ship 协议
/// §2.4 依赖取到它），不该被 Failed/TimedOut 一律拦成 "not done"。门槛统一为「终态 + proj.db 有 result 记录」；
/// 非终态（Pending/Running）或文件缺失，行为不变（沿用既有报错文案）。
/// 抽成纯函数便于单测，避免每条分支都要拉起子进程。
fn resolve_result(
    store: &redteam::state::JobStore,
    job_id: &str,
    format: &str,
    st: &redteam::state::JobState,
) -> Result<String, (String, i32)> {
    use redteam::state::JobStatus;
    let terminal = matches!(
        st.status,
        JobStatus::Done | JobStatus::Failed | JobStatus::TimedOut | JobStatus::Cancelled
    );
    if !terminal {
        return Err((format!("job {job_id} not done (status: {:?})", st.status), 2));
    }
    let content = if format == "md" {
        store.read_result_md(job_id).or_else(|| store.read_result(job_id))
    } else {
        store.read_result(job_id)
    };
    match content {
        Some(raw) => Ok(raw),
        None if st.status == JobStatus::Done => {
            Err((format!("result missing for {job_id}"), 2))
        }
        None => Err((format!("job {job_id} not done (status: {:?})", st.status), 2)),
    }
}

/// 构造 ProviderRegistry（前台与 worker 共用，避免重复逻辑）。
/// 返回 Err(消息) 代替直接 process::exit，调用方决定如何处理。
fn build_registry(args: &ReviewArgs, cfg: &Config, cwd: &Path) -> Result<ProviderRegistry, String> {
    let (planned, _replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    if planned.is_empty() {
        return Err("no provider available, please configure [providers.*] first".to_string());
    }

    // 工具档位生效值：--tools 覆盖 config [review].tools；未知/缺省字符串落 config 默认
    // （clap 已限值域，from_flag 返回 None 仅防漂移）。build_registry 是前台/resume/worker
    // 三条路径唯一的 registry 构造点，且已持有 args+cfg——在此就地解析，无需再加参数
    // 穿透调用方（resume 传入的是重建后的 args，天然拿到重建值）。
    let tools_mode = args.tools.as_deref()
        .and_then(redteam::config::ToolsMode::from_flag)
        .unwrap_or(cfg.review.tools);
    let tools_enabled = matches!(tools_mode, redteam::config::ToolsMode::On);

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
        registry.insert(pm.name.clone(), Box::new(ToolLoopProvider::new(backend, cwd.to_path_buf(), tools_enabled)));
    }

    Ok(registry)
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Review {
            base,
            scope,
            spec,
            focus,
            models,
            models_all,
            block_threshold,
            conf_floor,
            tools,
            background,
            format,
            resume,
            dismissed,
            no_dismissed,
            incremental_from,
        } => {
            let cfg = load_config_warned();
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

            let fmt = parse_format(&format);

            // R2.3：--resume <job_id> 补跑差集 cell，前台同步；忽略本次调用的其余 flag——
            // base/models 等从该作业落盘的 args.json 重建（run_resume 内部处理）。
            if let Some(job_id) = resume {
                let ws = cwd.clone();
                // ④级：JobStore 打不开是硬错误，直冒（main() 非 Result 返回，沿用本仓
                // match/exit(2) 惯用法做「?的等价物」，而非静默 let _=）。
                let store = match redteam::state::JobStore::open(&ws) {
                    Ok(s) => s,
                    Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
                };
                let orig_args_json = match store.read_args_raw(&job_id) {
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
                let registry = match build_registry(&orig_args, &cfg, &cwd) {
                    Ok(x) => x,
                    Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
                };
                let (report, code) =
                    redteam::cli::run_resume(&job_id, &registry, &cfg, &cwd, &store).await;
                println!("{}", render(&report, fmt));
                std::process::exit(code);
            }

            // scope 缺省定案（r1 20260703 事故：--base + 默认 working-tree = base 被静默
            // 忽略 → 干净树空评审秒出 approve）：有 --base 自动 branch；显式矛盾组合报错。
            let scope = match (scope.as_deref(), base.is_some()) {
                (Some("working-tree"), true) => {
                    eprintln!("error: --base only takes effect with --scope branch (working-tree reviews uncommitted changes only); drop --base or use --scope branch");
                    std::process::exit(2);
                }
                (Some(s), _) => s.to_string(),
                (None, true) => "branch".to_string(),
                (None, false) => "working-tree".to_string(),
            };
            let args = ReviewArgs {
                base: base.clone(), scope, spec, focus, models, models_all,
                block_threshold, conf_floor, tools, background, format,
                resume: None, dismissed, no_dismissed, incremental_from,
            };

            // 后台：写 pending state + args 快照（proj.db）→ spawn detached worker → 打印 job-id 退出。
            if background {
                let ws = cwd.clone();
                let job_id = gen_job_id(&cwd);
                // ④级：store 打不开是硬错误，直冒。
                let store = match redteam::state::JobStore::open(&ws) {
                    Ok(s) => s,
                    Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
                };
                let args_json = serde_json::to_string(&args).unwrap_or_default();
                if store.write_args_raw(&job_id, &args_json).is_err() {
                    eprintln!("error: failed to write job args");
                    std::process::exit(2);
                }
                // ④级：起手建 job 是 gating 写——失败则整条后台派发都不成立，直接报错退出。
                if let Err(e) = store.create_job(&job_id, now_secs()) {
                    eprintln!("error: failed to create job: {e}");
                    std::process::exit(2);
                }
                // JobStore::create_job 没有 workspace 形参（起手态只收 id+started_at）——
                // 这里补一次守卫式更新回填 workspace。二写之间存在极短窗口（workspace 短暂
                // 为 None），worker 尚未起跑不会读到；workspace 只是 status 表的展示字段，
                // 非 gating，失败只报警不 exit（C-Q4 决策点，详见任务报告）。
                let workspace = std::fs::canonicalize(&cwd)
                    .unwrap_or_else(|_| cwd.clone())
                    .to_string_lossy()
                    .into_owned();
                if let Err(e) = store.update_if_active(&job_id, |s| { s.workspace = Some(workspace); }) {
                    eprintln!("warn: job {job_id} workspace 回填失败: {e}");
                }
                match spawn_detached_worker(&job_id) {
                    Ok(pid) => {
                        // pid 登记走守卫（dogfood F2 抓漏）：子进程可能已抢先把状态推进到
                        // Running 甚至终态（极快失败），用 spawn 前的 Pending 快照裸写会把
                        // 它覆回去。守卫在新鲜快照上只补 pid/pid_start、不动 status；
                        // 已终态则拒写（worker 已结束，pid 无登记意义）。
                        let pid_start = redteam::procinfo::start_time(pid);
                        if let Err(e) = store.update_if_active(&job_id, |s| {
                            s.pid = Some(pid);
                            s.pid_start = pid_start;
                        }) {
                            eprintln!("warn: job {job_id} pid 登记失败: {e}");
                        }
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
            let registry = match build_registry(&args, &cfg, &cwd) {
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
            let ws = cwd.clone();
            // ④级：worker 主进程打不开 store 是硬错误，直接报错退出。
            // M17-c Task 3：单连接贯穿 worker 全生命周期——Arc 化后 run_job/panic hook/收尾
            // 共享同一个 JobStore（同一个底层 rusqlite::Connection），不再各自现开连接。
            let store = match redteam::state::JobStore::open(&ws) {
                Ok(s) => std::sync::Arc::new(s),
                Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
            };
            // 进程级 panic 兜底（E12-Q4，真·顶层）：worker 评审体若 panic，落 Failed 终态再正常
            // 展开——否则状态永停 Running（惰性收尸只治「进程已死」，panic 与退出间有窗口）。
            // hook 是 'static Fn：M17-c Task 3 前受限于闭包无法借用局部 &JobStore，只能在触发时
            // 现开一个临时 store；现在 store 已是 Arc<JobStore>（Send+Sync，见 db.rs 的
            // Mutex<ProjDb> 内部可变性），克隆进闭包即满足 'static，遂改为与主流程共享同一连接——
            // 少一次 panic 时的临时连接开销，且不再需要 ws_h 这份路径 clone。
            {
                let store_h = std::sync::Arc::clone(&store);
                let jid_h = job_id.clone();
                let default_hook = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    let msg = info
                        .payload()
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| info.payload().downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown".to_string());
                    redteam::cli::mark_failed_panic(&store_h, &jid_h, &msg);
                    default_hook(info);
                }));
            }
            let args_json = match store.read_args_raw(&job_id) {
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
            // 建 registry（复用 build_registry：resolve_models + 构造 ProviderRegistry）
            let registry = match build_registry(&args, &cfg, &cwd) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("error: {e}");
                    // 写 Failed 终态，让 status 命令能看到错误原因。走守卫：裸覆写
                    // 会盖掉 pre-worker 阶段已落定的 Cancelled 终态（首个终态封盘）。
                    match store.update_if_active(&job_id, |st| {
                        st.status = redteam::state::JobStatus::Failed;
                        st.error = Some(e);
                        st.exit_code = Some(2);
                    }) {
                        Ok(false) => eprintln!("warn: job {job_id} 终态写被终态封盘拒绝"),
                        Err(e) => eprintln!("warn: job {job_id} 终态写失败: {e}"),
                        Ok(true) => {}
                    }
                    std::process::exit(2);
                }
            };
            let started = store
                .read_state(&job_id)
                .map(|s| s.started_at)
                .unwrap_or_else(now_secs);
            // R1.2：worker 起手设 deadline_at（若尚未设置——例如状态文件是旧格式或已被设过）。
            // 从裸覆写改为守卫式更新（C-Q4 决策点：行为收紧），抽成 cli::stamp_deadline 小函数
            // 便于单测锚住守卫语义（见 cli/mod.rs stamp_deadline_tests）。
            redteam::cli::stamp_deadline(&store, &job_id, started);
            let code =
                redteam::cli::run_job(&job_id, &args, &registry, &cfg, &cwd, started, store)
                    .await;
            std::process::exit(code);
        }
        Cmd::Status { job_id, format, cells } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            // ④级：store 打不开是硬错误，直冒。
            let store = match redteam::state::JobStore::open(&ws) {
                Ok(s) => s,
                Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
            };
            if cells {
                return_code_for_cells(&store, job_id.as_deref())
            } else {
            // 两分支统一收集（列表 / 单 job），再按 format 分流；json 输出形状与改动前逐字节一致。
            let jobs: Option<Vec<redteam::state::JobState>> = match &job_id {
                None => Some(
                    store
                        .list_states()
                        .into_iter()
                        .map(|st| redteam::cli::reap_if_dead(&store, st))
                        .collect(),
                ),
                Some(id) => store
                    .read_state(id)
                    .map(|st| vec![redteam::cli::reap_if_dead(&store, st)]),
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
        }
        Cmd::Result { job_id, format } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            // ④级：store 打不开是硬错误，直冒。
            let store = match redteam::state::JobStore::open(&ws) {
                Ok(s) => s,
                Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
            };
            let st = store.read_state(&job_id);
            match st {
                Some(s) => match resolve_result(&store, &job_id, &format, &s) {
                    Ok(raw) => {
                        println!("{raw}");
                        s.exit_code.unwrap_or(0)
                    }
                    Err((msg, code)) => { eprintln!("{msg}"); code }
                },
                None => { eprintln!("no such job: {job_id}"); 2 }
            }
        }
        Cmd::Cancel { job_id } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            // ④级：store 打不开是硬错误，直冒。
            let store = match redteam::state::JobStore::open(&ws) {
                Ok(s) => s,
                Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
            };
            match store.read_state(&job_id) {
                Some(s) if matches!(s.status, redteam::state::JobStatus::Running | redteam::state::JobStatus::Pending) => {
                    // 仅当 worker 进程确活（pid 存活 且 身份锚吻合）才发信号——否则 pid 可能已被
                    // OS 复用给无辜进程，盲发 SIGTERM 会误杀（E12-Q3/bug③）。
                    if let Some(pid) = s.pid {
                        if redteam::procinfo::is_worker_alive(pid, s.pid_start) {
                            // SAFETY: kill 单系统调用；已确认 pid 属本 job 原进程。
                            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM); }
                        }
                    }
                    // 守卫写 Cancelled：若 worker 已抢先落终态（Done/Failed/TimedOut），守卫拒写、
                    // 保留其终态（首个终态封盘/bug①）。三分支化（评审 Important）：`.unwrap_or(false)`
                    // 曾把「守卫拒写 Ok(false)」与「db 真错误 Err(e)」折成同一路径，掩盖真实故障；
                    // 对照 cli/mod.rs 同批四处终态写的既有三分支模式拆开。
                    match store.update_if_active(&job_id, |st| {
                        st.status = redteam::state::JobStatus::Cancelled;
                    }) {
                        Ok(true) => {
                            println!("cancelled {job_id}");
                            0
                        }
                        Ok(false) => {
                            let now = store.read_state(&job_id).map(|s| s.status);
                            eprintln!("job {job_id} already finished ({now:?}), not cancelled");
                            2
                        }
                        Err(e) => {
                            eprintln!("error: job {job_id} cancel 写失败（非拒写，db 层异常）: {e}");
                            2
                        }
                    }
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
        Cmd::Dismiss { action } => run_dismiss(action),
        Cmd::Resolve { id, from, round, reason, title, file, quote, severity } => {
            run_resolve(id, from, round, reason, title, file, quote, severity)
        }
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

/// dismiss 子命令族：台账机器化（design 2026-07-03）。同步执行，直读写 proj.db。
fn run_dismiss(action: DismissCmd) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut db = match redteam::db::ProjDb::open(&cwd) {
        Ok(d) => d,
        Err(e) => { eprintln!("error: {e}"); return 2; }
    };
    let r = match action {
        DismissCmd::Add { id, reason, from, round, title, file, quote, severity } => {
            dismiss_add(&mut db, id, reason, from, round, title, file, quote, severity)
        }
        DismissCmd::List { status, format } => (|| {
            let cards = db.dismiss_list(status.as_deref())?;
            if format == "json" {
                println!("{}", serde_json::to_string_pretty(&cards)?);
            } else {
                println!("{:<16}  {:<15}  {:<8}  {:<4}  {:<10}  {}", "ID", "STATUS", "SEV", "RND", "UPDATED", "FILE | TITLE");
                for c in &cards {
                    let title: String = c.title.chars().take(60).collect();
                    println!("{:<16}  {:<15}  {:<8}  {:<4}  {:<10}  {} | {}", c.id, c.status, c.severity, c.round, c.updated_at, c.file, title);
                }
                println!("({} cards)", cards.len());
            }
            Ok(())
        })(),
        DismissCmd::Show { id } => (|| {
            let card = db.dismiss_get(&id)?.ok_or_else(|| anyhow::anyhow!("no card with id {id}"))?;
            println!("{}", serde_json::to_string_pretty(&card)?);
            println!("history:");
            for (status, round, reason, at) in db.dismiss_history(&id)? {
                println!("  {at}  {status}({round})  {reason}");
            }
            Ok(())
        })(),
        DismissCmd::SetStatus { id, status, round, reason } => (|| {
            let (card, warn) = db.dismiss_set_status(&id, &status, &round, reason.as_deref())?;
            if let Some(w) = warn { eprintln!("warning: {w}"); }
            println!("{} -> {}", card.id, card.status);
            Ok(())
        })(),
        DismissCmd::Gc { apply } => dismiss_gc(&mut db, &cwd, apply),
    };
    match r {
        Ok(()) => 0,
        Err(e) => { eprintln!("error: {e}"); 2 }
    }
}

#[allow(clippy::too_many_arguments)]
fn dismiss_add(
    db: &mut redteam::db::ProjDb,
    id: String, reason: String, from: Option<String>, round: String,
    title: Option<String>, file: Option<String>, quote: Option<String>, severity: Option<String>,
) -> anyhow::Result<()> {
    anyhow::ensure!(redteam::project::is_hex16(&id), "id must be a 16-hex finding id, got '{id}'");
    // --from 打底，单字段 flag 覆盖；两路都缺 → 报错拒瘦卡（无快照的卡 gc 不可判、判例残缺）
    let snap = match &from {
        Some(path) => Some(snapshot_from_report(path, &id)?),
        None => None,
    };
    let pick = |explicit: Option<String>, from_val: Option<String>, name: &str| -> anyhow::Result<String> {
        explicit.or(from_val).ok_or_else(|| anyhow::anyhow!(
            "missing snapshot field '{name}': pass --from <result.json> or --{name}"))
    };
    let (ft, ff, fq, fs) = match snap {
        Some((t, f, q, s)) => (Some(t), Some(f), Some(q), Some(s)),
        None => (None, None, None, None),
    };
    let card = redteam::db::DismissCard {
        id: id.clone(),
        status: "dismissed".into(),
        title: pick(title, ft, "title")?,
        file: pick(file, ff, "file")?,
        // quote 允许为空串（doc 遗漏类 finding 本就无引文），但字段必须显式来自快照或 flag
        quote: pick(quote, fq, "quote")?,
        severity: pick(severity, fs, "severity")?,
        reason,
        round,
        updated_at: String::new(),
    };
    db.dismiss_upsert(&card)?;
    println!("dismissed {id} recorded");
    Ok(())
}

/// resolve 子命令（M13 决议 3）：dismiss add 对称的销案入账。
#[allow(clippy::too_many_arguments)]
fn run_resolve(
    id: String, from: Option<String>, round: String, reason: String,
    title: Option<String>, file: Option<String>, quote: Option<String>, severity: Option<String>,
) -> i32 {
    let r = (|| -> anyhow::Result<()> {
        anyhow::ensure!(redteam::project::is_hex16(&id), "id must be a 16-hex finding id, got '{id}'");
        let snap = match &from {
            Some(path) => Some(snapshot_from_report(path, &id)?),
            None => None,
        };
        let pick = |explicit: Option<String>, from_val: Option<String>, name: &str| -> anyhow::Result<String> {
            explicit.or(from_val).ok_or_else(|| anyhow::anyhow!(
                "missing snapshot field '{name}': pass --from <result.json> or --{name}"))
        };
        let (ft, ff, fq, fs) = match snap {
            Some((t, f, q, s)) => (Some(t), Some(f), Some(q), Some(s)),
            None => (None, None, None, None),
        };
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut db = redteam::db::ProjDb::open(&cwd)?;
        db.resolve_upsert(&redteam::db::ResolveCard {
            id: id.clone(),
            title: pick(title, ft, "title")?,
            file: pick(file, ff, "file")?,
            quote: pick(quote, fq, "quote")?,
            severity: pick(severity, fs, "severity")?,
            reason,
            round,
            updated_at: String::new(),
        })?;
        println!("resolved {id} recorded");
        Ok(())
    })();
    match r {
        Ok(()) => 0,
        Err(e) => { eprintln!("error: {e}"); 2 }
    }
}

/// 从评审报告 JSON 按 id 抄快照四件套 (title, file, quote, severity)。
fn snapshot_from_report(path: &str, id: &str) -> anyhow::Result<(String, String, String, String)> {
    let raw = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read {path}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("parse {path}: {e}"))?;
    let f = v["findings"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{path}: no findings array"))?
        .iter()
        .find(|f| f["id"].as_str() == Some(id))
        .ok_or_else(|| anyhow::anyhow!("finding {id} not in {path}"))?;
    let s = |k: &str| f[k].as_str().unwrap_or_default().to_string();
    Ok((s("title"), s("file"), s("quote"), s("severity")))
}

/// 锚失效回收：反转遍历（一次 walk，对未存活候选做 contains 消活）——O(files) 而非 O(cards×files)。
/// 匹配保守（宁保卡不误杀）：整段 quote 或其最长非空行命中任一文件即视为存活。
fn dismiss_gc(db: &mut redteam::db::ProjDb, root: &std::path::Path, apply: bool) -> anyhow::Result<()> {
    let candidates: Vec<_> = db
        .active_cards()?
        .into_iter()
        .filter(|c| c.status != "human-confirmed") // 人裁终局豁免 gc
        .filter(|c| !c.quote.trim().is_empty())    // 无锚不可机械判，跳过
        .collect();
    if candidates.is_empty() {
        println!("gc: no eligible cards (need active, non-human-confirmed, with quote)");
        return Ok(());
    }
    let needle = |c: &redteam::db::DismissCard| -> (String, String) {
        let q = c.quote.trim().to_string();
        let longest = q.lines().map(str::trim).filter(|l| !l.is_empty())
            .max_by_key(|l| l.len()).unwrap_or_default().to_string();
        (q, longest)
    };
    let mut dead: Vec<(redteam::db::DismissCard, (String, String))> =
        candidates.into_iter().map(|c| { let n = needle(&c); (c, n) }).collect();
    for entry in ignore::Walk::new(root).flatten() {
        if dead.is_empty() { break; }
        if !entry.file_type().is_some_and(|t| t.is_file()) { continue; }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            dead.retain(|(_, (q, l))| !(content.contains(q.as_str()) || (!l.is_empty() && content.contains(l.as_str()))));
        }
    }
    if dead.is_empty() {
        println!("gc: all quotes still anchored, nothing to collect");
        return Ok(());
    }
    for (c, _) in &dead {
        println!("stale: {}  {}  {}", c.id, c.file, c.title.chars().take(60).collect::<String>());
    }
    if apply {
        for (c, _) in &dead {
            db.dismiss_set_status(&c.id, "stale", "gc", None)?;
        }
        println!("gc: {} card(s) marked stale", dead.len());
    } else {
        println!("gc: dry-run ({} candidate(s)); pass --apply to mark stale", dead.len());
    }
    Ok(())
}

/// `status <job> --cells`：cell 级运行学明细（JSON 数组，2026-07-03 grill 定案）。
fn return_code_for_cells(store: &redteam::state::JobStore, job_id: Option<&str>) -> i32 {
    match job_id {
        None => { eprintln!("--cells requires a job id"); 2 }
        Some(id) => {
            if store.read_state(id).is_none() {
                eprintln!("no such job: {id}");
                return 2;
            }
            let recs = store.read_partial(id);
            println!("{}", serde_json::to_string_pretty(&recs).unwrap_or_else(|_| "[]".into()));
            0
        }
    }
}

/// M5 批2：`status --format table` 人读视图（opt-in；默认 JSON 数组契约不动）。
/// 空格对齐、无框线——窄终端可 wrap、输出可 grep。now 注入以便测试。
fn render_status_table(jobs: &[redteam::state::JobState], now: u64) -> String {
    const HEADER: [&str; 7] = ["JOB", "STATUS", "PHASE", "PROG", "AGE", "IDLE", "WORKSPACE"];
    let rows: Vec<[String; 7]> = jobs
        .iter()
        .map(|j| {
            [
                j.job_id.clone(),
                serde_str(&j.status),
                // 运行中带当前 phase 停留时长与注记（refute[9 targets] (2m)）——
                // 「久了不知道卡哪」的第一查询面（2026-07-03 grill 定案）。
                {
                    let name = j.phase.as_ref().map(serde_str).unwrap_or_else(|| "-".into());
                    match j.phase_history.last() {
                        Some(stamp) if j.status == redteam::state::JobStatus::Running && stamp.at > 0 => {
                            let note = stamp.note.as_deref().map(|n| format!("[{n}]")).unwrap_or_default();
                            format!("{name}{note} ({})", fmt_dur(now.saturating_sub(stamp.at)))
                        }
                        _ => name,
                    }
                },
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
    fn status_table_running_phase_shows_dwell_and_note() {
        use redteam::state::{JobState, JobStatus, Phase};
        let mut a = JobState::new("abc123def456", 1000);
        a.status = JobStatus::Running;
        a.set_phase(Phase::Refute, Some("9 targets".into()));
        a.phase_history.last_mut().unwrap().at = 970; // 覆写为可控时刻（set_phase 用真实时钟）
        let out = render_status_table(&[a], 1000);
        assert!(out.contains("refute[9 targets] (30s)"), "phase 停留时长+注记: {out}");
    }

    #[test]
    fn render_status_table_shapes_columns() {
        use redteam::state::{JobState, JobStatus, Phase, Progress};
        let mut a = JobState::new("abc123def456", 1000);
        a.status = JobStatus::Running;
        a.phase = Some(Phase::Fanout);
        a.progress = Progress { done: 3, total: 10 };
        a.last_active = 4500;
        a.workspace = Some("/repos/sites".into());
        let b = JobState::new("fff000fff000", 4000); // pending：phase/workspace 全 None、last_active=0
        let out = render_status_table(&[a, b], 4600);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains("JOB") && lines[0].contains("STATUS") && lines[0].contains("PROG")
            && lines[0].contains("AGE") && lines[0].contains("IDLE"), "header: {}", lines[0]);
        assert!(lines[1].contains("abc123def456") && lines[1].contains("running")
            && lines[1].contains("fanout")
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

    /// C1：无 provider 配置的 Config + ReviewArgs → build_registry 返回 Err。
    #[test]
    fn build_registry_no_provider_returns_err() {
        use redteam::cli::ReviewArgs;
        use redteam::config::Config;

        // 空配置：无 [providers.*]，resolve_models 会产出空列表
        let cfg = Config::default();
        let cwd = std::path::Path::new(".");
        let args = ReviewArgs {
            base: None,
            scope: "working-tree".into(),
            spec: vec![],
            focus: None,
            models: None,
            models_all: false,
            block_threshold: None,
            conf_floor: None,
            background: false,
            format: "json".into(),
            resume: None,
            dismissed: None,
            no_dismissed: false,
            incremental_from: None,
            tools: None,
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
        use redteam::state::{JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let store = redteam::state::JobStore::open(ws).unwrap();
        store.write_result("j1", r#"{"verdict":"incomplete"}"#).unwrap();
        let mut st = JobState::new("j1", 0);
        st.status = JobStatus::Failed;
        st.exit_code = Some(2);
        let out = resolve_result(&store, "j1", "json", &st);
        assert_eq!(out, Ok(r#"{"verdict":"incomplete"}"#.to_string()));
    }

    /// Fix 1：Failed + 无 result.json → 仍报错（未曾 checkpoint 过任何 cell 的早期崩溃）。
    #[test]
    fn resolve_result_failed_without_result_file_errs() {
        use redteam::state::{JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let store = redteam::state::JobStore::open(ws).unwrap();
        let mut st = JobState::new("j2", 0);
        st.status = JobStatus::Failed;
        let out = resolve_result(&store, "j2", "json", &st);
        assert!(out.is_err());
        assert_eq!(out.unwrap_err().1, 2);
    }

    /// 非终态（Running）不受影响：即便 result.json 意外存在也不放行。
    #[test]
    fn resolve_result_running_stays_not_done() {
        use redteam::state::{JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let store = redteam::state::JobStore::open(ws).unwrap();
        store.write_result("j3", "{}").unwrap();
        let mut st = JobState::new("j3", 0);
        st.status = JobStatus::Running;
        let out = resolve_result(&store, "j3", "json", &st);
        let (msg, code) = out.unwrap_err();
        assert!(msg.contains("not done"), "msg: {msg}");
        assert_eq!(code, 2);
    }
}
