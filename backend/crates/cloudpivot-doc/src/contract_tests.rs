//! 词表 ↔ 仓库内其它**文本真相**的契约（P2-29 从应用 crate 的 `doc_kind_tests.rs` 搬来）。
//!
//! 这些用例**只依赖本 crate + 仓库里的文件**（读文件用 `std::fs`，不加任何依赖），
//! 不 import `business::erp::*`，所以它们跟着词表走：
//! 「改了枚举忘了改前端镜像 / 演示数据脚本 / 巡检 SQL」在本 crate 就会红，
//! 而不是要跑整个应用 crate 的测试才发现。
//!
//! | 被钉住的另一端 | 用例 |
//! |---|---|
//! | 前端 `frontend/src/api/business/erp/doc-kinds.ts`（数组 + 中文标签） | [`frontend_ts_mirror_matches_rust`] / [`attachment_doc_types_match_rust`] / [`frontend_ts_labels_match_rust`] |
//! | `scripts/demo_seed.sh`（手写凭证来源 + ⑩ 自检白名单） | [`demo_seed_voucher_sources_are_enum_values`] / [`demo_seed_source_guard_matches_enum`] |
//! | `docs/architecture/snapshot-drift/10_checks.sql`（`di.v_poly_ref_map` 登记表） | [`serial_source_map_matches_snapshot_registry`] / [`doc_kind_registries_match_doc_kind_table`] / [`txn_doc_map_matches_snapshot_registry`] / [`voucher_source_map_matches_snapshot_registry`] / [`flow_cc_source_map_matches_snapshot_registry`] |
//! | Python 巡检 `biz_type_domain.py::is_test_residue` 与 SQL 片段 | [`test_residue_predicate_matches_python_and_sql`] |
//!
//! 留在**应用 crate** 的是另外两条「应用内」约束（它们必须 import 应用代码，
//! 塞进本 crate 会把 `shared → erp` 的回边重新拉回来）：
//!
//! - `backend/src/business/shared/doc_kind_entity_tests.rs` —— `state_machine` 实体注册表 ∈ `DocKind` 域；
//! - `backend/src/business/erp/flow_biz_type_contract_tests.rs` —— 各 `*_bridge::BIZ_TYPE` /
//!   `approval_gate::FLOW_TYPES` ∈ `FlowBizType` 域。

use super::*;

/// 仓库根（`backend/` 的上一级），用于读前端镜像 / 脚本 / 巡检 SQL。
///
/// 本 crate 在 `backend/crates/cloudpivot-doc/`，所以仓库根是本文件之上的第 4 层
/// （`ancestors()[0]` 是 crate 自己）：crate → `crates` → `backend` → 仓库根。
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("`backend/crates/cloudpivot-doc` 之上应有三层（crates → backend → 仓库根）")
        .to_path_buf()
}

/// 读前端镜像文件全文。
fn read_frontend_mirror() -> String {
    let path = repo_root().join("frontend/src/api/business/erp/doc-kinds.ts");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到前端词表镜像 {}：{err}", path.display()))
}

/// 从前端 TS 里抠出 `export const NAME = [ ... ] as const;` 的字符串元素（保序）。
fn ts_array(text: &str, name: &str) -> Vec<String> {
    let marker = format!("export const {name} = [");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("前端镜像里找不到 {marker}"))
        + marker.len();
    let rest = &text[start..];
    let end = rest.find(']').unwrap_or_else(|| panic!("{name} 的数组没有闭合"));
    rest[..end]
        .split(',')
        .map(|item| item.trim().trim_matches('\'').trim())
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// 从前端 TS 的 `export const NAME: ... = { KEY: '值', ... };` 里抠出 `KEY → 值`。
fn ts_map(text: &str, name: &str) -> Vec<(String, String)> {
    let marker = format!("export const {name}");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("前端镜像里找不到 {marker}"));
    let rest = &text[start..];
    let open = rest.find('{').expect("标签表必须有 {");
    let close = rest[open..].find("};").expect("标签表必须以 }; 结束") + open;
    rest[open + 1..close]
        .lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(',');
            let (key, value) = line.split_once(':')?;
            let key = key.trim();
            let value = value.trim();
            if !value.starts_with('\'') {
                return None;
            }
            Some((key.to_string(), value.trim_matches('\'').trim().to_string()))
        })
        .collect()
}

