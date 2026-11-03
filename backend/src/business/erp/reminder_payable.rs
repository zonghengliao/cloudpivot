//! ① 应付到期提醒（D-7 / D-3 / D-1）。
//!
//! # 判据（唯一真源在本文件，`scan` 的 SQL 与 [`stage_of`] 是同一口径）
//!
//! ```text
//! biz_payable 未删除
//!   AND status IN ('OPEN', 'PARTIAL_PAID')     -- 已结清 / 已作废不提醒
//!   AND unpaid_amount > 0                       -- 金额为 0 的「已结清但状态没改」也不提醒
//!   AND due_date IN (today+7, today+3, today+1) -- 三个档位各提醒一次
//! ```
//!
//! **为什么只提醒「到期前」不提醒「已逾期」**：卡面口径是「前 7/3/1 天各提醒一次」。
//! 逾期未付当然也该有人管，但那是「逾期催收」的另一条业务线（要不要算滞纳金 / 影响信用评级
//! 是财务政策），本卡不擅自扩大语义；逾期后资金计划里那张单仍然在，只是本提醒不再重复打扰。
//!
//! # 收件人
//!
//! | 收件人 | 怎么定位 |
//! |--------|----------|
//! | 供应商对应的采购员 | 应付 → 来源收货单（`source_type='STOCK_IN'`）→ 来源采购单的 `create_by`；逐级回退到收货单制单人、应付单制单人 |
//! | 财务 | 角色 `FINANCE_MANAGER` + `FINANCE_DIRECTOR` 下的启用用户 |
//!
//! 「采购员」这一层是本仓数据模型的兜底选择：`biz_supplier` **没有采购员列**（V0137 只有
//! 联系人 / 电话 / 邮箱），能代表「谁在跟这家供应商」的唯一事实是那张采购单的制单人。

use nik::prelude::chrono::{DateTime, Duration, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::reminder::{self, ReminderMessage, SCAN_BATCH};

/// 提醒档位：(提前天数, 台账 stage)。
pub const STAGES: [(i64, &str); 3] = [(7, "D-7"), (3, "D-3"), (1, "D-1")];

/// 财务收件人角色（两个角色都要：经理审批付款、总监把关资金计划）。
pub const FINANCE_ROLES: [&str; 2] = ["FINANCE_MANAGER", "FINANCE_DIRECTOR"];

/// 档位名（`days_left` 不在 [`STAGES`] 里 → `None`）。
pub fn stage_of(days_left: i64) -> Option<&'static str> {
    STAGES
        .into_iter()
        .find(|(days, _)| *days == days_left)
        .map(|(_, stage)| stage)
}

/// 一行待提醒的应付单（扫描产物，已带收件人）。
pub fn message_of(candidate: &PayableCandidate, finance: &[Uuid]) -> Option<ReminderMessage> {
    let stage = stage_of(candidate.days_left)?;
    let recipients = reminder::merge_recipients(
        std::iter::once(candidate.purchaser_id)
            .chain(std::iter::once(candidate.created_by))
            .chain(finance.iter().copied().map(Some)),
    );
    let due_date = candidate.due_date.format("%Y-%m-%d").to_string();
    let amount = reminder::decimal_text(&candidate.unpaid_amount);
    let days_left = candidate.days_left;
    let title = format!("应付单 {days_left} 天后到期：{}", candidate.payable_no);
    let content = format!(
        "应付单 {}（供应商：{}）未付金额 {}，到期日 {}，还剩 {} 天。\n\
         请核对本期资金计划并及时安排付款；确需延期的，请提前与供应商沟通并更新账期。",
        candidate.payable_no, candidate.supplier_name, amount, due_date, days_left
    );
    let vars = serde_json::json!({
        "payableNo": candidate.payable_no,
        "supplierName": candidate.supplier_name,
        "unpaidAmount": amount,
        "dueDate": due_date,
        "daysLeft": days_left.to_string(),
        "action": "安排付款",
    });
    Some(ReminderMessage {
        doc_id: candidate.id,
        doc_no: candidate.payable_no.clone(),
        stage: stage.to_string(),
        title,
        content,
        vars,
        recipients,
        recipient_rule: "供应商对应采购员（采购单/收货单/应付单制单人逐级回退）+ 财务经理/财务总监"
            .to_string(),
    })
}

