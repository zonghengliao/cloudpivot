//! 成本对象（生产批）契约层（成本核算 · ARCH-9 §5.2 #1–#6）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//! §3.1 ② （`biz_cost_object` 表结构）、§4.1（成本对象状态机 + 关批 5 条前置）、
//! §5.1（出参口径）、§5.2（API 清单）、§6.2.1（表单字段）。
//!
//! # 出参口径（与仓库现状一致，不造第二套形状）
//!
//! ```text
//! 分页      { list, total, page, page_size }
//! 详情      单头 + collect_summary{mat,labor,oh,total} + period_items[]
//!           + next_statuses[] + legal_edges[[from,to]]
//! 金额/数量  一律字符串（Decimal.to_string()），防前端 JS number 精度丢失
//! ```
//!
//! # 为什么关批的 5 条前置拆成「事实 + 判定」两段
//!
//! §4.1 写的 5 条前置里，4 条要查库（工时草稿 / 未归集领料 / 当期批次状态 /
//! 未过账领料），1 条看参数（`wip_qty` + `force_wip`）。若把「查库 + 判定」写成一坨，
//! 单测只能靠真库，而真库用例在没配 `ERP_DB_URL` 时是 SKIP —— 等于没测。
//!
//! 所以：
//!
//! * [`CloseFacts`] —— **查库结果**（model 层填）；
//! * [`evaluate_close`] —— **纯函数**（本文件），5 条前置各出一个 [`CloseBlocker`]，
//!   可以一条一行地写单测（[`tests`] 里 5 条前置各一个用例）。
//!
//! handler 只做「取事实 → 调判定 → 抛错 / 落库」，判定口径只有一份。
//!
//! # 为什么 `material_input_mode` 一期只放 `ONE_TIME`
//!
//! §3.1 ② 的列注释写死「一期只实现 ONE_TIME」，§6.2.1 又说前端另一项置灰提示
//! 「三期支持」。后端不重复「界面置灰」这种软约束：`AS_PROGRESS` 直接 400，
//! 报文里写清楚是几期的事（否则接口看着支持、结算时悄悄按一次投料算，是静默错账）。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== 值域（与 V0370 的 CHECK 逐字一致） ====================

/// 成本对象类型：本期唯一实现（§3.1 ② 列注释「一期只写 BATCH」）。
pub const OBJECT_TYPE_BATCH: &str = "BATCH";
/// 成本对象类型值域（`ck_biz_cost_object_type`）。
pub const OBJECT_TYPES: [&str; 3] = ["BATCH", "ORDER", "PRODUCT"];

/// 在产（可领料 / 可报工 / 参与分摊与结算）。
pub const STATUS_OPEN: &str = "OPEN";
/// 已关批（只读；不再领料报工，历史数据全保留）。
pub const STATUS_CLOSED: &str = "CLOSED";
/// 作废（建错了；只读）。
pub const STATUS_CANCELLED: &str = "CANCELLED";
/// 状态值域（`ck_biz_cost_object_status`）。
pub const STATUSES: [&str; 3] = [STATUS_OPEN, STATUS_CLOSED, STATUS_CANCELLED];

/// 投料方式：一次投入（一期唯一可选）。
pub const MODE_ONE_TIME: &str = "ONE_TIME";
/// 投料方式：随进度投入（三期）。
pub const MODE_AS_PROGRESS: &str = "AS_PROGRESS";
/// 投料方式值域（`ck_biz_cost_object_input`）。
pub const MODES: [&str; 2] = [MODE_ONE_TIME, MODE_AS_PROGRESS];

/// 关批拦下的那条业务错误码（`66055`，见 `shared::error_code`）。
pub const BLOCKED_CODE: i32 = error_code::ERP_COST_OBJECT_CLOSE_BLOCKED;

/// 拦下原因的分类码（前端可据此给出不同引导，例如「去补归集」「去录工时」）。
pub const BLOCK_DRAFT_LABOR: &str = "DRAFT_LABOR";
/// 拦下原因：已过账未归集的领料出库单。
pub const BLOCK_UNCOLLECTED_STOCK_OUT: &str = "UNCOLLECTED_STOCK_OUT";
/// 拦下原因：当期成本批次已锁定（审批通过 / 已结算 / 已结账）。
pub const BLOCK_PERIOD_LOCKED: &str = "PERIOD_LOCKED";
/// 拦下原因：还有未过账（草稿 / 待审）的领料单。
pub const BLOCK_UNPOSTED_STOCK_OUT: &str = "UNPOSTED_STOCK_OUT";
/// 拦下原因：还有在制数量且未显式确认。
pub const BLOCK_WIP_NOT_ZERO: &str = "WIP_NOT_ZERO";

