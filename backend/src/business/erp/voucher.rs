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
/// 凭证类型：年度损益结转（ERP-R4，一个会计年度一张）。
///
/// 白名单在库层（`ck_biz_voucher_type`，V0190 扩成 `AUTO|MANUAL|OPENING|YEAR_END`），
/// 一个期间最多一张（部分唯一索引 `uk_biz_voucher_year_end_period`）。
///
/// **它不进「本期发生额」**（`report::load_accounts` 与 `OPENING` 一起被排除）：
/// 结转分录把 5xxx / 6xxx 结平，若算进损益发生额，年结之后的利润表会把整年的
/// 收入费用对冲成 0 —— 那不是「这一年的业绩」，是账务的内部搬运。
pub const VOUCHER_TYPE_YEAR_END: &str = "YEAR_END";
/// 凭证来源类型：年度损益结转（ERP-R4）。
pub const SOURCE_TYPE_YEAR_END: &str = "YEAR_END";
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
/// 科目编码：待处理财产损溢（无来源入库的贷方、报废损失的借方）。
///
/// 由 `V0160__erp_serial_init_accounts` 补种（ADR §6 R2）：全库原有 5 个科目里没有它，
/// 缺了凭证就写不出来 ⇒ 成本账一加金额，`erp_invariants.py` 的 B2（BLOCKER）必红。
pub const ACCOUNT_PENDING_GAIN_LOSS: &str = "1901";
/// 科目编码：主营业务成本（序列号出库结转存货成本的借方）。
pub const ACCOUNT_MAIN_COST: &str = "6401";

/// 凭证来源类型：序列号批量生成（= 无来源入库 / 期初）。
pub const SOURCE_SERIAL_BATCH: &str = "SERIAL_BATCH";
/// 凭证来源类型：序列号出库（与流水 `biz_type` 同名，凭证↔流水可按号对账）。
pub const SOURCE_SERIAL_OUT: &str = "SERIAL_OUT";
/// 凭证来源类型：在库序列号报废（同上）。
pub const SOURCE_SERIAL_SCRAP: &str = "SERIAL_SCRAP";
/// 凭证来源类型：出库单（领用 / 报废 / 盘亏 / 其他，ARCH-3 DC-13）。
///
/// 与 `biz_stock_out` 一一对应：一张出库单最多一张蓝字凭证（作废后再加一张红字）。
/// `biz_voucher.source_type` 是 `VARCHAR(24)` 且无 CHECK 白名单，新增取值不需要迁移。
pub const SOURCE_STOCK_OUT: &str = "STOCK_OUT";

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
    resolve_rule(ex, row).await
}

/// 按 `rule_code`（= 配置里的「凭证模板」）取启用的映射规则。
///
/// 【D3】联动动作 `GENERATE_VOUCHER` 的 `template_code` 参数走这条：
/// 与 `biz_type` 同表同栏位，只是换一个查询键 —— **不新增第二套映射表**。
async fn load_rule_by_code(
    ex: &impl SqlExecutor,
    rule_code: &str,
) -> DbResult<Option<ResolvedRule>> {
    // SCOPE-EXEMPT: 同上。
    let row = ex
        .query_first_sql(
            "SELECT debit_account_id, credit_account_id, tax_account_id \
               FROM biz_voucher_rule \
              WHERE rule_code = ? AND is_active = 1 AND deleted_at IS NULL \
              ORDER BY created_at ASC LIMIT 1"
                .to_string(),
            vec![Value::Text(rule_code.to_string())],
        )
        .await?;
    resolve_rule(ex, row).await
}

/// 把一行 `biz_voucher_rule` 解析成三个科目（`None` = 规则不存在）。
///
/// `load_rule`（按 biz_type）与 `load_rule_by_code`（按模板码）共用本函数：
/// 「规则 → 科目」的解析口径只能有一处，否则模板路径与默认路径的科目会分叉。
async fn resolve_rule(
    ex: &impl SqlExecutor,
    row: Option<nik::prelude::Row>,
) -> DbResult<Option<ResolvedRule>> {
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
    resolve_accounts_with(ex, rule, material_id, supplier_id).await
}

