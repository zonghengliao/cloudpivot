// ============================================================================
// stocktake_model.rs —— 盘点单数据层（ARCH-3 §5 **DC-14**）：取号 / 建改删 / 分页
//                       + 过账（三本账按差异正负）+ 作废（原路回滚）+ 盘点差异凭证
//
// # 这张表补的是什么（问题原样）
//
// 改造前：全系统没有盘点单。年底盘点，账面 100 实际 92，仓管**在系统里没有地方调平**，
// 只能手改 `biz_inventory` —— 没有凭证来源、没有审批、没有留痕，审计上说不清
// 「为什么库存变了」（ARCH-3 DC-14，P1）。
//
// # 三本账怎么动（**复用**，不重写）
//
// | 差异 | 库存方向 | 用的现成函数（出库单 / 入库单同一套） | 凭证 |
// |------|----------|----------------------------------------|------|
// | `diff > 0` 盘盈 | 增加 | `stockin_write::{apply_cost, apply_location_qty}`（收货过账同一个移动加权实现） | 借 1405 / 贷 1901 |
// | `diff < 0` 盘亏 | 减少 | `stockout_model::{lock_bucket, deduct_cost, lock_location_row, deduct_location_qty}`（出库过账同一个负库存闸） | 借 1901 / 贷 1405 |
// | 作废·盘盈行 | 扣回 | `stockin_cancel::{reverse_cost, reverse_location_qty}`（收货作废同一套按**账上真实金额**冲减） | 红冲 |
// | 作废·盘亏行 | 加回 | `stockout_model::{restore_cost, restore_location_qty}`（出库作废同一套原路退回） | 红冲 |
//
// **金额一律取「账上真实差额」**（`after.amount − before.amount`，或反冲的 `before − after`），
// 绝不在 Rust 里重算 `qty × avg_cost`：多桶 / 4 位舍入下两者不等，重算会让
// `erp_invariants.py` 的 B2（库存金额 = 存货科目余额）报 0.00x 的差 —— 那是 BLOCKER。
//
// # 流水（第三本账）
//
// `biz_inventory_txn.doc_type='STOCK_TAKE'`、`doc_id = 盘点单 id`，`biz_type` 三个值：
//   · `STOCK_TAKE_GAIN`   盘盈（txn_type=IN）
//   · `STOCK_TAKE_LOSS`   盘亏（txn_type=OUT）
//   · `STOCK_TAKE_CANCEL` 作废冲销（方向与正向相反）
// 数量恒正、方向看 `txn_type`（`ck_biz_inv_txn_type` 只认 IN/OUT，与出库单同口径）。
//
// # 为什么 `diff_qty` 是库里的生成列
//
// `actual_qty − book_qty` 由 PG 计算（`V0211` 的 `GENERATED ALWAYS AS … STORED`），
// 任何写入路径都不可能写出「差异与两数不符」的行；本层只读它，从不自己算。
// ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::inventory_writer::{self, TxnRecord};
use crate::business::erp::stockin_base::{CostBasis, InventorySnapshot};
use crate::business::erp::{stockin_cancel, stockin_write, stockout_model, voucher};
use crate::business::shared::scope;
use crate::business::shared::state_machine;

// ==================== 表名 / 常量 ====================

/// 单头表。
pub const TABLE: &str = "biz_stock_take";
/// 明细表（无 `deleted_at`，主单软删后明细留在库里）。
pub const ITEM_TABLE: &str = "biz_stock_take_item";

/// 状态：草稿（账面数已快照，实盘数可改）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：待过账（已提交审批 / 显式跳过审批，账一分没动）。
pub const STATUS_PENDING_POST: &str = "PENDING_POST";
/// 状态：已过账（三本账已按差异动过、盘点差异凭证已生成）。
pub const STATUS_POSTED: &str = "POSTED";
/// 状态：已作废（已过账的作废会回滚三本账 + 红冲凭证）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 盘点类型：全盘（建单时把该仓全部库存桶快照成明细）。
pub const TAKE_TYPE_FULL: &str = "FULL";
/// 盘点类型：抽盘（只盘指定物料 / 库位）。
pub const TAKE_TYPE_PARTIAL: &str = "PARTIAL";
/// 两种盘点类型（DTO 校验与巡检共用）。
pub const TAKE_TYPES: [&str; 2] = [TAKE_TYPE_FULL, TAKE_TYPE_PARTIAL];

/// 流水方向：入。
pub const TXN_TYPE_IN: &str = "IN";
/// 流水方向：出。
pub const TXN_TYPE_OUT: &str = "OUT";

/// 流水 / 凭证的来源单据类型。
pub const DOC_TYPE: &str = "STOCK_TAKE";
/// 流水业务类型：盘盈。
pub const BIZ_TYPE_GAIN: &str = "STOCK_TAKE_GAIN";
/// 流水业务类型：盘亏。
pub const BIZ_TYPE_LOSS: &str = "STOCK_TAKE_LOSS";
/// 流水业务类型：作废冲销。
pub const BIZ_TYPE_CANCEL: &str = "STOCK_TAKE_CANCEL";

/// 负库存哨兵前缀：走 `DbError::InvalidValue` 传上来，由 handler 翻成 409。
pub const NEGATIVE_PREFIX: &str = "ST_NEGATIVE|";

/// 单号前缀（`ST-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "ST";
/// `biz_seq.seq_type`。
pub const SEQ_TYPE: &str = "STOCK_TAKE";