fn assert_mirror(name: &str, ts_name: &str, rust: &[&str]) {
    let text = read_frontend_mirror();
    let ts = ts_array(&text, ts_name);
    let ts_ref: Vec<&str> = ts.iter().map(String::as_str).collect();
    assert_eq!(
        ts_ref, rust,
        "前端 {ts_name} 与 Rust {name} 不一致（顺序也要一致）—— \
         改了后端枚举就必须同步 frontend/src/api/business/erp/doc-kinds.ts"
    );
}

// ==================== 前端镜像 ====================

/// 前端 `doc-kinds.ts` 的七根轴数组与 Rust 枚举逐字一致（顺序也一致）。
#[test]
fn frontend_ts_mirror_matches_rust() {
    assert_mirror("DocKind::ALL", "DOC_KINDS", &DocKind::ALL.map(DocKind::as_str));
    assert_mirror(
        "FlowBizType::ALL",
        "FLOW_BIZ_TYPES",
        &FlowBizType::ALL.map(FlowBizType::as_str),
    );
    assert_mirror(
        "TxnBizType::ALL",
        "TXN_BIZ_TYPES",
        &TxnBizType::ALL.map(TxnBizType::as_str),
    );
    assert_mirror(
        "TxnDocType::ALL",
        "TXN_DOC_TYPES",
        &TxnDocType::ALL.map(TxnDocType::as_str),
    );
    assert_mirror(
        "VoucherSourceType::ALL",
        "VOUCHER_SOURCE_TYPES",
        &VoucherSourceType::ALL.map(VoucherSourceType::as_str),
    );
    assert_mirror(
        "SerialSourceType::ALL",
        "SERIAL_SOURCE_TYPES",
        &SerialSourceType::ALL.map(SerialSourceType::as_str),
    );
    assert_mirror(
        "DocSubtype::IN_TYPES",
        "IN_TYPES",
        &DocSubtype::IN_TYPES.map(DocSubtype::as_str),
    );
    assert_mirror(
        "DocSubtype::OUT_TYPES",
        "OUT_TYPES",
        &DocSubtype::OUT_TYPES.map(DocSubtype::as_str),
    );
    assert_mirror("TakeType::ALL", "TAKE_TYPES", &TakeType::ALL.map(TakeType::as_str));
}

/// 前端 `ATTACHMENT_DOC_TYPES` 与 `ATTACHMENT_DOC_KINDS` 逐字一致（顺序也一致）。
#[test]
fn attachment_doc_types_match_rust() {
    assert_mirror(
        "ATTACHMENT_DOC_KINDS",
        "ATTACHMENT_DOC_TYPES",
        &ATTACHMENT_DOC_KINDS.map(DocKind::as_str),
    );
}

// ==================== 演示数据脚本（scripts/demo_seed.sh） ====================

/// 读演示数据脚本全文。
fn read_demo_seed() -> String {
    let path = repo_root().join("scripts/demo_seed.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到演示数据脚本 {}：{err}", path.display()))
}

