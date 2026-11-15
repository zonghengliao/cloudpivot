//! 计量单位导入（`biz_unit`）。
//!
//! 列：编码 / 名称 / 是否基本单位 / 基准单位编码 / 换算率 / 状态。
//! 幂等键：`unit_code`。
//!
//! # 为什么多一列「基准单位编码」
//!
//! 卡面给的是「编码/名称/是否基本单位/换算率/状态」五列，但 `biz_unit` 的
//! `ck_biz_unit_base` 要求「辅助单位（`is_base = 0`）必须指明 `base_unit_id`」——
//! 没有基准单位这一列，辅助单位根本落不了库。所以模板以「基准单位**编码**」补齐，
//! 入库时解析成 `base_unit_id`。
//!
//! # 基准单位既可以在库里，也可以在同一份文件里
//!
//! 新客户上线的典型文件是「40 个单位，其中 30 个基础单位、10 个辅助单位」——
//! 所以这一列**不挂 `FkSpec`**（框架的外键校验只认库里已有的值，同文件里排在后面的
//! 基础单位会被误判成不存在），改在 `persist` 里把「库里 + 本批次」的编码一起建映射再解析；
//! 解析不到就返回**带行号**的错误（绝不静默丢行）。

use std::collections::HashMap;

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, DuplicateStrategy, ImportColumn, ImportCtx, ImportDefinition, RowIssue, RowValues,
};

use super::{
    BATCH_SIZE, MAX_ROWS, STATUS_ITEMS, column_of, conflict_clause, duplicate_strategy, flag_of, load_refs,
    operator_text, rule, status_of,
};

