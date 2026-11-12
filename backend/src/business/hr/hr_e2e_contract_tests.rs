//! HR-46 的契约测试：**10 步 E2E 脚本**的静态形态。
//!
//! 为什么这些用例不连库：本卡的交付物是一条**脚本**
//! （`scripts/verify/hr/hr_salary_e2e.sh` + `scripts/verify/hr/hr_e2e_xlsx.py`），
//! 它真正跑一次要「在 145 建演练库 → 回放 300+ 条迁移 → 起后端 → 走 10 步 HTTP」，
//! 几分钟量级且需要库口令 —— 那是 CI / 人工演练的活，不适合塞进 `cargo test`。
//!
//! 这里守的是**跑不了库也能发现**的退化（每一条都对应一次真实会踩的坑）：
//!
//! 1. 脚本或 10 步声明被删 / 被改名 → 卡面验收「一条命令跑完 10 步」不再成立；
//! 2. 某一步只剩 0~1 条断言 → 「每步有断言」变成空话（改坏了也绿）；
//! 3. 演练库护栏（名称含 drill/e2e/test）被删 → 脚本会往生产库写夹具；
//! 4. `--fresh` 不再 DROP 自己建的库 → 99/145 上开始堆垃圾库（用户已批评过一次）；
//! 5. 失败定位（`locate`）被删 → 失败时只剩一个 [FAIL]，没人知道是哪个请求；
//! 6. xlsx 生成器偷偷 `import openpyxl` → 换台机器就跑不起来（E2E 必须零第三方依赖）；
//! 7. 三个**设计硬口径**的断言消失：审批通过不出凭证（§4.3）、红冲借贷互换（§4.8.2）、
//!    逐科目净额 0（§4.6 勾稽）。
//!
//! 与 HR-45 的 `hr_invariants_tests.rs` 同一做法：用 [`env!("CARGO_MANIFEST_DIR")`]
//! 定位仓库根读文件，不引新依赖；读不到直接 panic（**不**静默跳过）。

use std::path::{Path, PathBuf};

/// 仓库根（`backend/` 的上一级）。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend/ 一定有上级目录")
        .to_path_buf()
}

/// 读仓库内文件；读不到直接失败（脚本被删时用例必须红，不许静默 SKIP）。
fn read_repo(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()))
}

const E2E: &str = "scripts/verify/hr/hr_salary_e2e.sh";
const XLSX: &str = "scripts/verify/hr/hr_e2e_xlsx.py";

/// 把脚本按 `step <n> "<名称>"` 切成 10 段（返回 `(序号, 名称, 段文本)`）。
///
/// 段文本 = 该 `step` 行到下一个 `step` 行之间的内容 —— 断言计数只在这个窗口里做，
/// 否则「后面所有步骤的断言」都会被算到第一段头上。
fn steps_of(script: &str) -> Vec<(u32, String, String)> {
    let mut out: Vec<(u32, String, String)> = Vec::new();
    for line in script.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("step ") {
            let mut parts = rest.splitn(2, ' ');
            let Ok(no) = parts.next().unwrap_or_default().parse::<u32>() else {
                continue;
            };
            let name = parts.next().unwrap_or_default().trim().trim_matches('"').to_string();
            out.push((no, name, String::new()));
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.2.push_str(line);
            last.2.push('\n');
        }
    }
    out
}

