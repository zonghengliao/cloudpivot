//! 会计科目导入（`biz_account`）。
//!
//! 列：科目编码 / 科目名称 / 科目类别 / 借贷方向 / 是否末级 / 上级科目编码 / 状态。
//! 幂等键：`account_code`（`uk_biz_account_code` **全表唯一**，不排除软删行 —— 见模块尾注释）。
//!
//! # 级次（`level`）不是「填」出来的，是「算」出来的
//!
//! Excel 里只填「上级科目编码」，级次由父级推导（一级 = 1、子级 = 父级 + 1）。父级既可能在
//! 库里，也可能在同一份文件的任何位置（排在子级后面也一样），所以这一列**不挂 `FkSpec`**，
//! 在 `persist` 里一次把「库里 + 本批次」的编码与级次建好映射再解析；成环或找不到父级
//! 都返回带行号的错误（绝不静默丢行）。
//!
//! # 软删行会被「复活」而不是新建
//!
//! `uk_biz_account_code` 是唯一不带 `WHERE deleted_at IS NULL` 的唯一索引（V0137 §2.1.3 的
//! 唯一例外条款：凭证会快照科目编码，编码一旦用过就永久占用）。所以同编码的已删行会被
//! `ON CONFLICT` 命中 —— 导入把它 `deleted_at = NULL` 复活，历史凭证指向的科目因此不变。

use std::collections::HashMap;

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, DuplicateStrategy, EnumItem, ImportColumn, ImportCtx, ImportDefinition, RowIssue,
    RowValues,
};

use super::{
    BATCH_SIZE, MAX_ROWS, STATUS_ITEMS, column_of, conflict_clause, duplicate_strategy, flag_of,
    load_refs, operator_text, rule, status_of,
};

/// 科目类别（落库码值与 `ck_biz_account_category` 一致）。
const CATEGORY_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "资产",
        code: "ASSET",
    },
    EnumItem {
        label: "负债",
        code: "LIABILITY",
    },
    EnumItem {
        label: "权益",
        code: "EQUITY",
    },
    EnumItem {
        label: "成本",
        code: "COST",
    },
    EnumItem {
        label: "损益",
        code: "PROFIT_LOSS",
    },
];

/// 余额方向（落库码值与 `ck_biz_account_direction` 一致）。
const DIRECTION_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "借",
        code: "DEBIT",
    },
    EnumItem {
        label: "贷",
        code: "CREDIT",
    },
];

