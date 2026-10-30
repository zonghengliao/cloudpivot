//! `sys_ai_agent` 的强类型行与数据访问函数。
//!
//! 对应迁移 `V0105__create_sys_ai_agent`（卡面写 V0092，编号已推进，见迁移头注释）。
//!
//! # 为什么手写 SQL 而不 `#[derive(Model)]`
//!
//! 表里有一列 `model_override jsonb`：
//!
//! - **写**：jsonb 列必须绑 `Value::Json(字符串)`；绑 `Value::Text` 会得到
//!   `error serializing parameter N`（批次 A 的既有教训，见 cloudpivot 后端**踩坑表**）；
//! - **读**：`jsonb` 直接 `get_typed::<String>` 类型不匹配，必须在 SQL 里 `::text` 转换。
//!
//! 这两条都超出 derive 宏的默认映射范围，与其给模型加一堆特例，不如把这一张表的 SQL
//! 写清楚（本文件只有 3 个写语句 + 3 个读语句）。
//!
//! # 软删与唯一性
//!
//! `deleted_at IS NULL` 是有效期判据；`hermes_profile` 的唯一索引是**部分索引**
//! （`WHERE deleted_at IS NULL`），因此「删掉同名 AI 用户后重建同名」不会被唯一约束挡住
//! —— 与 `sys_user.username` 的口径一致。

use nik::prelude::*;
use nik::uuid::Uuid;

/// `sys_ai_agent` 一行。
#[derive(Debug, Clone)]
pub struct AiAgentRow {
    /// `sys_user.id`（主键，外键指向 `sys_user`，`ON DELETE CASCADE`）。
    pub user_id: Uuid,
    /// Hermes profile 名（部分唯一索引）。
    pub hermes_profile: String,
    /// 工作区绝对路径。
    pub workspace_path: String,
    /// SOUL.md 全文（库内副本；写入磁盘成功后才落库，见 handler 的写入顺序）。
    pub soul_md: String,
    /// 职责。
    pub agent_description: Option<String>,
    /// 模型覆盖（已解析的 JSON）。
    pub model_override: Option<serde_json::Value>,
    /// 上次探活的 gateway 状态：`unknown` / `online` / `offline`。
    pub gateway_status: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// `sys_user` 里与 AI 用户相关的最小投影。
#[derive(Debug, Clone)]
pub struct AiUserBrief {
    /// 登录账号。
    pub username: String,
    /// 显示名（未填为 `None`）。
    pub real_name: Option<String>,
    /// 账号类型（`HUMAN` / `ROBOT`）。
    pub user_type: String,
}

const ROW_COLUMNS: &str = "user_id, hermes_profile, workspace_path, soul_md, agent_description, \
     model_override::text AS model_override, gateway_status, created_at, updated_at";

// ==================== 读 ====================

/// 按 `user_id` 取有效的 AI 用户行（软删的不算）。
///
/// # 返回值
///
/// 不存在或已软删返回 `None`。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn find_by_user(user_id: &Uuid) -> DbResult<Option<AiAgentRow>> {
    let sql =
        format!("SELECT {ROW_COLUMNS} FROM sys_ai_agent WHERE user_id = ? AND deleted_at IS NULL");
    let rows = db()
        .find_with_params(&sql, vec![Value::from(user_id)])
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(row_to_agent(row)?)),
        None => Ok(None),
    }
}

/// profile 名是否已被占用（软删的不算）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn profile_exists(hermes_profile: &str) -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM sys_ai_agent WHERE hermes_profile = ? AND deleted_at IS NULL",
            vec![Value::from(hermes_profile)],
        )
        .await?;
    Ok(count > 0)
}

