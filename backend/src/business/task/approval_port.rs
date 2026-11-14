//! 任务侧「审批能力」接缝（P2-37 · P2-B 解环：摘掉 `task → flow`）。
//!
//! # 这个文件解决什么
//!
//! 任务状态机与审批引擎之间有四条硬连接（D3b / P-FLOW-1B / P11 / T12 陆续加的），
//! 它们全部落在 `flow::task_bridge` 上，而 `flow` 侧又反过来读写 `biz_task`
//! （审批结论回调推进任务、流程造任务）——`task ⇄ flow` 就是这么成环的。
//!
//! 本模块只解**一半**：让 `task` 不再认识 `flow`。办法是**依赖倒置**——
//!
//! ```text
//! 改前：task ──直调 8 处──→ flow::task_bridge┄┄┄┄┄┄┄┄┄┄┄→ biz_task（flow 反向写任务表）
//! 改后：task ──trait + 注册表──→ (抽象)                        （本卡：task 侧零 flow 引用）
//!                                  ↑ 启动期注入
//!                          flow::task_approval_impl             （实现留在审批域，flow → task 原样不动）
//! ```
//!
//! 剩下那半边（`flow → task` 的 7 处）是 P2-38 的活：把写 `biz_task` 的那几段挪出 `flow`，
//! 由中立的接线域承接。本卡把**调用方向**先掰正，P2-38 才有地方落。
//!
//! # 为什么 trait 定义在 `task` 而不是 `flow`
//!
//! 与 P2-32（`flow::doc_reader`）/ P2-35（`hr::employee_reader`）**方向相反**，理由是可验证的：
//!
//! | 接缝 | 谁定义 | 为什么 |
//! |---|---|---|
//! | `doc_reader` / `employee_reader` | **被读方的域** | 「单据状态 / 员工基础信息」的**口径**（软删、数据范围、状态取值）归表属主，消费方只拿结果 |
//! | 本接缝（`TaskApprovalPort`） | **调用方的域（`task`）** | 这里传的是**命令**（发起审批、推进节点、撤审批）与**事务内联动**，不是「读别人的数据」；口径归审批引擎，但**需求**归任务状态机。定义在 `flow` 就等于 `task` 必须 import `flow` —— 边还在，等于没解 |
//!
//! 换个说法：`doc_reader` 是「我读你」，本接缝是「我请你办事且要在我这个事务里办」。
//! 后者的抽象只能长在请求方，否则就是「为了解环把环挪个位置」。
//!
//! # 六条纪律
//!
//! 1. **只在事务内/事务外各司其职**：`start_in_tx` / `on_status_changed` / `cancel_pending`
//!    收 `&Transaction`，**必须**与任务状态写入同一个事务 —— 这是「任务改了、审批没跟上」
//!    这类死单的唯一防线（AGENTS 约定 11）。别把它们改成事务后 fire-and-forget。
//! 2. **`prepare_accept` 在事务外**（只是几次只读查询），产出 [`PreparedAccept`] 不透明句柄，
//!    事务里再交回 [`TaskApprovalPort::start_in_tx`]。**任务侧不解释句柄内容** ——
//!    里面是审批引擎的发起参数（预生成的实例 id + 流程定义快照），任务侧看一眼就是耦合。
//! 3. **同一时刻只有一个实现**（不像 `doc_reader` 按 `biz_type` 分组）：任务侧的审批引擎只有一个。
//!    重复安装 → `Err`，由 `bootstrap` 按编程错误处理（prod `exit(1)` / 其它 profile `panic`）。
//! 4. **不许跨 `await` 持锁**：先克隆 `Arc` 再 `await`（跨 `await` 持读锁会把全进程的审批调用串行化）。
//! 5. **锁中毒直接放行**（`into_inner`）：注册期 panic 不该让之后每次调用都 500。
//! 6. **`task/` 里不许出现审批域路径**：闸门见 `approval_port_tests::task_source_has_no_flow_references`
//!    （基线 0，只降不升）。
//!
//! # 没装实现时是什么语义（每条都是**降级**，不是 500）
//!
//! 与 `hr::employee_reader` 同一口径：`task` 与 `flow` 同进程、同一份 `bootstrap`，
//! 未安装只可能是**启动链漏了一步**（编程错误，启动期就兜住），而不是请求能遇到的业务状态。
//!
//! | 入口 | 未安装 | 为什么是这个值 |
//! |---|---|---|
//! | [`instance_status_of`] | `Ok(None)` | 门闸的既有口径：「读不到实例状态 → 不拦」（让任务永久卡 `REVIEW` 比放行更坏） |
//! | [`prepare_accept`] | `Ok(None)` | 既有口径：「没配 `TASK_ACCEPT` 流程 → 走原两级验收，不算错」 |
//! | [`start_in_tx`] | `Ok(Uuid::nil())` + `WARN` | 不可达（`prepare_accept` 已返回 `None`）；留防御，绝不 panic |
//! | [`on_status_changed`] | `Ok(ApprovalOutcome::NotFlowTask)` + `WARN` | 流程任务的状态推进旁路掉：任务侧状态照样落库（宁可流程不动，也不让用户点不动按钮） |
//! | [`cancel_pending`] | `Ok(CancelledCounts::default())` + `WARN` | 没有审批能力就没有可撤的待办；返回 0 行让业务继续（孤儿待办由 `scripts/maintenance/clean_orphan_flow.sh` 清） |
//! | [`is_start_rejected`] | `false` | 没有引擎就没有引擎哨兵，交给 `utils::db_error` 兜成 500 |
//!
//! 生产**不允许**出现这种降级：`bootstrap::register_task_approval_port` 装不上就启动失败。

