#!/usr/bin/env bash
# redteam 发布：构建 → 打包 + SHA256SUMS → tag → GitHub Release → 可选 R2 镜像 → 下载回验。
#
# 通道（计划 §10 / 7.0）：GitHub Release 是公开发布面（standalone 安装 / `redteam update` 默认源）；
# R2 只是可选镜像。插件渠道（marketplace）由 `make build` + 单独提交二进制维护，不走本脚本。
#
# 用法：
#   scripts/release.sh v0.1.0                  # 完整发布（正式版）
#   scripts/release.sh v0.1.0-rc.1             # 预发布（GitHub 标 prerelease，R2 不更新 latest）
#   scripts/release.sh v0.1.0 --dry-run        # 只做前置检查 + 构建 + 打包，不碰 git/Release/R2
#   scripts/release.sh v0.1.0 --no-push        # 本地打 tag，不推、不建 Release、不上传 R2
#   scripts/release.sh v0.1.0 --skip-r2        # 只发 GitHub Release
#
# 前置：
#   - 工作区干净（已跟踪文件无改动；--dry-run 只警告）
#   - 已存在 tag 时必须指向 HEAD（产物来自 HEAD，tag 必须与它同一份源码）
#   - `gh auth status` 正常（非 dry-run / no-push）
#   - R2（可选）：环境变量或 .env 里的 R2_STATIC_ENDPOINT / R2_ACCESS_KEY_ID /
#     R2_SECRET_ACCESS_KEY / R2_BUCKET（可选 R2_BUCKET_DOMAIN）；缺失时自动跳过镜像
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

REPO="${REDTEAM_REPO:-itcook/redteam}"
TARGET="aarch64-apple-darwin"
DIST="$ROOT/dist/release"
R2_PREFIX="redteam"

die() { echo "错误: $*" >&2; exit 1; }
info() { echo "==> $*"; }

# ---------------------------------------------------------------- 参数
TAG_ARG="${1:-}"
[ -n "$TAG_ARG" ] || die "用法: scripts/release.sh vX.Y.Z[-pre] [--dry-run] [--no-push] [--skip-r2]"
shift
DRY_RUN=false
NO_PUSH=false
SKIP_R2=false
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=true ;;
        --no-push) NO_PUSH=true ;;
        --skip-r2) SKIP_R2=true ;;
        *) die "未知参数: $arg" ;;
    esac
done

# ---------------------------------------------------------------- 版本校验
case "$TAG_ARG" in
    v*) VERSION="${TAG_ARG#v}" ;;
    *) die "tag 必须形如 v0.1.0（收到: ${TAG_ARG}）" ;;
esac
printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$' \
    || die "非法版本: $VERSION"
BASE_VERSION="${VERSION%%-*}"
CARGO_VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)"
if [ "$CARGO_VERSION" != "$VERSION" ] && [ "$CARGO_VERSION" != "$BASE_VERSION" ]; then
    die "Cargo.toml 版本 ($CARGO_VERSION) 与 tag ($TAG_ARG) 不一致（先 bump Cargo.toml）"
fi
PRERELEASE=false
case "$VERSION" in *-*) PRERELEASE=true ;; esac

info "版本 ${VERSION}（prerelease=$PRERELEASE, dry-run=$DRY_RUN, no-push=$NO_PUSH）"

# ---------------------------------------------------------------- 前置检查
if [ "$DRY_RUN" = true ]; then
    [ -z "$(git status --porcelain --untracked-files=no)" ] \
        || echo "提示: dry-run 允许脏工作区（不会发布），但产物不代表任何已提交状态"
else
    [ -z "$(git status --porcelain --untracked-files=no)" ] \
        || die "工作区有未提交改动，先提交或 stash（发布产物必须来自确定的 commit）"
fi