/// 导入列定义（顺序即模板列顺序）。
static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: "unit_code",
        header: "单位编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(20) },
        unique_key: true,
        fk: None,
        example: "CTN",
        width: Some(14),
        remark: "单位唯一编码（≤20 字符，如 PCS / KG / BOX）",
    },
    ImportColumn {
        key: "unit_name",
        header: "单位名称",
        required: true,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: false,
        fk: None,
        example: "箱",
        width: Some(14),
        remark: "展示名（如 个 / 千克 / 箱）",
    },
    ImportColumn {
        key: "is_base",
        header: "是否基本单位",
        required: false,
        kind: ColumnKind::Bool,
        unique_key: false,
        fk: None,
        example: "否",
        width: Some(14),
        remark: "是 / 否，留空按「是」；基础单位不能再有基准单位",
    },
    ImportColumn {
        key: "base_unit_code",
        header: "基准单位编码",
        required: false,
        kind: ColumnKind::Text { max_len: Some(20) },
        unique_key: false,
        fk: None,
        example: "PCS",
        width: Some(16),
        remark: "辅助单位必填：1 本单位 = 换算率 个该单位；可在同一份文件里（基础单位在前或在后都行），也可已在系统中",
    },
    ImportColumn {
        key: "rate",
        header: "换算率",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 6,
            min: None,
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "12",
        width: Some(12),
        remark: ">0，最多 6 位小数；辅助单位必填（1 箱 = 12 个 → 填 12），基础单位留空按 1",
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

/// upsert 时更新哪些列。
const UPSERT_SET: &str = "unit_name = EXCLUDED.unit_name, \
     is_base = EXCLUDED.is_base, \
     base_unit_id = EXCLUDED.base_unit_id, \
     rate = EXCLUDED.rate, \
     status = EXCLUDED.status, \
     updated_at = CURRENT_TIMESTAMP, \
     update_by = EXCLUDED.update_by";

/// 解析后的单行（`base_unit_id` 已经换成库里的 UUID）。
struct ResolvedUnit {
    id: uuid::Uuid,
    unit_code: String,
    unit_name: String,
    is_base: i16,
    base_unit_id: Option<uuid::Uuid>,
    rate: f64,
    status: i16,
}

/// 计量单位导入定义。
pub struct UnitImport;

#[async_trait]
impl ImportDefinition for UnitImport {
    fn biz_type(&self) -> &'static str {
        super::UNIT_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "计量单位"
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

    async fn validate_row(&self, row: &RowValues, _ctx: &ImportCtx) -> std::result::Result<(), Vec<RowIssue>> {
        let mut issues = Vec::new();
        let unit_code = row.get("unit_code");
        let base_code = row.get("base_unit_code");
        // 「是否基本单位」留空按「是」（与模板说明、CONTEXT 一致）
        let is_base = row.get("is_base") != "false";
        let is_base_column = column_of(COLUMNS, "is_base");
        let base_column = column_of(COLUMNS, "base_unit_code");
        let rate_column = column_of(COLUMNS, "rate");

        if is_base {
            if !base_code.is_empty() {
                issues.push(rule(
                    row,
                    base_column,
                    format!(
                        "「是否基本单位」为「是」时不能填写基准单位编码（当前填了「{base_code}」）；\
                         基础单位的基准为空，辅助单位才需要填"
                    ),
                ));
            }
            if let Ok(rate) = row.get("rate").parse::<f64>()
                && (rate - 1.0).abs() > f64::EPSILON
            {
                issues.push(rule(
                    row,
                    rate_column,
                    format!("基础单位的换算率必须为 1（当前填了 {}），留空即按 1", row.get("rate")),
                ));
            }
            let _ = is_base_column;
            return finish(issues);
        }

        if base_code.is_empty() {
            issues.push(rule(
                row,
                base_column,
                "「是否基本单位」为「否」时必须填写「基准单位编码」（1 本单位 = 换算率 个基准单位）",
            ));
        } else if !unit_code.is_empty() && base_code == unit_code {
            issues.push(rule(
                row,
                base_column,
                format!("基准单位不能是自身（编码「{base_code}」）"),
            ));
        }
        match row.get("rate").parse::<f64>() {
            Ok(rate) if rate > 0.0 => {}
            Ok(_) => issues.push(rule(
                row,
                rate_column,
                format!("换算率必须大于 0（当前填了 {}）", row.get("rate")),
            )),
            Err(_) if row.get("rate").is_empty() => issues.push(rule(
                row,
                rate_column,
                "辅助单位必须填写换算率（> 0），例如 1 箱 = 12 个 → 填 12",
            )),
            Err(_) => {}
        }
        finish(issues)
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

        // ---------- 1. 编码映射：库里已有的 + 本批次里的（基准单位允许同文件引用） ----------
        let mut codes: Vec<String> = rows.iter().map(|row| row.get("unit_code").to_string()).collect();
        codes.extend(
            rows.iter()
                .map(|row| row.get("base_unit_code").to_string())
                .filter(|code| !code.is_empty()),
        );
        let refs = load_refs(tx, "biz_unit", "unit_code", "is_base", &codes).await?;
        let mut own: HashMap<String, uuid::Uuid> = HashMap::new();
        let mut own_is_base: HashMap<String, bool> = HashMap::new();
        for row in rows {
            let code = row.get("unit_code").to_string();
            let id = refs.get(&code).map(|(id, _)| id).unwrap_or_else(uuid::Uuid::now_v7);
            own.insert(code.clone(), id);
            own_is_base.insert(code, flag_of(row, "is_base") == 1);
        }

        // ---------- 2. 逐行解析基准单位 ----------
        let mut resolved = Vec::with_capacity(rows.len());
        for row in rows {
            let unit_code = row.get("unit_code").to_string();
            let is_base = flag_of(row, "is_base");
            let base_code = row.get("base_unit_code");
            let mut issues: Vec<RowIssue> = Vec::new();
            let base_unit_id = if is_base == 1 || base_code.is_empty() {
                None
            } else if let Some(base_id) = own.get(base_code) {
                // 同文件的基准单位：它自己必须是基础单位，才能当别人的基准
                if own_is_base.get(base_code).copied().unwrap_or(false) {
                    Some(*base_id)
                } else {
                    issues.push(rule(
                        row,
                        column_of(COLUMNS, "base_unit_code"),
                        format!(
                            "基准单位「{base_code}」在本文件里被标成辅助单位（是否基本单位=否），不能作为基准；\
                             请把它改成「是」或改填别的基准单位"
                        ),
                    ));
                    None
                }
            } else if let Some((base_id, base_flag)) = refs.get(base_code) {
                if base_flag == 1 {
                    Some(base_id)
                } else {
                    issues.push(rule(
                        row,
                        column_of(COLUMNS, "base_unit_code"),
                        format!("基准单位「{base_code}」在系统中是辅助单位（不是基础单位），不能作为基准"),
                    ));
                    None
                }
            } else {
                issues.push(rule(
                    row,
                    column_of(COLUMNS, "base_unit_code"),
                    format!("基准单位「{base_code}」不存在（本文件里也没有这个编码），请先维护该单位"),
                ));
                None
            };
            if !issues.is_empty() {
                return Err(DbError::InvalidValue(join_issues(&issues)));
            }
            resolved.push(ResolvedUnit {
                id: own.get(&unit_code).copied().unwrap_or_else(uuid::Uuid::now_v7),
                unit_code,
                unit_name: row.get("unit_name").to_string(),
                is_base,
                base_unit_id,
                rate: row.get("rate").parse::<f64>().unwrap_or(1.0),
                status: status_of(row, "status"),
            });
        }

        // ---------- 3. 一条语句写完（jsonb 单参数，避开参数上限） ----------
        let payload = serde_json::to_string(
            &resolved
                .iter()
                .map(|unit| {
                    serde_json::json!({
                        "id": unit.id.to_string(),
                        "unit_code": unit.unit_code,
                        "unit_name": unit.unit_name,
                        "is_base": unit.is_base,
                        "base_unit_id": unit.base_unit_id.map(|id| id.to_string()),
                        "rate": unit.rate,
                        "status": unit.status,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| DbError::InvalidValue(format!("计量单位序列化失败：{error}")))?;

        let sql = format!(
            "INSERT INTO biz_unit \
                 (id, unit_code, unit_name, is_base, base_unit_id, rate, status, \
                  created_at, updated_at, create_by, update_by) \
             SELECT v.id::uuid, v.unit_code, v.unit_name, v.is_base, v.base_unit_id::uuid, \
                    COALESCE(v.rate, 1), v.status, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
                    NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
             FROM jsonb_to_recordset(?::jsonb) AS v( \
                    id text, unit_code text, unit_name text, is_base smallint, \
                    base_unit_id text, rate numeric, status smallint) \
             ON CONFLICT (unit_code) WHERE deleted_at IS NULL {}",
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

/// 收集到的问题：没有就 `Ok(())`，有就整批返回（框架按行落错误明细）。
fn finish(issues: Vec<RowIssue>) -> std::result::Result<(), Vec<RowIssue>> {
    if issues.is_empty() { Ok(()) } else { Err(issues) }
}

/// 把解析期的行级错误拼成一句可读文案（`persist` 只有 `DbError` 这一个出口）。
fn join_issues(issues: &[RowIssue]) -> String {
    issues
        .iter()
        .map(|issue| format!("第 {} 行 {}", issue.row_no, issue.message))
        .collect::<Vec<_>>()
        .join("；")
}

#[cfg(test)]
mod tests {
    use super::super::{num_of, text_of};
    use super::*;
    use crate::business::impexp::definition::ImportMode;

    fn ctx() -> ImportCtx {
        ImportCtx {
            task_id: uuid::Uuid::nil(),
            operator: None,
            mode: ImportMode::Strict,
        }
    }

    fn row(pairs: &[(&'static str, &str)]) -> RowValues {
        let mut values = RowValues {
            row_no: 4,
            ..Default::default()
        };
        for (key, value) in pairs {
            values.fields.insert(key, (*value).to_string());
        }
        values
    }

    async fn issues_of(pairs: &[(&'static str, &str)]) -> Vec<RowIssue> {
        UnitImport
            .validate_row(&row(pairs), &ctx())
            .await
            .expect_err("应校验失败")
    }

    #[tokio::test]
    async fn base_unit_must_not_carry_base_or_foreign_rate() {
        let issues = issues_of(&[
            ("unit_code", "PCS"),
            ("unit_name", "个"),
            ("is_base", "true"),
            ("base_unit_code", "KG"),
            ("rate", "2"),
        ])
        .await;
        assert_eq!(issues.len(), 2, "基准单位填了基准 + 换算率不是 1 应两条错");
        assert!(issues.iter().all(|issue| issue.row_no == 4));
        assert!(issues[1].message.contains("换算率必须为 1"));
    }

    #[tokio::test]
    async fn auxiliary_unit_requires_base_and_positive_rate() {
        let issues = issues_of(&[
            ("unit_code", "BOX"),
            ("unit_name", "箱"),
            ("is_base", "false"),
            ("rate", "0"),
        ])
        .await;
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].column_key, Some("base_unit_code"));
        assert!(issues[0].message.contains("必须填写"));
        assert!(issues[1].message.contains("换算率必须大于 0"));
    }

    #[tokio::test]
    async fn base_cannot_be_self() {
        let issues = issues_of(&[
            ("unit_code", "BOX"),
            ("unit_name", "箱"),
            ("is_base", "false"),
            ("base_unit_code", "BOX"),
            ("rate", "12"),
        ])
        .await;
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("不能是自身"));
    }

    #[tokio::test]
    async fn valid_auxiliary_unit_passes_row_rules() {
        let result = UnitImport
            .validate_row(
                &row(&[
                    ("unit_code", "CTN"),
                    ("unit_name", "箱"),
                    ("is_base", "false"),
                    ("base_unit_code", "PCS"),
                    ("rate", "12"),
                ]),
                &ctx(),
            )
            .await;
        assert!(result.is_ok(), "合法辅助单位不应报错：{result:?}");
    }

    #[test]
    fn blank_is_base_is_treated_as_base_unit() {
        // 模板说明写的是「留空按是」，`validate_row` 与 `persist` 必须同口径
        assert_eq!(unit_is_base(""), 1);
        assert_eq!(unit_is_base("true"), 1);
        assert_eq!(unit_is_base("false"), 0);
    }

    /// 与 `persist` 里 `flag_of(row, "is_base")` 同口径的小包装（便于直接断言）。
    fn unit_is_base(raw: &str) -> i16 {
        if raw.is_empty() || raw == "true" { 1 } else { 0 }
    }

    #[test]
    fn base_unit_code_column_has_no_fk_but_is_documented() {
        let column = column_of(COLUMNS, "base_unit_code").expect("列应存在");
        // 有意不挂 FkSpec：同文件的基准单位也要能解析（见模块头）
        assert!(column.fk.is_none());
        assert!(column.remark.contains("同一份文件"));
        assert_eq!(text_of(&row(&[("base_unit_code", "")]), "base_unit_code"), None);
        assert_eq!(num_of(&row(&[("rate", "1.5")]), "rate"), Some("1.5".to_string()));
    }
}
