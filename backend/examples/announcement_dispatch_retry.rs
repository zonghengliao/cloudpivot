//! 公告扇出补偿的**手动触发**工具（批次 C · C3）。
//!
//! # 为什么需要它
//!
//! 补偿任务是 cron（`0 */10 * * * *`，每 10 分钟）。运维在「刚修完故障、不想等 10 分钟」
//! 的场景下需要一个入口，而 HTTP 层**没有**触发内置任务的通道：
//! `POST /support/job/{id}/trigger` 只认 `task::run_handler` 里注册的 5 个日志清理处理器
//! （`sys_job.invoke_target` 白名单），内置任务不在其中。
//!
//! 因此提供一个 example 直接调用**同一个**业务函数
//! [`cloudpivot::business::announcement::maintenance::run_dispatch_retry`]——
//! 与 cron 触发完全等价，不复制任何逻辑。
//!
//! # 用法
//!
//! ```bash
//! # 跑一轮（与 cron 同参数：单批 50 条）
//! C3_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot' \
//!   cargo run --example announcement_dispatch_retry
//!
//! # 连跑三轮（常用于确认幂等：跑多少次 sys_notice 都只有一行）
//! C3_DB_URL='...' cargo run --example announcement_dispatch_retry -- 3
//! ```
//!
//! 输出形如 `round 1: announcement_dispatch_retry: scanned=2 repaired=2 failed=0 skipped=0`。
//!
//! # 注意
//!
//! - 需要**运行期之外的独立连接**：本工具自己 `Db::connect`，不要与正在运行的服务共用同名
//!   进程（不会互相干扰，但两边都会扫同一批待补偿行 —— 靠幂等键保证不重复发送）；
//! - 全局开关 `sys_config.announcement.fanout_enabled=false` 时整轮 `skipped`（止血优先）。

use nik::prelude::*;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("C3_DB_URL").map_err(|_| {
        "缺少 C3_DB_URL（形如 postgres://user:pass@127.0.0.1:5432/cloudpivot）".to_string()
    })?;
    let rounds: usize = std::env::args()
        .nth(1)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(1);
    let connection = Db::connect(&url).await?;
    set_db(connection).map_err(|error| error.to_string())?;

    for round in 1..=rounds {
        let report = cloudpivot::business::announcement::maintenance::run_dispatch_retry()
            .await
            .map_err(|error| error.to_string())?;
        println!("round {round}: {}", report.summary());
    }
    Ok(())
}
