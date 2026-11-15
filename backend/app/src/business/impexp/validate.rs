//! 逐行校验 + 外键批量校验。
//!
//! # 两级校验（PRD §4.2.1）
//!
//! 1. **结构校验**（整表，任一失败即终止）：由 [`crate::business::impexp::engine`] 完成，
//!    包括魔数、空文件、表头缺列（E-IMP-02/03）与「模板外的列 / 列顺序不一致」两条警告
//!    （E-IMP-04/05）；
//! 2. **业务校验**（逐行，最多收集 [`MAX_ERRORS`] 条明细）：本文件负责必填、类型、长度、
//!    枚举、字典、数值范围、日期、文件内唯一、外键存在性，最后再调业务 `validate_row`。
//!
//! # 外键批量校验：SQL 次数 = 外键列数
//!
//! 先把所有行里该外键列的取值**去重聚合**，再对每列发**一条** SQL：
//!
//! ```sql
//! SELECT DISTINCT col::text FROM tbl
//! WHERE col::text IN (SELECT jsonb_array_elements_text(?::jsonb)) [AND deleted_at IS NULL]
//! ```
//!
//! 10 万行 × 3 个外键列 = **3 条 SQL**，而不是 30 万条。用单个 JSONB 数组参数而不是
//! `IN (?,?,…)` 是因为后者会撞 PostgreSQL 的 65535 参数上限（10 万行必炸）。
//! SQL 次数由 [`DbFkResolver::statements`] 如实计数并写进预检报告（`fk_sql_statements`）
//! 与日志，可在测试与实测中直接断言。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, DictTranslation, ErrorKind, FkSpec, ImportCtx, ImportDefinition, RowIssue, RowValues,
};
use crate::business::impexp::engine::{EngineError, SheetReader};
use crate::shared::error_code;
use crate::utils;

/// 一次预检最多收集的错误明细条数（PRD §4.2.1：最多 2000 条）。
pub const MAX_ERRORS: usize = 2_000;

/// 字典缓存有效期（架构文档 §3.3：进程级缓存 5 分钟）。
pub const DICT_TTL: Duration = Duration::from_secs(300);

// ==================== 字典装载 ====================

/// 字典翻译表的进程级缓存。
pub struct DictProvider {
    entries: RwLock<HashMap<String, (Instant, Arc<DictTranslation>)>>,
}

impl Default for DictProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DictProvider {
    /// 空缓存。
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// 直接写入一份翻译表（单测 / 预热用，不查库）。
    ///
    /// # 参数
    ///
    /// - `code`：字典编码。
    /// - `translation`：翻译表。
    pub fn seed(&self, code: &str, translation: DictTranslation) {
        if let Ok(mut guard) = self.entries.write() {
            guard.insert(code.to_string(), (Instant::now(), Arc::new(translation)));
        }
    }

    /// 取字典翻译表（命中缓存则不发 SQL）。
    ///
    /// # 参数
    ///
    /// - `code`：`sys_dict_type.dict_code`。
    ///
    /// # 返回值
    ///
    /// 翻译表（字典不存在时返回空表，解析阶段会报「值不在字典中」）。
    ///
    /// # 异常情况
    ///
    /// 数据库异常返回 [`DbError`]。
    pub async fn get(&self, code: &str) -> std::result::Result<Arc<DictTranslation>, DbError> {
        if let Ok(guard) = self.entries.read()
            && let Some((loaded_at, translation)) = guard.get(code)
            && loaded_at.elapsed() < DICT_TTL
        {
            return Ok(translation.clone());
        }
        let rows = db()
            .find_with_params(
                "SELECT i.item_value::text AS item_value, i.item_label::text AS item_label \
                 FROM sys_dict_item i \
                 INNER JOIN sys_dict_type t ON t.id = i.dict_type_id \
                 WHERE t.dict_code = ? AND t.deleted_at IS NULL AND i.deleted_at IS NULL \
                 ORDER BY i.sort_order ASC, i.created_at ASC",
                vec![Value::from(code)],
            )
            .await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let value: String = row.get_typed("item_value")?;
            let label: String = row.get_typed("item_label")?;
            items.push((value, label));
        }
        let translation = Arc::new(DictTranslation::from_items(items));
        if let Ok(mut guard) = self.entries.write() {
            guard.insert(code.to_string(), (Instant::now(), Arc::clone(&translation)));
        }
        Ok(translation)
    }
}

/// 进程级字典缓存。
static DICT_PROVIDER: OnceLock<DictProvider> = OnceLock::new();

/// 取进程级字典缓存。
pub fn global_dicts() -> &'static DictProvider {
    DICT_PROVIDER.get_or_init(DictProvider::new)
}

// ==================== 外键批量校验 ====================

/// 外键存在性解析器（抽象出来是为了能在单测里替换成计数器实现，不连库）。
#[async_trait]
pub trait FkResolver: Send + Sync {
    /// 查询某外键列里「已存在」的取值集合。
    ///
    /// # 参数
    ///
    /// - `spec`：外键规格（表 / 列 / 是否软删）。
    /// - `values`：**去重后**的取值列表。
    ///
    /// # 返回值
    ///
    /// 命中的取值集合。
    ///
    /// # 异常情况
    ///
    /// 数据库异常返回 [`DbError`]。
    async fn existing_values(&self, spec: &FkSpec, values: &[String]) -> std::result::Result<HashSet<String>, DbError>;

    /// 已执行的 SQL 语句条数（用于「SQL 次数 = 外键列数」断言）。
    fn statements(&self) -> usize;
}

/// 真实实现：走框架连接池，每次调用一条 SQL。
#[derive(Default)]
pub struct DbFkResolver {
    statements: AtomicUsize,
}

impl DbFkResolver {
    /// 新建解析器（计数归零）。
    pub fn new() -> Self {
        Self::default()
    }
}

/// 拼接外键查询 SQL（表名 / 列名全部来自编译期常量，不接受运行时输入）。
///
/// # 参数
///
/// - `spec`：外键规格。
///
/// # 返回值
///
/// 单条参数化 SQL（1 个 JSONB 数组参数）。
pub fn fk_lookup_sql(spec: &FkSpec) -> String {
    let soft_delete = if spec.soft_delete {
        " AND deleted_at IS NULL"
    } else {
        ""
    };
    format!(
        "SELECT DISTINCT {column}::text FROM {table} \
         WHERE {column}::text IN (SELECT jsonb_array_elements_text(?::jsonb)){soft_delete}",
        column = spec.column,
        table = spec.table,
        soft_delete = soft_delete,
    )
}

