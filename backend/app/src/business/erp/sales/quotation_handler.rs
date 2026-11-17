//! 销售报价单接口层（销售链 · M3-02）：CRUD + 状态机（草稿 → 已发出 → 已接受 / 已拒绝）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/quotation/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/quotation` | `business:erp:quotation:create` | 建单（`DRAFT`，取号 `QT-yyyyMMdd-000001`） |
//! | PUT | `/business/erp/quotation/{id}` | `business:erp:quotation:update` | 改单（**只有 DRAFT**，明细整单替换） |
//! | GET | `/business/erp/quotation/{id}` | `business:erp:quotation:list` | 详情（含明细 + 客户编码/名称 + 是否过期 + 审批） |
//! | GET | `/business/erp/quotation/page` | `business:erp:quotation:list` | 分页（`quote_no` 模糊 / `status` / `customer_id` / `expired`） |
//! | PATCH | `/business/erp/quotation/{id}/status` | `business:erp:quotation:status` | 状态流转（**只走 `LEGAL_EDGES` 的 6 条边**，其余 409） |
//! | DELETE | `/business/erp/quotation/{id}` | `business:erp:quotation:delete` | 软删（**只有 DRAFT / CANCELLED**） |
//! | POST | `/business/erp/quotation/{id}/convert` | `business:erp:quotation:convert` | **转销售订单（带行）**：ACCEPTED → 新建 SO + 回填 `converted_so_id`（M3-03） |
//! | POST | `/business/erp/quotation/{id}/submit-approval` | `business:erp:quotation:status` | **提交审批**（M3-04；发起 `quotation_approve`，走 flow 不硬编码） |
//!
//! # 状态机（唯一真相：`state_machine::QUOTATION_EDGES`）
//!
//! ```text
//! DRAFT ──发出──▶ SENT ──客户接受──▶ ACCEPTED
//!   │              │  └──客户拒绝──▶ REJECTED（终态）
//!   └──作废──▶ CANCELLED ◀──撤回 / 客户反悔──┘
//! ```
//!
//! - 非法迁移一律 **409 + 可选去向**（不是 500、也不是静默忽略）；
//! - 需要原因的边（客户拒绝、已发出后的两条作废）缺 `reason` → **400**；
//! - 流转落库走两道：`state_machine::transition`（边表校验 + 写 `biz_status_log`）→
//!   `quotation_model::apply_status`（CAS `WHERE status = from`），同一事务。
//!
//! # 三个业务守卫（卡面「状态流转有校验」的落点）
//!
//! | 边 | 守卫 | 为什么 |
//! |----|------|--------|
//! | `DRAFT → SENT` | ① ≥1 行明细 ② 客户未冻结 ③ 有效期未过 | 空单 / 冻结客户 / 过期价发给客户，都是对外承诺 |
//! | `SENT → ACCEPTED` | 有效期未过 | **过期价不能被接受**（`valid_until < 今天` → 409，要重报新单） |
//! | `ACCEPTED → CANCELLED` | 未转销售订单（`converted_so_id IS NULL`） | 已转单的报价作废，销售订单就失去了来源（M3-03 回链） |
//!
//! 「已过期」一律**现算**（`today > valid_until`，按 CST），不进状态机、不落状态列 ——
//! M3-01 建表注释的既定口径（做过期状态就要定时翻转 + 时钟回拨对账）。
//!
//! # 转销售订单（M3-03）
//!
//! `POST /quotation/{id}/convert` —— 一键转单，**带行**、**保留来源**、**幂等**：
//!
//! | 事项 | 口径 |
//! |------|------|
//! | 可转状态 | **只有 `ACCEPTED`**（客户已接受 = 已成交）。`SENT` 先走 `PATCH /status` 标接受（那条边自带「未过期」守卫） |
//! | 带行 | 报价明细逐行搬成订单明细：物料 / 数量 / 单价 / 税率 / 行金额**整列搬**，不重新报价、不重新算金额 |
//! | 金额守卫 | 行金额必须等于「单价 × 数量」重算值；明细汇总必须等于单头三项合计（对不上 → 409，先修报价单） |
//! | 保留来源 | ① 结构化：`biz_quotation.converted_so_id` + `converted_at`（V0432 的部分唯一索引保证一张订单只被一张报价认领）；② 人可读：订单备注固定追加「由报价单 QT-… 转换」；③ 轨迹：`biz_status_log` 一行 `ACCEPTED→ACCEPTED, action=CONVERT, biz_ref=SALES_ORDER/SO-…` |
//! | 幂等 | 已转过再点 → **不再建第二张**，200 回原订单（`meta.already_converted = true`） |
//! | 事务 | 建订单 + 回填报价单 + 写轨迹 **同一个事务**；任一步失败整笔回滚（约定 11：不留孤订单、不留只回链一半的单） |
//! | 并发 | 转单前 `SELECT … FOR UPDATE` 锁报价单行 —— 两个并发请求只有一个能建单 |
//! | 币种 | 报价单没有币种列 ⇒ 按账套本位币记（`exchange_rate = 1`）；外币报价要转外币单是另一个决定（报价单先补币种列） |
//! | 部分转单 | **不支持**：请求体带 `line_nos` / `lines` / `items` / … 一律 **400 + 专用文案**（理由见下） |
//! | 客户 | 必须存在、未软删、`ACTIVE`（冻结客户 → 409）；已接受但**今天已过期**仍可转（接受发生在有效期内，过期约束的是「接受时」） |
//!
//! ## 为什么「部分转单」是拒绝而不是漏做
//!
//! V0432 的回链是**单值**（`biz_quotation.converted_so_id` + `converted_at`），
//! `uk_biz_quotation_converted_so` 再钉死「一张销售订单只被一张报价单认领」
//! ⇒ 一张报价单只能换出**一张**订单，换完即用尽（再点走幂等回原单）。
//! 客户只接受部分明细时：**新开一张报价单只保留客户接受的行**再转，或整单转换后
//! 在还是 `DRAFT` 的销售订单里删行（有审计）。若把 `line_nos` 这类参数静默忽略，
//! 调用方以为只转了 2 行、库里却是含全部明细的订单 —— 那张单要占信用、要出库、要开票，
//! 界面上也看不出多转了几行。真要「一张报价单分批转多张订单」得改表结构
//! （行级已转数量 + 转换多对多），不是给接口加个参数。
//!
//! 新订单状态 = `DRAFT`：转单只把「成交价」搬过去，提交审批 / 占信用 / 出库仍走订单自己的流程。
//!
//! # 不碰的东西
//!
//! - **审批不是状态**：审批是 `DRAFT → SENT` 的**前置条件**（M3-04）。
//!   `biz_quotation.status` 五值域一个字没改，`ck_biz_quotation_status` 也没动 ——
//!   口径与收款退款（`receipt_refund_bridge`）同一条路线，理由见那边模块注释。
//! - **不写库存 / 不生成凭证**：转单只建 `DRAFT` 订单，一分钱一分货都没动；
//! - **不改订单里的价格**：报价单是客户认过的价，转单只是把它搬进订单。

//! # 审批流接线（M3-04）
//!
//! | 动作 | 谁做 | 单据状态 |
//! |------|------|----------|
//! | 提交审批 `POST /quotation/{id}/submit-approval` | 人（`quotation:status` 码） | **仍是 `DRAFT`**；发起 `quotation_approve` 实例 |
//! | 审批通过 | 流程引擎回调 `quotation_approval_bridge::on_quotation_settled` | **自动发出** `DRAFT → SENT`（与人工发出同一条路径） |
//! | 驳回 / 撤回 / 终止 | 同上 | 仍是 `DRAFT`（价格一分没对外，可改完再提） |
//! | 直接 `PATCH status=SENT` | 人 | **409 + 66088**：没有 APPROVED 实例就发不出去 |
//!
//! 「没配流程」（`biz_flow_def` 里没有已发布的 `quotation_approve`）走
//! `approval_gate::authorize_missing_flow`：**默认 409**，只有持 `business:erp:skip-approval`
//! 且写明理由（≥10 字）的账号才能显式跳过 —— 跳过时**直接发出**（理由写进状态日志），
//! 否则报价单会永远停在草稿且没人能发。
//!
//! 审批在跑时（有 `PENDING` 实例）不许改单 / 删单 / 换状态（409，报文给审批单号）；
//! 作废（→`CANCELLED`）例外：**同一事务**里把在跑的审批撤掉（`cancel_approval_of`），
//! 不留孤儿待办。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_quotation / biz_quotation_line 是 ERP 单据表（V0432 无 dept_id /
//! 归属人列），数据范围表达不出来；可见性由 business:erp:quotation:* 权限码控制。
//! ```

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::customer::customer_model::{self, CustomerRow, STATUS_ACTIVE};
use crate::business::erp::inventory_transit::model as transit_model;
use crate::business::erp::sales::quotation_approval_bridge;
use crate::business::erp::sales::quotation_model::{
    self as model, NewLine, NewQuotation, QuotationLineRow, QuotationRow, Totals,
};
use crate::business::erp::sales::sales_order_model as so_model;
use crate::business::erp::{approval_gate, currency, doc_shape};
use crate::business::flow::{engine, model as flow_model};
use cloudpivot_kernel::business::shared::state_machine;
use cloudpivot_kernel::shared::error_code;

// ==================== 权限码（与 V0433 / V0434 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 详情。
pub const PERM_LIST: &str = "business:erp:quotation:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:quotation:create";
/// 编辑（只有 DRAFT）。
pub const PERM_UPDATE: &str = "business:erp:quotation:update";
/// 删除（软删，只有 DRAFT / CANCELLED）。
pub const PERM_DELETE: &str = "business:erp:quotation:delete";
/// 状态流转。
pub const PERM_STATUS: &str = "business:erp:quotation:status";
/// 转销售订单（M3-03；V0434 种的行）。
pub const PERM_CONVERT: &str = "business:erp:quotation:convert";

/// 报价单域全部权限码（顺序 = V0433 + V0434 的 VALUES 顺序；`perm_menu_gate.py` 要求
/// 每个都在 `sys_menu` 有承载行）。
pub const QUOTATION_PERMISSIONS: [&str; 6] = [
    PERM_LIST,
    PERM_CREATE,
    PERM_UPDATE,
    PERM_DELETE,
    PERM_STATUS,
    PERM_CONVERT,
];

/// 转单在 `biz_status_log.action` 里的动作码（状态不变，动作是「转单」）。
///
/// 借用固定 32 字符以内的动词，前端时间线据此显示「转销售订单 SO-…」而不是
/// 一行读起来像脏数据的 `ACCEPTED → ACCEPTED`。
pub const CONVERT_ACTION: &str = "CONVERT";

/// 一张报价单最多多少行明细（与销售订单 / 采购单同口径：超过就是导入脚本出问题了）。
pub const MAX_LINES: usize = 200;

// ==================== 校验原语 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404`：报价单不存在 / 已删。
fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "报价单不存在或已删除".to_string())
}

/// `409`：状态 / 并发冲突（当前态与请求态不匹配）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 路径 / 请求体里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
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
            format!("{field} 不是合法 UUID：`{text}`"),
        )
    })
}

/// 请求体里的日期：`yyyy-MM-dd`；非法 → 400（不做「猜格式」）。
fn parse_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d").map(Some).map_err(|_| {
            ApiError::new(
                400,
                error_code::ERP_FILTER_INVALID,
                format!("{field} 必须是 yyyy-MM-dd（当前 `{text}`）"),
            )
        }),
    }
}

/// 写库错误：唯一键 / CHECK 违反翻译成 409 / 400，其余交给底座（5xx 带上下文）。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match err {
        // 存活行单号唯一 / 行号单内唯一
        DbError::UniqueViolation { .. } => conflict(format!("{ctx}失败：单号或行号冲突，请重试")),
        // ck_biz_quotation_* / ck_biz_quotation_line_*
        DbError::InvalidValue(text) => bad_request(format!("{ctx}失败：{text}")),
        other => cloudpivot_kernel::utils::db_error(other, ctx),
    }
}

// ==================== 入参 DTO ====================

/// 一行明细入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QuotationLineReq {
    /// 物料 id（UUID；弱引用 `biz_material.id`）。
    pub material_id: String,
    /// 报价数量（> 0）。
    #[serde(default)]
    pub qty: Option<Decimal>,
    /// 不含税单价（≥ 0，留空 = 0）。
    #[serde(default)]
    pub price: Option<Decimal>,
    /// 税率百分数（0–100，留空 = 0）。
    #[serde(default)]
    pub tax_rate: Option<Decimal>,
    /// 行备注（交期 / 替代料说明等）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 新建报价单。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateQuotationReq {
    /// 客户 id（UUID；必须存在、未软删且 `ACTIVE`）。
    pub customer_id: String,
    /// 报价日期（`yyyy-MM-dd`，留空 = 今天 CST）。
    #[serde(default)]
    pub quote_date: Option<String>,
    /// 有效期至（**必填**，`yyyy-MM-dd`，`>= quote_date`）。
    ///
    /// 不默认：有效期是报价的关键条款（给多少天是商业决定），
    /// 默认一个值等于替销售做了承诺 —— 客户拿着「30 天有效」来压价时，
    /// 系统里到底约定过几天必须是本人录进去的。
    pub valid_until: String,
    /// 备注（付款条件 / 报价说明等）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 明细（**至少 1 行**，最多 [`MAX_LINES`] 行）。
    #[serde(default)]
    pub items: Vec<QuotationLineReq>,
}

/// 编辑报价单（**只有 DRAFT**；明细传了就整单替换）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateQuotationReq {
    /// 客户 id（**不传 = 不改**）。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 报价日期（不传 = 不改）。
    #[serde(default)]
    pub quote_date: Option<String>,
    /// 有效期至（不传 = 不改）。
    #[serde(default)]
    pub valid_until: Option<String>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 明细（**不传 = 保留现有明细**；传了 = 整单替换，不能为空）。
    #[serde(default)]
    pub items: Option<Vec<QuotationLineReq>>,
}

/// 状态流转请求。
#[derive(Debug, Clone, Deserialize)]
pub struct QuotationStatusReq {
    /// 目标状态（5 值之一，且必须是 `LEGAL_EDGES` 里的合法边）。
    pub target_status: String,
    /// 原因（客户拒绝 / 已发出后作废必填）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 提交审批请求（M3-04；**可以完全不带 body**）。
///
/// 唯一用途是「没配 `quotation_approve` 流程时显式跳过审批」这个例外：
/// 配了流程时带 `skip_approval` 会 400（不许绕过已配置的内控）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QuotationSubmitReq {
    /// 跳过审批片段（`skip_approval` / `skip_reason`）。
    #[serde(flatten, default)]
    pub approval: approval_gate::SkipApprovalReq,
}

