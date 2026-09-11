use redteam::cli::{parse_format, ReviewArgs};
use redteam::config::Config;
use redteam::model::Prober as _;
use redteam::output::render;
use redteam::provider::backends::backend_from_config;
use redteam::provider::registry::ProviderRegistry;
use clap::{Parser, Subcommand};
use std::path::Path;

#[derive(Parser)]
// args_override_self：同名 flag 重复时后者胜（而非报错）——ship 等编排方把自己管理的
// flag 追加在用户参数之后即权威生效，无需依赖 prompt 层把用户重复参数「摘干净」
// （r5 dogfood 1c72b790：LLM 摘参不可靠，机制化消灭「重复即崩」类）。
#[command(name = "redteam-next", version, about = "redteam-next（M21 重构分支产物）：模型无关的对抗式代码评审器——scanner 疑点扫描地基；插件与 ship 循环仍用 dev `redteam`，M21c 切名", args_override_self = true,
    after_help = "快速上手：\n  \
    1. redteam-next init                                              # 项目内建 .redteam/ 骨架（状态与内容，无配置）\n  \
    2. redteam-next model setup --provider X --model <id> --role scanner # 给至少一个模型配 scanner 角色（key 见 --key-store）\n  \
    3. redteam-next review --base main                                # 评审当前分支相对 main 的改动\n  \
    配置：~/.redteam/config.json + secrets.json（REDTEAM_HOME 可覆盖；用户级唯一，无项目层；严格 JSON、无注释）。\n  \
    后台跑：review --background 得 job id → status <id> 看进度 → result <id> 取报告。")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// 发起对抗式评审（M21a：scan-only）：证据收集 → 引用图分组 → 改动中心切片装包 → 每组对每个 scanner 条目
    /// （× samples）并行扇出 → 疑点并集（agreement = 家族数）→ 疑点索引 + 覆盖自报报告。**不产 finding**
    /// （深挖 / 复核 / verdict 在 M21b 起）。退出码：0=scan 完成（verdict scan-only）、1=命令自身失败（参数/IO/配置错误）、
    /// 3=incomplete（任一 scanner cell 失败，疑点集有结构性缺口）；2 / 10 在 M21b 深挖落地前不可达。
    /// 并发 = 各 provider `maxConcurrency`（缺省继承 `review.maxConcurrency` = 10；无角色级 workers，D13）：路由到同一 provider 的
    /// cell 数超过其上限即排队，墙钟 = 排队 + 最长格（run.warnings 提示；每 cell `queue_wait_ms` 可读）；撞 429 时该 provider 门宽
    /// 自动收敛到网关真实上限并让撞门的请求排队重试（run.warnings / run.provider_rate_limit 可读；误填超大 maxConcurrency 也能自愈）。
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
        /// ship 循环的测试命令覆盖（缺省按 Cargo.toml / package.json / Makefile / go.mod 推断，
        /// 与 `redteam test-command` 同一推断链）；记入报告 run.test_command。
        #[arg(long)]
        test_command: Option<String>,
        /// verdict 阻断阈值：low | medium（默认）| high | critical——达到即 reject。M21b 深挖产 finding 后生效；
        /// M21a scan-only 下记录不消费。
        #[arg(long, value_parser = ["low", "medium", "high", "critical"])]
        block_threshold: Option<String>,
        /// finding 置信度下限（默认 0.5）：低于此值不参与 reject 判定。M21b 起生效；M21a scan-only 下记录不消费。
        #[arg(long)]
        conf_floor: Option<f32>,
        /// 工具档位：off | two-stage | on。scanner 恒单发（无工具、needs 只记录不补跑，M21b 深挖队列消费）；本旗对 scanner 无效，
        /// 留给 M21b reviewer 的工具分层授予（design §11）。缺省读 config [review].tools。
        #[arg(long, value_parser = ["off", "two-stage", "on"])]
        tools: Option<String>,
        /// 后台作业模式：立即返回 job id，用 status/result 跟进（40min deadline 兜底）。
        #[arg(long)]
        background: bool,
        /// 输出格式：json（默认）| md。
        #[arg(long, default_value = "json", value_parser = ["json", "md"])]
        format: String,
        /// R2.3：补跑指定作业的差集 cell（不重收集证据），前台同步执行；忽略其余 review flag
        /// （base 等从该作业落盘的 args.json 里重建；模型集按当前配置的 scanner 重建，M21b 起读队列快照）。
        #[arg(long)]
        resume: Option<String>,
        /// 台账来源覆盖：db 文件路径 或 16-hex id 逗号列表（csv 仅豁免）。缺省 zero-config：
        /// 项目存在 .redteam/proj-next.db 即自动加载，run.ledger 披露。M21a scan-only 下只披露不消费
        /// （判例前移进 scanner prompt 与豁免 / refute 裁决在 M21c 疑点台账起）。
        #[arg(long)]
        dismissed: Option<String>,
        /// 本次评审完全关闭台账（豁免/预筛/判例注入全停）。
        #[arg(long)]
        no_dismissed: bool,
        /// 增量轮（M13）：以指定作业的评审为基线只审 fix delta。M21a 硬切后**暂不可用**（显式报错，不静默降级全量）——
        /// M21c 疑点台账 + delta 激活落地后恢复；仅 branch scope（须 --base）。
        #[arg(long)]
        incremental_from: Option<String>,
        /// 预算仪表（M21a）：只收集证据 → 分组 → 装包 → 落盘 .redteam/packs/<job>/cell-<n>.md（= 真实发出的 scanner prompt：
        /// 共享头 system + 参考 / 证据 / diff / 角色尾四块）与 summary.json，每 cell 打印字节（含 system 段）与估算 token（÷3.5）、
        /// 分组统计与装包形态（body=slice|full 随 `review.pack.body`，缺省 slice：切片 / 折叠行 / 文件外片段计数）；不建 registry、
        /// 不调 provider，无模型配置也能跑，退出码 0。与 --background / --resume / --incremental-from 互斥；
        /// --format 对本模式无效（固定文本行，机读看 summary.json）。
        #[arg(long, conflicts_with_all = ["background", "resume", "incremental_from"])]
        pack_only: bool,
    },
    /// 查询作业状态：无 id 列本项目全部；有 id 打印该作业 JSON。
    Status {
        /// 作业 id（review --background 的输出）；缺省列出本项目全部作业。
        job_id: Option<String>,
        /// 输出格式：json（默认，机读契约）| table（人读）
        #[arg(long, default_value = "json", value_parser = ["json", "table"])]
        format: String,
        /// 输出该 job 的 cell 级明细（JSON 数组，schema v4：每条带 output{role,…} 角色分叉输出 + duration_ms/tokens/attempts/warn，调试用）。
        #[arg(long)]
        cells: bool,
    },
    /// 取回作业的评审结果（持久存储于 .redteam/proj-next.db，永不过期、可重复取）。
    /// 任一终态可取（failed/timed_out 有部分报告也放行）；未终态是命令自身失败，退出码 1。
    Result {
        /// 作业 id（review --background 的输出）。
        job_id: String,
        /// 输出格式：json（默认）| md。
        #[arg(long, default_value = "json", value_parser = ["json", "md"])]
        format: String,
    },
    /// 取消 running/pending 作业（发 SIGTERM）。
    Cancel {
        /// 作业 id（review --background 的输出）。
        job_id: String,
    },
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
    /// 驳回判例台账（.redteam/proj-next.db）：建卡/查卡/状态迁移/锚失效回收。
    Dismiss {
        #[command(subcommand)]
        action: DismissCmd,
    },
    /// 销案判例台账（.redteam/proj-next.db 独立表，不混 dismissed——已修复≠误报，不进豁免/预筛）：
    /// 建卡/查卡。增量轮（review --incremental-from）据此不再携带该 finding；quote 未随修复
    /// 消失的会注入下轮「顺带验证」防假修复。
    Resolve {
        #[command(subcommand)]
        action: ResolveCmd,
    },
    /// 幂等创建项目级 .redteam/ 骨架（.gitignore、rounds/、proj-next.db；配置在用户级 ~/.redteam/config.json，项目层无配置文件）。
    Init,
    /// 打印生效的测试命令（ship 循环第 2 步用）：--override > Cargo.toml→`cargo test` > package.json 有
    /// scripts.test→`npm test` > Makefile 有 test 目标→`make test` > go.mod→`go test ./...`；推不出 →
    /// stderr 提示、退出码 2（调用方转 AskUser）。
    TestCommand {
        /// 覆盖推断（与 `review --test-command` 同义）。
        #[arg(long = "override")]
        override_cmd: Option<String>,
    },
    /// 风险路由只读回放（M21b 批 A，零 provider / 零副作用）：跑阶段 0（成区 → 四类信号 → H0/R/档位）
    /// 打印 JSON——供 `eval/risk_replay.py compare` 对拍 Python v4 参考实现（θ 与权重改动必过的回归门，D11）。
    /// 读 `merge-base(base,head)...head` 与 head commit 内容（不依赖工作树），可回放任意历史 diff。
    Risk {
        /// diff 基线（分支名/commit）。
        #[arg(long)]
        base: String,
        /// 被评 head（缺省 HEAD）；回放历史 diff 时传当时的 head commit。
        #[arg(long)]
        head: Option<String>,
        /// 台账 db 覆盖（缺省读项目 `.redteam/proj-next.db`，不存在则不计台账信号；
        /// 对拍 Oracle B 可指向 dev 旧 `proj.db`）。
        #[arg(long)]
        dismissed: Option<String>,
        /// 时间守卫：只计严格早于该时刻（RFC3339）的台账记录（对拍复现 P0 标定条件）。
        #[arg(long)]
        before: Option<String>,
    },
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
        /// 快照：文件路径（同 title 规则）。
        #[arg(long)]
        file: Option<String>,
        /// 快照：问题代码原文（refute 锚定与 gc 锚失效检测依赖它，同 title 规则）。
        #[arg(long)]
        quote: Option<String>,
        /// 快照：严重度 low|medium|high|critical（同 title 规则）。
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
    Show {
        /// finding id（16 位 hex，来自评审报告；查无此卡报错退非零）。
        id: String,
    },
    /// 状态迁移（协议动作：re-upheld=复裁仍误报 / human-confirmed=人裁终局 /
    /// withdrawn=改判属实 / dismissed=回滚）。历史全留痕；不做状态机硬约束。
    SetStatus {
        /// finding id（16 位 hex，来自评审报告；查无此卡报错退非零）。
        id: String,
        /// 目标状态。
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
        /// 真落账（缺省 dry-run 只打印将被标 stale 的清单）。
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum ResolveCmd {
    /// 建卡（记一条「属实→已修复」销案判定）。快照四件套优先从 --from 报告按 id 自动抄写；
    /// 再销案（同 id 二次 add）缺省沿用卡上原理由。
    Add {
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
        /// 快照：文件路径（同 title 规则）。
        #[arg(long)]
        file: Option<String>,
        /// 快照：问题代码原文（增量轮假修复检测依赖它，同 title 规则）。
        #[arg(long)]
        quote: Option<String>,
        /// 快照：严重度 low|medium|high|critical（同 title 规则）。
        #[arg(long)]
        severity: Option<String>,
    },
    /// 单卡详情 + 全部修复历史（跨轮状态重建读口，D-QC1：ship.md 用 history 条数判断
    /// 复裁资格，不信会话记忆）。查无此卡报错退非零。
    Show {
        /// finding id（16 位 hex，来自评审报告；查无此卡报错退非零）。
        id: String,
    },
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum ModelCmd {
    /// 写 / 更新一个模型及其角色（flag 驱动）：`providers.<provider>.models.<id>.roles.<role>`，落
    /// ~/.redteam/config.json（REDTEAM_HOME 可覆盖）。provider / 模型 / 角色三层只写显式给出的字段，写前完整校验，
    /// 现有文件解析失败则拒写不覆写。
    Setup {
        /// provider 名（providers.<name>，如 deepseek、agc）。
        #[arg(long)]
        provider: String,
        /// 请求体里的 model id（providers.<name>.models.<id>，如 deepseek-v4-flash）。
        #[arg(long)]
        model: String,
        /// 角色，逗号分隔：scanner,reviewer,verifier（拥有该角色键的模型即活跃集，无候补）。
        #[arg(long)]
        role: String,
        /// 人读显示名（model list / 报告用）；身份串固定为 provider/id，不可配置。
        #[arg(long)]
        display_name: Option<String>,
        /// 协议类型：openai | anthropic（含 claude/gpt/gemini 等别名）。新建 provider 时写 provider 层（缺省按名 / base_url 推断）；
        /// provider 已存在时写模型层，不动共享的 provider 层。
        #[arg(long)]
        kind: Option<String>,
        /// API 端点 base URL（anthropic 网关须带 /v1）。
        #[arg(long)]
        base_url: Option<String>,
        /// key 环境变量名：新建 provider 时写 provider 层（缺省 `<PROVIDER>_API_KEY`）；provider 已存在时写模型层。
        #[arg(long)]
        api_key_env: Option<String>,
        /// 该 provider 同时在途请求上限（缺省继承 review.maxConcurrency）。
        #[arg(long)]
        max_concurrency: Option<u32>,
        /// API key（配合 --key-store 决定去处；不落 config.json）。
        #[arg(long)]
        key: Option<String>,
        /// key 去处：env(默认，打印 export) | secrets(写 secrets.json chmod 600)
        #[arg(long, default_value = "env", value_parser = ["env", "secrets"])]
        key_store: String,
        /// 与 --key-store secrets 并用：写模型级 models.<id> = key 而非 provider 级 apiKey。
        #[arg(long)]
        model_key: bool,
        /// context window（token，如 1000000）；缺省不写，预算按保守 128K 估；下限 32000。
        #[arg(long)]
        context_window: Option<u32>,
        /// 输出上限（max_tokens，≥ 1）；缺省 REDTEAM_MAX_TOKENS env / 32768。
        #[arg(long)]
        max_output_tokens: Option<u64>,
        /// 单轮超时秒数（模型层，≥ 1）；缺省 REDTEAM_PER_TURN_SECS > maxOutputTokens÷20+60 公式。
        #[arg(long)]
        per_turn_timeout_secs: Option<u64>,
        /// 思考型模型（thinking 计入 output 额度）：`--reasoning` 即 true，`--reasoning false` 显式关；不给不写。
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        reasoning: Option<bool>,
        /// 本次 --role 各角色的思考档位（角色属性）：off | low | medium | high。
        #[arg(long, value_parser = ["off", "low", "medium", "high"])]
        thinking_level: Option<String>,
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
/// 评审 / worker 路径：配置加载失败是硬错误（评审 S10：吞成空配置会把「解析失败」伪装成「未配置 scanner」，
/// 后台 worker 的 stderr 还是 /dev/null，job 终态错误就指错方向）。
fn load_config_or_exit() -> Config {
    match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

/// `--pack-only`：只装包不调 provider，不读 `secrets.json`（修补批 X-5）；配置错误同样硬失败。
fn load_config_no_secrets_or_exit() -> Config {
    match Config::load_no_secrets() {
        Ok((c, messages)) => {
            for m in &messages {
                eprintln!("{m}");
            }
            c
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn load_config_warned() -> Config {
    match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: 配置加载失败（{e}），回落空配置");
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
        // 五档：未终态是命令自身失败（参数/时序错误），退出码 1，不与降级 approve 的 2 混淆。
        return Err((format!("job {job_id} not done (status: {:?})", st.status), 1));
    }
    if format == "md" {
        // D3（表2-2，M18批2）：md 列缺失不再静默回落出 JSON（ship rN.md 依赖这条命令产出
        // 的是 markdown，静默回落=谎报格式）。md 缺但 json 在库 → 反序列化 ReviewReport
        // 现算 render_md；两者皆缺才报错退 1。
        if let Some(md) = store.read_result_md(job_id) {
            return Ok(md);
        }
        return match store.read_result(job_id) {
            Some(json) => match serde_json::from_str::<redteam::domain::ReviewReport>(&json) {
                Ok(mut report) => {
                    // md 快照缺失时的现算路径：从同库 cells 表补 per-cell 摘要（与终态
                    // 落盘时 attach 同源，旧作业 json 无此字段时补回）。
                    redteam::cli::attach_cells(store, job_id, &mut report);
                    Ok(render(&report, redteam::output::Format::Md))
                }
                Err(e) => Err((format!("result for {job_id} 解析失败，无法渲染 md: {e}"), 1)),
            },
            None if st.status == JobStatus::Done => Err((format!("result missing for {job_id}"), 1)),
            None => Err((format!("job {job_id} not done (status: {:?})", st.status), 1)),
        };
    }
    match store.read_result(job_id) {
        Some(raw) => Ok(raw),
        None if st.status == JobStatus::Done => {
            Err((format!("result missing for {job_id}"), 1))
        }
        None => Err((format!("job {job_id} not done (status: {:?})", st.status), 1)),
    }
}

/// 构造 ProviderRegistry（前台与 worker 共用）：只装 scanner 条目（route_key = `provider/id`，批 F-2 prd K1）——scanner 一律
/// scan 单发（`--tools` 对 scanner 无效，K3）；provider 在途门 = `maxConcurrency` 缺省继承 `review.max_concurrency`（D7 A5；唯一闸，无角色级 workers，D13）；
/// thinkingLevel 无映射的告警进 registry.warnings → RunMeta.warnings。reviewers / verifiers 在 M21b 接线（同一模型跨角色同名时键会
/// 冲突，届时 route_key 带角色前缀）。scanners 为空 → 配置错误；返回 Err(消息) 代替直接 process::exit，调用方决定如何处理。
fn build_registry(_args: &ReviewArgs, cfg: &Config, _cwd: &Path) -> Result<ProviderRegistry, String> {
    let roles = cfg.active_roles();
    if roles.scanners.is_empty() {
        return Err(redteam::cli::NO_SCANNER_MSG.to_string());
    }
    let mut registry = ProviderRegistry::new();
    for em in &roles.scanners {
        let key = cfg.resolve_key(&em.provider, &em.id).unwrap_or_default();
        let backend = match backend_from_config(em, key) {
            Ok(b) => b,
            Err(e) => {
                return Err(format!("{e}"));
            }
        };
        if let Some(w) = redteam::provider::backends::thinking_warning(em) {
            registry.push_warning(w);
        }
        // 单轮超时（design §12.3）：模型 perTurnTimeoutSecs > REDTEAM_PER_TURN_SECS > `max_tokens_effective ÷ 20 TPS + 60s`。
        let per_turn = std::time::Duration::from_secs(em.per_turn_secs());
        // usize → u32 饱和而非截断（评审修：误填 2^32 以上的值不会被截成 0 / 小数）。
        let cap = em.max_concurrency.unwrap_or(u32::try_from(cfg.review.max_concurrency).unwrap_or(u32::MAX));
        registry.insert_scan(em.name.clone(), &em.provider, cap, backend, per_turn, redteam::provider::DEFAULT_RETRY_BUDGET);
    }
    for em in &roles.reviewers {
        if registry.has_scan(&em.name) {
            continue;
        }
        let key = cfg.resolve_key(&em.provider, &em.id).unwrap_or_default();
        let backend = match backend_from_config(em, key) {
            Ok(b) => b,
            Err(e) => return Err(format!("{e}")),
        };
        if let Some(w) = redteam::provider::backends::thinking_warning(em) {
            registry.push_warning(w);
        }
        let per_turn = std::time::Duration::from_secs(em.per_turn_secs());
        let cap = em.max_concurrency.unwrap_or(u32::try_from(cfg.review.max_concurrency).unwrap_or(u32::MAX));
        registry.insert_scan(em.name.clone(), &em.provider, cap, backend, per_turn, redteam::provider::DEFAULT_RETRY_BUDGET);
    }
    Ok(registry)
}

#[tokio::main]
async fn main() {
    // T52：clap 解析错误默认退 2，与五档契约冲突（2=降级 approve、1=命令自身失败）——
    // try_parse 接管：--help/--version 属正常展示走 e.exit()（退 0），真解析错误打印后退 1。
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                e.exit(); // 内部按展示类退 0
            }
            let _ = e.print();
            std::process::exit(1);
        }
    };
    let code = match cli.cmd {
        Cmd::Review {
            base,
            scope,
            spec,
            focus,
            test_command,
            block_threshold,
            conf_floor,
            tools,
            background,
            format,
            resume,
            dismissed,
            no_dismissed,
            incremental_from,
            pack_only,
        } => {
            let cfg = if pack_only { load_config_no_secrets_or_exit() } else { load_config_or_exit() };
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

            let fmt = parse_format(&format);

            // R2.3：--resume <job_id> 补跑差集 cell，前台同步；忽略本次调用的其余 flag——
            // base/models 等从该作业落盘的 args.json 重建（run_resume 内部处理）。
            if let Some(job_id) = resume {
                let ws = cwd.clone();
                // ④级：JobStore 打不开是硬错误，直冒（main() 非 Result 返回，沿用本仓
                // match/exit(1) 惯用法做「?的等价物」，而非静默 let _=；五档下命令自身
                // 失败退出码为 1）。
                let store = match redteam::state::JobStore::open(&ws) {
                    Ok(s) => s,
                    Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
                };
                let orig_args_json = match store.read_args_raw(&job_id) {
                    Some(j) => j,
                    None => {
                        eprintln!("error: no args for job {job_id} (resume requires a job that ran via --background)");
                        std::process::exit(1);
                    }
                };
                let orig_args: ReviewArgs = match serde_json::from_str(&orig_args_json) {
                    Ok(a) => a,
                    Err(e) => { eprintln!("error: bad args for job {job_id}: {e}"); std::process::exit(1); }
                };
                let registry = match build_registry(&orig_args, &cfg, &cwd) {
                    Ok(x) => x,
                    Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
                };
                let (report, code) =
                    redteam::cli::run_resume(&job_id, &registry, &cfg, &cwd, &store).await;
                println!("{}", render(&report, fmt));
                std::process::exit(code);
            }

            // 值域校验（表2-5，M18批2）：conf_floor 越界（如 --conf-floor 5.0）会让
            // compute_verdict/refute 的 floor 门产生诡异行为（永久豁免/永久 blocking）——
            // 命令层直接拒收，退出码 1（命令自身失败，五档契约），不放行到评审流程里静默跑歪。
            if let Some(f) = conf_floor {
                if !(0.0..=1.0).contains(&f) {
                    eprintln!("error: --conf-floor must be within 0.0..=1.0, got {f}");
                    std::process::exit(1);
                }
            }

            // scope 缺省定案（r1 20260703 事故：--base + 默认 working-tree = base 被静默
            // 忽略 → 干净树空评审秒出 approve）：有 --base 自动 branch；显式矛盾组合报错。
            let scope = match (scope.as_deref(), base.is_some()) {
                (Some("working-tree"), true) => {
                    eprintln!("error: --base only takes effect with --scope branch (working-tree reviews uncommitted changes only); drop --base or use --scope branch");
                    std::process::exit(1);
                }
                (Some(s), _) => s.to_string(),
                (None, true) => "branch".to_string(),
                (None, false) => "working-tree".to_string(),
            };
            let args = ReviewArgs {
                base: base.clone(), scope, spec, focus,
                block_threshold, conf_floor, tools, background, format,
                resume: None, dismissed, no_dismissed, incremental_from, test_command, pack_only,
            };

            // 批 F-4（评审帽外项）：增量轮在 M21c 恢复前对前台 / 后台都在这里拒收——不能先建 job 再在 worker 里 bail 落成 Failed 作业。
            if args.incremental_from.is_some() {
                eprintln!("error: --incremental-from（增量轮）在 M21c 疑点台账回流后恢复；M21a scan-only 请跑全量 `review --base <base>`");
                std::process::exit(1);
            }

            // --pack-only（M21a 批 C）：装包落盘即返回，不建 registry、不派发。
            if pack_only {
                match redteam::cli::run_pack_only(&args, &cfg, &cwd) {
                    Ok(report) => {
                        print!("{}", redteam::cli::render_pack_only(&report));
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                }
            }

            // 后台：写 pending state + args 快照（proj.db）→ spawn detached worker → 打印 job-id 退出。
            if background {
                let ws = cwd.clone();
                let job_id = redteam::cli::gen_job_id(&cwd);
                // ④级：store 打不开是硬错误，直冒。
                let store = match redteam::state::JobStore::open(&ws) {
                    Ok(s) => s,
                    Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
                };
                let args_json = serde_json::to_string(&args).unwrap_or_default();
                if store.write_args_raw(&job_id, &args_json).is_err() {
                    eprintln!("error: failed to write job args");
                    std::process::exit(1);
                }
                // ④级：起手建 job 是 gating 写——失败则整条后台派发都不成立，直接报错退出。
                if let Err(e) = store.create_job(&job_id, now_secs()) {
                    eprintln!("error: failed to create job: {e}");
                    std::process::exit(1);
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
                        // dogfood F6：spawn 失败若不落终态，job 永悬 Pending 且无 pid——
                        // reap_if_dead 对无 pid 的 Pending 不收尸（无从判活），僵尸卡永久
                        // 累积。守卫写 Failed+exit 1（同 worker 配置失败分支范例）；拒写
                        // 说明期间已被 cancel 等落了终态，保首个终态不覆盖。
                        match store.update_if_active(&job_id, |st| {
                            st.status = redteam::state::JobStatus::Failed;
                            st.error = Some(format!("failed to spawn worker: {e}"));
                            st.exit_code = Some(1);
                        }) {
                            Ok(false) => eprintln!("warn: job {job_id} 终态写被终态封盘拒绝"),
                            Err(e2) => eprintln!("warn: job {job_id} 终态写失败: {e2}"),
                            Ok(true) => {}
                        }
                        std::process::exit(1);
                    }
                }
            }

            // 前台路径：构造 registry 后调 run_review_dispatch。
            let registry = match build_registry(&args, &cfg, &cwd) {
                Ok(x) => x,
                Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
            };

            // 前台直调无 job 上下文，sink=None（与 run_review/run_code_review 直调路径一致）。
            let result = redteam::cli::run_review_dispatch(&args, &registry, &cfg, &cwd, None).await;

            match result {
                Ok((report, code)) => {
                    println!("{}", render(&report, fmt));
                    code
                }
                Err(e) => {
                    // dispatch 基础设施错误（非评审结论）→ 命令自身失败，五档 1。
                    eprintln!("error: {e}");
                    1
                }
            }
        }
        Cmd::Worker { job_id } => {
            let cfg = load_config_or_exit();
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = cwd.clone();
            // ④级：worker 主进程打不开 store 是硬错误，直接报错退出。
            // M17-c Task 3：单连接贯穿 worker 全生命周期——Arc 化后 run_job/panic hook/收尾
            // 共享同一个 JobStore（同一个底层 rusqlite::Connection），不再各自现开连接。
            let store = match redteam::state::JobStore::open(&ws) {
                Ok(s) => std::sync::Arc::new(s),
                Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
            };
            // 进程级 panic 兜底（E12-Q4，真·顶层）：worker 评审体若 panic，落 Failed 终态再正常
            // 展开——否则状态永停 Running（惰性收尸只治「进程已死」，panic 与退出间有窗口）。
            // c-T3 评审 Critical：此前曾把 hook 改为与主流程共享 Arc<JobStore>（同一把
            // Mutex<ProjDb>）——若 panic 恰发生在 update_if_active 的 mutate 闭包执行期
            // （MutexGuard 存活、未 Drop、未中毒——poison 要等 guard Drop 时才置位，而 hook
            // 先于 unwind 跑到），hook 同线程再 lock() 同一把锁就是不可重入永久自死锁，进程
            // 挂起、default_hook 永不执行。现改回现开一个全新连接（全新 Mutex）——结构性免疫
            // 该重入场景，与主流程的 Arc 共享连接无关（run_job/JobCellSink 的单连接贯穿收益不动）。
            {
                let ws_h = ws.clone();
                let jid_h = job_id.clone();
                let default_hook = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    let msg = info
                        .payload()
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| info.payload().downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown".to_string());
                    match redteam::state::JobStore::open(&ws_h) {
                        Ok(fresh) => redteam::cli::mark_failed_panic(&fresh, &jid_h, &msg),
                        Err(e) => eprintln!(
                            "warn: panic hook 无法打开 proj.db，job {jid_h} 状态未落 Failed（需人工处理）: {e}"
                        ),
                    }
                    default_hook(info);
                }));
            }
            // e2e 回归资产，永久保留，不进 --help/prompt 面
            // 测试钩子：REDTEAM_TEST_PANIC=1 时在 fan-out 前触发 panic，验证 panic hook 全链路
            if std::env::var("REDTEAM_TEST_PANIC").is_ok() {
                panic!("REDTEAM_TEST_PANIC");
            }
            let args_json = match store.read_args_raw(&job_id) {
                Some(j) => j,
                None => {
                    eprintln!("error: no args for job {job_id}");
                    std::process::exit(1);
                }
            };
            let args: ReviewArgs = match serde_json::from_str(&args_json) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("error: bad args: {e}");
                    std::process::exit(1);
                }
            };
            // 建 registry（复用 build_registry：resolve_models + 构造 ProviderRegistry）
            let registry = match build_registry(&args, &cfg, &cwd) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("error: {e}");
                    // 写 Failed 终态，让 status 命令能看到错误原因。走守卫：裸覆写
                    // 会盖掉 pre-worker 阶段已落定的 Cancelled 终态（首个终态封盘）。
                    // 五档：配置/provider 构造失败是命令自身失败，exit_code 落 1（不与
                    // 降级 approve 的 2 混淆）。
                    match store.update_if_active(&job_id, |st| {
                        st.status = redteam::state::JobStatus::Failed;
                        st.error = Some(e);
                        st.exit_code = Some(1);
                    }) {
                        Ok(false) => eprintln!("warn: job {job_id} 终态写被终态封盘拒绝"),
                        Err(e) => eprintln!("warn: job {job_id} 终态写失败: {e}"),
                        Ok(true) => {}
                    }
                    std::process::exit(1);
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
                Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
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
                None => { eprintln!("no such job: {}", job_id.unwrap_or_default()); 1 }
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
                Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
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
                None => { eprintln!("no such job: {job_id}"); 1 }
            }
        }
        Cmd::Cancel { job_id } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            // ④级：store 打不开是硬错误，直冒。
            let store = match redteam::state::JobStore::open(&ws) {
                Ok(s) => s,
                Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
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
                            1
                        }
                        Err(e) => {
                            eprintln!("error: job {job_id} cancel 写失败（非拒写，db 层异常）: {e}");
                            1
                        }
                    }
                }
                Some(s) => { eprintln!("job {job_id} not cancellable (status: {:?})", s.status); 1 }
                None => { eprintln!("no such job: {job_id}"); 1 }
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
            ModelCmd::Setup { provider, model, role, display_name, kind, base_url, api_key_env, max_concurrency, key, key_store, model_key, context_window, max_output_tokens, per_turn_timeout_secs, reasoning, thinking_level } => {
                let mut roles: Vec<redteam::config::Role> = Vec::new();
                for r in role.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    match redteam::config::Role::parse(r) {
                        Some(x) if !roles.contains(&x) => roles.push(x),
                        Some(_) => {}
                        None => {
                            eprintln!("error: --role 取 scanner | reviewer | verifier（逗号分隔，如 scanner,reviewer），收到 {r:?}");
                            std::process::exit(1);
                        }
                    }
                }
                if roles.is_empty() {
                    eprintln!("error: --role 至少给一个角色：scanner | reviewer | verifier");
                    std::process::exit(1);
                }
                let thinking_level = match thinking_level.as_deref() {
                    Some(s) => match redteam::config::ThinkingLevel::parse(s) {
                        Some(t) => Some(t),
                        None => { eprintln!("error: --thinking-level 取 off | low | medium | high"); std::process::exit(1); }
                    },
                    None => None,
                };
                // flag 组合校验全部在写盘前（评审 #2）：--model-key 只配 secrets；secrets 必须带 --key
                if model_key && key_store != "secrets" {
                    eprintln!("error: --model-key 只与 --key-store secrets 并用（模型级 key 写 secrets.json providers.<provider>.models.<id>）");
                    std::process::exit(1);
                }
                if key_store == "secrets" && key.is_none() {
                    eprintln!("error: --key-store secrets requires --key");
                    std::process::exit(1);
                }
                let kind_if_new = redteam::model::derive_kind(&provider, kind.as_deref(), base_url.as_deref());
                // 写入前守卫：kind/base_url 不合法则提示并拒绝写入，不污染配置。kind 检查用原始 --kind 字符串
                // （若显式给出，哪怕非法）——`derive_kind` 内非法显式值已落回推断链、不会原样带出来。
                let kind_for_validate = kind.as_deref().unwrap_or_else(|| kind_if_new.as_str());
                if let Err(e) = redteam::model::validate_setup(kind_for_validate, base_url.as_deref()) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
                // 写前预检（评审 #1）：key 走 env 时该模型必须能落到某个 apiKeyEnv——新 provider 由推断补齐；已存在 provider
                // 须 provider 层 / 该模型层已有或本次 --api-key-env 给出，否则 --key 会指向配置里不存在的变量、静默失效。
                let existing = redteam::config::Config::load_with_messages().map(|(c, _)| c).unwrap_or_default();
                let env_configured = api_key_env.is_some()
                    || match existing.providers.get(&provider) {
                        None => true,
                        Some(p) => p.api_key_env.is_some() || p.models.get(&model).is_some_and(|m| m.api_key_env.is_some()),
                    };
                if key_store == "env" && key.is_some() && !env_configured {
                    eprintln!(
                        "error: provider {provider} 已存在且未配置 apiKeyEnv（provider 层与该模型层皆无）：加 --api-key-env <VAR>（写入模型层）或改用 --key-store secrets [--model-key]"
                    );
                    std::process::exit(1);
                }
                // design v9 §12.1 形状：providers.<provider>.models.<id>.roles.<role>；三层只写显式给出的字段，
                // 已存在的 provider 不被 --kind / --api-key-env 改写（这两项落模型层，评审 #3 / #5）；写前完整校验（#4）。
                let patch = redteam::config::SetupPatch {
                    provider: provider.clone(),
                    model_id: model.clone(),
                    roles: roles.clone(),
                    display_name: display_name.clone(),
                    kind: kind.as_deref().and_then(redteam::config::ProviderKind::parse),
                    base_url: base_url.clone(),
                    api_key_env: api_key_env.clone(),
                    max_concurrency,
                    context_window,
                    max_output_tokens,
                    per_turn_timeout_secs,
                    reasoning,
                    thinking_level,
                    kind_if_new,
                    api_key_env_if_new: Some(redteam::model::derive_api_key_env(&provider)),
                };
                let path = match redteam::config::write_setup(&patch) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("error: failed to write config: {e}");
                        if e.to_string().contains("缺 kind") {
                            eprintln!("提示：加 --kind openai|anthropic（provider 已存在时写入该模型层）");
                        }
                        std::process::exit(1);
                    }
                };
                // 落盘后重读：合并后的 provider / 模型层才是文案与探测的真相源。
                let cfg = load_config_warned();
                let Some(pc) = cfg.providers.get(&provider) else {
                    eprintln!("error: provider missing from just-written config (unexpected)");
                    std::process::exit(1);
                };
                let env_name = pc.models.get(&model).and_then(|m| m.api_key_env.clone()).or_else(|| pc.api_key_env.clone());
                // key 去处：env(默认，打印 export) / secrets(写 secrets.json；--model-key 写模型级)
                match key_store.as_str() {
                    "secrets" => {
                        let mid = if model_key { Some(model.as_str()) } else { None };
                        match redteam::config::secrets::write_secret(&provider, mid, key.as_deref().unwrap_or_default()) {
                            Ok(p) => println!("wrote key to {} (chmod 600)", p.display()),
                            Err(e) => { eprintln!("error: failed to write secret: {e}"); std::process::exit(1); }
                        }
                    }
                    _ => match (&env_name, &key) {
                        // env 默认：主配置只记 apiKeyEnv，提示用户设 env。
                        (Some(v), Some(k)) => println!("add to your shell profile:\n  export {v}={k}"),
                        (Some(v), None) => println!("set the API key in your environment:\n  export {v}=<your-key>"),
                        (None, _) => println!("notice: providers.{provider} 未配置 apiKeyEnv；key 可放 secrets.json（--key-store secrets [--model-key]）或补 --api-key-env"),
                    },
                }
                let em = cfg.effective(&provider, &model, roles[0]);
                let probe_kind = em.as_ref().map(|e| e.kind).unwrap_or_else(|| provider_probe_kind(pc));
                let shown = display_name.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
                println!(
                    "configured {provider}/{model}{shown} roles=[{}] (kind={}, apiKeyEnv={}) → {}",
                    roles.iter().map(|r| r.as_str()).collect::<Vec<_>>().join(","),
                    probe_kind.as_str(),
                    env_name.as_deref().unwrap_or("-"),
                    path.display()
                );
                // 落盘后自动 probe 刚配置的这个模型所在 provider（只探它、不是全部——复用 probe.rs 按 kind 认证的
                // list_models；key 走该模型的四级解析链 resolve_key(provider, id)，#5）。probe 失败只警告、不回滚
                // 已落盘的配置：离线环境先 setup、后续再联网核实是合理场景，不该因一次探测失败被拒绝写入
                // （T26，M18 decisions Q6 明确否掉「probe 失败拒绝落盘」的旧 TODO 框架）。展示文案抽成纯函数
                // format_probe_outcome 以便单测覆盖。
                let prober = redteam::model::probe::HttpProber::new();
                let probe_key = cfg.resolve_key(&provider, &model);
                let probe_result = prober
                    .list_models(&redteam::model::effective_base(pc.base_url.as_deref(), probe_kind), probe_key.as_deref(), probe_kind)
                    .await
                    .map_err(|e| e.to_string());
                println!("{}", format_probe_outcome(&provider, &model, &probe_result));
                0
            }
            ModelCmd::List { remote } => {
                let cfg = load_config_warned();
                let prober = redteam::model::probe::HttpProber::new();
                match remote {
                    Some(prov) => {
                        let pc = match cfg.providers.get(&prov) {
                            Some(p) => p,
                            None => { eprintln!("no such provider: {prov}"); std::process::exit(1); }
                        };
                        let kind = provider_probe_kind(pc);
                        let base = redteam::model::effective_base(pc.base_url.as_deref(), kind);
                        let key = cfg.resolve_provider_key(&prov);
                        match prober.list_models(&base, key.as_deref(), kind).await {
                            Ok(models) => {
                                for m in models { println!("{m}"); }
                                0
                            }
                            Err(e) => { eprintln!("error querying {base}/models: {e}"); 1 }
                        }
                    }
                    None => {
                        let avs = redteam::model::check_all(&cfg, &prober).await;
                        let path = redteam::config::config_path()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|e| format!("(无法定位：{e})"));
                        println!("{}", redteam::model::render_model_list(&cfg, &avs, &path));
                        0
                    }
                }
            }
        },
        Cmd::Dismiss { action } => run_dismiss(action),
        Cmd::Resolve { action } => run_resolve(action),
        Cmd::Init => {
            match redteam::project::init_project(std::path::Path::new(".")) {
                Ok(c) if c.is_empty() => println!(".redteam/ already complete"),
                Ok(c) => c.iter().for_each(|p| println!("created {p}")),
                Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
            }
            0
        }
        Cmd::TestCommand { override_cmd } => {
            let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            match override_cmd.or_else(|| redteam::project::infer_test_command(&root)) {
                Some(c) => { println!("{c}"); 0 }
                None => {
                    eprintln!("test-command: 推断不出（无 Cargo.toml / package.json scripts.test / Makefile test 目标 / go.mod）；用 --override <cmd> 或 review --test-command 指定");
                    2
                }
            }
        }
        Cmd::Risk { base, head, dismissed, before } => run_risk(&base, head.as_deref(), dismissed.as_deref(), before.as_deref()),
    };
    std::process::exit(code);
}

