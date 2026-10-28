//! `sys_field_mask` 的库侧读写（批次 B · B6）。
//!
//! # 文件边界
//!
//! 只做「取行 / 写行」，不含策略：规则的合并口径在 [`crate::business::masking::registry`]，
//! 七种类型的算法在 [`crate::business::masking::masker`]。于是策略与算法都能在没有数据库的
//! 环境里逐条断言，查库部分只剩 SQL 形态。
//!
//! # 两类查询，两种语义（架构 §3.1）
//!
//! | 用途 | 函数 | 规则集合 |
//! |------|------|----------|
//! | 出参脱敏（给某个用户算规则） | [`rules_for_user`] | 全局规则 ∪ **该用户持有的、未删除未停用角色**的角色规则 |
//! | 配置管理（列表 / 详情） | [`page_rules`] / [`find_rule`] | 全部未删除规则（管理面看全量） |
//!
//! **角色软删后规则不生效**（不是"变全局"）：[`rules_for_user`] 的 `EXISTS` 子查询里带了
//! `rr.deleted_at IS NULL AND rr.status = 1`。静默放宽脱敏是最危险的一类缺陷，故这里
//! 宁可写成显式条件，也不靠 `LEFT JOIN` 的 `NULL` 语义。
//!
//! # 为什么不用 `paginate_with_params`
//!
//! 批次 B 的「闸①」门禁要求分页只能出现在 `business/shared/scope.rs`。`sys_field_mask`
//! 不在数据权限登记表里（它是配置表，不是业务数据），不能走 `scope::paginate`，
//! 因此这里手写 `COUNT(*)` + `LIMIT/OFFSET`——门禁与语义都对。
//!
//! # 数据权限
//!
//! SCOPE-EXEMPT: 本文件对 `sys_user` / `sys_user_role` 的引用只出现在
//! 「按 user_id 单条查用户」与「判断某用户是否持有某角色」两类点查里（`WHERE user_id = ?`），
//! 不构成面向用户的列表查询；行级过滤由调用方（`/system/field-mask/effective` 的
//! `system:field-mask:list` 权限 + 单条 user_id 参数）收口。

use alun::prelude::*;
use alun::uuid::Uuid;

use crate::business::masking::dto::FieldMaskPageQuery;
use crate::shared::constants;
use crate::utils;

/// `sys_field_mask` 的一行（含 join 出来的角色名）。
#[derive(Debug, Clone)]
pub struct FieldMaskRow {
    /// 主键。
    pub id: Uuid,
    /// 表名。
    pub table_name: String,
    /// 列名。
    pub column_name: String,
    /// 脱敏类型（1..7）。
    pub mask_type: i16,
    /// 类型参数。
    pub mask_param: Option<String>,
    /// 适用角色（`None` = 全局）。
    pub role_id: Option<Uuid>,
    /// 适用角色名（`role_id` 为空或角色已删时为空）。
    pub role_name: Option<String>,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 脱敏理由。
    pub reason: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 新建规则的入参。
pub struct NewFieldMaskRule<'a> {
    /// 表名。
    pub table_name: &'a str,
    /// 列名。
    pub column_name: &'a str,
    /// 脱敏类型。
    pub mask_type: i16,
    /// 类型参数（`None` / 空串都落 `NULL`，读取时按类型回填缺省值）。
    pub mask_param: Option<&'a str>,
    /// 适用角色（`None` = 全局）。
    pub role_id: Option<Uuid>,
    /// 状态。
    pub status: i16,
    /// 脱敏理由。
    pub reason: &'a str,
    /// 操作者。
    pub operator: Uuid,
}

/// 修改规则的入参。
pub struct UpdateFieldMaskRule<'a> {
    /// 脱敏类型。
    pub mask_type: i16,
    /// 类型参数。
    pub mask_param: Option<&'a str>,
    /// 状态。
    pub status: i16,
    /// 脱敏理由。
    pub reason: &'a str,
    /// 操作者。
    pub operator: Uuid,
}