/// 转销售订单请求（**可以完全不带 body**；`{}` 与无 body 都当默认值）。
///
/// 四个参数全是「这张订单怎么来的」的可选项，**没有「转哪几行」** ——
/// 本接口只做**整单转单**（见 [`PARTIAL_CONVERT_KEYS`] 的注释）。
/// `deny_unknown_fields` 是刻意的：多写一个键（拼错的 `order_dtae`、或者调用方以为
/// 能传 `line_nos` 做部分转单）必须 **400 报出来**，不能被 serde 静默丢掉 ——
/// 静默丢参数会让「用户以为交期设上了 / 只转了两行」，而库里是另一回事。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConvertQuotationReq {
    /// 下单日期（`yyyy-MM-dd`，留空 = 今天 CST）。**默认今天，不是报价日期** ——
    /// 订单日期是「客户下单那天」，报价可能是两周前发的。
    #[serde(default)]
    pub order_date: Option<String>,
    /// 要求交期（`yyyy-MM-dd`，留空 = 未约定；不能早于 `order_date`）。
    #[serde(default)]
    pub delivery_date: Option<String>,
    /// 销售员 id（留空 = 不指定；填了必须存在且启用）。
    #[serde(default)]
    pub salesperson_id: Option<String>,
    /// 备注（留空 = `由报价单 QT-… 转换`；填了会拼在来源前面，来源不会被覆盖）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 分页筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QuotationFilter {
    /// 单号模糊。
    #[serde(default)]
    pub quote_no: Option<String>,
    /// 状态精确（5 值之一）。
    #[serde(default)]
    pub status: Option<String>,
    /// 客户精确。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 有效期筛选：`true` = 只要已过期、`false` = 只要未过期（含今天）、不传 = 全部。
    ///
    /// 「已过期」现算（`valid_until < 今天`），不是库里的状态值。
    #[serde(default)]
    pub expired: Option<bool>,
    /// 页码（1 起）。
    #[serde(default)]
    pub page: u64,
    /// 每页条数（0 = 默认 20，上限 200）。
    #[serde(default)]
    pub page_size: u64,
}

// ==================== 出参 DTO ====================

/// 明细出参。
#[derive(Debug, Clone, Serialize)]
pub struct QuotationLineRes {
    /// 主键。
    pub id: String,
    /// 行号（1 起）。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（软删物料 → `None`）。
    pub material_code: Option<String>,
    /// 物料名称（软删物料 → `None`）。
    pub material_name: Option<String>,
    /// 规格。
    pub spec: Option<String>,
    /// 计量单位名。
    pub unit_name: Option<String>,
    /// 数量。
    pub qty: Decimal,
    /// 不含税单价。
    pub price: Decimal,
    /// 行不含税金额。
    pub amount: Decimal,
    /// 税率百分数。
    pub tax_rate: Decimal,
    /// 行税额。
    pub tax_amount: Decimal,
    /// 行含税金额（= `amount + tax_amount`）。
    pub with_tax: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

impl QuotationLineRes {
    /// 由行结构构造（物料名由调用方批量补齐，避免 N+1）。
    fn of(row: &QuotationLineRow) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            material_id: row.material_id.to_string(),
            material_code: None,
            material_name: None,
            spec: None,
            unit_name: None,
            qty: row.qty,
            price: row.price,
            amount: row.amount,
            tax_rate: row.tax_rate,
            tax_amount: row.tax_amount,
            with_tax: row.with_tax(),
            remark: row.remark.clone(),
        }
    }
}

/// 单头出参。
#[derive(Debug, Clone, Serialize)]
pub struct QuotationRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub quote_no: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码（软删客户 → `None`）。
    pub customer_code: Option<String>,
    /// 客户名称（软删客户 → `None`）。
    pub customer_name: Option<String>,
    /// 报价日期。
    pub quote_date: NaiveDate,
    /// 有效期至（含当天）。
    pub valid_until: NaiveDate,
    /// **是否已过期**（现算 `today > valid_until`，不是状态）。
    pub expired: bool,
    /// 状态（5 值之一）。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 转成的销售订单 id（M3-03 回填；`None` = 未转单）。
    pub converted_so_id: Option<String>,
    /// 转单时间。
    pub converted_at: Option<String>,
    /// 最近一轮审批实例 id（M3-04 / V0435；`None` = 从没提交过审批）。
    pub approval_instance_id: Option<String>,
    /// 审批摘要（**详情页**才有；列表页为 `None`，避免每行回查实例 —— 防 N+1）。
    ///
    /// 前端不要只看 `status` 判断「审批中」：审批不是状态，`status` 仍是 `DRAFT`，
    /// 「已提交待批」只能从这里读（`approval.status == PENDING`）。
    pub approval: Option<QuotationApprovalRes>,
    /// 备注。
    pub remark: Option<String>,
    /// 现在能不能改（= 状态是 `DRAFT`）。
    pub editable: bool,
    /// 现在能不能删（= 状态是 `DRAFT` / `CANCELLED`）。
    pub deletable: bool,
    /// 现在能流转到哪些状态（前端按它显示按钮；空 = 终态）。
    pub allowed_targets: Vec<String>,
    /// 明细（列表页为空数组，详情页带全量）。
    pub items: Vec<QuotationLineRes>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<String>,
    /// 更新人。
    pub update_by: Option<String>,
}

impl QuotationRes {
    /// 由行结构构造（客户 / 物料名由调用方补齐）。
    fn of(row: &QuotationRow, items: Vec<QuotationLineRes>, today: NaiveDate) -> Self {
        Self {
            id: row.id.to_string(),
            quote_no: row.quote_no.clone(),
            customer_id: row.customer_id.to_string(),
            customer_code: None,
            customer_name: None,
            quote_date: row.quote_date,
            valid_until: row.valid_until,
            expired: row.expired_on(today),
            status: row.status.clone(),
            status_label: model::status_label(&row.status).to_string(),
            total_amount: row.total_amount,
            total_tax: row.total_tax,
            total_with_tax: row.total_with_tax,
            converted_so_id: row.converted_so_id.map(|id| id.to_string()),
            converted_at: row.converted_at.map(|at| at.to_rfc3339()),
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            approval: None,
            remark: row.remark.clone(),
            editable: row.status == model::STATUS_DRAFT,
            deletable: model::DELETABLE_STATUSES.contains(&row.status.as_str()),
            allowed_targets: model::allowed_targets(&row.status)
                .into_iter()
                .map(str::to_string)
                .collect(),
            items,
            created_at: row.created_at,
            updated_at: row.updated_at,
            create_by: row.create_by.map(|id| id.to_string()),
            update_by: row.update_by.map(|id| id.to_string()),
        }
    }
}

// ==================== 审批摘要（M3-04） ====================

/// 报价单审批摘要（详情页 `approval` 段）。
///
/// 只读；提交审批走 `POST /quotation/{id}/submit-approval`，审批动作在审批中心
/// （`POST /business/flow/task/{id}/approve`）。
#[derive(Debug, Clone, Serialize)]
pub struct QuotationApprovalRes {
    /// 审批实例 id。
    pub instance_id: String,
    /// 审批单号（`FLW-yyyyMMdd-XXXXXXXX`）。
    pub instance_no: String,
    /// 实例状态（`PENDING` / `APPROVED` / `REJECTED` / `WITHDRAWN` / `CANCELLED`）。
    pub status: String,
    /// 实例状态中文名（前端徽标直接用，不再自己映射一遍）。
    pub status_label: String,
    /// 还在等几个人批（`PENDING` 时的待办条数；其它状态恒 0）。
    pub pending_tasks: i64,
}

impl QuotationApprovalRes {
    /// 由桥的摘要构造（状态中文名在这里补，避免前端维护第二份映射）。
    fn of(brief: &quotation_approval_bridge::ApprovalBrief) -> Self {
        Self {
            instance_id: brief.instance_id.to_string(),
            instance_no: brief.instance_no.clone(),
            status: brief.status.clone(),
            status_label: instance_status_label(&brief.status),
            pending_tasks: brief.pending_tasks,
        }
    }
}

/// 审批实例状态的中文名（未知值原样给「未知」，不 panic）。
fn instance_status_label(status: &str) -> String {
    match status.trim().to_ascii_uppercase().as_str() {
        "PENDING" => "审批中",
        "APPROVED" => "已通过",
        "REJECTED" => "已驳回",
        "WITHDRAWN" => "已撤回",
        "CANCELLED" => "已终止",
        _ => "未知",
    }
    .to_string()
}

// ==================== 纯逻辑：入参 → 行结构 ====================

/// 一行入参 → [`NewLine`]（金额在这里算，**不接受客户端传金额**）。
///
/// 校验与库层 CHECK 逐条对应，但给人话：
/// `qty > 0`（`ck_biz_quotation_line_qty`）、`price >= 0`、
/// `tax_rate ∈ [0, 100]`（`ck_biz_quotation_line_rate`）。
fn build_line(req: &QuotationLineReq, index: usize) -> std::result::Result<NewLine, ApiError> {
    let line_no = index + 1;
    let material_id = parse_uuid(&req.material_id, &format!("items[{index}].material_id"))?;
    let qty = req.qty.unwrap_or(Decimal::ZERO);
    if qty <= Decimal::ZERO {
        return Err(bad_request(format!("第 {line_no} 行数量必须大于 0（当前 {qty}）")));
    }
    let price = req.price.unwrap_or(Decimal::ZERO);
    if price < Decimal::ZERO {
        return Err(bad_request(format!("第 {line_no} 行单价不能为负（当前 {price}）")));
    }
    let tax_rate = req.tax_rate.unwrap_or(Decimal::ZERO);
    if tax_rate < Decimal::ZERO || tax_rate > Decimal::from(100) {
        return Err(bad_request(format!(
            "第 {line_no} 行税率必须在 0–100 之间（当前 {tax_rate}）"
        )));
    }
    let (amount, tax_amount) = model::line_amounts(qty, price, tax_rate);
    Ok(NewLine {
        material_id,
        qty,
        price,
        tax_rate,
        amount,
        tax_amount,
        remark: req
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
    })
}

/// 明细入参整体校验 + 转换（至少 1 行、最多 [`MAX_LINES`] 行）。
fn build_lines(items: &[QuotationLineReq], allow_empty: bool) -> std::result::Result<Vec<NewLine>, ApiError> {
    if items.is_empty() {
        if allow_empty {
            return Ok(Vec::new());
        }
        return Err(bad_request("报价单至少要 1 行明细"));
    }
    if items.len() > MAX_LINES {
        return Err(bad_request(format!(
            "一张报价单最多 {MAX_LINES} 行明细（当前 {} 行）",
            items.len()
        )));
    }
    items
        .iter()
        .enumerate()
        .map(|(index, req)| build_line(req, index))
        .collect()
}

// ==================== 业务守卫 ====================

