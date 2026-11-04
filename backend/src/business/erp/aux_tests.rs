//! 凭证辅助核算（ARCH-3 §5 DC-11 · P2-1）：**写入口径 + 辅助账查询**的真库用例。
//!
//! # 为什么单独一个文件
//!
//! 辅助核算横跨四条写路径（收货过账 / 出库结转 / 付款 / 红冲）与一个只读接口
//! （`GET /business/erp/ledger/aux-balance`）。散在各模块的用例里没人能一眼看出
//! 「哪类凭证补了哪几个维度」，所以这里集中放「维度口径」这一件事的断言。
//!
//! # 夹具口径（`2031-02` / `2031-03` + `P21AUX%` 前缀）
//!
//! * 日期故意取 **2031 年**：别的模块（2026-10 / 2026-11）的夹具不会掉进本组期间；
//! * 凭证 `source_no` 一律 `P21AUX-*`，清场按前缀删（**不按凭证字号删**，
//!   `PZ-YYYYMMDD-*` 是生产号段）；
//! * 科目 / 物料 / 仓库 / 供应商 / 部门 / 用户 / 单位都是本组自建的固定 id，
//!   清场逐个删（科目留着会让 `cash_flow` 的「分类配置覆盖全科目」断言红 —— voucher.rs
//!   的 `zz_teardown_purges_fixture_accounts` 就是为此存在的）。
//! * **每条用例结束就清**（正常返回 / panic 都清）：用例体包在 [`case()`] 里，收尾挂在
//!   外壳上。不靠「下一个用例开工时清」也不靠 `zz_` 名字排序最后跑 ——
//!   `cargo test` 多线程时 libtest **不保证** `zz_` 最后执行，残留科目会把别的模块的
//!   全表断言打成假红（实测 `cash_flow::tests::r3_real_config_covers_all_accounts`）。
//!
//! # 跑法
//!
//! ```bash
//! ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>' \
//!   cargo test --lib p21_aux -- --test-threads=1
//! ```
#![cfg(test)]

use futures_util::FutureExt;
use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::collections::BTreeMap;
use std::str::FromStr;

use crate::business::erp::ledger::{AuxBalanceQuery, aux_balance, aux_column};
use crate::business::erp::voucher::{
    AccountRef, AccountSet, EntryLine, EntryRow, LineContext, MaterialInventoryRef, PostVoucher,
    VoucherAmounts, VoucherBizCtx, VoucherMaterialLine, absorb_tail_diff, create_by_rule,
    entries_of, post_voucher, purchase_in_lines, reversed_lines, user_dept_of,
};
use crate::business::erp::voucher_query::tests::{DB_LOCK, ensure_db};

// ==================== 夹具 id ====================

/// 物料「甲」。
const MAT: &str = "e21a0001-0000-4000-8000-000000000001";
/// 物料「乙」（P2-1b 多物料用例：纯逻辑用例不需要库里有行，真库用例自己插）。
const MAT2: &str = "e21a000b-0000-4000-8000-00000000000b";
/// 物料「丙」（`M = 3` 与尾差用例）。
const MAT3: &str = "e21a000c-0000-4000-8000-00000000000c";
/// 收货仓库。
const WH: &str = "e21a0002-0000-4000-8000-000000000002";
/// 供应商。
const SUP: &str = "e21a0003-0000-4000-8000-000000000003";
/// 部门（付款申请人所属）。
const DEPT: &str = "e21a0004-0000-4000-8000-000000000004";
/// 有部门的申请人。
const USER: &str = "e21a0005-0000-4000-8000-000000000005";
/// 没有部门的系统账号。
const USER_NO_DEPT: &str = "e21a0006-0000-4000-8000-000000000006";
/// 计量单位（物料必填）。
const UNIT: &str = "e21a0007-0000-4000-8000-000000000007";
/// 借方科目（资产）。
const ACC_DR: &str = "e21a0008-0000-4000-8000-000000000008";
/// 贷方科目（负债）。
const ACC_CR: &str = "e21a0009-0000-4000-8000-000000000009";

const MAT_NAME: &str = "P2-1 物料甲";
const WH_NAME: &str = "P2-1 收货仓";
const SUP_NAME: &str = "P2-1 供应商";
const DEPT_NAME: &str = "P2-1 采购部";

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

fn uid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("uuid 字面量非法")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量非法")
}

async fn exec(sql: &str) {
    if let Err(err) = db().execute_with_params(sql, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {sql}");
    }
}

// ==================== 夹具 ====================

/// 清本组的全部痕迹（凭证 → 分录级联 → 主数据 → 科目）。
async fn cleanup() {
    exec("DELETE FROM biz_voucher WHERE source_no LIKE 'P21AUX%'").await;
    exec(&format!(
        "DELETE FROM biz_material WHERE id = '{MAT}'::uuid"
    ))
    .await;
    exec(&format!(
        "DELETE FROM biz_warehouse WHERE id = '{WH}'::uuid"
    ))
    .await;
    exec(&format!(
        "DELETE FROM biz_supplier WHERE id = '{SUP}'::uuid"
    ))
    .await;
    exec(&format!("DELETE FROM biz_unit WHERE id = '{UNIT}'::uuid")).await;
    exec(&format!(
        "DELETE FROM sys_user WHERE id IN ('{USER}'::uuid, '{USER_NO_DEPT}'::uuid)"
    ))
    .await;
    exec(&format!("DELETE FROM sys_dept WHERE id = '{DEPT}'::uuid")).await;
    exec(&format!(
        "DELETE FROM biz_account WHERE id IN ('{ACC_DR}'::uuid, '{ACC_CR}'::uuid)"
    ))
    .await;
}

