//! 审批流需要任务侧做的两件事（P2-38 · P2-B 解环：摘掉 `flow → task`）。
//!
//! # 这个文件解决什么
//!
//! 流程引擎在 `TASK_NODE` 上**必须**造任务（`biz_task`），而审批结论落地时也**必须**
//! 推进任务状态 —— 这两处过去直接 `use` 任务域的 `model`，`flow` 因此认识
//! `task`，与 `task → flow`（P2-37 已摘）组成 2 环。
//!
//! 本模块是**调用方（`flow`）定义的接缝**：函数签名只收**本域算好的字面量**
//! （[`SpawnTaskSpec`]），实现放在中立接线域 `business::taskflow`，启动期由
//! `bootstrap` 注入。于是：
//!
//! ```text
//! 改前：flow ──直调 task::model / task::state──→ task
//! 改后：flow ──trait + 注册表──→ (抽象) ←──实现 + 启动期注册── business::taskflow
//! ```
//!
//! # 为什么 trait 定义在 `flow`（与 `flow::doc_reader` 同侧）
//!
//! 这里传的是**命令**（「给我造一张任务」），不是读别人的数据 —— 需求归调用方，
//! 实现归表属主（`biz_task` 的写入只能在 `taskflow`/`task` 一侧）。定义在 `task` 就等于
//! `flow` 必须 `import task`，边还在。
//!
//! # 六条纪律
//!
//! 1. **只在事务内**：两个方法都收 `&Transaction`，与节点激活 / 审批回调同生共死
//!    （造任务失败 → 整笔回滚，不许留「节点 ACTIVE、任务不存在」的实例）。
//! 2. **`SpawnTaskSpec` 只放字面量**：负责人解析、模板渲染、截止日计算**全部在 `flow` 侧算完**，
//!    接缝里不做任何业务判定 —— 否则判据会分叉成两份（`assignee::resolve` 只有一份）。
//! 3. **幂等归实现方**：唯一索引 `uk_biz_task_flow_node` 是最终依据，`23505` 当幂等成功，
//!    本侧只把结果翻译成 [`SpawnOutcome`]。
//! 4. **同一时刻只有一个实现**：重复安装 → `Err`（`bootstrap` 按编程错误处理）。
//! 5. **不许跨 `await` 持锁**：先克隆 `Arc` 再 `await`。
//! 6. **`flow/` 里不许出现任务域路径**：闸门见
//!    `flow::task_independence_tests::flow_source_has_no_task_references`（基线 0）。
//!
//! # 没装实现时的语义（每条都是降级，不是 500）
//!
//! | 入口 | 未安装 | 为什么是这个值 |
//! |---|---|---|
//! | [`task_of_node`] | `Ok(None)` | 幂等预查读不到 → 当作「还没造过」，让后面的 `spawn_task` 去撞唯一索引（真冲突仍是 `23505` 幂等） |
//! | [`spawn_task`] | `Ok(SpawnOutcome::Failed(500))` | 造不出任务就**不能**让节点静默 `ACTIVE`：那是「有节点、没活干」的死单。失败经引擎回滚整笔发起 |
//!
//! 生产**不允许**出现这种降级：`bootstrap::register_flow_task_port` 装不上就启动失败。

use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

pub use crate::business::flow::task_node::SpawnOutcome;

/// 造一张流程任务需要的**全部字面量**（`flow` 侧算好，接线域只落库）。
///
/// 字段口径与 `biz_task` 的列一一对应（除 `task_no` / `id` / `dept_id` 由实现方生成 /
/// 查询：取号、主键、负责人部门快照不属于「流程知道的事」）。
#[derive(Debug, Clone)]
pub struct SpawnTaskSpec {
    /// 生成它的流程实例（`biz_task.flow_instance_id`，也是幂等键的一段）。
    pub instance_id: Uuid,
    /// 节点 key（`biz_task.flow_node_key`，幂等键的一段）。
    pub node_key: String,
    /// 节点显示名（只进变更日志的说明文案，不入库）。
    pub node_name: String,
    /// 节点轮次（`biz_task.flow_round`，幂等键的一段）。
    pub round: i32,
    /// 任务标题（已渲染、已校验长度）。
    pub title: String,
    /// 任务描述（已渲染、已校验长度）。
    pub description: Option<String>,
    /// 优先级（`P0`–`P3`，已归一化）。
    pub priority: String,
    /// 是否需要两级验收（`config.task.need_review`）。
    pub need_review: bool,
    /// 负责人（多人解析里的第一个，按 `user_id` 排序后的第一个）。
    pub owner_id: Uuid,
    /// 协作人（其余解析出来的人，挂 `biz_task_member`）。
    pub member_ids: Vec<Uuid>,
    /// 创建人 / 流程发起人（`creator_id` / `create_by` / `update_by`）。
    pub creator_id: Uuid,
    /// 发起人姓名快照（变更日志的操作人）。
    pub creator_name: Option<String>,
    /// 截止日（`config.task.due` 或表单映射算出来的）。
    pub due_date: Option<NaiveDate>,
    /// 预估工时。
    pub estimate_hours: Option<f64>,
}

/// 审批流需要的**任务写入能力**（唯一实现者见 `business::taskflow::spawn`）。
///
/// 所有方法都**不给默认实现**：漏一个就是「节点在跑、任务没造」的死单，
/// required ⇒ 漏了编译不过。
#[async_trait]
pub trait FlowTaskPort: Send + Sync {
    /// 实现名（日志 / 启动摘要 / 用例断言，`snake_case`）。
    fn name(&self) -> &'static str;

