//! 尾段公共件（M21a 批 F-4 硬切后）：退出码映射与 model id 映射。旧 finding 尾段（anchor → dedup → refute → verdict →
//! `assemble_report` / `TailCtx` / `CarryCtx` / `mark_dismissed`）随旧 fan-out 删除；M21b reviewer / refuter 尾段另起。

use crate::domain::Verdict;

/// 退出码映射（五档契约·2026-07-27 定稿；批 F-3 加 scan-only）：reject→10、`Verdict::Incomplete`（部分结果 / scanner cell
/// 失败，design §8.4）→3、approve 但对照集不完整（failed_models 非空）→2、干净 approve→0、scan-only 完成→0。命令自身失败
/// （参数/IO/配置错误）不经此函数，走 main.rs 的 exit(1)。
pub fn exit_code(verdict: Verdict, failed_models_nonempty: bool) -> i32 {
    match (verdict, failed_models_nonempty) {
        (Verdict::Reject, _) => 10,
        (Verdict::Approve, true) => 2,
        (Verdict::Approve, false) => 0,
        (Verdict::Incomplete, _) => 3,
        (Verdict::ScanOnly, _) => 0,
    }
}

/// 模型身份串 name → 请求 id 映射（人读报告显示 name，rN.md 模板 2026-07-03）。
pub(crate) fn model_ids_for(names: &[String], roles: &crate::config::RoleSets) -> Vec<String> {
    names
        .iter()
        .map(|n| roles.id_of(n).map(str::to_string).unwrap_or_else(|| n.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_maps_all_cases() {
        assert_eq!(exit_code(Verdict::Reject, false), 10);
        assert_eq!(exit_code(Verdict::Reject, true), 10);
        assert_eq!(exit_code(Verdict::Approve, true), 2);
        assert_eq!(exit_code(Verdict::Approve, false), 0);
        assert_eq!(exit_code(Verdict::Incomplete, false), 3);
        assert_eq!(exit_code(Verdict::Incomplete, true), 3);
        assert_eq!(exit_code(Verdict::ScanOnly, false), 0);
        assert_eq!(exit_code(Verdict::ScanOnly, true), 0);
    }
}
