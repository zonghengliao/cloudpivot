//! 四种发送目标的扇出（指定用户 / 角色 / 部门含下级 / 全员）。
//!
//! # 铁律：集合式插入，禁止在 Rust 侧循环单条 insert（架构文档 §4.2 决策 D8）
//!
//! 四个目标各自对应**一条** `INSERT ... SELECT`，接收人由数据库一次性算出来再落库：
//!
//! | 目标 | 命中的用户集合 |
//! |------|----------------|
//! | `USER` | `sys_user.id IN (...)`，排除软删 / 停用 |
//! | `ROLE` | `sys_user_role` 关联（授权未过期），排除软删 / 停用 |
//! | `DEPT` | **递归 CTE** 展开部门子树后取该子树内用户（不依赖可能失同步的 `dept_path`） |
//! | `ALL`  | 全表，排除软删 / 停用 |
//!
//! 幂等与去重由 `UNIQUE(notice_id, user_id)` + `ON CONFLICT DO NOTHING` 兜底：
//! 同一通知重放零新增行；一个用户命中多个角色 / 多个部门时也只落一行。
//!
//! # 目标 ID 的传递方式
//!
//! 用 `jsonb_array_elements_text(?::jsonb)` 把 UUID 数组作为**一个 JSON 参数**传进 SQL，
//! 既不拼 SQL 字符串（注入面为零），也不依赖驱动对 PG 数组类型的绑定支持。
//!
//! # 规模上限
//!
//! 单条通知最多 [`MAX_RECEIVERS`] 个接收人（架构文档 §4.2 兜底条款）：超限返回
//! `422 + 60027`，提示改用「全员公告」或分批发送，避免长时间持锁。

use alun::prelude::*;

use crate::shared::error_code;

/// 单条通知的接收人上限（架构文档 §4.2：>5 万改用公告或分批）。
pub const MAX_RECEIVERS: i64 = 50_000;

/// 发送目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeTarget {
    /// 指定用户。
    User(Vec<uuid::Uuid>),
    /// 角色（含该角色下所有用户）。
    Role(Vec<uuid::Uuid>),
    /// 部门（**含下级**）。
    Dept(Vec<uuid::Uuid>),
    /// 全员。
    All,
}

impl NoticeTarget {
    /// 由 API 入参（`target_type` + `target_ids` 字符串）构造目标。
    ///
    /// # 参数
    ///
    /// - `target_type`：`USER` / `ROLE` / `DEPT` / `ALL`（大小写不敏感）。
    /// - `target_ids`：目标 ID 字符串列表（`ALL` 时忽略；重复 ID 自动去重）。
    ///
    /// # 异常情况
    ///
    /// - 目标类型未知 → `400 + 10002`；
    /// - ID 非法 → `400 + 10002`；
    /// - 非 `ALL` 但列表为空 → `400 + 60026`。
    pub fn parse(target_type: &str, target_ids: &[String]) -> std::result::Result<Self, ApiError> {
        let ids = parse_ids(target_ids)?;
        let target = match target_type.trim().to_ascii_uppercase().as_str() {
            "USER" => NoticeTarget::User(ids),
            "ROLE" => NoticeTarget::Role(ids),
            "DEPT" => NoticeTarget::Dept(ids),
            "ALL" => NoticeTarget::All,
            other => {
                return Err(ApiError::new(
                    400,
                    error_code::PARAM_INVALID,
                    format!("发送目标类型非法：{other}（允许 USER/ROLE/DEPT/ALL）"),
                ));
            }
        };
        target.validate()?;
        Ok(target)
    }

