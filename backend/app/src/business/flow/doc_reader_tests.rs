//! `flow::doc_reader` 的单测（**不需要数据库**）。
//!
//! # 覆盖什么
//!
//! | 断言面 | 用例 |
//! |---|---|
//! | 注册表：注册 / 查 / 重复注册 / 未注册 | `registry_*` |
//! | 查询接缝：编码解析、未注册降级、快照透传 | `doc_status_of_*` |
//! | 默认实现：`creator_id` / `internal_control_creator` 从快照派生、`None` 不查库 | `creator_*` |
//! | 覆盖默认实现（应付作废那种「制单人不在本单上」） | `creator_override_wins` |
//! | 终态扇出：每个注册实现都被调到、参数原样 | `dispatch_settled_*` |
//! | 内控闸分派：未注册放行、已注册透传错误码 | `guard_dispatch_*` |
//! | 对象轴默认映射（`PR_APPROVE` 如实 None） | `doc_kind_default_follows_biz_type` |
//! | **接缝自身不许依赖业务域**（红线） | `seam_source_is_business_agnostic` |
//!
//! # 为什么用一把锁串起来
//!
//! 注册表是进程级单例，用例要 `reset_for_tests` + 注册，与 `edge_admin_tests` 的 `DB_LOCK`
//! 同一做法：不加锁时 `dispatch_settled` 会看到别的用例注册的实现，断言随执行顺序漂移。

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cloudpivot_doc::{DocKind, FlowBizType};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::doc_reader::{self, DocReader, DocRegistry, DocSnapshot, GLOBAL_LOCK};
use cloudpivot_kernel::shared::error_code;

// 用例串行锁在 `doc_reader` 里（注册表的持有者）；`erp::doc_reader_impl_tests` 也拿同一把。

// ==================== 假实现 ====================

/// `on_settled` 收到的参数快照：`(instance_id, instance_status)`。
///
/// 单独起别名有两个原因：`type_complexity` 的 clippy 门禁（`--all-targets` 会编译测试），
/// 以及断言里能一眼看出「这里比的是回调参数」而不是一坨泛型。
type SettleCall = (Option<Uuid>, String);

/// 记录调用参数的假实现。
struct FakeReader {
    biz_type: FlowBizType,
    snapshot: Option<DocSnapshot>,
    /// `doc_status` 被调了几次、参数是什么。
    status_calls: Arc<Mutex<Vec<Uuid>>>,
    /// `on_settled` 收到的 (instance_id, instance_status)。
    settle_calls: Arc<Mutex<Vec<SettleCall>>>,
    /// `guard_self_approval` 是否拒绝。
    deny_guard: bool,
    /// `guard_self_approval` 被调了几次（P2-34 的 task 级分派要断言「每个实现都过一遍」）。
    guard_calls: Arc<Mutex<usize>>,
    /// `guard_approve` 是否拒绝（M3-11：信用闸分派）。
    deny_approve: bool,
    /// `guard_approve` 被调了几次。
    approve_calls: Arc<Mutex<usize>>,
}

impl FakeReader {
    fn new(biz_type: FlowBizType, snapshot: Option<DocSnapshot>) -> Self {
        Self {
            biz_type,
            snapshot,
            status_calls: Arc::new(Mutex::new(Vec::new())),
            settle_calls: Arc::new(Mutex::new(Vec::new())),
            deny_guard: false,
            guard_calls: Arc::new(Mutex::new(0)),
            deny_approve: false,
            approve_calls: Arc::new(Mutex::new(0)),
        }
    }

    fn status_call_count(&self) -> usize {
        self.status_calls.lock().map(|calls| calls.len()).unwrap_or(0)
    }

    fn settle_call_count(&self) -> usize {
        self.settle_calls.lock().map(|calls| calls.len()).unwrap_or(0)
    }
}

#[async_trait]
impl DocReader for FakeReader {
    fn biz_type(&self) -> FlowBizType {
        self.biz_type
    }