    /// 幂等预查：同一实例 + 同一节点 + 同一轮是否已有未软删的任务。
    ///
    /// `Ok(None)` = 还没造过（调用方继续走 [`Self::spawn_task`]）；唯一索引仍是最终依据。
    async fn task_of_node(
        &self,
        tx: &Transaction,
        instance_id: Uuid,
        node_key: &str,
        round: i32,
    ) -> DbResult<Option<Uuid>>;

    /// **事务内**落一张任务（含协作人、任务号、变更日志）。
    ///
    /// 并发撞唯一索引 `uk_biz_task_flow_node` → 幂等成功（[`SpawnOutcome::AlreadyExists`]）；
    /// 撞任务号 → 实现方自己重试一次（`next_task_no` 的换月窗口）。
    async fn spawn_task(&self, tx: &Transaction, spec: SpawnTaskSpec) -> DbResult<SpawnOutcome>;

    /// 审批单落终态后**推进任务**（`FlowCallback` 身份）并推进流程节点。
    ///
    /// # 为什么没有 `Err`
    ///
    /// 它跑在审批事务**提交之后**（审批结论已落库，红线：不许因任务侧失败回滚审批单），
    /// 失败只记 `WARN`；返回值是被推进的任务 id（没推进 / 不该推进 → `None`）。
    ///
    /// 事务边界：实现方自己开事务（会先写任务状态、再单独开一笔做父链级联），
    /// 与 P2-38 之前的 `on_instance_settled` 逐字一致。
    async fn on_instance_settled(&self, instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid>;
}

/// 接缝注册表（**实例**版，可单测；进程级单例见 [`registry`]）。
#[derive(Default)]
pub struct FlowTaskPortRegistry {
    /// 唯一实现者（接线域）。`None` = 启动链还没装（或单测里被清空）。
    port: Option<Arc<dyn FlowTaskPort>>,
}

impl FlowTaskPortRegistry {
    /// 空注册表（未安装状态）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 安装实现。已装过再装 → `Err`（**不静默覆盖**：谁生效不该取决于注册顺序）。
    pub fn install(&mut self, port: Arc<dyn FlowTaskPort>) -> std::result::Result<(), String> {
        if self.port.is_some() {
            return Err("流程任务接缝重复安装：一个进程只能有一个接线域实现".to_string());
        }
        self.port = Some(port);
        Ok(())
    }

    /// 取实现（未安装 → `None`）。
    pub fn port(&self) -> Option<Arc<dyn FlowTaskPort>> {
        self.port.clone()
    }

    /// 是否已安装。
    pub fn is_installed(&self) -> bool {
        self.port.is_some()
    }
}

/// 进程级注册表（「空表先就位、启动期写满、之后只读」，与 `task::approval_port` 同款）。
static REGISTRY: OnceLock<RwLock<FlowTaskPortRegistry>> = OnceLock::new();

fn registry() -> &'static RwLock<FlowTaskPortRegistry> {
    REGISTRY.get_or_init(|| RwLock::new(FlowTaskPortRegistry::new()))
}

fn read_registry() -> std::sync::RwLockReadGuard<'static, FlowTaskPortRegistry> {
    registry().read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_registry() -> std::sync::RwLockWriteGuard<'static, FlowTaskPortRegistry> {
    registry().write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 启动期安装实现（`bootstrap` 调用；重复安装返回 `Err`）。
pub fn install(port: Arc<dyn FlowTaskPort>) -> std::result::Result<(), String> {
    write_registry().install(port)
}

/// 是否已安装（启动期自检 / 用例断言用）。
pub fn is_installed() -> bool {
    read_registry().is_installed()
}

/// 取已安装的实现（未安装 → `None`）。调用方**不要**长期持有，用完就放。
pub fn port() -> Option<Arc<dyn FlowTaskPort>> {
    read_registry().port()
}

/// 已安装实现的名字（启动摘要 / 排障用；未安装 → `None`）。
pub fn port_name() -> Option<&'static str> {
    port().map(|port| port.name())
}

/// 幂等预查（未安装 → `Ok(None)`：当作还没造过，让 `spawn_task` 去撞唯一索引）。
pub async fn task_of_node(tx: &Transaction, instance_id: Uuid, node_key: &str, round: i32) -> DbResult<Option<Uuid>> {
    let port = port();
    match port {
        None => Ok(None),
        Some(port) => port.task_of_node(tx, instance_id, node_key, round).await,
    }
}

/// 事务内落一张任务（未安装 → 告警 + `Failed(500)`：不许让节点静默 `ACTIVE`）。
pub async fn spawn_task(tx: &Transaction, spec: SpawnTaskSpec) -> DbResult<SpawnOutcome> {
    let port = port();
    match port {
        None => {
            tracing::warn!(
                instance_id = %spec.instance_id,
                node_key = %spec.node_key,
                "流程任务接缝未安装：无法生成任务（启动链漏装）"
            );
            Ok(SpawnOutcome::Failed(ApiError::new(
                500,
                crate::shared::error_code::INTERNAL_ERROR,
                "流程任务接缝未安装：节点无法生成任务",
            )))
        }
        Some(port) => port.spawn_task(tx, spec).await,
    }
}

/// 审批结论落地：推进任务 + 推进流程节点（未安装 → 告警 + `None`）。
pub async fn on_instance_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let port = port();
    match port {
        None => {
            tracing::warn!(
                instance_id = ?instance_id,
                instance_status,
                "流程任务接缝未安装：审批结论未能推进任务（启动链漏装）"
            );
            None
        }
        Some(port) => port.on_instance_settled(instance_id, instance_status).await,
    }
}

/// 清空进程级注册表（**仅供单测**，生产代码不许调用）。
#[cfg(test)]
pub fn reset_for_tests() {
    *write_registry() = FlowTaskPortRegistry::new();
}

/// 进程级注册表的单测串行锁（**仅供单测**）。
#[cfg(test)]
pub static GLOBAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