    /// 数据库字面量（`sys_notice.target_type`）。
    pub fn target_type(&self) -> &'static str {
        match self {
            NoticeTarget::User(_) => "USER",
            NoticeTarget::Role(_) => "ROLE",
            NoticeTarget::Dept(_) => "DEPT",
            NoticeTarget::All => "ALL",
        }
    }

    /// 目标 ID（`ALL` 为空切片）。
    pub fn ids(&self) -> &[uuid::Uuid] {
        match self {
            NoticeTarget::User(ids) | NoticeTarget::Role(ids) | NoticeTarget::Dept(ids) => ids,
            NoticeTarget::All => &[],
        }
    }

    /// 目标 ID 的 JSON 数组文本（写 `sys_notice.target_ids` 留痕用）。
    pub fn ids_json(&self) -> String {
        let values: Vec<String> = self.ids().iter().map(|id| id.to_string()).collect();
        serde_json::Value::Array(values.into_iter().map(serde_json::Value::String).collect())
            .to_string()
    }

    /// 校验目标是否可用。
    ///
    /// # 异常情况
    ///
    /// 非 `ALL` 且 ID 列表为空 → `400 + 60026`（空目标会「发出去了但没人收到」，必须拦住）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if !matches!(self, NoticeTarget::All) && self.ids().is_empty() {
            return Err(ApiError::new(
                400,
                error_code::NOTICE_TARGET_INVALID,
                format!(
                    "发送目标 {} 的 target_ids 不能为空（全员通知请用 ALL）",
                    self.target_type()
                ),
            ));
        }
        Ok(())
    }

    /// 命中用户集合的子查询（`SELECT ... AS user_id`），扇出与计数共用同一份口径。
    ///
    /// 返回 `(SQL 片段, 参数)`；片段可安全地嵌入 `INSERT ... SELECT` 或 `SELECT COUNT(*)`。
    ///
    /// SCOPE-EXEMPT: 扇出**必须**能枚举目标用户（ALL / DEPT / DEPT_AND_CHILD / ROLE / USER）。
    /// 这里是「按发送者选定的目标找收件人」，注入**发送者**的数据范围会让「该收到的人收不到」
    /// ——那是业务正确性故障（静默少数据），比权限问题更危险；收件范围的合法性由
    /// `NoticeTarget` 的入参校验 + `business:notice:create` 权限码控制。
    fn user_set_sql(&self) -> (String, Vec<Value>) {
        match self {
            NoticeTarget::User(_) => (
                "SELECT u.id AS user_id FROM sys_user u \
                 WHERE u.id IN (SELECT (x)::uuid FROM jsonb_array_elements_text(?::jsonb) AS x) \
                   AND u.deleted_at IS NULL AND u.status = 1 \
                 ORDER BY u.id"
                    .to_string(),
                vec![Value::Json(self.ids_json())],
            ),
            NoticeTarget::Role(_) => (
                "SELECT DISTINCT u.id AS user_id FROM sys_user_role ur \
                 JOIN sys_user u ON u.id = ur.user_id \
                 WHERE ur.role_id IN (SELECT (x)::uuid FROM jsonb_array_elements_text(?::jsonb) AS x) \
                   AND (ur.expire_time IS NULL OR ur.expire_time > CURRENT_TIMESTAMP) \
                   AND u.deleted_at IS NULL AND u.status = 1"
                    .to_string(),
                vec![Value::Json(self.ids_json())],
            ),
            // 递归 CTE 与 system/dept/model.rs::find_dept_subtree_ids 同款：
            // 不依赖 dept_path（部门搬迁时不会重算，见 system/dept/handler.rs 的已知限制）。
            NoticeTarget::Dept(_) => (
                "WITH RECURSIVE dept_tree AS ( \
                     SELECT d.id FROM sys_dept d \
                     WHERE d.id IN (SELECT (x)::uuid FROM jsonb_array_elements_text(?::jsonb) AS x) \
                       AND d.deleted_at IS NULL \
                     UNION ALL \
                     SELECT c.id FROM sys_dept c \
                     JOIN dept_tree t ON c.parent_id = t.id \
                     WHERE c.deleted_at IS NULL \
                 ) \
                 SELECT DISTINCT u.id AS user_id FROM sys_user u \
                 JOIN dept_tree t ON u.dept_id = t.id \
                 WHERE u.deleted_at IS NULL AND u.status = 1"
                    .to_string(),
                vec![Value::Json(self.ids_json())],
            ),
            NoticeTarget::All => (
                "SELECT u.id AS user_id FROM sys_user u \
                 WHERE u.deleted_at IS NULL AND u.status = 1"
                    .to_string(),
                Vec::new(),
            ),
        }
    }

    /// 扇出用的完整 `INSERT ... SELECT` 语句与参数（顺序：`notice_id` 在前）。
    fn insert_sql(&self) -> (String, Vec<Value>) {
        let (body, body_params) = self.user_set_sql();
        let sql = format!(
            "INSERT INTO sys_notice_receiver (notice_id, user_id) \
             SELECT ?::uuid, t.user_id FROM ({body}) t \
             ON CONFLICT (notice_id, user_id) DO NOTHING"
        );
        let mut params = Vec::with_capacity(body_params.len() + 1);
        // notice_id 出现在 SELECT 列表里，位置在子查询之前 → 必须排第一个
        params.push(Value::from(uuid::Uuid::nil()));
        params.extend(body_params);
        (sql, params)
    }

    /// 计数用的 `SELECT COUNT(*)` 语句与参数。
    fn count_sql(&self) -> (String, Vec<Value>) {
        let (body, params) = self.user_set_sql();
        (format!("SELECT COUNT(*) FROM ({body}) t"), params)
    }
}