/// 客户必须存在、未软删且 `ACTIVE`（冻结客户禁开新报价：与销售订单同口径）。
async fn require_active_customer(customer_id: Uuid) -> std::result::Result<CustomerRow, ApiError> {
    let customer = customer_model::customer_find_by_id(customer_id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单客户"))?
        .ok_or_else(|| bad_request("客户不存在或已删除"))?;
    if customer.status != STATUS_ACTIVE {
        return Err(bad_request(format!(
            "客户 `{}`（{}）状态是 {}，不是 ACTIVE：冻结客户不允许新开报价单",
            customer.code, customer.name, customer.status
        )));
    }
    Ok(customer)
}

/// `DRAFT → SENT` 的三道守卫：≥1 行明细 / 客户未冻结 / 有效期未过。
async fn ensure_sendable(
    row: &QuotationRow,
    lines: &[QuotationLineRow],
    today: NaiveDate,
) -> std::result::Result<(), ApiError> {
    if lines.is_empty() {
        return Err(bad_request(format!(
            "报价单 {} 没有明细，不能发出：至少要 1 行明细（空单发给客户等于报了个空价）",
            row.quote_no
        )));
    }
    require_active_customer(row.customer_id).await?;
    if row.expired_on(today) {
        return Err(conflict(format!(
            "报价单 {} 的有效期已过（valid_until = {}，今天是 {today}）：\
             请先改有效期再发出（发给客户一张天生过期的报价，客户接受时也走不通）",
            row.quote_no, row.valid_until
        )));
    }
    Ok(())
}

/// `SENT → ACCEPTED` 的守卫：有效期未过（**过期价不能被接受**）。
fn ensure_acceptable(row: &QuotationRow, today: NaiveDate) -> std::result::Result<(), ApiError> {
    if row.expired_on(today) {
        return Err(conflict(format!(
            "报价单 {} 已于 {} 过期（今天是 {today}），不能标记为「已接受」：\
             请按当前价格重新报价后再走接受（否则等于用旧价成交，财务/成本口径都会歪）",
            row.quote_no, row.valid_until
        )));
    }
    Ok(())
}

/// `ACCEPTED → CANCELLED` 的守卫：已转销售订单的不许作废。
fn ensure_cancellable(row: &QuotationRow) -> std::result::Result<(), ApiError> {
    if let Some(so_id) = row.converted_so_id {
        return Err(conflict(format!(
            "报价单 {} 已转成销售订单（{so_id}），不能作废：作废它会让那张订单失去来源。\
             要撤销成交请走销售订单的作废流程",
            row.quote_no
        )));
    }
    Ok(())
}

// ==================== 业务实现 ====================

/// 详情（单头 + 明细 + 客户名 + 物料名，软删行 404）。
async fn detail_of(id: Uuid) -> std::result::Result<QuotationRes, ApiError> {
    let today = model::today_cst(Utc::now());
    let row = model::find_quotation(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单"))?
        .ok_or_else(not_found)?;
    let lines = model::lines_of(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单明细"))?;
    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials: std::collections::HashMap<Uuid, transit_model::MaterialBrief> =
        transit_model::material_labels(&material_ids)
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "批量查询报价单物料"))?
            .into_iter()
            .map(|brief| (brief.id, brief))
            .collect();
    let items: Vec<QuotationLineRes> = lines
        .iter()
        .map(|line| {
            let mut res = QuotationLineRes::of(line);
            if let Some(brief) = materials.get(&line.material_id) {
                res.material_code = Some(brief.code.clone());
                res.material_name = Some(brief.name.clone());
                res.spec = brief.spec.clone();
                res.unit_name = brief.unit_name.clone();
            }
            res
        })
        .collect();
    let mut res = QuotationRes::of(&row, items, today);
    // 审批摘要（M3-04）：详情页一次往返带上，前端不必再打审批中心拿「谁在等谁批」
    res.approval = quotation_approval_bridge::latest_approval_of(db(), id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单审批实例"))?
        .as_ref()
        .map(QuotationApprovalRes::of);
    if let Some((_, code, name)) = model::customer_labels(&[row.customer_id])
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单客户"))?
        .into_iter()
        .next()
    {
        res.customer_code = Some(code);
        res.customer_name = Some(name);
    }
    Ok(res)
}

/// 新建报价单（`DRAFT`）。
pub async fn create_quotation(
    operator: Option<Uuid>,
    req: CreateQuotationReq,
) -> std::result::Result<QuotationRes, ApiError> {
    let customer_id = parse_uuid(&req.customer_id, "customer_id")?;
    let today = model::today_cst(Utc::now());
    let quote_date = parse_date(req.quote_date.as_deref(), "quote_date")?.unwrap_or(today);
    let valid_until = parse_date(Some(&req.valid_until), "valid_until")?
        .ok_or_else(|| bad_request("valid_until（有效期至）必填：有效期是报价的关键条款"))?;
    if valid_until < quote_date {
        return Err(bad_request(format!(
            "valid_until（{valid_until}）不能早于 quote_date（{quote_date}）：\
             建单当天就过期的报价只会在客户砍价时才被发现"
        )));
    }
    let lines = build_lines(&req.items, false)?;
    let customer = require_active_customer(customer_id).await?;
    let totals = Totals::of(&lines);
    // 金额恒等在这里也过一遍：算错就是 400，而不是等库层 CHECK 报 23514（对用户是 500 手感）
    if !model::totals_consistent(totals.amount, totals.tax, totals.with_tax)
        || !model::amounts_non_negative(totals.amount, totals.tax, totals.with_tax)
    {
        return Err(bad_request(format!(
            "金额自检失败：价税合计 {} ≠ 不含税 {} + 税额 {}",
            totals.with_tax, totals.amount, totals.tax
        )));
    }
    let new = NewQuotation {
        customer_id,
        quote_date,
        valid_until,
        remark: req.remark.as_deref().map(str::trim).filter(|text| !text.is_empty()),
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "开启建单事务"))?;
    let created = model::create_quotation(
        &tx,
        &new,
        &lines,
        &totals,
        state_machine::Operator::of(operator),
        Utc::now(),
    )
    .await;
    let (id, quote_no) = match created {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "新建报价单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "提交建单事务"))?;
    let mut res = detail_of(id).await?;
    if res.customer_code.is_none() {
        res.customer_code = Some(customer.code.clone());
        res.customer_name = Some(customer.name.clone());
    }
    debug_assert_eq!(res.quote_no, quote_no, "回读单号必须与取号结果一致");
    Ok(res)
}

/// 编辑报价单（**只有 DRAFT**；明细传了就整单替换）。
pub async fn update_quotation(
    operator: Option<Uuid>,
    id_raw: &str,
    req: UpdateQuotationReq,
) -> std::result::Result<QuotationRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let current = model::find_quotation(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询待编辑报价单"))?
        .ok_or_else(not_found)?;
    // 入账态只读闸：发出即冻结（改价要走新报价单，M3-01 建表口径）
    if current.status != model::STATUS_DRAFT {
        return Err(conflict(format!(
            "只有草稿（DRAFT）能改：当前状态是 {}（{}）。已发出的报价要改价 / 改条款，\
             请作废本单后新开一张（客户手里那份报价不能被原地改掉）",
            model::status_label(&current.status),
            current.status
        )));
    }
    // 审批闸（M3-04）：审批人还在等这一票时不许改（他看到的价必须就是最终价）；
    // 已批未发时也不许改（改了价还拿旧批文发出 = 跳过复核）。
    // 注意用 `held_by_approver` 而不是 `biz_instance_running`：退回发起人（`RESUBMIT` 待办）
    // 时单据必须能改，改完回去点「重新提交」。
    if quotation_approval_bridge::held_by_approver(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = quotation_approval_bridge::latest_approval_of(db(), id)
            .await
            .ok()
            .flatten()
            .map(|brief| brief.instance_no)
            .unwrap_or_else(|| "审批单".to_string());
        return Err(conflict(format!(
            "报价单 {} 正在审批中（{instance_no}），不能改：请先从审批中心撤回，或等审批结束后再改",
            current.quote_no
        )));
    }
    if quotation_approval_bridge::approved_instance_of(db(), id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单审批实例"))?
        .is_some()
    {
        return Err(conflict(format!(
            "报价单 {} 已通过审批但还没发出：请先「发出」（PATCH status=SENT，审批已通过会放行），\
             或作废后重开一张再改 —— 改完价再发等于跳过复核",
            current.quote_no
        )));
    }
    let customer_id = match req
        .customer_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => current.customer_id,
        Some(raw) => parse_uuid(raw, "customer_id")?,
    };
    let quote_date = parse_date(req.quote_date.as_deref(), "quote_date")?.unwrap_or(current.quote_date);
    let valid_until = parse_date(req.valid_until.as_deref(), "valid_until")?.unwrap_or(current.valid_until);
    if valid_until < quote_date {
        return Err(bad_request(format!(
            "valid_until（{valid_until}）不能早于 quote_date（{quote_date}）"
        )));
    }
    // 备注：不传 = 沿用；传空串 = 清空
    let remark: Option<String> = match req.remark.as_deref() {
        Some(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        None => current.remark.clone(),
    };
    // 明细：不传 = 沿用库里现有明细（金额沿用已算好的值）；传了 = 整单替换
    let lines: Vec<NewLine> = match req.items.as_ref() {
        None => model::lines_of(id)
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单明细"))?
            .iter()
            .map(|line| NewLine {
                material_id: line.material_id,
                qty: line.qty,
                price: line.price,
                tax_rate: line.tax_rate,
                amount: line.amount,
                tax_amount: line.tax_amount,
                remark: line.remark.clone(),
            })
            .collect(),
        Some(items) => {
            if items.is_empty() {
                return Err(bad_request("改单不能把明细清空：报价单至少要 1 行明细"));
            }
            build_lines(items, false)?
        }
    };
    if customer_id != current.customer_id {
        // 换客户也要过「存在且未冻结」这一关（冻结客户禁开新报价）
        require_active_customer(customer_id).await?;
    }
    let new = NewQuotation {
        customer_id,
        quote_date,
        valid_until,
        remark: remark.as_deref(),
    };
    let totals = Totals::of(&lines);
    let tx = db()
        .begin()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "开启改单事务"))?;
    let affected = model::update_quotation(
        &tx,
        model::UpdateQuotationInput {
            id,
            new: &new,
            lines: &lines,
            totals: &totals,
            operator,
            now: Utc::now(),
        },
    )
    .await;
    let affected = match affected {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "编辑报价单"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(conflict(
            "报价单状态在本次改单期间已被改动（不再处于草稿），请刷新后重试",
        ));
    }
    tx.commit()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "提交改单事务"))?;
    detail_of(id).await
}

/// 报价单详情（软删行 404）。
pub async fn get_quotation(id_raw: &str) -> std::result::Result<QuotationRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    detail_of(id).await
}

/// 报价单分页。
pub async fn page_quotations(filter: QuotationFilter) -> std::result::Result<PageData<Vec<QuotationRes>>, ApiError> {
    // `status` 传了非法值 → 400（静默忽略会把「已接受」筛成「全部」，是假数据）
    if let Some(status) = filter.status.as_deref().map(str::trim).filter(|text| !text.is_empty())
        && !model::is_valid_status(status)
    {
        return Err(bad_request(format!(
            "status 必须是 {} 之一（当前 `{status}`）",
            model::STATUSES.join(" / ")
        )));
    }
    let customer_id = match filter
        .customer_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(raw) => Some(parse_uuid(raw, "customer_id")?),
    };
    let today = model::today_cst(Utc::now());
    let (rows, total) = model::quotation_page(&model::QuotationPageQuery {
        quote_no: filter.quote_no.as_deref(),
        status: filter.status.as_deref(),
        customer_id,
        expired: filter.expired,
        today,
        page: filter.page,
        page_size: filter.page_size,
    })
    .await
    .map_err(|error| cloudpivot_kernel::utils::db_error(error, "分页查询报价单"))?;
    // 客户编码 / 名称一次往返补齐（一页里多张单常常同客户；软删客户 → 两列 None）
    let customer_ids: Vec<Uuid> = rows.iter().map(|row| row.customer_id).collect();
    let naming: std::collections::HashMap<Uuid, (String, String)> = model::customer_labels(&customer_ids)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "批量查询报价单客户"))?
        .into_iter()
        .map(|(id, code, name)| (id, (code, name)))
        .collect();
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    let list = rows
        .iter()
        .map(|row| {
            let mut res = QuotationRes::of(row, Vec::new(), today);
            if let Some((code, name)) = naming.get(&row.customer_id) {
                res.customer_code = Some(code.clone());
                res.customer_name = Some(name.clone());
            }
            res
        })
        .collect();
    Ok(PageData {
        list,
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 发出闸的模式（M3-04）。
///
/// `DRAFT → SENT` 是「把价格发给客户」这个对外承诺，所以这条边上有一个前置条件。
/// 两种放行方式**都是显式的**：走完审批（默认），或「没配流程 + 持跳过权限 + 写明理由」
/// （`approval_gate::authorize_missing_flow` 产出）。没有第三种。
#[derive(Debug, Clone, PartialEq, Eq)]
enum IssueGate {
    /// HTTP 入口：必须有 `APPROVED` 的审批实例，否则 409 + 66088。
    RequireApproval,
    /// 显式跳过审批：跳过理由由 `approval_gate` 产出（写进状态日志）。
    Skipped(String),
}

/// 状态流转（HTTP 入口）：`DRAFT → SENT` 走**审批闸**（M3-04）。
pub async fn change_quotation_status(
    operator: Option<Uuid>,
    id_raw: &str,
    req: QuotationStatusReq,
) -> std::result::Result<QuotationRes, ApiError> {
    change_status_with(operator, id_raw, req, IssueGate::RequireApproval).await
}

/// 状态流转：边表校验 → 业务守卫 → 状态机留痕 → CAS 落库（同一事务）。
///
/// `issue_gate` 只影响 `DRAFT → SENT` 一条边（其余边不用审批，见模块注释）。
async fn change_status_with(
    operator: Option<Uuid>,
    id_raw: &str,
    req: QuotationStatusReq,
    issue_gate: IssueGate,
) -> std::result::Result<QuotationRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let target = req.target_status.trim().to_string();
    if !model::is_valid_status(&target) {
        return Err(bad_request(format!(
            "target_status 必须是 {} 之一（当前 `{}`）",
            model::STATUSES.join(" / "),
            req.target_status
        )));
    }
    let row = model::find_quotation(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单"))?
        .ok_or_else(not_found)?;
    let today = model::today_cst(Utc::now());

    // ① 合法边校验（非法迁移 409，文案给出当前态与可选去向）
    if !model::can_transition(&row.status, &target) {
        let targets = model::allowed_targets(&row.status);
        let message = if targets.is_empty() {
            format!(
                "报价单 {} 已是终态 {}（{}），不能再流转",
                row.quote_no,
                model::status_label(&row.status),
                row.status
            )
        } else {
            format!(
                "非法状态迁移：{}（{}）→ {}（{}）不在合法边表里；当前可流转到：{}",
                model::status_label(&row.status),
                row.status,
                model::status_label(&target),
                target,
                targets
                    .iter()
                    .map(|code| format!("{code}（{}）", model::status_label(code)))
                    .collect::<Vec<_>>()
                    .join(" / ")
            )
        };
        return Err(conflict(message));
    }

    let reason = req.reason.as_deref().map(str::trim).filter(|text| !text.is_empty());

    // ①' 审批人还在等这一票时不许换状态（M3-04）：状态先被推走 = 两个真相。
    // 例外是作废（→CANCELLED）：那条边要「连审批一起撤」，在事务里处理（见 ④）。
    // 判据是「有没有 `PENDING` 的 APPROVAL 待办」，**不是**「实例在不在跑」——
    // 驳回选「退发起人」后实例仍是 PENDING，但票在发起人手里（`RESUBMIT`），
    // 那时单据必须可改可推进，否则「退回发起人」等于把单据锁死。
    if target != model::STATUS_CANCELLED
        && quotation_approval_bridge::held_by_approver(id)
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = quotation_approval_bridge::latest_approval_of(db(), id)
            .await
            .ok()
            .flatten()
            .map(|brief| brief.instance_no)
            .unwrap_or_else(|| "审批单".to_string());
        return Err(conflict(format!(
            "报价单 {} 已在审批中（{instance_no}）：请等审批结果，或从审批中心撤回 / 作废本单后再改状态",
            row.quote_no
        )));
    }

    // ② 需要原因的边（与 state_machine 的 QUOTATION_REASON_EDGES 同源，提前给人话）
    if state_machine::Entity::Quotation.needs_reason(&row.status, &target) && reason.is_none() {
        return Err(bad_request(format!(
            "{}（{}）→ {}（{}）必须给出原因（reason）：这条边对外推翻过承诺，\
             事后要能回答「为什么」",
            model::status_label(&row.status),
            row.status,
            model::status_label(&target),
            target
        )));
    }

    // ③ 业务守卫（按边逐个过）
    // 发出前的三道（明细 / 客户 / 有效期）对「人工发出」与「审批通过后自动发出」是**同一套**。
    let mut reason_owned: Option<String> = reason.map(str::to_string);
    if row.status == model::STATUS_DRAFT && target == model::STATUS_SENT {
        let lines = model::lines_of(id)
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单明细"))?;
        ensure_sendable(&row, &lines, today).await?;
        // ③' 审批闸（M3-04）：没批过就发不出去。
        match &issue_gate {
            IssueGate::Skipped(skip_reason) => {
                // 显式跳过（没配流程 + 持 skip-approval + 理由 ≥10 字）：理由写进状态日志
                reason_owned = Some(skip_reason.clone());
            }
            IssueGate::RequireApproval => {
                let approved = quotation_approval_bridge::approved_instance_of(db(), id)
                    .await
                    .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单审批实例"))?;
                let Some(approved) = approved else {
                    return Err(ApiError::new(
                        409,
                        error_code::ERP_QUOTATION_APPROVAL_REQUIRED,
                        format!(
                            "报价单 {} 还没通过审批，不能发出：请先调 \
                             POST /business/erp/quotation/{id}/submit-approval 发起报价审批\
                             （流程 {}）；审批通过后系统会自动发出，不需要再点一次",
                            row.quote_no,
                            quotation_approval_bridge::FLOW_CODE
                        ),
                    ));
                };
                if reason_owned.is_none() {
                    reason_owned = Some(format!("审批通过（{}）发出", approved.instance_no));
                }
            }
        }
    }
    if row.status == model::STATUS_SENT && target == model::STATUS_ACCEPTED {
        ensure_acceptable(&row, today)?;
    }
    if target == model::STATUS_CANCELLED {
        ensure_cancellable(&row)?;
    }

    // ④ 状态机留痕 + CAS 落库（同一事务）
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "开启状态流转事务"))?;
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_QUOTATION,
        id,
        &row.quote_no,
        &model::LEGAL_EDGES,
        &row.status,
        &target,
        state_machine::Operator::of(operator),
        reason_owned.as_deref(),
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    // ④' 作废：把在跑的审批**一起撤掉**（同一事务），否则留一堆指向死单的待办
    // （审批人点进去发现单子已作废 —— 那是最典型的孤儿待办）。
    if target == model::STATUS_CANCELLED {
        let cancelled = quotation_approval_bridge::cancel_approval_of(
            &tx,
            id,
            &row.quote_no,
            reason_owned.as_deref().unwrap_or("报价单作废，撤回审批"),
        )
        .await
        .map_err(|error| write_error(error, "撤回报价单审批"))?;
        if cancelled.is_some() {
            model::apply_approval_instance(&tx, id, None, operator, now)
                .await
                .map_err(|error| write_error(error, "清空报价单审批指针"))?;
        }
    }
    let applied = model::apply_status(&tx, id, &row.status, &target, operator, now).await;
    match applied {
        Ok(0) => {
            let _ = tx.rollback().await;
            Err(conflict(format!(
                "报价单 {} 的状态在本次流转期间已被改动（期望 {}），请刷新后重试",
                row.quote_no, row.status
            )))
        }
        Ok(_) => {
            tx.commit()
                .await
                .map_err(|error| cloudpivot_kernel::utils::db_error(error, "提交状态流转事务"))?;
            detail_of(id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "报价单状态流转"))
        }
    }
}

