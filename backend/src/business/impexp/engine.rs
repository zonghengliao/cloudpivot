//! Excel 解析 / 写入引擎（xlsx 读写）。
//!
//! # 读：为什么直连 `calamine` 而不只用 `Import::from_xlsx`
//!
//! 框架封装（`nik_utils::export::Import`）返回 `Vec<HashMap<String,String>>`：
//!
//! 1. **不暴露表头**——「只有表头没有数据行」(E-IMP-02)、「表头缺必需列」(E-IMP-03)、
//!    「存在模板外的列」(E-IMP-04)、「列顺序与模板不一致」(E-IMP-05) 四条结构校验都必须
//!    先读到表头，封装层拿不到（只有表头时返回空 `Vec`）；
//! 2. **每行一个 HashMap**——10 万行时在 `Range` 之外再多一份几百 MB 的堆分配，
//!    与验收 4 的 400 MB 上限冲突。
//!
//! 因此读侧直接用 `calamine`（**与 `nik-utils` 的 xlsx feature 同一版本 0.36，依赖树里
//! 只有一份**）：`worksheet_range` 拿到紧凑的 `Range<Data>`，再用 [`SheetReader::row`]
//! **逐行**取，全表不产生第二份行集合。写侧仍走框架 `Export::to_xlsx_workbook`。
//!
//! # 行号
//!
//! [`SheetReader::row`] 的下标 0 对应 Excel 第 2 行（第 1 行是表头），
//! Excel 真实行号 = 下标 + 2，由 [`SheetReader::excel_row_no`] 统一换算。

use std::collections::HashMap;
use std::io::Cursor;

use calamine::{Data, Reader, Xlsx};
use nik::prelude::*;

use crate::business::impexp::definition::{DictTranslation, ExportColumn, ImportColumn};
use crate::business::masking::masker::MaskError;
use crate::business::masking::registry::MaskSet;
use crate::shared::error_code;

/// xlsx 文件头（zip 容器魔数 `PK\x03\x04`）。
pub const XLSX_MAGIC: &[u8; 4] = b"PK\x03\x04";
/// 旧版 xls（OLE2 复合文档）魔数。
pub const XLS_MAGIC: &[u8; 4] = &[0xD0, 0xCF, 0x11, 0xE0];

