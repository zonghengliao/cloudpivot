//! ② 采购订单超期未收货提醒。
//!
//! # 判据
//!
//! ```text
//! biz_purchase_order 未删除
//!   AND status IN ('APPROVED', 'PARTIAL_RECEIVED')  -- 「已承诺但没到货」
//!   AND expect_date IS NOT NULL
//!   AND expect_date < 今天（业务日）                  -- 超过期望到货日
//! ```
//!
//! **为什么只算这两个状态**：
//!
//! | 状态 | 是否提醒 | 为什么 |
//! |------|----------|--------|
//! | `DRAFT` / `SUBMITTED` | 否 | 还没批下来，供应商压根不知道有这张单，超期不是供应商的问题 |
//! | `APPROVED` | **是** | 已生效待收货：这是本提醒的主战场 |
//! | `PARTIAL_RECEIVED` | **是** | 收了一部分，剩下的还在路上 —— 「未收完」正是卡面口径 |
//! | `RECEIVED` / `CLOSED` / `CANCELLED` | 否 | 已收完 / 已关闭 / 已取消，没有「该催的事」 |
//!
//! **提醒频率**：状态型提醒（只要没处理就永远成立），因此不是每天轰炸 ——
//! 台账 + `sys_config.erp.reminder.po_overdue.realert_days`（缺省 3 天）决定重提醒间隔，
//! 由 [`crate::business::erp::reminder::run_kind`] 统一判定。
//!
//! # 收件人
//!
//! 采购单制单人（谁下的单谁去催）+ 采购经理（`PURCHASE_MANAGER` 角色下的启用用户）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::reminder::{self, ReminderMessage, SCAN_BATCH};

/// 采购侧收件人角色：采购经理。
pub const PURCHASE_ROLES: [&str; 1] = ["PURCHASE_MANAGER"];

/// 逾期天数 → 台账档位（`OVERDUE-{天数}`，最多 4 位；超过 9999 天按 9999 记，不撑破列宽）。
pub fn stage_of(overdue_days: i64) -> String {
    format!("OVERDUE-{}", overdue_days.clamp(1, 9999))
}

/// 一行待提醒的采购单（扫描产物）。
#[derive(Debug, Clone)]
pub struct PurchaseOverdueCandidate {
    /// 采购单主键。
    pub id: Uuid,
    /// 采购单号。
    pub order_no: String,
    /// 供应商名称（查不到写「供应商未知」）。
    pub supplier_name: String,
    /// 期望到货日。
    pub expect_date: NaiveDate,
    /// 收货仓库名称。
    pub warehouse_name: String,
    /// 逾期天数（业务日 - 期望到货日，至少 1）。
    pub overdue_days: i64,
    /// 采购单制单人。
    pub created_by: Option<Uuid>,
}

/// 组装一条提醒消息（收件人 = 制单人 + 采购经理）。
pub fn message_of(candidate: &PurchaseOverdueCandidate, managers: &[Uuid]) -> ReminderMessage {
    let recipients =
        reminder::merge_recipients(std::iter::once(candidate.created_by).chain(managers.iter().copied().map(Some)));
    let expect_date = candidate.expect_date.format("%Y-%m-%d").to_string();
    let overdue_days = candidate.overdue_days;
    let title = format!("采购单超期 {overdue_days} 天未收货：{}", candidate.order_no);
    let content = format!(
        "采购单 {}（供应商：{}）期望到货日 {}，已逾期 {} 天仍未收完（收货仓库：{}）。\n\
         请催供应商交货，或在本单上改期 / 关闭，避免「已审批未到货」长期悬空拖累计划与应付。",
        candidate.order_no, candidate.supplier_name, expect_date, overdue_days, candidate.warehouse_name
    );
    let vars = serde_json::json!({
        "poNo": candidate.order_no,
        "supplierName": candidate.supplier_name,
        "expectDate": expect_date,
        "overdueDays": overdue_days.to_string(),
        "warehouseName": candidate.warehouse_name,
        "action": "催货或改期",
    });
    ReminderMessage {
        doc_id: candidate.id,
        doc_no: candidate.order_no.clone(),
        stage: stage_of(overdue_days),
        title,
        content,
        vars,
        recipients,
        recipient_rule: "采购单制单人 + 采购经理（PURCHASE_MANAGER）".to_string(),
    }
}

/// 扫描「超期未收完」的采购单并组装成待发消息（只读查询，不发通知）。
pub async fn scan(now: DateTime<Utc>) -> DbResult<Vec<ReminderMessage>> {
    let day = reminder::business_day(now);
    let candidates = scan_candidates(day).await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let managers = reminder::users_of_roles(&PURCHASE_ROLES).await?;
    if managers.is_empty() {
        tracing::warn!(
            roles = ?PURCHASE_ROLES,
            "采购超期提醒：采购经理角色下没有启用用户，本轮只有制单人会收到（请补角色成员）"
        );
    }
    Ok(candidates
        .iter()
        .map(|candidate| message_of(candidate, &managers))
        .collect())
}

