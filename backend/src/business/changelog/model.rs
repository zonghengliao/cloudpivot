//! `sys_change_log` 的库侧读写（批次 C · C5）。
//!
//! # 文件边界
//!
//! 只做「把一行写进去 / 按对象读回来」，**不含任何策略**：白名单与摘要化在
//! [`crate::business::changelog::diff`]，脱敏在 [`crate::business::changelog::mask`]，
//! 编排（读规则 → 造 diff → 落库）在 [`crate::business::changelog::record`]。
//! 这样 diff 与 mask 都能在**没有数据库**的环境里逐条断言，查库部分只剩 SQL 形态。
//!
//! # 每个函数都吃 `&impl SqlExecutor`（D11 的物理保证）
//!
//! 本文件**没有任何 `db()` 调用**（门禁 G2：`grep -rn "db()" business/changelog/` = 0）。
//! 执行器只能由调用方传入，于是「日志与业务同事务」不是靠自觉，而是靠签名：
//! 想开第二个事务就必须先拿到 `db()`，而那一步在评审里看得见。
//!
//! # 为什么参数全走 `?::text` 再转型
//!
//! `sys_change_log` 的 6 个可空列（`request_id` / `operator_id` / `operator_dept_id` /
//! `client_ip` / `error_code` / `duration_ms`）与 1 个可空数组列（`changed_fields`）都需要
//! 「NULL 而不是空值」。驱动把 `Value::Null` 绑到 `?::uuid` / `?::inet` 这类位置时会因
//! 无法确定参数类型而报 `error serializing parameter`（批次 A 实测过的坑），
//! 因此统一传空串、由 SQL 侧 `NULLIF(?::text, '')::uuid` 还原成 NULL：
//! 一个参数表达式、一种绑定类型，没有分支也就没有「某条分支绑错」的可能。
//!
//! # 分区（V0084）
//!
//! 表按月 RANGE 分区、主键 `(id, created_at)`、**无 DEFAULT 分区**。因此：
//! - 写入的 `created_at` 必须落在已预建的月份里（越界 → PG 直接报
//!   `no partition of relation "sys_change_log" found for row`，这是设计要的：故障可见）；
//! - 任何「按 id 单查」的 SQL 都必须带 `created_at` 才能分区裁剪，
//!   所以本文件不提供 `find_by_id(id)`，只提供「按对象」与「按请求」两类查询。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::changelog::dto::OperatorType;

/// 表名（门禁 G1 的唯一写入者在 `business/changelog/model.rs`）。
pub const TABLE: &str = "sys_change_log";

/// 一行待写入的变更日志。
///
/// 字段顺序与 V0084 的列顺序一致，便于对照 DDL 复核；`result` 不放进来——
/// v1 只写 `SUCCESS`（架构 §4.3.3），由 [`insert`] 固定，调用方无从改。
#[derive(Debug, Clone)]
pub struct NewChangeLog<'a> {
    /// 行 ID（业务侧生成 UUIDv7，可按时间近似排序）。
    pub id: Uuid,
    /// 请求 ID（与 `operation_logs.request_id` 可关联）。
    pub request_id: Option<&'a str>,
    /// 来源 `API` / `JOB` / `IMPORT` / `SYSTEM`。
    pub trace_source: &'a str,
    /// 操作人 ID。
    pub operator_id: Option<Uuid>,
    /// 操作人名字（冗余）。
    pub operator_name: Option<&'a str>,
    /// 操作人类型（`HUMAN` / `ROBOT`，批次 C+ · T5）。
    ///
    /// 冗余落库而不是 join 出来：`sys_user.user_type` 会变（今天 ROBOT 明天改回
    /// HUMAN 也合法），审计要的是「**当时**是谁在操作」。
    pub operator_type: &'a str,
    /// 操作人部门（冗余）。
    pub operator_dept_id: Option<Uuid>,
    /// 客户端 IP。
    pub client_ip: Option<&'a str>,
    /// 模块 `system` / `business` / `support`。
    pub module: &'a str,
    /// 表名。
    pub resource: &'a str,
    /// 业务主键。
    pub resource_id: &'a str,
    /// 动作（`CREATE` / `UPDATE` / ...）。
    pub action: &'a str,
    /// 中文可读动作名。
    pub action_label: &'a str,
    /// 序列化后的 diff JSON。
    pub diff: String,
    /// 记录模式（v1 恒 `WHITELIST`）。
    pub diff_mode: &'a str,
    /// 变化字段名（写进 `changed_fields`，供 GIN `&&` 反查）。
    pub changed_fields: &'a [String],
    /// 错误码（成功路径为 `None`）。
    pub error_code: Option<i32>,
    /// 耗时（毫秒）。
    pub duration_ms: Option<i32>,
    /// 业务发生时刻（决定落哪个分区）。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 写入一行变更日志。
