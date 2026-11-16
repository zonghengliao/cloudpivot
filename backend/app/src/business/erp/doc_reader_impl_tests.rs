//! P2-33 的用例：ERP 侧 18 个 `DocReader` 实现的**契约**与**真库查询**。
//!
//! # 两类用例，判据不同
//!
//! | 类 | 需要库 | 钉住什么 |
//! |:--|:--:|:--|
//! | 契约（`#[test]`） | ❌ | 注册键集合 = `internal_control` 的 17 个 `governed` 分支；`biz_type` 落库码与各 `*_bridge::BIZ_TYPE` 常量逐字一致；重复注册被点名拒绝；`doc_status` 的四段 SQL 标识符在迁移里真实存在（防「表名 / 列名拼错 → 运行期 `column does not exist`」） |
//! | 真库（`ERP_DB_URL` 开关） | ✅ | 18 条 `doc_status` 的 SQL 真的能跑；采购单 / 报销单各一条**含数据**的往返（报销单专门钉住「制单人取 `reporter_id` 而不是 `create_by`」） |
//!
//! 真库用例不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。
//! 交付前必须真跑一遍：
//!
//! ```bash
//! # 99 开发机（本机无 PG）→ 连 145
//! HOST=172.18.10.145
//! PGPASSWORD=*** createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_p233_docreader
//! ERP_DB_URL="postgres://cloudpivot:***@$HOST:5432/cp_p233_docreader" \
//!   cargo test --lib business::erp::doc_reader_impl -- --test-threads=1
//! ```

use std::collections::BTreeSet;
use std::sync::Arc;

use cloudpivot_doc::FlowBizType;

use crate::business::erp::asset::asset_bridge;
use crate::business::erp::budget::budget_bridge;
use crate::business::erp::contract::contract_bridge;
use crate::business::erp::cost::cost_period_bridge;
use crate::business::erp::doc_reader_impl::{self, ErpDoc, ErpDocReader};
use crate::business::erp::expense::expense_bridge;
use crate::business::erp::payable::payable_bridge;
use crate::business::erp::purchase::{purchase_bridge, purchase_return_bridge};
use crate::business::erp::receipt_refund_bridge;
use crate::business::erp::requisition_bridge;
use crate::business::erp::sales::{quotation_approval_bridge, sales_approval_bridge};
use crate::business::erp::stockin::stockin_bridge;
use crate::business::erp::stocktake::stocktake_bridge;
use crate::business::erp::voucher::voucher_bridge;
use crate::business::flow::doc_reader::{self, DocRegistry};

// ==================== 契约：注册键 ====================

/// 「一条审批一个实现」的清单：`FlowBizType` + 该桥现有的 `BIZ_TYPE` 常量。
///
/// 这张表是**双向对账**的锚点：左边是 `flow::internal_control::of_biz` 今天 `governed`
/// 的 17 个分支（一个不多一个不少），右边是各桥自己认领审批实例时用的落库码。
/// 任何一边改了而另一边没跟上，用例就红 —— 这正是 P2-34 换调用点时的安全网。
fn bridge_codes() -> Vec<(FlowBizType, &'static str)> {
    vec![
        (FlowBizType::PoApprove, purchase_bridge::BIZ_TYPE),
        (FlowBizType::StockInPost, stockin_bridge::POST_BIZ_TYPE),
        (FlowBizType::StockInCancel, stockin_bridge::BIZ_TYPE),
        (FlowBizType::StockTakePost, stocktake_bridge::BIZ_TYPE),
        (FlowBizType::PayablePay, payable_bridge::BIZ_TYPE),
        (FlowBizType::PayableCancel, payable_bridge::CANCEL_BIZ_TYPE),
        (FlowBizType::VoucherReverse, voucher_bridge::BIZ_TYPE),
        (FlowBizType::ContractApprove, contract_bridge::BIZ_TYPE),
        (FlowBizType::BudgetApprove, budget_bridge::BIZ_TYPE),
        (FlowBizType::AssetDisposal, asset_bridge::BIZ_TYPE),
        (FlowBizType::Expense, expense_bridge::BIZ_TYPE),
        (FlowBizType::SoApprove, sales_approval_bridge::BIZ_TYPE),
        (FlowBizType::PrApprove, requisition_bridge::BIZ_TYPE),
        (FlowBizType::PrtApprove, purchase_return_bridge::BIZ_TYPE),
        (FlowBizType::CostSettle, cost_period_bridge::BIZ_TYPE),
        (FlowBizType::ReceiptRefund, receipt_refund_bridge::BIZ_TYPE),
        // M3-04：报价单审批（发出前置，受内控约束：制单人不可自审）
        (FlowBizType::QuotationApprove, quotation_approval_bridge::BIZ_TYPE),
    ]
}