// ==================== 入参 ====================

/// 列表查询（§5.2 #1 的 `keyword / status / product_id / workshop_dept_id /
/// start_from / start_to / page / page_size`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostObjectPageQuery {
    /// 关键字：对象号 / 产出批号 / 上游订单号（模糊）。
    pub keyword: Option<String>,
    /// 状态 `OPEN` / `CLOSED` / `CANCELLED`。
    pub status: Option<String>,
    /// 产出品。
    pub product_id: Option<String>,
    /// 生产车间。
    pub workshop_dept_id: Option<String>,
    /// 开工日期 ≥（含）。
    pub start_from: Option<String>,
    /// 开工日期 ≤（含）。
    pub start_to: Option<String>,
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（0 = 默认 20，上限 200）。
    pub page_size: Option<u64>,
}

/// 归一化后的列表筛选（model 只认这个，不认原始 query）。
#[derive(Debug, Clone)]
pub struct CostObjectFilter {
    /// 关键字（已转小写 LIKE 模式由 model 侧拼）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 产出品。
    pub product_id: Option<Uuid>,
    /// 生产车间。
    pub workshop_dept_id: Option<Uuid>,
    /// 开工日期起。
    pub start_from: Option<NaiveDate>,
    /// 开工日期止。
    pub start_to: Option<NaiveDate>,
    /// 页码（1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 新建请求（§5.2 #3）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostObjectCreateReq {
    /// 产出品（物料，只允许 `FINISHED` / `SEMI`）。
    pub product_id: Option<String>,
    /// 目标入库仓。
    pub target_warehouse_id: Option<String>,
    /// 目标库位（可空）。
    pub target_location_id: Option<String>,
    /// 计划数量（> 0）。
    pub plan_qty: Option<Decimal>,
    /// 生产车间。
    pub workshop_dept_id: Option<String>,
    /// 开工日期（`YYYY-MM-DD`）。
    pub start_date: Option<String>,
    /// 计划完工日期（≥ 开工日期）。
    pub due_date: Option<String>,
    /// 上游订单号（按订单生产时手填）。
    pub source_doc_no: Option<String>,
    /// 客户（按订单生产时）。
    pub customer_id: Option<String>,
    /// 产出批号（填了自动建 `biz_lot`，`source_type=PRODUCTION`）。
    pub lot_no: Option<String>,
    /// 投料方式（缺省 `ONE_TIME`）。
    pub material_input_mode: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 编辑请求（§5.2 #4）：**部分更新** —— 字段缺失 = 不改，`null` = 清空（可空字段）。
///
/// `product_id` 在本卡里保持「可写」，但 handler 会在**已有归集行**时拒绝改动
/// （§5.2 #4 备注「`product_id` 有归集后只读」）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostObjectUpdateReq {
    /// 产出品。
    pub product_id: Option<String>,
    /// 目标入库仓。
    pub target_warehouse_id: Option<String>,
    /// 目标库位（三态：缺失不改 / null 清空 / 有值）。
    #[serde(default, deserialize_with = "double_option")]
    pub target_location_id: Option<Option<String>>,
    /// 计划数量。
    pub plan_qty: Option<Decimal>,
    /// 生产车间。
    pub workshop_dept_id: Option<String>,
    /// 开工日期。
    pub start_date: Option<String>,
    /// 计划完工日期（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub due_date: Option<Option<String>>,
    /// 上游订单号（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub source_doc_no: Option<Option<String>>,
    /// 客户（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub customer_id: Option<Option<String>>,
    /// 产出批号（三态；改批号不动已建的 `biz_lot`，那是主数据）。
    #[serde(default, deserialize_with = "double_option")]
    pub lot_no: Option<Option<String>>,
    /// 投料方式（只认 `ONE_TIME`）。
    pub material_input_mode: Option<String>,
    /// 备注（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

/// 关批请求（§5.2 #6）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostObjectCloseReq {
    /// 带在制关批（§4.1 前置 ⑤ 的显式确认；`false` / 缺失 = 不确认）。
    pub force_wip: Option<bool>,
    /// 关批备注（落 `remark` 与状态日志 reason）。
    pub remark: Option<String>,
}

// ==================== 归一化规格 ====================