/// 预演命中人数（发送前做 5 万上限校验，避免「先插 20 万行再报错」）。
///
/// # 参数
///
/// - `target`：发送目标。
///
/// # 返回值
///
/// 命中用户数（已排除软删 / 停用 / 过期授权）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_receivers(target: &NoticeTarget) -> DbResult<i64> {
    let (sql, params) = target.count_sql();
    db().scalar_with_params(&sql, params).await
}

/// 在事务内执行扇出（**一条** `INSERT ... SELECT`）。
///
/// # 参数
///
/// - `tx`：调用方事务（notice 落库 → 扇出 → 计数回写必须同事务，避免半成品通知）。
/// - `notice_id`：通知主键。
/// - `target`：发送目标。
///
/// # 返回值
///
/// 实际新增的接收行数（重放时为 0，由唯一索引兜底）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn fanout(
    tx: &Transaction,
    notice_id: &uuid::Uuid,
    target: &NoticeTarget,
) -> DbResult<u64> {
    let (sql, mut params) = target.insert_sql();
    params[0] = Value::from(*notice_id);
    tx.execute_with_params(&sql, params).await
}

/// 校验接收人规模上限。
///
/// # 异常情况
///
/// 超过 [`MAX_RECEIVERS`] → `422 + 60027`。
pub fn ensure_within_limit(count: i64) -> std::result::Result<(), ApiError> {
    if count > MAX_RECEIVERS {
        return Err(ApiError::new(
            422,
            error_code::NOTICE_RECEIVER_LIMIT,
            format!(
                "接收人 {count} 人超过单条通知上限 {MAX_RECEIVERS} 人：请改用全员公告或分批发送"
            ),
        ));
    }
    Ok(())
}

