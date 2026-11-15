// ============================================================================
// approval_port_tests.rs —— 任务审批接缝（P2-37）的注册表语义 + 依赖边闸门
//
// 全部**离线**：不连库、不需要 `ERP_DB_URL`。真库等价性用例在审批域一侧
// （`business/flow/flow_model_delete_tests.rs` 的 `port_cancel_on_deleted_…`）。
//
// 为什么必须单独一个文件：注册表是**进程级单例**，而 `cargo test --lib` 是单进程多线程，
// 断言会随执行顺序漂移 —— 所有动注册表的用例串行拿同一把锁（`approval_port::GLOBAL_LOCK`），
// 与 `hr::employee_reader_tests` 同款。
// ============================================================================

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::approval_port::{
    self, AcceptRequest, ApprovalOutcome, ApprovalPortRegistry, CancelCause, CancelledCounts, PreparedAccept,
    TaskApprovalPort, TaskStatusChange,
};

/// 假实现：把「审批引擎被问到了什么」记在内存里，返回值固定。
///
/// 真实现（`flow::task_bridge`）在 `flow/task_approval_impl.rs`，**这里不许 import 它** ——
/// 否则本文件会把刚摘掉的那条边重新装回去（闸门用例会当场判红）。
#[derive(Default)]
struct FakePort {
    /// 记录 `prepare_accept` 收到的发起人（断言「任务侧把谁传下去了」）。
    initiator: Mutex<Option<Uuid>>,
}

#[async_trait]
impl TaskApprovalPort for FakePort {
    fn name(&self) -> &'static str {
        "fake_port"
    }

    async fn instance_status_of(&self, approval_instance_id: Option<Uuid>) -> DbResult<Option<String>> {
        // 有实例号 → 假装审批在跑；没有 → 没有审批可查。
        Ok(approval_instance_id.map(|_| "PENDING".to_string()))
    }

    async fn prepare_accept(&self, req: AcceptRequest) -> DbResult<Option<PreparedAccept>> {
        *self.initiator.lock().unwrap() = Some(req.initiator);
        Ok(Some(PreparedAccept::wrap(req.task_no)))
    }

    async fn start_in_tx(
        &self,
        _tx: &Transaction,
        _prepared: &PreparedAccept,
        _task_id: Uuid,
        _rejected: &Mutex<Option<ApiError>>,
    ) -> DbResult<Uuid> {
        // 离线用例不构造 `Transaction`，本方法只要求「能编译」。
        Ok(Uuid::nil())
    }

    async fn on_status_changed(&self, _tx: &Transaction, event: TaskStatusChange<'_>) -> DbResult<ApprovalOutcome> {
        Ok(ApprovalOutcome::Advanced {
            next_nodes: vec![format!("after-{}", event.to.code())],
        })
    }

    async fn cancel_pending(
        &self,
        _tx: &Transaction,
        _task_id: Uuid,
        _task_no: &str,
        cause: CancelCause,
    ) -> DbResult<CancelledCounts> {
        Ok(CancelledCounts {
            instances: u64::from(cause == CancelCause::Deleted),
            tasks: 0,
            nodes: 0,
        })
    }

    fn is_start_rejected(&self, error: &DbError) -> bool {
        matches!(error, DbError::InvalidValue(msg) if msg == "START_REJECTED")
    }

    fn cancel_comment(&self, _cause: CancelCause) -> &'static str {
        "假实现的文案"
    }
}

// ==================== 注册表语义 ====================

/// 装进去能取出来，且名字一致（启动摘要 / 排障靠它认「装了谁」）。
#[tokio::test]
async fn install_then_port_returns_the_same_impl() {
    let _guard = approval_port::GLOBAL_LOCK.lock().await;
    approval_port::reset_for_tests();
    assert!(!approval_port::is_installed(), "reset 之后必须是未安装");

    approval_port::install(Arc::new(FakePort::default())).expect("首次安装必须成功");
    assert!(approval_port::is_installed());
    assert_eq!(approval_port::port_name(), Some("fake_port"));
    assert_eq!(approval_port::port().map(|port| port.name()), Some("fake_port"));

    approval_port::reset_for_tests();
}