/// 归一化后的成本对象业务字段（建 / 改共用；落库与单测都拿它）。
#[derive(Debug, Clone)]
pub struct CostObjectSpec {
    /// 对象类型（一期恒 `BATCH`）。
    pub object_type: String,
    /// 产出品。
    pub product_id: Uuid,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 目标入库仓。
    pub target_warehouse_id: Uuid,
    /// 目标库位。
    pub target_location_id: Option<Uuid>,
    /// 计划数量。
    pub plan_qty: Decimal,
    /// 生产车间。
    pub workshop_dept_id: Uuid,
    /// 车间名快照（由 handler 从 `sys_dept` 取，建单时点写入）。
    pub workshop_dept_name: String,
    /// 开工日期。
    pub start_date: NaiveDate,
    /// 计划完工日期。
    pub due_date: Option<NaiveDate>,
    /// 上游订单号。
    pub source_doc_no: Option<String>,
    /// 客户。
    pub customer_id: Option<Uuid>,
    /// 投料方式。
    pub material_input_mode: String,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== 出参 ====================

/// 列表行（§5.2 #1 的 `list[]` 字段，一个不多一个不少）。
#[derive(Debug, Clone, Serialize)]
pub struct CostObjectListRes {
    /// 主键。
    pub id: String,
    /// 对象号 `CB-yyyyMMdd-NNNNNN`。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 产出品。
    pub product_id: String,
    /// 产出品名称（`biz_material.material_name`）。
    pub product_name: Option<String>,
    /// 生产车间。
    pub workshop_dept_id: String,
    /// 车间名称（实时 join 优先、建单快照兜底；见 model 头注释）。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: String,
    /// 累计完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_qty: String,
    /// 状态。
    pub status: String,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 上游订单号。
    pub source_doc_no: Option<String>,
    /// 归集总成本（料 + 工 + 费；`biz_cost_collect` 实时汇总）。
    pub cost_total: String,
}

/// 归集汇总（§5.2 #2 的 `collect_summary{mat,labor,oh,total}`）。
#[derive(Debug, Clone, Serialize)]
pub struct CollectSummary {
    /// 直接材料。
    pub mat: String,
    /// 直接人工。
    pub labor: String,
    /// 制造费用。
    pub oh: String,
    /// 合计 = 三项之和（由三项相加，不另查一次库）。
    pub total: String,
}

impl CollectSummary {
    /// 由三项构造（`total` 由三项相加，避免「三个数加起来不等于总数」）。
    pub fn of(mat: Decimal, labor: Decimal, oh: Decimal) -> Self {
        Self {
            mat: mat.to_string(),
            labor: labor.to_string(),
            oh: oh.to_string(),
            total: (mat + labor + oh).to_string(),
        }
    }
}

/// 成本计算单行（§5.2 #2 的 `period_items[]`；来自 `biz_cost_period_item`）。
#[derive(Debug, Clone, Serialize)]
pub struct CostPeriodItemRes {
    /// 期间 `yyyy-MM`（来自 `biz_cost_period`）。
    pub period_code: String,
    /// 批次状态（`DRAFT` / `SUBMITTED` / `APPROVED` / `SETTLED` / …）。
    pub period_status: String,
    /// 计算单行状态 `CALC` / `SETTLED`。
    pub status: String,
    /// 期初在制数量。
    pub wip_beg_qty: String,
    /// 期初在制成本。
    pub wip_beg_cost: String,
    /// 本期完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_end_qty: String,
    /// 完工进度%。
    pub completion_pct: String,
    /// 约当产量（材料）。
    pub equiv_material: String,
    /// 约当产量（转换成本）。
    pub equiv_conversion: String,
    /// 本期材料投入。
    pub mat_cur: String,
    /// 本期人工投入。
    pub labor_cur: String,
    /// 本期制造费用投入。
    pub oh_cur: String,
    /// 单位材料成本。
    pub unit_material: String,
    /// 单位人工成本。
    pub unit_labor: String,
    /// 单位制造费用。
    pub unit_overhead: String,
    /// 完工转出成本。
    pub finished_cost: String,
    /// 期末在制成本。
    pub wip_end_cost: String,
    /// 单位完工成本（→ 入库单价）。
    pub unit_finished_cost: String,
    /// 完工入库单（结算后才有）。
    pub stock_in_id: Option<String>,
}

/// 详情（§5.2 #2 + 建 / 改 / 关批成功后的统一回单）。
#[derive(Debug, Clone, Serialize)]
pub struct CostObjectDetailRes {
    /// 主键。
    pub id: String,
    /// 对象号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 产出品。
    pub product_id: String,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 产出品编码（前端显示「MAT-0001 钢轴」）。
    pub product_code: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 目标入库仓。
    pub target_warehouse_id: String,
    /// 目标仓名称。
    pub target_warehouse_name: Option<String>,
    /// 目标库位。
    pub target_location_id: Option<String>,
    /// 目标库位名称。
    pub target_location_name: Option<String>,
    /// 计划数量。
    pub plan_qty: String,
    /// 累计完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_qty: String,
    /// 期末在制完工进度%。
    pub completion_pct: String,
    /// 投料方式。
    pub material_input_mode: String,
    /// 上游订单号。
    pub source_doc_no: Option<String>,
    /// 客户。
    pub customer_id: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 生产车间。
    pub workshop_dept_id: String,
    /// 生产车间名称。
    pub workshop_dept_name: Option<String>,
    /// 开工日期。
    pub start_date: String,
    /// 计划完工日期。
    pub due_date: Option<String>,
    /// 状态。
    pub status: String,
    /// 关批时间（RFC3339）。
    pub closed_at: Option<String>,
    /// 关批人。
    pub closed_by: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 更新时间（RFC3339）。
    pub updated_at: String,
    /// 归集汇总。
    pub collect_summary: CollectSummary,
    /// 成本计算单（按期倒序）。
    pub period_items: Vec<CostPeriodItemRes>,
    /// 当前态可去的状态（读**有效边**：管理端禁用某条边后这里就不再出现它）。
    pub next_statuses: Vec<String>,
    /// 有效边表（与 `next_statuses` 同源，便于调试 / 单测对照）。
    pub legal_edges: Vec<(String, String)>,
}

// ==================== 关批：事实 + 判定 ====================

/// 关批前置的**查库事实**（model 层只填数，不做判断）。
#[derive(Debug, Clone, Default)]
pub struct CloseFacts {
    /// 该对象名下 `status='DRAFT'` 的工时台账行数（§4.1 前置 ①）。
    pub draft_labor: i64,
    /// 挂在该对象上、**已过账但没有归集行**的领料出库单（§4.1 前置 ②）。
    pub uncollected_stock_out: i64,
    /// 该对象有活动、且成本批次已锁定（状态 ∉ {DRAFT,SUBMITTED}）的期间
    /// `(period_code, status)`（§4.1 前置 ③）。
    pub locked_periods: Vec<(String, String)>,
    /// 挂在该对象上、**未过账**（DRAFT / 待审）的领料出库单数（§4.1 前置 ④）。
    pub unposted_stock_out: i64,
    /// 对象当前期末在制数量（§4.1 前置 ⑤）。
    pub wip_qty: Decimal,
    /// 用户是否显式确认「带在制关批」（§4.1 前置 ⑤ 的另一半）。
    pub force_wip: bool,
}

/// 一条关批拦截（`code` 给前端做引导，`message` 直接给用户看）。
#[derive(Debug, Clone, Serialize)]
pub struct CloseBlocker {
    /// 分类码，见 [`BLOCK_DRAFT_LABOR`] 等常量。
    pub code: &'static str,
    /// 人话（含条数 / 期间号，用户能据此去补数据）。
    pub message: String,
}

/// 关批判定（**纯函数**，5 条前置各产出一个 blocker；空 = 可关批）。
///
/// 顺序与 §4.1 的编号一致，前端 / 卡面排查时一眼对得上。
pub fn evaluate_close(facts: &CloseFacts) -> Vec<CloseBlocker> {
    let mut blockers = Vec::new();
    if facts.draft_labor > 0 {
        blockers.push(CloseBlocker {
            code: BLOCK_DRAFT_LABOR,
            message: format!(
                "还有 {} 条草稿工时台账没确认（`biz_cost_labor.status='DRAFT'`）：\
                 关批后再也归集不到人工成本，请先确认或删除这些行",
                facts.draft_labor
            ),
        });
    }
    if facts.uncollected_stock_out > 0 {
        blockers.push(CloseBlocker {
            code: BLOCK_UNCOLLECTED_STOCK_OUT,
            message: format!(
                "还有 {} 张领料出库单已过账但**没有归集**到本成本对象：\
                 先归集再关批，否则这批材料成本永远进不了成本计算单",
                facts.uncollected_stock_out
            ),
        });
    }
    if !facts.locked_periods.is_empty() {
        let list = facts
            .locked_periods
            .iter()
            .map(|(period, status)| format!("{period}({status})"))
            .collect::<Vec<_>>()
            .join("、");
        blockers.push(CloseBlocker {
            code: BLOCK_PERIOD_LOCKED,
            message: format!(
                "以下期间的**成本批次已锁定**：{list}。\
                 关批会改动已审批 / 已结算期间的归集基线，请先红冲该批次（或等它回到 DRAFT）",
            ),
        });
    }
    if facts.unposted_stock_out > 0 {
        blockers.push(CloseBlocker {
            code: BLOCK_UNPOSTED_STOCK_OUT,
            message: format!(
                "还有 {} 张挂在本对象上的领料出库单**未过账**（草稿 / 待审批）：\
                 关批后它们过账时找不到在产对象，请先过账或作废",
                facts.unposted_stock_out
            ),
        });
    }
    if facts.wip_qty > Decimal::ZERO && !facts.force_wip {
        blockers.push(CloseBlocker {
            code: BLOCK_WIP_NOT_ZERO,
            message: format!(
                "期末在制数量是 {}（不为 0）：带在制关批会把在制成本永久挂在已关闭的对象上，\
                 确认要关批请显式传 `force_wip=true`",
                facts.wip_qty
            ),
        });
    }
    blockers
}

/// 归一化投料方式：`AS_PROGRESS` 在一期直接拒绝（见模块头注释）。
pub fn normalize_input_mode(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let mode = match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => return Ok(MODE_ONE_TIME.to_string()),
        Some(text) => text.to_ascii_uppercase(),
    };
    match mode.as_str() {
        MODE_ONE_TIME => Ok(MODE_ONE_TIME.to_string()),
        MODE_AS_PROGRESS => Err(bad_request(
            "`material_input_mode='AS_PROGRESS'`（随进度投料）属三期能力，一期只支持 `ONE_TIME`（一次投入）",
        )),
        other => Err(bad_request(format!(
            "material_input_mode 只认 {MODES:?}（收到「{other}」）"
        ))),
    }
}