use std::sync::{Arc, Mutex, OnceLock, RwLock};

use async_trait::async_trait;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::task::state::TaskStatus;

/// 「已备好的审批发起参数」的不透明句柄。
///
/// # 为什么是不透明类型
///
/// 审批发起需要十几个字段（流程定义 id / 版本快照 / 发起人快照 / 表单快照 …），它们是
/// **审批引擎的入参结构**，不是任务的概念。任务侧只需要「拿住它、在事务里交回去」，
/// 一解释就重新耦合（P2-37 要解的就是这条）。
///
/// 于是：实现方 [`wrap`](Self::wrap) 自己的发起参数进来，[`downcast_ref`](Self::downcast_ref)
/// 在自己的 `start_in_tx` 里取回。类型不符 → `None`（不是本实现的句柄，绝不 panic）。
///
/// # 为什么要 `Send + Sync`（不是只有 `Send`）
///
/// 句柄会**跨 `await` 被引用**（`start_in_tx(&tx, &prepared, …)` 在事务闭包里的 future 必须
/// `Send`，而 future 持有 `&PreparedAccept`）。`&T: Send` 要求 `T: Sync` ——
/// 只写 `Send` 的话，`change_status` 的 async fn 直接编译不过（`Handler` bound 不满足）。
pub struct PreparedAccept(Box<dyn std::any::Any + Send + Sync>);

impl PreparedAccept {
    /// 由**实现方**（审批域）把自发起参数装进来。任务侧不要调用。
    pub fn wrap<T: std::any::Any + Send + Sync>(payload: T) -> Self {
        Self(Box::new(payload))
    }

    /// 由**实现方**在自己的 `start_in_tx` 里取回发起参数。
    ///
    /// 返回 `None` = 句柄不是本实现造的（例如两个实现混用），调用方按编程错误处理。
    pub fn downcast_ref<T: std::any::Any>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for PreparedAccept {
    /// 只打类型身份，不打内容（里面是流程定义快照 / 表单快照，日志里没有价值还有噪音）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedAccept(<opaque>)")
    }
}

/// 一次「提交验收即发起审批」的请求（任务侧准备，审批侧消费）。
///
/// 字段全部是**任务自己的事实**（谁、哪张单、什么表单快照），审批侧据此建实例。
#[derive(Debug, Clone)]
pub struct AcceptRequest {
    /// 任务 id（审批实例的 `biz_id`，也是回写 `biz_task.approval_instance_id` 的定位键）。
    pub task_id: Uuid,
    /// 任务号（审批单标题前缀 + `biz_no`，审批中心靠它对上业务单据）。
    pub task_no: String,
    /// 任务标题（拼进审批单标题 `任务号 标题`）。
    pub title: String,
    /// 发起人（提交验收的人）。
    pub initiator: Uuid,
    /// 发起时冻结的表单快照（条件节点表达式的取值域）。
    pub form_data: serde_json::Value,
}

