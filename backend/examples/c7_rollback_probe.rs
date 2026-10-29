//! C7 回滚探针：只打印一行 `ROLLBACK_PROBE=<回滚前>|<事务内>|<回滚后>`。
//!
//! 由 `scripts/verify/changelog_rollback.sh` 调用（不要直接拿来当生产工具）：
//!
//! ```bash
//! C7_DB_URL=postgres://… cargo run --quiet --features test-hooks --example c7_rollback_probe
//! ```
//!
//! # 为什么是 example 而不是 bin
//!
//! - `[[example]]` 带 `required-features = ["test-hooks"]`：不带 feature 时 cargo 连编译都不尝试，
//!   生产二进制（`target/release/cloudpivot`）里永远不会有这段代码路径；
//! - 它不是 `[[bin]]`，因此不会被 `cargo install` / 发布流程打包。
//!
//! # 连接串来源（优先级从高到低）
//!
//! 1. `C7_DB_URL`
//! 2. `ALUN_DATABASE_{USER,PASSWORD,HOST,PORT,NAME}`（脚本会 source `/etc/cloudpivot/cloudpivot-test.env`）
//! 3. 兜底 `postgres://cloudpivot@127.0.0.1:5432/cloudpivot_test`
//!
//! 诊断信息一律走 stderr，**stdout 只有一行 marker**——脚本据此断言，避免日志噪声污染门禁输出。

use alun::prelude::*;
use alun::uuid::Uuid;
use cloudpivot::business::changelog::test_hooks;
use cloudpivot::business::changelog::{Action, ChangeCtx, ChangeTarget, DiffInput};

/// 组装连接串（见文件头注释的优先级）。
fn database_url() -> String {
    if let Ok(url) = std::env::var("C7_DB_URL")
        && !url.trim().is_empty()
    {
        return url;
    }
    let pick = |key: &str, fallback: &str| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| fallback.to_string())
    };
    let user = pick("ALUN_DATABASE_USER", "cloudpivot");
    let password = std::env::var("ALUN_DATABASE_PASSWORD").unwrap_or_default();
    let host = pick("ALUN_DATABASE_HOST", "127.0.0.1");
    let port = pick("ALUN_DATABASE_PORT", "5432");
    let name = pick("ALUN_DATABASE_NAME", "cloudpivot_test");
    if password.is_empty() {
        format!("postgres://{user}@{host}:{port}/{name}")
    } else {
        format!("postgres://{user}:{password}@{host}:{port}/{name}")
    }
}

#[tokio::main]
async fn main() {
    let url = database_url();
    // 只打 host/db，不打口令
    eprintln!("探针连接：{}", redact(&url));
    let connection = match Db::connect(&url).await {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("FAIL: 连接数据库失败：{error}");
            std::process::exit(1);
        }
    };

    let resource_id = format!("c7-gate-{}", Uuid::now_v7());
    let before = serde_json::json!({"username": "c7-gate", "mobile": "13800001111", "status": 1});
    let after = serde_json::json!({"username": "c7-gate", "mobile": "13800009999", "status": 0});
    let ctx = ChangeCtx::api(None, Some("c7-rollback-gate"));

    let probe = match test_hooks::record_then_rollback(
        &connection,
        &ctx,
        ChangeTarget::of("sys_user", resource_id.as_str(), Action::Update, "修改用户"),
        DiffInput::update(&before, &after),
    )
    .await
    {
        Ok(probe) => probe,
        Err(error) => {
            eprintln!("FAIL: 探针执行失败 [{}]：{}", error.code, error.msg);
            std::process::exit(1);
        }
    };

    eprintln!(
        "探针明细：resource_id={} recorded={} before={} inside={} after={}",
        probe.resource_id, probe.recorded, probe.before, probe.inside, probe.after
    );
    // stdout 只这一行，格式即契约
    println!("ROLLBACK_PROBE={}", probe.flat());
    if probe.is_zero_residue() {
        std::process::exit(0);
    }
    eprintln!("FAIL: 期望 0|1|0（回滚零残留），实际 {}", probe.flat());
    std::process::exit(1);
}

/// 把连接串里的口令换成 `***`（诊断日志用）。
fn redact(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(scheme_end), Some(at)) if at > scheme_end => {
            let head = &url[..scheme_end + 3];
            let tail = &url[at..];
            let credentials = &url[scheme_end + 3..at];
            match credentials.find(':') {
                Some(colon) => format!("{head}{}:***{tail}", &credentials[..colon]),
                None => format!("{head}{credentials}{tail}"),
            }
        }
        _ => url.to_string(),
    }
}
