//! `redteam update`：解析目标版本、校验归档、原子替换当前可执行文件。
//!
//! 分发通道（计划 §10 / 7.0）：standalone 安装（`dist/install.sh`）用本命令自更新；
//! 插件渠道（`plugins/redteam/bin/`）默认拒写，见 `replace::is_plugin_path`。
//!
//! 默认下载源是 R2 镜像（本仓唯一公开源）；GitHub Release 保留为显式备源（`--from github`）。
//! `--base-url` 同时是本地验收的入口（起一个静态目录按 R2 布局即可端到端演练）。

pub mod asset;
pub mod replace;
pub mod version;

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::Client;

/// 默认下载源：r2（唯一公开源；换域名/路径时在 `R2_BASE_URL` 做旧源兼容）。
pub const DEFAULT_FROM: &str = "r2";
/// 公开仓（与 `scripts/release.sh` 的 `REPO` 一致）。
pub const GITHUB_REPO: &str = "itcook/redteam";
/// GitHub 资产基址（`{base}/{tag}/{asset}`）。
pub const GITHUB_DOWNLOAD_BASE: &str = "https://github.com/itcook/redteam/releases/download";
/// R2 镜像基址（`{base}/latest`、`{base}/{tag}/...`）。
/// 公开前缀已含产品路径（`executor/audit/redteam`），版本目录没有历史 `releases/` 段。
pub const R2_BASE_URL: &str = "https://release.hibot.sh/executor/audit/redteam";
/// 首期唯一产物（平台扩展位 7.7）。
pub const TARGET: &str = "aarch64-apple-darwin";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// 一次更新/检查的输入。
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// 只检查，不写任何文件（不取锁、不下载归档）。
    pub check: bool,
    /// 显式版本（`vX.Y.Z` 或 `X.Y.Z`）；给出时不请求 `latest`。
    pub version: Option<String>,
    /// 即使已是最新也重装。
    pub force: bool,
    /// 下载源：`github` | `r2`。
    pub from: String,
    /// 下载基址覆盖（环境变量/参数）。
    pub base_url: Option<String>,
}

/// 一次更新/检查的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub current: String,
    pub latest: String,
    pub up_to_date: bool,
    pub updated: bool,
}

impl Outcome {
    /// 人读输出（单行或多行，一次读完）。
    pub fn render(&self) -> String {
        if self.updated {
            format!("已更新：{} → {}", self.current, self.latest)
        } else if self.up_to_date {
            format!("已是最新：{}", self.current)
        } else {
            format!(
                "current: {}\nlatest:  {}\n→ 有可用更新：redteam update",
                self.current, self.latest
            )
        }
    }
}