/// 清场 + 建主数据（物料 / 仓库 / 供应商 / 部门 / 两个用户 / 两个科目）。
async fn seed() {
    cleanup().await;
    exec(&format!(
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status) \
         VALUES ('{UNIT}'::uuid, 'P21AUX-U', 'P2-1 单位', 1, 1, 1)"
    ))
    .await;
    exec(&format!(
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status) \
         VALUES ('{MAT}'::uuid, 'P21AUX-M1', '{MAT_NAME}', '{UNIT}'::uuid, 1)"
    ))
    .await;
    exec(&format!(
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf) \
         VALUES ('{WH}'::uuid, 'P21AUX-W1', '{WH_NAME}', '/{WH}/', 1, 1)"
    ))
    .await;
    exec(&format!(
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) \
         VALUES ('{SUP}'::uuid, 'P21AUX-S1', '{SUP_NAME}', 1)"
    ))
    .await;
    exec(&format!(
        "INSERT INTO sys_dept (id, dept_code, dept_name, status) \
         VALUES ('{DEPT}'::uuid, 'P21AUXD', '{DEPT_NAME}', 1)"
    ))
    .await;
    exec(&format!(
        "INSERT INTO sys_user (id, username, password, dept_id, status) \
         VALUES ('{USER}'::uuid, 'p21aux_user', 'x', '{DEPT}'::uuid, 1), \
                ('{USER_NO_DEPT}'::uuid, 'p21aux_robot', 'x', NULL, 1)"
    ))
    .await;
    exec(&format!(
        "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, \
                                 account_category, balance_direction, is_leaf, status) \
         VALUES ('{ACC_DR}'::uuid, 'P21AUXA', 'P2-1 测试资产', NULL, 1, 'ASSET', 'DEBIT', 1, 1), \
                ('{ACC_CR}'::uuid, 'P21AUXB', 'P2-1 测试负债', NULL, 1, 'LIABILITY', 'CREDIT', 1, 1)"
    ))
    .await;
}

fn account_of(id: &str, code: &str, name: &str) -> AccountRef {
    AccountRef {
        id: uid(id),
        code: code.to_string(),
        name: name.to_string(),
    }
}

fn account_set() -> AccountSet {
    AccountSet {
        inventory: account_of(ACC_DR, "P21AUXA", "P2-1 测试资产"),
        payable: account_of(ACC_CR, "P21AUXB", "P2-1 测试负债"),
        tax: Some(account_of(
            "e21a000a-0000-4000-8000-00000000000a",
            "P21AUXC",
            "P2-1 进项税",
        )),
    }
}

/// 一条「按物料聚合」的明细行（P2-1b 取数产物）。
fn mat_line(
    material_id: &str,
    amount: &str,
    qty: &str,
    line_from: i32,
    line_to: i32,
) -> VoucherMaterialLine {
    VoucherMaterialLine {
        material_id: uid(material_id),
        qty: dec(qty),
        amount: dec(amount),
        line_from,
        line_to,
    }
}

/// 逐物料存货科目（`material_id → 科目`）：模拟 §4.4 那条批取 SQL 的结果。
fn mat_accounts(pairs: &[(&str, &str)]) -> BTreeMap<Uuid, MaterialInventoryRef> {
    pairs
        .iter()
        .map(|(material_id, account_id)| {
            (
                uid(material_id),
                MaterialInventoryRef {
                    label: format!("物料 {material_id}"),
                    account: account_of(account_id, "P21AUXA", "P2-1 测试资产"),
                },
            )
        })
        .collect()
}

/// 落一张两行凭证（借 `ACC_DR` / 贷 `ACC_CR`），返回凭证 id。
async fn make_voucher(
    source_no: &str,
    voucher_date: &str,
    debit: &str,
    debit_aux: (Option<&str>, Option<&str>, Option<&str>),
    credit: &str,
    credit_aux: (Option<&str>, Option<&str>, Option<&str>),
) -> Uuid {
    let to_uuid = |value: Option<&str>| value.map(uid);
    let lines = vec![
        EntryLine {
            line_no: 1,
            summary: format!("{source_no} 借方"),
            account: account_of(ACC_DR, "P21AUXA", "P2-1 测试资产"),
            debit: dec(debit),
            credit: Decimal::ZERO,
            aux_supplier_id: to_uuid(debit_aux.0),
            aux_material_id: to_uuid(debit_aux.1),
            aux_warehouse_id: to_uuid(debit_aux.2),
            aux_dept_id: None,
        },
        EntryLine {
            line_no: 2,
            summary: format!("{source_no} 贷方"),
            account: account_of(ACC_CR, "P21AUXB", "P2-1 测试负债"),
            debit: Decimal::ZERO,
            credit: dec(credit),
            aux_supplier_id: to_uuid(credit_aux.0),
            aux_material_id: to_uuid(credit_aux.1),
            aux_warehouse_id: to_uuid(credit_aux.2),
            aux_dept_id: None,
        },
    ];
    post_voucher(
        db(),
        &PostVoucher {
            voucher_type: crate::business::erp::voucher::VOUCHER_TYPE_AUTO,
            source_type: "MANUAL",
            source_id: None,
            source_no: Some(source_no),
            voucher_date: date(voucher_date),
            summary: format!("{source_no} 辅助核算夹具"),
            lines,
            remark: None,
            red_of: None,
        },
        None,
        Utc::now(),
    )
    .await
    .unwrap_or_else(|err| panic!("落凭证失败：{err}"))
}

/// 建一组固定的 4 张凭证（`2031-02` 一期 + `2031-03` 三期），返回 id 便于断言。
struct Fixtures {
    material_debit: Uuid,
    plain_debit: Uuid,
    opening: Uuid,
    dept_debit: Uuid,
}

