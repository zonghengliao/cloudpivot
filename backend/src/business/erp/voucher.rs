//! 凭证 + 分录 + 科目映射规则（批次 D · P2-C1）：**入库过账即生成凭证**。
//!
//! 表结构以 `backend/migrations/business/V0149__erp_voucher.up.sql` 为准
//! （`\(\d` 实测口径见迁移头注释）：
//!
//! ```text
//! biz_voucher        id, voucher_word(默认「记」), voucher_no, period(yyyy-MM), voucher_date,
//!                    voucher_type(AUTO/MANUAL), source_type, source_id, source_no, summary,
//!                    total_debit, total_credit, entry_count, status(POSTED/DRAFT/AUDITED/CANCELLED),
//!                    is_red, reverse_of, reversed_by, posted_at, posted_by, remark + 审计五件套
//!                    CHECK total_debit = total_credit            ← ★ 借贷平衡（硬红线 3）
//!                    CHECK entry_count >= 2
//! biz_voucher_entry  id, voucher_id, line_no, summary, account_id, account_code, account_name,
//!                    debit_amount, credit_amount, aux_supplier_id, aux_material_id,
//!                    aux_warehouse_id, aux_dept_id, aux_qty, aux_extra, created_at
//!                    CHECK (debit=0 AND credit>0) OR (debit>0 AND credit=0)  ← 单边（硬红线 4）
//! biz_voucher_rule   id, rule_code, biz_type, debit_account_id, credit_account_id,
//!                    tax_account_id, is_active + 审计五件套
//! ```
//!
//! # 生成时机（PRD §3.3 方案 A）
//!
//! 收货单过账（`DRAFT → POSTED`）时，在**同一个事务**里（[`crate::business::erp::stockin_handler`]
//! 的 `post_in_tx` 第 ⑦ 步）生成，状态直接 `POSTED`（**不是 DRAFT**）：库存已动，
//! 账必须同步动，否则月末账实不符（PRD §3.3.3）。
//!
//! # 金额口径（硬红线 5：取自单据，不重算）
//!
//! | 分录 | 科目 | 金额 |
//! |------|------|------|
//! | 借 | 存货（规则 → 物料 `inventory_account_id` → `1405`） | `header.total_amount`（**不含税**） |
//! | 借 | 进项税（规则 → `2221.01`） | `header.total_tax`（税率为 0 时**不产生该行**） |
//! | 贷 | 应付（规则 → 供应商 `payable_account_id` → `2202`） | `header.total_with_tax`（价税合计） |
//!
//! 三行合计天然平衡：`biz_stock_in` 上有 `ck_biz_si_tax CHECK (total_with_tax = total_amount + total_tax)`。
//! 落库前仍在本模块再断言一次（`InvalidValue`），因为凭证表那条 `CHECK` 是最后一道闸，不是第一道。
//!
//! 多物料单据**按单头金额汇总成一行存货分录**（卡面 §三 ④ 明确「3 行」）。PRD §3.4.2 的
//! 「按物料汇总成多行」在本卡不落地：`biz_stock_in` 没有存「物料存货科目」的快照列，
//! 逐物料拆行会让同一张凭证在不同时点重放出不同行数。差异写进交付说明。
//!
//! # ★ 红冲口径：借贷互换，不用负数（与卡面 §五 的一处差异）
//!
//! 卡面写「红字凭证金额取负」，但同一张卡面要求 `CHECK debit_amount >= 0 AND credit_amount >= 0`
//! —— 负数写不进库。架构 §2.5.2 已选定**方案 B（借贷互换）**：原「借 100」在红字凭证里是
//! 「贷 100」，金额恒为正。净效果仍等价于负数（`Σ借 − Σ贷` 为负），但试算平衡、报表取数、
//! 对外导出都不需要特判负号。落地形态：
//!
//! * 红字凭证：`is_red=1`、`reverse_of=原凭证.id`、`status='POSTED'`；
//! * 原凭证：`status` **保持 `POSTED`**（卡面 §五），只打 `reversed_by=红字凭证.id`；
//! * 分录：原 `debit_amount` / `credit_amount` **互换**，行号从 1 重排，摘要加「红冲 」前缀。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 凭证 / 分录 / 科目映射规则是会计台账（V0149 建表，无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:voucher:* 权限码控制（P2-D1 落地）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列；biz_account 是会计科目表，属全局主数据。
//! ```
//!
//! # 事务边界
//!
//! 所有写函数吃 `&impl SqlExecutor`（`Db` / `Transaction` 都实现），**由调用方开事务**：
//! 过账的「库存 + 应付 + 凭证 + 回写引用」必须同一个事务（硬红线 2），
//! 任何一步失败整单回滚（卡面 ⑭）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::serial_model;
use crate::business::erp::stockin_model::StockInRow;
use crate::business::shared::state_machine;

// ==================== 表名 / 常量 ====================

/// 凭证主表。
pub const TABLE: &str = "biz_voucher";
/// 凭证分录表。
pub const ENTRY_TABLE: &str = "biz_voucher_entry";
/// 科目映射规则表。
pub const RULE_TABLE: &str = "biz_voucher_rule";

/// 凭证字：记（本卡只产生「记」字凭证）。
pub const WORD_DEFAULT: &str = "记";
/// 凭证来源类型：收货入库单。
pub const SOURCE_STOCK_IN: &str = "STOCK_IN";
/// 凭证业务类型（映射规则按它查）：采购入库。
pub const BIZ_TYPE_PURCHASE_IN: &str = "PURCHASE_IN";
/// 凭证类型：自动生成。
pub const VOUCHER_TYPE_AUTO: &str = "AUTO";
/// 凭证类型：手工录入（P7）。
pub const VOUCHER_TYPE_MANUAL: &str = "MANUAL";
/// 凭证来源类型：付款登记（P7，`biz_payment`）。
pub const SOURCE_PAYMENT: &str = "PAYMENT";
/// 凭证来源类型：手工录入（P7）。
pub const SOURCE_MANUAL: &str = "MANUAL";
/// 凭证状态：已记账（自动凭证直接进总账，PRD §3.3.3）。
pub const STATUS_POSTED: &str = "POSTED";
/// 会计期间状态：已结账（结账后不允许再红冲 / 记账）。
pub const PERIOD_CLOSED: &str = "CLOSED";
/// 会计期间状态：已开启（手工凭证只允许记到 OPEN 期间）。
pub const PERIOD_OPEN: &str = "OPEN";
/// 红字标记。
pub const IS_RED_YES: i16 = 1;
/// 蓝字标记。
pub const IS_RED_NO: i16 = 0;

/// 凭证号前缀（`PZ-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "PZ";
/// `biz_seq.seq_type`。
pub const SEQ_TYPE: &str = "PZ";
/// 流水号补零宽度。
pub const NO_SEQ_WIDTH: usize = 6;

/// 默认存货科目编码（`biz_account.account_code`）。
pub const DEFAULT_INVENTORY_CODE: &str = "1405";
/// 默认应付科目编码。
pub const DEFAULT_PAYABLE_CODE: &str = "2202";
/// 默认进项税科目编码。
pub const DEFAULT_TAX_CODE: &str = "2221.01";

// ==================== 取号（通用：`{前缀}-yyyyMMdd-NNNNNN`） ====================

/// 一条取号规则（`biz_seq` 的 4 个可配字段）。
#[derive(Debug, Clone, Copy)]
pub struct SeqSpec<'a> {
    /// `biz_seq.seq_type`（唯一）。
    pub seq_type: &'a str,
    /// 单号前缀（`AP` / `PZ`）。
    pub prefix: &'a str,
    /// 规则名（首次建行时写入）。
    pub seq_name: &'a str,
    /// 备注（首次建行时写入）。
    pub remark: &'a str,
}