    async fn doc_status(&self, doc_id: Uuid) -> DbResult<Option<DocSnapshot>> {
        if let Ok(mut calls) = self.status_calls.lock() {
            calls.push(doc_id);
        }
        Ok(self.snapshot.clone())
    }

    async fn on_settled(&self, instance_id: Option<Uuid>, instance_status: &str) {
        if let Ok(mut calls) = self.settle_calls.lock() {
            calls.push((instance_id, instance_status.to_string()));
        }
    }

    async fn guard_self_approval(&self, _task_id: Uuid, _operator: Uuid) -> std::result::Result<(), ApiError> {
        if let Ok(mut calls) = self.guard_calls.lock() {
            *calls += 1;
        }
        if self.deny_guard {
            return Err(ApiError::new(
                403,
                error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
                "测试：制单人不可审批自己制单的单据",
            ));
        }
        Ok(())
    }

    /// M3-11：业务前置闸默认放行，只有 `deny_approve` 的假实现会拒绝。
    async fn guard_approve(&self, _task_id: Uuid) -> std::result::Result<(), ApiError> {
        if let Ok(mut calls) = self.approve_calls.lock() {
            *calls += 1;
        }
        if self.deny_approve {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "测试：客户信用额度不足",
            ));
        }
        Ok(())
    }
}

/// 覆盖 `creator_id` 的假实现（模拟「制单人不在 `biz_id` 指向的那张单上」）。
struct OverrideCreatorReader {
    biz_type: FlowBizType,
    creator: Option<Uuid>,
    status_calls: Arc<Mutex<usize>>,
}

#[async_trait]
impl DocReader for OverrideCreatorReader {
    fn biz_type(&self) -> FlowBizType {
        self.biz_type
    }

    async fn doc_status(&self, _doc_id: Uuid) -> DbResult<Option<DocSnapshot>> {
        if let Ok(mut calls) = self.status_calls.lock() {
            *calls += 1;
        }
        Ok(None)
    }

    async fn creator_id(&self, _doc_id: Uuid) -> DbResult<Option<Uuid>> {
        Ok(self.creator)
    }

    async fn on_settled(&self, _instance_id: Option<Uuid>, _instance_status: &str) {}

    async fn guard_self_approval(&self, _task_id: Uuid, _operator: Uuid) -> std::result::Result<(), ApiError> {
        Ok(())
    }
}

/// 一份四字段齐全的快照。
fn snapshot_of(creator_id: Option<Uuid>) -> DocSnapshot {
    DocSnapshot {
        doc_no: "PO-202611-000001".to_string(),
        status: "PENDING_APPROVE".to_string(),
        status_label: "审批中".to_string(),
        creator_id,
    }
}

// ==================== 注册表（实例版，不碰全局） ====================

#[test]
fn registry_register_and_lookup() {
    let mut registry = DocRegistry::new();
    assert!(registry.is_empty());

    let reader = Arc::new(FakeReader::new(FlowBizType::PoApprove, None));
    registry.register(reader).expect("首次注册应成功");

    assert_eq!(registry.len(), 1);
    assert!(registry.contains(FlowBizType::PoApprove));
    assert!(registry.reader(FlowBizType::PoApprove).is_some());
    // 未注册的键：None，不 panic、不报错
    assert!(registry.reader(FlowBizType::StockInPost).is_none());
    assert!(!registry.contains(FlowBizType::StockInPost));
    assert_eq!(registry.registered(), vec![FlowBizType::PoApprove]);
}

#[test]
fn registry_rejects_duplicate_biz_type() {
    let mut registry = DocRegistry::new();
    registry
        .register(Arc::new(FakeReader::new(FlowBizType::PoApprove, None)))
        .expect("首次注册应成功");
    let error = registry
        .register(Arc::new(FakeReader::new(FlowBizType::PoApprove, None)))
        .expect_err("重复注册必须报错（不许静默覆盖）");
    // 报错要能点名是哪条审批线，否则启动期只能靠猜
    assert!(error.contains("PO_APPROVE"), "错误文案应点名 biz_type，实际：{error}");
    assert!(error.contains("重复注册"), "错误文案应说明原因，实际：{error}");
    assert_eq!(registry.len(), 1, "重复注册不许把表写大");
}

