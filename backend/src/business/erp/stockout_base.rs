// ============================================================================
// stockout_base.rs —— 出库单底座：常量 / 状态机 / 行结构 / 列清单 / 取号
//
// 与 `stockin_base.rs` 同构（同一个业务族），差别只有三处：
//   ① 方向相反：过账是**减法**（成本账 / 库位账 / 流水三本账都减），凭证借成本费用、贷存货；
//   ② `out_type` 四值决定借方科目（领用 6401 / 报废·盘亏 1901）；
//   ③ 状态机没有作废审批（本版本不给 STOCK_OUT 配审批流），`PENDING_POST` 是
//      「制单 → 过账」的交接位，由 `erp.separation_of_duties.post` 开关决定要不要分离。
//
// # 表结构（V0203 实测）
//
// ```text
// biz_stock_out       id, out_no, out_type, warehouse_id, out_date, status,
//                     total_qty, total_amount, voucher_id, reason, remark,
//                     posted_at, posted_by, cancelled_at, cancelled_by, cancel_reason,
//                     created_at, updated_at, create_by, update_by, deleted_at
// biz_stock_out_item  id, stock_out_id, line_no, material_id, unit_id, qty,
//                     unit_cost, amount, location_id, lot_no, remark, created_at, updated_at
// ```
// ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::voucher;
use crate::business::shared::scope;
use crate::business::shared::state_machine;

// ==================== 表名 / 常量 ====================

/// 单头表。
pub const TABLE: &str = "biz_stock_out";

/// 明细表（无 `deleted_at`，主单软删后明细留在库里）。
pub const ITEM_TABLE: &str = "biz_stock_out_item";

/// 状态：草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：待过账（已提交，等持过账权限的人过账；三本账一分没动）。
pub const STATUS_PENDING_POST: &str = "PENDING_POST";
/// 状态：已过账（三本账已减、凭证已生成）。
pub const STATUS_POSTED: &str = "POSTED";
/// 状态：已作废。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 出库类型：领用（借 6401 主营业务成本）。
pub const OUT_TYPE_ISSUE: &str = "ISSUE";
/// 出库类型：报废（借 1901 待处理财产损溢）。
pub const OUT_TYPE_SCRAP: &str = "SCRAP";
/// 出库类型：盘亏（借 1901 待处理财产损溢）。
pub const OUT_TYPE_LOSS: &str = "LOSS";
/// 出库类型：其他（借 6401，口径同领用）。
pub const OUT_TYPE_OTHER: &str = "OTHER";

/// 四种出库类型（唯一真相，DTO 校验与巡检共用）。
pub const OUT_TYPES: [&str; 4] = [
    OUT_TYPE_ISSUE,
    OUT_TYPE_SCRAP,
    OUT_TYPE_LOSS,
    OUT_TYPE_OTHER,
];

/// 库存流水方向：出（`ck_biz_inv_txn_type` 只认 IN / OUT，数量恒正）。
pub const TXN_TYPE_OUT: &str = "OUT";
/// 冲销流水的方向：作废出库单要把数量加回账上，只能写 IN（数量恒正）。
pub const TXN_TYPE_IN: &str = "IN";

/// 流水来源单据类型（`biz_inventory_txn.doc_type`）。
pub const DOC_TYPE_STOCK_OUT: &str = "STOCK_OUT";

/// 出库流水的业务类型（`biz_inventory_txn.biz_type`，四种出库各一个值）。
pub const BIZ_TYPE_ISSUE: &str = "ISSUE_OUT";

/// 单据来源类型：销售发货（`source_type`，V0285）。
///
/// 与 `out_type` 是**两个维度**：`out_type` 说「领用性质」（决定借方科目），
/// `source_type` 说「谁生成了这张单」（销售链 = 发货单过账）。
/// 销售出库固定 `out_type = OTHER`（借方 6401 主营业务成本）+ `source_type = SALES`。
pub const SOURCE_TYPE_SALES: &str = "SALES";

/// 报废流水的业务类型。
pub const BIZ_TYPE_SCRAP: &str = "SCRAP_OUT";
/// 盘亏流水的业务类型。
pub const BIZ_TYPE_LOSS: &str = "LOSS_OUT";
/// 其他出库流水的业务类型。
pub const BIZ_TYPE_OTHER: &str = "OTHER_OUT";
/// 作废冲销流水的业务类型（方向 IN）。
pub const BIZ_TYPE_STOCK_OUT_CANCEL: &str = "STOCK_OUT_CANCEL";

/// 单号前缀 / 取号类型（`OUT-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "OUT";
/// `biz_seq.seq_type`。
pub const SEQ_TYPE: &str = "STOCK_OUT";

