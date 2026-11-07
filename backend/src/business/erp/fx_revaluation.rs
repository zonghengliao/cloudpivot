//! 汇兑损益**月末重估**（销售链 · SO-41 · ARCH-7 §15.6.4 ②）
//!
//! # 会计口径（唯一真相，改这里必须同步 `V0315` 头注释 / ARCH-7 §15.6.4 / 卡内汇报）
//!
//! ```text
//! 月末结账时，**未核销的外币应收**按期末（月末）SPOT 汇率重估：
//!
//!   未核销原币   open   = biz_receivable.unsettled_amount        （= amount - received_amount）
//!   账面人民币   book   = round(base_total_with_tax × open / amount, 4) + Σ历史重估差额
//!   重估人民币   reval  = round(open × 期末汇率, 4)
//!   差额         diff   = reval - book
//!
//!   diff > 0（汇率涨、应收更值钱）: 借 1122 应收账款   / 贷 5603 财务费用-汇兑损益
//!   diff < 0（汇率跌、应收缩水）  : 借 5603 财务费用-汇兑损益 / 贷 1122 应收账款
//! ```
//!
//! # 为什么用「比例法」而不是「逐笔核销配对法」
//!
//! 收款核销只记原币金额（`biz_receipt_allocation.allocated_amount`），不记本次核销结转了
//! 多少人民币。若按「逐笔核销配对」算账面人民币，就得回溯每笔收款的结算汇率 —— 那是
//! **核销时点**确认汇兑损益的口径（另一张卡的范围）。本期按「账面人民币随未核销比例走」：
//! 期初确认多少人民币、未核销占比多少，账面就留多少 —— 差额统一由月末重估吸收，
//! 重估后账面 = 未核销原币 × 期末汇率，与「应收按期末汇率计价」的报表口径一致。
//!
//! # 累计重估（跨月不重复计提）
//!
//! 月末重估会改 1122 的余额，于是**下个月**再算「账面人民币」时必须把上个月提过的差额算进去，
//! 否则每个月都会把同一块汇兑损益再提一遍（账越提越偏）。所以每笔应收的重估结果落
//! `biz_fx_revaluation`（一行 = 一期 × 一笔应收），下期取 `Σ diff_base` 当 `prior_adjust`。
//!
//! # 幂等
//!
//! 同一期间已经提过重估（`biz_voucher.source_type='FX_REVALUATION'` 且
//! `source_no = 期间码`）⇒ 直接返回已存在的那张凭证，**不重复计提**。
//! 反结账后再结账：账面已经被上个月的重估改过，重算出来的 `diff` 自然趋近 0（不是靠特判）；
//! 真要重提，先按正常流程红冲那张重估凭证（`voucher::reverse_posted`）。
//!
//! # 本位币单据
//!
//! `currency_code = 本位币` 的应收**不参与重估**（README 的第三条验收）：换算率恒 1，
//! 重估恒等，SQL 层直接用 `currency_code <> 本位币` 滤掉，不产生任何行 / 凭证。
//!
//! ```text
//! SCOPE-EXEMPT: biz_fx_revaluation 无归属列（期间级台账，按 biz_receivable 的全局主数据派生）；
//! 可见性由 business:erp:* 权限码控制，且本模块只被结账流程在事务内调用，不暴露分页接口。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::currency::{self, scale_amount};
use crate::business::erp::voucher::{self, ACCOUNT_AR, ACCOUNT_FX_GAIN_LOSS};

/// 凭证来源类型：期末汇兑重估（`biz_voucher.source_type` 是 `VARCHAR(24)` 且无白名单 CHECK，
/// 新增取值不需要迁移 —— 与 `SOURCE_STOCK_OUT` 同一个理由）。
pub const SOURCE_FX_REVALUATION: &str = "FX_REVALUATION";

/// 重估台账表（V0315）。
pub const TABLE: &str = "biz_fx_revaluation";

/// 参与重估的应收状态（`SETTLED` / `WRITTEN_OFF` / `CANCELLED` 一律不在内）。
pub const OPEN_STATUSES: [&str; 2] = ["OPEN", "PARTIAL"];

/// 差额容差：0.0001（`numeric(18,4)` 的最小单位）。
///
/// 比 [`voucher::BALANCE_TOLERANCE`]（0.01）更严：重估差额是**账面与重估的直接相减**，
/// 不存在拆行定标，1 个最小单位的差就是真实差额，不该被吞掉。
pub fn tolerance() -> Decimal {
    Decimal::new(1, 4)
}

// ==================== 纯函数（不碰库，可直接单测） ====================