// ==================== 解析小件 ====================

/// 400 构造（本模块唯一出口，handler 不再各自 new）。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409 构造。
pub fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 解析 UUID（非法 → 400 + 指名）。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field} 不是合法的 UUID（收到「{raw}」）")))
}

/// 解析可空 UUID（空白串等同于空）。
pub fn parse_opt_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => parse_uuid(text, field).map(Some),
    }
}

/// 必填 UUID。
pub fn require_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request(format!("{field} 不能为空")))?;
    parse_uuid(text, field)
}

/// 解析 `YYYY-MM-DD`（10 位定长，形状先卡死再解析）。
pub fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw.trim();
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..].iter().all(u8::is_ascii_digit);
    if !shaped {
        return Err(bad_request(format!("{field} 必须是 YYYY-MM-DD（收到「{raw}」）")));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|error| bad_request(format!("{field} 不是合法日期（{error}）")))
}

/// 可选日期（空白串 = 不填）。
pub fn parse_opt_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => parse_date(text, field).map(Some),
    }
}

/// 清洗可空文本（trim 后空串 → `None`）。
pub fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 三态字段反序列化：缺失 ⇒ `None`（不改）；显式 `null` ⇒ `Some(None)`（清空）；
/// 有值 ⇒ `Some(Some(v))`。
fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ==================== 建 / 改：字段校验 ====================