/// 角色简要（校验 + 回显用）。
#[derive(Debug, Clone)]
pub struct RoleBrief {
    /// 角色 ID。
    pub id: Uuid,
    /// 角色编码。
    pub role_code: String,
    /// 角色名称。
    pub role_name: String,
}

/// 用户简要（自检接口用）。
#[derive(Debug, Clone)]
pub struct UserBrief {
    /// 用户 ID。
    pub id: Uuid,
    /// 用户名。
    pub username: String,
    /// 真实姓名。
    pub real_name: Option<String>,
}

// ==================== 配置面：分页 / 详情 ====================

/// 规则分页（管理面：看得到全部未删除规则）。
///
/// # 参数
///
/// - `query`：过滤条件（表名 / 列名 / 作用域 / 状态）；
/// - `page` / `page_size`：页码与每页条数（已规范化）。
///
/// # 返回值
///
/// `(本页规则, 总条数)`。
pub async fn page_rules(
    query: &FieldMaskPageQuery,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<FieldMaskRow>, i64)> {
    let mut where_sql = String::from(" WHERE m.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(table_name) = utils::trimmed(query.table_name.as_deref()) {
        where_sql.push_str(" AND m.table_name = ?");
        params.push(Value::from(table_name.to_string()));
    }
    if let Some(column_name) = utils::trimmed(query.column_name.as_deref()) {
        where_sql.push_str(" AND m.column_name = ?");
        params.push(Value::from(column_name.to_string()));
    }
    match query.scope.as_deref().map(str::trim) {
        Some("global") => where_sql.push_str(" AND m.role_id IS NULL"),
        Some("role") => where_sql.push_str(" AND m.role_id IS NOT NULL"),
        _ => {}
    }
    if let Some(status) = query.status {
        where_sql.push_str(" AND m.status = ?");
        params.push(Value::I16(status));
    }

    let count_sql = format!("SELECT COUNT(*) FROM sys_field_mask m{where_sql}");
    let total: i64 = db().scalar_with_params(&count_sql, params.clone()).await?;

    let offset =
        i64::try_from(page.saturating_sub(1).saturating_mul(page_size)).unwrap_or(i64::MAX);
    let limit = i64::try_from(page_size).unwrap_or(i64::MAX);
    let list_sql = format!(
        "{} {where_sql} ORDER BY m.table_name ASC, m.column_name ASC, m.role_id NULLS FIRST, \
         m.created_at ASC, m.id ASC LIMIT ? OFFSET ?",
        select_sql()
    );
    params.push(Value::I64(limit));
    params.push(Value::I64(offset));
    let rows = db().find_with_params(&list_sql, params).await?;
    let list = rows
        .iter()
        .map(map_row)
        .collect::<std::result::Result<Vec<_>, DbError>>()?;
    Ok((list, total))
}

/// 按 ID 取一条规则（含已停用，不含已软删）。
///
/// # 参数
///
/// - `id`：规则 ID。
pub async fn find_rule(id: &Uuid) -> DbResult<Option<FieldMaskRow>> {
    let sql = format!("{} WHERE m.id = ? AND m.deleted_at IS NULL", select_sql());
    let row = db()
        .find_first_with_params(&sql, vec![Value::from(*id)])
        .await?;
    row.as_ref().map(map_row).transpose()
}

/// `select` 子句：规则列 + 角色名（角色已删时角色名为 `NULL`，由合并侧决定是否生效）。
fn select_sql() -> &'static str {
    "SELECT m.id, m.table_name, m.column_name, m.mask_type, m.mask_param, m.role_id, \
     m.status, m.reason, m.created_at, m.updated_at, r.role_name AS role_name \
     FROM sys_field_mask m LEFT JOIN sys_role r ON r.id = m.role_id"
}