/// 17 个受控实现与 17 个 `governed` 分支**一一对应**，外加 1 个「只接回调」的调拨单，
/// 且落库码与桥常量逐字一致（M3-04 起实现数 = 18）。
#[test]
fn supported_set_matches_governed_branches() {
    let expected = bridge_codes();
    assert_eq!(expected.len(), 17, "登记表本身应恰好 17 条");

    let implemented: Vec<FlowBizType> = ErpDoc::ALL.iter().map(|doc| doc.biz_type()).collect();
    assert_eq!(
        implemented.len(),
        18,
        "ErpDoc::ALL 应恰好 18 个实现（17 受控 + 调拨单）"
    );

    // ⓪ P2-34：多出来的那一个必须**恰好**是 STOCK_TRANSFER —— 它只接终态回调、不做内控排除。
    //    多一个别的（或少了它）都说明受控集合与注册集合漂移了，那正是本用例要拦的事。
    let governed: BTreeSet<FlowBizType> = expected.iter().map(|(biz_type, _)| *biz_type).collect();
    let extra: BTreeSet<FlowBizType> = implemented
        .iter()
        .copied()
        .filter(|biz_type| !governed.contains(biz_type))
        .collect();
    assert_eq!(
        extra,
        BTreeSet::from([FlowBizType::StockTransfer]),
        "注册集合里比受控集合多出来的必须只有调拨单（实际多出：{extra:?}）"
    );

    // ① 同一 biz_type 不许被两个实现抢（注册表会在运行期报错，这里提前钉死）
    let unique: BTreeSet<FlowBizType> = implemented.iter().copied().collect();
    assert_eq!(
        unique.len(),
        implemented.len(),
        "ErpDoc::ALL 里出现了重复的 biz_type：{:?}",
        implemented
    );

    // ② 逐个对账：实现存在 + 落库码与桥常量逐字相同（typo 会在这里红，而不是等到审批线上）
    for (biz_type, code) in &expected {
        let doc = ErpDoc::ALL
            .iter()
            .find(|doc| doc.biz_type() == *biz_type)
            .unwrap_or_else(|| panic!("{}（{biz_type:?}）没有对应的实现", code));
        assert_eq!(
            doc.biz_type().as_str(),
            *code,
            "{doc:?} 的落库码与桥常量不一致（桥认领实例用的就是后者）"
        );
    }

    // ③ 反向：受控名单之外**只允许**调拨单 —— 它是 P2-34 补的「只接回调、不做内控排除」那条：
    //    终态回调从 `flow::task_bridge` 搬进 ERP 后需要一个入口被调起（注册表），
    //    而 `flow::internal_control::GOVERNED_CODES` 里仍然没有它（多拦一批人 = 改变生产行为）。
    let expected_types: BTreeSet<FlowBizType> = expected.iter().map(|(biz_type, _)| *biz_type).collect();
    for doc in ErpDoc::ALL {
        if doc.biz_type() == FlowBizType::StockTransfer {
            assert!(
                !expected_types.contains(&doc.biz_type()),
                "调拨单不许进受控名单：它今天没有内控闸（见 flow::internal_control::GOVERNED_CODES）"
            );
            continue;
        }
        assert!(
            expected_types.contains(&doc.biz_type()),
            "{doc:?}（{}）不在 internal_control 的 governed 名单里：多注册会让行为与今天不同",
            doc.biz_type().as_str()
        );
    }
}