/// 导入列定义（顺序即模板列顺序）。
static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: "account_code",
        header: "科目编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(30) },
        unique_key: true,
        fk: None,
        example: "6602",
        width: Some(16),
        remark: "科目编码（≤30 字符，按级次分段如 1405 / 1405.01）；全表唯一",
    },
    ImportColumn {
        key: "account_name",
        header: "科目名称",
        required: true,
        kind: ColumnKind::Text { max_len: Some(120) },
        unique_key: false,
        fk: None,
        example: "管理费用",
        width: Some(24),
        remark: "科目名称（≤120 字符）",
    },
    ImportColumn {
        key: "account_category",
        header: "科目类别",
        required: true,
        kind: ColumnKind::Enum(CATEGORY_ITEMS),
        unique_key: false,
        fk: None,
        example: "损益",
        width: Some(12),
        remark: "资产 / 负债 / 权益 / 成本 / 损益",
    },
    ImportColumn {
        key: "balance_direction",
        header: "借贷方向",
        required: true,
        kind: ColumnKind::Enum(DIRECTION_ITEMS),
        unique_key: false,
        fk: None,
        example: "借",
        width: Some(12),
        remark: "借 / 贷（余额方向）",
    },
    ImportColumn {
        key: "is_leaf",
        header: "是否末级",
        required: false,
        kind: ColumnKind::Bool,
        unique_key: false,
        fk: None,
        example: "是",
        width: Some(12),
        remark: "是 / 否，留空按「是」；非末级科目只做汇总，不能挂分录",
    },
    ImportColumn {
        key: "parent_code",
        header: "上级科目编码",
        required: false,
        kind: ColumnKind::Text { max_len: Some(30) },
        unique_key: false,
        fk: None,
        example: "",
        width: Some(16),
        remark: "一级科目留空；级次（level）由上级自动推导，可在同一份文件里（前后顺序不限）",
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

/// upsert 时更新哪些列（`deleted_at = NULL` 见模块头「软删行会被复活」）。
const UPSERT_SET: &str = "account_name = EXCLUDED.account_name, \
     parent_id = EXCLUDED.parent_id, \
     level = EXCLUDED.level, \
     account_category = EXCLUDED.account_category, \
     balance_direction = EXCLUDED.balance_direction, \
     is_leaf = EXCLUDED.is_leaf, \
     status = EXCLUDED.status, \
     deleted_at = NULL, \
     updated_at = CURRENT_TIMESTAMP, \
     update_by = EXCLUDED.update_by";

/// 解析后的一行。
#[derive(Debug, Clone, Copy)]
struct ResolvedAccount {
    id: uuid::Uuid,
    parent_id: Option<uuid::Uuid>,
    level: i16,
}

/// 会计科目导入定义。
pub struct AccountImport;

#[async_trait]
impl ImportDefinition for AccountImport {
    fn biz_type(&self) -> &'static str {
        super::ACCOUNT_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "会计科目"
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
        let mut issues = Vec::new();
        let code = row.get("account_code");
        let parent = row.get("parent_code");
        if !parent.is_empty() && parent == code {
            issues.push(rule(
                row,
                column_of(COLUMNS, "parent_code"),
                format!("上级科目不能是自身（科目编码「{code}」）"),
            ));
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
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

        let mut codes: Vec<String> = rows
            .iter()
            .map(|row| row.get("account_code").to_string())
            .collect();
        codes.extend(
            rows.iter()
                .map(|row| row.get("parent_code").to_string())
                .filter(|code| !code.is_empty()),
        );
        let refs = load_refs(tx, "biz_account", "account_code", "level", &codes).await?;

        let mut own: HashMap<String, usize> = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            own.entry(row.get("account_code").to_string())
                .or_insert(index);
        }

        let mut memo: Vec<Option<ResolvedAccount>> = vec![None; rows.len()];
        let mut stack: Vec<usize> = Vec::new();
        for index in 0..rows.len() {
            resolve(index, rows, &own, &refs, &mut memo, &mut stack)?;
        }
        let resolved = memo
            .into_iter()
            .map(|item| item.expect("全部行都应解析完成"))
            .collect::<Vec<_>>();

        let payload = serde_json::to_string(
            &rows
                .iter()
                .zip(resolved.iter())
                .map(|(row, item)| {
                    let category = super::text_of(row, "account_category");
                    serde_json::json!({
                        "id": item.id.to_string(),
                        "account_code": row.get("account_code"),
                        "account_name": row.get("account_name"),
                        "parent_id": item.parent_id.map(|id| id.to_string()),
                        "level": item.level,
                        "account_category": category,
                        "balance_direction": super::text_of(row, "balance_direction"),
                        "is_leaf": flag_of(row, "is_leaf"),
                        "status": status_of(row, "status"),
                    })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| DbError::InvalidValue(format!("会计科目序列化失败：{error}")))?;

        let sql = format!(
            "INSERT INTO biz_account \
                 (id, account_code, account_name, parent_id, level, account_category, \
                  balance_direction, is_leaf, status, created_at, updated_at, \
                  create_by, update_by) \
             SELECT v.id::uuid, v.account_code, v.account_name, v.parent_id::uuid, v.level, \
                    v.account_category, v.balance_direction, v.is_leaf, v.status, \
                    CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
                    NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
             FROM jsonb_to_recordset(?::jsonb) AS v( \
                    id text, account_code text, account_name text, parent_id text, \
                    level smallint, account_category text, balance_direction text, \
                    is_leaf smallint, status smallint) \
             ON CONFLICT (account_code) {}",
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
                "请重新上传",
            ));
        }
        Ok(affected as usize)
    }
}

