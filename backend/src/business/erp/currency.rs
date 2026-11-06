//! 多币种：本位币取法 / SPOT 汇率取数 / **建单汇率锁定** / 原币 → 本位币换算（SO-40 · ARCH-7 §15.6）
//!
//! # 口径（唯一真相，改一处必须同步本注释 / 迁移 V0314 / 卡内汇报）
//!
//! | 事项 | 口径 |
//! |------|------|
//! | 汇率方向 | `1 from_currency = rate to_currency`（SO-39 的 `biz_exchange_rate` 只存「外币 → 本位币」） |
//! | 取数 | `rate_type = 'SPOT'` 且 `effective_date <= 单据日期`，取**最近一条**（不是精确等于单据日：周末/节假日没有牌价，沿用到上一个工作日） |
//! | 锁定 | 建单那一刻取到的 rate **落到单据行**（`exchange_rate` 列）；此后汇率表怎么变都不回写单据（§15.6.4 ①） |
//! | 本位币 | `SELECT code FROM biz_currency WHERE is_base AND status='ACTIVE'`（部分唯一索引保证恰好 1 行）**不写死 CNY** |
//! | 本位币单 | `exchange_rate = 1`、`base_* = 原币 *`，**不写汇率行**（`from <> to` 的 CHECK 会拒自反汇率） |
//! | 外币单取不到汇率 | **4xx 拒绝建单**，绝不默认 1（默认 1 = 把外币单按人民币入账，账直接错） |
//! | 换算 | 逐项 `round(原币 × rate, 4)`，含税列**取前两项之和**（尾差落含税列，恒等式可被 CHECK 兜底） |
//!
//! # 为什么「尾差归到含税金额」而不是三项各自换算
//!
//! `round(a×r,4) + round(t×r,4)` 与 `round((a+t)×r,4)` 在 4 位小数下必然有 1 个最小单位的差。
//! 本仓四张表的 `base_total_with_tax = base_total_amount + base_total_tax` 是**库层 CHECK**
//! （V0314），所以含税列只能取前两项之和 —— 差额（≤ 1 个最小单位）自然落在含税列，
//! 这也是会计上「尾差进价税合计」的常规做法。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_currency / biz_exchange_rate 是全局主数据（V0272 无 dept_id / 归属人列），
//! 本模块只做「按码取值」，不提供分页接口。
//! ```

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;

use crate::shared::error_code;

/// 取汇率用的报价类型（与 `ck_biz_exchange_rate_type` 的 SPOT 逐字一致）。
pub const RATE_TYPE_SPOT: &str = "SPOT";
/// 本位币兜底值：`biz_currency` 里一行本位币都没有时用它（新装库 V0272 种的就是 CNY）。
pub const DEFAULT_BASE_CURRENCY: &str = "CNY";
/// 汇率表的有效小数位（`numeric(18,8)`）。
pub const RATE_SCALE: u32 = 8;
/// 本仓金额列统一 4 位小数（`numeric(18,4)`）。
pub const AMOUNT_SCALE: u32 = 4;

// ==================== 纯逻辑 ====================

/// 建单时「锁定」的汇率（单据行要落的三件事）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedRate {
    /// 单据原币币种（三位大写）。
    pub currency_code: String,
    /// 本位币币种（取数当时的账套本位币，落库只为可读性，不参与计算）。
    pub base_currency: String,
    /// `1 原币 = exchange_rate 本位币`。
    pub exchange_rate: Decimal,
}

/// 一次换算的三个本位币金额（与四张表的 `base_total_*` 三列一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseAmounts {
    /// 本位币不含税金额。
    pub amount: Decimal,
    /// 本位币税额。
    pub tax: Decimal,
    /// 本位币价税合计（= 前两项之和，尾差落这里）。
    pub with_tax: Decimal,
}

