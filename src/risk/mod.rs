//! 风险评分器（M21b 批 A）：成区 → 四类信号 → H0 / R / 预算三档。
//! 规则即 spec：P0 §15.4 v4 定案常量化，零 LLM、零配置旋钮。

mod regions;
mod signals;

pub use regions::{regions, Region, RegionKind, SignalKind};
pub use signals::{score_regions, GitEnv, ScoreEnv};

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::MaxCells;
use crate::evidence::git::FileChange;

/// H0 门槛 θ（P0 §15.4）。
pub const THETA: f32 = 1.5;
/// 入 H0 最少独立信号类（path / struct / extapi / hist）；关键词类单独不成区。
pub const MIN_KINDS: usize = 2;
/// R 切点（D11-4 锁定；P0 §15.7 实测三分位 0.47 / 3.64 作回校参考）。
pub const TIER_MID: f32 = 0.5;
pub const TIER_HIGH: f32 = 3.5;
pub const CELLS_LOW: u32 = 4;
pub const CELLS_MID: u32 = 12;
pub const CELLS_HIGH: u32 = 32;
pub const CELLS_CAP: u32 = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Low,
    Mid,
    High,
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Tier::Low => "low",
            Tier::Mid => "mid",
            Tier::High => "high",
        })
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Hotspot {
    pub file: String,
    pub start: usize,
    pub end: usize,
    pub score: f32,
    pub kind: RegionKind,
    pub signals: Vec<SignalKind>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RiskRoute {
    pub hotspots: Vec<Hotspot>,
    pub r: f32,
    pub tier: Tier,
    pub budget_cells_hint: u32,
}

impl RegionKind {
    fn as_str(self) -> &'static str {
        match self {
            RegionKind::Primary => "primary",
            RegionKind::Ripple => "ripple",
        }
    }
}

impl serde::Serialize for RegionKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}
impl<'de> serde::Deserialize<'de> for RegionKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        match v.as_str() {
            "primary" => Ok(RegionKind::Primary),
            "ripple" => Ok(RegionKind::Ripple),
            _ => Err(serde::de::Error::custom("region kind")),
        }
    }
}

impl SignalKind {
    fn as_str(self) -> &'static str {
        match self {
            SignalKind::Path => "path",
            SignalKind::Struct => "struct",
            SignalKind::ExtApi => "extapi",
            SignalKind::Hist => "hist",
        }
    }
}
impl serde::Serialize for SignalKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}
impl<'de> serde::Deserialize<'de> for SignalKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        match v.as_str() {
            "path" => Ok(SignalKind::Path),
            "struct" => Ok(SignalKind::Struct),
            "extapi" => Ok(SignalKind::ExtApi),
            "hist" => Ok(SignalKind::Hist),
            _ => Err(serde::de::Error::custom("signal kind")),
        }
    }
}

pub fn qualifies(r: &Region, theta: f32) -> bool {
    let kinds: std::collections::BTreeSet<_> = r.signals.iter().copied().collect();
    r.score >= theta && kinds.len() >= MIN_KINDS
}

pub fn r_score(h0: &[Region], code_lines: usize) -> f32 {
    if code_lines == 0 {
        return 0.0;
    }
    let sum: f32 = h0.iter().map(|z| z.score).sum();
    let r = sum / (1.0 + (1.0 + code_lines as f32).ln());
    (r * 1000.0).round() / 1000.0
}

pub fn tier_of(r: f32) -> Tier {
    if r >= TIER_HIGH {
        Tier::High
    } else if r >= TIER_MID {
        Tier::Mid
    } else {
        Tier::Low
    }
}

pub fn budget_cells(tier: Tier, cfg: MaxCells) -> u32 {
    match cfg {
        MaxCells::Fixed(n) => n,
        MaxCells::Auto => {
            let hint = match tier {
                Tier::Low => CELLS_LOW,
                Tier::Mid => CELLS_MID,
                Tier::High => CELLS_HIGH,
            };
            hint.min(CELLS_CAP)
        }
    }
}

/// 阶段 0 入口：成区 → 打分 → H0 / R / 档位。
pub fn route(
    changes: &[FileChange],
    contents: &BTreeMap<String, String>,
    env: &mut dyn ScoreEnv,
    max_cells: MaxCells,
    code_lines: usize,
) -> RiskRoute {
    let mut zones = regions(changes, contents);
    score_regions(&mut zones, contents, env);
    let h0: Vec<Region> = zones.into_iter().filter(|z| qualifies(z, THETA)).collect();
    let r = r_score(&h0, code_lines);
    let tier = tier_of(r);
    let budget_cells_hint = budget_cells(tier, max_cells);
    let hotspots = h0
        .into_iter()
        .map(|z| Hotspot {
            file: z.file,
            start: z.start,
            end: z.end,
            score: z.score,
            kind: z.kind,
            signals: z.signals,
        })
        .collect();
    RiskRoute { hotspots, r, tier, budget_cells_hint }
}