/// 软删报价单（**只有 DRAFT / CANCELLED**）。
pub async fn delete_quotation(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let row = model::find_quotation(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询待删除报价单"))?
        .ok_or_else(not_found)?;
    // 审批人还在等这一票时不许删（M3-04）：删了单就是孤儿待办。
    // （退回发起人时允许删：那时票在发起人手里，他有权作废 / 重来。）
    if quotation_approval_bridge::held_by_approver(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = quotation_approval_bridge::latest_approval_of(db(), id)
            .await
            .ok()
            .flatten()
            .map(|brief| brief.instance_no)
            .unwrap_or_else(|| "审批单".to_string());
        return Err(conflict(format!(
            "报价单 {} 正在审批中（{instance_no}），不能删除：请先从审批中心撤回，或等审批结束",
            row.quote_no
        )));
    }
    if !model::DELETABLE_STATUSES.contains(&row.status.as_str()) {
        return Err(conflict(format!(
            "报价单 {} 当前状态是 {}（{}），不能删除：只有 {} 能删。\
             已发出的报价请先作废（CANCELLED，需给原因），历史才留得住",
            row.quote_no,
            model::status_label(&row.status),
            row.status,
            model::DELETABLE_STATUSES
                .iter()
                .map(|code| format!("{code}（{}）", model::status_label(code)))
                .collect::<Vec<_>>()
                .join(" / ")
        )));
    }
    let affected = model::soft_delete(db(), id, &row.status, operator, Utc::now())
        .await
        .map_err(|error| write_error(error, "删除报价单"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "报价单 {} 的状态在本次删除期间已被改动（期望 {}），请刷新后重试",
            row.quote_no, row.status
        )));
    }
    Ok(())
}

// ==================== 转销售订单（M3-03）====================

/// 转单出参的单头段（`data.header`，与 ERP 单据出参三段式一致，见 `doc_shape`）。
#[derive(Debug, Clone, Serialize)]
pub struct ConvertedOrderHeader {
    /// 新销售订单 id。
    pub so_id: String,
    /// 新销售订单号（`SO-yyyyMMdd-000001`）。
    pub so_no: String,
    /// 订单状态（新建 = `DRAFT`）。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 客户 id（沿用报价单客户）。
    pub customer_id: String,
    /// 客户编码（软删客户 → `None`）。
    pub customer_code: Option<String>,
    /// 客户名称（软删客户 → `None`）。
    pub customer_name: Option<String>,
    /// 下单日期（`yyyy-MM-dd`）。
    pub order_date: String,
    /// 要求交期（未指定 → `None`）。
    pub delivery_date: Option<String>,
    /// 销售员 id（未指定 → `None`）。
    pub salesperson_id: Option<String>,
    /// 销售员姓名。
    pub salesperson_name: Option<String>,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 原币币种（转单默认账套本位币，见模块文档）。
    pub currency_code: String,
    /// 建单锁定的汇率（本位币单恒 1）。
    pub exchange_rate: Decimal,
    /// 本位币不含税金额。
    pub base_total_amount: Decimal,
    /// 本位币税额。
    pub base_total_tax: Decimal,
    /// 本位币价税合计。
    pub base_total_with_tax: Decimal,
    /// 订单备注（默认带「由报价单 QT-… 转换」，来源在单据上永远读得到）。
    pub remark: Option<String>,
}

/// 转单出参的明细段（`data.items`）。
#[derive(Debug, Clone, Serialize)]
pub struct ConvertedOrderLine {
    /// 行号（1 起，与报价单行号一一对应）。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（软删物料 → `None`）。
    pub material_code: Option<String>,
    /// 物料名称（软删物料 → `None`）。
    pub material_name: Option<String>,
    /// 规格。
    pub spec: Option<String>,
    /// 计量单位名。
    pub unit_name: Option<String>,
    /// 数量（原样搬自报价行）。
    pub qty: Decimal,
    /// 不含税单价（原样搬自报价行，**不重新报价**）。
    pub price: Decimal,
    /// 行不含税金额。
    pub amount: Decimal,
    /// 税率百分数。
    pub tax_rate: Decimal,
    /// 行税额。
    pub tax_amount: Decimal,
}

/// 转单出参（`data.header` / `data.items` / `data.meta` 三段式）。
#[derive(Debug, Clone, Serialize)]
pub struct QuotationConvertRes {
    /// 新销售订单单头。
    pub header: ConvertedOrderHeader,
    /// 新销售订单明细（行数与报价单一致）。
    pub items: Vec<ConvertedOrderLine>,
    /// 附属信息：报价单回链 / 是否幂等命中 / 行数。
    pub meta: doc_shape::DocMeta,
}

/// 出参需要的主数据名字（一次性查齐，避免 N+1，也避免构造出参时再失败）。
struct OrderLabels {
    customer_code: Option<String>,
    customer_name: Option<String>,
    salesperson_name: Option<String>,
    materials: std::collections::HashMap<Uuid, transit_model::MaterialBrief>,
}

/// 落库后订单的统一视图（新建路径与幂等路径都先收敛到这里，出参只拼一遍）。
struct OrderView {
    so_id: Uuid,
    so_no: String,
    status: String,
    order_date: NaiveDate,
    delivery_date: Option<NaiveDate>,
    salesperson_id: Option<Uuid>,
    total_amount: Decimal,
    total_tax: Decimal,
    total_with_tax: Decimal,
    currency_code: String,
    exchange_rate: Decimal,
    base_total_amount: Decimal,
    base_total_tax: Decimal,
    base_total_with_tax: Decimal,
    remark: Option<String>,
    items: Vec<LineView>,
}

/// 订单明细行的统一视图。
struct LineView {
    line_no: i32,
    material_id: Uuid,
    qty: Decimal,
    price: Decimal,
    amount: Decimal,
    tax_rate: Decimal,
    tax_amount: Decimal,
}

impl OrderView {
    /// 由库里的订单行 + 明细行构造（幂等路径用）。
    fn from_rows(row: &so_model::SalesOrderRow, items: &[so_model::SalesOrderItemRow]) -> Self {
        Self {
            so_id: row.id,
            so_no: row.so_no.clone(),
            status: row.status.clone(),
            order_date: row.order_date,
            delivery_date: row.delivery_date,
            salesperson_id: row.salesperson_id,
            total_amount: row.total_amount,
            total_tax: row.total_tax,
            total_with_tax: row.total_with_tax,
            currency_code: row.currency_code.clone(),
            exchange_rate: row.exchange_rate,
            base_total_amount: row.base_total_amount,
            base_total_tax: row.base_total_tax,
            base_total_with_tax: row.base_total_with_tax,
            remark: row.remark.clone(),
            items: items
                .iter()
                .map(|item| LineView {
                    line_no: item.line_no,
                    material_id: item.material_id,
                    qty: item.qty,
                    price: item.price,
                    amount: item.amount,
                    tax_rate: item.tax_rate,
                    tax_amount: item.tax_amount,
                })
                .collect(),
        }
    }
}

// ---------- 纯逻辑（单测直接调，不连库）----------

/// 能不能转单：只有「客户已接受」（`ACCEPTED`）的报价单可以。
///
/// 为什么不是 `SENT`：销售订单要占信用、要出库、要生成应收与凭证 —— 那是**已成交**的意思。
/// 报价发出去只代表「我方报了价」，客户还没表态。`SENT` 的单请先走状态接口标成
/// 「客户已接受」（M3-02 的 `PATCH /status`，它自己带「未过期」守卫），再来转单。
fn ensure_convertible(row: &QuotationRow) -> std::result::Result<(), ApiError> {
    if row.status == model::STATUS_ACCEPTED {
        return Ok(());
    }
    let hint = if row.status == model::STATUS_SENT {
        "请先在状态接口把报价单标成「客户已接受」（ACCEPTED），再转销售订单"
    } else {
        "只有「客户已接受」（ACCEPTED）的报价单能转销售订单"
    };
    Err(conflict(format!(
        "报价单 {} 当前是「{}」（{}），不能转销售订单：{hint}",
        row.quote_no,
        model::status_label(&row.status),
        row.status
    )))
}

/// 报价行 → 销售订单行（**金额整列搬，不算第二次**），并做四道数据自检。
///
/// 单测直接调这个函数（不连库）：它决定「转出来的订单和报价单是不是同一笔钱」。
///
/// | 自检 | 为什么 |
/// |------|--------|
/// | 至少 1 行 | 空单转出来的订单提交上去，审批人批的是空单 |
/// | `qty > 0` / `price >= 0` / `tax_rate ∈ [0,100]` | 与两张表的 CHECK 逐条对应，先给人话 |
/// | 每行 `amount` / `tax_amount` 与「单价 × 数量」重算一致 | 库里被绕过写入口改脏时，直接搬过去等于把脏数据洗成订单 |
/// | 明细汇总 == 单头三项合计 | 订单金额与报价单金额必须是同一分钱 |
fn plan_order_items(
    row: &QuotationRow,
    lines: &[QuotationLineRow],
) -> std::result::Result<(Vec<so_model::NewItem>, so_model::Totals), ApiError> {
    if lines.is_empty() {
        return Err(conflict(format!(
            "报价单 {} 没有明细，不能转销售订单（空的销售订单提交上去，审批人批的是一张空单）",
            row.quote_no
        )));
    }
    let mut items = Vec::with_capacity(lines.len());
    for line in lines {
        if line.qty <= Decimal::ZERO {
            return Err(conflict(format!(
                "报价单 {} 第 {} 行数量是 {}（必须 > 0），不能转销售订单：请先核对这张报价单的明细",
                row.quote_no, line.line_no, line.qty
            )));
        }
        if line.price < Decimal::ZERO {
            return Err(conflict(format!(
                "报价单 {} 第 {} 行单价是 {}（不能为负），不能转销售订单",
                row.quote_no, line.line_no, line.price
            )));
        }
        if line.tax_rate < Decimal::ZERO || line.tax_rate > Decimal::from(100) {
            return Err(conflict(format!(
                "报价单 {} 第 {} 行税率是 {}（必须在 0–100 之间），不能转销售订单",
                row.quote_no, line.line_no, line.tax_rate
            )));
        }
        let (amount, tax_amount) = model::line_amounts(line.qty, line.price, line.tax_rate);
        if amount != line.amount || tax_amount != line.tax_amount {
            return Err(conflict(format!(
                "报价单 {} 第 {} 行的金额与「单价 × 数量」对不上（库里 amount = {}，重算 = {}；\
                 tax_amount = {}，重算 = {}）：先把这张报价单的明细修对再转单\
                 （否则等于把脏金额洗成一张要出库、要开票的销售订单）",
                row.quote_no, line.line_no, line.amount, amount, line.tax_amount, tax_amount
            )));
        }
        items.push(so_model::NewItem {
            material_id: line.material_id,
            qty: line.qty,
            price: line.price,
            tax_rate: line.tax_rate,
            amount: line.amount,
            tax_amount: line.tax_amount,
        });
    }
    let totals = so_model::Totals::of(&items);
    if totals.amount != row.total_amount || totals.tax != row.total_tax || totals.with_tax != row.total_with_tax {
        return Err(conflict(format!(
            "报价单 {} 的明细汇总（不含税 {} / 税额 {} / 价税 {}）与单头合计（{} / {} / {}）不一致：\
             先把这张报价单的合计修对再转单",
            row.quote_no,
            totals.amount,
            totals.tax,
            totals.with_tax,
            row.total_amount,
            row.total_tax,
            row.total_with_tax
        )));
    }
    Ok((items, totals))
}