/// 一次任务状态变更（任务侧的事实，交给审批侧决定「流程要不要跟着动」）。
///
/// 字段口径与 `flow` 侧原有结构一一对应，**不做什么改写**（P2-37 是搬家，不是改语义）。
#[derive(Debug, Clone, Copy)]
pub struct TaskStatusChange<'a> {
    /// 任务 id。
    pub task_id: Uuid,
    /// 生成它的流程实例（`biz_task.flow_instance_id`；手建任务为 `None`）。
    pub instance_id: Option<Uuid>,
    /// 生成它的节点 key（`biz_task.flow_node_key`）。
    pub node_key: Option<&'a str>,
    /// 生成时的轮次（`biz_task.flow_round`）。
    pub round: Option<i32>,
    /// 变更前状态。
    pub from: TaskStatus,
    /// 变更后状态。
    pub to: TaskStatus,
    /// 操作人（写进流程侧日志）。
    pub operator: Uuid,
    /// 操作人姓名（日志 / 变更日志用）。
    pub operator_name: &'a str,
    /// 打回 / 取消原因。
    pub reason: Option<&'a str>,
}

/// 审批侧对「任务状态变更」的四种回答。
///
/// 与 `flow` 侧原枚举同形（`NotFlowTask` / `Advanced` / `Ignored` / `Conflict`）：
/// `Conflict` 必须让调用方**整笔回滚**（任务要推进流程、流程已经不允许了），
/// 静默成功会让前端显示「完成」而流程没动。
#[derive(Debug)]
pub enum ApprovalOutcome {
    /// 不是流程任务（绝大多数请求走这里 —— 普通任务一次审批查询都不发）。
    NotFlowTask,
    /// 流程侧已被本次变更推进；`next_nodes` 是本次激活的下游节点（终止 / 等审批时为空）。
    Advanced {
        /// 本次激活的下游节点 key。
        next_nodes: Vec<String>,
    },
    /// 本次变更不推进流程（映射表「不动」的行 / 实例已离开 `PENDING`）。
    Ignored {
        /// 当时实例的状态（读不到实例时为 `"MISSING"`）。
        instance_status: String,
    },
    /// 业务冲突：任务要求推进流程，流程侧已不允许 → 调用方回滚并把这个错误原样给前端。
    Conflict(ApiError),
}

/// 撤销在跑审批单的**原因**（决定写进审批单的审计文案，两者在证据链上是不同动作）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelCause {
    /// 任务被软删（`DELETE /business/task/{id}`）。
    Deleted,
    /// 任务被推 `CANCELLED`（还能反悔，所以文案不同）。
    Cancelled,
}

/// 三张表各被终止多少行（实例 / 待办 / 节点）。
///
/// # 为什么任务侧要这个数字
///
/// 1. 结构化日志（`instances` / `flow_tasks` / `nodes` 三个字段，排障与 e2e 断言按它 grep）；
/// 2. 「有没有真的撤掉东西」的判据（等于 [`Default`] 就不打那条 `info`，免得噪音）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CancelledCounts {
    /// 被置为 `CANCELLED` 的实例数（`biz_flow_instance`）。
    pub instances: u64,
    /// 被置为 `CANCELLED` 的审批待办数（`biz_flow_task`）。
    pub tasks: u64,
    /// 被置为 `CANCELLED` 的节点数（`biz_flow_instance_node`）。
    pub nodes: u64,
}

impl CancelledCounts {
    /// 一件都没撤（等价于 [`Default`]，但读起来更直白）。
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// 累加（级联取消子孙时逐个相加）。
    pub fn absorb(&mut self, other: Self) {
        self.instances += other.instances;
        self.tasks += other.tasks;
        self.nodes += other.nodes;
    }
}

/// 任务需要的**全部**审批能力（唯一实现者见 `flow::task_approval_impl`）。
///
/// # 实现方约定
///
/// - 实现放在**审批域**（`business/flow/task_approval_impl.rs`），启动期 [`install`] 注入；
/// - `start_in_tx` / `on_status_changed` / `cancel_pending` 收 `&Transaction`，
///   **只在这个事务里写**，不许自己开事务、不许提交后另跑一次（见模块注释纪律 1）；
/// - 事务内**不许**再开连接读同一批行：会撞自己刚拿的行锁（长事务自锁死）；
/// - 所有方法都**不给默认实现**：漏一个就是「任务改了、审批没跟上」的死单，
///   required ⇒ 漏了编译不过（同 `flow::doc_reader` 对 `on_settled` 的取舍）。
#[async_trait]
pub trait TaskApprovalPort: Send + Sync {
    /// 实现名（日志 / 启动摘要 / 用例断言，`snake_case`）。
    fn name(&self) -> &'static str;

    /// 任务上的 `approval_instance_id` → 审批单状态（审批门闸的输入）。
    ///
    /// `None` / 查不到实例 → `Ok(None)`（门闸口径：读不到不拦）。真库故障仍 `Err`。
    async fn instance_status_of(&self, approval_instance_id: Option<Uuid>) -> DbResult<Option<String>>;

