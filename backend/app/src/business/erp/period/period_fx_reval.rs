//! 期末调汇（M1-05）：按**科目余额**生成汇兑损益凭证。
//!
//! # 会计口径（唯一真相，改这里必须同步 V0410 头注释 / 卡内汇报）
//!
//! ```text
//! 期末（会计期间的 end_date）对每个「外币核算」科目（biz_account.is_foreign_currency = 1）
//! 按币种分别算两个余额，再把差额调成损益：
//!
//!   原币余额  fc    = Σ(fc_debit − fc_credit)      —— 只取**外币凭证**的分录
//!   账面本位币 book = Σ(debit − credit)            —— 全部已过账分录（含本位币分录）
//!   期末汇率  rate  = currency::spot_rate(币种, 本位币, 期末日)（最近一条 SPOT，V0407 区间口径）
//!   重估后    reval = round(fc × rate, 4)
//!   差额      diff  = reval − book
//!
//!   diff > 0 ⇒ 借 该科目 diff   （资产类 = 调增；负债类 = 减少负债）
//!   diff < 0 ⇒ 贷 该科目 |diff|
//!   合计 net = Σdiff ≠ 0 ⇒ 对侧 5603 财务费用-汇兑损益（net > 0 贷方收益 / net < 0 借方损失）
//! ```
//!
//! **记账方向一律用「借方为正」的有符号口径**：`debit − credit`。这样资产（借方余额）
//! 与负债（贷方余额）用同一套公式，不需要按 `balance_direction` 分支 ——
//! 分支写法在「科目核算方向被改过」的历史账上会出现「调增写成调减」的镜像错误，
//! 而有符号口径的结论只依赖金额本身，与科目属性无关。
//!
//! # 三个容易写错的地方（每一个都会让账静默错）
//!
//! ## ① 原币余额只能取**外币凭证**的分录
//!
//! `fc_debit_amount` 列是 `NULL` 表示「本位币分录（原币 ≡ 本位币）」（V0409）。
//! 若按 `SUM(COALESCE(fc_debit_amount, debit_amount) − …)` 直接求和，**调汇凭证自己**
//! （本位币凭证、落在外币科目上、金额恰是上一次的调汇差额）会被当成原币金额算进
//! `fc` ⇒ 下一期「原币余额」被自己的调汇额污染，差额越滚越大。
//! 所以取数按**凭证币种**分流：
//! `CASE WHEN v.currency_code <> 本位币 THEN COALESCE(fc_*, e.*) ELSE 0 END`
//! —— 与 M1-04 交接的口径一致（JOIN `biz_voucher` 按 `currency_code` 分组），
//! 同时天然排除所有本位币凭证（含调汇凭证本身）。
//!
//! ## ② 账面余额必须**包含**本位币分录（含历次调汇）
//!
//! `book` 取全部已过账分录。这不只是「顺便」：上期调汇把账面调到了上期期末汇率，
//! 本期差额 = 本期重估 − 含上期调汇的账面，才能算出「汇率从 7.1 走到 7.9」这一段。
//! 若把调汇凭证排除掉，每期都会把历史累计的差额再提一遍（账越提越偏）。
//!
//! **一个科目一个账面**，不是「一个币种一个账面」：本位币分录（含调汇凭证自己）没有币种归属，
//! 按币种切分会把它们整段丢掉。所以取数用窗口函数同时算「科目合计账面」与「分币种账面」：
//! 单币种科目用合计口径（正常情况），同一科目被记了多个外币币种时退到「该币种自身账面」并
//! 在 `warnings` 里点名（治理办法是把不同币种拆到不同科目，而不是在这里猜分摊比例）。
//!
//! ## ③ 科目声明币种（`biz_account.currency_code`）**不参与计算**
//!
//! 一个科目可能在不同凭证上出现不同币种（历史数据 / 人工改过币种）。取数以**分录所在
//! 凭证的币种**为准（那是账实），按 `(科目, 币种)` 分组各出一行；科目声明币种与凭证币种
//! 不一致时只在响应的 `warnings` 里点名（不拦、也不改金额）—— 拦下来会让「先建科目、
//! 后补外币凭证」的正常路径变成 400，而金额本身并不因此算错。
//!
//! # 幂等与重跑
//!
//! 同一期间只允许一张蓝字调汇凭证（`source_type = 'FX_REVAL_ACCOUNT'`、`source_no = 期间码`）。
//!
//! * 已存在**未被红冲**的调汇凭证（`reversed_by IS NULL`）⇒ `skipped = true`，不重复计提；
//! * 已被红冲（`reversed_by` 非空）⇒ 允许重跑：红字凭证已经把账面还原，重算出来的差额
//!   就是当前应提的金额（**不是**「先删凭证再重算」那种要改库的做法）；
//! * 并发：调用方在期间行 `FOR UPDATE` 的事务里跑（见 `period::fx_reval`），
//!   两次点击被行锁串行化，第二次看得见第一张凭证 ⇒ 命中上面的 `skipped`。
//!
//! # 与 SO-41 应收逐笔重估（`fx_revaluation.rs`）的关系
//!
//! 两条路径**并存且互补**，不是二选一：
//!
//! | 路径 | 取数 | 凭证 | 幂等键 |
//! |------|------|------|--------|
//! | SO-41 应收逐笔重估 | `biz_receivable` 未核销原币 | 借 1122 / 贷 5603 | `FX_REVALUATION` + 期间码 |
//! | M1-05 科目余额调汇（本文件） | 全部外币科目的分录余额 | 按科目逐行 + 一笔 5603 | `FX_REVAL_ACCOUNT` + 期间码 |
//!
//! 结账时 SO-41 先跑（改 1122 的账面），本模块若随后在同一期间跑，1122 的差额已趋近 0
//! （只有逐笔五入的尾差），不会重复计提 —— 这是口径对齐的结果，不是特判。
//! 本模块**不挂在结账流程里**：调汇是财务要复核、可能要按币种排除的动作，
//! 且结账链路不允许因为「某个币种没维护牌价」而封不了账；入口是独立接口
//! `POST /business/erp/period/{id}/fx-reval`（可 `dry_run` 先试算）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_account / biz_voucher / biz_voucher_entry / biz_exchange_rate 是
//! 全局会计字典与台账（无 dept_id / 归属人列），数据范围表达不出来；本模块只被
//! 期间级接口在事务内调用，不提供列表接口。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::currency;
use crate::business::erp::voucher::{self, AccountRef, EntryLine, PostVoucher};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