/// 「部分转单」的入参键名 —— 出现任一即 **400**，绝不静默当成整单转单。
///
/// # 为什么本接口只能整单转（而不是漏做了部分转单）
///
/// V0432 的回链是**单值**的（`biz_quotation.converted_so_id` + `converted_at`），
/// 并且 `uk_biz_quotation_converted_so` 钉死「一张销售订单只被一张报价单认领」——
/// 一张报价单只能换出**一张**订单，换完就「用尽」（再点走幂等回原单）。
/// 客户只接受部分明细时，正确做法是**新开一张报价单只保留客户接受的行**再转，
/// 或先整单转换、再在销售订单里删掉没成交的行（订单此时还是 `DRAFT`，改单有审计）。
///
/// 反过来，若把这些键静默忽略：调用方以为只转了 2 行，库里却多出一张含全部明细的订单
/// —— 那张单要占信用、要出库、要开票，而且**没有任何界面能看出它比客户认的多转了几行**。
/// 如果哪天要做真正的「一张报价单分批转多张订单」，那是改表结构（行级已转数量 +
/// 转换多对多），不是给这个接口加个参数 —— 到那时这里的键名会被正式接上。
const PARTIAL_CONVERT_KEYS: [&str; 6] = ["line_nos", "line_no", "lines", "items", "line_ids", "convert_lines"];

/// 转单请求体解析（纯逻辑，单测直接调）。
///
/// 空 body（含纯空白）= 全默认参数（前端「一键转单」不必先拼 `{}`）；
/// 有 body 必须是**合法 JSON 对象**，且只认四个键
/// （`order_date` / `delivery_date` / `salesperson_id` / `remark`）：
///
/// | 请求体 | 结果 | 为什么 |
/// |--------|------|--------|
/// | 非法 JSON / 非 UTF-8 / 不是对象（`[1,2,3]`） | **400** | 静默当没传会让用户以为交期设上了 |
/// | 部分转单的键（`line_nos` / `lines` / `items` / …） | **400**（专门文案） | 见 [`PARTIAL_CONVERT_KEYS`]：否则会静默转出整张订单 |
/// | 其它未知键（拼错、旧版本前端多传） | **400**（`deny_unknown_fields`） | 参数被丢掉 = 用户看到的行为和参数不符 |
fn parse_convert_body(body: &[u8]) -> std::result::Result<ConvertQuotationReq, ApiError> {
    let text = std::str::from_utf8(body)
        .map_err(|_| bad_request("转单请求体必须是 UTF-8 编码的 JSON"))?
        .trim();
    if text.is_empty() {
        return Ok(ConvertQuotationReq::default());
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| bad_request(format!("转单请求体不是合法的 JSON 参数：{error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| bad_request("转单请求体必须是 JSON 对象（例：{\"order_date\":\"2026-11-17\"}）"))?;
    if let Some(key) = PARTIAL_CONVERT_KEYS.iter().find(|key| object.contains_key(**key)) {
        return Err(bad_request(format!(
            "本接口只做整单转单，不支持部分转单（请求体里的 `{key}` 已拒绝）：\
             客户只接受部分明细时，请新建一张报价单只保留客户接受的行再转，\
             或先整单转换、再到订单里删掉未成交的行 —— \
             静默忽略这个参数会转出一张含全部明细的订单（要占信用、要出库、要开票）"
        )));
    }
    serde_json::from_value(value).map_err(|error| bad_request(format!("转单请求体参数不认识：{error}")))
}

/// 订单备注：**永远带来源**（`由报价单 QT-… 转换`）。
/// 来源有两层：结构化的是 `biz_quotation.converted_so_id`（M3-03 回填），
/// 人可读的是这句备注 —— 订单单独导出去 / 打印出来后，只有备注能回答「这单是哪张报价换的」。
fn compose_order_remark(quote_no: &str, requested: Option<&str>) -> String {
    match requested.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => format!("{text}｜由报价单 {quote_no} 转换"),
        None => format!("由报价单 {quote_no} 转换"),
    }
}

/// 主数据名字一次查齐（客户 / 销售员 / 物料）。
async fn labels_for(
    customer_id: Uuid,
    salesperson_id: Option<Uuid>,
    material_ids: &[Uuid],
) -> std::result::Result<OrderLabels, ApiError> {
    let customer = customer_model::customer_find_by_id(customer_id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询转单客户"))?;
    let salesperson_name = match salesperson_id {
        None => None,
        Some(id) => so_model::salesperson_labels(&[id])
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询转单销售员"))?
            .into_iter()
            .next()
            .map(|(_, name)| name),
    };
    let materials = transit_model::material_labels(material_ids)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "批量查询转单物料"))?
        .into_iter()
        .map(|brief| (brief.id, brief))
        .collect();
    Ok(OrderLabels {
        customer_code: customer.as_ref().map(|row| row.code.clone()),
        customer_name: customer.as_ref().map(|row| row.name.clone()),
        salesperson_name,
        materials,
    })
}

/// 出参构造（**纯函数**：名字都已查齐，构造过程不可能失败）。
fn render_conversion(
    row: &QuotationRow,
    view: OrderView,
    labels: &OrderLabels,
    line_count: usize,
    already_converted: bool,
    today: NaiveDate,
) -> QuotationConvertRes {
    let items: Vec<ConvertedOrderLine> = view
        .items
        .iter()
        .map(|item| {
            let (code, name, spec, unit) = match labels.materials.get(&item.material_id) {
                Some(brief) => (
                    Some(brief.code.clone()),
                    Some(brief.name.clone()),
                    brief.spec.clone(),
                    brief.unit_name.clone(),
                ),
                None => (None, None, None, None),
            };
            ConvertedOrderLine {
                line_no: item.line_no,
                material_id: item.material_id.to_string(),
                material_code: code,
                material_name: name,
                spec,
                unit_name: unit,
                qty: item.qty,
                price: item.price,
                amount: item.amount,
                tax_rate: item.tax_rate,
                tax_amount: item.tax_amount,
            }
        })
        .collect();
    let meta = doc_shape::DocMeta::empty()
        .with_extra("quotation_id", serde_json::json!(row.id.to_string()))
        .with_extra("quote_no", serde_json::json!(row.quote_no))
        .with_extra("quotation_status", serde_json::json!(row.status))
        .with_extra("quotation_expired", serde_json::json!(row.expired_on(today)))
        .with_extra("line_count", serde_json::json!(line_count))
        .with_extra("already_converted", serde_json::json!(already_converted));
    QuotationConvertRes {
        header: ConvertedOrderHeader {
            so_id: view.so_id.to_string(),
            so_no: view.so_no,
            status_label: so_model::status_label(&view.status).to_string(),
            status: view.status,
            customer_id: row.customer_id.to_string(),
            customer_code: labels.customer_code.clone(),
            customer_name: labels.customer_name.clone(),
            order_date: view.order_date.to_string(),
            delivery_date: view.delivery_date.map(|date| date.to_string()),
            salesperson_id: view.salesperson_id.map(|id| id.to_string()),
            salesperson_name: labels.salesperson_name.clone(),
            total_amount: view.total_amount,
            total_tax: view.total_tax,
            total_with_tax: view.total_with_tax,
            currency_code: view.currency_code,
            exchange_rate: view.exchange_rate,
            base_total_amount: view.base_total_amount,
            base_total_tax: view.base_total_tax,
            base_total_with_tax: view.base_total_with_tax,
            remark: view.remark,
        },
        items,
        meta,
    }
}

/// 幂等路径：报价单已经转过单 → 回那张订单（**不再建第二张**）。
async fn existing_conversion(
    row: &QuotationRow,
    so_id: Uuid,
    today: NaiveDate,
) -> std::result::Result<QuotationConvertRes, ApiError> {
    let order = so_model::find_order(so_id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单已转出的销售订单"))?
        .ok_or_else(|| {
            conflict(format!(
                "报价单 {} 记录的销售订单 {so_id} 已不存在（被删除 / 数据被改）：\
                 不能重复转单，请人工核对后处理",
                row.quote_no
            ))
        })?;
    let items = so_model::items_of(so_id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询已转出订单的明细"))?;
    let material_ids: Vec<Uuid> = items.iter().map(|item| item.material_id).collect();
    let labels = labels_for(row.customer_id, order.salesperson_id, &material_ids).await?;
    let line_count = items.len();
    let view = OrderView::from_rows(&order, &items);
    Ok(render_conversion(row, view, &labels, line_count, true, today))
}

/// 客户必须存在、未软删且 `ACTIVE`（**冻结客户不许开新订单**，与销售订单建单同口径）。
///
/// 这里刻意不复用报价单侧的 `require_active_customer`（它给 400）：转单是「开一张要出库、
/// 要占信用的订单」，客户冻结属于**当前状态冲突**，给 409 才说得清。
async fn ensure_customer_convertible(customer_id: Uuid) -> std::result::Result<CustomerRow, ApiError> {
    let customer = customer_model::customer_find_by_id(customer_id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询转单客户"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("客户 {customer_id} 不存在或已删除（客户是弱引用，转单前必须校验）"),
            )
        })?;
    if customer.status != STATUS_ACTIVE {
        return Err(conflict(format!(
            "客户 `{}`（{}）已冻结（{}）：不能新开销售订单，请先解冻再转单",
            customer.code, customer.name, customer.status
        )));
    }
    Ok(customer)
}

/// 转销售订单（**带行**，一次事务：建订单 + 回填报价单 + 写轨迹）。
///
/// # 事务边界（与「采购申请 → 采购订单」的关键区别）
///
/// 采购申请转单用的是「先提交 PO 再回写申请单」两段事务，留下一段已知的中间态；
/// 这里**全部在一个事务里**（`sales_order_model::create_order` 收执行器，不自己开事务）：
///
/// ```text
/// BEGIN
///   SELECT … FROM biz_quotation WHERE id = ? FOR UPDATE     -- 锁单：并发第二个请求在这里排队
///   已转过 → ROLLBACK，返回那张订单（幂等，200 + meta.already_converted = true）
///   状态守卫 / 明细自检 / 客户未冻结 / 汇率锁定
///   INSERT biz_sales_order(+item)                           -- 新订单 DRAFT，取号 SO-…
///   UPDATE biz_quotation SET converted_so_id, converted_at  -- CAS：status 未变 且 尚未转单
///   INSERT biz_status_log（QUOTATION，ACCEPTED→ACCEPTED，action=CONVERT，biz_ref=SALES_ORDER/SO-…）
/// COMMIT
/// ```
///
/// 任何一步失败 → 整笔回滚：**不会出现「订单建了、报价单没回链」或「回链了、订单没建」**
/// （约定 11：不许留只能改库才能收拾的中间态）。
///
/// # 币种
/// 报价单没有币种列（V0432 口径：报价阶段不锁汇率），转单按**账套本位币**记
/// （`currency::lock(None, …)`），`exchange_rate = 1`。外币报价要转外币订单，
/// 是另一个决定（报价单先补币种列），不在本卡。
pub async fn convert_quotation(
    operator: Option<Uuid>,
    id_raw: &str,
    req: ConvertQuotationReq,
) -> std::result::Result<QuotationConvertRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let today = model::today_cst(Utc::now());
    let order_date = parse_date(req.order_date.as_deref(), "order_date")?.unwrap_or(today);
    let delivery_date = parse_date(req.delivery_date.as_deref(), "delivery_date")?;
    if let Some(delivery) = delivery_date
        && delivery < order_date
    {
        return Err(bad_request(format!(
            "delivery_date（{delivery}）不能早于 order_date（{order_date}）"
        )));
    }
    let salesperson_id = match req
        .salesperson_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(raw) => Some(parse_uuid(raw, "salesperson_id")?),
    };
    if let Some(salesperson) = salesperson_id
        && so_model::active_salesperson(salesperson)
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "校验转单销售员"))?
            .is_none()
    {
        return Err(bad_request(format!(
            "销售员 {salesperson} 不存在、已删除或已停用：只能选启用状态的账号（停用账号不该再接新单计佣）"
        )));
    }

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "开启转单事务"))?;

    // ① 锁单（并发第二个请求在这里排队，拿到的是「已转过」的最新行）
    let row = match model::lock_quotation(&tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(cloudpivot_kernel::utils::db_error(error, "锁定报价单"));
        }
    };

    // ② 幂等：已经转过 → 回那张订单，绝不建第二张
    if let Some(so_id) = row.converted_so_id {
        let _ = tx.rollback().await;
        return existing_conversion(&row, so_id, today).await;
    }

    // ③ 状态守卫 + 明细自检（纯逻辑，读的是锁内快照）
    if let Err(error) = ensure_convertible(&row) {
        let _ = tx.rollback().await;
        return Err(error);
    }
    let lines = match model::lines_of_in(&tx, id).await {
        Ok(lines) => lines,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(cloudpivot_kernel::utils::db_error(error, "查询待转单报价单明细"));
        }
    };
    let (items, totals) = match plan_order_items(&row, &lines) {
        Ok(plan) => plan,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };

    // ④ 客户 / 汇率（读，不写）
    let customer = match ensure_customer_convertible(row.customer_id).await {
        Ok(customer) => customer,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let locked = match currency::lock(db(), None, order_date)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "取转单汇率"))
        .and_then(|outcome| outcome.into_locked("销售订单（由报价单转换）"))
    {
        Ok(locked) => locked,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    debug_assert_eq!(locked.exchange_rate, Decimal::from(1), "本位币单汇率恒为 1");
    let remark = compose_order_remark(&row.quote_no, req.remark.as_deref());
    let material_ids: Vec<Uuid> = items.iter().map(|item| item.material_id).collect();
    // 名字在提交前查齐：提交之后再查失败虽然不影响数据（重试走幂等路径），但会白扔一次 500
    let labels = match labels_for(row.customer_id, salesperson_id, &material_ids).await {
        Ok(labels) => labels,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let base_amounts = currency::BaseAmounts::of(totals.amount, totals.tax, locked.exchange_rate);
    let new_order = so_model::NewSalesOrder {
        customer_id: row.customer_id,
        // 报价单不挂合同（V0432 无 contract_id）：合同在订单上另行关联，不在这里猜
        contract_id: None,
        salesperson_id,
        order_date,
        delivery_date,
        remark: Some(remark.as_str()),
    };

    // ⑤ 建订单（同一个事务）
    let (so_id, so_no) = match so_model::create_order(
        &tx,
        &new_order,
        &items,
        &totals,
        &locked,
        state_machine::Operator::of(operator),
        now,
    )
    .await
    {
        Ok(created) => created,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "转单建销售订单"));
        }
    };

    // ⑥ 回填报价单（CAS：状态没变 且 还没转过）—— 0 行就整笔回滚，绝不留孤订单
    let affected = match model::mark_converted(&tx, id, &row.status, so_id, operator, now).await {
        Ok(affected) => affected,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "转单回填报价单"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(conflict(format!(
            "报价单 {} 的状态在本次转单期间已被改动（期望 {} 且未转过），请刷新后重试",
            row.quote_no, row.status
        )));
    }

    // ⑦ 轨迹：状态没变（ACCEPTED→ACCEPTED）但这是一次业务事件，走 log_event 留痕
    let reason = format!("转销售订单 {so_no}");
    if let Err(error) = state_machine::log_event(
        &tx,
        state_machine::ENTITY_QUOTATION,
        id,
        &row.quote_no,
        &row.status,
        Some(CONVERT_ACTION),
        state_machine::Operator::of(operator),
        Some(&reason),
        Some(("SALES_ORDER", &so_no)),
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }

    tx.commit()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "提交转单事务"))?;
    tracing::info!(
        quotation_id = %id,
        quote_no = %row.quote_no,
        so_id = %so_id,
        so_no = %so_no,
        lines = items.len(),
        total_with_tax = %totals.with_tax,
        "报价单转销售订单完成"
    );

    let view = OrderView {
        so_id,
        so_no,
        status: so_model::STATUS_DRAFT.to_string(),
        order_date,
        delivery_date,
        salesperson_id,
        total_amount: totals.amount,
        total_tax: totals.tax,
        total_with_tax: totals.with_tax,
        currency_code: locked.currency_code.clone(),
        exchange_rate: locked.exchange_rate,
        base_total_amount: base_amounts.amount,
        base_total_tax: base_amounts.tax,
        base_total_with_tax: base_amounts.with_tax,
        remark: Some(remark),
        items: items
            .iter()
            .enumerate()
            .map(|(index, item)| LineView {
                line_no: i32::try_from(index + 1).unwrap_or(i32::MAX),
                material_id: item.material_id,
                qty: item.qty,
                price: item.price,
                amount: item.amount,
                tax_rate: item.tax_rate,
                tax_amount: item.tax_amount,
            })
            .collect(),
    };
    let line_count = items.len();
    // 客户行刚刚查过（冻结客户在上面就 409 了）；名字没查到时用这里的兜底
    let mut labels = labels;
    if labels.customer_code.is_none() {
        labels.customer_code = Some(customer.code.clone());
        labels.customer_name = Some(customer.name.clone());
    }
    Ok(render_conversion(&row, view, &labels, line_count, false, today))
}