/// 组装单号：`{prefix}-{period_key}-{no:0NO_SEQ_WIDTH$}`。
pub fn format_doc_no(prefix: &str, period_key: &str, no: i64) -> String {
    format!("{prefix}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// `biz_seq(seq_type=?)` 缺失时幂等补一行（并发下唯一键冲突视为已存在）。
async fn ensure_seq_row(ex: &impl SqlExecutor, spec: &SeqSpec<'_>) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, ?, ?, ?, 'yyyyMMdd', '', 0, 1, '-', 1, ? \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = ? AND deleted_at IS NULL)"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(spec.seq_type.to_string()),
            Value::Text(spec.seq_name.to_string()),
            Value::Text(spec.prefix.to_string()),
            Value::Text(spec.remark.to_string()),
            Value::Text(spec.seq_type.to_string()),
        ],
    )
    .await
    .or_else(|err| match err {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq` 一个号（三分支：同日累加 / 新日重置 / 时钟回拨**绝不**重置）。
///
/// 第三支是 W2b 实测的撞号地雷：`CASE WHEN period_key = ? THEN +1 ELSE 1` 会在
/// 「A→B→A（NTP 回拨 / 单测用假日期）」时把 A 当天已发的号整段撞掉。
async fn bump_seq(
    ex: &impl SqlExecutor,
    seq_type: &str,
    period_key: &str,
) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上，biz_seq 是全局取号规则字典。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = ? AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::Text(seq_type.to_string()),
            ],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("current_no"))
        .transpose()
}

/// 取一个单号（`{prefix}-yyyyMMdd-NNNNNN`，周期键按北京时间）。
pub async fn next_doc_no(
    ex: &impl SqlExecutor,
    spec: &SeqSpec<'_>,
    now: DateTime<Utc>,
) -> DbResult<String> {
    let period_key = serial_model::period_key_of(now);
    ensure_seq_row(ex, spec).await?;
    match bump_seq(ex, spec.seq_type, &period_key).await? {
        Some(no) => Ok(format_doc_no(spec.prefix, &period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='{}') 行不存在（period_key={period_key}）",
            spec.seq_type
        ))),
    }
}

/// 凭证取号规则（`PZ-yyyyMMdd-000001`）。
pub const VOUCHER_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "凭证号",
    remark: "凭证号：PZ-yyyyMMdd-000001，按天（CST）滚动",
};

/// 取一个凭证号。
pub async fn next_voucher_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    next_doc_no(ex, &VOUCHER_SEQ, now).await
}

/// 红字凭证号前缀（`PZR-yyyyMMdd-000001`）。
///
/// **独立号段**（卡面 §二 ③）：红字凭证不能和蓝字凭证抢同一段 `biz_seq`，
/// 否则「今天最后一张蓝字凭证」与「今天第一张红字凭证」的号码语义混在一起，
/// 月末按号段排查「今天记了几张凭证 / 红冲了几张」就说不清。
pub const RED_NO_PREFIX: &str = "PZR";
/// 红字号段的 `biz_seq.seq_type`。
pub const RED_SEQ_TYPE: &str = "PZR";

/// 红字凭证取号规则。
pub const RED_VOUCHER_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: RED_SEQ_TYPE,
    prefix: RED_NO_PREFIX,
    seq_name: "红字凭证号",
    remark: "红字凭证号：PZR-yyyyMMdd-000001，按天（CST）滚动（独立于 PZ 号段）",
};

/// 取一个红字凭证号。
pub async fn next_red_voucher_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    next_doc_no(ex, &RED_VOUCHER_SEQ, now).await
}

// ==================== 会计期间 ====================

/// 某日期所属会计期间的 `yyyy-MM`。
///
/// 先按 `biz_accounting_period` 的 `[start_date, end_date]` 区间查（期间可能不按自然月切）；
/// **查不到就退化成日期的 `yyyy-MM`** —— 入库日期落在未开期间时，凭证仍要能记账
/// （fail-open 只影响"期间是否已开"的校验，那属结账模块；本卡不拦过账）。
pub async fn period_of(ex: &impl SqlExecutor, date: NaiveDate) -> DbResult<String> {
    // SCOPE-EXEMPT: biz_accounting_period 是全局会计期间字典，没有归属列。
    let rows = ex
        .query_sql(
            "SELECT period_code FROM biz_accounting_period \
              WHERE start_date <= ?::text::date AND end_date >= ?::text::date \
              ORDER BY period_code DESC LIMIT 1"
                .to_string(),
            vec![Value::Text(date.to_string()), Value::Text(date.to_string())],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("period_code"),
        None => Ok(date.format("%Y-%m").to_string()),
    }
}

/// 某期间的结账状态（`biz_accounting_period.status`）。
///
/// 期间不存在 → `None`（仓库里 `period_of` 对未开期间是 fail-open 的，这里同样不拦）。
pub async fn period_status(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: biz_accounting_period 是全局会计期间字典，没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT status FROM biz_accounting_period WHERE period_code = ?".to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    match row.as_ref() {
        Some(row) => Ok(Some(row.get_typed("status")?)),
        None => Ok(None),
    }
}

/// 期间是否已结账（红冲 / 记账的最后一道闸）。
pub async fn is_period_closed(ex: &impl SqlExecutor, period_code: &str) -> DbResult<bool> {
    Ok(period_status(ex, period_code)
        .await?
        .as_deref()
        .is_some_and(|status| status == PERIOD_CLOSED))
}

/// 北京时间「今天」（周期键 `yyyyMMdd` → 日期）。
///
/// 红字凭证的日期用「作废当天」，**必须按 CST 算**：`Utc::now().date_naive()` 在凌晨
/// （北京 00:00–08:00）还是前一天，会把凭证记到错误的期间。
pub fn today_cst(now: DateTime<Utc>) -> NaiveDate {
    let key = serial_model::period_key_of(now);
    NaiveDate::parse_from_str(&key, "%Y%m%d").unwrap_or_else(|_| now.date_naive())
}

// ==================== 科目解析 ====================
/// 科目引用（id + 编码 + 名称；分录要落编码/名称快照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRef {
    /// `biz_account.id`。
    pub id: Uuid,
    /// 科目编码（如 `1405`）。
    pub code: String,
    /// 科目名称（如 `库存商品`）。
    pub name: String,
}

/// 采购入库要用的三个科目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSet {
    /// 借：存货。
    pub inventory: AccountRef,
    /// 贷：应付账款。
    pub payable: AccountRef,
    /// 借：进项税（可空 = 系统里没有进项税科目；税率 0 时不需要它）。
    pub tax: Option<AccountRef>,
}

fn account_missing(what: &str, source: &str) -> DbError {
    DbError::InvalidValue(format!(
        "科目映射失败：{what}（{source}）不存在 / 已停用 / 已删除。\
         请先启用该科目，或在 biz_voucher_rule 里配置合法的映射规则"
    ))
}

async fn load_account_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AccountRef>> {
    // SCOPE-EXEMPT: biz_account 是会计科目主数据（全局），没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT id, account_code, account_name FROM biz_account \
              WHERE id = ?::uuid AND status = 1 AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(account_of).transpose()
}

async fn load_account_by_code(ex: &impl SqlExecutor, code: &str) -> DbResult<Option<AccountRef>> {
    // SCOPE-EXEMPT: 同上。
    let row = ex
        .query_first_sql(
            "SELECT id, account_code, account_name FROM biz_account \
              WHERE account_code = ? AND status = 1 AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(code.to_string())],
        )
        .await?;
    row.as_ref().map(account_of).transpose()
}

fn account_of(row: &Row) -> DbResult<AccountRef> {
    Ok(AccountRef {
        id: row.get_typed("id")?,
        code: row.get_typed("account_code")?,
        name: row.get_typed("account_name")?,
    })
}

/// 按科目编码取启用中的科目（可空）。
///
/// 供付款凭证（2202 / 1002）与手工凭证按 `account_id` 回填编码 / 名称快照复用。
pub async fn account_by_code(ex: &impl SqlExecutor, code: &str) -> DbResult<Option<AccountRef>> {
    load_account_by_code(ex, code).await
}

/// 按科目编码取科目，取不到就报**点名科目编码**的明确错误（卡面 §一 ②：科目不存在要报明确错误）。
pub async fn account_required(
    ex: &impl SqlExecutor,
    code: &str,
    what: &str,
) -> DbResult<AccountRef> {
    load_account_by_code(ex, code)
        .await?
        .ok_or_else(|| account_missing(what, &format!("科目编码 {code}")))
}

/// 按科目 id 取启用中的科目（可空），供手工凭证按 `account_id` 取快照。
pub async fn account_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AccountRef>> {
    load_account_by_id(ex, id).await
}

/// 一条映射规则（三个科目 id 都已解析成 `AccountRef`；税科目可空）。
#[derive(Debug, Clone)]
struct ResolvedRule {
    debit: Option<AccountRef>,
    credit: Option<AccountRef>,
    tax: Option<AccountRef>,
}

/// 按 `biz_type` 取启用的映射规则（`rule_code` 唯一，取最早一条）。
async fn load_rule(ex: &impl SqlExecutor, biz_type: &str) -> DbResult<Option<ResolvedRule>> {
    // SCOPE-EXEMPT: biz_voucher_rule 是全局科目映射配置，没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT debit_account_id, credit_account_id, tax_account_id \
               FROM biz_voucher_rule \
              WHERE biz_type = ? AND is_active = 1 AND deleted_at IS NULL \
              ORDER BY created_at ASC LIMIT 1"
                .to_string(),
            vec![Value::Text(biz_type.to_string())],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let debit_id: Option<Uuid> = row.get_typed("debit_account_id")?;
    let credit_id: Option<Uuid> = row.get_typed("credit_account_id")?;
    let tax_id: Option<Uuid> = row.get_typed("tax_account_id")?;
    let debit = match debit_id {
        Some(id) => load_account_by_id(ex, id).await?,
        None => None,
    };
    let credit = match credit_id {
        Some(id) => load_account_by_id(ex, id).await?,
        None => None,
    };
    let tax = match tax_id {
        Some(id) => load_account_by_id(ex, id).await?,
        None => None,
    };
    Ok(Some(ResolvedRule { debit, credit, tax }))
}

/// 物料的存货科目（`biz_material.inventory_account_id`，可空）。
async fn material_inventory_account(
    ex: &impl SqlExecutor,
    material_id: Uuid,
) -> DbResult<Option<AccountRef>> {
    // SCOPE-EXEMPT: biz_material 是物料主数据（全局），没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT inventory_account_id FROM biz_material \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&material_id)],
        )
        .await?;
    match row
        .as_ref()
        .map(|row| row.get_typed::<Option<Uuid>>("inventory_account_id"))
    {
        Some(Ok(Some(id))) => load_account_by_id(ex, id).await,
        Some(Ok(None)) | None => Ok(None),
        Some(Err(err)) => Err(err),
    }
}

/// 供应商的应付科目（`biz_supplier.payable_account_id`，可空）。
async fn supplier_payable_account(
    ex: &impl SqlExecutor,
    supplier_id: Uuid,
) -> DbResult<Option<AccountRef>> {
    // SCOPE-EXEMPT: biz_supplier 是供应商主数据（全局），没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT payable_account_id FROM biz_supplier \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    match row
        .as_ref()
        .map(|row| row.get_typed::<Option<Uuid>>("payable_account_id"))
    {
        Some(Ok(Some(id))) => load_account_by_id(ex, id).await,
        Some(Ok(None)) | None => Ok(None),
        Some(Err(err)) => Err(err),
    }
}

/// 解析本次业务要用的三个科目（PRD §3.4 / 卡面 §四）。
///
/// 优先级（**逐项独立**，规则里缺哪项就单独退化，不会整条规则作废）：
///
/// ```text
/// 借存货：biz_voucher_rule.debit_account_id  → 物料 inventory_account_id → 1405
/// 贷应付：biz_voucher_rule.credit_account_id → 供应商 payable_account_id → 2202
/// 借进项税：biz_voucher_rule.tax_account_id  → 2221.01（可缺；税率 0 时用不到）
/// ```
///
/// 存货 / 应付科目最终解析不到 → `InvalidValue`（**宁可拦住，不许兜底**，PRD §3.4.3）。
pub async fn resolve_accounts(
    ex: &impl SqlExecutor,
    biz_type: &str,
    material_id: Option<Uuid>,
    supplier_id: Option<Uuid>,
) -> DbResult<AccountSet> {
    let rule = load_rule(ex, biz_type).await?;

    let inventory = match rule.as_ref().and_then(|r| r.debit.clone()) {
        Some(account) => account,
        None => {
            let from_material = match material_id {
                Some(id) => material_inventory_account(ex, id).await?,
                None => None,
            };
            match from_material {
                Some(account) => account,
                None => load_account_by_code(ex, DEFAULT_INVENTORY_CODE)
                    .await?
                    .ok_or_else(|| {
                        account_missing(
                            "默认存货科目 1405 库存商品",
                            "biz_voucher_rule 无 PURCHASE_IN 规则且物料未配存货科目",
                        )
                    })?,
            }
        }
    };

    let payable = match rule.as_ref().and_then(|r| r.credit.clone()) {
        Some(account) => account,
        None => {
            let from_supplier = match supplier_id {
                Some(id) => supplier_payable_account(ex, id).await?,
                None => None,
            };
            match from_supplier {
                Some(account) => account,
                None => load_account_by_code(ex, DEFAULT_PAYABLE_CODE)
                    .await?
                    .ok_or_else(|| {
                        account_missing(
                            "默认应付科目 2202 应付账款",
                            "biz_voucher_rule 无 PURCHASE_IN 规则且供应商未配应付科目",
                        )
                    })?,
            }
        }
    };

    let tax = match rule.as_ref().and_then(|r| r.tax.clone()) {
        Some(account) => Some(account),
        None => load_account_by_code(ex, DEFAULT_TAX_CODE).await?,
    };

    Ok(AccountSet {
        inventory,
        payable,
        tax,
    })
}

// ==================== 分录（纯逻辑） ====================

/// 一张单据要入账的三个金额（**全部取自单头，不重算**）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoucherAmounts {
    /// 不含税金额（`biz_stock_in.total_amount`）→ 借存货。
    pub amount: Decimal,
    /// 税额（`total_tax`）→ 借进项税（0 时不出行）。
    pub tax: Decimal,
    /// 价税合计（`total_with_tax`）→ 贷应付。
    pub with_tax: Decimal,
}

impl VoucherAmounts {
    /// 由收货单单头三列构造。
    pub fn of_stock_in(header: &StockInRow) -> Self {
        Self {
            amount: header.total_amount.round_dp(4),
            tax: header.total_tax.round_dp(4),
            with_tax: header.total_with_tax.round_dp(4),
        }
    }

    /// 借贷合计（蓝字 = 价税合计）。
    pub fn total(&self) -> Decimal {
        self.with_tax
    }
}

/// 一条待落库的分录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryLine {
    /// 行号（从 1 起）。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目（编码 / 名称要快照进分录）。
    pub account: AccountRef,
    /// 借方金额。
    pub debit: Decimal,
    /// 贷方金额。
    pub credit: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<Uuid>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<Uuid>,
}

/// 分录构造参数（避免 `too_many_arguments`）。
#[derive(Debug, Clone, Copy)]
pub struct LineContext {
    /// 借方存货行的辅助核算：仓库。
    pub warehouse_id: Option<Uuid>,
    /// 借方存货行的辅助核算：物料（多物料汇总时为空）。
    pub material_id: Option<Uuid>,
    /// 贷方应付行的辅助核算：供应商。
    pub supplier_id: Option<Uuid>,
}

/// 采购入库蓝字分录（PRD §3.4.2 / 卡面 §三 ④）。
///
/// ```text
/// 借 库存商品        total_amount   （不含税）
/// 借 应交税费-进项税  total_tax      （税率为 0 时不产生该行）
/// 贷 应付账款        total_with_tax （价税合计）
/// ```
///
/// 金额为 0 的行直接略过（例如 0 元赠品行），行号最后统一重排。
pub fn purchase_in_lines(
    accounts: &AccountSet,
    amounts: VoucherAmounts,
    summary: &str,
    ctx: LineContext,
) -> Vec<EntryLine> {
    let mut lines: Vec<EntryLine> = Vec::with_capacity(3);
    if amounts.amount > Decimal::ZERO {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{summary} 存货"),
            account: accounts.inventory.clone(),
            debit: amounts.amount,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: ctx.material_id,
            aux_warehouse_id: ctx.warehouse_id,
        });
    }
    if amounts.tax > Decimal::ZERO
        && let Some(tax_account) = accounts.tax.clone()
    {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{summary} 进项税额"),
            account: tax_account,
            debit: amounts.tax,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
        });
    }
    if amounts.with_tax > Decimal::ZERO {
        lines.push(EntryLine {
            line_no: 0,
            summary: summary.to_string(),
            account: accounts.payable.clone(),
            debit: Decimal::ZERO,
            credit: amounts.with_tax,
            aux_supplier_id: ctx.supplier_id,
            aux_material_id: None,
            aux_warehouse_id: None,
        });
    }
    for (index, line) in lines.iter_mut().enumerate() {
        line.line_no = i32::try_from(index + 1).unwrap_or(0);
    }
    lines
}