/// 取号规则（`biz_seq` 缺失时 `voucher::next_doc_no` 会幂等补一行）。
pub const STOCK_OUT_SEQ: voucher::SeqSpec<'static> = voucher::SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "出库单号",
    remark: "出库单号：OUT-yyyyMMdd-000001，按天（CST）滚动",
};

/// 本卡负责的合法边（与 `state_machine::STOCK_OUT_EDGES` 同源）。
pub const LEGAL_EDGES: [(&str, &str); 6] = state_machine::STOCK_OUT_EDGES;

/// 列表默认排序（与收货 / 采购同款：日期倒序 + 单号倒序）。
pub const LIST_ORDER: &str = "ORDER BY out_date DESC, out_no DESC";

/// 列表分页目标（`SCOPE-EXEMPT`：出库单是 ERP 单据表，无归属列，可见性靠权限码）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_stock_out");

// ==================== 纯函数 ====================

/// 金额 / 单价舍入：四舍五入（与 PG `ROUND(numeric, n)` 同口径）。
pub fn money(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(4, rust_decimal_rounding())
}

/// 舍入策略：四舍五入（`rust_decimal` 默认是「四舍六入五成双」，与 PG 差 1 分钱）。
const fn rust_decimal_rounding() -> nik::prelude::rust_decimal::RoundingStrategy {
    nik::prelude::rust_decimal::RoundingStrategy::MidpointAwayFromZero
}

/// 状态中文名（错误文案 / 详情展示共用）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_PENDING_POST => "待过账",
        STATUS_POSTED => "已过账",
        STATUS_CANCELLED => "已作废",
        _ => "未知状态",
    }
}

/// 出库类型中文名。
pub fn out_type_label(out_type: &str) -> &'static str {
    match out_type {
        OUT_TYPE_ISSUE => "领用",
        OUT_TYPE_SCRAP => "报废",
        OUT_TYPE_LOSS => "盘亏",
        OUT_TYPE_OTHER => "其他",
        _ => "未知类型",
    }
}

/// 该类型走哪个借方科目（`6401 主营业务成本` / `1901 待处理财产损溢`）。
///
/// 报废与盘亏是**资产损失**（先挂待处理财产损溢，等审批定性后再转出），
/// 领用与其他是**营业消耗**（直接进成本）。这条映射是 `stockout_handler::post_out_tx`
/// 唯一取科目口径的地方，凭证函数不再自己判一遍。
pub fn debit_account_of(out_type: &str) -> (&'static str, &'static str) {
    match out_type {
        OUT_TYPE_SCRAP | OUT_TYPE_LOSS => (voucher::ACCOUNT_PENDING_GAIN_LOSS, "待处理财产损溢"),
        _ => (voucher::ACCOUNT_MAIN_COST, "主营业务成本"),
    }
}

/// `out_type` → 流水业务类型。
pub fn biz_type_of(out_type: &str) -> &'static str {
    match out_type {
        OUT_TYPE_SCRAP => BIZ_TYPE_SCRAP,
        OUT_TYPE_LOSS => BIZ_TYPE_LOSS,
        OUT_TYPE_OTHER => BIZ_TYPE_OTHER,
        _ => BIZ_TYPE_ISSUE,
    }
}

/// 该状态能否去目标状态（查注册表，不写第二份边表）。
pub fn can_transition(from: &str, to: &str) -> bool {
    LEGAL_EDGES
        .iter()
        .any(|(start, end)| *start == from && *end == to)
}

/// 非法流转的 400 文案（列出当前状态能去哪）。
pub fn transition_rejected_message(from: &str, to: &str) -> String {
    let allowed: Vec<String> = LEGAL_EDGES
        .iter()
        .filter(|(start, _)| *start == from)
        .map(|(_, end)| format!("{end}（{}）", status_label(end)))
        .collect();
    if allowed.is_empty() {
        return format!(
            "出库单当前状态 {}（{}）是终态，不能再流转到 {to}",
            from,
            status_label(from)
        );
    }
    format!(
        "出库单不允许从 {}（{}）流转到 {to}；当前状态能去：{}",
        from,
        status_label(from),
        allowed.join(" / ")
    )
}

// ==================== 入参结构 ====================

/// 建单 / 改单的单头字段。
#[derive(Debug, Clone)]
pub struct NewStockOut<'a> {
    /// 出库类型（四值之一，已校验）。
    pub out_type: &'a str,
    /// 出库仓库。
    pub warehouse_id: Uuid,
    /// 出库日期（决定会计期间）。
    pub out_date: NaiveDate,
    /// 出库事由（领用用途 / 报废原因 / 盘亏说明）。
    pub reason: Option<&'a str>,
    /// 备注。
    pub remark: Option<&'a str>,
    /// 单据来源类型（SO-10：`Some("SALES")` = 销售发货过账生成；手工建单 `None`）。
    ///
    /// 与 [`super::stockout_base::SOURCE_TYPE_SALES`] 配套，落 `biz_stock_out.source_type`
    /// （V0285）。来源 + 来源 id 上有**部分唯一索引**，同一来源只能有一张出库单。
    pub source_type: Option<&'a str>,
    /// 来源单据 id（销售链 = `biz_shipment.id`）。
    pub source_id: Option<Uuid>,
}

