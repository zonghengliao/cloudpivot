//! 真实业务定义：`sys_dict_item`（字典项）导入 / 导出。
//!
//! # 端到端用例
//!
//! 模板下载 → 上传预检（含外键批量校验 `sys_dict_type.dict_code`）→ 确认入库
//! （`INSERT ... ON CONFLICT DO UPDATE`，幂等）→ 同步导出（状态码 1/0 翻译成「启用/停用」）。
//!
//! # 落库规则
//!
//! - 外键：Excel 里写**字典编码**（`sys_dict_type.dict_code`），入库时 JOIN 换成 `dict_type_id`；
//! - 唯一：`(字典编码, 字典项值)` 组合，文件内重复报 `UNIQUE`，库内重复按
//!   `ON CONFLICT ... DO UPDATE` 更新（幂等重导）；
//! - 状态：Excel 写「启用/停用」，落库 `'1'/'0'`。
//!
//! # 为什么用 `jsonb_to_recordset` 而不是多行 VALUES
//!
//! 一批 2000 行 × 7 列 = 14000 个占位符，接近 PostgreSQL 65535 参数上限；用 1 个 JSONB
//! 参数既避开上限，也让「批大小」可以按需调整而不动 SQL。

use std::collections::HashMap;

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, EnumItem, ExportColumn, ExportDefinition, ExportFilter, FkSpec, ImportColumn,
    ImportCtx, ImportDefinition, RowValues,
};

/// 状态枚举：Excel 写中文，落库 `'1'/'0'`。
const STATUS_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "启用",
        code: "1",
    },
    EnumItem {
        label: "停用",
        code: "0",
    },
];

/// 导入列定义（顺序即模板列顺序）。
static IMPORT_COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: "dict_type_code",
        header: "字典编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: true,
        fk: Some(FkSpec {
            table: "sys_dict_type",
            column: "dict_code",
            label: "字典类型",
            soft_delete: true,
        }),
        example: "user_status",
        width: Some(18),
        remark: "字典类型的编码，须已在「字典管理」中维护",
    },
    ImportColumn {
        key: "item_value",
        header: "字典项值",
        required: true,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: true,
        fk: None,
        example: "1",
        width: Some(14),
        remark: "落库值，同一字典内唯一（如 1 / 0）",
    },
    ImportColumn {
        key: "item_label",
        header: "字典项名称",
        required: true,
        kind: ColumnKind::Text { max_len: Some(100) },
        unique_key: false,
        fk: None,
        example: "启用",
        width: Some(16),
        remark: "页面展示文案",
    },
    ImportColumn {
        key: "description",
        header: "描述",
        required: false,
        kind: ColumnKind::Text { max_len: Some(500) },
        unique_key: false,
        fk: None,
        example: "账号可正常登录",
        width: Some(28),
        remark: "可选",
    },
    ImportColumn {
        key: "sort_order",
        header: "排序号",
        required: false,
        kind: ColumnKind::Int {
            min: Some(0),
            max: Some(9_999),
        },
        unique_key: false,
        fk: None,
        example: "0",
        width: Some(8),
        remark: "升序展示，留空按 0",
    },
    ImportColumn {
        key: "is_default",
        header: "是否默认",
        required: false,
        kind: ColumnKind::Bool,
        unique_key: false,
        fk: None,
        example: "否",
        width: Some(10),
        remark: "是 / 否，留空按「否」",
    },
    ImportColumn {
        key: "status",
        header: "状态",
        required: false,
        kind: ColumnKind::Enum(STATUS_ITEMS),
        unique_key: false,
        fk: None,
        example: "启用",
        width: Some(10),
        remark: "启用 / 停用，留空按「启用」",
    },
];