/// 新建请求 → 归一化规格（**纯逻辑**：查库部分由 handler 补）。
///
/// 这里做的是「不看库就能判的」校验：必填、正数、日期先后、值域。
/// 「物料是不是产出品 / 仓库存不存在」必须查库，由 handler 的 `ensure_refs` 做。
pub fn spec_from_create(req: &CostObjectCreateReq) -> std::result::Result<CostObjectSpec, ApiError> {
    let plan_qty = require_positive_qty(req.plan_qty)?;
    let start_date = req
        .start_date
        .as_deref()
        .ok_or_else(|| bad_request("start_date（开工日期）不能为空"))
        .and_then(|raw| parse_date(raw, "start_date"))?;
    let due_date = parse_opt_date(req.due_date.as_deref(), "due_date")?;
    ensure_dates(start_date, due_date)?;
    Ok(CostObjectSpec {
        object_type: OBJECT_TYPE_BATCH.to_string(),
        product_id: require_uuid(req.product_id.as_deref(), "product_id")?,
        lot_no: clean_optional(req.lot_no.clone()),
        target_warehouse_id: require_uuid(req.target_warehouse_id.as_deref(), "target_warehouse_id")?,
        target_location_id: parse_opt_uuid(req.target_location_id.as_deref(), "target_location_id")?,
        plan_qty,
        workshop_dept_id: require_uuid(req.workshop_dept_id.as_deref(), "workshop_dept_id")?,
        // 车间名快照由 handler 查 `sys_dept` 后回填；这里先留空，落库前必须非空。
        workshop_dept_name: String::new(),
        start_date,
        due_date,
        source_doc_no: clean_optional(req.source_doc_no.clone()),
        customer_id: parse_opt_uuid(req.customer_id.as_deref(), "customer_id")?,
        material_input_mode: normalize_input_mode(req.material_input_mode.as_deref())?,
        remark: clean_optional(req.remark.clone()),
    })
}

