//! AI 用户（云枢 × Hermes）的请求与响应 DTO。
//!
//! 命名与前端契约 `frontend/src/api/system/ai-user-api.ts` 逐字对齐（snake_case 直出）。
//!
//! # 与 `system/user/dto.rs` 的关系
//!
//! 人类用户的 DTO（`UserRes` / `UserCreateReq` …）不动；AI 用户走独立的一套：
//! - 创建时**不要密码**（机器人不登录，口令由服务端随机生成且永不对外）；
//! - 创建时**要 SOUL.md 与职责**（这是「人格」，人类用户没有）；
//! - 详情里带 `soul_path` / `workspace_path`，前端「保存前提示」需要真实路径。

use nik::prelude::*;
// 显式遮蔽 prelude 的 `Result<T>`（错误类型固定为 `nik::Error`）；本模块统一返回 `ApiError`
use serde::{Deserialize, Serialize};
use std::result::Result;

use crate::business::aiagent::hermes::ProfileStatus;

/// 创建 AI 用户请求（`POST /api/system/user/ai`）。
#[derive(Debug, Clone, Deserialize)]
pub struct AiUserCreateReq {
    /// 用户名 = Hermes profile 名（`^[a-z][a-z0-9-]{1,30}$`，同时是登录账号）。
    pub username: String,
    /// 显示名（进 `sys_user.real_name`，也用于渲染默认 SOUL.md）。
    #[serde(default)]
    pub real_name: Option<String>,
    /// 职责（一句话，进 `sys_ai_agent.agent_description` + `hermes profile describe`）。
    #[serde(default)]
    pub agent_description: Option<String>,
    /// SOUL.md 全文（Markdown）。留空则用模板生成默认结构。
    #[serde(default)]
    pub soul_md: Option<String>,
    /// 模型覆盖（任意 JSON；`null` / 缺省表示用 profile 自身 `config.yaml` 的模型）。
    #[serde(default)]
    pub model_override: Option<serde_json::Value>,
    /// 克隆模板 profile 名（缺省用配置里的 `CLOUDPIVOT_AIAGENT_CLONE_FROM`）。
    #[serde(default)]
    pub clone_from: Option<String>,
}

/// 编辑 AI 用户请求（`PUT /api/system/user/ai/{user_id}`）。
///
/// 三个字段都是 `Option`：`None` = 不改（区别于 `Some("")` = 清空）。
///
/// 显示名不在此列：它属于 `sys_user` 的业务字段，走既有的 `PUT /system/user/{id}`，
/// 避免同一字段出现两条更新路径（两条路径必然有一天漂移）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AiUserUpdateReq {
    /// 职责（给了就同步 `hermes profile describe`）。
    #[serde(default)]
    pub agent_description: Option<String>,
    /// SOUL.md 全文（给了就原子写 `<profiles_root>/<name>/SOUL.md`）。
    #[serde(default)]
    pub soul_md: Option<String>,
    /// 模型覆盖；`Some(Value::Null)` 表示清空。
    #[serde(default)]
    pub model_override: Option<serde_json::Value>,
}