#[test]
fn registry_register_all_stops_on_first_duplicate() {
    let mut registry = DocRegistry::new();
    registry
        .register(Arc::new(FakeReader::new(FlowBizType::PoApprove, None)))
        .expect("首次注册应成功");
    let batch: Vec<Arc<dyn DocReader>> = vec![
        Arc::new(FakeReader::new(FlowBizType::StockInPost, None)),
        Arc::new(FakeReader::new(FlowBizType::PoApprove, None)),
        Arc::new(FakeReader::new(FlowBizType::StockTakePost, None)),
    ];
    let error = registry.register_all(batch).expect_err("批里有重复必须报错");
    assert!(error.contains("PO_APPROVE"), "实际：{error}");
    // 已成功的不回滚：StockInPost 进去了，StockTakePost 没来得及
    assert!(registry.contains(FlowBizType::StockInPost));
    assert!(!registry.contains(FlowBizType::StockTakePost));
    assert_eq!(registry.len(), 2);
}

#[test]
fn registry_reader_of_code_is_strict() {
    let mut registry = DocRegistry::new();
    registry
        .register(Arc::new(FakeReader::new(FlowBizType::StockInPost, None)))
        .expect("注册应成功");
    // 落库编码（大写下划线）命中
    assert!(registry.reader_of_code("STOCK_IN_POST").is_some());
    // 大小写 / 连字符不归一：解析不出来就是 None（词表 crate 的口径，不在接缝里再放宽一次）
    assert!(registry.reader_of_code("stock_in_post").is_none());
    assert!(registry.reader_of_code("stock-in-post").is_none());
    // 编解码认得但没注册
    assert!(registry.reader_of_code("PO_APPROVE").is_none());
}

// ==================== 查询接缝（全局注册表） ====================

#[tokio::test]
async fn doc_status_of_returns_snapshot_and_degrades_on_unknown() {
    let _guard = GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();

    let creator = Uuid::now_v7();
    let reader = Arc::new(FakeReader::new(
        FlowBizType::PoApprove,
        Some(snapshot_of(Some(creator))),
    ));
    doc_reader::register(reader.clone()).expect("注册应成功");

    let doc_id = Uuid::now_v7();
    let snapshot = doc_reader::doc_status_of(FlowBizType::PoApprove, doc_id)
        .await
        .expect("查询不应报错")
        .expect("查到了");
    assert_eq!(snapshot.doc_no, "PO-202611-000001");
    assert_eq!(snapshot.status, "PENDING_APPROVE");
    assert_eq!(snapshot.status_label, "审批中");
    assert_eq!(snapshot.creator_id, Some(creator));
    assert_eq!(reader.status_call_count(), 1, "实现方应被调用一次");
    assert_eq!(
        reader
            .status_calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default(),
        vec![doc_id],
        "doc_id 必须原样传下去"
    );

    // 未注册的业务类型：Ok(None)（降级，不报错）；实现方一次都不该被调
    assert!(
        doc_reader::doc_status_of(FlowBizType::StockTakePost, doc_id)
            .await
            .expect("降级不报错")
            .is_none()
    );
    // 编码不认识 / 认得但没注册：同样 None
    assert!(
        doc_reader::doc_status_of_code("NOT_A_BIZ_TYPE", doc_id)
            .await
            .expect("降级不报错")
            .is_none()
    );
    assert!(
        doc_reader::doc_status_of_code("CONTRACT_APPROVE", doc_id)
            .await
            .expect("降级不报错")
            .is_none()
    );
    // 编码命中
    assert!(
        doc_reader::doc_status_of_code("PO_APPROVE", doc_id)
            .await
            .expect("查询不应报错")
            .is_some()
    );
    assert_eq!(reader.status_call_count(), 2);
}

