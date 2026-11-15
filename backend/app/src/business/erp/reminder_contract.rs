//! ③ 合同到期提醒（D-30 / D-15 / D-7 / D-1）。
//!
//! # 判据
//!
//! ```text
//! biz_contract 未删除
//!   AND status IN ('SIGNED', 'ACTIVE')            -- 「已经生效、还会到期」的合同
//!   AND end_date IN (今天+30, +15, +7, +1)
//! ```
//!
//! | 状态 | 是否提醒 | 为什么 |
//! |------|----------|--------|
//! | `DRAFT` | 否 | 草稿还没签，谈不上「到期」 |
//! | `PENDING_APPROVAL` | 否 | 审批中，还没成为法律事实（P1-1 引入的中间态） |
//! | `SIGNED` / `ACTIVE` | **是** | 已签订 / 执行中 —— 到期前必须有人决定续不续 |
//! | `COMPLETED` / `TERMINATED` | 否 | 已履行完 / 已终止，没有「到期」这回事 |
//!
//! **档位为什么是 30/15/7/1 而不是只有一个 30 天**：卡面要求「前 30 天提醒」——
//! 30 天是**起点**。只提醒一次 30 天，遇到「当时没空、过后忘了」就彻底失守；
//! 按 30 → 15 → 7 → 1 递进加密（越近越急），仍由台账保证**同一档同一天只发一条**。
//!
//! # 收件人
//!
//! 合同制单人 + 合同负责人（`owner_id`）+ 采购经理（`PURCHASE_MANAGER`）。
//! 负责人是 `biz_contract.owner_id`（V0195 的「负责人」列），比制单人更贴近
//! 「谁该管这份合同」；两者都空时才只剩采购经理兜底。

use nik::prelude::chrono::{DateTime, Duration, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::reminder::{self, ReminderMessage, SCAN_BATCH};

/// 提醒档位：(提前天数, 台账 stage)。
pub const STAGES: [(i64, &str); 4] = [(30, "D-30"), (15, "D-15"), (7, "D-7"), (1, "D-1")];

/// 收件人角色：采购经理（合同的商务口）。
pub const CONTRACT_ROLES: [&str; 1] = ["PURCHASE_MANAGER"];

/// 档位名（`days_left` 不在 [`STAGES`] 里 → `None`）。
pub fn stage_of(days_left: i64) -> Option<&'static str> {
    STAGES
        .into_iter()
        .find(|(days, _)| *days == days_left)
        .map(|(_, stage)| stage)
}

/// 一行待提醒的合同（扫描产物）。
#[derive(Debug, Clone)]
pub struct ContractExpireCandidate {
    /// 合同主键。
    pub id: Uuid,
    /// 合同号。
    pub contract_no: String,
    /// 合同名称。
    pub contract_name: String,
    /// 乙方名称快照（V0195 的 `party_b_name`，签约时冻结）。
    pub party_b_name: String,
    /// 到期日。
    pub end_date: NaiveDate,
    /// 距到期天数（30 / 15 / 7 / 1）。
    pub days_left: i64,
    /// 制单人。
    pub created_by: Option<Uuid>,
    /// 负责人。
    pub owner_id: Option<Uuid>,
}

/// 组装一条提醒消息；不在档位上返回 `None`（防御性：SQL 已过滤）。
pub fn message_of(candidate: &ContractExpireCandidate, managers: &[Uuid]) -> Option<ReminderMessage> {
    let stage = stage_of(candidate.days_left)?;
    let recipients = reminder::merge_recipients(
        std::iter::once(candidate.created_by)
            .chain(std::iter::once(candidate.owner_id))
            .chain(managers.iter().copied().map(Some)),
    );
    let end_date = candidate.end_date.format("%Y-%m-%d").to_string();
    let days_left = candidate.days_left;
    let title = format!("合同 {days_left} 天后到期：{}", candidate.contract_no);
    let content = format!(
        "合同 {}（{}，乙方：{}）将于 {} 到期，还剩 {} 天。\n\
         请确认是否续签：续签请在本合同到期前起草新合同并走审批；\
         不续签请及时终止合同并结清未了事项。",
        candidate.contract_no, candidate.contract_name, candidate.party_b_name, end_date, days_left
    );
    let vars = serde_json::json!({
        "contractNo": candidate.contract_no,
        "contractName": candidate.contract_name,
        "partyBName": candidate.party_b_name,
        "endDate": end_date,
        "daysLeft": days_left.to_string(),
        "action": "确认是否续签",
    });
    Some(ReminderMessage {
        doc_id: candidate.id,
        doc_no: candidate.contract_no.clone(),
        stage: stage.to_string(),
        title,
        content,
        vars,
        recipients,
        recipient_rule: "合同制单人 + 负责人 + 采购经理（PURCHASE_MANAGER）".to_string(),
    })
}