/// 编辑请求 → 归一化规格（与当前值合并；**纯逻辑**）。
pub fn spec_from_update(
    current: &CostObjectSpec,
    req: &CostObjectUpdateReq,
) -> std::result::Result<CostObjectSpec, ApiError> {
    let mut spec = current.clone();
    if let Some(raw) = req.product_id.as_deref() {
        spec.product_id = parse_uuid(raw, "product_id")?;
    }
    if let Some(raw) = req.target_warehouse_id.as_deref() {
        spec.target_warehouse_id = parse_uuid(raw, "target_warehouse_id")?;
    }
    if let Some(location) = req.target_location_id.as_ref() {
        spec.target_location_id = parse_opt_uuid(location.as_deref(), "target_location_id")?;
    }
    if let Some(qty) = req.plan_qty {
        spec.plan_qty = require_positive_qty(Some(qty))?;
    }
    if let Some(raw) = req.workshop_dept_id.as_deref() {
        spec.workshop_dept_id = parse_uuid(raw, "workshop_dept_id")?;
    }
    if let Some(raw) = req.start_date.as_deref() {
        spec.start_date = parse_date(raw, "start_date")?;
    }
    if let Some(due) = req.due_date.as_ref() {
        spec.due_date = parse_opt_date(due.as_deref(), "due_date")?;
    }
    if let Some(no) = req.source_doc_no.as_ref() {
        spec.source_doc_no = clean_optional(no.clone());
    }
    if let Some(customer) = req.customer_id.as_ref() {
        spec.customer_id = parse_opt_uuid(customer.as_deref(), "customer_id")?;
    }
    if let Some(lot) = req.lot_no.as_ref() {
        spec.lot_no = clean_optional(lot.clone());
    }
    if let Some(mode) = req.material_input_mode.as_deref() {
        spec.material_input_mode = normalize_input_mode(Some(mode))?;
    }
    if let Some(remark) = req.remark.as_ref() {
        spec.remark = clean_optional(remark.clone());
    }
    ensure_dates(spec.start_date, spec.due_date)?;
    Ok(spec)
}

/// 计划数量必须 > 0（与 `ck_biz_cost_object_qty` 一致，提前给 400 而不是 500）。
fn require_positive_qty(value: Option<Decimal>) -> std::result::Result<Decimal, ApiError> {
    match value {
        Some(qty) if qty > Decimal::ZERO => Ok(qty),
        Some(qty) => Err(bad_request(format!("plan_qty（计划数量）必须 > 0（收到 {qty}）"))),
        None => Err(bad_request("plan_qty（计划数量）不能为空")),
    }
}