/// 递归解析一行的 `(主键, 上级, 级次)`，成环直接报错。
///
/// # 参数
///
/// - `index`：当前行下标。
/// - `rows` / `own` / `refs`：行、本批次的「编码 → 下标」、库里的「编码 → (主键, level)」。
/// - `memo`：已解析结果（兼作「正在解析」的标记）。
/// - `stack`：当前解析路径（判环用）。
fn resolve(
    index: usize,
    rows: &[RowValues],
    own: &HashMap<String, usize>,
    refs: &super::CodeRefs,
    memo: &mut Vec<Option<ResolvedAccount>>,
    stack: &mut Vec<usize>,
) -> std::result::Result<ResolvedAccount, DbError> {
    if let Some(done) = memo[index] {
        return Ok(done);
    }
    let row = &rows[index];
    let code = row.get("account_code").to_string();
    let parent_code = row.get("parent_code").to_string();
    if stack.contains(&index) {
        let chain = stack
            .iter()
            .map(|item| rows[*item].get("account_code"))
            .chain(std::iter::once(code.as_str()))
            .collect::<Vec<_>>()
            .join(" → ");
        return Err(DbError::InvalidValue(format!(
            "第 {} 行：上级科目形成环（{chain}），请检查「上级科目编码」列",
            row.row_no
        )));
    }
    let id = refs
        .get(&code)
        .map(|(id, _)| id)
        .unwrap_or_else(uuid::Uuid::now_v7);
    let (parent_id, level) = if parent_code.is_empty() {
        (None, 1)
    } else if let Some(parent_index) = own.get(&parent_code).copied() {
        stack.push(index);
        let parent = resolve(parent_index, rows, own, refs, memo, stack)?;
        stack.pop();
        (Some(parent.id), parent.level + 1)
    } else if let Some((parent_id, parent_level)) = refs.get(&parent_code) {
        (Some(parent_id), parent_level + 1)
    } else {
        return Err(DbError::InvalidValue(format!(
            "第 {} 行：上级科目「{parent_code}」不存在（本文件与科目表里都没有），\
             请先维护该科目或清空「上级科目编码」",
            row.row_no
        )));
    };
    let resolved = ResolvedAccount {
        id,
        parent_id,
        level,
    };
    memo[index] = Some(resolved);
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::impexp::definition::ImportMode;

    fn ctx() -> ImportCtx {
        ImportCtx {
            task_id: uuid::Uuid::nil(),
            operator: None,
            mode: ImportMode::Strict,
        }
    }

    fn row(row_no: u32, code: &str, parent: &str) -> RowValues {
        let mut values = RowValues {
            row_no,
            ..Default::default()
        };
        values.fields.insert("account_code", code.to_string());
        values.fields.insert("account_name", code.to_string());
        values.fields.insert("parent_code", parent.to_string());
        values
    }

    #[tokio::test]
    async fn self_parent_is_rejected_with_row_number() {
        let issues = AccountImport
            .validate_row(&row(9, "1405", "1405"), &ctx())
            .await
            .expect_err("自引用应被拒");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].row_no, 9);
        assert_eq!(issues[0].column_key, Some("parent_code"));
        assert!(issues[0].message.contains("不能是自身"));
    }

    #[tokio::test]
    async fn level_is_derived_from_parent_chain_in_any_order() {
        // 子级排在父级前面：解析器必须能跨行推到三级（1 / 2 / 3）
        let rows = vec![
            row(2, "1405.01.01", "1405.01"),
            row(3, "1405", ""),
            row(4, "1405.01", "1405"),
        ];
        let mut own: HashMap<String, usize> = HashMap::new();
        for (index, item) in rows.iter().enumerate() {
            own.insert(item.get("account_code").to_string(), index);
        }
        let refs = super::super::CodeRefs { db: HashMap::new() };
        let mut memo: Vec<Option<ResolvedAccount>> = vec![None; rows.len()];
        let mut stack = Vec::new();
        for index in 0..rows.len() {
            resolve(index, &rows, &own, &refs, &mut memo, &mut stack).expect("同文件父级应可解析");
        }
        assert_eq!(memo[1].expect("1405").level, 1);
        assert_eq!(memo[2].expect("1405.01").level, 2);
        assert_eq!(memo[0].expect("1405.01.01").level, 3);
        assert_eq!(
            memo[0].expect("1405.01.01").parent_id,
            Some(memo[2].expect("1405.01").id)
        );
    }

    #[test]
    fn missing_parent_is_reported_with_row_number() {
        let rows = vec![row(7, "2202.01", "9999")];
        let own: HashMap<String, usize> = HashMap::new();
        let refs = super::super::CodeRefs { db: HashMap::new() };
        let mut memo: Vec<Option<ResolvedAccount>> = vec![None; rows.len()];
        let mut stack = Vec::new();
        let error =
            resolve(0, &rows, &own, &refs, &mut memo, &mut stack).expect_err("父级不存在应报错");
        let text = format!("{error}");
        assert!(text.contains("第 7 行"), "错误文案必须带行号：{text}");
        assert!(text.contains("9999"));
    }

    #[test]
    fn cycle_is_detected_instead_of_hanging() {
        let rows = vec![row(2, "A", "B"), row(3, "B", "A")];
        let mut own: HashMap<String, usize> = HashMap::new();
        for (index, item) in rows.iter().enumerate() {
            own.insert(item.get("account_code").to_string(), index);
        }
        let refs = super::super::CodeRefs { db: HashMap::new() };
        let mut memo: Vec<Option<ResolvedAccount>> = vec![None; rows.len()];
        let mut stack = Vec::new();
        let error = resolve(0, &rows, &own, &refs, &mut memo, &mut stack).expect_err("成环应报错");
        assert!(format!("{error}").contains("形成环"));
    }

    #[test]
    fn enums_match_db_check_constraints() {
        let category = COLUMNS[2].kind;
        assert_eq!(category.parse("资产", None).expect("资产"), "ASSET");
        assert_eq!(category.parse("损益", None).expect("损益"), "PROFIT_LOSS");
        let direction = COLUMNS[3].kind;
        assert_eq!(direction.parse("借", None).expect("借"), "DEBIT");
        assert_eq!(direction.parse("贷", None).expect("贷"), "CREDIT");
        assert!(COLUMNS[2].required && COLUMNS[3].required);
    }
}