/// 解析阶段的结构性错误（整表级，任一出现即终止，不出行明细）。
#[derive(Debug, Clone)]
pub enum EngineError {
    /// 文件不是 xlsx（魔数不符）。
    NotXlsx,
    /// 旧版 `.xls`，本卡不支持（要求另存为 xlsx）。
    LegacyXls,
    /// 文件损坏 / 不是合法工作簿。
    Broken(String),
    /// 工作簿里没有工作表。
    NoSheet,
    /// 只有表头，没有数据行（E-IMP-02）。
    NoDataRows,
    /// 表头缺少必需列（E-IMP-03），值为中文列名列表。
    MissingHeaders(Vec<&'static str>),
}

impl EngineError {
    /// 转成对外 [`ApiError`]（PRD 附录 A 的 E-IMP 文案）。
    ///
    /// # 返回值
    ///
    /// HTTP 400（格式类）或 422（内容类）错误。
    pub fn into_api_error(self) -> ApiError {
        match self {
            EngineError::NotXlsx => ApiError::new(
                400,
                error_code::IMPORT_FILE_INVALID,
                "文件格式不支持，请上传 Excel 文件（.xlsx）",
            ),
            EngineError::LegacyXls => ApiError::new(
                400,
                error_code::IMPORT_FILE_INVALID,
                "暂不支持 .xls 旧格式，请在 Excel 里另存为 .xlsx 后重新上传",
            ),
            EngineError::Broken(detail) => ApiError::new(
                400,
                error_code::IMPORT_FILE_INVALID,
                format!("文件无法解析（{detail}），请确认是有效的 .xlsx 文件"),
            ),
            EngineError::NoSheet => ApiError::new(
                400,
                error_code::IMPORT_FILE_INVALID,
                "工作簿内没有工作表，请重新下载模板填写",
            ),
            EngineError::NoDataRows => ApiError::new(
                422,
                error_code::IMPORT_FILE_INVALID,
                "文件中没有数据行，请填写后重新上传",
            ),
            EngineError::MissingHeaders(headers) => ApiError::new(
                422,
                error_code::IMPORT_FILE_INVALID,
                format!("文件第 1 行表头缺少必需列：{}", headers.join("、")),
            ),
        }
    }
}

/// 表头映射结果。
#[derive(Debug, Clone, Default)]
pub struct HeaderMap {
    /// 定义列 → 文件中的列下标（`None` 表示该列在文件里缺失）。
    pub indexes: Vec<Option<usize>>,
    /// 缺失的必需列中文名。
    pub missing_required: Vec<&'static str>,
    /// 模板外的列（表头原文，按文件顺序）。
    pub extra_headers: Vec<String>,
    /// 列顺序与模板不一致（仅警告，按列名匹配）。
    pub order_mismatch: bool,
    /// 解析到的工作表名。
    pub sheet_name: String,
}

/// 一行原始单元格（借自 [`SheetReader`] 内部的 `Range`）。
pub type RawRow<'a> = &'a [Data];

/// xlsx 工作表读取器：一次打开，逐行取用。
pub struct SheetReader {
    range: calamine::Range<Data>,
    header: HeaderMap,
    columns: &'static [ImportColumn],
}

impl SheetReader {
    /// 打开字节流并解析表头。
    ///
    /// # 参数
    ///
    /// - `bytes`：上传文件字节。
    /// - `columns`：业务列定义（决定表头匹配与缺失判定）。
    ///
    /// # 返回值
    ///
    /// 成功返回读取器（此时表头已校验：缺必需列直接返回
    /// [`EngineError::MissingHeaders`]）。
    ///
    /// # 异常情况
    ///
    /// 见 [`EngineError`]。
    pub fn open(bytes: &[u8], columns: &'static [ImportColumn]) -> std::result::Result<Self, EngineError> {
        if bytes.len() < 4 {
            return Err(EngineError::NotXlsx);
        }
        if bytes[..4] == XLS_MAGIC[..] {
            return Err(EngineError::LegacyXls);
        }
        if bytes[..4] != XLSX_MAGIC[..] {
            return Err(EngineError::NotXlsx);
        }

        let mut workbook: Xlsx<Cursor<&[u8]>> =
            Xlsx::new(Cursor::new(bytes)).map_err(|error| EngineError::Broken(error.to_string()))?;
        // 「数据」sheet 优先（模板的第 1 个 sheet），否则用第一个 sheet：
        // 用户另存 / 重命名 sheet 后仍然能导入，报错文案里的行号也仍然是 Excel 行号。
        let names = workbook.sheet_names().to_vec();
        if names.is_empty() {
            return Err(EngineError::NoSheet);
        }
        let sheet_name = names
            .iter()
            .find(|name| name.as_str() == "数据")
            .cloned()
            .unwrap_or_else(|| names[0].clone());
        let range = workbook
            .worksheet_range(&sheet_name)
            .map_err(|error| EngineError::Broken(error.to_string()))?;

        let mut first = range.rows();
        let header_row = first.next().unwrap_or(&[]);
        let header = build_header(header_row, columns, &sheet_name);
        if !header.missing_required.is_empty() {
            return Err(EngineError::MissingHeaders(header.missing_required.clone()));
        }
        if range.height() <= 1 {
            return Err(EngineError::NoDataRows);
        }
        Ok(Self { range, header, columns })
    }

    /// 表头映射结果。
    pub fn header(&self) -> &HeaderMap {
        &self.header
    }

    /// 数据行数（不含表头；含尾部空行，由调用方按行判定空行）。
    pub fn data_len(&self) -> usize {
        self.range.height().saturating_sub(1)
    }

    /// 取第 `idx` 个数据行（`idx = 0` 对应 Excel 第 2 行）。
    ///
    /// # 返回值
    ///
    /// 越界返回 `None`。
    ///
    /// # 性能说明
    ///
    /// 底层 `Range::rows()` 是**顺序迭代器**，`nth()` 每次都从头推进（O(idx)）；
    /// 在 10 万行上逐行调用会退化成 O(n²)（实测 debug 版 28 万毫秒 → 顺序迭代后 10 秒级）。
    /// **批量校验请用 [`Self::data_rows`]**，本方法只给「取第 N 行」的零散场景用。
    pub fn row(&self, idx: usize) -> Option<RawRow<'_>> {
        self.range.rows().nth(idx + 1)
    }