/// 扫描候选行（`expect_date < 业务日`）。
pub async fn scan_candidates(day: NaiveDate) -> DbResult<Vec<PurchaseOverdueCandidate>> {
    // SCOPE-EXEMPT: 采购单没有 dept_id / 归属人列（V0137），提醒是作业口径，
    // 收件人由制单人与采购经理决定；注入数据范围会让别人的超期单永不提醒。
    let rows = db()
        .query_sql(
            "SELECT o.id AS id, o.order_no AS order_no, \
                    COALESCE(s.supplier_name, '供应商未知') AS supplier_name, \
                    o.expect_date::text AS expect_date, \
                    COALESCE(w.warehouse_name, '未知仓库') AS warehouse_name, \
                    o.create_by AS create_by \
               FROM biz_purchase_order o \
               LEFT JOIN biz_supplier s ON s.id = o.supplier_id AND s.deleted_at IS NULL \
               LEFT JOIN biz_warehouse w ON w.id = o.warehouse_id AND w.deleted_at IS NULL \
              WHERE o.deleted_at IS NULL \
                AND o.status IN ('APPROVED', 'PARTIAL_RECEIVED') \
                AND o.expect_date IS NOT NULL \
                AND o.expect_date < ?::text::date \
              ORDER BY o.expect_date ASC, o.order_no ASC \
              LIMIT ?"
                .to_string(),
            vec![Value::Text(day.format("%Y-%m-%d").to_string()), Value::I64(SCAN_BATCH)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let raw: String = row.get_typed("expect_date")?;
        let expect_date = NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
            .map_err(|error| DbError::InvalidValue(format!("采购单期望到货日解析失败（{raw}）：{error}")))?;
        out.push(PurchaseOverdueCandidate {
            id: row.get_typed("id")?,
            order_no: row.get_typed("order_no")?,
            supplier_name: row.get_typed("supplier_name")?,
            expect_date,
            warehouse_name: row.get_typed("warehouse_name")?,
            overdue_days: (day - expect_date).num_days().max(1),
            created_by: row.get_typed("create_by")?,
        });
    }
    Ok(out)
}

/// 逾期天数的便捷计算（纯函数，供测试与文案复用）。
pub fn overdue_days_of(expect_date: NaiveDate, day: NaiveDate) -> i64 {
    (day - expect_date).num_days()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Duration;

    /// 最近 N 天的日期（造夹具时算 `expect_date`）。
    fn days_ago(day: NaiveDate, days: i64) -> NaiveDate {
        day - Duration::days(days)
    }

    fn candidate(overdue_days: i64) -> PurchaseOverdueCandidate {
        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        PurchaseOverdueCandidate {
            id: Uuid::parse_str("0199dddd-0000-7000-8000-000000000001").expect("固定 uuid"),
            order_no: "PO-20261001-000007".to_string(),
            supplier_name: "上海示例供应商".to_string(),
            expect_date: days_ago(day, overdue_days),
            warehouse_name: "原料仓".to_string(),
            overdue_days,
            created_by: Some(Uuid::parse_str("0199dddd-0000-7000-8000-000000000011").unwrap()),
        }
    }

    /// ① 档位：逾期天数 + 1，最长 4 位（台账 stage 列宽 16）。
    #[test]
    fn stage_is_overdue_days() {
        assert_eq!(stage_of(1), "OVERDUE-1");
        assert_eq!(stage_of(42), "OVERDUE-42");
        assert_eq!(stage_of(0), "OVERDUE-1", "0 天不会是超期，兜底成 1");
        assert!(stage_of(120_000).len() <= 16);
    }

    /// ② 文案三件套：单号 + 期望到货日 + 该做什么；逾期天数写进标题。
    #[test]
    fn message_carries_doc_no_date_and_action() {
        let managers = vec![Uuid::parse_str("0199dddd-0000-7000-8000-000000000021").unwrap()];
        let message = message_of(&candidate(14), &managers);
        assert!(message.title.contains("PO-20261001-000007"), "{}", message.title);
        assert!(message.title.contains("14"), "标题要写逾期天数：{}", message.title);
        for fact in [
            "PO-20261001-000007",
            "上海示例供应商",
            "2026-10-20",
            "原料仓",
            "催供应商交货",
        ] {
            assert!(message.content.contains(fact), "正文缺 `{fact}`：{}", message.content);
        }
        assert_eq!(message.stage, "OVERDUE-14");
        assert_eq!(message.vars["overdueDays"], "14");
        // 制单人 + 采购经理 2 人
        assert_eq!(message.recipients.len(), 2);
    }

    /// ③ 没有制单人时只发采购经理；一个都没有 → 空列表（上层 WARN + 跳过）。
    #[test]
    fn recipients_degrade_gracefully() {
        let manager = Uuid::parse_str("0199dddd-0000-7000-8000-000000000021").unwrap();
        let mut no_creator = candidate(3);
        no_creator.created_by = None;
        assert_eq!(message_of(&no_creator, &[manager]).recipients, vec![manager]);

        let mut nobody = candidate(3);
        nobody.created_by = None;
        assert!(message_of(&nobody, &[]).recipients.is_empty());
    }

    /// ④ 逾期天数口径：同一天 = 0，超过一天才算超期（SQL 用 `expect_date < 业务日`）。
    #[test]
    fn overdue_days_boundary() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        assert_eq!(overdue_days_of(day, day), 0, "当天到货不算超期");
        assert_eq!(overdue_days_of(day - Duration::days(1), day), 1);
        assert_eq!(overdue_days_of(day + Duration::days(1), day), -1);
    }

    /// ⑤ 消息里的 `doc_id` 必须是采购单主键（台账幂等靠它）。
    #[test]
    fn doc_id_is_order_id() {
        let message = message_of(&candidate(5), &[]);
        assert_eq!(message.doc_id, candidate(5).id);
        assert_eq!(message.doc_no, "PO-20261001-000007");
    }
}