    /// 准备一张验收审批单（**不写库**）。
    ///
    /// `Ok(None)` = 没配流程 / 没发布版本 / 该任务已有在跑的实例 → 调用方走原两级验收，
    /// **不是错误**。真库故障才 `Err`。
    async fn prepare_accept(&self, req: AcceptRequest) -> DbResult<Option<PreparedAccept>>;

    /// **事务内**发起审批单并把实例 id 回写到 `biz_task.approval_instance_id`。
    ///
    /// 业务拒绝（派不出审批人 / 自审被禁）必须走「哨兵 + 槽位」：
    /// 原始 `ApiError` 塞进 `rejected` 槽位、返回哨兵错误让事务整笔回滚 ——
    /// 否则会留下「任务已 `REVIEW` + 一张 0 待办的 `PENDING` 审批单」这种批不动也提交不了的单据。
    async fn start_in_tx(
        &self,
        tx: &Transaction,
        prepared: &PreparedAccept,
        task_id: Uuid,
        rejected: &Mutex<Option<ApiError>>,
    ) -> DbResult<Uuid>;

    /// **事务内**把「任务状态变了」告诉审批侧（节点通过 / 退回 / 终止），返回它的回答。
    ///
    /// 返回 [`ApprovalOutcome::Conflict`] 时调用方必须回滚（任务完成后又打回、流程已终止…）。
    async fn on_status_changed(&self, tx: &Transaction, event: TaskStatusChange<'_>) -> DbResult<ApprovalOutcome>;

    /// **事务内**撤掉这张任务名下所有在跑的审批单（任务取消 / 软删两条入口）。
    ///
    /// 撤不掉 → `Err`（调用方整笔回滚）：留一条「任务没了、待办还挂着」的孤儿审批
    /// 比这次操作失败更麻烦（用户已踩过，见 `task_bridge` 的 P11 / T12 注释）。
    async fn cancel_pending(
        &self,
        tx: &Transaction,
        task_id: Uuid,
        task_no: &str,
        cause: CancelCause,
    ) -> DbResult<CancelledCounts>;

    /// 这个事务错误是不是「发起被引擎业务拒绝」的哨兵（`rejected` 槽位里有原始 `ApiError`）。
    fn is_start_rejected(&self, error: &DbError) -> bool;

    /// 撤销审批时写进审批单 / 待办的审计文案（**唯一真源在审批域**，任务侧只拿去打日志）。
    fn cancel_comment(&self, cause: CancelCause) -> &'static str;
}

/// 审批能力注册表（**实例**版，可单测；进程级单例见 [`registry`]）。
#[derive(Default)]
pub struct ApprovalPortRegistry {
    /// 唯一实现者（审批域）。`None` = 启动链还没装（或单测里被清空）。
    port: Option<Arc<dyn TaskApprovalPort>>,
}

impl ApprovalPortRegistry {
    /// 空注册表（未安装状态）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 安装实现。
    ///
    /// # 异常情况
    ///
    /// 已经装了还再装一次 → `Err`（**不静默覆盖**）：谁生效不该取决于注册顺序。
    pub fn install(&mut self, port: Arc<dyn TaskApprovalPort>) -> std::result::Result<(), String> {
        if self.port.is_some() {
            return Err("任务审批接缝重复安装：一个进程只能有一个审批引擎".to_string());
        }
        self.port = Some(port);
        Ok(())
    }

    /// 取实现（未安装 → `None`）。
    pub fn port(&self) -> Option<Arc<dyn TaskApprovalPort>> {
        self.port.clone()
    }

    /// 是否已安装。
    pub fn is_installed(&self) -> bool {
        self.port.is_some()
    }
}

/// 进程级注册表。
///
/// # 为什么是 `OnceLock<RwLock<_>>` 而不是 `OnceLock<Arc<dyn _>>`
///
/// 与 `flow::doc_reader` / `hr::employee_reader` 同一个理由：装载必须由 `bootstrap` 驱动，
/// 而 `OnceLock::get_or_init` 的闭包里只能调本模块能看到的东西 —— 那正好变成
/// 「接缝 → 实现」的反向依赖，把要解的边又装回去。所以「空表先就位、启动期写满、之后只读」。
static REGISTRY: OnceLock<RwLock<ApprovalPortRegistry>> = OnceLock::new();

fn registry() -> &'static RwLock<ApprovalPortRegistry> {
    REGISTRY.get_or_init(|| RwLock::new(ApprovalPortRegistry::new()))
}