/// 扫描「该提醒的应付单」并组装成待发消息（只读查询，不发通知）。
pub async fn scan(now: DateTime<Utc>) -> DbResult<Vec<ReminderMessage>> {
    let day = reminder::business_day(now);
    let candidates = scan_candidates(day).await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let finance = reminder::users_of_roles(&FINANCE_ROLES).await?;
    if finance.is_empty() {
        tracing::warn!(
            roles = ?FINANCE_ROLES,
            "应付到期提醒：财务角色下没有启用用户，本轮只有采购员会收到（请补角色成员）"
        );
    }
    Ok(candidates
        .iter()
        .filter_map(|candidate| message_of(candidate, &finance))
        .collect())
}

/// 扫描候选行（`due_date` = 三个档位之一）。
///
/// 日期在 Rust 侧算好再绑定（`?::text::date`），不用 `current_date + 7` —— 否则
/// 「扫到的档位」与「文案里的剩余天数」可能因为时区/跨零点差一天。
pub async fn scan_candidates(day: NaiveDate) -> DbResult<Vec<PayableCandidate>> {
    let mut params = Vec::with_capacity(STAGES.len() + 1);
    for (days, _) in STAGES {
        let date = day + Duration::days(days);
        params.push(Value::Text(date.format("%Y-%m-%d").to_string()));
    }
    params.push(Value::I64(SCAN_BATCH));
    // SCOPE-EXEMPT: 应付单没有 dept_id / 归属人列（V0148），提醒是「谁该付钱」的作业口径，
    // 收件人由采购员与财务角色决定；注入数据范围只会让别人的到期单永不提醒。
    let rows = db()
        .query_sql(
            "SELECT p.id AS id, p.payable_no AS payable_no, \
                    COALESCE(s.supplier_name, '供应商未知') AS supplier_name, \
                    p.unpaid_amount::text AS unpaid_amount, \
                    p.due_date::text AS due_date, \
                    p.create_by AS create_by, \
                    COALESCE(po.create_by, si.create_by) AS purchaser_id \
               FROM biz_payable p \
               LEFT JOIN biz_supplier s ON s.id = p.supplier_id AND s.deleted_at IS NULL \
               LEFT JOIN biz_stock_in si ON si.id = p.source_id \
                                        AND p.source_type = 'STOCK_IN' AND si.deleted_at IS NULL \
               LEFT JOIN biz_purchase_order po ON po.id = si.source_order_id \
                                              AND po.deleted_at IS NULL \
              WHERE p.deleted_at IS NULL \
                AND p.status IN ('OPEN', 'PARTIAL_PAID') AND p.unpaid_amount > 0 \
                AND p.due_date IN (?::text::date, ?::text::date, ?::text::date) \
              ORDER BY p.due_date ASC, p.payable_no ASC \
              LIMIT ?"
                .to_string(),
            params,
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let due_date = parse_date(row.get_typed::<String>("due_date")?.as_str())?;
        out.push(PayableCandidate {
            id: row.get_typed("id")?,
            payable_no: row.get_typed("payable_no")?,
            supplier_name: row.get_typed("supplier_name")?,
            unpaid_amount: row.get_typed("unpaid_amount")?,
            due_date,
            days_left: (due_date - day).num_days(),
            purchaser_id: row.get_typed("purchaser_id")?,
            created_by: row.get_typed("create_by")?,
        });
    }
    Ok(out)
}

/// `yyyy-MM-dd` 文本 → `NaiveDate`（扫描 SQL 统一把 DATE 列 `::text` 取出，避免驱动类型差异）。
pub fn parse_date(raw: &str) -> DbResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|error| DbError::InvalidValue(format!("日期解析失败（{raw}）：{error}")))
}

/// 一行扫描候选。
#[derive(Debug, Clone)]
pub struct PayableCandidate {
    /// 应付单主键（台账 `doc_id`、通知 `biz_id`）。
    pub id: Uuid,
    /// 应付单号。
    pub payable_no: String,
    /// 供应商名称（查不到写「供应商未知」，不 panic）。
    pub supplier_name: String,
    /// 未付金额（库里的 NUMERIC 文本，写文案时去尾零）。
    pub unpaid_amount: String,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 距到期天数（7 / 3 / 1）。
    pub days_left: i64,
    /// 供应商对应的采购员（采购单制单人 → 收货单制单人）。
    pub purchaser_id: Option<Uuid>,
    /// 应付单制单人（采购员也定位不到时的兜底收件人）。
    pub created_by: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::TimeZone;