/// 凭证来源类型：期末调汇（科目余额口径）。
///
/// `biz_voucher.source_type` 是 `VARCHAR(24)` 且无白名单 CHECK（与 `FX_REVALUATION`
/// 同一个理由），新增取值不需要改结构；但**必须与 SO-41 的 `FX_REVALUATION` 不同**：
/// 两者是同一期间里两张独立的凭证，撞了就互相把对方当「已提过」而静默少提。
pub const SOURCE_FX_REVAL_ACCOUNT: &str = "FX_REVAL_ACCOUNT";

/// 只统计已过账凭证（与 `ledger` 的口径逐字一致：`status = 'POSTED'` + 未软删）。
const POSTED: &str = "POSTED";

/// 调汇最小金额：0.01 元（1 分）。
///
/// 单科目差额 < 1 分不生成分录。理由：
/// * 4 位小数的余额里，1 分以下的差额来自逐行五入，调它只会让凭证多出几十行噪音；
/// * 残差**不会累积放大**：下期 `diff = 重估 − 账面`，残差已经包含在账面里，
///   所以每期最多残留 1 分，不会滚雪球。
pub fn tolerance() -> Decimal {
    Decimal::new(1, 2)
}

// ==================== 纯函数（不碰库，可直接单测） ====================

/// 一条待调汇的科目余额（读模型：一个 `(科目, 币种)` 一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FxAccountBalance {
    /// 科目 id。
    pub account_id: Uuid,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目主数据声明的核算币种（只用于比对告警，不参与计算）。
    pub account_currency: String,
    /// 分录所在凭证的原币币种（真实的核算币种）。
    pub currency_code: String,
    /// 原币余额（借方为正）。
    pub fc_balance: Decimal,
    /// **该币种**分录的账面本位币余额（借方为正）。
    pub currency_book: Decimal,
    /// **该科目全部币种**的账面本位币余额（借方为正，含历次调汇与本位币分录）。
    pub account_book: Decimal,
    /// 该科目名下的外币币种个数（1 = 单币种科目，可用 `account_book` 做账面）。
    pub fx_currency_count: i64,
}

impl FxAccountBalance {
    /// 调汇用的账面本位币金额（口径见模块文档 ②）。
    ///
    /// * 单币种科目（正常情况）⇒ 用**科目合计**：本位币分录（含上期调汇凭证）必须一起算进去，
    ///   否则每期都会把历史累计差额再提一遍；
    /// * 多币种科目（同一科目被记了 USD 与 EUR）⇒ 只能退到**该币种自身**的账面：
    ///   本位币分录没有币种归属，无法按币种分摊。这条退化路径会在响应 `warnings` 里点名，
    ///   正确的治理是把不同币种拆到不同科目。
    pub fn book_balance(&self) -> Decimal {
        if self.fx_currency_count > 1 {
            self.currency_book
        } else {
            self.account_book
        }
    }

    /// 是否走了「多币种科目」的退化口径（调用方据此吐 warning）。
    pub fn is_multi_currency(&self) -> bool {
        self.fx_currency_count > 1
    }
}

/// 一条调汇结论（一个 `(科目, 币种)` 一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevalRow {
    /// 科目 id。
    pub account_id: Uuid,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目主数据声明的核算币种。
    pub account_currency: String,
    /// 凭证原币币种。
    pub currency_code: String,
    /// 原币余额。
    pub fc_balance: Decimal,
    /// 账面本位币余额。
    pub book_balance: Decimal,
    /// 期末汇率。
    pub close_rate: Decimal,
    /// 重估后本位币金额。
    pub revalued_base: Decimal,
    /// 差额（正 = 借方调整）。
    pub diff: Decimal,
}

impl RevalRow {
    /// 账面汇率（给人看的派生列）：`book / fc`；原币为 0 时退回期末汇率。
    pub fn book_rate(&self) -> Decimal {
        if self.fc_balance.is_zero() {
            return self.close_rate;
        }
        currency::scale_rate(self.book_balance / self.fc_balance)
    }
}

/// 重估后的本位币金额：`round(原币 × 期末汇率, 4)`（五入，与全仓金额定标同口径）。
pub fn revalued_base_of(fc_balance: Decimal, close_rate: Decimal) -> Decimal {
    currency::scale_amount(fc_balance.round_dp(4) * close_rate)
}

/// 调汇差额：`重估后 − 账面`（正 = 借方调整，负 = 贷方调整），4 位定标。
pub fn diff_of(fc_balance: Decimal, close_rate: Decimal, book_balance: Decimal) -> Decimal {
    (revalued_base_of(fc_balance, close_rate) - book_balance.round_dp(4)).round_dp(4)
}

/// 一次调汇的完整结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// 期间码 `yyyy-MM`。
    pub period_code: String,
    /// 期末日（取汇率日 + 凭证日期）。
    pub period_end: NaiveDate,
    /// 账套本位币。
    pub base_currency: String,
    /// 逐 `(科目, 币种)` 结论（差额为 0 的不进来）。
    pub rows: Vec<RevalRow>,
    /// 差额合计（正 = 净收益）。
    pub net: Decimal,
    /// 生成的凭证 id（差额合计为 0 / 试算 / 已提过 ⇒ `None`）。
    pub voucher_id: Option<Uuid>,
    /// 生成的凭证号。
    pub voucher_no: Option<String>,
    /// 该期间之前已经提过 ⇒ `true`（本次什么都没写）。
    pub skipped: bool,
    /// 科目声明币种与凭证币种不一致等**不拦账**的提示。
    pub warnings: Vec<String>,
}

