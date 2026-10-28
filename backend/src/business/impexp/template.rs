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
//! # 未做的部分（诚实边界）
//!
//! PRD 里的「枚举列写 Excel 下拉数据验证」框架写入器不支持（`rust_xlsxwriter` 的
//! `data_validation` 未被 `alun-utils` 封装暴露），本卡以「字段说明 sheet 的『可选值』列 +
//! 导入期枚举校验」替代；下拉验证留待 A7 前端模板或后续版本。

use alun::prelude::ApiError;

use crate::business::impexp::definition::{ImportDefinition, RowIssue};
use crate::business::impexp::engine::write_workbook;

/// 「字段说明」sheet 的表头。
const NOTE_HEADERS: [&str; 6] = ["字段名", "是否必填", "类型", "长度/范围", "可选值", "说明"];

/// 「填写须知」sheet 的固定文案。
const GUIDE_LINES: [&str; 6] = [
    "1. 请勿修改第 1 行表头，勿增删列；",
    "2. 第 2 行是示例，导入前请删除（系统也会自动识别并跳过）；",
    "3. 单次最多 100,000 行，超出请拆分；",
    "4. 必填列（带 *）不能为空；",
    "5. 日期格式统一 YYYY-MM-DD，枚举 / 字典列请照「字段说明」的可选值填写；",
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
    write_workbook(&[
        ("数据", data_header_refs.as_slice(), data_rows.as_slice()),
        ("字段说明", NOTE_HEADERS.as_slice(), note_rows.as_slice()),
        ("填写须知", guide_headers.as_slice(), guide_rows.as_slice()),
    ])
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
pub fn build_error_sheet_rows(
    lines: &[(u32, Option<String>, String)],
) -> std::result::Result<Vec<u8>, ApiError> {
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
    use alun::prelude::Import;

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
            _tx: &alun::prelude::Transaction,
            _rows: &[crate::business::impexp::definition::RowValues],
            _ctx: &crate::business::impexp::definition::ImportCtx,
        ) -> std::result::Result<usize, alun::prelude::DbError> {
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
        assert!(
            header_row.contains(&"物料编码*".to_string()),
            "必填列带星标"
        );
        assert!(
            header_row.contains(&"计量单位".to_string()),
            "非必填列不带星标"
        );
        assert_eq!(
            data[0].get("物料编码*").map(String::as_str),
            Some("MAT-000001")
        );
        assert_eq!(
            data[0].get("物料名称*").map(String::as_str),
            Some("螺丝 M6×20")
        );

        let notes = Import::from_xlsx_sheet(&bytes, "字段说明").expect("读字段说明 sheet");
        assert_eq!(notes.len(), COLUMNS.len());
        assert_eq!(
            notes[0].get("字段名").map(String::as_str),
            Some("物料编码*")
        );
        assert_eq!(notes[0].get("是否必填").map(String::as_str), Some("是"));
        assert_eq!(notes[0].get("类型").map(String::as_str), Some("文本"));
        assert_eq!(
            notes[0].get("长度/范围").map(String::as_str),
            Some("≤32 个字符")
        );
        assert_eq!(notes[1].get("是否必填").map(String::as_str), Some("是"));
        assert_eq!(notes[2].get("是否必填").map(String::as_str), Some("否"));

        let guide = Import::from_xlsx_sheet(&bytes, "填写须知").expect("读填写须知 sheet");
        assert_eq!(guide.len(), GUIDE_LINES.len());
        assert_eq!(
            guide[0].get("填写须知").map(String::as_str),
            Some(GUIDE_LINES[0])
        );
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
        assert_eq!(
            rows[1].get("错误原因").map(String::as_str),
            Some("长度超限")
        );
    }
}
