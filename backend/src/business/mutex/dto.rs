//! 角色互斥的请求 / 响应 DTO（批次 B · B7）。
//!
//! # 约定
//!
//! - ID 一律下发 `String`（`uuid::Uuid` 未开 serde feature，与 `business/` 其余模块一致）；
//! - 规则的 `role_a` / `role_b` 已按 `role_code` 字典序规范化下发，**前端不要自己排序**
//!   （否则两处排序规则不一致会出现「同一条规则在列表里顺序变了」的错觉）；
//! - 违规清单里的用户带 `display_name`（`real_name` 为空回落 `username`），
//!   前端不必再判空拼接。

use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::mutex::model::{HolderRow, MutexRow, PairHit, UserBrief};

/// 规则分页查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MutexPageQuery {
    /// 页码，从 1 开始。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 只看与某角色相关的规则（两侧任一侧）。
    pub role_id: Option<String>,
    /// 角色编码 / 名称 / 理由的模糊匹配。
    pub keyword: Option<String>,
    /// 状态：1 只看生效、0 只看停用、留空都看。
    pub status: Option<i16>,
}

/// 一条互斥规则（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct MutexRes {
    /// 规则 ID。
    pub id: String,
    /// 较小一侧角色 ID。
    pub role_a_id: String,
    /// 较小一侧角色编码。
    pub role_a_code: String,
    /// 较小一侧角色名称。
    pub role_a_name: String,
    /// 较大一侧角色 ID。
    pub role_b_id: String,
    /// 较大一侧角色编码。
    pub role_b_code: String,
    /// 较大一侧角色名称。
    pub role_b_name: String,
    /// 互斥理由。
    pub reason: String,
    /// 是否生效。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 创建人。
    pub create_by: Option<String>,
}

impl MutexRes {
    /// 由库侧行投影成出参。
    pub fn from_row(row: &MutexRow) -> Self {
        Self {
            id: row.id.to_string(),
            role_a_id: row.role_a_id.to_string(),
            role_a_code: row.role_a_code.clone(),
            role_a_name: row.role_a_name.clone(),
            role_b_id: row.role_b_id.to_string(),
            role_b_code: row.role_b_code.clone(),
            role_b_name: row.role_b_name.clone(),
            reason: row.reason.clone(),
            enabled: row.enabled,
            created_at: row.created_at,
            create_by: row.create_by.map(|v| v.to_string()),
        }
    }
}

/// 新建互斥规则的请求体。
#[derive(Debug, Clone, Deserialize)]
pub struct MutexCreateReq {
    /// 角色 A（与 B 无方向语义）。
    pub role_a_id: String,
    /// 角色 B（与 A 无方向语义）。
    pub role_b_id: String,
    /// 互斥理由（必填，合规留痕）。
    pub reason: String,
}

/// 编辑互斥规则的请求体（**只能改理由与启停**；换角色对请删除后重建）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MutexUpdateReq {
    /// 新的互斥理由；`None` = 不改。
    pub reason: Option<String>,
    /// 启停：`true` 启用（恢复），`false` 停用（软删）；`None` = 不改。
    pub enabled: Option<bool>,
}

/// `?force=true` 的开关（跳过存量冲突阻断）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MutexForceQuery {
    /// 是否强制落库（默认 `false`）。
    pub force: Option<bool>,
}

/// 单用户体检的查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MutexConflictQuery {
    /// 目标用户 ID。
    pub user_id: Option<String>,
}

/// 一条被命中的互斥对（体检结果）。
#[derive(Debug, Clone, Serialize)]
pub struct MutexPairRes {
    /// 规则 ID。
    pub mutex_id: String,
    /// 互斥理由。
    pub reason: String,
    /// 较小一侧角色 ID。
    pub role_a_id: String,
    /// 较小一侧角色编码。
    pub role_a_code: String,
    /// 较小一侧角色名称。
    pub role_a_name: String,
    /// 较大一侧角色 ID。
    pub role_b_id: String,
    /// 较大一侧角色编码。
    pub role_b_code: String,
    /// 较大一侧角色名称。
    pub role_b_name: String,
}