/// 科目解析的**唯一实现**（规则已取好，只做「规则 → 物料 → 默认」三层退化）。
///
/// `rule = None` 表示没有可用映射规则（按 `biz_type` 或模板码都没取到）——
/// 这时候走物料 / 供应商 / 默认科目退化，与拆分前逐字一致。
async fn resolve_accounts_with(
    ex: &impl SqlExecutor,
    rule: Option<ResolvedRule>,
    material_id: Option<Uuid>,
    supplier_id: Option<Uuid>,
) -> DbResult<AccountSet> {
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
     pending_reverse_instance_id, reverse_reason, \
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
    /// 红冲审批实例（P-FLOW-3B）：有值 = 正在审批红冲（`status` 仍是 `POSTED`，报表口径不变）。
    pub pending_reverse_instance_id: Option<Uuid>,
    /// 红冲原因（P-FLOW-3B）：提交审批时填写，通过后成为红字凭证的分录摘要。
    pub reverse_reason: Option<String>,
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
            pending_reverse_instance_id: row.get_typed("pending_reverse_instance_id")?,
            reverse_reason: row.get_typed("reverse_reason")?,
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

/// 一张业务单据生成凭证所需的**全部上下文**（取数结果，不含算法）。
///
/// # 【D3】为什么要有这个结构（「先抽公共函数，再接管」的那一步）
///
/// 「取数」在凭证链路上必须只有一份口径：金额只从单头拿（**不重算**）、
/// 摘要只由「单号 + 价税合计」拼、存货辅助核算取首个物料、凭证日期取单据业务日期。
/// 收货过账（`stockin_handler::post_in_tx` ⑦）与联动动作 `GENERATE_VOUCHER`
/// 如果各写一遍取数，同一张单会按「哪条路径先跑」生成两张不一样的凭证 —— 那就是双真相。
///
/// 所以取数只有 [`VoucherBizCtx::of_stock_in`] 一处：
/// - 过账路径用它把**内存里已经加锁的单头**转成上下文（不再查库）；
/// - 配置路径用 `stockin_model::voucher_biz_ctx` 从库里取单头，再走**同一个**函数。
///
/// 取号 / 借贷平衡 / 落库仍然全部在 [`create_by_rule`] → [`post_voucher`]，
/// 本结构只承载数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherBizCtx {
    /// 来源单据类型（`STOCK_IN` / `PAYMENT` / …）。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: String,
    /// 凭证日期（默认 = 单据业务日期；配置可用 `account_date` 覆盖）。
    pub voucher_date: NaiveDate,
    /// 三个金额（取自单头）。
    pub amounts: VoucherAmounts,
    /// 凭证摘要（整单级）。
    pub summary: String,
    /// 映射规则用的业务类型（`PURCHASE_IN`）。
    pub biz_type: String,
    /// 存货科目退化用的物料。
    pub material_id: Option<Uuid>,
    /// 应付科目退化用的供应商。
    pub supplier_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub warehouse_id: Option<Uuid>,
}

impl VoucherBizCtx {
    /// **取数的唯一实现**：收货单单头 → 凭证上下文。
    ///
    /// 与 `create_for_stock_in` 拆分前逐字相同（金额 `round_dp(4)`、摘要格式、
    /// `biz_type = PURCHASE_IN`、辅助核算取首个物料 / 供应商 / 仓库）。
    pub fn of_stock_in(header: &StockInRow, first_material: Option<Uuid>) -> Self {
        Self {
            source_type: SOURCE_STOCK_IN.to_string(),
            source_id: header.id,
            source_no: header.in_no.clone(),
            voucher_date: header.in_date,
            amounts: VoucherAmounts::of_stock_in(header),
            summary: format!(
                "采购入库 {} 价税合计 {}",
                header.in_no, header.total_with_tax
            ),
            biz_type: BIZ_TYPE_PURCHASE_IN.to_string(),
            material_id: first_material,
            supplier_id: header.supplier_id,
            warehouse_id: Some(header.warehouse_id),
        }
    }

    /// 换一个记账日期（配置参数 `account_date` 的覆盖口径：只动日期，不动金额与摘要）。
    pub fn with_voucher_date(&self, date: NaiveDate) -> Self {
        let mut ctx = self.clone();
        ctx.voucher_date = date;
        ctx
    }
}

/// 模板（`biz_voucher_rule.rule_code`）是否启用：联动动作的第 3 层校验用。
///
/// 停用的模板 = 「target 不存在」→ 调用方跳过该动作（不是失败，更不能凭它生成凭证）。
pub async fn template_active(ex: &impl SqlExecutor, rule_code: &str) -> DbResult<bool> {
    Ok(load_rule_by_code(ex, rule_code).await?.is_some())
}

/// **按模板 / 上下文驱动的唯一凭证生成实现**（取数 → 取号 → 平衡校验 → 落库）。
///
/// # 参数
///
/// - `template_code`：`Some` = 用 `biz_voucher_rule.rule_code` 指定映射模板（配置的
///   `template_code` 参数）；`None` = 按上下文里的 `biz_type` 取默认模板 ——
///   与拆分前 `create_auto` 的行为**逐字相同**。
/// - `ctx`：取数结果（[`VoucherBizCtx`]）。
///
/// # 事务
///
/// 吃 `&impl SqlExecutor`（`Db` / `Transaction`），**不 begin / 不 commit**；
/// 调用方（过账事务 / 联动引擎的 `InTx` 动作）负责收口。
///
/// # 谁在调用
///
/// - [`create_for_stock_in`]（薄封装，收货过账 ⑦ 的旧路径）；
/// - `linkage::actions::generate_voucher`（配置路径，第 3 步接管）。
///
/// 两条路走同一个函数 ⇒ 凭证张数、编号、借贷行、金额不可能分叉。
pub async fn create_by_rule(
    ex: &impl SqlExecutor,
    template_code: Option<&str>,
    ctx: &VoucherBizCtx,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let rule = match template_code {
        Some(code) => Some(load_rule_by_code(ex, code).await?.ok_or_else(|| {
            DbError::InvalidValue(format!(
                "凭证生成失败：模板 {code} 未启用（biz_voucher_rule 里没有这条存活规则），\
                 单据 {} 无法生成凭证",
                ctx.source_no
            ))
        })?),
        None => load_rule(ex, &ctx.biz_type).await?,
    };
    let accounts = resolve_accounts_with(ex, rule, ctx.material_id, ctx.supplier_id).await?;

    // 税率 > 0 但系统里没有进项税科目 → 拦住（不许静默漏记进项税）
    if ctx.amounts.tax > Decimal::ZERO && accounts.tax.is_none() {
        return Err(account_missing(
            format!("进项税科目 {}", DEFAULT_TAX_CODE).as_str(),
            "本次入库有税额，但系统里没有可用的进项税科目",
        ));
    }

    let lines = purchase_in_lines(
        &accounts,
        ctx.amounts,
        &ctx.summary,
        LineContext {
            warehouse_id: ctx.warehouse_id,
            material_id: ctx.material_id,
            supplier_id: ctx.supplier_id,
        },
    );
    let (debit, credit) = totals_of(&lines);
    if lines.len() < 2 {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：单据 {} 的金额全为 0（分录只有 {} 行），会计上不构成一张凭证",
            ctx.source_no,
            lines.len()
        )));
    }
    if debit != credit {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：借贷不平衡（借 {debit} / 贷 {credit}），单据 {}",
            ctx.source_no
        )));
    }

    post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: &ctx.source_type,
            source_id: Some(ctx.source_id),
            source_no: Some(&ctx.source_no),
            voucher_date: ctx.voucher_date,
            summary: ctx.summary.clone(),
            lines,
            remark: None,
            red_of: None,
        },
        operator,
        now,
    )
    .await
}