///
/// # 参数
///
/// - `ex`：**业务事务**的执行器（D11：不要传 `db()`，否则退化成异步旁路）；
/// - `row`：待写入行。
///
/// # 异常情况
///
/// - 数据库异常（含「`created_at` 落在未预建的月份」→ 分区越界）：返回 [`DbError`]。
///   调用方**必须**让错误向上传播：同事务意味着「日志写不进去 → 业务也回滚」，
///   而不是「业务成功但无凭证」（T-CL-12 就是这么断言的）。
pub async fn insert(ex: &impl SqlExecutor, row: &NewChangeLog<'_>) -> DbResult<()> {
    // v1 只写 SUCCESS：失败尝试不进本表（§4.3.3）。放在这里而不是调用方，避免被绕过。
    let result = "SUCCESS";
    debug_assert_eq!(result, "SUCCESS");

    let operator_id = row.operator_id.map(|id| id.to_string()).unwrap_or_default();
    let operator_dept = row.operator_dept_id.map(|id| id.to_string()).unwrap_or_default();
    let error_code = row.error_code.map(|code| code.to_string()).unwrap_or_default();
    let duration_ms = row.duration_ms.map(|ms| ms.to_string()).unwrap_or_default();

    let params = vec![
        Value::from(row.id),
        Value::from(row.request_id.unwrap_or("")),
        Value::from(row.trace_source),
        Value::from(operator_id),
        Value::from(row.operator_name.unwrap_or("")),
        Value::from(row.operator_type),
        Value::from(operator_dept),
        Value::from(row.client_ip.unwrap_or("")),
        Value::from(row.module),
        Value::from(row.resource),
        Value::from(row.resource_id),
        Value::from(row.action),
        Value::from(row.action_label),
        Value::Json(row.diff.clone()),
        Value::from(row.diff_mode),
        Value::from(array_literal(row.changed_fields)),
        Value::from(result),
        Value::from(error_code),
        Value::from(duration_ms),
        Value::from(row.created_at),
    ];
    ex.exec_sql(INSERT_SQL.to_string(), params).await?;
    Ok(())
}

/// 一条 INSERT 语句（列顺序与 [`NewChangeLog`] 一致）。
const INSERT_SQL: &str = "\
INSERT INTO sys_change_log \
    (id, request_id, trace_source, operator_id, operator_name, operator_type, operator_dept_id, \
     client_ip, module, resource, resource_id, action, action_label, diff, diff_mode, \
     changed_fields, result, error_code, duration_ms, created_at) \
VALUES \
    (?::uuid, NULLIF(?::text, ''), ?::text, NULLIF(?::text, '')::uuid, NULLIF(?::text, ''), \
     ?::text, NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::inet, ?::text, ?::text, ?::text, \
     ?::text, ?::text, ?::jsonb, ?::text, NULLIF(?::text, '')::text[], ?::text, \
     NULLIF(?::text, '')::integer, NULLIF(?::text, '')::integer, ?::timestamptz)";