impl BaseAmounts {
    /// 原币三项 × 汇率 → 本位币三项。
    ///
    /// 四舍五入（`MidpointAwayFromZero`）而不是 `round_dp` 的默认银行家舍入 ——
    /// 与 `sales_order_model::line_amounts` 同口径（财务习惯「五入」，不是「取偶」）。
    pub fn of(amount: Decimal, tax: Decimal, rate: Decimal) -> Self {
        let base_amount = scale_amount(amount * rate);
        let base_tax = scale_amount(tax * rate);
        Self {
            amount: base_amount,
            tax: base_tax,
            // 尾差归含税：**不**再按 with_tax × rate 单独舍入（那会与恒等式差 1 个最小单位）
            with_tax: base_amount + base_tax,
        }
    }

    /// 三项是否满足库层恒等式 `with_tax = amount + tax`（浮点无关，Decimal 精确相等）。
    pub fn identity_holds(&self) -> bool {
        self.with_tax == self.amount + self.tax
    }
}

/// 金额定标：4 位小数、四舍五入。
pub fn scale_amount(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(AMOUNT_SCALE, RoundingStrategy::MidpointAwayFromZero)
}

/// 汇率定标：8 位小数、四舍五入（落库前用，避免 `numeric(18,8)` 静默截断）。
pub fn scale_rate(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(RATE_SCALE, RoundingStrategy::MidpointAwayFromZero)
}

/// 币种码规范化：去空格 + 转大写，并要求 `^[A-Z]{3}$`。
///
/// 返回 `None` 即非法（调用方给 400，**不要**静默回落成 CNY：用户填错币种却按人民币入账，
/// 比报错危险得多）。
pub fn normalize_currency(raw: &str) -> Option<String> {
    let text = raw.trim().to_ascii_uppercase();
    let bytes = text.as_bytes();
    let ok = bytes.len() == 3 && bytes.iter().all(u8::is_ascii_uppercase);
    if ok { Some(text) } else { None }
}

/// 从「(生效日, 汇率) 列表」里取**生效日 ≤ 单据日**的最晚一条。
///
/// 纯函数，取数口径的**唯一实现**：SQL 只是把这事下推给索引
/// （`uk_biz_exchange_rate` 最左前缀 + `ORDER BY effective_date DESC LIMIT 1`），
/// 单测直接喂列表验证「未来汇率不算数、同一天取最新」。
pub fn latest_on_or_before(rows: &[(NaiveDate, Decimal)], on_date: NaiveDate) -> Option<Decimal> {
    rows.iter()
        .filter(|(date, _)| *date <= on_date)
        .max_by_key(|(date, _)| *date)
        .map(|(_, rate)| *rate)
}

/// 锁定决策的结果：要么锁到了汇率，要么「外币单但取不到汇率」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOutcome {
    /// 已锁定。
    Locked(LockedRate),
    /// 外币单、但在单据日（含）之前没有任何 SPOT 汇率 —— 必须 4xx 拒绝建单。
    MissingRate {
        /// 单据币种。
        currency_code: String,
        /// 账套本位币。
        base_currency: String,
        /// 单据日期（取数上界）。
        on_date: NaiveDate,
    },
}

impl LockOutcome {
    /// 转成 handler 侧结果：`MissingRate` → `400 + PARAM_INVALID`（附可直接照做的文案）。
    pub fn into_locked(self, doc_label: &str) -> std::result::Result<LockedRate, ApiError> {
        match self {
            Self::Locked(rate) => Ok(rate),
            Self::MissingRate {
                currency_code,
                base_currency,
                on_date,
            } => Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!(
                    "{doc_label}币种 {currency_code} 在 {on_date}（含）之前没有可用的 SPOT 汇率\
                     （本位币 {base_currency}）：请先在「币种 / 汇率」里维护该币种的即时汇率；\
                     外币单不允许按汇率 1 入账"
                ),
            )),
        }
    }
}

