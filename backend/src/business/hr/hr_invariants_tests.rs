//! HR-45 的契约测试：**收口自检迁移 + 巡检脚本**的静态形态。
//!
//! 为什么这些用例不连库：本卡交付的两件东西都是「**脚本**」——
//! `backend/migrations/business/V0398__hr_selfcheck.up.sql`（一次性，缺种子就拦升级）与
//! `scripts/verify/hr_invariants.sql`（可反复跑，10 条数据一致性断言）。
//! 它们的真库行为由 `scripts/verify/hr_invariants_selftest.sh` 在演练库上实测
//! （造错自检：R0 全绿零误报 + I1~I10 十条断言逐条报红 + F1 fail-closed），
//! 那是「跑一次要几秒 + 需要库」的活，不适合塞进 `cargo test`。
//!
//! 这里守的是**跑不了库也能发现**的退化：脚本被删、断言被悄悄减成 9 条、
//! 巡检脚本被人加了一条 `UPDATE`（巡检必须是只读的）、自检迁移被人塞进了 DDL、
//! 造错自检的注入项漏了一条（于是「十条断言都有牙」变成「九条有牙」）。
//!
//! 相邻口径：`shared/state_machine.rs` 的 `hr_ddl()` 也读迁移文件做对拍，
//! 同样用 [`env!("CARGO_MANIFEST_DIR")`] 定位仓库根 —— 沿用同一做法，不引新依赖。

use std::path::{Path, PathBuf};

/// 仓库根（`backend/` 的上一级）。脚本在 `<root>/scripts/verify/`，迁移在 `<root>/backend/migrations/`。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend/ 一定有上级目录")
        .to_path_buf()
}

/// 读仓库内文件；读不到直接失败（**不**静默跳过 —— 脚本被删时用例必须红）。
fn read_repo(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()))
}