command -v cargo >/dev/null || die "缺少 cargo"
command -v tar >/dev/null || die "缺少 tar"
command -v shasum >/dev/null || die "缺少 shasum"
[ -f LICENSE ] || die "缺少 LICENSE（发布归档需要它）"
[ -f README.md ] || die "缺少 README.md"
[ -f dist/install.sh ] || die "缺少 dist/install.sh（发布资产）"
[ -f dist/config.schema.json ] || die "缺少 dist/config.schema.json（发布资产，hibot 契约）"

if [ "$DRY_RUN" = false ] && [ "$NO_PUSH" = false ]; then
    command -v gh >/dev/null || die "缺少 gh"
    gh auth status >/dev/null 2>&1 || die "gh 未登录（gh auth login）"
fi

# tag 必须指向 HEAD：产物永远构建自 HEAD，tag 指向别处会让「源码 ↔ 二进制」对不上。
if git rev-parse -q --verify "refs/tags/$TAG_ARG" >/dev/null; then
    tag_commit="$(git rev-parse "$TAG_ARG^{commit}")"
    head_commit="$(git rev-parse HEAD)"
    if [ "$tag_commit" != "$head_commit" ]; then
        die "tag $TAG_ARG 指向 ${tag_commit:0:7}，但 HEAD 是 ${head_commit:0:7}：产物会来自 HEAD 而 tag 指向别处。先把 tag 移到 HEAD（git tag -f）或换版本号"
    fi
    info "tag 已存在且指向 HEAD: $TAG_ARG"
elif [ "$DRY_RUN" = true ]; then
    info "tag $TAG_ARG 不存在（dry-run 不创建）"
fi

info "门禁：make verify"
make verify >/dev/null || die "make verify 未通过（门禁不过不发布）"

# ---------------------------------------------------------------- 构建
# 关键：**不跑 `make build`**——它会把新二进制写进入库的插件目录（5.9MB diff），
# 而发布单元约定「不提交重建的插件二进制」。发布产物直接取 cargo 产物；
# 插件渠道仍由 `make build` + 单独提交维护（runbook 有说明）。
info "同步派生物（make gen）"
make gen >/dev/null || die "make gen 失败"
info "构建 ${TARGET}"
cargo build --release --locked --target "$TARGET"
BIN="target/${TARGET}/release/redteam"
[ -x "$BIN" ] || die "缺少构建产物: $BIN"
built_version="$("$BIN" --version | awk '{print $2}')"
[ "$built_version" = "$BASE_VERSION" ] \
    || die "二进制报告 $built_version，期望 $BASE_VERSION（Cargo.toml 与构建产物不同源）"

# ---------------------------------------------------------------- 打包
rm -rf "$DIST"
mkdir -p "$DIST"
ASSET="redteam-${VERSION}-${TARGET}.tar.gz"

pkg="$(mktemp -d)"
trap 'rm -rf "$pkg"' EXIT
cp "$BIN" "$pkg/redteam"
cp README.md LICENSE "$pkg/"
info "打包 $ASSET"
tar -czf "$DIST/$ASSET" -C "$pkg" .
cp dist/install.sh "$DIST/install.sh"
cp dist/config.schema.json "$DIST/config.schema.json"

# SHA256SUMS 覆盖归档与 schema（hibot 契约要求 schema 可校验；install.sh 自身不入清单）。
(cd "$DIST" && shasum -a 256 "$ASSET" "config.schema.json" >SHA256SUMS)

# release notes：优先取 CHANGELOG 对应版本段，退回 [Unreleased]，再加资产指纹。
NOTES="$DIST/release-notes.md"
awk -v version="$VERSION" '
    $0 ~ "^## \\[" version "\\]" { inside = 1; next }
    inside && /^## \[/ { inside = 0 }
    inside { print }
' CHANGELOG.md >"$NOTES"
if [ ! -s "$NOTES" ]; then
    awk '
        /^## \[Unreleased\]/ { inside = 1; next }
        inside && /^## \[/ { inside = 0 }
        inside { print }
    ' CHANGELOG.md >"$NOTES"