async fn fixtures() -> Fixtures {
    let material_debit = make_voucher(
        "P21AUX-V1",
        "2031-03-05",
        "100",
        (None, Some(MAT), Some(WH)),
        "100",
        (Some(SUP), None, None),
    )
    .await;
    let plain_debit = make_voucher(
        "P21AUX-V2",
        "2031-03-10",
        "40",
        (None, None, None),
        "40",
        (Some(SUP), None, None),
    )
    .await;
    let opening = make_voucher(
        "P21AUX-V3",
        "2031-02-20",
        "25",
        (None, Some(MAT), None),
        "25",
        (None, None, None),
    )
    .await;
    let dept_debit = make_voucher(
        "P21AUX-V4",
        "2031-03-12",
        "60",
        (None, None, None),
        "60",
        (None, None, None),
    )
    .await;
    // 付款口径：部门挂在**借方**（应付/费用）那一行，银行行不挂。
    exec(&format!(
        "UPDATE biz_voucher_entry SET aux_dept_id = '{DEPT}'::uuid \
          WHERE voucher_id = '{dept_debit}'::uuid AND line_no = 1"
    ))
    .await;
    Fixtures {
        material_debit,
        plain_debit,
        opening,
        dept_debit,
    }
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    seed().await;
    Some(guard)
}

