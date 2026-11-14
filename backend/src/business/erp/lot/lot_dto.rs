//! 批次主数据契约层（成本核算 · ARCH-9 §3.1 ① / §5.2 #7–#9 / §5.3）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//! §3.1 ①（`biz_lot` 表结构 + 4 条 CHECK + 唯一索引
//! `uk_biz_lot_material_no (material_id, lot_no) WHERE deleted_at IS NULL`）、
//! §5.2「批次主数据」3 个接口、§5.3 权限码 `business:erp:lot:{list,create,update}`。
//!
//! # 为什么批次主数据要有独立档案（设计 §2.1 实测结论）
//!
//! 库里原先只有三处**自由文本**批号（`biz_stock_in_item.lot_no` /
//! `biz_stock_out_item.lot_no` / `biz_material_serial.batch_no`），同一批货写法
//! 不同即断链。`biz_lot` 是「物料级」档案：唯一键是 `(material_id, lot_no)`，
//! **不是** `lot_no` 全局唯一 —— 不同物料可以碰巧用同一个批号写法。
//!
//! # 出参口径（与仓库现状一致，§5.1）
//!
//! ```text
//! 列表   { list, total, page, page_size }
//! 下拉   [ { id, lot_no, material_id, ... } ]   （`GET /business/erp/lot/options`）
//! 日期   "YYYY-MM-DD" 字符串；状态 `status` 是 1/0 数字（DDL 是 SMALLINT）
//! ```
//!
//! # 唯一键冲突为什么要单独一个错误码
//!
//! `uk_biz_lot_material_no` 是**部分唯一索引**（`WHERE deleted_at IS NULL`），
//! PG 报错原文是 `ERROR: duplicate key value violates unique constraint
//! "uk_biz_lot_material_no"`。如果直接冒到前端，用户看到的是英文约束名；
//! 如果按文案猜（匹配 "duplicate key"），`DbError` 的 `Display` 又是
//! `unique constraint violated constraint=…`，会判成 500（本仓已有踩坑记录，
//! 见 `erp::handler::write_error` 的注释）。所以：
//!
//! * 判据走 **`DbError::UniqueViolation` 变体**，不看文案；
//! * 文案由 [`duplicate_message`] 生成（带物料名 + 批号 + 怎么办），
//!   错误码 [`error_code::ERP_LOT_NO_DUPLICATE`]（`66056`）。
//!
//! # 哪些字段建了就不能改
//!
//! | 字段 | 口径 | 理由 |
//! |---|---|---|
//! | `material_id` | **建单后只读** | 它是业务键的一半：改物料等于把这条档案换成另一个物料的批次，历史单据上的批号会指向错误物料 |
//! | `source_type` / `source_doc_*` | 建单可填、可改 | 来源是审计信息，允许手工维护（期初 / 导入场景） |
//! | `lot_no` | 可改 | 允许纠错（写错字），但改完仍受唯一键约束 |
//!
//! 设计稿没有逐字段写「可不可改」，本条是「业务键不可变」的通用口径落地，
//! 已在卡内作为已知偏差记录。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== 值域（与 V0370 的 CHECK 逐字一致） ====================

/// 来源类型：采购批。
pub const SOURCE_PURCHASE: &str = "PURCHASE";
/// 来源类型：生产批（成本对象建批时自动写这个）。
pub const SOURCE_PRODUCTION: &str = "PRODUCTION";
/// 来源类型：期初（上线时存量批）。
pub const SOURCE_OPENING: &str = "OPENING";
/// 来源类型：手工建（默认值）。
pub const SOURCE_MANUAL: &str = "MANUAL";
/// 来源类型值域（`ck_biz_lot_source`）。
pub const SOURCE_TYPES: [&str; 4] = [SOURCE_PURCHASE, SOURCE_PRODUCTION, SOURCE_OPENING, SOURCE_MANUAL];

/// 质检状态：不适用（默认）。
pub const QC_NA: &str = "NA";
/// 质检状态：待检。
pub const QC_PENDING: &str = "PENDING";
/// 质检状态：合格。
pub const QC_PASSED: &str = "PASSED";
/// 质检状态：不合格。
pub const QC_REJECTED: &str = "REJECTED";
/// 质检状态值域（`ck_biz_lot_qc`）。
pub const QC_STATUSES: [&str; 4] = [QC_NA, QC_PENDING, QC_PASSED, QC_REJECTED];

