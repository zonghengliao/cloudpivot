//! FIX-04 · BUG-D-05 真库验收：公告软删 → 关联通知**同事务**下线。
//!
//! # 用法（144 实测口径）
//!
//! ```bash
//! su postgres -c "createdb -O alun cloudpivot_fix04"
//! # 先起一次实例把迁移跑完（或复用已迁移的库）
//! export FIX04_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_fix04"
//! cd backend && cargo test --test fix04_notice_offline -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `FIX04_DB_URL` 时全部用例打印 `SKIP` 并返回（`cargo test` 在无库机器上仍全绿）。
//! 一键脚本：`scripts/verify/fix04_notice_offline.sh`（自动建库 / 迁移 / 跑本文件）。
//!
//! # 覆盖的断言（对应卡面验收 ①~⑤ + 红线 2/3）
//!
//! | 用例 | 断言 |
//! |---|---|
//! | T1 | 删公告 → `sys_notice.deleted_at` 有值；两个接收人行 `is_deleted = TRUE` |
//! | T2 | **已读回执不动**：`sys_notice_receiver` 行数不变、`read_flag`/`read_at` 一字未改、`sys_announcement_read` 仍在 |
//! | T3 | 徽标数正确减少：`count_unread` 由 1 → 0 |
//! | T4 | 消息中心列表查不到该通知：`page_my_notices` 由 1 → 0 |
//! | T5 | 幂等：第二次删返回 `(0, 空报告)`，不报错、不重复改行 |
//! | T6 | **同事务**：事务内后续语句失败 → 公告**未**删、通知**仍在线**（整段回滚） |
//! | T7 | 已建未发的定时通知（`dispatch_status='PENDING'`）被置 `CANCELED`，不会到点再扇出 |
//! | T8 | 误伤检查：别的公告 / 别的业务类型（`biz_type='workflow'`）的通知一行不动 |
//! | T9 | 从未扇出的公告（无通知行）删除不报错、报告为空 |
//!
//! ⚠️ 全部夹具带 `fix04-` 前缀，开头结尾各清一次；文件内用例串行跑
//! （`--test-threads=1`），互不干扰。

use cloudpivot::business::announcement::model as ann;
use cloudpivot::business::notice::model as notice;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 夹具前缀（`ann_no` / `notice_no` 都带它，便于清理）。
const TAG: &str = "fix04-";

/// 初始化全局数据库（进程内只初始化一次）；返回 `false` 表示跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("FIX04_DB_URL") else {
                println!("SKIP: 未设置 FIX04_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("FIX-04 已连接验证库");
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

/// 清掉本文件的夹具（接收行 → 通知 → 公告已读 → 公告）。
async fn cleanup() {
    let pattern = format!("{TAG}%");
    // 接收行 / 投递行随 notice 走（外键 CASCADE 只对硬删生效），这里按通知 ID 反查删除
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_notice_receiver \
              WHERE notice_id IN (SELECT id FROM sys_notice WHERE notice_no LIKE ?::text)",
            vec![Value::from(pattern.clone())],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_notice WHERE notice_no LIKE ?::text",
            vec![Value::from(pattern.clone())],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_announcement_read \
              WHERE announcement_id IN (SELECT id FROM sys_announcement WHERE ann_no LIKE ?::text)",
            vec![Value::from(pattern.clone())],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_announcement WHERE ann_no LIKE ?::text",
            vec![Value::from(pattern)],
        )
        .await;
}

/// 一条公告 + 一条通知 + 两个接收人（一个已读、一个未读）+ 一条公告已读回执。
struct Fixture {
    /// 公告主键。
    announcement_id: Uuid,
    /// 通知主键。
    notice_id: Uuid,
    /// 已读的接收人。
    user_read: Uuid,
    /// 未读的接收人（徽标数就是它）。
    user_unread: Uuid,
}