/// 取号规则（`biz_seq` 缺失时 `voucher::next_doc_no` 会幂等补一行）。
pub const STOCK_TAKE_SEQ: voucher::SeqSpec<'static> = voucher::SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "盘点单号",
    remark: "盘点单号：ST-yyyyMMdd-000001，按天（CST）滚动",
};

/// 本模块负责的合法边（与 `state_machine::STOCK_TAKE_EDGES` 同源）。
pub const LEGAL_EDGES: [(&str, &str); 6] = state_machine::STOCK_TAKE_EDGES;

/// 明细行上限（防误传超大数组）。
pub const MAX_LINES: usize = 2000;

/// 列表默认排序（与收货 / 出库同款：日期倒序 + 单号倒序）。
pub const LIST_ORDER: &str = "ORDER BY take_date DESC, take_no DESC";

/// 列表分页目标（`SCOPE-EXEMPT`：盘点是 ERP 单据表，无归属列，可见性靠权限码）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_stock_take");

// ==================== 纯函数 ====================

/// 状态中文名（错误文案 / 列表展示共用）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_PENDING_POST => "待过账",
        STATUS_POSTED => "已过账",
        STATUS_CANCELLED => "已作废",
        _ => "未知状态",
    }
}

/// 盘点类型中文名。
pub fn take_type_label(take_type: &str) -> &'static str {
    match take_type {
        TAKE_TYPE_FULL => "全盘",
        TAKE_TYPE_PARTIAL => "抽盘",
        _ => "未知类型",
    }
}

/// 金额舍入：4 位（与 PG `numeric(18,4)` 同口径）。
pub fn money(value: Decimal) -> Decimal {
    value.round_dp(4)
}

// ==================== 行结构 ====================

/// 单头一行。
#[derive(Debug, Clone)]
pub struct StockTakeRow {
    /// 主键。
    pub id: Uuid,
    /// 盘点单号。
    pub take_no: String,
    /// 盘点仓库。
    pub warehouse_id: Uuid,
    /// 盘点日期。
    pub take_date: NaiveDate,
    /// 盘点类型（FULL / PARTIAL）。
    pub take_type: String,
    /// 状态。
    pub status: String,
    /// 提交审批的实例 id。
    pub approval_instance_id: Option<Uuid>,
    /// 盘点差异凭证（蓝字）。
    pub voucher_id: Option<Uuid>,
    /// 红字凭证。
    pub reverse_voucher_id: Option<Uuid>,
    /// 账面数量合计。
    pub total_book_qty: Decimal,
    /// 实盘数量合计。
    pub total_actual_qty: Decimal,
    /// 盘盈数量合计。
    pub total_gain_qty: Decimal,
    /// 盘亏数量合计（绝对值）。
    pub total_loss_qty: Decimal,
    /// 盘盈金额合计。
    pub total_gain_amount: Decimal,
    /// 盘亏金额合计。
    pub total_loss_amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 过账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 过账人。
    pub posted_by: Option<Uuid>,
    /// 作废时间。
    pub cancelled_at: Option<DateTime<Utc>>,
    /// 作废人。
    pub cancelled_by: Option<Uuid>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 制单人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
}

impl StockTakeRow {
    /// 差异金额合计（盘盈 + 盘亏，审批流的 `form.total_diff_amount` 用它）。
    pub fn total_diff_amount(&self) -> Decimal {
        (self.total_gain_amount + self.total_loss_amount).round_dp(4)
    }

    /// 差异数量合计（绝对值）。
    pub fn total_diff_qty(&self) -> Decimal {
        (self.total_gain_qty + self.total_loss_qty).round_dp(4)
    }
}

/// 明细一行。
#[derive(Debug, Clone)]
pub struct StockTakeItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属盘点单。
    pub take_id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 单位（物料基础单位快照）。
    pub unit_id: Uuid,
    /// 库位账行 id（`biz_inventory_location.id`；NULL = 未分配桶）。
    pub location_id: Option<Uuid>,
    /// 账面数（建单时快照）。
    pub book_qty: Decimal,
    /// 实盘数。
    pub actual_qty: Decimal,
    /// 差异 = 实盘 − 账面（PG 生成列）。
    pub diff_qty: Decimal,
    /// 账面成本快照。
    pub unit_cost: Decimal,
    /// 差异金额（过账时按账上真实增量/减量写入）。
    pub diff_amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
}

/// 列表筛选。
#[derive(Debug, Clone, Default)]
pub struct StockTakeFilter {
    /// 状态。
    pub status: Option<String>,
    /// 仓库。
    pub warehouse_id: Option<Uuid>,
    /// 盘点类型。
    pub take_type: Option<String>,
    /// 单号关键字。
    pub keyword: Option<String>,
    /// 页码（1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 建单时的一行账面快照（调用方算好交给本层）。
#[derive(Debug, Clone)]
pub struct NewStockTakeLine {
    /// 物料。
    pub material_id: Uuid,
    /// 物料基础单位。
    pub unit_id: Uuid,
    /// 库位账行。
    pub location_id: Option<Uuid>,
    /// 账面数量。
    pub book_qty: Decimal,
    /// 账面成本（移动加权快照）。
    pub unit_cost: Decimal,
}

/// 建单入参。
#[derive(Debug, Clone)]
pub struct NewStockTake<'a> {
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 盘点日期。
    pub take_date: NaiveDate,
    /// 盘点类型。
    pub take_type: &'a str,
    /// 备注。
    pub remark: Option<&'a str>,
}

