//! 流程引擎 ↔ 业务单据的**查询与回调接缝**（P2-32 · P2-B 解环）。
//!
//! # 这个文件解决什么
//!
//! `flow` 与 `erp` / `hr` 现在是**双向依赖**：`erp → flow` 132 处（提交审批、落终态时读实例），
//! `flow → erp` 131 处（见下方对账表）。后者是那个环的另一半。本模块把
//! 「`flow` → 业务域」那半条边**倒转**成 trait + 注册表：
//!
//! | 谁 | 依赖方向 | 说明 |
//! |---|---|---|
//! | `flow`（本模块） | → `cloudpivot-doc`（零出边叶子 crate）+ 框架 | **不 import 任何业务域**：本文件源码里不出现业务域路径前缀（[`super::doc_reader_tests`] 的 `seam_source_is_business_agnostic` 有断言） |
//! | `erp` / `hr` | → `flow` | 各自实现 [`DocReader`]，启动期调 [`register`] 注册进来 |
//!
//! 注册表是 `OnceLock<RwLock<DocRegistry>>`：**启动期写、请求期读**，没有任何运行期改写路径。
//! 调用方拿到的是一份克隆的 `Arc<dyn DocReader>`，读锁在 `await` 之前就已经放掉 ——
//! 跨 `await` 持锁会让整个进程的单据查询串行化，这一条写在 [`registry`] 的注释里。
//!
//! # 三个方法各自替换掉什么（逐条对账，数值取自 `grep -c` 实测）
//!
//! | 现有直连（`flow` 里写死的） | 行数 | 替换成 |
//! |---|:--:|---|
//! | `handler.rs` 里 4 处「每个桥都调一遍」的 `on_*_settled` 扇出 | 60 | [`dispatch_settled`] |
//! | 审批入口的 `*_bridge::guard_self_approval` | 17 | [`guard_self_approval`] |
//! | `internal_control::of_biz` 的 17 段 `if biz_type == *_bridge::BIZ_TYPE` 分派 + `internal_control_creator` 调用 | 22 | [`internal_control_creator_of_code`] + [`is_registered`] |
//!
//! 本模块替换掉其中 **99 / 135** 行（2026-11-15 实测：`flow` 下 55 个 `.rs` 里含
//! 「业务域绝对路径 + `erp`」的行共 135 行，本模块两个文件不计）。其余 **36 行**不属于
//! 「按单据取状态」，P2-34 已按类处置完（**`flow` 侧现已归零**，由
//! `flow::doc_reader_tests::flow_erp_reference_budget_is_not_exceeded` 的 0 基线钉住）：
//!
//! - `separation` 的全局开关（自审金额上限 / 职责分离开关）→ 搬 `business::shared::separation`
//!   （那是**配置**，不是单据查询）；
//! - 联动规则 / 凭证快照 / `POSTED_RECEIVED_SQL` 这类 SQL 片段 → 整块搬 `business::erp::linkage`
//!   （动作、规则表、执行引擎全是 ERP 单据的事；`flow` 只剩一个类型引用，也一并消除）；
//! - `approval_gate` 的通用闸门（跳过审批的判据）→ 搬本域 `flow::approval_gate`；
//! - 单据 PDF 公共件 → 独立域 `business::docpdf`（`erp` / `flow` / `task` 三方共用）；
//! - 审批终态的「联动订阅」→ 本域新接缝 [`super::instance_hooks`]（实现在 ERP，注册进来）。
//!
//! 分类脚本与逐类样本见 `docs/architecture/P2-32-flow单据查询接缝-DocReader.md` §2。
//!
//! # 为什么注册表的键是 [`FlowBizType`] 而不是 `DocKind`
//!
//! 1. 审批实例落库的就是 `biz_type`（`biz_flow_instance.biz_type`），查表时手里只有它；
//! 2. [`FlowBizType::doc_kind`] 对 `PR_APPROVE`（采购申请）返回 `None` —— 它没有对象轴成员，
//!    用 `DocKind` 做键会把「采购申请审批」整条线漏掉（而它恰好有自己的制单人查询）；
//! 3. 一个业务对象可以有多条审批：应付单既是 `PAYABLE_PAY` 又是 `PAYABLE_CANCEL`，
//!    而两条审批的「制单人怎么查」并不相同（`internal_control_creator` 对
//!    `internal_control_creator_of_payable`）。键必须是 `biz_type`，**一条审批一个实现**。
//!
//! # 为什么 `on_settled` / `guard_self_approval` 不给默认实现
//!
//! Rust 检测不到「实现方有没有覆盖默认方法」。给一个默认 `Ok(())` / 空体，等于：
//!
//! - 内控闸**静默失效** —— 漏实现的那条审批线，制单人又能批自己的单了；
//! - 单据**永久停在中间态** —— 审批结束了，回调没落地，没有任何待办能推进它
//!   （AGENTS.md 约定 11 描述的就是这一类事故，只能改库才能恢复）。
//!
//! 所以这两个方法是 **required**：漏了**编译不过**。这是编译期唯一能发现漏项的工具，
//! 比任何「运行期兜底」都可靠。确实没有该动作的实体，请写空体 + 一行注释说明原因
//! （例如「本单据不经审批、无结算回调」），不要靠默认值蒙过去。
//!
//! # [`DocSnapshot`] 为什么只有四个字段
//!
//! 每个字段都能指到现有调用点，没有猜的：
//!
//! | 字段 | 现有调用点 |
//! |---|---|
//! | `creator_id` | `internal_control::of_biz` → `*_bridge::internal_control_creator`（把制单人从审批人里排除） |
//! | `status` / `status_label` | 审批详情与审批单 PDF 的「单据状态」（落库值 + 中文文案） |
//! | `doc_no` | 待办 / 通知 / PDF 文案里的单号（`biz_flow_instance.instance_no` 是**审批单**号，不是单据号） |
//!
//! **金额不在快照里**：自审金额上限的判据取自实例 `form_data`（`self_approval::amount_of`），
//! 不从单据侧再读一个来源 —— 两个来源迟早不一致，而不一致的金额判据是内控事故的温床。

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use cloudpivot_doc::{DocKind, FlowBizType};
use nik::prelude::*;
use nik::uuid::Uuid;

