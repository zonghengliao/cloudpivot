//! M1-22 账龄单测：**跨期** 与 **部分收款**（部分核销之后的余额分摊）。
//!
//! # 这张卡要钉住什么
//!
//! M1-19 / M1-20 / M1-21 已经把「分档刻度」「透视 / 汇总」「导出」各自的形状钉住了，
//! 但它们喂进去的都是**一分没收到**的单（`received_amount = 0`）。真实账龄表里最多的
//! 恰恰是**收了一半的单**：一张 1000 的应收收了 300，剩 700 挂在账上 45 天 —— 这 700
//! 该落哪一档、按什么排客户、是否要把已收的 300 另外摊到「未到期」里，都不是分档函数
//! 能回答的问题，而一旦答错**不会报错**，只是账龄表的合计与总账对不上。
//!
//! 本文件把四条口径用断言钉死（每条都是「实现怎么写的」+「为什么这么写」）：
//!
//! | # | 口径 | 错成什么样不会被发现 |
//! |:--|:--|:--|
//! | ① | 入表金额 = **未收余额**（`amount − received_amount`），不是应收金额 | 用 `amount` ⇒ 部分已收的单虚增欠款（本文件里 1000 会顶 700） |
//! | ② | 余额**整笔**落在该单自己的到期日档位，不按核销比例拆档 | 按比例拆 ⇒ 700 被摊成「45 天档 700 × (700/1000) + 未到期 700 × (300/1000)」之类，表里多出根本不存在的钱 |
//! | ③ | 一张收款核销多张应收时，**逐单减各自的余额**，各归各档 | 按收款单整体处理 ⇒ 核销明细的 300 被当成账龄金额，或者把 A 单的钱摊到 B 单的档 |
//! | ④ | 跨期：单据按 `period_code ≤ 基准月` 截断；基准日推进只改**逾期天数 / 档位**，不改余额；**基准日之后发生的收款不回滚成历史余额**（当前余额快照，出参 `hints` 明示） | 「历史重算」与「当前快照」两种语义混着写 ⇒ 9 月末的账龄表被 10 月的收款改写，与已出的报表 / 总账永久对不上 |
//!
//! ④ 的第二半是**已决策语义**（`receivable_aging::assemble` 的 `hints` 第 2 条原文
//! 「金额是「当前」未收余额快照：基准日之后发生的收款不会回滚成历史值（本表不做历史
//! 余额重算）」），不是漏做：真要做「历史余额重算」得按 `biz_receipt_allocation ⋈
//! biz_receipt.receipt_date ≤ 基准日` 回溯，那是**另一张卡**的范围。本文件用
//! [`cross_period_receipt_does_not_rewrite_prior_balance_hint_is_explicit`] 与
//! [`db_cross_period_receipt_does_not_restate_prior_aging`] 两条用例把这条界线钉住：
//! 谁哪天把口径改成「回溯」，这两条会红，改的人必须同时改 `hints` 文案 —— 不许悄悄换语义。
//!
//! # 真库用例走的是**真写路径**，不是手灌余额
//!
//! 部分收款这件事的真相在写侧：`receipt::create` 登记收款 → `receipt::allocate`
//! 核销到应收（锁应收 → 校验可核销余额 → 落 `biz_receipt_allocation` → 回写
//! `received_amount / unsettled_amount / status`）。如果夹具直接 `UPDATE` 余额，
//! 测的就只是分档函数，**回写算错（比如把 delta 累加成绝对值）依然全绿**。
//! 所以 ⑤⑥⑦ 三条真库用例全部调真接口：
//!
//! ```text
//! receipt::create ──► receipt::allocate ──► receivable_aging::build_report
//!         (部分收款)        (部分核销)              (账龄出表)
//! ```
//!
//! # 夹具（本文件独占；账龄是**全库口径**，所以清整表 —— 理由见
//!           `receivable_aging::tests::setup` 的注释：挂 `receivable_id` 的三张表都是 RESTRICT）
//!
//! ```text
//! 客户 AG22-CUST（甲）
//!
//! 单号            期间     应收    已收   未收   到期日        基准日 2026-11-05 逾期   档位
//! AR-AG22-0001   2026-08  1000     0     1000  2026-08-31       66 天              D61_90
//! AR-AG22-0002   2026-09  1000     0     1000  2026-09-30       36 天              D31_60
//! AR-AG22-0003   2026-10   800     0      800  2026-10-20       16 天              D0_30
//! AR-AG22-0004   2026-11   600     0      600  2026-12-05      −30 天（未到期）      NOT_DUE
//!                                       合计 3400 / 已收 0 / 未收 3400
//!
//! 部分收款（⑤⑥ 用；收款日期刻意留在 2026-10，落在 §跨期 用例的基准日之后）
//!   RC-…（2026-10-15，700）：核 0001 → 300、核 0002 → 200（**剩 200 未核销**，DRAFT 允许）
//!   RC-…（2026-10-20，500）：核 0003 → 500
//!
//! 改金额（⑤b 用，单独一张收款）
//!   RC-…（2026-10-15，700）：核 0001 → 300，再改成 500（**同一行改金额**，不是插第二行）
//!
//! 收清（⑦ 用）
//!   RC-…（2026-11-03，200）+ RC-…（2026-11-04，400）：两次部分核销把 0004 收清 → SETTLED
//! ```
//!
//! # 跑法
//!
//! ```bash
//! # 99（开发机，连 145 的 PG）
//! PGPASSWORD=*** createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m122_unit
//! ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cp_m122_unit' \
//!   cargo test --lib business::erp::receivable::aging_partial_tests -- --test-threads=1
//! # 145（测试机，走 loopback）
//! ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/<库>' \
//!   cargo test --lib business::erp::receivable::aging_partial_tests -- --test-threads=1
//! ```
//!
//! 缺 `ERP_DB_URL` 时真库用例逐条打印 `SKIP:` 后返回（纯逻辑用例照常跑）。

