//! 仓库主数据导入（`biz_warehouse`）。
//!
//! 列：编码 / 名称 / 仓库类型 / 地址 / 负责人 / 是否允许负库存 / 状态。
//! 幂等键：`warehouse_code`。
//!
//! # 只导「一级仓库」，不导层级
//!
//! `biz_warehouse` 是树（`parent_id` / `level` / `warehouse_path` 物化路径 / `is_leaf`）。
//! 本定义把每一行落成**一级仓库**（`level = 1`、`parent_id = NULL`、
//! `warehouse_path = '/' || id || '/'`、`is_leaf = 1` 即可挂库位）：
//! 层级维护要跟「子仓库搬移 = 整棵子树改路径」绑在一起，属于仓库管理页的交互，
//! 不是「填几百条主数据」的场景。已存在的仓库重导时**不动**这四列（改层级必须在页面上改），
//! 否则一次导入就会把已有子树接错父级。

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, DuplicateStrategy, EnumItem, ImportColumn, ImportCtx, ImportDefinition, RowValues,
};

use super::{
    BATCH_SIZE, MAX_ROWS, STATUS_ITEMS, conflict_clause, duplicate_strategy, flag_of, operator_text, status_of,
};

/// 仓库类型（落库码值与 `ck_biz_warehouse_type` 一致）。
const TYPE_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "普通仓",
        code: "NORMAL",
    },
    EnumItem {
        label: "原料仓",
        code: "RAW",
    },
    EnumItem {
        label: "成品仓",
        code: "FINISHED",
    },
    EnumItem {
        label: "废品仓",
        code: "SCRAP",
    },
    EnumItem {
        label: "在途仓",
        code: "IN_TRANSIT",
    },
];