/// 单据状态快照（流程侧能看到的、业务单据的那几个字段）。
///
/// 见模块注释「[`DocSnapshot`] 为什么只有四个字段」：字段与现有调用点一一对应，
/// 加字段前先找调用点，别凭想象加。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocSnapshot {
    /// 单据号（`biz_purchase_order.order_no` / `biz_stock_in.in_no` … 的落库值）。
    ///
    /// 注意与本审批单的 `biz_flow_instance.instance_no` 不是一回事：审批单号是
    /// 「谁发起的哪一次审批」，单据号是「哪张单」。审计追溯要的是后者。
    pub doc_no: String,
    /// 单据当前状态（落库原值，如 `DRAFT` / `PENDING_APPROVE` / `POSTED`）。
    ///
    /// 必须是**原值**，不是归一后的业务语义：调用方要拿它与状态机的边逐字比对。
    pub status: String,
    /// 单据状态的**中文文案**（审批详情 / PDF 直接打）。
    ///
    /// 与 `status` 分开的理由：文案改字不该影响任何判据；而判据需要的是原值。
    pub status_label: String,
    /// 制单人（`create_by`），历史行 / 外部导入可能为 `None`。
    ///
    /// `None` 只表示「取不到制单人」，**不表示这张单没有内控约束**：
    /// 内控要排除的是「这个人」，取不到人就没人被排除，审批照常能发起
    /// （内控不能因为取不到数据把流程锁死）。
    pub creator_id: Option<Uuid>,
}

/// 业务单据的查询与回调接口（一条审批业务类型一个实现）。
///
/// # 实现方约定
///
/// - 实现放在**业务域**里（`erp/…` / `hr/…`），**不在本模块**；
/// - 一个 [`FlowBizType`] 一个实现：同一个实现**不要**注册到两个键上
///   （两条审批的制单人 / 回调用哪个函数通常不同，见 [`super::internal_control`] 的现有分支）；
/// - 启动期注册（`bootstrap`），不要在请求里注册 —— 注册表没有运行期改写路径。
///
/// # 参数口径
///
/// - `doc_id`：业务单据主键（就是 `biz_flow_instance.biz_id` 的值）；
/// - `instance_id`：审批实例 id（终态回调用，`None` 表示「按单据反查」的场景）；
/// - `instance_status`：审批实例的终态**原值**（`APPROVED` / `REJECTED` / `WITHDRAWN` /
///   `CANCELLED` / `TERMINATED` …），实现方自己决定哪些状态要动单据。
#[async_trait]
pub trait DocReader: Send + Sync {
    /// 本实现负责的审批业务类型（注册表的键，必须全局唯一）。
    fn biz_type(&self) -> FlowBizType;