// ==================== 默认实现：制单人派生 ====================

#[tokio::test]
async fn creator_id_defaults_from_snapshot() {
    let _guard = GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();

    let creator = Uuid::now_v7();
    let reader = Arc::new(FakeReader::new(
        FlowBizType::StockTransfer,
        Some(snapshot_of(Some(creator))),
    ));
    doc_reader::register(reader.clone()).expect("注册应成功");

    let doc_id = Uuid::now_v7();
    assert_eq!(
        doc_reader::creator_of(FlowBizType::StockTransfer, doc_id)
            .await
            .unwrap(),
        Some(creator)
    );
    // 内控入口（`biz_id` 有值）走同一条路
    assert_eq!(
        doc_reader::internal_control_creator_of_code("STOCK_TRANSFER", Some(doc_id))
            .await
            .unwrap(),
        Some(creator)
    );
    assert_eq!(reader.status_call_count(), 2, "两次都从快照派生，各查一次");
}

#[tokio::test]
async fn internal_control_creator_without_biz_id_does_not_query() {
    let _guard = GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();

    let reader = Arc::new(FakeReader::new(FlowBizType::StockInCancel, Some(snapshot_of(None))));
    doc_reader::register(reader.clone()).expect("注册应成功");

    // `biz_id = None`（审批单没挂单据）→ 直接 None，**不查库**
    assert_eq!(
        doc_reader::internal_control_creator_of_code("STOCK_IN_CANCEL", None)
            .await
            .unwrap(),
        None
    );
    assert_eq!(reader.status_call_count(), 0, "None 时不许为了一条内控查询去打库");

    // 未注册的业务类型：None
    assert_eq!(
        doc_reader::internal_control_creator_of_code("BUDGET_APPROVE", Some(Uuid::now_v7()))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn creator_override_wins() {
    let _guard = GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();

    // 应付作废审批：`biz_id` 是应付单 id，但制单人查法不同（今日实现里那两套函数并存）
    let creator = Uuid::now_v7();
    let calls = Arc::new(Mutex::new(0usize));
    let reader = Arc::new(OverrideCreatorReader {
        biz_type: FlowBizType::PayableCancel,
        creator: Some(creator),
        status_calls: calls.clone(),
    });
    doc_reader::register(reader).expect("注册应成功");

    assert_eq!(
        doc_reader::internal_control_creator_of_code("PAYABLE_CANCEL", Some(Uuid::now_v7()))
            .await
            .unwrap(),
        Some(creator),
        "覆盖实现应生效"
    );
    assert_eq!(*calls.lock().unwrap(), 0, "覆盖后不该再走默认的 doc_status 派生");
}

// ==================== 终态扇出 ====================

#[tokio::test]
async fn dispatch_settled_reaches_every_registration_in_order() {
    let _guard = GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();

    // 注册顺序与枚举顺序相反，扇出按 BTreeMap 键序（可复现），不看注册顺序
    let take = Arc::new(FakeReader::new(FlowBizType::StockTakePost, None));
    let post = Arc::new(FakeReader::new(FlowBizType::StockInPost, None));
    doc_reader::register(take.clone()).expect("注册应成功");
    doc_reader::register(post.clone()).expect("注册应成功");
    assert_eq!(doc_reader::len(), 2);

    let instance_id = Uuid::now_v7();
    let hit = doc_reader::dispatch_settled(Some(instance_id), "WITHDRAWN").await;
    assert_eq!(hit, 2, "两个已注册实现都必须被调到");
    for reader in [&take, &post] {
        assert_eq!(reader.settle_call_count(), 1, "每个实现恰好一次");
        let calls = reader.settle_calls.lock().unwrap();
        assert_eq!(
            calls.as_slice(),
            &[(Some(instance_id), "WITHDRAWN".to_string())],
            "参数必须原样透传"
        );
    }

    // 「按单据反查」场景：instance_id = None 也要原样传下去（不许被吞成空串）
    let hit = doc_reader::dispatch_settled(None, "CANCELLED").await;
    assert_eq!(hit, 2);
    assert_eq!(
        take.settle_calls.lock().unwrap().as_slice(),
        &[
            (Some(instance_id), "WITHDRAWN".to_string()),
            (None, "CANCELLED".to_string())
        ]
    );
}

#[tokio::test]
async fn dispatch_settled_with_empty_registry_is_noop() {
    let _guard = GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();
    assert_eq!(doc_reader::dispatch_settled(Some(Uuid::now_v7()), "APPROVED").await, 0);
}

// ==================== 内控闸分派 ====================

#[tokio::test]
async fn guard_dispatch_allows_unknown_and_propagates_deny() {
    let _guard = GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();

    let task_id = Uuid::now_v7();
    let operator = Uuid::now_v7();

    // 未注册 → 放行（与今天「biz_type 没有对应桥就没有这道闸」逐字一致）
    doc_reader::guard_self_approval(FlowBizType::ContractApprove, task_id, operator)
        .await
        .expect("未注册应放行");
    doc_reader::guard_self_approval_of_code("CONTRACT_APPROVE", task_id, operator)
        .await
        .expect("未注册应放行");

    let mut reader = FakeReader::new(FlowBizType::ContractApprove, None);
    reader.deny_guard = true;
    doc_reader::register(Arc::new(reader)).expect("注册应成功");

    // 已注册且拒绝：HTTP 状态 + 业务码 + 文案都必须原样透传
    // （P2-34 之后调用点的 403 + 65019 就是靠它）
    let error = doc_reader::guard_self_approval(FlowBizType::ContractApprove, task_id, operator)
        .await
        .expect_err("应被拦下");
    assert_eq!(error.status, 403);
    assert_eq!(error.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
    assert!(error.msg.contains("制单人"), "文案应透传实现方的，实际：{}", error.msg);

    let error = doc_reader::guard_self_approval_of_code("CONTRACT_APPROVE", task_id, operator)
        .await
        .expect_err("应被拦下");
    assert_eq!(error.status, 403);
    assert_eq!(error.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
}

// ==================== 对象轴默认映射 ====================

#[test]
fn doc_kind_default_follows_biz_type() {
    let post = FakeReader::new(FlowBizType::StockInPost, None);
    assert_eq!(post.doc_kind(), Some(DocKind::StockIn), "审批轴 → 对象轴走词表 crate");
    let cancel = FakeReader::new(FlowBizType::StockInCancel, None);
    assert_eq!(
        cancel.doc_kind(),
        Some(DocKind::StockIn),
        "过账与作废两条审批挂在同一张收货单上"
    );
    // 采购申请在对象轴里**没有**成员：如实 None，不许硬塞 PurchaseOrder
    let pr = FakeReader::new(FlowBizType::PrApprove, None);
    assert_eq!(pr.doc_kind(), None);
}

// ==================== 类型约束 ====================

/// `Arc<dyn DocReader>` 要能跨线程共享（`tokio` 多线程运行时 + `static` 注册表的前提）。
#[test]
fn trait_object_is_send_sync() {
    fn assert_send_sync<T: ?Sized + Send + Sync>() {}
    assert_send_sync::<dyn DocReader>();
}

// ==================== 红线：接缝不许依赖业务域 ====================

/// 本模块的源码里**不许出现任何业务域路径**（`erp` / `hr` / `task` / `impexp` …）。
///
/// 这是 P2-32 这张卡的**唯一判据**：解环不是「把 import 挪个位置」，而是这个文件
/// 必须对业务域一无所知。实现在业务域里（P2-33），调用点走注册表（P2-34）。
///
/// # 为什么要断言扫描面
///
/// 路径写错 / 文件被移走后读不到内容，`assert!(content.contains(..) == false)` 会**静默变绿**。
/// 所以先断言「文件行数 > 200 且读到了正文」，再断言不含业务域前缀 —— 与 P2-31 的
/// 护栏同一口径（扫空 = 假绿是本类卡最大的坑）。
#[test]
fn seam_source_is_business_agnostic() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/business/flow/doc_reader.rs");
    let source = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("读不到接缝源码 {path}：{error}"));

    let lines: Vec<&str> = source.lines().collect();
    assert!(
        lines.len() > 200,
        "扫描面异常：{path} 只有 {} 行（预期 > 200），读错文件会假绿",
        lines.len()
    );

    // 业务域前缀的两种写法：绝对路径与裸模块名
    let forbidden = ["crate::business", "super::erp", "super::hr"];
    let mut hits: Vec<String> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if forbidden.iter().any(|token| line.contains(token)) {
            hits.push(format!("{}:{}: {}", path, index + 1, line.trim()));
        }
    }
    assert!(
        hits.is_empty(),
        "接缝源码里出现了业务域路径（解环红线）：\n{}",
        hits.join("\n")
    );

    // 正面确认：它确实依赖了那个零出边的词表 crate（否则这个文件是空壳）
    assert!(
        lines.iter().any(|line| line.contains("cloudpivot_doc::")),
        "接缝应只依赖词表 crate（cloudpivot_doc）"
    );
}

// ==================== 燃烧率闸门：flow → erp 的直连只许减少 ====================

/// `flow` 里对 `erp` 的直连引用条数基线（P2-32 落地时的实测值，按「命中行数」计）。
///
/// # 这个常量的用法
///
/// - **只许下调**：P2-33 注册实现、P2-34 把调用点换成注册表之后，把这里改成新的实测值；
/// - 上调就是**回归**：等价于「又有人往 `flow` 里写死了 `erp` 的调用」，用例会红。
///
/// 基线值来自模块注释那张对账表的实测（`grep -rn` 匹配「业务域绝对路径 + `erp`」）。
const FLOW_TO_ERP_REFS_BASELINE: usize = 0;

/// 检索用的 token 拆成两段拼：本文件不能出现完整 token，否则 `grep` 对账时会把
/// 这条用例自身也算成「`flow → erp` 的直连」（实测差 3 行，P2-34 对账会被绊住）。
const ERP_TOKEN: &str = concat!("crate::business::", "erp");

/// 扫描面自检：`flow` 目录下的 `.rs` 文件数与总字节数（扫空 = 假绿，本类卡最大的坑）。
const FLOW_SOURCE_MIN_FILES: usize = 40;
/// 扫描面自检：`flow` 源码总字节数（handler.rs 一个文件就 200KB+，低于这个数说明目录没扫对）。
const FLOW_SOURCE_MIN_BYTES: usize = 300_000;

/// `flow → erp` 的直连引用**必须为 0**（P2-B 解环的燃烧率闸门）。
///
/// # 基线为什么是 0
///
/// P2-32 建接缝（135 不动）、P2-33 在 ERP 侧实现并注册、**P2-34 把全部直连摘掉**：
/// 现在 `flow` 下任何 `.rs` 里出现「业务域绝对路径 + `erp`」都是**回退**，不是「新功能」。
/// 闸门留在 0 而不是删掉，是因为 `scripts/lint/module_cycle_report.py` 是人工工具、不进 CI ——
/// 没有这条用例，下一个人写回一行直连就没人拦得住。
///
/// # 统计口径
///
/// - 只扫 `src/business/flow/**/*.rs`，**排除本模块的两个文件**（`doc_reader.rs` /
///   `doc_reader_tests.rs` 里出现业务域路径本来就是红线，由上一个用例管，不在这里重复计）；
/// - 统计「含 `ERP_TOKEN`（业务域绝对路径 + `erp`）的**行数**」（与 `grep -rn | wc -l` 同口径，便于对账）。
///
/// # 扫描面断言
///
/// `FLOW_SOURCE_MIN_FILES` / `FLOW_SOURCE_MIN_BYTES` 任一不满足直接失败 ——
/// 路径写错时「扫到 0 个文件、0 条引用」会静默变绿，这正是本类卡最大的假绿来源。
#[test]
fn flow_erp_reference_budget_is_not_exceeded() {
    fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
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

    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/business/flow"));
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    walk(root, &mut files).unwrap_or_else(|error| panic!("扫不了 {}：{error}", root.display()));

    // 排除本模块自身（接缝 + 它的用例）
    files.retain(|path| {
        !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("doc_reader"))
    });

    let mut total_bytes = 0usize;
    let mut refs = 0usize;
    let mut worst: Vec<String> = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("读不了 {}：{error}", path.display()));
        total_bytes += source.len();
        let mut file_refs = 0usize;
        for (index, line) in source.lines().enumerate() {
            if line.contains(ERP_TOKEN) {
                file_refs += 1;
                if worst.len() < 10 {
                    worst.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
                }
            }
        }
        refs += file_refs;
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
        refs == FLOW_TO_ERP_REFS_BASELINE,
        "flow → erp 直连引用**回退**了：{refs} 条 > 基线 {FLOW_TO_ERP_REFS_BASELINE} 条（{} 个文件 / {total_bytes} 字节）。\n\
         P2-34 已经把这条边摘干净了：单据查询 / 终态回调走 `flow::doc_reader` 的注册表，审批终态订阅走 `flow::instance_hooks` —— 不要往 `flow` 里再写死 `erp` 的调用。\n\
         前 10 处命中：\n{}",
        files.len(),
        worst.join("\n")
    );
    // 基线是 0（终态）：没有「还能再降」的空间 —— 这条分支原来提示「下调基线」，
    // P2-34 归零后它恒假，改成一句话说明，免得下一个人以为常量还能动。
    println!("flow → erp 直连：{refs} 条（基线 {FLOW_TO_ERP_REFS_BASELINE}，已归零）");
}