/// 红字分录：**借贷互换**（架构 §2.5.2 方案 B），金额原样、行号重排、摘要加「红冲 」。
pub fn reversed_lines(original: &[EntryRow], reason: Option<&str>) -> Vec<EntryLine> {
    let prefix = match reason.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => format!("红冲（{text}）"),
        None => "红冲".to_string(),
    };
    original
        .iter()
        .enumerate()
        .map(|(index, row)| EntryLine {
            line_no: i32::try_from(index + 1).unwrap_or(0),
            summary: format!("{prefix}{}", row.summary),
            account: row.account.clone(),
            debit: row.credit,
            credit: row.debit,
            aux_supplier_id: row.aux_supplier_id,
            aux_material_id: row.aux_material_id,
            aux_warehouse_id: row.aux_warehouse_id,
        })
        .collect()
}

/// 分录合计（借方 / 贷方）。
pub fn totals_of(lines: &[EntryLine]) -> (Decimal, Decimal) {
    let debit = lines.iter().fold(Decimal::ZERO, |acc, l| acc + l.debit);
    let credit = lines.iter().fold(Decimal::ZERO, |acc, l| acc + l.credit);
    (debit.round_dp(4), credit.round_dp(4))
}

// ==================== 行结构（读） ====================

/// `biz_voucher` 列清单。
pub const VOUCHER_COLUMNS: &str = "id, voucher_word, voucher_no, period, voucher_date, \
     voucher_type, source_type, source_id, source_no, summary, total_debit, total_credit, \
     entry_count, status, is_red, reverse_of, reversed_by, posted_at, posted_by, remark, \
     created_at, updated_at, create_by, update_by, deleted_at";