    /// 本实现负责的业务对象（对象轴）。
    ///
    /// 默认走 [`FlowBizType::doc_kind`]（跨轴映射的唯一真相在那个 crate 里）；
    /// `PR_APPROVE` 这类「没有对象轴成员」的审批返回 `None`，这是如实结果，不要硬塞一个。
    fn doc_kind(&self) -> Option<DocKind> {
        self.biz_type().doc_kind()
    }

    /// 按单据 id 读当前状态快照（**本 trait 的核心方法**）。
    ///
    /// # 返回值
    ///
    /// - `Ok(Some(snapshot))`：查到了；
    /// - `Ok(None)`：单据不存在（物理删了 / id 是脏数据）——是**数据事实**，不是错误；
    /// - `Err(e)`：数据库故障，调用方按 500 处理。
    ///
    /// 「查不到」与「出错」必须分开：查不到的展示成「单据已删除」，出错要暴露给运维，
    /// 混成一种会让真故障看起来像业务数据问题。
    async fn doc_status(&self, doc_id: Uuid) -> DbResult<Option<DocSnapshot>>;

    /// 单据的制单人（内控排除审批人用）。
    ///
    /// 默认实现从 [`DocReader::doc_status`] 的快照里取，**实现方通常不需要覆盖**；
    /// 只有当「制单人」不在这张单的 `create_by` 上（例如应付作废审批要查的是**应付单**的
    /// `create_by`，而 `biz_id` 指向的是付款记录）才需要覆盖。
    async fn creator_id(&self, doc_id: Uuid) -> DbResult<Option<Uuid>> {
        Ok(self.doc_status(doc_id).await?.and_then(|snapshot| snapshot.creator_id))
    }

    /// 内控用：按 `biz_id` 取制单人，签名与现有各 `*_bridge::internal_control_creator` 逐字一致。
    ///
    /// `biz_id` 为 `None`（审批单没挂单据，例如自定义流程）→ `Ok(None)`，**不查库**。
    async fn internal_control_creator(&self, biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
        match biz_id {
            None => Ok(None),
            Some(doc_id) => self.creator_id(doc_id).await,
        }
    }

    /// 审批实例落终态后的回调：把结论落到单据上（打回草稿 / 置已过账 / 标作废 …）。
    ///
    /// # 实现方必须做到
    ///
    /// 1. **幂等**：同一个实例重复回调不许重复落状态（重复回调来自重试与人工补发，
    ///    现有各桥用 `settle_guard` 判定「还在等这个审批吗」，实现方应继续走那条通道）；
    /// 2. **自过滤**：本实现只处理自己 `biz_type` 的实例，其它实例直接返回
    ///    （[`dispatch_settled`] 会把**所有**已注册实现都调一遍，与今天 `handler` 里
    ///    「每个桥都调」的行为一致）；
    /// 3. **不许 `panic`**：返回类型是 `()`，没有错误通道 —— 失败必须自己记日志并让单据
    ///    处于「用户还能继续操作」的状态（AGENTS.md 约定 11）。
    async fn on_settled(&self, instance_id: Option<Uuid>, instance_status: &str);

    /// 审批入口的内控闸（**事务之前**调，越早拒绝越好）。
    ///
    /// 与「单据没实现」等价的行为是 [`Ok(())`]（放行）；要不要拦是**实现方的判据**，
    /// 所以本方法没有默认实现（见模块注释「为什么 `on_settled` / `guard_self_approval`
    /// 不给默认实现」）。
    async fn guard_self_approval(&self, task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError>;

    /// 审批点「同意」**之前**的业务前置闸（**事务之前**调；M3-11 起用于客户信用额度）。
    ///
    /// # 与 [`Self::guard_self_approval`] 的区别（为什么这个给默认实现）
    ///
    /// - `guard_self_approval` 是**内控**：漏实现的后果是「能自审」，必须逼着每条线表态，
    ///   所以没有默认实现；
    /// - 本方法是**业务前置条件**（例：销售订单 `APPROVED` 即占客户信用额度，超限要拦）。
    ///   绝大多数审批线**根本没有这门生意**，默认 `Ok(())` = 「本审批线没有这一关」，
    ///   与「不实现」语义完全相同；逼着 17 条线各写一行 `Ok(())` 只会制造噪音。
    ///
    /// 实现方约定：判据必须**同源于业务侧唯一真相**（例：`credit::decide`），
    /// 拒绝时返回 409 + 业务码并在报文里写清「本次未生效、待办仍在」；
    /// 查不到单据 / 与本 `biz_type` 无关时返回 `Ok(())`，不要抢流程自己的 404。
    async fn guard_approve(&self, _task_id: Uuid) -> std::result::Result<(), ApiError> {
        Ok(())
    }
}

/// `biz_type` → [`DocReader`] 的注册表（**实例**版，可单测；进程级单例见 [`registry`]）。
#[derive(Default)]
pub struct DocRegistry {
    /// 键是审批业务类型：一条审批一个实现（见模块注释）。
    readers: BTreeMap<FlowBizType, Arc<dyn DocReader>>,
}

impl DocRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个实现。
    ///
    /// # 异常情况
    ///
    /// 同一个 [`FlowBizType`] 重复注册返回 `Err`（**不静默覆盖**：两个实现抢一个键，
    /// 谁赢取决于注册顺序，这种不确定性在启动期必须暴露出来）。
    pub fn register(&mut self, reader: Arc<dyn DocReader>) -> std::result::Result<(), String> {
        let biz_type = reader.biz_type();
        if self.readers.contains_key(&biz_type) {
            return Err(format!(
                "单据查询器重复注册：{}（{}）",
                biz_type.as_str(),
                biz_type.label()
            ));
        }
        self.readers.insert(biz_type, reader);
        Ok(())
    }