/// 导出列定义。
static EXPORT_COLUMNS: &[ExportColumn] = &[
    ExportColumn {
        key: "dict_type_code",
        header: "字典编码",
        kind: ColumnKind::Text { max_len: None },
        width: Some(18),
        table: "sys_dict_item",
        sensitive: false,
    },
    ExportColumn {
        key: "item_value",
        header: "字典项值",
        kind: ColumnKind::Text { max_len: None },
        width: Some(14),
        table: "sys_dict_item",
        sensitive: false,
    },
    ExportColumn {
        key: "item_label",
        header: "字典项名称",
        kind: ColumnKind::Text { max_len: None },
        width: Some(16),
        table: "sys_dict_item",
        sensitive: false,
    },
    ExportColumn {
        key: "description",
        header: "描述",
        kind: ColumnKind::Text { max_len: None },
        width: Some(28),
        table: "sys_dict_item",
        sensitive: false,
    },
    ExportColumn {
        key: "sort_order",
        header: "排序号",
        kind: ColumnKind::Int {
            min: None,
            max: None,
        },
        width: Some(8),
        table: "sys_dict_item",
        sensitive: false,
    },
    ExportColumn {
        key: "is_default",
        header: "是否默认",
        kind: ColumnKind::Bool,
        width: Some(10),
        table: "sys_dict_item",
        sensitive: false,
    },
    ExportColumn {
        key: "status",
        header: "状态",
        kind: ColumnKind::Enum(STATUS_ITEMS),
        width: Some(10),
        table: "sys_dict_item",
        sensitive: false,
    },
];

/// 字典项导入定义。
pub struct DictItemImport;

#[async_trait]
impl ImportDefinition for DictItemImport {
    fn biz_type(&self) -> &'static str {
        super::DICT_ITEM_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "字典项"
    }

    fn template_version(&self) -> u32 {
        1
    }

    fn max_rows(&self) -> usize {
        // 与 PRD「单次最多 100,000 行」及模板「填写须知」保持一致
        100_000
    }

    fn batch_size(&self) -> usize {
        2_000
    }

    fn columns(&self) -> &'static [ImportColumn] {
        IMPORT_COLUMNS
    }

    async fn persist(
        &self,
        tx: &Transaction,
        rows: &[RowValues],
        ctx: &ImportCtx,
    ) -> std::result::Result<usize, DbError> {
        if rows.is_empty() {
            return Ok(0);
        }
        let payload = serde_json::to_string(
            &rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "dict_type_code": row.get("dict_type_code"),
                        "item_value": row.get("item_value"),
                        "item_label": row.get("item_label"),
                        "description": non_empty(row.get("description")),
                        "sort_order": row.get("sort_order").parse::<i32>().unwrap_or(0),
                        "is_default": row.get("is_default") == "true",
                        "status": row.get("status").parse::<i16>().unwrap_or(1),
                    })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| DbError::InvalidValue(format!("字典项序列化失败：{error}")))?;

        // 操作人为空时直接用 SQL 里的 NULL（`Value::Null` 绑到 `?::uuid` 会编码失败）
        const SQL_WITH_OPERATOR: &str = "INSERT INTO sys_dict_item \
             (id, dict_type_id, item_value, item_label, description, sort_order, is_default, \
              status, created_at, updated_at, create_by, update_by) \
             SELECT gen_random_uuid(), t.id, v.item_value, v.item_label, v.description, \
                    v.sort_order, v.is_default, v.status, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
                    ?::uuid, ?::uuid \
             FROM jsonb_to_recordset(?::jsonb) AS v(dict_type_code text, item_value text, \
                    item_label text, description text, sort_order int, is_default boolean, \
                    status smallint) \
             INNER JOIN sys_dict_type t ON t.dict_code = v.dict_type_code \
                    AND t.deleted_at IS NULL \
             ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL \
             DO UPDATE SET item_label = EXCLUDED.item_label, \
                    description = EXCLUDED.description, \
                    sort_order = EXCLUDED.sort_order, \
                    is_default = EXCLUDED.is_default, \
                    status = EXCLUDED.status, \
                    updated_at = CURRENT_TIMESTAMP, \
                    update_by = EXCLUDED.update_by";
        const SQL_WITHOUT_OPERATOR: &str = "INSERT INTO sys_dict_item \
             (id, dict_type_id, item_value, item_label, description, sort_order, is_default, \
              status, created_at, updated_at) \
             SELECT gen_random_uuid(), t.id, v.item_value, v.item_label, v.description, \
                    v.sort_order, v.is_default, v.status, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP \
             FROM jsonb_to_recordset(?::jsonb) AS v(dict_type_code text, item_value text, \
                    item_label text, description text, sort_order int, is_default boolean, \
                    status smallint) \
             INNER JOIN sys_dict_type t ON t.dict_code = v.dict_type_code \
                    AND t.deleted_at IS NULL \
             ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL \
             DO UPDATE SET item_label = EXCLUDED.item_label, \
                    description = EXCLUDED.description, \
                    sort_order = EXCLUDED.sort_order, \
                    is_default = EXCLUDED.is_default, \
                    status = EXCLUDED.status, \
                    updated_at = CURRENT_TIMESTAMP";

        let (sql, params) = match ctx.operator {
            Some(operator) => (
                SQL_WITH_OPERATOR,
                vec![
                    Value::from(operator),
                    Value::from(operator),
                    Value::Json(payload),
                ],
            ),
            None => (SQL_WITHOUT_OPERATOR, vec![Value::from(payload)]),
        };
        let affected = tx.execute_with_params(sql, params).await?;
        Ok(affected as usize)
    }
}