/// 基址优先级：显式参数 > `REDTEAM_BASE_URL` > 按 `from` 的默认值。
pub fn resolve_base_url(from: &str, explicit: Option<&str>) -> Result<String> {
    let chosen = explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("REDTEAM_BASE_URL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        });
    let base = match (from, chosen) {
        (_, Some(base)) => base,
        ("github", None) => GITHUB_DOWNLOAD_BASE.to_string(),
        ("r2", None) => R2_BASE_URL.to_string(),
        (other, None) => bail!("未知下载源: {other}（可选 github / r2）"),
    };
    Ok(base.trim_end_matches('/').to_string())
}

/// 执行一次更新/检查。`exe` 显式传入：命令层用 `std::env::current_exe()`，测试用 tempdir 假二进制。
pub async fn run(options: &UpdateOptions, exe: &Path) -> Result<Outcome> {
    ensure_platform()?;
    let base = resolve_base_url(&options.from, options.base_url.as_deref())?;
    let latest = match &options.version {
        Some(version) => version::normalize_version(version)?,
        None => fetch_latest(&options.from, &base).await?,
    };
    let current = format!("v{}", env!("CARGO_PKG_VERSION"));
    let explicit_version = options.version.is_some();
    let up_to_date = if explicit_version {
        version::same_version(&current, &latest)
    } else {
        !version::needs_update(&current, &latest)
    };

    let mut outcome = Outcome {
        current,
        latest,
        up_to_date,
        updated: false,
    };

    // --check 承诺零写入：不取锁、不下载归档、不创建临时文件。
    if options.check || (up_to_date && !options.force) {
        return Ok(outcome);
    }

    let exe = replace::resolve_target(exe);
    if replace::is_plugin_path(&exe) && !options.force {
        bail!(replace::plugin_guard_message(&exe));
    }

    let exe_for_lock = exe.clone();
    let _lock = tokio::task::spawn_blocking(move || replace::UpdateLock::acquire(&exe_for_lock))
        .await
        .context("获取更新锁失败")??;

    let (archive, sums) = download_release(&options.from, &base, &outcome.latest).await?;
    let asset = asset::asset_name(&outcome.latest, TARGET);
    let expected = asset::checksum_for(&sums, &asset)
        .ok_or_else(|| anyhow::anyhow!("SHA256SUMS 中没有 {asset}（发布产物不完整）"))?;
    let actual = asset::sha256_hex(&archive);
    if actual != expected {
        bail!("SHA256 校验失败（{asset}），已中止；现有二进制未改动");
    }

    let bytes = asset::extract_binary(&archive)?;
    let staged = replace::StagedBinary::write(&exe, &bytes)?;
    // 发布 tag 可能带预发布后缀（v0.1.0-rc.1），二进制报告的是 Cargo 版本（0.1.0）。
    let expected_version = outcome.latest.split('-').next().unwrap_or(&outcome.latest);
    replace::self_check(staged.path(), expected_version).await?;
    staged.commit()?;

    outcome.updated = true;
    Ok(outcome)
}

fn ensure_platform() -> Result<()> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(())
    } else {
        bail!("首期仅 macOS arm64 支持自更新（当前构建平台不是 aarch64-apple-darwin）")
    }
}

async fn fetch_latest(from: &str, base: &str) -> Result<String> {
    let client = http_client()?;
    if from == "github" && base == GITHUB_DOWNLOAD_BASE {
        // GitHub API 的 releases/latest 天然排除 prerelease——`latest` 永远指向正式版。
        let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
        let body = fetch_text(&client, &url).await?;
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .with_context(|| format!("解析 GitHub Release 响应失败（{url}）"))?;
        let tag = parsed["tag_name"]
            .as_str()
            .filter(|tag| !tag.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("GitHub Release 响应缺少 tag_name（{url}）"))?;
        version::normalize_version(tag)
    } else {
        let url = format!("{base}/latest");
        let body = fetch_text(&client, &url).await?;
        version::resolve_latest(&body)
            .with_context(|| format!("无法解析最新版本（{url}）"))
    }
}

/// 版本目录下的归档与 SHA256SUMS URL：`{base}/{tag}/{asset}`。
/// GitHub 与 R2 同形；R2 的 `releases/` 段已随公开前缀收敛去掉。
fn release_urls(from: &str, base: &str, tag: &str) -> Result<(String, String)> {
    match from {
        "github" | "r2" => {}
        other => bail!("未知下载源: {other}（可选 github / r2）"),
    }
    let dir = format!("{base}/{tag}");
    let asset = asset::asset_name(tag, TARGET);
    Ok((format!("{dir}/{asset}"), format!("{dir}/SHA256SUMS")))
}

async fn download_release(from: &str, base: &str, tag: &str) -> Result<(Vec<u8>, String)> {
    let (archive_url, sums_url) = release_urls(from, base, tag)?;

    let client = http_client()?;
    let archive = fetch_bytes(&client, &archive_url).await?;
    let sums = fetch_text(&client, &sums_url).await?;
    Ok((archive, sums))
}