    /// 批量注册；遇到第一个重复就返回 `Err`（**已注册成功的不回滚**）。
    ///
    /// 启动期调用：重复注册是编程错误，调用方（`bootstrap`）应当 `panic` 或记 error 后
    /// 停止服务 —— 带着半个注册表跑起来，等于随机几条审批线没有内控闸。
    pub fn register_all(
        &mut self,
        readers: impl IntoIterator<Item = Arc<dyn DocReader>>,
    ) -> std::result::Result<(), String> {
        for reader in readers {
            self.register(reader)?;
        }
        Ok(())
    }

    /// 按业务类型取实现。
    pub fn reader(&self, biz_type: FlowBizType) -> Option<Arc<dyn DocReader>> {
        self.readers.get(&biz_type).cloned()
    }

    /// 按业务类型的**落库编码**取实现（`biz_flow_instance.biz_type` 是字符串，
    /// 这是调用点最常用的入口）。
    ///
    /// # 返回值
    ///
    /// 编码不认识（不在 [`FlowBizType::ALL`] 里）或未注册 → `None`。
    /// **不报错**：历史实例的 `biz_type` 可能是已废弃的值，展示层要能降级显示。
    pub fn reader_of_code(&self, code: &str) -> Option<Arc<dyn DocReader>> {
        FlowBizType::parse(code).and_then(|biz_type| self.reader(biz_type))
    }

    /// 该业务类型是否已注册（= 是否受内控约束，替换现有 `if/else` 里的 `governed` 判定）。
    pub fn contains(&self, biz_type: FlowBizType) -> bool {
        self.readers.contains_key(&biz_type)
    }

    /// 已注册的业务类型（按枚举声明顺序，BTreeMap 保证稳定）。
    pub fn registered(&self) -> Vec<FlowBizType> {
        self.readers.keys().copied().collect()
    }

    /// 已注册数量。
    pub fn len(&self) -> usize {
        self.readers.len()
    }

    /// 是否一个都没注册（启动早期的合法状态）。
    pub fn is_empty(&self) -> bool {
        self.readers.is_empty()
    }
}

/// 进程级注册表。
///
/// # 为什么是 `RwLock` 而不是 `OnceLock<HashMap>`（impexp 注册表的做法）
///
/// impexp 的定义注册表在 `get_or_init` 里自己装载内置定义，所以能做成只读的 `OnceLock`。
/// 这里不行：实现分散在 `erp` / `hr`，装载动作必须由 `bootstrap` 驱动，而
/// `get_or_init` 的闭包里只能调本模块能看到的东西 —— 那正好又变成 `flow → erp` 的边。
/// 所以是「空表先就位、启动期由业务域写满、之后只读」，用 `RwLock` 表达这个两阶段。
///
/// # 三条纪律
///
/// 1. **不许跨 `await` 持锁**：先 `.read()` 取出 `Arc` 克隆，`drop` 掉 guard 再 `await`；
/// 2. **读锁 poisoning 直接放行**（`into_inner`）：注册期 panic 不该让整个进程之后的
///    每次查询都返回错误 —— 一个已经写坏的注册表比「全部 500」更容易排障；
/// 3. **只有一个写入口** [`register`] / [`register_all`]，调用方固定是 `bootstrap`。
static REGISTRY: OnceLock<RwLock<DocRegistry>> = OnceLock::new();