/// `risk` 子命令（M21b 批 A）：阶段 0 只读回放，JSON 输出供 `eval/risk_replay.py compare` 对拍。
/// 零 provider / 零写库；配置加载失败回落空配置（`budgetCells` 信息位，不影响 R 与 H0）。
fn run_risk(base: &str, head: Option<&str>, dismissed: Option<&str>, before: Option<&str>) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let head = head.map(str::to_string).unwrap_or_else(|| {
        redteam::evidence::git::head_sha_at(&cwd).unwrap_or_else(|_| "HEAD".into())
    });
    let changes = match redteam::evidence::git::changed_files_at(&cwd, base, &head) {
        Ok(c) => c,
        Err(e) => { eprintln!("error: {e}"); return 1; }
    };
    let cfg = load_config_warned();
    // 与 `cli::stage0_risk` 同口径：只接受存在的文件路径（csv/目录/错路径一律不加台账，
    // 且只读打开——不存在的路径不会被创建，见 `risk::GitEnv`）。
    let ledger = match dismissed {
        Some(p) if std::path::Path::new(p).is_file() => Some(std::path::PathBuf::from(p)),
        None => {
            let p = cwd.join(redteam::project::PROJECT_DIR).join(redteam::db::DB_FILE);
            p.is_file().then_some(p)
        }
        _ => None,
    };
    let route = redteam::risk::replay_from_git(
        &changes, &cwd, base, &head, ledger.as_deref(), before, cfg.review.reviewers.max_cells,
    );
    let out = serde_json::json!({
        "schema": "risk-v1",
        "repo": cwd.to_string_lossy(),
        "base": base,
        "head": head,
        "theta": redteam::risk::THETA,
        "r": route.r,
        "tier": route.tier,
        "budget_cells": route.budget_cells_hint,
        "hotspots": route.hotspots,
        "zones": route.zones,
        "code_lines": route.code_lines,
    });
    match serde_json::to_string_pretty(&out) {
        Ok(s) => { println!("{s}"); 0 }
        Err(e) => { eprintln!("error: {e}"); 1 }
    }
}

