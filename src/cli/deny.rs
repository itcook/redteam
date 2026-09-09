//! 敏感路径 denylist 单一真相源（M18 批 1 · 1-2）：`spec.rs` 的 `--spec` 参照读点与
//! `second_pass.rs` 的 needs 补查读点此前各自维护一份分叉 denylist——两侧本该同族但各漏了
//! 对方独有项（spec 侧漏 `.redteam/`/`.git` 工具目录段；needs 侧漏敏感词判据与 `.netrc`/`.p12`
//! 等扩展名，T46④）。本模块抽出两侧**并集**（收紧安全侧，宁可多拒不可漏拒）供两处调用。
//!
//! PEM 私钥内容嗅探（`contains_pem_private_key`，spec.rs 独有）**不**并入本模块——那是内容级
//! 检查（读到内容后才能判），本模块只管路径级 denylist；needs 读点按 E11-Q4 裁定「路径已够
//! 收窄威胁面」不做内容嗅探，维持原判。

/// 敏感扩展名（大小写不敏感，按 base 文件名结尾匹配）。两侧并集。
const DENY_EXT: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".pkcs12", ".p8", ".crt", ".cer"];

/// 敏感文件名（精确匹配 base，或 base 以 "{name}." 开头——兼容 `id_rsa.pub` 等变体）。
/// 两侧并集 + T46④ 补齐（needs 侧原缺 `.netrc`/`.htpasswd`/`id_dsa`/`id_ecdsa`）。
const DENY_BASE: &[&str] = &["id_rsa", "id_ed25519", "id_dsa", "id_ecdsa", ".netrc", ".htpasswd"];

/// 文档类扩展名白名单（豁免敏感词检查——`design.md` 这类文档即便含 "key"/"token" 子串仍放行）。
const DOC_EXT: &[&str] = &[".md", ".txt", ".rst", ".adoc"];

/// 敏感词（非文档类文件名含其一即拒）。needs 侧原完全没有此判据（T46④ 并集后新获得）。
const SENSITIVE_WORDS: &[&str] = &["secret", "credential", "password", "token", "key"];

/// 凭据类路径判据（扩展名 / `.env` 家族 / `id_rsa` 等基名 / 工具与 VCS 目录）——不含敏感词子串规则。
/// 供**非模型可控**的自动读点（pack 文件外走仓，M21a 批 E）：它只读支持语言的源文件，`token_bucket.rs` /
/// `keyboard.ts` 这类名含 key / token 的源码不该被子串规则静默跳过。
pub(crate) fn is_credential_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    if DENY_EXT.iter().any(|e| base.ends_with(e)) {
        return true;
    }
    // .env 家族：.env / .env.local / .env.production / config.env / app.env.local 全拒。
    // T46 顺手清死判：旧 spec.rs 的 `base == ".env" ||` 分支被本行的 `ends_with(".env")`
    // 完全蕴含（".env" 本身也以 ".env" 结尾），故只留一个条件。
    if base.ends_with(".env") || base.starts_with(".env.") || base.contains(".env.") {
        return true;
    }
    if DENY_BASE.iter().any(|b| base == *b || base.starts_with(&format!("{b}."))) {
        return true;
    }
    // 工具自建目录（.redteam/）与 VCS 内部目录（.git/）——任意路径深度，逐段匹配。原 needs
    // 侧独有判据；并集后 --spec 同样受益（`--spec .git/config` 这类同样该拒）。
    lower.split(['/', '\\']).any(|seg| seg == ".redteam" || seg == ".git")
}

/// 路径级 denylist 判定（两清单并集、收紧安全侧）——`spec.rs`/`second_pass.rs` 唯一真相源：
/// 凭据判据 + 敏感词子串规则（模型可控读点：宁可多拒不可漏拒）。
/// 内容级检查（PEM 嗅探）不在此列，各读点自行决定是否叠加。
pub(crate) fn is_denylisted_path(path: &str) -> bool {
    if is_credential_path(path) {
        return true;
    }
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    // 名含敏感词且非文档类 → 拒（design.md 这类文档放行）。"key" 覆盖 private_key/ssh_key/
    // deploy_key/prod-key.json 等改名密钥；代价=非文档类名含 "key" 子串误伤（如 monkey.go），
    // 与既有 secret/token 同款权衡，错在安全侧可接受。
    let doc = DOC_EXT.iter().any(|e| base.ends_with(e));
    !doc && SENSITIVE_WORDS.iter().any(|w| base.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_rejects_spec_side_only_items() {
        // spec 侧原有、needs 侧原缺——并集后两侧都拒。
        // 注：.txt 在 DOC_EXT 白名单内（豁免敏感词检查），故用非文档扩展名举例。
        for p in ["secret.rs", "my-password-store.py", "credential_helper.py", "deploy-token.json"] {
            assert!(is_denylisted_path(p), "{p} 应被敏感词判据拒绝");
        }
    }

    #[test]
    fn union_rejects_needs_side_only_items() {
        // needs 侧原有、spec 侧原缺——并集后两侧都拒。
        assert!(is_denylisted_path(".redteam/proj.db"));
        assert!(is_denylisted_path("sub/.git/config"));
        assert!(is_denylisted_path(".git/HEAD"));
    }

    #[test]
    fn t46_4_netrc_and_p12_rejected() {
        assert!(is_denylisted_path(".netrc"));
        assert!(is_denylisted_path("backup/.netrc"));
        assert!(is_denylisted_path("client.p12"));
    }

    #[test]
    fn doc_extensions_bypass_sensitive_word_check() {
        assert!(!is_denylisted_path("design-token-system.md"));
        assert!(!is_denylisted_path("environment.md"));
    }

    #[test]
    fn env_family_all_variants_rejected() {
        for p in [".env", ".env.local", ".env.production", "config.env", "app.env.local"] {
            assert!(is_denylisted_path(p), "{p} 应被 .env 家族判据拒绝");
        }
    }

    #[test]
    fn ordinary_source_files_pass() {
        for p in ["src/main.rs", "src/lib.rs", "README.md"] {
            assert!(!is_denylisted_path(p), "{p} 不应被误伤");
        }
    }

    #[test]
    fn credential_path_excludes_sensitive_word_rule() {
        // M21a 批 E 评审修 10：走仓读点用凭据判据，名含 key / token 的源码放行；敏感词规则仍归 is_denylisted_path。
        for src in ["src/token_bucket.rs", "src/keyboard.ts", "src/hotkeys.go", "src/monkey.go"] {
            assert!(!is_credential_path(src) && is_denylisted_path(src), "{src}");
        }
        for cred in ["src/id_rsa.rs", "conf/.env.local", "certs/x.pem", ".redteam/proj.db", "a/.git/config"] {
            assert!(is_credential_path(cred) && is_denylisted_path(cred), "{cred}");
        }
    }
}