/// 取进程级注册表（惰性初始化成空表）。
fn registry() -> &'static RwLock<DocRegistry> {
    REGISTRY.get_or_init(|| RwLock::new(DocRegistry::new()))
}

/// 读锁（poisoning 时取回内部值，见 [`registry`] 纪律 2）。
fn read_registry() -> std::sync::RwLockReadGuard<'static, DocRegistry> {
    registry().read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 写锁（同上）。
fn write_registry() -> std::sync::RwLockWriteGuard<'static, DocRegistry> {
    registry().write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 启动期注册一个实现（`bootstrap` 调用；重复注册返回 `Err`）。
pub fn register(reader: Arc<dyn DocReader>) -> std::result::Result<(), String> {
    write_registry().register(reader)
}

/// 启动期批量注册（语义见 [`DocRegistry::register_all`]）。
pub fn register_all(readers: impl IntoIterator<Item = Arc<dyn DocReader>>) -> std::result::Result<(), String> {
    write_registry().register_all(readers)
}

/// 按业务类型取实现（请求期调用）。
pub fn reader(biz_type: FlowBizType) -> Option<Arc<dyn DocReader>> {
    read_registry().reader(biz_type)
}

/// 按落库编码取实现（请求期最常用的入口）。
pub fn reader_of_code(code: &str) -> Option<Arc<dyn DocReader>> {
    read_registry().reader_of_code(code)
}

/// 该业务类型是否已注册。
pub fn is_registered(biz_type: FlowBizType) -> bool {
    read_registry().contains(biz_type)
}

/// 已注册的业务类型。
pub fn registered() -> Vec<FlowBizType> {
    read_registry().registered()
}

/// 已注册数量。
pub fn len() -> usize {
    read_registry().len()
}

/// 按「业务类型 + 单据 id」取单据状态快照。
///
/// # 返回值
///
/// - 未注册该业务类型 → `Ok(None)`（**与「单据不存在」同形**：调用方展示层要能降级，
///   内控侧则按「没人被排除」处理 —— 与今天「`biz_type` 不在 if/else 链里」完全一致）；
/// - 已注册 → 透传实现方的结果（含 `Err`）。
pub async fn doc_status_of(biz_type: FlowBizType, doc_id: Uuid) -> DbResult<Option<DocSnapshot>> {
    match reader(biz_type) {
        None => Ok(None),
        Some(reader) => reader.doc_status(doc_id).await,
    }
}

/// 同 [`doc_status_of`]，入参是落库编码。
pub async fn doc_status_of_code(code: &str, doc_id: Uuid) -> DbResult<Option<DocSnapshot>> {
    match reader_of_code(code) {
        None => Ok(None),
        Some(reader) => reader.doc_status(doc_id).await,
    }
}

/// 按「业务类型 + 单据 id」取制单人。
pub async fn creator_of(biz_type: FlowBizType, doc_id: Uuid) -> DbResult<Option<Uuid>> {
    match reader(biz_type) {
        None => Ok(None),
        Some(reader) => reader.creator_id(doc_id).await,
    }
}

/// 内控用：按落库编码 + `biz_id` 取制单人（`internal_control::of_biz` 的替换点）。
pub async fn internal_control_creator_of_code(code: &str, biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    match reader_of_code(code) {
        None => Ok(None),
        Some(reader) => reader.internal_control_creator(biz_id).await,
    }
}

/// 审批入口的内控闸（`internal_control` 的替换点）。
///
/// # 返回值
///
/// - 该业务类型未注册 → `Ok(())`（放行）。与今天「`biz_type` 没有对应桥就没有这道闸」一致；
/// - 已注册 → 透传实现方的判定（`Err` 通常是 403 + `FLOW_SELF_APPROVAL_FORBIDDEN`）。
pub async fn guard_self_approval(
    biz_type: FlowBizType,
    task_id: Uuid,
    operator: Uuid,
) -> std::result::Result<(), ApiError> {
    match reader(biz_type) {
        None => Ok(()),
        Some(reader) => reader.guard_self_approval(task_id, operator).await,
    }
}

/// 同 [`guard_self_approval`]，入参是落库编码。
pub async fn guard_self_approval_of_code(
    code: &str,
    task_id: Uuid,
    operator: Uuid,
) -> std::result::Result<(), ApiError> {
    match reader_of_code(code) {
        None => Ok(()),
        Some(reader) => reader.guard_self_approval(task_id, operator).await,
    }
}

