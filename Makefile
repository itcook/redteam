BIN := plugins/redteam/bin/darwin/aarch64/redteam
TARGET := aarch64-apple-darwin

.PHONY: all gen build verify schema

all: build gen

# 从 commands/ 真相源重生成 codex-skills/
gen:
	bash scripts/gen-codex-skills

# 编译 darwin-arm64 release 二进制并放入插件 bin/。**M21c 批 0 已切名**（D19-1）：产物就叫 `redteam`，
# 直接覆盖 launcher（bin/redteam）指向的那个文件——插件与 ship 循环从此跑新引擎。
# 二进制入库（marketplace 分发要它），所以 build 后工作树会有 5MB 级 diff，按仓内惯例单独提交（`build: 重建二进制`）。
# 回退到 M21 之前的 dev 二进制：`git show feebc92:plugins/redteam/bin/darwin/aarch64/redteam > $(BIN) && chmod +x $(BIN)`。
# 先 cp 到 .new 再 mv 覆盖（收口评审 #6）：`rm` 与 `cp` 之间 cp 失败会让插件里一个二进制都没有，
# 且 mv 是原子替换、不会写正在运行的那个 inode。launcher 已入库带执行位，不再 chmod。
build:
	cargo build --release --target $(TARGET)
	mkdir -p $(dir $(BIN))
	cp target/$(TARGET)/release/redteam $(BIN).new
	mv -f $(BIN).new $(BIN)

# build-dev-baseline（说明，非目标）：插件里的 dev 二进制 plugins/redteam/bin/darwin/aarch64/redteam 来自 `dev` 分支
# （最后重建于 feebc92，2026-09-04）并入库；它是 M21 对照基线（design 铁律 6），本分支不重建、不覆盖。需要重建时
# 切到 dev 检出用其 Makefile 的 build 目标。

# 本地等价 CI 防漂移：重生成后无 git 改动（codex-skills 镜像 + 配置 schema 生成物）
verify: gen schema
	git diff --exit-code plugins/redteam/codex-skills/
	git diff --exit-code dist/config.schema.json
	cargo test --test packaging

# 配置契约生成物（P17 / 1.9）：schema 从类型 derive，入库防漂移。
schema:
	cargo run --quiet -- config schema --out dist/config.schema.json