/// 演示数据里**手写**的每个凭证来源都必须是 [`VoucherSourceType`] 的枚举值。
///
/// 事故背景：`demo_seed.sh` 曾把销售开票凭证写成 `source_type = 'RECEIVABLE'` —— 那是
/// 「业务对象轴」（`DocKind::Receivable`）的取值，不在凭证来源轴上，**没有任何生产路径会写它**。
/// 结果是演示库里出现商品永远不会产生的值：前端来源单据映射表认不出（落到「来源类型未识别」，
/// 凭证详情的来源单据不可点），拿 source_type 校验枚举的巡检/单测也会把它判成非法值。
///
/// 扫描口径：凭证 INSERT 的每一行形如 `… , 'AUTO', 'STOCK_IN', '<source_id>'`（手工凭证是
/// `'MANUAL', 'MANUAL', NULL`）⇒ 取 `'AUTO', '` / `'MANUAL', '` 之后的下一个引号内字面量。
#[test]
fn demo_seed_voucher_sources_are_enum_values() {
    let text = read_demo_seed();
    let allowed: Vec<&str> = VoucherSourceType::ALL.iter().map(|item| item.as_str()).collect();

    let mut found: Vec<String> = Vec::new();
    let mut bad: Vec<String> = Vec::new();
    for marker in ["'AUTO', '", "'MANUAL', '"] {
        let mut rest = text.as_str();
        while let Some(pos) = rest.find(marker) {
            let after = &rest[pos + marker.len()..];
            let Some(end) = after.find('\'') else { break };
            let token = &after[..end];
            if !allowed.contains(&token) {
                bad.push(token.to_string());
            } else if !found.iter().any(|item| item == token) {
                found.push(token.to_string());
            }
            rest = &after[end..];
        }
    }

    assert!(
        bad.is_empty(),
        "演示数据里出现了 VoucherSourceType 枚举外的凭证来源：{bad:?}。\
         凭证来源轴只认枚举值（VoucherSourceType::ALL）；业务对象轴的值（RECEIVABLE / PAYABLE / \
         SHIPMENT …）不是来源，别手写。"
    );
    // 防止扫描失效（脚本结构变了、标记对不上了却静默变绿）
    assert!(
        found.len() >= 6,
        "只扫到 {} 种凭证来源（{found:?}）—— 演示数据的凭证 INSERT 结构变了，请同步本用例的扫描口径",
        found.len()
    );
    for expected in ["STOCK_IN", "STOCK_OUT", "PAYMENT", "RECEIPT", "INVOICE", "MANUAL"] {
        assert!(
            found.iter().any(|item| item == expected),
            "演示数据少了 {expected} 来源的凭证"
        );
    }
}

/// 演示数据的 ⑩ 自检里那份「来源白名单」与枚举逐字一致（顺序也一致）。
///
/// 自检是最后一道闸：白名单比枚举旧，演示数据就能把枚举外的值灌进库而不报错；
/// 白名单比枚举新，则是自检在认可一个并不存在的枚举值。两边必须同步。
#[test]
fn demo_seed_source_guard_matches_enum() {
    let text = read_demo_seed();
    let marker = "v.source_type NOT IN (";
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("demo_seed.sh 的 ⑩ 自检里找不到 {marker} —— 白名单被删了？"))
        + marker.len();
    let close = text[start..].find(')').expect("demo_seed.sh 的来源白名单没有闭合的 )");
    let guard: Vec<&str> = text[start..start + close]
        .split(',')
        .map(|item| item.trim().trim_matches('\'').trim())
        .filter(|item| !item.is_empty())
        .collect();
    assert_eq!(
        guard,
        VoucherSourceType::ALL.map(VoucherSourceType::as_str).to_vec(),
        "demo_seed.sh ⑩ 自检的来源白名单与 VoucherSourceType::ALL 不一致（顺序也要一致）"
    );
}

/// 前端标签表与 Rust `label()` 逐条一致（中文文案也不许漂）。
#[test]
fn frontend_ts_labels_match_rust() {
    let text = read_frontend_mirror();
    fn check(pairs: Vec<(String, String)>, expected: Vec<(&str, &str)>, name: &str) {
        for (key, label) in expected {
            let hit = pairs
                .iter()
                .find(|(item, _)| item == key)
                .unwrap_or_else(|| panic!("{name} 里没有 {key}"));
            assert_eq!(hit.1, label, "前端 {name}[{key}] 与 Rust label() 不一致");
        }
    }
    check(
        ts_map(&text, "DOC_KIND_LABELS"),
        DocKind::ALL.iter().map(|item| (item.as_str(), item.label())).collect(),
        "DOC_KIND_LABELS",
    );
    check(
        ts_map(&text, "FLOW_BIZ_TYPE_LABELS"),
        FlowBizType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "FLOW_BIZ_TYPE_LABELS",
    );
    check(
        ts_map(&text, "TXN_BIZ_TYPE_LABELS"),
        TxnBizType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "TXN_BIZ_TYPE_LABELS",
    );
    check(
        ts_map(&text, "TXN_DOC_TYPE_LABELS"),
        TxnDocType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "TXN_DOC_TYPE_LABELS",
    );
    check(
        ts_map(&text, "VOUCHER_SOURCE_TYPE_LABELS"),
        VoucherSourceType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "VOUCHER_SOURCE_TYPE_LABELS",
    );
    check(
        ts_map(&text, "SERIAL_SOURCE_TYPE_LABELS"),
        SerialSourceType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "SERIAL_SOURCE_TYPE_LABELS",
    );
}