/// 导入列定义（顺序即模板列顺序）。
static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: "warehouse_code",
        header: "仓库编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(40) },
        unique_key: true,
        fk: None,
        example: "WH-DEMO01",
        width: Some(16),
        remark: "仓库唯一编码（≤40 字符，如 WH01）",
    },
    ImportColumn {
        key: "warehouse_name",
        header: "仓库名称",
        required: true,
        kind: ColumnKind::Text { max_len: Some(120) },
        unique_key: false,
        fk: None,
        example: "北京中心仓",
        width: Some(22),
        remark: "仓库名称（≤120 字符）",
    },
    ImportColumn {
        key: "warehouse_type",
        header: "仓库类型",
        required: false,
        kind: ColumnKind::Enum(TYPE_ITEMS),
        unique_key: false,
        fk: None,
        example: "普通仓",
        width: Some(14),
        remark: "普通仓 / 原料仓 / 成品仓 / 废品仓 / 在途仓，留空按「普通仓」",
    },
    ImportColumn {
        key: "address",
        header: "地址",
        required: false,
        kind: ColumnKind::Text { max_len: Some(255) },
        unique_key: false,
        fk: None,
        example: "北京市海淀区上地十街 10 号",
        width: Some(30),
        remark: "可选",
    },
    ImportColumn {
        key: "manager",
        header: "负责人",
        required: false,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: false,
        fk: None,
        example: "李四",
        width: Some(14),
        remark: "可选（姓名文本，不是账号）",
    },
    ImportColumn {
        key: "allow_negative",
        header: "是否允许负库存",
        required: false,
        kind: ColumnKind::Bool,
        unique_key: false,
        fk: None,
        example: "否",
        width: Some(16),
        remark: "是 / 否，留空按「否」（禁止负库存）",
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

/// upsert 时更新哪些列（**不含** `parent_id` / `level` / `warehouse_path` / `is_leaf`）。
const UPSERT_SET: &str = "warehouse_name = EXCLUDED.warehouse_name, \
     warehouse_type = EXCLUDED.warehouse_type, \
     manager = EXCLUDED.manager, \
     allow_negative = EXCLUDED.allow_negative, \
     address = EXCLUDED.address, \
     status = EXCLUDED.status, \
     updated_at = CURRENT_TIMESTAMP, \
     update_by = EXCLUDED.update_by";

/// 仓库导入定义。
pub struct WarehouseImport;

#[async_trait]
impl ImportDefinition for WarehouseImport {
    fn biz_type(&self) -> &'static str {
        super::WAREHOUSE_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "仓库"
    }

    fn max_rows(&self) -> usize {
        MAX_ROWS
    }

    fn batch_size(&self) -> usize {
        BATCH_SIZE
    }

    fn allow_partial(&self) -> bool {
        true
    }

    fn columns(&self) -> &'static [ImportColumn] {
        COLUMNS
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
        let strategy = duplicate_strategy(ctx).await?;
        let payload = serde_json::to_string(
            &rows
                .iter()
                .map(|row| {
                    // 主键在 Rust 侧生成：`warehouse_path` 必须等于 `'/' || id || '/'`，
                    // 让 SQL 再 `RETURNING id` 拼路径会多一次往返。
                    let id = uuid::Uuid::now_v7();
                    serde_json::json!({
                        "id": id.to_string(),
                        "warehouse_code": row.get("warehouse_code"),
                        "warehouse_name": row.get("warehouse_name"),
                        "warehouse_type": super::text_of(row, "warehouse_type")
                            .unwrap_or_else(|| "NORMAL".to_string()),
                        "manager": super::text_of(row, "manager"),
                        "allow_negative": flag_of(row, "allow_negative"),
                        "address": super::text_of(row, "address"),
                        "status": status_of(row, "status"),
                    })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| DbError::InvalidValue(format!("仓库序列化失败：{error}")))?;

        let sql = format!(
            "INSERT INTO biz_warehouse \
                 (id, warehouse_code, warehouse_name, warehouse_type, manager, allow_negative, \
                  address, status, parent_id, level, warehouse_path, is_leaf, \
                  created_at, updated_at, create_by, update_by) \
             SELECT v.id::uuid, v.warehouse_code, v.warehouse_name, v.warehouse_type, v.manager, \
                    v.allow_negative, v.address, v.status, NULL, 1, '/' || v.id || '/', 1, \
                    CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
                    NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
             FROM jsonb_to_recordset(?::jsonb) AS v( \
                    id text, warehouse_code text, warehouse_name text, warehouse_type text, \
                    manager text, allow_negative smallint, address text, status smallint) \
             ON CONFLICT (warehouse_code) WHERE deleted_at IS NULL {}",
            conflict_clause(strategy, UPSERT_SET)
        );
        let operator = operator_text(ctx);
        let affected = tx
            .execute_with_params(
                &sql,
                vec![
                    Value::from(operator.clone()),
                    Value::from(operator),
                    Value::Json(payload),
                ],
            )
            .await?;
        if strategy == DuplicateStrategy::Upsert && affected as usize != rows.len() {
            return Err(super::missing_rows_error(rows.len(), affected as usize, "请重新上传"));
        }
        Ok(affected as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_contract_is_stable() {
        assert_eq!(COLUMNS.len(), 7);
        assert!(COLUMNS[0].required && COLUMNS[0].unique_key);
        assert!(COLUMNS[1].required);
        assert!(COLUMNS.iter().all(|column| column.fk.is_none()));
    }

    #[test]
    fn upsert_never_touches_tree_columns() {
        // 改层级必须在仓库管理页做（子仓库搬移 = 整棵子树改路径）；导入只更新描述性字段
        for column in ["warehouse_path", "parent_id", "level", "is_leaf"] {
            assert!(!UPSERT_SET.contains(column), "upsert 不应更新树结构列 {column}");
        }
        assert!(UPSERT_SET.contains("warehouse_name"));
    }

    #[test]
    fn warehouse_type_labels_match_check_constraint() {
        let kind = COLUMNS[2].kind;
        for (label, code) in [
            ("普通仓", "NORMAL"),
            ("原料仓", "RAW"),
            ("成品仓", "FINISHED"),
            ("废品仓", "SCRAP"),
            ("在途仓", "IN_TRANSIT"),
        ] {
            assert_eq!(kind.parse(label, None).expect("应合法"), code);
        }
        assert!(kind.parse("虚拟仓", None).is_err());
    }

    #[test]
    fn allow_negative_defaults_to_forbidden() {
        let mut row = RowValues {
            row_no: 2,
            ..Default::default()
        };
        assert_eq!(flag_of(&row, "allow_negative"), 0);
        row.fields.insert("allow_negative", "true".to_string());
        assert_eq!(flag_of(&row, "allow_negative"), 1);
    }
}