// ==================== 并行对比：凭证快照 ====================

/// 一行分录的快照（对比用，只留「应该相同」的字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherLineSnapshot {
    /// 行号。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目编码（名称可能因主数据改名而变，编码才是账的口径）。
    pub account_code: String,
    /// 借方。
    pub debit: Decimal,
    /// 贷方。
    pub credit: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<Uuid>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<Uuid>,
}

/// 一张凭证的逐字段快照 —— 「旧路径发的凭证」与「配置发的凭证」的对比物。
///
/// 【D3】SHADOW 阶段的判据就是它：[`compare_snapshots`] 返回空 = 逐字段一致。
/// 字段选择口径：凡是会在报表 / 下钻 / 审计里被看到的东西都进快照
/// （字号、期间、来源、借贷合计、行数、逐行科目与金额、辅助核算），
/// 只有 id 类主键与时间戳不比（换个事务就必然不同）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherSnapshot {
    /// 同一来源单据下的凭证张数（正常应为 1；红冲后为 2）。
    pub voucher_count: i64,
    /// 凭证字。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 凭证类型（`AUTO`）。
    pub voucher_type: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Option<Uuid>,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 会计期间。
    pub period: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 摘要。
    pub summary: String,
    /// 借方合计。
    pub total_debit: Decimal,
    /// 贷方合计。
    pub total_credit: Decimal,
    /// 分录行数。
    pub entry_count: i32,
    /// 是否红字。
    pub is_red: i16,
    /// 逐行分录。
    pub lines: Vec<VoucherLineSnapshot>,
}

/// 取一张凭证的快照（读主表 + 分录 + 同一来源的凭证张数）。
pub async fn snapshot_of(ex: &impl SqlExecutor, voucher_id: Uuid) -> DbResult<VoucherSnapshot> {
    let Some(row) = find_voucher(ex, voucher_id).await? else {
        return Err(DbError::InvalidValue(format!(
            "凭证快照失败：凭证 {voucher_id} 不存在"
        )));
    };
    let voucher_count = match (row.source_type.as_str(), row.source_id) {
        (source_type, Some(source_id)) => {
            // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）；这里只数同一来源单据的凭证张数。
            ex.scalar_sql(
                format!(
                    "SELECT count(*) FROM {TABLE} \
                      WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL"
                ),
                vec![
                    Value::Text(source_type.to_string()),
                    Value::from(&source_id),
                ],
            )
            .await?
        }
        _ => 1,
    };
    let lines = entries_of(ex, voucher_id)
        .await?
        .into_iter()
        .map(|entry| VoucherLineSnapshot {
            line_no: entry.line_no,
            summary: entry.summary,
            account_code: entry.account.code,
            debit: entry.debit,
            credit: entry.credit,
            aux_supplier_id: entry.aux_supplier_id,
            aux_material_id: entry.aux_material_id,
            aux_warehouse_id: entry.aux_warehouse_id,
        })
        .collect();
    Ok(VoucherSnapshot {
        voucher_count,
        voucher_word: row.voucher_word,
        voucher_no: row.voucher_no,
        voucher_type: row.voucher_type,
        source_type: row.source_type,
        source_id: row.source_id,
        source_no: row.source_no,
        period: row.period,
        voucher_date: row.voucher_date,
        summary: row.summary,
        total_debit: row.total_debit,
        total_credit: row.total_credit,
        entry_count: row.entry_count,
        is_red: row.is_red,
        lines,
    })
}