/// 批次状态：启用（可被领料下拉选中）。
pub const STATUS_ENABLED: i16 = 1;
/// 批次状态：停用（历史保留，但不再出现在下拉里）。
pub const STATUS_DISABLED: i16 = 0;

/// 来源单据类型值域（DDL 无 CHECK，这里按实际写入方收敛，防止手填垃圾）。
///
/// 写入方实测：`STOCK_IN`（入库过账，B2）、`COST_PERIOD`（期间回填）、
/// `COST_OBJECT`（`cost_object_model::ensure_production_lot`）、
/// `BACKFILL`（V0380 存量回填）。空值合法（手工建批没有来源单据）。
pub const SOURCE_DOC_TYPES: [&str; 4] = ["STOCK_IN", "COST_PERIOD", "COST_OBJECT", "BACKFILL"];

/// 批号长度上限（DDL `lot_no VARCHAR(64)`）。
pub const LOT_NO_MAX: usize = 64;
/// 来源单号长度上限（DDL `source_doc_no VARCHAR(32)`）。
pub const SOURCE_DOC_NO_MAX: usize = 32;
/// 下拉接口最多返回多少条（一次领料不会挑 200 个批；上限防止整表吐出去）。
pub const OPTIONS_LIMIT: i64 = 200;

/// 批号重复的业务错误码（`66056`，见 `shared::error_code`）。
pub const DUPLICATE_CODE: i32 = error_code::ERP_LOT_NO_DUPLICATE;

// ==================== 入参 ====================

/// 列表查询（§5.2 #7；筛选口径不在设计稿里逐条写，按批号档案的实际用法给）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LotPageQuery {
    /// 关键字：批号 / 物料编码 / 物料名称（模糊）。
    pub keyword: Option<String>,
    /// 物料。
    pub material_id: Option<String>,
    /// 来源类型 `PURCHASE` / `PRODUCTION` / `OPENING` / `MANUAL`。
    pub source_type: Option<String>,
    /// 质检状态 `NA` / `PENDING` / `PASSED` / `REJECTED`。
    pub qc_status: Option<String>,
    /// 状态 `1` / `0`。
    pub status: Option<i16>,
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（0 = 默认 20，上限 200）。
    pub page_size: Option<u64>,
}

/// 归一化后的列表筛选（model 只认这个）。
#[derive(Debug, Clone)]
pub struct LotFilter {
    /// 关键字（`LIKE` 模式由 model 侧拼）。
    pub keyword: Option<String>,
    /// 物料。
    pub material_id: Option<Uuid>,
    /// 来源类型。
    pub source_type: Option<String>,
    /// 质检状态。
    pub qc_status: Option<String>,
    /// 状态。
    pub status: Option<i16>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 「按物料取批号」下拉查询（本卡新增的内部接口，供领料 / 成本对象表单用）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LotOptionQuery {
    /// 物料（必填）。
    pub material_id: Option<String>,
    /// 批号关键字（前端输入即筛）。
    pub keyword: Option<String>,
}

/// 新建批次（§5.2 #8）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LotCreateReq {
    /// 批号（必填，≤64）。
    pub lot_no: Option<String>,
    /// 物料（必填）。
    pub material_id: Option<String>,
    /// 来源类型（缺省 `MANUAL`）。
    pub source_type: Option<String>,
    /// 来源单据类型（可空）。
    pub source_doc_type: Option<String>,
    /// 来源单据 id（可空）。
    pub source_doc_id: Option<String>,
    /// 来源单号快照（可空，≤32）。
    pub source_doc_no: Option<String>,
    /// 供应商（采购批；可空）。
    pub supplier_id: Option<String>,
    /// 生产日期 / 到货日期（`YYYY-MM-DD`）。
    pub prod_date: Option<String>,
    /// 失效日期（≥ `prod_date`）。
    pub expire_date: Option<String>,
    /// 质检状态（缺省 `NA`）。
    pub qc_status: Option<String>,
    /// 状态（缺省 1 启用）。
    pub status: Option<i16>,
    /// 备注。
    pub remark: Option<String>,
}