    /// 数据行**顺序**迭代器（跳过表头）。
    ///
    /// # 返回值
    ///
    /// 从 Excel 第 2 行开始的原始行迭代器；不产生任何额外行集合。
    pub fn data_rows(&self) -> impl Iterator<Item = RawRow<'_>> + '_ {
        self.range.rows().skip(1)
    }

    /// 数据行下标 → Excel 真实行号。
    ///
    /// # 参数
    ///
    /// - `idx`：数据行下标（从 0 开始）。
    pub const fn excel_row_no(idx: usize) -> u32 {
        idx as u32 + 2
    }

    /// 单元格原文（按定义列取）。
    ///
    /// # 参数
    ///
    /// - `row`：原始行。
    /// - `column_index`：定义列下标（[`HeaderMap::indexes`] 中的值）。
    ///
    /// # 返回值
    ///
    /// 单元格文本；列缺失或单元格为空时返回空串。
    pub fn cell(&self, row: RawRow<'_>, column_index: usize) -> String {
        match self.header.indexes.get(column_index).copied().flatten() {
            Some(found) => row.get(found).map(cell_text).unwrap_or_default(),
            None => String::new(),
        }
    }

    /// 该行是否整行为空（Excel 尾部空行 / 用户多余回车）。
    ///
    /// 不分配字符串：只对文本类单元格做 trim 判定。
    pub fn is_blank(row: RawRow<'_>) -> bool {
        row.iter().all(cell_is_blank)
    }

    /// 该行是否为**模板示例行**（PRD §4.2.2：与模板示例值完全一致则自动跳过）。
    ///
    /// # 参数
    ///
    /// - `row`：原始行。
    ///
    /// # 返回值
    ///
    /// 所有非空示例值都与单元格一致，且至少有一个示例值时返回 `true`。
    pub fn is_example_row(&self, row: RawRow<'_>) -> bool {
        let mut compared = 0usize;
        for (index, column) in self.columns.iter().enumerate() {
            if column.example.is_empty() {
                continue;
            }
            compared += 1;
            if self.cell(row, index).trim() != column.example.trim() {
                return false;
            }
        }
        compared > 0
    }
}

/// 表头行 → [`HeaderMap`]。
fn build_header(header_row: RawRow<'_>, columns: &'static [ImportColumn], sheet_name: &str) -> HeaderMap {
    let mut indexes = vec![None; columns.len()];
    let mut matched_order: Vec<usize> = Vec::new();
    let mut extra_headers = Vec::new();

    for (position, cell) in header_row.iter().enumerate() {
        let raw = cell_text(cell);
        let normalized = normalize_header(&raw);
        if normalized.is_empty() {
            continue;
        }
        match columns.iter().position(|column| column.header == normalized) {
            Some(column_index) => {
                if indexes[column_index].is_none() {
                    indexes[column_index] = Some(position);
                    matched_order.push(column_index);
                }
            }
            None => extra_headers.push(raw.trim().to_string()),
        }
    }

    let missing_required = columns
        .iter()
        .enumerate()
        .filter(|(index, column)| column.required && indexes[*index].is_none())
        .map(|(_, column)| column.header)
        .collect::<Vec<_>>();

    let expected_order = columns
        .iter()
        .enumerate()
        .filter(|(index, _)| indexes[*index].is_some())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let order_mismatch = matched_order != expected_order;

    HeaderMap {
        indexes,
        missing_required,
        extra_headers,
        order_mismatch,
        sheet_name: sheet_name.to_string(),
    }
}

/// 表头归一化：去首尾空白、去掉必填星标（`*` / `＊`）。
///
/// # 参数
///
/// - `raw`：表头原文。
///
/// # 返回值
///
/// 归一化后的表头（用于与定义列 `header` 比较）。
pub fn normalize_header(raw: &str) -> String {
    raw.trim().trim_end_matches(['*', '＊']).trim().to_string()
}