use std::str::FromStr;

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::payable::payable_aging::{self, AsOf};
use crate::business::erp::receipt::{self, AllocateItemReq, AllocateReq, CreateReceiptReq};
use crate::business::erp::receivable::receivable_aging::{
    self, BUCKETS, CustomerBucketRow, ReceivableAgingDetailRow, ReceivableAgingQuery, ReceivableAgingRes,
    ReceivableAgingSnapshot, assemble,
};
use crate::business::erp::voucher::voucher_query::tests::{DB_LOCK, ensure_db};

/// 基准日（夹具所有到期日都相对它算，与 `receivable_aging::tests` 用同一天，便于对拍）。
const AS_OF: &str = "2026-11-05";

// ---------- 纯逻辑夹具 ----------

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("定点可解析")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
}

/// 一张「应收单」的纯逻辑形态（只带账龄要用的列）。
struct PureDoc<'a> {
    /// 客户主键。
    customer: &'a str,
    /// 客户名。
    customer_name: &'a str,
    /// 单号。
    no: &'a str,
    /// 应收金额。
    amount: &'a str,
    /// 已收金额。
    received: &'a str,
    /// 到期日。
    due: &'a str,
    /// 所属期间。
    period: &'a str,
}

/// 按「未收余额」把单据聚成 `客户 × 档位` 的格子 + 明细。
///
/// 这是**独立的第二实现**（不是调 `load_cells`）：先按到期日算逾期 → 分档 → 按格聚合，
/// 期望值由它算出，再由 [`assemble`] 从明细重新分档 —— 两条路给出同一个数才算过。
fn snapshot_of(as_of: NaiveDate, docs: &[PureDoc<'_>]) -> ReceivableAgingSnapshot {
    let mut cells: Vec<CustomerBucketRow> = Vec::new();
    for doc in docs {
        let customer = Uuid::parse_str(doc.customer).expect("客户 id 合法");
        let days = payable_aging::overdue_days(as_of, date(doc.due));
        let key = payable_aging::bucket_of_in(&BUCKETS, days);
        let unsettled = dec(doc.amount) - dec(doc.received);
        match cells
            .iter_mut()
            .find(|cell| cell.customer_id == customer && cell.bucket == key)
        {
            Some(cell) => {
                cell.doc_count += 1;
                cell.unsettled += unsettled;
                cell.min_days = cell.min_days.min(days);
                cell.max_days = cell.max_days.max(days);
            }
            None => cells.push(CustomerBucketRow {
                customer_id: customer,
                customer_name: doc.customer_name.to_string(),
                bucket: key.to_string(),
                doc_count: 1,
                unsettled,
                min_days: days,
                max_days: days,
            }),
        }
    }
    let details: Vec<ReceivableAgingDetailRow> = docs
        .iter()
        .map(|doc| {
            let amount = dec(doc.amount);
            let received = dec(doc.received);
            ReceivableAgingDetailRow {
                id: Uuid::now_v7(),
                ar_no: doc.no.to_string(),
                customer_id: Uuid::parse_str(doc.customer).expect("客户 id 合法"),
                customer_name: doc.customer_name.to_string(),
                source_type: "MANUAL".to_string(),
                source_no: "MANUAL".to_string(),
                invoice_no: None,
                period_code: doc.period.to_string(),
                credit_days: 30,
                due_date: date(doc.due),
                amount,
                received_amount: received,
                unsettled_amount: amount - received,
                status: if received.is_zero() { "OPEN" } else { "PARTIAL" }.to_string(),
            }
        })
        .collect();
    ReceivableAgingSnapshot {
        cells,
        total_amount: docs.iter().map(|doc| dec(doc.amount)).sum(),
        total_received: docs.iter().map(|doc| dec(doc.received)).sum(),
        total_unsettled: docs.iter().map(|doc| dec(doc.amount) - dec(doc.received)).sum(),
        doc_count: docs.len() as i64,
        customer_count: details
            .iter()
            .map(|row| row.customer_id)
            .collect::<std::collections::HashSet<_>>()
            .len() as i64,
        details,
    }
}

/// 出参里某一档的金额。
fn bucket_amount(report: &ReceivableAgingRes, key: &str) -> Decimal {
    report
        .summary
        .iter()
        .find(|cell| cell.key == key)
        .unwrap_or_else(|| panic!("汇总里没有档位 {key}：{:?}", report.summary))
        .unpaid
}

// ---------- ① 部分核销后的余额分摊（纯逻辑） ----------

/// ① 部分核销后的余额**整笔**落在该单自己的档位，且客户按**余额**而不是应收金额排序。
///
/// 甲：应收 1000 / 已收 300 / 未收 700，到期 2026-09-21 ⇒ 基准日逾期 45 天 ⇒ 31-60 档。
/// 乙：应收 800 / 已收 0 / 未收 800，到期 2026-10-26 ⇒ 逾期 10 天 ⇒ 1-30 档。
///
/// 两条断言各自抓一个坑：
/// * 31-60 档必须是 **700**（不是 1000）—— 用 `amount` 的实现会在这里多 300；
/// * 客户顺序必须是 **乙(800) 在甲(700) 前** —— 按应收金额排会得到「甲 1000 在前」，
///   页面第一行就错（财务看账龄先看谁欠得多）。
#[test]
fn partial_receipt_keeps_balance_in_its_own_bucket_and_sorting() {
    let alpha = "a22b0000-0000-4000-8000-000000000001";
    let beta = "a22b0000-0000-4000-8000-000000000002";
    let docs = [
        PureDoc {
            customer: alpha,
            customer_name: "甲客户",
            no: "AR-PURE-0001",
            amount: "1000",
            received: "300",
            due: "2026-09-21",
            period: "2026-09",
        },
        PureDoc {
            customer: beta,
            customer_name: "乙客户",
            no: "AR-PURE-0002",
            amount: "800",
            received: "0",
            due: "2026-10-26",
            period: "2026-10",
        },
    ];
    let as_of = date(AS_OF);
    let snapshot = snapshot_of(as_of, &docs);
    let report = assemble(
        AsOf {
            date: as_of,
            source: payable_aging::AS_OF_QUERY,
        },
        None,
        None,
        snapshot,
        "云枢",
    );

    // 余额口径：各档之和 = 未收合计 = 700 + 800
    assert_eq!(
        bucket_amount(&report, "D31_60"),
        dec("700"),
        "甲只剩 700，不是应收金额 1000"
    );
    assert_eq!(bucket_amount(&report, "D0_30"), dec("800"));
    assert_eq!(bucket_amount(&report, "NOT_DUE"), Decimal::ZERO);
    assert_eq!(report.total_unsettled, dec("1500"));
    // 应收 / 已收是**另外两个字段**，不许混进档位里（混进去就会加两遍）
    assert_eq!(report.total_amount, dec("1800"), "应收合计含已收部分");
    assert_eq!(report.total_received, dec("300"));
    assert_eq!(
        report.total_amount - report.total_received,
        report.total_unsettled,
        "应收 − 已收 = 未收（三个合计必须自洽）"
    );

    // 客户排序按余额：乙 800 > 甲 700（不是甲 1000 > 乙 800）
    assert_eq!(report.customers.len(), 2);
    assert_eq!(report.customers[0].customer_name, "乙客户", "余额大的在前");
    assert_eq!(report.customers[0].total_unsettled, dec("800"));
    assert_eq!(report.customers[1].customer_name, "甲客户");
    assert_eq!(report.customers[1].total_unsettled, dec("700"));
    assert_eq!(report.customers[1].max_overdue_days, 45);

    // 明细行：三列（应收 / 已收 / 未收）各自对，且档位按到期日现算
    let detail = report
        .details
        .iter()
        .find(|row| row.ar_no == "AR-PURE-0001")
        .expect("明细里必须有甲的单");
    assert_eq!(detail.amount, dec("1000"));
    assert_eq!(detail.received_amount, dec("300"));
    assert_eq!(detail.unsettled_amount, dec("700"));
    assert_eq!(detail.overdue_days, 45);
    assert_eq!(detail.bucket, "D31_60");
    assert_eq!(detail.status, "PARTIAL", "部分已收的单状态是 PARTIAL，仍然进表");

    // 逾期占比按余额算：1800 逾期 / 1500 未收…… 这里两张都逾期 ⇒ 100%
    assert_eq!(report.overdue_unsettled, dec("1500"));
    assert_eq!(report.overdue_ratio, dec("100.00"));
}

/// ② 已收的那部分**不进任何档位**（最常见的错法：把「已收」当成「未到期」摊进 NOT_DUE）。
///
/// 单张 1000 / 已收 300 / 未收 700，到期日在基准日之后 ⇒ 整笔落「未到期」。
/// 错法会把 1000 摊成「未到期 1000」或「未到期 700 + 别的档 300」，两种都会让
/// 汇总行多出钱；正确实现只有一个数：700。
#[test]
fn received_part_never_lands_in_a_bucket() {
    let docs = [PureDoc {
        customer: "a22b0000-0000-4000-8000-000000000003",
        customer_name: "丙客户",
        no: "AR-PURE-0003",
        amount: "1000",
        received: "300",
        due: "2026-12-31",
        period: "2026-11",
    }];
    let as_of = date(AS_OF);
    let report = assemble(
        AsOf {
            date: as_of,
            source: payable_aging::AS_OF_QUERY,
        },
        None,
        None,
        snapshot_of(as_of, &docs),
        "云枢",
    );

    assert_eq!(bucket_amount(&report, "NOT_DUE"), dec("700"), "未到期只有未收余额 700");
    let mut bucket_sum = Decimal::ZERO;
    for cell in &report.summary {
        bucket_sum += cell.unpaid;
        assert_ne!(cell.unpaid, dec("300"), "「已收 300」不许单独占一格：{}", cell.label);
    }
    assert_eq!(
        bucket_sum,
        dec("700"),
        "各档之和 = 未收余额（不是应收金额，也不是应收+已收）"
    );
    assert_eq!(report.total_unsettled, dec("700"));
    assert_eq!(report.overdue_unsettled, Decimal::ZERO, "未到期的钱不算逾期");
    assert_eq!(report.overdue_ratio, Decimal::ZERO);
    // 已收 300 只在「已收合计」里出现一次
    assert_eq!(report.total_received, dec("300"));
    assert_eq!(report.details[0].bucket, "NOT_DUE");
    assert_eq!(report.details[0].overdue_days, -56);
}

/// ③ 跨期：**同一张单、同一个余额**，基准日推进只改档位。
///
/// 到期 2026-09-15 / 应收 1000 / 已收 300（未收 700）：
///
/// | 基准日 | 逾期 | 档位 |
/// |:--|--:|:--|
/// | 2026-09-05 | −10 | 未到期 |
/// | 2026-09-25 | +10 | 1-30 天 |
/// | 2026-11-15 | +61 | 61-90 天 |
///
/// 三次的金额都必须是 700 —— 谁把「跨期」实现成「按基准日重算已收 / 按比例摊销」，
/// 这里立刻红。
#[test]
fn cross_period_base_date_moves_bucket_but_not_balance() {
    let docs = [PureDoc {
        customer: "a22b0000-0000-4000-8000-000000000004",
        customer_name: "丁客户",
        no: "AR-PURE-0004",
        amount: "1000",
        received: "300",
        due: "2026-09-15",
        period: "2026-09",
    }];
    let cases = [
        ("2026-09-05", -10i64, "NOT_DUE", "未到期"),
        ("2026-09-25", 10, "D0_30", "1-30 天"),
        ("2026-11-15", 61, "D61_90", "61-90 天"),
    ];
    for (as_of_text, overdue, key, label) in cases {
        let as_of = date(as_of_text);
        let report = assemble(
            AsOf {
                date: as_of,
                source: payable_aging::AS_OF_QUERY,
            },
            None,
            None,
            snapshot_of(as_of, &docs),
            "云枢",
        );
        let detail = &report.details[0];
        assert_eq!(detail.overdue_days, overdue, "基准日 {as_of_text} 的逾期天数");
        assert_eq!(detail.bucket, key, "基准日 {as_of_text} 的档位");
        assert_eq!(detail.bucket_label, label);
        assert_eq!(
            detail.unsettled_amount,
            dec("700"),
            "基准日 {as_of_text}：余额不随基准日变"
        );
        assert_eq!(
            bucket_amount(&report, key),
            dec("700"),
            "基准日 {as_of_text}：{label} 档里必须是 700"
        );
        assert_eq!(report.total_unsettled, dec("700"), "基准日 {as_of_text}：未收合计");
        // 每次只有这一档有钱，其余六档必须是 0（跨期不许「一天摊两份」）
        assert_eq!(
            report.summary.iter().filter(|cell| !cell.unpaid.is_zero()).count(),
            1,
            "基准日 {as_of_text}：只应有一个非零档"
        );
    }
}

/// ④ 跨期收款的边界是**已决策**的：本表是「当前余额快照」，`hints` 必须把这句话
///    写给用户看（前端直接展示 `hints`）。谁把口径改成「按收款日期回溯」，这条会红 ——
///    改的人必须同时改文案，不能悄悄换语义。
#[test]
fn cross_period_receipt_does_not_rewrite_prior_balance_hint_is_explicit() {
    let docs = [PureDoc {
        customer: "a22b0000-0000-4000-8000-000000000005",
        customer_name: "戊客户",
        no: "AR-PURE-0005",
        amount: "1000",
        received: "300",
        due: "2026-09-15",
        period: "2026-09",
    }];
    let as_of = date("2026-09-30");
    let report = assemble(
        AsOf {
            date: as_of,
            source: payable_aging::AS_OF_QUERY,
        },
        Some("2026-09"),
        Some("OPEN".to_string()),
        snapshot_of(as_of, &docs),
        "云枢",
    );

    let current_balance_hint = report
        .hints
        .iter()
        .find(|hint| hint.contains("当前") && hint.contains("未收余额"))
        .unwrap_or_else(|| panic!("口径提示必须写明「金额是当前未收余额」：{:?}", report.hints));
    assert!(
        current_balance_hint.contains("不会回滚"),
        "提示必须明确「基准日之后的收款不会回滚成历史值」，实际：{current_balance_hint}"
    );
    assert!(
        current_balance_hint.contains("所属会计期间"),
        "提示必须写明单据按期间截断的规则，实际：{current_balance_hint}"
    );
    // 另一半口径（纳入哪些状态 / 分档刻度）也必须在同一份提示里
    assert!(report.hints.iter().any(|hint| hint.contains("OPEN / PARTIAL")));
    assert!(report.hints.iter().any(|hint| hint.contains("30/60/90/180/365")));
    // 基准日在 9 月末、余额已经是「收过 300 之后」的 700 —— 这就是上面那句话的含义
    assert_eq!(report.total_unsettled, dec("700"));
    assert_eq!(report.total_received, dec("300"));
    assert_eq!(report.as_of, as_of);
    assert_eq!(report.period_status.as_deref(), Some("OPEN"));
}

// ==================== 真库用例（需 `ERP_DB_URL`，缺省 SKIP） ====================

/// 客户编码（清场按它收；主数据走 `test_seed::ensure_customer`，不手写 INSERT —— 见
/// `docs/architecture/P2-62-数据准备脚本化.md` 与 `scripts/verify/test_seed_gate.sh` 的预算闸）。
const CUSTOMER_CODE: &str = "AG22-CUST";
/// 客户名。
const CUSTOMER_NAME: &str = "AG22 部分收款客户";

/// 应收 1000，期间 2026-08，到期 2026-08-31（基准日逾期 66 天 ⇒ 61-90 档）。
const DOC_A: &str = "a22a0001-0000-4000-8000-000000000001";
/// 应收 1000，期间 2026-09，到期 2026-09-30（逾期 36 天 ⇒ 31-60 档）。
const DOC_B: &str = "a22a0001-0000-4000-8000-000000000002";
/// 应收 800，期间 2026-10，到期 2026-10-20（逾期 16 天 ⇒ 1-30 档）。
const DOC_C: &str = "a22a0001-0000-4000-8000-000000000003";
/// 应收 600，期间 2026-11，到期 2026-12-05（未到期 30 天 ⇒ 未到期档）。
const DOC_D: &str = "a22a0001-0000-4000-8000-000000000004";

async fn exec(statement: &str) {
    db().exec_sql(statement.to_string(), vec![])
        .await
        .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
}

/// 夹具客户（主数据只走 `test_seed::ensure_customer`：幂等、id 由 code 派生，跨用例稳定）。
async fn customer() -> Uuid {
    crate::test_seed::ensure_customer(CUSTOMER_CODE, CUSTOMER_NAME).await
}

/// 清场 + 四张应收（已收 0）。
///
/// 清场顺序不能改：`biz_invoice / biz_receipt_allocation / biz_receipt_refund_allocation`
/// 都挂着 `biz_receivable.id`（RESTRICT），直接删应收会 23503（与 `receivable_aging::tests`
/// 踩过的同一个坑），所以先「解绑发票 + 删核销明细」再删应收。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    exec("UPDATE biz_invoice SET receivable_id = NULL WHERE receivable_id IS NOT NULL").await;
    exec("DELETE FROM biz_receipt_refund_allocation WHERE receivable_id IS NOT NULL").await;
    exec("DELETE FROM biz_receipt_allocation WHERE receivable_id IS NOT NULL").await;
    exec("DELETE FROM biz_receivable").await;
    // 收款 / 信用台账先按客户编码清（客户行还在，用子查询拿 id，不必先建客户）
    exec(
        "DELETE FROM biz_receipt WHERE customer_id IN \
            (SELECT id FROM biz_customer WHERE code LIKE 'AG22-%')",
    )
    .await;
    exec(
        "DELETE FROM biz_credit_log WHERE customer_id IN \
            (SELECT id FROM biz_customer WHERE code LIKE 'AG22-%')",
    )
    .await;
    exec("DELETE FROM biz_status_log WHERE entity_type IN ('RECEIVABLE', 'RECEIPT')").await;
    exec("DELETE FROM biz_customer WHERE code LIKE 'AG22-%'").await;

    let customer = customer().await;

    let rows = [
        ("AR-AG22-0001", DOC_A, "1000", "2026-08-31", "2026-08"),
        ("AR-AG22-0002", DOC_B, "1000", "2026-09-30", "2026-09"),
        ("AR-AG22-0003", DOC_C, "800", "2026-10-20", "2026-10"),
        ("AR-AG22-0004", DOC_D, "600", "2026-12-05", "2026-11"),
    ];
    for (ar_no, id, amount, due, period) in rows {
        exec(&format!(
            "INSERT INTO biz_receivable \
                (id, ar_no, customer_id, source_type, source_id, source_no, currency_code, \
                 amount, received_amount, unsettled_amount, credit_days, due_date, period_code, \
                 status, created_at, updated_at) \
             VALUES ('{id}'::uuid, '{ar_no}', '{customer}'::uuid, 'MANUAL', gen_random_uuid(), \
                     'MANUAL', 'CNY', {amount}::numeric, 0, {amount}::numeric, 30, \
                     '{due}'::date, '{period}', 'OPEN', now(), now())"
        ))
        .await;
    }
    Some(guard)
}

