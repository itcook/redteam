BIN := plugins/breakpoint/bin/darwin/aarch64/brk
TARGET := aarch64-apple-darwin

.PHONY: all gen build verify

all: build gen

# 从 commands/ 真相源重生成 codex-skills/
gen:
	bash scripts/gen-codex-skills

# 编译 darwin-arm64 release 二进制并放入插件 bin/
build:
	cargo build --release --target $(TARGET)
	mkdir -p $(dir $(BIN))
	cp target/$(TARGET)/release/brk $(BIN)
	chmod +x plugins/breakpoint/bin/brk

# 本地等价 CI 防漂移：重生成后无 git 改动
verify: gen
	git diff --exit-code plugins/breakpoint/codex-skills/
	cargo test --test packaging