/// AI 用户响应（创建 / 编辑 / 详情共用）。
#[derive(Debug, Clone, Serialize)]
pub struct AiUserRes {
    /// `sys_user.id`。
    pub user_id: String,
    /// 登录账号 = profile 名。
    pub username: String,
    /// 显示名（`sys_user.real_name`）。
    pub real_name: String,
    /// 账号类型，恒为 `ROBOT`（前端据此显示机器人徽标）。
    pub user_type: String,
    /// Hermes profile 名（当前恒等于 `username`，保留字段以便将来解耦）。
    pub hermes_profile: String,
    /// 工作区绝对路径（`<workspace_root>/<name>/`）。
    pub workspace_path: String,
    /// SOUL.md 绝对路径（前端「保存前提示」直接展示它）。
    pub soul_path: String,
    /// 职责。
    pub agent_description: String,
    /// SOUL.md 全文。
    pub soul_md: String,
    /// 模型覆盖（原样 JSON）。
    pub model_override: Option<serde_json::Value>,
    /// 上次探活记录的 gateway 状态：`unknown` / `online` / `offline`。
    pub gateway_status: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// AI 用户状态响应（`GET /api/system/user/ai/{user_id}/status`）。
#[derive(Debug, Clone, Serialize)]
pub struct AiUserStatusRes {
    /// `sys_user.id`。
    pub user_id: String,
    /// profile 名。
    pub hermes_profile: String,
    /// profile 目录是否存在。
    pub profile_dir_exists: bool,
    /// SOUL.md 是否存在。
    pub soul_file_exists: bool,
    /// workspace 目录是否存在。
    pub workspace_exists: bool,
    /// CLI 是否可用。
    pub cli_available: bool,
    /// gateway 状态（本次探测值）：`online` / `offline` / `unknown`。
    pub gateway: String,
    /// 库里记录的 gateway 状态（上次探测值）。
    pub gateway_status: String,
    /// 整体健康（目录 + SOUL.md + CLI 都在）。
    pub healthy: bool,
    /// 凭据是否可用于认证（profile 级 `.env` 有当前 provider 的 key 且权限 600）。
    ///
    /// **这是「能不能干活」的判据**：`healthy=true` 但 `credentials_ok=false` 的账号，
    /// 派活会 200、worker 一执行就 `AuthError`（2026-10-29 的真实故障形态）。
    pub credentials_ok: bool,
    /// 凭据文件里非空的**键名**（值永不出后端）。
    pub credential_keys: Vec<String>,
    /// 当前 provider 需要的键名。
    pub credential_required: Option<String>,
    /// 凭据文件权限位（`384` = `0o600`；文件不存在为 `null`）。
    pub env_file_mode: Option<u32>,
    /// 凭据自检的补充说明（缺哪个键 / 权限不对）。
    pub credential_detail: Option<String>,
    /// 探测过程中的补充说明（命令失败原因等）。
    pub detail: Option<String>,
}

impl AiUserStatusRes {
    /// 由库里的行与本次探测结果组装。
    pub fn new(user_id: &uuid::Uuid, hermes_profile: &str, db_gateway_status: &str, status: &ProfileStatus) -> Self {
        Self {
            user_id: user_id.to_string(),
            hermes_profile: hermes_profile.to_string(),
            profile_dir_exists: status.profile_dir_exists,
            soul_file_exists: status.soul_file_exists,
            workspace_exists: status.workspace_exists,
            cli_available: status.cli_available,
            gateway: status.gateway.clone(),
            gateway_status: db_gateway_status.to_string(),
            healthy: status.healthy(),
            credentials_ok: status.credentials_ok,
            credential_keys: status.credential_keys.clone(),
            credential_required: status.credential_required.clone(),
            env_file_mode: status.env_file_mode,
            credential_detail: status.credential_detail.clone(),
            detail: status.detail.clone(),
        }
    }
}

/// 凭据检查 / 修复响应（`POST /api/system/user/ai/{user_id}/repair`）。
///
/// 与 [`AiUserStatusRes`] 的差别：这里是**修复动作**的结果（`repaired_keys` 说明补了哪些键），
/// 而 status 是「现在什么状态」的只读探活。
#[derive(Debug, Clone, Serialize)]
pub struct AiUserCredentialRes {
    /// `sys_user.id`。
    pub user_id: String,
    /// profile 名。
    pub hermes_profile: String,
    /// profile 凭据文件绝对路径。
    pub env_path: String,
    /// 权限位（`384` = `0o600`）。
    pub env_file_mode: Option<u32>,
    /// 当前 provider。
    pub provider: Option<String>,
    /// 当前 provider 需要的键名。
    pub required_key: Option<String>,
    /// 修复后凭据文件里非空的键名。
    pub present_keys: Vec<String>,
    /// 修复后是否可用（缺键 / 权限不对 → `false`）。
    pub credentials_ok: bool,
    /// 本次**实际写入**的键名（空数组 = 原本就齐）。
    pub repaired_keys: Vec<String>,
    /// 不可用时的原因。
    pub detail: Option<String>,
}

/// 删除 AI 用户响应（`DELETE /api/system/user/ai/{user_id}`）。
///
/// 回传备份路径是**给运维看的凭证**：删号后要能找回人格内容。
#[derive(Debug, Clone, Serialize)]
pub struct AiUserDeleteRes {
    /// 被删除的 `sys_user.id`。
    pub user_id: String,
    /// 被删除的 profile 名。
    pub hermes_profile: String,
    /// SOUL.md 备份文件绝对路径（SOUL.md 不存在时为 `null`）。
    pub soul_backup_path: Option<String>,
    /// profile 目录是否已确认消失。
    pub profile_removed: bool,
    /// workspace 目录是否已确认消失。
    pub workspace_removed: bool,
}

/// 把请求里的「职责」规整成单行文本。
///
/// # 规则
///
/// - 去掉首尾空白，内部换行压成空格（职责要进 `hermes profile describe`，多行会在
///   表格化输出里断行）；
/// - 超 [`DESCRIPTION_MAX_CHARS`](crate::business::aiagent::hermes::DESCRIPTION_MAX_CHARS)
///   返回 `400 + AI_AGENT_DESCRIPTION_INVALID`。
///
/// # 异常情况
///
/// 长度超限时返回 400。
pub fn normalize_description(raw: Option<&str>) -> Result<String, ApiError> {
    let Some(raw) = raw else {
        return Ok(String::new());
    };
    let flattened = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let max = crate::business::aiagent::hermes::DESCRIPTION_MAX_CHARS;
    if flattened.chars().count() > max {
        return Err(ApiError::new(
            400,
            crate::shared::error_code::AI_AGENT_DESCRIPTION_INVALID,
            format!("职责最多 {max} 个字符（当前 {}）", flattened.chars().count()),
        ));
    }
    Ok(flattened)
}