/// 去掉 `--` 行注释（迁移/脚本头注释里大量出现 INSERT / UPDATE 字样，
/// 直接 grep 关键词会把「注释在说禁止写」误判成「真的在写」）。
fn strip_line_comments(sql: &str) -> String {
    sql.lines()
        .map(|line| match line.find("--") {
            Some(pos) => &line[..pos],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const SELFTEST: &str = "scripts/verify/hr_invariants_selftest.sh";
const INVARIANTS: &str = "scripts/verify/hr_invariants.sql";
const FIXTURE: &str = "scripts/verify/hr/hr_invariants_fixture.sql";
const SELFCHECK_UP: &str = "backend/migrations/business/V0398__hr_selfcheck.up.sql";
const SELFCHECK_DOWN: &str = "backend/migrations/business/V0398__hr_selfcheck.down.sql";

#[test]
fn invariants_sql_lists_all_ten_blocker_assertions() {
    let sql = read_repo(INVARIANTS);
    // 设计 §4.9 的级别：A1/A2/A3/A6/A7/A8/A9/A10 = BLOCKER，A4/A5 = WARN（合同类只报不拦），
    // 另加 W1/W2 两条 WARN 观察项。级别写错等于把「观察」变成「拦升级」或反过来。
    let levels = [
        ("A1", "BLOCKER"),
        ("A2", "BLOCKER"),
        ("A3", "BLOCKER"),
        ("A4", "WARN"),
        ("A5", "WARN"),
        ("A6", "BLOCKER"),
        ("A7", "BLOCKER"),
        ("A8", "BLOCKER"),
        ("A9", "BLOCKER"),
        ("A10", "BLOCKER"),
        ("W1", "WARN"),
        ("W2", "WARN"),
    ];
    for (no, level) in levels {
        let needle = format!("SELECT '{no}', '{level}'");
        assert!(
            sql.contains(&needle),
            "巡检脚本缺少断言 {no}（找不到 `{needle}`）—— 设计 §4.9 是 10 条 + 2 条观察项"
        );
    }
}

#[test]
fn invariants_sql_is_read_only() {
    // 巡检会在生产库上跑：任何写业务表的语句都是事故（改动线上数据 / 锁表）。
    let raw = read_repo(INVARIANTS);
    let code = strip_line_comments(&raw);
    let upper = code.to_uppercase();

    for banned in [
        "INSERT INTO BIZ_",
        "INSERT INTO SYS_",
        "UPDATE BIZ_",
        "UPDATE SYS_",
        "DELETE FROM BIZ_",
        "DELETE FROM SYS_",
        "ALTER TABLE",
        "TRUNCATE BIZ_",
        "TRUNCATE SYS_",
        "DROP TABLE BIZ_",
        "DROP TABLE SYS_",
        "CREATE INDEX",
        "CREATE UNIQUE INDEX",
        "CREATE TABLE BIZ_",
        "CREATE TABLE SYS_",
    ] {
        assert!(
            !upper.contains(banned),
            "巡检脚本里出现了 `{banned}` —— 巡检必须只读（唯一允许的建表是 CREATE TEMP）"
        );
    }
    // 结果表必须是 TEMP：普通表会留下垃圾表，且两个会话并发跑会互相踩。
    assert!(
        code.contains("CREATE TEMP TABLE IF NOT EXISTS hr_inv_result"),
        "结果表必须是 `CREATE TEMP TABLE IF NOT EXISTS hr_inv_result`"
    );
    // 退出语义：没违规也要打印汇总行（脚本靠它判定），有 BLOCKER 要能 RAISE。
    for needle in ["HR_INVARIANTS_SUMMARY", "RAISE EXCEPTION 'HR 巡检失败"] {
        assert!(code.contains(needle), "巡检脚本缺少退出语义锚点：{needle}");
    }
}

#[test]
fn invariants_sql_keeps_the_money_and_status_contracts() {
    let sql = read_repo(INVARIANTS);
    // A1 的机器人账号口径（`sys_user.user_type = 'HUMAN'`，见 shared::ai_actor）
    assert!(
        sql.contains("'HUMAN'"),
        "A1 丢掉 user_type='HUMAN' 口径 ⇒ 机器人账号能挂员工档"
    );
    // A8 的凭证来源类型（HR-06 的 VoucherSourceType 库内值）
    for source in ["PAYROLL_ACCRUAL", "PAYROLL_PAYMENT"] {
        assert!(sql.contains(source), "A8 丢掉来源类型 {source}");
    }
    // A9 的两个分支（凭证期间存在 / 已结账期间的待过账死单）都要在
    for needle in ["biz_accounting_period", "'CLOSED'", "'SUBMITTED', 'APPROVED'"] {
        assert!(sql.contains(needle), "A9 缺少分支锚点：{needle}");
    }
    // A10 的实例状态口径（免审直过时实例 APPROVED、单据也 APPROVED）
    assert!(sql.contains("biz_flow_instance"), "A10 必须查审批实例");
}

#[test]
fn selftest_injects_one_break_per_assertion() {
    let sh = read_repo(SELFTEST);
    // 目标断言清单必须恰好是 A1~A10（少一条 = 某条断言的牙齿没人验）
    let line = sh
        .lines()
        .find(|line| line.trim_start().starts_with("TARGETS=("))
        .expect("造错自检里找不到 TARGETS=(...) 目标断言清单");
    for no in 1..=10 {
        assert!(line.contains(&format!("A{no}")), "造错自检的 TARGETS 缺 A{no}：{line}");
    }
    // 十条注入 + 一条 F1 fail-closed
    assert!(sh.contains("MUTS=("), "造错自检里找不到 MUTS=(...) 注入清单");
    assert!(
        sh.contains("fail_closed"),
        "造错自检必须验「裸跑 sql 有违规就非 0 退出」（否则巡检可能静默假绿）"
    );
    // 护栏：造错自检会写数据，必须在任何 DML 之前挡住非演练库
    let guard = sh.find("hr_require_drill_db").expect("造错自检缺演练库护栏");
    let first_round = sh.find("round R0").expect("造错自检缺 R0 基线轮");
    assert!(guard < first_round, "演练库护栏必须写在第一轮之前");
}

#[test]
fn fixture_covers_the_whole_document_chain() {
    let sql = read_repo(FIXTURE);
    // 基线要能同时喂饱 10 条断言 ⇒ 员工 / 合同 / 标准 / 考勤 / 请假 / 薪资单 / 凭证 全链路
    for table in [
        "sys_user",
        "biz_employee",
        "biz_employee_contract",
        "biz_employee_salary",
        "biz_attendance_period",
        "biz_attendance_record",
        "biz_flow_instance",
        "biz_leave_request",
        "biz_salary_sheet",
        "biz_salary_detail",
        "biz_salary_detail_item",
        "biz_voucher",
        "biz_voucher_entry",
    ] {
        assert!(
            sql.contains(&format!("INSERT INTO {table}")),
            "夹具没有造 {table}：A1~A10 里至少有一条会因此假绿（0 行 = 无违规）"
        );
    }
    assert!(
        sql.contains("PAYROLL_ACCRUAL") && sql.contains("PAYROLL_PAYMENT"),
        "夹具的两张凭证必须按 PAYROLL_ACCRUAL / PAYROLL_PAYMENT 造，否则 A8 测不到来源类型"
    );
}

#[test]
fn selfcheck_migration_is_pure_readonly_do_block() {
    let raw = read_repo(SELFCHECK_UP);
    let code = strip_line_comments(&raw);
    let upper = code.to_uppercase();

    // 收口自检的价值就在「原子回滚」：它自己绝不能写东西，否则回滚后状态不干净。
    for banned in [
        "INSERT INTO",
        "UPDATE ",
        "DELETE FROM",
        "ALTER TABLE",
        "CREATE TABLE",
        "CREATE INDEX",
        "TRUNCATE",
        "DROP ",
    ] {
        assert!(
            !upper.contains(banned),
            "V0398 收口自检里出现了 `{banned}` —— 它必须是纯只读断言（写种子是 V0383~V0397 的事）"
        );
    }
    // 只有一个 DO 块，且缺项时 RAISE EXCEPTION（缺一即红，不是 NOTICE 放行）
    assert_eq!(
        upper.matches("DO $$").count(),
        1,
        "V0398 应当只有一个 DO 块（多段断言请并进同一个块，方便一次性列出全部缺项）"
    );
    assert!(
        upper.contains("RAISE EXCEPTION 'V0398 人力资源收口自检失败"),
        "V0398 缺项时必须 RAISE EXCEPTION，不能用 NOTICE 放行"
    );
    // 8 组断言的在位口径都要有锚点（10 表 / 科目 / 号段+字典 / 菜单 / 脱敏+配置 / 流程 / 薪资项）
    for anchor in [
        "biz_salary_detail_item",
        "biz_voucher_rule",
        "sys_dict_type",
        "business:hr:",
        "HR_MANAGER",
        "sys_field_mask",
        "biz_flow_def",
        "biz_salary_item",
    ] {
        assert!(raw.contains(anchor), "V0398 缺少断言锚点：{anchor}");
    }
}

#[test]
fn selfcheck_migration_has_a_down_and_a_unique_version() {
    let down = read_repo(SELFCHECK_DOWN);
    assert!(
        down.contains("DO $$"),
        "down 必须是一个可执行的 DO 块（迁移器要读得到它），不能是空文件"
    );

    // 版本号跨目录全局唯一（nik 的加载器对重号直接启动失败）：扫一遍所有迁移目录，
    // V0398 只许有 up/down 这一对。
    let mut found: Vec<String> = Vec::new();
    let mroot = repo_root().join("backend/migrations");
    let mut stack = vec![mroot];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("V0398__") {
                found.push(name);
            }
        }
    }
    found.sort();
    assert_eq!(
        found,
        vec![
            "V0398__hr_selfcheck.down.sql".to_string(),
            "V0398__hr_selfcheck.up.sql".to_string(),
        ],
        "V0398 必须恰好是 hr_selfcheck 的 up/down 一对（重号会让服务启动直接失败）"
    );
}
