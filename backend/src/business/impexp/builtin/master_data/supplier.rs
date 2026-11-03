//! 供应商主数据导入（`biz_supplier`）。
//!
//! 列：编码 / 名称 / 简称 / 联系人 / 电话 / 税号 / 信用额度 / 状态。
//! 幂等键：`supplier_code`（`uk_biz_supplier_code` 带 `WHERE deleted_at IS NULL`）。

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, ImportColumn, ImportCtx, ImportDefinition, RowValues,
};

use super::{
    BATCH_SIZE, MAX_ROWS, STATUS_ITEMS, conflict_clause, duplicate_strategy, operator_text,
};

/// 导入列定义（顺序即模板列顺序）。
static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: "supplier_code",
        header: "供应商编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(40) },
        unique_key: true,
        fk: None,
        example: "SUP-DEMO01",
        width: Some(18),
        remark: "供应商唯一编码（≤40 字符）；已存在的编码按「重复数据策略」更新或跳过",
    },
    ImportColumn {
        key: "supplier_name",
        header: "供应商名称",
        required: true,
        kind: ColumnKind::Text { max_len: Some(200) },
        unique_key: false,
        fk: None,
        example: "北京云枢供应链有限公司",
        width: Some(30),
        remark: "全称（开票口径）",
    },
    ImportColumn {
        key: "short_name",
        header: "简称",
        required: false,
        kind: ColumnKind::Text { max_len: Some(100) },
        unique_key: false,
        fk: None,
        example: "云枢供应链",
        width: Some(20),
        remark: "可选，列表与单据上展示的短名",
    },
    ImportColumn {
        key: "contact_person",
        header: "联系人",
        required: false,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: false,
        fk: None,
        example: "张三",
        width: Some(14),
        remark: "可选",
    },
    ImportColumn {
        key: "contact_phone",
        header: "联系电话",
        required: false,
        kind: ColumnKind::Text { max_len: Some(30) },
        unique_key: false,
        fk: None,
        example: "13800000000",
        width: Some(16),
        remark: "可选；建议写成文本格式，避免 Excel 把前导 0 吃掉",
    },
    ImportColumn {
        key: "tax_no",
        header: "纳税人识别号",
        required: false,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: false,
        fk: None,
        example: "91110108MA01ABCD12",
        width: Some(22),
        remark: "可选",
    },
    ImportColumn {
        key: "credit_limit",
        header: "信用额度",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 4,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "100000",
        width: Some(14),
        remark: "≥0，最多 4 位小数；留空按 0（0 = 不限）",
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

/// upsert 时更新哪些列（`id` / `created_at` / `create_by` 保持首次入库的值）。
const UPSERT_SET: &str = "supplier_name = EXCLUDED.supplier_name, \
     short_name = EXCLUDED.short_name, \
     contact_person = EXCLUDED.contact_person, \
     contact_phone = EXCLUDED.contact_phone, \
     tax_no = EXCLUDED.tax_no, \
     credit_limit = EXCLUDED.credit_limit, \
     status = EXCLUDED.status, \
     updated_at = CURRENT_TIMESTAMP, \
     update_by = EXCLUDED.update_by";

/// 供应商导入定义。
pub struct SupplierImport;

#[async_trait]
impl ImportDefinition for SupplierImport {
    fn biz_type(&self) -> &'static str {
        super::SUPPLIER_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "供应商"
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
                    serde_json::json!({
                        "supplier_code": row.get("supplier_code"),
                        "supplier_name": row.get("supplier_name"),
                        "short_name": super::text_of(row, "short_name"),
                        "contact_person": super::text_of(row, "contact_person"),
                        "contact_phone": super::text_of(row, "contact_phone"),
                        "tax_no": super::text_of(row, "tax_no"),
                        "credit_limit": super::num_of(row, "credit_limit"),
                        "status": super::status_of(row, "status"),
                    })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| DbError::InvalidValue(format!("供应商序列化失败：{error}")))?;

        let sql = format!(
            "INSERT INTO biz_supplier \
                 (id, supplier_code, supplier_name, short_name, contact_person, contact_phone, \
                  tax_no, credit_limit, status, created_at, updated_at, create_by, update_by) \
             SELECT gen_random_uuid(), v.supplier_code, v.supplier_name, v.short_name, \
                    v.contact_person, v.contact_phone, v.tax_no, COALESCE(v.credit_limit, 0), \
                    v.status, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
                    NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
             FROM jsonb_to_recordset(?::jsonb) AS v( \
                    supplier_code text, supplier_name text, short_name text, \
                    contact_person text, contact_phone text, tax_no text, \
                    credit_limit numeric, status smallint) \
             ON CONFLICT (supplier_code) WHERE deleted_at IS NULL {}",
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
        Ok(affected as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_contract_is_stable() {
        let keys = COLUMNS.iter().map(|column| column.key).collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "supplier_code",
                "supplier_name",
                "short_name",
                "contact_person",
                "contact_phone",
                "tax_no",
                "credit_limit",
                "status"
            ]
        );
        assert!(COLUMNS[0].required && COLUMNS[0].unique_key);
        assert!(COLUMNS[1].required);
        assert_eq!(
            COLUMNS
                .iter()
                .filter(|column| column.required)
                .map(|column| column.key)
                .collect::<Vec<_>>(),
            vec!["supplier_code", "supplier_name"]
        );
        assert!(COLUMNS.iter().all(|column| column.fk.is_none()));
    }
}