/// 编辑批次（§5.2 #9）：**部分更新** —— 缺失 = 不改，`null` = 清空（可空字段）。
///
/// `material_id` 不在入参里（业务键不可变，见模块头注释）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LotUpdateReq {
    /// 批号（纠错用；仍受唯一键约束）。
    pub lot_no: Option<String>,
    /// 来源类型。
    pub source_type: Option<String>,
    /// 来源单据类型（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub source_doc_type: Option<Option<String>>,
    /// 来源单据 id（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub source_doc_id: Option<Option<String>>,
    /// 来源单号（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub source_doc_no: Option<Option<String>>,
    /// 供应商（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub supplier_id: Option<Option<String>>,
    /// 生产日期（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub prod_date: Option<Option<String>>,
    /// 失效日期（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub expire_date: Option<Option<String>>,
    /// 质检状态。
    pub qc_status: Option<String>,
    /// 状态（1 启用 / 0 停用）。
    pub status: Option<i16>,
    /// 备注（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

// ==================== 归一化规格（dto 与 model 之间唯一的形状） ====================

/// 归一化后的批次规格（纯逻辑产物，查库校验由 handler 做）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LotSpec {
    /// 批号。
    pub lot_no: String,
    /// 物料。
    pub material_id: Uuid,
    /// 来源类型。
    pub source_type: String,
    /// 来源单据类型。
    pub source_doc_type: Option<String>,
    /// 来源单据 id。
    pub source_doc_id: Option<Uuid>,
    /// 来源单号快照。
    pub source_doc_no: Option<String>,
    /// 供应商。
    pub supplier_id: Option<Uuid>,
    /// 生产日期。
    pub prod_date: Option<NaiveDate>,
    /// 失效日期。
    pub expire_date: Option<NaiveDate>,
    /// 质检状态。
    pub qc_status: String,
    /// 状态（1/0）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== 出参 ====================

/// 列表行（`*_id` 都带 `*_name` —— AGENTS 约定 13）。
#[derive(Debug, Clone, Serialize)]
pub struct LotListRes {
    /// 主键。
    pub id: String,
    /// 批号。
    pub lot_no: String,
    /// 物料。
    pub material_id: String,
    /// 物料名称（`biz_material` 实时 join）。
    pub material_name: Option<String>,
    /// 物料编码（`biz_material` 实时 join）。
    pub material_code: Option<String>,
    /// 来源类型。
    pub source_type: String,
    /// 来源单据类型。
    pub source_doc_type: Option<String>,
    /// 来源单据 id。
    pub source_doc_id: Option<String>,
    /// 来源单号快照。
    pub source_doc_no: Option<String>,
    /// 供应商。
    pub supplier_id: Option<String>,
    /// 供应商名称（`biz_supplier` 实时 join）。
    pub supplier_name: Option<String>,
    /// 生产日期。
    pub prod_date: Option<String>,
    /// 失效日期。
    pub expire_date: Option<String>,
    /// 质检状态。
    pub qc_status: String,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 更新时间（RFC3339）。
    pub updated_at: String,
}

/// 下拉选项（`GET /business/erp/lot/options`，供领料 / 成本对象表单）。
///
/// 只给「选批号」需要的字段：id / lot_no / 物料 / 来源 / 日期 / 质检。
/// 不给 `remark` / 审计列 —— 下拉里用不到，多传只是多一份要维护的口径。
#[derive(Debug, Clone, Serialize)]
pub struct LotOptionRes {
    /// 主键。
    pub id: String,
    /// 批号（显示 + 回填两用）。
    pub lot_no: String,
    /// 物料。
    pub material_id: String,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 来源类型。
    pub source_type: String,
    /// 生产日期。
    pub prod_date: Option<String>,
    /// 失效日期。
    pub expire_date: Option<String>,
    /// 质检状态（前端要能把「不合格」标红）。
    pub qc_status: String,
}

// ==================== 解析 / 校验小件 ====================

/// 400 构造（本模块唯一出口）。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409 构造。
pub fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 404 构造。
pub fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("批次不存在或已删除（{id}）"),
    )
}

