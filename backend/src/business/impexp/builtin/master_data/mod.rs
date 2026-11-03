//! ERP 主数据导入定义（6 个）：供应商 / 物料 / 计量单位 / 会计科目 / 仓库 / 库位。
//!
//! # 为什么需要这 6 个定义
//!
//! impexp 框架（模板下载 + 结构校验 + 逐行校验 + 错误明细 + 入库）本来就是现成的，
//! 但此前只接了一个 `system.dict_item`。新客户上线要手工录「13 个供应商、40 个计量单位、
//! 56 个会计科目，加上若干物料 / 仓库 / 库位」，这是**上线成本**级的断链
//! （ARCH-3 §6 R-P1-4 / DC-18）。本模块照 `dict_item.rs` 的范本补齐 6 个
//! `ImportDefinition`，不碰框架。
//!
//! # 每个定义都做到的四件事
//!
//! 1. **列契约**：必填 / 可空 / 列顺序写死在 `static COLUMNS`，模板「字段说明」sheet 自动生成；
//! 2. **行级校验**：编码唯一（文件内组合唯一，由框架按 `unique_key` 做）、外键存在
//!    （`FkSpec`，表名/列名是编译期常量）、数值下限（`ColumnKind::Decimal{min}`），
//!    再加本模块的跨列规则（`validate_row`），**错误一律带 Excel 真实行号 + 具体原因**；
//! 3. **两种失败口径**：`STRICT`（默认，任一行校验失败 → 一行都不入库）/ `PARTIAL`
//!    （跳过失败行继续入库）。定义里 `allow_partial() == true` 让页面能选，默认仍是 STRICT；
//! 4. **幂等**：按业务编码 upsert（`ON CONFLICT ... DO UPDATE`），重复上传同一份文件不产生重复行；
//!    上传时可选 `duplicate=SKIP` 改成「已存在则跳过」（策略快照在
//!    `sys_import_task.options.duplicate_strategy`，见 [`duplicate_strategy`]）。
//!
//! # 为什么 `batch_size == max_rows`（整份文件一个事务）
//!
//! 框架的 `STRICT` 语义是「任一行错就整体回滚」，而入库是**按批**开事务的：
//! 若一批只装 2000 行，第 2 批失败时第 1 批已经提交，界面会显示「前 2000 行已入库」——
//! 那对主数据是灾难（半个供应商表）。主数据单文件量级是几百行，把批大小设成行数上限，
//! 一份文件就落在一个事务里，`STRICT` 才真的是「全有或全无」。
//!
//! # 跨行 / 跨表引用的两处特例（框架不覆盖的部分）
//!
//! - **计量单位的基准单位**、**会计科目的上级科目**：同一份文件里父级常常排在后面或与子级同批，
//!   框架的外键校验只认库里已有的值，所以这两列**不挂 `FkSpec`**，改在 `persist` 里一次把
//!   「库里 + 本批次」的编码映射建好再解析（见 [`load_refs`]）。解析不到时返回带行号的错误，
//!   绝不静默丢行。
//! - **库位的所属仓库**：挂 `FkSpec`（库里必须先有仓库），另在 `validate_row` 里逐行校验
//!   「仓库必须是叶子仓库」（DB 无法跨表校验这一条）。
//!
//! # 软删行的两种处理（与 DB 唯一索引口径一致，不自行发挥）
//!
//! - 供应商 / 物料 / 计量单位 / 仓库 / 库位：唯一索引带 `WHERE deleted_at IS NULL`，
//!   所以「同编码的已删行」不影响插入 —— 导入会**新建**一行（软删后编码可复用）；
//! - 会计科目：`uk_biz_account_code` **是全局唯一**（V0137 §2.1.3 的唯一例外，因为凭证
//!   快照了科目编码），所以 `ON CONFLICT` 命中的是那条已删行：导入把它**复活**
//!   （`deleted_at = NULL`）而不是新建，历史凭证指向的科目因此不变。

use std::collections::HashMap;

use nik::prelude::*;

use crate::business::impexp::definition::{
    DuplicateStrategy, EnumItem, ErrorKind, FieldIssue, ImportColumn, ImportCtx, RowIssue,
    RowValues,
};