/// 过账 / 作废时一行差异的处理结果。
#[derive(Debug, Clone, Copy)]
pub struct DiffApplied {
    /// 明细 id。
    pub item_id: Uuid,
    /// 差异数量（带符号；0 = 无差异，没动账）。
    pub diff_qty: Decimal,
    /// 账上真实金额变动（恒正）。
    pub amount: Decimal,
}

/// 一条库存流水的写库参数（收进结构体：避开 `clippy::too_many_arguments`）。
#[derive(Debug, Clone)]
pub struct TxnWrite {
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 方向（IN / OUT）。
    pub txn_type: &'static str,
    /// 业务类型（STOCK_TAKE_GAIN / LOSS / CANCEL）。
    pub biz_type: &'static str,
    /// 盘点单 id。
    pub doc_id: Uuid,
    /// 盘点单号。
    pub doc_no: String,
    /// 数量（恒正）。
    pub qty: Decimal,
    /// 单价（6 位）。
    pub unit_cost: Decimal,
    /// 金额（恒正）。
    pub amount: Decimal,
    /// 变动前快照。
    pub before: InventorySnapshot,
    /// 变动后快照。
    pub after: InventorySnapshot,
    /// 操作人。
    pub operator: Option<Uuid>,
    /// 记账时间（流水 `created_at` 与期间编码都取它）。
    pub now: DateTime<Utc>,
}

// ==================== 取号 / 建单 / 改单 ====================

/// 取一个盘点单号（`ST-yyyyMMdd-000001`，按天 CST 滚动）。
pub async fn next_take_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &STOCK_TAKE_SEQ, now).await
}

/// 建单（`status = DRAFT`）：单头 + 明细 + 建单日志，**同一事务**（调用方负责事务边界）。
///
/// 明细的 `actual_qty` **预填账面数**（盘点惯例：先打印账面数，仓管只改有差异的行），
/// 所以 `diff_qty` 建单时为 0，过账前必须人工确认。
pub async fn create(
    ex: &impl SqlExecutor,
    new: &NewStockTake<'_>,
    lines: &[NewStockTakeLine],
    operator: state_machine::Operator,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let take_no = next_take_no(ex, now).await?;
    let id = Uuid::now_v7();
    let book_total: Decimal = lines.iter().map(|line| line.book_qty).sum();
    let operator_text = Value::Text(operator.id.map(|v| v.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, take_no, warehouse_id, take_date, take_type, status, \
                 total_book_qty, total_actual_qty, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?::uuid, ?::text::date, ?, '{STATUS_DRAFT}', \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, NULLIF(?::text,''), \
                 ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(take_no.clone()),
            Value::from(&new.warehouse_id),
            Value::Text(new.take_date.to_string()),
            Value::from(new.take_type),
            Value::Text(book_total.round_dp(4).to_string()),
            Value::Text(book_total.round_dp(4).to_string()),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await?;

    for (index, line) in lines.iter().enumerate() {
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (id, take_id, line_no, material_id, unit_id, location_id, \
                     book_qty, actual_qty, unit_cost, diff_amount) \
                 VALUES (?, ?::uuid, ?, ?::uuid, ?::uuid, NULLIF(?::text,'')::uuid, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, 0)"
            ),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&id),
                Value::from(i32::try_from(index + 1).unwrap_or(i32::MAX)),
                Value::from(&line.material_id),
                Value::from(&line.unit_id),
                Value::Text(line.location_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(line.book_qty.round_dp(4).to_string()),
                // 实盘数预填账面数（差异 0，等人工确认）
                Value::Text(line.book_qty.round_dp(4).to_string()),
                Value::Text(line.unit_cost.to_string()),
            ],
        )
        .await?;
    }

    // 建单留痕（`NULL → DRAFT`），与建单同一事务：宁可建不出单，也不要「有单没轨迹」。
    state_machine::log_created(
        ex,
        state_machine::ENTITY_STOCK_TAKE,
        id,
        &take_no,
        STATUS_DRAFT,
        operator,
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok(id)
}

/// 改单头（**仅 DRAFT 由调用方保证**）：盘点日期 / 备注。返回 `affected`。
pub async fn update_head(
    ex: &impl SqlExecutor,
    id: Uuid,
    take_date: NaiveDate,
    remark: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET take_date = ?::text::date, remark = NULLIF(?::text,''), \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = '{STATUS_DRAFT}' AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(take_date.to_string()),
            Value::Text(remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 写一行的实盘数（**仅 DRAFT 由调用方保证**）。返回 `affected`（0 = 行不属于本单）。
pub async fn set_actual_qty(
    ex: &impl SqlExecutor,
    take_id: Uuid,
    item_id: Uuid,
    actual_qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {ITEM_TABLE} SET actual_qty = NULLIF(?::text,'')::numeric, updated_at = ? \
              WHERE id = ?::uuid AND take_id = ?::uuid"
        ),
        vec![
            Value::Text(actual_qty.round_dp(4).to_string()),
            Value::from(now),
            Value::from(&item_id),
            Value::from(&take_id),
        ],
    )
    .await
}

/// 重算单头的账面 / 实盘合计（改完实盘数后调用，**同一事务**）。
pub async fn refresh_totals(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} t SET \
                 total_book_qty = COALESCE((SELECT SUM(i.book_qty) FROM {ITEM_TABLE} i WHERE i.take_id = t.id), 0), \
                 total_actual_qty = COALESCE((SELECT SUM(i.actual_qty) FROM {ITEM_TABLE} i WHERE i.take_id = t.id), 0), \
                 updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE t.id = ?::uuid"
        ),
        vec![
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await?;
    Ok(())
}