/// 建单 / 改单的一行明细。
///
/// 单价与金额**不在这里**：过账才知道账上的移动加权单价是多少，
/// 让客户端传等于给它一个「自己定价」的入口（成本结转金额必须是账上差额）。
#[derive(Debug, Clone)]
pub struct NewStockOutLine {
    /// 物料。
    pub material_id: Uuid,
    /// 单位（物料基础单位快照，由服务端带出）。
    pub unit_id: Uuid,
    /// 出库数量（> 0）。
    pub qty: Decimal,
    /// 库位账行 id（`biz_inventory_location.id`，可空 = 未分配库位桶）。
    pub location_id: Option<Uuid>,
    /// 批号（可空）。
    pub lot_no: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 单头合计（只有数量；金额要过账才算得出）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Totals {
    /// 数量合计。
    pub qty: Decimal,
}

impl Totals {
    /// 按明细汇总。
    pub fn of(lines: &[NewStockOutLine]) -> Self {
        let qty = lines.iter().fold(Decimal::ZERO, |acc, line| acc + line.qty);
        Self { qty: money(qty) }
    }
}

// ==================== 行结构 ====================

/// 单头列清单（`SELECT` 与 `from_row` 必须同步）。
pub const HEADER_COLUMNS: &str = "id, out_no, out_type, warehouse_id, out_date, status, \
     total_qty, total_amount, voucher_id, reason, remark, posted_at, posted_by, \
     cancelled_at, cancelled_by, cancel_reason, created_at, updated_at, create_by, update_by";

/// 单头行。
#[derive(Debug, Clone)]
pub struct StockOutRow {
    /// 主键。
    pub id: Uuid,
    /// 单号（`OUT-yyyyMMdd-000001`）。
    pub out_no: String,
    /// 出库类型。
    pub out_type: String,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 出库日期。
    pub out_date: NaiveDate,
    /// 状态。
    pub status: String,
    /// 数量合计。
    pub total_qty: Decimal,
    /// 成本结转金额合计（过账前为 0）。
    pub total_amount: Decimal,
    /// 凭证（过账生成 / 作废后指向红字凭证）。
    pub voucher_id: Option<Uuid>,
    /// 出库事由。
    pub reason: Option<String>,
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
    /// 最后修改人。
    pub update_by: Option<Uuid>,
}

impl StockOutRow {
    /// 行 → 结构体。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            out_no: row.get_typed("out_no")?,
            out_type: row.get_typed("out_type")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            out_date: row.get_typed("out_date")?,
            status: row.get_typed("status")?,
            total_qty: row.get_typed("total_qty")?,
            total_amount: row.get_typed("total_amount")?,
            voucher_id: row.get_typed("voucher_id")?,
            reason: row.get_typed("reason")?,
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
}

/// 明细列清单。
pub const ITEM_COLUMNS: &str = "id, stock_out_id, line_no, material_id, unit_id, qty, \
     unit_cost, amount, location_id, lot_no, remark";

/// 明细行。
#[derive(Debug, Clone)]
pub struct StockOutItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属出库单。
    pub stock_out_id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 单位。
    pub unit_id: Uuid,
    /// 出库数量。
    pub qty: Decimal,
    /// 过账时的移动加权单价快照（草稿为 0）。
    pub unit_cost: Decimal,
    /// 过账时账上真实扣减金额（草稿为 0）。
    pub amount: Decimal,
    /// 库位账行（`biz_inventory_location.id`）。
    pub location_id: Option<Uuid>,
    /// 批号。
    pub lot_no: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
}

impl StockOutItemRow {
    /// 行 → 结构体。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            stock_out_id: row.get_typed("stock_out_id")?,
            line_no: row.get_typed("line_no")?,
            material_id: row.get_typed("material_id")?,
            unit_id: row.get_typed("unit_id")?,
            qty: row.get_typed("qty")?,
            unit_cost: row.get_typed("unit_cost")?,
            amount: row.get_typed("amount")?,
            location_id: row.get_typed("location_id")?,
            lot_no: row.get_typed("lot_no")?,
            remark: row.get_typed("remark")?,
        })
    }
}

/// 列表筛选。
#[derive(Debug, Clone, Default)]
pub struct StockOutFilter {
    /// 关键词（单号 / 事由 / 备注）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 出库类型。
    pub out_type: Option<String>,
    /// 仓库。
    pub warehouse_id: Option<String>,
    /// 出库日期起。
    pub date_from: Option<NaiveDate>,
    /// 出库日期止。
    pub date_to: Option<NaiveDate>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}