pub mod account;
pub mod location;
pub mod material;
pub mod supplier;
pub mod unit;
pub mod warehouse;

#[cfg(test)]
mod db_tests;

/// 供应商的业务编码。
pub const SUPPLIER_BIZ_TYPE: &str = "business.erp.supplier";
/// 物料的业务编码。
pub const MATERIAL_BIZ_TYPE: &str = "business.erp.material";
/// 计量单位的业务编码。
pub const UNIT_BIZ_TYPE: &str = "business.erp.unit";
/// 会计科目的业务编码。
pub const ACCOUNT_BIZ_TYPE: &str = "business.erp.account";
/// 仓库的业务编码。
pub const WAREHOUSE_BIZ_TYPE: &str = "business.erp.warehouse";
/// 库位的业务编码。
pub const LOCATION_BIZ_TYPE: &str = "business.erp.location";

/// 单次导入行数上限（客户上线场景实测是几百行，2 万留足余量）。
pub(super) const MAX_ROWS: usize = 20_000;

/// 分批大小 = 行数上限 ⇒ 一份文件一个事务（见模块头「为什么 batch_size == max_rows」）。
pub(super) const BATCH_SIZE: usize = MAX_ROWS;

/// 状态枚举：Excel 写中文，落库 `1` / `0`。
pub(super) const STATUS_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "启用",
        code: "1",
    },
    EnumItem {
        label: "停用",
        code: "0",
    },
];

/// 读回本任务生效的重复数据策略。
///
/// # 参数
///
/// - `ctx`：导入上下文（只用到 `task_id`）。
///
/// # 返回值
///
/// `options.duplicate_strategy` 解析结果；未设置 / 不是主数据任务时按默认 `UPSERT`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（宁可整批失败，也不猜一个策略去覆盖线上主数据）。
pub(super) async fn duplicate_strategy(
    ctx: &ImportCtx,
) -> std::result::Result<DuplicateStrategy, DbError> {
    let raw = crate::business::impexp::model::import_task_options(&ctx.task_id).await?;
    let parsed = raw
        .as_deref()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| {
            value
                .get("duplicate_strategy")
                .and_then(|item| item.as_str())
                .map(str::to_string)
        })
        .and_then(|text| DuplicateStrategy::parse(&text));
    Ok(parsed.unwrap_or_default())
}

/// 操作人文本（空串 → SQL 里 `NULLIF(?::text,'')::uuid` → NULL）。
///
/// # 参数
///
/// - `ctx`：导入上下文。
pub(super) fn operator_text(ctx: &ImportCtx) -> String {
    ctx.operator.map(|id| id.to_string()).unwrap_or_default()
}

/// 构造一条「业务规则」问题（带行号；有列就带列名与原始值）。
///
/// # 参数
///
/// - `row`：当前行。
/// - `column`：相关列（整行级问题传 `None`）。
/// - `message`：面向用户的文案（必须说清「哪个值、错在哪、怎么改」）。
pub(super) fn rule(
    row: &RowValues,
    column: Option<&ImportColumn>,
    message: impl Into<String>,
) -> RowIssue {
    let issue = FieldIssue {
        kind: ErrorKind::Rule,
        message: message.into(),
    };
    match column {
        Some(column) => RowIssue::column(row.row_no, column, row.get(column.key), issue),
        None => RowIssue::row(row.row_no, issue.kind, issue.message),
    }
}

/// 取列定义（按 key），用于给规则错误挂列名。
///
/// # 参数
///
/// - `columns`：定义的列集合。
/// - `key`：列键。
pub(super) fn column_of<'a>(columns: &'a [ImportColumn], key: &str) -> Option<&'a ImportColumn> {
    columns.iter().find(|column| column.key == key)
}

/// 状态列：空 → 1（启用）。
pub(super) fn status_of(row: &RowValues, key: &str) -> i16 {
    row.get(key).parse::<i16>().unwrap_or(1)
}