/// 造夹具。
///
/// # 参数
///
/// - `status`：公告状态（`DRAFT` / `PUBLISHED` / `OFFLINE` / `EXPIRED`）；
/// - `with_notice`：是否造关联通知（`false` 模拟「发布后扇出失败、`notice_id IS NULL`」）；
/// - `dispatch_status`：通知投递状态（`SENT` 正常 / `PENDING` 定时未发）。
async fn seed(status: &str, with_notice: bool, dispatch_status: &str) -> Fixture {
    let announcement_id = Uuid::now_v7();
    let notice_id = Uuid::now_v7();
    let user_read = Uuid::now_v7();
    let user_unread = Uuid::now_v7();
    let ann_no = format!("{TAG}A{}", &announcement_id.simple().to_string()[..12]);
    let notice_no = format!("{TAG}N{}", &notice_id.simple().to_string()[..12]);

    db().execute_with_params(
        "INSERT INTO sys_announcement \
             (id, ann_no, title, summary, content_md, content_html, level, scope_type, status, \
              publish_at, publisher_id, version, receiver_count, read_count) \
         VALUES (?::uuid, ?::text, 'FIX-04 夹具公告', '摘要', '正文', '<p>正文</p>', 'NORMAL', 'ALL', \
                 ?::text, \
                 CASE WHEN ?::text = 'DRAFT' THEN NULL ELSE now() END, \
                 ?::uuid, 1, 2, 1)",
        vec![
            Value::from(announcement_id),
            Value::from(ann_no.clone()),
            Value::from(status.to_string()),
            Value::from(status.to_string()),
            Value::from(Uuid::now_v7()),
        ],
    )
    .await
    .expect("插入公告夹具");

    if with_notice {
        db().execute_with_params(
            "INSERT INTO sys_notice \
                 (id, notice_no, notice_type, level, title, content, content_format, link_url, \
                  biz_type, biz_id, sender_name, target_type, target_ids, receiver_count, \
                  read_count, dispatch_status, status) \
             VALUES (?::uuid, ?::text, 'CIRCULAR', 'INFO', 'FIX-04 夹具公告', '正文', 'TEXT', \
                     ?::text, ?::text, ?::text, '公告', 'ALL', '[]'::jsonb, 2, 1, ?::text, 1)",
            vec![
                Value::from(notice_id),
                Value::from(notice_no.clone()),
                Value::from(format!("/announcement/{announcement_id}")),
                Value::from(notice::BIZ_TYPE_ANNOUNCEMENT.to_string()),
                Value::from(announcement_id.to_string()),
                Value::from(dispatch_status.to_string()),
            ],
        )
        .await
        .expect("插入通知夹具");

        for (user, read) in [(user_read, true), (user_unread, false)] {
            db().execute_with_params(
                "INSERT INTO sys_notice_receiver (notice_id, user_id, read_flag, read_at) \
                 VALUES (?::uuid, ?::uuid, ?, CASE WHEN ? THEN now() ELSE NULL END)",
                vec![
                    Value::from(notice_id),
                    Value::from(user),
                    Value::from(read),
                    Value::from(read),
                ],
            )
            .await
            .expect("插入接收人夹具");
        }

        // 公告侧已读回执（与通知侧是两张表，删公告后都必须还在）
        db().execute_with_params(
            "INSERT INTO sys_announcement_read (announcement_id, user_id) VALUES (?::uuid, ?::uuid)",
            vec![Value::from(announcement_id), Value::from(user_read)],
        )
        .await
        .expect("插入公告已读夹具");
    }

    Fixture {
        announcement_id,
        notice_id,
        user_read,
        user_unread,
    }
}

/// 事务内执行「删公告（含通知下线）」，返回软删行数 + 下线报告。
async fn delete_in_tx(id: Uuid, operator: Uuid) -> DbResult<(u64, notice::BizWithdrawReport)> {
    db().transaction(move |tx| async move {
        let result = ann::soft_delete_with_notice(&tx, &id, operator).await?;
        Ok::<_, DbError>(result)
    })
    .await
}

/// 读某条通知的 `(deleted_at 是否有值, dispatch_status)`。
async fn notice_row(id: Uuid) -> (bool, String) {
    let rows = db()
        .find_with_params(
            "SELECT deleted_at IS NOT NULL AS gone, dispatch_status FROM sys_notice WHERE id = ?::uuid",
            vec![Value::from(id)],
        )
        .await
        .expect("读通知行");
    let row = rows.first().expect("通知行必须存在（本模块不做硬删）");
    (
        row.get_typed("gone").expect("gone"),
        row.get_typed("dispatch_status").expect("dispatch_status"),
    )
}