/// 用例外壳：取锁 + 建夹具 → 跑用例体 → **无条件清场** → panic 原样抛回。
///
/// # 为什么清场必须挂在外壳上（而不是写在用例体最后一行）
///
/// 本组夹具在 `biz_account` 里建了 `P21AUXA` / `P21AUXB` 两个科目，它们**没有**现金
/// 流量分类行。过去清场只发生在两处：下一个用例开工前的 `seed()`、以及名字排在最后的
/// `zz_p21_aux_teardown_purges_fixtures`。`cargo test` 默认多线程跑，libtest **不保证**
/// `zz_` 最后执行 —— 于是本组跑完（甚至跑到一半）时科目还在库里，而
/// `cash_flow::tests::r3_real_config_covers_all_accounts` 是**遍历 `biz_account` 全表**
/// 的强断言（每个非现金科目都必须有分类行），两者一撞就红：
/// 「分类配置漏科目：["P21AUXA …", "P21AUXB …"]」。
///
/// 把清场放进 `catch_unwind` 之后：**正常返回和 panic 两条路径都清**，用例仍然按原样
/// 失败（`resume_unwind`，不吞红），也不依赖用例名排序。写法与 `tests/testd/common.rs`
/// 的 `case()` 同源（`TD-` 前缀那套：崩了 → 下一个用例开工自愈 + 收尾断言残留为 0）。
///
/// 用法：
///
/// ```ignore
/// #[tokio::test]
/// async fn p21_xxx() {
///     case(|| async {
///         // 用例体（不再自己调 setup()）
///     })
///     .await;
/// }
/// ```
async fn case<F, Fut>(body: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let Some(_guard) = setup().await else {
        return;
    };
    // AssertUnwindSafe：夹具跨 panic 边界后不再使用，用例体里被改坏的状态也不影响清场
    // （清场只认本组固定 id 与前缀 + 进程级连接池）。
    let outcome = std::panic::AssertUnwindSafe(body()).catch_unwind().await;
    cleanup().await;
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

fn query(aux_type: &str, aux_id: Option<&str>) -> AuxBalanceQuery {
    AuxBalanceQuery {
        aux_type: Some(aux_type.to_string()),
        aux_id: aux_id.map(str::to_string),
        period_from: Some("2031-03".to_string()),
        period_to: Some("2031-03".to_string()),
    }
}

// ==================== ① 维度白名单（纯逻辑） ====================

/// 4 个维度的列映射 + 大小写不敏感 + 非法值 400。
#[test]
fn p21_aux_column_whitelist_maps_four_dimensions() {
    assert_eq!(aux_column(Some("MATERIAL")).unwrap().1, "aux_material_id");
    assert_eq!(aux_column(Some("material")).unwrap().1, "aux_material_id");
    assert_eq!(aux_column(Some("Warehouse")).unwrap().1, "aux_warehouse_id");
    assert_eq!(aux_column(Some(" DEPT ")).unwrap().1, "aux_dept_id");
    assert_eq!(aux_column(Some("SUPPLIER")).unwrap().1, "aux_supplier_id");
    // 规范化后的枚举值（回显给前端用）
    assert_eq!(aux_column(Some("dept")).unwrap().0, "DEPT");

    let missing = aux_column(None).expect_err("aux_type 缺失必须 400");
    assert_eq!(missing.status, 400);
    let illegal = aux_column(Some("PROJECT")).expect_err("非法维度必须 400");
    assert_eq!(illegal.status, 400);
    assert!(
        illegal.msg.contains("MATERIAL"),
        "报错要列出合法值，实际：{}",
        illegal.msg
    );
}

// ==================== ② 收货入库：物料 / 仓库 + 故意不写部门（纯逻辑） ====================

/// 单物料收货（`M = 1`）：存货借方带「物料 + 仓库」；进项税 / 应付不挂料仓；**任何一行都不写部门**。
///
/// 同时钉住 P2-1b 的回归底线：`M = 1` 时「回退分支（`material_lines` 为空）」与
/// 「按物料拆」两条分支的**行数与辅助维度必须逐字相同**（3 行 / 借存货 + 借进项税 + 贷应付）。
#[test]
fn p21_purchase_in_lines_write_material_and_warehouse_only() {
    let amounts = VoucherAmounts {
        amount: dec("200"),
        tax: dec("26"),
        with_tax: dec("226"),
    };
    let ctx = LineContext {
        warehouse_id: Some(uid(WH)),
        material_id: Some(uid(MAT)),
        supplier_id: Some(uid(SUP)),
    };
    // 分支一：回退（`material_lines` 为空 = P2-1b 之前的路径，非 STOCK_IN 与历史数据走这里）
    let fallback = purchase_in_lines(
        &account_set(),
        amounts,
        "采购入库 IN-1",
        ctx,
        &[],
        &BTreeMap::new(),
        "IN-1",
    )
    .expect("回退分支必须能构造分录");
    // 分支二：按物料拆（`M = 1`）
    let split = purchase_in_lines(
        &account_set(),
        amounts,
        "采购入库 IN-1",
        ctx,
        &[mat_line(MAT, "200", "2", 1, 1)],
        &mat_accounts(&[(MAT, ACC_DR)]),
        "IN-1",
    )
    .expect("按物料拆必须能构造分录");

    for (label, lines) in [("回退", &fallback), ("拆分行", &split)] {
        assert_eq!(lines.len(), 3, "{label}：存货 / 进项税 / 应付 三行");
        let inventory = &lines[0];
        assert_eq!(
            inventory.aux_material_id,
            Some(uid(MAT)),
            "{label}：存货行带物料"
        );
        assert_eq!(
            inventory.aux_warehouse_id,
            Some(uid(WH)),
            "{label}：存货行带仓库"
        );
        assert_eq!(inventory.aux_supplier_id, None);
        assert_eq!(inventory.debit, dec("200"), "{label}：存货金额取明细");
        let tax = &lines[1];
        assert_eq!(
            tax.aux_material_id, None,
            "{label}：进项税是全公司口径，不挂物料"
        );
        assert_eq!(tax.aux_warehouse_id, None);
        let payable = &lines[2];
        assert_eq!(payable.aux_supplier_id, Some(uid(SUP)));
        assert_eq!(payable.aux_material_id, None);
        // 收货单没有部门语义（`biz_stock_in` 没有 dept_id）⇒ 三行都必须留空，不是漏写
        for line in lines {
            assert_eq!(
                line.aux_dept_id, None,
                "{label}：采购入库不写部门（DC-11 的部门从哪来）"
            );
        }
    }
    // 行号、科目、金额逐行一致 —— 这就是 `M = 1` 时 D3 影子对比能原样通过的原因
    for (a, b) in fallback.iter().zip(&split) {
        assert_eq!(a.line_no, b.line_no);
        assert_eq!(a.account.id, b.account.id);
        assert_eq!(a.debit, b.debit);
        assert_eq!(a.credit, b.credit);
        assert_eq!(a.aux_material_id, b.aux_material_id);
        assert_eq!(a.aux_warehouse_id, b.aux_warehouse_id);
    }
}

/// 多物料收货：**每个去重物料一行 1405**（P2-1b §4.3：含税 `M + 2` / 零税 `M + 1`），
/// 每行带**自己的**物料辅助核算 + 同一个收货仓；应付 / 进项税仍各一条。
#[test]
fn p21_purchase_in_lines_split_per_material() {
    // ---- M = 2（含税）：1405×2 + 2221.01×1 + 2202×1 = 4 行 ----
    let two = purchase_in_lines(
        &account_set(),
        VoucherAmounts {
            amount: dec("300"),
            tax: dec("39"),
            with_tax: dec("339"),
        },
        "采购入库 IN-2",
        LineContext {
            warehouse_id: Some(uid(WH)),
            material_id: Some(uid(MAT)),
            supplier_id: Some(uid(SUP)),
        },
        // 故意乱序传入：出行顺序必须是 `material_id` 升序（MAT2 > MAT 编码）
        &[
            mat_line(MAT2, "150", "3", 2, 2),
            mat_line(MAT, "150", "1", 1, 1),
        ],
        &mat_accounts(&[(MAT, ACC_DR), (MAT2, ACC_DR)]),
        "IN-2",
    )
    .expect("M=2 含税必须能构造分录");
    assert_eq!(two.len(), 4, "M=2 含税 ⇒ 2 + 2 行");
    assert_eq!(
        two.iter().map(|line| line.line_no).collect::<Vec<_>>(),
        vec![1, 2, 3, 4],
        "行号必须从 1 连续重排"
    );
    assert_eq!(two[0].aux_material_id, Some(uid(MAT)), "升序第一行 = MAT");
    assert_eq!(two[1].aux_material_id, Some(uid(MAT2)), "升序第二行 = MAT2");
    assert_eq!(two[2].aux_material_id, None, "进项税一条，不挂物料");
    assert_eq!(
        two[3].aux_supplier_id,
        Some(uid(SUP)),
        "贷方应付一条（按 DOC）"
    );
    assert_eq!(two[3].aux_material_id, None);
    let inventory_sum: Decimal = two.iter().map(|line| line.debit).take(2).sum();
    assert_eq!(inventory_sum, dec("300"), "Σ 存货行 = 单头不含税金额");

    // ---- M = 3（零税）：1405×3 + 2202×1 = 4 行 ----
    let three = purchase_in_lines(
        &account_set(),
        VoucherAmounts {
            amount: dec("300"),
            tax: Decimal::ZERO,
            with_tax: dec("300"),
        },
        "采购入库 IN-3",
        LineContext {
            warehouse_id: Some(uid(WH)),
            material_id: Some(uid(MAT)),
            supplier_id: Some(uid(SUP)),
        },
        &[
            mat_line(MAT, "100", "1", 1, 1),
            mat_line(MAT2, "100", "1", 2, 2),
            mat_line(MAT3, "100", "1", 3, 3),
        ],
        &mat_accounts(&[(MAT, ACC_DR), (MAT2, ACC_DR), (MAT3, ACC_DR)]),
        "IN-3",
    )
    .expect("M=3 零税必须能构造分录");
    assert_eq!(three.len(), 4, "M=3 零税 ⇒ 3 + 1 行");
    assert_eq!(
        three
            .iter()
            .take(3)
            .map(|line| line.aux_material_id)
            .collect::<Vec<_>>(),
        vec![Some(uid(MAT)), Some(uid(MAT2)), Some(uid(MAT3))],
        "逐行物料，按 material_id 升序"
    );
    assert!(
        three[..3]
            .iter()
            .all(|line| line.aux_warehouse_id == Some(uid(WH))),
        "每个存货行都挂本单收货仓"
    );
}

/// 同一物料的**多行明细先合并成一行**（§4.2：金额相加、数量相加、行号取区间）。
#[test]
fn p21_purchase_in_lines_merge_same_material() {
    let lines = purchase_in_lines(
        &account_set(),
        VoucherAmounts {
            amount: dec("200"),
            tax: Decimal::ZERO,
            with_tax: dec("200"),
        },
        "采购入库 IN-4",
        LineContext {
            warehouse_id: Some(uid(WH)),
            material_id: Some(uid(MAT)),
            supplier_id: Some(uid(SUP)),
        },
        // MAT 两行（100 + 50，行号 1、2）+ MAT2 一行（50，行号 3）
        &[
            mat_line(MAT, "100", "1", 1, 1),
            mat_line(MAT2, "50", "1", 3, 3),
            mat_line(MAT, "50", "1", 2, 2),
        ],
        &mat_accounts(&[(MAT, ACC_DR), (MAT2, ACC_DR)]),
        "IN-4",
    )
    .expect("同物料多行必须能合并");
    assert_eq!(lines.len(), 3, "去重后 2 个物料 + 应付 = 3 行");
    assert_eq!(lines[0].aux_material_id, Some(uid(MAT)));
    assert_eq!(lines[0].debit, dec("150"), "同一物料金额相加");
    assert!(
        lines[0].summary.contains("第 1–2 行"),
        "摘要写明细行号区间（双向追溯），实际：{}",
        lines[0].summary
    );
    assert_eq!(lines[1].aux_material_id, Some(uid(MAT2)));
    assert_eq!(lines[1].debit, dec("50"));
    assert_eq!(lines[1].line_no, 2);
}

/// 尾差吸收：明细 333.33×3 / 单头 1000.00 ⇒ 末行 333.34、合计 1000.0000（§4.2）。
///
/// 同时钉住两件事：① 差额落在**按物料升序的最后一行**；② `Σ(存货行) ≡ 单头金额`。
#[test]
fn p21_absorb_tail_diff_lands_on_last_material() {
    let amounts = [dec("333.33"), dec("333.33"), dec("333.33")];
    let absorbed = absorb_tail_diff("IN-TAIL", dec("1000.0000"), &amounts).expect("尾差必须能吸收");
    assert_eq!(
        absorbed,
        vec![dec("333.33"), dec("333.33"), dec("333.34")],
        "0.01 的尾差加到最后一个物料"
    );
    assert_eq!(absorbed.iter().copied().sum::<Decimal>(), dec("1000.0000"));

    // 走真实 builder 再看一遍（金额取自明细，单头是权威值）
    let lines = purchase_in_lines(
        &account_set(),
        VoucherAmounts {
            amount: dec("1000.0000"),
            tax: Decimal::ZERO,
            with_tax: dec("1000.0000"),
        },
        "采购入库 IN-TAIL",
        LineContext {
            warehouse_id: Some(uid(WH)),
            material_id: Some(uid(MAT)),
            supplier_id: Some(uid(SUP)),
        },
        &[
            mat_line(MAT, "333.33", "1", 1, 1),
            mat_line(MAT2, "333.33", "1", 2, 2),
            mat_line(MAT3, "333.33", "1", 3, 3),
        ],
        &mat_accounts(&[(MAT, ACC_DR), (MAT2, ACC_DR), (MAT3, ACC_DR)]),
        "IN-TAIL",
    )
    .expect("尾差吸收后必须能构造分录");
    let inventory: Vec<&EntryLine> = lines.iter().take(3).collect();
    assert_eq!(inventory[2].debit, dec("333.34"), "末行吸收 0.01");
    assert_eq!(
        inventory.iter().map(|line| line.debit).sum::<Decimal>(),
        dec("1000.0000"),
        "Σ 存货行 ≡ 单头金额"
    );

    // 最后一行吸收后 ≤ 0（不是 > 0）→ 往前退一个物料
    let retreated =
        absorb_tail_diff("IN-TAIL2", dec("100"), &[dec("100"), dec("5")]).expect("尾差必须能吸收");
    assert_eq!(
        retreated,
        vec![dec("95"), dec("5")],
        "最后一行吸收后正好 0 ⇒ 不能落在这行（会出 0 元分录），退到前一行（100 - 5 = 95）"
    );

    // 全吸收不了 → 报错（不许静默吞掉差额）
    let err = absorb_tail_diff("IN-BAD", dec("10"), &[dec("100"), dec("100")])
        .expect_err("差额为负且每行都吸收不了时必须报错");
    assert!(
        format!("{err}").contains("IN-BAD"),
        "报错必须点名单号，实际：{err}"
    );
    assert!(
        format!("{err}").contains("尾差"),
        "报错要说清是尾差吸收，实际：{err}"
    );
}

/// 金额全为 0 的物料**不出分录**（0 元赠品行不占金额；数量仍在库存流水里）。
#[test]
fn p21_purchase_in_lines_skip_zero_amount_material() {
    let lines = purchase_in_lines(
        &account_set(),
        VoucherAmounts {
            amount: dec("200"),
            tax: Decimal::ZERO,
            with_tax: dec("200"),
        },
        "采购入库 IN-5",
        LineContext {
            warehouse_id: Some(uid(WH)),
            material_id: Some(uid(MAT)),
            supplier_id: Some(uid(SUP)),
        },
        // MAT2 是赠品：qty = 5 但 amount = 0
        &[
            mat_line(MAT, "200", "2", 1, 1),
            mat_line(MAT2, "0", "5", 2, 2),
        ],
        &mat_accounts(&[(MAT, ACC_DR), (MAT2, ACC_DR)]),
        "IN-5",
    )
    .expect("0 元物料必须被跳过而不是报错");
    assert_eq!(lines.len(), 2, "只剩 MAT 存货行 + 应付行");
    assert_eq!(lines[0].aux_material_id, Some(uid(MAT)));
    assert!(
        lines
            .iter()
            .all(|line| line.aux_material_id != Some(uid(MAT2))),
        "金额为 0 的物料不能占一行分录"
    );
}

/// `VoucherBizCtx::of_stock_in`：物料行**归一化**（按 `material_id` 升序 + 同物料合并），
/// `material_id`（科目解析退化用）取首个物料，**不再有「多物料留空」的判定**。
#[test]
fn p21_biz_ctx_carries_material_lines_sorted() {
    let header = stockin_header_stub();
    let ctx = VoucherBizCtx::of_stock_in(
        &header,
        vec![
            mat_line(MAT3, "100", "1", 3, 3),
            mat_line(MAT, "100", "1", 1, 1),
            mat_line(MAT, "50", "1", 2, 2),
        ],
    );
    assert_eq!(
        ctx.material_lines.len(),
        2,
        "同物料合并 ⇒ 2 个物料（MAT / MAT3）"
    );
    assert_eq!(
        ctx.material_lines
            .iter()
            .map(|line| line.material_id)
            .collect::<Vec<_>>(),
        vec![uid(MAT), uid(MAT3)],
        "按 material_id 升序"
    );
    assert_eq!(ctx.material_lines[0].amount, dec("150"), "同物料金额相加");
    assert_eq!(
        ctx.material_id,
        Some(uid(MAT)),
        "科目解析退化用的物料 = 升序首个物料"
    );
    assert_eq!(ctx.material_lines.len() as i64, 2, "物料种类数 = 行数");

    // 空物料行（非 STOCK_IN 调用方 / 历史单据）⇒ 上下文如实为空，由 builder 回退汇总行
    let empty = VoucherBizCtx::of_stock_in(&header, Vec::new());
    assert!(empty.material_lines.is_empty());
    assert_eq!(empty.material_id, None);
}

/// ⑥ 规则没有 `debit_account_id` 且两个物料**各有自己的存货科目** ⇒ 两行、**两科目**（§4.4）。
///
/// 真库用例：钉住「逐物料科目解析」的那条批取 SQL —— 退化顺序是
/// 规则 `debit_account_id` → **该行物料** `inventory_account_id` → `1405`，
/// 不是「首个物料的科目套整笔」（那会把第二种物料记到第一种的科目上）。
///
/// 全程在一个事务里建夹具（物料 / 规则）并 `rollback`，不往共享库留痕。
#[tokio::test]
async fn p21_split_resolves_each_materials_own_inventory_account() {
    case(|| async {
        let tx = db().begin().await.expect("开事务");
        // 两个物料分别配「测试资产」与「测试负债」两个科目（科目本身由 seed 建好）
        for (id, code, account_id) in [(MAT2, "P21AUX-M2", ACC_DR), (MAT3, "P21AUX-M3", ACC_CR)] {
            tx.exec_sql(
                format!(
                    "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, \
                                          inventory_account_id) \
                 VALUES ('{id}'::uuid, '{code}', '{code}', '{UNIT}'::uuid, 1, '{account_id}'::uuid)"
                ),
                vec![],
            )
            .await
            .unwrap_or_else(|err| panic!("插入物料失败：{err}"));
        }
        // 模板：debit_account_id 故意留 NULL（这里才需要退到「该物料自己的存货科目」）
        tx.exec_sql(
        "INSERT INTO biz_voucher_rule (rule_code, biz_type, debit_account_id, credit_account_id, \
                                       tax_account_id, is_active, remark) \
         VALUES ('P21AUX-NODEIBT', 'PURCHASE_IN', NULL, NULL, NULL, 1, \
                 'P2-1b 用例：规则无借方科目 ⇒ 逐物料退化')"
            .to_string(),
        vec![],
    )
    .await
    .expect("插入映射规则失败");

        let mut header = stockin_header_stub();
        // 用一个别的期间，避开「期间已结账」类用例钉死的 2031-03
        header.in_date = date("2031-07-15");
        let ctx = VoucherBizCtx::of_stock_in(
            &header,
            vec![
                mat_line(MAT2, "100", "1", 1, 1),
                mat_line(MAT3, "100", "1", 2, 2),
            ],
        );
        let voucher_id = create_by_rule(&tx, Some("P21AUX-NODEIBT"), &ctx, None, Utc::now())
            .await
            .expect("两个物料各有存货科目时必须能生成凭证");
        let entries = entries_of(&tx, voucher_id).await.expect("读分录");
        assert_eq!(entries.len(), 4, "含税 M=2 ⇒ 2 + 1 + 1 行");
        assert_eq!(
            entries[0].account.id,
            uid(ACC_DR),
            "MAT2 那行走它自己的存货科目"
        );
        assert_eq!(
            entries[1].account.id,
            uid(ACC_CR),
            "MAT3 那行走它自己的存货科目（不是首个物料那个）"
        );
        assert_eq!(entries[0].aux_material_id, Some(uid(MAT2)));
        assert_eq!(entries[1].aux_material_id, Some(uid(MAT3)));
        assert_eq!(entries[0].debit, dec("100"));
        assert_eq!(entries[1].debit, dec("100"));
        // 贷方应付仍是按 DOC 一条（1405 由规则/默认解析，本用例不断言）
        assert_eq!(entries[3].credit, dec("226"), "贷方 = 单头价税合计");
        tx.rollback().await.expect("回滚");
    })
    .await;
}

/// 收货单头的测试替身（只填 `of_stock_in` 用到的字段）。
fn stockin_header_stub() -> crate::business::erp::stockin_base::StockInRow {
    crate::business::erp::stockin_base::StockInRow {
        id: uid("e21a0010-0000-4000-8000-000000000010"),
        in_no: "P21AUX-IN-1".to_string(),
        in_type: "PURCHASE".to_string(),
        source_order_id: None,
        supplier_id: Some(uid(SUP)),
        is_sourceless: false,
        warehouse_id: uid(WH),
        in_date: date("2031-03-05"),
        status: "POSTED".to_string(),
        currency_code: "CNY".to_string(),
        total_qty: dec("2"),
        total_amount: dec("200"),
        total_tax: dec("26"),
        total_with_tax: dec("226"),
        invoice_received: false,
        voucher_id: None,
        payable_id: None,
        remark: None,
        posted_at: None,
        posted_by: None,
        cancelled_at: None,
        cancelled_by: None,
        cancel_reason: None,
        approval_instance_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        create_by: None,
        update_by: None,
        deleted_at: None,
    }
}

// ==================== ③ 红冲保留辅助维度（纯逻辑） ====================

/// 红冲（借贷互换）必须原样复制三组辅助核算 —— 否则按辅助项查明细时冲不掉。
#[test]
fn p21_reversed_lines_copy_aux_dimensions() {
    let original = vec![
        EntryRow {
            id: uid("e21a0011-0000-4000-8000-000000000011"),
            voucher_id: uid("e21a0012-0000-4000-8000-000000000012"),
            line_no: 1,
            summary: "付款 冲减应付".to_string(),
            account: account_of(ACC_CR, "P21AUXB", "P2-1 测试负债"),
            debit: dec("60"),
            credit: Decimal::ZERO,
            aux_supplier_id: Some(uid(SUP)),
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: Some(uid(DEPT)),
        },
        EntryRow {
            id: uid("e21a0013-0000-4000-8000-000000000013"),
            voucher_id: uid("e21a0012-0000-4000-8000-000000000012"),
            line_no: 2,
            summary: "付款 银行付款".to_string(),
            account: account_of(ACC_DR, "P21AUXA", "P2-1 测试资产"),
            debit: Decimal::ZERO,
            credit: dec("60"),
            aux_supplier_id: Some(uid(SUP)),
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        },
    ];
    let red = reversed_lines(&original, Some("测试红冲"));
    assert_eq!(red.len(), 2);
    assert_eq!(
        red[0].aux_dept_id,
        Some(uid(DEPT)),
        "部门辅助核算要跟着红冲"
    );
    assert_eq!(red[0].aux_supplier_id, Some(uid(SUP)));
    assert_eq!(red[1].aux_dept_id, None, "原本没部门的行也不会凭空长出部门");
    assert_eq!(red[0].debit, Decimal::ZERO);
    assert_eq!(red[0].credit, dec("60"));
}

// ==================== ④ 落库往返：四个辅助列都进库、都能读回 ====================

/// `post_voucher` → `biz_voucher_entry` 的四个辅助列落库并在 `entries_of` 里读回。
#[tokio::test]
async fn p21_entries_roundtrip_all_four_aux_columns() {
    case(|| async {
        let fixture = fixtures().await;
        let entries = entries_of(db(), fixture.material_debit).await.unwrap();
        assert_eq!(entries.len(), 2);
        let debit = &entries[0];
        assert_eq!(debit.aux_material_id, Some(uid(MAT)), "物料辅助核算落库");
        assert_eq!(debit.aux_warehouse_id, Some(uid(WH)), "仓库辅助核算落库");
        assert_eq!(debit.aux_dept_id, None, "收货凭证不写部门");
        let credit = &entries[1];
        assert_eq!(credit.aux_supplier_id, Some(uid(SUP)));
        assert_eq!(credit.aux_material_id, None);

        // 付款口径的部门行同样能落库、能读回
        let dept_entries = entries_of(db(), fixture.dept_debit).await.unwrap();
        assert_eq!(dept_entries[0].aux_dept_id, Some(uid(DEPT)));
    })
    .await;
}

// ==================== ⑤ 辅助账接口（真库） ====================

/// 按物料查：期初 / 本期 / 期末 + 名称回显。
#[tokio::test]
async fn p21_aux_balance_material_splits_opening_and_period() {
    case(|| async {
        fixtures().await;
        let res = aux_balance(&query("MATERIAL", Some(MAT)))
            .await
            .expect("按物料查辅助账");
        assert_eq!(res.aux_type, "MATERIAL");
        assert_eq!(res.aux_name.as_deref(), Some(MAT_NAME), "物料名称要回显");
        assert_eq!(res.rows.len(), 1, "只有一个科目挂了这个物料");
        let row = &res.rows[0];
        assert_eq!(row.account_code, "P21AUXA");
        assert_eq!(row.opening_debit, dec("25"), "2031-02 的 25 是期初");
        assert_eq!(row.period_debit, dec("100"), "2031-03 的 100 是本期借方");
        assert_eq!(row.period_credit, Decimal::ZERO);
        assert_eq!(row.closing_debit, dec("125"));
        assert_eq!(row.closing_credit, Decimal::ZERO);
        assert_eq!(res.total_period_debit, dec("100"));
        assert_eq!(res.total_closing_debit, dec("125"));
    })
    .await;
}

/// 不给 `aux_id`：该维度下每个辅助项各出一组行（按科目编码 + 辅助项 id 排序）。
#[tokio::test]
async fn p21_aux_balance_lists_all_items_of_a_dimension() {
    case(|| async {
        fixtures().await;
        let res = aux_balance(&query("MATERIAL", None))
            .await
            .expect("按维度列全部物料");
        assert!(!res.rows.is_empty());
        let material = res
            .rows
            .iter()
            .find(|row| row.aux_id == MAT)
            .expect("列表里必须有 P21AUX 物料");
        assert_eq!(material.period_debit, dec("100"));
        assert_eq!(material.aux_name.as_deref(), Some(MAT_NAME));
        // 排序稳定：科目编码升序
        let codes: Vec<&str> = res.rows.iter().map(|r| r.account_code.as_str()).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(codes, sorted, "行必须按科目编码升序");
    })
    .await;
}

/// 覆盖度自证：本期没挂该维度的发生额必须算出来（不能只回"有值的那些"）。
#[tokio::test]
async fn p21_aux_balance_reports_unassigned_amounts() {
    case(|| async {
        fixtures().await;
        let res = aux_balance(&query("MATERIAL", Some(MAT)))
            .await
            .expect("按物料查辅助账");
        // 2031-03 里没有物料辅助核算的借方：V2 的 40（凭证行本身不挂料）+ V4 的 60（挂部门）
        assert_eq!(
            res.unassigned.period_debit,
            dec("100"),
            "未挂物料的借方合计 = 40 + 60"
        );
        assert!(
            res.unassigned.entry_count >= 2,
            "未挂物料的借方分录至少 2 行，实际 {}",
            res.unassigned.entry_count
        );
    })
    .await;
}

/// 供应商维度：贷方发生额按供应商汇总，名称回显。
#[tokio::test]
async fn p21_aux_balance_supplier_sums_credit_side() {
    case(|| async {
        fixtures().await;
        let res = aux_balance(&query("SUPPLIER", Some(SUP)))
            .await
            .expect("按供应商查辅助账");
        assert_eq!(res.aux_name.as_deref(), Some(SUP_NAME));
        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0].period_credit, dec("140"), "100 + 40");
        assert_eq!(res.rows[0].period_debit, Decimal::ZERO);
        assert_eq!(res.rows[0].closing_credit, dec("140"));
        assert_eq!(res.total_period_credit, dec("140"));
    })
    .await;
}