/// 登记一张收款单（走真接口：取号 / 期间闸 / 汇率锁定 / 状态日志全部生效）。
async fn create_receipt(receipt_date: &str, amount: &str) -> Uuid {
    let created = receipt::create(
        None,
        CreateReceiptReq {
            customer_id: customer().await.to_string(),
            receipt_date: Some(receipt_date.to_string()),
            amount: dec(amount),
            method: Some("BANK_TRANSFER".to_string()),
            bank_account: Some("6222 0000 0000 1111".to_string()),
            remark: Some("M1-22 部分收款夹具".to_string()),
            currency_code: None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("登记收款失败（{receipt_date} / {amount}）：{error:?}"));
    Uuid::parse_str(&created.receipt.id).expect("收款单 id 合法")
}

/// 把一张收款核销到若干张应收（走真接口：锁应收 / 校验可核销余额 / 回写余额与状态）。
async fn allocate(receipt_id: &Uuid, items: &[(&str, &str)]) -> std::result::Result<(), String> {
    let req = AllocateReq {
        items: items
            .iter()
            .map(|(receivable_id, amount)| AllocateItemReq {
                receivable_id: (*receivable_id).to_string(),
                amount: dec(amount),
                remark: None,
            })
            .collect(),
    };
    receipt::allocate(None, &receipt_id.to_string(), req)
        .await
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

/// ⑤⑥ 共用的部分收款夹具：一笔 700 核两张（300 + 200，剩 200 未核销）+ 一笔 500 核一张。
///
/// 收款日期刻意留在 **2026-10**：⑤ 的基准日 2026-11-05 在它之后（余额已经减掉），
/// ⑥ 的基准日 2026-09-30 在它之前（用来证明「不回滚历史」）。
async fn seed_partial_receipts() -> (Uuid, Uuid) {
    let first = create_receipt("2026-10-15", "700").await;
    allocate(&first, &[(DOC_A, "300"), (DOC_B, "200")])
        .await
        .expect("一笔收款核两张应收应成功");
    let second = create_receipt("2026-10-20", "500").await;
    allocate(&second, &[(DOC_C, "500")])
        .await
        .expect("第二笔收款核一张应收应成功");
    (first, second)
}

/// 库里的一张应收（三个金额 + 状态）。
async fn receivable_state(id: &str) -> (Decimal, Decimal, String) {
    let sql = format!("SELECT received_amount, unsettled_amount, status FROM biz_receivable WHERE id = '{id}'::uuid");
    let row = db()
        .query_first_sql(sql.clone(), vec![])
        .await
        .unwrap_or_else(|error| panic!("查询失败：{error}\n{sql}"))
        .unwrap_or_else(|| panic!("应收 {id} 不存在"));
    (
        row.get_typed("received_amount").expect("读已收"),
        row.get_typed("unsettled_amount").expect("读未收"),
        row.get_typed("status").expect("读状态"),
    )
}

/// 出表里某张单的明细行。
fn detail_of<'a>(report: &'a ReceivableAgingRes, ar_no: &str) -> &'a receivable_aging::ReceivableAgingDetailRes {
    report.details.iter().find(|row| row.ar_no == ar_no).unwrap_or_else(|| {
        panic!(
            "明细里没有 {ar_no}，实际：{:?}",
            report.details.iter().map(|row| row.ar_no.as_str()).collect::<Vec<_>>()
        )
    })
}