/// 锁定决策（纯函数）：把「账套本位币 + 请求币种 + 查到的 SPOT 汇率」拼成 [`LockOutcome`]。
///
/// 三条分支：
/// 1. 未指定币种 ⇒ 用账套本位币、`rate = 1`（不查汇率表）；
/// 2. 指定的是本位币 ⇒ `rate = 1`（哪怕汇率表里恰好有反向行也不用 —— `from <> to` 本就不允许）；
/// 3. 外币 ⇒ 必须有汇率，否则 [`LockOutcome::MissingRate`]。
pub fn decide(
    base_currency: &str,
    requested: Option<&str>,
    spot_rate: Option<Decimal>,
    on_date: NaiveDate,
) -> LockOutcome {
    let base = base_currency.trim().to_ascii_uppercase();
    let currency_code = match requested {
        None => base.clone(),
        Some(raw) => normalize_currency(raw).unwrap_or_else(|| raw.trim().to_string()),
    };
    if currency_code == base {
        return LockOutcome::Locked(LockedRate {
            currency_code: base.clone(),
            base_currency: base,
            exchange_rate: Decimal::ONE,
        });
    }
    match spot_rate {
        // rate <= 0 的脏数据（库里 CHECK 已拦，但别让 0 流进金额换算）同样按「取不到汇率」处理
        Some(rate) if rate > Decimal::ZERO => LockOutcome::Locked(LockedRate {
            currency_code,
            base_currency: base,
            exchange_rate: scale_rate(rate),
        }),
        _ => LockOutcome::MissingRate {
            currency_code,
            base_currency: base,
            on_date,
        },
    }
}

// ==================== 库访问 ====================

/// 账套本位币：`biz_currency WHERE is_base AND status='ACTIVE'`。
///
/// 部分唯一索引保证最多 1 行；一行都没有时回落 [`DEFAULT_BASE_CURRENCY`] 并打 warn
/// （宁可让单据按 CNY 记，也不要让整个开单流程因为主数据缺失而 500 ——
/// 但这是账套配置错误，日志里必须看得见）。
pub async fn base_currency_code(ex: &impl SqlExecutor) -> DbResult<String> {
    let row = ex
        .query_first_sql(
            "SELECT code FROM biz_currency WHERE is_base AND status = 'ACTIVE' LIMIT 1".to_string(),
            vec![],
        )
        .await?;
    match row {
        Some(row) => row.get_typed::<String>("code"),
        None => {
            tracing::warn!(
                fallback = DEFAULT_BASE_CURRENCY,
                "biz_currency 里没有 is_base 且 ACTIVE 的行：账套本位币缺失，按 CNY 兜底"
            );
            Ok(DEFAULT_BASE_CURRENCY.to_string())
        }
    }
}

/// 取「截至 `on_date`（含）最近一条 SPOT 汇率」：`1 from = rate to`。
///
/// SQL 与 SO-39 交接的取数口径逐字一致；`uk_biz_exchange_rate`
/// `(from_currency, to_currency, rate_type, effective_date)` 的最左前缀正好覆盖。
pub async fn spot_rate(
    ex: &impl SqlExecutor,
    from_currency: &str,
    to_currency: &str,
    on_date: NaiveDate,
) -> DbResult<Option<Decimal>> {
    let row = ex
        .query_first_sql(
            "SELECT rate FROM biz_exchange_rate \
              WHERE from_currency = ? AND to_currency = ? AND rate_type = ? \
                AND effective_date <= ?::text::date \
              ORDER BY effective_date DESC LIMIT 1"
                .to_string(),
            vec![
                Value::Text(from_currency.to_string()),
                Value::Text(to_currency.to_string()),
                Value::Text(RATE_TYPE_SPOT.to_string()),
                Value::Text(on_date.to_string()),
            ],
        )
        .await?;
    match row {
        Some(row) => Ok(Some(row.get_typed::<Decimal>("rate")?)),
        None => Ok(None),
    }
}