/// provider 级探测（`model list --remote` / setup 后探测）用的协议：provider 层 kind > 任一模型的 kind > openai-compatible。
fn provider_probe_kind(pc: &redteam::config::ProviderConfig) -> redteam::config::ProviderKind {
    pc.kind
        .or_else(|| pc.models.values().find_map(|m| m.kind))
        .unwrap_or(redteam::config::ProviderKind::OpenaiCompatible)
}

/// T26：`model setup` 落盘后自动 probe 该 provider 的结果文案——纯函数，网络调用
/// 已在调用方完成，这里只管把 `Result<Vec<String>, String>` 渲成人读行，便于单测覆盖
/// 「probe 失败仍要给出警告 + 复核提示」这条语义，不必真打网络。
fn format_probe_outcome(provider: &str, model: &str, probe: &Result<Vec<String>, String>) -> String {
    match probe {
        Ok(ids) if ids.is_empty() || ids.iter().any(|m| m == model) => {
            format!("{provider}  ✓ available")
        }
        Ok(_) => format!("{provider}  ⚠ reachable, model not in /models"),
        Err(e) => format!(
            "{provider}  ⚠ probe failed（{e}）——配置已保存，联网恢复后可用 `redteam model check` 复核"
        ),
    }
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
        Err(e) => { eprintln!("error: {e}"); return 1; }
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
            // dogfood F5 同步统一：history 并入 JSON 数组字段（每项 {status, round, reason, at}），
            // 输出整体为合法 JSON 单文档，供 jq/脚本消费；此前 JSON 后拼散文行两不靠。
            let mut v = serde_json::to_value(&card)?;
            let history: Vec<serde_json::Value> = db
                .dismiss_history(&id)?
                .into_iter()
                .map(|(status, round, reason, at)| {
                    serde_json::json!({ "status": status, "round": round, "reason": reason, "at": at })
                })
                .collect();
            v["history"] = serde_json::Value::Array(history);
            println!("{}", serde_json::to_string_pretty(&v)?);
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
        Err(e) => { eprintln!("error: {e}"); 1 }
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

/// resolve 子命令族（M13 决议 3 + M17-d D-QC1）：dismiss 对称的销案入账 + 跨轮重建读口。
fn run_resolve(action: ResolveCmd) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut db = match redteam::db::ProjDb::open(&cwd) {
        Ok(d) => d,
        Err(e) => { eprintln!("error: {e}"); return 1; }
    };
    let r = match action {
        ResolveCmd::Add { id, from, round, reason, title, file, quote, severity } => {
            resolve_add(&mut db, id, from, round, reason, title, file, quote, severity)
        }
        ResolveCmd::Show { id } => resolve_show(&db, &id).map(|out| println!("{out}")),
    };
    match r {
        Ok(()) => 0,
        Err(e) => { eprintln!("error: {e}"); 1 }
    }
}

