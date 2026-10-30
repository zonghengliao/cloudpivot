//! 批次 C · C3：公告扇出补偿的**幂等**真库集成测试（A-05 / A-06 / A-07 的可复跑证据）。
//!
//! # 跑法
//!
//! ```bash
//! su postgres -c "createdb -O cloudpivot cloudpivot_c3v"
//! C3_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_c3v' \
//!   cargo test --test batchC_c3_dispatch -- --nocapture
//! ```
//!
//! 未设置 `C3_DB_URL` 时打印 `SKIP` 并直接返回（`cargo test` 在无库机器上仍全绿）。
//!
//! # 测什么（对应架构 §5.5.2 的场景号）
//!
//! | 用例 | 断言 |
//! |------|------|
//! | 补偿回填（A-07） | `PUBLISHED` + `notice_id IS NULL` → 跑一轮补偿 → `notice_id` 回填、`sys_notice` **恰好 1 行**、`sys_notice_receiver` **恰好 2 行** |
//! | 幂等重放（A-06） | 再跑两轮（期间把 `notice_id` 清空模拟「回执丢失」）→ `sys_notice` **仍 1 行**、接收行仍 2 行、`notice_id` 与首轮相同 |
//! | 幂等键（A-05） | 落库的 `idempotency_key` == `announcement:{id}:v{version}` |
//! | 止血开关 | `sys_config.announcement.fanout_enabled=false` → 整轮 `skipped`、不补发；改回 `true` → 立刻能补 |
//! | 到期下线 | `expire_at` 已过 → `status = EXPIRED`（同一模块的另一个任务） |
//!
//! # 为什么全部塞进**一个** `#[tokio::test]`
//!
//! `cargo test` 默认多线程并发跑测试函数，而夹具用的是同一批固定 UUID：两个用例并行时
//! 「A 刚插完用户、B 的 cleanup 又把它删掉」会随机撞 `sys_user_pkey`（实测踩到）。
//! 串行写在一个用例里既消掉竞态，也避免两个用例抢同一个全局数据库连接。
//!
//! # 为什么用 SQL 造「发布后」的状态
//!
//! A-07 描述的前置状态就是「公告 `PUBLISHED` 且 `notice_id IS NULL`」（扇出失败后的合法中间态）。
//! 直接用 SQL 插入这个状态，比在测试里 hack 一个 dispatch 失败更贴近真实（真实失败来自
//! 通知表约束 / 数据库抖动，与公告侧代码无关）；「发布接口在扇出失败时确实留在这个状态」
//! 由真机 HTTP 验收（注入 CHECK 约束）单独证明。

use nik::prelude::*;
use tokio::sync::OnceCell;

use cloudpivot::business::announcement::maintenance;
use cloudpivot::business::announcement::model;

/// 夹具用户 A（接收人）。
const USER_A: &str = "c3000000-0000-4000-8000-00000000000a";
/// 夹具用户 B（接收人）。
const USER_B: &str = "c3000000-0000-4000-8000-00000000000b";
/// 夹具公告主键。
const ANN: &str = "c3000000-0000-4000-8000-0000000000f1";
/// 夹具公告编号。
const ANN_NO: &str = "AN-C3-DISPATCH-TEST";

static READY: OnceCell<bool> = OnceCell::const_new();

/// 把常量字符串解析成 UUID（`Value::from(&str)` 是 `Text`，绑到 `$1::uuid` 会被驱动拒绝）。
fn uid(raw: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(raw).expect("夹具 UUID 常量必须合法")
}

/// 连接验证库（一次）。
async fn ensure_db() -> bool {
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("C3_DB_URL") else {
                println!("SKIP: 未设置 C3_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("C3 已连接验证库");
                    true
                }
                Err(error) => {
                    println!("SKIP: 连接数据库失败：{error}");
                    false
                }
            }
        })
        .await
}

/// 清理夹具（顺序：通知 → 公告（scope/read/attachment 级联）→ 用户 → 配置）。
async fn cleanup() {
    let statements = [
        format!("DELETE FROM sys_notice WHERE idempotency_key LIKE 'announcement:{ANN}%'"),
        format!("DELETE FROM sys_announcement WHERE id = '{ANN}'"),
        format!("DELETE FROM sys_user WHERE id IN ('{USER_A}', '{USER_B}')"),
        format!(
            "DELETE FROM sys_config WHERE config_key = '{}'",
            model::CONFIG_FANOUT_ENABLED
        ),
    ];
    for sql in statements {
        if let Err(error) = db().execute_with_params(&sql, Vec::new()).await {
            println!("清理夹具失败（忽略）：{error}");
        }
    }
}