impl MutexPairRes {
    /// 由一次命中投影成出参。
    pub fn from_hit(hit: &PairHit) -> Self {
        Self {
            mutex_id: hit.mutex_id.to_string(),
            reason: hit.reason.clone(),
            role_a_id: hit.a.id.to_string(),
            role_a_code: hit.a.code.clone(),
            role_a_name: hit.a.name.clone(),
            role_b_id: hit.b.id.to_string(),
            role_b_code: hit.b.code.clone(),
            role_b_name: hit.b.name.clone(),
        }
    }
}

/// 违规用户名（体检 / 扫描清单的一条）。
#[derive(Debug, Clone, Serialize)]
pub struct MutexHolderRes {
    /// 用户 ID。
    pub user_id: String,
    /// 登录名。
    pub username: String,
    /// 展示名（`real_name` 为空时等于 `username`）。
    pub display_name: String,
    /// 部门名称；无部门为空串。
    pub dept_name: String,
}

impl MutexHolderRes {
    /// 由库侧行投影成出参（可空部门降级为空串，前端不判 `null`）。
    pub fn from_row(row: &HolderRow) -> Self {
        Self {
            user_id: row.user_id.to_string(),
            username: row.username.clone(),
            display_name: row.display_name.clone(),
            dept_name: row.dept_name.clone().unwrap_or_default(),
        }
    }
}

/// 单用户互斥体检结果。
#[derive(Debug, Clone, Serialize)]
pub struct MutexConflictRes {
    /// 用户 ID。
    pub user_id: String,
    /// 登录名。
    pub username: String,
    /// 展示名。
    pub display_name: String,
    /// 该用户当前已违规的互斥对（空数组 = 未违规）。
    pub conflicts: Vec<MutexPairRes>,
}

impl MutexConflictRes {
    /// 组装体检结果。
    pub fn new(user: &UserBrief, hits: &[PairHit]) -> Self {
        Self {
            user_id: user.user_id.to_string(),
            username: user.username.clone(),
            display_name: user.display_name.clone(),
            conflicts: hits.iter().map(MutexPairRes::from_hit).collect(),
        }
    }
}

/// 全量扫描里的一组违规（某条互斥规则 + 双持用户）。
#[derive(Debug, Clone, Serialize)]
pub struct MutexScanItemRes {
    /// 规则 ID。
    pub mutex_id: String,
    /// 互斥理由。
    pub reason: String,
    /// 较小一侧角色编码。
    pub role_a_code: String,
    /// 较小一侧角色名称。
    pub role_a_name: String,
    /// 较大一侧角色编码。
    pub role_b_code: String,
    /// 较大一侧角色名称。
    pub role_b_name: String,
    /// 违规用户数。
    pub total: u64,
    /// 违规用户样本（最多 [`crate::business::mutex::checker::SAMPLE_LIMIT`] 条）。
    pub sample: Vec<MutexHolderRes>,
}

/// 全量存量扫描结果。
///
/// # 字段口径
///
/// - `scanned_pairs`：本次扫描的生效规则数；
/// - `violated_pairs`：其中存在双持用户的规则数（= `items.len()`）；
/// - `violated_holders`：违规**人次**（同一用户命中两条规则会被计两次，看人次比看人数更
///   贴近「要处理多少个绑定」）。
#[derive(Debug, Clone, Serialize)]
pub struct MutexScanRes {
    /// 扫描的生效规则数。
    pub scanned_pairs: u64,
    /// 违规规则数。
    pub violated_pairs: u64,
    /// 违规人次。
    pub violated_holders: u64,
    /// 违规明细（按规则）。
    pub items: Vec<MutexScanItemRes>,
}