/// 单元格是否为空（零分配版本，用于空行判定）。
fn cell_is_blank(cell: &Data) -> bool {
    match cell {
        Data::Empty => true,
        Data::String(text) => text.trim().is_empty(),
        Data::DateTimeIso(text) | Data::DurationIso(text) => text.trim().is_empty(),
        _ => false,
    }
}

/// 单元格 → 文本。
///
/// # 参数
///
/// - `cell`：calamine 单元格。
///
/// # 返回值
///
/// 文本化结果：数字取原始精度、布尔 `true`/`false`、日期转 `YYYY-MM-DD[ HH:MM:SS]`。
pub fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(text) => text.clone(),
        Data::Int(value) => value.to_string(),
        Data::Float(value) => {
            // 1.0 → "1"（避免把整数写成 1.0 后被文本列当成「1.0」）
            if value.fract() == 0.0 && value.abs() < 1e15 {
                format!("{}", *value as i64)
            } else {
                value.to_string()
            }
        }
        Data::Bool(value) => value.to_string(),
        Data::DateTime(value) => match crate::business::impexp::definition::excel_serial_to_datetime(value.as_f64()) {
            Some(datetime) => {
                if datetime.time() == chrono::NaiveTime::MIN {
                    datetime.format("%Y-%m-%d").to_string()
                } else {
                    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
                }
            }
            None => value.as_f64().to_string(),
        },
        Data::DateTimeIso(text) | Data::DurationIso(text) => text.clone(),
        Data::Error(error) => format!("{error:?}"),
    }
}