fi
[ -s "$NOTES" ] || echo "redteam v${VERSION}" >"$NOTES"
{
    printf '\n## 发布资产\n\n| 文件 | sha256 |\n|---|---|\n'
    while read -r hash name; do
        [ -n "$name" ] && printf '| `%s` | `%s` |\n' "$name" "$hash"
    done <"$DIST/SHA256SUMS"
    printf '\n配置 schema：`config.schema.json`（`$id: urn:redteam:config/1`，`schemaVersion: 1`）\n'
} >>"$NOTES"

printf '\n产物：\n'
ls -l "$DIST" | awk 'NR>1 {printf "  %-52s %10d B  %5.2f MiB\n", $9, $5, $5/1048576}'
printf '\nSHA256SUMS：\n'
sed 's/^/  /' "$DIST/SHA256SUMS"

if [ "$DRY_RUN" = true ]; then
    info "dry-run：已跳过 tag / GitHub Release / R2 上传"
    exit 0
fi

# ---------------------------------------------------------------- tag
if git rev-parse -q --verify "refs/tags/$TAG_ARG" >/dev/null; then
    info "tag 已存在且指向 HEAD: $TAG_ARG"
else
    info "创建 tag $TAG_ARG"
    git tag "$TAG_ARG"
fi

if [ "$NO_PUSH" = true ]; then
    info "--no-push：已建本地 tag，未推送、未建 Release、未上传 R2"
    exit 0
fi

info "推送 tag 到 origin"
git push origin "refs/tags/$TAG_ARG"

# ---------------------------------------------------------------- GitHub Release
info "发布 GitHub Release"
release_args=(--title "redteam $TAG_ARG" --notes-file "$NOTES")
if [ "$PRERELEASE" = true ]; then release_args+=(--prerelease); fi
if gh release view "$TAG_ARG" >/dev/null 2>&1; then
    info "Release 已存在，覆盖上传资产"
    gh release upload "$TAG_ARG" \
        "$DIST/$ASSET" "$DIST/SHA256SUMS" "$DIST/install.sh" "$DIST/config.schema.json" --clobber
else
    gh release create "$TAG_ARG" \
        "$DIST/$ASSET" "$DIST/SHA256SUMS" "$DIST/install.sh" "$DIST/config.schema.json" \
        "${release_args[@]}"
fi

# ---------------------------------------------------------------- R2 镜像（可选）
R2_ENABLED=true
if [ "$SKIP_R2" = true ]; then
    R2_ENABLED=false
    info "--skip-r2：跳过 R2 上传"
else
    if [ -f .env ]; then
        set -a; . ./.env; set +a
    fi
    missing=""
    for v in R2_STATIC_ENDPOINT R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY R2_BUCKET; do
        eval "value=\${$v:-}"
        [ -n "$value" ] || missing="$missing $v"
    done
    if [ -n "$missing" ]; then
        R2_ENABLED=false
        info "缺少 R2 凭据（$missing），跳过 R2 镜像（不影响 GitHub Release）"
    elif ! command -v aws >/dev/null; then
        R2_ENABLED=false
        info "缺少 aws CLI，跳过 R2 镜像（不影响 GitHub Release）"
    fi
fi

if [ "$R2_ENABLED" = true ]; then
    # 统一 no-cache：版本化路径也会被重发覆盖（rc 重跑、修资产），不能让 CDN 继续供旧副本。
    r2_put() { # local_path remote_key [content-type]
        local ct="${3:-application/octet-stream}"
        AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
        AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
            aws s3 cp "$1" "s3://${R2_BUCKET}/${R2_PREFIX}/$2" \
                --endpoint-url "$R2_STATIC_ENDPOINT" \
                --content-type "$ct" --cache-control no-cache >/dev/null
    }

    info "上传到 R2（bucket=$R2_BUCKET, prefix=${R2_PREFIX}/）"
    r2_put "$DIST/$ASSET" "releases/v$VERSION/$ASSET"
    r2_put "$DIST/SHA256SUMS" "releases/v$VERSION/SHA256SUMS" text/plain
    r2_put "$DIST/config.schema.json" "releases/v$VERSION/config.schema.json" application/json
    r2_put "$DIST/install.sh" "install.sh" text/plain
    if [ "$PRERELEASE" = true ]; then
        info "prerelease：不更新 ${R2_PREFIX}/latest"
    else
        printf 'v%s\n' "$VERSION" >"$DIST/latest"
        r2_put "$DIST/latest" "latest" text/plain
    fi
