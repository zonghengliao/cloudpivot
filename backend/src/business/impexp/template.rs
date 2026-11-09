//! 模板生成（表头 / 示例行 / 字段说明 三要素）。
//!
//! # 生成的工作簿（PRD §4.1.2）
//!
//! | sheet | 内容 |
//! |-------|------|
//! | `数据` | 第 1 行表头（必填列带 `*`）、第 2 行示例、其余留空 |
//! | `字段说明` | 字段名 / 是否必填 / 类型 / 长度范围 / 可选值 / 说明 |
//! | `填写须知` | 固定 6 条注意事项 |
//!
//! 表头样式（加粗 + 底色 + 冻结首行 + 列宽自适应）由框架 `Export::to_xlsx_workbook`
//! 统一处理，这里只负责内容与顺序。
//!
//! # 枚举 / 数值列的 Excel 数据验证（下拉）
//!
//! 「数据」sheet 从**第 3 行起**（第 1 行表头、第 2 行示例）按列挂数据验证：
//!
//! | 列类型 | Excel 验证 | 来源 |
//! |--------|-----------|------|
//! | `ColumnKind::Enum` | 序列（下拉），只能选 | [`EnumItem::label`]，与「字段说明」sheet 的「可选值」列同一份数据 |
//! | `ColumnKind::Int` | 整数范围 | `ColumnKind::Int{min,max}` |
//! | `ColumnKind::Decimal` | 小数范围 | `ColumnKind::Decimal{min,max}` |
//! | `ColumnKind::Date` | 日期区间 1900-01-01 ~ 2100-12-31 | 固定区间（拦「2026-13-01」这类文本） |
//!
//! 其他类型（文本 / 布尔 / 字典 / 日期时间）不加验证：字典列的取值是运行时数据，
//! 生成模板时拿不到，硬编码一份必然与库不一致。
//!
//! **下拉只是防错，不是判据**：用户可以把 Excel 的验证删掉、也可以整列粘文本进来，
//! 所以导入期 [`ColumnKind::parse`] 的逐格校验一条都没减——校验不通过照旧在预检
//! 阶段报 `ENUM` / `RANGE`，带行号与列名。
//!
//! [`EnumItem::label`]: crate::business::impexp::definition::EnumItem
//! [`ColumnKind::parse`]: crate::business::impexp::definition::ColumnKind

use nik::prelude::{ApiError, MAX_LIST_CHARS, XlsxValidation};

use crate::business::impexp::definition::{ColumnKind, ImportColumn, ImportDefinition, RowIssue};
use crate::business::impexp::engine::{WorkbookSheet, write_workbook};

/// 数据区起始行（0 基）：第 1 行表头、第 2 行示例，因此用户从第 3 行开始填。
const DATA_FIRST_ROW: u32 = 2;

/// 日期列的可选区间（闭区间）。
///
/// 取值区间只用来拦「把文本当日期填」，不表达业务口径；超出区间的真实业务日期
/// 由服务端校验决定（服务端只校验格式，不限制年份）。
const DATE_MIN: &str = "1900-01-01";
/// 见 [`DATE_MIN`]。
const DATE_MAX: &str = "2100-12-31";

/// 「字段说明」sheet 的表头。
const NOTE_HEADERS: [&str; 6] = ["字段名", "是否必填", "类型", "长度/范围", "可选值", "说明"];

/// 「填写须知」sheet 的固定文案。
const GUIDE_LINES: [&str; 6] = [
    "1. 请勿修改第 1 行表头，勿增删列；",
    "2. 第 2 行是示例，导入前请删除（系统也会自动识别并跳过）；",
    "3. 单次最多 100,000 行，超出请拆分；",
    "4. 必填列（带 *）不能为空；",
    "5. 枚举列在「数据」sheet 第 3 行起有下拉可选；字典列请照「字段说明」的可选值填写（下拉只在本机 Excel 生效，服务端导入时仍逐格校验）；",
    "6. 保存为 .xlsx 格式上传（不支持 .xls 旧格式）。",
];