// ==================== 测试残留口径（t_61dca492） ====================

/// [`is_test_residue`] 的判据必须与 Python 巡检（`biz_type_domain.py::is_test_residue`）逐字一致。
///
/// 三边（Rust / Python / SQL 片段）任何一边改了拼写，这条就会红 —— 目的就是逼人同步改，
/// 而不是又一次「第 8 套词表」。
#[test]
fn test_residue_predicate_matches_python_and_sql() {
    // 真阳性：e2e 脚本写进生产库的那两种形态
    assert!(is_test_residue("E2E_AP_E2EMVUARCAT"));
    assert!(is_test_residue("E2E_"));
    assert!(is_test_residue("D2_SMOKE"));
    // 真阴性：内置流程 / 客户自定义流程 / 形似但不是
    for raw in ["PO_APPROVE", "TASK_ACCEPT", "OTHER", "E2EX", "e2e_ap_x", "D2_SMOKE_X"] {
        assert!(!is_test_residue(raw), "{raw} 不该被当成测试残留");
    }
    // SQL 片段与函数同口径（前缀 4 字符 + 精确值）
    assert_eq!(
        TEST_RESIDUE_SQL,
        "(left(biz_type, 4) = 'E2E_' OR biz_type = 'D2_SMOKE')"
    );
    assert_eq!(
        TEST_RESIDUE_PREFIX.len(),
        4,
        "SQL 里用的是 left(biz_type, 4)，前缀长度必须也是 4"
    );
    // 内置词表里绝不能有测试残留值（否则「删残留」会把内置流程一起删掉）
    for item in COLUMN_DOMAINS {
        for value in item.values {
            assert!(
                !is_test_residue(value),
                "{}.{} 的词表里出现了测试残留值 {value}",
                item.table,
                item.column
            );
        }
    }
}

// ==================== 巡检登记表（di.v_poly_ref_map）机械核对 ====================

/// 读巡检 SQL（`docs/architecture/snapshot-drift/10_checks.sql`）。
///
/// 登记的「类型 → 目标表」在这份 SQL 里写了一遍、在 Rust 枚举里又写了一遍 ——
/// 两份不一致时，代码会写一种映射、巡检按另一种查（**静默漏报**）。
/// 所以这里把 SQL 当数据解析，逐行与 Rust 比对（P2-6b）。
fn read_snapshot_checks_sql() -> String {
    let path = repo_root().join("docs/architecture/snapshot-drift/10_checks.sql");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到巡检 SQL {}：{err}", path.display()))
}

/// 从一个 VALUES 行里抠出前几个字段（`'字符串'` 或裸 `NULL`）。
fn scan_sql_tuple(text: &str) -> Vec<Option<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() && out.len() < 4 {
        match chars[i] {
            '\'' => {
                let mut buf = String::new();
                let mut j = i + 1;
                while j < chars.len() {
                    if chars[j] == '\'' {
                        // `''` 是转义的单引号
                        if j + 1 < chars.len() && chars[j + 1] == '\'' {
                            buf.push('\'');
                            j += 2;
                            continue;
                        }
                        break;
                    }
                    buf.push(chars[j]);
                    j += 1;
                }
                out.push(Some(buf));
                i = j + 1;
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let mut buf = String::new();
                let mut j = i;
                while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                    buf.push(chars[j]);
                    j += 1;
                }
                out.push(if buf == "NULL" { None } else { Some(buf) });
                i = j;
            }
            _ => i += 1,
        }
    }
    out
}

