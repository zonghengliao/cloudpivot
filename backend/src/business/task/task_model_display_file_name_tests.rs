// ============================================================================
// task_model_display_file_name_tests.rs —— 附件显示名纯逻辑回归
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::display_file_name;

/// 正常路径：有 `original_name` 时一律用它（AI 产出的 `recheck.md` 就靠这条）。
#[test]
fn prefers_original_name() {
    assert_eq!(
        display_file_name(Some("recheck.md"), "2026/10/29/01a1874d.md"),
        "recheck.md"
    );
    // 原始名带空白要 trim，不能把空格写进保存名
    assert_eq!(
        display_file_name(Some("  report.md "), "2026/10/29/x.md"),
        "report.md"
    );
}

/// 边界 1：`original_name` 为 NULL → 退回存储名的 basename（去掉日期目录）。
#[test]
fn falls_back_to_stored_basename() {
    assert_eq!(
        display_file_name(None, "2026/10/29/01a1874d.md"),
        "01a1874d.md"
    );
    // Windows 风格分隔符同样要剥掉（备份 / 迁移数据里出现过）
    assert_eq!(
        display_file_name(None, "2026\\10\\29\\01a1874d.md"),
        "01a1874d.md"
    );
    // 没有分隔符 → 原样返回
    assert_eq!(display_file_name(None, "plain.md"), "plain.md");
}

/// 边界 2：`original_name` 是空串 / 全空白（上传时文件名缺失）→ 仍要 basename，不能下发空名或路径。
#[test]
fn empty_original_name_is_treated_as_missing() {
    assert_eq!(display_file_name(Some(""), "2026/10/29/x.md"), "x.md");
    assert_eq!(display_file_name(Some("   "), "2026/10/29/x.md"), "x.md");
}

/// 边界 3：两侧都空 → 空串（前端 `|| 'attachment'` 兜底），且绝不返回带分隔符的名字。
#[test]
fn never_returns_a_path() {
    assert_eq!(display_file_name(None, ""), "");
    // 尾部带分隔符时不能返回空串（取最后一个非空段）
    assert_eq!(display_file_name(None, "2026/10/29/"), "29");
    for case in [
        display_file_name(None, "2026/10/29/x.md"),
        display_file_name(Some(""), "2026/10/29/x.md"),
        display_file_name(Some("y.md"), "2026/10/29/x.md"),
    ] {
        assert!(!case.contains('/'), "显示名不许含路径分隔符：{case}");
    }
}