/// 批号重复的友好文案（唯一键冲突的唯一出口）。
///
/// `material_label` 由 handler 查 `biz_material` 得到（「MAT-0001 钢轴」这样的
/// 人话标识）；查不到就退化成物料 UUID —— 但那时 `ensure_refs` 已经先报 400 了，
/// 这里只是兜底。
pub fn duplicate_message(lot_no: &str, material_label: &str) -> String {
    format!(
        "批号「{lot_no}」在物料「{material_label}」下已存在：同一物料不能有两个相同批号。\n\
         请改用其它批号，或到「批次管理」里编辑已有那一行（已软删的批号可以复用）"
    )
}

/// 409 + [`DUPLICATE_CODE`]（`66056`）。
fn duplicate_error(lot_no: &str, material_label: &str) -> ApiError {
    ApiError::new(409, DUPLICATE_CODE, duplicate_message(lot_no, material_label))
}

/// 批号唯一键冲突 → 友好 409；其它 DB 错误原样交给调用方的上下文处理。
///
/// 判据是 `DbError::UniqueViolation` **变体**（不看 `Display` 文案）。
/// `constraint` 不是 `uk_biz_lot_material_no` 时也按批号重复报 —— 本表只有这一个
/// 唯一索引（主键冲突意味着刚生成的 v7 UUID 撞了，属天灾，同样回归这条码比 500 好）。
pub fn map_write_error(err: DbError, lot_no: &str, material_label: &str, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { .. } => duplicate_error(lot_no, material_label),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足数据库约束{}（批次的枚举 / 日期先后关系）",
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        _ => crate::utils::db_error(err, ctx),
    }
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