/// 逐字段对比两张凭证快照，返回**不一致的字段名**（空 = 逐字段一致）。
///
/// 这是 SHADOW 阶段「并行跑一段、对比一致再切换」的判据：日志里 `matched` / `diffs`
/// 是可 grep 的事实，不是口头保证。
pub fn compare_snapshots(
    configured: &VoucherSnapshot,
    legacy: &VoucherSnapshot,
) -> Vec<&'static str> {
    let mut diffs: Vec<&'static str> = Vec::new();
    if configured.voucher_count != legacy.voucher_count {
        diffs.push("voucher_count");
    }
    if configured.voucher_word != legacy.voucher_word {
        diffs.push("voucher_word");
    }
    if configured.voucher_no != legacy.voucher_no {
        diffs.push("voucher_no");
    }
    if configured.voucher_type != legacy.voucher_type {
        diffs.push("voucher_type");
    }
    if configured.source_type != legacy.source_type {
        diffs.push("source_type");
    }
    if configured.source_id != legacy.source_id {
        diffs.push("source_id");
    }
    if configured.source_no != legacy.source_no {
        diffs.push("source_no");
    }
    if configured.period != legacy.period {
        diffs.push("period");
    }
    if configured.voucher_date != legacy.voucher_date {
        diffs.push("voucher_date");
    }
    if configured.summary != legacy.summary {
        diffs.push("summary");
    }
    if configured.total_debit != legacy.total_debit {
        diffs.push("total_debit");
    }
    if configured.total_credit != legacy.total_credit {
        diffs.push("total_credit");
    }
    if configured.entry_count != legacy.entry_count {
        diffs.push("entry_count");
    }
    if configured.is_red != legacy.is_red {
        diffs.push("is_red");
    }
    if configured.lines != legacy.lines {
        diffs.push("lines");
    }
    diffs
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

