BIN := plugins/redteam/bin/darwin/aarch64/redteam-next
TARGET := aarch64-apple-darwin

.PHONY: all gen build verify

all: build gen

# 从 commands/ 真相源重生成 codex-skills/
gen:
	bash scripts/gen-codex-skills

# 编译 darwin-arm64 release 二进制并放入插件 bin/（M21 分支产物名 redteam-next，D10）：
# 只放置 $(BIN)=…/redteam-next，绝不覆盖同目录的 dev `redteam`（插件 launcher bin/redteam 与 ship 循环全程指向它，
# M21c 切名前不动）；launcher 已入库带执行位，不再 chmod。`redteam-next` 被 .gitignore 忽略，不入库。
build:
	cargo build --release --target $(TARGET)
	mkdir -p $(dir $(BIN))
	rm -f $(BIN)
	cp target/$(TARGET)/release/redteam-next $(BIN)

# build-dev-baseline（说明，非目标）：插件里的 dev 二进制 plugins/redteam/bin/darwin/aarch64/redteam 来自 `dev` 分支
# （最后重建于 feebc92，2026-09-04）并入库；它是 M21 对照基线（design 铁律 6），本分支不重建、不覆盖。需要重建时
# 切到 dev 检出用其 Makefile 的 build 目标。

# 本地等价 CI 防漂移：重生成后无 git 改动
verify: gen
	git diff --exit-code plugins/redteam/codex-skills/
	cargo test --test packaging