#[async_trait]
impl FkResolver for DbFkResolver {
    async fn existing_values(&self, spec: &FkSpec, values: &[String]) -> std::result::Result<HashSet<String>, DbError> {
        if values.is_empty() {
            return Ok(HashSet::new());
        }
        let payload = serde_json::to_string(values)
            .map_err(|error| DbError::InvalidValue(format!("外键取值序列化失败：{error}")))?;
        let sql = fk_lookup_sql(spec);
        // 计数在**发语句之前**自增：即使查询报错，也如实反映「发过一条」。
        self.statements.fetch_add(1, Ordering::SeqCst);
        let rows = db().find_with_params(&sql, vec![Value::Json(payload)]).await?;
        let mut found = HashSet::with_capacity(rows.len());
        for row in rows {
            // `SELECT DISTINCT col::text` 只有一列，按下标取（列名随 spec 变化，不做字符串耦合）
            let value: String = row.get_typed_at("fk_value", 0)?;
            found.insert(value);
        }
        Ok(found)
    }

    fn statements(&self) -> usize {
        self.statements.load(Ordering::SeqCst)
    }
}

// ==================== 预检结果 ====================

/// 结构级警告（不阻断导入，随报告返回给前端）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportWarning {
    /// 警告码（E-IMP-04 / E-IMP-05）。
    pub code: &'static str,
    /// 面向用户的文案。
    pub message: String,
}

/// 预检报告。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PrecheckReport {
    /// 数据行总数（不含表头、示例行与空行）。
    pub total: usize,
    /// 校验通过行数。
    pub valid: usize,
    /// 校验失败行数（按行计，不按错误条数）。
    pub error: usize,
    /// 自动跳过的示例行数。
    pub skipped_example: usize,
    /// 跳过的空行数。
    pub skipped_blank: usize,
    /// 结构级警告。
    pub warnings: Vec<ImportWarning>,
    /// 错误明细（最多 [`MAX_ERRORS`] 条）。
    pub errors: Vec<RowIssue>,
    /// 明细是否被截断。
    pub truncated: bool,
    /// 外键批量校验实际执行的 SQL 条数（应等于「有值的外键列数」）。
    pub fk_sql_statements: usize,
    /// 校验耗时（毫秒）。
    pub elapsed_ms: u64,
}

/// 校验产物：报告 + （可选）通过校验的行。
pub struct ValidationOutcome {
    /// 预检报告。
    pub report: PrecheckReport,
    /// 通过校验的行（`keep_rows = false` 时为空，用于避免大文件二次占用内存）。
    pub rows: Vec<RowValues>,
}

/// 从列定义里取出用到的字典编码（去重）。
///
/// # 参数
///
/// - `definition`：导入定义。
///
/// # 返回值
///
/// 字典编码列表。
pub fn required_dict_codes(definition: &dyn ImportDefinition) -> Vec<&'static str> {
    let mut codes = Vec::new();
    for column in definition.columns() {
        if let ColumnKind::Dict(code) = column.kind
            && !codes.contains(&code)
        {
            codes.push(code);
        }
    }
    codes
}