/// 真库：**部分收款后余额逐单分摊**（⑤）。
///
/// 一笔收款核两张应收（300 + 200），另一笔核第三张（500）：
///
/// | 单 | 应收 | 已收 | 未收 | 到期日 | 逾期 | 档位 |
/// |:--|--:|--:|--:|:--|--:|:--|
/// | 0001 | 1000 | 300 | 700 | 2026-08-31 | 66 | 61-90 |
/// | 0002 | 1000 | 200 | 800 | 2026-09-30 | 36 | 31-60 |
/// | 0003 | 800 | 500 | 300 | 2026-10-20 | 16 | 1-30 |
/// | 0004 | 600 | 0 | 600 | 2026-12-05 | −30 | 未到期 |
/// | 合计 | 3400 | 1000 | 2400 | | | |
///
/// 关键点：核销明细里写的是 300 / 200 / 500，账龄表里的金额是 700 / 800 / 300 ——
/// **余额不是核销额**，也不是按核销比例把余额拆到多个档（那是「余额分摊」的错解）。
#[tokio::test]
async fn db_partial_receipts_allocate_balance_per_document() {
    let Some(_guard) = setup().await else { return };
    let (first, second) = seed_partial_receipts().await;

    // 写侧与账龄读的是同一份余额：先核库里三张单的收回写（真接口的产物）
    assert_eq!(
        receivable_state(DOC_A).await,
        (dec("300"), dec("700"), "PARTIAL".to_string())
    );
    assert_eq!(
        receivable_state(DOC_B).await,
        (dec("200"), dec("800"), "PARTIAL".to_string())
    );
    assert_eq!(
        receivable_state(DOC_C).await,
        (dec("500"), dec("300"), "PARTIAL".to_string())
    );
    assert_eq!(
        receivable_state(DOC_D).await,
        (Decimal::ZERO, dec("600"), "OPEN".to_string())
    );

    // 收款头：第一笔只核了 500 / 700（剩 200 未核销也是合法的「部分核销」）
    let first_detail = receipt::receipt_detail(first).await.expect("读收款详情");
    assert_eq!(first_detail.receipt.allocated_amount, dec("500"));
    assert_eq!(first_detail.allocations.len(), 2, "一笔收款核两张应收 = 两条核销明细");
    let second_detail = receipt::receipt_detail(second).await.expect("读收款详情");
    assert_eq!(second_detail.receipt.allocated_amount, dec("500"));
    assert_eq!(second_detail.allocations.len(), 1);

    // 账龄（走接口整条链：基准日解析 → 快照事务 → 透视 → 组装）
    let report = receivable_aging::build_report(&ReceivableAgingQuery {
        as_of: Some(AS_OF.to_string()),
        period: None,
    })
    .await
    .expect("应收账龄应出表");

    assert_eq!(report.doc_count, 4);
    assert_eq!(report.customer_count, 1);
    assert_eq!(report.details.len(), 4);

    // 逐单：金额三列 + 档位 + 状态
    let expect = [
        ("AR-AG22-0001", "1000", "300", "700", 66, "D61_90", "61-90 天"),
        ("AR-AG22-0002", "1000", "200", "800", 36, "D31_60", "31-60 天"),
        ("AR-AG22-0003", "800", "500", "300", 16, "D0_30", "1-30 天"),
        ("AR-AG22-0004", "600", "0", "600", -30, "NOT_DUE", "未到期"),
    ];
    for (ar_no, amount, received, unsettled, days, key, label) in expect {
        let row = detail_of(&report, ar_no);
        assert_eq!(row.amount, dec(amount), "{ar_no} 应收金额");
        assert_eq!(row.received_amount, dec(received), "{ar_no} 已收金额");
        assert_eq!(
            row.unsettled_amount,
            dec(unsettled),
            "{ar_no} 未收金额（余额分摊的分子）"
        );
        assert_eq!(row.overdue_days, days, "{ar_no} 逾期天数");
        assert_eq!(row.bucket, key, "{ar_no} 档位");
        assert_eq!(row.bucket_label, label, "{ar_no} 档位中文名");
        assert_eq!(
            row.unsettled_amount,
            row.amount - row.received_amount,
            "{ar_no} 未收 ≠ 应收 − 已收"
        );
    }

    // 汇总：七个档位逐档，缺档给 0（不是少列）
    let amounts: Vec<Decimal> = report.summary.iter().map(|cell| cell.unpaid).collect();
    assert_eq!(
        amounts,
        vec![
            dec("600"),
            dec("300"),
            dec("800"),
            dec("700"),
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO
        ],
        "未到期 600 / 1-30 300 / 31-60 800 / 61-90 700"
    );
    assert_eq!(report.summary.len(), 7);
    let bucket_sum: Decimal = report.summary.iter().map(|cell| cell.unpaid).sum();
    assert_eq!(bucket_sum, dec("2400"), "各档之和 = 未收合计");
    assert_eq!(report.total_unsettled, dec("2400"));
    assert_eq!(report.total_amount, dec("3400"), "应收合计含已收部分");
    assert_eq!(report.total_received, dec("1000"), "300 + 200 + 500");
    assert_eq!(
        report.total_amount - report.total_received,
        report.total_unsettled,
        "应收 − 已收 = 未收"
    );
    assert_eq!(report.overdue_unsettled, dec("1800"), "只有未到期那 600 不算逾期");
    assert_eq!(report.overdue_ratio, dec("75.00"));

    // 客户透视：格子数 = 表头数，各档之和 = 未收合计，最长逾期取 max
    let customer = &report.customers[0];
    assert_eq!(customer.customer_name, "AG22 部分收款客户");
    assert_eq!(customer.doc_count, 4);
    assert_eq!(customer.buckets.len(), report.buckets.len());
    assert_eq!(
        customer.buckets.iter().map(|cell| cell.unpaid).sum::<Decimal>(),
        customer.total_unsettled
    );
    assert_eq!(customer.max_overdue_days, 66);

    // 反证：核销明细的金额（300 / 200 / 500）**不许**出现在账龄档位里，
    // 也不许把已收的 1000 单独摊进「未到期」（那样未到期会是 1600）
    for cell in &report.summary {
        assert_ne!(cell.unpaid, dec("1000"), "已收合计不许进档位");
    }
    assert_eq!(
        bucket_amount(&report, "NOT_DUE"),
        dec("600"),
        "未到期只有 0004 的未收 600"
    );
}