/// 凭证行。
#[derive(Debug, Clone)]
pub struct VoucherRow {
    /// 主键。
    pub id: Uuid,
    /// 凭证字。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 会计期间（`yyyy-MM`）。
    pub period: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证类型（AUTO / MANUAL）。
    pub voucher_type: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Option<Uuid>,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 摘要。
    pub summary: String,
    /// 借方合计。
    pub total_debit: Decimal,
    /// 贷方合计。
    pub total_credit: Decimal,
    /// 分录行数。
    pub entry_count: i32,
    /// 状态。
    pub status: String,
    /// 是否红字。
    pub is_red: i16,
    /// 红字凭证指向的原凭证。
    pub reverse_of: Option<Uuid>,
    /// 原凭证指向的红字凭证。
    pub reversed_by: Option<Uuid>,
    /// 过账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 过账人。
    pub posted_by: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

impl VoucherRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            voucher_word: row.get_typed("voucher_word")?,
            voucher_no: row.get_typed("voucher_no")?,
            period: row.get_typed("period")?,
            voucher_date: row.get_typed("voucher_date")?,
            voucher_type: row.get_typed("voucher_type")?,
            source_type: row.get_typed("source_type")?,
            source_id: row.get_typed("source_id")?,
            source_no: row.get_typed("source_no")?,
            summary: row.get_typed("summary")?,
            total_debit: row.get_typed("total_debit")?,
            total_credit: row.get_typed("total_credit")?,
            entry_count: row.get_typed("entry_count")?,
            status: row.get_typed("status")?,
            is_red: row.get_typed("is_red")?,
            reverse_of: row.get_typed("reverse_of")?,
            reversed_by: row.get_typed("reversed_by")?,
            posted_at: row.get_typed("posted_at")?,
            posted_by: row.get_typed("posted_by")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// `biz_voucher_entry` 列清单。
pub const ENTRY_COLUMNS: &str = "id, voucher_id, line_no, summary, account_id, account_code, \
     account_name, debit_amount, credit_amount, aux_supplier_id, aux_material_id, \
     aux_warehouse_id, created_at";

/// 分录行（读回用）。
#[derive(Debug, Clone)]
pub struct EntryRow {
    /// 主键。
    pub id: Uuid,
    /// 所属凭证。
    pub voucher_id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目（id / 编码 / 名称快照）。
    pub account: AccountRef,
    /// 借方金额。
    pub debit: Decimal,
    /// 贷方金额。
    pub credit: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<Uuid>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<Uuid>,
}

impl EntryRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            voucher_id: row.get_typed("voucher_id")?,
            line_no: row.get_typed("line_no")?,
            summary: row.get_typed("summary")?,
            account: AccountRef {
                id: row.get_typed("account_id")?,
                code: row.get_typed("account_code")?,
                name: row.get_typed("account_name")?,
            },
            debit: row.get_typed("debit_amount")?,
            credit: row.get_typed("credit_amount")?,
            aux_supplier_id: row.get_typed("aux_supplier_id")?,
            aux_material_id: row.get_typed("aux_material_id")?,
            aux_warehouse_id: row.get_typed("aux_warehouse_id")?,
        })
    }
}

/// 按 id 取凭证。
pub async fn find_voucher(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<VoucherRow>> {
    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {VOUCHER_COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(VoucherRow::from_row).transpose()
}

/// 按 id 取凭证并**加行锁**（`SELECT ... FOR UPDATE`，红冲用：防同一张凭证被并发红冲两次）。
pub async fn find_voucher_locked(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<VoucherRow>> {
    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {VOUCHER_COLUMNS} FROM {TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(VoucherRow::from_row).transpose()
}

/// 按来源单据取真凭证（`is_red = 0`，取最早一张）。
pub async fn find_voucher_by_source(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
) -> DbResult<Option<VoucherRow>> {
    // SCOPE-EXEMPT: 同上。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {VOUCHER_COLUMNS} FROM {TABLE} \
                  WHERE source_type = ? AND source_id = ?::uuid AND is_red = 0 AND deleted_at IS NULL \
                  ORDER BY created_at ASC LIMIT 1"
            ),
            vec![
                Value::Text(source_type.to_string()),
                Value::from(&source_id),
            ],
        )
        .await?;
    row.as_ref().map(VoucherRow::from_row).transpose()
}

/// 取一张凭证的全部分录（行号升序）。
pub async fn entries_of(ex: &impl SqlExecutor, voucher_id: Uuid) -> DbResult<Vec<EntryRow>> {
    // SCOPE-EXEMPT: 同上。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {ENTRY_COLUMNS} FROM {ENTRY_TABLE} WHERE voucher_id = ?::uuid \
                 ORDER BY line_no ASC"
            ),
            vec![Value::from(&voucher_id)],
        )
        .await?;
    rows.iter().map(EntryRow::from_row).collect()
}

// ==================== 写：生成凭证 ====================

/// 生成凭证的全部入参（`too_many_arguments` 的结构体化）。
#[derive(Debug, Clone)]
pub struct AutoVoucher<'a> {
    /// 来源单据类型（`STOCK_IN`）。
    pub source_type: &'a str,
    /// 来源单据 id。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: &'a str,
    /// 凭证日期（= 单据业务日期）。
    pub voucher_date: NaiveDate,
    /// 三个金额（取自单头）。
    pub amounts: VoucherAmounts,
    /// 凭证摘要（整单级）。
    pub summary: String,
    /// 映射规则用的业务类型（`PURCHASE_IN`）。
    pub biz_type: &'a str,
    /// 存货科目退化用的物料。
    pub material_id: Option<Uuid>,
    /// 应付科目退化用的供应商。
    pub supplier_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub warehouse_id: Option<Uuid>,
}

/// 落一张凭证 + 它的分录（**不 commit**，由调用方的事务收口）。
///
/// 步骤：解析科目 → 定期间 → 取号 → 造分录 → 校验平衡 → 写主表 → 写分录。
/// 返回凭证 id。
pub async fn create_auto(
    ex: &impl SqlExecutor,
    req: &AutoVoucher<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let accounts = resolve_accounts(ex, req.biz_type, req.material_id, req.supplier_id).await?;

    // 税率 > 0 但系统里没有进项税科目 → 拦住（不许静默漏记进项税）
    if req.amounts.tax > Decimal::ZERO && accounts.tax.is_none() {
        return Err(account_missing(
            format!("进项税科目 {}", DEFAULT_TAX_CODE).as_str(),
            "本次入库有税额，但系统里没有可用的进项税科目",
        ));
    }

    let lines = purchase_in_lines(
        &accounts,
        req.amounts,
        &req.summary,
        LineContext {
            warehouse_id: req.warehouse_id,
            material_id: req.material_id,
            supplier_id: req.supplier_id,
        },
    );
    let (debit, credit) = totals_of(&lines);
    if lines.len() < 2 {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：单据 {} 的金额全为 0（分录只有 {} 行），会计上不构成一张凭证",
            req.source_no,
            lines.len()
        )));
    }
    if debit != credit {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：借贷不平衡（借 {debit} / 贷 {credit}），单据 {}",
            req.source_no
        )));
    }

    post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: req.source_type,
            source_id: Some(req.source_id),
            source_no: Some(req.source_no),
            voucher_date: req.voucher_date,
            summary: req.summary.clone(),
            lines,
            remark: None,
            red_of: None,
        },
        operator,
        now,
    )
    .await
}

