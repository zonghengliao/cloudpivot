// ============================================================================
// independence_tests.rs —— `masking` 侧零导出域引用的闸门（P2-39 · P2-B 解环）
//
// 依赖方向只有一条：**导出域 → 脱敏域**。
//   - 导出引擎 `format_export_rows` 拿 `&dyn Masker`（本模块的接缝）；`MaskSet`
//     只是它的实现之一 —— 导出域不需要知道规则怎么合并；
//   - 脱敏域**不许**反过来引用导出域：`masking → impexp` 归零，`impexp ⇄ masking`
//     的 2 环才真的不存在。
//
// 为什么要有这个用例（而不是只靠 `scripts/lint/module_cycle_report.py`）：
// 那个脚本是人工工具、不进 CI；下一个人把「导出定义」或「导出 e2e 用例」写回
// `masking/` 时，没有任何东西拦得住 —— 直到某天有人重新画依赖图才发现环又长回来了。
//
// 红绿自证（本文件提交时实测过）：
//   - 绿：本文件扫描时命中 0 条；
//   - 红：在任意 `masking/*.rs` 里加一行（注释也算）写完整的导出域绝对路径，
//     本条立刻失败并打印命中行 —— 与 `module_cycle_report --edge` 的口径一致。
// ============================================================================

use std::path::{Path, PathBuf};

/// 导出域路径的 token 拆成两段拼：本文件不能出现完整 token，
/// 否则扫描自身时会把这条闸门也算成「`masking → impexp` 的直连」。
const IMPEXP_TOKEN: &str = concat!("crate::business::", "impexp");

/// 基线：**0**（终态）。上调就是回归，不是「新功能」。
const MASKING_TO_IMPEXP_REFS_BASELINE: usize = 0;

/// 扫描面自检：`masking/` 下的 `.rs` 文件数（扫空 = 假绿，本类卡最大的坑）。
const MASKING_SOURCE_MIN_FILES: usize = 4;
/// 扫描面自检：`masking/` 源码总字节数（实测约 152 KB，取 100 KB 兜底）。
const MASKING_SOURCE_MIN_BYTES: usize = 100_000;

fn masking_dir() -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/business/masking")).to_path_buf()
}

/// 收集目录下全部 `.rs`（不递归 —— `masking/` 是平铺的；将来若分子目录，改成递归即可）。
fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

