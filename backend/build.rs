//! 构建期元信息注入。
//!
//! 往编译产物里塞两个**只能在这一刻确定**的值：
//!
//! | 环境变量 | 用途 | 取法 |
//! |----------|------|------|
//! | `CLOUDPIVOT_GIT_COMMIT` | 服务器状态页「环境信息 → commit」与诊断包 `info.json` | 构建时的 `git rev-parse --short HEAD`（可用同名环境变量覆盖） |
//! | `CLOUDPIVOT_RUSTC_VERSION` | 同上「运行时 → Rust 版本」 | 构建时执行 `$RUSTC --version` |
//!
//! # 为什么不用 `option_env!("CARGO_PKG_RUST_VERSION")`
//!
//! 那个值来自 `Cargo.toml` 的 `rust-version` 字段，是**最低要求**而不是「实际编译用的版本」。
//! 排障时用户问的是「这个二进制是哪个 rustc 编的」，两者差一个版本档就会误导。
//!
//! # 失败策略
//!
//! 取不到 git（源码包 / 无 `.git` 的构建环境）不报错，只写 `unknown`：
//! 构建不能因为「没有版本号」这种可选元信息而失败（那是把监控信息变成构建依赖）。
//!
//! `cargo:rerun-if-changed` 只盯 `build.rs` 与 `.git/HEAD`：
//! 提交后重新构建会重新取一次 commit，但不会因为业务源码变动而反复跑 git。

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // 分支切换 / 新提交都会改 HEAD，重新构建即可拿到新的短哈希
    println!("cargo:rerun-if-changed=../.git/HEAD");

    println!("cargo:rustc-env=CLOUDPIVOT_GIT_COMMIT={}", git_commit());
    println!("cargo:rustc-env=CLOUDPIVOT_RUSTC_VERSION={}", rustc_version());
}

/// 构建时的 git 短哈希（环境变量优先，其次 `git rev-parse`，都没有则 `unknown`）。
fn git_commit() -> String {
    if let Ok(value) = std::env::var("CLOUDPIVOT_GIT_COMMIT")
        && !value.trim().is_empty()
    {
        return value.trim().to_string();
    }
    let output = Command::new("git").args(["rev-parse", "--short", "HEAD"]).output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "unknown".to_string(),
    }
}

/// 构建时的 rustc 版本（如 `rustc 1.94.1 (29ea6fb6f 2026-03-24)`）。
fn rustc_version() -> String {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = Command::new(rustc).arg("--version").output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "unknown".to_string(),
    }
}
