//! 佣金结算 · 接口契约（销售链 · SO-36 / ARCH-7 §15.4.3）。
//!
//! 三个接口（前缀 `/api` 由 `[router].prefix` 注入，宏内不写）：
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | POST | `/business/erp/commission/settle` | `business:erp:commission-settle:create` |
//! | GET | `/business/erp/commission/page` | `business:erp:commission-settle:list` |
//! | PATCH | `/business/erp/commission/{id}/status` | `business:erp:commission-settle:status` |
//!
//! # 金额口径（前端照抄，别自己算）
//!
//! * 所有金额都是 `NUMERIC(18,4)` 的字符串（前端按 2 位展示即可）；
//! * `base_amount` **有符号**：正 = 本期新增已核销（收到钱）金额，负 = 退货 / 退款冲回；
//! * `total_sales` = 本期净基数合计，**按 0 兜底**（纯冲回期间记 0，冲回体现在
//!   `total_commission` 的负数上 —— 库层 `ck_biz_cmset_total_sales` 要求非负）；
//! * `commission` 允许负（冲回行）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== 常量 ====================

/// 结算单状态（与 `ck_biz_cmset_status` 逐字一致，单测钉住）。
pub const STATUSES: [&str; 3] = [STATUS_DRAFT, STATUS_APPROVED, STATUS_PAID];

/// 草稿：已算出金额，未入账（可重算覆盖）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 已审批：已生成计提凭证（进入总账，不可重算）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 已发放：佣金已付出（已生成付款凭证）。
pub const STATUS_PAID: &str = "PAID";

/// 状态迁移边（`DRAFT → APPROVED → PAID`；无回退边 —— 回退等于改写已入账的凭证）。
pub const LEGAL_EDGES: [(&str, &str); 2] = [(STATUS_DRAFT, STATUS_APPROVED), (STATUS_APPROVED, STATUS_PAID)];

/// 状态中文名（错误文案用）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_APPROVED => "已审批",
        STATUS_PAID => "已发放",
        _ => "未知",
    }
}

// ==================== 校验 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 解析 uuid 路径 / 请求参数。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field} 不是合法的 UUID（收到「{raw}」）")))
}

/// 期间必须是 `yyyy-MM` 且月份 01–12（与 `ck_biz_cmset_period` 同一口径）。
pub fn validate_period_code(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 7
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..].iter().all(u8::is_ascii_digit);
    if !shaped {
        return Err(bad_request(format!("period_code 必须是 yyyy-MM（收到「{text}」）")));
    }
    let month: u32 = text[5..].parse().unwrap_or(0);
    if !(1..=12).contains(&month) {
        return Err(bad_request(format!("period_code 的月份必须在 01–12（收到「{text}」）")));
    }
    Ok(text.to_string())
}

/// 当前月（北京时间）的期间编码。
pub fn current_period_code(now: DateTime<Utc>) -> String {
    let cst = now.with_timezone(&nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"));
    format!(
        "{:04}-{:02}",
        nik::prelude::chrono::Datelike::year(&cst),
        nik::prelude::chrono::Datelike::month(&cst)
    )
}

/// 期间的**最后一天**（凭证日期用：佣金属于整个期间，不做「按天分摊」）。
///
/// 返回 `None` = 期间字符串非法（调用方已用 [`validate_period_code`] 挡过，这是兜底）。
pub fn period_end_date(period_code: &str) -> Option<NaiveDate> {
    let (year, month) = period_code.split_once('-')?;
    let year: i32 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    next.pred_opt()
}

// ==================== 生成结算单 ====================

/// 生成结算单的入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SettleReq {
    /// 结算期间 `yyyy-MM`；缺省 = 当前月（北京时间）。
    pub period_code: Option<String>,
    /// 只给这个销售员结算；缺省 = 所有「有已收款差额」的销售员各生成一张。
    pub salesperson_id: Option<String>,
    /// 写进结算单的备注（重算时覆盖旧备注）。
    pub remark: Option<String>,
}

/// 生成结算单的返回：逐销售员的处理结果（成功的在 `settlements`，没数据的在 `skipped`）。
#[derive(Debug, Clone, Serialize)]
pub struct SettleRes {
    /// 实际使用的期间。
    pub period_code: String,
    /// 生成了的结算单（DRAFT）。
    pub settlements: Vec<SettlementRes>,
    /// 没有生成的原因（没有可计佣的已收款 / 已有已审批结算单）。
    pub skipped: Vec<SkippedRes>,
}

/// 跳过原因的销售员与原因（前端提示用；不是错误）。
#[derive(Debug, Clone, Serialize)]
pub struct SkippedRes {
    /// 销售员 id。
    pub salesperson_id: String,
    /// 销售员姓名。
    pub salesperson_name: String,
    /// 跳过原因（人读）。
    pub reason: String,
}

// ==================== 查询 ====================