/// 期间码 `yyyy-MM` → 该月最后一天（期末汇率取数日）。
///
/// 非法期间码（`2026-13` / `202611` / 非数字）返回 `None` —— 调用方给 4xx，
/// 不静默回落成「今天」（拿错日期取汇率 = 用错汇率重估账）。
pub fn month_end_of(period_code: &str) -> Option<NaiveDate> {
    let (year_text, month_text) = period_code.split_once('-')?;
    if year_text.len() != 4 || month_text.len() != 2 {
        return None;
    }
    let year: i32 = year_text.parse().ok()?;
    let month: u32 = month_text.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let (next_year, next_month) = if month == 12 {
        (year.checked_add(1)?, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)?.pred_opt()
}

/// 未核销部分的**账面人民币**（重估前）：按未核销占比取 `base_total_with_tax` 的份额，
/// 再加上本笔应收历史上已提过的重估差额（`prior_adjust`，跨月累计，正负都可能）。
///
/// `amount <= 0`（脏数据，正常被 `ck_biz_ar_amount` 拦住）⇒ 退回「全额 + 历史差额」，
/// 让重估至少不会因为除零把整次结账打挂。
pub fn book_base_of(
    base_total_with_tax: Decimal,
    amount: Decimal,
    unsettled: Decimal,
    prior_adjust: Decimal,
) -> Decimal {
    let base = base_total_with_tax.round_dp(4);
    let amount = amount.round_dp(4);
    let unsettled = unsettled.round_dp(4);
    let share = if amount <= Decimal::ZERO {
        base
    } else {
        scale_amount(base * unsettled / amount)
    };
    (share + prior_adjust.round_dp(4)).round_dp(4)
}

/// 重估后的人民币金额：`round(未核销原币 × 期末汇率, 4)`。
pub fn revalued_of(open_amount: Decimal, close_rate: Decimal) -> Decimal {
    scale_amount(open_amount.round_dp(4) * close_rate)
}

/// 重估差额：`revalued - book`（正 = 应收调增，贷方收益；负 = 应收调减，借方损失）。
pub fn diff_of(open_amount: Decimal, close_rate: Decimal, book_base: Decimal) -> Decimal {
    (revalued_of(open_amount, close_rate) - book_base.round_dp(4)).round_dp(4)
}

/// 一笔应收的重估明细（台账一行 + 日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevalLine {
    /// 应收单 id。
    pub receivable_id: Uuid,
    /// 应收单号（人读 + 对账用）。
    pub ar_no: String,
    /// 原币币种。
    pub currency_code: String,
    /// 未核销原币。
    pub open_amount: Decimal,
    /// 账面人民币（含历史重估）。
    pub book_base: Decimal,
    /// 期末汇率。
    pub close_rate: Decimal,
    /// 重估后人民币。
    pub revalued_base: Decimal,
    /// 差额（正 = 调增应收）。
    pub diff: Decimal,
}

impl RevalLine {
    /// 账面汇率（台账里给人看的派生列）：`book_base / open_amount`，未核销为 0 时退回期末汇率。
    pub fn book_rate(&self) -> Decimal {
        if self.open_amount <= Decimal::ZERO {
            return self.close_rate;
        }
        currency::scale_rate(self.book_base / self.open_amount)
    }
}

/// 一次重估的完整结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevalOutcome {
    /// 期间码 `yyyy-MM`。
    pub period_code: String,
    /// 期末取汇率日（月末）。
    pub close_date: NaiveDate,
    /// 账套本位币。
    pub base_currency: String,
    /// 逐笔明细（差额为 0 的不进来）。
    pub lines: Vec<RevalLine>,
    /// 差额合计（正 = 本期应收调增）。
    pub net: Decimal,
    /// 生成的重估凭证（差额合计为 0 ⇒ `None`）。
    pub voucher_id: Option<Uuid>,
    /// 该期间之前已经提过重估 ⇒ `true`（本次什么都没写）。
    pub skipped: bool,
}

// ==================== 库访问 ====================

/// 一笔待重估的外币应收（读模型）。
#[derive(Debug, Clone)]
pub struct OpenFxAr {
    /// 应收 id。
    pub receivable_id: Uuid,
    /// 应收单号。
    pub ar_no: String,
    /// 原币币种。
    pub currency_code: String,
    /// 原币金额。
    pub amount: Decimal,
    /// 未核销原币。
    pub unsettled_amount: Decimal,
    /// 挂账时锁定的本位币含税额。
    pub base_total_with_tax: Decimal,
    /// 历史重估差额合计（跨期间累计）。
    pub prior_adjust: Decimal,
}

/// 取出该期间（含以前期间）所有**未核销的外币**应收。
///
/// 为什么 `period_code <= P` 而不是 `= P`：上月挂账、本月还没收的应收，本月月末同样要重估
/// （否则「只在挂账月提一次汇兑损益」，之后汇率怎么动都不认）。
pub async fn open_fx_receivables(
    ex: &impl SqlExecutor,
    period_code: &str,
    base_currency: &str,
) -> DbResult<Vec<OpenFxAr>> {
    // SCOPE-EXEMPT: 同上（期间级聚合，按应收表全局可读）
    let rows = ex
        .query_sql(
            format!(
                "SELECT r.id, r.ar_no, r.currency_code, r.amount, r.unsettled_amount, \
                        r.base_total_with_tax, \
                        COALESCE((SELECT SUM(f.diff_base) FROM {TABLE} f \
                                   WHERE f.receivable_id = r.id), 0) AS prior_adjust \
                   FROM biz_receivable r \
                  WHERE r.deleted_at IS NULL \
                    AND r.status IN ('OPEN','PARTIAL') \
                    AND r.unsettled_amount > 0 \
                    AND r.currency_code <> ? \
                    AND r.period_code <= ? \
                  ORDER BY r.ar_no"
            ),
            vec![
                Value::Text(base_currency.to_string()),
                Value::Text(period_code.to_string()),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(OpenFxAr {
                receivable_id: row.get_typed("id")?,
                ar_no: row.get_typed("ar_no")?,
                currency_code: row.get_typed("currency_code")?,
                amount: row.get_typed("amount")?,
                unsettled_amount: row.get_typed("unsettled_amount")?,
                base_total_with_tax: row.get_typed("base_total_with_tax")?,
                prior_adjust: row.get_typed("prior_adjust")?,
            })
        })
        .collect()
}

