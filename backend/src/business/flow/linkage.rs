//! 跨单据联动 · **动作白名单**（【D2】流程配置化 · 阶段 4）。
//!
//! # 一句话
//!
//! 把「单据状态变了以后还要做哪些事」从代码里搬到配置里，但**只允许配置白名单动作**
//! ——用户能配「在 `STOCK_IN/POSTED` 之后发一条通知」，配不了「执行这段 SQL」。
//!
//! # 为什么不能直接配 SQL
//!
//! 数据库账号是应用身份，任何一条配置里的 SQL 都以应用权限执行：
//! 一条 `DELETE FROM biz_voucher` 就能把账删干净，而且**审计里看是「系统自动」干的**。
//! 所以本模块的配置面只有两个自由度：**选哪个预定义动作** + **给该动作声明过的参数**。
//!
//! # 结构（四件套 + 校验/引擎）
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`spec`] | 白名单动作注册表（11 个动作的参数契约 / 事务归属 / 失败等级）+ 第 2 层参数校验 |
//! | [`model`] | `biz_flow_linkage_rule` 的读写；配置写入口（超管 + 二次确认 + 留痕）；读路径 fail-safe |
//! | [`actions`] | 动作实现（**只复用既有 handler/model**，不重写算法）+ 第 3 层运行时校验 |
//! | [`engine`] | 执行引擎（seq 排序 / 事务边界 / 失败策略）+ 第 2 步的 `SEND_NOTICE` 接管入口 |
//! | [`admin`] | 配置管理面（HTTP）：规则列表 / 增改 / 启停 / 删除 / **预览影响** |
//!
//! # 三层校验
//!
//! 1. **数据库 CHECK**：`ck_flow_linkage_action_code`（动作码白名单）、
//!    `ck_flow_linkage_no_sql_key`（参数里不许有 sql/statement/query 这类键）；
//! 2. **写配置时**（[`model::upsert_rule`]）：触发点/状态/动作码/参数键/类型/必填全查一遍，
//!    任何一条不过 → `400`；
//! 3. **运行时**（[`engine::run`] / [`engine::status_notice`]）：执行前再校验一次参数与
//!    target 是否存在，不过 → `ERROR` 日志 + 跳过该动作（不中断整个流程）。
//!
//! # 事务边界（详表见 [`engine`] 模块文档）
//!
//! - `InTx` 动作跑在**调用方的事务**里：关键动作失败 → [`engine::run`] 返回 `Err`，整笔回滚；
//! - `AfterCommit` 动作（发通知 / 建任务）在**提交之后**跑，失败只记日志，永不回滚业务。
//!
//! # 本卡的范围（第 1 步 + 第 2 步）
//!
//! - ✅ 建表 / 白名单 / 三层校验 / 执行引擎 / 单测；
//! - ✅ 第 2 步：`SEND_NOTICE` 接管任务状态通知（并行影子对比 → 一致后接管）；
//! - ⛔ 第 3 步（财务动作 `GENERATE_VOUCHER` / `WRITE_INVENTORY` …）**另开卡**：
//!   本卡把它们留在占位状态（`spec.implemented = false`），配置写入直接 `400`，
//!   运行时打 ERROR 跳过，绝不会半执行。
//!
//! # 与其它模块的边界
//!
//! - **不碰业务算法**：动作体只调既有函数（[`spec::ActionSpec::reuse`] 里逐个点名）；
//! - **不碰状态机**：本模块不实现流转，`UPDATE_DOC_STATUS` 的占位说明指向
//!   `shared::state_machine::transition`；
//! - **不另造通知通道**：一切通知都走 `business::notice::dispatch`。

pub mod actions;
pub mod admin;
pub mod engine;
pub mod model;
pub mod spec;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_presets;

#[cfg(test)]
mod tests_wiring;

pub use engine::{LinkageContext, LinkageRun, NoticeRoute, Outcome, RuleRun, ShadowCompare};
pub use model::{LinkageRuleRow, RulePatch};
pub use spec::{ActionSpec, ParamKind, ParamSpec, Phase, Severity};