/// 登记表里某个域的全部 (类型, 目标表) 行。
fn registry_rows(sql: &str, domain: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for line in sql.lines() {
        let text = line.trim();
        if !text.starts_with("('") {
            continue;
        }
        let fields = scan_sql_tuple(text);
        if fields.len() < 3 {
            continue;
        }
        if fields[0].as_deref() != Some(domain) {
            continue;
        }
        out.push((fields[1].clone().unwrap_or_default(), fields[2].clone()));
    }
    out
}

/// 序列号来源的「类型 → 目标表」：Rust 与巡检登记表逐行一致。
#[test]
fn serial_source_map_matches_snapshot_registry() {
    let sql = read_snapshot_checks_sql();
    let rows = registry_rows(&sql, "SERIAL_SOURCE");
    assert_eq!(
        rows.len(),
        SerialSourceType::ALL.len(),
        "di.v_poly_ref_map 的 SERIAL_SOURCE 行数必须 = 枚举成员数（实际 {rows:?}）"
    );
    for kind in SerialSourceType::ALL {
        let row = rows
            .iter()
            .find(|(ref_type, _)| ref_type == kind.as_str())
            .unwrap_or_else(|| panic!("登记表缺 SERIAL_SOURCE.{}", kind.as_str()));
        assert_eq!(
            row.1.as_deref(),
            kind.target_table(),
            "SERIAL_SOURCE.{} 的目标表：Rust={:?} / SQL={:?}",
            kind.as_str(),
            kind.target_table(),
            row.1
        );
    }
}

/// 单据类型轴的三个 `doc_type` 列（附件 / 提醒 / 状态日志）：登记表 = `DocKind::table()`。
#[test]
fn doc_kind_registries_match_doc_kind_table() {
    let sql = read_snapshot_checks_sql();
    let cases: [(&str, &[DocKind]); 3] = [
        ("ATTACHMENT_DOC", &ATTACHMENT_DOC_KINDS),
        ("REMINDER_DOC", &REMINDER_DOC_KINDS),
        ("STATUS_ENTITY", &DocKind::ALL),
    ];
    for (domain, kinds) in cases {
        let rows = registry_rows(&sql, domain);
        // 登记表必须**覆盖该域的全部取值**（无实体表的写 NULL 也要登记一行，
        // 否则 DI-22 会把「没登记」当成漏报 —— ACCOUNTING_YEAR 就是这种虚拟对象）。
        let expected: Vec<(String, Option<String>)> = kinds
            .iter()
            .map(|kind| (kind.as_str().to_string(), kind.table().map(str::to_string)))
            .collect();
        assert_eq!(
            rows.len(),
            expected.len(),
            "{domain} 登记行数不符（SQL {rows:?} / Rust {expected:?}）"
        );
        for (ref_type, table) in &expected {
            let row = rows
                .iter()
                .find(|(registered, _)| registered == ref_type)
                .unwrap_or_else(|| panic!("登记表缺 {domain}.{ref_type}"));
            assert_eq!(
                row.1.as_deref(),
                table.as_deref(),
                "{domain}.{ref_type} 的目标表：Rust={table:?} / SQL={:?}",
                row.1
            );
        }
    }
}

/// `TXN_DOC` 登记域：`doc_type` → 目标表（测试侧的期望，与 SQL 登记表逐行核对）。
///
/// 未知取值直接 panic —— 这是给「新增了 `TxnDocType` 却忘了往巡检登记表补一行」
/// 设的绊索：`vocab_tests::domain_consts_match_enums` 只管 Rust 内部一致，这里管 Rust ↔ 巡检。
fn txn_doc_target_table(value: &str) -> Option<&'static str> {
    Some(match value {
        "STOCK_IN" => "biz_stock_in",
        "STOCK_OUT" => "biz_stock_out",
        "STOCK_TRANSFER" => "biz_stock_transfer",
        "STOCK_TAKE" => "biz_stock_take",
        "SERIAL_BATCH" | "MATERIAL_SERIAL" => "biz_material_serial",
        "OPENING" | "ADJUST" => return None,
        other => {
            panic!("TXN_DOC 取值 {other} 没登记目标表：改了 TxnDocType 要同步 di.v_poly_ref_map")
        }
    })
}