/// 建夹具：两个启用用户 + 一条「已发布但未扇出」的公告（`USER` 范围，2 个接收人）。
async fn seed() {
    cleanup().await;
    for (id, username) in [(USER_A, "c3_recv_a"), (USER_B, "c3_recv_b")] {
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password, real_name, status, created_at, updated_at) \
             VALUES (?::uuid, ?, 'x', ?, 1, now(), now())",
            vec![
                Value::from(uid(id)),
                Value::from(username),
                Value::from(format!("C3 接收人 {username}")),
            ],
        )
        .await
        .expect("插入夹具用户");
    }
    db().execute_with_params(
        "INSERT INTO sys_announcement \
             (id, ann_no, title, summary, content_md, content_html, level, scope_type, status, \
              publish_at, publisher_name, receiver_count, read_count, version, created_at, updated_at) \
         VALUES (?::uuid, ?, 'C3 扇出补偿测试公告', '补偿测试摘要', '## 正文', '<h2>正文</h2>', \
                 'IMPORTANT', 'USER', 'PUBLISHED', now(), 'C3 测试', 0, 0, 1, now(), now())",
        vec![Value::from(uid(ANN)), Value::from(ANN_NO)],
    )
    .await
    .expect("插入夹具公告");
    for user in [USER_A, USER_B] {
        db().execute_with_params(
            "INSERT INTO sys_announcement_scope (id, announcement_id, scope_type, target_id, created_at) \
             VALUES (gen_random_uuid(), ?::uuid, 'USER', ?::uuid, now())",
            vec![Value::from(uid(ANN)), Value::from(uid(user))],
        )
        .await
        .expect("插入夹具范围");
    }
}

/// `sys_notice` 行数（本夹具的公告）。
async fn notice_rows() -> i64 {
    db().scalar_with_params(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key LIKE ?::text",
        vec![Value::from(format!("announcement:{ANN}%"))],
    )
    .await
    .expect("统计通知行数")
}

/// `sys_notice_receiver` 行数（本夹具的公告）。
async fn receiver_rows() -> i64 {
    db().scalar_with_params(
        "SELECT count(*) FROM sys_notice_receiver r JOIN sys_notice n ON n.id = r.notice_id \
          WHERE n.idempotency_key LIKE ?::text",
        vec![Value::from(format!("announcement:{ANN}%"))],
    )
    .await
    .expect("统计接收行数")
}

/// 当前 `notice_id`（`None` = 尚未扇出）。
async fn notice_id() -> Option<uuid::Uuid> {
    let rows = db()
        .find_with_params(
            "SELECT notice_id FROM sys_announcement WHERE id = ?::uuid",
            vec![Value::from(uid(ANN))],
        )
        .await
        .expect("查 notice_id");
    rows.first()
        .and_then(|row| row.get_typed("notice_id").ok().flatten())
}

/// 把 `notice_id` 清空（模拟「扇出成功但回执丢失」：补偿必须能重放且不重复发送）。
async fn forget_notice_id() {
    db().execute_with_params(
        "UPDATE sys_announcement SET notice_id = NULL WHERE id = ?::uuid",
        vec![Value::from(uid(ANN))],
    )
    .await
    .expect("清空 notice_id");
}

/// 写 / 删全局扇出开关。
async fn set_fanout(enabled: bool) {
    if enabled {
        db().execute_with_params(
            "DELETE FROM sys_config WHERE config_key = ?",
            vec![Value::from(model::CONFIG_FANOUT_ENABLED)],
        )
        .await
        .expect("删除扇出开关");
    } else {
        db().execute_with_params(
            "INSERT INTO sys_config (id, config_key, config_value, config_type, status, created_at, updated_at) \
             VALUES (gen_random_uuid(), ?, 'false', 'BOOLEAN', 1, now(), now())",
            vec![Value::from(model::CONFIG_FANOUT_ENABLED)],
        )
        .await
        .expect("写入扇出开关");
    }
}