/// 覆盖一张凭证的全部分录（期初凭证「覆盖式更新」专用）。
///
/// 会计上凭证一经记账不可改，唯一例外是**期初凭证**：同一期间只有一张，重复录入是
/// 「把这张凭证的分录整组换掉」，凭证号保持不变（卡面 §三 硬红线 2）。
/// 删除 + 重写放在同一个函数里，避免调用方只做了一半（删了老分录、新分录没写进去）。
pub async fn replace_entries(
    ex: &impl SqlExecutor,
    voucher_id: Uuid,
    lines: &[EntryLine],
) -> DbResult<()> {
    ex.exec_sql(
        format!("DELETE FROM {ENTRY_TABLE} WHERE voucher_id = ?::uuid"),
        vec![Value::from(&voucher_id)],
    )
    .await?;
    write_entries(ex, voucher_id, lines).await
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
    // ERP-P1（卡面 §一 ②「结账后」）：已结账的期间**只读**。
    // 闸放在这里而不是各业务入口，是因为自动凭证（收货 / 应付 / 付款）与手工凭证
    // 都走 `post_voucher` —— 在唯一收口拦一次，就没有「某条路径忘了查期间」的可能。
    // 语义是 400（参数/状态不允许这次操作），由各调用点的 write_error 翻成
    // 400 + 原文；红冲的 409 前置闸在 `reverse_posted` 里，比这里更早生效。
    if period_status(ex, &period)
        .await?
        .as_deref()
        .is_some_and(|status| status == PERIOD_CLOSED)
    {
        // 唯一的例外：**年末损益结转凭证**（ERP-R4）。
        //
        // 年结的前置条件就是「本年 12 个月全部结账」，所以结转凭证要落的 12 月
        // 必然是 CLOSED —— 这不是绕闸，是设计决议（卡面 §四 ⚠️ 的第二个选项）：
        // 结转凭证的 period 允许写入已结账期间，而白名单在库层
        // （`ck_biz_voucher_type` 只放 AUTO|MANUAL|OPENING|YEAR_END），
        // 只有 `year_end` 模块会传这个类型。反年结会把这张凭证整张删掉，
        // 12 月重新解封前账务是一致的。
        if req.voucher_type != VOUCHER_TYPE_YEAR_END || req.source_type != SOURCE_TYPE_YEAR_END {
            return Err(DbError::InvalidValue(format!(
                "会计期间 {period} 已结账，不能再生成凭证（凭证日期 {}）：\
                 已封账的月份只读，请在下一个未结账的期间做调整（或先反结账）",
                req.voucher_date
            )));
        }
        tracing::info!(
            period = %period,
            "年末损益结转凭证写入已结账期间（YEAR_END 的既定例外）"
        );
    }
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
    //
    // ARCH-3 DC-01：红字凭证的日志 reason 带上**红冲原因**（`红字冲销凭证｜<原因>`）——
    // 红冲没有状态流转可写，"为什么这次红冲没走复核"（显式跳过审批）就靠这段文字留痕；
    // 蓝字凭证仍为 `None`（建单不是业务动作，reason 无意义）。
    let log_reason = match req.red_of {
        None => None,
        Some(_) => {
            let own = req.remark.map(str::trim).filter(|text| !text.is_empty());
            Some(match own {
                Some(text) => format!("红字冲销凭证｜{text}"),
                None => "红字冲销凭证".to_string(),
            })
        }
    };
    state_machine::log_created(
        ex,
        state_machine::ENTITY_VOUCHER,
        voucher_id,
        &voucher_no_text,
        STATUS_POSTED,
        operator,
        log_reason.as_deref(),
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
///
/// # 【D3】薄封装
///
/// 「取数 → 取号 → 借贷平衡 → 落库」已经全部在 [`create_by_rule`]；
/// 本函数只做一件事：把**内存里已经加锁的收货单头**转成 [`VoucherBizCtx`]
/// （走 [`VoucherBizCtx::of_stock_in`]，与配置路径同一份取数），然后调它。
///
/// 留在原地的原因是「调用点语义」：过账路径手上有锁好的单头，不该为此再查一次库；
/// 而配置路径（联动动作）手上只有单据 id，走 `stockin_model::voucher_biz_ctx` 取数。
/// 两条路上的**算法只有一份**。
pub async fn create_for_stock_in(
    ex: &impl SqlExecutor,
    header: &StockInRow,
    first_material: Option<Uuid>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let ctx = VoucherBizCtx::of_stock_in(header, first_material);
    create_by_rule(ex, None, &ctx, operator, now).await
}

// ==================== 写：序列号侧三种「无来源」库存变动 ====================

/// 序列号的三种库存变动（ADR-20261031 §6 R1，规格 v3 §3.10）。
///
/// 三者的共同点是「没有供应商、没有采购单」——借方必须是成本类 / 资产类科目而不是应付，
/// 所以既不能复用 [`create_auto`]（它按 `biz_voucher_rule` 走采购入库映射），
/// 也不该在调用点各写一遍分录：本枚举 + [`create_for_serial_movement`] 是**唯一写入口**。
#[derive(Debug, Clone, Copy)]
pub enum SerialMovement<'a> {
    /// 期初 / 其他入库（批量生成）：借 `1405 库存商品` / 贷 `1901 待处理财产损溢`。
    Inbound {
        /// 物料。
        material_id: Uuid,
        /// 仓库。
        warehouse_id: Uuid,
        /// 本次生成件数（只进摘要，金额由调用方按账上差额传入）。
        count: i64,
    },
    /// 出库（序列号发货）：借 `6401 主营业务成本` / 贷 `1405 库存商品`。
    Outbound {
        /// 物料。
        material_id: Uuid,
        /// 仓库。
        warehouse_id: Uuid,
        /// 序列号（进摘要与来源单号）。
        serial_no: &'a str,
    },
    /// 报废：借 `1901 待处理财产损溢` / 贷 `1405 库存商品`。
    Scrapped {
        /// 物料。
        material_id: Uuid,
        /// 仓库。
        warehouse_id: Uuid,
        /// 序列号（进摘要与来源单号）。
        serial_no: &'a str,
    },
}

/// 序列号库存变动 → 凭证（**同事务**；`amount` 为账上真实差额，不在本函数重算）。
///
/// # 为什么金额必须由调用方传入
///
/// 生成侧 = `apply_cost` 的 `after.amount − before.amount`（该桶的实际增量）；
/// 出库 / 报废侧 = 仓级 `before.amount − after.amount`。两处都是 `numeric(18,4)`，
/// 谁都不该在 Rust 里再乘一次 `N × unit_cost` —— 4 位舍入一错，
/// `erp_invariants.py` 的 B2（容差 `0.0001`）就会报 0.00x 的差。
///
/// # 差额 ≤ 0 直接跳过（不是错误）
///
/// 改判前生成的历史号是零成本（`qty>0 / avg_cost=0 / amount=0`），它们出库 / 报废时
/// 差额恰好为 0。`post_voucher` 在 `借贷合计 <= 0` 时会报错（那是对**单据**的护栏），
/// 所以这里提前 `Ok(None)` 并打 WARN 点名 —— 这是「存量不补账」的过渡口径
/// （规格 v3 §8.2），财务可以按这条日志判断要不要补账。
///
/// # 事务
///
/// 吃 `&impl SqlExecutor`（`Db` / `Transaction`），由调用方开事务；本函数不 `begin`
/// 不 `commit`，也绝不出现「事务里再开事务」。
pub async fn create_for_serial_movement(
    ex: &impl SqlExecutor,
    kind: SerialMovement<'_>,
    doc_id: Uuid,
    doc_no: &str,
    amount: Decimal,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let amount = amount.round_dp(4);
    let (material_id, warehouse_id, source_type, summary, debit_side) = match kind {
        SerialMovement::Inbound {
            material_id,
            warehouse_id,
            count,
        } => (
            material_id,
            warehouse_id,
            SOURCE_SERIAL_BATCH,
            format!("期初入库：序列号批量生成 {count} 件"),
            DebitSide::Inventory,
        ),
        SerialMovement::Outbound {
            material_id,
            warehouse_id,
            serial_no,
        } => (
            material_id,
            warehouse_id,
            SOURCE_SERIAL_OUT,
            format!("序列号出库结转成本：{serial_no}"),
            DebitSide::Counterpart(ACCOUNT_MAIN_COST, "主营业务成本"),
        ),
        SerialMovement::Scrapped {
            material_id,
            warehouse_id,
            serial_no,
        } => (
            material_id,
            warehouse_id,
            SOURCE_SERIAL_SCRAP,
            format!("序列号报废损失：{serial_no}"),
            DebitSide::Counterpart(ACCOUNT_PENDING_GAIN_LOSS, "待处理财产损溢"),
        ),
    };
    if amount <= Decimal::ZERO {
        tracing::warn!(
            material_id = %material_id,
            warehouse_id = %warehouse_id,
            doc_no,
            amount = %amount,
            source_type,
            "序列号变动金额为 0（历史零成本存量，ADR 口径不补账）：跳过凭证生成，只有流水没有分录"
        );
        return Ok(None);
    }

    let inventory = account_required(ex, DEFAULT_INVENTORY_CODE, "库存商品").await?;
    // 存货行带「物料 + 仓库」辅助核算（账上能按仓按料拆开看）；对方科目只带物料 ——
    // 1901 / 6401 是全公司口径的过渡 / 损益科目，挂仓库会把报表拆碎。
    let inventory_aux_warehouse = Some(warehouse_id);
    let lines = match debit_side {
        DebitSide::Inventory => {
            let counterpart =
                account_required(ex, ACCOUNT_PENDING_GAIN_LOSS, "待处理财产损溢").await?;
            vec![
                EntryLine {
                    line_no: 1,
                    summary: summary.clone(),
                    account: inventory,
                    debit: amount,
                    credit: Decimal::ZERO,
                    aux_supplier_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: inventory_aux_warehouse,
                },
                EntryLine {
                    line_no: 2,
                    summary: summary.clone(),
                    account: counterpart,
                    debit: Decimal::ZERO,
                    credit: amount,
                    aux_supplier_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: None,
                },
            ]
        }
        DebitSide::Counterpart(code, name) => {
            let counterpart = account_required(ex, code, name).await?;
            vec![
                EntryLine {
                    line_no: 1,
                    summary: summary.clone(),
                    account: counterpart,
                    debit: amount,
                    credit: Decimal::ZERO,
                    aux_supplier_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: None,
                },
                EntryLine {
                    line_no: 2,
                    summary: summary.clone(),
                    account: inventory,
                    debit: Decimal::ZERO,
                    credit: amount,
                    aux_supplier_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: inventory_aux_warehouse,
                },
            ]
        }
    };

    let voucher_id = post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type,
            source_id: Some(doc_id),
            source_no: Some(doc_no),
            voucher_date: today_cst(now),
            summary,
            lines,
            remark: None,
            red_of: None,
        },
        operator,
        now,
    )
    .await?;
    Ok(Some(voucher_id))
}

