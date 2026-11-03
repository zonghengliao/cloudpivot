//! 物料主数据导入（`biz_material`）。
//!
//! 列：编码 / 名称 / 规格 / 基本单位 / 分类 / 成本法 / 安全库存 / 状态。
//! 幂等键：`material_code`。
//!
//! 「基本单位」在 Excel 里写**单位编码**（`biz_unit.unit_code`），入库时 JOIN 换成 `unit_id`；
//! 该列挂 `FkSpec`，预检阶段就能报「基本单位不存在，请先在计量单位中维护」（行号 + 列名 + 原因）。
//!
//! 「成本法」的值域只有移动加权平均（V0201 收敛：FIFO / STANDARD 从未实现，选了会静默按
//! 移动加权算 —— 所以这里只给一个可选值，写别的值在预检阶段就报 ENUM 错）。

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, EnumItem, FkSpec, ImportColumn, ImportCtx, ImportDefinition, RowValues,
};

use super::{
    BATCH_SIZE, MAX_ROWS, STATUS_ITEMS, conflict_clause, duplicate_strategy, missing_rows_error,
    operator_text,
};

/// 物料分类（落库码值与 `ck_biz_material_type` 一致）。
const MATERIAL_TYPE_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "原料",
        code: "RAW",
    },
    EnumItem {
        label: "半成品",
        code: "SEMI",
    },
    EnumItem {
        label: "成品",
        code: "FINISHED",
    },
    EnumItem {
        label: "服务",
        code: "SERVICE",
    },
];

/// 计价方式（V0201 之后只剩移动加权平均）。
const COSTING_ITEMS: &[EnumItem] = &[EnumItem {
    label: "移动加权平均",
    code: "MOVING_AVG",
}];