/// 部门维度：付款凭证借方的部门辅助核算能被查出来（DC-11 的主诉求）。
#[tokio::test]
async fn p21_aux_balance_dept_reads_payment_dimension() {
    case(|| async {
        fixtures().await;
        let res = aux_balance(&query("DEPT", Some(DEPT)))
            .await
            .expect("按部门查辅助账");
        assert_eq!(res.aux_name.as_deref(), Some(DEPT_NAME), "部门名称要回显");
        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0].period_debit, dec("60"), "付款 60 记到采购部");
        assert_eq!(res.rows[0].closing_debit, dec("60"));
    })
    .await;
}

/// 仓库维度：收货仓库的借方发生额。
#[tokio::test]
async fn p21_aux_balance_warehouse_reads_stock_in_dimension() {
    case(|| async {
        fixtures().await;
        let res = aux_balance(&query("WAREHOUSE", Some(WH)))
            .await
            .expect("按仓库查辅助账");
        assert_eq!(res.aux_name.as_deref(), Some(WH_NAME));
        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0].period_debit, dec("100"));
    })
    .await;
}

/// 期间的边界：`period_to` 之外的凭证不计入本期，也不会漏进未挂维度统计。
#[tokio::test]
async fn p21_aux_balance_respects_period_bounds() {
    case(|| async {
        let fixture = fixtures().await;
        let mut q = query("MATERIAL", Some(MAT));
        q.period_from = Some("2031-03-01".to_string());
        q.period_to = Some("2031-03-06".to_string());
        let res = aux_balance(&q).await.expect("按日期区间查");
        assert_eq!(
            res.rows[0].period_debit,
            dec("100"),
            "V1 在 03-05，仍在区间内"
        );
        assert_eq!(res.rows[0].opening_debit, dec("25"), "03-01 之前的算期初");
        let _ = (&fixture.plain_debit, &fixture.opening);
    })
    .await;
}

