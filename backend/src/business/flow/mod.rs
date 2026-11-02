//! 可编辑审批流 · 定义侧（批次 D · D1）。
//!
//! 依据：卡面 t_a92e9d5e；`docs/批次D-工作流审批-技术架构.md` §3（流程模型）/ §3.7（发布校验 12 条）/
//! §6.1（DDL）/ §8（环检测）/ §10.1.1（定义侧 11 个接口）。
//!
//! # 一句话定义
//!
//! **用户编辑的是一张图，引擎执行的是这张图在某一个版本上的快照。**
//! 本模块负责「编辑 → 校验 → 发布成不可变快照」这一段；运行时（实例 / 待办 / 抄送）是 D2–D5 的范围。
//!
//! # 三个表示（架构 D1，本模块只做前两层）
//!
//! | 层 | 存哪 | 谁写 | 可变性 | 本模块的落点 |
//! |----|------|------|--------|--------------|
//! | **L1 编辑期行** | `biz_flow_node` / `biz_flow_edge` / `biz_flow_node_assignee` | `PUT /version/{vid}/graph`（只写 DRAFT） | 仅 DRAFT 可改 | [`model::replace_graph`] / [`model::load_graph`] |
//! | **L2 发布快照** | `biz_flow_def_version.graph_snapshot` JSONB + `graph_hash` | `POST /def/{id}/publish`（从 L1 序列化） | **不可变** | [`graph::FlowGraph::snapshot_json`] / [`model::apply_publish`] |
//! | L3 运行期内存图 | 进程内 LRU（key = `version_id`） | D2 引擎 | 每次流转重建 | **不在本卡范围** |
//!
//! 为什么 L1 / L2 都要：只留 JSONB → 改一个节点要重写整块、并发编辑互相覆盖、无法
//! `WHERE` 反查「哪些流程用了这个角色」；只留行 → 行是「可改」的，改一行就等于悄悄改了
//! 所有引用该版本的在跑实例（版本漂移的第二条路径，比换版本更隐蔽）。
//!
//! # 七种节点与三条硬约束（架构 §3.2）
//!
//! `START` / `APPROVAL` / `COUNTERSIGN` / `CONDITION` / `PARALLEL` / `CC` / `END`。
//!
//! 1. **审批类（`APPROVAL` / `COUNTERSIGN` / `CC`）出度必须 = 1**（D7：只有显式网关能分叉）。
//!    允许隐式分叉 ⇒ 退回时「上一节点」有歧义 ⇒ 退回功能不可实现。
//! 2. **`CONDITION` 的出边恰有一条 `is_default=true`，其余必须带条件**（架构 §3.2 硬约束 2）。
//! 3. **恰一个 `START`、至少一个 `END`、所有节点可达且都能走到 `END`**。
//!
//! # 环检测为什么在应用层（架构 D20 / §8）
//!
//! 实测：200 节点 + 60 条跳跃边的**合法 DAG** 上，递归 CTE 的路径枚举写法跑 **300 秒未结束**；
//! `UNION` 去重写法跑得完但**识别不出环**（有环也是 200 行）。而「一次读全图」只要 **0.250 ms**，
//! 内存 O(V+E) 遍历是微秒级。所以 [`graph::FlowGraph::find_cycle`] 是纯 Rust DFS（`visited` + `in_stack`），
//! **全模块 `grep RECURSIVE` 必须 0 命中**（验收 6）。
//!
//! # 12 条发布校验（卡面 R1–R12 + 架构 §3.7 的 V7/V9/V12）
//!
//! [`graph::FlowGraph::validate`] 一次跑完 R1–R15，返回**全部**不通过项（不是第一个）：
//! 前端编辑器要把所有问题一次性标在图上。每条规则的错误码与文案格式见 `graph.rs` 的规则注释，
//! 卡面 12 条与架构 V1–V12 的映射表见 `docs/批次D-D1-验证证据.md`。
//!
//! # 三条红线（卡面「红线」段）
//!
//! 1. **环检测必须应用层 DFS**，严禁递归 CTE —— 见上。
//! 2. **`biz_flow_node_assignee` 必须独立表**，不许塞进 `node.config` JSONB
//!    （无法外键 / 无法反查影响面 / 本栈驱动不支持数组参数绑定，架构 §0.2-C）。
//! 3. **PUBLISHED 版本的行只读** —— [`handler::save_graph`] 对非 DRAFT 版本直接 4xx，
//!    不是「前端隐藏按钮」。
//!
//! # 与其它模块的边界
//!
//! - **不碰 `business/task/`**（除 [`task_bridge`]）：任务侧的 `approval_instance_id` 与
//!   `REVIEW→DONE` 闸门由 D3a 的门闸 + D3b 的 [`task_bridge`] 负责；本模块其余部分对任务表
//!   零依赖，任务 ↔ 流程的全部耦合都收在那一个文件里。
//! - **不碰 `business/aiagent/`**：AI 审批人是 D4 的解析逻辑（D18）。
//! - **不碰 `shared/scope.rs`**：流程定义是全局配置，走权限码而不是数据范围（见 `model.rs` 的 SCOPE-EXEMPT）。

pub mod assignee;
pub mod batch;
pub mod dto;
pub mod engine;
pub mod expression;
pub mod graph;
pub mod handler;
pub mod internal_control;
pub mod linkage;
pub mod maintain;
pub mod model;
pub mod node_config;
pub mod notify;
pub mod notify_node;
pub mod task_bridge;
pub mod task_node;
pub mod template_vars;
pub mod timeout;