// ==================== P2-34：审批入口的 task 级闸 + 受控码表 ====================

/// 审批入口只有 `task_id`（没有实例 `biz_type`）：闸必须**把所有已注册实现过一遍**。
///
/// 这条钉住 `handler::approve_task` 的替换形态：改造前是「每个桥的闸都调一遍」，
/// 每个实现自己查实例、自己判 `biz_type` 是不是我的（不是就 `Ok`）。判据换成注册表之后
/// 语义必须**逐字不变** —— 所以这里断言「全都调到了」+「第一个拒绝原样透传」。
#[tokio::test]
async fn guard_self_approval_for_task_walks_every_reader_and_propagates_the_denial() {
    let _guard = GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();

    let task_id = Uuid::now_v7();
    let operator = Uuid::now_v7();

    // ① 一个实现都没有 → 放行（与「没有这条闸」一致，不能把审批全锁死）
    doc_reader::guard_self_approval_for_task(task_id, operator)
        .await
        .expect("空注册表必须放行");

    // ② 两个实现都放行 → 两次都被调到（不能只问第一个）
    let allow_a = Arc::new(FakeReader::new(FlowBizType::PoApprove, None));
    let allow_b = Arc::new(FakeReader::new(FlowBizType::ContractApprove, None));
    doc_reader::register(allow_a.clone()).expect("注册应成功");
    doc_reader::register(allow_b.clone()).expect("注册应成功");
    doc_reader::guard_self_approval_for_task(task_id, operator)
        .await
        .expect("都放行应 Ok");
    assert_eq!(*allow_a.guard_calls.lock().expect("锁"), 1, "第一个实现必须被调到一次");
    assert_eq!(
        *allow_b.guard_calls.lock().expect("锁"),
        1,
        "第二个实现也必须被调到一次（只问第一个 = 「另一个桥的闸永远不响」）"
    );

    // ③ 有一个拒绝 → HTTP 状态 + 业务码原样透传（调用点的 `?` 直接变 403）
    let mut deny = FakeReader::new(FlowBizType::StockTakePost, None);
    deny.deny_guard = true;
    doc_reader::register(Arc::new(deny)).expect("注册应成功");
    let error = doc_reader::guard_self_approval_for_task(task_id, operator)
        .await
        .expect_err("应被拦下");
    assert_eq!(error.status, 403);
    assert_eq!(error.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
}

/// M3-11：审批点「同意」前的业务前置闸走同一条注册表扇出 ——
/// 未注册放行、每个实现都被调到、第一个拒绝原样透传（409 + 业务码）。
#[tokio::test]
async fn guard_approve_for_task_walks_every_reader_and_propagates_the_denial() {
    let _guard = GLOBAL_LOCK.lock().await;
    doc_reader::reset_for_tests();

    let task_id = Uuid::now_v7();

    // ① 空注册表 → 放行（「没有这条闸」= 不锁死任何审批线）
    doc_reader::guard_approve_for_task(task_id)
        .await
        .expect("空注册表必须放行");

    // ② 两个实现都放行 → 两次都被调到（默认实现 = 该审批线没有额度闸）
    let allow_a = Arc::new(FakeReader::new(FlowBizType::PoApprove, None));
    let allow_b = Arc::new(FakeReader::new(FlowBizType::ContractApprove, None));
    doc_reader::register(allow_a.clone()).expect("注册应成功");
    doc_reader::register(allow_b.clone()).expect("注册应成功");
    doc_reader::guard_approve_for_task(task_id).await.expect("都放行应 Ok");
    assert_eq!(*allow_a.approve_calls.lock().expect("锁"), 1, "第一个实现必须被调到");
    assert_eq!(
        *allow_b.approve_calls.lock().expect("锁"),
        1,
        "第二个实现也必须被调到（只问第一个 = 销售那关永远不响）"
    );

    // ③ 有一个拒绝 → HTTP 状态 + 业务码原样透传（调用点的 `?` 直接变 409）
    let mut deny = FakeReader::new(FlowBizType::SoApprove, None);
    deny.deny_approve = true;
    doc_reader::register(Arc::new(deny)).expect("注册应成功");
    let error = doc_reader::guard_approve_for_task(task_id).await.expect_err("应被拦下");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::RESOURCE_CONFLICT);
}