/// 行映射。
fn map_row(row: &Row) -> std::result::Result<FieldMaskRow, DbError> {
    Ok(FieldMaskRow {
        id: row.get_typed("id")?,
        table_name: row.get_typed("table_name")?,
        column_name: row.get_typed("column_name")?,
        mask_type: row.get_typed("mask_type")?,
        mask_param: row.get_typed("mask_param")?,
        role_id: row.get_typed("role_id")?,
        role_name: row.get_typed("role_name")?,
        status: row.get_typed("status")?,
        reason: row.get_typed("reason")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
    })
}

// ==================== 写：增 / 改 / 删 ====================

/// 新建规则。
///
/// # 参数
///
/// - `rule`：入参。
///
/// # 异常情况
///
/// 唯一索引 `uk_sys_field_mask_rule` 冲突 → `DbError`（调用方转 409）。
pub async fn insert_rule(rule: &NewFieldMaskRule<'_>) -> DbResult<()> {
    // `role_id` 为空时**不能**绑 `Value::Null` 到 `?::uuid`（驱动无法推断类型，
    // 见 `docs/后端二开指南.md` 的参数绑定坑）：改用第二条常量 SQL 直接写 `NULL`。
    let mask_param = rule.mask_param.map(str::to_string);
    match rule.role_id {
        Some(role_id) => {
            db().execute_with_params(
                "INSERT INTO sys_field_mask \
                 (table_name, column_name, mask_type, mask_param, role_id, status, reason, create_by) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                vec![
                    Value::from(rule.table_name.to_string()),
                    Value::from(rule.column_name.to_string()),
                    Value::I16(rule.mask_type),
                    opt_text(mask_param),
                    Value::from(role_id),
                    Value::I16(rule.status),
                    Value::from(rule.reason.to_string()),
                    Value::from(rule.operator),
                ],
            )
            .await?;
        }
        None => {
            db().execute_with_params(
                "INSERT INTO sys_field_mask \
                 (table_name, column_name, mask_type, mask_param, role_id, status, reason, create_by) \
                 VALUES (?, ?, ?, ?, NULL, ?, ?, ?)",
                vec![
                    Value::from(rule.table_name.to_string()),
                    Value::from(rule.column_name.to_string()),
                    Value::I16(rule.mask_type),
                    opt_text(mask_param),
                    Value::I16(rule.status),
                    Value::from(rule.reason.to_string()),
                    Value::from(rule.operator),
                ],
            )
            .await?;
        }
    }
    Ok(())
}

/// 修改规则（表名 / 列名 / 角色不可改）。
///
/// # 参数
///
/// - `id`：规则 ID；
/// - `rule`：新值。
///
/// # 返回值
///
/// 受影响行数（0 = 不存在或已软删）。
pub async fn update_rule(id: &Uuid, rule: &UpdateFieldMaskRule<'_>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_field_mask SET mask_type = ?, mask_param = ?, status = ?, reason = ?, \
         updated_at = CURRENT_TIMESTAMP, update_by = ? \
         WHERE id = ? AND deleted_at IS NULL",
        vec![
            Value::I16(rule.mask_type),
            opt_text(rule.mask_param.map(str::to_string)),
            Value::I16(rule.status),
            Value::from(rule.reason.to_string()),
            Value::from(rule.operator),
            Value::from(*id),
        ],
    )
    .await
}

/// 软删规则。
///
/// # 参数
///
/// - `id`：规则 ID；
/// - `operator`：操作者。
///
/// # 返回值
///
/// 受影响行数（0 = 不存在或已软删）。
pub async fn soft_delete_rule(id: &Uuid, operator: &Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_field_mask SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP, \
         update_by = ? WHERE id = ? AND deleted_at IS NULL",
        vec![Value::from(*operator), Value::from(*id)],
    )
    .await
}

// ==================== 出参面：某用户实际生效的规则 ====================