/// 真库：**跨期**（⑥）—— 基准日在收款之前时，余额是「当前值」（不回滚），单据按期间截断。
///
/// 收款日期 2026-10-15 / 2026-10-20，基准日取 2026-09-30：
/// * 0003（期间 2026-10）/ 0004（期间 2026-11）被期间截断 ⇒ 只剩 0001 / 0002；
/// * 0001 / 0002 的余额已经是**核销之后**的 700 / 800（300 / 200 是 10 月收的）——
///   这就是 `hints` 那句「基准日之后发生的收款不会回滚成历史值」的含义；
/// * 档位随基准日重算：0001（到期 08-31）逾期 30 天 ⇒ 1-30；0002（到期 09-30）当天 ⇒ 未到期。
///
/// 再把基准日推到 2026-10-31 做对照：0003 进表（期间 2026-10），0004 仍被截断 ⇒ 3 张。
#[tokio::test]
async fn db_cross_period_receipt_does_not_restate_prior_aging() {
    let Some(_guard) = setup().await else { return };
    seed_partial_receipts().await;

    // ---------- ① 基准日 2026-09-30（在收款之前） ----------
    let report = receivable_aging::build_report(&ReceivableAgingQuery {
        as_of: Some("2026-09-30".to_string()),
        period: Some("2026-09".to_string()),
    })
    .await
    .expect("9 月末账龄应出表");

    assert_eq!(report.as_of, date("2026-09-30"));
    assert_eq!(report.doc_count, 2, "10 / 11 月的两张被期间截断");
    assert!(
        report.details.iter().all(|row| row.period_code.as_str() <= "2026-09"),
        "明细里不许出现基准月之后的单据：{:?}",
        report
            .details
            .iter()
            .map(|row| row.period_code.as_str())
            .collect::<Vec<_>>()
    );
    // 跨期收款不回滚：9 月末表里的余额已经是核销后的 700 / 800
    assert_eq!(report.total_unsettled, dec("1500"), "700 + 800（10 月收的钱已经减掉）");
    assert_eq!(report.total_received, dec("500"), "300 + 200：已收取当前值");
    assert_eq!(report.total_amount, dec("2000"));
    // 档位按 9-30 重算：0001 逾期 30 天、0002 到期当天
    assert_eq!(detail_of(&report, "AR-AG22-0001").overdue_days, 30);
    assert_eq!(detail_of(&report, "AR-AG22-0001").bucket, "D0_30");
    assert_eq!(detail_of(&report, "AR-AG22-0002").overdue_days, 0);
    assert_eq!(detail_of(&report, "AR-AG22-0002").bucket, "NOT_DUE");
    let amounts: Vec<Decimal> = report.summary.iter().map(|cell| cell.unpaid).collect();
    assert_eq!(
        amounts,
        vec![
            dec("800"),
            dec("700"),
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO
        ]
    );
    // 出参自带这句话（前端直接展示），语义变更必须同步改文案
    assert!(
        report.hints.iter().any(|hint| hint.contains("不会回滚")),
        "出参必须写明「基准日之后的收款不会回滚」：{:?}",
        report.hints
    );

    // ---------- ② 基准日 2026-10-31（收款之后） ----------
    let later = receivable_aging::build_report(&ReceivableAgingQuery {
        as_of: Some("2026-10-31".to_string()),
        period: None,
    })
    .await
    .expect("10 月末账龄应出表");
    assert_eq!(later.doc_count, 3, "10 月的 0003 进表，11 月的 0004 仍被截断");
    assert_eq!(later.total_unsettled, dec("1800"), "700 + 800 + 300");
    assert_eq!(detail_of(&later, "AR-AG22-0001").overdue_days, 61);
    assert_eq!(detail_of(&later, "AR-AG22-0001").bucket, "D61_90");
    assert_eq!(detail_of(&later, "AR-AG22-0003").overdue_days, 11);
    assert_eq!(detail_of(&later, "AR-AG22-0003").bucket, "D0_30");
    assert!(
        later.details.iter().all(|row| row.ar_no != "AR-AG22-0004"),
        "11 月的单不许进 10 月末的账龄"
    );

    // ---------- ③ 余额在三次基准日之间只被「收款」改变，不被「基准日」改变 ----------
    // 同一张 0001：9-30 是 700、10-31 是 700、11-05 也是 700（只有逾期天数在走）
    let full = receivable_aging::build_report(&ReceivableAgingQuery {
        as_of: Some(AS_OF.to_string()),
        period: None,
    })
    .await
    .expect("11 月账龄应出表");
    for (label, value) in [
        ("2026-09-30", detail_of(&report, "AR-AG22-0001").unsettled_amount),
        ("2026-10-31", detail_of(&later, "AR-AG22-0001").unsettled_amount),
        (AS_OF, detail_of(&full, "AR-AG22-0001").unsettled_amount),
    ] {
        assert_eq!(value, dec("700"), "基准日 {label} 的 0001 余额");
    }
    assert_eq!(
        detail_of(&full, "AR-AG22-0001").overdue_days,
        66,
        "同一个余额在 11-05 已经逾期 66 天（档位随基准日推进）"
    );
}

