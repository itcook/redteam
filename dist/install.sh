#!/bin/sh
# redteam installer — macOS Apple Silicon (arm64)
#
#   curl -fsSL https://github.com/itcook/redteam/releases/latest/download/install.sh | sh
#   REDTEAM_VERSION=v0.1.0 curl -fsSL .../install.sh | sh
#   curl -fsSL .../install.sh | REDTEAM_INSTALL_DIR=/usr/local/bin sh
#
# 通道（计划 §10 / 7.0）：standalone CLI 装到 REDTEAM_INSTALL_DIR，之后用 `redteam update` 自更新；
# 插件渠道（marketplace / git pull）不经过本脚本。
#
# 环境变量（也可用同名长参数覆盖）：
#   REDTEAM_VERSION      版本号，`latest` = 最新**正式版**（预发布不参与）  默认 latest
#   REDTEAM_INSTALL_DIR  安装目录                                          默认 ~/.local/bin
#   REDTEAM_BASE_URL     下载基址（覆盖默认）
#   REDTEAM_FROM         下载源：github | r2                                默认 github
#
# 平台扩展位（7.7）：首期只有 macOS arm64。加平台 = 改下面 TARGET/REQUIRED_* 常量 + 发布产物矩阵，
# 主逻辑不动（Linux/Windows 归 M20，含交叉构建与 install.ps1）。
set -eu

REPO="${REDTEAM_REPO:-itcook/redteam}"
TARGET="aarch64-apple-darwin"
REQUIRED_OS="Darwin"
REQUIRED_ARCH="arm64"
R2_DEFAULT_BASE="https://release.hibot.sh/executor/audit/redteam"
GITHUB_DEFAULT_BASE="https://github.com/${REPO}/releases/download"

VERSION="${REDTEAM_VERSION:-latest}"
INSTALL_DIR="${REDTEAM_INSTALL_DIR:-$HOME/.local/bin}"
BASE_URL="${REDTEAM_BASE_URL:-}"
FROM="${REDTEAM_FROM:-github}"

err() {
    printf '错误: %s\n' "$*" >&2
    exit 1
}

info() {
    printf '%s\n' "$*"
}

have() {
    command -v "$1" >/dev/null 2>&1
}

usage() {
    cat <<'EOF'
redteam 安装脚本（macOS arm64）

用法:
  curl -fsSL <url>/install.sh | sh
  curl -fsSL <url>/install.sh | REDTEAM_VERSION=v0.1.0 sh

选项:
  --version <vX.Y.Z>   指定版本（默认 latest）
  --dir <path>         安装目录（默认 ~/.local/bin）
  --base-url <url>     下载基址（覆盖默认）
  --from <github|r2>   下载源（默认 github）
  -h, --help           显示本帮助

环境变量: REDTEAM_VERSION / REDTEAM_INSTALL_DIR / REDTEAM_BASE_URL / REDTEAM_FROM
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || err "--version 需要参数"; VERSION="$2"; shift 2 ;;
        --version=*) VERSION="${1#*=}"; shift ;;
        --dir) [ $# -ge 2 ] || err "--dir 需要参数"; INSTALL_DIR="$2"; shift 2 ;;
        --dir=*) INSTALL_DIR="${1#*=}"; shift ;;
        --base-url) [ $# -ge 2 ] || err "--base-url 需要参数"; BASE_URL="$2"; shift 2 ;;
        --base-url=*) BASE_URL="${1#*=}"; shift ;;
        --from) [ $# -ge 2 ] || err "--from 需要参数"; FROM="$2"; shift 2 ;;
        --from=*) FROM="${1#*=}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) err "未知参数: $1（用 --help 查看用法）" ;;
    esac
done

[ -n "$INSTALL_DIR" ] || err "安装目录不能为空"
case "$FROM" in
    github|r2) ;;
    *) err "未知下载源: ${FROM}（可选 github / r2）" ;;
esac

# 平台守卫（7.3 / 7.7）：先判平台再谈下载，避免在错误平台上下载一堆没用的东西。
os="$(uname -s)"
arch="$(uname -m)"
if [ "$os" != "$REQUIRED_OS" ] || [ "$arch" != "$REQUIRED_ARCH" ]; then
    err "首期仅 macOS arm64（检测到 ${os} ${arch}）；Linux/Windows 与 Intel Mac 暂无二进制"
fi

if ! have curl && ! have wget; then
    err "需要 curl 或 wget"
fi
have tar || err "需要 tar"

fetch_to() { # url dest
    if have curl; then
        curl -fsSL "$1" -o "$2"
    else
        wget -q -O "$2" "$1"
    fi
}

fetch_stdout() { # url
    if have curl; then
        curl -fsSL "$1"
    else
        wget -q -O - "$1"
    fi
}