/// 出参脱敏用的原始规则行（合并前的形态）。
#[derive(Debug, Clone)]
pub struct RawRule {
    /// 规则 ID。
    pub id: Uuid,
    /// 表名。
    pub table_name: String,
    /// 列名。
    pub column_name: String,
    /// 脱敏类型（1..7）。
    pub mask_type: i16,
    /// 类型参数。
    pub mask_param: Option<String>,
    /// 适用角色（`None` = 全局）。
    pub role_id: Option<Uuid>,
    /// 适用角色名。
    pub role_name: Option<String>,
    /// 创建时间（合并时的同层排序键）。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 某用户实际生效的规则（全局 ∪ 该用户持有角色；角色规则优先）。
///
/// # 参数
///
/// - `user_id`：用户 ID。
///
/// # 返回值
///
/// 未合并的原始规则行；合并口径见
/// [`crate::business::masking::registry::pick_effective`]。
pub async fn rules_for_user(user_id: &Uuid) -> DbResult<Vec<RawRule>> {
    let rows = db()
        .find_with_params(
            "SELECT m.id, m.table_name, m.column_name, m.mask_type, m.mask_param, m.role_id, \
             r.role_name AS role_name, m.created_at \
             FROM sys_field_mask m LEFT JOIN sys_role r ON r.id = m.role_id \
             WHERE m.deleted_at IS NULL AND m.status = 1 \
               AND (m.role_id IS NULL OR EXISTS ( \
                     SELECT 1 FROM sys_user_role ur \
                     JOIN sys_role rr ON rr.id = ur.role_id \
                     WHERE ur.user_id = ?::uuid AND ur.role_id = m.role_id \
                       AND rr.deleted_at IS NULL AND rr.status = 1)) \
             ORDER BY m.table_name ASC, m.column_name ASC, (m.role_id IS NULL) ASC, \
                      m.created_at ASC, m.id ASC",
            vec![Value::from(*user_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(RawRule {
                id: row.get_typed("id")?,
                table_name: row.get_typed("table_name")?,
                column_name: row.get_typed("column_name")?,
                mask_type: row.get_typed("mask_type")?,
                mask_param: row.get_typed("mask_param")?,
                role_id: row.get_typed("role_id")?,
                role_name: row.get_typed("role_name")?,
                created_at: row.get_typed("created_at")?,
            })
        })
        .collect()
}

// ==================== 校验辅助 ====================

/// 表 / 列是否真实存在于 `information_schema`（防止规则指向不存在的列，静默不生效）。
///
/// # 参数
///
/// - `table_name`：表名；
/// - `column_name`：列名。
pub async fn column_exists(table_name: &str, column_name: &str) -> DbResult<bool> {
    let row = db()
        .find_first_with_params(
            "SELECT 1 AS hit FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = ? AND column_name = ? \
             LIMIT 1",
            vec![
                Value::from(table_name.to_string()),
                Value::from(column_name.to_string()),
            ],
        )
        .await?;
    Ok(row.is_some())
}

/// 按 ID 取未删除角色（用来填规则的角色名 / 校验角色是否可用）。
///
/// # 参数
///
/// - `role_id`：角色 ID。
pub async fn find_role(role_id: &Uuid) -> DbResult<Option<RoleBrief>> {
    let row = db()
        .find_first_with_params(
            "SELECT id, role_code, role_name FROM sys_role WHERE id = ? AND deleted_at IS NULL",
            vec![Value::from(*role_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(RoleBrief {
        id: row.get_typed("id")?,
        role_code: row.get_typed("role_code")?,
        role_name: row.get_typed("role_name")?,
    }))
}

/// 按 ID 取未删除用户（自检接口用）。
///
/// # 参数
///
/// - `user_id`：用户 ID。
pub async fn find_user_brief(user_id: &Uuid) -> DbResult<Option<UserBrief>> {
    let row = db()
        .find_first_with_params(
            "SELECT id, username, real_name FROM sys_user WHERE id = ? AND deleted_at IS NULL",
            vec![Value::from(*user_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(UserBrief {
        id: row.get_typed("id")?,
        username: row.get_typed("username")?,
        real_name: row.get_typed("real_name")?,
    }))
}

/// 该用户是否持有 `SUPER_ADMIN`（库侧判定，用于自检接口展示豁免原因）。
///
/// # 参数
///
/// - `user_id`：用户 ID。
pub async fn is_super_admin(user_id: &Uuid) -> DbResult<bool> {
    let row = db()
        .find_first_with_params(
            "SELECT 1 AS hit FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id \
             WHERE ur.user_id = ? AND r.role_code = ? AND r.deleted_at IS NULL \
             LIMIT 1",
            vec![
                Value::from(*user_id),
                Value::from(constants::SUPER_ADMIN_ROLE_CODE),
            ],
        )
        .await?;
    Ok(row.is_some())
}

// ==================== 明文访问留痕 ====================

/// 写一条「持 `system:field-mask:plain` 看了明文」的审计记录（架构 §3.6）。
///
/// # 参数
///
/// - `user_id`：看明文的人；
/// - `detail`：JSON 文本（事件 / 路由 / 说明），由调用方序列化好。
///
/// # 为什么直接 INSERT 而不入日志总线
///
/// 日志总线的 `record_audit` 需要 `&HeaderMap` + `SocketAddr`（落 `request_id` / 真实 IP），
/// 而脱敏解析发生在列表 handler 中段，那里通常没有这两个提取器。这里写一条**最小可审计**
/// 记录（`ip` / `user_agent` 为空，`detail` 里如实说明），由调用方 `tokio::spawn` 到后台，
/// 不阻塞主请求。
pub async fn insert_plain_read_audit(user_id: &Uuid, detail: &str) -> DbResult<()> {
    db().execute_with_params(
        "INSERT INTO audit_logs (id, request_id, user_id, username, operation, target_type, \
         target_id, detail, created_at) \
         VALUES (gen_random_uuid(), ?, ?, NULL, ?, ?, ?, ?::jsonb, CURRENT_TIMESTAMP)",
        vec![
            Value::from(format!("field-mask:{}", uuid::Uuid::now_v7())),
            Value::from(*user_id),
            Value::from("FIELD_MASK_PLAIN_READ"),
            Value::from("field_mask"),
            Value::from(user_id.to_string()),
            Value::Json(detail.to_string()),
        ],
    )
    .await?;
    Ok(())
}

// ==================== 缓存失效 ====================

/// 规则变更后清空所有用户的脱敏规则缓存。
///
/// # 为什么可以整片删
///
/// 规则表是**全局配置**（预计 < 50 行），改一条会影响所有用户的组合结果，
/// 逐用户精确失效需要「反查哪些用户受影响」= 全量；而规则变更是低频运维动作
/// （人工点按钮），一次 `SCAN + DEL` 的代价远小于「漏失效导致旧规则继续脱敏/放行」。
/// 键前缀见 [`constants::CACHE_KEY_PREFIX_USER_MASK`]。
///
/// # 返回值
///
/// 删除的键数量。缓存不可用返回 `0`（不是错误：没有缓存就没有脏读）。
pub async fn invalidate_all() -> u64 {
    let Some(store) = try_cache() else {
        return 0;
    };
    let pattern = format!("{}*", constants::CACHE_KEY_PREFIX_USER_MASK);
    match Cache::delete_pattern(store, &pattern).await {
        Ok(count) => count,
        Err(error) => {
            tracing::warn!(error = %error, pattern, "清理脱敏规则缓存失败（靠 TTL 兜底）");
            0
        }
    }
}

/// 可空文本参数：空串归一为 `NULL`（避免库里同时存在 `''` 与 `NULL` 两种「没参数」）。
fn opt_text(value: Option<String>) -> Value {
    match value.map(|item| item.trim().to_string()) {
        Some(text) if !text.is_empty() => Value::from(text),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_param_becomes_null() {
        assert!(matches!(opt_text(None), Value::Null));
        assert!(matches!(opt_text(Some("   ".to_string())), Value::Null));
        assert!(matches!(opt_text(Some("3,4".to_string())), Value::Text(_)));
    }
}