/// 去掉 shell 注释行（`#` 开头的行，含缩进后的 `#`）。
///
/// 为什么要去：脚本头注释里大量出现「**审批通过不出凭证**」「CREATE DATABASE」「drill」
/// 这类字样 —— 注释在说「禁止/要求」，直接 grep 会把「注释里写了」当成「代码里做了」，
/// 于是断言被删掉、行为被改掉，用例照样绿（HR-45 的契约测试踩过同一个坑）。
fn code_only(script: &str) -> String {
    script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 从 `start` 起取一段**字符边界安全**的窗口（用于「函数体里有没有调 X」这类检查）。
fn window(text: &str, start: usize, len: usize) -> &str {
    let Some(tail) = text.get(start..) else {
        return "";
    };
    let end = tail
        .char_indices()
        .map(|(idx, ch)| idx + ch.len_utf8())
        .take_while(|end| *end <= len)
        .last()
        .unwrap_or(0);
    &tail[..end]
}

#[test]
fn script_declares_exactly_ten_steps_in_order() {
    let script = read_repo(E2E);
    let steps = steps_of(&script);
    let numbers: Vec<u32> = steps.iter().map(|(no, _, _)| *no).collect();
    assert_eq!(
        numbers,
        (1..=10).collect::<Vec<u32>>(),
        "卡面要求「一条命令跑完 10 步」，步骤编号必须恰好是 1..=10 且不重复（实际 {numbers:?}）"
    );
    for (no, name, _) in &steps {
        assert!(!name.is_empty(), "第 {no} 步没有中文名（打印出来没法定位）");
    }
    // 卡面点名的 8 个业务环节必须都能在步骤名里找到对应词，别把某一步悄悄换成别的事
    let all: String = steps
        .iter()
        .map(|(_, name, _)| name.clone())
        .collect::<Vec<_>>()
        .join(" ");
    for keyword in ["建员工", "导入考勤", "请假", "试算", "审批", "过账", "红冲", "对账"] {
        assert!(
            all.contains(keyword),
            "步骤名里找不到卡面环节「{keyword}」，实际：{all}"
        );
    }
}

#[test]
fn every_step_carries_at_least_two_assertions() {
    let script = read_repo(E2E);
    for (no, name, body) in steps_of(&script) {
        let count = body.matches("expect_eq").count()
            + body.matches("expect_numeric").count()
            + body.matches("expect_ne").count()
            + body.matches("fail_line").count();
        assert!(
            count >= 2,
            "第 {no} 步「{name}」只有 {count} 条断言 —— 卡面要求「每步有断言」，"
        );
    }
}

#[test]
fn drill_db_guard_runs_before_any_write() {
    let script = read_repo(E2E);
    let code = code_only(&script);
    let guard = code
        .find("*drill*|*Drill*|*e2e*|*E2E*|*test*|*Test*")
        .expect("演练库护栏（库名含 drill/e2e/test）被删了 —— 脚本会往生产库写夹具");
    let create = code
        .find("CREATE DATABASE")
        .expect("脚本不再自己建库（--fresh 模式消失？）");
    assert!(
        guard < create,
        "护栏必须在建库/写库之前执行（当前护栏偏移 {guard}，建库偏移 {create}）"
    );
    assert!(code.contains("不像演练库"), "护栏要有可读的拒绝原因（不能只 return 0）");
}

#[test]
fn fresh_mode_drops_only_its_own_database_on_exit() {
    let script = read_repo(E2E);
    let code = code_only(&script);
    assert!(
        code.contains("trap cleanup EXIT"),
        "没有 EXIT trap：中途失败会留下实例/库"
    );
    let cleanup = code
        .find("cleanup()")
        .map(|pos| &code[pos..])
        .expect("cleanup() 不见了");
    assert!(
        cleanup.contains("DROP DATABASE IF EXISTS"),
        "cleanup 里没有 DROP：--fresh 会给 145 堆垃圾库（99 上已清理过 655 个）"
    );
    assert!(
        cleanup.contains(r#"[ "$KEEP_DB" != "1" ]"#),
        "DROP 必须能被 KEEP_DB=1 关掉（排查现场要留库）"
    );
    // 只删自己建的库：外部给的 HR_DB_URL 一律不动（否则会把别人的演练库删掉）
    assert!(
        cleanup.contains(r#"[ "$OWN_DB" = "1" ]"#),
        "cleanup 的 DROP 没有 OWN_DB 守卫：会把外部 HR_DB_URL 指的库一起删掉"
    );
    assert!(
        code.contains("OWN_DB=1"),
        "没有 OWN_DB 标记位：区分不出「自己建的库」和「别人给的库」"
    );
}

#[test]
fn failures_print_request_response_and_log_tail() {
    let script = read_repo(E2E);
    let locate = script
        .find("locate()")
        .map(|pos| &script[pos..])
        .expect("locate()（失败定位）被删了：失败只剩一个 [FAIL]，没人知道是哪个请求");
    let end = locate.find("\n# ---").unwrap_or(locate.len());
    let body = &locate[..end];
    for needle in ["LAST_METHOD", "LAST_PATH", "回执", "服务端日志尾"] {
        assert!(
            body.contains(needle),
            "失败定位里缺「{needle}」：定位不到就等于失败不可诊断"
        );
    }
    // 断言函数必须真的调 locate（否则定义了也没人用）
    for helper in ["expect_eq() {", "expect_numeric() {"] {
        let start = script.find(helper).expect("断言函数不见了");
        let body = window(&script, start, 700);
        assert!(
            body.contains("locate"),
            "{} 失败时没有调 locate",
            helper.trim_end_matches("() {")
        );
    }
}

#[test]
fn xlsx_helper_has_no_third_party_import() {
    let py = read_repo(XLSX);
    // 只看**真 import 语句**：文件头注释里提到「不依赖 openpyxl」是解释，不是依赖
    for banned in [
        "import openpyxl",
        "from openpyxl",
        "import xlsxwriter",
        "import pandas",
        "import numpy",
    ] {
        assert!(
            !py.contains(banned),
            "xlsx 生成器不许依赖第三方库（{banned}）：E2E 要能在任何测试机零安装跑通"
        );
    }
    for needed in ["zipfile", "sharedStrings", "sheet1.xml"] {
        assert!(py.contains(needed), "xlsx 生成器缺 OOXML 必要部分：{needed}");
    }
}

#[test]
fn design_hard_rules_are_asserted_in_the_chain() {
    let script = read_repo(E2E);
    let code = code_only(&script);
    // §4.3：审批通过不出凭证（发给钱的那一步必须是显式「过账」，不是审批回调）
    assert!(
        code.contains("审批通过**不出凭证**"),
        "缺 §4.3 硬口径断言：审批通过不出凭证"
    );
    // §4.8.2：红冲两张红字、借贷互换、原凭证回写 reversed_by
    for needle in ["两张红字", "红字借贷互换", "reversed_by"] {
        assert!(code.contains(needle), "缺红冲断言：{needle}（设计 §4.8.2）");
    }
    // §4.6：逐科目净额 0（红冲之后整条工资链在账上不留痕）
    assert!(
        code.contains("逐科目净额"),
        "缺 §4.6 勾稽断言：工资链凭证逐科目净额必须为 0"
    );
    // §4.9：10 条巡检断言复用 HR-45 的脚本，本链路跑完必须 BLOCKER 全 0
    assert!(
        code.contains("hr_invariants.sh"),
        "对账那一步没有复用 HR-45 的巡检脚本（10 条断言的唯一实现）"
    );
    assert!(code.contains("blocker_fail"), "没有断言 blocker_fail=0：巡检红了也绿");
}

#[test]
fn amounts_in_expected_values_are_self_consistent() {
    // 期望值不是拍脑袋写的：应发 = 标准 3 项 + 加班费 + 全勤，实发 = 应发 − 代扣
    let script = read_repo(E2E);
    let number = |key: &str| -> f64 {
        let line = script
            .lines()
            .find(|line| line.starts_with(&format!("{key}=")))
            .unwrap_or_else(|| panic!("脚本里找不到夹具变量 {key}="));
        line.split('=')
            .nth(1)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or_else(|| panic!("{key} 的值不是数字：{line}"))
    };
    let base = number("BASE_AMT");
    let post = number("POST_AMT");
    let meal = number("MEAL_AMT");
    let ot = number("EXPECT_OT_WD");
    let bonus = number("EXPECT_ATT_BONUS");
    let gross = number("EXPECT_GROSS");
    let net = number("EXPECT_NET");
    let accrual = number("EXPECT_ACCRUAL");
    let deduct = number("SI_PERSONAL") + number("HF_PERSONAL") + number("IIT");
    let company = number("SI_COMPANY") + number("HF_COMPANY");

    assert!(
        (base + post + meal + ot + bonus - gross).abs() < 0.005,
        "应发合计对不上：{base}+{post}+{meal}+{ot}+{bonus} != {gross}"
    );
    assert!(
        (gross - deduct - net).abs() < 0.005,
        "实发合计对不上：{gross}-{deduct} != {net}"
    );
    assert!(
        (gross + company - accrual).abs() < 0.005,
        "计提合计对不上：{gross}+{company} != {accrual}"
    );
}