if [ -z "$BASE_URL" ]; then
    if [ "$FROM" = "r2" ]; then
        BASE_URL="$R2_DEFAULT_BASE"
    else
        BASE_URL="$GITHUB_DEFAULT_BASE"
    fi
fi
BASE_URL="${BASE_URL%/}"

resolve_latest() {
    if [ "$FROM" = "r2" ]; then
        fetch_stdout "${BASE_URL%/}/latest" || return 1
    else
        # GitHub API 的 releases/latest 天然排除 prerelease——`latest` 永远指向正式版。
        body="$(fetch_stdout "https://api.github.com/repos/${REPO}/releases/latest")" || return 1
        printf '%s' "$body" |
            sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
            head -n 1
    fi
}

if [ "$VERSION" = "latest" ]; then
    VERSION="$(resolve_latest)" ||
        err "无法解析最新版本（${BASE_URL}/latest 不可达或尚未发布）；请用 REDTEAM_VERSION=v0.1.0 指定"
    [ -n "$VERSION" ] || err "无法解析最新版本，请用 REDTEAM_VERSION=v0.1.0 指定"
fi
case "$VERSION" in
    v*) ;;
    *) VERSION="v$VERSION" ;;
esac
# 与 release.sh 的 tag 校验同口径：允许 vX.Y.Z-rc.N，但仍然拒绝任意字符串。
if ! printf '%s' "$VERSION" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$'; then
    err "非法版本号: ${VERSION}（应形如 v0.1.0 或 v0.1.0-rc.1）"
fi
PLAIN_VERSION="${VERSION#v}"
ASSET="redteam-${PLAIN_VERSION}-${TARGET}.tar.gz"

# 版本目录：GitHub 与 R2 同形 `{base}/{VERSION}`（R2 公开前缀已含产品路径，没有历史 releases 段）。
RELEASE_BASE="${BASE_URL}/${VERSION}"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/redteam-install.XXXXXX")" || err "无法创建临时目录"
STAGED=""
cleanup() {
    rm -rf "$TMP_DIR"
    if [ -n "$STAGED" ]; then
        rm -f "$STAGED"
    fi
}
trap cleanup EXIT HUP INT TERM

info "下载 redteam ${VERSION}（${TARGET}）..."
fetch_to "${RELEASE_BASE}/${ASSET}" "${TMP_DIR}/${ASSET}" || err "下载失败: ${RELEASE_BASE}/${ASSET}"
fetch_to "${RELEASE_BASE}/SHA256SUMS" "${TMP_DIR}/SHA256SUMS" || err "下载 SHA256SUMS 失败"

expected="$(awk -v name="$ASSET" '$2 == name { print; exit }' "${TMP_DIR}/SHA256SUMS")"
[ -n "$expected" ] || err "SHA256SUMS 中没有 ${ASSET}"
printf '%s\n' "$expected" >"${TMP_DIR}/expected.sum"

info "校验 SHA256..."
if have shasum; then
    (cd "$TMP_DIR" && shasum -a 256 -c expected.sum >/dev/null) ||
        err "校验和验证失败，已中止安装（未写入任何文件）"
elif have sha256sum; then
    (cd "$TMP_DIR" && sha256sum -c expected.sum >/dev/null) ||
        err "校验和验证失败，已中止安装（未写入任何文件）"
else
    err "需要 shasum 或 sha256sum 校验 SHA256"
fi

mkdir -p "${TMP_DIR}/extract"
tar -xzf "${TMP_DIR}/${ASSET}" -C "${TMP_DIR}/extract" || err "解压失败"
[ -f "${TMP_DIR}/extract/redteam" ] || err "归档中缺少 redteam 二进制"

mkdir -p "$INSTALL_DIR" || err "无法创建安装目录: $INSTALL_DIR"
# staged + mv：同目录 rename 是原子的，失败时旧二进制原样保留。
STAGED="${INSTALL_DIR}/.redteam.staged.$$"
cp "${TMP_DIR}/extract/redteam" "$STAGED" || err "写入安装目录失败（旧版本未被覆盖）"
chmod 755 "$STAGED"
mv -f "$STAGED" "${INSTALL_DIR}/redteam" || err "安装失败（旧版本未被覆盖）"
STAGED=""

info "已安装到 ${INSTALL_DIR}/redteam"

case ":${PATH:-}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        printf '\n提示: %s 不在 PATH 中。把下面一行加入 ~/.zshrc（或 ~/.bash_profile）：\n' "$INSTALL_DIR"
        printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
        printf '然后执行: exec "$SHELL" -l\n'
        ;;
esac

"${INSTALL_DIR}/redteam" --version
