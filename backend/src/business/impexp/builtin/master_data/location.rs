//! 库位导入（`biz_location`）。
//!
//! 列：库位编码 / 名称 / 所属仓库编码 / 库位类型 / 是否默认 / 容量 / 状态。
//!
//! # 幂等键是「仓库 + 库位编码」，不是库位编码本身
//!
//! `uk_biz_location_code` 是 `(warehouse_id, location_code)` —— `A-01-03` 在每个仓库都该这么叫，
//! 所以模板里同时有「库位编码」与「所属仓库编码」两列，两列都参与文件内唯一校验（重复上传先报
//! `UNIQUE`，而不是等数据库撞索引）。
//!
//! # 两条跨行规则（框架只有逐行校验，做不到，所以在 `persist` 里前置检查）
//!
//! 1. **同仓最多一个默认库位**（`uk_biz_location_default`）：同一份文件里同一仓库出现两行
//!    「是否默认=是」→ 直接报错并给出两行行号；只出现一行 → 先把该仓其它库位的默认标记清掉再落库
//!    （与页面新建库位的行为一致：置默认即抢占）。
//! 2. **库位只能挂在叶子仓库下**：`validate_row` 逐行查一次仓库（`is_leaf`），非叶子仓库
//!    当场报错带行号；`persist` 的 JOIN 再兜一次（预检后仓库被删/加了子仓库 → 整批回滚，
//!    不静默丢行）。

use std::collections::HashMap;

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, DuplicateStrategy, EnumItem, FkSpec, ImportColumn, ImportCtx, ImportDefinition,
    RowIssue, RowValues,
};

use super::{
    BATCH_SIZE, MAX_ROWS, STATUS_ITEMS, column_of, conflict_clause, duplicate_strategy, flag_of,
    operator_text, rule, status_of,
};

/// 库位类型（落库码值与 `ck_biz_location_type` 一致）。
const TYPE_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "存储",
        code: "STORAGE",
    },
    EnumItem {
        label: "拣货",
        code: "PICKING",
    },
    EnumItem {
        label: "收货",
        code: "RECEIVING",
    },
    EnumItem {
        label: "发货",
        code: "SHIPPING",
    },
    EnumItem {
        label: "待检",
        code: "QUARANTINE",
    },
    EnumItem {
        label: "报废",
        code: "SCRAP",
    },
];