/// A-07 + A-06 + A-05 + 止血开关 + 到期下线：一趟跑完（串行，避免夹具竞态）。
#[tokio::test]
async fn retry_compensation_is_idempotent() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    seed().await;
    set_fanout(true).await;

    // ---------- ① 首轮补偿：PUBLISHED + notice_id IS NULL → 回填 ----------
    let first = maintenance::run_dispatch_retry().await.expect("首轮补偿");
    assert_eq!(first.repaired, 1, "应补偿 1 条：{}", first.summary());
    let first_notice = notice_id().await.expect("首轮必须回填 notice_id");
    assert_eq!(notice_rows().await, 1, "首轮补偿后应恰好 1 条通知");
    assert_eq!(receiver_rows().await, 2, "两个 USER 范围接收人");

    // 幂等键形态（A-05）
    let key: String = db()
        .scalar_with_params(
            "SELECT idempotency_key FROM sys_notice WHERE id = ?::uuid",
            vec![Value::from(&first_notice)],
        )
        .await
        .expect("查幂等键");
    assert_eq!(key, format!("announcement:{ANN}:v1"));

    // ---------- ② 模拟回执丢失后连续重放两轮 → 仍 1 行通知 / 2 行接收人（A-06）----------
    for round in 2..=3 {
        forget_notice_id().await;
        let report = maintenance::run_dispatch_retry().await.expect("重放轮次");
        assert_eq!(
            report.repaired,
            1,
            "第 {round} 轮应再次回填：{}",
            report.summary()
        );
        assert_eq!(
            notice_id().await,
            Some(first_notice),
            "第 {round} 轮必须回填同一个通知"
        );
        assert_eq!(
            notice_rows().await,
            1,
            "第 {round} 轮后 sys_notice 仍必须 1 行"
        );
        assert_eq!(receiver_rows().await, 2, "第 {round} 轮后接收行仍必须 2 行");
    }

    // ---------- ③ 无事可做时是空转（不会反复重建）----------
    let idle = maintenance::run_dispatch_retry().await.expect("空转轮");
    assert_eq!(idle.scanned, 0, "{}", idle.summary());
    assert_eq!(idle.repaired, 0);
    println!("[A-06/A-07] 补偿回填 + 连续两轮重放：sys_notice 恒 1 行、接收行恒 2 行 ✓");

    // ---------- ④ 止血开关：关闭时整轮跳过，打开后立刻能补 ----------
    forget_notice_id().await;
    set_fanout(false).await;
    let skipped = maintenance::run_dispatch_retry().await.expect("关闭态补偿");
    assert_eq!(skipped.scanned, 1, "{}", skipped.summary());
    assert_eq!(skipped.skipped, 1, "关闭态必须整轮跳过");
    assert_eq!(skipped.repaired, 0);
    assert!(notice_id().await.is_none(), "关闭态不得补发");

    set_fanout(true).await;
    let repaired = maintenance::run_dispatch_retry().await.expect("打开态补偿");
    assert_eq!(repaired.repaired, 1, "{}", repaired.summary());
    assert!(notice_id().await.is_some());
    assert_eq!(notice_rows().await, 1, "开关来回切换也不得产生第二行通知");
    println!("[止血开关] 关闭时 skipped=1 不补发；打开后 repaired=1 且仍 1 行 ✓");

    // ---------- ⑤ 到期下线（同一模块的另一个任务）----------
    db().execute_with_params(
        // `ck_sys_announcement_expire` 要求 `expire_at > publish_at`，
        // 所以把发布时间也往前挪，否则改过期时间会直接撞约束（实测踩到）
        "UPDATE sys_announcement \
            SET publish_at = now() - interval '10 minutes', expire_at = now() - interval '1 minute' \
          WHERE id = ?::uuid",
        vec![Value::from(uid(ANN))],
    )
    .await
    .expect("把夹具公告设为已过期");
    let expired = maintenance::run_auto_expire().await.expect("到期下线任务");
    assert_eq!(expired, 1, "应下线 1 条");
    let status: String = db()
        .scalar_with_params(
            "SELECT status FROM sys_announcement WHERE id = ?::uuid",
            vec![Value::from(uid(ANN))],
        )
        .await
        .expect("查状态");
    assert_eq!(status, "EXPIRED", "到期后状态必须变 EXPIRED");
    println!("[auto_expire] expire_at 已过 → status=EXPIRED ✓");

    cleanup().await;
}