/// 取用户的账号密码投影（账号 / 显示名 / 账号类型）。
///
/// # 返回值
///
/// 不存在或已软删返回 `None`。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
// SCOPE-EXEMPT: AI 用户管理只按主键取单个账号的类型/显示名，不做列表与范围过滤；
// 调用方已通过 `system:user:*` 权限码与 `sys_ai_agent` 归属校验（本模块无列表接口）。
pub async fn find_user_brief(user_id: &Uuid) -> DbResult<Option<AiUserBrief>> {
    let rows = db()
        .find_with_params(
            "SELECT username, real_name, user_type FROM sys_user \
             WHERE id = ? AND deleted_at IS NULL",
            vec![Value::from(user_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(AiUserBrief {
        username: row.get_typed::<String>("username")?,
        real_name: row.get_typed::<Option<String>>("real_name")?,
        user_type: row.get_typed::<String>("user_type")?,
    }))
}

fn row_to_agent(row: &Row) -> DbResult<AiAgentRow> {
    let model_text: Option<String> = row.get_typed::<Option<String>>("model_override")?;
    let model_override = match model_text {
        Some(text) => Some(serde_json::from_str(&text).map_err(|e| {
            DbError::InvalidValue(format!("sys_ai_agent.model_override 不是合法 JSON：{e}"))
        })?),
        None => None,
    };
    Ok(AiAgentRow {
        user_id: row.get_typed::<Uuid>("user_id")?,
        hermes_profile: row.get_typed::<String>("hermes_profile")?,
        workspace_path: row.get_typed::<String>("workspace_path")?,
        soul_md: row.get_typed::<String>("soul_md")?,
        agent_description: row.get_typed::<Option<String>>("agent_description")?,
        model_override,
        gateway_status: row.get_typed::<String>("gateway_status")?,
        created_at: row.get_typed::<chrono::DateTime<chrono::Utc>>("created_at")?,
        updated_at: row.get_typed::<chrono::DateTime<chrono::Utc>>("updated_at")?,
    })
}

// ==================== 写 ====================

/// 插入 AI 用户行。
///
/// # 参数
///
/// - `ex`：执行器（连接或事务，便于与 `sys_user` 的改动同事务）；
/// - `row`：待插入行。
///
/// # 异常情况
///
/// 唯一索引冲突或数据库异常返回 [`DbError`]。
pub async fn insert(ex: &impl SqlExecutor, row: &AiAgentRow) -> DbResult<()> {
    let prefix = "INSERT INTO sys_ai_agent \
         (user_id, hermes_profile, workspace_path, soul_md, agent_description, model_override, \
          gateway_status, created_at, updated_at, deleted_at) VALUES (?, ?, ?, ?, ?, ";
    let suffix = ", ?, now(), now(), NULL)";
    let mut params = vec![
        Value::from(&row.user_id),
        Value::from(row.hermes_profile.clone()),
        Value::from(row.workspace_path.clone()),
        Value::from(row.soul_md.clone()),
    ];
    params.push(match &row.agent_description {
        Some(desc) => Value::from(desc.clone()),
        None => Value::Null,
    });
    let sql = match &row.model_override {
        Some(model) => {
            // jsonb 必须绑 Value::Json（内含合法 JSON 文本），否则驱动编码不上
            params.push(Value::Json(model.to_string()));
            format!("{prefix}?::jsonb{suffix}")
        }
        None => {
            // 无值时写 NULL 字面量：`Value::Null` 绑 `?::jsonb` 在 PG 侧类型推断失败
            format!("{prefix}NULL{suffix}")
        }
    };
    params.push(Value::from(row.gateway_status.clone()));
    ex.exec_sql(sql, params).await?;
    Ok(())
}

/// 覆盖写「可编辑三件套」+ gateway 状态。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `row`：已经合并好新值的整行（调用方从库里读出后改字段再传入，避免「部分更新」
///   把没传的字段写成 NULL）。
///
/// # 返回值
///
/// 受影响行数（0 表示行不存在或已软删）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn update_content(ex: &impl SqlExecutor, row: &AiAgentRow) -> DbResult<u64> {
    let mut params = vec![
        match &row.agent_description {
            Some(desc) => Value::from(desc.clone()),
            None => Value::Null,
        },
        Value::from(row.soul_md.clone()),
    ];
    let (model_placeholder, sql_tail) = match &row.model_override {
        Some(model) => {
            params.push(Value::Json(model.to_string()));
            (
                "?::jsonb",
                ", updated_at = now() WHERE user_id = ? AND deleted_at IS NULL",
            )
        }
        None => (
            "NULL",
            ", updated_at = now() WHERE user_id = ? AND deleted_at IS NULL",
        ),
    };
    params.push(Value::from(row.gateway_status.clone()));
    params.push(Value::from(&row.user_id));
    let sql = format!(
        "UPDATE sys_ai_agent SET agent_description = ?, soul_md = ?, \
         model_override = {model_placeholder}, gateway_status = ?{sql_tail}"
    );
    ex.exec_sql(sql, params).await
}

/// 只更新 gateway 状态（探活后回写，避免每次都跑一遍全字段更新）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn update_gateway_status(
    ex: &impl SqlExecutor,
    user_id: &Uuid,
    gateway_status: &str,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE sys_ai_agent SET gateway_status = ?, updated_at = now() \
         WHERE user_id = ? AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(gateway_status), Value::from(user_id)],
    )
    .await
}

/// 软删 AI 用户行。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn soft_delete(ex: &impl SqlExecutor, user_id: &Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE sys_ai_agent SET deleted_at = now(), updated_at = now() \
         WHERE user_id = ? AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(user_id)],
    )
    .await
}

/// 把账号类型改写为 `ROBOT`（建号后立刻打标，与 `sys_user` 插入同事务）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
// SCOPE-EXEMPT: 按主键改单个账号的类型标记，不涉及范围过滤（写入方已在 handler 里
// 通过 `system:user:create` 权限码与「新建行」语义限定）。
pub async fn mark_user_robot(ex: &impl SqlExecutor, user_id: &Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE sys_user SET user_type = 'ROBOT', updated_at = now() \
         WHERE id = ? AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(user_id)],
    )
    .await
}
