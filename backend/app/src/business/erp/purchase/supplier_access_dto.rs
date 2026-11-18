//! 供应商准入 / 黑名单接口 DTO（M4-14）。
//!
//! 与 `V0455__supplier_access.up.sql` 的 4 个接口码一一对应：
//! `business:erp:supplier-access:{list,create,update,submit}`。
//! 页面显形码 `…:page` **只给前端**，本文件（以及整个后端源码）零引用 —— 约定 14。
//!
//! # 校验口径
//!
//! * 枚举值一律**闭集校验**（不在闭集 → 400，不静默忽略）；
//! * 理由在**提交**时才要求 ≥ 5 字（草稿允许边写边存）；
//! * 等级快照只允许出现在黑名单单上（与库层 `ck_biz_sa_grade_only_blacklist` 同一条判定）。

use axum::Json;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::purchase::supplier_access_model::{
    self as model, ACCESS_STATUSES, ACCESS_TYPES, AccessRow, MIN_REASON_CHARS,
};
use cloudpivot_kernel::shared::error_code;

// ==================== 权限码（与 V0455 的 sys_menu.auth_code 逐字一致） ====================

/// 本模块全部接口码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const ACCESS_PERMISSIONS: [&str; 4] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_SUBMIT];
/// 列表 + 详情。
pub const PERM_LIST: &str = "business:erp:supplier-access:list";
/// 新建申请单。
pub const PERM_CREATE: &str = "business:erp:supplier-access:create";
/// 改 / 删草稿。
pub const PERM_UPDATE: &str = "business:erp:supplier-access:update";
/// 提交审批。
pub const PERM_SUBMIT: &str = "business:erp:supplier-access:submit";

// ==================== 400 / 409 小工具 ====================

/// 400 + 参数错。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409 + 状态闸（申请单状态不接受该动作）。
pub fn status_conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::ERP_SUPPLIER_ACCESS_STATUS_GUARD, message.into())
}

/// 404 + 单据不存在。
pub fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("供应商准入 / 黑名单申请单 {id} 不存在或已删除"),
    )
}

// ==================== 分页查询 ====================

/// 分页查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AccessPageQuery {
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（1–200）。
    pub page_size: Option<u64>,
    /// 关键字（单号 / 供应商编码 / 供应商名 / 理由）。
    pub keyword: Option<String>,
    /// 状态筛选（DRAFT / SUBMITTED / APPROVED）。
    pub status: Option<String>,
    /// 类型筛选（ADMIT / BLACKLIST / RELEASE）。
    pub access_type: Option<String>,
    /// 供应商筛选。
    pub supplier_id: Option<String>,
}

/// 已校验的分页筛选（model 只认这个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 关键字（已 trim）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 类型。
    pub access_type: Option<String>,
    /// 供应商。
    pub supplier_id: Option<Uuid>,
}

impl AccessPageQuery {
    /// 校验并归一化（非法值 → 400，不静默忽略）。
    pub fn filter(&self) -> std::result::Result<AccessFilter, ApiError> {
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(20).clamp(1, 200);
        let status = match self.status.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(status) => {
                let status = status.to_ascii_uppercase();
                if !ACCESS_STATUSES.contains(&status.as_str()) {
                    return Err(bad_request(format!(
                        "status={status} 不在状态闭集内（{}）",
                        ACCESS_STATUSES.join(" / ")
                    )));
                }
                Some(status)
            }
        };
        let access_type = match self.access_type.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(access_type) => {
                let access_type = access_type.to_ascii_uppercase();
                if !ACCESS_TYPES.contains(&access_type.as_str()) {
                    return Err(bad_request(format!(
                        "access_type={access_type} 不在类型闭集内（{}）",
                        ACCESS_TYPES.join(" / ")
                    )));
                }
                Some(access_type)
            }
        };
        let keyword = match self.keyword.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => {
                if text.chars().count() > 64 {
                    return Err(bad_request("keyword 最长 64 个字符"));
                }
                Some(text.to_string())
            }
        };
        Ok(AccessFilter {
            page,
            page_size,
            keyword,
            status,
            access_type,
            supplier_id: parse_optional_uuid(self.supplier_id.as_deref(), "supplier_id")?,
        })
    }
}

// ==================== 入参 ====================