/// 读某条公告的 `deleted_at` 是否有值。
async fn announcement_deleted(id: Uuid) -> bool {
    db().scalar_with_params(
        "SELECT (deleted_at IS NOT NULL) FROM sys_announcement WHERE id = ?::uuid",
        vec![Value::from(id)],
    )
    .await
    .expect("读公告行")
}

// ==================== T1 通知随公告一起下线 ====================

#[tokio::test]
async fn t1_delete_offlines_notice_and_receivers() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    let f = seed("PUBLISHED", true, "SENT").await;
    let operator = Uuid::now_v7();

    let (affected, report) = delete_in_tx(f.announcement_id, operator).await.expect("删公告成功");
    assert_eq!(affected, 1, "公告本身要软删 1 行");
    assert_eq!(report.notices, 1, "关联通知要下线 1 行");
    assert_eq!(report.receivers, 2, "两个接收人都要标用户侧删除");

    let (gone, _) = notice_row(f.notice_id).await;
    assert!(gone, "T1: sys_notice.deleted_at 必须有值（BUG-D-05 的原始症状点）");
    assert!(announcement_deleted(f.announcement_id).await, "T1: 公告必须软删");

    let left: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM sys_notice_receiver \
              WHERE notice_id = ?::uuid AND is_deleted = FALSE",
            vec![Value::from(f.notice_id)],
        )
        .await
        .expect("数接收行");
    assert_eq!(left, 0, "T1: 不应再有未标删的接收行");
    println!("[PASS] t1_delete_offlines_notice_and_receivers: notices=1 receivers=2");
    cleanup().await;
}

// ==================== T2 已读回执不动（红线 2） ====================

#[tokio::test]
async fn t2_read_receipt_is_preserved() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    let f = seed("PUBLISHED", true, "SENT").await;

    let before = db()
        .find_with_params(
            "SELECT COUNT(*) AS total, \
                    COUNT(*) FILTER (WHERE read_flag) AS read_rows, \
                    COUNT(*) FILTER (WHERE read_at IS NOT NULL) AS read_at_rows \
               FROM sys_notice_receiver WHERE notice_id = ?::uuid",
            vec![Value::from(f.notice_id)],
        )
        .await
        .expect("删除前统计");
    let before_total: i64 = before[0].get_typed("total").expect("total");
    let before_read: i64 = before[0].get_typed("read_rows").expect("read_rows");

    delete_in_tx(f.announcement_id, Uuid::now_v7())
        .await
        .expect("删公告成功");

    let after = db()
        .find_with_params(
            "SELECT COUNT(*) AS total, \
                    COUNT(*) FILTER (WHERE read_flag) AS read_rows, \
                    COUNT(*) FILTER (WHERE read_at IS NOT NULL) AS read_at_rows \
               FROM sys_notice_receiver WHERE notice_id = ?::uuid",
            vec![Value::from(f.notice_id)],
        )
        .await
        .expect("删除后统计");
    let after_total: i64 = after[0].get_typed("total").expect("total");
    let after_read: i64 = after[0].get_typed("read_rows").expect("read_rows");
    let after_read_at: i64 = after[0].get_typed("read_at_rows").expect("read_at_rows");

    assert_eq!(after_total, before_total, "T2: 接收行**一条都不许删**（审计要）");
    assert_eq!(after_read, before_read, "T2: read_flag 不许改");
    assert_eq!(after_read_at, after_read, "T2: 已读必有 read_at，不许丢");

    let receipt: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM sys_announcement_read WHERE announcement_id = ?::uuid",
            vec![Value::from(f.announcement_id)],
        )
        .await
        .expect("数公告已读");
    assert_eq!(receipt, 1, "T2: 公告已读回执（sys_announcement_read）必须还在");

    // 已读那一位的 read_flag / read_at 必须原样还在（按人核对，不是只看总数）
    let (read_flag, has_read_at): (bool, bool) = {
        let rows = db()
            .find_with_params(
                "SELECT read_flag, (read_at IS NOT NULL) AS has_read_at \
                   FROM sys_notice_receiver WHERE notice_id = ?::uuid AND user_id = ?::uuid",
                vec![Value::from(f.notice_id), Value::from(f.user_read)],
            )
            .await
            .expect("读已读接收行");
        let row = rows.first().expect("已读接收行必须还在");
        (
            row.get_typed("read_flag").expect("read_flag"),
            row.get_typed("has_read_at").expect("has_read_at"),
        )
    };
    assert!(read_flag, "T2: 已读那一位的 read_flag 必须仍是 TRUE");
    assert!(has_read_at, "T2: 已读那一位的 read_at 必须还在");
    println!("[PASS] t2_read_receipt_is_preserved: receivers={after_total} read={after_read} 未删未改");
    cleanup().await;
}