/// 导入列定义（顺序即模板列顺序）。
static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: "location_code",
        header: "库位编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(40) },
        unique_key: true,
        fk: None,
        example: "A-01-01",
        width: Some(16),
        remark: "仓库内唯一（如 A-01-03）；不同仓库可以重名",
    },
    ImportColumn {
        key: "warehouse_code",
        header: "所属仓库编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(40) },
        unique_key: true,
        fk: Some(FkSpec {
            table: "biz_warehouse",
            column: "warehouse_code",
            label: "仓库",
            soft_delete: true,
        }),
        example: "WH01",
        width: Some(16),
        remark: "填仓库编码，须已在「仓库」中维护且是**叶子**仓库（还有下级仓库的不能挂库位）",
    },
    ImportColumn {
        key: "location_name",
        header: "库位名称",
        required: false,
        kind: ColumnKind::Text { max_len: Some(120) },
        unique_key: false,
        fk: None,
        example: "A 区一排一列",
        width: Some(20),
        remark: "可选（如 A区一排三列）",
    },
    ImportColumn {
        key: "location_type",
        header: "库位类型",
        required: false,
        kind: ColumnKind::Enum(TYPE_ITEMS),
        unique_key: false,
        fk: None,
        example: "存储",
        width: Some(12),
        remark: "存储 / 拣货 / 收货 / 发货 / 待检 / 报废，留空按「存储」",
    },
    ImportColumn {
        key: "is_default",
        header: "是否默认",
        required: false,
        kind: ColumnKind::Bool,
        unique_key: false,
        fk: None,
        example: "否",
        width: Some(12),
        remark: "是 / 否，留空按「否」；每仓最多一个默认（该仓其它库位的默认标记会被清掉）",
    },
    ImportColumn {
        key: "capacity",
        header: "容量",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 4,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "1000",
        width: Some(12),
        remark: "≥0，最多 4 位小数；留空表示不限",
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

/// upsert 时更新哪些列（`warehouse_id` 不在其中：换仓库等于换身份，应重建库位）。
const UPSERT_SET: &str = "location_name = EXCLUDED.location_name, \
     location_type = EXCLUDED.location_type, \
     is_default = EXCLUDED.is_default, \
     capacity = EXCLUDED.capacity, \
     status = EXCLUDED.status, \
     updated_at = CURRENT_TIMESTAMP, \
     update_by = EXCLUDED.update_by";

/// 库位导入定义。
pub struct LocationImport;

#[async_trait]
impl ImportDefinition for LocationImport {
    fn biz_type(&self) -> &'static str {
        super::LOCATION_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "库位"
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

    async fn validate_row(
        &self,
        row: &RowValues,
        _ctx: &ImportCtx,
    ) -> std::result::Result<(), Vec<RowIssue>> {
        let warehouse_code = row.get("warehouse_code");
        if warehouse_code.is_empty() {
            // 必填与「仓库不存在」由框架统一报，这里不重复报同一格
            return Ok(());
        }
        let found = db()
            .find_with_params(
                "SELECT warehouse_name::text AS warehouse_name, is_leaf::text AS is_leaf \
                 FROM biz_warehouse WHERE warehouse_code = ? AND deleted_at IS NULL",
                vec![Value::from(warehouse_code)],
            )
            .await;
        let rows = match found {
            Ok(rows) => rows,
            Err(error) => {
                return Err(vec![rule(
                    row,
                    column_of(COLUMNS, "warehouse_code"),
                    format!("校验所属仓库时数据库异常，请稍后重试（{error}）"),
                )]);
            }
        };
        let Some(record) = rows.into_iter().next() else {
            // 不存在：交给 `FkSpec` 的统一文案（「请先在仓库中维护」）
            return Ok(());
        };
        let name: String = record
            .get_typed("warehouse_name")
            .unwrap_or_else(|_| warehouse_code.to_string());
        let is_leaf: String = record
            .get_typed("is_leaf")
            .unwrap_or_else(|_| "1".to_string());
        if is_leaf == "0" {
            return Err(vec![rule(
                row,
                column_of(COLUMNS, "warehouse_code"),
                format!(
                    "所属仓库「{name}」（{warehouse_code}）还有下级仓库，不是叶子仓库；\
                     库位只能挂在叶子仓库下"
                ),
            )]);
        }
        Ok(())
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

        // ---------- 1. 跨行规则：同一仓库只能有一个默认库位 ----------
        let mut defaults: HashMap<&str, u32> = HashMap::new();
        for row in rows.iter().filter(|row| flag_of(row, "is_default") == 1) {
            let warehouse = row.get("warehouse_code");
            if let Some(first) = defaults.insert(warehouse, row.row_no) {
                return Err(DbError::InvalidValue(format!(
                    "第 {first} 行与第 {} 行都标成「默认库位」，但同属仓库「{warehouse}」——\
                     每仓最多一个默认库位，请只保留一行",
                    row.row_no
                )));
            }
        }

        // ---------- 2. 抢占默认位：先清掉这些仓库里其它库位的默认标记 ----------
        if !defaults.is_empty() {
            let payload = serde_json::to_string(&defaults.keys().collect::<Vec<_>>())
                .map_err(|error| DbError::InvalidValue(format!("仓库编码序列化失败：{error}")))?;
            tx.execute_with_params(
                "UPDATE biz_location l SET is_default = 0, updated_at = CURRENT_TIMESTAMP \
                 FROM biz_warehouse w \
                 WHERE l.warehouse_id = w.id AND l.deleted_at IS NULL AND l.is_default = 1 \
                   AND w.deleted_at IS NULL \
                   AND w.warehouse_code::text IN (SELECT jsonb_array_elements_text(?::jsonb))",
                vec![Value::Json(payload)],
            )
            .await?;
        }

        // ---------- 3. 一条语句落库 ----------
        let payload = serde_json::to_string(
            &rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "location_code": row.get("location_code"),
                        "warehouse_code": row.get("warehouse_code"),
                        "location_name": super::text_of(row, "location_name"),
                        "location_type": super::text_of(row, "location_type")
                            .unwrap_or_else(|| "STORAGE".to_string()),
                        "is_default": flag_of(row, "is_default"),
                        "capacity": super::num_of(row, "capacity"),
                        "status": status_of(row, "status"),
                    })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| DbError::InvalidValue(format!("库位序列化失败：{error}")))?;

        let sql = format!(
            "INSERT INTO biz_location \
                 (id, location_code, location_name, warehouse_id, location_type, is_default, \
                  capacity, status, created_at, updated_at, create_by, update_by) \
             SELECT gen_random_uuid(), v.location_code, v.location_name, w.id, v.location_type, \
                    v.is_default, v.capacity, v.status, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
                    NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
             FROM jsonb_to_recordset(?::jsonb) AS v( \
                    location_code text, warehouse_code text, location_name text, \
                    location_type text, is_default smallint, capacity numeric, status smallint) \
             INNER JOIN biz_warehouse w ON w.warehouse_code = v.warehouse_code \
                    AND w.deleted_at IS NULL AND w.is_leaf = 1 \
             ON CONFLICT (warehouse_id, location_code) WHERE deleted_at IS NULL {}",
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
            return Err(super::missing_rows_error(
                rows.len(),
                affected as usize,
                "所属仓库可能已被删除或不再是叶子仓库，请重新上传",
            ));
        }
        Ok(affected as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_key_is_warehouse_plus_code() {
        let unique = COLUMNS
            .iter()
            .filter(|column| column.unique_key)
            .map(|column| column.key)
            .collect::<Vec<_>>();
        assert_eq!(unique, vec!["location_code", "warehouse_code"]);
        let fk = COLUMNS[1].fk.expect("所属仓库应有外键");
        assert_eq!(fk.table, "biz_warehouse");
        assert_eq!(fk.column, "warehouse_code");
        assert!(fk.soft_delete);
    }

    #[test]
    fn upsert_never_moves_a_location_between_warehouses() {
        assert!(!UPSERT_SET.contains("warehouse_id"));
        assert!(UPSERT_SET.contains("is_default"));
    }

    #[test]
    fn capacity_is_optional_but_never_negative() {
        let kind = COLUMNS[5].kind;
        assert_eq!(kind.parse("", None).expect("空值交必填判定"), "");
        assert_eq!(kind.parse("0", None).expect("0 应合法"), "0");
        assert!(kind.parse("-5", None).is_err());
    }

    #[test]
    fn location_type_labels_match_check_constraint() {
        let kind = COLUMNS[3].kind;
        for (label, code) in [
            ("存储", "STORAGE"),
            ("拣货", "PICKING"),
            ("收货", "RECEIVING"),
            ("发货", "SHIPPING"),
            ("待检", "QUARANTINE"),
            ("报废", "SCRAP"),
        ] {
            assert_eq!(kind.parse(label, None).expect("应合法"), code);
        }
    }
}