/// 结算单行（列表 / 详情的公共部分）。
#[derive(Debug, Clone, Serialize)]
pub struct SettlementRes {
    /// 主键。
    pub id: String,
    /// 结算单号 `CM-yyyyMM-000001`。
    pub settlement_no: String,
    /// 销售员 id。
    pub salesperson_id: String,
    /// 销售员姓名。
    pub salesperson_name: String,
    /// 结算期间 `yyyy-MM`。
    pub period_code: String,
    /// 期间佣金基数合计（已收款口径，负值按 0 兜底）。
    pub total_sales: Decimal,
    /// 应发佣金合计（可为负：纯冲回期间）。
    pub total_commission: Decimal,
    /// 状态 `DRAFT` / `APPROVED` / `PAID`。
    pub status: String,
    /// 发放时间（仅 PAID）。
    pub paid_at: Option<DateTime<Utc>>,
    /// 计提凭证 id（审批通过后才有）。
    pub voucher_id: Option<String>,
    /// 计提凭证号（`PZ-...`，没有凭证时为空）。
    pub voucher_no: Option<String>,
    /// 明细行数。
    pub item_count: i64,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 佣金明细行。
#[derive(Debug, Clone, Serialize)]
pub struct CommissionItemRes {
    /// 主键。
    pub id: String,
    /// 来源销售订单 id。
    pub so_id: Option<String>,
    /// 来源销售订单号。
    pub so_no: Option<String>,
    /// 对应应收 id。
    pub receivable_id: Option<String>,
    /// 对应应收单号。
    pub ar_no: Option<String>,
    /// 命中的规则 id。
    pub rule_id: Option<String>,
    /// 命中的规则名（规则改版不影响这里的历史快照名，取明细写入时的作用域）。
    pub rule_name: Option<String>,
    /// 佣金基数（有符号：正计提 / 负冲回）。
    pub base_amount: Decimal,
    /// 比率快照（PERCENT 的百分数 / FIXED 的金额）。
    pub rate: Decimal,
    /// 本行佣金。
    pub commission: Decimal,
    /// 行备注（如「退货 / 退款冲回」）。
    pub remark: Option<String>,
}

/// 结算单详情（单 + 明细）。
#[derive(Debug, Clone, Serialize)]
pub struct SettlementDetailRes {
    /// 结算单本体。
    #[serde(flatten)]
    pub settlement: SettlementRes,
    /// 明细（按佣金倒序：计提在前、冲回在后）。
    pub items: Vec<CommissionItemRes>,
}

// ==================== 分页 ====================

/// 分页查询入参（全部可空）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CommissionPageQuery {
    /// 页码（默认 1）。
    pub page: Option<u64>,
    /// 每页条数（默认 20，上限 200）。
    pub page_size: Option<u64>,
    /// 期间 `yyyy-MM`。
    pub period_code: Option<String>,
    /// 销售员 id。
    pub salesperson_id: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 关键字：结算单号 / 销售员姓名 / 登录名。
    pub keyword: Option<String>,
}

impl CommissionPageQuery {
    /// 入参校验（分页与域值）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(raw) = self.status.as_deref().map(str::trim)
            && !raw.is_empty()
            && !STATUSES.contains(&raw)
        {
            return Err(bad_request(format!("status 只认 {STATUSES:?}（收到「{raw}」）")));
        }
        if let Some(raw) = self.period_code.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            validate_period_code(raw)?;
        }
        if let Some(raw) = self.salesperson_id.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            parse_uuid(raw, "salesperson_id")?;
        }
        Ok(())
    }
}

/// 数据层筛选条件（空白串 = 没传）。
#[derive(Debug, Clone, Default)]
pub struct CommissionFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 期间。
    pub period_code: Option<String>,
    /// 销售员。
    pub salesperson_id: Option<Uuid>,
    /// 状态。
    pub status: Option<String>,
    /// 关键字。
    pub keyword: Option<String>,
}

// ==================== 状态迁移 ====================

/// 状态迁移入参。
#[derive(Debug, Clone, Deserialize)]
pub struct CommissionStatusReq {
    /// 目标状态 `APPROVED` / `PAID`。
    pub status: String,
    /// 备注（发放说明；写进结算单备注的追加行）。
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_domain_matches_migration() {
        assert_eq!(STATUSES, ["DRAFT", "APPROVED", "PAID"]);
        assert_eq!(status_label(STATUS_DRAFT), "草稿");
        assert_eq!(status_label(STATUS_PAID), "已发放");
    }

    #[test]
    fn legal_edges_are_a_chain_without_rewind() {
        assert_eq!(LEGAL_EDGES, [("DRAFT", "APPROVED"), ("APPROVED", "PAID")]);
        // 没有任何边指向 DRAFT：已入账的结算单不许回到草稿（否则凭证与单据脱钩）
        assert!(!LEGAL_EDGES.iter().any(|(_, to)| *to == STATUS_DRAFT));
    }

    #[test]
    fn period_code_validation() {
        assert_eq!(validate_period_code(" 2026-11 ").unwrap(), "2026-11");
        assert_eq!(validate_period_code("2026-12").unwrap(), "2026-12");
        for bad in ["2026-13", "2026-00", "202611", "2026-1", "2026/11", ""] {
            let err = validate_period_code(bad).expect_err(bad);
            assert_eq!(err.status, 400, "{bad}");
            assert_eq!(err.code, error_code::PARAM_INVALID, "{bad}");
        }
    }

    #[test]
    fn period_end_date_handles_month_and_year_boundaries() {
        assert_eq!(
            period_end_date("2026-11").unwrap(),
            NaiveDate::from_ymd_opt(2026, 11, 30).unwrap()
        );
        assert_eq!(
            period_end_date("2026-12").unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()
        );
        // 闰年 2 月
        assert_eq!(
            period_end_date("2028-02").unwrap(),
            NaiveDate::from_ymd_opt(2028, 2, 29).unwrap()
        );
        assert_eq!(
            period_end_date("2026-02").unwrap(),
            NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()
        );
        assert!(period_end_date("2026-13").is_none());
    }
}