/// **建单汇率锁定**：按请求币种 + 单据日期查一次 SPOT，得到要落到单据行的三件事。
///
/// 调用点（四个单据的建单入口）**必须**用它，不要在业务代码里另写取汇率的 SQL。
pub async fn lock(
    ex: &impl SqlExecutor,
    requested: Option<&str>,
    on_date: NaiveDate,
) -> DbResult<LockOutcome> {
    let base = base_currency_code(ex).await?;
    // 请求的是本位币 / 未指定 ⇒ 不必查汇率表（少一次往返，也不会被「历史上没有 CNY 行」卡住）
    let needs_lookup = match requested.map(str::trim).filter(|text| !text.is_empty()) {
        None => false,
        Some(raw) => normalize_currency(raw)
            .map(|code| code != base)
            .unwrap_or(false),
    };
    if !needs_lookup {
        return Ok(decide(&base, requested, None, on_date));
    }
    let code = normalize_currency(requested.unwrap_or_default()).unwrap_or_default();
    let rate = spot_rate(ex, &code, &base, on_date).await?;
    Ok(decide(&base, requested, rate, on_date))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("固定日期合法")
    }

    fn dec(raw: &str) -> Decimal {
        raw.parse().expect("测试用十进制字面量合法")
    }

    // ---------- 币种码规范化 ----------

    #[test]
    fn normalize_currency_accepts_three_uppercase_letters() {
        assert_eq!(normalize_currency("USD").as_deref(), Some("USD"));
        // 前后空格 + 小写都要能救回来（前端 input 很常见）
        assert_eq!(normalize_currency("  usd ").as_deref(), Some("USD"));
        assert_eq!(normalize_currency("Hkd").as_deref(), Some("HKD"));
    }

    #[test]
    fn normalize_currency_rejects_bad_input() {
        // 长度不对 / 含数字 / 空串 / 中文：一律 None（调用方 400，不静默回落 CNY）
        assert_eq!(normalize_currency("US"), None);
        assert_eq!(normalize_currency("USDX"), None);
        assert_eq!(normalize_currency("U1D"), None);
        assert_eq!(normalize_currency(""), None);
        assert_eq!(normalize_currency("   "), None);
        assert_eq!(normalize_currency("美元"), None);
    }

    // ---------- 取数口径 ----------

    #[test]
    fn latest_on_or_before_ignores_future_rates() {
        let rows = vec![
            (date(2026, 11, 1), dec("7.10000000")),
            (date(2026, 11, 5), dec("7.20000000")),
            (date(2026, 11, 9), dec("7.30000000")), // 未来汇率
        ];
        assert_eq!(
            latest_on_or_before(&rows, date(2026, 11, 6)),
            Some(dec("7.20000000"))
        );
        // 单据日恰好有牌价 ⇒ 取当天的
        assert_eq!(
            latest_on_or_before(&rows, date(2026, 11, 5)),
            Some(dec("7.20000000"))
        );
        // 早于全部牌价 ⇒ 取不到（4xx，而不是拿最早那条倒推）
        assert_eq!(latest_on_or_before(&rows, date(2026, 10, 31)), None);
    }

    #[test]
    fn latest_on_or_before_handles_empty_and_single() {
        assert_eq!(latest_on_or_before(&[], date(2026, 11, 7)), None);
        let single = vec![(date(2026, 1, 1), dec("1.00000000"))];
        assert_eq!(
            latest_on_or_before(&single, date(2026, 12, 31)),
            Some(Decimal::ONE)
        );
    }

    // ---------- 锁定决策 ----------

    #[test]
    fn decide_defaults_to_base_currency_with_rate_one() {
        let outcome = decide("CNY", None, None, date(2026, 11, 7));
        let locked = outcome
            .into_locked("销售订单")
            .expect("本位币单必定锁定成功");
        assert_eq!(locked.currency_code, "CNY");
        assert_eq!(locked.exchange_rate, Decimal::ONE);
    }

    #[test]
    fn decide_uses_rate_one_for_base_currency_even_if_rate_present() {
        // 账套本位币被切成 USD 时，USD 单同样不必查汇率（rate 恒 1）
        let outcome = decide(
            "USD",
            Some("usd"),
            Some(dec("7.30000000")),
            date(2026, 11, 7),
        );
        let locked = outcome.into_locked("销售订单").expect("本位币单");
        assert_eq!(locked.base_currency, "USD");
        assert_eq!(locked.currency_code, "USD");
        assert_eq!(locked.exchange_rate, Decimal::ONE);
    }

    #[test]
    fn decide_locks_foreign_rate_and_scales_to_eight() {
        let outcome = decide(
            "CNY",
            Some("USD"),
            Some(dec("7.123456789")), // 9 位：落 numeric(18,8) 前必须舍到 8 位
            date(2026, 11, 7),
        );
        let locked = outcome.into_locked("销售订单").expect("外币单有汇率");
        assert_eq!(locked.currency_code, "USD");
        assert_eq!(locked.exchange_rate, dec("7.12345679"));
    }

    #[test]
    fn decide_rejects_foreign_currency_without_rate() {
        let outcome = decide("CNY", Some("EUR"), None, date(2026, 11, 7));
        assert_eq!(
            outcome,
            LockOutcome::MissingRate {
                currency_code: "EUR".to_string(),
                base_currency: "CNY".to_string(),
                on_date: date(2026, 11, 7),
            }
        );
        let error = outcome.into_locked("销售订单").expect_err("必须 4xx");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("EUR"), "文案要带上币种：{}", error.msg);
    }

    #[test]
    fn decide_treats_zero_rate_as_missing() {
        // 库层 CHECK 已拦 rate <= 0，但模型层不许多一条「× 0 = 0 元」的路径
        let outcome = decide("CNY", Some("USD"), Some(Decimal::ZERO), date(2026, 11, 7));
        assert!(matches!(outcome, LockOutcome::MissingRate { .. }));
    }

    // ---------- 换算 ----------

    #[test]
    fn base_amounts_identity_for_rate_one() {
        let base = BaseAmounts::of(dec("100.1234"), dec("13.0160"), Decimal::ONE);
        assert_eq!(base.amount, dec("100.1234"));
        assert_eq!(base.tax, dec("13.0160"));
        assert_eq!(base.with_tax, dec("113.1394"));
        assert!(base.identity_holds());
    }

    #[test]
    fn base_amounts_multiply_and_keep_identity() {
        let base = BaseAmounts::of(dec("100.0000"), dec("13.0000"), dec("7.12345678"));
        assert_eq!(base.amount, dec("712.3457")); // 712.345678 → 4 位五入
        assert_eq!(base.tax, dec("92.6049")); // 92.604938 → 五入
        assert_eq!(base.with_tax, dec("804.9506"));
        assert!(base.identity_holds());
    }

    #[test]
    fn base_amounts_puts_rounding_residual_into_gross() {
        // 逐项五入之和 ≠ 总额五入的经典场景：
        //   amount 0.00005、tax 0.00005、rate 1 ⇒ 逐项都五入成 0.0001
        //   含税必须取两项之和 0.0002（尾差落在含税列），而不是 round(0.0001×1) = 0.0001
        let base = BaseAmounts::of(dec("0.00005"), dec("0.00005"), Decimal::ONE);
        assert_eq!(base.amount, dec("0.0001"));
        assert_eq!(base.tax, dec("0.0001"));
        assert_eq!(base.with_tax, dec("0.0002"));
        assert!(base.identity_holds());

        // 更接近真实业务的尾差：不含税 33.3334 / 税额 4.3333 / 汇率 7.12345678
        let base = BaseAmounts::of(dec("33.3334"), dec("4.3333"), dec("7.12345678"));
        assert_eq!(base.with_tax, base.amount + base.tax);
        assert!(base.identity_holds());
    }

    #[test]
    fn base_amounts_keep_sign_for_red_invoice() {
        // 红字发票（原币三项为负）换算后必须仍是负数，否则 ck_biz_invoice_base_amount 会拒
        let base = BaseAmounts::of(dec("-1000.0000"), dec("-130.0000"), dec("7.20000000"));
        assert_eq!(base.amount, dec("-7200.0000"));
        assert_eq!(base.tax, dec("-936.0000"));
        assert_eq!(base.with_tax, dec("-8136.0000"));
        assert!(base.identity_holds());
    }

    #[test]
    fn scaling_uses_midpoint_away_from_zero_not_bankers() {
        // 银行家舍入会把 0.00025 舍成 0.0002；财务口径必须是 0.0003
        assert_eq!(scale_amount(dec("0.00025")), dec("0.0003"));
        assert_eq!(scale_amount(dec("-0.00025")), dec("-0.0003"));
        assert_eq!(scale_rate(dec("7.123456785")), dec("7.12345679"));
    }
}