/// 新建申请单。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAccessReq {
    /// 供应商 id（必填）。
    pub supplier_id: String,
    /// 类型（ADMIT / BLACKLIST / RELEASE，必填）。
    pub access_type: String,
    /// 申请理由（草稿可空；提交时 ≥ 5 字）。
    pub reason: Option<String>,
    /// 评估等级快照（只有黑名单可填 A/B/C/D）。
    pub grade_snapshot: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 改草稿（类型与供应商不可改）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateAccessReq {
    /// 申请理由。
    pub reason: Option<String>,
    /// 评估等级快照。
    pub grade_snapshot: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 提交审批。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubmitAccessReq {
    /// 申请理由（给了就覆盖草稿里的；两者都没有 → 400）。
    pub reason: Option<String>,
    /// 显式跳过审批的请求片段（**只对「没配流程」生效**；配了流程还请求跳过 → 400）。
    #[serde(flatten)]
    pub skip: crate::business::erp::approval_gate::SkipApprovalReq,
}

// ==================== 出参 ====================

/// 申请单行（列表 / 详情共用）。
#[derive(Debug, Clone, Serialize)]
pub struct AccessRowRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub access_no: String,
    /// 供应商 id。
    pub supplier_id: String,
    /// 供应商编码快照。
    pub supplier_code: String,
    /// 供应商名快照。
    pub supplier_name: String,
    /// 类型。
    pub access_type: String,
    /// 类型中文。
    pub access_type_label: String,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 申请理由。
    pub reason: Option<String>,
    /// 评估等级快照。
    pub grade_snapshot: Option<String>,
    /// 申请人。
    pub apply_by: Option<String>,
    /// 申请人姓名快照。
    pub apply_by_name: Option<String>,
    /// 审批实例 id。
    pub approval_instance_id: Option<String>,
    /// 结论人。
    pub decided_by: Option<String>,
    /// 结论人姓名快照。
    pub decided_by_name: Option<String>,
    /// 结论时间（ISO-8601）。
    pub decided_at: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（ISO-8601）。
    pub created_at: String,
    /// 供应商**现值**状态（1 启用 / 0 停用）。
    pub supplier_status: i16,
    /// 供应商现值状态中文（启用 / 停用）。
    pub supplier_status_label: String,
    /// 通过后会把供应商置成什么状态（1 启用 / 0 停用）—— 前端「生效后」预览用。
    pub target_supplier_status: i16,
    /// 草稿可改可删（前端按钮显隐用；真正的闸在服务端）。
    pub editable: bool,
    /// 可提交（草稿 + 理由够字）。
    pub submittable: bool,
}

/// 行 → DTO。
pub fn to_row_res(row: &AccessRow) -> AccessRowRes {
    let target = model::target_supplier_status(&row.access_type);
    let reason_ok = model::check_reason(row.reason.as_deref()).is_ok();
    AccessRowRes {
        id: row.id.to_string(),
        access_no: row.access_no.clone(),
        supplier_id: row.supplier_id.to_string(),
        supplier_code: row.supplier_code.clone(),
        supplier_name: row.supplier_name.clone(),
        access_type: row.access_type.clone(),
        access_type_label: model::type_label(&row.access_type).to_string(),
        status: row.status.clone(),
        status_label: model::status_label(&row.status).to_string(),
        reason: row.reason.clone(),
        grade_snapshot: row.grade_snapshot.clone(),
        apply_by: row.apply_by.map(|id| id.to_string()),
        apply_by_name: row.apply_by_name.clone(),
        approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
        decided_by: row.decided_by.map(|id| id.to_string()),
        decided_by_name: row.decided_by_name.clone(),
        decided_at: row.decided_at.map(|at| at.to_rfc3339()),
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
        supplier_status: row.supplier_status,
        supplier_status_label: if row.supplier_status == model::SUPPLIER_ENABLED {
            "启用".to_string()
        } else {
            "停用".to_string()
        },
        target_supplier_status: target,
        editable: row.status == model::STATUS_DRAFT,
        submittable: row.status == model::STATUS_DRAFT && reason_ok,
    }
}

// ==================== 解析小工具 ====================

/// 必填 uuid（空 / 非 uuid → 400）。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field}={raw} 不是合法的 UUID")))
}

/// 可选 uuid（空 → None）。
pub fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => Ok(Some(parse_uuid(text, field)?)),
    }
}

/// 类型闭集校验（大小写不敏感）。
pub fn check_access_type(raw: &str) -> std::result::Result<String, ApiError> {
    let value = raw.trim().to_ascii_uppercase();
    if ACCESS_TYPES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(bad_request(format!(
            "access_type={raw} 不在类型闭集内（{}）",
            ACCESS_TYPES.join(" / ")
        )))
    }
}

/// 理由校验（提交时用）—— 失败 → 400，报文点名要求的最小字数。
pub fn require_reason(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    model::check_reason(raw).map_err(bad_request)
}

/// 等级快照校验（只有黑名单能带）。
pub fn check_grade(access_type: &str, raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    model::check_grade(access_type, raw).map_err(bad_request)
}

/// 把 `Json<T>` 拆包（handler 与单测共用同一条解包路径）。
pub fn unpack<T>(Json(value): Json<T>) -> T {
    value
}

/// 最小字数常量透出（前端提示文案与本模块共用）。
pub const REASON_MIN_CHARS: usize = MIN_REASON_CHARS;
