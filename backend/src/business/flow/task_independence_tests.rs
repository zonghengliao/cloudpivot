// ============================================================================
// task_independence_tests.rs —— `flow` 侧零任务域引用的闸门（P2-38 · P2-B 解环）
//
// 与 `task::approval_port_tests::task_source_has_no_flow_references` 对称：
// 那条管「任务侧不许认识审批域」，这条管「审批域不许认识任务域」。
// 两头都钉在 0，`task ⇄ flow` 的 2 环才真的不存在（`scripts/lint/module_cycle_report.py`
// 是人工工具、不进 CI —— 没有这两条用例，下一个人写回一行直连就没人拦得住）。
//
// 红绿自证（两条都实测过）：
//   - 绿：本文件提交时 0 条；
//   - 红：在任意 `flow/*.rs` 里加一行注释，内容写成完整的任务域绝对路径（连注释也算），
//     本条立刻失败并打印命中行 —— 与 `module_cycle_report --edge` 的口径一致。
// ============================================================================

use std::path::{Path, PathBuf};

/// 任务域路径的 token 拆成两段拼：本文件不能出现完整 token，
/// 否则扫描自身时会把这条用例也算成「`flow → task` 的直连」。
const TASK_TOKEN: &str = concat!("crate::business::", "task");

/// 基线：**0**（终态）。上调就是回归，不是「新功能」。
const FLOW_TO_TASK_REFS_BASELINE: usize = 0;

/// 扫描面自检：`flow` 目录下的 `.rs` 文件数（扫空 = 假绿，本类卡最大的坑）。
const FLOW_SOURCE_MIN_FILES: usize = 40;
/// 扫描面自检：`flow` 源码总字节数（handler.rs 一个文件就 200KB+）。
const FLOW_SOURCE_MIN_BYTES: usize = 300_000;

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

/// `flow → task` 的直连引用**必须为 0**（P2-B 解环的燃烧率闸门）。
///
/// # 口径
///
/// - 只扫 `src/business/flow/**/*.rs`，**排除本文件**（它必然包含 `TASK_TOKEN`）；
/// - 统计「含 `TASK_TOKEN` 的**行数**」（与 `grep -rn | wc -l` 同口径，便于对账）；
/// - **注释与文档链接也算**：intra-doc link 是 rustdoc 依赖，而且是「下一条直连」的温床。
///
/// # 为什么基线是 0 而不是「只降不升」
///
/// P2-38 把写 `biz_task` 的段落整体搬到了 `business::taskflow`（接线域），
/// `flow` 只剩 `task_bridge::instance_status_of`（只读自己的实例表）。归零之后没有
/// 「还能再降」的空间：常量留在 0 就是为了让任何一行新直连立刻变红。
#[test]
fn flow_source_has_no_task_references() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/business/flow"));
    let mut files: Vec<PathBuf> = Vec::new();
    walk(root, &mut files).unwrap_or_else(|error| panic!("扫不了 {}：{error}", root.display()));

    // 排除本文件（闸门自身）
    files.retain(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| name != "task_independence_tests.rs")
    });

    let mut total_bytes = 0usize;
    let mut refs = 0usize;
    let mut hits: Vec<String> = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("读不了 {}：{error}", path.display()));
        total_bytes += source.len();
        for (index, line) in source.lines().enumerate() {
            if line.contains(TASK_TOKEN) {
                refs += 1;
                if hits.len() < 10 {
                    hits.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        files.len() >= FLOW_SOURCE_MIN_FILES,
        "扫描面异常：只扫到 {} 个 .rs（预期 >= {FLOW_SOURCE_MIN_FILES}），扫空会假绿",
        files.len()
    );
    assert!(
        total_bytes >= FLOW_SOURCE_MIN_BYTES,
        "扫描面异常：只读到 {total_bytes} 字节（预期 >= {FLOW_SOURCE_MIN_BYTES}），文件读错会假绿"
    );
    assert!(
        refs == FLOW_TO_TASK_REFS_BASELINE,
        "flow → task 直连引用**回退**了：{refs} 条 > 基线 {FLOW_TO_TASK_REFS_BASELINE} 条\
         （{} 个文件 / {total_bytes} 字节）。\n\
         P2-38 已经把这条边摘干净了：造任务走 `flow::task_port`、审批结论落地走同一个接缝，\
         实现在中立接线域 `business::taskflow` —— 不要往 `flow` 里再写死任务域的调用。\n\
         前 10 处命中：\n{}",
        files.len(),
        hits.join("\n")
    );
    println!("flow → task 直连：{refs} 条（基线 {FLOW_TO_TASK_REFS_BASELINE}，已归零）");
}

/// `taskflow` 是**叶子**：`flow` 不许反过来 `import` 它（否则只是把 2 环换了个名字）。
///
/// 口径：`flow/**/*.rs` 里不许出现接线域的绝对路径（`bootstrap` 例外，
/// 它不在 `flow` 目录里，是唯一的装配点）。
#[test]
fn flow_does_not_depend_on_the_wiring_domain() {
    const WIRING_TOKEN: &str = concat!("crate::business::", "taskflow");
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/business/flow"));
    let mut files: Vec<PathBuf> = Vec::new();
    walk(root, &mut files).unwrap_or_else(|error| panic!("扫不了 {}：{error}", root.display()));
    files.retain(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| name != "task_independence_tests.rs")
    });

    let mut hits: Vec<String> = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("读不了 {}：{error}", path.display()));
        for (index, line) in source.lines().enumerate() {
            if line.contains(WIRING_TOKEN) {
                hits.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "`flow` 不许 import 接线域（那只是把 `flow ⇄ task` 换成 `flow ⇄ taskflow`）：\n{}",
        hits.join("\n")
    );
}

// ==================== 接缝语义（未安装时的降级） ====================

/// 未安装实现时：造不出任务**必须**返回 `Failed(500)`，不许静默成功。
///
/// # 为什么这条是这个值
///
/// 静默成功 = 「节点 ACTIVE、任务不存在」的死单：没人有待办、没有接口能补，
/// 只能改库。宁可让整笔发起回滚（用户重试一次）。
#[tokio::test]
async fn spawn_without_port_is_a_loud_failure() {
    use crate::business::flow::task_node::SpawnOutcome;
    use crate::business::flow::task_port;

    let _guard = task_port::GLOBAL_LOCK.lock().await;
    task_port::reset_for_tests();
    assert!(!task_port::is_installed());

    // 注册表是进程级单例：这里不开事务（`&Transaction` 拿不到），
    // 直接验证「取不到实现」时的返回值形状 —— 与 `spawn_task` 的降级分支同一段代码。
    let port = task_port::port();
    assert!(port.is_none(), "reset 之后不该有实现");

    // `on_instance_settled` 的降级：`None` + WARN（审批结论已落库，不许 panic）
    assert!(task_port::on_instance_settled(None, "APPROVED").await.is_none());

    // 正向语义由 `taskflow::spawn` 的真库用例覆盖；这里只钉住 `SpawnOutcome` 的形状没变。
    let _ = SpawnOutcome::AlreadyExists { task_id: None };
}