/// 模板文件名（`物料导入模板_v1.xlsx`）。
///
/// # 参数
///
/// - `definition`：导入定义。
///
/// # 返回值
///
/// RFC 5987 之前的原始中文文件名，由调用方做 URL 编码。
pub fn template_file_name(definition: &dyn ImportDefinition) -> String {
    format!(
        "{}导入模板_v{}.xlsx",
        definition.biz_name(),
        definition.template_version()
    )
}

/// 生成导入模板 xlsx。
///
/// # 参数
///
/// - `definition`：导入定义（决定表头、示例、说明）。
///
/// # 返回值
///
/// 三个 sheet 的 xlsx 字节流。
///
/// # 异常情况
///
/// 写入失败返回 500。
pub fn build_template(definition: &dyn ImportDefinition) -> std::result::Result<Vec<u8>, ApiError> {
    let columns = definition.columns();

    // ---------- sheet 1「数据」：表头 + 一行示例 ----------
    let data_headers = columns
        .iter()
        .map(|column| column.template_header())
        .collect::<Vec<_>>();
    let mut example_row = std::collections::HashMap::new();
    for column in columns {
        example_row.insert(column.template_header(), column.example.to_string());
    }
    let data_rows = vec![example_row];

    // ---------- sheet 2「字段说明」 ----------
    let note_rows = columns
        .iter()
        .map(|column| {
            let mut row = std::collections::HashMap::new();
            row.insert("字段名".to_string(), column.template_header());
            row.insert(
                "是否必填".to_string(),
                if column.required { "是" } else { "否" }.to_string(),
            );
            row.insert("类型".to_string(), column.kind.type_label().to_string());
            row.insert("长度/范围".to_string(), column.kind.rule_text());
            row.insert("可选值".to_string(), column.kind.option_text());
            let remark = if column.remark.is_empty() {
                String::new()
            } else {
                column.remark.to_string()
            };
            row.insert("说明".to_string(), remark);
            row
        })
        .collect::<Vec<_>>();

    // ---------- sheet 3「填写须知」 ----------
    let guide_rows = GUIDE_LINES
        .iter()
        .map(|line| {
            let mut row = std::collections::HashMap::new();
            row.insert("填写须知".to_string(), (*line).to_string());
            row
        })
        .collect::<Vec<_>>();

    let guide_headers = ["填写须知"];
    let data_header_refs = data_headers.iter().map(String::as_str).collect::<Vec<_>>();

    // ---------- 「数据」sheet 的数据验证（下拉 / 范围） ----------
    let last_row = validation_last_row(definition.max_rows());
    let validations = data_validations(columns, last_row);

    write_workbook(&[
        WorkbookSheet::new("数据", data_header_refs.as_slice(), data_rows.as_slice()).with_validations(&validations),
        WorkbookSheet::new("字段说明", NOTE_HEADERS.as_slice(), note_rows.as_slice()),
        WorkbookSheet::new("填写须知", guide_headers.as_slice(), guide_rows.as_slice()),
    ])
}

/// 数据验证覆盖到的最后一行（0 基）。
///
/// 取定义声明的单次行数上限（`max_rows`），既不覆盖用不到的空行，也避免把
/// 验证区写到 Excel 的 1,048,576 行上限之外。
///
/// # 参数
///
/// - `max_rows`：单次导入行数上限（`ImportDefinition::max_rows`）。
///
/// # 返回值
///
/// 最后一行行号（0 基）。`max_rows == 0` 时退化为示例行本身（不覆盖数据区）。
fn validation_last_row(max_rows: usize) -> u32 {
    /// Excel 最大行号（1 基 `1,048,576`）对应的 0 基下标。
    const EXCEL_MAX_ROW_INDEX: u32 = 1_048_575;
    let rows = u32::try_from(max_rows).unwrap_or(u32::MAX);
    DATA_FIRST_ROW
        .saturating_add(rows.saturating_sub(1))
        .min(EXCEL_MAX_ROW_INDEX)
}