/// 把字段名数组拼成 PG 数组字面量（`{"mobile","status"}`）；空数组返回空串（SQL 侧转 NULL）。
fn array_literal(fields: &[String]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    let items: Vec<String> = fields.iter().map(|name| format!("\"{name}\"")).collect();
    format!("{{{}}}", items.join(","))
}

/// 某对象的日志条数（自检 / 测试 / C6 的「变更次数」列用）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `resource` / `resource_id`：对象坐标。
pub async fn count_for_object(ex: &impl SqlExecutor, resource: &str, resource_id: &str) -> DbResult<i64> {
    ex.scalar_sql(
        "SELECT COUNT(*) FROM sys_change_log WHERE resource = ?::text AND resource_id = ?::text".to_string(),
        vec![Value::from(resource), Value::from(resource_id)],
    )
    .await
}

/// 某对象**最近一条**变更日志的「操作人名字 + 时间」（详情页展示「谁什么时候填的」）。
///
/// # 为什么只取两列
///
/// 调用方（成本对象详情）要的是设计 §8.3 R-4 的「填写人 + 时间」两个事实，
/// diff 全文另有审计页按需查。`operator_name` 是**写入时的快照**（账号改名后历史
/// 仍按当时的名字显示 —— AGENTS 约定 13 的审计口径）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `resource` / `resource_id`：对象坐标；
/// - `action_label`：只认某个动作（`None` = 该对象的任意变更里取最新）。
///   成本对象传 [`crate::business::erp::cost_object_dto::OUTPUT_ACTION_LABEL`]，
///   这样将来改主数据也接审计时，面板不会把「改了备注」显示成「填了产量」。
///
/// # 返回值
///
/// `Some((operator_name, created_at))`；没有任何日志 = `None`（不造「未知操作人」假行）。
///
/// 走 `idx_sys_change_log_object (resource, resource_id, created_at DESC)`，单次索引扫描。
pub async fn latest_for_object(
    ex: &impl SqlExecutor,
    resource: &str,
    resource_id: &str,
    action_label: Option<&str>,
) -> DbResult<Option<(Option<String>, chrono::DateTime<chrono::Utc>)>> {
    // 两个固定 SQL 而不是「? IS NULL OR = ?」：后者在 PG 上要显式类型标注，
    // 且 plan 会退化成不带条件的两路扫描（历史上的 `Parameters(n, n-2)` 坑同源）。
    let (sql, params) = match action_label {
        Some(label) => (
            "SELECT operator_name, created_at FROM sys_change_log \
             WHERE resource = ?::text AND resource_id = ?::text AND action_label = ?::text \
             ORDER BY created_at DESC, id DESC LIMIT 1",
            vec![Value::from(resource), Value::from(resource_id), Value::from(label)],
        ),
        None => (
            "SELECT operator_name, created_at FROM sys_change_log \
             WHERE resource = ?::text AND resource_id = ?::text \
             ORDER BY created_at DESC, id DESC LIMIT 1",
            vec![Value::from(resource), Value::from(resource_id)],
        ),
    };
    let rows = ex.query_sql(sql.to_string(), params).await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let operator_name: Option<String> = row.get_typed("operator_name")?;
    let created_at: chrono::DateTime<chrono::Utc> = row.get_typed("created_at")?;
    Ok(Some((operator_name, created_at)))
}

/// 某对象的全部 `diff` 文本（按时间升序；测试与排障用）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `resource` / `resource_id`：对象坐标。
pub async fn diff_texts(ex: &impl SqlExecutor, resource: &str, resource_id: &str) -> DbResult<Vec<String>> {
    let rows = ex
        .query_sql(
            "SELECT diff::text AS diff FROM sys_change_log \
             WHERE resource = ?::text AND resource_id = ?::text \
             ORDER BY created_at, id"
                .to_string(),
            vec![Value::from(resource), Value::from(resource_id)],
        )
        .await?;
    let mut texts = Vec::with_capacity(rows.len());
    for row in &rows {
        texts.push(row.get_typed::<String>("diff")?);
    }
    Ok(texts)
}