/// 导出列 → 表头数组（单测用；生产路径直接内联 map）。
#[cfg(test)]
fn export_headers(columns: &'static [ExportColumn]) -> Vec<&'static str> {
    columns.iter().map(|column| column.header).collect()
}

/// 按导出列定义格式化数据行（枚举 / 字典翻译、布尔转是/否、日期去时间）**并同步脱敏**。
///
/// # 参数
///
/// - `columns`：导出列定义。
/// - `rows`：库中值（键为列 `key`）。
/// - `dicts`：字典翻译表（键为字典编码）。
/// - `mask`：本次导出请求者的脱敏规则集合（由 handler / worker 解析后传入）。
///
/// # 返回值
///
/// 表头为中文列名的行集合，可直接交给 `Export::to_xlsx_bytes`。
///
/// # 异常情况
///
/// - `sensitive: true` 的列**没有生效规则** → [`MaskError::MissingRule`]（422 + 61008）；
/// - 规则参数不合法 → 对应的 [`MaskError`]。
///
/// 这是**刻意的 fail-closed**（架构 §3.4 闸 3）：宁可让导出失败，也不要出一份明文
/// Excel 流到微信群里。函数签名里带 `mask` 参数同样是刻意的——它会让所有调用点
/// 在编译期就暴露出来，漏改一条路径是不可能的。
pub fn format_export_rows(
    columns: &'static [ExportColumn],
    rows: &[HashMap<&'static str, String>],
    dicts: &HashMap<String, DictTranslation>,
    mask: &MaskSet,
) -> std::result::Result<Vec<HashMap<String, String>>, MaskError> {
    rows.iter()
        .map(|row| {
            let mut formatted = HashMap::with_capacity(columns.len());
            for column in columns {
                let stored = row.get(column.key).map(String::as_str).unwrap_or("");
                let dict = match column.kind {
                    crate::business::impexp::definition::ColumnKind::Dict(code) => dicts.get(code),
                    _ => None,
                };
                let value = column.kind.format(stored, dict);
                // 豁免（超管 / 持 `system:field-mask:plain`）→ 不看规则、不脱敏，直接出明文。
                // 这是架构 §3.6 的口径，不是 fail-closed 的例外：豁免本身就是「允许明文」，
                // 且明版权限持有者的访问已由 `registry::audit_plain_read` 留痕。
                let value = if column.sensitive && !mask.is_exempt() {
                    match mask.mask_value(column.table, column.key, &value) {
                        Ok(Some(masked)) => masked,
                        Ok(None) => {
                            return Err(MaskError::MissingRule {
                                table: column.table.to_string(),
                                column: column.key.to_string(),
                            });
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    value
                };
                formatted.insert(column.header.to_string(), value);
            }
            Ok(formatted)
        })
        .collect()
}

/// 生成 xlsx 字节流（单 sheet）。
///
/// # 参数
///
/// - `headers`：中文表头。
/// - `rows`：行数据（键必须与表头一致）。
///
/// # 返回值
///
/// xlsx 字节流。
///
/// # 异常情况
///
/// 写入失败（底层 `rust_xlsxwriter` 报错）返回 500。
pub fn write_sheet(headers: &[&str], rows: &[HashMap<String, String>]) -> std::result::Result<Vec<u8>, ApiError> {
    Export::to_xlsx_bytes(headers, rows)
        .map_err(|error| ApiError::new(500, error_code::INTERNAL_ERROR, format!("生成 Excel 失败：{error}")))
}

/// 多 sheet 写入的入参。
pub struct WorkbookSheet<'a> {
    /// sheet 名。
    pub name: &'a str,
    /// 表头（列顺序即模板列顺序）。
    pub headers: &'a [&'a str],
    /// 行数据。
    pub rows: &'a [HashMap<String, String>],
    /// 该 sheet 的数据验证（Excel 下拉 / 数值范围 / 日期范围）；空切片表示不加。
    pub validations: &'a [XlsxValidation<'a>],
}

impl<'a> WorkbookSheet<'a> {
    /// 纯数据 sheet（不带数据验证）。
    pub fn new(name: &'a str, headers: &'a [&'a str], rows: &'a [HashMap<String, String>]) -> Self {
        Self {
            name,
            headers,
            rows,
            validations: &[],
        }
    }

    /// 追加数据验证（逐列一条，见 [`XlsxValidation`]）。
    pub fn with_validations(mut self, validations: &'a [XlsxValidation<'a>]) -> Self {
        self.validations = validations;
        self
    }
}

/// 生成多 sheet xlsx 字节流（模板用）。
///
/// # 参数
///
/// - `sheets`：每个 sheet 的名称 / 表头 / 行数据 / 数据验证。
///
/// # 返回值
///
/// xlsx 字节流。
///
/// # 异常情况
///
/// 写入失败返回 500。
pub fn write_workbook(sheets: &[WorkbookSheet<'_>]) -> std::result::Result<Vec<u8>, ApiError> {
    let built = sheets
        .iter()
        .map(|sheet| XlsxSheet::new(sheet.name, sheet.headers, sheet.rows).with_validations(sheet.validations))
        .collect::<Vec<_>>();
    Export::to_xlsx_workbook(&built)
        .map_err(|error| ApiError::new(500, error_code::INTERNAL_ERROR, format!("生成 Excel 失败：{error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::impexp::definition::{ColumnKind, EnumItem, FkSpec, ImportColumn};

    const STATUS: &[EnumItem] = &[EnumItem {
        label: "启用",
        code: "1",
    }];

    static COLUMNS: &[ImportColumn] = &[
        ImportColumn {
            key: "code",
            header: "编码",
            required: true,
            kind: ColumnKind::Text { max_len: Some(32) },
            unique_key: true,
            fk: None,
            example: "MAT-000001",
            width: None,
            remark: "",
        },
        ImportColumn {
            key: "name",
            header: "名称",
            required: true,
            kind: ColumnKind::Text { max_len: Some(64) },
            unique_key: false,
            fk: Some(FkSpec {
                table: "sys_dict_type",
                column: "dict_code",
                label: "字典",
                soft_delete: true,
            }),
            example: "螺丝 M6×20",
            width: None,
            remark: "",
        },
        ImportColumn {
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

    /// 造一张表：`rows` 为数据行（不含表头）。
    fn build_xlsx(headers: &[&str], rows: Vec<Vec<&str>>) -> Vec<u8> {
        let rows = rows
            .into_iter()
            .map(|values| {
                let mut map = HashMap::new();
                for (index, header) in headers.iter().enumerate() {
                    map.insert(
                        (*header).to_string(),
                        values.get(index).copied().unwrap_or("").to_string(),
                    );
                }
                map
            })
            .collect::<Vec<_>>();
        Export::to_xlsx_bytes(headers, &rows).expect("生成测试 xlsx 应成功")
    }

    #[test]
    fn opens_real_xlsx_and_maps_headers() {
        let bytes = build_xlsx(
            &["编码", "名称", "状态"],
            vec![vec!["MAT-000001", "螺丝", "启用"], vec!["MAT-000002", "螺母", "停用"]],
        );
        let reader = SheetReader::open(&bytes, COLUMNS).expect("应能打开");
        assert_eq!(reader.data_len(), 2);
        assert_eq!(reader.header().indexes, vec![Some(0), Some(1), Some(2)]);
        assert!(!reader.header().order_mismatch);
        let first = reader.row(0).expect("第一行");
        assert_eq!(reader.cell(first, 0), "MAT-000001");
        assert_eq!(SheetReader::excel_row_no(0), 2);
        assert_eq!(SheetReader::excel_row_no(4), 6);
    }

    #[test]
    fn header_with_star_and_reordered_columns_is_accepted_with_warning() {
        let bytes = build_xlsx(&["名称*", "状态", "编码＊"], vec![vec!["螺丝", "启用", "MAT-000001"]]);
        let reader = SheetReader::open(&bytes, COLUMNS).expect("应能打开");
        assert_eq!(reader.header().indexes, vec![Some(2), Some(0), Some(1)]);
        assert!(reader.header().order_mismatch, "列顺序不一致应被标记");
        let row = reader.row(0).expect("第一行");
        assert_eq!(reader.cell(row, 0), "MAT-000001");
    }

    #[test]
    fn missing_required_header_is_rejected() {
        let bytes = build_xlsx(&["编码", "状态"], vec![vec!["MAT-1", "启用"]]);
        let error = SheetReader::open(&bytes, COLUMNS).err().expect("缺必填列应拒绝");
        match error {
            EngineError::MissingHeaders(headers) => assert_eq!(headers, vec!["名称"]),
            other => panic!("错误类型不符: {other:?}"),
        }
    }

    #[test]
    fn extra_columns_are_reported_not_fatal() {
        let bytes = build_xlsx(
            &["编码", "名称", "备注1", "备注2"],
            vec![vec!["MAT-1", "螺丝", "x", "y"]],
        );
        let reader = SheetReader::open(&bytes, COLUMNS).expect("多余列只警告");
        assert_eq!(reader.header().extra_headers, vec!["备注1", "备注2"]);
    }

    #[test]
    fn header_only_file_reports_no_data_rows() {
        let bytes = build_xlsx(&["编码", "名称"], vec![]);
        let error = SheetReader::open(&bytes, COLUMNS).err().expect("只有表头应报错");
        assert!(matches!(error, EngineError::NoDataRows));
        assert_eq!(error.into_api_error().status, 422);
    }

    #[test]
    fn non_xlsx_magic_is_rejected() {
        let error = SheetReader::open(b"not a zip file", COLUMNS).err().expect("魔数不符");
        assert!(matches!(error, EngineError::NotXlsx));
        let legacy = SheetReader::open(&[0xD0, 0xCF, 0x11, 0xE0, 0, 0], COLUMNS)
            .err()
            .expect("旧 xls 应拒绝");
        assert!(matches!(legacy, EngineError::LegacyXls));
    }

    #[test]
    fn blank_and_example_rows_are_recognised() {
        let bytes = build_xlsx(
            &["编码", "名称", "状态"],
            vec![
                vec!["MAT-000001", "螺丝 M6×20", "启用"],
                vec!["", "", ""],
                vec!["MAT-000003", "螺母", ""],
            ],
        );
        let reader = SheetReader::open(&bytes, COLUMNS).expect("应能打开");
        let example = reader.row(0).expect("第 1 行");
        assert!(reader.is_example_row(example), "示例行应被识别");
        let blank = reader.row(1).expect("第 2 行");
        assert!(SheetReader::is_blank(blank));
        let data = reader.row(2).expect("第 3 行");
        assert!(!reader.is_example_row(data));
        assert!(!SheetReader::is_blank(data));
    }

    #[test]
    fn numeric_and_date_cells_are_textualised() {
        assert_eq!(cell_text(&Data::Float(13.0)), "13");
        assert_eq!(cell_text(&Data::Float(13.5)), "13.5");
        assert_eq!(cell_text(&Data::Int(7)), "7");
        assert_eq!(cell_text(&Data::Bool(true)), "true");
        assert_eq!(cell_text(&Data::Empty), "");
        // 含时间的日期单元格保留日 + 时分秒
        let serial = Data::DateTime(calamine::ExcelDateTime::new(
            46323.5,
            calamine::ExcelDateTimeType::DateTime,
            false,
        ));
        assert_eq!(cell_text(&serial), "2026-10-28 12:00:00");
    }

    #[test]
    fn normalize_header_strips_star_and_spaces() {
        assert_eq!(normalize_header(" 物料编码* "), "物料编码");
        assert_eq!(normalize_header("物料编码＊"), "物料编码");
        assert_eq!(normalize_header("物料编码"), "物料编码");
    }

    #[test]
    fn export_rows_are_translated_to_labels() {
        static EXPORT_COLUMNS: &[ExportColumn] = &[ExportColumn {
            key: "status",
            header: "状态",
            kind: ColumnKind::Enum(&[EnumItem {
                label: "启用",
                code: "1",
            }]),
            width: None,
            table: "sys_dict_item",
            sensitive: false,
        }];
        let mut row = HashMap::new();
        row.insert("status", "1".to_string());
        let formatted = format_export_rows(EXPORT_COLUMNS, &[row], &HashMap::new(), &MaskSet::empty())
            .expect("非敏感列不需要脱敏规则");
        assert_eq!(formatted[0].get("状态").map(String::as_str), Some("启用"));
        assert_eq!(export_headers(EXPORT_COLUMNS), vec!["状态"]);
    }

    /// 导出脱敏的两条性质：敏感列被脱敏；没有规则时 **fail-closed**（不返回明文）。
    #[test]
    fn export_rows_mask_sensitive_columns_and_fail_closed() {
        static COLUMNS: &[ExportColumn] = &[
            ExportColumn {
                key: "username",
                header: "用户名",
                kind: ColumnKind::Text { max_len: None },
                width: None,
                table: "sys_user",
                sensitive: false,
            },
            ExportColumn {
                key: "mobile",
                header: "手机号",
                kind: ColumnKind::Text { max_len: None },
                width: None,
                table: "sys_user",
                sensitive: true,
            },
        ];
        let mut row = HashMap::new();
        row.insert("username", "alice".to_string());
        row.insert("mobile", "13812345678".to_string());

        // ① 有规则 → 单元格被打码，非敏感列不动
        let mask = crate::business::masking::registry::single_rule_set(
            "sys_user",
            "mobile",
            crate::business::masking::masker::MaskType::Mobile,
            None,
        );
        let masked = format_export_rows(COLUMNS, &[row.clone()], &HashMap::new(), &mask).expect("有规则应成功");
        assert_eq!(masked[0].get("用户名").map(String::as_str), Some("alice"));
        assert_eq!(masked[0].get("手机号").map(String::as_str), Some("138****5678"));

        // ② 没有规则 → 明确报错（61008），绝不返回明文
        let error = format_export_rows(COLUMNS, &[row.clone()], &HashMap::new(), &MaskSet::empty())
            .expect_err("敏感列没有规则必须失败");
        assert_eq!(error.error_code(), error_code::FIELD_MASK_RULE_MISSING);
        assert_eq!(error.http_status(), 422);
        assert!(error.message().contains("sys_user.mobile"), "{}", error.message());

        // ③ 豁免（超管 / 明版权限）→ 明文，且**不**触发 fail-closed（架构 §3.6）
        let exempt = crate::business::masking::registry::resolve_super();
        let plain =
            format_export_rows(COLUMNS, &[row], &HashMap::new(), &exempt).expect("豁免用户导出不应 fail-closed");
        assert_eq!(plain[0].get("手机号").map(String::as_str), Some("13812345678"));
    }
}