// ==================== 审批（M3-04）：提交 / 回调自动发出 ====================

/// 审批通过后「自动发出」的结果（回调据此决定日志口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueOutcome {
    /// 本次真的发出去了（`DRAFT → SENT`）。
    Issued,
    /// 之前已经发出（重复回调）→ 幂等，不再动状态。
    AlreadyIssued,
    /// 当前状态不是草稿（被别的路径改过）→ 交人工处置，不乱推状态。
    NotDraftable {
        /// 当前状态。
        status: String,
    },
}

/// **审批通过后的发出执行体**（由 `quotation_approval_bridge::on_quotation_settled` 调）。
///
/// 与人工 `PATCH status=SENT` 走**同一条代码路径**（[`change_status_with`]：明细 / 客户 /
/// 有效期三道守卫 → 审批闸 → 状态机留痕 → CAS），不另写一套 —— 两套写法迟早会在守卫上走岔。
/// 此刻审批实例已是 `APPROVED`，所以审批闸对它是放行的。
///
/// # 失败口径（约定 11）
///
/// 这里**没有**「事务已提交后再失败」的窗口：发出一条路径一个事务，失败整笔回滚。
/// 万一失败（例如有效期已过），报价单留在 `DRAFT` 且仍有 `APPROVED` 实例 ——
/// 销售补好有效期后手工 `PATCH status=SENT` 即可补发（闸对它放行），
/// 不会出现「批了却永远发不出去」的死局。
pub async fn issue_after_approval(
    quotation_id: Uuid,
    operator: Option<Uuid>,
    reason: &str,
) -> std::result::Result<IssueOutcome, ApiError> {
    let Some(row) = model::find_quotation(quotation_id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单"))?
    else {
        return Err(not_found());
    };
    match row.status.as_str() {
        model::STATUS_SENT => return Ok(IssueOutcome::AlreadyIssued),
        model::STATUS_DRAFT => {}
        other => {
            return Ok(IssueOutcome::NotDraftable {
                status: other.to_string(),
            });
        }
    }
    let req = QuotationStatusReq {
        target_status: model::STATUS_SENT.to_string(),
        reason: Some(reason.to_string()),
    };
    change_status_with(operator, &quotation_id.to_string(), req, IssueGate::RequireApproval).await?;
    Ok(IssueOutcome::Issued)
}

/// 提交审批（`POST /business/erp/quotation/{id}/submit-approval` 的唯一入口）。
///
/// # 前置（缺一条就 409，报文点名缺什么）
///
/// 1. 状态必须是 `DRAFT`（已发出的报价审批没有意义）；
/// 2. 不能已有在跑的审批（`biz_instance_running` + 库侧唯一索引双保险）；
/// 3. 不能已有**已通过但还没发出**的审批（先发出 / 作废，别堆第二轮）；
/// 4. 发出前的三道守卫（明细 ≥ 1 / 客户未冻结 / 有效期未过）—— 审批通过后回调**直接发出**，
///    批了却发不出去的单才是真正的坑，所以提交前就把这三关过掉。
///
/// # 没配流程
///
/// `published_quotation_flow == None` → `approval_gate::authorize_missing_flow`：
/// 默认 409 + `66033`（fail-closed），只有持 `business:erp:skip-approval` 且理由 ≥ 10 字的
/// 账号才能显式跳过 —— 此时**直接发出**（理由写进状态日志），
/// 否则报价单会永远停在草稿且没人能发出去。
pub async fn submit_approval(
    operator: Option<Uuid>,
    id_raw: &str,
    gate: &approval_gate::ApprovalGate<'_>,
) -> std::result::Result<QuotationRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let row = model::find_quotation(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单"))?
        .ok_or_else(not_found)?;
    if row.status != model::STATUS_DRAFT {
        return Err(conflict(format!(
            "报价单 {} 当前是「{}」（{}）：只有草稿（DRAFT）可以提交审批",
            row.quote_no,
            model::status_label(&row.status),
            row.status
        )));
    }
    if flow_model::biz_instance_running(quotation_approval_bridge::BIZ_TYPE, id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = quotation_approval_bridge::latest_approval_of(db(), id)
            .await
            .ok()
            .flatten()
            .map(|brief| brief.instance_no)
            .unwrap_or_else(|| "审批单".to_string());
        // 退回发起人（`RESUBMIT` 待办）与被审批人占着是两件事：前者要「重新提交」那一条待办，
        // 不能走这里再建一个实例（库侧 `uk_biz_flow_instance_biz_active` 也会挡）。
        if let Some(task_id) = quotation_approval_bridge::pending_resubmit_of(id)
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询重新提交待办"))?
        {
            return Err(conflict(format!(
                "报价单 {} 被审批人退回待重新提交（{instance_no}）：请改完后在审批中心点「重新提交」\
                 （待办 {task_id}），不要再走提交审批 —— 那会另起一轮",
                row.quote_no
            )));
        }
        return Err(conflict(format!(
            "报价单 {} 已在审批中（{instance_no}）：请等审批结果，或从审批中心撤回后再提交",
            row.quote_no
        )));
    }
    if quotation_approval_bridge::approved_instance_of(db(), id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单审批实例"))?
        .is_some()
    {
        return Err(conflict(format!(
            "报价单 {} 已通过审批（还没发出）：不需要再审一轮，直接「发出」即可\
             （PATCH status=SENT），或作废后重开一张",
            row.quote_no
        )));
    }
    // 发出前的三道守卫（与人工发出同一套）
    let lines = model::lines_of(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单明细"))?;
    ensure_sendable(&row, &lines, model::today_cst(Utc::now())).await?;

    match quotation_approval_bridge::published_quotation_flow()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单审批流程"))?
    {
        Some(flow) => {
            // 有流程就必须走流程：请求里带了 skip_approval 也要 400（不许绕过已配置的内控）
            approval_gate::ensure_not_skipped(quotation_approval_bridge::FLOW_CODE, gate)?;
            start_approval(&row, &flow, operator).await
        }
        None => {
            let skip_reason = approval_gate::authorize_missing_flow(
                quotation_approval_bridge::BIZ_TYPE,
                quotation_approval_bridge::FLOW_CODE,
                gate,
            )?;
            tracing::warn!(
                quotation_id = %row.id,
                quote_no = %row.quote_no,
                %skip_reason,
                "报价单提交审批：没有已发布的 quotation_approve 流程，按「显式跳过审批」直接发出\
                 （理由已写 biz_status_log）"
            );
            let req = QuotationStatusReq {
                target_status: model::STATUS_SENT.to_string(),
                // 跳过理由整条写进状态日志（`authorize_missing_flow` 已经把用户填的理由并进去了）
                reason: None,
            };
            change_status_with(operator, id_raw, req, IssueGate::Skipped(skip_reason)).await
        }
    }
}

/// [`submit_approval`] 的「有流程」分支：发起审批实例（报价单状态**不变**，仍是 `DRAFT`）。
async fn start_approval(
    row: &QuotationRow,
    flow: &quotation_approval_bridge::QuotationFlow,
    operator: Option<Uuid>,
) -> std::result::Result<QuotationRes, ApiError> {
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 报价单审批 {} 元", row.quote_no, row.total_with_tax)
        .chars()
        .take(200)
        .collect::<String>();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(row.id),
        biz_no: Some(row.quote_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: quotation_approval_bridge::form_data(row),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "开启报价单审批事务"))?;
    let mut out = match engine::start(&tx, &start).await {
        Ok(out) => out,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(if flow_model::is_biz_active_duplicate(&error) {
                flow_model::approval_in_progress("报价单", Some(row.quote_no.as_str()))
            } else {
                write_error(error, "发起报价单审批单")
            });
        }
    };
    if let Some(rejected) = out.rejected.take() {
        let _ = tx.rollback().await;
        return Err(rejected);
    }
    if out.instance_id != Some(instance_id) {
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            "审批单发起后返回的实例 id 与预生成的不一致（引擎内部错误）",
        ));
    }
    let auto_finished = out.instance_status.eq_ignore_ascii_case("APPROVED");
    if !auto_finished && !out.instance_status.eq_ignore_ascii_case("PENDING") {
        let status = out.instance_status.clone();
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("报价审批单发起后状态是 {status}（应为 PENDING）：流程定义可能没有可执行的审批节点"),
        ));
    }
    // 把「最近一轮审批实例」写回单头（V0435）—— 与实例**同一事务**，
    // 不会出现「实例建了、指针没落」（列表页显示不出审批中，人又看不到待办）。
    model::apply_approval_instance(&tx, row.id, Some(instance_id), operator, now)
        .await
        .map_err(|error| write_error(error, "回写报价单审批实例"))?;
    tx.commit()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "提交报价单审批事务"))?;
    // 锁外副作用（审批通知 / 客户自配的通知节点）：提交之后才发，失败只 WARN
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    tracing::info!(
        quotation_id = %row.id,
        quote_no = %row.quote_no,
        %instance_id,
        flow_code = %start.flow_code,
        "报价单已提交审批（仍是草稿，等审批通过后自动发出）"
    );
    // 流程里没有任何可执行的审批节点（全被跳空）→ 实例在同一个事务里就走完了，
    // 必须立刻跑一次「审批通过」的回调，否则报价单停在 DRAFT 且没有待办能推进它。
    if auto_finished {
        quotation_approval_bridge::on_quotation_settled(Some(instance_id), "APPROVED").await;
    }
    detail_of(row.id).await
}

// ==================== HTTP 入口 ====================

/// 报价单分页。
#[nik::permission(
    path = "/business/erp/quotation/page",
    method = "GET",
    permission = "business:erp:quotation:list"
)]
#[nik::get("/business/erp/quotation/page")]
pub async fn quotation_page(Query(filter): Query<QuotationFilter>) -> ResResult<PageData<Vec<QuotationRes>>> {
    Ok(Res::ok(page_quotations(filter).await?))
}

/// 报价单详情。
#[nik::permission(
    path = "/business/erp/quotation/{id}",
    method = "GET",
    permission = "business:erp:quotation:list"
)]
#[nik::get("/business/erp/quotation/{id}")]
pub async fn quotation_detail(Path(id): Path<String>) -> ResResult<QuotationRes> {
    Ok(Res::ok(get_quotation(&id).await?))
}

/// 新建报价单。
#[nik::permission(
    path = "/business/erp/quotation",
    method = "POST",
    permission = "business:erp:quotation:create"
)]
#[nik::post("/business/erp/quotation")]
pub async fn quotation_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateQuotationReq>,
) -> ResResult<QuotationRes> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    Ok(Res::ok(create_quotation(operator, req).await?))
}

/// 编辑报价单（只有 DRAFT）。
#[nik::permission(
    path = "/business/erp/quotation/{id}",
    method = "PUT",
    permission = "business:erp:quotation:update"
)]
#[nik::put("/business/erp/quotation/{id}")]
pub async fn quotation_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateQuotationReq>,
) -> ResResult<QuotationRes> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    Ok(Res::ok(update_quotation(operator, &id, req).await?))
}

/// 状态流转（非法迁移 409）。
#[nik::permission(
    path = "/business/erp/quotation/{id}/status",
    method = "PATCH",
    permission = "business:erp:quotation:status"
)]
#[nik::patch("/business/erp/quotation/{id}/status")]
pub async fn quotation_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<QuotationStatusReq>,
) -> ResResult<QuotationRes> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    Ok(Res::ok(change_quotation_status(operator, &id, req).await?))
}

/// 删除报价单（软删，只有 DRAFT / CANCELLED）。
#[nik::permission(
    path = "/business/erp/quotation/{id}",
    method = "DELETE",
    permission = "business:erp:quotation:delete"
)]
#[nik::delete("/business/erp/quotation/{id}")]
pub async fn quotation_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    Ok(Res::ok(delete_quotation(operator, &id).await?))
}