/// 布尔列（框架已规范成 `true` / `false`）：`true` → 1。
pub(super) fn flag_of(row: &RowValues, key: &str) -> i16 {
    if row.get(key) == "true" { 1 } else { 0 }
}

/// 文本列：空串 → `None`（落库 NULL，而不是空串）。
///
/// 框架在把单元格写进 [`RowValues`] 之前已经 trim 过；这里再 trim 一次是给
/// 「业务自己构造的行」（单测 / 修复脚本）兜底，保证「只有空格」也按空处理。
pub(super) fn text_of(row: &RowValues, key: &str) -> Option<String> {
    let value = row.get(key).trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// 数值列：空串 → `None`（落库走 SQL 里的 `COALESCE` 默认值）。
pub(super) fn num_of(row: &RowValues, key: &str) -> Option<String> {
    text_of(row, key)
}

/// 编码 → `(主键, 附加数值列)` 映射的装载结果。
///
/// 附加列在会计科目上是 `level`（算下级级次用），在计量单位上是 `is_base`（判定基准单位）。
pub(super) struct CodeRefs {
    /// 编码 → `(库里那一行的主键, 附加列值)`。
    pub db: HashMap<String, (uuid::Uuid, i16)>,
}

impl CodeRefs {
    /// 库里是否已有该编码，及其附加列值。
    pub(super) fn get(&self, code: &str) -> Option<(uuid::Uuid, i16)> {
        self.db.get(code).copied()
    }
}

/// 一次 SQL 装载「编码 → (主键, 附加列)」映射（只读，`code` / 列名都是编译期常量）。
///
/// # 参数
///
/// - `tx`：当前事务（与随后的 UPSERT 同一个快照可见性）。
/// - `table` / `code_col` / `extra_col`：来自定义文件的常量（**不接受运行时输入**）。
/// - `codes`：本批次涉及的编码（含自己与父级）。
///
/// # 返回值
///
/// [`CodeRefs`]；`codes` 为空时返回空映射且不发 SQL。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub(super) async fn load_refs(
    tx: &Transaction,
    table: &'static str,
    code_col: &'static str,
    extra_col: &'static str,
    codes: &[String],
) -> std::result::Result<CodeRefs, DbError> {
    let mut db = HashMap::new();
    if codes.is_empty() {
        return Ok(CodeRefs { db });
    }
    let mut unique = codes.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let payload = serde_json::to_string(&unique)
        .map_err(|error| DbError::InvalidValue(format!("编码序列化失败：{error}")))?;
    let sql = format!(
        "SELECT {code_col}::text AS code, id::text AS id, {extra_col}::text AS extra \
         FROM {table} \
         WHERE {code_col}::text IN (SELECT jsonb_array_elements_text(?::jsonb)) \
           AND deleted_at IS NULL"
    );
    let rows = tx
        .find_with_params(&sql, vec![Value::Json(payload)])
        .await?;
    for row in rows {
        let code: String = row.get_typed("code")?;
        let id: String = row.get_typed("id")?;
        let extra: Option<String> = row.get_typed("extra")?;
        let Ok(id) = uuid::Uuid::parse_str(&id) else {
            return Err(DbError::InvalidValue(format!(
                "{table} 主键不是合法 UUID：{id}"
            )));
        };
        let extra = extra.and_then(|text| text.parse::<i16>().ok()).unwrap_or(0);
        db.insert(code, (id, extra));
    }
    Ok(CodeRefs { db })
}

/// 冲突处理子句：`UPSERT` 更新 / `SKIP` 跳过。
///
/// # 参数
///
/// - `strategy`：本任务的重复数据策略。
/// - `update`：`DO UPDATE SET ...` 的右半段（`SKIP` 时忽略）。
pub(super) fn conflict_clause(strategy: DuplicateStrategy, update: &'static str) -> String {
    match strategy {
        DuplicateStrategy::Upsert => format!("DO UPDATE SET {update}"),
        DuplicateStrategy::Skip => "DO NOTHING".to_string(),
    }
}