fn http_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(format!("redteam/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("初始化 HTTP 客户端失败")
}

async fn fetch_text(client: &Client, url: &str) -> Result<String> {
    let bytes = fetch_bytes(client, url).await?;
    String::from_utf8(bytes).with_context(|| format!("响应不是 UTF-8 文本: {url}"))
}

async fn fetch_bytes(client: &Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| network_error(url, &error))?;
    let status = response.status();
    if !status.is_success() {
        bail!("下载失败（HTTP {}）: {url}", status.as_u16());
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| network_error(url, &error))
}

fn network_error(url: &str, error: &reqwest::Error) -> anyhow::Error {
    let reason = if error.is_timeout() {
        "请求超时"
    } else if error.is_connect() {
        "无法连接服务端"
    } else {
        "网络请求失败"
    };
    anyhow::anyhow!("{reason}: {url}（检查网络后重试）")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_prefers_explicit_then_default() {
        assert_eq!(
            resolve_base_url("r2", Some("http://127.0.0.1:9/redteam/")).unwrap(),
            "http://127.0.0.1:9/redteam"
        );
        assert_eq!(resolve_base_url("github", None).unwrap(), GITHUB_DOWNLOAD_BASE);
        assert_eq!(resolve_base_url("r2", None).unwrap(), R2_BASE_URL);
        assert_eq!(DEFAULT_FROM, "r2");
        assert_eq!(resolve_base_url(DEFAULT_FROM, None).unwrap(), R2_BASE_URL);
        assert!(resolve_base_url("ftp", None).is_err());
    }

    #[test]
    fn release_urls_are_tag_scoped_for_both_sources() {
        // R2：公开前缀已含产品路径，版本目录直接 `{base}/{tag}`（没有 releases 段）。
        assert_eq!(R2_BASE_URL, "https://release.hibot.sh/executor/audit/redteam");
        let (r2_archive, r2_sums) = release_urls("r2", R2_BASE_URL, "v0.1.0").unwrap();
        assert_eq!(
            r2_archive,
            "https://release.hibot.sh/executor/audit/redteam/v0.1.0/redteam-0.1.0-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            r2_sums,
            "https://release.hibot.sh/executor/audit/redteam/v0.1.0/SHA256SUMS"
        );
        assert!(!r2_archive.contains("/releases/"));

        // GitHub：仍是 `{base}/{tag}`（base 自身带 releases/download）。
        let (gh_archive, gh_sums) = release_urls("github", GITHUB_DOWNLOAD_BASE, "v0.1.0").unwrap();
        assert_eq!(
            gh_archive,
            "https://github.com/itcook/redteam/releases/download/v0.1.0/redteam-0.1.0-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            gh_sums,
            "https://github.com/itcook/redteam/releases/download/v0.1.0/SHA256SUMS"
        );

        // 预发布 tag 原样进路径。
        let (rc_archive, _) = release_urls("r2", R2_BASE_URL, "v0.1.0-rc.1").unwrap();
        assert!(rc_archive.contains("/v0.1.0-rc.1/redteam-0.1.0-rc.1-"));

        assert!(release_urls("ftp", R2_BASE_URL, "v0.1.0").is_err());
    }

    #[test]
    fn outcome_rendering_is_one_glance() {
        let up_to_date = Outcome {
            current: "v0.1.0".into(),
            latest: "v0.1.0".into(),
            up_to_date: true,
            updated: false,
        };
        assert_eq!(up_to_date.render(), "已是最新：v0.1.0");

        let behind = Outcome {
            current: "v0.1.0".into(),
            latest: "v0.2.0".into(),
            up_to_date: false,
            updated: false,
        };
        assert!(behind.render().contains("redteam update"));

        let updated = Outcome {
            current: "v0.1.0".into(),
            latest: "v0.2.0".into(),
            up_to_date: false,
            updated: true,
        };
        assert_eq!(updated.render(), "已更新：v0.1.0 → v0.2.0");
    }
}