/// 拼调汇凭证的分录（纯函数，便于直接断言「借贷平衡 / 行序 / 方向」）。
///
/// * 每个 `(科目, 币种)` 一行，落**差额的绝对值**在差额那一侧；
/// * `net ≠ 0` 时追加最后一行 5603 财务费用-汇兑损益（net > 0 贷方收益 / net < 0 借方损失）；
/// * `net = 0` 时不加 5603（各科目正负相抵，凭证本身就平了，多一行 0 金额分录会违反
///   `ck_biz_voucher_entry_side` 的「一边有金额、另一边为 0」）。
pub fn build_entries(period_code: &str, rows: &[RevalRow], net: Decimal, fx_account: &AccountRef) -> Vec<EntryLine> {
    let mut entries: Vec<EntryLine> = Vec::with_capacity(rows.len() + 1);
    for row in rows {
        let debit = if row.diff > Decimal::ZERO {
            row.diff.abs()
        } else {
            Decimal::ZERO
        };
        let credit = if row.diff < Decimal::ZERO {
            row.diff.abs()
        } else {
            Decimal::ZERO
        };
        entries.push(EntryLine {
            line_no: i32::try_from(entries.len() + 1).unwrap_or(1),
            summary: format!(
                "{period_code} 期末调汇：{} {}（原币 {} {:.2} @ {:.8}）",
                row.account_code, row.account_name, row.currency_code, row.fc_balance, row.close_rate
            ),
            account: AccountRef {
                id: row.account_id,
                code: row.account_code.clone(),
                name: row.account_name.clone(),
            },
            debit,
            credit,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    if !net.is_zero() {
        let (debit, credit) = if net > Decimal::ZERO {
            (Decimal::ZERO, net)
        } else {
            (net.abs(), Decimal::ZERO)
        };
        entries.push(EntryLine {
            line_no: i32::try_from(entries.len() + 1).unwrap_or(1),
            summary: format!(
                "{period_code} 期末调汇：汇兑{}",
                if net > Decimal::ZERO { "收益" } else { "损失" }
            ),
            account: fx_account.clone(),
            debit,
            credit,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    entries
}

// ==================== 库访问 ====================

/// 取该期间期末**全部外币核算科目**的双口径余额（按 `(科目, 币种)` 分组）。
///
/// 取数口径见模块文档 ①②③：原币只取外币凭证、账面取全部已过账分录、按凭证币种分组。
/// 只回 `fc_balance <> 0` 的行 —— 原币余额为 0 的科目没有可调金额（账面若因历史
/// 本位币分录残留差额，会由该科目下一次有原币余额时一并调回；强行在 0 原币上调
/// 等于把差额挂到一个没有外币业务往来的科目上）。
pub async fn balances(
    ex: &impl SqlExecutor,
    period_end: NaiveDate,
    base_currency: &str,
) -> DbResult<Vec<FxAccountBalance>> {
    // SCOPE-EXEMPT: 科目 / 凭证 / 分录都是全局会计台账（见模块文档「数据权限」）。
    //
    // 窗口函数 `SUM(...) OVER (PARTITION BY a.id)` 把「科目合计账面」和「分币种账面」一次取回：
    // 单币种科目用前者（本位币分录必须算进去），多币种科目退到后者（见 `book_balance()`）。
    let rows = ex
        .query_sql(
            "SELECT a.id AS account_id, a.account_code, a.account_name, \
                    a.currency_code AS account_currency, v.currency_code AS voucher_currency, \
                    (SUM(CASE WHEN v.currency_code <> ? THEN COALESCE(e.fc_debit_amount, e.debit_amount) \
                              ELSE 0 END) \
                   - SUM(CASE WHEN v.currency_code <> ? THEN COALESCE(e.fc_credit_amount, e.credit_amount) \
                              ELSE 0 END))::numeric(20,4) AS fc_balance, \
                    (SUM(e.debit_amount) - SUM(e.credit_amount))::numeric(20,4) AS currency_book, \
                    SUM(SUM(e.debit_amount) - SUM(e.credit_amount)) \
                        OVER (PARTITION BY a.id)::numeric(20,4) AS account_book, \
                    COUNT(DISTINCT v.currency_code) FILTER (WHERE v.currency_code <> ?) AS fx_currency_count \
               FROM biz_voucher_entry e \
               JOIN biz_voucher v ON v.id = e.voucher_id \
               JOIN biz_account a ON a.id = e.account_id \
              WHERE a.is_foreign_currency = 1 \
                AND a.deleted_at IS NULL \
                AND v.status = ? \
                AND v.deleted_at IS NULL \
                AND v.voucher_date <= ?::text::date \
              GROUP BY a.id, a.account_code, a.account_name, a.currency_code, v.currency_code \
              ORDER BY a.account_code, v.currency_code"
                .to_string(),
            vec![
                Value::Text(base_currency.to_string()),
                Value::Text(base_currency.to_string()),
                Value::Text(base_currency.to_string()),
                Value::Text(POSTED.to_string()),
                Value::Text(period_end.to_string()),
            ],
        )
        .await?;
    let mut out: Vec<FxAccountBalance> = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let balance = FxAccountBalance {
            account_id: row.get_typed("account_id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            account_currency: row.get_typed("account_currency")?,
            currency_code: row.get_typed("voucher_currency")?,
            fc_balance: row.get_typed("fc_balance")?,
            currency_book: row.get_typed("currency_book")?,
            account_book: row.get_typed("account_book")?,
            fx_currency_count: row.get_typed("fx_currency_count")?,
        };
        if balance.fc_balance.is_zero() {
            continue;
        }
        out.push(balance);
    }
    Ok(out)
}

/// 该期间已提过、且**未被红冲**的调汇凭证（幂等闸）。
async fn existing_voucher(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<(Uuid, String)>> {
    let row = ex
        .query_first_sql(
            "SELECT id, voucher_no FROM biz_voucher \
              WHERE source_type = ? AND source_no = ? \
                AND is_red = 0 AND reversed_by IS NULL AND deleted_at IS NULL \
              ORDER BY created_at ASC LIMIT 1"
                .to_string(),
            vec![
                Value::Text(SOURCE_FX_REVAL_ACCOUNT.to_string()),
                Value::Text(period_code.to_string()),
            ],
        )
        .await?;
    match row {
        Some(row) => Ok(Some((row.get_typed("id")?, row.get_typed("voucher_no")?))),
        None => Ok(None),
    }
}

/// 期末调汇主流程。
///
/// # 参数
///
/// * `period_code` / `period_end` —— 由调用方从 `biz_accounting_period` 读出（**不要**自己按
///   `yyyy-MM` 推月末：期间表可以有非自然月的边界，"期末"的唯一定义是 `end_date`）；
/// * `dry_run` —— 只算不落账（试算）。返回同样的逐行结论，`voucher_id = None`。
///
/// # 错误
///
/// * 币种在期末日（含）之前没有 SPOT 牌价 ⇒ `400 + 66082`，文案点名币种与日期
///   （**不**默认 rate = 1：拿 1 调汇等于把外币余额按人民币重估，账直接错）；
/// * 5603 财务费用-汇兑损益科目缺失 ⇒ `400`（`voucher::account_required` 的既有文案）。
///
/// 期间状态（已结账 / 未开账）由调用方在事务内先判，这里不再重复查。
pub async fn revalue(
    ex: &impl SqlExecutor,
    period_code: &str,
    period_end: NaiveDate,
    dry_run: bool,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Outcome, ApiError> {
    let base_currency = currency::base_currency_code(ex)
        .await
        .map_err(|error| utils::db_error(error, "查询账套本位币"))?;

    // ① 幂等闸：该期间已提过（含「上次差额合计恰好为 0、没生成凭证」的情况不跳过 ——
    //    那种情况没有凭证，说明当时无可调金额，本次重新算即可）。
    if let Some((voucher_id, voucher_no)) = existing_voucher(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询本期调汇凭证"))?
    {
        tracing::info!(
            period = period_code,
            voucher = %voucher_id,
            "期末调汇：该期间已提过，跳过（幂等）"
        );
        return Ok(Outcome {
            period_code: period_code.to_string(),
            period_end,
            base_currency,
            rows: Vec::new(),
            net: Decimal::ZERO,
            voucher_id: Some(voucher_id),
            voucher_no: Some(voucher_no),
            skipped: true,
            warnings: Vec::new(),
        });
    }

    // ② 逐 `(科目, 币种)` 算差额（本位币凭证已在 SQL 里被排除出原币口径）
    let balances = balances(ex, period_end, &base_currency)
        .await
        .map_err(|error| utils::db_error(error, "查询外币科目余额"))?;
    let mut rates: Vec<(String, Decimal)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut rows: Vec<RevalRow> = Vec::new();
    for balance in balances {
        let close_rate = match rates.iter().find(|(code, _)| code == &balance.currency_code) {
            Some((_, rate)) => *rate,
            None => {
                let rate = currency::spot_rate(ex, &balance.currency_code, &base_currency, period_end)
                    .await
                    .map_err(|error| utils::db_error(error, "查询期末汇率"))?
                    .ok_or_else(|| {
                        ApiError::new(
                            400,
                            error_code::ERP_FX_REVAL_RATE_MISSING,
                            format!(
                                "期末调汇失败：币种 {} 在 {period_end}（含）之前没有可用的 SPOT 汇率\
                                 （本位币 {base_currency}），{period_code} 无法对科目 {} {} 调汇；\
                                 请先在「币种 / 汇率」里维护该币种截至期末的即时汇率",
                                balance.currency_code, balance.account_code, balance.account_name
                            ),
                        )
                    })?;
                if rate <= Decimal::ZERO {
                    return Err(ApiError::new(
                        400,
                        error_code::ERP_FX_REVAL_RATE_MISSING,
                        format!(
                            "期末调汇失败：币种 {} 在 {period_end} 的汇率是 {rate}（必须 > 0），\
                             {period_code} 无法对科目 {} 调汇",
                            balance.currency_code, balance.account_code
                        ),
                    ));
                }
                let rate = currency::scale_rate(rate);
                rates.push((balance.currency_code.clone(), rate));
                rate
            }
        };
        // ③ 科目声明币种与凭证币种不一致 ⇒ 只记 warning（见模块文档 ③）
        let declared = balance.account_currency.trim().to_ascii_uppercase();
        if declared != balance.currency_code && declared != base_currency {
            warnings.push(format!(
                "科目 {} {}（{}）声明核算币种是 {}，但本期有 {} 的分录：\
                 调汇按分录所在凭证的币种（{}）计算，请核对科目主数据是否该改成 {}",
                balance.account_code,
                balance.account_name,
                balance.account_id,
                declared,
                balance.currency_code,
                balance.currency_code,
                balance.currency_code
            ));
        }
        // ④ 同一科目挂了多个外币币种 ⇒ 本位币分录无法按币种分摊，退到「该币种自身账面」
        if balance.is_multi_currency() {
            warnings.push(format!(
                "科目 {} {}（{}）名下同时有多个外币币种的分录：本位币分录（含上期调汇）没有币种归属，\
                 本次按『该币种自身账面』重估 {} —— 建议把不同币种拆到不同科目，\
                 否则本位币口径调整无法按币种追溯",
                balance.account_code, balance.account_name, balance.account_id, balance.currency_code
            ));
        }
        let book_balance = balance.book_balance();
        let revalued_base = revalued_base_of(balance.fc_balance, close_rate);
        let diff = diff_of(balance.fc_balance, close_rate, book_balance);
        if diff.abs() < tolerance() {
            tracing::debug!(
                period = period_code,
                account = %balance.account_code,
                currency = %balance.currency_code,
                "期末调汇：差额不足 1 分（账面已按期末汇率计价），不出调整行"
            );
            continue;
        }
        rows.push(RevalRow {
            account_id: balance.account_id,
            account_code: balance.account_code,
            account_name: balance.account_name,
            account_currency: balance.account_currency,
            currency_code: balance.currency_code,
            fc_balance: balance.fc_balance,
            book_balance,
            close_rate,
            revalued_base,
            diff,
        });
    }
    let net = rows.iter().map(|row| row.diff).sum::<Decimal>().round_dp(4);

    // ④ 试算 / 无可调金额：不落账
    if dry_run || rows.is_empty() {
        if rows.is_empty() {
            tracing::debug!(period = period_code, "期末调汇：本期没有需要调汇的外币科目余额");
        }
        return Ok(Outcome {
            period_code: period_code.to_string(),
            period_end,
            base_currency,
            rows,
            net,
            voucher_id: None,
            voucher_no: None,
            skipped: false,
            warnings,
        });
    }

    // ⑤ 生成调汇凭证：**本位币凭证**（currency = 本位币、rate = 1、分录不带原币）
    let fx_account = voucher::account_required(
        ex,
        voucher::ACCOUNT_FX_GAIN_LOSS,
        "期末调汇凭证的 5603 财务费用-汇兑损益",
    )
    .await
    .map_err(|error| utils::db_error(error, "查询汇兑损益科目 5603"))?;
    let entries = build_entries(period_code, &rows, net, &fx_account);
    let label = format!(
        "{period_code} 期末调汇（{} 个外币科目 / 币种 {} 个，期末汇率取 {period_end}）",
        rows.len(),
        rates.len()
    );
    let voucher_id = voucher::post_voucher_fx(
        ex,
        &PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: SOURCE_FX_REVAL_ACCOUNT,
            source_id: None,
            source_no: Some(period_code),
            source_doc_no: None,
            voucher_date: period_end,
            summary: label.clone(),
            lines: entries,
            remark: Some("M1-05 期末调汇：按外币科目余额 × 期末 SPOT 汇率重估，差额计当期损益"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        None,
        // 调汇凭证本身记本位币：币种 = 账套本位币、汇率恒 1、分录无原币列
        Some((base_currency.as_str(), Decimal::ONE)),
        operator,
        now,
    )
    .await
    .map_err(|error| utils::db_error(error, "写入期末调汇凭证"))?;

    let voucher_no = ex
        .query_first_sql(
            "SELECT voucher_no FROM biz_voucher WHERE id = ?::uuid".to_string(),
            vec![Value::from(&voucher_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "回读调汇凭证号"))?
        .map(|row| row.get_typed::<String>("voucher_no"))
        .transpose()
        .map_err(|error| utils::db_error(error, "读取调汇凭证号"))?;

    tracing::info!(
        period = period_code,
        voucher = %voucher_id,
        net = %net,
        rows = rows.len(),
        "期末调汇：已生成调汇凭证"
    );
    Ok(Outcome {
        period_code: period_code.to_string(),
        period_end,
        base_currency,
        rows,
        net,
        voucher_id: Some(voucher_id),
        voucher_no,
        skipped: false,
        warnings,
    })
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse::<Decimal>().expect("定点数可解析")
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("固定日期合法")
    }

    fn rows_of(items: &[(Uuid, &str, Decimal, Decimal, Decimal, Decimal)]) -> Vec<RevalRow> {
        items
            .iter()
            .map(|(id, code, fc, book, rate, diff)| RevalRow {
                account_id: *id,
                account_code: (*code).to_string(),
                account_name: format!("{code} 名称"),
                account_currency: "CNY".to_string(),
                currency_code: "USD".to_string(),
                fc_balance: *fc,
                book_balance: *book,
                close_rate: *rate,
                revalued_base: revalued_base_of(*fc, *rate),
                diff: *diff,
            })
            .collect()
    }

    fn fx_account() -> AccountRef {
        AccountRef {
            id: Uuid::nil(),
            code: voucher::ACCOUNT_FX_GAIN_LOSS.to_string(),
            name: "财务费用-汇兑损益".to_string(),
        }
    }

    // ---------- 纯函数 ----------

    #[test]
    fn diff_sign_follows_rate_movement() {
        // 100 USD 账面 710（历史 7.1），期末 7.9 ⇒ +80（借 该科目）
        assert_eq!(diff_of(dec("100"), dec("7.9"), dec("710")), dec("80"));
        // 期末 6.5 ⇒ -60（贷 该科目）
        assert_eq!(diff_of(dec("100"), dec("6.5"), dec("710")), dec("-60"));
        // 汇率没动 ⇒ 0（不出调整行）
        assert_eq!(diff_of(dec("100"), dec("7.1"), dec("710")), Decimal::ZERO);
        // 贷方余额（负债）同口径：-100 USD 账面 -710，期末 6.5 ⇒ -650 重估，diff = +60
        // （负债缩水 ⇒ 借 负债 +60，与有符号口径一致）
        assert_eq!(diff_of(dec("-100"), dec("6.5"), dec("-710")), dec("60"));
    }

    #[test]
    fn revalued_base_scales_to_four_decimals_midpoint_away_from_zero() {
        // 333.3333 × 7.12345678 = 2374.48535… ⇒ 五入到 4 位
        assert_eq!(revalued_base_of(dec("333.3333"), dec("7.12345678")), dec("2374.4854"));
        // 0.0005 的半分位必须进位（银行家舍入会舍掉）
        assert_eq!(revalued_base_of(dec("1"), dec("0.0005")), dec("0.0005"));
        assert_eq!(revalued_base_of(dec("1"), dec("0.00025")), dec("0.0003"));
    }

    #[test]
    fn tolerance_is_one_cent_and_filters_rounding_noise() {
        assert_eq!(tolerance(), dec("0.01"));
        // 差额 0.004（逐行五入残差）⇒ 低于容差，不出行
        assert!(dec("0.004") < tolerance());
        // 差额 0.01 正好到容差 ⇒ 要调（不小于容差）
        assert!(dec("0.01") >= tolerance());
    }

    #[test]
    fn book_rate_is_derived_from_book_balance() {
        let row = &rows_of(&[(Uuid::nil(), "1122", dec("100"), dec("710"), dec("7.9"), dec("80"))])[0];
        assert_eq!(row.book_rate(), dec("7.10000000"));
    }

    #[test]
    fn book_rate_falls_back_to_close_rate_without_foreign_balance() {
        let row = &rows_of(&[(Uuid::nil(), "1122", Decimal::ZERO, dec("5"), dec("7.9"), dec("0"))])[0];
        assert_eq!(row.book_rate(), dec("7.9"));
    }

    #[test]
    fn entries_put_each_account_on_its_own_side_and_balance_through_5603() {
        let rows = rows_of(&[
            (Uuid::nil(), "1122", dec("100"), dec("710"), dec("7.9"), dec("80")),
            (Uuid::nil(), "2202", dec("-50"), dec("-360"), dec("7.9"), dec("-35")),
        ]);
        let net = dec("45");
        let entries = build_entries("2051-01", &rows, net, &fx_account());
        assert_eq!(entries.len(), 3, "两个科目 + 一笔汇兑损益");
        assert_eq!(entries[0].debit, dec("80"));
        assert_eq!(entries[0].credit, Decimal::ZERO);
        assert_eq!(entries[1].debit, Decimal::ZERO);
        assert_eq!(entries[1].credit, dec("35"));
        // 净收益 45 ⇒ 贷 5603
        assert_eq!(entries[2].account.code, voucher::ACCOUNT_FX_GAIN_LOSS);
        assert_eq!(entries[2].debit, Decimal::ZERO);
        assert_eq!(entries[2].credit, dec("45"));
        let debit: Decimal = entries.iter().map(|line| line.debit).sum();
        let credit: Decimal = entries.iter().map(|line| line.credit).sum();
        assert_eq!(debit, credit, "凭证必须借贷平衡：借 {debit} / 贷 {credit}");
        // 行号连续从 1 起（分录表 uk_biz_voucher_entry_line 兜底）
        for (index, line) in entries.iter().enumerate() {
            assert_eq!(line.line_no, i32::try_from(index + 1).unwrap());
        }
    }

    #[test]
    fn entries_skip_5603_when_net_is_zero() {
        // 一增一减正好抵平 ⇒ 不出 5603 行（0 金额分录会违反 ck_biz_voucher_entry_side）
        let rows = rows_of(&[
            (Uuid::nil(), "1122", dec("100"), dec("710"), dec("7.9"), dec("80")),
            (Uuid::nil(), "2121", dec("-100"), dec("-710"), dec("7.9"), dec("-80")),
        ]);
        let entries = build_entries("2051-01", &rows, Decimal::ZERO, &fx_account());
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .all(|line| line.account.code != voucher::ACCOUNT_FX_GAIN_LOSS)
        );
        let debit: Decimal = entries.iter().map(|line| line.debit).sum();
        let credit: Decimal = entries.iter().map(|line| line.credit).sum();
        assert_eq!(debit, credit);
    }

    #[test]
    fn entries_put_net_loss_on_debit_side_of_5603() {
        let rows = rows_of(&[(Uuid::nil(), "1122", dec("100"), dec("710"), dec("6.5"), dec("-60"))]);
        let entries = build_entries("2051-01", &rows, dec("-60"), &fx_account());
        assert_eq!(entries[0].credit, dec("60"), "差额为负 ⇒ 贷 科目");
        assert_eq!(entries[1].debit, dec("60"), "净损失 ⇒ 借 5603");
        assert!(entries[1].summary.contains("汇兑损失"), "{}", entries[1].summary);
    }

    // ---------- 真库（ERP_DB_URL 未设则 SKIP） ----------

    // 期间命名空间 **2051-xx 归本模块独占**（2050 = fx_revaluation 应收重估、
    // 2031 = period::tests、2033-2040 = 其它期间类用例；2026 是生产期间，绝不碰）。
    const PERIOD: &str = "2051-01";
    /// 资产类外币科目（借方余额）。
    const ACC_ASSET: &str = "M105FX01";
    /// 对方外币科目（贷方余额，模拟「另一侧的科目」）。
    const ACC_COUNTER: &str = "M105FX02";
    /// 本位币科目（用来给外币科目灌一笔纯本位币分录，制造 net ≠ 0）。
    const ACC_BASE: &str = "M105BASE";

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = crate::business::erp::voucher::voucher_query::tests::DB_LOCK
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
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn cleanup() {
        for sql in [
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'M105%' OR source_no = '2051-01')",
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'M105%' OR source_no = '2051-01')",
            "DELETE FROM biz_voucher WHERE source_no LIKE 'M105%' OR source_no = '2051-01'",
            // 汇率表在 V0407 之后带区间排除约束：模板库里已有 [2026-11-15, ∞) 的 USD 行，
            // 2051 年的行会与它重叠 ⇒ 必须整对清掉（与 fx_revaluation 的 cleanup 同口径）。
            "DELETE FROM biz_exchange_rate WHERE from_currency IN ('USD', 'EUR')",
            "DELETE FROM biz_account WHERE account_code LIKE 'M105%'",
            "DELETE FROM biz_accounting_period WHERE period_code LIKE '2051-%'",
        ] {
            let _ = db().exec_sql(sql.to_string(), vec![]).await;
        }
    }

    /// 种三个科目 + 开放期间 2051-01。
    async fn seed_accounts() -> (AccountRef, AccountRef, AccountRef) {
        let mut accounts = Vec::new();
        for (code, name) in [
            (ACC_ASSET, "M105 外币资产"),
            (ACC_COUNTER, "M105 外币对方"),
            (ACC_BASE, "M105 本位币"),
        ] {
            let is_fx = if code == ACC_BASE { 0 } else { 1 };
            db().exec_sql(
                "INSERT INTO biz_account (account_code, account_name, level, account_category, \
                                         balance_direction, is_leaf, is_foreign_currency) \
                 VALUES (?, ?, 1, 'ASSET', 'DEBIT', 1, ?) \
                 ON CONFLICT (account_code) DO UPDATE SET is_leaf = 1, is_foreign_currency = EXCLUDED.is_foreign_currency"
                    .to_string(),
                vec![
                    Value::Text(code.to_string()),
                    Value::Text(name.to_string()),
                    Value::I16(is_fx),
                ],
            )
            .await
            .expect("种科目");
            let row = db()
                .query_first_sql(
                    "SELECT id FROM biz_account WHERE account_code = ?".to_string(),
                    vec![Value::Text(code.to_string())],
                )
                .await
                .expect("读科目")
                .expect("科目必须存在");
            accounts.push(AccountRef {
                id: row.get_typed("id").expect("科目 id"),
                code: code.to_string(),
                name: name.to_string(),
            });
        }
        db().exec_sql(
            format!(
                "INSERT INTO biz_accounting_period \
                    (period_year, period_month, period_code, start_date, end_date, status) \
                 VALUES (2051, 1, '{PERIOD}', '2051-01-01', '2051-01-31', 'OPEN') \
                 ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN', closed_at = NULL, closed_by = NULL"
            ),
            vec![],
        )
        .await
        .expect("种期间");
        (accounts[0].clone(), accounts[1].clone(), accounts[2].clone())
    }

    async fn seed_rate(on_date: &str, rate: &str) {
        db().exec_sql(
            "INSERT INTO biz_exchange_rate (from_currency, to_currency, rate_type, effective_date, rate) \
             VALUES ('USD', 'CNY', 'SPOT', ?::text::date, ?::text::numeric) \
             ON CONFLICT (from_currency, to_currency, rate_type, effective_date) \
             DO UPDATE SET rate = EXCLUDED.rate"
                .to_string(),
            vec![Value::Text(on_date.to_string()), Value::Text(rate.to_string())],
        )
        .await
        .expect("种汇率");
    }

    /// 一张外币凭证（两行，本位币金额由调用方给定、借贷相等）。
    async fn post_fx_voucher(
        tag: &str,
        date_text: &str,
        rate: &str,
        debit_account: &AccountRef,
        credit_account: &AccountRef,
        fc_amount: &str,
        base_amount: &str,
    ) {
        let entries = vec![
            EntryLine {
                line_no: 1,
                summary: format!("M105 造数 {tag} 借"),
                account: debit_account.clone(),
                debit: dec(base_amount),
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
            EntryLine {
                line_no: 2,
                summary: format!("M105 造数 {tag} 贷"),
                account: credit_account.clone(),
                debit: Decimal::ZERO,
                credit: dec(base_amount),
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
        ];
        let fc = vec![
            voucher::FcAmount {
                debit: dec(fc_amount),
                credit: Decimal::ZERO,
            },
            voucher::FcAmount {
                debit: Decimal::ZERO,
                credit: dec(fc_amount),
            },
        ];
        let date = NaiveDate::parse_from_str(date_text, "%Y-%m-%d").expect("日期合法");
        voucher::post_voucher_fx(
            db(),
            &PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_AUTO,
                source_type: "M105",
                source_id: None,
                source_no: Some(tag),
                source_doc_no: None,
                voucher_date: date,
                summary: format!("M105 造数凭证 {tag}"),
                lines: entries,
                remark: Some("M1-05 真库用例夹具"),
                red_of: None,
                status: voucher::STATUS_POSTED,
            },
            Some(&fc),
            Some(("USD", rate.parse::<Decimal>().expect("汇率"))),
            None,
            Utc::now(),
        )
        .await
        .expect("外币凭证落库");
    }

    /// 真库：期末外币科目按余额调汇 ⇒ 一张本币凭证（科目行 + 5603），再跑一次被幂等闸跳过。
    #[tokio::test]
    async fn revalue_posts_account_rows_and_is_idempotent() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (asset, counter, base_acc) = seed_accounts().await;
        // 100 USD @ 7.1 记进 资产 / 对方（两行外币）
        post_fx_voucher("M105-A1", "2051-01-10", "7.1", &asset, &counter, "100", "710").await;
        // 给对方科目再灌一笔纯本位币分录：只改账面、不改原币 ⇒ net ≠ 0
        let entries = vec![
            EntryLine {
                line_no: 1,
                summary: "M105 本位币调整 借".to_string(),
                account: counter.clone(),
                debit: dec("50"),
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
            EntryLine {
                line_no: 2,
                summary: "M105 本位币调整 贷".to_string(),
                account: base_acc.clone(),
                debit: Decimal::ZERO,
                credit: dec("50"),
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
        ];
        voucher::post_voucher_fx(
            db(),
            &PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_AUTO,
                source_type: "M105",
                source_id: None,
                source_no: Some("M105-A2"),
                source_doc_no: None,
                voucher_date: date(2051, 1, 20),
                summary: "M105 本位币调整".to_string(),
                lines: entries,
                remark: None,
                red_of: None,
                status: voucher::STATUS_POSTED,
            },
            None,
            Some(("CNY", Decimal::ONE)),
            None,
            Utc::now(),
        )
        .await
        .expect("本位币凭证落库");
        seed_rate("2051-01-31", "7.9").await;

        let outcome = revalue(db(), PERIOD, date(2051, 1, 31), false, None, Utc::now())
            .await
            .expect("调汇应成功");
        assert!(!outcome.skipped);
        assert_eq!(outcome.base_currency, "CNY");
        assert_eq!(outcome.rows.len(), 2, "两个外币科目各一行：{:?}", outcome.rows);
        // 资产：原币 +100、账面 710 ⇒ 790 − 710 = +80
        let asset_row = outcome
            .rows
            .iter()
            .find(|row| row.account_code == ACC_ASSET)
            .expect("资产科目行");
        assert_eq!(asset_row.fc_balance, dec("100"));
        assert_eq!(asset_row.book_balance, dec("710"));
        assert_eq!(asset_row.revalued_base, dec("790"));
        assert_eq!(asset_row.diff, dec("80"));
        // 对方：原币 -100、账面 -660（-710 贷 + 50 本位币借） ⇒ -790 − (-660) = -130
        let counter_row = outcome
            .rows
            .iter()
            .find(|row| row.account_code == ACC_COUNTER)
            .expect("对方科目行");
        assert_eq!(counter_row.fc_balance, dec("-100"));
        assert_eq!(
            counter_row.book_balance,
            dec("-660"),
            "账面必须是**科目合计**（含那笔 50 的本位币分录），按币种切分会把它丢掉"
        );
        assert_eq!(counter_row.diff, dec("-130"));
        assert_eq!(outcome.net, dec("-50"), "净损失 50（80 − 130）");

        let voucher_id = outcome.voucher_id.expect("应生成调汇凭证");
        assert!(outcome.voucher_no.is_some());
        // 凭证：本位币凭证（rate = 1、无原币列）
        let header = db()
            .query_first_sql(
                "SELECT currency_code, exchange_rate, status, source_type, period \
                   FROM biz_voucher WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("读凭证")
            .expect("凭证必须存在");
        assert_eq!(header.get_typed::<String>("currency_code").unwrap(), "CNY");
        assert_eq!(header.get_typed::<Decimal>("exchange_rate").unwrap(), Decimal::ONE);
        assert_eq!(header.get_typed::<String>("status").unwrap(), "POSTED");
        assert_eq!(
            header.get_typed::<String>("source_type").unwrap(),
            SOURCE_FX_REVAL_ACCOUNT
        );
        assert_eq!(header.get_typed::<String>("period").unwrap(), PERIOD);

        let entries = db()
            .query_sql(
                "SELECT line_no, account_code, debit_amount, credit_amount, fc_debit_amount \
                   FROM biz_voucher_entry WHERE voucher_id = ?::uuid ORDER BY line_no"
                    .to_string(),
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("读分录");
        assert_eq!(entries.len(), 3, "两个科目 + 一笔 5603");
        let mut debit = Decimal::ZERO;
        let mut credit = Decimal::ZERO;
        for row in &entries {
            debit += row.get_typed::<Decimal>("debit_amount").unwrap();
            credit += row.get_typed::<Decimal>("credit_amount").unwrap();
            assert!(
                row.get_typed::<Option<Decimal>>("fc_debit_amount").unwrap().is_none(),
                "调汇凭证是本金凭证，分录不许带原币"
            );
        }
        assert_eq!(debit, credit, "调汇凭证必须借贷平衡");
        let fx_line = entries
            .iter()
            .find(|row| row.get_typed::<String>("account_code").unwrap() == voucher::ACCOUNT_FX_GAIN_LOSS)
            .expect("必须有 5603 汇兑损益行");
        assert_eq!(
            fx_line.get_typed::<Decimal>("debit_amount").unwrap(),
            dec("50"),
            "净损失 ⇒ 借 5603"
        );

        // 调汇凭证自身的分录不得污染原币余额（本位币凭证不算原币）
        let balances = balances(db(), date(2051, 1, 31), "CNY").await.expect("再取余额");
        let asset_after = balances
            .iter()
            .find(|row| row.account_code == ACC_ASSET)
            .expect("资产科目仍在");
        assert_eq!(asset_after.fc_balance, dec("100"), "调汇凭证不能改原币余额");

        // 幂等：同一期间第二次 ⇒ skipped，不再落凭证
        let again = revalue(db(), PERIOD, date(2051, 1, 31), false, None, Utc::now())
            .await
            .expect("重跑应命中幂等闸");
        assert!(again.skipped);
        assert_eq!(again.voucher_id, Some(voucher_id));
        let count: i64 = db()
            .query_first_sql(
                "SELECT count(*) AS n FROM biz_voucher \
                  WHERE source_type = ? AND source_no = ? AND is_red = 0 AND deleted_at IS NULL"
                    .to_string(),
                vec![
                    Value::Text(SOURCE_FX_REVAL_ACCOUNT.to_string()),
                    Value::Text(PERIOD.to_string()),
                ],
            )
            .await
            .expect("统计凭证")
            .expect("count 必有行")
            .get_typed("n")
            .expect("count 值");
        assert_eq!(count, 1, "同一期间只能有一张调汇凭证");
    }

    /// 真库：`dry_run` 只算不落账；差额不足 1 分的科目不出行。
    #[tokio::test]
    async fn dry_run_computes_lines_without_posting() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (asset, counter, _base_acc) = seed_accounts().await;
        post_fx_voucher("M105-B1", "2051-01-10", "7.1", &asset, &counter, "100", "710").await;
        seed_rate("2051-01-31", "7.9").await;

        let outcome = revalue(db(), PERIOD, date(2051, 1, 31), true, None, Utc::now())
            .await
            .expect("试算应成功");
        assert!(outcome.voucher_id.is_none(), "试算不落账");
        assert!(!outcome.skipped);
        assert_eq!(outcome.rows.len(), 2);
        let zero: i64 = db()
            .query_first_sql(
                "SELECT count(*) AS n FROM biz_voucher WHERE source_type = ?".to_string(),
                vec![Value::Text(SOURCE_FX_REVAL_ACCOUNT.to_string())],
            )
            .await
            .expect("统计")
            .expect("行")
            .get_typed("n")
            .expect("值");
        assert_eq!(zero, 0, "试算不许留下凭证");

        // 汇率没动 ⇒ 无差额 ⇒ 无凭证（不发 0 金额凭证）
        seed_rate("2051-01-31", "7.1").await;
        let flat = revalue(db(), PERIOD, date(2051, 1, 31), false, None, Utc::now())
            .await
            .expect("汇率未变");
        assert!(flat.rows.is_empty(), "差额为 0 不出行：{:?}", flat.rows);
        assert!(flat.voucher_id.is_none(), "无差额不发凭证");
    }

    /// 真库：期末没有可维护的 SPOT 牌价 ⇒ 400 + 66082，且点名币种与期末日。
    #[tokio::test]
    async fn missing_rate_is_400_with_account_and_currency() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (asset, counter, _base_acc) = seed_accounts().await;
        post_fx_voucher("M105-C1", "2051-01-10", "7.1", &asset, &counter, "100", "710").await;
        // 故意不种 2051-01-31 的汇率（cleanup 已清掉 2051 年之后的 USD 行）

        let err = revalue(db(), PERIOD, date(2051, 1, 31), false, None, Utc::now())
            .await
            .expect_err("缺牌价必须报错");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_FX_REVAL_RATE_MISSING);
        assert!(err.msg.contains("USD"), "文案要点名币种：{}", err.msg);
        assert!(err.msg.contains("2051-01-31"), "文案要点名期末日：{}", err.msg);
        assert!(err.msg.contains(ACC_ASSET), "文案要点名科目：{}", err.msg);
    }
}