/// 批量写分录（单条语句，行号唯一键冲突即失败）。
async fn write_entries(
    ex: &impl SqlExecutor,
    voucher_id: Uuid,
    lines: &[EntryLine],
) -> DbResult<()> {
    for line in lines {
        // SCOPE-EXEMPT: 凭证分录无归属列（见模块文档）。
        ex.exec_sql(
            format!(
                "INSERT INTO {ENTRY_TABLE} \
                    (voucher_id, line_no, summary, account_id, account_code, account_name, \
                     debit_amount, credit_amount, aux_supplier_id, aux_material_id, aux_warehouse_id) \
                 VALUES (?::uuid, ?, ?, ?::uuid, ?, ?, NULLIF(?::text,'')::numeric, \
                         NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            ),
            vec![
                Value::from(&voucher_id),
                Value::from(line.line_no),
                Value::Text(line.summary.clone()),
                Value::from(&line.account.id),
                Value::Text(line.account.code.clone()),
                Value::Text(line.account.name.clone()),
                Value::Text(line.debit.to_string()),
                Value::Text(line.credit.to_string()),
                Value::Text(
                    line.aux_supplier_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
                Value::Text(
                    line.aux_material_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
                Value::Text(
                    line.aux_warehouse_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 落一张凭证的全部入参（自动 / 手工 / 红字**共用同一个写入口**）。
#[derive(Debug, Clone)]
pub struct PostVoucher<'a> {
    /// 凭证类型（`AUTO` / `MANUAL`）。
    pub voucher_type: &'a str,
    /// 来源单据类型（`STOCK_IN` / `PAYMENT` / `MANUAL`）。
    pub source_type: &'a str,
    /// 来源单据 id（手工凭证为 `None`）。
    pub source_id: Option<Uuid>,
    /// 来源单号（手工凭证为 `None`）。
    pub source_no: Option<&'a str>,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证摘要。
    pub summary: String,
    /// 分录（≥ 2 行、借贷必须平衡）。
    pub lines: Vec<EntryLine>,
    /// 备注。
    pub remark: Option<&'a str>,
    /// `Some(原凭证 id)` → 落红字凭证（`is_red = 1`、`reverse_of = 原凭证`、走 `PZR` 独立号段）。
    pub red_of: Option<Uuid>,
}

/// 落一张凭证 + 它的分录（**不 commit**，由调用方的事务收口）。
///
/// 三条写路径（收货自动凭证 / 付款凭证 / 手工凭证）与红冲**共用这一个函数**：
/// 校验（≥ 2 行、借贷平衡）与落库（主表 + 分录 + 取号）只有一份实现，
/// 不存在「某条路径忘了校验平衡」的可能。
pub async fn post_voucher(
    ex: &impl SqlExecutor,
    req: &PostVoucher<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    if req.lines.len() < 2 {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：至少要 2 条分录（当前 {} 行），会计上不构成一张凭证",
            req.lines.len()
        )));
    }
    let (debit, credit) = totals_of(&req.lines);
    if debit != credit {
        let diff = debit - credit;
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：借贷不平衡（借 {debit} / 贷 {credit}，差额 {diff}）"
        )));
    }
    if debit <= Decimal::ZERO {
        return Err(DbError::InvalidValue(
            "凭证生成失败：借贷合计为 0，金额全为 0 的单据不构成一张凭证".to_string(),
        ));
    }

    let period = period_of(ex, req.voucher_date).await?;
    let voucher_no = match req.red_of {
        Some(_) => next_red_voucher_no(ex, now).await?,
        None => next_voucher_no(ex, now).await?,
    };
    let voucher_no_text = voucher_no.clone();
    let entry_count = i32::try_from(req.lines.len()).unwrap_or(0);
    let is_red = if req.red_of.is_some() {
        IS_RED_YES
    } else {
        IS_RED_NO
    };
    let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();

    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    let inserted = ex
        .query_first_sql(
            format!(
                "INSERT INTO {TABLE} \
                    (voucher_word, voucher_no, period, voucher_date, voucher_type, source_type, \
                     source_id, source_no, summary, total_debit, total_credit, entry_count, \
                     status, is_red, reverse_of, posted_at, posted_by, remark, \
                     created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?, ?, NULLIF(?::text,'')::date, ?, ?, NULLIF(?::text,'')::uuid, ?, ?, \
                         NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?, ?, ?, \
                         NULLIF(?::text,'')::uuid, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
                         ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
                 RETURNING id"
            ),
            vec![
                Value::Text(WORD_DEFAULT.to_string()),
                Value::Text(voucher_no),
                Value::Text(period),
                Value::Text(req.voucher_date.to_string()),
                Value::Text(req.voucher_type.to_string()),
                Value::Text(req.source_type.to_string()),
                Value::Text(req.source_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(req.source_no.unwrap_or_default().to_string()),
                Value::Text(req.summary.clone()),
                Value::Text(debit.to_string()),
                Value::Text(credit.to_string()),
                Value::from(entry_count),
                Value::Text(STATUS_POSTED.to_string()),
                Value::I16(is_red),
                Value::Text(req.red_of.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(now),
                Value::Text(operator_text.clone()),
                Value::Text(req.remark.unwrap_or_default().to_string()),
                Value::from(now),
                Value::from(now),
                Value::Text(operator_text.clone()),
                Value::Text(operator_text),
            ],
        )
        .await?;
    let voucher_id: Uuid = match inserted.as_ref() {
        Some(row) => row.get_typed("id")?,
        None => {
            return Err(DbError::InvalidValue(format!(
                "凭证写入失败：INSERT 没有返回 id（{}）",
                req.source_no.unwrap_or("手工凭证")
            )));
        }
    };

    write_entries(ex, voucher_id, &req.lines).await?;
    // P10 铁律 ③：凭证建单留痕（`NULL → POSTED`）。凭证没有原地状态流转（红冲开新单），
    // 所以这里只留「谁在什么时候记了这张凭证」。
    state_machine::log_created(
        ex,
        state_machine::ENTITY_VOUCHER,
        voucher_id,
        &voucher_no_text,
        STATUS_POSTED,
        operator,
        req.red_of.map(|_| "红字冲销凭证"),
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok(voucher_id)
}

/// 红冲入参。
#[derive(Debug, Clone, Copy)]
pub struct ReversalInput<'a> {
    /// 红冲原因（写进红字凭证分录摘要与备注）。
    pub reason: Option<&'a str>,
    /// 红字凭证日期（业务口径：作废 / 更正当天）。
    pub voucher_date: NaiveDate,
}

/// 红冲一张**已记账**凭证：生成红字凭证（借贷互换）+ 回写原凭证 `reversed_by`。
///
/// # 校验（卡面 §二 ②）
///
/// ```text
/// ① status 必须 POSTED
/// ② 本身是红字凭证（is_red = 1）的不能再红冲
/// ③ 已红冲过（reversed_by 非空）的拒绝，文案点名红字凭证号
/// ④ 所属期间已结账（CLOSED）的拒绝
/// ```
///
/// 原凭证 `status` **保持 `POSTED`**（与 P2-C1 同口径），只打 `reversed_by`。
pub async fn reverse_posted(
    ex: &impl SqlExecutor,
    original: &VoucherRow,
    input: ReversalInput<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    if original.is_red == IS_RED_YES {
        return Err(DbError::InvalidValue(format!(
            "红冲失败：凭证 {} 本身是红字凭证，红字凭证不能再红冲（要更正请直接补一张蓝字凭证）",
            original.voucher_no
        )));
    }
    if original.status != STATUS_POSTED {
        return Err(DbError::InvalidValue(format!(
            "红冲失败：凭证 {} 当前状态是 {}，只有已记账（POSTED）的凭证能红冲",
            original.voucher_no, original.status
        )));
    }
    if let Some(red_id) = original.reversed_by {
        let red_no = find_voucher(ex, red_id)
            .await?
            .map(|row| row.voucher_no)
            .unwrap_or_else(|| red_id.to_string());
        return Err(DbError::InvalidValue(format!(
            "红冲失败：该凭证 {} 已经红冲（红字凭证 {red_no}），不能重复红冲",
            original.voucher_no
        )));
    }
    if is_period_closed(ex, &original.period).await? {
        return Err(DbError::InvalidValue(format!(
            "红冲失败：凭证 {} 所属期间 {} 已结账，不能再红冲（请先反结账）",
            original.voucher_no, original.period
        )));
    }

    let entries = entries_of(ex, original.id).await?;
    if entries.is_empty() {
        return Err(DbError::InvalidValue(format!(
            "红冲失败：原凭证 {} 没有任何分录",
            original.voucher_no
        )));
    }
    let lines = reversed_lines(&entries, input.reason);
    let summary = format!(
        "红冲凭证 {} 借贷合计 {}",
        original.voucher_no, original.total_debit
    );

    let red_id = post_voucher(
        ex,
        &PostVoucher {
            voucher_type: &original.voucher_type,
            source_type: &original.source_type,
            source_id: original.source_id,
            source_no: original.source_no.as_deref(),
            voucher_date: input.voucher_date,
            summary,
            lines,
            remark: input.reason.map(str::trim).filter(|text| !text.is_empty()),
            red_of: Some(original.id),
        },
        operator,
        now,
    )
    .await?;

    // 原凭证保持 POSTED（卡面 §二 ②），只打反向指针
    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET reversed_by = ?::uuid, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&red_id),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&original.id),
        ],
    )
    .await?;

    Ok(red_id)
}

/// 收货单过账时生成蓝字凭证（卡面 §三 ②③④）。
pub async fn create_for_stock_in(
    ex: &impl SqlExecutor,
    header: &StockInRow,
    first_material: Option<Uuid>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let summary = format!(
        "采购入库 {} 价税合计 {}",
        header.in_no, header.total_with_tax
    );
    create_auto(
        ex,
        &AutoVoucher {
            source_type: SOURCE_STOCK_IN,
            source_id: header.id,
            source_no: &header.in_no,
            voucher_date: header.in_date,
            amounts: VoucherAmounts::of_stock_in(header),
            summary,
            biz_type: BIZ_TYPE_PURCHASE_IN,
            material_id: first_material,
            supplier_id: header.supplier_id,
            warehouse_id: Some(header.warehouse_id),
        },
        operator,
        now,
    )
    .await
}

// ==================== 写：红冲 ====================