// ==================== T3 徽标数正确减少 ====================

#[tokio::test]
async fn t3_unread_badge_drops() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    let f = seed("PUBLISHED", true, "SENT").await;

    let before = notice::count_unread(&f.user_unread).await.expect("未读数");
    assert_eq!(before, 1, "T3 前置：未读用户徽标应为 1");

    delete_in_tx(f.announcement_id, Uuid::now_v7())
        .await
        .expect("删公告成功");

    let after = notice::count_unread(&f.user_unread).await.expect("未读数");
    assert_eq!(after, 0, "T3: 删公告后徽标必须减到 0（不能留「列表空 + 徽标 1」）");
    println!("[PASS] t3_unread_badge_drops: 1 → 0");
    cleanup().await;
}

// ==================== T4 消息中心列表查不到 ====================

#[tokio::test]
async fn t4_message_center_hides_notice() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    let f = seed("PUBLISHED", true, "SENT").await;

    let before = notice::page_my_notices(&f.user_unread, None, 1, 20)
        .await
        .expect("我的通知");
    assert_eq!(before.total, 1, "T4 前置：消息中心应有 1 条");

    delete_in_tx(f.announcement_id, Uuid::now_v7())
        .await
        .expect("删公告成功");

    let after = notice::page_my_notices(&f.user_unread, None, 1, 20)
        .await
        .expect("我的通知");
    assert_eq!(after.total, 0, "T4: 消息中心必须查不到已删公告的通知");
    assert!(after.list.is_empty(), "T4: 列表必须为空");
    println!("[PASS] t4_message_center_hides_notice: 1 → 0");
    cleanup().await;
}

// ==================== T5 幂等 ====================

#[tokio::test]
async fn t5_second_delete_is_idempotent() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    let f = seed("PUBLISHED", true, "SENT").await;
    let operator = Uuid::now_v7();

    let first = delete_in_tx(f.announcement_id, operator).await.expect("首次删公告成功");
    assert_eq!(first.0, 1);
    assert_eq!(first.1.notices, 1);

    let second = delete_in_tx(f.announcement_id, operator)
        .await
        .expect("二次删公告不应该报错");
    assert_eq!(second.0, 0, "T5: 公告已删，第二次应影响 0 行");
    assert!(second.1.is_empty(), "T5: 第二次不应再改动任何通知行（幂等）");
    println!("[PASS] t5_second_delete_is_idempotent: 第二次 0 行、报告为空");
    cleanup().await;
}

// ==================== T6 同事务回滚（红线 3） ====================

#[tokio::test]
async fn t6_failure_rolls_back_both() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    let f = seed("PUBLISHED", true, "SENT").await;

    // 事务里先删公告（含通知下线），再故意制造一条 SQL 错误 → 整段必须回滚
    let outcome = db()
        .transaction(|tx| async move {
            ann::soft_delete_with_notice(&tx, &f.announcement_id, Uuid::now_v7()).await?;
            tx.execute_with_params("SELECT * FROM fix04_not_such_table", Vec::new())
                .await?;
            Ok::<(), DbError>(())
        })
        .await;
    assert!(outcome.is_err(), "T6: 事务应当因非法 SQL 失败");

    assert!(
        !announcement_deleted(f.announcement_id).await,
        "T6: 公告软删必须随之回滚（不许「公告删了、通知还在」）"
    );
    let (gone, _) = notice_row(f.notice_id).await;
    assert!(!gone, "T6: 通知下线也必须回滚 —— 这就是「同事务」的证据");
    let left: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM sys_notice_receiver \
              WHERE notice_id = ?::uuid AND is_deleted = FALSE",
            vec![Value::from(f.notice_id)],
        )
        .await
        .expect("数接收行");
    assert_eq!(left, 2, "T6: 接收行的 is_deleted 也必须回滚");
    println!("[PASS] t6_failure_rolls_back_both: 公告 / 通知 / 接收行三者全部回滚");
    cleanup().await;
}