/// 批号必填 + 长度 + 不许全是空白。
fn require_lot_no(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("lot_no（批号）不能为空"))?;
    if text.chars().count() > LOT_NO_MAX {
        return Err(bad_request(format!(
            "lot_no（批号）最长 {LOT_NO_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(text.to_string())
}

/// 来源类型归一化（大小写无关；空 → `MANUAL`）。
pub fn normalize_source_type(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => return Ok(SOURCE_MANUAL.to_string()),
        Some(text) => text.to_ascii_uppercase(),
    };
    if !SOURCE_TYPES.contains(&text.as_str()) {
        return Err(bad_request(format!(
            "source_type 只认 {SOURCE_TYPES:?}（收到「{text}」）"
        )));
    }
    Ok(text)
}

/// 质检状态归一化（空 → `NA`）。
pub fn normalize_qc_status(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => return Ok(QC_NA.to_string()),
        Some(text) => text.to_ascii_uppercase(),
    };
    if !QC_STATUSES.contains(&text.as_str()) {
        return Err(bad_request(format!("qc_status 只认 {QC_STATUSES:?}（收到「{text}」）")));
    }
    Ok(text)
}

/// 来源单据类型归一化（空 = 没有来源单据；非空必须在值域内）。
pub fn normalize_source_doc_type(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let upper = text.to_ascii_uppercase();
    if !SOURCE_DOC_TYPES.contains(&upper.as_str()) {
        return Err(bad_request(format!(
            "source_doc_type 只认 {SOURCE_DOC_TYPES:?} 或留空（收到「{text}」）"
        )));
    }
    Ok(Some(upper))
}

/// 状态归一化（缺省 1；只认 0 / 1，与 `ck_biz_lot_status` 一致）。
pub fn normalize_status(raw: Option<i16>) -> std::result::Result<i16, ApiError> {
    match raw {
        None => Ok(STATUS_ENABLED),
        Some(value @ (STATUS_ENABLED | STATUS_DISABLED)) => Ok(value),
        Some(other) => Err(bad_request(format!("status 只认 0（停用）/ 1（启用）（收到 {other}）"))),
    }
}

/// 失效日期不得早于生产日期（与 `ck_biz_lot_dates` 一致，提前给 400）。
pub fn ensure_dates(prod_date: Option<NaiveDate>, expire_date: Option<NaiveDate>) -> std::result::Result<(), ApiError> {
    if let (Some(prod), Some(expire)) = (prod_date, expire_date)
        && expire < prod
    {
        return Err(bad_request(format!(
            "expire_date（{expire}）不能早于 prod_date（{prod}）"
        )));
    }
    Ok(())
}

/// 来源单号长度（`VARCHAR(32)`），超长提前 400 而不是让库抛 500。
fn clean_doc_no(value: Option<String>) -> std::result::Result<Option<String>, ApiError> {
    let text = clean_optional(value);
    if let Some(text) = text.as_deref()
        && text.chars().count() > SOURCE_DOC_NO_MAX
    {
        return Err(bad_request(format!(
            "source_doc_no（来源单号）最长 {SOURCE_DOC_NO_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(text)
}

// ==================== 建 / 改：字段校验（纯逻辑） ====================

/// 新建请求 → 归一化规格（**纯逻辑**：物料 / 供应商是否存在由 handler 查库）。
pub fn spec_from_create(req: &LotCreateReq) -> std::result::Result<LotSpec, ApiError> {
    let prod_date = parse_opt_date(req.prod_date.as_deref(), "prod_date")?;
    let expire_date = parse_opt_date(req.expire_date.as_deref(), "expire_date")?;
    ensure_dates(prod_date, expire_date)?;
    Ok(LotSpec {
        lot_no: require_lot_no(req.lot_no.as_deref())?,
        material_id: require_uuid(req.material_id.as_deref(), "material_id")?,
        source_type: normalize_source_type(req.source_type.as_deref())?,
        source_doc_type: normalize_source_doc_type(req.source_doc_type.as_deref())?,
        source_doc_id: parse_opt_uuid(req.source_doc_id.as_deref(), "source_doc_id")?,
        source_doc_no: clean_doc_no(req.source_doc_no.clone())?,
        supplier_id: parse_opt_uuid(req.supplier_id.as_deref(), "supplier_id")?,
        prod_date,
        expire_date,
        qc_status: normalize_qc_status(req.qc_status.as_deref())?,
        status: normalize_status(req.status)?,
        remark: clean_optional(req.remark.clone()),
    })
}

/// 编辑请求 → 归一化规格（与当前值合并；**纯逻辑**）。
///
/// `material_id` 不在可改范围内（业务键不可变，见模块头注释）。
pub fn spec_from_update(current: &LotSpec, req: &LotUpdateReq) -> std::result::Result<LotSpec, ApiError> {
    let mut spec = current.clone();
    if let Some(raw) = req.lot_no.as_deref() {
        spec.lot_no = require_lot_no(Some(raw))?;
    }
    if let Some(raw) = req.source_type.as_deref() {
        spec.source_type = normalize_source_type(Some(raw))?;
    }
    if let Some(value) = req.source_doc_type.as_ref() {
        spec.source_doc_type = normalize_source_doc_type(value.as_deref())?;
    }
    if let Some(value) = req.source_doc_id.as_ref() {
        spec.source_doc_id = parse_opt_uuid(value.as_deref(), "source_doc_id")?;
    }
    if let Some(value) = req.source_doc_no.as_ref() {
        spec.source_doc_no = clean_doc_no(value.clone())?;
    }
    if let Some(value) = req.supplier_id.as_ref() {
        spec.supplier_id = parse_opt_uuid(value.as_deref(), "supplier_id")?;
    }
    if let Some(value) = req.prod_date.as_ref() {
        spec.prod_date = parse_opt_date(value.as_deref(), "prod_date")?;
    }
    if let Some(value) = req.expire_date.as_ref() {
        spec.expire_date = parse_opt_date(value.as_deref(), "expire_date")?;
    }
    if let Some(raw) = req.qc_status.as_deref() {
        spec.qc_status = normalize_qc_status(Some(raw))?;
    }
    if let Some(raw) = req.status {
        spec.status = normalize_status(Some(raw))?;
    }
    if let Some(value) = req.remark.as_ref() {
        spec.remark = clean_optional(value.clone());
    }
    ensure_dates(spec.prod_date, spec.expire_date)?;
    Ok(spec)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn base_create() -> LotCreateReq {
        LotCreateReq {
            lot_no: Some("  LOT-A1  ".to_string()),
            material_id: Some("018f4b2c-1111-7222-8333-444455556666".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn value_domains_match_migration_checks() {
        // 与 V0370 的 4 条 CHECK 逐字一致（改这里就得先改迁移，迁移不可改 ⇒ 两边都不能动）
        assert_eq!(SOURCE_TYPES, ["PURCHASE", "PRODUCTION", "OPENING", "MANUAL"]);
        assert_eq!(QC_STATUSES, ["NA", "PENDING", "PASSED", "REJECTED"]);
        assert_eq!((STATUS_ENABLED, STATUS_DISABLED), (1, 0));
        assert_eq!(LOT_NO_MAX, 64, "lot_no VARCHAR(64)");
        assert_eq!(SOURCE_DOC_NO_MAX, 32, "source_doc_no VARCHAR(32)");
    }

    #[test]
    fn duplicate_code_is_66056_and_message_is_human() {
        assert_eq!(DUPLICATE_CODE, 66056);
        let message = duplicate_message("LOT-A1", "MAT-0001 钢轴");
        assert!(message.contains("LOT-A1"), "{message}");
        assert!(message.contains("MAT-0001 钢轴"), "{message}");
        assert!(message.contains("已存在"), "{message}");
        assert!(!message.contains("duplicate key"), "不许把 PG 原文给用户：{message}");
        assert!(
            !message.contains("uk_biz_lot_material_no"),
            "约束名不该出现在文案里：{message}"
        );
    }

    #[test]
    fn unique_violation_maps_to_409_66056() {
        let error = map_write_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_biz_lot_material_no".to_string()),
                source: None,
            },
            "LOT-A1",
            "MAT-0001 钢轴",
            "测试",
        );
        assert_eq!(error.status, 409);
        assert_eq!(error.code, DUPLICATE_CODE);
        assert!(error.msg.contains("LOT-A1"), "{}", error.msg);

        // CHECK 兜底 → 400（不是 500）
        let error = map_write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_lot_dates".to_string()),
                source: None,
            },
            "LOT-A1",
            "MAT-0001 钢轴",
            "测试",
        );
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("ck_biz_lot_dates"), "{}", error.msg);

        // 其它错误仍是 500
        let error = map_write_error(DbError::InvalidValue("别的错".to_string()), "LOT-A1", "M", "测试");
        assert_eq!(error.status, 500);
        assert_eq!(error.code, error_code::INTERNAL_ERROR);
    }

    #[test]
    fn create_fills_defaults_and_trims() {
        let spec = spec_from_create(&base_create()).expect("建批");
        assert_eq!(spec.lot_no, "LOT-A1", "批号要 trim");
        assert_eq!(spec.source_type, SOURCE_MANUAL, "来源缺省 MANUAL");
        assert_eq!(spec.qc_status, QC_NA, "质检缺省 NA");
        assert_eq!(spec.status, STATUS_ENABLED, "状态缺省启用");
        assert_eq!(spec.source_doc_type, None);
        assert_eq!(spec.prod_date, None);
    }

    #[test]
    fn create_rejects_missing_lot_no_and_material() {
        let mut req = base_create();
        req.lot_no = Some("   ".to_string());
        let error = spec_from_create(&req).expect_err("空批号");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("lot_no"), "{}", error.msg);

        let mut req = base_create();
        req.material_id = None;
        let error = spec_from_create(&req).expect_err("缺物料");
        assert!(error.msg.contains("material_id"), "{}", error.msg);
    }

    #[test]
    fn create_rejects_overlong_lot_no_and_doc_no() {
        let mut req = base_create();
        req.lot_no = Some("L".repeat(LOT_NO_MAX + 1));
        let error = spec_from_create(&req).expect_err("批号超长");
        assert!(error.msg.contains("64"), "{}", error.msg);

        let mut req = base_create();
        req.source_doc_no = Some("D".repeat(SOURCE_DOC_NO_MAX + 1));
        let error = spec_from_create(&req).expect_err("来源单号超长");
        assert!(error.msg.contains("source_doc_no"), "{}", error.msg);
    }

    #[test]
    fn create_rejects_unknown_enums() {
        let mut req = base_create();
        req.source_type = Some("MES".to_string());
        let error = spec_from_create(&req).expect_err("未知来源");
        assert!(error.msg.contains("source_type"), "{}", error.msg);

        let mut req = base_create();
        req.qc_status = Some("OK".to_string());
        let error = spec_from_create(&req).expect_err("未知质检");
        assert!(error.msg.contains("qc_status"), "{}", error.msg);

        let mut req = base_create();
        req.source_doc_type = Some("PICKING".to_string());
        let error = spec_from_create(&req).expect_err("未知来源单据类型");
        assert!(error.msg.contains("source_doc_type"), "{}", error.msg);

        let mut req = base_create();
        req.status = Some(2);
        let error = spec_from_create(&req).expect_err("未知状态");
        assert!(error.msg.contains("status"), "{}", error.msg);
    }

    #[test]
    fn create_normalizes_enums_case_insensitively() {
        let mut req = base_create();
        req.source_type = Some(" production ".to_string());
        req.qc_status = Some("passed".to_string());
        req.source_doc_type = Some(" cost_object ".to_string());
        let spec = spec_from_create(&req).expect("归一化");
        assert_eq!(spec.source_type, "PRODUCTION");
        assert_eq!(spec.qc_status, "PASSED");
        assert_eq!(spec.source_doc_type.as_deref(), Some("COST_OBJECT"));
    }

    #[test]
    fn create_rejects_expire_before_prod() {
        let mut req = base_create();
        req.prod_date = Some("2026-11-10".to_string());
        req.expire_date = Some("2026-11-09".to_string());
        let error = spec_from_create(&req).expect_err("日期倒挂");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("expire_date"), "{}", error.msg);
    }

    #[test]
    fn create_accepts_same_day_expiry() {
        let mut req = base_create();
        req.prod_date = Some("2026-11-10".to_string());
        req.expire_date = Some("2026-11-10".to_string());
        assert!(spec_from_create(&req).is_ok(), "同一天合法（CHECK 是 expire >= prod）");
    }

    #[test]
    fn update_merges_partially_and_keeps_material() {
        let current = spec_from_create(&base_create()).expect("建批");
        // 只给一个字段：其余不变
        let req = LotUpdateReq {
            qc_status: Some("rejected".to_string()),
            ..Default::default()
        };
        let spec = spec_from_update(&current, &req).expect("改批");
        assert_eq!(spec.qc_status, QC_REJECTED);
        assert_eq!(spec.lot_no, current.lot_no);
        assert_eq!(spec.material_id, current.material_id);
        assert_eq!(spec.status, STATUS_ENABLED);
    }

    #[test]
    fn update_three_state_fields_clear_on_explicit_null() {
        let mut create = base_create();
        create.prod_date = Some("2026-11-01".to_string());
        create.expire_date = Some("2026-12-01".to_string());
        create.remark = Some("首件".to_string());
        let current = spec_from_create(&create).expect("建批");

        let req = LotUpdateReq {
            expire_date: Some(None),
            remark: Some(None),
            ..Default::default()
        };
        let spec = spec_from_update(&current, &req).expect("清空");
        assert_eq!(spec.expire_date, None, "显式 null 要清空");
        assert_eq!(spec.remark, None);
        assert_eq!(spec.prod_date, Some("2026-11-01".parse().unwrap()), "没提的字段不动");
    }

    #[test]
    fn update_rechecks_date_order_after_merge() {
        let mut create = base_create();
        create.prod_date = Some("2026-11-10".to_string());
        create.expire_date = Some("2026-12-10".to_string());
        let current = spec_from_create(&create).expect("建批");

        // 只把生产日期挪到失效日期之后 ⇒ 合并后必须报 400（不是靠库的 CHECK）
        let req = LotUpdateReq {
            prod_date: Some(Some("2027-01-01".to_string())),
            ..Default::default()
        };
        let error = spec_from_update(&current, &req).expect_err("日期倒挂");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("expire_date"), "{}", error.msg);
    }

    #[test]
    fn update_rejects_blank_lot_no() {
        let current = spec_from_create(&base_create()).expect("建批");
        let req = LotUpdateReq {
            lot_no: Some("  ".to_string()),
            ..Default::default()
        };
        let error = spec_from_update(&current, &req).expect_err("空批号");
        assert!(error.msg.contains("lot_no"), "{}", error.msg);
    }

    #[test]
    fn option_limit_is_bounded() {
        assert_eq!(OPTIONS_LIMIT, 200);
        // 上限必须大于 0 且不超过列表硬上限（200 与 `model_base::MAX_PAGE_SIZE` 同口径）
        let limit = usize::try_from(OPTIONS_LIMIT).expect("上限能转 usize");
        assert!(limit > 0 && limit <= 200, "下拉上限 {limit}");
    }
}