/// `masking → impexp` 的直连引用**必须为 0**（P2-39 解环的燃烧率闸门）。
///
/// # 口径（与 `scripts/lint/module_cycle_report.py` 一致）
///
/// - 只扫 `src/business/masking/**/*.rs`，**排除本文件**（它必然包含 `IMPEXP_TOKEN`）；
/// - 统计「含 `IMPEXP_TOKEN` 的**行数**」（与 `grep -rn | wc -l` 同口径，便于对账）；
/// - **注释与文档链接也算**：intra-doc link 是 rustdoc 依赖，也是「下一条直连」的温床。
///
/// # 为什么基线是 0
///
/// P2-39 把 `sys_user` 导出定义搬去 `impexp/builtin/user_export.rs`、把导出链路的
/// e2e 用例搬去 `impexp/export_mask_tests.rs`；脱敏域只剩「规则解析 + [`Masker`]
/// 接缝」。归零之后没有「还能再降」的空间 —— 常量留在 0 就是为了让任何一行新直连
/// 立刻变红。
///
/// [`Masker`]: crate::business::masking::masker::Masker
#[test]
fn masking_source_has_no_impexp_references() {
    let root = masking_dir();
    let mut files: Vec<PathBuf> = Vec::new();
    walk(&root, &mut files).unwrap_or_else(|error| panic!("扫不了 {}：{error}", root.display()));

    // 排除本文件（闸门自身）
    files.retain(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| name != "independence_tests.rs")
    });

    let mut total_bytes = 0usize;
    let mut refs = 0usize;
    let mut hits: Vec<String> = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("读不了 {}：{error}", path.display()));
        total_bytes += source.len();
        for (index, line) in source.lines().enumerate() {
            if line.contains(IMPEXP_TOKEN) {
                refs += 1;
                if hits.len() < 10 {
                    hits.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        files.len() >= MASKING_SOURCE_MIN_FILES,
        "扫描面异常：只扫到 {} 个 .rs（预期 >= {MASKING_SOURCE_MIN_FILES}），扫空会假绿",
        files.len()
    );
    assert!(
        total_bytes >= MASKING_SOURCE_MIN_BYTES,
        "扫描面异常：只读到 {total_bytes} 字节（预期 >= {MASKING_SOURCE_MIN_BYTES}），文件读错会假绿"
    );
    assert!(
        refs == MASKING_TO_IMPEXP_REFS_BASELINE,
        "masking → impexp 直连引用**回退**了：{refs} 条 > 基线 {MASKING_TO_IMPEXP_REFS_BASELINE} 条\
         （{} 个文件 / {total_bytes} 字节）。\n\
         P2-39 已经把这条边摘干净了：导出定义放 `impexp/builtin/`、导出脱敏的 e2e 用例放\
         `impexp/export_mask_tests.rs`，脱敏域只对外提供 `masker::Masker` 接缝 —— \
         不要再往 `masking/` 里写导出域的类型或路径。\n\
         前 10 处命中：\n{}",
        files.len(),
        hits.join("\n")
    );
    println!("masking → impexp 直连：{refs} 条（基线 {MASKING_TO_IMPEXP_REFS_BASELINE}，已归零）");
}

/// 接缝本身必须**对象安全**，且 `MaskSet` 是它的实现：这是「导出域只认 trait」的编译期保证。
///
/// 断言点：
/// 1. `&MaskSet` 能直接当 `&dyn Masker` 用（导出引擎的函数签名就是这么收的）；
/// 2. 空集合未豁免、且对任何列返回 `None`（调用方按 fail-closed 处理）；
/// 3. `is_exempt` 在 trait 层与固有方法层给出同一个答案（两处口径不许漂）。
#[test]
fn mask_set_satisfies_the_masker_seam() {
    use crate::business::masking::masker::Masker;
    use crate::business::masking::registry::MaskSet;

    let set = MaskSet::empty();
    let seam: &dyn Masker = &set;
    assert_eq!(
        seam.is_exempt(),
        MaskSet::is_exempt(&set),
        "trait 层与固有层对「是否豁免」必须同口径"
    );
    assert!(!seam.is_exempt(), "空集合不是豁免集合");
    assert!(
        seam.mask_value("sys_user", "mobile", "13812345678")
            .expect("空集合的脱敏计算不应失败")
            .is_none(),
        "空集合对任何列都应返回 None（= 没有规则），由调用方 fail-closed"
    );
    println!("MaskSet → &dyn Masker 接缝可用；空集合返回 None ✓");
}

/// 反方向（导出域 → 脱敏域）必须**存在**：`format_export_rows` 的第四个参数是 `&dyn Masker`。
///
/// 这条用例与 [`masking_source_has_no_impexp_references`] 配对，钉住「解环」不是
/// 「把两边都删掉」：导出侧如果真的不再用脱敏接缝，那说明脱敏被静默摘掉了 —— 那是
/// 安全回归，比环更严重。
#[test]
fn impexp_engine_consumes_the_masker_seam() {
    let engine = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/business/impexp/engine.rs"));
    let source = std::fs::read_to_string(engine).unwrap_or_else(|error| panic!("读不了 {}：{error}", engine.display()));
    // 生产面（`#[cfg(test)] mod tests` 之前）才是依赖方向的证据：
    // 测试里用 `MaskSet` 造夹具不算焊死。注意 `engine.rs` 里还有 `#[cfg(test)] fn`
    // 这样的单测辅助函数，所以按「文件尾部测试模块」切，不能按第一个 `#[cfg(test)]` 切。
    let production = source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("文件至少有一段生产代码");
    let seam_token = concat!("masking::masker::", "Masker");
    // 接缝的 import 可能是 `{MaskError, Masker}` 这类分组写法，所以分两步查：
    // ① 生产面 import 了脱敏域的 masker 模块；② 那次 import 里带 `Masker`。
    let imports_seam = production
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("use ") && line.contains("business::masking::masker::"))
        .any(|line| line.contains("Masker"));
    assert!(
        imports_seam,
        "导出引擎必须 import 脱敏接缝 `{seam_token}`——脱敏若被摘掉就是安全回归"
    );
    assert!(
        production.contains("mask: &dyn Masker"),
        "`format_export_rows` 的 `mask` 参数必须收 `&dyn Masker`（收具体 `MaskSet` 等于把脱敏域的实现细节焊进导出域）"
    );
    assert!(
        !production.contains("registry::MaskSet"),
        "引擎生产面不许出现 `registry::MaskSet`：规则集怎么合并是脱敏域的事，导出域只认接缝"
    );
    println!("导出引擎生产面经 &dyn Masker 取规则、零 MaskSet 依赖 ✓");
}