// ==================== T7 定时未发通知被取消 ====================

#[tokio::test]
async fn t7_pending_scheduled_notice_canceled() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    let f = seed("PUBLISHED", true, "PENDING").await;

    delete_in_tx(f.announcement_id, Uuid::now_v7())
        .await
        .expect("删公告成功");

    let (gone, dispatch_status) = notice_row(f.notice_id).await;
    assert!(gone, "T7: 通知必须下线");
    assert_eq!(
        dispatch_status, "CANCELED",
        "T7: 已建未发的定时通知要置 CANCELED，不能到点再扇出"
    );
    println!("[PASS] t7_pending_scheduled_notice_canceled: PENDING → CANCELED");
    cleanup().await;
}

// ==================== T8 不误伤别的业务 ====================

#[tokio::test]
async fn t8_other_biz_notice_untouched() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    let f = seed("PUBLISHED", true, "SENT").await;

    // 同 biz_id 但不同 biz_type（流程通知）+ 同 biz_type 但不同 biz_id（另一条公告）
    let workflow_notice = Uuid::now_v7();
    let other_ann_notice = Uuid::now_v7();
    let other_ann = Uuid::now_v7();
    for (id, biz_type, biz_id, no) in [
        (
            workflow_notice,
            "workflow",
            f.announcement_id.to_string(),
            format!("{TAG}W{}", &workflow_notice.simple().to_string()[..8]),
        ),
        (
            other_ann_notice,
            notice::BIZ_TYPE_ANNOUNCEMENT,
            other_ann.to_string(),
            format!("{TAG}X{}", &other_ann_notice.simple().to_string()[..8]),
        ),
    ] {
        db().execute_with_params(
            "INSERT INTO sys_notice \
                 (id, notice_no, notice_type, level, title, content, content_format, biz_type, \
                  biz_id, target_type, target_ids, receiver_count, read_count, dispatch_status, status) \
             VALUES (?::uuid, ?::text, 'SYSTEM', 'INFO', '旁路夹具', '正文', 'TEXT', ?::text, \
                     ?::text, 'ALL', '[]'::jsonb, 0, 0, 'SENT', 1)",
            vec![
                Value::from(id),
                Value::from(no),
                Value::from(biz_type.to_string()),
                Value::from(biz_id),
            ],
        )
        .await
        .expect("插入旁路通知");
    }

    delete_in_tx(f.announcement_id, Uuid::now_v7())
        .await
        .expect("删公告成功");

    assert!(
        !notice_row(workflow_notice).await.0,
        "T8: 不同 biz_type 的通知不许被误杀"
    );
    assert!(
        !notice_row(other_ann_notice).await.0,
        "T8: 不同 biz_id 的通知不许被误杀"
    );
    println!("[PASS] t8_other_biz_notice_untouched: 旁路通知 2 条均未受影响");

    // 清掉旁路夹具
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_notice WHERE id IN (?::uuid, ?::uuid)",
            vec![Value::from(workflow_notice), Value::from(other_ann_notice)],
        )
        .await;
    cleanup().await;
}

// ==================== T9 没扇出过的公告 ====================

#[tokio::test]
async fn t9_announcement_without_notice_is_noop() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    // 草稿：绝不能有通知（发布才会扇出），删除不该报错
    let f = seed("DRAFT", false, "SENT").await;

    let (affected, report) = delete_in_tx(f.announcement_id, Uuid::now_v7())
        .await
        .expect("删无通知的公告不应报错");
    assert_eq!(affected, 1, "T9: 公告本身要软删");
    assert!(report.is_empty(), "T9: 没有通知行 → 报告为空");
    assert!(announcement_deleted(f.announcement_id).await, "T9: 公告必须软删");
    println!("[PASS] t9_announcement_without_notice_is_noop: 0 通知、不报错");
    cleanup().await;
}