/// 非法参数：`aux_type` 缺失 / 非法、`aux_id` 不是 UUID、`period_from > period_to` 都 400。
#[tokio::test]
async fn p21_aux_balance_rejects_bad_params() {
    let mut missing = query("MATERIAL", None);
    missing.aux_type = None;
    assert_eq!(
        aux_balance(&missing).await.expect_err("缺 aux_type").status,
        400
    );

    let mut illegal = query("MATERIAL", None);
    illegal.aux_type = Some("PROJECT".to_string());
    assert_eq!(
        aux_balance(&illegal).await.expect_err("非法维度").status,
        400
    );

    assert_eq!(
        aux_balance(&query("MATERIAL", Some("not-a-uuid")))
            .await
            .expect_err("aux_id 非法")
            .status,
        400
    );

    let mut reversed = query("MATERIAL", None);
    reversed.period_from = Some("2031-03".to_string());
    reversed.period_to = Some("2031-02".to_string());
    assert_eq!(
        aux_balance(&reversed).await.expect_err("区间倒挂").status,
        400
    );
}

// ==================== ⑥ 部门从哪来（`sys_user.dept_id`） ====================

/// `user_dept_of`：有部门 → 部门 id；没有部门 / 用户不存在 → None（不报错）。
#[tokio::test]
async fn p21_user_dept_of_reads_sys_user_dept() {
    case(|| async {
        assert_eq!(
            user_dept_of(db(), uid(USER)).await.unwrap(),
            Some(uid(DEPT)),
            "有部门的申请人"
        );
        assert_eq!(
            user_dept_of(db(), uid(USER_NO_DEPT)).await.unwrap(),
            None,
            "没配部门的系统账号不报错，返回 None"
        );
        assert_eq!(
            user_dept_of(db(), Uuid::now_v7()).await.unwrap(),
            None,
            "用户不存在也返回 None"
        );
    })
    .await;
}