    fn candidate(days_left: i64) -> PayableCandidate {
        let due = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap() + Duration::days(days_left);
        PayableCandidate {
            id: Uuid::parse_str("0199cccc-0000-7000-8000-000000000001").expect("固定 uuid"),
            payable_no: "AP-20261103-000001".to_string(),
            supplier_name: "上海示例供应商".to_string(),
            unpaid_amount: "1234.5000".to_string(),
            due_date: due,
            days_left,
            purchaser_id: Some(Uuid::parse_str("0199cccc-0000-7000-8000-000000000011").unwrap()),
            created_by: Some(Uuid::parse_str("0199cccc-0000-7000-8000-000000000012").unwrap()),
        }
    }

    /// ① 档位映射：只有 7 / 3 / 1 三天会提醒，其余一律不提醒。
    #[test]
    fn stage_only_on_three_days() {
        assert_eq!(stage_of(7), Some("D-7"));
        assert_eq!(stage_of(3), Some("D-3"));
        assert_eq!(stage_of(1), Some("D-1"));
        for days in [0, 2, 4, 5, 6, 8, 30, -1, -7] {
            assert_eq!(stage_of(days), None, "{days} 天不该提醒");
        }
    }

    /// ② 文案三件套：单据号 + 关键日期 + 一句话该做什么；金额去尾零。
    #[test]
    fn message_carries_doc_no_date_and_action() {
        let finance = vec![Uuid::parse_str("0199cccc-0000-7000-8000-000000000021").unwrap()];
        let message = message_of(&candidate(3), &finance).expect("D-3 应产出消息");
        assert_eq!(message.stage, "D-3");
        assert!(
            message.title.contains("AP-20261103-000001"),
            "{}",
            message.title
        );
        assert!(
            message.title.contains('3'),
            "标题要写剩余天数：{}",
            message.title
        );
        for fact in [
            "AP-20261103-000001",
            "上海示例供应商",
            "1234.5",
            "2026-11-06",
            "安排付款",
        ] {
            assert!(
                message.content.contains(fact),
                "正文缺 `{fact}`：{}",
                message.content
            );
        }
        assert_eq!(message.vars["daysLeft"], "3");
        assert_eq!(message.vars["unpaidAmount"], "1234.5");
        // 收件人：采购员 + 应付制单人 + 财务 = 3 人（去重后）
        assert_eq!(message.recipients.len(), 3);
    }

    /// ③ 不在档位上的天数不产出消息（防御：SQL 已过滤，纯函数也不放行）。
    #[test]
    fn non_stage_day_yields_nothing() {
        assert!(message_of(&candidate(5), &[]).is_none());
        assert!(message_of(&candidate(0), &[]).is_none());
    }

    /// ④ 没有任何收件人时 `recipients` 为空（由 `run_kind` 记 WARN + 跳过，不静默丢）。
    #[test]
    fn no_recipient_marks_empty() {
        let mut only = candidate(1);
        only.purchaser_id = None;
        only.created_by = None;
        let message = message_of(&only, &[]).expect("仍应产出消息（收件人由上层判定）");
        assert!(message.recipients.is_empty());
        assert!(
            message.recipient_rule.contains("采购员"),
            "WARN 里要写清试过哪些规则"
        );
    }

    /// ⑤ 解析日期失败要给可读错误（不是 panic）。
    #[test]
    fn parse_date_reports_error() {
        assert!(parse_date("2026-11-03").is_ok());
        assert!(parse_date("not-a-date").is_err());
    }

    /// ⑥ 时间基准：`business_day` 决定档位，北京 07:10 与 UTC 23:10 不能差一天。
    #[test]
    fn stage_uses_business_day() {
        let now = Utc
            .with_ymd_and_hms(2026, 11, 3, 23, 10, 0)
            .single()
            .unwrap();
        let day = reminder::business_day(now);
        assert_eq!(day.to_string(), "2026-11-04", "UTC 23:10 = 北京次日 07:10");
    }
}