/// 每个实现都带一个说得清的单据名（日志 / 断言里的人读标识，不是装饰）。
#[test]
fn every_implementation_names_its_doc() {
    for doc in ErpDoc::ALL {
        assert!(!doc.doc_name().is_empty(), "{doc:?} 缺单据名");
    }
}

// ==================== 契约：注册表 ====================

#[test]
fn registers_every_reader_once() {
    let mut registry = DocRegistry::new();
    assert!(registry.is_empty(), "空注册表应 is_empty");

    let count = doc_reader_impl::register_into(&mut registry).expect("18 个实现应能一次注册完");
    assert_eq!(count, 18, "register_into 应报告注册了 18 个（17 受控 + 调拨单）");
    assert_eq!(registry.len(), 18, "注册表里应有 18 个键");

    // 按落库编码取（P2-34 调用点最常用的入口）
    for doc in ErpDoc::ALL {
        let reader = registry
            .reader_of_code(doc.biz_type().as_str())
            .unwrap_or_else(|| panic!("{} 未注册", doc.biz_type().as_str()));
        assert_eq!(reader.biz_type(), doc.biz_type(), "取回的实现不是同一个 biz_type");
    }

    let registered: BTreeSet<FlowBizType> = registry.registered().into_iter().collect();
    assert_eq!(registered.len(), 18, "registered() 应给出 18 个键（17 受控 + 调拨单）");
    assert_eq!(
        registered,
        ErpDoc::ALL.iter().map(|doc| doc.biz_type()).collect::<BTreeSet<_>>(),
        "registered() 与 ErpDoc::ALL 应完全一致"
    );
}

/// 重复注册**不许静默覆盖**：两个实现抢一个键时谁赢取决于注册顺序，必须在启动期暴露。
#[test]
fn duplicate_registration_is_rejected_by_name() {
    let mut registry = DocRegistry::new();
    let reader = Arc::new(ErpDocReader::new(ErpDoc::PurchaseOrder));
    registry.register(reader).expect("首次注册应成功");

    let error = registry
        .register(Arc::new(ErpDocReader::new(ErpDoc::PurchaseOrder)))
        .expect_err("重复注册应被拒绝");
    assert!(
        error.contains(purchase_bridge::BIZ_TYPE),
        "报错应点名 biz_type，实际：{error}"
    );

    // 被拒绝之后注册表原样不动（没有把第一个实现顶掉）
    assert_eq!(registry.len(), 1);
    assert!(registry.reader_of_code(purchase_bridge::BIZ_TYPE).is_some());
}

/// 编码不认识 / 未注册 → `None`（**不报错**：历史实例的 biz_type 可能是废弃值，展示层要能降级）。
#[test]
fn unknown_code_is_none_not_error() {
    let mut registry = DocRegistry::new();
    doc_reader_impl::register_into(&mut registry).expect("注册应成功");

    assert!(
        registry.reader_of_code("NOT_A_BIZ_TYPE").is_none(),
        "不认识的编码应返回 None"
    );
    // P2-34：调拨单**已注册**（回调从 `flow::task_bridge` 搬进 ERP，需要一个入口被调起），
    // 但它不在 `flow::internal_control::GOVERNED_CODES` 里 —— 注册 ≠ 受内控约束。
    assert!(
        registry.reader_of_code(FlowBizType::StockTransfer.as_str()).is_some(),
        "STOCK_TRANSFER 必须已注册（否则审批终态的扇出到不了调拨单）"
    );
    assert!(
        !crate::business::flow::internal_control::GOVERNED_CODES.contains(&FlowBizType::StockTransfer.as_str()),
        "STOCK_TRANSFER 不许进受控集合：它今天没有内控闸，加进去 = 多拦一批人"
    );
    // 人力资源由另一张卡实现
    assert!(
        registry.reader_of_code(FlowBizType::HrLeave.as_str()).is_none(),
        "HR 的实现在 business::hr，不属于 ERP 侧"
    );
}