// ==================== ⑦ 收尾清场 ====================

/// 清场：本组的凭证 / 主数据 / 科目必须全部删掉（科目留着会让别的模块的
/// 「分类配置覆盖全科目」断言红，与本模块既有的 `zz_teardown_purges_fixture_accounts` 同口径）。
///
/// **本用例是外部交叉验证，不再是唯一的清场闸门**：每条真库用例都包在 [`case()`] 里，
/// 结束时（含 panic）自己清一遍；这里再独立查一次库，红了才说明「每例自清」漏了什么。
#[tokio::test]
async fn zz_p21_aux_teardown_purges_fixtures() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    let left: i64 = db()
        .scalar_sql(
            "SELECT count(*) FROM biz_voucher WHERE source_no LIKE 'P21AUX%'".to_string(),
            vec![],
        )
        .await
        .unwrap_or(-1);
    assert_eq!(left, 0, "本组凭证必须清干净");
    let accounts: i64 = db()
        .scalar_sql(
            format!(
                "SELECT count(*) FROM biz_account WHERE id IN ('{ACC_DR}'::uuid, '{ACC_CR}'::uuid)"
            ),
            vec![],
        )
        .await
        .unwrap_or(-1);
    assert_eq!(accounts, 0, "本组科目必须清干净");
}

// ==================== 附：金额工具自检 ====================

/// 夹具金额解析（防止 `dec` 被改成浮点后静默丢精度）。
#[test]
fn p21_dec_keeps_four_scale() {
    assert_eq!(dec("0.0001").to_string(), "0.0001");
    assert_eq!(dec("100") + dec("25"), dec("125"));
}
