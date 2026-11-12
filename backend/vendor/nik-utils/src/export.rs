//! 导出导入工具：CSV / JSON / XLSX
//!
//! 三件事先说清楚，避免历史上的「名实不符」重演：
//!
//! 1. `to_csv*` 产出 **CSV**（`to_csv_bytes` 额外加 UTF-8 BOM，便于 Excel 双击打开）。
//!    它不是 XLSX，下载响应请把 `Content-Type` 设为 `text/csv; charset=utf-8`、
//!    扩展名用 `.csv`。
//! 2. 真正的 `.xlsx`（OOXML 工作簿）由 **`xlsx` feature** 提供：
//!    [`Export::to_xlsx_bytes`] / [`Export::to_xlsx_typed`] / [`Export::to_xlsx_workbook`]
//!    写入，[`Import::from_xlsx`] 等解析。**不开 feature 时这些 API 不存在**
//!    ——而不是「存在但返回 CSV」。旧的 `to_xlsx`（实为 BOM+CSV）已**删除**。
//! 3. 因此本模块**没有** `ExportFormat` 枚举：它从未被任何方法消费，只是个
//!    空壳类型，属于诱导误用的死 API。

use nik_core::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;

/// 导出工具
pub struct Export;

impl Export {
    /// 将 KV 数据集导出为 CSV 字符串
    ///
    /// `columns` 指定列顺序，`rows` 为 key→value 映射的集合。
    ///
    /// ```ignore
    /// let csv = Export::to_csv(&["id", "name"], &records)?;
    /// ```
    pub fn to_csv(columns: &[&str], rows: &[HashMap<String, String>]) -> Result<String> {
        let mut w = csv::Writer::from_writer(Vec::new());
        w.write_record(columns)
            .map_err(|e| Error::Msg(format!("CSV写入失败: {}", e)))?;
        for row in rows {
            let vals: Vec<&str> = columns
                .iter()
                .map(|c| row.get(*c).map(|s| s.as_str()).unwrap_or(""))
                .collect();
            w.write_record(&vals)
                .map_err(|e| Error::Msg(format!("CSV写入失败: {}", e)))?;
        }
        let data = w.into_inner().map_err(|e| Error::Msg(format!("CSV输出失败: {}", e)))?;
        String::from_utf8(data).map_err(|e| Error::Msg(format!("UTF8转换失败: {}", e)))
    }

    /// 导出为 **带 UTF-8 BOM 的 CSV 字节流**（Excel 双击可直接打开）。
    ///
    /// 语义明确：产物是 CSV，不是 XLSX。需要真正的工作簿请用
    /// [`Export::to_xlsx_bytes`]（需 `xlsx` feature）。
    ///
    /// ```ignore
    /// let bytes = Export::to_csv_bytes(&["id", "name"], &records)?;
    /// std::fs::write("out.csv", bytes)?;
    /// ```
    pub fn to_csv_bytes(columns: &[&str], rows: &[HashMap<String, String>]) -> Result<Vec<u8>> {
        let csv_content = Self::to_csv(columns, rows)?;
        let mut data = vec![0xEFu8, 0xBB, 0xBF];
        data.extend(csv_content.as_bytes());
        Ok(data)
    }

    /// 将结构化数据导出为 JSON
    pub fn to_json<T: Serialize>(items: &[T]) -> Result<String> {
        serde_json::to_string_pretty(items).map_err(|e| Error::Msg(format!("JSON序列化失败: {}", e)))
    }
}

/// 导入工具
pub struct Import;

impl Import {
    /// 从 CSV 字符串解析为 `Vec<HashMap<String, String>>`
    pub fn from_csv(csv_str: &str) -> Result<Vec<HashMap<String, String>>> {
        let mut reader = csv::Reader::from_reader(csv_str.as_bytes());
        let headers = reader
            .headers()
            .map_err(|e| Error::Msg(format!("CSV头解析失败: {}", e)))?
            .clone();
        let mut rows = Vec::new();
        for result in reader.records() {
            let record = result.map_err(|e| Error::Msg(format!("CSV行解析失败: {}", e)))?;
            let mut map = HashMap::new();
            for (i, val) in record.iter().enumerate() {
                if let Some(h) = headers.get(i) {
                    map.insert(h.to_string(), val.to_string());
                }
            }
            rows.push(map);
        }
        Ok(rows)
    }

    /// 从 JSON 反序列化
    pub fn from_json<T: DeserializeOwned>(json_str: &str) -> Result<Vec<T>> {
        serde_json::from_str(json_str).map_err(|e| Error::Msg(format!("JSON解析失败: {}", e)))
    }
}