/// 重复安装必须报错（谁赢不能取决于注册顺序）。
#[tokio::test]
async fn duplicate_install_is_an_error() {
    let _guard = approval_port::GLOBAL_LOCK.lock().await;
    approval_port::reset_for_tests();

    approval_port::install(Arc::new(FakePort::default())).expect("首次安装必须成功");
    let error = approval_port::install(Arc::new(FakePort::default())).expect_err("第二次安装必须失败");
    assert!(error.contains("重复安装"), "报错要点名原因，实际：{error}");

    approval_port::reset_for_tests();
}

/// 实例版注册表（不带进程级副作用）也要遵守同一口径 —— 这是给 `bootstrap` 之外的
/// 装配代码（将来 P2-38 的接线域）留的可测入口。
#[test]
fn registry_instance_semantics() {
    let mut registry = ApprovalPortRegistry::new();
    assert!(!registry.is_installed());
    assert!(registry.port().is_none());
    registry.install(Arc::new(FakePort::default())).expect("首次安装");
    assert!(registry.is_installed());
    assert!(registry.install(Arc::new(FakePort::default())).is_err());
}

// ==================== 未安装时的降级语义（模块注释那张表逐行可执行） ====================

/// 未安装 → 门闸读状态 `Ok(None)`（既有口径：读不到不拦）。
#[tokio::test]
async fn uninstalled_instance_status_is_none() {
    let _guard = approval_port::GLOBAL_LOCK.lock().await;
    approval_port::reset_for_tests();
    let status = approval_port::instance_status_of(Some(Uuid::now_v7()))
        .await
        .expect("未安装不该报错");
    assert!(status.is_none());
    assert!(!approval_port::is_start_rejected(&DbError::InvalidValue(
        "START_REJECTED".to_string()
    )));
    assert_eq!(approval_port::cancel_comment(CancelCause::Cancelled), "");
}

/// 未安装 → `prepare_accept` 返回 `None`（等价于「没配流程」，走原两级验收）。
#[tokio::test]
async fn uninstalled_prepare_accept_is_none() {
    let _guard = approval_port::GLOBAL_LOCK.lock().await;
    approval_port::reset_for_tests();
    let prepared = approval_port::prepare_accept(AcceptRequest {
        task_id: Uuid::now_v7(),
        task_no: "P237-0001".to_string(),
        title: "占位".to_string(),
        initiator: Uuid::now_v7(),
        form_data: serde_json::json!({}),
    })
    .await
    .expect("未安装不该报错");
    assert!(prepared.is_none());
}

/// 装上之后同一个入口必须**真的问到实现**（证明上面那条不是因为「入口永远返回 None」）。
#[tokio::test]
async fn installed_port_is_actually_consulted() {
    let _guard = approval_port::GLOBAL_LOCK.lock().await;
    approval_port::reset_for_tests();
    let fake = Arc::new(FakePort::default());
    approval_port::install(fake.clone()).expect("安装");

    let initiator = Uuid::now_v7();
    let prepared = approval_port::prepare_accept(AcceptRequest {
        task_id: Uuid::now_v7(),
        task_no: "P237-0002".to_string(),
        title: "占位".to_string(),
        initiator,
        form_data: serde_json::json!({}),
    })
    .await
    .expect("装上之后不该报错")
    .expect("假实现返回 Some");
    assert_eq!(
        prepared.downcast_ref::<String>(),
        Some(&"P237-0002".to_string()),
        "句柄必须原样带回实现自己装进去的东西"
    );
    assert_eq!(*fake.initiator.lock().unwrap(), Some(initiator));

    let status = approval_port::instance_status_of(Some(Uuid::now_v7()))
        .await
        .expect("查询不该报错");
    assert_eq!(status.as_deref(), Some("PENDING"));
    assert!(approval_port::is_start_rejected(&DbError::InvalidValue(
        "START_REJECTED".to_string()
    )));
    assert_eq!(approval_port::cancel_comment(CancelCause::Cancelled), "假实现的文案");

    approval_port::reset_for_tests();
}

// ==================== 纯数据结构的口径 ====================