/// 日期先后（与 `ck_biz_cost_object_dates` 一致）。
fn ensure_dates(start: NaiveDate, due: Option<NaiveDate>) -> std::result::Result<(), ApiError> {
    if let Some(due) = due
        && due < start
    {
        return Err(bad_request(format!("due_date（{due}）不能早于 start_date（{start}）")));
    }
    Ok(())
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额 / 数量字面量")
    }

    fn create_req() -> CostObjectCreateReq {
        CostObjectCreateReq {
            product_id: Some(Uuid::now_v7().to_string()),
            target_warehouse_id: Some(Uuid::now_v7().to_string()),
            target_location_id: None,
            plan_qty: Some(dec("100")),
            workshop_dept_id: Some(Uuid::now_v7().to_string()),
            start_date: Some("2026-11-01".to_string()),
            due_date: Some("2026-11-30".to_string()),
            source_doc_no: Some("SO-0001".to_string()),
            customer_id: None,
            lot_no: Some("LOT-A".to_string()),
            material_input_mode: None,
            remark: None,
        }
    }

    // ---------- 权限 / 值域字面量 ----------

    #[test]
    fn status_vocabulary_matches_migration() {
        // 与 V0370 的 ck_biz_cost_object_status / _type / _input 逐字一致
        assert_eq!(STATUSES, ["OPEN", "CLOSED", "CANCELLED"]);
        assert_eq!(OBJECT_TYPES, ["BATCH", "ORDER", "PRODUCT"]);
        assert_eq!(MODES, ["ONE_TIME", "AS_PROGRESS"]);
        assert_eq!(OBJECT_TYPE_BATCH, "BATCH");
        assert_eq!(MODE_ONE_TIME, "ONE_TIME");
    }

    #[test]
    fn close_block_code_is_the_erp_cost_code() {
        assert_eq!(BLOCKED_CODE, 66055, "关批被拦的业务码必须是 66055");
        for code in [
            BLOCK_DRAFT_LABOR,
            BLOCK_UNCOLLECTED_STOCK_OUT,
            BLOCK_PERIOD_LOCKED,
            BLOCK_UNPOSTED_STOCK_OUT,
            BLOCK_WIP_NOT_ZERO,
        ] {
            assert!(!code.is_empty());
            assert!(code.chars().all(|c| c.is_ascii_uppercase() || c == '_'), "{code}");
        }
    }

    // ---------- 建单校验 ----------

    #[test]
    fn create_defaults_to_batch_and_one_time() {
        let spec = spec_from_create(&create_req()).expect("建单规格");
        assert_eq!(spec.object_type, OBJECT_TYPE_BATCH, "一期只写 BATCH");
        assert_eq!(spec.material_input_mode, MODE_ONE_TIME, "缺省一次投入");
        assert_eq!(spec.plan_qty, dec("100"));
        assert_eq!(spec.lot_no.as_deref(), Some("LOT-A"));
    }

    #[test]
    fn create_rejects_as_progress_mode() {
        let mut req = create_req();
        req.material_input_mode = Some("AS_PROGRESS".to_string());
        let error = spec_from_create(&req).expect_err("三期能力一期不许写");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("ONE_TIME"), "{}", error.msg);
    }

    #[test]
    fn create_rejects_non_positive_qty_and_missing_fields() {
        let mut req = create_req();
        req.plan_qty = Some(dec("0"));
        assert_eq!(spec_from_create(&req).expect_err("0 不行").status, 400);

        let mut req = create_req();
        req.plan_qty = Some(dec("-5"));
        assert_eq!(spec_from_create(&req).expect_err("负数不行").status, 400);

        let mut req = create_req();
        req.product_id = None;
        let error = spec_from_create(&req).expect_err("产品必填");
        assert!(error.msg.contains("product_id"), "{}", error.msg);

        let mut req = create_req();
        req.start_date = None;
        assert!(
            spec_from_create(&req)
                .expect_err("开工日期必填")
                .msg
                .contains("start_date")
        );
    }

    #[test]
    fn create_rejects_due_date_before_start() {
        let mut req = create_req();
        req.due_date = Some("2026-10-31".to_string());
        let error = spec_from_create(&req).expect_err("完工不能早于开工");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("due_date"), "{}", error.msg);
    }

    #[test]
    fn date_shape_is_ten_chars() {
        // chrono 的 %d 接受一位数日，接口契约只认定长
        assert!(parse_date("2026-11-01", "d").is_ok());
        assert!(parse_date("2026-11-1", "d").is_err());
        assert!(parse_date("2026/11/01", "d").is_err());
    }

    // ---------- 改单：部分更新 ----------

    #[test]
    fn update_merges_only_given_fields() {
        let current = spec_from_create(&create_req()).expect("建单规格");
        let req = CostObjectUpdateReq {
            plan_qty: Some(dec("150")),
            due_date: Some(None), // 显式清空
            ..Default::default()
        };
        let spec = spec_from_update(&current, &req).expect("改单");
        assert_eq!(spec.plan_qty, dec("150"));
        assert_eq!(spec.due_date, None, "显式 null 要清空");
        assert_eq!(spec.product_id, current.product_id, "没给的字段不动");
        assert_eq!(spec.start_date, current.start_date);
        assert_eq!(spec.lot_no, current.lot_no);
    }

    #[test]
    fn update_rejects_start_after_due() {
        let current = spec_from_create(&create_req()).expect("建单规格");
        let req = CostObjectUpdateReq {
            start_date: Some("2026-12-01".to_string()),
            ..Default::default()
        };
        let error = spec_from_update(&current, &req).expect_err("开工晚于完工");
        assert!(error.msg.contains("due_date"), "{}", error.msg);
    }

    // ---------- 关批 5 条前置：一条一个用例 ----------

    fn open_facts() -> CloseFacts {
        CloseFacts {
            draft_labor: 0,
            uncollected_stock_out: 0,
            locked_periods: Vec::new(),
            unposted_stock_out: 0,
            wip_qty: dec("0"),
            force_wip: false,
        }
    }

    #[test]
    fn clean_facts_can_close() {
        assert!(evaluate_close(&open_facts()).is_empty(), "5 条前置全过 ⇒ 可以关批");
    }

    /// 前置 ①：还有草稿工时台账。
    #[test]
    fn close_blocked_by_draft_labor() {
        let facts = CloseFacts {
            draft_labor: 3,
            ..open_facts()
        };
        let blockers = evaluate_close(&facts);
        assert_eq!(blockers.len(), 1, "只该命中 ①");
        assert_eq!(blockers[0].code, BLOCK_DRAFT_LABOR);
        assert!(blockers[0].message.contains('3'), "{}", blockers[0].message);
    }

    /// 前置 ②：有已过账但未归集的领料出库单（卡面点名的那条）。
    #[test]
    fn close_blocked_by_uncollected_stock_out() {
        let facts = CloseFacts {
            uncollected_stock_out: 2,
            ..open_facts()
        };
        let blockers = evaluate_close(&facts);
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code, BLOCK_UNCOLLECTED_STOCK_OUT);
        assert!(blockers[0].message.contains("归集"), "{}", blockers[0].message);
    }

    /// 前置 ③：当期成本批次已锁定（APPROVED / SETTLED / CLOSED）。
    #[test]
    fn close_blocked_by_locked_period() {
        let facts = CloseFacts {
            locked_periods: vec![("2026-10".to_string(), "SETTLED".to_string())],
            ..open_facts()
        };
        let blockers = evaluate_close(&facts);
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code, BLOCK_PERIOD_LOCKED);
        assert!(blockers[0].message.contains("2026-10"), "{}", blockers[0].message);
        assert!(blockers[0].message.contains("SETTLED"));
    }

    /// 前置 ④：还有未过账（草稿 / 待审）的领料单。
    #[test]
    fn close_blocked_by_unposted_stock_out() {
        let facts = CloseFacts {
            unposted_stock_out: 1,
            ..open_facts()
        };
        let blockers = evaluate_close(&facts);
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code, BLOCK_UNPOSTED_STOCK_OUT);
        assert!(blockers[0].message.contains("未过账"), "{}", blockers[0].message);
    }

    /// 前置 ⑤：期末在制不为 0 且没显式确认。
    #[test]
    fn close_blocked_by_wip_unless_forced() {
        let facts = CloseFacts {
            wip_qty: dec("12.5"),
            ..open_facts()
        };
        let blockers = evaluate_close(&facts);
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code, BLOCK_WIP_NOT_ZERO);
        assert!(blockers[0].message.contains("force_wip"), "{}", blockers[0].message);

        // 显式确认「带在制关批」⇒ 这条不再拦
        let forced = CloseFacts {
            force_wip: true,
            ..facts
        };
        assert!(evaluate_close(&forced).is_empty());
    }

    /// 五条同时命中：全部报出来（不是只报第一条）——用户一次就能把活干完。
    #[test]
    fn all_blockers_are_reported_together() {
        let facts = CloseFacts {
            draft_labor: 1,
            uncollected_stock_out: 1,
            locked_periods: vec![("2026-09".to_string(), "APPROVED".to_string())],
            unposted_stock_out: 1,
            wip_qty: dec("1"),
            force_wip: false,
        };
        let blockers = evaluate_close(&facts);
        assert_eq!(blockers.len(), 5, "5 条前置各一条");
        let codes: Vec<&str> = blockers.iter().map(|item| item.code).collect();
        assert_eq!(
            codes,
            [
                BLOCK_DRAFT_LABOR,
                BLOCK_UNCOLLECTED_STOCK_OUT,
                BLOCK_PERIOD_LOCKED,
                BLOCK_UNPOSTED_STOCK_OUT,
                BLOCK_WIP_NOT_ZERO,
            ],
            "顺序与 §4.1 的编号一致"
        );
        // 负数在制不拦（库 CHECK 不许它为负，这里按 >0 判，避免「-0.0000 也算有在制」）
        let negative = CloseFacts {
            wip_qty: dec("-0.0001"),
            ..open_facts()
        };
        assert!(evaluate_close(&negative).is_empty());
    }

    // ---------- 出参形状 ----------

    #[test]
    fn collect_summary_total_is_sum_of_three() {
        let summary = CollectSummary::of(dec("100.0000"), dec("20.0000"), dec("5.5000"));
        assert_eq!(summary.mat, "100.0000");
        assert_eq!(summary.labor, "20.0000");
        assert_eq!(summary.oh, "5.5000");
        assert_eq!(summary.total, "125.5000");
        let json = serde_json::to_value(&summary).expect("序列化");
        assert_eq!(json["total"], serde_json::json!("125.5000"));
    }
}