// ==================== 契约：快照取法（表 / 列语义） ====================

/// 「制单人」这一列不是一刀切的 `create_by`：报销单是 `reporter_id`。
///
/// 这条用例钉的是**内控口径**：`expense_bridge::internal_control_creator` 查的就是
/// `reporter_id`，快照若取 `create_by`，「详情里显示的制单人」与「被排除的那个人」会分成两个人。
#[test]
fn expense_takes_its_creator_from_reporter_id() {
    let expense = ErpDoc::Expense.spec();
    assert_eq!(expense.table, "biz_expense_report");
    assert_eq!(
        expense.creator, "reporter_id",
        "报销单的制单人是 reporter_id（代录人 create_by 不算）"
    );
    for doc in ErpDoc::ALL {
        if doc == ErpDoc::Expense {
            continue;
        }
        assert_eq!(doc.spec().creator, "create_by", "{doc:?} 的制单人列应与其他单据一致");
    }
}

/// 付款台账没有 `status` 列：`PAYABLE_PAY` 的状态取 `approval_status`。
#[test]
fn payable_pay_reads_approval_status() {
    let pay = ErpDoc::PayablePay.spec();
    assert_eq!(pay.table, "biz_payment");
    assert_eq!(pay.status, "approval_status", "biz_payment 只有审批态，没有账务态");
}

/// 收货过账与收货作废挂在**同一张单**上：两个实现、两个键、同一份快照。
#[test]
fn stock_in_post_and_cancel_share_one_table() {
    assert_eq!(
        ErpDoc::StockInPost.spec().table,
        ErpDoc::StockInCancel.spec().table,
        "过账与作废是同一张收货单的两条审批"
    );
    assert_ne!(
        ErpDoc::StockInPost.biz_type(),
        ErpDoc::StockInCancel.biz_type(),
        "两条审批必须是两个注册键"
    );
}

/// 状态文案对域外值也要给得出东西（脏数据不许把审批详情打空）。
#[test]
fn status_label_falls_back_for_unknown_values() {
    for doc in ErpDoc::ALL {
        let spec = doc.spec();
        let label = (spec.label)("NOT_A_REAL_STATUS");
        assert!(
            !label.is_empty(),
            "{doc:?} 的状态文案对域外值返回了空串（审批详情会空一格）"
        );
    }
}

// ==================== 契约：SQL 标识符真的存在（离线，防拼错） ====================

/// 迁移目录里的全部 `.sql` 文件（递归）。
fn migration_files() -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, files)?;
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("sql") {
                files.push(path);
            }
        }
        Ok(())
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut files = Vec::new();
    walk(&root, &mut files).unwrap_or_else(|error| panic!("扫不到迁移目录 {}：{error}", root.display()));
    files
}