/// 三列计数：`default` = 「一件都没撤」，`absorb` 逐列相加（**不许**折成一个总数）。
#[test]
fn cancelled_counts_keep_columns_and_add_up() {
    let mut total = CancelledCounts::default();
    assert!(total.is_empty());
    total.absorb(CancelledCounts {
        instances: 1,
        tasks: 2,
        nodes: 3,
    });
    total.absorb(CancelledCounts {
        instances: 10,
        tasks: 20,
        nodes: 30,
    });
    assert_eq!(
        total,
        CancelledCounts {
            instances: 11,
            tasks: 22,
            nodes: 33
        }
    );
    assert!(!total.is_empty());
}

/// 句柄取错类型 → `None`（不 panic、不猜）。
#[test]
fn prepared_accept_typed_round_trip() {
    let handle = PreparedAccept::wrap(7_u8);
    assert_eq!(handle.downcast_ref::<u8>(), Some(&7));
    assert!(handle.downcast_ref::<String>().is_none());
    assert!(format!("{handle:?}").contains("opaque"), "调试输出不该泄露内容");
}

// ==================== 闸门：`task/` 生产面零审批域引用（P2-37 的边归零，只降不升） ====================

/// `task/` 生产面出现**审批域全路径**（`crate::business::` + `flow`）的引用行数基线
/// （P2-37 之后必须是 0）。
const TASK_TO_FLOW_REFS_BASELINE: usize = 0;

/// **闸门**：`task/` 里不许出现审批域路径（含注释 —— intra-doc link 是 rustdoc 依赖）。
///
/// # 为什么这条闸门必须存在
///
/// P2-37 摘掉了 `task → flow` 的 8 处代码引用；没有闸门，下一次「顺手直接调
/// `flow::task_bridge` 更快」就会把它装回来，而这类回边在 code review 里很难看见
/// （一处 `use` 加三行调用，diff 里毫不起眼）。
///
/// # 红绿自证
///
/// 往任何一个 `task/*.rs` 里插一行以 `crate::business::` 开头的 `use …flow…;`
/// → 本用例 FAILED 并点名到 `文件:行`；删掉 → 转绿。（本文件自身用 `concat!` 拼判据串，
/// 不做自污染：直接在源码里写全路径会让「引用行数」凭空 +1，闸门自己先判红。）
#[test]
fn task_source_has_no_flow_references() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/business/task");
    assert!(root.is_dir(), "源码根不存在：{}", root.display());

    // 判据串拆开拼：本文件自己也在扫描面里。
    let needle = concat!("crate::business::", "flow");

    let mut hits: Vec<String> = Vec::new();
    let mut files = 0usize;
    let mut bytes = 0usize;
    let entries = std::fs::read_dir(&root).expect("读不到 business/task 目录");
    for entry in entries {
        let path = entry.expect("目录项不可读").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("读不到源文件");
        files += 1;
        bytes += text.len();
        for (index, line) in text.lines().enumerate() {
            if line.contains(needle) {
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("<unknown>");
                hits.push(format!("{name}:{}: {}", index + 1, line.trim()));
            }
        }
    }

    // 扫描面硬断言：扫空时「0 命中」是假绿，必须先证明真的扫到了东西。
    assert!(
        files >= 40,
        "扫描面异常：只扫到 {files} 个 .rs（business/task 现有 48 个）"
    );
    assert!(bytes >= 300_000, "扫描面异常：字节数只有 {bytes}");

    assert!(
        hits.len() == TASK_TO_FLOW_REFS_BASELINE,
        "task/ 里出现了 {count} 处审批域引用（基线 {TASK_TO_FLOW_REFS_BASELINE}）：\n{list}\n\
         改法：走 `task::approval_port` 接缝（实现在 `flow/task_approval_impl.rs`）",
        count = hits.len(),
        list = hits.join("\n")
    );
}

/// 接缝模块自身的文档也不许出现审批域路径 —— 上面那条闸门扫它，这条钉住「为什么它没写」：
/// 文档里点名实现方用的是**文件路径散文**（`business/flow/task_approval_impl.rs`），
/// 不是 `crate::` 全路径（后者会变成 rustdoc 依赖 = 真依赖）。
#[test]
fn seam_docs_use_prose_paths() {
    let source = include_str!("approval_port.rs");
    assert!(
        source.contains("business/flow/task_approval_impl.rs"),
        "接缝文档要点名实现方落点（散文路径），否则下一个人找不到实现"
    );
}