/// 「本批次有行没落库」的错误文案（JOIN 把行过滤掉时不静默返回成功）。
///
/// # 参数
///
/// - `expected`：本批次行数。
/// - `affected`：数据库报告的影响行数。
/// - `hint`：给人看的下一步动作。
pub(super) fn missing_rows_error(expected: usize, affected: usize, hint: &str) -> DbError {
    DbError::InvalidValue(format!(
        "本批 {expected} 行中只有 {affected} 行落库，其余行的关联主数据在预检后不存在了（{hint}），已整批回滚"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_strategy_parsing_is_lenient_but_explicit() {
        assert_eq!(
            DuplicateStrategy::parse("upsert"),
            Some(DuplicateStrategy::Upsert)
        );
        assert_eq!(
            DuplicateStrategy::parse(" SKIP "),
            Some(DuplicateStrategy::Skip)
        );
        // 空串按默认（老客户端不带该参数）
        assert_eq!(
            DuplicateStrategy::parse(""),
            Some(DuplicateStrategy::Upsert)
        );
        assert_eq!(DuplicateStrategy::parse("覆盖"), None);
        assert_eq!(DuplicateStrategy::default().as_str(), "UPSERT");
        assert_eq!(DuplicateStrategy::Skip.as_str(), "SKIP");
    }

    #[test]
    fn conflict_clause_switches_between_update_and_skip() {
        let upsert = conflict_clause(DuplicateStrategy::Upsert, "a = EXCLUDED.a");
        assert_eq!(upsert, "DO UPDATE SET a = EXCLUDED.a");
        let skip = conflict_clause(DuplicateStrategy::Skip, "a = EXCLUDED.a");
        assert_eq!(skip, "DO NOTHING");
    }

    #[test]
    fn value_helpers_normalise_blank_cells() {
        let mut row = RowValues {
            row_no: 7,
            ..Default::default()
        };
        row.fields.insert("status", String::new());
        row.fields.insert("flag", "true".to_string());
        row.fields.insert("text", "  ".to_string());
        row.fields.insert("num", "12.50".to_string());
        assert_eq!(status_of(&row, "status"), 1);
        assert_eq!(flag_of(&row, "flag"), 1);
        assert_eq!(flag_of(&row, "missing"), 0);
        assert_eq!(text_of(&row, "text"), None);
        assert_eq!(num_of(&row, "num"), Some("12.50".to_string()));
    }

    #[test]
    fn rule_issue_carries_row_number_and_column() {
        use crate::business::impexp::definition::ImportDefinition;
        let definition = supplier::SupplierImport;
        let column = &definition.columns()[0];
        let mut row = RowValues {
            row_no: 5,
            ..Default::default()
        };
        row.fields.insert(column.key, "SUP-1".to_string());
        let issue = rule(&row, Some(column), "编码不可用");
        assert_eq!(issue.row_no, 5);
        assert_eq!(issue.kind, ErrorKind::Rule);
        assert_eq!(issue.column_key, Some(column.key));
        assert_eq!(issue.raw_value.as_deref(), Some("SUP-1"));
        let whole = rule(&row, None, "整行有问题");
        assert!(whole.column_key.is_none());
        assert_eq!(whole.message, "整行有问题");
    }

    #[test]
    fn six_definitions_have_distinct_biz_types_and_names() {
        use crate::business::impexp::definition::ImportDefinition;
        let defs: Vec<Box<dyn ImportDefinition>> = vec![
            Box::new(supplier::SupplierImport),
            Box::new(material::MaterialImport),
            Box::new(unit::UnitImport),
            Box::new(account::AccountImport),
            Box::new(warehouse::WarehouseImport),
            Box::new(location::LocationImport),
        ];
        let mut biz_types = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for def in &defs {
            assert!(
                biz_types.insert(def.biz_type()),
                "业务编码重复：{}",
                def.biz_type()
            );
            assert!(
                names.insert(def.biz_name()),
                "业务名称重复：{}",
                def.biz_name()
            );
            assert!(def.allow_partial(), "{} 应允许部分导入", def.biz_type());
            assert_eq!(def.batch_size(), MAX_ROWS);
            assert!(!def.columns().is_empty());
        }
        assert_eq!(biz_types.len(), 6);
    }
}