fn read_registry() -> std::sync::RwLockReadGuard<'static, ApprovalPortRegistry> {
    registry().read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_registry() -> std::sync::RwLockWriteGuard<'static, ApprovalPortRegistry> {
    registry().write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 启动期安装实现（`bootstrap` 调用；重复安装返回 `Err`）。
pub fn install(port: Arc<dyn TaskApprovalPort>) -> std::result::Result<(), String> {
    write_registry().install(port)
}

/// 是否已安装（启动期自检 / 用例断言用）。
pub fn is_installed() -> bool {
    read_registry().is_installed()
}

/// 取已安装的实现（未安装 → `None`）。调用方**不要**长期持有，用完就放。
pub fn port() -> Option<Arc<dyn TaskApprovalPort>> {
    read_registry().port()
}

/// 已安装实现的名字（启动摘要 / 排障用；未安装 → `None`）。
pub fn port_name() -> Option<&'static str> {
    port().map(|port| port.name())
}

/// 任务上的审批单状态（未安装 → `Ok(None)`）。
pub async fn instance_status_of(approval_instance_id: Option<Uuid>) -> DbResult<Option<String>> {
    let port = port();
    match port {
        None => Ok(None),
        Some(port) => port.instance_status_of(approval_instance_id).await,
    }
}

/// 准备验收审批单（未安装 → `Ok(None)`）。
pub async fn prepare_accept(req: AcceptRequest) -> DbResult<Option<PreparedAccept>> {
    let port = port();
    match port {
        None => Ok(None),
        Some(port) => port.prepare_accept(req).await,
    }
}

/// 事务内发起审批单（未安装 → 告警 + `Ok(Uuid::nil())`，见模块注释的降级表）。
pub async fn start_in_tx(
    tx: &Transaction,
    prepared: &PreparedAccept,
    task_id: Uuid,
    rejected: &Mutex<Option<ApiError>>,
) -> DbResult<Uuid> {
    let port = port();
    match port {
        None => {
            tracing::warn!(%task_id, "任务审批接缝未安装：跳过发起审批单（启动链漏装）");
            Ok(Uuid::nil())
        }
        Some(port) => port.start_in_tx(tx, prepared, task_id, rejected).await,
    }
}

/// 事务内通知「任务状态变了」（未安装 → 告警 + `NotFlowTask`）。
pub async fn on_status_changed(tx: &Transaction, event: TaskStatusChange<'_>) -> DbResult<ApprovalOutcome> {
    let port = port();
    match port {
        None => {
            tracing::warn!(task_id = %event.task_id, "任务审批接缝未安装：本次状态变更未通知审批侧（启动链漏装）");
            Ok(ApprovalOutcome::NotFlowTask)
        }
        Some(port) => port.on_status_changed(tx, event).await,
    }
}

/// 事务内撤掉在跑审批单（未安装 → 告警 + 0 行）。
pub async fn cancel_pending(
    tx: &Transaction,
    task_id: Uuid,
    task_no: &str,
    cause: CancelCause,
) -> DbResult<CancelledCounts> {
    let port = port();
    match port {
        None => {
            tracing::warn!(%task_id, ?cause, "任务审批接缝未安装：没有可撤的审批单");
            Ok(CancelledCounts::default())
        }
        Some(port) => port.cancel_pending(tx, task_id, task_no, cause).await,
    }
}

/// 事务错误是不是「发起被引擎业务拒绝」的哨兵（未安装 → `false`）。
pub fn is_start_rejected(error: &DbError) -> bool {
    match port() {
        None => false,
        Some(port) => port.is_start_rejected(error),
    }
}

/// 撤销审批的审计文案（未安装 → `""`：调用方只在真的撤到东西时才用它对日志）。
pub fn cancel_comment(cause: CancelCause) -> &'static str {
    match port() {
        None => "",
        Some(port) => port.cancel_comment(cause),
    }
}

/// 清空进程级注册表（**仅供单测**，生产代码不许调用）。
#[cfg(test)]
pub fn reset_for_tests() {
    *write_registry() = ApprovalPortRegistry::new();
}

/// 进程级注册表的单测串行锁（**仅供单测**）。
///
/// 注册表是进程级单例：所有「reset / install + 断言」的用例（含真库用例）必须拿**同一把**锁，
/// 否则断言会随执行顺序漂移。
#[cfg(test)]
pub static GLOBAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