/// 18 张单据表的单号 / 状态 / 制单人列，**都在迁移里真实存在**。
///
/// # 为什么离线也要查一遍
///
/// `DocSpec` 里四段标识符是拼进 SQL 的常量。拼错表名 / 列名的后果是**运行期**才炸
/// （`relation/column does not exist`），而那正好发生在「用户点审批」的时候。
/// 真库用例（下面的 `doc_status_sql_executes_for_every_doc`）能抓到，但它需要 `ERP_DB_URL`，
/// 在没配库的 CI 上是 SKIP —— 于是拼错会**假绿**。这条离线用例把同样的判据压到迁移文本上：
/// 迁移是表的唯一真相，写进去的列名一定在迁移里出现过。
///
/// # 扫描面断言
///
/// 目录读错时「一个都找不到 → 全都通过」是静默假绿，所以先断言扫到了足够多的文件与字节。
#[test]
fn doc_spec_identifiers_exist_in_migrations() {
    let files = migration_files();
    let mut total_bytes = 0usize;
    let mut corpus = String::new();
    for path in &files {
        let sql = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("读不到 {}：{error}", path.display()));
        total_bytes += sql.len();
        corpus.push_str(&sql);
    }
    assert!(
        files.len() >= 400 && total_bytes >= 2_000_000,
        "扫描面异常：{} 个 .sql / {total_bytes} 字节（预期 ≥400 个 / ≥2MB），读错目录会假绿",
        files.len()
    );

    let mut misses: Vec<String> = Vec::new();
    for doc in ErpDoc::ALL {
        let spec = doc.spec();
        for identifier in [spec.table, spec.doc_no, spec.status, spec.creator] {
            if !corpus.contains(identifier) {
                misses.push(format!("{doc:?}: 迁移里找不到标识符 `{identifier}`"));
            }
        }
        // 快照查询固定带 `deleted_at IS NULL`：表必须有这一列
        if !corpus.contains(&format!("CREATE TABLE IF NOT EXISTS {}", spec.table))
            && !corpus.contains(&format!("CREATE TABLE {}", spec.table))
        {
            misses.push(format!("{doc:?}: 迁移里没有 `{}` 的建表语句", spec.table));
        }
    }
    assert!(misses.is_empty(), "DocSpec 与迁移对不上：\n{}", misses.join("\n"));
}

// ==================== 进程级注册（启动期那条路） ====================

/// `doc_reader_impl::register_all()`（`bootstrap` 调的正是它）能填满**进程级**注册表。
///
/// # 为什么要锁
///
/// 注册表是进程级单例，与本文件 + `flow::doc_reader_tests` 的其他用例共用。
/// 两个模块各拿一把锁也挡不住彼此，所以锁是**共享**的
/// （[`doc_reader::GLOBAL_LOCK`]）：任何「reset + 注册 + 断言」的用例都必须拿同一把。
#[tokio::test]
async fn process_registry_is_filled_by_register_all() {
    let _guard = doc_reader::GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();
    assert_eq!(doc_reader::len(), 0, "reset 之后应为空");

    let count = doc_reader_impl::register_all().expect("启动期注册应成功");
    assert_eq!(count, 18);
    assert_eq!(doc_reader::len(), 18, "进程级注册表应有 18 个键（17 受控 + 调拨单）");

    // 分派助手（P2-34 的替换点）能按编码取到实现
    for doc in ErpDoc::ALL {
        assert!(
            doc_reader::is_registered(doc.biz_type()),
            "{} 应已注册（is_registered 是 governed 判定的替换点）",
            doc.biz_type().as_str()
        );
        assert!(doc_reader::reader_of_code(doc.biz_type().as_str()).is_some());
    }

    // 二次注册 = 编程错误：必须报错并点名，绝不静默覆盖
    let error = doc_reader_impl::register_all().expect_err("二次注册应被拒绝");
    assert!(error.contains("重复注册"), "报错应说明是重复注册，实际：{error}");

    doc_reader::reset_for_tests();
}

// ==================== 真库：18 条 doc_status 的 SQL 真的能跑 ====================

mod db {
    //! 真库用例（`ERP_DB_URL` 开关）。共用全局 `db()`，必须 `--test-threads=1` 串行跑。

    use nik::prelude::chrono::Utc;
    use nik::prelude::*;
    use nik::uuid::Uuid;
    use tokio::sync::OnceCell;

    use super::*;
    use crate::business::flow::doc_reader::{self as flow_doc_reader, DocReader};