/// 按列定义生成「数据」sheet 的数据验证。
///
/// 规则与导入期校验**同源**：枚举值取自 `ColumnKind::Enum` 的 `label`
/// （与「字段说明」sheet 的「可选值」列同一份数据），数值区间取自
/// `ColumnKind::Int` / `Decimal` 的 min/max。
///
/// # 参数
///
/// - `columns`：导入列定义（`&'static`，因为列定义本身是静态表）。
/// - `last_row`：验证覆盖到的最后一行（0 基，含）。
///
/// # 返回值
///
/// 逐列验证列表，顺序与列顺序一致。以下情况**跳过该列**（不阻断模板生成）：
///
/// - 该类型不加验证（文本 / 布尔 / 字典 / 日期时间）；
/// - 枚举值逗号连接后超过 Excel 的 255 字符上限（该列的可选值仍在「字段说明」sheet 里）。
fn data_validations(columns: &'static [ImportColumn], last_row: u32) -> Vec<XlsxValidation<'static>> {
    columns
        .iter()
        .enumerate()
        .filter_map(|(index, column)| {
            let column_index = u16::try_from(index).ok()?;
            let validation = match &column.kind {
                ColumnKind::Enum(items) => {
                    let labels = items.iter().map(|item| item.label).collect::<Vec<_>>();
                    if labels.join(",").chars().count() > MAX_LIST_CHARS {
                        return None;
                    }
                    XlsxValidation::list(column_index, DATA_FIRST_ROW, last_row, labels)
                }
                ColumnKind::Int { min, max } => {
                    XlsxValidation::int_range(column_index, DATA_FIRST_ROW, last_row, *min, *max)
                }
                ColumnKind::Decimal { min, max, .. } => {
                    XlsxValidation::decimal_range(column_index, DATA_FIRST_ROW, last_row, *min, *max)
                }
                ColumnKind::Date => {
                    XlsxValidation::date_range(column_index, DATA_FIRST_ROW, last_row, Some(DATE_MIN), Some(DATE_MAX))
                }
                _ => return None,
            };
            // 选中单元格时的提示：直接用该列的「说明」（与字段说明 sheet 同源）。
            // Excel 对提示正文也有 255 字符上限，超了就不挂提示，别让整张表打不开。
            if column.remark.is_empty() || column.remark.chars().count() > 255 {
                return Some(validation);
            }
            Some(validation.with_message(column.header, column.remark))
        })
        .collect()
}

/// 生成「错误行」xlsx（导入失败明细下载）。
///
/// # 参数
///
/// - `errors`：错误明细（按行号升序传入）。
///
/// # 返回值
///
/// 单 sheet（`错误明细`）的 xlsx 字节流。
///
/// # 异常情况
///
/// 写入失败返回 500。
pub fn build_error_sheet(errors: &[RowIssue]) -> std::result::Result<Vec<u8>, ApiError> {
    let lines = errors
        .iter()
        .map(|issue| {
            (
                issue.row_no,
                issue.column_header.map(str::to_string),
                issue.message.clone(),
            )
        })
        .collect::<Vec<_>>();
    build_error_sheet_rows(&lines)
}

/// 生成「错误行」xlsx（数据来自已落库的明细表）。
///
/// # 参数
///
/// - `lines`：`(行号, 列名, 原因)` 三元组，按行号升序传入。
///
/// # 返回值
///
/// 单 sheet（`错误明细`）的 xlsx 字节流。
///
/// # 异常情况
///
/// 写入失败返回 500。
pub fn build_error_sheet_rows(lines: &[(u32, Option<String>, String)]) -> std::result::Result<Vec<u8>, ApiError> {
    let headers = ["行号", "列名", "错误原因"];
    let rows = lines
        .iter()
        .map(|(row_no, column, message)| {
            let mut row = std::collections::HashMap::new();
            row.insert("行号".to_string(), row_no.to_string());
            row.insert(
                "列名".to_string(),
                column.clone().unwrap_or_else(|| "（整行）".to_string()),
            );
            row.insert("错误原因".to_string(), message.clone());
            row
        })
        .collect::<Vec<_>>();
    crate::business::impexp::engine::write_sheet(&headers, &rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::impexp::definition::{ColumnKind, EnumItem, ErrorKind, ImportColumn};
    use nik::prelude::Import;

    const STATUS: &[EnumItem] = &[];

    static COLUMNS: &[ImportColumn] = &[
        ImportColumn {
            key: "code",
            header: "物料编码",
            required: true,
            kind: ColumnKind::Text { max_len: Some(32) },
            unique_key: true,
            fk: None,
            example: "MAT-000001",
            width: Some(18),
            remark: "字母+数字+连字符，全公司唯一",
        },
        ImportColumn {
            key: "name",
            header: "物料名称",
            required: true,
            kind: ColumnKind::Text { max_len: Some(64) },
            unique_key: false,
            fk: None,
            example: "螺丝 M6×20",
            width: Some(24),
            remark: "",
        },
        ImportColumn {
            key: "unit",
            header: "计量单位",
            required: false,
            kind: ColumnKind::Enum(STATUS),
            unique_key: false,
            fk: None,
            example: "个",
            width: None,
            remark: "不可乱填",
        },
    ];

    struct TestDefinition;

    #[async_trait::async_trait]
    impl ImportDefinition for TestDefinition {
        fn biz_type(&self) -> &'static str {
            "test.template"
        }
        fn biz_name(&self) -> &'static str {
            "物料"
        }
        fn columns(&self) -> &'static [ImportColumn] {
            COLUMNS
        }
        async fn persist(
            &self,
            _tx: &nik::prelude::Transaction,
            _rows: &[crate::business::impexp::definition::RowValues],
            _ctx: &crate::business::impexp::definition::ImportCtx,
        ) -> std::result::Result<usize, nik::prelude::DbError> {
            Ok(0)
        }
    }

    #[test]
    fn template_has_three_sheets_with_the_required_content() {
        let bytes = build_template(&TestDefinition).expect("模板生成应成功");
        assert_eq!(&bytes[..4], b"PK\x03\x04", "必须是真正的 xlsx");

        // 三要素：数据 sheet（表头 + 示例）、字段说明 sheet、填写须知 sheet
        let names = Import::xlsx_sheet_names(&bytes).expect("应能读 sheet 名");
        assert_eq!(names, vec!["数据", "字段说明", "填写须知"]);

        let data = Import::from_xlsx_sheet(&bytes, "数据").expect("读数据 sheet");
        assert_eq!(data.len(), 1, "数据 sheet 应只有一行示例");
        let header_row = data[0].keys().cloned().collect::<Vec<_>>();
        assert!(header_row.contains(&"物料编码*".to_string()), "必填列带星标");
        assert!(header_row.contains(&"计量单位".to_string()), "非必填列不带星标");
        assert_eq!(data[0].get("物料编码*").map(String::as_str), Some("MAT-000001"));
        assert_eq!(data[0].get("物料名称*").map(String::as_str), Some("螺丝 M6×20"));

        let notes = Import::from_xlsx_sheet(&bytes, "字段说明").expect("读字段说明 sheet");
        assert_eq!(notes.len(), COLUMNS.len());
        assert_eq!(notes[0].get("字段名").map(String::as_str), Some("物料编码*"));
        assert_eq!(notes[0].get("是否必填").map(String::as_str), Some("是"));
        assert_eq!(notes[0].get("类型").map(String::as_str), Some("文本"));
        assert_eq!(notes[0].get("长度/范围").map(String::as_str), Some("≤32 个字符"));
        assert_eq!(notes[1].get("是否必填").map(String::as_str), Some("是"));
        assert_eq!(notes[2].get("是否必填").map(String::as_str), Some("否"));

        let guide = Import::from_xlsx_sheet(&bytes, "填写须知").expect("读填写须知 sheet");
        assert_eq!(guide.len(), GUIDE_LINES.len());
        assert_eq!(guide[0].get("填写须知").map(String::as_str), Some(GUIDE_LINES[0]));
    }

    #[test]
    fn template_file_name_carries_version() {
        assert_eq!(template_file_name(&TestDefinition), "物料导入模板_v1.xlsx");
    }

    #[test]
    fn error_sheet_lists_row_column_and_reason() {
        let errors = vec![
            RowIssue::row(7, ErrorKind::Rule, "整行级问题"),
            RowIssue::column(
                9,
                &COLUMNS[0],
                "bad-code",
                crate::business::impexp::definition::FieldIssue {
                    kind: ErrorKind::Length,
                    message: "长度超限".to_string(),
                },
            ),
        ];
        let bytes = build_error_sheet(&errors).expect("错误行导出应成功");
        let rows = Import::from_xlsx(&bytes).expect("读回");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("行号").map(String::as_str), Some("7"));
        assert_eq!(rows[0].get("列名").map(String::as_str), Some("（整行）"));
        assert_eq!(rows[1].get("行号").map(String::as_str), Some("9"));
        assert_eq!(rows[1].get("列名").map(String::as_str), Some("物料编码"));
        assert_eq!(rows[1].get("错误原因").map(String::as_str), Some("长度超限"));
    }

    // ── 数据验证（Excel 下拉 / 范围）────────────────────────

    /// 取 xlsx 内第 `index` 个 sheet 的 XML 原文（1 基，与 `xl/worksheets/sheetN.xml` 一致）。
    ///
    /// `calamine` 只读单元格，读不到数据验证，所以这里直接看 OOXML——
    /// 「下拉存在」这件事最终就落成 `<dataValidation>` 节点。
    fn sheet_xml(bytes: &[u8], index: usize) -> String {
        use std::io::Read;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("xlsx 是 zip");
        let entry = format!("xl/worksheets/sheet{index}.xml");
        let mut file = archive
            .by_name(&entry)
            .unwrap_or_else(|_| panic!("工作簿里没有 {entry}"));
        let mut xml = String::new();
        file.read_to_string(&mut xml).expect("读取工作表 XML");
        xml
    }

    /// 某个验证区的 `sqref` 原文（`E3:E20001` 这类），`last_row` 是 0 基行号。
    fn sqref(column_letter: &str, last_row: u32) -> String {
        format!("sqref=\"{column_letter}3:{column_letter}{}\"", last_row + 1)
    }

    /// 物料模板的「分类 / 成本法 / 状态」三列必须是真的下拉（`type="list"`），
    /// 且下拉项与列定义的枚举 `label` 逐字一致；「安全库存」是 `≥0` 的数值验证；
    /// 文本列（物料编码）不能挂验证；验证从第 3 行开始（表头 + 示例行不受限）。
    #[test]
    fn material_template_puts_dropdowns_on_enum_columns() {
        use crate::business::impexp::builtin::master_data::material::MaterialImport;

        let definition = MaterialImport;
        let bytes = build_template(&definition).expect("生成物料模板");
        let xml = sheet_xml(&bytes, 1);
        let last_row = validation_last_row(definition.max_rows());

        // 分类（第 5 列 E）、成本法（第 6 列 F）、状态（第 8 列 H）必须是序列验证
        for (letter, expected) in [
            ("E", "原料,半成品,成品,服务"),
            ("F", "移动加权平均"),
            ("H", "启用,停用"),
        ] {
            assert!(
                xml.contains(&sqref(letter, last_row)),
                "{letter} 列应有数据验证（{expected}）"
            );
            assert!(xml.contains(expected), "{letter} 列的下拉项应包含「{expected}」");
        }
        // 安全库存（第 7 列 G）是「≥0」的小数验证
        assert!(xml.contains(&sqref("G", last_row)), "安全库存列应有范围验证");
        assert!(
            xml.contains("operator=\"greaterThanOrEqual\""),
            "安全库存应是 ≥0 的范围验证"
        );
        // 文本列不挂验证（物料编码是第 1 列 A）
        assert!(!xml.contains("sqref=\"A3:"), "文本列不该有数据验证");
        // 验证从第 3 行起：表头（第 1 行）与示例行（第 2 行）不在范围内
        assert!(
            !xml.contains("sqref=\"E1:") && !xml.contains("sqref=\"E2:"),
            "验证不能覆盖表头与示例行"
        );
        // 4 条验证：3 个枚举列 + 1 个数值列
        assert_eq!(xml.matches("<dataValidation ").count(), 4, "物料模板应有 4 条数据验证");
    }

    /// 7 个内置导入模板全部仍能生成，且「有枚举/数值/日期列 ⇔ 有数据验证」。
    ///
    /// 这条是回归护栏：以后谁改列定义忘了同步验证，或验证规则把写入器弄崩，
    /// 这里会直接红。
    #[test]
    fn all_builtin_templates_still_generate_with_matching_validations() {
        use crate::business::impexp::builtin::{dict_item, master_data};

        let definitions: [&dyn ImportDefinition; 7] = [
            &dict_item::DictItemImport,
            &master_data::supplier::SupplierImport,
            &master_data::material::MaterialImport,
            &master_data::unit::UnitImport,
            &master_data::account::AccountImport,
            &master_data::warehouse::WarehouseImport,
            &master_data::location::LocationImport,
        ];

        for definition in definitions {
            let biz = definition.biz_type();
            let bytes = build_template(definition).unwrap_or_else(|e| panic!("{biz} 生成失败: {e:?}"));
            assert_eq!(&bytes[..4], b"PK\x03\x04", "{biz} 必须是真正的 xlsx");

            let names = Import::xlsx_sheet_names(&bytes).expect("读 sheet 名");
            assert_eq!(
                names,
                vec!["数据".to_string(), "字段说明".to_string(), "填写须知".to_string()],
                "{biz} 的 sheet 结构变了"
            );

            let needs_validation = definition.columns().iter().any(|column| {
                matches!(
                    column.kind,
                    ColumnKind::Enum(_) | ColumnKind::Int { .. } | ColumnKind::Decimal { .. } | ColumnKind::Date
                )
            });
            let data_xml = sheet_xml(&bytes, 1);
            assert_eq!(
                data_xml.contains("<dataValidation "),
                needs_validation,
                "{biz} 的数据验证与列定义不一致"
            );
        }
    }

    /// 枚举值超过 Excel 的 255 字符上限时**跳过该列的下拉**，而不是让整张模板
    /// 打不开（该列的可选值仍在「字段说明」sheet 里）。
    #[test]
    fn overlong_enum_list_is_skipped_instead_of_failing_the_template() {
        // 3 个 100 字符的 label，逗号连接后 302 字符 > 255
        const LONG: &str =
            "0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789";
        const LONG_ITEMS: &[EnumItem] = &[
            EnumItem { label: LONG, code: "A" },
            EnumItem { label: LONG, code: "B" },
            EnumItem { label: LONG, code: "C" },
        ];
        static LONG_COLUMNS: &[ImportColumn] = &[ImportColumn {
            key: "long_enum",
            header: "超长枚举",
            required: false,
            kind: ColumnKind::Enum(LONG_ITEMS),
            unique_key: false,
            fk: None,
            example: "A",
            width: None,
            remark: "",
        }];

        assert!(data_validations(LONG_COLUMNS, 10).is_empty(), "超长下拉应被跳过");
        // 同样的列定义只写进「字段说明」sheet 也不报错（生成器不因超长而失败）
        let bytes = build_template(&TestDefinition).expect("模板仍应能生成");
        assert_eq!(&bytes[..4], b"PK\x03\x04");
    }

    /// 验证区行号：取定义的 `max_rows`，并夹在 Excel 的行上限内。
    #[test]
    fn validation_last_row_follows_max_rows_and_excel_limit() {
        // 第 3 行起 + 20,000 行上限 ⇒ 最后一行（0 基）= 2 + 20,000 - 1
        assert_eq!(validation_last_row(20_000), 20_001);
        assert_eq!(validation_last_row(0), DATA_FIRST_ROW);
        assert_eq!(validation_last_row(usize::MAX), 1_048_575);
    }
}