/// 该期间是否已经提过重估凭证（幂等闸）。
async fn existing_voucher(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<Uuid>> {
    let row = ex
        .query_first_sql(
            "SELECT id FROM biz_voucher \
              WHERE source_type = ? AND source_no = ? AND is_red = 0 AND deleted_at IS NULL \
              ORDER BY created_at ASC LIMIT 1"
                .to_string(),
            vec![
                Value::Text(SOURCE_FX_REVALUATION.to_string()),
                Value::Text(period_code.to_string()),
            ],
        )
        .await?;
    row.map(|row| row.get_typed::<Uuid>("id")).transpose()
}

/// 落一行重估台账（同一期同一笔应收重复跑 ⇒ 覆盖为最新计算结果，不新增行）。
async fn upsert_line(
    ex: &impl SqlExecutor,
    period_code: &str,
    line: &RevalLine,
    voucher_id: Option<Uuid>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (period_code, receivable_id, ar_no, currency_code, book_rate, close_rate, \
                 open_amount, book_base, revalued_base, diff_base, voucher_id, create_by, created_at) \
             VALUES (?, ?::uuid, ?, ?, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, ?::uuid, ?::uuid, ?) \
             ON CONFLICT (period_code, receivable_id) DO UPDATE SET \
                ar_no = EXCLUDED.ar_no, currency_code = EXCLUDED.currency_code, \
                book_rate = EXCLUDED.book_rate, close_rate = EXCLUDED.close_rate, \
                open_amount = EXCLUDED.open_amount, book_base = EXCLUDED.book_base, \
                revalued_base = EXCLUDED.revalued_base, diff_base = EXCLUDED.diff_base, \
                voucher_id = EXCLUDED.voucher_id"
        ),
        vec![
            Value::Text(period_code.to_string()),
            Value::from(&line.receivable_id),
            Value::Text(line.ar_no.clone()),
            Value::Text(line.currency_code.clone()),
            Value::Text(line.book_rate().to_string()),
            Value::Text(line.close_rate.to_string()),
            Value::Text(line.open_amount.to_string()),
            Value::Text(line.book_base.to_string()),
            Value::Text(line.revalued_base.to_string()),
            Value::Text(line.diff.to_string()),
            voucher_id.map(Value::from).unwrap_or(Value::Null),
            operator.map(Value::from).unwrap_or(Value::Null),
            Value::from(now),
        ],
    )
    .await?;
    Ok(())
}