/// 解析并去重 UUID 列表。
fn parse_ids(raw: &[String]) -> std::result::Result<Vec<uuid::Uuid>, ApiError> {
    let mut ids = Vec::with_capacity(raw.len());
    for item in raw {
        let text = item.trim();
        if text.is_empty() {
            continue;
        }
        let id = uuid::Uuid::parse_str(text).map_err(|_| {
            ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("target_ids 含非法 UUID：{text}"),
            )
        })?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuids(count: usize) -> Vec<String> {
        (0..count)
            .map(|_| uuid::Uuid::now_v7().to_string())
            .collect()
    }

    /// 四种目标类型都能解析，字面量与 `sys_notice.target_type` 一致。
    #[test]
    fn parse_all_target_types() {
        let ids = uuids(2);
        assert_eq!(
            NoticeTarget::parse("user", &ids).unwrap().target_type(),
            "USER"
        );
        assert_eq!(
            NoticeTarget::parse("ROLE", &ids).unwrap().target_type(),
            "ROLE"
        );
        let dept = NoticeTarget::parse("dept", &ids).unwrap();
        assert_eq!(dept.target_type(), "DEPT");
        assert_eq!(dept.ids().len(), 2);
        let all = NoticeTarget::parse("all", &[]).unwrap();
        assert_eq!(all.target_type(), "ALL");
        assert!(all.ids().is_empty());
    }

    /// 非 ALL 目标给空列表必须被拦住（否则会发出「没人收到」的通知）。
    #[test]
    fn empty_target_ids_rejected() {
        let error = NoticeTarget::parse("USER", &[]).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TARGET_INVALID);
        assert_eq!(error.status, 400);
        // 空白字符串等同未提供
        assert!(NoticeTarget::parse("ROLE", &["  ".to_string()]).is_err());
    }

    /// 目标类型未知 / ID 非法 → 400（不静默降级成全员，这会是灾难性误发）。
    #[test]
    fn invalid_target_rejected() {
        assert!(NoticeTarget::parse("EVERYONE", &uuids(1)).is_err());
        let error = NoticeTarget::parse("USER", &["not-a-uuid".to_string()]).unwrap_err();
        assert_eq!(error.code, error_code::PARAM_INVALID);
    }

    /// 重复 ID 去重（前端多选重复提交不该变成两倍接收人）。
    #[test]
    fn duplicate_ids_deduped() {
        let id = uuid::Uuid::now_v7().to_string();
        let target = NoticeTarget::parse("USER", &[id.clone(), id.clone()]).unwrap();
        assert_eq!(target.ids().len(), 1);
        assert_eq!(target.ids_json(), format!("[\"{id}\"]"));
    }

    /// 四条 SQL 的口径：必须是集合式 INSERT ... SELECT，且部门走递归 CTE。
    #[test]
    fn fanout_sql_is_set_based() {
        let ids = uuids(1);
        for kind in ["USER", "ROLE", "DEPT", "ALL"] {
            let target = NoticeTarget::parse(kind, &ids).unwrap();
            let (sql, params) = target.insert_sql();
            assert!(
                sql.starts_with("INSERT INTO sys_notice_receiver"),
                "{kind} 必须是单条 INSERT ... SELECT"
            );
            assert!(sql.contains("SELECT"), "{kind} 必须是集合式插入");
            assert!(sql.contains("ON CONFLICT (notice_id, user_id) DO NOTHING"));
            // 参数：notice_id + （可选）目标 ID 数组
            assert_eq!(params.len(), if kind == "ALL" { 1 } else { 2 });
        }

        let dept = NoticeTarget::parse("DEPT", &ids).unwrap();
        let (sql, _) = dept.insert_sql();
        assert!(
            sql.contains("WITH RECURSIVE dept_tree"),
            "部门必须递归展开子树"
        );

        // 计数与扇出共用同一份用户集合口径（口径不一致会导致上限校验失真）
        let (count_sql, params) = dept.count_sql();
        assert!(count_sql.starts_with("SELECT COUNT(*) FROM (WITH RECURSIVE"));
        assert_eq!(params.len(), 1);
    }

    /// 超出 5 万上限 → 422 + 60027；等于上限是允许的。
    #[test]
    fn receiver_limit_enforced() {
        assert!(ensure_within_limit(MAX_RECEIVERS).is_ok());
        let error = ensure_within_limit(MAX_RECEIVERS + 1).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_RECEIVER_LIMIT);
        assert_eq!(error.status, 422);
    }
}