/// 收货单作废时生成红字凭证（卡面 §五）。
///
/// * 原凭证 `status` 保持 `POSTED`，只写 `reversed_by`；
/// * 红字凭证 `is_red=1`、`reverse_of=原凭证`，分录借贷互换；
/// * 找不到原凭证（未过账的历史脏数据）→ 返回 `Ok(None)`，不阻断作废。
pub async fn reverse_for_stock_in(
    ex: &impl SqlExecutor,
    header: &StockInRow,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let Some(original) = find_voucher_by_source(ex, SOURCE_STOCK_IN, header.id).await? else {
        return Ok(None);
    };
    // 红字凭证日期：作废当天（CST）。已作废单用 cancelled_at，未作废的兜底「今天」。
    let voucher_date = header
        .cancelled_at
        .map(|at| at.date_naive())
        .unwrap_or_else(|| today_cst(now));
    // 红冲只保留一份实现（[`reverse_posted`]）；本函数只负责「找到原凭证 + 定日期」。
    let red_id = reverse_posted(
        ex,
        &original,
        ReversalInput {
            reason,
            voucher_date,
        },
        operator,
        now,
    )
    .await?;
    Ok(Some(red_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::stockin_dto::{CreateStockInReq, StockInItemReq, StockInStatusReq};
    use crate::business::erp::stockin_handler::{change_stock_in_status, create_stock_in};
    use crate::business::erp::{payable, stockin_model};
    use nik::prelude::chrono::Duration;
    use std::str::FromStr;

    const SUPPLIER: &str = "b2c10001-0000-4000-8000-000000000002";
    const WAREHOUSE: &str = "b2c10001-0000-4000-8000-000000000003";
    const MATERIAL_A: &str = "b2c10001-0000-4000-8000-000000000011";
    /// 配了存货科目的物料（用于验证「规则缺省时退化到物料科目」）。
    const MATERIAL_B: &str = "b2c10001-0000-4000-8000-000000000012";
    /// 测试专用用户科目（规则指向它，证明「规则优先」）。
    const TEST_ACCOUNT: &str = "b2c10001-0000-4000-8000-000000000021";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('b2c10001-0000-4000-8000-000000000001', 'P2C1-PCS', '个', 1, NULL, 1.000000, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days)
         VALUES ('b2c10001-0000-4000-8000-000000000002', 'P2C1-SUP', 'P2C1 测试供应商', 1, 30)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
         VALUES ('b2c10001-0000-4000-8000-000000000003', 'P2C1-WH', 'P2C1 收货仓',
                 '/b2c10001-0000-4000-8000-000000000003/', 1, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
         VALUES ('b2c10001-0000-4000-8000-000000000011', 'P2C1-M1', 'P2C1 螺丝',
                 'b2c10001-0000-4000-8000-000000000001', 1, false),
                ('b2c10001-0000-4000-8000-000000000012', 'P2C1-M2', 'P2C1 垫片',
                 'b2c10001-0000-4000-8000-000000000001', 1, false)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                                  account_category, balance_direction, is_leaf, status)
         VALUES ('b2c10001-0000-4000-8000-000000000021', 'P2C1.1405', 'P2C1 测试存货科目', NULL, 1,
                 'ASSET', 'DEBIT', 1, 1)
         ON CONFLICT DO NOTHING",
    ];

    /// 真库用例必须串行（共用全局 `db()`、同一批夹具与同一批 `biz_seq`，与 P2-B1 同范式）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
                // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
                // `max_lifetime = 0` 逼池子每次新建连接（W1b 实测的「借到死连接」）。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn exec(sql: &str) {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具 + 复位科目映射规则。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        let cleanups = [
            format!(
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                 (SELECT id FROM biz_voucher WHERE source_id IN \
                   (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid))"
            ),
            format!(
                "DELETE FROM biz_voucher WHERE source_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!("DELETE FROM biz_payable WHERE supplier_id = '{SUPPLIER}'::uuid"),
            format!(
                "DELETE FROM biz_inventory_txn WHERE doc_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_inventory WHERE material_id IN \
                 ('{MATERIAL_A}'::uuid, '{MATERIAL_B}'::uuid) AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            format!("DELETE FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid"),
            // 复位上一轮可能改过的映射规则 / 科目状态
            "UPDATE biz_voucher_rule SET deleted_at = NULL, is_active = 1, \
                    debit_account_id = (SELECT id FROM biz_account WHERE account_code = '1405' AND deleted_at IS NULL), \
                    credit_account_id = (SELECT id FROM biz_account WHERE account_code = '2202' AND deleted_at IS NULL), \
                    tax_account_id = (SELECT id FROM biz_account WHERE account_code = '2221.01' AND deleted_at IS NULL) \
              WHERE rule_code = 'PURCHASE_IN_DEFAULT'".to_string(),
            "UPDATE biz_account SET status = 1 WHERE account_code IN ('1405','2202','2221.01')".to_string(),
            format!("UPDATE biz_material SET inventory_account_id = NULL WHERE id = '{MATERIAL_B}'::uuid"),
            "DELETE FROM biz_voucher WHERE source_no LIKE 'BROKEN-%' OR voucher_no LIKE '__P2C1%'"
                .to_string(),
        ];
        for sql in cleanups {
            exec(&sql).await;
        }
        for sql in FIXTURES {
            exec(sql).await;
        }
        Some(guard)
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    fn line_req(material: &str, qty: &str, price: &str, rate: &str) -> StockInItemReq {
        StockInItemReq {
            purchase_order_item_id: None,
            material_id: Some(material.to_string()),
            qty: Some(dec(qty)),
            price_excl: Some(dec(price)),
            tax_rate: Some(dec(rate)),
            location_id: None,
            serial_ids: Vec::new(),
            remark: None,
        }
    }

    fn req(items: Vec<StockInItemReq>) -> CreateStockInReq {
        CreateStockInReq {
            source_order_id: None,
            supplier_id: Some(SUPPLIER.to_string()),
            warehouse_id: Some(WAREHOUSE.to_string()),
            in_date: None,
            invoice_received: None,
            remark: None,
            items,
            total_qty: None,
            total_amount: None,
            total_tax: None,
        }
    }

    fn post_req() -> StockInStatusReq {
        StockInStatusReq {
            status: Some("POSTED".to_string()),
            reason: None,
            serials: Vec::new(),
        }
    }

    fn cancel_req(reason: &str) -> StockInStatusReq {
        StockInStatusReq {
            status: Some("CANCELLED".to_string()),
            reason: Some(reason.to_string()),
            serials: Vec::new(),
        }
    }

    fn req_with_date(items: Vec<StockInItemReq>, date: NaiveDate) -> CreateStockInReq {
        let mut r = req(items);
        r.in_date = Some(date);
        r
    }

    /// 建单 + 过账，返回收货单 id（过账失败直接 panic，测试里好定位）。
    async fn post_new(lines: Vec<StockInItemReq>) -> Uuid {
        let id = create_stock_in(&req(lines), None).await.unwrap();
        change_stock_in_status(id, &post_req(), None)
            .await
            .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));
        id
    }

    /// 取凭证（真凭证，非红字）。
    async fn voucher_of(stock_in_id: Uuid) -> VoucherRow {
        find_voucher_by_source(db(), SOURCE_STOCK_IN, stock_in_id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("收货单 {stock_in_id} 没有生成凭证"))
    }

    async fn payable_of(stock_in_id: Uuid) -> payable::PayableRow {
        payable::find_by_source(db(), SOURCE_STOCK_IN, stock_in_id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("收货单 {stock_in_id} 没有生成应付单"))
    }

    // ---------- ① 应付 ----------

    /// ① 过账生成应付：金额 = 价税合计（226），status=OPEN，未付 = 全额。
    #[tokio::test]
    async fn post_creates_open_payable_with_with_tax_amount() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let row = payable_of(id).await;
        assert_eq!(
            row.amount,
            dec("226"),
            "应付金额 = 价税合计（不是不含税 200）"
        );
        assert_eq!(row.paid_amount, Decimal::ZERO);
        assert_eq!(row.unpaid_amount, dec("226"));
        assert_eq!(row.status, payable::STATUS_OPEN);
        assert_eq!(row.source_type, "STOCK_IN");
        assert_eq!(row.source_id, id);
        assert!(row.payable_no.starts_with("AP-"), "{}", row.payable_no);
        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(
            row.period_code,
            period_of(db(), header.in_date).await.unwrap(),
            "期间跟收货日期走"
        );
        assert_eq!(row.credit_days, 30, "账期快照来自供应商档案");
        assert_eq!(
            row.due_date,
            header.in_date + Duration::days(30),
            "到期日 = 入库日期 + 账期"
        );
    }

    // ---------- ② 凭证状态 / ③ 分录 / ④ 平衡 ----------

    /// ② 过账生成的凭证直接 `POSTED`（不是 DRAFT），且是自动凭证。
    #[tokio::test]
    async fn post_creates_posted_auto_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let voucher = voucher_of(id).await;
        assert_eq!(voucher.status, STATUS_POSTED, "自动凭证必须直接已记账");
        assert_eq!(voucher.voucher_type, VOUCHER_TYPE_AUTO);
        assert_eq!(voucher.is_red, IS_RED_NO);
        assert_eq!(voucher.voucher_word, WORD_DEFAULT);
        assert!(
            voucher.voucher_no.starts_with("PZ-"),
            "{}",
            voucher.voucher_no
        );
        assert_eq!(
            voucher.source_no.as_deref(),
            Some(id_to_in_no(id).await.as_str())
        );
        assert_eq!(voucher.entry_count, 3);
    }

    async fn id_to_in_no(id: Uuid) -> String {
        stockin_model::find_stock_in(id)
            .await
            .unwrap()
            .expect("收货单不存在")
            .in_no
    }

    /// ③ 分录 3 行：借存货(200) / 借进项税(26) / 贷应付(226)，科目编码逐一核对。
    #[tokio::test]
    async fn post_creates_three_entries_debit_inventory_debit_tax_credit_payable() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let voucher = voucher_of(id).await;
        let entries = entries_of(db(), voucher.id).await.unwrap();
        assert_eq!(entries.len(), 3, "借存货 + 借进项税 + 贷应付");

        let inventory = &entries[0];
        assert_eq!(inventory.line_no, 1);
        assert_eq!(inventory.account.code, DEFAULT_INVENTORY_CODE);
        assert_eq!(inventory.account.name, "库存商品");
        assert_eq!(inventory.debit, dec("200"));
        assert_eq!(inventory.credit, Decimal::ZERO);
        assert_eq!(
            inventory.aux_warehouse_id,
            Some(Uuid::parse_str(WAREHOUSE).unwrap())
        );

        let tax = &entries[1];
        assert_eq!(tax.line_no, 2);
        assert_eq!(tax.account.code, DEFAULT_TAX_CODE);
        assert_eq!(tax.debit, dec("26"));
        assert_eq!(tax.credit, Decimal::ZERO);

        let ap = &entries[2];
        assert_eq!(ap.line_no, 3);
        assert_eq!(ap.account.code, DEFAULT_PAYABLE_CODE);
        assert_eq!(ap.debit, Decimal::ZERO);
        assert_eq!(ap.credit, dec("226"));
        assert_eq!(ap.aux_supplier_id, Some(Uuid::parse_str(SUPPLIER).unwrap()));
    }

    /// ④ 借贷平衡：主表两列相等，且等于分录两列之和。
    #[tokio::test]
    async fn voucher_totals_are_balanced() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![
            line_req(MATERIAL_A, "100", "2", "13"),
            line_req(MATERIAL_B, "3", "10.5", "13"),
        ])
        .await;
        let voucher = voucher_of(id).await;
        assert_eq!(voucher.total_debit, voucher.total_credit);
        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(
            voucher.total_debit, header.total_with_tax,
            "借贷合计 = 单头价税合计（金额取自单据，不重算）"
        );
        let entries = entries_of(db(), voucher.id).await.unwrap();
        let (debit, credit) = totals_of(
            &entries
                .iter()
                .map(|row| EntryLine {
                    line_no: row.line_no,
                    summary: row.summary.clone(),
                    account: row.account.clone(),
                    debit: row.debit,
                    credit: row.credit,
                    aux_supplier_id: None,
                    aux_material_id: None,
                    aux_warehouse_id: None,
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(debit, voucher.total_debit);
        assert_eq!(credit, voucher.total_credit);
    }

    /// ⑤ 税率 0：只有 2 行，不产生进项税行。
    #[tokio::test]
    async fn zero_tax_receipt_has_two_entries() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "10", "2", "0")]).await;
        let voucher = voucher_of(id).await;
        let entries = entries_of(db(), voucher.id).await.unwrap();
        assert_eq!(entries.len(), 2, "税率 0 → 无进项税行");
        assert!(
            entries
                .iter()
                .all(|row| row.account.code != DEFAULT_TAX_CODE),
            "不应出现进项税科目"
        );
        assert_eq!(voucher.total_debit, dec("20"));
        assert_eq!(voucher.entry_count, 2);
    }

    /// ⑥ 金额口径：存货用不含税、应付用价税合计。
    #[tokio::test]
    async fn amount_basis_inventory_excludes_tax_payable_includes_tax() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "7", "1.5", "13")]).await;
        let voucher = voucher_of(id).await;
        let entries = entries_of(db(), voucher.id).await.unwrap();
        let inventory = entries
            .iter()
            .find(|row| row.account.code == DEFAULT_INVENTORY_CODE)
            .unwrap();
        let tax = entries
            .iter()
            .find(|row| row.account.code == DEFAULT_TAX_CODE)
            .unwrap();
        let ap = entries
            .iter()
            .find(|row| row.account.code == DEFAULT_PAYABLE_CODE)
            .unwrap();
        // 金额一律等于单头三列（7 × 1.5 = 10.5；税额由建单侧算，本模块不重算）
        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(inventory.debit, header.total_amount);
        assert_eq!(tax.debit, header.total_tax);
        assert_eq!(ap.credit, header.total_with_tax);
        assert_eq!(inventory.debit, dec("10.5"), "存货 = 不含税金额");
        assert_eq!(
            ap.credit,
            inventory.debit + tax.debit,
            "应付 = 存货 + 进项税"
        );
        let payable_row = payable_of(id).await;
        assert_eq!(payable_row.amount, ap.credit, "应付金额 = 贷方合计");
    }

    // ---------- ⑦ 回写 / ⑧ 期间 ----------

    /// ⑦ 回写 `stock_in.voucher_id` / `payable_id`。
    #[tokio::test]
    async fn post_writes_back_voucher_and_payable_ids() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "1", "1", "0")]).await;
        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        let voucher = voucher_of(id).await;
        let payable_row = payable_of(id).await;
        assert_eq!(header.voucher_id, Some(voucher.id));
        assert_eq!(header.payable_id, Some(payable_row.id));
        assert_eq!(
            payable_row.voucher_id,
            Some(voucher.id),
            "应付单也要指向同一张凭证"
        );
    }

    /// ⑧ 凭证 period = 收货日期所属会计期间（落在未开期间时退化成 yyyy-MM）。
    #[tokio::test]
    async fn voucher_period_follows_stock_in_date() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 2026-10 在 biz_accounting_period 里是 OPEN 期间 → 按区间命中
        let id = create_stock_in(
            &req_with_date(
                vec![line_req(MATERIAL_A, "1", "1", "0")],
                NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
            ),
            None,
        )
        .await
        .unwrap();
        change_stock_in_status(id, &post_req(), None).await.unwrap();
        assert_eq!(voucher_of(id).await.period, "2026-10");
        assert_eq!(payable_of(id).await.period_code, "2026-10");

        // 2025-03 没有期间记录 → 退化 yyyy-MM，不阻断过账
        let id2 = create_stock_in(
            &req_with_date(
                vec![line_req(MATERIAL_A, "1", "1", "0")],
                NaiveDate::from_ymd_opt(2025, 3, 15).unwrap(),
            ),
            None,
        )
        .await
        .unwrap();
        change_stock_in_status(id2, &post_req(), None)
            .await
            .unwrap();
        assert_eq!(voucher_of(id2).await.period, "2025-03");
    }

    // ---------- ⑨ 规则优先 / ⑩ 默认科目 ----------

    /// ⑨ 科目映射：优先用 `biz_voucher_rule` 的规则（把借方改到测试科目）。
    #[tokio::test]
    async fn account_mapping_uses_voucher_rule() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        exec(&format!(
            "UPDATE biz_voucher_rule SET debit_account_id = '{TEST_ACCOUNT}'::uuid \
              WHERE rule_code = 'PURCHASE_IN_DEFAULT'"
        ))
        .await;
        let id = post_new(vec![line_req(MATERIAL_A, "10", "2", "0")]).await;
        let entries = entries_of(db(), voucher_of(id).await.id).await.unwrap();
        assert_eq!(
            entries[0].account.code, "P2C1.1405",
            "规则里的借方科目必须优先于默认 1405"
        );
        assert_eq!(entries[1].account.code, DEFAULT_PAYABLE_CODE);
    }

    /// ⑩ 找不到规则 → 用默认科目（1405 / 2221.01 / 2202）。
    #[tokio::test]
    async fn missing_rule_falls_back_to_default_accounts() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        exec(
            "UPDATE biz_voucher_rule SET deleted_at = now() WHERE rule_code = 'PURCHASE_IN_DEFAULT'",
        )
        .await;
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let entries = entries_of(db(), voucher_of(id).await.id).await.unwrap();
        let codes: Vec<&str> = entries
            .iter()
            .map(|row| row.account.code.as_str())
            .collect();
        assert_eq!(
            codes,
            vec![
                DEFAULT_INVENTORY_CODE,
                DEFAULT_TAX_CODE,
                DEFAULT_PAYABLE_CODE
            ]
        );
    }

    /// ⑩b 规则与物料科目都没有 → 供应商 / 物料档案里的科目优先于硬编码默认值。
    #[tokio::test]
    async fn material_account_is_used_when_rule_missing() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        exec(
            "UPDATE biz_voucher_rule SET deleted_at = now() WHERE rule_code = 'PURCHASE_IN_DEFAULT'",
        )
        .await;
        exec(&format!(
            "UPDATE biz_material SET inventory_account_id = '{TEST_ACCOUNT}'::uuid \
              WHERE id = '{MATERIAL_B}'::uuid"
        ))
        .await;
        let id = post_new(vec![line_req(MATERIAL_B, "10", "2", "0")]).await;
        let entries = entries_of(db(), voucher_of(id).await.id).await.unwrap();
        assert_eq!(
            entries[0].account.code, "P2C1.1405",
            "物料配的存货科目优先于 1405"
        );
    }

    // ---------- ⑪⑫⑬ 红冲 ----------

    /// ⑪ 作废 → 生成红字凭证；借贷互换 ⇒ 净效果等于原凭证取负。
    #[tokio::test]
    async fn cancel_creates_red_voucher_with_negative_net_effect() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let original = voucher_of(id).await;
        change_stock_in_status(id, &cancel_req("重复入库"), None)
            .await
            .unwrap();

        // 红字凭证 = 同一来源、is_red=1 的另一张
        let red = db()
            .query_first_sql(
                format!(
                    "SELECT {VOUCHER_COLUMNS} FROM {TABLE} \
                      WHERE source_id = ?::uuid AND is_red = 1 AND deleted_at IS NULL \
                      ORDER BY created_at DESC LIMIT 1"
                ),
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .expect("作废必须生成红字凭证");
        let red = VoucherRow::from_row(&red).unwrap();
        assert_eq!(red.status, STATUS_POSTED);
        assert_eq!(red.reverse_of, Some(original.id));
        assert_eq!(
            red.total_debit, original.total_debit,
            "红字凭证金额与原凭证等额"
        );

        let red_entries = entries_of(db(), red.id).await.unwrap();
        assert_eq!(red_entries.len(), 3);
        // 借贷互换：原来的借方(1405/2221.01) 变成贷方，原来的贷方(2202) 变成借方
        let debit_codes: Vec<&str> = red_entries
            .iter()
            .filter(|row| row.debit > Decimal::ZERO)
            .map(|row| row.account.code.as_str())
            .collect();
        assert_eq!(
            debit_codes,
            vec![DEFAULT_PAYABLE_CODE],
            "红字凭证借方 = 原贷方"
        );
        let credit_total: Decimal = red_entries.iter().map(|row| row.credit).sum();
        assert_eq!(credit_total, dec("226"));

        // 净效果：Σ借 − Σ贷 = −226（等价于「金额取负」）
        let net = red.total_debit - red.total_credit;
        assert_eq!(net, Decimal::ZERO, "红字凭证自身仍然借贷平衡");
        let (rd, rc) = totals_of(
            &red_entries
                .iter()
                .map(|row| EntryLine {
                    line_no: row.line_no,
                    summary: row.summary.clone(),
                    account: row.account.clone(),
                    debit: row.debit,
                    credit: row.credit,
                    aux_supplier_id: None,
                    aux_material_id: None,
                    aux_warehouse_id: None,
                })
                .collect::<Vec<_>>(),
        );
        // 原凭证 + 红字凭证合起来：每个科目净额为 0（蓝字 200 借 / 红字 200 贷）
        let original_entries = entries_of(db(), original.id).await.unwrap();
        let combine = |code: &str| -> Decimal {
            let d1: Decimal = original_entries
                .iter()
                .filter(|r| r.account.code == code)
                .map(|r| r.debit - r.credit)
                .sum();
            let d2: Decimal = red_entries
                .iter()
                .filter(|r| r.account.code == code)
                .map(|r| r.debit - r.credit)
                .sum();
            d1 + d2
        };
        assert_eq!(combine(DEFAULT_INVENTORY_CODE), Decimal::ZERO);
        assert_eq!(combine(DEFAULT_TAX_CODE), Decimal::ZERO);
        assert_eq!(combine(DEFAULT_PAYABLE_CODE), Decimal::ZERO);
        assert_eq!(rd - rc, Decimal::ZERO);
    }

    /// ⑫ 红字凭证 `reverse_of` 指向原凭证；原凭证 `reversed_by` 指向红字凭证、状态仍 POSTED。
    #[tokio::test]
    async fn red_voucher_links_back_to_original() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "5", "2", "0")]).await;
        let original = voucher_of(id).await;
        change_stock_in_status(id, &cancel_req("供应商取消"), None)
            .await
            .unwrap();

        let refreshed = find_voucher(db(), original.id).await.unwrap().unwrap();
        assert_eq!(
            refreshed.status, STATUS_POSTED,
            "原凭证状态保持 POSTED（卡面 §五）"
        );
        let red_id = refreshed.reversed_by.expect("原凭证必须打 reversed_by");
        let red = find_voucher(db(), red_id).await.unwrap().unwrap();
        assert_eq!(red.reverse_of, Some(original.id));
        assert_eq!(red.is_red, IS_RED_YES);
        assert_eq!(red.source_id, Some(id));
        // 重复红冲必须被拒（同一事务语义下不该出现两张红字凭证）
        let err = reverse_for_stock_in(
            db(),
            &stockin_model::find_stock_in(id).await.unwrap().unwrap(),
            Some("再来一次"),
            None,
            Utc::now(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, DbError::InvalidValue(ref msg) if msg.contains("已经红冲")),
            "{err:?}"
        );
    }

    /// ⑬ 作废 → 应付单 `CANCELLED`。
    #[tokio::test]
    async fn cancel_marks_payable_cancelled() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "10", "2", "13")]).await;
        assert_eq!(payable_of(id).await.status, payable::STATUS_OPEN);
        change_stock_in_status(id, &cancel_req("质检不合格退货"), None)
            .await
            .unwrap();
        let row = payable_of(id).await;
        assert_eq!(row.status, payable::STATUS_CANCELLED);
        assert_eq!(row.amount, dec("22.6"), "作废不改金额，只改状态");
    }

    // ---------- ⑭ 事务性 / ⑮ DB CHECK ----------

    /// ⑭ 事务性：凭证生成失败 → 库存也不变（整单回滚，收货单回到 DRAFT）。
    #[tokio::test]
    async fn voucher_failure_rolls_back_inventory() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 停用进项税科目 → 解析科目失败 → create_auto 报错 → 整单回滚
        exec("UPDATE biz_account SET status = 0 WHERE account_code = '2221.01'").await;
        let id = create_stock_in(&req(vec![line_req(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        let err = change_stock_in_status(id, &post_req(), None)
            .await
            .expect_err("进项税科目停用时过账必须失败");
        assert!(err.msg.contains("科目"), "{}", err.msg);

        // 库存、流水、凭证、应付：一条都不能留
        let qty: Decimal = db()
            .scalar_with_params(
                "SELECT COALESCE(SUM(qty),0) FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![
                    Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
                    Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(qty, Decimal::ZERO, "凭证失败 → 库存回滚");
        let txns: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(txns, 0, "凭证失败 → 库存流水回滚");
        let vouchers: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(vouchers, 0);
        let payables: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_payable WHERE source_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(payables, 0);

        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(header.status, "DRAFT", "过账失败必须回到 DRAFT（可重试）");
        assert_eq!(header.posted_at, None);

        // 恢复科目后可正常过账（同一张单重试成功）
        exec("UPDATE biz_account SET status = 1 WHERE account_code = '2221.01'").await;
        change_stock_in_status(id, &post_req(), None).await.unwrap();
        assert_eq!(voucher_of(id).await.entry_count, 3);
    }

    /// ⑮ 借贷不平衡的脏数据被 DB CHECK 拦住（绕过引擎直接写表也不行）。
    #[tokio::test]
    async fn db_rejects_unbalanced_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = db()
            .execute_with_params(
                "INSERT INTO biz_voucher (voucher_no, period, voucher_date, total_debit, total_credit, entry_count)
                 VALUES ('__P2C1_UNBALANCED__', '2026-10', DATE '2026-10-01', 100, 99, 2)",
                vec![],
            )
            .await
            .expect_err("借贷不平衡必须被 ck_biz_voucher_balance 拦住");
        assert!(
            matches!(err, DbError::CheckViolation { .. }),
            "期望 CheckViolation，实际 {err:?}"
        );
    }

    /// ⑮b 分录借贷双边同时有值被 DB CHECK 拦住。
    #[tokio::test]
    async fn db_rejects_entry_with_both_sides() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "1", "1", "0")]).await;
        let voucher = voucher_of(id).await;
        let err = db()
            .execute_with_params(
                &format!(
                    "INSERT INTO biz_voucher_entry \
                       (voucher_id, line_no, summary, account_id, account_code, account_name, \
                        debit_amount, credit_amount) \
                     VALUES ('{}'::uuid, 99, '__P2C1_BOTH__', gen_random_uuid(), 'X', 'X', 10, 10)",
                    voucher.id
                ),
                vec![],
            )
            .await
            .expect_err("双边分录必须被 ck_biz_voucher_entry_side 拦住");
        assert!(
            matches!(err, DbError::CheckViolation { .. }),
            "期望 CheckViolation，实际 {err:?}"
        );
    }

    // ---------- 纯逻辑 ----------

    /// 凭证号格式：`PZ-yyyyMMdd-000001`（6 位补零，周期键按北京时间）。
    #[test]
    fn voucher_no_format_is_pz_prefix_with_six_digits() {
        assert_eq!(format_doc_no("PZ", "20261031", 1), "PZ-20261031-000001");
        assert_eq!(
            format_doc_no("PZ", "20261031", 123456),
            "PZ-20261031-123456"
        );
        assert_eq!(format_doc_no("AP", "20261031", 7), "AP-20261031-000007");
        assert_eq!(VOUCHER_SEQ.prefix, "PZ");
        assert_eq!(VOUCHER_SEQ.seq_type, "PZ");
    }

    /// 纯逻辑：红字分录 = 借贷互换 + 行号重排 + 摘要加前缀。
    #[test]
    fn reversed_lines_swap_sides_and_keep_amounts() {
        let accounts = AccountSet {
            inventory: AccountRef {
                id: Uuid::nil(),
                code: "1405".to_string(),
                name: "库存商品".to_string(),
            },
            payable: AccountRef {
                id: Uuid::nil(),
                code: "2202".to_string(),
                name: "应付账款".to_string(),
            },
            tax: None,
        };
        let lines = purchase_in_lines(
            &accounts,
            VoucherAmounts {
                amount: dec("200"),
                tax: Decimal::ZERO,
                with_tax: dec("200"),
            },
            "采购入库 IN-1",
            LineContext {
                warehouse_id: None,
                material_id: None,
                supplier_id: None,
            },
        );
        assert_eq!(lines.len(), 2);
        let original: Vec<EntryRow> = lines
            .iter()
            .map(|line| EntryRow {
                id: Uuid::nil(),
                voucher_id: Uuid::nil(),
                line_no: line.line_no,
                summary: line.summary.clone(),
                account: line.account.clone(),
                debit: line.debit,
                credit: line.credit,
                aux_supplier_id: line.aux_supplier_id,
                aux_material_id: line.aux_material_id,
                aux_warehouse_id: line.aux_warehouse_id,
            })
            .collect();
        let red = reversed_lines(&original, Some("退货"));
        assert_eq!(red[0].debit, Decimal::ZERO);
        assert_eq!(red[0].credit, dec("200"));
        assert_eq!(red[1].debit, dec("200"));
        assert_eq!(red[1].credit, Decimal::ZERO);
        assert!(red[0].summary.starts_with("红冲（退货）"));
        assert_eq!(red[1].line_no, 2);
    }
}