/// 扫描「即将到期」的合同并组装成待发消息（只读查询，不发通知）。
pub async fn scan(now: DateTime<Utc>) -> DbResult<Vec<ReminderMessage>> {
    let day = reminder::business_day(now);
    let candidates = scan_candidates(day).await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let managers = reminder::users_of_roles(&CONTRACT_ROLES).await?;
    if managers.is_empty() {
        tracing::warn!(
            roles = ?CONTRACT_ROLES,
            "合同到期提醒：采购经理角色下没有启用用户，本轮只有制单人/负责人会收到（请补角色成员）"
        );
    }
    Ok(candidates
        .iter()
        .filter_map(|candidate| message_of(candidate, &managers))
        .collect())
}

/// 扫描候选行（`end_date` = 四个档位之一）。
pub async fn scan_candidates(day: NaiveDate) -> DbResult<Vec<ContractExpireCandidate>> {
    let mut params = Vec::with_capacity(STAGES.len() + 1);
    for (days, _) in STAGES {
        params.push(Value::Text((day + Duration::days(days)).format("%Y-%m-%d").to_string()));
    }
    params.push(Value::I64(SCAN_BATCH));
    // SCOPE-EXEMPT: 合同虽带 dept_id，但提醒是「谁签的谁续」的作业口径 ——
    // 数据范围过滤会让别人部门的合同到期无人知晓（提醒的失效形态是静默，比越权更难发现）。
    let rows = db()
        .query_sql(
            "SELECT c.id AS id, c.contract_no AS contract_no, c.contract_name AS contract_name, \
                    c.party_b_name AS party_b_name, c.end_date::text AS end_date, \
                    c.create_by AS create_by, c.owner_id AS owner_id \
               FROM biz_contract c \
              WHERE c.deleted_at IS NULL \
                AND c.status IN ('SIGNED', 'ACTIVE') \
                AND c.end_date IN (?::text::date, ?::text::date, ?::text::date, ?::text::date) \
              ORDER BY c.end_date ASC, c.contract_no ASC \
              LIMIT ?"
                .to_string(),
            params,
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let raw: String = row.get_typed("end_date")?;
        let end_date = NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
            .map_err(|error| DbError::InvalidValue(format!("合同到期日解析失败（{raw}）：{error}")))?;
        out.push(ContractExpireCandidate {
            id: row.get_typed("id")?,
            contract_no: row.get_typed("contract_no")?,
            contract_name: row.get_typed("contract_name")?,
            party_b_name: row.get_typed("party_b_name")?,
            end_date,
            days_left: (end_date - day).num_days(),
            created_by: row.get_typed("create_by")?,
            owner_id: row.get_typed("owner_id")?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(days_left: i64) -> ContractExpireCandidate {
        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        ContractExpireCandidate {
            id: Uuid::parse_str("0199eeee-0000-7000-8000-000000000001").expect("固定 uuid"),
            contract_no: "HT-20260101-000001".to_string(),
            contract_name: "2026 年度原料采购框架协议".to_string(),
            party_b_name: "上海示例供应商".to_string(),
            end_date: day + Duration::days(days_left),
            days_left,
            created_by: Some(Uuid::parse_str("0199eeee-0000-7000-8000-000000000011").unwrap()),
            owner_id: Some(Uuid::parse_str("0199eeee-0000-7000-8000-000000000012").unwrap()),
        }
    }

    /// ① 四个档位只有 30/15/7/1，别的天数一律不提醒。
    #[test]
    fn stage_only_on_four_days() {
        for days in [30, 15, 7, 1] {
            assert!(stage_of(days).is_some(), "{days} 天应提醒");
        }
        for days in [0, 2, 5, 6, 8, 14, 16, 29, 31, 60, -1] {
            assert_eq!(stage_of(days), None, "{days} 天不该提醒");
        }
    }

    /// ② 文案三件套：合同号 + 到期日 + 该做什么。
    #[test]
    fn message_carries_doc_no_date_and_action() {
        let managers = vec![Uuid::parse_str("0199eeee-0000-7000-8000-000000000021").unwrap()];
        let message = message_of(&candidate(30), &managers).expect("D-30 应产出消息");
        assert_eq!(message.stage, "D-30");
        assert!(message.title.contains("HT-20260101-000001"), "{}", message.title);
        for fact in [
            "HT-20260101-000001",
            "2026 年度原料采购框架协议",
            "上海示例供应商",
            "2026-12-03",
            "续签",
        ] {
            assert!(message.content.contains(fact), "正文缺 `{fact}`：{}", message.content);
        }
        assert_eq!(message.vars["daysLeft"], "30");
        // 制单人 + 负责人 + 采购经理
        assert_eq!(message.recipients.len(), 3);
    }

    /// ③ 不在档位上 → 不产出（防御）。
    #[test]
    fn non_stage_day_yields_nothing() {
        assert!(message_of(&candidate(20), &[]).is_none());
    }

    /// ④ 制单人 = 负责人时不重复推。
    #[test]
    fn recipient_dedup() {
        let mut same = candidate(7);
        same.owner_id = same.created_by;
        let message = message_of(&same, &[]).expect("应产出");
        assert_eq!(message.recipients.len(), 1);
    }

    /// ⑤ 收件人规则写进消息（WARN 日志取证用）。
    #[test]
    fn recipient_rule_is_described() {
        let message = message_of(&candidate(1), &[]).expect("应产出");
        assert!(
            message.recipient_rule.contains("采购经理"),
            "{}",
            message.recipient_rule
        );
    }
}
