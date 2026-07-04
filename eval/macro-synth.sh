#!/bin/sh
# 宏合成样本:本仓零 macro_rules,宏场景用此 micro-crate 补测(报告 §3/§5)。
# 用法:./macro-synth.sh <dir> && EVAL_REPO=<dir> EVAL_TREES=<dir>/trees python3 harness.py run <fix-commit>
set -eu
S=$1
mkdir -p "$S/src" && cd "$S"
cat > Cargo.toml <<'EOF'
[package]
name = "macro-synth"
version = "0.1.0"
edition = "2021"
EOF
cat > src/lib.rs <<'EOF'
pub mod gen;
pub mod caller;

/// 场景A:普通声明宏,def + 跨文件调用点。
#[macro_export]
macro_rules! log_err {
    ($msg:expr) => {
        eprintln!("[err] {}", $msg)
    };
}

/// 场景B:宏生成 fn 定义(标识符由调用方传入)——三档提取器共同盲区。
#[macro_export]
macro_rules! gen_getter {
    ($name:ident, $val:expr) => {
        pub fn $name() -> u32 {
            $val
        }
    };
}
EOF
cat > src/gen.rs <<'EOF'
// 宏生成的 fn:width / height 两个标识符在此"隐形定义"。
crate::gen_getter!(width, 42);
crate::gen_getter!(height, 7);
EOF
cat > src/caller.rs <<'EOF'
pub fn use_all() -> u32 {
    crate::log_err!("boom");
    let a = crate::gen::width();
    let b = crate::gen::height();
    a + b
}

pub fn use_again() -> u32 {
    crate::log_err!("again");
    crate::gen::width() * 2
}
EOF
git init -q && git add -A && git commit -qm base
# "fix" commit:改两个宏的 body(触发 changed symbol 提取)
sed -i '' 's/\[err\]/[error]/' src/lib.rs
sed -i '' 's/\$val$/$val + 0/' src/lib.rs
git add -A && git commit -qm 'fix(macro): touch both macro bodies'
git log --oneline