/// 受控码表 `flow::internal_control::GOVERNED_CODES` 的自检（**不改表就改这条**）。
///
/// 三件事一起验：① 恰好 19 个且不重复；② 每个码都是词表里真实存在的 `FlowBizType`；
/// ③ **调拨单不在里面** —— 它注册了终态回调但不做内控排除，加进去 = 多拦一批人（改变生产行为）。
#[test]
fn governed_codes_are_the_sixteen_approval_lines() {
    use crate::business::flow::internal_control::GOVERNED_CODES;

    assert_eq!(
        GOVERNED_CODES.len(),
        19,
        "受控集合必须恰好 19 个审批码（M4-14 加 SUPPLIER_ACCESS）"
    );
    let unique: std::collections::BTreeSet<&str> = GOVERNED_CODES.iter().copied().collect();
    assert_eq!(unique.len(), GOVERNED_CODES.len(), "受控码表里有重复项");

    for code in GOVERNED_CODES {
        assert!(
            FlowBizType::parse(code).is_some(),
            "受控码 `{code}` 在词表里不存在（拼错 = 这条审批线的内控闸永远不生效）"
        );
    }

    assert!(
        !GOVERNED_CODES.contains(&FlowBizType::StockTransfer.as_str()),
        "调拨单（STOCK_TRANSFER）今天没有内控闸：把它加进受控码表 = 多拦一批人 = 改变生产行为"
    );
    // 任务是另一条审批线，也不受 ERP 内控约束
    assert!(
        !GOVERNED_CODES.contains(&FlowBizType::TaskAccept.as_str()),
        "任务验收不受内控约束（TaskAccept 不在这 19 条里）"
    );
}