/// 删除某对象的全部日志（**只给测试夹具用**：业务代码不得删审计表）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `resource` / `resource_id`：对象坐标。
///
/// # 返回值
///
/// 删除行数。
pub async fn purge_for_object(ex: &impl SqlExecutor, resource: &str, resource_id: &str) -> DbResult<u64> {
    ex.exec_sql(
        "DELETE FROM sys_change_log WHERE resource = ?::text AND resource_id = ?::text".to_string(),
        vec![Value::from(resource), Value::from(resource_id)],
    )
    .await
}

/// 按操作人 ID 读「操作人类型」（`HUMAN` / `ROBOT`）。
///
/// # 为什么放在 `model` 而不是 `record`
///
/// 它是一条 SQL，必须和 [`insert`] 一样只吃调用方传进来的执行器：
/// 在**业务事务的快照**里读 `sys_user.user_type`，保证「日志记的类型」与
/// 「同一事务里读到的账号类型」一致（会话外改 `user_type` 不会插进来）。
///
/// # SCOPE-EXEMPT
///
/// 按主键读一列的账号类型，不涉及行级可见性（不是列表 / 导出查询），
/// 因此不注入数据范围片段 —— 见函数上方的门禁豁免标记。
///
/// # 参数
///
/// - `ex`：业务事务的执行器；
/// - `operator_id`：日志行上的 `operator_id`。`None`（系统任务）**不查库**，
///   直接返回 `HUMAN` —— 与迁移里的 `DEFAULT 'HUMAN'` 一致；系统任务的来源靠
///   `trace_source = SYSTEM/JOB` 表达，不靠 `operator_type`。
///
/// # 返回值
///
/// 查到 `ROBOT` → [`OperatorType::Robot`]；查不到 / 读到脏值 → [`OperatorType::Human`]。
/// **读侧不因为找不到人而报错**：日志是「事后凭证」，不该被一个已删除的账号卡住。
// SCOPE-EXEMPT: 按主键读 sys_user.user_type（单列账号类型，不涉及行级可见性，非列表/导出查询）
pub async fn operator_type_of(ex: &impl SqlExecutor, operator_id: Option<Uuid>) -> DbResult<OperatorType> {
    let Some(operator_id) = operator_id else {
        return Ok(OperatorType::Human);
    };
    let rows = ex
        .query_sql(
            "SELECT user_type FROM sys_user WHERE id = ?::uuid".to_string(),
            vec![Value::from(operator_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(OperatorType::from_user_type(
            row.get_typed::<Option<String>>("user_type")?.as_deref(),
        )),
        None => Ok(OperatorType::Human),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 数组字面量：空 → 空串（SQL 侧 NULL），有值 → PG 数组语法。
    #[test]
    fn array_literal_shapes() {
        assert_eq!(array_literal(&[]), "");
        assert_eq!(
            array_literal(&["mobile".to_string(), "status".to_string()]),
            r#"{"mobile","status"}"#
        );
    }

    /// SQL 形态自检：20 个占位符、20 个参数位、没有任何字面量拼接进 SQL。
    #[test]
    fn insert_sql_placeholder_count_matches_params() {
        let holes = INSERT_SQL.matches('?').count();
        assert_eq!(holes, 20, "INSERT 必须有 20 个占位符，实际 {holes}");
        // 关键列必须在 SQL 里（防止手改 SQL 漏列）
        for column in [
            "id",
            "request_id",
            "trace_source",
            "operator_id",
            "operator_name",
            "operator_type",
            "operator_dept_id",
            "client_ip",
            "module",
            "resource",
            "resource_id",
            "action",
            "action_label",
            "diff",
            "diff_mode",
            "changed_fields",
            "result",
            "error_code",
            "duration_ms",
            "created_at",
        ] {
            assert!(INSERT_SQL.contains(column), "INSERT 缺少列 {column}");
        }
        assert!(INSERT_SQL.contains("?::jsonb"), "diff 必须走 jsonb 转型");
    }
}