/// 借方落在哪一侧（决定分录两行的科目配对）。
#[derive(Debug, Clone, Copy)]
enum DebitSide {
    /// 借存货、贷对方科目（期初入库）。
    Inventory,
    /// 借对方科目、贷存货（出库 / 报废）。
    Counterpart(&'static str, &'static str),
}

// ==================== 写：出库单成本结转凭证（ARCH-3 DC-13） ====================

/// 出库单过账 → 蓝字凭证的入参（一张单**一张**凭证，不按行拆）。
///
/// 打包成结构体是必须的：`post_voucher` 之外还有 9 个业务字段，
/// 摊平会直接撞 clippy 的 `too_many_arguments`（-D warnings 下是硬错）。
#[derive(Debug, Clone)]
pub struct StockOutVoucher<'a> {
    /// 出库单 id（`biz_voucher.source_id`）。
    pub doc_id: Uuid,
    /// 出库单号。
    pub doc_no: &'a str,
    /// 凭证日期（= 出库单的 `out_date`，决定会计期间）。
    pub voucher_date: NaiveDate,
    /// 出库类型（`ISSUE` / `SCRAP` / `LOSS` / `OTHER`，进摘要）。
    pub out_type: &'a str,
    /// 借方科目编码（报废·盘亏 1901 / 其余 6401）。
    pub debit_code: &'a str,
    /// 借方科目名（写进分录摘要，便于人直接看懂）。
    pub debit_name: &'a str,
    /// 第一行物料（`biz_voucher_entry.aux_material_id` 辅助核算用；多行单取首行）。
    pub first_material: Option<Uuid>,
    /// 出库仓库（存货行的 `aux_warehouse_id`：账上能按仓拆开看）。
    pub warehouse_id: Uuid,
    /// **账上真实差额**（Σ 各明细 `biz_inventory.amount` 的变动量），
    /// 由调用方从 `RETURNING` 的 before / after 算出，本函数**绝不重算** `qty × avg`。
    pub amount: Decimal,
    /// 凭证摘要。
    pub summary: String,
}

/// 出库单过账时生成蓝字凭证：借 6401 / 1901，贷 1405。
///
/// # 金额口径（硬红线）
///
/// `amount` 必须是**账上真实差额**（`before.amount − after.amount` 的合计），
/// 不是 `Σ qty × avg_cost`。理由与序列号出库同一条（见
/// [`create_for_serial_movement`]）：多桶 / 4 位舍入下两者不相等，
/// 拿重算值去贷 1405 会让 `erp_invariants.py` 的 B2（库存金额 = 存货科目余额）
/// 报出 0.00x 的差 —— 那是 BLOCKER。
///
/// # 差额 ≤ 0 直接跳过（不是错误）
///
/// 零成本存量（`qty>0 / avg_cost=0 / amount=0`）出库时差额恰好为 0；
/// `post_voucher` 在 `借贷合计 <= 0` 时会报错（那是对**单据**的护栏），
/// 所以这里提前 `Ok(None)` 并打 WARN 点名单据 —— 财务按这条日志判断要不要补账。
///
/// # 事务
///
/// 吃 `&impl SqlExecutor`（`Db` / `Transaction`），由调用方开事务；不 `begin` 不 `commit`。
pub async fn create_for_stock_out(
    ex: &impl SqlExecutor,
    req: &StockOutVoucher<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let amount = req.amount.round_dp(4);
    if amount <= Decimal::ZERO {
        tracing::warn!(
            doc_no = req.doc_no,
            doc_id = %req.doc_id,
            amount = %amount,
            out_type = req.out_type,
            "出库单成本差额为 0（零成本存量 / 账面金额已为 0）：跳过凭证生成，只动库存与流水"
        );
        return Ok(None);
    }

    let inventory = account_required(ex, DEFAULT_INVENTORY_CODE, "库存商品").await?;
    let debit = account_required(ex, req.debit_code, req.debit_name).await?;
    let lines = vec![
        EntryLine {
            line_no: 1,
            summary: req.summary.clone(),
            account: debit,
            debit: amount,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: req.first_material,
            // 借方（成本 / 待处理损溢）是全公司口径的科目，不挂仓库辅助核算。
            aux_warehouse_id: None,
        },
        EntryLine {
            line_no: 2,
            summary: req.summary.clone(),
            account: inventory,
            debit: Decimal::ZERO,
            credit: amount,
            aux_supplier_id: None,
            aux_material_id: req.first_material,
            aux_warehouse_id: Some(req.warehouse_id),
        },
    ];

    let voucher_id = post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: SOURCE_STOCK_OUT,
            source_id: Some(req.doc_id),
            source_no: Some(req.doc_no),
            voucher_date: req.voucher_date,
            summary: req.summary.clone(),
            lines,
            remark: None,
            red_of: None,
        },
        operator,
        now,
    )
    .await?;
    Ok(Some(voucher_id))
}

