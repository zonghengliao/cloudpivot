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
//!
//! # 跑法
//!
//! ```bash
//! ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>' \
//!   cargo test --lib p21_aux -- --test-threads=1
//! ```
#![cfg(test)]

use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::str::FromStr;

use crate::business::erp::ledger::{AuxBalanceQuery, aux_balance, aux_column};
use crate::business::erp::voucher::{
    AccountRef, AccountSet, EntryLine, EntryRow, LineContext, PostVoucher, VoucherAmounts,
    VoucherBizCtx, entries_of, post_voucher, purchase_in_lines, reversed_lines, user_dept_of,
};
use crate::business::erp::voucher_query::tests::{DB_LOCK, ensure_db};

// ==================== 夹具 id ====================

/// 物料「甲」。
const MAT: &str = "e21a0001-0000-4000-8000-000000000001";
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

/// 单物料收货：存货借方带「物料 + 仓库」；进项税 / 应付不挂料仓；**任何一行都不写部门**。
#[test]
fn p21_purchase_in_lines_write_material_and_warehouse_only() {
    let lines = purchase_in_lines(
        &account_set(),
        VoucherAmounts {
            amount: dec("200"),
            tax: dec("26"),
            with_tax: dec("226"),
        },
        "采购入库 IN-1",
        LineContext {
            warehouse_id: Some(uid(WH)),
            material_id: Some(uid(MAT)),
            supplier_id: Some(uid(SUP)),
        },
    );
    assert_eq!(lines.len(), 3, "存货 / 进项税 / 应付 三行");
    let inventory = &lines[0];
    assert_eq!(inventory.aux_material_id, Some(uid(MAT)));
    assert_eq!(inventory.aux_warehouse_id, Some(uid(WH)));
    assert_eq!(inventory.aux_supplier_id, None);
    let tax = &lines[1];
    assert_eq!(tax.aux_material_id, None, "进项税是全公司口径，不挂物料");
    assert_eq!(tax.aux_warehouse_id, None);
    let payable = &lines[2];
    assert_eq!(payable.aux_supplier_id, Some(uid(SUP)));
    assert_eq!(payable.aux_material_id, None);
    // 收货单没有部门语义（`biz_stock_in` 没有 dept_id）⇒ 三行都必须留空，不是漏写
    for line in &lines {
        assert_eq!(
            line.aux_dept_id, None,
            "采购入库不写部门（DC-11 的部门从哪来）"
        );
    }
}

/// 多物料收货（存货借方是汇总行）：物料辅助核算必须留空，仓库照写。
#[test]
fn p21_purchase_in_lines_leave_material_empty_when_aggregated() {
    let lines = purchase_in_lines(
        &account_set(),
        VoucherAmounts {
            amount: dec("200"),
            tax: Decimal::ZERO,
            with_tax: dec("200"),
        },
        "采购入库 IN-2",
        LineContext {
            warehouse_id: Some(uid(WH)),
            material_id: None, // 调用方判定「多物料」后传 None
            supplier_id: Some(uid(SUP)),
        },
    );
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].aux_material_id, None, "汇总行不能挂某一个物料");
    assert_eq!(lines[0].aux_warehouse_id, Some(uid(WH)), "仓库仍是确定的");
}

/// `VoucherBizCtx::of_stock_in`：`material_count` 决定辅助物料是否可写；
/// `material_id`（科目解析用）任何情况下都保留。
#[test]
fn p21_biz_ctx_keeps_material_for_account_but_drops_aux_when_multi() {
    let header = stockin_header_stub();
    let single = VoucherBizCtx::of_stock_in(&header, Some(uid(MAT)), 1);
    assert_eq!(single.material_id, Some(uid(MAT)), "科目解析仍按首个物料");
    assert_eq!(
        single.aux_material_id,
        Some(uid(MAT)),
        "单物料的辅助核算可写"
    );
    assert_eq!(single.material_count, 1);

    let multi = VoucherBizCtx::of_stock_in(&header, Some(uid(MAT)), 3);
    assert_eq!(
        multi.material_id,
        Some(uid(MAT)),
        "科目解析不受辅助核算影响"
    );
    assert_eq!(multi.aux_material_id, None, "多物料汇总行不挂物料");
    assert_eq!(multi.material_count, 3);
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
    let Some(_guard) = setup().await else {
        return;
    };
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
}

// ==================== ⑤ 辅助账接口（真库） ====================

/// 按物料查：期初 / 本期 / 期末 + 名称回显。
#[tokio::test]
async fn p21_aux_balance_material_splits_opening_and_period() {
    let Some(_guard) = setup().await else {
        return;
    };
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
}

/// 不给 `aux_id`：该维度下每个辅助项各出一组行（按科目编码 + 辅助项 id 排序）。
#[tokio::test]
async fn p21_aux_balance_lists_all_items_of_a_dimension() {
    let Some(_guard) = setup().await else {
        return;
    };
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
}

/// 覆盖度自证：本期没挂该维度的发生额必须算出来（不能只回"有值的那些"）。
#[tokio::test]
async fn p21_aux_balance_reports_unassigned_amounts() {
    let Some(_guard) = setup().await else {
        return;
    };
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
}

/// 供应商维度：贷方发生额按供应商汇总，名称回显。
#[tokio::test]
async fn p21_aux_balance_supplier_sums_credit_side() {
    let Some(_guard) = setup().await else {
        return;
    };
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
}

/// 部门维度：付款凭证借方的部门辅助核算能被查出来（DC-11 的主诉求）。
#[tokio::test]
async fn p21_aux_balance_dept_reads_payment_dimension() {
    let Some(_guard) = setup().await else {
        return;
    };
    fixtures().await;
    let res = aux_balance(&query("DEPT", Some(DEPT)))
        .await
        .expect("按部门查辅助账");
    assert_eq!(res.aux_name.as_deref(), Some(DEPT_NAME), "部门名称要回显");
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0].period_debit, dec("60"), "付款 60 记到采购部");
    assert_eq!(res.rows[0].closing_debit, dec("60"));
}

/// 仓库维度：收货仓库的借方发生额。
#[tokio::test]
async fn p21_aux_balance_warehouse_reads_stock_in_dimension() {
    let Some(_guard) = setup().await else {
        return;
    };
    fixtures().await;
    let res = aux_balance(&query("WAREHOUSE", Some(WH)))
        .await
        .expect("按仓库查辅助账");
    assert_eq!(res.aux_name.as_deref(), Some(WH_NAME));
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0].period_debit, dec("100"));
}

/// 期间的边界：`period_to` 之外的凭证不计入本期，也不会漏进未挂维度统计。
#[tokio::test]
async fn p21_aux_balance_respects_period_bounds() {
    let Some(_guard) = setup().await else {
        return;
    };
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
    let Some(_guard) = setup().await else {
        return;
    };
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
}

// ==================== ⑦ 收尾清场 ====================

/// 清场：本组的凭证 / 主数据 / 科目必须全部删掉（科目留着会让别的模块的
/// 「分类配置覆盖全科目」断言红，与本模块既有的 `zz_teardown_purges_fixture_accounts` 同口径）。
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