/// `VOUCHER_SOURCE` 登记域：`source_type` → 目标表（报销两值都挂在报销单上）。
fn voucher_source_target_table(value: &str) -> Option<&'static str> {
    Some(match value {
        "STOCK_IN" => "biz_stock_in",
        "STOCK_OUT" => "biz_stock_out",
        "PAYMENT" => "biz_payment",
        "STOCK_TAKE" => "biz_stock_take",
        "SERIAL_BATCH" | "SERIAL_OUT" | "SERIAL_SCRAP" => "biz_material_serial",
        "OPENING" | "YEAR_END" | "MANUAL" => return None,
        "FIXED_ASSET_ACQ" | "FIXED_ASSET_DISPOSAL" => "biz_fixed_asset",
        "DEPRECIATION" => "biz_depreciation_batch",
        "EXPENSE_REPORT" | "EXPENSE_PAYMENT" => "biz_expense_report",
        // 销售链 SO-17：收款单过账凭证挂在收款单上
        "RECEIPT" => "biz_receipt",
        // 销售链 SO-14 遗留卡：收款退款过账凭证挂在退款单上
        "RECEIPT_REFUND" => "biz_receipt_refund",
        // 销售链 SO-20：发票开票的收入凭证（含作废红冲）挂在发票上
        "INVOICE" => "biz_invoice",
        // 销售链 SO-36：佣金计提 / 发放两张凭证都挂在结算单上
        "COMMISSION" => "biz_commission_settlement",
        // 成本核算（ARCH-9）：结算批次的 3–4 张凭证都挂在结算批次上
        "COST_PERIOD" => "biz_cost_period",
        // 人力资源（ARCH-9-2 §4.6）：工资计提 / 工资发放两张凭证都挂在**同一张薪资单**上
        // （source_id = biz_salary_sheet.id），见 HR-06 / `VoucherSourceType::doc_kind`。
        "PAYROLL_ACCRUAL" | "PAYROLL_PAYMENT" => "biz_salary_sheet",
        other => panic!("VOUCHER_SOURCE 取值 {other} 没登记目标表：改了 VoucherSourceType 要同步 di.v_poly_ref_map"),
    })
}

/// 库存流水来源单据（`biz_inventory_txn.doc_type` + `doc_id`）：登记表 = `TxnDocType::ALL` 的落库字面量。
///
/// 与 [`doc_kind_registries_match_doc_kind_table`] 同一把尺子：登记表漏一行 ⇒
/// DI-22 报「覆盖缺口」、DI-18 根本查不到这类流水的悬空（**静默漏报**，比漏数据更危险）。
/// 本域多一个**遗留值** `PURCHASE_ORDER`（`TxnDocType` 已无此值，存量直接挂采购单），
/// 所以期望行 = 枚举成员 + 该遗留值。
#[test]
fn txn_doc_map_matches_snapshot_registry() {
    let sql = read_snapshot_checks_sql();
    let rows = registry_rows(&sql, "TXN_DOC");
    // 期望行由**枚举**推导（P2-27 起私有常量 `TXN_DOC_TYPE_VALUES` 留在词表 crate 内部；
    // 它与 `TxnDocType::ALL` 逐字同序，由 `vocab_tests::domain_consts_match_enums` 钉住）
    let mut expected: Vec<(String, Option<String>)> = TxnDocType::ALL
        .iter()
        .map(|item| {
            (
                item.as_str().to_string(),
                txn_doc_target_table(item.as_str()).map(str::to_string),
            )
        })
        .collect();
    expected.push(("PURCHASE_ORDER".to_string(), Some("biz_purchase_order".to_string())));
    assert_eq!(
        rows.len(),
        expected.len(),
        "TXN_DOC 登记行数不符（SQL {rows:?} / 期望 {expected:?}）"
    );
    for (ref_type, table) in &expected {
        let row = rows
            .iter()
            .find(|(registered, _)| registered == ref_type)
            .unwrap_or_else(|| panic!("登记表缺 TXN_DOC.{ref_type}"));
        assert_eq!(
            row.1.as_deref(),
            table.as_deref(),
            "TXN_DOC.{ref_type} 的目标表：SQL={:?} / 期望={table:?}",
            row.1
        );
    }
}