/// 导入列定义（顺序即模板列顺序）。
static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: "material_code",
        header: "物料编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(40) },
        unique_key: true,
        fk: None,
        example: "MAT-DEMO01",
        width: Some(18),
        remark: "物料唯一编码（≤40 字符）；已存在的编码按「重复数据策略」更新或跳过",
    },
    ImportColumn {
        key: "material_name",
        header: "物料名称",
        required: true,
        kind: ColumnKind::Text { max_len: Some(120) },
        unique_key: false,
        fk: None,
        example: "六角螺栓",
        width: Some(24),
        remark: "名称（≤120 字符）",
    },
    ImportColumn {
        key: "spec",
        header: "规格型号",
        required: false,
        kind: ColumnKind::Text { max_len: Some(200) },
        unique_key: false,
        fk: None,
        example: "M8x40 不锈钢",
        width: Some(24),
        remark: "可选",
    },
    ImportColumn {
        key: "unit_code",
        header: "基本单位",
        required: true,
        kind: ColumnKind::Text { max_len: Some(20) },
        unique_key: false,
        fk: Some(FkSpec {
            table: "biz_unit",
            column: "unit_code",
            label: "计量单位",
            soft_delete: true,
        }),
        example: "PCS",
        width: Some(12),
        remark: "填计量单位编码（如 PCS / KG），须已在「计量单位」中维护",
    },
    ImportColumn {
        key: "material_type",
        header: "分类",
        required: false,
        kind: ColumnKind::Enum(MATERIAL_TYPE_ITEMS),
        unique_key: false,
        fk: None,
        example: "原料",
        width: Some(12),
        remark: "原料 / 半成品 / 成品 / 服务，留空按「原料」",
    },
    ImportColumn {
        key: "costing_method",
        header: "成本法",
        required: false,
        kind: ColumnKind::Enum(COSTING_ITEMS),
        unique_key: false,
        fk: None,
        example: "移动加权平均",
        width: Some(16),
        remark: "本版本只支持「移动加权平均」，留空按该值",
    },
    ImportColumn {
        key: "safety_stock",
        header: "安全库存",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 4,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "100",
        width: Some(14),
        remark: "≥0，最多 4 位小数；留空按 0",
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

/// upsert 时更新哪些列（`unit_id` 跟着改：单位换算是主数据自身的属性）。
const UPSERT_SET: &str = "material_name = EXCLUDED.material_name, \
     spec = EXCLUDED.spec, \
     unit_id = EXCLUDED.unit_id, \
     material_type = EXCLUDED.material_type, \
     safety_stock = EXCLUDED.safety_stock, \
     costing_method = EXCLUDED.costing_method, \
     status = EXCLUDED.status, \
     updated_at = CURRENT_TIMESTAMP, \
     update_by = EXCLUDED.update_by";

/// 物料导入定义。
pub struct MaterialImport;

#[async_trait]
impl ImportDefinition for MaterialImport {
    fn biz_type(&self) -> &'static str {
        super::MATERIAL_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "物料"
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
                    let material_type = super::text_of(row, "material_type");
                    serde_json::json!({
                        "material_code": row.get("material_code"),
                        "material_name": row.get("material_name"),
                        "spec": super::text_of(row, "spec"),
                        "unit_code": row.get("unit_code"),
                        "material_type": material_type.unwrap_or_else(|| "RAW".to_string()),
                        "costing_method": super::text_of(row, "costing_method")
                            .unwrap_or_else(|| "MOVING_AVG".to_string()),
                        "safety_stock": super::num_of(row, "safety_stock"),
                        "status": super::status_of(row, "status"),
                    })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| DbError::InvalidValue(format!("物料序列化失败：{error}")))?;

        let sql = format!(
            "INSERT INTO biz_material \
                 (id, material_code, material_name, spec, unit_id, material_type, \
                  safety_stock, costing_method, status, created_at, updated_at, \
                  create_by, update_by) \
             SELECT gen_random_uuid(), v.material_code, v.material_name, v.spec, u.id, \
                    v.material_type, COALESCE(v.safety_stock, 0), v.costing_method, v.status, \
                    CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
                    NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
             FROM jsonb_to_recordset(?::jsonb) AS v( \
                    material_code text, material_name text, spec text, unit_code text, \
                    material_type text, costing_method text, safety_stock numeric, \
                    status smallint) \
             INNER JOIN biz_unit u ON u.unit_code = v.unit_code AND u.deleted_at IS NULL \
             ON CONFLICT (material_code) WHERE deleted_at IS NULL {}",
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
        // STRICT 语义：JOIN 掉的行不能悄悄变少（预检后被删的单位会让这里少行）
        if strategy == crate::business::impexp::definition::DuplicateStrategy::Upsert
            && affected as usize != rows.len()
        {
            return Err(missing_rows_error(
                rows.len(),
                affected as usize,
                "基本单位可能在预检后被删除，请重新上传",
            ));
        }
        Ok(affected as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::impexp::definition::{ColumnKind, ErrorKind};

    #[test]
    fn column_contract_is_stable() {
        let keys = COLUMNS.iter().map(|column| column.key).collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "material_code",
                "material_name",
                "spec",
                "unit_code",
                "material_type",
                "costing_method",
                "safety_stock",
                "status"
            ]
        );
        let unit = &COLUMNS[3];
        assert!(unit.required);
        let spec = unit.fk.expect("基本单位应有外键");
        assert_eq!(spec.table, "biz_unit");
        assert_eq!(spec.column, "unit_code");
        assert!(spec.soft_delete);
    }

    #[test]
    fn costing_method_rejects_retired_values() {
        // V0201：FIFO / STANDARD 已收敛掉，导入必须当场报错而不是静默按移动加权算
        let kind = COLUMNS[5].kind;
        assert_eq!(
            kind.parse("移动加权平均", None).expect("中文值应合法"),
            "MOVING_AVG"
        );
        let issue = kind.parse("FIFO", None).expect_err("FIFO 应被拒");
        assert_eq!(issue.kind, ErrorKind::Enum);
        assert!(issue.message.contains("移动加权平均"));
    }

    #[test]
    fn material_type_is_closed_set() {
        let kind = COLUMNS[4].kind;
        for (label, code) in [
            ("原料", "RAW"),
            ("半成品", "SEMI"),
            ("成品", "FINISHED"),
            ("服务", "SERVICE"),
        ] {
            assert_eq!(kind.parse(label, None).expect("应合法"), code);
        }
        assert_eq!(
            kind.parse("包材", None).expect_err("未定义分类").kind,
            ErrorKind::Enum
        );
        assert!(matches!(kind, ColumnKind::Enum(_)));
    }

    #[test]
    fn safety_stock_rejects_negative() {
        let kind = COLUMNS[6].kind;
        assert_eq!(kind.parse("0", None).expect("下界应合法"), "0");
        assert_eq!(
            kind.parse("-1", None).expect_err("负数应被拒").kind,
            ErrorKind::Range
        );
    }
}
