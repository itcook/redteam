use crate::domain::{Mode, Severity};

pub struct Lens {
    pub id: &'static str,
    pub title: &'static str,
    pub focus: &'static str,        // 注入 prompt 的"本 lens 攻击面"
    pub max_severity: Option<Severity>,
}

static CODE: &[Lens] = &[
    Lens { id: "L1", title: "安全与边界", focus: "鉴权/越权/租户隔离/信任边界/输入校验", max_severity: None },
    Lens { id: "L2", title: "数据与状态", focus: "数据丢失/损坏/重复/不可逆状态/回滚安全/幂等", max_severity: None },
    Lens { id: "L3", title: "并发与失败", focus: "竞态/顺序/重入/重试/部分失败/超时/降级依赖/空态null", max_severity: None },
    Lens { id: "L4", title: "演进与可观测", focus: "版本漂移/schema 漂移/迁移风险/兼容回归/可观测盲区", max_severity: None },
    Lens { id: "L0", title: "spec 一致性", focus: "实现是否偏离所给 spec 的设计意图（drift）", max_severity: None },
    Lens { id: "L5", title: "规范遵循", focus: "编码约定/命名/结构/团队标准；非强约束时仅 low/medium", max_severity: Some(Severity::Medium) },
];

static DOC: &[Lens] = &[
    Lens { id: "D1", title: "内部一致性", focus: "段落矛盾、互斥需求、自相冲突约束", max_severity: None },
    Lens { id: "D2", title: "完备性与歧义", focus: "缺失分支/未定义错误处理/欠规约/会坑实现的歧义", max_severity: None },
    Lens { id: "D3", title: "隐藏假设与风险", focus: "未言明假设/未验证依赖；设计层是否回应安全/数据/失败/迁移", max_severity: None },
    Lens { id: "D4", title: "可测性与成功标准", focus: "成功标准可否度量、验收条件是否定义", max_severity: None },
    Lens { id: "D5", title: "规范遵循", focus: "是否遵循团队文档规范/模板；非强约束时仅 low/medium", max_severity: Some(Severity::Medium) },
];

pub fn all_for(mode: Mode) -> Vec<&'static Lens> {
    match mode { Mode::Code => CODE.iter().collect(), Mode::Doc => DOC.iter().collect() }
}

pub fn by_id(id: &str) -> Option<&'static Lens> {
    CODE.iter().chain(DOC.iter()).find(|l| l.id == id)
}

pub fn select(mode: Mode, ids: &[String]) -> anyhow::Result<Vec<&'static Lens>> {
    if ids.is_empty() {
        return Ok(all_for(mode));
    }
    let mut out = Vec::new();
    for id in ids {
        let l = by_id(id).ok_or_else(|| anyhow::anyhow!("未知 lens: {id}"))?;
        out.push(l);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Mode;
    #[test]
    fn code_has_six_default_lenses_incl_l0_l5() {
        let ids: Vec<_> = all_for(Mode::Code).iter().map(|l| l.id).collect();
        assert!(ids.contains(&"L1") && ids.contains(&"L0") && ids.contains(&"L5"));
    }
    #[test]
    fn compliance_lens_caps_at_medium() {
        assert_eq!(by_id("L5").unwrap().max_severity, Some(crate::domain::Severity::Medium));
        assert_eq!(by_id("D5").unwrap().max_severity, Some(crate::domain::Severity::Medium));
        assert_eq!(by_id("L1").unwrap().max_severity, None);
    }
    #[test]
    fn select_subset_and_reject_unknown() {
        assert_eq!(select(Mode::Code, &["L1".into(), "L2".into()]).unwrap().len(), 2);
        assert!(select(Mode::Code, &["NOPE".into()]).is_err());
    }
}