/// 真库：**同一张收款重复核销 = 改金额**（UPSERT 改金额），余额只按**差额**动（⑤b）。
///
/// 「部分核销」的第二种形态：钱还没定下来，财务先把 300 挂上去，改成 500（不是再录一笔）。
/// 这条链上最容易错的是回写算术 —— 用「本次金额」而不是「差额」累加已收：
///
/// * 正确：300 → 改 500 ⇒ 已收 = 300 + (500−300) = 500，未收 = 500；
/// * 错法：300 → 改 500 ⇒ 已收 = 300 + 500 = 800，未收 = 200（账龄表直接少 300）。
///
/// 同时钉住「核销明细只有一行」（`uk_biz_rcpt_alloc_pair`：改金额不是插第二行，
/// 否则 `SUM(amount)` 会把同一笔钱算两遍）。
#[tokio::test]
async fn db_reallocating_same_receipt_moves_balance_by_delta_only() {
    let Some(_guard) = setup().await else { return };

    let receipt_id = create_receipt("2026-10-15", "700").await;
    allocate(&receipt_id, &[(DOC_A, "300")]).await.expect("首次核销 300");
    assert_eq!(
        receivable_state(DOC_A).await,
        (dec("300"), dec("700"), "PARTIAL".to_string())
    );

    // 改金额：300 → 500（差额 +200）
    allocate(&receipt_id, &[(DOC_A, "500")])
        .await
        .expect("改核销金额到 500");
    assert_eq!(
        receivable_state(DOC_A).await,
        (dec("500"), dec("500"), "PARTIAL".to_string()),
        "已收按差额累加（300 + 200 = 500），不是「上一次 + 本次金额」"
    );
    let detail = receipt::receipt_detail(receipt_id).await.expect("读收款详情");
    assert_eq!(
        detail.allocations.len(),
        1,
        "同一 (收款, 应收) 只有一行，改金额不动行数"
    );
    assert_eq!(detail.receipt.allocated_amount, dec("500"));

    // 超过收款金额（700）→ 拒，且两个金额都要点名
    let error = allocate(&receipt_id, &[(DOC_A, "800")])
        .await
        .expect_err("核销总额不许超过收款金额");
    assert!(error.contains("700"), "错误文案要点名收款金额，实际：{error}");
    assert_eq!(
        receivable_state(DOC_A).await,
        (dec("500"), dec("500"), "PARTIAL".to_string()),
        "被拒的核销不许改动余额"
    );

    // 账龄：0001 的未收是 500（不是 800 / 200），其余三张没被动过
    let report = receivable_aging::build_report(&ReceivableAgingQuery {
        as_of: Some(AS_OF.to_string()),
        period: None,
    })
    .await
    .expect("账龄应出表");
    assert_eq!(detail_of(&report, "AR-AG22-0001").unsettled_amount, dec("500"));
    assert_eq!(detail_of(&report, "AR-AG22-0001").received_amount, dec("500"));
    assert_eq!(detail_of(&report, "AR-AG22-0001").status, "PARTIAL");
    assert_eq!(report.total_received, dec("500"));
    assert_eq!(report.total_unsettled, dec("2900"), "500 + 1000 + 800 + 600");
    assert_eq!(bucket_amount(&report, "D61_90"), dec("500"), "余额 500 落在 61-90 档");
}