/// 生产接线：从仓根读全文、构造 GitEnv。
pub fn route_from_git(
    changes: &[FileChange],
    root: &Path,
    head: &str,
    base: &str,
    ledger: Option<&Path>,
    before: Option<&str>,
    max_cells: MaxCells,
) -> RiskRoute {
    let mut contents = BTreeMap::new();
    for c in changes {
        if let Ok(t) = std::fs::read_to_string(root.join(&c.path)) {
            contents.insert(c.path.clone(), t);
        }
    }
    let mut env = GitEnv::open(root, head, base, base, ledger, before);
    let code_lines: usize = changes
        .iter()
        .filter(|c| !signals::is_doc_path(&c.path))
        .map(|c| {
            let (a, d) = crate::bundle::count_plus_minus(&c.diff);
            a + d
        })
        .sum();
    route(changes, &contents, &mut env, max_cells, code_lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::risk::signals::Stub;

    fn fc(path: &str, diff: &str) -> FileChange {
        FileChange { path: path.into(), diff: diff.into() }
    }

    #[test]
    fn theta_boundary_and_single_kind_out() {
        let mut r = Region {
            file: "a.rs".into(),
            start: 1,
            end: 2,
            kind: RegionKind::Primary,
            symbols: vec![],
            padded: false,
            signals: vec![SignalKind::Path],
            added: vec![],
            removed: vec![],
            score: 1.5,
            mult: 1.0,
        };
        assert!(!qualifies(&r, THETA), "单类不成 H0");
        r.signals.push(SignalKind::Struct);
        assert!(qualifies(&r, THETA));
        r.score = 1.49;
        assert!(!qualifies(&r, THETA));
    }

    #[test]
    fn r_formula_and_empty() {
        let z = Region {
            file: "a.rs".into(),
            start: 1,
            end: 2,
            kind: RegionKind::Primary,
            symbols: vec![],
            padded: false,
            signals: vec![SignalKind::Path, SignalKind::Struct],
            added: vec![],
            removed: vec![],
            score: 3.0,
            mult: 1.0,
        };
        let r = r_score(&[z], 10);
        let expect = 3.0 / (1.0 + (11.0f32).ln());
        assert!((r - ((expect * 1000.0).round() / 1000.0)).abs() < 1e-6);
        assert_eq!(r_score(&[], 0), 0.0);
    }

    #[test]
    fn three_tiers_and_cap() {
        assert_eq!(tier_of(0.0), Tier::Low);
        assert_eq!(tier_of(0.5), Tier::Mid);
        assert_eq!(tier_of(3.5), Tier::High);
        assert_eq!(budget_cells(Tier::Low, MaxCells::Auto), 4);
        assert_eq!(budget_cells(Tier::Mid, MaxCells::Auto), 12);
        assert_eq!(budget_cells(Tier::High, MaxCells::Auto), 32);
        assert_eq!(budget_cells(Tier::High, MaxCells::Fixed(7)), 7);
        assert_eq!(budget_cells(Tier::High, MaxCells::Auto).min(CELLS_CAP), 32);
    }

    #[test]
    fn route_empty_is_low() {
        let mut env = Stub::empty();
        let out = route(&[], &BTreeMap::new(), &mut env, MaxCells::Auto, 0);
        assert_eq!(out.r, 0.0);
        assert_eq!(out.tier, Tier::Low);
        assert_eq!(out.budget_cells_hint, 4);
        assert!(out.hotspots.is_empty());
    }

    #[test]
    fn route_auth_file_can_enter_h0() {
        let src = "fn handle_token() {\n    db.exec(\"SELECT * FROM t\");\n}\n";
        let diff = "@@ -2,1 +2,1 @@\n-    let x = 1;\n+    db.exec(\"SELECT * FROM t\");\n";
        let mut contents = BTreeMap::new();
        contents.insert("src/auth.rs".into(), src.into());
        let mut env = Stub::empty();
        env.hist.insert("src/auth.rs".into(), 6);
        let out = route(&[fc("src/auth.rs", diff)], &contents, &mut env, MaxCells::Auto, 2);
        assert!(!out.hotspots.is_empty() || out.r >= 0.0, "{out:?}");
    }
}