    /// 连库（进程内只做一次）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
    async fn ensure_db() -> bool {
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 池参数 / 建池 runtime 由夹具复用模板统一提供（P2-60）：旧写法把 `max_lifetime`
                // 与 `idle_timeout` 压成 ZERO ⇒ **每条语句**新建连接（99→145 每次 1~3 ms 握手）。
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 真库用例串行锁（共用全局 `db()` 与既有 181 表模板库）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 18 条 `doc_status` 的 SQL **都能执行**（表 / 列 / 类型全对），且不存在的单据返回 `Ok(None)`。
    ///
    /// 这条用例的价值在「能执行」而不是「返回什么」：一个拼错的表名或列名，
    /// 或者「`status` 其实是 `i16`」这类类型不符，都会在这里变成 `Err`。
    #[tokio::test]
    async fn doc_status_sql_executes_for_every_doc() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;

        let missing = Uuid::now_v7();
        for doc in ErpDoc::ALL {
            let reader = ErpDocReader::new(doc);
            match reader.doc_status(missing).await {
                Ok(None) => {}
                Ok(Some(snapshot)) => panic!(
                    "{doc:?}：不存在的 id 竟查到了单据（{} / {}），说明 WHERE 条件没起作用",
                    snapshot.doc_no, snapshot.status
                ),
                Err(error) => panic!(
                    "{doc:?}（{}，表 {}）的 doc_status 查询失败：{error}",
                    doc.biz_type().as_str(),
                    doc.spec().table
                ),
            }
        }
    }

    /// 采购订单：含数据的往返 —— 单号 / 状态原值 / 中文文案 / 制单人四字段全对；
    /// 软删之后变 `Ok(None)`（流程侧显示「单据已删除」而不是把已删单当现状）。
    #[tokio::test]
    async fn purchase_order_snapshot_roundtrip() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;

        let order_id = Uuid::now_v7();
        let creator = Uuid::now_v7();
        let order_no = format!("P233-PO-{}", &order_id.to_string()[..8]);
        db().execute_with_params(
            "INSERT INTO biz_purchase_order \
                (id, order_no, supplier_id, order_date, warehouse_id, status, create_by) \
             VALUES (?, ?, ?, ?::text::date, ?, ?, ?)",
            vec![
                Value::from(&order_id),
                Value::Text(order_no.clone()),
                Value::from(&Uuid::now_v7()),
                Value::Text(Utc::now().format("%Y-%m-%d").to_string()),
                Value::from(&Uuid::now_v7()),
                Value::Text("SUBMITTED".to_string()),
                Value::from(&creator),
            ],
        )
        .await
        .expect("插入采购单失败");

        let reader = ErpDocReader::new(ErpDoc::PurchaseOrder);
        let snapshot = reader
            .doc_status(order_id)
            .await
            .expect("查询应成功")
            .expect("应查得到");
        assert_eq!(snapshot.doc_no, order_no, "单号应是单据号，不是审批单号");
        assert_eq!(snapshot.status, "SUBMITTED", "状态必须是落库原值");
        assert_eq!(
            snapshot.status_label, "已提交",
            "状态中文文案走 purchase_model::status_label"
        );
        assert_eq!(snapshot.creator_id, Some(creator), "制单人应取自单据 create_by");

        // 默认实现从快照派生 creator_id，应与内控桥的查询同源
        assert_eq!(
            reader.creator_id(order_id).await.expect("派生 creator 应成功"),
            Some(creator)
        );
        assert_eq!(
            reader
                .internal_control_creator(Some(order_id))
                .await
                .expect("内控制单人查询应成功"),
            Some(creator),
            "内控与快照必须是同一个制单人"
        );

