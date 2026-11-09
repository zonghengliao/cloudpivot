//! 用户列表导出定义（`system.user`）——**导出脱敏的实机验证载体**（批次 B · B6）。
//!
//! # 为什么 B6 要新增这个定义
//!
//! 架构 §3.7 把「导出必须走同一套脱敏」列为安全红线，而 A3 交付时唯一的内建导出
//! （`system.dict_item`）**没有任何敏感列**——于是「导出路径是否真的脱敏」在实机上无法
//! 被验证。本文件注册一个真实的 `sys_user` 导出定义：
//!
//! - 三个敏感列（`real_name` / `email` / `mobile`）标记 `sensitive: true` 并带 `table`；
//! - 取数走 `scope::filter_for`（与 `system/user/page` 同一套数据权限片段），
//!   避免「列表看得到 5 个人、导出拿到 5000 个人」。
//!
//! 它不是玩具：运维批量导出用户通讯录是真需求，而这份文件恰恰是最该被脱敏的产物。
//!
//! # 与脱敏的关系
//!
//! 本文件**不自己脱敏**：它只声明「哪列敏感、属于哪张表」，实际脱敏发生在
//! [`crate::business::impexp::engine::format_export_rows`]（唯一改点，架构 §3.7）。
//! 这样同步导出与异步 worker 两条路径不可能出现「一条脱了、一条没脱」。

use std::collections::HashMap;

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{ColumnKind, EnumItem, ExportColumn, ExportDefinition, ExportFilter};
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope::{self, ScopeTarget};
use crate::utils;

/// 业务编码。
pub const USER_BIZ_TYPE: &str = "system.user";

/// 状态枚举（导出把码值翻回中文）。
static STATUS_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "启用",
        code: "1",
    },
    EnumItem {
        label: "停用",
        code: "0",
    },
];

/// 导出列定义。
///
/// `table` 是本卡新增的字段（架构 §3.7 的「额外硬约束」）：脱敏规则按
/// `(表, 列)` 匹配，没有表名就无法判断「这一列该套哪条规则」。
static EXPORT_COLUMNS: &[ExportColumn] = &[
    ExportColumn {
        key: "username",
        header: "用户名",
        kind: ColumnKind::Text { max_len: None },
        width: Some(18),
        table: "sys_user",
        sensitive: false,
    },
    ExportColumn {
        key: "real_name",
        header: "真实姓名",
        kind: ColumnKind::Text { max_len: None },
        width: Some(14),
        table: "sys_user",
        sensitive: true,
    },
    ExportColumn {
        key: "email",
        header: "邮箱",
        kind: ColumnKind::Text { max_len: None },
        width: Some(26),
        table: "sys_user",
        sensitive: true,
    },
    ExportColumn {
        key: "mobile",
        header: "手机号",
        kind: ColumnKind::Text { max_len: None },
        width: Some(18),
        table: "sys_user",
        sensitive: true,
    },
    ExportColumn {
        key: "status",
        header: "状态",
        kind: ColumnKind::Enum(STATUS_ITEMS),
        width: Some(10),
        table: "sys_user",
        sensitive: false,
    },
];

/// `sys_user` 导出定义。
pub struct UserExport;

impl UserExport {
    /// 拼数据权限片段与过滤条件（`count` 与 `fetch` 共用，避免两处 WHERE 不一致）。
    ///
    /// # 参数
    ///
    /// - `filter`：业务过滤条件；
    /// - `principal`：当前导出发起人的数据范围；`None`（无法确认发起人）→ **拒绝全部行**，
    ///   而不是把全表导出去。
    fn build_where(filter: &ExportFilter, principal: Option<&ScopedPrincipal>) -> (String, Vec<Value>) {
        let mut sql = String::from(" WHERE u.deleted_at IS NULL");
        let mut params: Vec<Value> = Vec::new();
        // 数据权限（B5 的注入点，本文件按 scope::filter_for 生成片段 + 参数，一起拼）
        let row_filter = match principal {
            Some(principal) => scope::filter_for(principal, ScopeTarget::SYS_USER),
            None => scope::RowFilter::deny_all(),
        };
        sql.push_str(row_filter.sql());
        params.extend(row_filter.params().iter().cloned());

        if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
            sql.push_str(" AND (u.username LIKE ? OR u.real_name LIKE ? OR u.email LIKE ? OR u.mobile LIKE ?)");
            params.push(Value::from(pattern.clone()));
            params.push(Value::from(pattern.clone()));
            params.push(Value::from(pattern.clone()));
            params.push(Value::from(pattern));
        }
        if let Some(status) = filter.status {
            sql.push_str(" AND u.status = ?");
            params.push(Value::from(i32::from(status)));
        }
        (sql, params)
    }
}