/// 转销售订单（一键转单，带行；已转过 → 幂等回同一张订单）。
///
/// 请求体用 `Bytes` 而不是 `Json<…>`：一键转单的调用方常常**不带 body**
/// （`opt = None` + `Content-Type: application/json` 也会让 `Json` 提取器报
/// 「Failed to parse the request body as JSON: EOF」）。空 body 一律当默认参数，
/// 有 body 才按 JSON 解析，非法 JSON 给 400（不静默当没传）。
#[nik::permission(
    path = "/business/erp/quotation/{id}/convert",
    method = "POST",
    permission = "business:erp:quotation:convert"
)]
#[nik::post("/business/erp/quotation/{id}/convert")]
pub async fn quotation_convert(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Bytes,
) -> ResResult<QuotationConvertRes> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    Ok(Res::ok(
        convert_quotation(operator, &id, parse_convert_body(&body)?).await?,
    ))
}

/// 提交审批（M3-04）：发起 `quotation_approve` 审批实例，报价单**仍是草稿**。
///
/// 复用 `business:erp:quotation:status`（不另开码）：提交审批是「推进这张单」的一个动作，
/// 与收款退款（V0297）/ 销售订单（SO-06）同一口径 —— 另开码就要多一行菜单，而它并没有
/// 独立授权语义（能改状态的人就是要能送审）。
///
/// 请求体可空（一键提交不必拼 `{}`）；带 body 时可写 `skip_approval` / `skip_reason`
/// （只在「没配流程」时生效，需要 `business:erp:skip-approval` 权限码）。
#[nik::permission(
    path = "/business/erp/quotation/{id}/submit-approval",
    method = "POST",
    permission = "business:erp:quotation:status"
)]
#[nik::post("/business/erp/quotation/{id}/submit-approval")]
pub async fn quotation_submit_approval(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Bytes,
) -> ResResult<QuotationRes> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    // 空 body = 不请求跳过审批（与转单同口径：`Option<Json<…>>` 在空 body 上会 400）
    let text = String::from_utf8_lossy(&body);
    let req: QuotationSubmitReq = if text.trim().is_empty() {
        QuotationSubmitReq::default()
    } else {
        serde_json::from_str(&text)
            .map_err(|error| ApiError::new(400, error_code::PARAM_INVALID, format!("请求体不是合法 JSON：{error}")))?
    };
    let gate = approval_gate::ApprovalGate::new(&req.approval, &claims);
    Ok(Res::ok(submit_approval(operator, &id, &gate).await?))
}

/// 权限码自检：6 个码、无重复、前缀 `business:erp:quotation:`（V0433 / V0434 的菜单行逐字对应）。
pub fn all_permissions() -> Vec<&'static str> {
    QUOTATION_PERMISSIONS.to_vec()
}

// ==================== 单测（M3-05：状态机 + 转单） ====================
//
// 挂在 handler 里（而不是 `sales/mod.rs`）只为一件事：**够得着私有守卫**
// （`ensure_sendable` / `ensure_acceptable` / `ensure_cancellable` / `change_status_with` /
// `IssueGate`）。子模块可以看父模块的私有项，反过来不行。

#[cfg(test)]
#[path = "quotation_state_tests.rs"]
mod quotation_state_tests;

// ==================== 单测（M3-08：报价单 → 销售订单，含部分转单） ====================
//
// 同样挂在 handler 里：要够到 `parse_convert_body` / `plan_order_items` /
// `compose_order_remark` / `ensure_convertible` 这些**私有**转单件，以及 `CONVERT_ACTION`。
// 与 M3-05 那份的分工写在文件头（那边钉状态机与审批闸，这里只钉转单这一条链路）。