/// 空串转 `None`（避免把空描述写成空串而不是 NULL）。
fn non_empty(value: &str) -> Option<&str> {
    if value.is_empty() { None } else { Some(value) }
}

/// 字典项导出定义。
pub struct DictItemExport;

#[async_trait]
impl ExportDefinition for DictItemExport {
    fn biz_type(&self) -> &'static str {
        super::DICT_ITEM_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "字典项"
    }

    fn columns(&self) -> &'static [ExportColumn] {
        EXPORT_COLUMNS
    }

    async fn count(&self, filter: &ExportFilter) -> std::result::Result<i64, DbError> {
        let mut sql = String::from(
            "SELECT COUNT(*) FROM sys_dict_item i \
             INNER JOIN sys_dict_type t ON t.id = i.dict_type_id \
             WHERE i.deleted_at IS NULL AND t.deleted_at IS NULL",
        );
        let mut params: Vec<Value> = Vec::new();
        if let Some(pattern) = crate::utils::like_pattern(filter.keyword.as_deref()) {
            sql.push_str(" AND (t.dict_code LIKE ? OR i.item_value LIKE ? OR i.item_label LIKE ?)");
            params.push(Value::from(pattern.clone()));
            params.push(Value::from(pattern.clone()));
            params.push(Value::from(pattern));
        }
        if let Some(status) = filter.status {
            sql.push_str(" AND i.status = ?");
            params.push(Value::from(i32::from(status)));
        }
        db().scalar_with_params(&sql, params).await
    }

    async fn fetch(
        &self,
        filter: &ExportFilter,
        after_id: Option<&str>,
        limit: u32,
    ) -> std::result::Result<Vec<(String, HashMap<&'static str, String>)>, DbError> {
        let mut sql = String::from(
            "SELECT i.id::text AS id, t.dict_code::text AS dict_type_code, \
                    i.item_value::text AS item_value, i.item_label::text AS item_label, \
                    COALESCE(i.description::text, '') AS description, \
                    i.sort_order::text AS sort_order, i.is_default::text AS is_default, \
                    i.status::text AS status \
             FROM sys_dict_item i \
             INNER JOIN sys_dict_type t ON t.id = i.dict_type_id \
             WHERE i.deleted_at IS NULL AND t.deleted_at IS NULL",
        );
        let mut params: Vec<Value> = Vec::new();
        if let Some(pattern) = crate::utils::like_pattern(filter.keyword.as_deref()) {
            sql.push_str(" AND (t.dict_code LIKE ? OR i.item_value LIKE ? OR i.item_label LIKE ?)");
            params.push(Value::from(pattern.clone()));
            params.push(Value::from(pattern.clone()));
            params.push(Value::from(pattern));
        }
        if let Some(status) = filter.status {
            sql.push_str(" AND i.status = ?");
            params.push(Value::from(i32::from(status)));
        }
        if let Some(after) = after_id {
            // `?::uuid` 位置必须绑 uuid 值：传 `Value::Text`（字符串）会在驱动侧就
            // 「error serializing parameter」失败（A4 实测：10 万行导出的第 2 页必挂，
            // 因为第一页之后才开始带 after_id）。
            let after = uuid::Uuid::parse_str(after).map_err(|error| {
                DbError::InvalidValue(format!("after_id 不是合法 UUID（{after}）：{error}"))
            })?;
            sql.push_str(" AND i.id > ?::uuid");
            params.push(Value::from(after));
        }
        sql.push_str(" ORDER BY i.id ASC LIMIT ?");
        params.push(Value::from(i64::from(limit)));

        let rows = db().find_with_params(&sql, params).await?;
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
    use crate::business::impexp::definition::{ImportMode, RowValues};

    fn row(pairs: &[(&'static str, &str)]) -> RowValues {
        let mut values = RowValues {
            row_no: 2,
            ..Default::default()
        };
        for (key, value) in pairs {
            values.fields.insert(key, (*value).to_string());
        }
        values
    }

    #[test]
    fn import_definition_shape_is_consistent_with_export() {
        assert_eq!(DictItemImport.biz_type(), super::super::DICT_ITEM_BIZ_TYPE);
        assert_eq!(
            DictItemImport.columns().len(),
            DictItemExport.columns().len(),
            "导入导出列数应一致"
        );
        // 导入列的中文表头必须能在导出列里找到（同一套字段语义）
        for column in DictItemImport.columns() {
            assert!(
                DictItemExport
                    .columns()
                    .iter()
                    .any(|export| export.key == column.key && export.header == column.header),
                "导入列 {} 在导出定义里没有对应列",
                column.key
            );
        }
        assert_eq!(DictItemImport.template_version(), 1);
        assert_eq!(
            DictItemImport.columns()[0]
                .fk
                .expect("字典编码应有外键")
                .table,
            "sys_dict_type"
        );
    }

    #[test]
    fn unique_key_covers_type_code_and_item_value() {
        let unique = DictItemImport
            .columns()
            .iter()
            .filter(|column| column.unique_key)
            .map(|column| column.key)
            .collect::<Vec<_>>();
        assert_eq!(unique, vec!["dict_type_code", "item_value"]);
    }

    #[test]
    fn defaults_are_applied_when_optional_columns_are_empty() {
        // 空的可选列：sort_order 按 0、is_default 按 false、status 按 1（在 persist 里落默认值）
        let values = row(&[
            ("dict_type_code", "user_status"),
            ("item_value", "1"),
            ("item_label", "启用"),
            ("sort_order", ""),
            ("is_default", ""),
            ("status", ""),
        ]);
        assert_eq!(values.get("sort_order").parse::<i32>().unwrap_or(0), 0);
        assert_ne!(values.get("is_default"), "true");
        assert_eq!(values.get("status").parse::<i16>().unwrap_or(1), 1);
        assert_eq!(non_empty(""), None);
        assert_eq!(non_empty("x"), Some("x"));
    }

    #[test]
    fn export_filter_defaults_are_empty() {
        let filter = ExportFilter::default();
        assert!(filter.keyword.is_none());
        assert!(filter.status.is_none());
        assert_eq!(ImportMode::Strict.as_str(), "STRICT");
    }
}