        // 软删 → 对流程侧等于「不存在」
        db().execute_with_params(
            "UPDATE biz_purchase_order SET deleted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .expect("软删失败");
        assert!(
            reader.doc_status(order_id).await.expect("查询应成功").is_none(),
            "软删的单据应返回 None"
        );

        // 清场（按 id，绝不按前缀扫全库）
        db().execute_with_params(
            "DELETE FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .expect("清场失败");
    }

    /// 报销单：制单人取 `reporter_id`（**不是** `create_by`），且与内控桥的查询逐字同源。
    ///
    /// 反证：把 `create_by` 造成另一个用户，如果实现取错了列，本用例会红。
    #[tokio::test]
    async fn expense_snapshot_uses_reporter_as_creator() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;

        let report_id = Uuid::now_v7();
        let reporter = Uuid::now_v7();
        let recorder = Uuid::now_v7();
        let report_no = format!("P233-ER-{}", &report_id.to_string()[..8]);
        db().execute_with_params(
            "INSERT INTO biz_expense_report \
                (id, report_no, reporter_id, reporter_name, dept_id, dept_name, title, \
                 expense_date, period_code, status, create_by, item_count) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?::text::date, ?, ?, ?, 1)",
            vec![
                Value::from(&report_id),
                Value::Text(report_no.clone()),
                Value::from(&reporter),
                Value::Text("P233 报销人".to_string()),
                Value::from(&Uuid::now_v7()),
                Value::Text("P233 部门".to_string()),
                Value::Text("P233 单据查询器往返用例".to_string()),
                Value::Text(Utc::now().format("%Y-%m-%d").to_string()),
                Value::Text(Utc::now().format("%Y-%m").to_string()),
                // `ck_biz_exp_report_inst`：`SUBMITTED` 必须挂审批实例，夹具不造实例 → 用 `APPROVED`。
                Value::Text("APPROVED".to_string()),
                Value::from(&recorder),
            ],
        )
        .await
        .expect("插入报销单失败");

        let reader = ErpDocReader::new(ErpDoc::Expense);
        let snapshot = reader
            .doc_status(report_id)
            .await
            .expect("查询应成功")
            .expect("应查得到");
        assert_eq!(snapshot.doc_no, report_no);
        assert_eq!(snapshot.status, "APPROVED", "状态必须是落库原值");
        assert_eq!(
            snapshot.status_label, "已审批（待付款）",
            "文案走 expense_dto::status_label"
        );
        assert_eq!(
            snapshot.creator_id,
            Some(reporter),
            "报销单的制单人必须是 reporter_id（取成 create_by 会把代录人当成制单人）"
        );
        assert_eq!(
            reader
                .internal_control_creator(Some(report_id))
                .await
                .expect("内控制单人查询应成功"),
            Some(reporter),
            "内控桥查的也是 reporter_id，两处必须同源"
        );

        db().execute_with_params(
            "DELETE FROM biz_expense_report WHERE id = ?::uuid",
            vec![Value::from(&report_id)],
        )
        .await
        .expect("清场失败");
    }

    /// `biz_id = None`（审批单没挂单据）→ `Ok(None)` 且**不查库**。
    #[tokio::test]
    async fn internal_control_creator_without_biz_id_is_none() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;

        for doc in ErpDoc::ALL {
            let reader = ErpDocReader::new(doc);
            assert_eq!(
                reader.internal_control_creator(None).await.expect("不应报错"),
                None,
                "{doc:?}：没有 biz_id 时制单人应为 None（内控不能因为取不到数据把流程锁死）"
            );
        }
    }

    /// 端到端：`register_all` → `flow::doc_reader` 的分派助手按编码取到**同一个**实现。
    #[tokio::test]
    async fn dispatch_helpers_reach_erp_readers() {
        if !ensure_db().await {
            return;
        }
        let _guard_db = DB_LOCK.lock().await;
        let _guard_global = flow_doc_reader::GLOBAL_LOCK.lock().await;
        flow_doc_reader::reset_for_tests();
        doc_reader_impl::register_all().expect("注册应成功");

        // 未注册的业务类型仍然降级放行（与今天「没有对应桥就没有这道闸」逐字一致）
        assert!(flow_doc_reader::reader_of_code("NOT_A_BIZ_TYPE").is_none());
        assert_eq!(
            flow_doc_reader::internal_control_creator_of_code("NOT_A_BIZ_TYPE", None)
                .await
                .expect("未注册应降级"),
            None
        );

        // 已注册：按落库编码取到 ERP 的实现
        for doc in ErpDoc::ALL {
            let reader = flow_doc_reader::reader_of_code(doc.biz_type().as_str())
                .unwrap_or_else(|| panic!("{} 未能从进程级注册表取到", doc.biz_type().as_str()));
            assert_eq!(reader.doc_kind(), ErpDoc::biz_type(doc).doc_kind());
        }

        flow_doc_reader::reset_for_tests();
    }
}
