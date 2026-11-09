//! 期初余额录入（批次 D · ERP-R5）：客户从旧系统迁移过来，第一件事是录期初。
//!
//! # 两种录入口径
//!
//! | 方式 | 谁用 | 接口 | 落表 |
//! |------|------|------|------|
//! | A：按科目录 | 财务 | `POST /business/erp/opening-balance` | `biz_opening_balance` |
//! | B：库存期初（方式 B 的第一项） | 仓管 | `POST /business/erp/opening-balance/inventory` | `biz_opening_inventory` + 库存三本账 |
//! | 查询（合并两种口径 + 状态） | 都看 | `GET /business/erp/opening-balance?period=2026-11` | — |
//!
//! # 核心不变量
//!
//! ```text
//! 期初借方合计 = 期初贷方合计   （不平衡不许保存，400 + 差额 + 是哪些科目）
//! 每个期间只有一张期初凭证（voucher_type = 'OPENING'，重复录入 = 覆盖更新同一张）
//! 期初凭证不计入「本期发生额」（利润表看不到；资产负债表算进「期初」而不是「本期」）
//! 期间必须 OPEN（结账后 → 409，整页只读）
//! ```
//!
//! # 为什么期初要落成「凭证」而不是只落一张表
//!
//! 报表（资产负债表 / 利润表 / 试算平衡 / 总账）全部按 `biz_voucher_entry` 求数。
//! 期初若只进 `biz_opening_balance`，上面四处都要各加一条 union 分支 —— 四份实现、
//! 四种漏改的可能。落成一张 `voucher_type='OPENING'` 的凭证后，**取数逻辑一行不改**
//! 就自动包含期初（卡面 §三 推荐 B，本卡按推荐落地）。
//!
//! 代价是「期初凭证会被当成普通凭证统计本期发生额」——所以报表侧做了两条明确的排除：
//!
//! * [`crate::business::erp::report::load_accounts`]：`voucher_type='OPENING'` 的凭证
//!   从「本期发生额」里剔除，改记为「期初」（即报表的 期末 = 期初 + 本期 恒成立）；
//! * 利润表的本期 / 本年累计直接复用上面那条（[`crate::business::erp::income_statement`]），
//!   于是期初永远进不了损益发生额。
//!
//! # 库存期初为什么要同步三本账（硬红线 5）
//!
//! ```text
//! ① biz_inventory           qty += Δqty、amount += Δamount、avg_cost 重算（成本账，唯一真相）
//! ② biz_inventory_location  qty += Δqty（**仓库默认库位桶**，与收货过账同口径；
//!                            该仓没有默认库位时才落 location_id = NULL 的未分配库位桶 —— FIX-4 问题 1）
//! ③ biz_inventory_txn       txn_type = Δqty>0 ? 'IN' : 'OUT'，biz_type='OPENING'
//! ④ 期初凭证的 借1405 = Σ(biz_opening_inventory.amount)
//! ```
//!
//! 三本账 + 凭证四者必须同时成立，否则巡检 B1（库位账=成本账）/ B2（库存金额=1405 余额）/
//! B4（流水累计=当前库存）立刻红。所以本模块的写路径全部在**同一个事务**里，
//! 且复用 `stockin_model::apply_cost` / `apply_location_qty`（三列表达式唯一索引的
//! `ON CONFLICT` 口径只有一份实现，见 V0139 文件头 ④）。
//!
//! # 库存期初与「手工 1405 期初」互斥
//!
//! 库存期初会**自动**产出 `借 1405 / 贷 <对方科目>`。如果财务同时在方式 A 里手录了 1405 借方
//! （或对方科目贷方），1405 就会被计两次 → 库存金额 ≠ 总账 1405 余额（巡检 B2 直接红）。
//! 所以保存时会**拦住并点名**：同一期间只能用一种口径录 1405 / 对方科目。
//!
//! 对方科目**可配**（FIX-6 / V0328）：`sys_config.inventory_opening_equity_account`，
//! 缺省 `3103`（本年利润）。生成凭证、互斥校验、返回体提示三处都读它
//! （[`inventory_equity_account_code`]），**不要**再直接引用常量 `ACCOUNT_INVENTORY_OFFSET`。
//!
//! # 数据权限（门禁闸①/②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_opening_balance / biz_opening_inventory 是会计台账与库存台账，
//! 表里没有 dept_id / 归属人列（V0189 建表），数据范围表达不出来；
//! 可见性由 business:erp:opening:{read,write,inventory} 三个权限码控制。
//! ```
//!
//! # 迁移
//!
//! `backend/migrations/business/V0189__erp_opening_balance.{up,down}.sql`。
//! 卡面 §二 的两处差异（另建 `biz_opening_inventory` 而不是给 `biz_inventory` 加
//! `opening_flag`；两个表都不做软删）写在迁移头注释里。

use axum::{Extension, Json};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use super::inventory_writer::{self, TxnRecord};
use crate::business::erp::period;
use crate::business::erp::stockin_model::{self, CostBasis};
use crate::business::erp::voucher::{self, AccountRef, EntryLine, PostVoucher};
use crate::shared::error_code;
use crate::support::config::model::SysConfigModel;
use crate::utils;

// ==================== 常量 ====================

/// 页面 / 查询权限码。
pub const PERM_READ: &str = "business:erp:opening:read";
/// 录入科目期初（方式 A）权限码。
pub const PERM_WRITE: &str = "business:erp:opening:write";
/// 录入库存期初（方式 B）权限码。
pub const PERM_INVENTORY: &str = "business:erp:opening:inventory";

/// 三个权限码（迁移 V0189 的 `sys_menu` 节点与这里逐字对应）。
pub const OPENING_PERMISSIONS: [(&str, &str); 3] = [
    (PERM_READ, "期初余额"),
    (PERM_WRITE, "录入科目期初"),
    (PERM_INVENTORY, "录入库存期初"),
];

/// 科目期初表。
pub const TABLE_BALANCE: &str = "biz_opening_balance";
/// 库存期初录入台账。
pub const TABLE_INVENTORY: &str = "biz_opening_inventory";

/// 凭证类型：期初（V0149 的白名单由 V0189 扩成 `AUTO|MANUAL|OPENING`）。
pub const VOUCHER_TYPE_OPENING: &str = "OPENING";
/// 凭证来源类型。
pub const SOURCE_TYPE_OPENING: &str = "OPENING";
/// 期初凭证摘要。
pub const SUMMARY_OPENING: &str = "期初余额结转";
/// 期初凭证 / 库存期初流水的单号前缀（`OPENING-2026-11`）。
pub const SOURCE_NO_PREFIX: &str = "OPENING-";

/// 库存期初自动产出的借方科目（库存商品）。
pub const ACCOUNT_INVENTORY: &str = "1405";
/// 库存期初自动产出的贷方科目（对方科目）的**默认值**：本年利润。
///
/// FIX-6 起可配：`sys_config.inventory_opening_equity_account`（见
/// [`inventory_equity_account_code`]）。常量只作为「配置缺省 / 配置为空」时的兜底，
/// 生成期初凭证、互斥校验、返回体提示三处**都必须读配置**，不许直接引用本常量。
pub const ACCOUNT_INVENTORY_OFFSET: &str = "3103";
/// 库存期初对方科目的 `sys_config` 键（`config_value` 存科目编码，如 `3103` / `3101`）。
pub const CONFIG_INVENTORY_EQUITY_ACCOUNT: &str = "inventory_opening_equity_account";

/// 状态：未录。
pub const STATE_NONE: &str = "NONE";
/// 状态：已录（期间 OPEN，可改）。
pub const STATE_ENTERED: &str = "ENTERED";
/// 状态：已锁定（期间已结账，只读）。
pub const STATE_LOCKED: &str = "LOCKED";

/// 库存流水的业务类型（`biz_inventory_txn.biz_type`，该列无白名单）。
pub const BIZ_TYPE_OPENING: &str = "OPENING";
/// 库存流水的来源单据类型（`biz_inventory_txn.doc_type`）。
pub const DOC_TYPE_OPENING: &str = "OPENING";

/// 期初来源：手工录入（`biz_opening_balance.source`，V0190）。
pub const SOURCE_MANUAL: &str = "MANUAL";
/// 期初来源：由年结自动生成（ERP-R4）。这种行**不许手工覆盖** ——
/// 覆盖后它与历史累计各算一遍就是双计，只能先反年结。
pub const SOURCE_YEAR_END: &str = "YEAR_END";

// ==================== 入参 ====================

/// 一条科目期初。
#[derive(Debug, Clone, Deserialize)]
pub struct OpeningEntryReq {
    /// 科目编码（`biz_account.account_code`，如 `1002`）。
    pub account_code: Option<String>,
    /// 借方期初（缺省 0）。
    pub debit: Option<Decimal>,
    /// 贷方期初（缺省 0）。
    pub credit: Option<Decimal>,
    /// 备注。
    pub remark: Option<String>,
}

/// 保存科目期初（方式 A）。
#[derive(Debug, Clone, Deserialize)]
pub struct SaveOpeningReq {
    /// 期间（`yyyy-MM`，含两端）。
    pub period: Option<String>,
    /// 科目期初行（**整体覆盖**：本请求没提到的科目视为「没有期初」）。
    #[serde(default)]
    pub entries: Vec<OpeningEntryReq>,
}

/// 一条库存期初。
#[derive(Debug, Clone, Deserialize)]
pub struct OpeningInventoryItemReq {
    /// 物料 id。
    pub material_id: Option<String>,
    /// 仓库 id。
    pub warehouse_id: Option<String>,
    /// 期初数量（> 0）。
    pub qty: Option<Decimal>,
    /// 期初单价（缺省 0）。
    pub unit_cost: Option<Decimal>,
    /// 备注。
    pub remark: Option<String>,
}

/// 保存库存期初（方式 B 的第一项）。
#[derive(Debug, Clone, Deserialize)]
pub struct SaveOpeningInventoryReq {
    /// 期间（`yyyy-MM`）。
    pub period: Option<String>,
    /// 库存期初行（**整体覆盖**）。
    #[serde(default)]
    pub items: Vec<OpeningInventoryItemReq>,
}

/// 查询入参。
#[derive(Debug, Clone, Deserialize)]
pub struct OpeningQuery {
    /// 期间（`yyyy-MM`）。
    pub period: Option<String>,
}

// ==================== 出参 ====================

/// 一条科目期初（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct OpeningEntryRes {
    /// 科目 id。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别（`ASSET` / `LIABILITY` / `EQUITY` / `COST` / `PROFIT_LOSS`）。
    pub account_category: String,
    /// 借方期初。
    pub debit: Decimal,
    /// 贷方期初。
    pub credit: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 来源（`MANUAL` 手工录入 / `YEAR_END` 由上一年的年结自动生成，V0190）。
    #[serde(default)]
    pub source: String,
}

/// 一条库存期初（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct OpeningInventoryRes {
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库 id。
    pub warehouse_id: String,
    /// 仓库编码。
    pub warehouse_code: String,
    /// 仓库名称。
    pub warehouse_name: String,
    /// 期初数量。
    pub qty: Decimal,
    /// 期初单价。
    pub unit_cost: Decimal,
    /// 期初金额。
    pub amount: Decimal,
}

/// 期初凭证（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct OpeningVoucherRes {
    /// 凭证 id。
    pub id: String,
    /// 凭证号（`PZ-yyyyMM-000001`）。
    pub voucher_no: String,
    /// 凭证日期（期初期间的第一天）。
    pub voucher_date: NaiveDate,
    /// 状态（恒为 `POSTED`）。
    pub status: String,
    /// 借方合计。
    pub total_debit: Decimal,
    /// 贷方合计。
    pub total_credit: Decimal,
    /// 分录行数。
    pub entry_count: i32,
}

/// 期初余额总览（`GET` 的返回体，也是两个保存接口的返回体）。
#[derive(Debug, Clone, Serialize)]
pub struct OpeningBalanceRes {
    /// 期间。
    pub period: String,
    /// 期间状态（`OPEN` / `CLOSED` / `NOT_OPENED`）。
    pub period_status: String,
    /// 期初状态（`NONE` 未录 / `ENTERED` 已录 / `LOCKED` 已锁定）。
    pub state: String,
    /// 是否只读（= 期间已结账）。
    pub locked: bool,
    /// 科目期初行。
    pub entries: Vec<OpeningEntryRes>,
    /// 科目期初借方合计（**不含**库存期初）。
    pub debit_total: Decimal,
    /// 科目期初贷方合计（**不含**库存期初）。
    pub credit_total: Decimal,
    /// 库存期初行。
    pub inventory: Vec<OpeningInventoryRes>,
    /// 库存期初金额合计（= 期初凭证的 1405 借方）。
    pub inventory_amount: Decimal,
    /// 库存期初的借方科目（`1405`）。
    pub inventory_debit_account: String,
    /// 库存期初的贷方科目（`3103`）。
    pub inventory_credit_account: String,
    /// 合并口径借方合计（科目期初 + 库存期初。
    pub combined_debit_total: Decimal,
    /// 合并口径贷方合计。
    pub combined_credit_total: Decimal,
    /// 合并口径是否平衡。
    pub balanced: bool,
    /// 合并口径差额（借 − 贷）。
    pub difference: Decimal,
    /// 期初凭证（没录过 = `None`）。
    pub voucher: Option<OpeningVoucherRes>,
    /// 提示（不平 / 已有库存期初 / 损益类科目等）。
    pub hints: Vec<String>,
}

// ==================== 错误收口 ====================

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 400：期初不平衡（独立业务码，前端按它显示「差多少」）。
fn unbalanced_error(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_OPENING_UNBALANCED, message.into())
}

/// 409：状态冲突（期间已结账 / 未开账）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 写路径的数据库错误收口。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "期初写入冲突（唯一约束{}）：同一期间同一科目/物料只能有一行，请刷新后重试",
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足库约束 {}（期初金额非负；同一科目单边；期初数量 > 0）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        DbError::InvalidValue(message) => bad_request(message),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 期间 / 期间闸 ====================