/// 凭证来源（`biz_voucher.source_type` + `source_id`）：登记表 = `VoucherSourceType::ALL` 的落库字面量。
///
/// 漏登记一行 = 这类凭证的悬空永远不被 DI-17 看到；FIN-3 报销的
/// `EXPENSE_REPORT` / `EXPENSE_PAYMENT` 就是曾漏掉的两个值。
#[test]
fn voucher_source_map_matches_snapshot_registry() {
    let sql = read_snapshot_checks_sql();
    let rows = registry_rows(&sql, "VOUCHER_SOURCE");
    // 同 txn_doc：期望行由枚举推导（私有常量 `VOUCHER_SOURCE_VALUES` 归词表 crate，
    // 与 `VoucherSourceType::ALL` 的同序等价由 `vocab_tests::domain_consts_match_enums` 钉住）
    let expected: Vec<(String, Option<String>)> = VoucherSourceType::ALL
        .iter()
        .map(|item| {
            (
                item.as_str().to_string(),
                voucher_source_target_table(item.as_str()).map(str::to_string),
            )
        })
        .collect();
    assert_eq!(
        rows.len(),
        expected.len(),
        "VOUCHER_SOURCE 登记行数不符（SQL {rows:?} / 期望 {expected:?}）"
    );
    for (ref_type, table) in &expected {
        let row = rows
            .iter()
            .find(|(registered, _)| registered == ref_type)
            .unwrap_or_else(|| panic!("登记表缺 VOUCHER_SOURCE.{ref_type}"));
        assert_eq!(
            row.1.as_deref(),
            table.as_deref(),
            "VOUCHER_SOURCE.{ref_type} 的目标表：SQL={:?} / 期望={table:?}",
            row.1
        );
    }
}

/// 抄送来源（`biz_flow_cc.source_type` + `source_ref`）：只有角色/部门会带 ref。
#[test]
fn flow_cc_source_map_matches_snapshot_registry() {
    let sql = read_snapshot_checks_sql();
    let rows = registry_rows(&sql, "FLOW_CC_SOURCE");
    // ROLE / ADMIN → sys_role；DEPT_LEADER / INITIATOR_SUPERIOR → sys_dept；
    // 其余来源（USER / INITIATOR / TASK_* / FORM_VAR / EXPRESSION）不带 source_ref。
    let expected = [
        ("ROLE", Some("sys_role")),
        ("ADMIN", Some("sys_role")),
        ("DEPT_LEADER", Some("sys_dept")),
        ("INITIATOR_SUPERIOR", Some("sys_dept")),
        ("USER", None),
        ("INITIATOR", None),
        ("TASK_OWNER", None),
        ("TASK_CREATOR", None),
        ("TASK_ASSIGNER", None),
        ("FORM_VAR", None),
        ("EXPRESSION", None),
    ];
    assert_eq!(rows.len(), expected.len(), "FLOW_CC_SOURCE 登记行数不符：{rows:?}");
    for (ref_type, table) in expected {
        let row = rows
            .iter()
            .find(|(registered, _)| registered == ref_type)
            .unwrap_or_else(|| panic!("登记表缺 FLOW_CC_SOURCE.{ref_type}"));
        assert_eq!(
            row.1.as_deref(),
            table,
            "FLOW_CC_SOURCE.{ref_type} 的目标表：SQL={:?} / 期望={table:?}",
            row.1
        );
    }
}