/// **月末结账时的汇兑重估**（ARCH-7 §15.6.4 ②）：对未核销外币应收按期末汇率重估，
/// 差额记为当期损益（5603）对 1122 的调整分录。
///
/// # 事务
///
/// 吃 `&impl SqlExecutor`，由调用方（`period::close_period`）在**结账事务内**调用：
/// 重估凭证与期间状态同生共死 —— 不会出现「期间封了、重估没记」或反过来。
///
/// # 返回
///
/// * `skipped = true`：该期间已提过（幂等命中），什么都没写；
/// * `voucher_id = None`：没有需要重估的外币应收，或差额合计为 0（无分录可记）；
/// * 其余：一张 `FX_REVALUATION` 凭证 + 每笔应收一行台账。
pub async fn revalue_period(
    ex: &impl SqlExecutor,
    period_code: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<RevalOutcome> {
    let close_date = month_end_of(period_code).ok_or_else(|| {
        DbError::InvalidValue(format!(
            "汇兑重估失败：会计期间码 `{period_code}` 不是 `yyyy-MM` 形态，算不出期末取汇率日"
        ))
    })?;
    let base_currency = currency::base_currency_code(ex).await?;

    // ① 幂等闸：同一期间已经提过（含「上次差额合计恰好为 0、没生成凭证」的情况也照样跳过，
    //    因为台账行已经写过，再跑只会用同一份数据得出同一结论）
    if let Some(voucher_id) = existing_voucher(ex, period_code).await? {
        tracing::info!(
            period = period_code,
            voucher = %voucher_id,
            "期末汇兑重估：该期间已提过，跳过（幂等）"
        );
        return Ok(RevalOutcome {
            period_code: period_code.to_string(),
            close_date,
            base_currency,
            lines: Vec::new(),
            net: Decimal::ZERO,
            voucher_id: Some(voucher_id),
            skipped: true,
        });
    }

    // ② 逐笔算差额（本位币应收已在 SQL 里滤掉）
    let receivables = open_fx_receivables(ex, period_code, &base_currency).await?;
    let mut lines: Vec<RevalLine> = Vec::new();
    // 币种 → 期末汇率（同一期间多笔同币种只查一次汇率表）
    let mut rates: Vec<(String, Decimal)> = Vec::new();
    for row in receivables {
        let close_rate = match rates.iter().find(|(code, _)| code == &row.currency_code) {
            Some((_, rate)) => *rate,
            None => {
                let rate = currency::spot_rate(ex, &row.currency_code, &base_currency, close_date)
                    .await?
                    .ok_or_else(|| {
                        DbError::InvalidValue(format!(
                            "期末汇兑重估失败：币种 {} 在 {close_date}（含）之前没有可用的 SPOT 汇率\
                             （本位币 {base_currency}），{period_code} 无法重估应收 {}；\
                             请先在「币种 / 汇率」里维护该币种的即时汇率",
                            row.currency_code, row.ar_no
                        ))
                    })?;
                if rate <= Decimal::ZERO {
                    return Err(DbError::InvalidValue(format!(
                        "期末汇兑重估失败：币种 {} 在 {close_date} 的汇率为 {rate}（必须 > 0），\
                         {period_code} 无法重估应收 {}",
                        row.currency_code, row.ar_no
                    )));
                }
                let rate = currency::scale_rate(rate);
                rates.push((row.currency_code.clone(), rate));
                rate
            }
        };
        let book_base = book_base_of(
            row.base_total_with_tax,
            row.amount,
            row.unsettled_amount,
            row.prior_adjust,
        );
        let revalued_base = revalued_of(row.unsettled_amount, close_rate);
        let diff = (revalued_base - book_base).round_dp(4);
        if diff.abs() < tolerance() {
            tracing::debug!(
                period = period_code,
                ar_no = row.ar_no,
                "期末汇兑重估：差额为 0（汇率未变 / 账面已按期末汇率计价），不出调整行"
            );
            continue;
        }
        lines.push(RevalLine {
            receivable_id: row.receivable_id,
            ar_no: row.ar_no,
            currency_code: row.currency_code,
            open_amount: row.unsettled_amount,
            book_base,
            close_rate,
            revalued_base,
            diff,
        });
    }

    let net: Decimal = lines
        .iter()
        .map(|line| line.diff)
        .sum::<Decimal>()
        .round_dp(4);

    // ③ 差额合计为 0：不记账（借贷同额会互相抵消，凭证没有信息量），但台账照写留痕
    if lines.is_empty() || net.abs() < tolerance() {
        for line in &lines {
            upsert_line(ex, period_code, line, None, operator, now).await?;
        }
        if lines.is_empty() {
            // 没有外币未核销应收：正常月份的常态，不刷 WARN
            tracing::debug!(
                period = period_code,
                "期末汇兑重估：本期没有未核销的外币应收，无需调整"
            );
        } else {
            tracing::warn!(
                period = period_code,
                net = %net,
                lines = lines.len(),
                "期末汇兑重估：差额合计为 0（正负相抵），本期间不生成调整凭证（台账已逐笔留痕）"
            );
        }
        return Ok(RevalOutcome {
            period_code: period_code.to_string(),
            close_date,
            base_currency,
            lines,
            net,
            voucher_id: None,
            skipped: false,
        });
    }

    // ④ 生成调整分录：一张凭证两行（1122 应收账款 / 5603 财务费用-汇兑损益）
    let ar_account =
        voucher::account_required(ex, ACCOUNT_AR, "汇兑重估凭证的 1122 应收账款").await?;
    let fx_account = voucher::account_required(
        ex,
        ACCOUNT_FX_GAIN_LOSS,
        // 卡面写 6603，本仓科目表是小企业准则（财务费用 = 5603），口径见 `ACCOUNT_FX_GAIN_LOSS`
        "汇兑重估凭证的 5603 财务费用（卡面 6603 在本库不存在）",
    )
    .await?;
    let amount = net.abs();
    let label = format!(
        "{period_code} 期末汇兑重估（{} 笔外币应收，期末汇率取 {close_date}）",
        lines.len()
    );
    let (debit, credit, summary) = if net > Decimal::ZERO {
        (
            (ar_account, format!("{label} 调增应收账款")),
            (fx_account, format!("{label} 汇兑收益")),
            label.clone(),
        )
    } else {
        (
            (fx_account, format!("{label} 汇兑损失")),
            (ar_account, format!("{label} 调减应收账款")),
            label.clone(),
        )
    };
    let lines_entry = vec![
        voucher::EntryLine {
            line_no: 1,
            summary: debit.1,
            account: debit.0,
            debit: amount,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        },
        voucher::EntryLine {
            line_no: 2,
            summary: credit.1,
            account: credit.0,
            debit: Decimal::ZERO,
            credit: amount,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        },
    ];
    let voucher_id = voucher::post_voucher(
        ex,
        &voucher::PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: SOURCE_FX_REVALUATION,
            source_id: None,
            source_no: Some(period_code),
            voucher_date: close_date,
            summary,
            lines: lines_entry,
            remark: Some("ARCH-7 §15.6.4 ②：未核销外币应收按期末汇率重估，差额计当期损益"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await?;

    // ⑤ 台账（凭证会提高 1122 的账面人民币 ⇒ 下期必须把它当 prior_adjust 算进去）
    for line in &lines {
        upsert_line(ex, period_code, line, Some(voucher_id), operator, now).await?;
    }
    tracing::info!(
        period = period_code,
        voucher = %voucher_id,
        net = %net,
        lines = lines.len(),
        "期末汇兑重估：已生成调整凭证"
    );
    Ok(RevalOutcome {
        period_code: period_code.to_string(),
        close_date,
        base_currency,
        lines,
        net,
        voucher_id: Some(voucher_id),
        skipped: false,
    })
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    fn dec(text: &str) -> Decimal {
        text.parse::<Decimal>().expect("定点数可解析")
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("固定日期合法")
    }

    // ---------- 纯函数 ----------

    #[test]
    fn month_end_handles_31_30_and_february() {
        assert_eq!(month_end_of("2026-01"), Some(date(2026, 1, 31)));
        assert_eq!(month_end_of("2026-04"), Some(date(2026, 4, 30)));
        assert_eq!(month_end_of("2026-02"), Some(date(2026, 2, 28)));
        assert_eq!(
            month_end_of("2024-02"),
            Some(date(2024, 2, 29)),
            "闰年 2 月必须给 29 号（拿错日期取汇率 = 用错汇率重估账）"
        );
        assert_eq!(
            month_end_of("2026-12"),
            Some(date(2026, 12, 31)),
            "12 月要跨年算（下一年 1 月 1 日的前一天）"
        );
    }

    #[test]
    fn month_end_rejects_malformed_periods() {
        for bad in [
            "2026-13", "2026-00", "26-01", "202601", "2026-1", "abcd-01", "",
        ] {
            assert_eq!(month_end_of(bad), None, "非法期间码 {bad} 必须返回 None");
        }
    }

    #[test]
    fn book_base_follows_unsettled_share() {
        // 1000 USD @ 7.1 ⇒ 账面 7100；未核销 400/1000 ⇒ 账面 2840
        assert_eq!(
            book_base_of(dec("7100"), dec("1000"), dec("400"), Decimal::ZERO),
            dec("2840")
        );
        // 全未核销 ⇒ 全额
        assert_eq!(
            book_base_of(dec("7100"), dec("1000"), dec("1000"), Decimal::ZERO),
            dec("7100")
        );
        // 历史重估差额累计进来（上月提过 +100）
        assert_eq!(
            book_base_of(dec("7100"), dec("1000"), dec("400"), dec("100")),
            dec("2940")
        );
        // 除零防护：amount = 0 的脏数据不许 panic
        assert_eq!(
            book_base_of(dec("7100"), Decimal::ZERO, dec("400"), Decimal::ZERO),
            dec("7100")
        );
    }

    #[test]
    fn diff_sign_follows_rate_movement() {
        // 汇率 7.1 → 7.9：未核销 1000 USD 账面 7100，重估 7900 ⇒ +800（调增应收）
        let book = book_base_of(dec("7100"), dec("1000"), dec("1000"), Decimal::ZERO);
        assert_eq!(diff_of(dec("1000"), dec("7.9"), book), dec("800"));
        // 汇率 7.1 → 6.5：账面 7100，重估 6500 ⇒ -600（调减应收）
        assert_eq!(diff_of(dec("1000"), dec("6.5"), book), dec("-600"));
        // 汇率没动 ⇒ 0（本位币单 / 汇率未变的月份不该出调整行）
        assert_eq!(diff_of(dec("1000"), dec("7.1"), book), Decimal::ZERO);
    }

    #[test]
    fn diff_rounds_to_four_decimals() {
        // 333.3333 USD @ 7.12345678 = 2374.48535588 ⇒ round 4 位 = 2374.4854
        let revalued = revalued_of(dec("333.3333"), dec("7.12345678"));
        assert_eq!(revalued, dec("2374.4854"));
        // 1000 × 1.00000001 = 1000.00001 ⇒ 4 位定标后回到 1000.0000，差额为 0
        assert_eq!(
            diff_of(dec("1000"), dec("1.00000001"), dec("1000.0000")),
            dec("0.0000"),
            "4 位定标后相等 ⇒ 不产生调整行"
        );
        // 差正好 1 个最小单位的量级：重估 999.9999 - 账面 1000.0001 = -0.0002
        assert_eq!(
            diff_of(dec("999.9999"), Decimal::ONE, dec("1000.0001")),
            dec("-0.0002")
        );
    }

    #[test]
    fn reval_line_book_rate_is_derived_from_book_base() {
        let line = RevalLine {
            receivable_id: Uuid::nil(),
            ar_no: "AR-20261107-000001".to_string(),
            currency_code: "USD".to_string(),
            open_amount: dec("400"),
            book_base: dec("2840"),
            close_rate: dec("7.9"),
            revalued_base: dec("3160"),
            diff: dec("320"),
        };
        assert_eq!(
            line.book_rate(),
            dec("7.10000000"),
            "账面汇率 = 账面人民币 / 未核销原币（8 位定标）"
        );
    }

    // ---------- 真库（ERP_DB_URL 未设则 SKIP） ----------

    // 真库用例串行：共用 2050-01 期间 / FXR- 前缀夹具，并行跑会自己撞自己
    // （唯一索引 uk_biz_ar_no / uk_biz_period_code）。与 `voucher::tests` 共用同一把锁，
    // 免得两模块同时改 `biz_accounting_period` 的 2050-xx 期间。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = crate::business::erp::voucher_query::tests::DB_LOCK
            .lock()
            .await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    const PERIOD: &str = "2050-01";
    const CUSTOMER: &str = "b2c1f041-0000-4000-8000-000000000001";

    async fn cleanup() {
        for sql in [
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_type = 'FX_REVALUATION' AND source_no LIKE '2050-%')",
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
             (SELECT id FROM biz_voucher WHERE source_type = 'FX_REVALUATION' AND source_no LIKE '2050-%')",
            "DELETE FROM biz_voucher WHERE source_type = 'FX_REVALUATION' AND source_no LIKE '2050-%'",
            "DELETE FROM biz_fx_revaluation WHERE period_code LIKE '2050-%'",
            "DELETE FROM biz_receivable WHERE ar_no LIKE 'FXR-%'",
            "DELETE FROM biz_exchange_rate WHERE from_currency IN ('USD','EUR')",
            "DELETE FROM biz_accounting_period WHERE period_code LIKE '2050-%'",
        ] {
            let _ = db().exec_sql(sql.to_string(), vec![]).await;
        }
    }

    /// 造一条外币应收（未核销）+ 期末汇率 + 开放期间。
    async fn fixtures_with(
        currency: &str,
        base_with_tax: &str,
        amount: &str,
        settled: &str,
        tag: &str,
    ) {
        db().exec_sql(
            format!(
                "INSERT INTO biz_customer (id, code, name, status) \
                 VALUES ('{CUSTOMER}', 'FXR-CUST', 'FXR 单测客户', 'ACTIVE') \
                 ON CONFLICT DO NOTHING"
            ),
            vec![],
        )
        .await
        .expect("种客户");
        let settled: Decimal = settled.parse().expect("已核销金额");
        let amount_dec: Decimal = amount.parse().expect("原币金额");
        db().exec_sql(
            format!(
                "INSERT INTO biz_receivable \
                    (ar_no, customer_id, source_type, source_id, source_no, currency_code, amount, \
                     received_amount, unsettled_amount, credit_days, due_date, period_code, status, \
                     exchange_rate, base_total_amount, base_total_tax, base_total_with_tax) \
                 VALUES (?, '{CUSTOMER}', 'MANUAL', gen_random_uuid(), 'FXR-SRC', ?, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, 30, \
                         ?::text::date, ?, ?, 7.1, ?::text::numeric, 0, ?::text::numeric)"
            ),
            vec![
                Value::Text(format!("FXR-{currency}-{tag}")),
                Value::Text(currency.to_string()),
                Value::Text(amount_dec.to_string()),
                Value::Text(settled.to_string()),
                Value::Text((amount_dec - settled).to_string()),
                Value::Text("2050-01-10".to_string()),
                Value::Text(PERIOD.to_string()),
                Value::Text(if settled > Decimal::ZERO {
                    "PARTIAL".to_string()
                } else {
                    "OPEN".to_string()
                }),
                Value::Text(base_with_tax.to_string()),
                // base_total_with_tax = base_total_amount + base_total_tax（ck_biz_ar_base_amount 兜底）
                Value::Text(base_with_tax.to_string()),
            ],
        )
        .await
        .expect("种应收");
        db().exec_sql(
            format!(
                "INSERT INTO biz_accounting_period \
                    (id, period_year, period_month, period_code, start_date, end_date, status) \
                 VALUES (gen_random_uuid(), 2050, 1, '{PERIOD}', '2050-01-01', '2050-01-31', 'OPEN') \
                 ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN', \
                     closed_at = NULL, closed_by = NULL"
            ),
            vec![],
        )
        .await
        .expect("种期间");
    }

    /// 单笔夹具的便捷包装（等价 `fixtures_with(.., "0001")`）。
    async fn fixtures(currency: &str, base_with_tax: &str, amount: &str, settled: &str) {
        fixtures_with(currency, base_with_tax, amount, settled, "0001").await;
    }

    async fn seed_rate(currency: &str, on_date: &str, rate: &str) {
        db().exec_sql(
            format!(
                "INSERT INTO biz_exchange_rate \
                    (from_currency, to_currency, rate_type, effective_date, rate) \
                 VALUES (?, 'CNY', 'SPOT', ?::text::date, ?::text::numeric) \
                 ON CONFLICT (from_currency, to_currency, rate_type, effective_date) \
                 DO UPDATE SET rate = EXCLUDED.rate"
            ),
            vec![
                Value::Text(currency.to_string()),
                Value::Text(on_date.to_string()),
                Value::Text(rate.to_string()),
            ],
        )
        .await
        .expect("种汇率");
    }

    /// 期次命名空间：**2050-xx 归本模块独占**（2031 = period::tests、2033 = remind/stocktake、
    /// 2034–2035 = opening、2036–2038 = year_end、2039 = reminder、2040 = period_items、
    /// 2026 = **生产期间，绝不碰**）。换期前先 `grep -rn "2050-" src/` 确认没人抢。
    ///
    /// 真库：外币未核销应收 + 期末汇率上涨 ⇒ 一张 FX_REVALUATION 凭证（借 1122 / 贷 5603），
    /// 台账一行，差额 = 原币 × (期末汇率 − 账面汇率)。
    #[tokio::test]
    async fn revalue_period_posts_gain_and_loss_lines() {
        let Some(_guard) = setup().await else {
            return;
        };
        fixtures("USD", "7100", "1000", "0").await;
        seed_rate("USD", "2050-01-31", "7.9").await;

        let outcome = revalue_period(db(), PERIOD, None, Utc::now())
            .await
            .expect("重估应成功");
        assert!(!outcome.skipped);
        assert_eq!(outcome.close_date, date(2050, 1, 31), "取汇率日必须是月末");
        assert_eq!(outcome.net, dec("800"), "1000 USD × (7.9 − 7.1) = 800");
        let voucher_id = outcome.voucher_id.expect("应生成重估凭证");

        let rows = db()
            .query_sql(
                "SELECT account_code, debit_amount, credit_amount FROM biz_voucher_entry \
                  WHERE voucher_id = ?::uuid ORDER BY line_no"
                    .to_string(),
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("查分录");
        assert_eq!(rows.len(), 2, "一张凭证两行：1122 / 5603");
        let debit_acc: String = rows[0].get_typed("account_code").expect("科目");
        let credit_acc: String = rows[1].get_typed("account_code").expect("科目");
        assert_eq!(debit_acc, ACCOUNT_AR, "应收调增在借方");
        assert_eq!(
            credit_acc, ACCOUNT_FX_GAIN_LOSS,
            "汇兑收益在贷方（财务费用冲减）"
        );
        assert_eq!(
            rows[0].get_typed::<Decimal>("debit_amount").unwrap(),
            dec("800")
        );
        assert_eq!(
            rows[1].get_typed::<Decimal>("credit_amount").unwrap(),
            dec("800")
        );

        let sl = db()
            .query_first_sql(
                format!("SELECT open_amount, book_base, revalued_base, diff_base, book_rate FROM {TABLE} WHERE period_code = ?"),
                vec![Value::Text(PERIOD.to_string())],
            )
            .await
            .expect("查台账")
            .expect("台账必须留痕");
        assert_eq!(sl.get_typed::<Decimal>("open_amount").unwrap(), dec("1000"));
        assert_eq!(sl.get_typed::<Decimal>("book_base").unwrap(), dec("7100"));
        assert_eq!(
            sl.get_typed::<Decimal>("revalued_base").unwrap(),
            dec("7900")
        );
        assert_eq!(sl.get_typed::<Decimal>("diff_base").unwrap(), dec("800"));
        assert_eq!(
            sl.get_typed::<Decimal>("book_rate").unwrap(),
            dec("7.10000000")
        );

        cleanup().await;
        println!("[SO41-①] 真库 USD 应收重估：借 1122 800 / 贷 5603 800，台账留痕 ✓");
    }

    /// 真库：汇率下跌 ⇒ 反向分录（借 5603 / 贷 1122）。
    #[tokio::test]
    async fn revalue_period_posts_loss_when_rate_falls() {
        let Some(_guard) = setup().await else {
            return;
        };
        fixtures("USD", "7100", "1000", "0").await;
        seed_rate("USD", "2050-01-31", "6.5").await;

        let outcome = revalue_period(db(), PERIOD, None, Utc::now())
            .await
            .expect("重估应成功");
        assert_eq!(outcome.net, dec("-600"));
        let voucher_id = outcome.voucher_id.expect("应生成重估凭证");
        let rows = db()
            .query_sql(
                "SELECT account_code, debit_amount, credit_amount FROM biz_voucher_entry \
                  WHERE voucher_id = ?::uuid ORDER BY line_no"
                    .to_string(),
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("查分录");
        assert_eq!(
            rows[0].get_typed::<String>("account_code").unwrap(),
            ACCOUNT_FX_GAIN_LOSS
        );
        assert_eq!(
            rows[1].get_typed::<String>("account_code").unwrap(),
            ACCOUNT_AR
        );
        assert_eq!(
            rows[0].get_typed::<Decimal>("debit_amount").unwrap(),
            dec("600")
        );
        assert_eq!(
            rows[1].get_typed::<Decimal>("credit_amount").unwrap(),
            dec("600")
        );
        cleanup().await;
        println!("[SO41-②] 真库汇率下跌：借 5603 600 / 贷 1122 600 ✓");
    }

    /// 真库：本位币应收**不做重估**（不出凭证、不写台账）。
    #[tokio::test]
    async fn revalue_period_ignores_base_currency_receivables() {
        let Some(_guard) = setup().await else {
            return;
        };
        fixtures("CNY", "1130", "1130", "0").await;
        seed_rate("USD", "2050-01-31", "7.9").await;

        let outcome = revalue_period(db(), PERIOD, None, Utc::now())
            .await
            .expect("重估应成功");
        assert_eq!(outcome.net, Decimal::ZERO);
        assert!(outcome.voucher_id.is_none(), "本位币单不该有重估凭证");
        assert!(outcome.lines.is_empty(), "本位币单不进明细");

        let count_sql = format!("SELECT count(*) FROM {TABLE} WHERE period_code = ?");
        let leaked = db()
            .scalar_with_params::<Option<i64>>(
                count_sql.as_str(),
                vec![Value::Text(PERIOD.to_string())],
            )
            .await
            .expect("查台账")
            .unwrap_or(0);
        assert_eq!(leaked, 0, "本位币单不许留重估台账行");
        cleanup().await;
        println!("[SO41-③] 真库本位币应收：0 凭证 / 0 台账行 ✓");
    }

    /// 真库：同一期间跑两遍 ⇒ 幂等（第二遍 skipped，不新增凭证）。
    #[tokio::test]
    async fn revalue_period_is_idempotent() {
        let Some(_guard) = setup().await else {
            return;
        };
        fixtures("USD", "7100", "1000", "0").await;
        seed_rate("USD", "2050-01-31", "7.9").await;

        let first = revalue_period(db(), PERIOD, None, Utc::now())
            .await
            .expect("第一次重估");
        let first_id = first.voucher_id.expect("第一次应有凭证");
        let second = revalue_period(db(), PERIOD, None, Utc::now())
            .await
            .expect("第二次重估");
        assert!(second.skipped, "第二遍必须幂等跳过");
        assert_eq!(second.voucher_id, Some(first_id), "必须返回同一张凭证");

        let count = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_voucher WHERE source_type = 'FX_REVALUATION' \
                  AND source_no = ?",
                vec![Value::Text(PERIOD.to_string())],
            )
            .await
            .expect("查凭证数")
            .unwrap_or(0);
        assert_eq!(count, 1, "同一期间只许一张重估凭证");
        cleanup().await;
        println!("[SO41-④] 真库幂等：同期间重跑 skipped=true，凭证数 1 ✓");
    }

    /// 真库：外币应收但**期末没有汇率** ⇒ 明确报错（不静默按 1 重估）。
    #[tokio::test]
    async fn revalue_period_refuses_missing_close_rate() {
        let Some(_guard) = setup().await else {
            return;
        };
        fixtures("USD", "7100", "1000", "0").await;
        // 只种「月末之后」才生效的汇率：月末那天取不到
        seed_rate("USD", "2050-02-01", "7.9").await;

        let err = revalue_period(db(), PERIOD, None, Utc::now())
            .await
            .expect_err("缺期末汇率必须拒绝重估");
        let text = format!("{err}");
        assert!(text.contains("USD"), "文案要点名币种：{text}");
        assert!(text.contains("2050-01-31"), "文案要点名取汇率日：{text}");
        cleanup().await;
        println!("[SO41-⑤] 真库缺期末汇率：拒绝并点名币种/日期 ✓ {text}");
    }

    /// 真库：部分核销 ⇒ 只按未核销部分重估（比例法）。
    #[tokio::test]
    async fn revalue_period_uses_unsettled_share() {
        let Some(_guard) = setup().await else {
            return;
        };
        fixtures("USD", "7100", "1000", "600").await;
        seed_rate("USD", "2050-01-31", "7.9").await;

        let outcome = revalue_period(db(), PERIOD, None, Utc::now())
            .await
            .expect("重估应成功");
        assert_eq!(outcome.lines.len(), 1);
        let line = &outcome.lines[0];
        assert_eq!(line.open_amount, dec("400"), "只重估未核销部分");
        assert_eq!(line.book_base, dec("2840"), "账面人民币按未核销占比");
        assert_eq!(line.revalued_base, dec("3160"));
        assert_eq!(line.diff, dec("320"));
        cleanup().await;
        println!("[SO41-⑥] 真库部分核销：400 USD 账面 2840 → 重估 3160，差额 320 ✓");
    }

    /// 真库：**结账链路真的会触发重估** —— 走 `period::close_period`（不是只调模块函数），
    /// 断言期间 CLOSED + 重估凭证在库 + 台账挂上凭证 id。
    #[tokio::test]
    async fn close_period_triggers_fx_revaluation() {
        let Some(_guard) = setup().await else {
            return;
        };
        fixtures("USD", "7100", "1000", "0").await;
        seed_rate("USD", "2050-01-31", "7.9").await;
        let period_id = db()
            .query_first_sql(
                "SELECT id FROM biz_accounting_period WHERE period_code = ?".to_string(),
                vec![Value::Text(PERIOD.to_string())],
            )
            .await
            .expect("查期间")
            .expect("期间必须存在")
            .get_typed::<Uuid>("id")
            .expect("期间 id");

        let closed = crate::business::erp::period::close_period(
            period_id,
            None,
            crate::business::erp::period_test_support::operator(),
        )
        .await
        .expect("结账应成功（无阻塞项）");
        assert_eq!(closed.status, "CLOSED", "期间必须已结账");

        let voucher = db()
            .query_first_sql(
                "SELECT id, summary, voucher_date FROM biz_voucher \
                  WHERE source_type = 'FX_REVALUATION' AND source_no = ? AND is_red = 0"
                    .to_string(),
                vec![Value::Text(PERIOD.to_string())],
            )
            .await
            .expect("查重估凭证")
            .expect("结账必须产出汇兑重估凭证");
        let voucher_id: Uuid = voucher.get_typed("id").expect("凭证 id");
        let summary: String = voucher.get_typed("summary").expect("摘要");
        assert!(summary.contains("汇兑重估"), "摘要必须可辨认：{summary}");
        assert_eq!(
            voucher
                .get_typed::<NaiveDate>("voucher_date")
                .expect("凭证日期"),
            date(2050, 1, 31),
            "重估凭证日期必须是期末（月末）"
        );

        let linked = db()
            .query_first_sql(
                format!("SELECT voucher_id, diff_base FROM {TABLE} WHERE period_code = ?"),
                vec![Value::Text(PERIOD.to_string())],
            )
            .await
            .expect("查台账")
            .expect("台账必须有行");
        assert_eq!(
            linked
                .get_typed::<Option<Uuid>>("voucher_id")
                .expect("凭证引用"),
            Some(voucher_id),
            "台账必须挂上重估凭证 id（可回溯）"
        );
        assert_eq!(
            linked.get_typed::<Decimal>("diff_base").expect("差额"),
            dec("800")
        );

        cleanup().await;
        println!("[SO41-⑦] 真库结账链路：close_period → FX 凭证 + 台账挂凭证 + 期间 CLOSED ✓");
    }

    /// 真库：两笔应收**差额正负相抵**（一笔 +800 / 一笔 −800）⇒ 合计为 0，
    /// 本期间不生成凭证（借贷同额会互相抵消，凭证没有信息量），但台账逐笔留痕（voucher_id NULL）。
    #[tokio::test]
    async fn revalue_period_skips_voucher_when_net_is_zero() {
        let Some(_guard) = setup().await else {
            return;
        };
        // ① 账面按 7.1 锁 → 期末 7.9 ⇒ +800
        fixtures_with("USD", "7100", "1000", "0", "0001").await;
        // ② 账面按 8.7 锁（库里 rate 8.7）→ 期末 7.9 ⇒ −800
        fixtures_with("USD", "8700", "1000", "0", "0002").await;
        seed_rate("USD", "2050-01-31", "7.9").await;

        let outcome = revalue_period(db(), PERIOD, None, Utc::now())
            .await
            .expect("重估应成功");
        assert_eq!(outcome.lines.len(), 2, "两笔应收都要算差额");
        assert_eq!(outcome.net, Decimal::ZERO, "+800 与 −800 相抵");
        assert!(
            outcome.voucher_id.is_none(),
            "合计为 0 ⇒ 不生成调整凭证（不能凭空造一张借贷同额的凭证）"
        );

        let rows = db()
            .query_sql(
                format!("SELECT ar_no, diff_base, voucher_id FROM {TABLE} WHERE period_code = ? ORDER BY ar_no"),
                vec![Value::Text(PERIOD.to_string())],
            )
            .await
            .expect("查台账");
        assert_eq!(rows.len(), 2, "台账必须逐笔留痕：{rows:?}");
        for row in &rows {
            assert!(
                row.get_typed::<Option<Uuid>>("voucher_id")
                    .expect("凭证引用")
                    .is_none(),
                "没有凭证时台账的 voucher_id 必须为 NULL"
            );
        }
        let diffs: Vec<Decimal> = rows
            .iter()
            .map(|row| row.get_typed::<Decimal>("diff_base").expect("差额"))
            .collect();
        assert_eq!(diffs, vec![dec("800"), dec("-800")], "逐笔差额符号相反");

        cleanup().await;
        println!("[SO41-⑧] 真库正负相抵：net=0 不生成凭证，台账 2 行留痕 ✓");
    }
}