#[async_trait]
impl ExportDefinition for UserExport {
    fn biz_type(&self) -> &'static str {
        USER_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "用户"
    }

    fn columns(&self) -> &'static [ExportColumn] {
        EXPORT_COLUMNS
    }

    /// 用户的状态就是 `1` 启用 / `0` 停用 → 认整型 `status`（本卡不改这个口径）。
    /// `status_code`（文本码）没有白名单 → 422，不做静默映射。
    fn supports_status_flag(&self) -> bool {
        true
    }

    async fn count(&self, filter: &ExportFilter) -> std::result::Result<i64, DbError> {
        // 无 principal 的调用只可能是「没有调用方身份的旧路径」：按拒绝全部处理
        self.count_scoped(filter, None).await
    }

    async fn count_scoped(
        &self,
        filter: &ExportFilter,
        principal: Option<&ScopedPrincipal>,
    ) -> std::result::Result<i64, DbError> {
        let (where_sql, params) = Self::build_where(filter, principal);
        let sql = format!("SELECT COUNT(*) FROM sys_user u{where_sql}");
        db().scalar_with_params(&sql, params).await
    }

    async fn fetch(
        &self,
        filter: &ExportFilter,
        after_id: Option<&str>,
        limit: u32,
    ) -> std::result::Result<Vec<(String, HashMap<&'static str, String>)>, DbError> {
        self.fetch_scoped(filter, after_id, limit, None).await
    }

    async fn fetch_scoped(
        &self,
        filter: &ExportFilter,
        after_id: Option<&str>,
        limit: u32,
        principal: Option<&ScopedPrincipal>,
    ) -> std::result::Result<Vec<(String, HashMap<&'static str, String>)>, DbError> {
        let (mut sql, mut params) = Self::build_where(filter, principal);
        let head = "SELECT u.id::text AS id, u.username::text AS username, \
                    COALESCE(u.real_name::text, '') AS real_name, \
                    COALESCE(u.email::text, '') AS email, \
                    COALESCE(u.mobile::text, '') AS mobile, \
                    u.status::text AS status \
                    FROM sys_user u";
        if let Some(after) = after_id {
            // `?::uuid` 必须绑 uuid 值（传字符串会在驱动侧报 error serializing parameter）
            let after = uuid::Uuid::parse_str(after)
                .map_err(|error| DbError::InvalidValue(format!("after_id 不是合法 UUID（{after}）：{error}")))?;
            sql.push_str(" AND u.id > ?::uuid");
            params.push(Value::from(after));
        }
        sql.push_str(" ORDER BY u.id ASC LIMIT ?");
        params.push(Value::from(i64::from(limit)));

        let rows = db().find_with_params(&format!("{head}{sql}"), params).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get_typed("id")?;
            let mut values: HashMap<&'static str, String> = HashMap::new();
            for column in EXPORT_COLUMNS {
                let value: String = row.get_typed(column.key)?;
                values.insert(column.key, value);
            }
            out.push((id, values));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_columns_carry_table() {
        for column in EXPORT_COLUMNS {
            assert_eq!(column.table, "sys_user", "每列都必须带表名（脱敏要按表匹配规则）");
        }
        let sensitive: Vec<&str> = EXPORT_COLUMNS
            .iter()
            .filter(|column| column.sensitive)
            .map(|column| column.key)
            .collect();
        assert_eq!(sensitive, vec!["real_name", "email", "mobile"]);
    }

    #[test]
    fn unknown_principal_yields_deny_all_filter() {
        let (sql, params) = UserExport::build_where(&ExportFilter::default(), None);
        assert!(sql.contains("AND 1 = 0"), "无身份必须拒绝全部行：{sql}");
        assert!(params.is_empty());
    }

    /// 回归（卡 `t_93cba3f4`）：用户导出的 `status`（1 启用 / 0 停用）口径不变。
    #[test]
    fn status_filter_contract_is_unchanged_for_user_export() {
        use crate::business::impexp::definition::check_export_filter;

        let definition = UserExport;
        assert!(definition.supports_status_flag());
        assert!(definition.status_codes().is_empty(), "用户没有文本状态码");

        let mut filter = ExportFilter {
            status: Some(1),
            ..Default::default()
        };
        check_export_filter(&definition, &mut filter).expect("status=1 必须继续可用");
        // SQL 侧照旧：`AND u.status = ?`、参数 1
        let (sql, params) = UserExport::build_where(&filter, None);
        assert!(sql.contains("AND u.status = ?"), "{sql}");
        assert_eq!(params.len(), 1);

        let mut filter = ExportFilter {
            status_code: Some("POSTED".to_string()),
            ..Default::default()
        };
        let error = check_export_filter(&definition, &mut filter).expect_err("没有白名单");
        assert_eq!(error.status, 422);
    }
}