/// 软删（**仅 DRAFT 由调用方保证**），返回 `affected`。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>, now: DateTime<Utc>) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = '{STATUS_DRAFT}' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 状态 CAS（`WHERE id = ? AND status = from`），返回 `affected`。
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    let mut sets = String::from("status = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid");
    let mut params = vec![Value::from(to), Value::from(now), operator_text.clone()];
    match to {
        STATUS_POSTED => {
            sets.push_str(", posted_at = ?, posted_by = NULLIF(?::text,'')::uuid");
            params.push(Value::from(now));
            params.push(operator_text);
        }
        STATUS_CANCELLED => {
            sets.push_str(", cancelled_at = ?, cancelled_by = NULLIF(?::text,'')::uuid, cancel_reason = ?");
            params.push(Value::from(now));
            params.push(operator_text);
            params.push(Value::Text(reason.unwrap_or_default().to_string()));
        }
        _ => {}
    }
    params.push(Value::from(&id));
    params.push(Value::from(from));
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET {sets} \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
        ),
        params,
    )
    .await
}

/// 回写审批实例 id（提交审批 / 驳回时清空）。
pub async fn link_approval_instance(
    ex: &impl SqlExecutor,
    id: Uuid,
    instance_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET approval_instance_id = NULLIF(?::text,'')::uuid, updated_at = ? \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(instance_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}

/// 回写凭证引用（过账写蓝字；作废写红字到 `reverse_voucher_id`）。
pub async fn link_voucher(
    ex: &impl SqlExecutor,
    id: Uuid,
    voucher_id: Option<Uuid>,
    red: bool,
    now: DateTime<Utc>,
) -> DbResult<()> {
    let column = if red { "reverse_voucher_id" } else { "voucher_id" };
    ex.exec_sql(
        format!("UPDATE {TABLE} SET {column} = NULLIF(?::text,'')::uuid, updated_at = ? WHERE id = ?::uuid"),
        vec![
            Value::Text(voucher_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await?;
    Ok(())
}

/// 过账后回写差异合计（盘盈 / 盘亏的数量与金额）。
pub async fn set_diff_totals(
    ex: &impl SqlExecutor,
    id: Uuid,
    gain_qty: Decimal,
    loss_qty: Decimal,
    gain_amount: Decimal,
    loss_amount: Decimal,
    now: DateTime<Utc>,
) -> DbResult<()> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET total_gain_qty = NULLIF(?::text,'')::numeric, \
                    total_loss_qty = NULLIF(?::text,'')::numeric, \
                    total_gain_amount = NULLIF(?::text,'')::numeric, \
                    total_loss_amount = NULLIF(?::text,'')::numeric, updated_at = ? \
              WHERE id = ?::uuid"
        ),
        vec![
            Value::Text(gain_qty.round_dp(4).to_string()),
            Value::Text(loss_qty.round_dp(4).to_string()),
            Value::Text(gain_amount.round_dp(4).to_string()),
            Value::Text(loss_amount.round_dp(4).to_string()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await?;
    Ok(())
}

/// 回写一行的差异金额（过账 / 作废都写实值）。
pub async fn set_item_amount(
    ex: &impl SqlExecutor,
    item_id: Uuid,
    amount: Decimal,
    now: DateTime<Utc>,
) -> DbResult<()> {
    ex.exec_sql(
        format!(
            "UPDATE {ITEM_TABLE} SET diff_amount = NULLIF(?::text,'')::numeric, updated_at = ? \
              WHERE id = ?::uuid"
        ),
        vec![
            Value::Text(amount.round_dp(4).to_string()),
            Value::from(now),
            Value::from(&item_id),
        ],
    )
    .await?;
    Ok(())
}

// ==================== 读 ====================

const HEAD_COLUMNS: &str = "id, take_no, warehouse_id, take_date, take_type, status, \
     approval_instance_id, voucher_id, reverse_voucher_id, \
     total_book_qty, total_actual_qty, total_gain_qty, total_loss_qty, \
     total_gain_amount, total_loss_amount, remark, \
     posted_at, posted_by, cancelled_at, cancelled_by, cancel_reason, \
     created_at, updated_at, create_by, update_by";

const ITEM_COLUMNS: &str = "id, take_id, line_no, material_id, unit_id, location_id, \
     book_qty, actual_qty, diff_qty, unit_cost, diff_amount, remark";

fn row_of(row: &Row) -> DbResult<StockTakeRow> {
    Ok(StockTakeRow {
        id: row.get_typed("id")?,
        take_no: row.get_typed("take_no")?,
        warehouse_id: row.get_typed("warehouse_id")?,
        take_date: row.get_typed("take_date")?,
        take_type: row.get_typed("take_type")?,
        status: row.get_typed("status")?,
        approval_instance_id: row.get_typed("approval_instance_id")?,
        voucher_id: row.get_typed("voucher_id")?,
        reverse_voucher_id: row.get_typed("reverse_voucher_id")?,
        total_book_qty: row.get_typed("total_book_qty")?,
        total_actual_qty: row.get_typed("total_actual_qty")?,
        total_gain_qty: row.get_typed("total_gain_qty")?,
        total_loss_qty: row.get_typed("total_loss_qty")?,
        total_gain_amount: row.get_typed("total_gain_amount")?,
        total_loss_amount: row.get_typed("total_loss_amount")?,
        remark: row.get_typed("remark")?,
        posted_at: row.get_typed("posted_at")?,
        posted_by: row.get_typed("posted_by")?,
        cancelled_at: row.get_typed("cancelled_at")?,
        cancelled_by: row.get_typed("cancelled_by")?,
        cancel_reason: row.get_typed("cancel_reason")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
        create_by: row.get_typed("create_by")?,
        update_by: row.get_typed("update_by")?,
    })
}

fn item_of(row: &Row) -> DbResult<StockTakeItemRow> {
    Ok(StockTakeItemRow {
        id: row.get_typed("id")?,
        take_id: row.get_typed("take_id")?,
        line_no: row.get_typed("line_no")?,
        material_id: row.get_typed("material_id")?,
        unit_id: row.get_typed("unit_id")?,
        location_id: row.get_typed("location_id")?,
        book_qty: row.get_typed("book_qty")?,
        actual_qty: row.get_typed("actual_qty")?,
        diff_qty: row.get_typed("diff_qty")?,
        unit_cost: row.get_typed("unit_cost")?,
        diff_amount: row.get_typed("diff_amount")?,
        remark: row.get_typed("remark")?,
    })
}

/// 单查（不存在 / 已删 → `None`）。
pub async fn find(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<StockTakeRow>> {
    // SCOPE-EXEMPT: 盘点单是 ERP 单据，按主键取 1 行；可见性由权限码控制。
    let row = ex
        .query_first_sql(
            format!("SELECT {HEAD_COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(row_of).transpose()
}

/// 批量解析凭证号（`biz_voucher.voucher_no`），键 = 凭证 id。
///
/// 盘点单只存 `voucher_id` / `reverse_voucher_id`（弱引用，和应付 / 出入库同一口径），
/// 页面要显示人可读的 `PZ-…` / `PZR-…` 就得回查一次。**一次 `id = ANY(...)` 查完**，
/// 不做 N+1（一页 20 张单最多 40 个 id）。
///
/// 软删的凭证不返回（`deleted_at IS NULL`）：查不到就让前端退回占位，绝不显示半截 UUID 当凭证号。
pub async fn voucher_no_map(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let joined = ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
    // SCOPE-EXEMPT: biz_voucher 按主键批量取凭证号，只补展示字段，不产生行集。
    let rows = ex
        .query_sql(
            "SELECT id, voucher_no FROM biz_voucher \
              WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(format!("{{{joined}}}"))],
        )
        .await?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        let no: String = row.get_typed("voucher_no")?;
        map.insert(id, no);
    }
    Ok(map)
}

/// 锁读单头（`FOR UPDATE`）。
pub async fn lock(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<StockTakeRow>> {
    // SCOPE-EXEMPT: 同上，按主键锁 1 行。
    let row = ex
        .query_first_sql(
            format!("SELECT {HEAD_COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(row_of).transpose()
}

/// 明细（按行号升序）。
pub async fn items_of(ex: &impl SqlExecutor, take_id: Uuid) -> DbResult<Vec<StockTakeItemRow>> {
    let rows = ex
        .query_sql(
            format!("SELECT {ITEM_COLUMNS} FROM {ITEM_TABLE} WHERE take_id = ?::uuid ORDER BY line_no"),
            vec![Value::from(&take_id)],
        )
        .await?;
    rows.iter().map(item_of).collect()
}

/// 该仓库当前全部库存桶（全盘建单时快照成明细）。
///
/// 一桶一行（`(物料, 仓库, 库位桶)`），与 `biz_inventory` 的唯一键同口径；
/// 连 `biz_material` 取基础单位，物料已删的行不参与盘点。
pub async fn book_rows_of_warehouse(ex: &impl SqlExecutor, warehouse_id: Uuid) -> DbResult<Vec<NewStockTakeLine>> {
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列。
    let rows = ex
        .query_sql(
            "SELECT i.material_id, i.location_id, i.qty, i.avg_cost, m.unit_id \
               FROM biz_inventory i \
               JOIN biz_material m ON m.id = i.material_id AND m.deleted_at IS NULL \
              WHERE i.warehouse_id = ?::uuid \
              ORDER BY m.material_code, i.location_id NULLS FIRST"
                .to_string(),
            vec![Value::from(&warehouse_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(NewStockTakeLine {
                material_id: row.get_typed("material_id")?,
                unit_id: row.get_typed("unit_id")?,
                location_id: row.get_typed("location_id")?,
                book_qty: row.get_typed("qty")?,
                unit_cost: row.get_typed("avg_cost")?,
            })
        })
        .collect()
}

/// 单个库存桶的账面快照（抽盘用）：`(账面数, 移动加权单价)`；无行 → `(0, 0)`。
pub async fn book_of_bucket(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<(Decimal, Decimal)> {
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列。
    let row = ex
        .query_first_sql(
            "SELECT qty, avg_cost FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                  = COALESCE(NULLIF(?::text,'')::uuid, '00000000-0000-0000-0000-000000000000'::uuid)"
                .to_string(),
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::Text(location_id.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await?;
    match row {
        None => Ok((Decimal::ZERO, Decimal::ZERO)),
        Some(row) => Ok((row.get_typed("qty")?, row.get_typed("avg_cost")?)),
    }
}

/// 分页。返回 `(行, 总数)`。
///
/// 走 `scope::paginate_unscoped`（架构闸①：分页只能从那一个入口下去），
/// `?` 占位符由框架按参数顺序绑定 —— **不要**手写 `$1`/`$2`：驱动是扫 `?` 绑定参数的，
/// 手写编号会在第一个文本参数上抛 `error serializing parameter 1`。
pub async fn page(q: &StockTakeFilter) -> DbResult<(Vec<StockTakeRow>, i64)> {
    let mut clauses = vec!["deleted_at IS NULL".to_string()];
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = q.status.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        params.push(Value::Text(status.to_uppercase()));
        clauses.push("status = ?".to_string());
    }
    if let Some(warehouse_id) = q.warehouse_id {
        params.push(Value::from(&warehouse_id));
        clauses.push("warehouse_id = ?::uuid".to_string());
    }
    if let Some(take_type) = q.take_type.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        params.push(Value::Text(take_type.to_uppercase()));
        clauses.push("take_type = ?".to_string());
    }
    if let Some(keyword) = q.keyword.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        params.push(Value::Text(format!("%{keyword}%")));
        clauses.push("take_no ILIKE ?".to_string());
    }
    let base_sql = format!("SELECT {HEAD_COLUMNS} FROM {TABLE} WHERE {}", clauses.join(" AND "));
    let result = scope::paginate_unscoped(
        LIST_TARGET,
        &base_sql,
        params,
        Some(LIST_ORDER),
        q.page.max(1),
        q.page_size.clamp(1, 200),
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(row_of)
        .collect::<DbResult<Vec<StockTakeRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

// ==================== 第三本账：流水 ====================

/// 写一条盘点流水（`doc_type='STOCK_TAKE'`，数量与金额恒正，方向看 `txn_type`）。
pub async fn write_txn(ex: &impl SqlExecutor, txn: &TxnWrite) -> DbResult<()> {
    // P2-5A：流水 SQL 收口到 [`inventory_writer::insert_txn`]。
    inventory_writer::insert_txn(
        ex,
        &TxnRecord {
            material_id: txn.material_id,
            warehouse_id: txn.warehouse_id,
            txn_type: txn.txn_type,
            biz_type: txn.biz_type,
            doc_type: DOC_TYPE,
            doc_id: txn.doc_id,
            doc_no: txn.doc_no.as_str(),
            line_no: txn.line_no,
            qty: txn.qty.round_dp(4),
            unit_cost: txn.unit_cost.round_dp(6),
            amount: txn.amount.round_dp(4),
            before: txn.before,
            after: txn.after,
            operator: txn.operator,
            period_code: None,
            remark: None,
            now: txn.now,
        },
    )
    .await
    .map(|_| ())
}

// ==================== 过账：一行差异动三本账 ====================

/// 盘盈（`diff > 0`）：走**入库方向** —— 成本账加、库位账加、写 IN 流水。
///
/// 复用收货过账的 `stockin_write::{apply_cost, apply_location_qty}`（同一个移动加权实现），
/// 金额取 `after.amount − before.amount` 的**账上真实增量**。
pub async fn apply_gain(
    ex: &impl SqlExecutor,
    take: &StockTakeRow,
    item: &StockTakeItemRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Decimal> {
    let qty = item.diff_qty.round_dp(4);
    let unit_cost = item.unit_cost.round_dp(6);
    let cost = CostBasis {
        qty,
        amount: money(qty * unit_cost),
        unit_cost,
    };
    let applied =
        stockin_write::apply_cost(ex, item.material_id, take.warehouse_id, item.location_id, cost, now).await?;
    stockin_write::apply_location_qty(ex, item.material_id, take.warehouse_id, item.location_id, qty, now).await?;
    let amount = (applied.after.amount - applied.before.amount).round_dp(4);
    write_txn(
        ex,
        &TxnWrite {
            material_id: item.material_id,
            warehouse_id: take.warehouse_id,
            line_no: item.line_no,
            txn_type: TXN_TYPE_IN,
            biz_type: BIZ_TYPE_GAIN,
            doc_id: take.id,
            doc_no: take.take_no.clone(),
            qty,
            unit_cost,
            amount,
            before: applied.before,
            after: applied.after,
            operator,
            now,
        },
    )
    .await?;
    Ok(amount)
}

/// 盘亏（`diff < 0`）：走**出库方向** —— 成本账减（带负库存闸）、库位账减、写 OUT 流水。
///
/// 复用出库过账的 `stockout_model::{lock_bucket, deduct_cost, lock_location_row,
/// deduct_location_qty}`：锁成本账行 → 判可用量 → 扣减，三步同一事务
/// （先读后判会让并发出库双双读到「够」，账面变负）。
pub async fn apply_loss(
    ex: &impl SqlExecutor,
    take: &StockTakeRow,
    item: &StockTakeItemRow,
    policy: &stockout_model::WarehousePolicy,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Decimal> {
    let qty = (-item.diff_qty).round_dp(4);
    let label = item.material_id.to_string();
    let Some(bucket) = stockout_model::lock_bucket(ex, item.material_id, take.warehouse_id, item.location_id).await?
    else {
        return Err(DbError::InvalidValue(format!(
            "{NEGATIVE_PREFIX}物料 {label} 在仓库 {}（{}）该库位桶上没有库存行（可用 0），\
             本次盘亏 {qty} —— 账面本来就没有，不能盘亏；请先核对盘点明细或走盘盈",
            policy.warehouse_code, policy.warehouse_name
        )));
    };
    if !policy.allow_negative && bucket.qty < qty {
        return Err(DbError::InvalidValue(format!(
            "{NEGATIVE_PREFIX}物料 {label} 在仓库 {}（{}）本次盘亏 {qty}，账面可用 {}：\
             盘亏不能把库存冲成负数（该仓未开启「允许负库存」）——请先核对是否漏入库",
            policy.warehouse_code,
            policy.warehouse_name,
            bucket.qty.round_dp(4)
        )));
    }
    let applied = stockout_model::deduct_cost(ex, &bucket, qty, now).await?;
    let Some((location_row_id, location_qty)) =
        stockout_model::lock_location_row(ex, item.material_id, take.warehouse_id, item.location_id).await?
    else {
        return Err(DbError::InvalidValue(format!(
            "物料 {label} 在仓库 {}（{}）的库位账缺行：账账已经不一致，请先跑库存巡检修复库位账",
            policy.warehouse_code, policy.warehouse_name
        )));
    };
    if !policy.allow_negative && location_qty < qty {
        return Err(DbError::InvalidValue(format!(
            "{NEGATIVE_PREFIX}物料 {label} 在仓库 {}（{}）该库位的库位账只有 {}，\
             本次盘亏 {qty}（库位账与成本账口径不一致时取更小者）",
            policy.warehouse_code,
            policy.warehouse_name,
            location_qty.round_dp(4)
        )));
    }
    stockout_model::deduct_location_qty(ex, location_row_id, qty, now).await?;
    let before = InventorySnapshot {
        qty: bucket.qty,
        amount: bucket.amount,
        avg_cost: bucket.avg_cost,
    };
    let after = InventorySnapshot {
        qty: applied.after_qty,
        amount: applied.after_amount,
        avg_cost: applied.after_cost,
    };
    write_txn(
        ex,
        &TxnWrite {
            material_id: item.material_id,
            warehouse_id: take.warehouse_id,
            line_no: item.line_no,
            txn_type: TXN_TYPE_OUT,
            biz_type: BIZ_TYPE_LOSS,
            doc_id: take.id,
            doc_no: take.take_no.clone(),
            qty,
            unit_cost: bucket.avg_cost.round_dp(6),
            amount: applied.amount,
            before,
            after,
            operator,
            now,
        },
    )
    .await?;
    Ok(applied.amount)
}

/// 作废·盘盈行回滚：按**过账时写下的金额**原路冲减（复用收货作废的 `reverse_cost`）。
///
/// 库存已被消耗到不足时拒绝（`INV_SHORTFALL|` 前缀由调用方翻译成 409）：宁可让人先处理，
/// 也不把库存冲成负数。
pub async fn reverse_gain(
    ex: &impl SqlExecutor,
    take: &StockTakeRow,
    item: &StockTakeItemRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    let qty = item.diff_qty.round_dp(4);
    let amount = item.diff_amount.round_dp(4);
    let line = stockin_cancel::CancelLine {
        line_no: item.line_no,
        material_id: item.material_id,
        warehouse_id: take.warehouse_id,
        location_id: item.location_id,
        qty,
        amount,
        unit_cost: item.unit_cost.round_dp(6),
    };
    let applied =
        stockin_cancel::reverse_cost(ex, item.material_id, take.warehouse_id, item.location_id, &line, now).await?;
    stockin_cancel::reverse_location_qty(ex, item.material_id, take.warehouse_id, item.location_id, qty, now).await?;
    write_txn(
        ex,
        &TxnWrite {
            material_id: item.material_id,
            warehouse_id: take.warehouse_id,
            line_no: item.line_no,
            txn_type: TXN_TYPE_OUT,
            biz_type: BIZ_TYPE_CANCEL,
            doc_id: take.id,
            doc_no: take.take_no.clone(),
            qty,
            unit_cost: line.unit_cost,
            amount,
            before: applied.before,
            after: applied.after,
            operator,
            now,
        },
    )
    .await?;
    Ok(())
}

/// 作废·盘亏行回滚：把当时扣掉的数量与金额**原路加回**（复用出库作废的 `restore_cost`）。
pub async fn reverse_loss(
    ex: &impl SqlExecutor,
    take: &StockTakeRow,
    item: &StockTakeItemRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    let qty = (-item.diff_qty).round_dp(4);
    let amount = item.diff_amount.round_dp(4);
    let before = stockout_model::lock_bucket(ex, item.material_id, take.warehouse_id, item.location_id)
        .await?
        .map(|bucket| InventorySnapshot {
            qty: bucket.qty,
            amount: bucket.amount,
            avg_cost: bucket.avg_cost,
        })
        .unwrap_or_else(InventorySnapshot::zero);
    stockout_model::restore_cost(
        ex,
        item.material_id,
        take.warehouse_id,
        item.location_id,
        qty,
        amount,
        now,
    )
    .await?;
    stockout_model::restore_location_qty(ex, item.material_id, take.warehouse_id, item.location_id, qty, now).await?;
    let after = stockout_model::lock_bucket(ex, item.material_id, take.warehouse_id, item.location_id)
        .await?
        .map(|bucket| InventorySnapshot {
            qty: bucket.qty,
            amount: bucket.amount,
            avg_cost: bucket.avg_cost,
        })
        .unwrap_or(before);
    write_txn(
        ex,
        &TxnWrite {
            material_id: item.material_id,
            warehouse_id: take.warehouse_id,
            line_no: item.line_no,
            txn_type: TXN_TYPE_IN,
            biz_type: BIZ_TYPE_CANCEL,
            doc_id: take.id,
            doc_no: take.take_no.clone(),
            qty,
            unit_cost: item.unit_cost.round_dp(6),
            amount,
            before,
            after,
            operator,
            now,
        },
    )
    .await?;
    Ok(())
}

// ==================== 凭证：盘点差异 ====================

/// 一张盘点差异凭证的一行配对（盘盈或盘亏）。
#[derive(Debug, Clone, Copy)]
pub struct VoucherPair {
    /// 明细行号（凭证分录摘要里点名用）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 账上真实金额（恒正）。
    pub amount: Decimal,
    /// `true` = 盘盈（借 1405 / 贷 1901）；`false` = 盘亏（借 1901 / 贷 1405）。
    pub gain: bool,
}

/// 生成「盘点差异凭证」：盘盈行 借 1405 / 贷 1901；盘亏行 借 1901 / 贷 1405。
///
/// # 为什么一张单**一张**凭证、按行配对而不是汇总成 4 行
///
/// `biz_voucher_entry.aux_material_id` 要能按物料拆开看（问题「这批货到底差在哪」），
/// 汇总成 4 行就只剩一个总金额。贷 / 借两侧金额相同（每行都是同额对冲），
/// 所以凭证天然平衡，`post_voucher` 的平衡校验照样生效。
///
/// # 合计为 0 直接跳过（不是错误）
///
/// 零成本存量（`qty>0 / avg_cost=0 / amount=0`）盘点时差异金额恰好 0；
/// `post_voucher` 在 `借贷合计 <= 0` 会报错（那是对**单据**的护栏），
/// 所以这里提前 `Ok(None)` 并打 WARN —— 与出库单 / 序列号同口径。
pub async fn create_voucher(
    ex: &impl SqlExecutor,
    take: &StockTakeRow,
    pairs: &[VoucherPair],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let total: Decimal = pairs.iter().map(|pair| pair.amount).sum();
    if total.round_dp(4) <= Decimal::ZERO {
        tracing::warn!(
            take_id = %take.id,
            take_no = %take.take_no,
            "盘点差异金额为 0（零成本存量 / 实盘等于账面）：跳过凭证生成，只动库存与流水"
        );
        return Ok(None);
    }
    let inventory = voucher::account_required(ex, voucher::DEFAULT_INVENTORY_CODE, "库存商品").await?;
    let pending = voucher::account_required(ex, voucher::ACCOUNT_PENDING_GAIN_LOSS, "待处理财产损溢").await?;
    let mut lines = Vec::with_capacity(pairs.len() * 2);
    for pair in pairs {
        let amount = pair.amount.round_dp(4);
        if amount <= Decimal::ZERO {
            continue;
        }
        let base = lines.len() as i32;
        let summary = format!(
            "盘点{} {} 第 {} 行：物料 {} 差异金额 {}",
            if pair.gain { "盘盈" } else { "盘亏" },
            take.take_no,
            pair.line_no,
            pair.material_id,
            amount
        );
        let (debit_account, credit_account) = if pair.gain {
            (inventory.clone(), pending.clone())
        } else {
            (pending.clone(), inventory.clone())
        };
        lines.push(voucher::EntryLine {
            line_no: base + 1,
            summary: summary.clone(),
            account: debit_account,
            debit: amount,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: Some(pair.material_id),
            // 存货行挂仓库辅助核算；1901 是全公司口径的过渡科目，不挂仓库（与出库单同口径）。
            aux_warehouse_id: pair.gain.then_some(take.warehouse_id),
            aux_dept_id: None,
        });
        lines.push(voucher::EntryLine {
            line_no: base + 2,
            summary,
            account: credit_account,
            debit: Decimal::ZERO,
            credit: amount,
            aux_supplier_id: None,
            aux_material_id: Some(pair.material_id),
            aux_dept_id: None,
            aux_warehouse_id: (!pair.gain).then_some(take.warehouse_id),
        });
    }
    let summary = format!(
        "盘点差异 {}：盘盈 {} / 盘亏 {}",
        take.take_no, take.total_gain_amount, take.total_loss_amount
    );
    let voucher_id = voucher::post_voucher(
        ex,
        &voucher::PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: voucher::SOURCE_STOCK_TAKE,
            source_id: Some(take.id),
            source_no: Some(take.take_no.as_str()),
            voucher_date: take.take_date,
            summary,
            lines,
            remark: None,
            red_of: None,
            status: crate::business::erp::voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await?;
    Ok(Some(voucher_id))
}

/// 作废 → 红冲盘点差异凭证（找不到原凭证 = 当时零成本跳过，不阻断作废）。
pub async fn reverse_voucher(
    ex: &impl SqlExecutor,
    take: &StockTakeRow,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let voucher_date = (now + nik::prelude::chrono::Duration::hours(8)).date_naive();
    voucher::reverse_by_source(
        ex,
        voucher::SOURCE_STOCK_TAKE,
        take.id,
        voucher_date,
        reason,
        operator,
        now,
    )
    .await
}

/// 期间闸：盘点日期落在已结账月份时返回 `Some((期间, 状态))`。
pub async fn period_closed(ex: &impl SqlExecutor, date: NaiveDate) -> DbResult<Option<(String, String)>> {
    stockout_model::period_closed(ex, date).await
}

/// 仓库负库存策略（不存在 / 已删 → `None`）。
pub async fn warehouse_policy(
    ex: &impl SqlExecutor,
    warehouse_id: Uuid,
) -> DbResult<Option<stockout_model::WarehousePolicy>> {
    stockout_model::warehouse_policy(ex, warehouse_id).await
}