// ══════════════════════════════════════════════════════════════════
// 真 XLSX（OOXML）：feature `xlsx`
//
// 写：`rust_xlsxwriter`（纯 Rust，无系统依赖）
// 读：`calamine`（xlsx / xlsb / xls / ods）
// ══════════════════════════════════════════════════════════════════

#[cfg(feature = "xlsx")]
mod xlsx_impl {
    use super::{Error, Export, HashMap, Import, Result, Serialize};
    use calamine::{Data, Reader, Xlsx, open_workbook_from_rs};
    use chrono::{Datelike, NaiveDate};
    use rust_xlsxwriter::{Color, DataValidation, DataValidationRule, Format, Formula, Note, Workbook, Worksheet};
    use serde::de::DeserializeOwned;
    use std::io::Cursor;

    // ── 单元格类型推断 ────────────────────────────────────────
    //
    // 只把**能原样往返**的文本转成数字：`"00123"` 格式化回字符串是 `"123"`，
    // 与原文不等 → 保持文本。这样订单号 / 手机号 / 身份证之类的零填充或
    // 超长数字不会被 Excel 悄悄改写成科学计数法或丢前导零。

    /// 文本 → 单元格类型。返回 `Ok(Cell::*)` 表示可安全数值化。
    fn classify(v: &str) -> Cell<'_> {
        if let Ok(i) = v.parse::<i64>() {
            // 排除前导零与 `+1` 这类非规范写法
            if i.to_string() == v {
                return Cell::Int(i);
            }
        }
        if let Ok(f) = v.parse::<f64>() {
            if f.is_finite() && f.to_string() == v {
                return Cell::Float(f);
            }
        }
        match v {
            "true" => Cell::Bool(true),
            "false" => Cell::Bool(false),
            _ => Cell::Text(v),
        }
    }

    enum Cell<'a> {
        Int(i64),
        Float(f64),
        Bool(bool),
        Text(&'a str),
    }

    fn write_cell(ws: &mut Worksheet, row: u32, col: u16, v: &str) -> Result<()> {
        if v.is_empty() {
            // 显式留空，让列宽/边框仍然作用于该单元格
            ws.write_blank(row, col, &Format::new()).map_err(map_err)?;
            return Ok(());
        }
        match classify(v) {
            Cell::Int(i) => ws.write_number(row, col, i as f64).map_err(map_err)?,
            Cell::Float(f) => ws.write_number(row, col, f).map_err(map_err)?,
            Cell::Bool(b) => ws.write_boolean(row, col, b).map_err(map_err)?,
            Cell::Text(s) => ws.write_string(row, col, s).map_err(map_err)?,
        };
        Ok(())
    }

    /// 带样式的写单元格（文本列 / 示例行用）。
    ///
    /// # 与 [`write_cell`] 的差别
    ///
    /// 传进来的 `format` 若带 `@`（文本格式），**一律按字符串写**，不再做
    /// [`classify`] 的数值化推断 —— 文本列里 `13800138000` 必须原样是字符串，
    /// 否则 Excel 一旦把列当成数值，18 位身份证号就会丢精度。
    ///
    /// # 参数
    ///
    /// - `ws`：目标工作表；
    /// - `row` / `col`：0 基坐标；
    /// - `v`：单元格文本；
    /// - `format`：样式（文本格式 / 示例行斜体灰字）；
    /// - `as_text`：是否强制按字符串写（文本列传 `true`）。
    fn write_cell_with_format(
        ws: &mut Worksheet,
        row: u32,
        col: u16,
        v: &str,
        format: &Format,
        as_text: bool,
    ) -> Result<()> {
        if v.is_empty() {
            ws.write_blank(row, col, format).map_err(map_err)?;
            return Ok(());
        }
        if as_text {
            ws.write_string_with_format(row, col, v, format).map_err(map_err)?;
            return Ok(());
        }
        match classify(v) {
            Cell::Int(i) => ws.write_number_with_format(row, col, i as f64, format),
            Cell::Float(f) => ws.write_number_with_format(row, col, f, format),
            Cell::Bool(b) => ws.write_boolean_with_format(row, col, b, format),
            Cell::Text(s) => ws.write_string_with_format(row, col, s, format),
        }
        .map_err(map_err)?;
        Ok(())
    }

    // ── 数据验证（Excel「数据 → 数据验证」）──────────────────
    //
    // 下拉 / 范围验证是**防错**，不是判据：真正说了算的仍然是导入期的
    // 逐格校验（`ColumnKind::parse`）。用户可以把 Excel 的验证删掉、也可以
    // 直接粘文本进来，所以服务端校验一条都不能少。

    /// Excel 单个下拉列表的字符上限（逗号连接后，含逗号）。
    ///
    /// 超过时 Excel 会判定整张表损坏并要求修复，因此写入器直接返回错误，
    /// 由调用方决定「截断 / 换区间引用 / 放弃该列的下拉」。
    pub const MAX_LIST_CHARS: usize = 255;

    /// 数据验证规则，与 Excel「数据验证」对话框的类型一一对应。
    #[derive(Debug, Clone)]
    pub enum XlsxValidationRule<'a> {
        /// 序列（下拉列表），取值来自固定字符串。
        List(Vec<&'a str>),
        /// 整数范围；`None` 表示该侧不设界，两侧都 `None` 等于不验证。
        IntRange {
            /// 下界（含）。
            min: Option<i64>,
            /// 上界（含）。
            max: Option<i64>,
        },
        /// 小数范围；语义同 [`XlsxValidationRule::IntRange`]。
        DecimalRange {
            /// 下界（含）。
            min: Option<f64>,
            /// 上界（含）。
            max: Option<f64>,
        },
        /// 日期范围（`YYYY-MM-DD`）；`None` 表示该侧不设界。
        ///
        /// 写成 `DATE(y,m,d)` 公式而不是 Excel 序列号，避免 1900/1904
        /// 两种日期系统下的偏移坑。
        DateRange {
            /// 起始日期（含）。
            min: Option<&'a str>,
            /// 结束日期（含）。
            max: Option<&'a str>,
        },
    }

    /// 一条数据验证：作用于**某一列**的 `[first_row, last_row]`（0 基行号）。
    ///
    /// 行号与 [`Worksheet::write`] 的 `row` 同一坐标系：`0` 是 Excel 第 1 行，
    /// 所以「从第 3 行起」= `first_row: 2`。
    #[derive(Debug, Clone)]
    pub struct XlsxValidation<'a> {
        /// 列号（0 基）。
        pub column: u16,
        /// 起始行（0 基，含）。
        pub first_row: u32,
        /// 结束行（0 基，含），必须 ≥ `first_row`。
        pub last_row: u32,
        /// 规则。
        pub rule: XlsxValidationRule<'a>,
        /// 选中单元格时的提示标题（可选，≤255 字符）。
        pub title: Option<&'a str>,
        /// 选中单元格时的提示正文（可选，≤255 字符）。
        pub message: Option<&'a str>,
    }

    impl<'a> XlsxValidation<'a> {
        /// 下拉列表验证。
        pub fn list(column: u16, first_row: u32, last_row: u32, values: impl IntoIterator<Item = &'a str>) -> Self {
            Self {
                column,
                first_row,
                last_row,
                rule: XlsxValidationRule::List(values.into_iter().collect()),
                title: None,
                message: None,
            }
        }

        /// 整数范围验证。
        pub fn int_range(column: u16, first_row: u32, last_row: u32, min: Option<i64>, max: Option<i64>) -> Self {
            Self {
                column,
                first_row,
                last_row,
                rule: XlsxValidationRule::IntRange { min, max },
                title: None,
                message: None,
            }
        }

        /// 小数范围验证。
        pub fn decimal_range(column: u16, first_row: u32, last_row: u32, min: Option<f64>, max: Option<f64>) -> Self {
            Self {
                column,
                first_row,
                last_row,
                rule: XlsxValidationRule::DecimalRange { min, max },
                title: None,
                message: None,
            }
        }

        /// 日期范围验证（`YYYY-MM-DD`）。
        pub fn date_range(
            column: u16,
            first_row: u32,
            last_row: u32,
            min: Option<&'a str>,
            max: Option<&'a str>,
        ) -> Self {
            Self {
                column,
                first_row,
                last_row,
                rule: XlsxValidationRule::DateRange { min, max },
                title: None,
                message: None,
            }
        }

        /// 补上「选中该格时」的提示（Excel 只在选中时显示，不影响判定）。
        pub fn with_message(mut self, title: &'a str, message: &'a str) -> Self {
            self.title = Some(title);
            self.message = Some(message);
            self
        }
    }

    /// 把一个日期字符串转成 `DATE(y,m,d)` 公式。
    fn date_formula(value: &str) -> Result<Formula> {
        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| Error::Msg(format!("XLSX数据验证失败: 日期「{value}」不是 YYYY-MM-DD 格式")))?;
        Ok(Formula::new(format!(
            "DATE({},{},{})",
            date.year(),
            date.month(),
            date.day()
        )))
    }

    fn int_bound(value: i64) -> Result<i32> {
        i32::try_from(value)
            .map_err(|_| Error::Msg(format!("XLSX数据验证失败: 整数边界 {value} 超出 Excel 的 ±2^31 范围")))
    }

    /// 把一条 [`XlsxValidation`] 编译成 `rust_xlsxwriter` 的验证对象。
    ///
    /// 返回 `Ok(None)` 表示「这条验证等于不验证」（空列表 / 两侧都无界），
    /// 此时**不写 XML 节点**——避免生成一个什么都不限制的空验证。
    fn build_data_validation(validation: &XlsxValidation<'_>) -> Result<Option<DataValidation>> {
        let mut dv = DataValidation::new();
        match &validation.rule {
            XlsxValidationRule::List(values) => {
                if values.is_empty() {
                    return Ok(None);
                }
                let joined = values.join(",");
                let chars = joined.chars().count();
                if chars > MAX_LIST_CHARS {
                    return Err(Error::Msg(format!(
                        "XLSX数据验证失败: 下拉列表「{}」共 {chars} 字符，超过 Excel 上限 {MAX_LIST_CHARS}",
                        values.first().copied().unwrap_or("")
                    )));
                }
                dv = dv.allow_list_strings(values.as_slice()).map_err(dv_err)?;
            }
            XlsxValidationRule::IntRange { min, max } => {
                let rule = match (min, max) {
                    (Some(low), Some(high)) => DataValidationRule::Between(int_bound(*low)?, int_bound(*high)?),
                    (Some(low), None) => DataValidationRule::GreaterThanOrEqualTo(int_bound(*low)?),
                    (None, Some(high)) => DataValidationRule::LessThanOrEqualTo(int_bound(*high)?),
                    (None, None) => return Ok(None),
                };
                dv = dv.allow_whole_number(rule);
            }
            XlsxValidationRule::DecimalRange { min, max } => {
                let rule = match (min, max) {
                    (Some(low), Some(high)) => DataValidationRule::Between(*low, *high),
                    (Some(low), None) => DataValidationRule::GreaterThanOrEqualTo(*low),
                    (None, Some(high)) => DataValidationRule::LessThanOrEqualTo(*high),
                    (None, None) => return Ok(None),
                };
                dv = dv.allow_decimal_number(rule);
            }
            XlsxValidationRule::DateRange { min, max } => {
                let low = min.map(date_formula).transpose()?;
                let high = max.map(date_formula).transpose()?;
                let rule = match (low, high) {
                    (Some(low), Some(high)) => DataValidationRule::Between(low, high),
                    (Some(low), None) => DataValidationRule::GreaterThanOrEqualTo(low),
                    (None, Some(high)) => DataValidationRule::LessThanOrEqualTo(high),
                    (None, None) => return Ok(None),
                };
                dv = dv.allow_date_formula(rule);
            }
        }
        if let Some(title) = validation.title {
            dv = dv.set_input_title(title).map_err(dv_err)?;
        }
        if let Some(message) = validation.message {
            dv = dv.set_input_message(message).map_err(dv_err)?;
        }
        Ok(Some(dv))
    }

    fn dv_err<E: std::fmt::Display>(e: E) -> Error {
        Error::Msg(format!("XLSX数据验证失败: {e}"))
    }

    // ── 写 ───────────────────────────────────────────────────

    /// 一个工作表的内容：`name` 为 sheet 名，`columns` 定列顺序，
    /// `rows` 为 key→value 映射。
    ///
    /// ```ignore
    /// let sheets = [XlsxSheet::new("用户", &["id", "name"], &users)];
    /// let bytes = Export::to_xlsx_workbook(&sheets)?;
    /// ```
    pub struct XlsxSheet<'a> {
        /// 工作表名（≤31 字符，且不含 `\ / ? * [ ] :`）
        pub name: &'a str,
        /// 列顺序；不在其中的字段不会导出
        pub columns: &'a [&'a str],
        /// 数据行
        pub rows: &'a [HashMap<String, String>],
        /// 该 sheet 的数据验证（下拉 / 数值范围 / 日期范围）；空切片表示不加
        pub validations: &'a [XlsxValidation<'a>],
        /// 需要强制**文本格式**的列下标（`@` 数字格式）。
        ///
        /// 空切片 = 保持原行为（能数值化的文本写成数字）。
        ///
        /// 为什么需要它：Excel 对「常规」格式的单元格会把 `6222021234567890123`
        /// 这类长数字转成数值（15 位有效位，后几位**直接丢失**），身份证 / 手机号 /
        /// 银行账号填进模板就会悄悄变成科学计数法。列被标成文本后，用户新填的
        /// 单元格与已写入的示例值都保持字符串，导入解析拿到的才是原文。
        pub text_columns: &'a [u16],
        /// 表头批注：`(列下标, 批注正文)`。空切片表示不加批注。
        ///
        /// 批注挂在第 1 行（表头单元格）上，鼠标悬停即可看到该列的填写说明；
        /// 与「字段说明」sheet 的说明文案同一来源（调用方决定）。
        pub header_comments: &'a [(u16, &'a str)],
        /// 第一个数据行是否按「示例行」渲染（灰字 + 斜体）。
        ///
        /// 模板的第 2 行是给用户看的示例（导入时自动跳过），样式上必须与用户
        /// 自己填的行区分开，否则用户会以为那是真实数据。
        pub style_first_row_as_sample: bool,
    }

    impl<'a> XlsxSheet<'a> {
        pub fn new(name: &'a str, columns: &'a [&'a str], rows: &'a [HashMap<String, String>]) -> Self {
            Self {
                name,
                columns,
                rows,
                validations: &[],
                text_columns: &[],
                header_comments: &[],
                style_first_row_as_sample: false,
            }
        }

        /// 追加数据验证（见 [`XlsxValidation`]）。同一条验证只作用于单列，
        /// 需要多列各加一条。
        pub fn with_validations(mut self, validations: &'a [XlsxValidation<'a>]) -> Self {
            self.validations = validations;
            self
        }

        /// 指定按文本格式写入的列（见 [`Self::text_columns`]）。
        pub fn with_text_columns(mut self, text_columns: &'a [u16]) -> Self {
            self.text_columns = text_columns;
            self
        }

        /// 指定表头批注（见 [`Self::header_comments`]）。
        pub fn with_header_comments(mut self, header_comments: &'a [(u16, &'a str)]) -> Self {
            self.header_comments = header_comments;
            self
        }

        /// 第一个数据行按示例行渲染（灰字 + 斜体，见 [`Self::style_first_row_as_sample`]）。
        pub fn with_sample_first_row(mut self) -> Self {
            self.style_first_row_as_sample = true;
            self
        }
    }

    impl Export {
        /// 导出为**真正的 `.xlsx`（OOXML）工作簿**字节流。
        ///
        /// 首行表头加粗 + 冻结窗格，列宽按内容自适应。
        /// 产物可被 Excel / LibreOffice / `calamine` / `openpyxl` 正常解析。
        ///
        /// ```ignore
        /// let bytes = Export::to_xlsx_bytes(&["id", "name"], &records)?;
        /// // Content-Type: application/vnd.openxmlformats-officedocument
        /// //              .spreadsheetml.sheet
        /// ```
        pub fn to_xlsx_bytes(columns: &[&str], rows: &[HashMap<String, String>]) -> Result<Vec<u8>> {
            Self::to_xlsx_workbook(&[XlsxSheet::new("Sheet1", columns, rows)])
        }

        /// 从**序列化类型**导出（字段取值走 `serde_json`，与 [`Export::to_json`] 同源）。
        ///
        /// `columns` 为字段名顺序；对象 / 数组等非标量按紧凑 JSON 文本写入。
        pub fn to_xlsx_typed<T: Serialize>(columns: &[&str], items: &[T]) -> Result<Vec<u8>> {
            let rows = typed_to_rows(columns, items)?;
            Self::to_xlsx_bytes(columns, &rows)
        }

        /// 导出**多 sheet** 工作簿。
        pub fn to_xlsx_workbook(sheets: &[XlsxSheet<'_>]) -> Result<Vec<u8>> {
            if sheets.is_empty() {
                return Err(Error::Msg("XLSX导出失败: 至少需要一个工作表".into()));
            }
            let mut wb = Workbook::new();
            let header = Format::new().set_bold().set_background_color(Color::RGB(0xD9E1F2));
            // 文本列：`@` 是 Excel 的「文本」数字格式。列级设置让**用户新填**的单元格
            // 也走文本（否则只有已写入的示例值是字符串，用户一填长数字又被转成数值）。
            let text_format = Format::new().set_num_format("@");
            // 示例行：灰字 + 斜体（与用户自己填的行视觉上区分开）
            let sample_plain = Format::new().set_italic().set_font_color(Color::RGB(0x80_80_80));
            let sample_text = Format::new()
                .set_italic()
                .set_font_color(Color::RGB(0x80_80_80))
                .set_num_format("@");
            for sheet in sheets {
                let ws = wb.add_worksheet();
                ws.set_name(sheet.name).map_err(map_err)?;
                for (c, name) in sheet.columns.iter().enumerate() {
                    ws.write_string_with_format(0, c as u16, *name, &header)
                        .map_err(map_err)?;
                }
                // 表头批注（鼠标悬停可见的填写说明）
                for (col, text) in sheet.header_comments {
                    if text.trim().is_empty() {
                        continue;
                    }
                    ws.insert_note(0, *col, &Note::new(*text)).map_err(map_err)?;
                }
                // 列级文本格式（对用户后续填写生效）
                for col in sheet.text_columns {
                    ws.set_column_format(*col, &text_format).map_err(map_err)?;
                }
                for (r, row) in sheet.rows.iter().enumerate() {
                    let r = (r + 1) as u32;
                    let is_sample = sheet.style_first_row_as_sample && r == 1;
                    for (c, col) in sheet.columns.iter().enumerate() {
                        let v = row.get(*col).map(|s| s.as_str()).unwrap_or("");
                        let is_text = sheet.text_columns.contains(&(c as u16));
                        let style = match (is_sample, is_text) {
                            (true, true) => Some(&sample_text),
                            (true, false) => Some(&sample_plain),
                            (false, true) => Some(&text_format),
                            (false, false) => None,
                        };
                        match style {
                            Some(format) => write_cell_with_format(ws, r, c as u16, v, format, is_text)?,
                            None => write_cell(ws, r, c as u16, v)?,
                        }
                    }
                }
                // 数据验证（下拉 / 范围）：只作用在调用方指定的行区间上
                for validation in sheet.validations {
                    if let Some(dv) = build_data_validation(validation)? {
                        ws.add_data_validation(
                            validation.first_row,
                            validation.column,
                            validation.last_row,
                            validation.column,
                            &dv,
                        )
                        .map_err(map_err)?;
                    }
                }
                // 冻结表头 + 按内容自适应列宽
                ws.set_freeze_panes(1, 0).map_err(map_err)?;
                ws.autofit();
            }
            wb.save_to_buffer().map_err(map_err)
        }
    }

    fn typed_to_rows<T: Serialize>(columns: &[&str], items: &[T]) -> Result<Vec<HashMap<String, String>>> {
        let mut rows = Vec::with_capacity(items.len());
        for item in items {
            let value =
                serde_json::to_value(item).map_err(|e| Error::Msg(format!("XLSX导出失败: 序列化失败 {}", e)))?;
            let mut row = HashMap::new();
            for col in columns {
                let text = match value.get(*col) {
                    None | Some(serde_json::Value::Null) => String::new(),
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                };
                row.insert((*col).to_string(), text);
            }
            rows.push(row);
        }
        Ok(rows)
    }

    // ── 读 ───────────────────────────────────────────────────

    fn open(data: &[u8]) -> Result<Xlsx<Cursor<&[u8]>>> {
        open_workbook_from_rs(Cursor::new(data)).map_err(|e| Error::Msg(format!("XLSX解析失败: {}", e)))
    }

    fn cell_to_string(d: &Data) -> String {
        match d {
            Data::Int(i) => i.to_string(),
            Data::Float(f) => f.to_string(),
            Data::String(s) => s.clone(),
            Data::Bool(b) => b.to_string(),
            Data::DateTime(dt) => dt.to_string(),
            Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
            Data::Error(e) => e.to_string(),
            Data::Empty => String::new(),
        }
    }

    fn range_to_rows(range: &calamine::Range<Data>) -> Vec<HashMap<String, String>> {
        let mut out = Vec::new();
        let mut headers: Vec<String> = Vec::new();
        for (r, row) in range.rows().enumerate() {
            if r == 0 {
                headers = row.iter().map(cell_to_string).collect();
                continue;
            }
            let mut map = HashMap::new();
            for (i, cell) in row.iter().enumerate() {
                if let Some(h) = headers.get(i) {
                    if !h.is_empty() {
                        map.insert(h.clone(), cell_to_string(cell));
                    }
                }
            }
            out.push(map);
        }
        out
    }

    impl Import {
        /// 列出工作簿内的所有 sheet 名（按文件内顺序）。
        pub fn xlsx_sheet_names(data: &[u8]) -> Result<Vec<String>> {
            let wb = open(data)?;
            Ok(wb.sheet_names().to_vec())
        }

        /// 解析**第一个** sheet，首行作表头。
        pub fn from_xlsx(data: &[u8]) -> Result<Vec<HashMap<String, String>>> {
            let mut wb = open(data)?;
            let first = wb
                .sheet_names()
                .first()
                .cloned()
                .ok_or_else(|| Error::Msg("XLSX解析失败: 工作簿内没有工作表".into()))?;
            let range = wb
                .worksheet_range(&first)
                .map_err(|e| Error::Msg(format!("XLSX解析失败: {}", e)))?;
            Ok(range_to_rows(&range))
        }

        /// 解析**指定** sheet，首行作表头。
        pub fn from_xlsx_sheet(data: &[u8], sheet: &str) -> Result<Vec<HashMap<String, String>>> {
            let mut wb = open(data)?;
            let range = wb
                .worksheet_range(sheet)
                .map_err(|e| Error::Msg(format!("XLSX解析失败: 工作表`{}`: {}", sheet, e)))?;
            Ok(range_to_rows(&range))
        }

        /// 解析第一个 sheet 并反序列化为具体类型（表头 = 字段名）。
        ///
        /// 单元格值**先按推断类型**解析（数字 / 布尔落成对应标量，空单元格为 `""`）；
        /// 若目标字段是 `String` 而单元格恰好是纯数字，则退回"全部按字符串"再试一次。
        /// 与写侧一致，`"00123"` 这类**不能原样往返**的文本不会被当成数字。
        pub fn from_xlsx_typed<T: DeserializeOwned>(data: &[u8]) -> Result<Vec<T>> {
            let rows = Self::from_xlsx(data)?;
            rows_to_typed(rows)
        }
    }

    fn rows_to_typed<T: DeserializeOwned>(rows: Vec<HashMap<String, String>>) -> Result<Vec<T>> {
        // 先用**推断类型**解析（数字/布尔落成对应的 JSON 标量）；失败则退回
        // "全部按字符串"再试一次——这样 `name: String` 里恰好写着 `42` 也能过。
        // 两次都失败时报第一次的错误（更贴近单元格的真实类型）。
        match deserialize_rows::<T>(&rows, true) {
            Ok(v) => Ok(v),
            Err(first) => deserialize_rows::<T>(&rows, false).map_err(|_| first),
        }
    }

    fn deserialize_rows<T: DeserializeOwned>(rows: &[HashMap<String, String>], infer: bool) -> Result<Vec<T>> {
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let item = serde_json::from_value(json_value(row, infer))
                .map_err(|e| Error::Msg(format!("XLSX解析失败: 反序列化失败 {}", e)))?;
            out.push(item);
        }
        Ok(out)
    }

    fn json_value(row: &HashMap<String, String>, infer: bool) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (k, v) in row {
            map.insert(k.clone(), cell_json(v, infer));
        }
        serde_json::Value::Object(map)
    }

    fn cell_json(v: &str, infer: bool) -> serde_json::Value {
        if !infer {
            return serde_json::Value::String(v.to_string());
        }
        // 与写侧同一条"能原样往返才数值化"的规则，保证 `"00123"` 仍是文本
        match classify(v) {
            Cell::Int(i) => serde_json::Value::Number(i.into()),
            Cell::Float(f) => serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or_else(|| serde_json::Value::String(v.to_string())),
            Cell::Bool(b) => serde_json::Value::Bool(b),
            Cell::Text(s) => serde_json::Value::String(s.to_string()),
        }
    }

    fn map_err<E: std::fmt::Display>(e: E) -> Error {
        Error::Msg(format!("XLSX写入失败: {}", e))
    }
}