/// resolve add：销案入账（与更名前的平铺 `resolve` 逐字等价，纯挪壳）。
#[allow(clippy::too_many_arguments)]
fn resolve_add(
    db: &mut redteam::db::ProjDb,
    id: String, from: Option<String>, round: String, reason: String,
    title: Option<String>, file: Option<String>, quote: Option<String>, severity: Option<String>,
) -> anyhow::Result<()> {
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
}

/// resolve show：纯 JSON 单文档——卡片对象内嵌 `history` 数组（每项 {round, reason}；
/// resolve_history 无 status/at 列）。查无此卡报错（调用方映射非零退出）。
/// dogfood F5：此前 JSON 后拼散文 history 行，整体非法 JSON，jq/脚本（ship 跨轮重建
/// 引用此命令）无法消费——改为 history 并入 JSON 字段，dismiss show 同步统一。
fn resolve_show(db: &redteam::db::ProjDb, id: &str) -> anyhow::Result<String> {
    let card = db.resolve_get(id)?.ok_or_else(|| anyhow::anyhow!("no card with id {id}"))?;
    let mut v = serde_json::to_value(&card)?;
    let history: Vec<serde_json::Value> = db
        .resolve_history(id)?
        .into_iter()
        .map(|(round, reason)| serde_json::json!({ "round": round, "reason": reason }))
        .collect();
    v["history"] = serde_json::Value::Array(history);
    Ok(serde_json::to_string_pretty(&v)?)
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
        None => { eprintln!("--cells requires a job id"); 1 }
        Some(id) => {
            if store.read_state(id).is_none() {
                eprintln!("no such job: {id}");
                return 1;
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

    /// T26：probe 成功且 model 在列表中 → available。
    #[test]
    fn format_probe_outcome_success_lists_model() {
        let out = format_probe_outcome("alpha", "m1", &Ok(vec!["m1".into(), "m2".into()]));
        assert_eq!(out, "alpha  ✓ available");
    }

    /// T26：probe 成功但配置的 model 不在返回列表里 → 软提示，非报错。
    #[test]
    fn format_probe_outcome_success_model_missing() {
        let out = format_probe_outcome("alpha", "m1", &Ok(vec!["other".into()]));
        assert_eq!(out, "alpha  ⚠ reachable, model not in /models");
    }

    /// T26 核心：probe 失败只警告 + 给复核提示，措辞里不出现「拒绝/回滚/失败退出」类字眼——
    /// 已落盘的配置不受 probe 结果影响（离线 setup 场景）。
    #[test]
    fn format_probe_outcome_failure_warns_without_rollback_language() {
        let out = format_probe_outcome("alpha", "m1", &Err("connection refused".into()));
        assert!(out.contains("⚠ probe failed"), "应是警告而非错误: {out}");
        assert!(out.contains("connection refused"), "应带上原始错误: {out}");
        assert!(out.contains("redteam model check"), "应提示复核命令: {out}");
        assert!(!out.contains("回滚") && !out.contains("rollback"), "不应暗示回滚: {out}");
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

    /// M21a 批 C（R-C4.3）：--pack-only 与 --background / --resume / --incremental-from 互斥。
    #[test]
    fn pack_only_conflicts_with_background_resume_incremental() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["redteam", "review", "--pack-only"]).is_ok());
        assert!(Cli::try_parse_from(["redteam", "review", "--pack-only", "--base", "dev", "--focus", "x"]).is_ok());
        for extra in [["--background", ""], ["--resume", "abc"], ["--incremental-from", "abc"]] {
            let mut argv = vec!["redteam", "review", "--pack-only", extra[0]];
            if !extra[1].is_empty() {
                argv.push(extra[1]);
            }
            assert!(Cli::try_parse_from(&argv).is_err(), "{argv:?} 应冲突");
        }
    }

    /// M21b 批 A：`risk` 子命令解析——`--base` 必填，可选 `--head/--dismissed/--before`。
    #[test]
    fn risk_subcommand_parses_and_requires_base() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["redteam", "risk", "--base", "main"]).is_ok());
        assert!(Cli::try_parse_from([
            "redteam", "risk", "--base", "a7c827b70", "--head", "215afd77b",
            "--dismissed", "/tmp/proj.db", "--before", "2026-07-28T03:45:57Z",
        ])
        .is_ok());
        assert!(Cli::try_parse_from(["redteam", "risk"]).is_err(), "--base 必填");
    }

    /// C1：无 provider 配置的 Config + ReviewArgs → build_registry 返回 Err。
    #[test]
    fn build_registry_no_provider_returns_err() {
        use redteam::cli::ReviewArgs;
        use redteam::config::Config;

        // 空配置：无 scanner 角色 → 角色集为空 → Err
        let cfg = Config::default();
        let cwd = std::path::Path::new(".");
        let args = ReviewArgs { pack_only: false,
            base: None,
            scope: "working-tree".into(),
            spec: vec![],
            focus: None,
            block_threshold: None,
            conf_floor: None,
            background: false,
            format: "json".into(),
            resume: None,
            dismissed: None,
            no_dismissed: false,
            incremental_from: None,
            tools: None,
            test_command: None,
        };
        let result = build_registry(&args, &cfg, cwd);
        assert!(result.is_err(), "无 scanner 时 build_registry 应返回 Err");
        let msg = result.err().unwrap();
        assert!(
            msg.contains("scanner"),
            "错误消息应指向 scanner 角色配置，实际: {msg}"
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
        st.exit_code = Some(3); // 五档：verdict=incomplete 对应的落盘 exit_code（此测不读该字段，仅保持语义一致）
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
        assert_eq!(out.unwrap_err().1, 1); // 五档：命令自身失败（结果缺失）→ 1
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
        assert_eq!(code, 1); // 五档：命令自身失败（未终态）→ 1
    }

    // ---- resolve add|show 子命令空间化（M17-d Task 1，D-QC1）----

    /// ①等价锚：resolve_add 挪壳后行为与更名前的平铺 `resolve` 逐字一致
    /// （id 校验 + 快照字段落卡 + reason/round 直传，不经 pick）。
    #[test]
    fn resolve_add_records_card_with_explicit_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = redteam::db::ProjDb::open_at(&dir.path().join("proj.db")).unwrap();
        let id = "a1b2c3d4e5f60718".to_string();
        resolve_add(
            &mut db, id.clone(), None, "r2".into(), "修好了".into(),
            Some("标题".into()), Some("src/x.rs".into()), Some("let x = 1;".into()), Some("high".into()),
        ).unwrap();
        let card = db.resolve_get(&id).unwrap().unwrap();
        assert_eq!(card.title, "标题");
        assert_eq!(card.file, "src/x.rs");
        assert_eq!(card.quote, "let x = 1;");
        assert_eq!(card.severity, "high");
        assert_eq!(card.round, "r2");
        assert_eq!(card.reason, "修好了");
    }

    /// 非 16-hex id 逐字拒绝（校验搬迁未变形）。
    #[test]
    fn resolve_add_rejects_non_hex16_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = redteam::db::ProjDb::open_at(&dir.path().join("proj.db")).unwrap();
        let err = resolve_add(&mut db, "short".into(), None, "".into(), "".into(), None, None, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("16-hex"), "{err}");
    }

    /// 无 --from 且快照字段未全给 → 明确报错（pick 逻辑逐字保留）。
    #[test]
    fn resolve_add_missing_snapshot_field_without_from_errs() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = redteam::db::ProjDb::open_at(&dir.path().join("proj.db")).unwrap();
        let err = resolve_add(
            &mut db, "a1b2c3d4e5f60718".into(), None, "".into(), "".into(), None, None, None, None,
        ).unwrap_err();
        assert!(err.to_string().contains("missing snapshot field"), "{err}");
    }

    /// ②show 输出断言：卡四件套 + reason/round + 两条 history 逐条都在；且（dogfood F5）
    /// 整体必须是单份合法 JSON、history 为对象内数组字段——jq/脚本可直接消费。
    #[test]
    fn resolve_show_renders_card_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = redteam::db::ProjDb::open_at(&dir.path().join("proj.db")).unwrap();
        let id = "a1b2c3d4e5f60718".to_string();
        resolve_add(
            &mut db, id.clone(), None, "r1".into(), "第一次修".into(),
            Some("标题".into()), Some("src/x.rs".into()), Some("let x = 1;".into()), Some("high".into()),
        ).unwrap();
        resolve_add(
            &mut db, id.clone(), None, "r3".into(), "第二次修".into(),
            Some("标题".into()), Some("src/x.rs".into()), Some("let x = 1;".into()), Some("high".into()),
        ).unwrap();
        let out = resolve_show(&db, &id).unwrap();
        // dogfood F5 锚：输出整体可被 serde_json 解析（此前 JSON 后拼散文行会在此失败）。
        let v: serde_json::Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("resolve show 输出必须是合法 JSON: {e}\n{out}"));
        assert!(out.contains("标题") && out.contains("src/x.rs") && out.contains("let x = 1;") && out.contains("high"),
            "缺卡四件套: {out}");
        let hist = v["history"].as_array().expect("history 应为 JSON 数组字段");
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0]["round"], "r1");
        assert_eq!(hist[0]["reason"], "第一次修");
        assert_eq!(hist[1]["round"], "r3");
        assert_eq!(hist[1]["reason"], "第二次修");
    }

    /// ③查无此卡 → resolve_show 返回 Err（run_resolve 外层映射为非零退出 + eprintln）。
    #[test]
    fn resolve_show_missing_card_errs_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let db = redteam::db::ProjDb::open_at(&dir.path().join("proj.db")).unwrap();
        let err = resolve_show(&db, "a1b2c3d4e5f60718").unwrap_err();
        assert!(err.to_string().contains("a1b2c3d4e5f60718"), "{err}");
    }
}