#[cfg(test)]
#[path = "quotation_convert_tests.rs"]
mod quotation_convert_tests;

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    fn line_req(qty: &str, price: &str, rate: &str) -> QuotationLineReq {
        QuotationLineReq {
            material_id: Uuid::now_v7().to_string(),
            qty: Some(dec(qty)),
            price: Some(dec(price)),
            tax_rate: Some(dec(rate)),
            remark: None,
        }
    }

    /// 权限码：6 个、无重复、统一前缀（与 V0433 / V0434 的 sys_menu 行逐字一致）。
    #[test]
    fn permission_codes_are_unique_and_prefixed() {
        assert_eq!(QUOTATION_PERMISSIONS.len(), 6);
        assert_eq!(all_permissions().len(), 6);
        let prefix = "business:erp:quotation:";
        let mut seen = std::collections::BTreeSet::new();
        for code in QUOTATION_PERMISSIONS {
            assert!(code.starts_with(prefix), "{code} 前缀必须统一");
            assert!(seen.insert(code), "{code} 重复");
        }
        for suffix in ["list", "create", "update", "delete", "status", "convert"] {
            assert!(
                QUOTATION_PERMISSIONS.contains(&format!("{prefix}{suffix}").as_str()),
                "缺权限码 {prefix}{suffix}"
            );
        }
    }

    /// 行入参校验：数量 > 0 / 单价非负 / 税率 0-100，金额服务端算。
    #[test]
    fn build_line_rejects_bad_input_and_computes_amounts() {
        // 数量 0 / 负数
        let err = build_line(&line_req("0", "10", "13"), 0).expect_err("数量 0 必须被拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("数量必须大于 0"), "文案要指名字段：{}", err.msg);
        let err = build_line(&line_req("-1", "10", "13"), 0).expect_err("负数数量必须被拒");
        assert_eq!(err.status, 400);
        // 单价负数
        let err = build_line(&line_req("1", "-0.01", "13"), 1).expect_err("负单价必须被拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("第 2 行"), "文案要带行号：{}", err.msg);
        // 税率越界
        let err = build_line(&line_req("1", "10", "100.0001"), 0).expect_err("税率 > 100 必须被拒");
        assert_eq!(err.status, 400);
        let err = build_line(&line_req("1", "10", "-0.0001"), 0).expect_err("负税率必须被拒");
        assert_eq!(err.status, 400);
        // 物料 id 非法
        let mut bad = line_req("1", "10", "13");
        bad.material_id = "not-a-uuid".to_string();
        let err = build_line(&bad, 0).expect_err("非法物料 id 必须被拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);

        // 正常：金额由服务端算（3 × 17.655、税 13%）
        let line = build_line(&line_req("3", "17.655", "13"), 0).expect("正常行应通过");
        assert_eq!(line.amount, dec("52.9650"));
        assert_eq!(line.tax_amount, dec("6.8855"));
        // 留空的 price / tax_rate 按 0 处理（不是「不传就报错」）
        let mut zero = line_req("2", "0", "0");
        zero.price = None;
        zero.tax_rate = None;
        let line = build_line(&zero, 0).expect("缺省价税按 0");
        assert_eq!((line.amount, line.tax_amount), (Decimal::ZERO, Decimal::ZERO));
    }

    /// 明细整体：空明细拒（除非明确允许）、超 200 行拒。
    #[test]
    fn build_lines_enforces_count_bounds() {
        let err = build_lines(&[], false).expect_err("空明细必须被拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("至少要 1 行"));
        assert!(build_lines(&[], true).expect("允许空时返回空表").is_empty());
        let many: Vec<QuotationLineReq> = (0..=MAX_LINES).map(|_| line_req("1", "1", "0")).collect();
        let err = build_lines(&many, false).expect_err("超过上限必须被拒");
        assert!(err.msg.contains("最多 200 行"));
        let ok: Vec<QuotationLineReq> = (0..MAX_LINES).map(|_| line_req("1", "1", "0")).collect();
        assert_eq!(build_lines(&ok, false).expect("200 行应通过").len(), MAX_LINES);
    }

    /// 日期解析：严格 `yyyy-MM-dd`，空串当没传。
    #[test]
    fn parse_date_is_strict() {
        assert_eq!(
            parse_date(Some("2026-11-17"), "quote_date").expect("合法日期"),
            NaiveDate::from_ymd_opt(2026, 11, 17)
        );
        assert!(parse_date(Some("   "), "quote_date").expect("空串 = 没传").is_none());
        assert!(parse_date(None, "quote_date").expect("None 合法").is_none());
        for bad in ["2026/11/17", "20261117", "2026-13-01", "yesterday"] {
            let err = parse_date(Some(bad), "quote_date").expect_err("非法日期必须被拒");
            assert_eq!(err.status, 400);
            assert_eq!(err.code, error_code::ERP_FILTER_INVALID);
        }
    }

    /// UUID 解析：空串与非法值都报 400（绝不静默当作「没传」）。
    #[test]
    fn parse_uuid_never_silently_ignores() {
        let id = Uuid::now_v7();
        assert_eq!(parse_uuid(&id.to_string(), "id").expect("合法"), id);
        let err = parse_uuid("", "id").expect_err("空串必须拒");
        assert_eq!(err.status, 400);
        let err = parse_uuid("  abc  ", "customer_id").expect_err("非法值必须拒");
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);
        assert!(err.msg.contains("customer_id"), "文案要指名字段");
    }

    // ==================== 转单：纯逻辑（M3-03） ====================

    /// 一张报价单（只有转单用得上的字段有真实值）。
    fn quote_row(status: &str, amount: &str, tax: &str) -> QuotationRow {
        let today = NaiveDate::from_ymd_opt(2026, 11, 17).expect("固定日期");
        QuotationRow {
            id: Uuid::now_v7(),
            quote_no: "QT-20261117-000001".to_string(),
            customer_id: Uuid::now_v7(),
            quote_date: today,
            valid_until: today,
            status: status.to_string(),
            total_amount: dec(amount),
            total_tax: dec(tax),
            total_with_tax: dec(amount) + dec(tax),
            converted_so_id: None,
            converted_at: None,
            approval_instance_id: None,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
            deleted_at: None,
        }
    }

    fn quote_line(line_no: i32, qty: &str, price: &str, rate: &str) -> QuotationLineRow {
        let (amount, tax_amount) = model::line_amounts(dec(qty), dec(price), dec(rate));
        QuotationLineRow {
            id: Uuid::now_v7(),
            quotation_id: Uuid::now_v7(),
            line_no,
            material_id: Uuid::now_v7(),
            qty: dec(qty),
            price: dec(price),
            amount,
            tax_rate: dec(rate),
            tax_amount,
            remark: None,
        }
    }

    /// 只有 ACCEPTED 能转单；SENT 的文案要告诉用户「先标接受」。
    #[test]
    fn convert_guard_only_accepts_accepted() {
        ensure_convertible(&quote_row(model::STATUS_ACCEPTED, "0", "0")).expect("已接受的单必须能转");
        for status in [model::STATUS_DRAFT, model::STATUS_REJECTED, model::STATUS_CANCELLED] {
            let err = ensure_convertible(&quote_row(status, "0", "0")).expect_err("非 ACCEPTED 一律不能转");
            assert_eq!(err.status, 409, "状态冲突给 409 不是 400：{status}");
            assert!(err.msg.contains(status), "文案要带当前状态码：{}", err.msg);
        }
        let sent = ensure_convertible(&quote_row(model::STATUS_SENT, "0", "0")).expect_err("SENT 不能直接转单");
        assert!(sent.msg.contains("客户已接受"), "SENT 要给可执行提示：{}", sent.msg);
        assert!(sent.msg.contains("ACCEPTED"), "提示要给出目标状态码：{}", sent.msg);
    }

    /// 转单带行：物料 / 数量 / 单价 / 税率 / 行金额原样搬，单头三项合计一致。
    #[test]
    fn convert_plan_copies_lines_verbatim() {
        let lines = vec![quote_line(1, "3", "17.655", "13"), quote_line(2, "2.5", "100", "0")];
        let amount = lines[0].amount + lines[1].amount;
        let tax = lines[0].tax_amount + lines[1].tax_amount;
        let row = quote_row(model::STATUS_ACCEPTED, &amount.to_string(), &tax.to_string());
        let (items, totals) = plan_order_items(&row, &lines).expect("干净数据应通过");
        assert_eq!(items.len(), 2);
        for (item, line) in items.iter().zip(lines.iter()) {
            assert_eq!(item.material_id, line.material_id, "物料不能换");
            assert_eq!(item.qty, line.qty);
            assert_eq!(item.price, line.price);
            assert_eq!(item.tax_rate, line.tax_rate);
            assert_eq!(item.amount, line.amount, "行金额必须整列搬，不重算");
            assert_eq!(item.tax_amount, line.tax_amount);
        }
        assert_eq!(totals.amount, row.total_amount);
        assert_eq!(totals.tax, row.total_tax);
        assert_eq!(totals.with_tax, row.total_with_tax, "价税合计要与报价单同一分钱");
        // 6 位小数单价 × 数量：金额 4 位定标（3 × 17.655 = 52.9650）
        assert_eq!(items[0].amount, dec("52.9650"));
    }

    /// 脏数据一律 409 拒绝（不许把脏金额洗成一张要出库、要开票的订单）。
    #[test]
    fn convert_plan_rejects_dirty_data() {
        // ① 空明细
        let row = quote_row(model::STATUS_ACCEPTED, "0", "0");
        let err = plan_order_items(&row, &[]).expect_err("空明细不能转");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("没有明细"), "{}", err.msg);

        // ② 数量 0 / 单价负 / 税率越界
        for (qty, price, rate, needle) in [
            ("0", "10", "13", "数量"),
            ("1", "-0.01", "13", "单价"),
            ("1", "10", "100.0001", "税率"),
        ] {
            let line = quote_line(1, qty, price, rate);
            // 让单头与行一致，确保命中的是「行级校验」而不是「合计不一致」
            let row = quote_row(
                model::STATUS_ACCEPTED,
                &line.amount.to_string(),
                &line.tax_amount.to_string(),
            );
            let err = plan_order_items(&row, std::slice::from_ref(&line)).expect_err("非法行必须拒");
            assert_eq!(err.status, 409);
            assert!(err.msg.contains(needle), "文案要指名字段（{needle}）：{}", err.msg);
        }

        // ③ 行金额被绕过写入口改脏 → 拒绝（不能原样搬过去）
        let mut line = quote_line(1, "2", "10", "13");
        line.amount = dec("999.9999");
        let row = quote_row(
            model::STATUS_ACCEPTED,
            &line.amount.to_string(),
            &line.tax_amount.to_string(),
        );
        let err = plan_order_items(&row, std::slice::from_ref(&line)).expect_err("脏行金额必须拒");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("对不上"), "{}", err.msg);

        // ④ 明细汇总与单头合计不一致 → 拒绝
        let line = quote_line(1, "2", "10", "13");
        let row = quote_row(model::STATUS_ACCEPTED, "1", "1");
        let err = plan_order_items(&row, std::slice::from_ref(&line)).expect_err("合计不一致必须拒");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("不一致"), "{}", err.msg);
    }

    /// 订单备注永远带来源（用户填的备注只是前缀，不能把来源顶掉）。
    #[test]
    fn convert_remark_always_keeps_source() {
        assert_eq!(
            compose_order_remark("QT-20261117-000001", None),
            "由报价单 QT-20261117-000001 转换"
        );
        assert_eq!(
            compose_order_remark("QT-20261117-000001", Some("   ")),
            "由报价单 QT-20261117-000001 转换",
            "空白备注等同没填"
        );
        assert_eq!(
            compose_order_remark("QT-20261117-000001", Some("款到发货")),
            "款到发货｜由报价单 QT-20261117-000001 转换"
        );
    }

    /// 请求体解析：空 body = 默认参数；有 body 必须合法 JSON（非法一律 400，不静默吞）。
    #[test]
    fn convert_body_is_optional_but_never_silently_ignored() {
        // 空 / 纯空白 body → 全默认（前端「一键转单」不用先拼 `{}`）
        for empty in [&b""[..], b"   ", b"\n\t "] {
            let req = parse_convert_body(empty).expect("空 body 必须走默认值");
            assert!(req.order_date.is_none() && req.delivery_date.is_none());
            assert!(req.salesperson_id.is_none() && req.remark.is_none());
        }
        // 合法 JSON：字段全部可选
        let req = parse_convert_body(r#"{"order_date":"2026-11-17","remark":"款到发货"}"#.as_bytes())
            .expect("合法 JSON 应解析成功");
        assert_eq!(req.order_date.as_deref(), Some("2026-11-17"));
        assert_eq!(req.remark.as_deref(), Some("款到发货"));
        assert!(parse_convert_body(b"{}").expect("空对象合法").order_date.is_none());
        // 非法 JSON / 非 UTF-8 → 400（静默当没传会让用户以为交期设上了）
        for bad in [&b"{"[..], b"[1,2,3]", b"order_date=2026-11-17"] {
            let err = parse_convert_body(bad).expect_err("非法 JSON 必须 400");
            assert_eq!(err.status, 400);
            assert!(err.msg.contains("JSON"), "文案要说清是请求体问题：{}", err.msg);
        }
        let err = parse_convert_body(&[0xff, 0xfe]).expect_err("非 UTF-8 必须 400");
        assert_eq!(err.status, 400);
    }

    // ==================== 转单：真库用例（M3-03） ====================
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    /// 夹具客户编码（清场按它定位 —— 单号是取号器生成的，不能按前缀删单据）。
    const CUSTOMER_CODE: &str = "M303-CUST";

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

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 清场：状态日志 → 销售订单（明细 CASCADE）→ 报价单（明细 CASCADE）→ 夹具客户。
    async fn cleanup() {
        let statements: [(&str, Vec<nik::prelude::Value>); 5] = [
            (
                "DELETE FROM biz_status_log \
                  WHERE entity_id IN (SELECT id FROM biz_quotation \
                                        WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = ?))",
                vec![Value::from(CUSTOMER_CODE)],
            ),
            (
                "DELETE FROM biz_status_log \
                  WHERE entity_id IN (SELECT id FROM biz_sales_order \
                                        WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = ?))",
                vec![Value::from(CUSTOMER_CODE)],
            ),
            (
                "DELETE FROM biz_sales_order \
                  WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = ?)",
                vec![Value::from(CUSTOMER_CODE)],
            ),
            (
                "DELETE FROM biz_quotation \
                  WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = ?)",
                vec![Value::from(CUSTOMER_CODE)],
            ),
            (
                "DELETE FROM biz_customer WHERE code = ?",
                vec![Value::from(CUSTOMER_CODE)],
            ),
        ];
        for (sql, params) in statements {
            if let Err(err) = db().execute_with_params(sql, params).await {
                panic!("清理夹具失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 夹具客户（ACTIVE）。存在则复用。
    async fn fixture_customer() -> Uuid {
        db().execute_with_params(
            "INSERT INTO biz_customer (id, code, name, credit_limit, status) \
             VALUES (gen_random_uuid(), ?, 'M3-03 夹具客户', 1000000, 'ACTIVE') \
             ON CONFLICT DO NOTHING",
            vec![Value::from(CUSTOMER_CODE)],
        )
        .await
        .expect("插夹具客户应成功");
        db().scalar_with_params(
            "SELECT id FROM biz_customer WHERE code = ? AND deleted_at IS NULL",
            vec![Value::from(CUSTOMER_CODE)],
        )
        .await
        .expect("查夹具客户应成功")
    }

    fn new_line(qty: &str, price: &str, rate: &str) -> NewLine {
        let (qty, price, rate) = (dec(qty), dec(price), dec(rate));
        let (amount, tax_amount) = model::line_amounts(qty, price, rate);
        NewLine {
            material_id: Uuid::now_v7(),
            qty,
            price,
            tax_rate: rate,
            amount,
            tax_amount,
            remark: None,
        }
    }

    /// 建一张报价单并推到 `status`（按合法边逐跳 CAS，不绕过状态机）。
    async fn quotation_in_status(customer: Uuid, status: &str, lines: &[NewLine]) -> Uuid {
        let today = model::today_cst(Utc::now());
        let totals = model::Totals::of(lines);
        let tx = db().begin().await.expect("开事务");
        let (id, _no) = model::create_quotation(
            &tx,
            &model::NewQuotation {
                customer_id: customer,
                quote_date: today,
                valid_until: today + nik::prelude::chrono::Duration::days(14),
                remark: Some("M3-03 转单夹具"),
            },
            lines,
            &totals,
            state_machine::Operator::of(None),
            Utc::now(),
        )
        .await
        .expect("建报价单应成功");
        let path: &[&str] = match status {
            model::STATUS_DRAFT => &[],
            model::STATUS_SENT => &[model::STATUS_SENT],
            model::STATUS_ACCEPTED => &[model::STATUS_SENT, model::STATUS_ACCEPTED],
            other => panic!("夹具不支持的中间状态：{other}"),
        };
        let mut from = model::STATUS_DRAFT;
        for target in path {
            let affected = model::apply_status(&tx, id, from, target, None, Utc::now())
                .await
                .expect("状态流转应成功");
            assert_eq!(affected, 1, "CAS 应命中（{from} → {target}）");
            from = target;
        }
        tx.commit().await.expect("提交");
        id
    }

    async fn order_count(customer: Uuid) -> i64 {
        db().scalar_with_params(
            "SELECT count(*) FROM biz_sales_order WHERE customer_id = ? AND deleted_at IS NULL",
            vec![Value::from(&customer)],
        )
        .await
        .expect("统计订单数应成功")
    }

    /// 转单主链：ACCEPTED → 新 DRAFT 订单（行数与金额逐行一致）+ 报价单回填 + 轨迹一行。
    #[tokio::test]
    async fn convert_creates_order_with_lines_and_backfills_quotation() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let lines = vec![new_line("3", "17.655", "13"), new_line("2.5", "100", "0")];
        let quotation_id = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;
        let before = model::find_quotation(quotation_id)
            .await
            .expect("查报价单")
            .expect("报价单必须存在");

        let res = convert_quotation(None, &quotation_id.to_string(), ConvertQuotationReq::default())
            .await
            .expect("转单应成功");

        // ① 订单单头：DRAFT / SO- 单号 / 金额与报价单同一分钱 / 本位币汇率 1 / 备注带来源
        assert!(res.header.so_no.starts_with("SO-"), "订单号前缀：{}", res.header.so_no);
        assert_eq!(res.header.status, so_model::STATUS_DRAFT);
        assert_eq!(res.header.total_amount, before.total_amount);
        assert_eq!(res.header.total_tax, before.total_tax);
        assert_eq!(res.header.total_with_tax, before.total_with_tax);
        assert_eq!(res.header.exchange_rate, Decimal::from(1));
        assert_eq!(
            res.header.base_total_with_tax, before.total_with_tax,
            "本位币单 base = 原币"
        );
        let remark = res.header.remark.clone().unwrap_or_default();
        assert!(remark.contains(&before.quote_no), "订单备注必须留来源：{remark}");
        assert_eq!(res.meta.extra.get("already_converted"), Some(&serde_json::json!(false)));

        // ② 明细逐行搬（物料 / 数量 / 单价 / 税率 / 金额）
        assert_eq!(res.items.len(), 2);
        for (item, line) in res.items.iter().zip(lines.iter()) {
            assert_eq!(item.material_id, line.material_id.to_string());
            assert_eq!(item.qty, line.qty);
            assert_eq!(item.price, line.price);
            assert_eq!(item.tax_rate, line.tax_rate);
            assert_eq!(item.amount, line.amount);
            assert_eq!(item.tax_amount, line.tax_amount);
        }
        // 库里也真的落了这两行（不是只在出参里对）
        let so_id = Uuid::parse_str(&res.header.so_id).expect("订单 id 是 uuid");
        let stored = so_model::items_of(so_id).await.expect("查订单明细");
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].line_no, 1);
        assert_eq!(stored[1].line_no, 2);
        assert_eq!(stored[0].amount, lines[0].amount);

        // ③ 报价单回填（converted_so_id + converted_at 成对）
        let after = model::find_quotation(quotation_id)
            .await
            .expect("查报价单")
            .expect("报价单必须存在");
        assert_eq!(after.converted_so_id, Some(so_id), "必须回填转成的订单 id");
        assert!(after.converted_at.is_some(), "converted_at 必须与 id 成对");
        assert_eq!(after.status, model::STATUS_ACCEPTED, "转单不改状态（仍是已接受）");

        // ④ 轨迹：状态不变但动作是 CONVERT，biz_ref 指向订单
        let log: (String, String, String) = db()
            .find_with_params(
                "SELECT COALESCE(action,'') AS action, to_status, COALESCE(biz_ref_no,'') AS biz_ref_no \
                   FROM biz_status_log \
                  WHERE entity_type = 'QUOTATION' AND entity_id = ?::uuid AND action = ? \
                  ORDER BY created_at DESC LIMIT 1",
                vec![Value::from(&quotation_id), Value::from(CONVERT_ACTION)],
            )
            .await
            .expect("查轨迹应成功")
            .first()
            .map(|row| {
                (
                    row.get_typed("action").unwrap_or_default(),
                    row.get_typed("to_status").unwrap_or_default(),
                    row.get_typed("biz_ref_no").unwrap_or_default(),
                )
            })
            .expect("必须有 CONVERT 轨迹行");
        assert_eq!(log.0, CONVERT_ACTION);
        assert_eq!(log.1, model::STATUS_ACCEPTED, "状态没变（ACCEPTED→ACCEPTED）");
        assert_eq!(log.2, res.header.so_no, "轨迹要指向订单号");
    }

    /// 幂等：再点一次不建第二张订单，200 回原单（`already_converted = true`）。
    #[tokio::test]
    async fn convert_is_idempotent_and_never_builds_two_orders() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let lines = vec![new_line("1", "10", "13")];
        let quotation_id = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;

        let first = convert_quotation(None, &quotation_id.to_string(), ConvertQuotationReq::default())
            .await
            .expect("第一次转单应成功");
        assert!(!first.meta.extra["already_converted"].as_bool().unwrap_or(true));
        let second = convert_quotation(None, &quotation_id.to_string(), ConvertQuotationReq::default())
            .await
            .expect("第二次转单应成功（幂等）");
        assert_eq!(second.header.so_id, first.header.so_id, "必须回同一张订单");
        assert_eq!(second.header.so_no, first.header.so_no);
        assert_eq!(
            second.meta.extra.get("already_converted"),
            Some(&serde_json::json!(true)),
            "第二次要标记 already_converted = true"
        );
        assert_eq!(order_count(customer).await, 1, "重复转单绝不能建出第二张订单");
        assert_eq!(second.items.len(), 1, "幂等路径也要带明细");
    }

    /// 守卫：非 ACCEPTED 一律 409，且**一张订单都不许建**（不能留半截数据）。
    #[tokio::test]
    async fn convert_refuses_non_accepted_and_leaves_no_order() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let lines = vec![new_line("1", "10", "13")];

        // SENT：提示先标「客户已接受」
        let sent = quotation_in_status(customer, model::STATUS_SENT, &lines).await;
        let err = convert_quotation(None, &sent.to_string(), ConvertQuotationReq::default())
            .await
            .expect_err("SENT 不能转单");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("客户已接受"), "文案要给可执行提示：{}", err.msg);
        assert_eq!(order_count(customer).await, 0, "被守卫挡下时不许建订单");
        assert!(
            model::find_quotation(sent)
                .await
                .expect("查报价单")
                .expect("报价单在")
                .converted_so_id
                .is_none(),
            "被守卫挡下时不许回填"
        );

        // DRAFT：同样 409
        let draft = quotation_in_status(customer, model::STATUS_DRAFT, &lines).await;
        let err = convert_quotation(None, &draft.to_string(), ConvertQuotationReq::default())
            .await
            .expect_err("DRAFT 不能转单");
        assert_eq!(err.status, 409);
        assert_eq!(order_count(customer).await, 0);

        // 客户冻结：ACCEPTED 也转不了，且订单数仍为 0（整笔回滚）
        let accepted = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;
        db().execute_with_params(
            "UPDATE biz_customer SET status = 'FROZEN' WHERE id = ?",
            vec![Value::from(&customer)],
        )
        .await
        .expect("冻结客户应成功");
        let err = convert_quotation(None, &accepted.to_string(), ConvertQuotationReq::default())
            .await
            .expect_err("冻结客户不能转单");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("冻结"), "{}", err.msg);
        assert_eq!(order_count(customer).await, 0, "失败路径不许留孤订单");
        db().execute_with_params(
            "UPDATE biz_customer SET status = 'ACTIVE' WHERE id = ?",
            vec![Value::from(&customer)],
        )
        .await
        .expect("解冻客户应成功");

        // 解冻后同一张单能转（证明前置失败没有留下「转不了」的中间态）
        let ok = convert_quotation(None, &accepted.to_string(), ConvertQuotationReq::default())
            .await
            .expect("解冻后应能转单");
        assert_eq!(ok.header.status, so_model::STATUS_DRAFT);
        assert_eq!(order_count(customer).await, 1);
    }
}