/// 审批入口的内控闸：**手里只有 `task_id`**，把所有已注册实现过一遍。
///
/// # 为什么不是 [`guard_self_approval_of_code`]
///
/// `handler::approve_task`（同意）在开事务之前只有待办 id，没有实例的 `biz_type`；
/// 今天就是「每个桥的闸都调一遍」（每个实现自己查待办 → 实例 → 判 `biz_type` 是不是我的，
/// 不是就返回 `Ok`）。本函数保留同一个调用形态，语义逐字不变：
///
/// - 未注册的实现根本不在循环里 → 与「没有这条闸」一致；
/// - 第一个 `Err` 立刻返回（同一个实例只可能命中一个实现，顺序不影响结论）。
///
/// 代价与改造前相同（每个实现一次自己的查询），**没有**引入额外往返。
pub async fn guard_self_approval_for_task(task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
    let readers: Vec<Arc<dyn DocReader>> = {
        let registry = read_registry();
        registry.readers.values().cloned().collect()
    };
    for reader in &readers {
        reader.guard_self_approval(task_id, operator).await?;
    }
    Ok(())
}

/// 审批点「同意」前的**业务前置闸**：手里只有 `task_id`，把所有已注册实现过一遍。
///
/// 与 [`guard_self_approval_for_task`] 同形（每个实现自己查待办 → 实例 → 判
/// `biz_type` 是不是我的）：
///
/// - 未注册 / 没覆盖 [`DocReader::guard_approve`] 的实现 → 默认 `Ok(())`（放行）；
/// - 第一个 `Err` 立刻返回（同一个实例只可能命中一个实现）。
pub async fn guard_approve_for_task(task_id: Uuid) -> std::result::Result<(), ApiError> {
    let readers: Vec<Arc<dyn DocReader>> = {
        let registry = read_registry();
        registry.readers.values().cloned().collect()
    };
    for reader in &readers {
        reader.guard_approve(task_id).await?;
    }
    Ok(())
}

/// 审批实例落终态后，把结论扇出给**所有**已注册实现（`handler.rs` 里那 4 处扇出的替换点）。
///
/// # 为什么是「全部调一遍」而不是「按实例的 biz_type 只调一个」
///
/// 今天的行为就是全部调一遍：撤回 / 作废 / 终止接口拿到的只有实例 id，实现方自己查实例、
/// 自己判断「这条结论是不是给我的」（[`DocReader::on_settled`] 的实现方约定 2）。
/// 这是**有意保留**的：按 `biz_type` 精确分派会让「历史实例的 biz_type 与回调不匹配」
/// 这种情况（曾经真实发生过：作废审批挂在 `STOCK_IN_CANCEL` 而上游按过账路径收尾）
/// 从「被另一条桥捞回来」变成「谁都不处理」。
///
/// # 返回值
///
/// 实际调用了几次（= 已注册的实现数）。用于日志与测试断言，不参与业务判定。
///
/// # 失败处理
///
/// 单个实现 panic 会打断整个扇出（`on_settled` 没有错误通道，约定见 trait 注释）；
/// `Err` 不在这里发生 —— 落状态失败由实现方自己记日志并打回可操作状态。
pub async fn dispatch_settled(instance_id: Option<Uuid>, instance_status: &str) -> usize {
    // 锁在 await 之前放掉：先把 Arc 们克隆出来。
    let readers: Vec<Arc<dyn DocReader>> = {
        let registry = read_registry();
        registry.readers.values().cloned().collect()
    };
    for reader in &readers {
        reader.on_settled(instance_id, instance_status).await;
    }
    readers.len()
}

/// 清空进程级注册表（**仅供单测**，生产代码不许调用）。
#[cfg(test)]
pub fn reset_for_tests() {
    let mut registry = write_registry();
    *registry = DocRegistry::new();
}

/// 进程级注册表的单测串行锁（**仅供单测**）。
///
/// 注册表是进程级单例：所有「reset + 注册 + 断言」的用例必须拿**同一把**锁。
/// 实现分散在业务域（P2-33 起 `erp::doc_reader_impl_tests` 也会注册 16 个实现），
/// 各测试模块各定义一份锁挡不住彼此 —— 断言会随执行顺序漂移（`dispatch_settled`
/// 会看到别的用例注册的实现）。所以锁放在持有注册表的这个文件里，谁要动全局注册表都来拿它。
#[cfg(test)]
pub static GLOBAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