/// 校验并规范化期间编码（`2026-11`）。
pub fn validate_period(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(
            "period 必填：格式 yyyy-MM（如 2026-11 表示 11 月初的期初）",
        ));
    }
    match crate::business::erp::report::parse_period(text) {
        Some((year, month)) => Ok(format!("{year:04}-{month:02}")),
        None => Err(bad_request(format!(
            "period 格式不合法：{text}（应为 yyyy-MM，如 2026-11）"
        ))),
    }
}

/// 期间的第一天（期初凭证的凭证日期）。
fn first_day_of(period: &str) -> std::result::Result<NaiveDate, ApiError> {
    let (year, month) = crate::business::erp::report::parse_period(period)
        .ok_or_else(|| bad_request(format!("period 格式不合法：{period}")))?;
    crate::business::erp::report::month_range(&format!("{year:04}-{month:02}"))
        .map(|(start, _)| start)
        .ok_or_else(|| bad_request(format!("period 无法换算成日期区间：{period}")))
}

/// 锁读期间行并返回状态（`FOR UPDATE`：同一期间的两次录入串行化）。
///
/// * 期间不存在 → 400（未开账就不能录期初）；
/// * `NOT_OPENED` / `CLOSED` → **409**（卡面 ⑥ ④「结账后录入 → 409」）。
async fn lock_period(ex: &impl SqlExecutor, period_code: &str) -> std::result::Result<String, ApiError> {
    let row = ex
        .query_first_sql(
            "SELECT status FROM biz_accounting_period WHERE period_code = ? FOR UPDATE".to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .map_err(|error| write_error(error, "锁定会计期间"))?;
    let Some(row) = row else {
        return Err(bad_request(format!(
            "会计期间 {period_code} 尚未开账：期初余额必须挂在已开账的期间上（先在「会计期间」页开账）"
        )));
    };
    let status: String = row
        .get_typed("status")
        .map_err(|error| write_error(error, "读期间状态"))?;
    match status.as_str() {
        period::STATUS_OPEN => Ok(status),
        period::STATUS_CLOSED => Err(conflict(format!(
            "会计期间 {period_code} 已结账，期初不可改：已封账月份的期初余额只读，\
             如需调整请先反结账（反结账需要填写原因）"
        ))),
        other => Err(conflict(format!(
            "会计期间 {period_code} 的状态是 {other}（不是 OPEN），不能录期初：请先在「会计期间」页开账"
        ))),
    }
}

// ==================== 读 ====================

/// 科目期初行。
async fn load_balance_rows(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<OpeningEntryRes>> {
    // SCOPE-EXEMPT: 期初台账无归属列（见模块文档）。
    let sql = format!(
        "SELECT b.account_id, b.account_code, \
                COALESCE(a.account_name, b.account_code) AS account_name, \
                COALESCE(a.account_category, '') AS account_category, \
                b.debit_opening, b.credit_opening, b.remark, \
                COALESCE(b.source, 'MANUAL') AS source \
           FROM {TABLE_BALANCE} b \
           LEFT JOIN biz_account a ON a.id = b.account_id \
          WHERE b.period_code = ? \
          ORDER BY b.account_code"
    );
    let rows = ex.query_sql(sql, vec![Value::Text(period_code.to_string())]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(OpeningEntryRes {
            account_id: row.get_typed::<Uuid>("account_id")?.to_string(),
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            account_category: row.get_typed("account_category")?,
            debit: row.get_typed("debit_opening")?,
            credit: row.get_typed("credit_opening")?,
            remark: row.get_typed("remark").ok(),
            source: row
                .get_typed::<String>("source")
                .unwrap_or_else(|_| SOURCE_MANUAL.to_string()),
        });
    }
    Ok(out)
}

/// 库存期初行。
async fn load_inventory_rows(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<OpeningInventoryRes>> {
    // SCOPE-EXEMPT: 期初台账无归属列（见模块文档）。
    let sql = format!(
        "SELECT o.material_id, COALESCE(m.material_code, o.material_id::text) AS material_code, \
                COALESCE(m.material_name, '') AS material_name, \
                o.warehouse_id, COALESCE(w.warehouse_code, o.warehouse_id::text) AS warehouse_code, \
                COALESCE(w.warehouse_name, '') AS warehouse_name, \
                o.qty, o.unit_cost, o.amount \
           FROM {TABLE_INVENTORY} o \
           LEFT JOIN biz_material m ON m.id = o.material_id \
           LEFT JOIN biz_warehouse w ON w.id = o.warehouse_id \
          WHERE o.period_code = ? \
          ORDER BY 2, 5"
    );
    let rows = ex.query_sql(sql, vec![Value::Text(period_code.to_string())]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(OpeningInventoryRes {
            material_id: row.get_typed::<Uuid>("material_id")?.to_string(),
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            warehouse_id: row.get_typed::<Uuid>("warehouse_id")?.to_string(),
            warehouse_code: row.get_typed("warehouse_code")?,
            warehouse_name: row.get_typed("warehouse_name")?,
            qty: row.get_typed("qty")?,
            unit_cost: row.get_typed("unit_cost")?,
            amount: row.get_typed("amount")?,
        });
    }
    Ok(out)
}

/// 库存期初金额合计。
async fn inventory_amount_of(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Decimal> {
    let sql = format!(
        "SELECT COALESCE(SUM(amount), 0)::numeric(20,4) AS total FROM {TABLE_INVENTORY} \
          WHERE period_code = ?"
    );
    ex.scalar_sql(sql, vec![Value::Text(period_code.to_string())]).await
}

/// 该期间的期初凭证（最多一张，库层面有部分唯一索引兜着）。
async fn load_opening_voucher(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<OpeningVoucherRes>> {
    let sql = format!(
        "SELECT id, voucher_no, voucher_date, status, total_debit, total_credit, entry_count \
           FROM {} \
          WHERE voucher_type = '{}' AND period = ? AND deleted_at IS NULL",
        voucher::TABLE,
        VOUCHER_TYPE_OPENING
    );
    let row = ex
        .query_first_sql(sql, vec![Value::Text(period_code.to_string())])
        .await?;
    match row {
        Some(row) => Ok(Some(OpeningVoucherRes {
            id: row.get_typed::<Uuid>("id")?.to_string(),
            voucher_no: row.get_typed("voucher_no")?,
            voucher_date: row.get_typed("voucher_date")?,
            status: row.get_typed("status")?,
            total_debit: row.get_typed("total_debit")?,
            total_credit: row.get_typed("total_credit")?,
            entry_count: row.get_typed("entry_count")?,
        })),
        None => Ok(None),
    }
}

/// 期间状态（不存在 → `None`）。
async fn period_status_of(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<String>> {
    voucher::period_status(ex, period_code).await
}

/// 读一整份期初总览（GET 与两个保存接口的返回体）。
pub async fn load_opening_in(
    ex: &impl SqlExecutor,
    period_code: &str,
) -> std::result::Result<OpeningBalanceRes, ApiError> {
    let status = period_status_of(ex, period_code)
        .await
        .map_err(|error| write_error(error, "读期间状态"))?
        .ok_or_else(|| {
            ApiError::new(
                400,
                error_code::ERP_PERIOD_NOT_FOUND,
                format!("会计期间 {period_code} 不存在：请先在「会计期间」页开账，再录期初"),
            )
        })?;

    let entries = load_balance_rows(ex, period_code)
        .await
        .map_err(|error| write_error(error, "读科目期初"))?;
    let inventory = load_inventory_rows(ex, period_code)
        .await
        .map_err(|error| write_error(error, "读库存期初"))?;
    let voucher_row = load_opening_voucher(ex, period_code)
        .await
        .map_err(|error| write_error(error, "读期初凭证"))?;

    let mut debit_total = Decimal::ZERO;
    let mut credit_total = Decimal::ZERO;
    for row in &entries {
        debit_total += row.debit;
        credit_total += row.credit;
    }
    let mut inventory_amount = Decimal::ZERO;
    for row in &inventory {
        inventory_amount += row.amount;
    }
    let combined_debit = debit_total + inventory_amount;
    let combined_credit = credit_total + inventory_amount;
    let difference = combined_debit - combined_credit;
    let locked = status == period::STATUS_CLOSED;
    let entered = !entries.is_empty() || !inventory.is_empty() || voucher_row.is_some();
    let state = if locked {
        STATE_LOCKED
    } else if entered {
        STATE_ENTERED
    } else {
        STATE_NONE
    };

    let equity_code = inventory_equity_account_code().await;
    let mut hints: Vec<String> = Vec::new();
    if !difference.is_zero() {
        hints.push(format!(
            "期初不平衡：借方合计 {combined_debit} / 贷方合计 {combined_credit}，差额 {difference}。\
             不平衡的期初不能保存，也不能出报表"
        ));
    }
    if inventory_amount > Decimal::ZERO {
        hints.push(format!(
            "库存期初 {inventory_amount} 已自动生成「借 {ACCOUNT_INVENTORY} / 贷 {equity_code}」\
             （对方科目由系统参数 {CONFIG_INVENTORY_EQUITY_ACCOUNT} 指定）；\
             方式 A 里不要再单独录这两个科目的期初（会重复计数）"
        ));
    }
    if entries.iter().any(|row| row.source == SOURCE_YEAR_END) {
        hints.push(
            "这份期初由上一年的「年结」自动生成（结转凭证已把它带进新年度）：不能手工改，\
             要调整请先在「年结」页反年结"
                .to_string(),
        );
    }
    if locked {
        hints.push(format!("会计期间 {period_code} 已结账：期初余额只读，要改必须先反结账"));
    }
    if entries.iter().any(|row| row.account_category == "PROFIT_LOSS") {
        hints.push(
            "损益类科目（收入 / 费用）没有期初余额：报表的「本期 / 本年累计」都不含期初，\
             录了只会进「期末累计」列。确认是历史未结转余额才保留"
                .to_string(),
        );
    }

    Ok(OpeningBalanceRes {
        period: period_code.to_string(),
        period_status: status,
        state: state.to_string(),
        locked,
        entries,
        debit_total,
        credit_total,
        inventory,
        inventory_amount,
        inventory_debit_account: ACCOUNT_INVENTORY.to_string(),
        inventory_credit_account: equity_code,
        combined_debit_total: combined_debit,
        combined_credit_total: combined_credit,
        balanced: difference.is_zero(),
        difference,
        voucher: voucher_row,
        hints,
    })
}

// ==================== 期初凭证：生成 / 覆盖 ====================

/// 把「科目期初 + 库存期初」拼成凭证分录。
///
/// * 科目期初：一行一条（借方或贷方）；
/// * 库存期初：合计一行 `借 1405`、一行 `贷 3103`（金额 = Σ amount）。
fn build_lines(
    entries: &[(AccountRef, Decimal, Decimal)],
    inventory_amount: Decimal,
    inventory_accounts: Option<(AccountRef, AccountRef)>,
) -> Vec<EntryLine> {
    let mut lines: Vec<EntryLine> = Vec::with_capacity(entries.len() + 2);
    for (account, debit, credit) in entries {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{} 期初", account.name),
            account: account.clone(),
            debit: *debit,
            credit: *credit,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    if inventory_amount > Decimal::ZERO
        && let Some((debit_account, credit_account)) = inventory_accounts
    {
        lines.push(EntryLine {
            line_no: 0,
            summary: "库存期初".to_string(),
            account: debit_account,
            debit: inventory_amount,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_dept_id: None,
            aux_warehouse_id: None,
        });
        lines.push(EntryLine {
            line_no: 0,
            summary: "库存期初（对方科目）".to_string(),
            account: credit_account,
            debit: Decimal::ZERO,
            credit: inventory_amount,
            aux_supplier_id: None,
            aux_dept_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
        });
    }
    for (index, line) in lines.iter_mut().enumerate() {
        line.line_no = i32::try_from(index + 1).unwrap_or(0);
    }
    lines
}

/// 取库存期初的两个固定科目（`1405` / 配置的对方科目）。
///
/// 对方科目读 `sys_config.inventory_opening_equity_account`（缺省 [`ACCOUNT_INVENTORY_OFFSET`]
/// = 3103 本年利润）。配置成不存在的科目 / 汇总科目 → 400 点名字段，别让它静默回落到 3103
/// （静默回落会让「改了配置却没生效」这种事查不出来）。
async fn inventory_accounts(ex: &impl SqlExecutor) -> std::result::Result<(AccountRef, AccountRef), ApiError> {
    let debit = voucher::account_by_code(ex, ACCOUNT_INVENTORY)
        .await
        .map_err(|error| write_error(error, "解析库存科目 1405"))?
        .ok_or_else(|| {
            bad_request(format!(
                "科目 {ACCOUNT_INVENTORY}（库存商品）不存在或已停用：库存期初要借它，\
                 请先在「会计科目」页启用"
            ))
        })?;
    let equity_code = inventory_equity_account_code().await;
    let credit = voucher::account_by_code(ex, &equity_code)
        .await
        .map_err(|error| write_error(error, "解析库存期初对方科目"))?
        .ok_or_else(|| {
            bad_request(format!(
                "科目 {equity_code} 不存在或已停用：库存期初要贷它（对方科目由系统参数 \
                 {CONFIG_INVENTORY_EQUITY_ACCOUNT} 指定，缺省 {ACCOUNT_INVENTORY_OFFSET} = 本年利润）—— \
                 请改系统参数或先在「会计科目」页启用该科目"
            ))
        })?;
    Ok((debit, credit))
}

/// 库存期初对方科目编码：`sys_config.inventory_opening_equity_account` → 缺省 `3103`。
///
/// 读失败 / 配置为空一律回退 [`ACCOUNT_INVENTORY_OFFSET`]：这条参数是「期初贷哪个权益科目」
/// 的账务口径，读不到就整条链路 500 等于期初录不进去，代价远大于「按默认 3103 记账」。
pub async fn inventory_equity_account_code() -> String {
    SysConfigModel::read_text(CONFIG_INVENTORY_EQUITY_ACCOUNT)
        .await
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ACCOUNT_INVENTORY_OFFSET.to_string())
}

/// 生成 / 覆盖该期间的期初凭证（**不 commit**）。
///
/// * 分录为空 → 把已存在的期初凭证删掉（期初被清空了，凭据也要撤）；
/// * 凭证已存在 → **原地覆盖**（凭证号不变、分录整组替换），不新增第二张；
/// * 不存在 → 走 [`voucher::post_voucher`]（取号 / 借贷平衡 / 期间闸 / 留痕都在那一处）。
async fn regenerate_voucher_in(
    ex: &impl SqlExecutor,
    period_code: &str,
    lines: Vec<EntryLine>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    let source_no = format!("{SOURCE_NO_PREFIX}{period_code}");
    let existing = ex
        .query_first_sql(
            format!(
                "SELECT id FROM {} WHERE voucher_type = '{}' AND period = ? \
                  AND deleted_at IS NULL FOR UPDATE",
                voucher::TABLE,
                VOUCHER_TYPE_OPENING
            ),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .map_err(|error| write_error(error, "锁定期初凭证"))?
        .map(|row| row.get_typed::<Uuid>("id"))
        .transpose()
        .map_err(|error| write_error(error, "读期初凭证 id"))?;

    if lines.is_empty() {
        if let Some(id) = existing {
            // 分录随 biz_voucher_entry 的 ON DELETE CASCADE 一起走
            ex.exec_sql(
                format!("DELETE FROM {} WHERE id = ?::uuid", voucher::TABLE),
                vec![Value::from(&id)],
            )
            .await
            .map_err(|error| write_error(error, "删除空期初凭证"))?;
            return Ok(None);
        }
        return Ok(None);
    }

    if let Some(id) = existing {
        let (debit, credit) = voucher::totals_of(&lines);
        if debit != credit {
            return Err(unbalanced_error(format!(
                "期初凭证不能生成：借贷不平衡（借 {debit} / 贷 {credit}，差额 {}）",
                debit - credit
            )));
        }
        let entry_count = i32::try_from(lines.len()).unwrap_or(0);
        let voucher_date = first_day_of(period_code)?;
        ex.exec_sql(
            format!(
                "UPDATE {} SET voucher_date = ?::text::date, summary = ?, \
                    total_debit = NULLIF(?::text,'')::numeric, \
                    total_credit = NULLIF(?::text,'')::numeric, \
                    entry_count = ?, source_no = ?, \
                    remark = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid",
                voucher::TABLE
            ),
            vec![
                Value::Text(voucher_date.to_string()),
                Value::Text(SUMMARY_OPENING.to_string()),
                Value::Text(debit.to_string()),
                Value::Text(credit.to_string()),
                Value::from(entry_count),
                Value::Text(source_no),
                Value::Text("期初余额结转（覆盖式更新：同一期间只有一张期初凭证，凭证号保持不变）".to_string()),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(&id),
            ],
        )
        .await
        .map_err(|error| write_error(error, "覆盖期初凭证单头"))?;
        voucher::replace_entries(ex, id, &lines)
            .await
            .map_err(|error| write_error(error, "覆盖期初凭证分录"))?;
        return Ok(Some(id));
    }

    let voucher_date = first_day_of(period_code)?;
    let voucher_no = format!("{SOURCE_NO_PREFIX}{period_code}");
    let id = voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_OPENING,
            source_type: SOURCE_TYPE_OPENING,
            source_id: None,
            source_no: Some(&voucher_no),
            source_doc_no: None,
            voucher_date,
            summary: SUMMARY_OPENING.to_string(),
            lines,
            remark: Some("期初余额结转（由「期初余额」页录入，一张凭证对应一个期间）"),
            red_of: None,
            status: crate::business::erp::voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成期初凭证"))?;
    Ok(Some(id))
}

// ==================== 年结专用写入口（ERP-R4）====================
//
// 年结要往「下一年度第一个期间」写期初，但**不能**走 `save_opening_in`：
// 它会顺手生成一张 `voucher_type='OPENING'` 的期初凭证，而新年度之前的历史
// （也就是本年的结转凭证）已经在凭证里了 —— 期初凭证会让报表的「期初」列
// 与「历史累计」各算一遍，存货 / 银行存款全变两倍。
//
// 所以年结只写**期初台账**（`biz_opening_balance`，`source='YEAR_END'`）：
// 它回答「新的一年从多少开始」，报表侧的「期初」仍由历史累计给出（两者相等，
// 由 `year_end` 的 ⑨ 号单测钉住）。

/// 一条要被写入的期初行（科目 id / 科目编码 / 借 / 贷）。
pub(crate) type YearEndOpeningRow = (Uuid, String, Decimal, Decimal);

/// 年结写「下年期初」：整组覆盖该期间的科目期初，来源标记为 `YEAR_END`。
///
/// 返回写入的行数。**不生成期初凭证**（理由见本节开头）。
pub(crate) async fn write_year_end_opening_in(
    tx: &impl SqlExecutor,
    period_code: &str,
    rows: &[YearEndOpeningRow],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<usize, ApiError> {
    let deleted = clear_opening_in(tx, period_code).await?;
    if deleted > 0 {
        tracing::info!(period = %period_code, rows = deleted, "年结覆盖旧期初");
    }
    let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
    for (account_id, account_code, debit, credit) in rows {
        tx.exec_sql(
            format!(
                "INSERT INTO {TABLE_BALANCE} \
                    (period_code, account_id, account_code, debit_opening, credit_opening, \
                     remark, source, created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?::uuid, ?, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                         ?, '{}', ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
                SOURCE_YEAR_END
            ),
            vec![
                Value::Text(period_code.to_string()),
                Value::from(account_id),
                Value::Text(account_code.clone()),
                Value::Text(debit.to_string()),
                Value::Text(credit.to_string()),
                Value::Text("由上一会计年度年结自动生成".to_string()),
                Value::from(now),
                Value::from(now),
                Value::Text(operator_text.clone()),
                Value::Text(operator_text.clone()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "写入下年期初"))?;
    }
    Ok(rows.len())
}

/// 清空某期间的科目期初（反年结用；返回删掉的行数）。
pub(crate) async fn clear_opening_in(tx: &impl SqlExecutor, period_code: &str) -> std::result::Result<u64, ApiError> {
    tx.exec_sql(
        format!("DELETE FROM {TABLE_BALANCE} WHERE period_code = ?"),
        vec![Value::Text(period_code.to_string())],
    )
    .await
    .map_err(|error| write_error(error, "清空该期间科目期初"))
}

/// 该期间的科目期初里有没有年结生成的行（有 → 手工保存必须拒绝）。
async fn guard_not_year_end(ex: &impl SqlExecutor, period_code: &str) -> std::result::Result<(), ApiError> {
    let count: i64 = ex
        .scalar_sql(
            format!(
                "SELECT COUNT(*) FROM {TABLE_BALANCE} WHERE period_code = ? AND source = '{}'",
                SOURCE_YEAR_END
            ),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .map_err(|error| write_error(error, "统计年结期初行"))?;
    if count > 0 {
        return Err(conflict(format!(
            "期间 {period_code} 的期初有 {count} 行是上一年的「年结」自动生成的，不能手工覆盖：\
             它已经与历史累计一致，再写一遍会重复计数。要调整请先在「年结」页反年结"
        )));
    }
    Ok(())
}

// ==================== 保存：科目期初（方式 A）====================

/// 一条校验过的科目期初（内部用）。
#[derive(Debug, Clone)]
struct FlatEntry {
    account: AccountRef,
    debit: Decimal,
    credit: Decimal,
    remark: Option<String>,
    profit_loss: bool,
}

/// 解析 + 校验一条科目期初。
async fn resolve_entry(
    ex: &impl SqlExecutor,
    raw: &OpeningEntryReq,
    seen: &mut std::collections::BTreeMap<String, ()>,
) -> std::result::Result<Option<FlatEntry>, ApiError> {
    let code = raw
        .account_code
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("每条期初都必须给 account_code（如 1002）"))?;
    if seen.insert(code.to_string(), ()).is_some() {
        return Err(bad_request(format!(
            "科目 {code} 重复录入：一个科目在一个期间只能有一个期初余额，请合并成一行"
        )));
    }
    let row = ex
        .query_first_sql(
            "SELECT id, account_code, account_name, is_leaf, account_category \
               FROM biz_account WHERE account_code = ? AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(code.to_string())],
        )
        .await
        .map_err(|error| write_error(error, "查询科目"))?
        .ok_or_else(|| {
            bad_request(format!(
                "科目 {code} 不存在或已删除：请先在「会计科目」页建好科目再录期初"
            ))
        })?;
    let is_leaf: i16 = row
        .get_typed("is_leaf")
        .map_err(|error| write_error(error, "读科目末级标记"))?;
    if is_leaf != 1 {
        return Err(bad_request(format!(
            "科目 {code} 不是末级科目：期初只能录到最末级科目（上级科目由下级汇总）"
        )));
    }
    let category: String = row
        .get_typed("account_category")
        .map_err(|error| write_error(error, "读科目类别"))?;
    let account = AccountRef {
        id: row
            .get_typed::<Uuid>("id")
            .map_err(|error| write_error(error, "读科目 id"))?,
        code: row
            .get_typed("account_code")
            .map_err(|error| write_error(error, "读科目编码"))?,
        name: row
            .get_typed("account_name")
            .map_err(|error| write_error(error, "读科目名称"))?,
    };
    let debit = raw.debit.unwrap_or_default();
    let credit = raw.credit.unwrap_or_default();
    if debit < Decimal::ZERO || credit < Decimal::ZERO {
        return Err(bad_request(format!(
            "科目 {code} 的期初金额不能为负（借 {debit} / 贷 {credit}）：\
             负数请换成对方科目，或把方向换成贷方"
        )));
    }
    if debit > Decimal::ZERO && credit > Decimal::ZERO {
        return Err(bad_request(format!(
            "科目 {code} 的借方与贷方期初不能同时非零（借 {debit} / 贷 {credit}）：\
             期初余额是一个净额，请只填一边"
        )));
    }
    if debit.is_zero() && credit.is_zero() {
        // 0 元的行等于没录，直接丢掉（不写库、不占唯一键）
        return Ok(None);
    }
    Ok(Some(FlatEntry {
        account,
        debit: debit.round_dp(4),
        credit: credit.round_dp(4),
        remark: raw.remark.clone(),
        profit_loss: category == "PROFIT_LOSS",
    }))
}

/// 校验「合计借贷平衡」，不平就点名差额 + 两边各是哪些科目（卡面 O5）。
fn ensure_balanced(
    entries: &[FlatEntry],
    inventory_amount: Decimal,
    inventory_accounts: &Option<(AccountRef, AccountRef)>,
) -> std::result::Result<(), ApiError> {
    let mut debit = inventory_amount;
    let mut credit = inventory_amount;
    let mut debit_parts: Vec<String> = Vec::new();
    let mut credit_parts: Vec<String> = Vec::new();
    for row in entries {
        debit += row.debit;
        credit += row.credit;
        if row.debit > Decimal::ZERO {
            debit_parts.push(format!("{} {} {}", row.account.code, row.account.name, row.debit));
        }
        if row.credit > Decimal::ZERO {
            credit_parts.push(format!("{} {} {}", row.account.code, row.account.name, row.credit));
        }
    }
    if let Some((debit_account, _)) = inventory_accounts
        && inventory_amount > Decimal::ZERO
    {
        debit_parts.push(format!(
            "{} {} {}（库存期初自动生成）",
            debit_account.code, debit_account.name, inventory_amount
        ));
    }
    if debit == credit {
        return Ok(());
    }
    if debit_parts.is_empty() {
        debit_parts.push("（借方一行都没有）".to_string());
    }
    if credit_parts.is_empty() {
        credit_parts.push("（贷方一行都没有 —— 是不是漏录了对方科目？）".to_string());
    }
    let profit_loss_note = if entries.iter().any(|row| row.profit_loss) {
        "\n提示：损益类科目（收入 / 费用）没有期初余额，它不会进利润表的「本期 / 本年累计」——\
         如果这行本来就不该录，删掉它往往就平了"
    } else {
        ""
    };
    Err(unbalanced_error(format!(
        "期初借贷不平衡，不能保存：借方合计 {debit} / 贷方合计 {credit}，差额 {}。\
         \n借方：{}\n贷方：{}{profit_loss_note}",
        debit - credit,
        debit_parts.join("；"),
        credit_parts.join("；")
    )))
}

/// 保存科目期初（整体覆盖，同事务生成/覆盖期初凭证）。
pub async fn save_opening(
    raw_period: Option<&str>,
    entries: &[OpeningEntryReq],
    operator: Option<Uuid>,
) -> std::result::Result<OpeningBalanceRes, ApiError> {
    let period_code = validate_period(raw_period)?;
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| write_error(error, "开启期初保存事务"))?;
    let result = save_opening_in(&tx, &period_code, entries, operator, now).await;
    match result {
        Ok(out) => {
            tx.commit()
                .await
                .map_err(|error| write_error(error, "提交期初保存事务"))?;
            Ok(out)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 保存科目期初（事务内实现，单测可直接调用）。
pub async fn save_opening_in(
    tx: &impl SqlExecutor,
    period_code: &str,
    entries: &[OpeningEntryReq],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<OpeningBalanceRes, ApiError> {
    lock_period(tx, period_code).await?;
    guard_not_year_end(tx, period_code).await?;

    let mut seen = std::collections::BTreeMap::new();
    let mut flat: Vec<FlatEntry> = Vec::with_capacity(entries.len());
    for raw in entries {
        if let Some(row) = resolve_entry(tx, raw, &mut seen).await? {
            flat.push(row);
        }
    }

    let inventory_amount = inventory_amount_of(tx, period_code)
        .await
        .map_err(|error| write_error(error, "读库存期初合计"))?;

    // 互斥：库存期初已经产出了 借1405 / 贷<对方科目>，方式 A 里再录就是重复计数
    let equity_code = inventory_equity_account_code().await;
    if inventory_amount > Decimal::ZERO {
        if let Some(row) = flat
            .iter()
            .find(|row| row.account.code == ACCOUNT_INVENTORY && row.debit > Decimal::ZERO)
        {
            return Err(bad_request(format!(
                "科目 {} 已经有库存期初 {}（自动生成 借{}），不能再单独录借方期初 {}：\
                 两个口径会重复计数，库存金额就会与总账 1405 余额对不上。\
                 要么删掉这一行，要么先把库存期初清零",
                row.account.code, inventory_amount, ACCOUNT_INVENTORY, row.debit
            )));
        }
        if let Some(row) = flat
            .iter()
            .find(|row| row.account.code == equity_code && row.credit > Decimal::ZERO)
        {
            return Err(bad_request(format!(
                "科目 {} 已经有库存期初 {}（自动生成 贷{}），不能再单独录贷方期初 {}：\
                 两个口径会重复计数。要么删掉这一行，要么先把库存期初清零",
                row.account.code, inventory_amount, equity_code, row.credit
            )));
        }
    }

    let accounts = if inventory_amount > Decimal::ZERO {
        Some(inventory_accounts(tx).await?)
    } else {
        None
    };
    ensure_balanced(&flat, inventory_amount, &accounts)?;

    // 覆盖式写入：先删干净，再按本次 entries 重建
    tx.exec_sql(
        format!("DELETE FROM {TABLE_BALANCE} WHERE period_code = ?"),
        vec![Value::Text(period_code.to_string())],
    )
    .await
    .map_err(|error| write_error(error, "清空旧科目期初"))?;
    for row in &flat {
        tx.exec_sql(
            format!(
                "INSERT INTO {TABLE_BALANCE} \
                    (period_code, account_id, account_code, debit_opening, credit_opening, \
                     remark, created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?::uuid, ?, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                         NULLIF(?::text,''), ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            ),
            vec![
                Value::Text(period_code.to_string()),
                Value::from(&row.account.id),
                Value::Text(row.account.code.clone()),
                Value::Text(row.debit.to_string()),
                Value::Text(row.credit.to_string()),
                Value::Text(row.remark.clone().unwrap_or_default()),
                Value::from(now),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "写入科目期初"))?;
    }

    // 期初凭证：科目期初 + 库存期初（同一事务）
    let inventory_rows = load_inventory_rows(tx, period_code)
        .await
        .map_err(|error| write_error(error, "读库存期初"))?;
    let inventory_amount = inventory_rows.iter().fold(Decimal::ZERO, |sum, row| sum + row.amount);
    let accounts = if inventory_amount > Decimal::ZERO {
        Some(inventory_accounts(tx).await?)
    } else {
        None
    };
    let pairs: Vec<(AccountRef, Decimal, Decimal)> = flat
        .iter()
        .map(|row| (row.account.clone(), row.debit, row.credit))
        .collect();
    let lines = build_lines(&pairs, inventory_amount, accounts);
    regenerate_voucher_in(tx, period_code, lines, operator, now).await?;

    load_opening_in(tx, period_code).await
}

// ==================== 保存：库存期初（方式 B）====================

/// 一条校验过的库存期初（内部用）。
#[derive(Debug, Clone)]
struct FlatInventory {
    material_id: Uuid,
    warehouse_id: Uuid,
    qty: Decimal,
    unit_cost: Decimal,
    amount: Decimal,
    remark: Option<String>,
}

/// 解析 + 校验一条库存期初。
async fn resolve_inventory_item(
    ex: &impl SqlExecutor,
    raw: &OpeningInventoryItemReq,
    seen: &mut std::collections::BTreeSet<(Uuid, Uuid)>,
) -> std::result::Result<FlatInventory, ApiError> {
    let material_id = parse_uuid(raw.material_id.as_deref(), "material_id")?;
    let warehouse_id = parse_uuid(raw.warehouse_id.as_deref(), "warehouse_id")?;
    let qty = raw.qty.unwrap_or_default();
    if qty <= Decimal::ZERO {
        return Err(bad_request(format!(
            "期初数量必须 > 0（物料 {material_id} 传的是 {qty}）：数量为 0 的行请直接删掉"
        )));
    }
    let unit_cost = raw.unit_cost.unwrap_or_default();
    if unit_cost < Decimal::ZERO {
        return Err(bad_request(format!(
            "期初单价不能为负（物料 {material_id} 传的是 {unit_cost}）"
        )));
    }
    if !seen.insert((material_id, warehouse_id)) {
        return Err(bad_request(format!(
            "物料 {material_id} / 仓库 {warehouse_id} 重复录入：同物料同仓只能有一行期初，请合并"
        )));
    }

    let material = ex
        .query_first_sql(
            "SELECT material_code, serial_required, status FROM biz_material \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&material_id)],
        )
        .await
        .map_err(|error| write_error(error, "查询物料"))?
        .ok_or_else(|| {
            bad_request(format!(
                "物料 {material_id} 不存在或已删除：请先在「物料管理」页建好物料"
            ))
        })?;
    let material_code: String = material
        .get_typed("material_code")
        .map_err(|error| write_error(error, "读物料编码"))?;
    let status: i16 = material
        .get_typed("status")
        .map_err(|error| write_error(error, "读物料状态"))?;
    if status != 1 {
        return Err(bad_request(format!(
            "物料 {material_code} 已停用，不能录期初：请先启用"
        )));
    }
    // 一物一码物料的 qty 是「序列号件数」的派生值（巡检 C5），按数量录期初必然对不上
    let serial_required: bool = material.get_typed("serial_required").unwrap_or(false);
    if serial_required {
        return Err(bad_request(format!(
            "物料 {material_code} 是一物一码物料：库存数量由序列号件数派生，\
             不能按数量录期初（录了巡检 C5 立刻红）。请等「序列号期初导入」卡落地后再录"
        )));
    }

    let warehouse = ex
        .query_first_sql(
            "SELECT warehouse_code FROM biz_warehouse WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&warehouse_id)],
        )
        .await
        .map_err(|error| write_error(error, "查询仓库"))?
        .ok_or_else(|| {
            bad_request(format!(
                "仓库 {warehouse_id} 不存在或已删除：请先在「仓库管理」页建好仓库"
            ))
        })?;
    let _warehouse_code: String = warehouse
        .get_typed("warehouse_code")
        .map_err(|error| write_error(error, "读仓库编码"))?;

    // 金额只算一次（Rust 侧四舍五入到 4 位），落库的 amount 与库存增量用同一个值
    let amount = (qty * unit_cost).round_dp(4);
    Ok(FlatInventory {
        material_id,
        warehouse_id,
        qty: qty.round_dp(4),
        unit_cost: unit_cost.round_dp(6),
        amount,
        remark: raw.remark.clone(),
    })
}

/// 路径 / 请求体里的 UUID：非法 → 400 + 66006。
fn parse_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

/// 保存库存期初（整体覆盖，同事务写三本账 + 期初凭证）。
pub async fn save_inventory(
    raw_period: Option<&str>,
    items: &[OpeningInventoryItemReq],
    operator: Option<Uuid>,
) -> std::result::Result<OpeningBalanceRes, ApiError> {
    let period_code = validate_period(raw_period)?;
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| write_error(error, "开启库存期初事务"))?;
    let result = save_inventory_in(&tx, &period_code, items, operator, now).await;
    match result {
        Ok(out) => {
            tx.commit()
                .await
                .map_err(|error| write_error(error, "提交库存期初事务"))?;
            Ok(out)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 可空 UUID → 参数（`Value::Null` 绑 `?::uuid` 在驱动侧会编码失败，统一走「空串 + NULLIF」）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 读本期已有库存期初行落在哪个库位桶：`(物料, 仓库) → 库位账行 id`。
///
/// `None` = 该行没有记录桶（V0324 之前写的老行 / 该仓当时没有默认库位），
/// 调用方按「当前默认库位」重新解析。
async fn load_opening_buckets(
    ex: &impl SqlExecutor,
    period_code: &str,
) -> DbResult<std::collections::BTreeMap<(Uuid, Uuid), Option<Uuid>>> {
    // SCOPE-EXEMPT: 期初台账无归属列（见模块文档）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT material_id, warehouse_id, location_row_id FROM {TABLE_INVENTORY} \
                  WHERE period_code = ?"
            ),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    let mut out = std::collections::BTreeMap::new();
    for row in &rows {
        out.insert(
            (
                row.get_typed::<Uuid>("material_id")?,
                row.get_typed::<Uuid>("warehouse_id")?,
            ),
            row.get_typed::<Option<Uuid>>("location_row_id")?,
        );
    }
    Ok(out)
}

/// 库存期初该落哪个库位桶（**FIX-4 问题 1**）：与收货过账同口径 —— 仓库默认库位桶。
///
/// 返回 `None` = 落「未分配库位桶」（该仓没有默认库位 / 默认库位已停用），
/// 与收货过账的兜底逐字一致（详情里会提示「先设一个默认库位」）。
///
/// 库位账行走 [`stockin_model::ensure_location_row`] find-or-create：期初不需要用户
/// 先在收货表单里造出「某物料的库存账行」，与 P2-4 的收货路径共用同一个原语。
async fn opening_location_row(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    now: DateTime<Utc>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    let Some(dict) = stockin_model::default_location_in(ex, warehouse_id)
        .await
        .map_err(|error| write_error(error, "查询仓库默认库位"))?
    else {
        return Ok(None);
    };
    // 停用的默认库位不接受新入库（与收货 `plan_lines` 的判据一致）
    if dict.status != 1 {
        return Ok(None);
    }
    stockin_model::ensure_location_row(ex, material_id, warehouse_id, dict.id, now)
        .await
        .map(Some)
        .map_err(|error| write_error(error, "库存期初按默认库位建账"))
}

/// 保存库存期初（事务内实现，单测可直接调用）。
///
/// # 覆盖语义（卡面 ③）
///
/// 库存期初**不是**「删掉旧行再插新行」这种纯表内操作 —— 它已经把数量加进了
/// `biz_inventory` / `biz_inventory_location`，还写了流水。所以覆盖要**按差额调整**：
///
/// ```text
/// Δqty    = 新数量 - 旧数量
/// Δamount = 新金额 - 旧金额
/// Δqty = 0 且 Δamount = 0  → 不动（不写流水）
/// Δqty > 0 → 流水 IN ；Δqty < 0 → 流水 OUT（ck_biz_inv_txn_qty 要求 qty > 0）
/// ```
///
/// 差额法的好处：期初之后发生的收货/出库不会被覆盖动作抹掉（它们的增减都叠加在
/// 同一个库存行上，按差额调整只会挪动期初那一份）。
///
/// # 期初落哪个库位桶（FIX-4 问题 1）
///
/// 修复前：期初一律写 `location_id = NULL` 的「未分配库位桶」，而收货过账写
/// 「仓库默认库位桶」（`biz_location.is_default = 1`）—— 同一个仓同一个物料两桶并存，
/// 这正是「分库位库存永久漂移」的触发条件（汇总不变式只比合计，对错位无感）。
///
/// 现在与收货**同一口径**：落该仓的默认库位桶（经 `stockin_model::ensure_location_row`
/// find-or-create 出库位账行）；该仓没有默认库位时仍落「未分配」桶（与收货的兜底一致，
/// 详情里会提示先设默认库位）。
///
/// 重录 / 取消必须落**同一个桶**：已记录在 `biz_opening_inventory.location_row_id` 上的
/// 桶优先（V0324 新增列），Δ 才冲得准；只有新行 / 存量无记录的旧行才按「当前默认库位」解析。
pub async fn save_inventory_in(
    tx: &impl SqlExecutor,
    period_code: &str,
    items: &[OpeningInventoryItemReq],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<OpeningBalanceRes, ApiError> {
    lock_period(tx, period_code).await?;
    // 库存期初会重算整张期初凭证（含科目侧），所以同样不许碰「年结生成」的期间。
    guard_not_year_end(tx, period_code).await?;

    let mut seen = std::collections::BTreeSet::new();
    let mut flat: Vec<FlatInventory> = Vec::with_capacity(items.len());
    for raw in items {
        flat.push(resolve_inventory_item(tx, raw, &mut seen).await?);
    }

    // 旧行 → 差额
    let old_rows = load_inventory_rows(tx, period_code)
        .await
        .map_err(|error| write_error(error, "读旧库存期初"))?;
    let mut old_map: std::collections::BTreeMap<(Uuid, Uuid), (Decimal, Decimal)> = std::collections::BTreeMap::new();
    for row in &old_rows {
        let material_id = Uuid::parse_str(&row.material_id)
            .map_err(|_| DbError::InvalidValue(format!("旧库存期初的物料 id 非法：{}", row.material_id)))?;
        let warehouse_id = Uuid::parse_str(&row.warehouse_id)
            .map_err(|_| DbError::InvalidValue(format!("旧库存期初的仓库 id 非法：{}", row.warehouse_id)))?;
        old_map.insert((material_id, warehouse_id), (row.qty, row.amount));
    }

    // 互斥：方式 A 里已经有 1405 借方 / 对方科目贷方时，库存期初会重复计数
    let equity_code = inventory_equity_account_code().await;
    let period_entries = load_balance_rows(tx, period_code)
        .await
        .map_err(|error| write_error(error, "读科目期初"))?;
    if !flat.is_empty() {
        if let Some(row) = period_entries
            .iter()
            .find(|row| row.account_code == ACCOUNT_INVENTORY && row.debit > Decimal::ZERO)
        {
            return Err(bad_request(format!(
                "科目 {} 已有手工期初 {}（借方）：库存期初会自动生成 借{}，两个口径会重复计数。\
                 请先删掉科目期初里的 1405 那一行，再录库存期初",
                ACCOUNT_INVENTORY, row.debit, ACCOUNT_INVENTORY
            )));
        }
        if let Some(row) = period_entries
            .iter()
            .find(|row| row.account_code == equity_code && row.credit > Decimal::ZERO)
        {
            return Err(bad_request(format!(
                "科目 {} 已有手工期初 {}（贷方）：库存期初会自动生成 贷{}（对方科目由系统参数 {} \
                 指定），两个口径会重复计数。请先删掉科目期初里的 {} 那一行，再录库存期初",
                equity_code, row.credit, equity_code, CONFIG_INVENTORY_EQUITY_ACCOUNT, equity_code
            )));
        }
    }

    // 差额落地：三本账 + 流水
    let doc_no = format!("{SOURCE_NO_PREFIX}{period_code}");
    // 本期已有期初行落在哪个库位桶（重录要冲同一个桶，见函数文档「期初落哪个库位桶」）
    let old_buckets = load_opening_buckets(tx, period_code)
        .await
        .map_err(|error| write_error(error, "读旧库存期初的库位桶"))?;
    let mut new_keys: std::collections::BTreeSet<(Uuid, Uuid)> = std::collections::BTreeSet::new();
    let mut bucket_of: std::collections::BTreeMap<(Uuid, Uuid), Option<Uuid>> = std::collections::BTreeMap::new();
    for row in &flat {
        let key = (row.material_id, row.warehouse_id);
        new_keys.insert(key);
        let (old_qty, old_amount) = old_map.get(&key).copied().unwrap_or((Decimal::ZERO, Decimal::ZERO));
        let delta_qty = row.qty - old_qty;
        let delta_amount = row.amount - old_amount;
        // 桶：已记录的沿用（Δ 冲回同一个桶）；新行才按「仓库默认库位桶」解析。
        // 零差额不解析 —— 解析会 find-or-create 出一条 0 数量的库位账行，
        // 白白把「库位库存」Tab 撑出空行（与收货过账「不落空账行」同口径）。
        let location_row = match old_buckets.get(&key).copied().flatten() {
            Some(row_id) => Some(row_id),
            None if delta_qty.is_zero() && delta_amount.is_zero() => None,
            None => opening_location_row(tx, key.0, key.1, now).await?,
        };
        bucket_of.insert(key, location_row);
        if delta_qty.is_zero() && delta_amount.is_zero() {
            continue;
        }
        let cost = CostBasis {
            qty: delta_qty,
            amount: delta_amount,
            unit_cost: row.unit_cost,
        };
        let applied = stockin_model::apply_cost(tx, row.material_id, row.warehouse_id, location_row, cost, now)
            .await
            .map_err(|error| write_error(error, "库存期初写入成本账"))?;
        stockin_model::apply_location_qty(tx, row.material_id, row.warehouse_id, location_row, delta_qty, now)
            .await
            .map_err(|error| write_error(error, "库存期初写入库位账"))?;

        let txn_type = if delta_qty >= Decimal::ZERO { "IN" } else { "OUT" };
        // P2-5A：流水 SQL 收口到唯一写入口 [`inventory_writer::insert_txn`]。
        let remark = format!("库存期初（{doc_no}）");
        inventory_writer::insert_txn(
            tx,
            &TxnRecord {
                material_id: row.material_id,
                warehouse_id: row.warehouse_id,
                txn_type,
                biz_type: BIZ_TYPE_OPENING,
                doc_type: DOC_TYPE_OPENING,
                doc_id: Uuid::now_v7(),
                doc_no: doc_no.as_str(),
                line_no: 1,
                qty: delta_qty.abs().round_dp(4),
                unit_cost: row.unit_cost,
                amount: delta_amount.abs().round_dp(4),
                before: applied.before,
                after: applied.after,
                operator,
                period_code: Some(period_code),
                remark: Some(remark.as_str()),
                now,
            },
        )
        .await
        .map_err(|error| write_error(error, "库存期初写入流水"))?;
    }

    // 旧期初里被删掉的行：数量与金额按差额退回（Δ = 0 - 旧值）
    for ((material_id, warehouse_id), (old_qty, old_amount)) in &old_map {
        if new_keys.contains(&(*material_id, *warehouse_id)) {
            continue;
        }
        let unit_cost = if old_qty.is_zero() {
            Decimal::ZERO
        } else {
            (*old_amount / *old_qty).round_dp(6)
        };
        // 退回**同一只桶**（与写入时同一个判据：已记录优先，否则按当前默认库位解析）
        let location_row = match old_buckets.get(&(*material_id, *warehouse_id)).copied().flatten() {
            Some(row_id) => Some(row_id),
            None => opening_location_row(tx, *material_id, *warehouse_id, now).await?,
        };
        let cost = CostBasis {
            qty: -*old_qty,
            amount: -*old_amount,
            unit_cost,
        };
        let applied = stockin_model::apply_cost(tx, *material_id, *warehouse_id, location_row, cost, now)
            .await
            .map_err(|error| write_error(error, "库存期初退回成本账"))?;
        stockin_model::apply_location_qty(tx, *material_id, *warehouse_id, location_row, -*old_qty, now)
            .await
            .map_err(|error| write_error(error, "库存期初退回库位账"))?;
        // P2-5A：流水 SQL 收口到唯一写入口 [`inventory_writer::insert_txn`]。
        let remark = format!("库存期初取消（{doc_no}）");
        inventory_writer::insert_txn(
            tx,
            &TxnRecord {
                material_id: *material_id,
                warehouse_id: *warehouse_id,
                txn_type: "OUT",
                biz_type: BIZ_TYPE_OPENING,
                doc_type: DOC_TYPE_OPENING,
                doc_id: Uuid::now_v7(),
                doc_no: doc_no.as_str(),
                line_no: 1,
                qty: old_qty.abs().round_dp(4),
                unit_cost,
                amount: old_amount.abs().round_dp(4),
                before: applied.before,
                after: applied.after,
                operator,
                period_code: Some(period_code),
                remark: Some(remark.as_str()),
                now,
            },
        )
        .await
        .map_err(|error| write_error(error, "库存期初取消写入流水"))?;
    }

    // 覆盖台账
    tx.exec_sql(
        format!("DELETE FROM {TABLE_INVENTORY} WHERE period_code = ?"),
        vec![Value::Text(period_code.to_string())],
    )
    .await
    .map_err(|error| write_error(error, "清空旧库存期初"))?;
    for row in &flat {
        tx.exec_sql(
            format!(
                "INSERT INTO {TABLE_INVENTORY} \
                    (period_code, material_id, warehouse_id, qty, unit_cost, amount, remark, \
                     location_row_id, created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, \
                         NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                         NULLIF(?::text,''), NULLIF(?::text,'')::uuid, ?, ?, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::uuid)"
            ),
            vec![
                Value::Text(period_code.to_string()),
                Value::from(&row.material_id),
                Value::from(&row.warehouse_id),
                Value::Text(row.qty.to_string()),
                Value::Text(row.unit_cost.to_string()),
                Value::Text(row.amount.to_string()),
                Value::Text(row.remark.clone().unwrap_or_default()),
                // 期初落在哪个库位桶：重录 / 取消时按它冲回同一个桶（FIX-4 问题 1）
                uuid_text(bucket_of.get(&(row.material_id, row.warehouse_id)).copied().flatten()),
                Value::from(now),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "写入库存期初"))?;
    }

    // 期初凭证（科目期初 + 新库存期初，同一事务）
    let inventory_amount = flat.iter().fold(Decimal::ZERO, |sum, row| sum + row.amount);
    let accounts = if inventory_amount > Decimal::ZERO {
        Some(inventory_accounts(tx).await?)
    } else {
        None
    };
    let mut pairs: Vec<(AccountRef, Decimal, Decimal)> = Vec::new();
    for row in &period_entries {
        let account = voucher::account_by_code(tx, &row.account_code)
            .await
            .map_err(|error| write_error(error, "解析科目期初的科目"))?
            .ok_or_else(|| {
                bad_request(format!(
                    "科目 {} 已被删除，但它还有期初余额：请先在科目期初里删掉这一行",
                    row.account_code
                ))
            })?;
        pairs.push((account, row.debit, row.credit));
    }
    // 库存期初与手工 1405/3103 互斥已在上面拦过，这里再按合并口径算一次平衡
    let flat_for_check: Vec<FlatEntry> = pairs
        .iter()
        .map(|(account, debit, credit)| FlatEntry {
            account: account.clone(),
            debit: *debit,
            credit: *credit,
            remark: None,
            profit_loss: false,
        })
        .collect();
    ensure_balanced(&flat_for_check, inventory_amount, &accounts)?;

    let lines = build_lines(&pairs, inventory_amount, accounts);
    regenerate_voucher_in(tx, period_code, lines, operator, now).await?;

    load_opening_in(tx, period_code).await
}

// ==================== 接口层 ====================

/// 期初余额总览。
#[nik::permission(
    path = "/business/erp/opening-balance",
    method = "GET",
    permission = "business:erp:opening:read"
)]
#[nik::get("/business/erp/opening-balance")]
pub async fn opening_get_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<OpeningQuery>,
) -> ResResult<OpeningBalanceRes> {
    period::ensure_permission(&claims, PERM_READ, "查看期初余额")?;
    let period_code = validate_period(query.period.as_deref())?;
    Ok(Res::ok(load_opening_in(db(), &period_code).await?))
}

/// 保存科目期初（方式 A）。
#[nik::permission(
    path = "/business/erp/opening-balance",
    method = "POST",
    permission = "business:erp:opening:write"
)]
#[nik::post("/business/erp/opening-balance")]
pub async fn opening_save_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SaveOpeningReq>,
) -> ResResult<OpeningBalanceRes> {
    period::ensure_permission(&claims, PERM_WRITE, "录入科目期初")?;
    Ok(Res::ok(
        save_opening(req.period.as_deref(), &req.entries, utils::current_user_uuid(&claims)).await?,
    ))
}

/// 保存库存期初（方式 B 的第一项）。
#[nik::permission(
    path = "/business/erp/opening-balance/inventory",
    method = "POST",
    permission = "business:erp:opening:inventory"
)]
#[nik::post("/business/erp/opening-balance/inventory")]
pub async fn opening_inventory_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SaveOpeningInventoryReq>,
) -> ResResult<OpeningBalanceRes> {
    period::ensure_permission(&claims, PERM_INVENTORY, "录入库存期初")?;
    Ok(Res::ok(
        save_inventory(req.period.as_deref(), &req.items, utils::current_user_uuid(&claims)).await?,
    ))
}
// ==================== 单测（真库，15 条）====================
//
// 需要真库：`ERP_DB_URL` 指向「已应用 V0137–V0189 的隔离库」；缺省则全部 SKIP：
//
// ```bash
// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_r5'
// cargo test --lib business::erp::opening -- --test-threads=1
// ```
//
// # 夹具约定（两条都不能破）
//
// 1. 期间编码一律 `2034-xx`（生产在用 `2026-xx`，period 模块的夹具在 `2031–2033`）。
//    **绝不碰 2026-xx**：别的模块（report / income_statement / voucher_write）的夹具
//    往 2026-10 记凭证，本模块把它的状态改了就成跨模块污染。
// 2. 串行（`DB_LOCK`）：清理按前缀删，并行会互删夹具。
//
// 夹具前缀：物料 `R5-M-`、仓库 `R5-WH`、期间 `2034-`。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::fixture_hygiene;
    use crate::business::erp::income_statement;
    use crate::business::erp::report;
    use crate::business::shared::state_machine;
    use crate::support::config::model::reload_runtime_snapshot;
    use std::str::FromStr;

    /// 真库用例串行跑。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    /// 夹具操作人（`create_by` 是弱引用，不要求真实用户）。
    const TEST_UID: &str = "00000000-0000-7000-8000-0000000000b5";

    /// 夹具操作人 id。
    fn uid() -> Uuid {
        Uuid::parse_str(TEST_UID).expect("测试 uuid 非法")
    }

    /// 夹具操作人（开账用）。
    fn operator() -> state_machine::Operator {
        state_machine::Operator::named(Some(uid()), "r5-tester")
    }

    /// 金额字面量。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    /// 一条科目期初。
    fn entry(code: &str, debit: &str, credit: &str) -> OpeningEntryReq {
        OpeningEntryReq {
            account_code: Some(code.to_string()),
            debit: Some(dec(debit)),
            credit: Some(dec(credit)),
            remark: None,
        }
    }

    /// 一条库存期初。
    fn item(material: Uuid, warehouse: Uuid, qty: &str, cost: &str) -> OpeningInventoryItemReq {
        OpeningInventoryItemReq {
            material_id: Some(material.to_string()),
            warehouse_id: Some(warehouse.to_string()),
            qty: Some(dec(qty)),
            unit_cost: Some(dec(cost)),
            remark: None,
        }
    }

    /// 连库（只从 `ERP_DB_URL` 取；缺省 SKIP）。
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

    /// 执行一条写 SQL（失败直接 panic，不静默吞错）。
    async fn sql(text: &str) {
        if let Err(err) = db().execute_with_params(text, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {text}");
        }
    }

    /// 清掉本模块造的所有夹具（按前缀收敛，**不碰 2026-xx 的生产期间**）。
    async fn cleanup() {
        // FIX-6 夹具可能把「期初对方科目」写成 3101（用例中途 panic 不会走还原）：
        // 只回退这个精确值，运维手工配的别的科目一律不动。
        sql(&format!(
            "UPDATE sys_config SET config_value = '{ACCOUNT_INVENTORY_OFFSET}' \
              WHERE config_key = '{CONFIG_INVENTORY_EQUITY_ACCOUNT}' \
                AND config_value = '3101' AND deleted_at IS NULL"
        ))
        .await;
        // 状态日志没有外键，必须按期间编码先删（期间行一删就查不到 id）
        sql("DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' \
             AND (entity_no LIKE '2034-%' OR entity_no LIKE '2035-%')")
        .await;
        // 分录随凭证 CASCADE
        // 凭证的状态日志（无外键）：必须在删凭证之前按 id 清，否则每轮留一批孤儿（`fixture_hygiene`）
        sql("DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' \
             AND entity_id IN (SELECT id FROM biz_voucher \
                                WHERE period LIKE '2034-%' OR period LIKE '2035-%')")
        .await;
        sql("DELETE FROM biz_voucher WHERE period LIKE '2034-%' OR period LIKE '2035-%'").await;
        sql("DELETE FROM biz_opening_balance WHERE period_code LIKE '2034-%' \
             OR period_code LIKE '2035-%'")
        .await;
        sql("DELETE FROM biz_opening_inventory WHERE period_code LIKE '2034-%' \
             OR period_code LIKE '2035-%'")
        .await;
        // 库存三本账按夹具物料收敛（biz_inventory 无 deleted_at，物理删）。
        // FK 顺序（FIX-4 起期初会落「默认库位桶」，成本账的 location_id 指向库位账行）：
        // 流水（无外键）→ **成本账先删**（它 FK 指向库位账行）→ 库位账 → 物料
        sql("DELETE FROM biz_inventory_txn WHERE material_id IN \
             (SELECT id FROM biz_material WHERE material_code LIKE 'R5-%')")
        .await;
        sql("DELETE FROM biz_inventory WHERE material_id IN \
             (SELECT id FROM biz_material WHERE material_code LIKE 'R5-%')")
        .await;
        sql("DELETE FROM biz_inventory_location WHERE material_id IN \
             (SELECT id FROM biz_material WHERE material_code LIKE 'R5-%')")
        .await;
        sql("DELETE FROM biz_material WHERE material_code LIKE 'R5-%'").await;
        // FIX-4 夹具库位（默认库位桶用例造的）：biz_location 对 warehouse 是弱引用，先按库位编码清
        sql("DELETE FROM biz_location WHERE location_code LIKE 'R5-LOC-%'").await;
        sql("DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'R5-%'").await;
        sql("DELETE FROM biz_accounting_period WHERE period_code LIKE '2034-%' \
             OR period_code LIKE '2035-%'")
        .await;
    }

    /// 取锁 + 连库 + 清场。`None` = 没有测试库，用例直接 return。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 开账（生产同款入口）。
    async fn open_month(month: i16) {
        open_at(2034, month).await;
    }

    /// 按年月开账。
    async fn open_at(year: i16, month: i16) {
        period::open_period(year, month, Some("R5 夹具"), operator())
            .await
            .unwrap_or_else(|err| panic!("开账 {year}-{month:02} 失败：{} {}", err.code, err.msg));
    }

    /// 直接把期间置为已结账（只改状态，不走结账预检 —— 本卡测的是期初侧的闸）。
    async fn close_month_direct(month: i16) {
        sql(&format!(
            "UPDATE biz_accounting_period SET status = 'CLOSED' WHERE period_code = '2034-{month:02}'"
        ))
        .await;
    }

    /// 建一个夹具物料（`serial` = 一物一码）。
    async fn make_material(code: &str, serial: bool) -> Uuid {
        let unit: Uuid = db()
            .scalar_with_params("SELECT id FROM biz_unit LIMIT 1", vec![])
            .await
            .expect("取夹具计量单位");
        let id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                      serial_required, status) \
             VALUES (?::uuid, ?, ?, ?::uuid, 'RAW', ?, 1)",
            vec![
                Value::from(&id),
                Value::Text(code.to_string()),
                Value::Text(format!("R5 夹具 {code}")),
                Value::from(&unit),
                Value::from(serial),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("建夹具物料失败：{err}"));
        id
    }

    /// 建一个夹具仓库。
    async fn make_warehouse(code: &str) -> Uuid {
        let id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, \
                                        status, warehouse_path) \
             VALUES (?::uuid, ?, ?, 'NORMAL', 1, ?)",
            vec![
                Value::from(&id),
                Value::Text(code.to_string()),
                Value::Text(format!("R5 夹具仓 {code}")),
                Value::Text(format!("/{id}/")),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("建夹具仓库失败：{err}"));
        id
    }

    /// 统计期间内某类凭证的数量。
    async fn voucher_count(period_code: &str, kind: &str) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_voucher WHERE period = ? AND voucher_type = ?",
            vec![Value::Text(period_code.to_string()), Value::Text(kind.to_string())],
        )
        .await
        .expect("统计凭证")
    }

    /// 科目期初行数。
    async fn balance_rows(period_code: &str) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_opening_balance WHERE period_code = ?",
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .expect("统计科目期初")
    }

    /// 库存期初行数。
    async fn inventory_rows(period_code: &str) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_opening_inventory WHERE period_code = ?",
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .expect("统计库存期初")
    }

    /// 库存成本账的一行 `(qty, amount, avg_cost)`。
    async fn cost_row(material: Uuid, warehouse: Uuid) -> Option<(Decimal, Decimal, Decimal)> {
        let rows = db()
            .find_with_params(
                "SELECT qty, amount, avg_cost FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
                vec![Value::from(&material), Value::from(&warehouse)],
            )
            .await
            .expect("读成本账");
        rows.first().map(|row| {
            (
                row.get_typed("qty").expect("qty"),
                row.get_typed("amount").expect("amount"),
                row.get_typed("avg_cost").expect("avg_cost"),
            )
        })
    }

    /// 库位账（未分配库位桶）数量。
    async fn location_qty(material: Uuid, warehouse: Uuid) -> Decimal {
        db().scalar_with_params(
            "SELECT COALESCE(SUM(qty), 0)::numeric(18,4) FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![Value::from(&material), Value::from(&warehouse)],
        )
        .await
        .expect("读库位账")
    }

    /// 库存流水条数（按业务类型）。
    async fn txn_count(material: Uuid, biz_type: &str) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_inventory_txn WHERE material_id = ?::uuid AND biz_type = ?",
            vec![Value::from(&material), Value::Text(biz_type.to_string())],
        )
        .await
        .expect("统计流水")
    }

    /// 期初对方科目的夹具值（FIX-6 可配用例）——写 3101，测完还原 3103。
    ///
    /// 写完必须 `reload_runtime_snapshot`：`SysConfigModel::read_text` 优先读**进程内快照**，
    /// 同进程里别的用例可能已经把快照装载过，只改库不重载会读到旧值（假绿/假红都出现过）。
    async fn set_equity_config(value: &str) {
        sql(&format!(
            "INSERT INTO sys_config (config_key, config_value, config_type, description, category, \
                                    config_level, is_modifiable, status) \
             VALUES ('{CONFIG_INVENTORY_EQUITY_ACCOUNT}', '{value}', 'STRING', \
                     '库存期初对方科目（FIX-6 夹具）', 'erp', 2, TRUE, 1) \
             ON CONFLICT (config_key) WHERE deleted_at IS NULL \
             DO UPDATE SET config_value = EXCLUDED.config_value"
        ))
        .await;
        reload_runtime_snapshot()
            .await
            .expect("本夹具改完参数必须能重载运行时快照");
    }

    /// 还原期初对方科目为默认 3103（只回退本夹具写过的 3101，不动运维手工配置）。
    async fn restore_equity_config() {
        sql(&format!(
            "UPDATE sys_config SET config_value = '{ACCOUNT_INVENTORY_OFFSET}' \
              WHERE config_key = '{CONFIG_INVENTORY_EQUITY_ACCOUNT}' \
                AND config_value = '3101' AND deleted_at IS NULL"
        ))
        .await;
        let _ = reload_runtime_snapshot().await;
    }

    /// 期初凭证里某科目的借方合计。
    async fn voucher_entry_of(period_code: &str, account_code: &str, side: &str) -> Decimal {
        let column = if side == "debit" {
            "debit_amount"
        } else {
            "credit_amount"
        };
        db().scalar_with_params(
            &format!(
                "SELECT COALESCE(SUM(e.{column}), 0)::numeric(20,4) \
                   FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
                  WHERE v.period = ? AND v.voucher_type = 'OPENING' AND e.account_code = ?"
            ),
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(account_code.to_string()),
            ],
        )
        .await
        .expect("读期初凭证分录")
    }

    // ==================== ① 录入 ====================

    /// ① 借贷平衡 → 保存成功（并生成一张期初凭证）。
    #[tokio::test]
    async fn r5_01_balanced_save_succeeds() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(1).await;

        let res = save_opening(
            Some("2034-01"),
            &[entry("1002", "50000", "0"), entry("3001", "0", "50000")],
            Some(uid()),
        )
        .await
        .expect("平衡的期初应当保存成功");

        assert_eq!(res.state, STATE_ENTERED, "录过之后状态是 ENTERED");
        assert!(res.balanced, "借贷平衡");
        assert_eq!(res.debit_total, dec("50000"));
        assert_eq!(res.credit_total, dec("50000"));
        assert_eq!(res.entries.len(), 2);
        assert!(res.voucher.is_some(), "平衡的期初要生成期初凭证");
        assert_eq!(balance_rows("2034-01").await, 2);
    }

    /// ② 借贷不平 → 400 + 差额 + 点名两边科目（卡面 O5）。
    #[tokio::test]
    async fn r5_02_unbalanced_rejected_with_difference() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(2).await;

        let err = save_opening(
            Some("2034-02"),
            &[entry("1002", "50000", "0"), entry("3001", "0", "40000")],
            Some(uid()),
        )
        .await
        .expect_err("借贷不平必须被拒");

        assert_eq!(err.status, 400, "不平衡是参数错误");
        assert_eq!(
            err.code,
            error_code::ERP_OPENING_UNBALANCED,
            "用独立的 66012，前端才能按码渲染差额"
        );
        assert!(err.msg.contains("10000"), "消息必须报差额：{}", err.msg);
        assert!(err.msg.contains("1002"), "消息要点名借方科目：{}", err.msg);
        assert!(err.msg.contains("3001"), "消息要点名贷方科目：{}", err.msg);
        assert_eq!(balance_rows("2034-02").await, 0, "被拒的期初一行都不该落库");
        assert_eq!(voucher_count("2034-02", VOUCHER_TYPE_OPENING).await, 0);
    }

    /// ③ 重复录入同一期间 → 覆盖（不新增第二张期初凭证，凭证号不变）。
    #[tokio::test]
    async fn r5_03_repeat_save_overwrites_single_voucher() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(3).await;

        let first = save_opening(
            Some("2034-03"),
            &[entry("1002", "50000", "0"), entry("3001", "0", "50000")],
            Some(uid()),
        )
        .await
        .expect("第一次保存");
        let first_no = first.voucher.as_ref().expect("第一次就该有期初凭证").voucher_no.clone();
        let first_id = first.voucher.as_ref().map(|v| v.id.clone()).unwrap();

        let second = save_opening(
            Some("2034-03"),
            &[entry("1002", "60000", "0"), entry("3001", "0", "60000")],
            Some(uid()),
        )
        .await
        .expect("第二次保存（覆盖式）");

        assert_eq!(
            voucher_count("2034-03", VOUCHER_TYPE_OPENING).await,
            1,
            "硬红线 2：一个期间只有一张期初凭证"
        );
        let voucher = second.voucher.as_ref().expect("覆盖后仍有期初凭证");
        assert_eq!(voucher.voucher_no, first_no, "覆盖不换号");
        assert_eq!(voucher.id, first_id, "覆盖的是同一张凭证");
        assert_eq!(voucher.total_debit, dec("60000"), "金额被覆盖成新值");
        assert_eq!(voucher.entry_count, 2, "分录整组替换，不是累加");
        assert_eq!(balance_rows("2034-03").await, 2, "科目期初也是覆盖，不是新增");
    }

    /// ④ 结账后录入 → 409。
    #[tokio::test]
    async fn r5_04_closed_period_rejected() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(4).await;
        close_month_direct(4).await;

        let err = save_opening(
            Some("2034-04"),
            &[entry("1002", "100", "0"), entry("3001", "0", "100")],
            Some(uid()),
        )
        .await
        .expect_err("已结账期间不许改期初");

        assert_eq!(err.status, 409, "结账后是状态冲突（不是 400）");
        assert!(err.msg.contains("已结账"), "消息要点明已结账：{}", err.msg);
        assert_eq!(balance_rows("2034-04").await, 0);

        // 库存期初同样被拦
        let material = make_material("R5-M-004", false).await;
        let warehouse = make_warehouse("R5-WH1").await;
        let err = save_inventory(Some("2034-04"), &[item(material, warehouse, "10", "10")], Some(uid()))
            .await
            .expect_err("已结账期间的库存期初同样不许改");
        assert_eq!(err.status, 409);
        assert!(cost_row(material, warehouse).await.is_none(), "一分库存都不该动");
    }

    // ==================== ② 期初凭证 ====================

    /// ⑤ 期初生成一张 `voucher_type='OPENING'` 的凭证（日期 = 期间第一天）。
    #[tokio::test]
    async fn r5_05_opening_voucher_type_and_date() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(5).await;
        save_opening(
            Some("2034-05"),
            &[entry("1002", "50000", "0"), entry("3001", "0", "50000")],
            Some(uid()),
        )
        .await
        .expect("保存");

        let rows = db()
            .find_with_params(
                "SELECT voucher_type, voucher_date, summary, source_type, source_no, status \
                   FROM biz_voucher WHERE period = ?",
                vec![Value::Text("2034-05".to_string())],
            )
            .await
            .expect("读凭证");
        assert_eq!(rows.len(), 1, "该期间只有一张期初凭证");
        let row = &rows[0];
        assert_eq!(row.get_typed::<String>("voucher_type").unwrap(), VOUCHER_TYPE_OPENING);
        assert_eq!(
            row.get_typed::<NaiveDate>("voucher_date").unwrap().to_string(),
            "2034-05-01",
            "期初凭证日期 = 期间第一天"
        );
        assert_eq!(row.get_typed::<String>("summary").unwrap(), SUMMARY_OPENING);
        assert_eq!(row.get_typed::<String>("source_type").unwrap(), SOURCE_TYPE_OPENING);
        assert_eq!(row.get_typed::<String>("source_no").unwrap(), "OPENING-2034-05");
        assert_eq!(row.get_typed::<String>("status").unwrap(), "POSTED");
    }

    /// ⑥ 该凭证借贷平衡（单头 + 分录两侧都平）。
    #[tokio::test]
    async fn r5_06_opening_voucher_is_balanced() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(6).await;
        save_opening(
            Some("2034-06"),
            &[
                entry("1002", "30000", "0"),
                entry("1405", "20000", "0"),
                entry("3001", "0", "50000"),
            ],
            Some(uid()),
        )
        .await
        .expect("保存");

        let row = db()
            .find_with_params(
                "SELECT v.voucher_no, v.total_debit, v.total_credit, v.entry_count, \
                        COALESCE(SUM(e.debit_amount), 0)::numeric(20,4) AS entry_debit, \
                        COALESCE(SUM(e.credit_amount), 0)::numeric(20,4) AS entry_credit \
                   FROM biz_voucher v LEFT JOIN biz_voucher_entry e ON e.voucher_id = v.id \
                  WHERE v.period = ? AND v.voucher_type = 'OPENING' \
                  GROUP BY v.id, v.voucher_no, v.total_debit, v.total_credit, v.entry_count",
                vec![Value::Text("2034-06".to_string())],
            )
            .await
            .expect("读凭证")
            .into_iter()
            .next()
            .expect("应有期初凭证");

        let total_debit: Decimal = row.get_typed("total_debit").unwrap();
        let total_credit: Decimal = row.get_typed("total_credit").unwrap();
        let entry_debit: Decimal = row.get_typed("entry_debit").unwrap();
        let entry_credit: Decimal = row.get_typed("entry_credit").unwrap();
        assert_eq!(total_debit, dec("50000"));
        assert_eq!(total_debit, total_credit, "单头借贷平衡");
        assert_eq!(entry_debit, entry_credit, "分录借贷平衡");
        assert_eq!(entry_debit, total_debit, "分录合计 = 单头合计（A2 不变量的口径）");
        assert_eq!(row.get_typed::<i32>("entry_count").unwrap(), 3);
    }

    /// ⑦ 期初凭证不计入本期发生额（利润表看不到，但「期末累计」含它）。
    #[tokio::test]
    async fn r5_07_opening_excluded_from_income_statement() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(7).await;
        // 期初：借 1002 货币资金 1000 / 贷 5001 主营业务收入 1000
        // （损益类录期初不合理，但正是这条用例要证明的口径边界）
        // 基线：5001 的「期末累计」在录期初之前是多少（别的模块的用例也可能动过 5001）
        let base_cum = {
            let tx = db().begin().await.expect("开启基线事务");
            let rows = income_statement::build_snapshot(&tx, "2034-07")
                .await
                .expect("基线利润表");
            let _ = tx.rollback().await;
            rows.accounts
                .iter()
                .find(|row| row.account_code == "5001")
                .map(|row| row.net_cum)
                .unwrap_or_default()
        };
        save_opening(
            Some("2034-07"),
            &[entry("1002", "1000", "0"), entry("5001", "0", "1000")],
            Some(uid()),
        )
        .await
        .expect("保存");

        let tx = db().begin().await.expect("开启报表事务");
        let snap = income_statement::build_snapshot(&tx, "2034-07")
            .await
            .expect("利润表应能算出");
        let _ = tx.rollback().await;

        let revenue = snap
            .computed
            .items
            .iter()
            .find(|row| row.item_code == "REVENUE")
            .expect("利润表必须有营业收入行");
        assert_eq!(revenue.current, Decimal::ZERO, "硬红线 3：期初不进「本期发生额」");
        assert_eq!(revenue.ytd, Decimal::ZERO, "硬红线 3：期初不进「本年累计」");

        let account = snap
            .accounts
            .iter()
            .find(|row| row.account_code == "5001")
            .expect("5001 在科目表里");
        assert_eq!(account.net_period, Decimal::ZERO, "本期发生额 = 0");
        assert_eq!(
            account.net_cum - base_cum,
            dec("-1000"),
            "期末累计含期初（贷方 1000 → 净额 -1000，增量口径）"
        );
    }

    // ==================== ③ 库存期初 ====================

    /// ⑧ 录 100 件 × 10 元 → biz_inventory qty=100 amount=1000 avg_cost=10。
    #[tokio::test]
    async fn r5_08_inventory_opening_writes_cost_book() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(8).await;
        let material = make_material("R5-M-008", false).await;
        let warehouse = make_warehouse("R5-WH1").await;

        let res = save_inventory(Some("2034-08"), &[item(material, warehouse, "100", "10")], Some(uid()))
            .await
            .expect("库存期初应保存成功");

        let (qty, amount, avg_cost) = cost_row(material, warehouse).await.expect("成本账应有一行");
        assert_eq!(qty, dec("100"));
        assert_eq!(amount, dec("1000"));
        assert_eq!(avg_cost, dec("10"));
        assert_eq!(res.inventory_amount, dec("1000"));
        assert_eq!(res.inventory.len(), 1);
        assert_eq!(inventory_rows("2034-08").await, 1);
    }

    /// ⑨ 同时写库位账 + 流水（OPENING 类型）。
    #[tokio::test]
    async fn r5_09_inventory_opening_writes_location_and_txn() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(9).await;
        let material = make_material("R5-M-009", false).await;
        let warehouse = make_warehouse("R5-WH1").await;

        save_inventory(Some("2034-09"), &[item(material, warehouse, "100", "10")], Some(uid()))
            .await
            .expect("库存期初应保存成功");

        assert_eq!(
            location_qty(material, warehouse).await,
            dec("100"),
            "库位账（未分配库位桶）必须同步"
        );
        let rows = db()
            .find_with_params(
                "SELECT txn_type, biz_type, doc_type, doc_no, qty, amount, period_code, \
                        after_qty, after_amount \
                   FROM biz_inventory_txn WHERE material_id = ?::uuid",
                vec![Value::from(&material)],
            )
            .await
            .expect("读流水");
        assert_eq!(rows.len(), 1, "一条期初只写一条流水");
        let row = &rows[0];
        assert_eq!(row.get_typed::<String>("txn_type").unwrap(), "IN");
        assert_eq!(row.get_typed::<String>("biz_type").unwrap(), BIZ_TYPE_OPENING);
        assert_eq!(row.get_typed::<String>("doc_type").unwrap(), DOC_TYPE_OPENING);
        assert_eq!(row.get_typed::<String>("doc_no").unwrap(), "OPENING-2034-09");
        assert_eq!(row.get_typed::<Decimal>("qty").unwrap(), dec("100"));
        assert_eq!(row.get_typed::<Decimal>("amount").unwrap(), dec("1000"));
        assert_eq!(row.get_typed::<String>("period_code").unwrap(), "2034-09");
    }

    // ========================================================================
    // FIX-4 问题 1（t_763677cd）：期初落「默认库位桶」，与收货过账同一个桶
    //
    // 修复前期初一律写 `location_id IS NULL` 的「未分配库位桶」，收货过账写
    // 「仓库默认库位桶」⇒ 同仓同物料两桶并存，「分库位库存永久漂移」的触发条件。
    // 汇总不变式只比合计，对这种错位完全无感（所以必须从「落在哪个桶」上断言）。
    // ========================================================================

    /// 给夹具仓库设一个默认库位（`is_default = 1`、启用、未软删），返回库位字典 id。
    async fn make_default_location(warehouse: Uuid, code: &str) -> Uuid {
        let id = Uuid::now_v7();
        sql(&format!(
            "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, \
                                       location_type, is_default, capacity, status, sort_order) \
             VALUES ('{id}', '{code}', 'R5 夹具默认库位', '{warehouse}', 'STORAGE', 1, NULL, 1, 1)"
        ))
        .await;
        id
    }

    /// 某物料在某仓的**全部库存桶**（`(库位字典 id, 数量)`；`None` = 未分配桶）。
    async fn buckets_of(material: Uuid, warehouse: Uuid) -> Vec<(Option<Uuid>, Decimal)> {
        let rows = db()
            .find_with_params(
                "SELECT il.location_id, i.qty FROM biz_inventory_location il \
                   LEFT JOIN biz_inventory i \
                          ON i.material_id = il.material_id AND i.warehouse_id = il.warehouse_id \
                         AND i.location_id = il.id \
                  WHERE il.material_id = ?::uuid AND il.warehouse_id = ?::uuid \
                  ORDER BY il.location_id NULLS LAST",
                vec![Value::from(&material), Value::from(&warehouse)],
            )
            .await
            .expect("读库存桶");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed::<Option<Uuid>>("location_id").expect("库位"),
                    row.get_typed::<Option<Decimal>>("qty")
                        .expect("qty")
                        .unwrap_or(Decimal::ZERO),
                )
            })
            .collect()
    }

    /// 期初台账里这行的库位桶（`biz_opening_inventory.location_row_id`）。
    async fn opening_bucket(period_code: &str, material: Uuid, warehouse: Uuid) -> Option<Uuid> {
        let rows = db()
            .find_with_params(
                "SELECT location_row_id FROM biz_opening_inventory \
                  WHERE period_code = ? AND material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![
                    Value::Text(period_code.to_string()),
                    Value::from(&material),
                    Value::from(&warehouse),
                ],
            )
            .await
            .expect("读期初台账的库位桶");
        rows.first()
            .map(|row| row.get_typed::<Option<Uuid>>("location_row_id"))
            .transpose()
            .expect("解析期初台账的库位桶")
            .flatten()
    }

    /// ⑨b 期初落**默认库位桶**（不是未分配桶），重录 / 取消都冲回同一个桶。
    #[tokio::test]
    async fn r5_16_inventory_opening_lands_on_default_location_bucket() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(8).await;
        let material = make_material("R5-M-016", false).await;
        let warehouse = make_warehouse("R5-WH16").await;
        let dict = make_default_location(warehouse, "R5-LOC-16").await;

        save_inventory(Some("2034-08"), &[item(material, warehouse, "100", "10")], Some(uid()))
            .await
            .expect("库存期初应保存成功");

        // ① 只有一个桶，且是默认库位桶（不是「未分配桶」）
        let buckets = buckets_of(material, warehouse).await;
        assert_eq!(
            buckets,
            vec![(Some(dict), dec("100"))],
            "期初必须落「默认库位桶」，不许再落「未分配库位桶」（实际 {buckets:?}）"
        );
        // ② 成本账同一行同一桶（数量 / 金额 / 均价）
        let cost = db()
            .find_with_params(
                "SELECT i.qty, i.amount, i.avg_cost FROM biz_inventory i \
                   JOIN biz_inventory_location il ON il.id = i.location_id \
                  WHERE i.material_id = ?::uuid AND i.warehouse_id = ?::uuid \
                    AND il.location_id = ?::uuid",
                vec![Value::from(&material), Value::from(&warehouse), Value::from(&dict)],
            )
            .await
            .expect("读成本账");
        assert_eq!(cost.len(), 1, "成本账只有默认库位桶这一行");
        assert_eq!(cost[0].get_typed::<Decimal>("qty").unwrap(), dec("100"));
        assert_eq!(cost[0].get_typed::<Decimal>("amount").unwrap(), dec("1000"));
        assert_eq!(cost[0].get_typed::<Decimal>("avg_cost").unwrap(), dec("10"));
        // ③ 期初台账记下了「这笔期初落在哪个桶」
        let row_id: Uuid = db()
            .scalar_with_params(
                "SELECT il.id FROM biz_inventory_location il \
                  WHERE il.material_id = ?::uuid AND il.warehouse_id = ?::uuid \
                    AND il.location_id = ?::uuid",
                vec![Value::from(&material), Value::from(&warehouse), Value::from(&dict)],
            )
            .await
            .expect("读库位账行");
        assert_eq!(
            opening_bucket("2034-08", material, warehouse).await,
            Some(row_id),
            "期初台账必须记下落在哪个库位桶（重录时按它冲销）"
        );

        // ④ 重录成 80：差额 20 从**同一个桶**退回（不是新开一个未分配桶）
        save_inventory(Some("2034-08"), &[item(material, warehouse, "80", "10")], Some(uid()))
            .await
            .expect("期初重录");
        assert_eq!(
            buckets_of(material, warehouse).await,
            vec![(Some(dict), dec("80"))],
            "重录必须冲回同一个默认库位桶"
        );
        assert_eq!(
            opening_bucket("2034-08", material, warehouse).await,
            Some(row_id),
            "重录后桶不变"
        );

        // ⑤ 取消期初（传空明细）：整笔从同一个桶退干净，桶结构不变、总数归零
        save_inventory(Some("2034-08"), &[], Some(uid()))
            .await
            .expect("取消期初");
        let buckets = buckets_of(material, warehouse).await;
        assert!(
            buckets.iter().all(|(_, qty)| qty.is_zero()),
            "取消期初后数量必须归零（实际 {buckets:?}）"
        );
        assert!(
            buckets.iter().all(|(loc, _)| *loc != None),
            "取消期初不许凭空造出「未分配库位桶」（实际 {buckets:?}）"
        );
        assert_eq!(
            opening_bucket("2034-08", material, warehouse).await,
            None,
            "期初台账已清空"
        );
    }

    /// ⑨c 没有默认库位的仓：期初仍落「未分配库位桶」（与收货的兜底逐字一致）。
    #[tokio::test]
    async fn r5_17_inventory_opening_without_default_location_keeps_unassigned() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(11).await;
        let material = make_material("R5-M-017", false).await;
        let warehouse = make_warehouse("R5-WH17").await;

        save_inventory(Some("2034-11"), &[item(material, warehouse, "30", "5")], Some(uid()))
            .await
            .expect("库存期初应保存成功");

        assert_eq!(
            location_qty(material, warehouse).await,
            dec("30"),
            "没有默认库位时仍落「未分配库位桶」（与收货过账的兜底一致）"
        );
        let (qty, amount, _) = cost_row(material, warehouse).await.expect("成本账汇总桶行必须存在");
        assert_eq!(qty, dec("30"));
        assert_eq!(amount, dec("150"));
    }

    /// ⑩ 库存期初金额累加到 1405 借方（对方科目 3103 贷方）。
    #[tokio::test]
    async fn r5_10_inventory_amount_lands_on_1405_debit() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(10).await;
        let material = make_material("R5-M-010", false).await;
        let warehouse = make_warehouse("R5-WH1").await;

        // 先录科目期初（平衡），再录库存期初 —— 两块必须拼成同一张凭证
        save_opening(
            Some("2034-10"),
            &[entry("1002", "50000", "0"), entry("3001", "0", "50000")],
            Some(uid()),
        )
        .await
        .expect("科目期初");
        let res = save_inventory(Some("2034-10"), &[item(material, warehouse, "100", "10")], Some(uid()))
            .await
            .expect("库存期初");

        assert_eq!(
            voucher_entry_of("2034-10", ACCOUNT_INVENTORY, "debit").await,
            dec("1000"),
            "库存期初金额必须累加到 1405 借方"
        );
        assert_eq!(
            voucher_entry_of("2034-10", ACCOUNT_INVENTORY_OFFSET, "credit").await,
            dec("1000"),
            "对方科目 3103 贷方同额"
        );
        assert_eq!(res.combined_debit_total, dec("51000"));
        assert_eq!(res.combined_credit_total, dec("51000"));
        assert!(res.balanced);
        assert_eq!(
            voucher_count("2034-10", VOUCHER_TYPE_OPENING).await,
            1,
            "科目期初 + 库存期初仍然只有一张期初凭证"
        );
        let voucher = res.voucher.expect("应有期初凭证");
        assert_eq!(voucher.entry_count, 4, "1002 借 / 1405 借 / 3001 贷 / 3103 贷");
        assert_eq!(voucher.total_debit, dec("51000"));
    }

    /// ⑩b FIX-6：期初对方科目**可配** —— 把系统参数改成 3101（盈余公积），
    /// 新生成的期初凭证必须贷 3101 而不是写死的 3103；返回体与互斥提示同步换科目。
    #[tokio::test]
    async fn r5_10b_inventory_offset_account_follows_config() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_at(2035, 7).await;
        let material = make_material("R5-M-010B", false).await;
        let warehouse = make_warehouse("R5-WH1B").await;

        set_equity_config("3101").await;
        let saved = save_inventory(Some("2035-07"), &[item(material, warehouse, "100", "10")], Some(uid())).await;
        restore_equity_config().await;
        let res = saved.expect("库存期初应保存成功");

        assert_eq!(res.inventory_credit_account, "3101", "返回体必须回显配置的对方科目");
        assert_eq!(
            voucher_entry_of("2035-07", "3101", "credit").await,
            dec("1000"),
            "生成期初凭证时必须贷配置的 3101（不再是写死的 3103）"
        );
        assert_eq!(
            voucher_entry_of("2035-07", "3103", "credit").await,
            Decimal::ZERO,
            "3103 一分钱都不该有"
        );
        assert_eq!(
            voucher_entry_of("2035-07", ACCOUNT_INVENTORY, "debit").await,
            dec("1000"),
            "借方仍是 1405，借贷平衡不变"
        );
        assert!(res.balanced, "换对方科目之后仍必须借贷平衡");
    }

    /// ⑬ 库存期初覆盖：按差额调整，不重复计数（含清空）。
    #[tokio::test]
    async fn r5_13_inventory_overwrite_adjusts_by_difference() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_at(2035, 1).await;
        let material = make_material("R5-M-013", false).await;
        let warehouse = make_warehouse("R5-WH1").await;

        save_inventory(Some("2035-01"), &[item(material, warehouse, "100", "10")], Some(uid()))
            .await
            .expect("第一次录 100 件");
        save_inventory(Some("2035-01"), &[item(material, warehouse, "150", "10")], Some(uid()))
            .await
            .expect("第二次录 150 件（覆盖）");

        let (qty, amount, avg_cost) = cost_row(material, warehouse).await.expect("成本账");
        assert_eq!(qty, dec("150"), "覆盖后数量是 150 而不是 250");
        assert_eq!(amount, dec("1500"));
        assert_eq!(avg_cost, dec("10"));
        assert_eq!(location_qty(material, warehouse).await, dec("150"));
        assert_eq!(txn_count(material, BIZ_TYPE_OPENING).await, 2, "100 进 + 50 进");
        assert_eq!(inventory_rows("2035-01").await, 1, "台账也是一行");
        assert_eq!(
            voucher_entry_of("2035-01", ACCOUNT_INVENTORY, "debit").await,
            dec("1500")
        );

        // 清空库存期初 → 数量与金额按差额退回，流水补一条 OUT
        let res = save_inventory(Some("2035-01"), &[], Some(uid()))
            .await
            .expect("清空库存期初");
        let (qty, amount, _) = cost_row(material, warehouse).await.expect("成本账仍在");
        assert_eq!(qty, Decimal::ZERO, "数量退回 0");
        assert_eq!(amount, Decimal::ZERO, "金额退回 0");
        assert_eq!(location_qty(material, warehouse).await, Decimal::ZERO);
        assert_eq!(txn_count(material, BIZ_TYPE_OPENING).await, 3, "清空补一条 OUT");
        assert_eq!(inventory_rows("2035-01").await, 0);
        assert!(res.voucher.is_none(), "期初全清后凭证也撤掉");
        assert_eq!(voucher_count("2035-01", VOUCHER_TYPE_OPENING).await, 0);
    }

    /// ⑭ 一物一码物料不许按数量录期初（qty 由序列号件数派生）。
    #[tokio::test]
    async fn r5_14_serial_required_material_rejected() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_at(2035, 2).await;
        let material = make_material("R5-M-014", true).await;
        let warehouse = make_warehouse("R5-WH1").await;

        let err = save_inventory(Some("2035-02"), &[item(material, warehouse, "10", "10")], Some(uid()))
            .await
            .expect_err("一物一码物料必须被拒");

        assert_eq!(err.status, 400);
        assert!(err.msg.contains("一物一码"), "消息要点明原因：{}", err.msg);
        assert!(cost_row(material, warehouse).await.is_none(), "一分库存都不该动");
    }

    /// ⑮ 库存期初与手工 1405 / 3103 期初互斥（否则 1405 会被计两次）。
    #[tokio::test]
    async fn r5_15_inventory_conflicts_with_manual_1405() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_at(2035, 3).await;
        let material = make_material("R5-M-015", false).await;
        let warehouse = make_warehouse("R5-WH1").await;

        save_opening(
            Some("2035-03"),
            &[entry("1405", "2000", "0"), entry("3001", "0", "2000")],
            Some(uid()),
        )
        .await
        .expect("手工录 1405 期初是允许的（没有库存期初时）");

        let err = save_inventory(Some("2035-03"), &[item(material, warehouse, "100", "10")], Some(uid()))
            .await
            .expect_err("手工 1405 + 库存期初必须被拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("1405"), "消息要点名科目：{}", err.msg);
        assert!(cost_row(material, warehouse).await.is_none());
        assert_eq!(
            voucher_entry_of("2035-03", ACCOUNT_INVENTORY, "debit").await,
            dec("2000"),
            "被拒之后期初凭证仍是手工那一版"
        );
    }

    // ==================== ④ 报表 ====================

    /// ⑪ 有期初时资产负债表「期末余额 = 期初 + 本期」。
    #[tokio::test]
    async fn r5_11_balance_sheet_end_equals_opening_plus_period() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(11).await;

        // ★ 基线（增量口径）：云枢里 1002 的历史发生额不止本卡的夹具（别的模块的用例
        //   也在 2026-xx 记过银行存款），所以「期初 = 50000」这种绝对值断言会随库污染假红。
        //   这里先量一次「录期初之前」的余额，断言只比**增量**。
        let (base_before, base_monetary) = {
            let tx = db().begin().await.expect("开启基线事务");
            let rows = report::load_accounts(
                &tx,
                NaiveDate::from_ymd_opt(2034, 11, 1).expect("日期"),
                NaiveDate::from_ymd_opt(2034, 11, 30).expect("日期"),
                Some("2034-11"),
            )
            .await
            .expect("读基线科目余额");
            let snapshot = report::build_snapshot(&tx, "2034-11")
                .await
                .expect("基线资产负债表应能算出");
            let _ = tx.rollback().await;
            let before = rows
                .iter()
                .find(|row| row.account_code == "1002")
                .map(|row| row.net_before)
                .unwrap_or_default();
            let monetary = snapshot
                .sheet
                .amounts
                .get("MONETARY_FUNDS")
                .copied()
                .unwrap_or_default();
            (before, monetary)
        };

        save_opening(
            Some("2034-11"),
            &[entry("1002", "50000", "0"), entry("3001", "0", "50000")],
            Some(uid()),
        )
        .await
        .expect("期初");

        // 本期一张普通凭证：借 1002 2500 / 贷 1122 2500（收回应收）
        let cash = voucher::account_required(db(), "1002", "本卡夹具")
            .await
            .expect("科目 1002");
        let receivable = voucher::account_required(db(), "1122", "本卡夹具")
            .await
            .expect("科目 1122");
        let tx = db().begin().await.expect("开启本期凭证事务");
        voucher::post_voucher(
            &tx,
            &PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_AUTO,
                source_type: "MANUAL",
                source_id: None,
                source_no: Some("R5-PERIOD-2034-11"),
                source_doc_no: None,
                voucher_date: NaiveDate::from_ymd_opt(2034, 11, 15).expect("日期"),
                summary: "R5 夹具：本期收回货款".to_string(),
                lines: vec![
                    EntryLine {
                        line_no: 1,
                        summary: "本期收回货款".to_string(),
                        account: cash,
                        debit: dec("2500"),
                        credit: Decimal::ZERO,
                        aux_dept_id: None,
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_warehouse_id: None,
                    },
                    EntryLine {
                        line_no: 2,
                        summary: "本期收回货款".to_string(),
                        account: receivable,
                        debit: Decimal::ZERO,
                        aux_dept_id: None,
                        credit: dec("2500"),
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_warehouse_id: None,
                    },
                ],
                remark: Some("R5 夹具"),
                red_of: None,
                status: crate::business::erp::voucher::STATUS_POSTED,
            },
            Some(uid()),
            Utc::now(),
        )
        .await
        .expect("本期凭证");
        tx.commit().await.expect("提交本期凭证");

        let tx = db().begin().await.expect("开启报表事务");
        let accounts = report::load_accounts(
            &tx,
            NaiveDate::from_ymd_opt(2034, 11, 1).expect("日期"),
            NaiveDate::from_ymd_opt(2034, 11, 30).expect("日期"),
            Some("2034-11"),
        )
        .await
        .expect("读科目余额");
        let snapshot = report::build_snapshot(&tx, "2034-11")
            .await
            .expect("资产负债表应能算出");
        let _ = tx.rollback().await;

        let row = accounts
            .iter()
            .find(|row| row.account_code == "1002")
            .expect("1002 在科目表里");
        assert_eq!(
            row.net_before - base_before,
            dec("50000"),
            "期初增量 = 期初凭证（不算本期）"
        );
        assert_eq!(row.net_period, dec("2500"), "本期 = 期间内的普通凭证");
        assert_eq!(row.net() - base_before, dec("52500"), "期末 = 期初 + 本期");

        let monetary = snapshot
            .sheet
            .amounts
            .get("MONETARY_FUNDS")
            .copied()
            .unwrap_or_default();
        assert_eq!(
            monetary - base_monetary,
            dec("52500"),
            "报表的货币资金增量 = 期初 + 本期"
        );
        assert_eq!(
            snapshot.sheet.assets_total(),
            snapshot.sheet.liability_equity_total(),
            "资产 ≡ 负债 + 权益（期初不破坏勾稽）"
        );
    }

    /// ⑫ 期初不平衡时报表报错（不许出表）。
    #[tokio::test]
    async fn r5_12_unbalanced_opening_blocks_report() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(12).await;
        // 绕过接口直接插一行不平衡的期初（模拟手工改库 / 未完成的迁移）
        let account = voucher::account_required(db(), "1002", "本卡夹具")
            .await
            .expect("科目 1002");
        db().execute_with_params(
            "INSERT INTO biz_opening_balance (period_code, account_id, account_code, debit_opening) \
             VALUES ('2034-12', ?::uuid, '1002', 100)",
            vec![Value::from(&account.id)],
        )
        .await
        .expect("插不平衡的期初");

        let tx = db().begin().await.expect("开启报表事务");
        let result = report::build_snapshot(&tx, "2034-12").await;
        let _ = tx.rollback().await;

        match result {
            Err(DbError::InvalidValue(message)) => {
                assert!(message.contains("期初余额不平衡"), "必须点名期初不平衡：{message}");
                assert!(message.contains("100"), "必须报差额：{message}");
            }
            other => panic!("不平衡的期初不许出表，实际：{other:?}"),
        }

        // 接口层把它翻成 400 + 66012（不是 500）
        let error = report::period_error_for_test("2034-12", "期初余额不平衡，不允许出表");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_OPENING_UNBALANCED);
    }

    // ---------- 收尾清场：模块级 teardown（`zz_` 排在本模块最后） ----------

    /// 本模块跑完**不许留**期初凭证的 `VOUCHER` 状态日志（含孤儿行）。
    ///
    /// 期初凭证走真实 `voucher::post_voucher`（会写一条 `VOUCHER` 日志），而 `cleanup()`
    /// 以前只删凭证本体 ⇒ 每跑一遍全量多 11 条孤儿（本卡实测）。`zz_` 排在本模块最后。
    #[tokio::test]
    async fn zz_teardown_purges_opening_voucher_status_logs() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_month(1).await;
        save_opening(
            Some("2034-01"),
            &[entry("1002", "50000", "0"), entry("3001", "0", "50000")],
            Some(uid()),
        )
        .await
        .expect("平衡的期初应当保存成功");
        let voucher_id: Option<Uuid> = db()
            .query_first_sql(
                "SELECT id FROM biz_voucher WHERE period = '2034-01' LIMIT 1".to_string(),
                vec![],
            )
            .await
            .expect("查期初凭证")
            .map(|row| row.get_typed("id").expect("读凭证 id"));
        let voucher_id = voucher_id.expect("期初保存必须生成凭证");
        let logs: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'VOUCHER' AND entity_id = ?::uuid",
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0);
        assert!(logs > 0, "期初凭证落地必须写 VOUCHER 状态日志");

        // 走模块真实的清场
        cleanup().await;

        let left: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'VOUCHER' AND entity_id = ?::uuid",
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("查清场后日志")
            .unwrap_or(0);
        assert_eq!(left, 0, "清场后 VOUCHER 状态日志必须为 0（日志无外键，必须按 id 先删）");

        // 兜底：整库不再有 PAYABLE / VOUCHER 的孤儿行
        // 兜底：整库最终不许留 PAYABLE / VOUCHER 的孤儿行。
        // 并行跑时别的模块可能正处在「单据已删、它自己的日志还没删」的毫秒级中间态，
        // 一次断言会把别人的中间态判成本模块假红 ⇒ 收敛式判定（`settle_orphans`）：
        // 真孤儿等多久都还在，仍然会红；只吸收别人正在删的瞬时窗口。
        let left = fixture_hygiene::settle_orphans(fixture_hygiene::FINANCE, std::time::Duration::from_secs(5)).await;
        assert_eq!(
            left, 0,
            "整库不许留 PAYABLE / VOUCHER 的孤儿状态日志（收敛等待 5s 后仍非 0）"
        );
    }
}