fi

# ---------------------------------------------------------------- 下载回验
info "回验下载"
fail=0
check_url() {
    local url="$1" code
    code="$(curl -s -o /dev/null -w '%{http_code}' -L -I "$url" || true)"
    if [ "$code" = "200" ]; then printf '  ok   %s\n' "$url"
    else printf '  FAIL %s (HTTP %s)\n' "$url" "$code"; fail=1; fi
}

# 只查 200 不够：CDN / 重发场景可能供旧对象（HTTP 也是 200）。下载回来比对 SHA256。
check_asset() { # local_path url
    local url="$2" tmp want have attempt=1
    tmp="$(mktemp)"
    want="$(shasum -a 256 "$1" | awk '{print $1}')"
    while :; do
        curl -fsSL "$url" -o "$tmp" 2>/dev/null || true
        have="$(shasum -a 256 "$tmp" | awk '{print $1}')"
        if [ "$want" = "$have" ]; then
            printf '  ok   %s (sha256 匹配)\n' "$url"
            break
        fi
        if [ "$attempt" -ge 3 ]; then
            if curl -fsSL "$url?nocache=$(date +%s)" -o "$tmp" 2>/dev/null \
                && [ "$want" = "$(shasum -a 256 "$tmp" | awk '{print $1}')" ]; then
                printf '  FAIL %s（对象正确，但边缘缓存仍是旧副本；重发同一版本需等 TTL 或 purge）\n' "$url"
            else
                printf '  FAIL %s（远端对象与本地不一致）\n' "$url"
            fi
            fail=1
            break
        fi
        attempt=$((attempt + 1))
        sleep 3
    done
    rm -f "$tmp"
}

GH_BASE="https://github.com/${REPO}/releases/download/${TAG_ARG}"
gh release view "$TAG_ARG" --json assets --jq '.assets[].name' | sed 's/^/  release: /'
check_asset "$DIST/$ASSET" "$GH_BASE/$ASSET"
check_asset "$DIST/config.schema.json" "$GH_BASE/config.schema.json"
check_url "$GH_BASE/install.sh"
check_url "$GH_BASE/SHA256SUMS"

if [ "$R2_ENABLED" = true ]; then
    PUBLIC_BASE="https://${R2_BUCKET_DOMAIN:-static.hiapi.ai}/${R2_PREFIX}"
    check_asset "$DIST/$ASSET" "$PUBLIC_BASE/releases/v$VERSION/$ASSET"
    check_asset "$DIST/config.schema.json" "$PUBLIC_BASE/releases/v$VERSION/config.schema.json"
    check_url "$PUBLIC_BASE/releases/v$VERSION/SHA256SUMS"
    check_url "$PUBLIC_BASE/install.sh"
    [ "$PRERELEASE" = true ] || check_url "$PUBLIC_BASE/latest"
fi
[ "$fail" = 0 ] || die "有下载回验失败，见上方 FAIL 行"

printf '\n✅ 发布完成: %s\n' "$TAG_ARG"
cat <<EOF

安装验证：
  curl -fsSL https://github.com/${REPO}/releases/download/${TAG_ARG}/install.sh | sh
  REDTEAM_VERSION=${TAG_ARG} curl -fsSL https://github.com/${REPO}/releases/download/${TAG_ARG}/install.sh | sh
EOF