#[cfg(feature = "xlsx")]
pub use xlsx_impl::{MAX_LIST_CHARS, XlsxSheet, XlsxValidation, XlsxValidationRule};

#[cfg(test)]
#[cfg(feature = "xlsx")]
mod tests {
    use super::*;

    fn row(id: &str, name: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("id".into(), id.into());
        m.insert("name".into(), name.into());
        m
    }

    /// .xlsx 本质是一个 zip 包，必须以 `PK\x03\x04` 开始。
    #[test]
    fn output_is_a_real_zip_container() {
        let bytes = Export::to_xlsx_bytes(&["id", "name"], &[row("1", "Alice")]).unwrap();
        assert_eq!(&bytes[..4], &[0x50, 0x4B, 0x03, 0x04]);
    }

    #[test]
    fn roundtrip_preserves_header_and_values() {
        let bytes = Export::to_xlsx_bytes(&["id", "name"], &[row("1", "Alice"), row("2", "Bob")]).unwrap();
        let rows = Import::from_xlsx(&bytes).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("id").unwrap(), "1");
        assert_eq!(rows[0].get("name").unwrap(), "Alice");
        assert_eq!(rows[1].get("name").unwrap(), "Bob");
    }

    /// 零填充编号不能被 Excel 悄悄变成数字（会丢前导零）。
    #[test]
    fn leading_zero_text_is_not_coerced_to_number() {
        let bytes = Export::to_xlsx_bytes(&["id"], &[row("00123", "x")]).unwrap();
        let rows = Import::from_xlsx(&bytes).unwrap();
        assert_eq!(rows[0].get("id").unwrap(), "00123");
    }

    /// 规范写法的数字**应当**落成数值单元格（Excel 右对齐、可直接求和）。
    #[test]
    fn canonical_numbers_become_numeric_cells() {
        use calamine::{Data, Reader};
        let bytes = Export::to_xlsx_bytes(&["id"], &[row("42", "x")]).unwrap();
        let mut wb: calamine::Xlsx<_> =
            calamine::open_workbook_from_rs(std::io::Cursor::new(bytes.as_slice())).unwrap();
        let range = wb.worksheet_range("Sheet1").unwrap();
        assert!(matches!(range.get((1, 0)), Some(Data::Float(_)) | Some(Data::Int(_))));
    }

    #[test]
    fn multi_sheet_workbook() {
        let a = [row("1", "Alice")];
        let b = [row("9", "Zoe")];
        let sheets = [
            XlsxSheet::new("用户", &["id", "name"], &a),
            XlsxSheet::new("订单", &["id", "name"], &b),
        ];
        let bytes = Export::to_xlsx_workbook(&sheets).unwrap();

        let names = Import::xlsx_sheet_names(&bytes).unwrap();
        assert_eq!(names, vec!["用户".to_string(), "订单".to_string()]);
        assert_eq!(Import::from_xlsx(&bytes).unwrap()[0].get("name").unwrap(), "Alice");
        assert_eq!(
            Import::from_xlsx_sheet(&bytes, "订单").unwrap()[0].get("name").unwrap(),
            "Zoe"
        );
    }

    #[test]
    fn typed_export_and_import() {
        #[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Item {
            id: i64,
            name: String,
        }
        let items = [
            Item {
                id: 1,
                name: "A".into(),
            },
            Item {
                id: 2,
                name: "B".into(),
            },
        ];
        let bytes = Export::to_xlsx_typed(&["id", "name"], &items).unwrap();
        let back: Vec<Item> = Import::from_xlsx_typed(&bytes).unwrap();
        assert_eq!(back, items);
    }

    #[test]
    fn empty_workbook_is_rejected() {
        assert!(Export::to_xlsx_workbook(&[]).is_err());
    }

    // ── 数据验证 ────────────────────────────────────────────

    /// 生成一个带验证的 sheet，返回字节流。
    fn sheet_with(validations: &[XlsxValidation<'_>]) -> Result<Vec<u8>> {
        let rows = [row("1", "Alice")];
        let sheet = XlsxSheet::new("Sheet1", &["id", "name"], &rows).with_validations(validations);
        Export::to_xlsx_workbook(&[sheet])
    }

    /// Excel 对下拉列表有 255 字符硬上限，超限必须**报错**而不是生成一张
    /// 打开就提示「修复」的残表。
    #[test]
    fn overlong_list_is_rejected_instead_of_writing_a_broken_sheet() {
        let long: Vec<&str> = (0..40).map(|_| "0123456789").collect();
        let validations = [XlsxValidation::list(1, 2, 9, long.iter().copied())];
        let err = sheet_with(&validations).expect_err("超长下拉应报错");
        let msg = format!("{err}");
        assert!(msg.contains("255"), "错误信息应点明 255 上限：{msg}");
    }

    /// 两侧都无界的范围、或空列表，等于「什么都不限制」——不写 XML 节点，
    /// 但整张表仍然能正常生成。
    #[test]
    fn ineffective_validations_are_skipped_without_breaking_the_workbook() {
        let empty: [&str; 0] = [];
        let validations = [
            XlsxValidation::list(1, 2, 9, empty.iter().copied()),
            XlsxValidation::int_range(0, 2, 9, None, None),
            XlsxValidation::decimal_range(0, 2, 9, None, None),
        ];
        let bytes = sheet_with(&validations).expect("空验证不应影响生成");
        assert_eq!(&bytes[..4], &[0x50, 0x4B, 0x03, 0x04]);
        assert_eq!(Import::from_xlsx(&bytes).unwrap()[0]["name"], "Alice");
    }

    /// 合法区间 / 日期区间必须能真的写进去（不报错），且不破坏单元格内容。
    #[test]
    fn ranges_and_dates_can_be_written() {
        let validations = [
            XlsxValidation::list(1, 2, 9, ["Pass", "Fail"]).with_message("结果", "Pass / Fail"),
            XlsxValidation::int_range(0, 2, 9, Some(0), Some(9999)),
            XlsxValidation::decimal_range(0, 2, 9, Some(0.0), None),
            XlsxValidation::date_range(0, 2, 9, Some("2020-01-01"), Some("2100-12-31")),
        ];
        let bytes = sheet_with(&validations).expect("合法验证应能写入");
        assert_eq!(&bytes[..4], &[0x50, 0x4B, 0x03, 0x04]);
        assert_eq!(Import::from_xlsx(&bytes).unwrap()[0]["id"], "1");
    }

    /// 日期字符串必须严格 `YYYY-MM-DD`，否则写成 `DATE(y,m,d)` 会注入坏公式。
    #[test]
    fn malformed_date_bound_is_rejected() {
        let validations = [XlsxValidation::date_range(0, 2, 9, Some("2020/01/01"), None)];
        let err = sheet_with(&validations).expect_err("非 YYYY-MM-DD 应报错");
        assert!(format!("{err}").contains("YYYY-MM-DD"));
    }
}