/// 真库：**两次部分核销把单收清**（⑦）—— 收清的单整体退出账龄表。
///
/// 0004（600）分两笔收（200 + 400）：
/// * 库层状态 `OPEN → PARTIAL → SETTLED`，`unsettled_amount = 0`（`ck_biz_ar_settled` 兜底）；
/// * 账龄表里它**整行消失**（不是留一行 0），它的已收 600 也**不**进 `total_received`
///   （表里只统计未收清的单，收清的单连已收一起退出 —— 与 `SETTLED` 不进账龄同口径）；
/// * 收清之后再核 1 元必须 409（没有余额可核），不能静默写 0。
#[tokio::test]
async fn db_two_partial_receipts_settle_document_and_it_leaves_aging() {
    let Some(_guard) = setup().await else { return };

    let first = create_receipt("2026-11-03", "200").await;
    allocate(&first, &[(DOC_D, "200")]).await.expect("第一次部分核销");
    assert_eq!(
        receivable_state(DOC_D).await,
        (dec("200"), dec("400"), "PARTIAL".to_string())
    );

    let second = create_receipt("2026-11-04", "400").await;
    allocate(&second, &[(DOC_D, "400")])
        .await
        .expect("第二次部分核销（收清）");
    assert_eq!(
        receivable_state(DOC_D).await,
        (dec("600"), Decimal::ZERO, "SETTLED".to_string())
    );

    let report = receivable_aging::build_report(&ReceivableAgingQuery {
        as_of: Some(AS_OF.to_string()),
        period: None,
    })
    .await
    .expect("账龄应出表");

    assert_eq!(report.doc_count, 3, "收清的那张退出账龄表");
    assert!(
        report.details.iter().all(|row| row.ar_no != "AR-AG22-0004"),
        "SETTLED 的单不许留在明细里（哪怕金额是 0）"
    );
    assert_eq!(report.total_unsettled, dec("2800"), "1000 + 1000 + 800");
    assert_eq!(report.total_amount, dec("2800"), "收清单的应收金额也退出");
    assert_eq!(
        report.total_received,
        Decimal::ZERO,
        "收清单的已收 600 不进表：账龄只算「还在账上的单」的已收"
    );
    // 未到期那一档也跟着消失（0004 是唯一未到期的单）
    assert_eq!(bucket_amount(&report, "NOT_DUE"), Decimal::ZERO);
    assert_eq!(report.overdue_ratio, dec("100.00"), "剩下的三张全部逾期");

    // 收清之后再核 1 元 → 409（`has_balance` 为假），不许静默成功
    let third = create_receipt("2026-11-05", "1").await;
    let error = allocate(&third, &[(DOC_D, "1")])
        .await
        .expect_err("收清的应收不能再核销");
    assert!(
        error.contains("SETTLED") || error.contains("不能核销"),
        "错误文案应说明「已结清不能核销」，实际：{error}"
    );
    // 失败不许留痕：余额仍是 600 / 0，收款单也还是 0 核销
    assert_eq!(
        receivable_state(DOC_D).await,
        (dec("600"), Decimal::ZERO, "SETTLED".to_string())
    );
    let untouched = receipt::receipt_detail(third).await.expect("读收款详情");
    assert_eq!(untouched.receipt.allocated_amount, Decimal::ZERO);
    assert!(untouched.allocations.is_empty());
}