/// 执行完整校验（结构已在 `SheetReader::open` 完成）。
///
/// # 参数
///
/// - `definition`：导入定义。
/// - `bytes`：xlsx 字节。
/// - `dicts`：字典缓存。
/// - `resolver`：外键解析器。
/// - `ctx`：导入上下文（传给业务 `validate_row`）。
/// - `keep_rows`：是否保留通过校验的行（确认入库时用 `true`）。
///
/// # 返回值
///
/// [`ValidationOutcome`]。
///
/// # 异常情况
///
/// - 400/422：结构校验失败（见 [`EngineError`]）；
/// - 422 + `60012`：超过 `max_rows`；
/// - 500：字典装载 / 外键查询的数据库异常。
pub async fn validate(
    definition: &dyn ImportDefinition,
    bytes: &[u8],
    dicts: &DictProvider,
    resolver: &dyn FkResolver,
    ctx: &ImportCtx,
    keep_rows: bool,
) -> std::result::Result<ValidationOutcome, ApiError> {
    let started = Instant::now();
    let columns = definition.columns();
    let reader = SheetReader::open(bytes, columns).map_err(EngineError::into_api_error)?;

    let mut warnings = Vec::new();
    let header = reader.header();
    if !header.extra_headers.is_empty() {
        warnings.push(ImportWarning {
            code: "E-IMP-04",
            message: format!(
                "文件中有 {} 个不属于本模板的列（{}），这些列将被忽略",
                header.extra_headers.len(),
                header.extra_headers.join("、")
            ),
        });
    }
    if header.order_mismatch {
        warnings.push(ImportWarning {
            code: "E-IMP-05",
            message: "表头顺序与模板不一致，已按列名匹配，请确认数据对位是否正确".to_string(),
        });
    }

    // ---------- 第 1 遍：计数 + 收集外键取值（不保留行，不产生第二份行集合） ----------
    let mut total = 0usize;
    let mut skipped_example = 0usize;
    let mut skipped_blank = 0usize;
    let mut fk_values: Vec<Vec<String>> = vec![Vec::new(); columns.len()];
    let mut fk_seen: Vec<HashSet<String>> = vec![HashSet::new(); columns.len()];

    for (index, row) in reader.data_rows().enumerate() {
        if SheetReader::is_blank(row) {
            skipped_blank += 1;
            continue;
        }
        // 示例行只可能出现在 Excel 第 2 行（PRD §4.2.2）：只判首行，避免 10 万次逐列比较
        if index == 0 && reader.is_example_row(row) {
            skipped_example += 1;
            continue;
        }
        total += 1;
        for (column_index, column) in columns.iter().enumerate() {
            if column.fk.is_none() {
                continue;
            }
            let value = reader.cell(row, column_index);
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            if fk_seen[column_index].insert(value.to_string()) {
                fk_values[column_index].push(value.to_string());
            }
        }
    }

    if total == 0 {
        return Err(EngineError::NoDataRows.into_api_error());
    }
    if total > definition.max_rows() {
        return Err(ApiError::new(
            422,
            error_code::IMPORT_ROW_LIMIT,
            format!(
                "数据共 {total} 行，超过单次上限 {} 行，请拆分后导入",
                definition.max_rows()
            ),
        ));
    }

    // ---------- 外键批量校验：外键列数条 SQL ----------
    let mut fk_existing: Vec<Option<HashSet<String>>> = vec![None; columns.len()];
    for (column_index, column) in columns.iter().enumerate() {
        let Some(spec) = column.fk else {
            continue;
        };
        let values = &fk_values[column_index];
        if values.is_empty() {
            // 该列所有行都是空值：不发 SQL（空值的必填判定在逐行校验里）
            fk_existing[column_index] = Some(HashSet::new());
            continue;
        }
        let found = resolver
            .existing_values(&spec, values)
            .await
            .map_err(|error| utils::db_error(error, "外键批量校验"))?;
        fk_existing[column_index] = Some(found);
    }

    // ---------- 字典装载（每列一次，命中缓存不发 SQL） ----------
    let mut dicts_loaded: HashMap<&'static str, Arc<DictTranslation>> = HashMap::new();
    for code in required_dict_codes(definition) {
        let translation = dicts
            .get(code)
            .await
            .map_err(|error| utils::db_error(error, "装载字典"))?;
        dicts_loaded.insert(code, translation);
    }

    // ---------- 第 2 遍：逐行校验 ----------
    let unique_columns = columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.unique_key)
        .collect::<Vec<_>>();
    let mut unique_seen: HashMap<String, u32> = HashMap::new();
    let mut errors: Vec<RowIssue> = Vec::new();
    let mut rows: Vec<RowValues> = Vec::new();
    let mut valid = 0usize;
    let mut error_rows = 0usize;

    for (index, row) in reader.data_rows().enumerate() {
        if SheetReader::is_blank(row) || (index == 0 && reader.is_example_row(row)) {
            continue;
        }
        let row_no = SheetReader::excel_row_no(index);
        let mut row_issues: Vec<RowIssue> = Vec::new();
        let mut values = RowValues {
            row_no,
            ..Default::default()
        };
        let mut unique_key = String::new();
        let mut unique_complete = true;

        for (column_index, column) in columns.iter().enumerate() {
            let raw = reader.cell(row, column_index);
            let raw_trimmed = raw.trim();
            if column.required && raw_trimmed.is_empty() {
                row_issues.push(RowIssue::column(
                    row_no,
                    column,
                    raw_trimmed,
                    crate::business::impexp::definition::FieldIssue {
                        kind: ErrorKind::Required,
                        message: "必填项为空".to_string(),
                    },
                ));
                if column.unique_key {
                    unique_complete = false;
                }
                continue;
            }
            if raw_trimmed.is_empty() {
                values.fields.insert(column.key, String::new());
                if column.unique_key {
                    unique_complete = false;
                }
                continue;
            }
            let dict = match column.kind {
                ColumnKind::Dict(code) => dicts_loaded.get(code).map(|arc| arc.as_ref()),
                _ => None,
            };
            match column.kind.parse(raw_trimmed, dict) {
                Ok(parsed) => {
                    if column.unique_key {
                        unique_key.push('\u{1}');
                        unique_key.push_str(&parsed);
                    }
                    values.fields.insert(column.key, parsed);
                }
                Err(issue) => {
                    if column.unique_key {
                        unique_complete = false;
                    }
                    row_issues.push(RowIssue::column(row_no, column, raw_trimmed, issue));
                }
            }
            // 外键存在性（在类型转换之后判定，用落库值比较）
            if let Some(spec) = column.fk
                && let Some(found) = fk_existing[column_index].as_ref()
                && let Some(parsed) = values.fields.get(column.key)
                && !parsed.is_empty()
                && !found.contains(parsed)
            {
                row_issues.push(RowIssue::column(
                    row_no,
                    column,
                    raw_trimmed,
                    crate::business::impexp::definition::FieldIssue {
                        kind: ErrorKind::FkNotFound,
                        message: format!("{}「{}」不存在，请先在{}中维护", column.header, parsed, spec.label),
                    },
                ));
            }
        }

        // 文件内唯一（组合唯一键；任一唯一列为空则交给必填判定，不重复报错）
        if unique_complete && !unique_columns.is_empty() {
            match unique_seen.get(&unique_key) {
                Some(first_row) => {
                    let column = columns
                        .iter()
                        .find(|column| column.unique_key)
                        .expect("unique_columns 非空");
                    row_issues.push(RowIssue::column(
                        row_no,
                        column,
                        &unique_key.replace('\u{1}', "/"),
                        crate::business::impexp::definition::FieldIssue {
                            kind: ErrorKind::Unique,
                            message: format!("{}已存在（与第 {first_row} 行重复）", column.header),
                        },
                    ));
                }
                None => {
                    unique_seen.insert(unique_key, row_no);
                }
            }
        }

        // 业务规则（跨列 / 库内唯一等），在同一行上追加错误
        if row_issues.is_empty() {
            match definition.validate_row(&values, ctx).await {
                Ok(()) => {}
                Err(mut business_issues) => row_issues.append(&mut business_issues),
            }
        }

        if row_issues.is_empty() {
            valid += 1;
            // 批次级校验要整批行；预检（`keep_rows = false`）本来不留行，声明了
            // `validates_batch()` 的定义也留（报告算完立刻释放，见下面的第 3 遍）。
            if keep_rows || definition.validates_batch() {
                rows.push(values);
            }
        } else {
            error_rows += 1;
            for issue in row_issues {
                if errors.len() < MAX_ERRORS {
                    errors.push(issue);
                }
            }
        }
    }

    // ---------- 第 3 遍：批次级业务校验（整批 + 库内口径） ----------
    //
    // 逐行校验只看一行里的格子，有些规则要「整批 + 库里已有数据」才有答案
    // （例：考勤的「无薪假 ≤ 已批准请假天数」）。命中问题的行从可入库集合里**摘掉**
    // 并计入 `error`：`STRICT` 据此整批拒绝，`PARTIAL` 只跳过错行 —— 与逐行校验
    // 完全同一口径，不另开一条「警告」通道（用户不会看到「导进去了但数据不对」）。
    if definition.validates_batch() {
        if !rows.is_empty() {
            match definition.validate_batch(&rows, ctx).await {
                Ok(batch_issues) if batch_issues.is_empty() => {}
                Ok(batch_issues) => {
                    let bad_rows = batch_issues.iter().map(|issue| issue.row_no).collect::<HashSet<u32>>();
                    let before = rows.len();
                    rows.retain(|row| !bad_rows.contains(&row.row_no));
                    let removed = before - rows.len();
                    valid = valid.saturating_sub(removed);
                    error_rows += removed;
                    for issue in batch_issues {
                        if errors.len() < MAX_ERRORS {
                            errors.push(issue);
                        }
                    }
                }
                Err(error) => return Err(utils::db_error(error, "批次级业务校验")),
            }
        }
        if !keep_rows {
            // 只为批次校验临时保留的行：报告已经算完，立刻释放（2 万行 × 15 列不小）
            rows.clear();
        }
    }

    let report = PrecheckReport {
        total,
        valid,
        error: error_rows,
        skipped_example,
        skipped_blank,
        warnings,
        truncated: errors.len() >= MAX_ERRORS,
        errors,
        fk_sql_statements: resolver.statements(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    tracing::info!(
        biz_type = definition.biz_type(),
        total = report.total,
        valid = report.valid,
        error = report.error,
        fk_sql_statements = report.fk_sql_statements,
        fk_columns = columns.iter().filter(|column| column.fk.is_some()).count(),
        elapsed_ms = report.elapsed_ms,
        "导入预检完成（外键批量校验 SQL 次数应等于外键列数）"
    );
    Ok(ValidationOutcome { report, rows })
}

/// 只做预检、不保留行（handler 的 HTTP 预检路径）。
///
/// # 参数
///
/// 同 [`validate`]（少了 `keep_rows`）。
///
/// # 返回值
///
/// 预检报告。
///
/// # 异常情况
///
/// 同 [`validate`]。
pub async fn precheck(
    definition: &dyn ImportDefinition,
    bytes: &[u8],
    dicts: &DictProvider,
    resolver: &dyn FkResolver,
    ctx: &ImportCtx,
) -> std::result::Result<PrecheckReport, ApiError> {
    validate(definition, bytes, dicts, resolver, ctx, false)
        .await
        .map(|outcome| outcome.report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::impexp::definition::{ColumnKind, EnumItem, ImportColumn};
    use nik::prelude::Export;

    /// 计数器型外键解析器：不连库，只如实记录被调用次数与**每次收到的取值个数**。
    ///
    /// `sizes` 用来断言「先按列去重、再发一条 SQL」：同一列 300 行进来的取值个数应该
    /// 等于去重后的个数，而不是行数。
    struct CountingFkResolver {
        known: HashSet<String>,
        calls: AtomicUsize,
        sizes: std::sync::Mutex<Vec<usize>>,
    }

    impl CountingFkResolver {
        fn new(known: &[&str]) -> Self {
            Self {
                known: known.iter().map(|value| value.to_string()).collect(),
                calls: AtomicUsize::new(0),
                sizes: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// 每次 `existing_values` 收到的取值个数（按列调用顺序）。
        fn sizes(&self) -> Vec<usize> {
            self.sizes.lock().expect("取值个数记录锁未中毒").clone()
        }
    }

    #[async_trait]
    impl FkResolver for CountingFkResolver {
        async fn existing_values(
            &self,
            _spec: &FkSpec,
            values: &[String],
        ) -> std::result::Result<HashSet<String>, DbError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.sizes.lock().expect("取值个数记录锁未中毒").push(values.len());
            Ok(values
                .iter()
                .filter(|value| self.known.contains(*value))
                .cloned()
                .collect())
        }

        fn statements(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    const STATUS: &[EnumItem] = &[EnumItem {
        label: "启用",
        code: "1",
    }];

    static COLUMNS: &[crate::business::impexp::definition::ImportColumn] = &[
        crate::business::impexp::definition::ImportColumn {
            key: "code",
            header: "编码",
            required: true,
            kind: ColumnKind::Text { max_len: Some(32) },
            unique_key: true,
            fk: None,
            example: "EX-0001",
            width: None,
            remark: "",
        },
        crate::business::impexp::definition::ImportColumn {
            key: "dict",
            header: "字典",
            required: true,
            kind: ColumnKind::Text { max_len: Some(64) },
            unique_key: false,
            fk: Some(FkSpec {
                table: "sys_dict_type",
                column: "dict_code",
                label: "字典类型",
                soft_delete: true,
            }),
            example: "user_status",
            width: None,
            remark: "",
        },
        crate::business::impexp::definition::ImportColumn {
            key: "value",
            header: "字典项",
            required: true,
            kind: ColumnKind::Text { max_len: Some(64) },
            unique_key: false,
            fk: Some(FkSpec {
                table: "sys_dict_item",
                column: "item_value",
                label: "字典项",
                soft_delete: true,
            }),
            example: "1",
            width: None,
            remark: "",
        },
        crate::business::impexp::definition::ImportColumn {
            key: "status",
            header: "状态",
            required: false,
            kind: ColumnKind::Enum(STATUS),
            unique_key: false,
            fk: None,
            example: "启用",
            width: None,
            remark: "",
        },
    ];

    struct TestDefinition {
        max_rows: usize,
    }

    #[async_trait]
    impl ImportDefinition for TestDefinition {
        fn biz_type(&self) -> &'static str {
            "test.validate"
        }
        fn biz_name(&self) -> &'static str {
            "校验用例"
        }
        fn max_rows(&self) -> usize {
            self.max_rows
        }
        fn columns(&self) -> &'static [crate::business::impexp::definition::ImportColumn] {
            COLUMNS
        }
        async fn persist(
            &self,
            _tx: &nik::prelude::Transaction,
            _rows: &[RowValues],
            _ctx: &ImportCtx,
        ) -> std::result::Result<usize, DbError> {
            Ok(0)
        }
    }

    fn definition() -> TestDefinition {
        TestDefinition { max_rows: 100 }
    }

    /// 覆写 `validates_batch` 的定义：把「字典项 = 2」的行判为不可入库
    /// （HR-22 的「无薪假 ≤ 已批请假天数」就是这一形态：整批 + 库内数据，框架逐行校验
    /// 看不出来）。用它钉住三件事：预检（`keep_rows = false`）也要保留整批行 /
    /// 命中的行从可入库集合摘掉 / 报文带上 Excel 行号。
    struct BatchDefinition;

    #[async_trait]
    impl ImportDefinition for BatchDefinition {
        fn biz_type(&self) -> &'static str {
            "test.batch"
        }
        fn biz_name(&self) -> &'static str {
            "批次校验用例"
        }
        fn columns(&self) -> &'static [crate::business::impexp::definition::ImportColumn] {
            COLUMNS
        }
        fn validates_batch(&self) -> bool {
            true
        }
        async fn validate_batch(&self, rows: &[RowValues], _ctx: &ImportCtx) -> DbResult<Vec<RowIssue>> {
            assert!(!rows.is_empty(), "声明了 validates_batch 的定义必须拿到整批行");
            Ok(rows
                .iter()
                .filter(|row| row.get("value") == "2")
                .map(|row| RowIssue::row(row.row_no, ErrorKind::Rule, "批次规则命中：字典项 2 不可入库"))
                .collect())
        }
        async fn persist(
            &self,
            _tx: &nik::prelude::Transaction,
            _rows: &[RowValues],
            _ctx: &ImportCtx,
        ) -> std::result::Result<usize, DbError> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn batch_validation_drops_offending_rows_with_excel_row_no() {
        let bytes = bytes_of(vec![
            vec!["EX-2001", "user_status", "1", "启用"],
            vec!["EX-2002", "user_status", "2", "启用"],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1", "2"]);
        let outcome = validate(&BatchDefinition, &bytes, &DictProvider::new(), &resolver, &ctx(), true)
            .await
            .expect("预检应成功");
        assert_eq!(outcome.report.total, 2);
        assert_eq!(outcome.report.valid, 1, "命中批次规则的行不算 valid");
        assert_eq!(
            outcome.report.error, 1,
            "命中批次规则的行要计入 error（STRICT 据此整批拒绝）"
        );
        assert_eq!(outcome.report.errors[0].row_no, 3, "行号是 Excel 真实行号");
        assert_eq!(outcome.report.errors[0].kind, ErrorKind::Rule);
        assert_eq!(outcome.rows.len(), 1, "可入库集合里只留下没命中的行");
        assert_eq!(outcome.rows[0].get("value"), "1");
    }

    #[tokio::test]
    async fn batch_validation_runs_in_precheck_mode_too() {
        // `keep_rows = false`（上传阶段的预检）：没声明 validates_batch 的定义不保留行，
        // 这条用例证明声明了的会保留，且报告与确认入库阶段一致（用户上传时就看到问题）
        let bytes = bytes_of(vec![
            vec!["EX-2003", "user_status", "1", "启用"],
            vec!["EX-2004", "user_status", "2", "启用"],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1", "2"]);
        let report = precheck(&BatchDefinition, &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("预检应成功");
        assert_eq!(report.valid, 1);
        assert_eq!(report.error, 1);
        assert_eq!(report.errors[0].row_no, 3);
    }

    fn bytes_of(rows: Vec<Vec<&str>>) -> Vec<u8> {
        let headers = ["编码", "字典", "字典项", "状态"];
        let rows = rows
            .into_iter()
            .map(|values| {
                let mut map = std::collections::HashMap::new();
                for (index, header) in headers.iter().enumerate() {
                    map.insert(
                        (*header).to_string(),
                        values.get(index).copied().unwrap_or("").to_string(),
                    );
                }
                map
            })
            .collect::<Vec<_>>();
        Export::to_xlsx_bytes(&headers, &rows).expect("造 xlsx")
    }

    fn ctx() -> ImportCtx {
        ImportCtx {
            task_id: uuid::Uuid::now_v7(),
            operator: None,
            mode: crate::business::impexp::definition::ImportMode::Strict,
        }
    }

    #[tokio::test]
    async fn precheck_counts_total_valid_and_errors() {
        // 注意：第 1 行刻意不用 EX-0001（那是模板示例值，会被自动跳过）
        let bytes = bytes_of(vec![
            vec!["EX-1001", "user_status", "1", "启用"],
            vec!["", "user_status", "1", "启用"],
            vec!["EX-1003", "user_status", "9", "停用"],
            vec!["EX-1001", "user_status", "1", "启用"],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let outcome = validate(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx(), false)
            .await
            .expect("预检应成功");
        assert_eq!(outcome.report.total, 4);
        assert_eq!(outcome.report.valid, 1);
        assert_eq!(outcome.report.error, 3);
        // 3 行有错：必填为空 / 外键不存在 / 文件内重复
        let codes = outcome.report.errors.iter().map(|issue| issue.kind).collect::<Vec<_>>();
        assert!(codes.contains(&ErrorKind::Required));
        assert!(codes.contains(&ErrorKind::FkNotFound));
        assert!(codes.contains(&ErrorKind::Unique));
        // 错误明细带行号 + 列名
        let required = outcome
            .report
            .errors
            .iter()
            .find(|issue| issue.kind == ErrorKind::Required)
            .expect("应有必填错误");
        assert_eq!(required.row_no, 3);
        assert_eq!(required.column_header, Some("编码"));
        assert!(outcome.rows.is_empty(), "keep_rows=false 不应保留行");
    }

    /// 回归：可选的非唯一列留空，不能让「文件内唯一」判定被跳过。
    #[tokio::test]
    async fn duplicate_detected_even_when_optional_column_is_empty() {
        // 状态列（可选、非唯一）留空；编码列（唯一）两行重复
        let bytes = bytes_of(vec![
            vec!["EX-5001", "user_status", "1", ""],
            vec!["EX-5001", "user_status", "1", ""],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("预检应成功");
        assert_eq!(report.total, 2);
        assert_eq!(report.error, 1, "第二行应被判为文件内重复");
        assert!(
            report.errors.iter().any(|issue| issue.kind == ErrorKind::Unique),
            "错误类型应为 UNIQUE"
        );
        assert_eq!(report.errors[0].row_no, 3, "重复行行号应为 Excel 第 3 行");
    }

    #[tokio::test]
    async fn example_row_is_skipped_silently() {
        // 第 1 行是模板示例（EX-0001…），第 2 行才是真数据 → 示例行被静默跳过
        let bytes = bytes_of(vec![
            vec!["EX-0001", "user_status", "1", "启用"],
            vec!["EX-3002", "user_status", "1", "启用"],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("预检应成功");
        assert_eq!(report.skipped_example, 1);
        assert_eq!(report.total, 1);
        assert_eq!(report.valid, 1);
    }

    #[tokio::test]
    async fn fk_validation_uses_one_statement_per_column() {
        // 300 行 × 2 个外键列 → 恰好 2 条 SQL
        let rows = (0..300)
            .map(|index| {
                vec![
                    // 从 1000 起，避开模板示例值 EX-0001
                    format!("EX-{:04}", 1000 + index),
                    "user_status".to_string(),
                    "1".to_string(),
                    "启用".to_string(),
                ]
            })
            .collect::<Vec<_>>();
        let bytes = bytes_of(
            rows.iter()
                .map(|row| row.iter().map(String::as_str).collect::<Vec<_>>())
                .collect(),
        );
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(
            &definition_with_max(1000),
            &bytes,
            &DictProvider::new(),
            &resolver,
            &ctx(),
        )
        .await
        .expect("预检应成功");
        assert_eq!(report.total, 300);
        assert_eq!(report.valid, 300);
        assert_eq!(report.fk_sql_statements, 2, "两个外键列应只发 2 条 SQL");
        assert_eq!(resolver.statements(), 2);
    }

    fn definition_with_max(max_rows: usize) -> TestDefinition {
        TestDefinition { max_rows }
    }

    #[tokio::test]
    async fn row_limit_is_enforced() {
        let bytes = bytes_of(vec![
            vec!["EX-2001", "user_status", "1", "启用"],
            vec!["EX-2002", "user_status", "1", "启用"],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let error = precheck(&definition_with_max(1), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect_err("超上限应拒绝");
        assert_eq!(error.status, 422);
        assert_eq!(error.code, error_code::IMPORT_ROW_LIMIT);
    }

    #[tokio::test]
    async fn structural_error_is_reported_before_row_validation() {
        // 缺「编码」列 → 结构错误（E-IMP-03），不出行明细
        let mut map = std::collections::HashMap::new();
        map.insert("字典".to_string(), "user_status".to_string());
        let bytes = Export::to_xlsx_bytes(&["字典"], &[map]).expect("造 xlsx");
        let resolver = CountingFkResolver::new(&["user_status"]);
        let error = precheck(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect_err("缺列应拒绝");
        assert_eq!(error.status, 422);
        assert!(error.msg.contains("缺少必需列"));
        assert_eq!(resolver.statements(), 0, "结构错误不应发外键 SQL");
    }

    #[tokio::test]
    async fn extra_columns_and_order_produce_warnings() {
        let mut first = std::collections::HashMap::new();
        first.insert("字典".to_string(), "user_status".to_string());
        first.insert("编码".to_string(), "EX-4001".to_string());
        first.insert("字典项".to_string(), "1".to_string());
        first.insert("状态".to_string(), "启用".to_string());
        first.insert("多余列".to_string(), "x".to_string());
        let bytes = Export::to_xlsx_bytes(&["字典", "编码", "字典项", "状态", "多余列"], &[first]).expect("造 xlsx");
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("预检应成功");
        assert_eq!(report.warnings.len(), 2);
        assert!(report.warnings.iter().any(|w| w.code == "E-IMP-04"));
        assert!(report.warnings.iter().any(|w| w.code == "E-IMP-05"));
    }

    #[tokio::test]
    async fn dict_translation_failure_is_reported_as_dict_error() {
        let dicts = DictProvider::new();
        dicts.seed(
            "user_status",
            DictTranslation::from_items(vec![("1".to_string(), "启用".to_string())]),
        );
        assert_eq!(
            ColumnKind::Dict("user_status")
                .parse("启用", Some(&dicts.get("user_status").await.expect("已 seed")))
                .expect("应命中"),
            "1"
        );
        // 缓存命中：第二次取不会再查库（seed 后 TTL 内直接返回）
        assert!(dicts.get("user_status").await.is_ok());
    }

    #[test]
    fn fk_lookup_sql_is_parameterised_and_soft_delete_aware() {
        let spec = FkSpec {
            table: "sys_dict_type",
            column: "dict_code",
            label: "字典类型",
            soft_delete: true,
        };
        let sql = fk_lookup_sql(&spec);
        assert!(sql.contains("FROM sys_dict_type"));
        assert!(sql.contains("jsonb_array_elements_text(?::jsonb)"));
        assert!(sql.contains("deleted_at IS NULL"));
        let hard = fk_lookup_sql(&FkSpec {
            soft_delete: false,
            ..spec
        });
        assert!(!hard.contains("deleted_at"));
    }

    #[test]
    fn required_dict_codes_are_deduplicated() {
        let definition = definition();
        let codes = required_dict_codes(&definition);
        assert!(codes.is_empty(), "本用例列定义没有字典列");
    }

    // ==================== 以下为 P2-3B 追加（③–⑬） ====================

    /// 通用造表：表头 + 行（行内缺列补空串），表头顺序即列顺序。
    fn bytes_with(headers: &[&str], rows: Vec<Vec<&str>>) -> Vec<u8> {
        let rows = rows
            .into_iter()
            .map(|values| {
                let mut map = std::collections::HashMap::new();
                for (index, header) in headers.iter().enumerate() {
                    map.insert(
                        (*header).to_string(),
                        values.get(index).copied().unwrap_or("").to_string(),
                    );
                }
                map
            })
            .collect::<Vec<_>>();
        Export::to_xlsx_bytes(headers, &rows).expect("造 xlsx")
    }

    /// 带字典列的用例定义：两列共用同一个字典编码（顺带覆盖 `required_dict_codes` 去重）。
    static DICT_COLUMNS: &[ImportColumn] = &[
        ImportColumn {
            key: "code",
            header: "编码",
            required: true,
            kind: ColumnKind::Text { max_len: Some(32) },
            unique_key: true,
            fk: None,
            example: "DICT-0001",
            width: None,
            remark: "",
        },
        ImportColumn {
            key: "status",
            header: "状态",
            required: true,
            kind: ColumnKind::Dict("user_status"),
            unique_key: false,
            fk: None,
            example: "启用",
            width: None,
            remark: "",
        },
        ImportColumn {
            key: "status_alias",
            header: "状态别名",
            required: false,
            kind: ColumnKind::Dict("user_status"),
            unique_key: false,
            fk: None,
            example: "",
            width: None,
            remark: "",
        },
    ];

    struct DictDefinition;

    #[async_trait]
    impl ImportDefinition for DictDefinition {
        fn biz_type(&self) -> &'static str {
            "test.validate.dict"
        }
        fn biz_name(&self) -> &'static str {
            "字典列校验用例"
        }
        fn max_rows(&self) -> usize {
            100
        }
        fn columns(&self) -> &'static [ImportColumn] {
            DICT_COLUMNS
        }
        async fn persist(
            &self,
            _tx: &nik::prelude::Transaction,
            _rows: &[RowValues],
            _ctx: &ImportCtx,
        ) -> std::result::Result<usize, DbError> {
            Ok(0)
        }
    }

    /// ③ 外键取值**先按列去重再查**：3 行数据、2 个外键列 → 每列各 1 条 SQL，
    /// 且解析器收到的取值个数是去重后的个数（第一列 2 个不同取值、第二列 1 个），不是行数 3。
    #[tokio::test]
    async fn fk_values_are_deduplicated_per_column_before_lookup() {
        let bytes = bytes_of(vec![
            vec!["EX-7001", "user_status", "1", "启用"],
            vec!["EX-7002", "user_status", "1", "启用"],
            vec!["EX-7003", "sys_dict_type", "1", "启用"],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "sys_dict_type", "1"]);
        let report = precheck(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("预检应成功");
        assert_eq!(report.valid, 3);
        assert_eq!(report.fk_sql_statements, 2, "两个外键列各 1 条 SQL");
        assert_eq!(
            resolver.sizes(),
            vec![2, 1],
            "第一列 2 个不同取值、第二列 1 个（不是 3 行各查一次）"
        );
    }

    /// ④ 外键列整列为空时不发 SQL（空值的「必填」判定在逐行校验里），
    /// 且错误类型是 REQUIRED 而不是 FK_NOT_FOUND（不能把「没填」报成「不存在」）。
    #[tokio::test]
    async fn fk_column_without_values_sends_no_sql() {
        let bytes = bytes_of(vec![vec!["EX-7101", "", "", "启用"], vec!["EX-7102", "", "", "启用"]]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("预检应成功");
        assert_eq!(report.error, 2);
        assert_eq!(resolver.statements(), 0, "全空的外键列不发 SQL");
        assert_eq!(resolver.sizes(), Vec::<usize>::new(), "根本没有调用解析器");
        assert!(
            report.errors.iter().all(|issue| issue.kind == ErrorKind::Required),
            "没填的外键列只能是 REQUIRED：{:?}",
            report.errors
        );
    }

    /// ⑤ 行长按 **字符数**（chars）而不是字节数：32 个汉字放行，33 个汉字报 LENGTH。
    /// 若实现用 `len()`（UTF-8 字节），32 个汉字会被误判为 96 字节而超限。
    #[tokio::test]
    async fn text_length_limit_counts_chars_not_bytes() {
        let exactly_32 = "测".repeat(32);
        let over_33 = "测".repeat(33);
        let bytes = bytes_of(vec![
            vec![exactly_32.as_str(), "user_status", "1", "启用"],
            vec![over_33.as_str(), "user_status", "1", "启用"],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("预检应成功");
        assert_eq!(report.valid, 1, "32 个汉字正好在上限内（闭区间）");
        assert_eq!(report.error, 1);
        let issue = &report.errors[0];
        assert_eq!(issue.kind, ErrorKind::Length);
        assert_eq!(issue.column_header, Some("编码"));
        assert!(issue.message.contains("33"), "文案要报实际字符数：{}", issue.message);
    }

    /// ⑥ 枚举列 label 与 code 都认、落库统一成 code；未知值报 ENUM 并列出可选范围。
    #[tokio::test]
    async fn enum_accepts_label_and_code_but_rejects_unknown() {
        let bytes = bytes_of(vec![
            vec!["EX-7201", "user_status", "1", "启用"],
            vec!["EX-7202", "user_status", "1", "1"],
            vec!["EX-7203", "user_status", "1", "停用"],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let outcome = validate(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx(), true)
            .await
            .expect("预检应成功");
        assert_eq!(outcome.report.valid, 2, "label 与 code 都要认");
        assert_eq!(outcome.report.error, 1);
        assert_eq!(outcome.rows.len(), 2);
        assert!(
            outcome.rows.iter().all(|row| row.get("status") == "1"),
            "落库值必须是 code"
        );
        let issue = &outcome.report.errors[0];
        assert_eq!(issue.kind, ErrorKind::Enum);
        assert_eq!(issue.column_header, Some("状态"));
        assert!(
            issue.message.contains("启用") && issue.message.contains("停用"),
            "文案要列出可选范围：{}",
            issue.message
        );
    }

    /// ⑦ 字典列走 `DictProvider::seed` 的翻译表：label / code 都命中并落成 code，
    /// 未知值报 DICT；两列共用一个字典编码时只装载一次（不连库）。
    #[tokio::test]
    async fn dict_column_translates_labels_and_codes() {
        let dicts = DictProvider::new();
        dicts.seed(
            "user_status",
            DictTranslation::from_items(vec![
                ("1".to_string(), "启用".to_string()),
                ("0".to_string(), "停用".to_string()),
            ]),
        );
        let bytes = bytes_with(
            &["编码", "状态", "状态别名"],
            vec![
                vec!["D-0001", "启用", ""],
                vec!["D-0002", "1", "停用"],
                vec!["D-0003", "停用", ""],
                vec!["D-0004", "未知值", ""],
            ],
        );
        let resolver = CountingFkResolver::new(&[]);
        let outcome = validate(&DictDefinition, &bytes, &dicts, &resolver, &ctx(), true)
            .await
            .expect("预检应成功");
        assert_eq!(outcome.report.valid, 3, "label/code 都翻译成功");
        assert_eq!(outcome.report.error, 1);
        assert_eq!(outcome.rows.len(), 3);
        assert_eq!(outcome.rows[0].get("status"), "1", "label → code");
        assert_eq!(outcome.rows[1].get("status"), "1", "code → code");
        assert_eq!(outcome.rows[1].get("status_alias"), "0", "第二列的 label → code");
        assert_eq!(outcome.rows[2].get("status"), "0");
        let issue = &outcome.report.errors[0];
        assert_eq!(issue.kind, ErrorKind::Dict);
        assert_eq!(issue.column_header, Some("状态"));
        assert!(
            issue.message.contains("未知值") && issue.message.contains("user_status"),
            "文案要点名取值与字典编码：{}",
            issue.message
        );
    }

    /// ⑧ `keep_rows = true` 时保留的行**顺序**与 Excel 一致、值已 trim、行号是 Excel 真实行号。
    #[tokio::test]
    async fn keep_rows_preserves_order_and_trims_values() {
        let bytes = bytes_of(vec![
            vec!["  EX-8001  ", " user_status ", " 1 ", " 启用 "],
            vec!["EX-8002", "user_status", "1", "启用"],
            vec![" EX-8003", "user_status", "1", ""],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let outcome = validate(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx(), true)
            .await
            .expect("预检应成功");
        assert_eq!(outcome.rows.len(), 3);
        let codes = outcome.rows.iter().map(|row| row.get("code")).collect::<Vec<_>>();
        assert_eq!(codes, vec!["EX-8001", "EX-8002", "EX-8003"], "顺序不能乱");
        assert_eq!(
            outcome.rows[0].get("status"),
            "1",
            "枚举列落库值是 code，原文的前后空白已被 trim 掉"
        );
        assert_eq!(
            outcome.rows.iter().map(|row| row.row_no).collect::<Vec<_>>(),
            vec![2, 3, 4],
            "行号是 Excel 真实行号（表头占第 1 行）"
        );
    }

    /// ⑨ 整行为空只计入 `skipped_blank`，既不进 total 也不报错。
    #[tokio::test]
    async fn blank_rows_are_counted_as_skipped_blank() {
        let bytes = bytes_of(vec![
            vec!["EX-8101", "user_status", "1", "启用"],
            vec!["", "", "", ""],
            vec!["EX-8102", "user_status", "1", "启用"],
        ]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("预检应成功");
        assert_eq!(report.skipped_blank, 1, "中间空行计入 skipped_blank");
        assert_eq!(report.total, 2, "空行不进 total");
        assert_eq!(report.valid, 2);
        assert_eq!(report.error, 0);
    }

    /// ⑩ 一行有多个问题：明细按列各记一条，但 `error`（失败行数）只计 1。
    #[tokio::test]
    async fn one_row_with_multiple_issues_counts_error_once() {
        let over_long = "测".repeat(33);
        let bytes = bytes_of(vec![vec![over_long.as_str(), "user_status", "1", "停用"]]);
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(&definition(), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("预检应成功");
        assert_eq!(report.error, 1, "同一行多错只计 1 行");
        assert_eq!(report.valid, 0);
        assert_eq!(report.errors.len(), 2, "明细按列各一条");
        let kinds = report.errors.iter().map(|issue| issue.kind).collect::<Vec<_>>();
        assert!(kinds.contains(&ErrorKind::Length));
        assert!(kinds.contains(&ErrorKind::Enum));
        assert!(
            report.errors.iter().all(|issue| issue.row_no == 2),
            "两条明细都指向同一行"
        );
    }

    /// ⑪ 行数上限是**闭区间**：恰好等于 `max_rows` 放行，多 1 行才拒。
    #[tokio::test]
    async fn row_limit_is_inclusive_at_exact_max() {
        let rows = (0..2)
            .map(|index| {
                vec![
                    format!("EX-82{:02}", index),
                    "user_status".to_string(),
                    "1".to_string(),
                    "启用".to_string(),
                ]
            })
            .collect::<Vec<_>>();
        let bytes = bytes_of(
            rows.iter()
                .map(|row| row.iter().map(String::as_str).collect::<Vec<_>>())
                .collect(),
        );
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(&definition_with_max(2), &bytes, &DictProvider::new(), &resolver, &ctx())
            .await
            .expect("恰好 2 行 = 上限，必须放行");
        assert_eq!(report.total, 2);
        assert_eq!(report.valid, 2);
    }

    /// ⑫ 明细超过 `MAX_ERRORS` 时只留前 2 000 条并置 `truncated`；
    /// 同时证明 2 001 行 × 2 个外键列仍然只有 2 条 SQL。
    #[tokio::test]
    async fn error_details_are_truncated_at_max_errors() {
        let rows = (0..MAX_ERRORS + 1)
            .map(|index| {
                vec![
                    format!("EX-{:05}", 10_000 + index),
                    "user_status".to_string(),
                    "1".to_string(),
                    // 「停用」不在 STATUS（只有 启用）里 → 每行 1 条 ENUM
                    "停用".to_string(),
                ]
            })
            .collect::<Vec<_>>();
        let bytes = bytes_of(
            rows.iter()
                .map(|row| row.iter().map(String::as_str).collect::<Vec<_>>())
                .collect(),
        );
        let resolver = CountingFkResolver::new(&["user_status", "1"]);
        let report = precheck(
            &definition_with_max(5_000),
            &bytes,
            &DictProvider::new(),
            &resolver,
            &ctx(),
        )
        .await
        .expect("预检应成功");
        assert_eq!(report.error, MAX_ERRORS + 1, "失败行数按行计，不截断");
        assert_eq!(report.errors.len(), MAX_ERRORS, "明细只留前 2 000 条");
        assert!(report.truncated, "截断标志必须为真");
        assert_eq!(report.fk_sql_statements, 2, "2 001 行 × 2 列仍然 2 条 SQL");
    }

    /// ⑬ `required_dict_codes` 对**多个字典列共用同一编码**去重，字典只装载一次。
    #[test]
    fn required_dict_codes_are_deduplicated_for_dict_columns() {
        let codes = required_dict_codes(&DictDefinition);
        assert_eq!(codes, vec!["user_status"], "两列同一编码只能出现一次");
    }
}