/// 按**来源单据**找原凭证并红冲（出库单作废用；找不到原凭证 → `Ok(None)`）。
///
/// 与 [`reverse_for_stock_in`] 的区别只有一处：不写死 `SOURCE_STOCK_IN`，
/// 而是把来源类型当参数 —— 这样出库单（`SOURCE_STOCK_OUT`）与将来的退货单
/// 不用各抄一份「找凭证 + 定日期 + 红冲」的胶水代码。
/// 红冲本身**只有一份实现**（[`reverse_posted`]）：原凭证保持 `POSTED` + 写
/// `reversed_by`，新凭证 `is_red = 1`。
///
/// # 返回
///
/// * `Ok(Some(red_id))`：已红冲；
/// * `Ok(None)`：这张单没有凭证（零成本存量 / 从未过账），不阻断作废。
pub async fn reverse_by_source(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
    voucher_date: NaiveDate,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let Some(original) = find_voucher_by_source(ex, source_type, source_id).await? else {
        return Ok(None);
    };
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
    // ★ P-FLOW-3B 反向（L6 关联物级联）：收货单作废要联动红冲这张凭证 →
    //   先终止它在跑的**红冲审批**（否则审批中心留孤儿待办，pending 也永远挂着：
    //   入口会一直 409「已在红冲审批中」，且已红冲的凭证再批一次会撞「不能重复红冲」）。
    //   必须在 reverse_posted 之前做：`cancel_approval_of` 会清 pending，
    //   而 `ck_biz_voucher_pending_reverse` 要求「pending 非空 ⟹ reversed_by 为空」。
    crate::business::erp::voucher_bridge::cancel_approval_of(
        ex,
        original.id,
        &original.voucher_no,
        "收货单作废联动红冲凭证，红冲审批自动终止",
    )
    .await?;
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
    use crate::business::erp::{fixture_hygiene, payable, stockin_model};
    use crate::business::flow::engine;
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
        // 作废审批的发起人（P16 起作废走审批，必须有登录用户；**非超管**，避免自审被 FORBID 挡）
        "INSERT INTO sys_user (id, username, password, real_name)
         VALUES ('b2c10009-0000-4000-8000-000000000001', 'p2c1_keeper', 'p2c1-not-a-real-hash', 'P2C1 仓管')
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

    /// 清掉本模块夹具的**全部痕迹**（状态日志 → 凭证分录 → 凭证 → 应付 → 流水 → 库存 → 收货单），
    /// 并复位上一轮改过的映射规则 / 科目状态。
    ///
    /// 两处调用：每个用例开头的 `setup()`，以及模块里名字排最后的
    /// `zz_teardown_purges_finance_status_logs`（libtest 按名排序，`zz_` 必然收尾）。
    ///
    /// ⚠️ 状态日志必须**在删单据之前**按 id 清：`biz_status_log` 没有外键（`fixture_hygiene`）。
    /// 本模块的收货过账会同时产生 `STOCK_IN` / `PAYABLE` / `VOUCHER` 三类日志，
    /// 漏一类就是每跑一遍全量多 20 行孤儿（本卡实测）。
    async fn cleanup_fixtures() {
        let cleanups = [
            // P16 起作废走审批：先把本夹具的作废审批实例清掉（否则下一轮发起撞「已有在跑的审批单」）
            format!(
                "DELETE FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_CANCEL' AND biz_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            // 状态日志（无外键）：必须在删单据之前按 id 清，否则每轮留一批孤儿（`fixture_hygiene`）
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
                 (SELECT id FROM biz_voucher WHERE source_id IN \
                   (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid) \
                   OR source_no LIKE 'BROKEN-%' OR voucher_no LIKE '__P2C1%')"
            ),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'PAYABLE' AND entity_id IN \
                 (SELECT id FROM biz_payable WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND entity_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
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
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具 + 复位科目映射规则。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup_fixtures().await;
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
            purchase_order_id: None,
            supplier_id: Some(SUPPLIER.to_string()),
            // A4：没有来源采购单 ⇒ 必须显式声明「无源单据」+ 写明来源（AP-5）
            is_sourceless: Some(true),
            warehouse_id: Some(WAREHOUSE.to_string()),
            in_date: None,
            invoice_received: None,
            remark: Some("A4 夹具：无源入库（凭证链路用例）".to_string()),
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
            ..Default::default()
        }
    }

    fn cancel_req(reason: &str) -> StockInStatusReq {
        StockInStatusReq {
            status: Some("CANCELLED".to_string()),
            reason: Some(reason.to_string()),
            serials: Vec::new(),
            ..Default::default()
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

    /// 作废的发起人（**非超管**：审批人来源是 ADMIN，超管发起会撞 `self_approval=FORBID`）。
    const KEEPER: &str = "b2c10009-0000-4000-8000-000000000001";

    /// 作废 = **发起审批 → 复核通过 → 执行**（P16 之后作废不再一步到位）。
    ///
    /// 为什么要这个 helper：直接 `change_stock_in_status(id, &cancel_req(..), None)` 会
    /// `401 发起作废审批需要登录用户`（V0174 起作废先走审批，必须有发起人）。
    /// 这里把三步串起来，走的仍是**生产同一条路径**，所以旧断言（红字凭证 / 应付作废）不变。
    async fn cancel_via_approval(stock_in_id: Uuid, reason: &str) {
        change_stock_in_status(stock_in_id, &cancel_req(reason), Some(uid(KEEPER)))
            .await
            .unwrap_or_else(|err| panic!("发起作废审批失败：{} {}", err.code, err.msg));
        let rows = db()
            .find_with_params(
                "SELECT t.id AS task_id, t.assignee_id AS assignee \
                   FROM biz_flow_task t JOIN biz_flow_instance i ON i.id = t.instance_id \
                  WHERE i.biz_type = 'STOCK_IN_CANCEL' AND i.biz_id = ?::uuid AND t.status = 'PENDING' \
                  ORDER BY t.id LIMIT 1",
                vec![Value::from(&stock_in_id)],
            )
            .await
            .expect("查作废待办失败");
        let row = rows.first().expect("作废审批必须生成待办");
        let task_id: Uuid = row.get_typed("task_id").unwrap();
        let approver: Uuid = row.get_typed("assignee").unwrap();
        let tx = db().begin().await.expect("开审批事务");
        let out = engine::approve(&tx, task_id, approver, Some("单测：同意作废"))
            .await
            .expect("引擎同意失败");
        tx.commit().await.expect("提交审批事务");
        crate::business::erp::stockin_bridge::on_cancel_settled(
            out.instance_id,
            &out.instance_status,
        )
        .await;
    }

    fn uid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
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
        cancel_via_approval(id, "重复入库").await;

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
        cancel_via_approval(id, "供应商取消").await;

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
        cancel_via_approval(id, "质检不合格退货").await;
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

    /// ㉖ 收尾清场：本模块的**科目夹具**（`P2C1.1405`）必须删掉（t_06c528fc）。
    ///
    /// 科目没有分类行，留在共享库里会让 `cash_flow` 的「分类配置覆盖全科目」断言
    /// 在**第二遍**全量时红。`biz_account` 无外键引用，直接删安全。
    #[tokio::test]
    async fn zz_teardown_purges_fixture_accounts() {
        let Some(_guard) = setup().await else {
            return;
        };
        db().execute_with_params(
            "DELETE FROM biz_account WHERE account_code LIKE 'P2C1%'",
            vec![],
        )
        .await
        .expect("清场删夹具科目失败");
        let left: i64 = db()
            .scalar_sql(
                "SELECT count(*) FROM biz_account WHERE account_code LIKE 'P2C1%'".to_string(),
                vec![],
            )
            .await
            .expect("计数失败");
        assert_eq!(left, 0, "夹具科目必须删掉（留着会让第二遍全量变红）");
    }

    /// ㉗ 收尾清场：本模块的**状态日志**（`STOCK_IN` / `PAYABLE` / `VOUCHER`）必须按 id 收干。
    ///
    /// 收货过账会同时写三类日志，而 `biz_status_log` 没有外键：清场里漏掉「删单据前先删
    /// 日志」这 20 行就成孤儿，永远收不回来（本卡实测 `voucher::tests` 每遍 +20 PAYABLE
    /// +20 VOUCHER）。`zz_` 排在本模块最后，是这一块的收口。
    #[tokio::test]
    async fn zz_teardown_purges_finance_status_logs() {
        let Some(_guard) = setup().await else {
            return;
        };
        let stock_in_id = post_new(vec![line_req(MATERIAL_A, "2", "100", "13")]).await;
        let payable = payable_of(stock_in_id).await;
        let voucher = voucher_of(stock_in_id).await;

        let count_logs = |entity_type: &'static str, target: Uuid| async move {
            db().scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = ? AND entity_id = ?::uuid",
                vec![Value::from(entity_type), Value::from(&target)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0)
        };
        let targets = [
            ("STOCK_IN", stock_in_id),
            ("PAYABLE", payable.id),
            ("VOUCHER", voucher.id),
        ];
        for (label, target) in targets {
            assert!(
                count_logs(label, target).await > 0,
                "清场前必须有 {label} 状态日志，否则这条用例证明不了什么"
            );
        }

        // 走模块真实的清场
        cleanup_fixtures().await;

        for (label, target) in targets {
            assert_eq!(
                count_logs(label, target).await,
                0,
                "清场后 {label} 状态日志必须为 0（日志无外键，必须按 id 先删）"
            );
        }

        // 兜底：整库不再有 PAYABLE / VOUCHER 的孤儿行
        fixture_hygiene::purge_orphans(fixture_hygiene::FINANCE).await;
        assert_eq!(
            fixture_hygiene::orphans(fixture_hygiene::FINANCE).await,
            0,
            "整库不许留 PAYABLE / VOUCHER 的孤儿状态日志"
        );
    }
}
