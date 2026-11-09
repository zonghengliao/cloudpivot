//! 测试辅助：读 xlsx（导入导出产物断言用）。

use calamine::Reader as _;

/// 读第一个 sheet → 二维文本（含表头行）。
pub fn read_xlsx(bytes: &[u8]) -> Vec<Vec<String>> {
    use calamine::{Xlsx, open_workbook_from_rs};
    let cursor = std::io::Cursor::new(bytes.to_vec());
    let mut workbook: Xlsx<_> = open_workbook_from_rs(cursor).expect("打开 xlsx");
    let name = workbook.sheet_names().first().cloned().expect("xlsx 至少一个 sheet");
    let range = workbook.worksheet_range(&name).expect("读 sheet");
    range
        .rows()
        .map(|row| row.iter().map(|cell| cell.to_string()).collect())
        .collect()
}
