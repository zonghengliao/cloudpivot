//! 工作任务管理 · 数据访问与领域行为。
//!
//! 依据：`docs/批次C+-工作任务管理-核心设计.md`
//! §七（可见性）、§十（创建默认值）、§十二（数据模型 7 张表 / 索引）、§十三 T1 行。
//!
//! # 本卡唯一的「非标准」之处：可见性是 **scope ∪ 参与关系（OR）**
//!
//! 批次 B 的 [`scope::paginate`] 注入的数据权限片段是**AND 关系**（`… AND dept_id IN (…)`），
//! 而任务管理的可见性设计文档写的是**并集**：
//!
//! ```text
//! 可见 = 部门数据范围 ∪ 我是负责人 ∪ 我是创建人 ∪ 我是分派人 ∪ 我是协作人/关注者
//! ```
//!
//! 这是跨部门协作能成立的唯一原因：把市场部的人拉进研发部的任务后，他必须能看见
//! **那一条**，而不是「因为他不在研发部所以看不见」。
//!
//! ## 实现方式：**参与 id 数组**（`docs/批次C+-工作任务管理-详细架构.md` §4.4 决策 D6）
//!
//! `scope.rs` 是批次 B 的门禁核心（闸①/闸② 都盯着它），本卡红线不许动。所以这里
//! 用一个**不改变片段语义**的拼法把 AND 片段包进 OR 组：
//!
//! ```sql
//! WHERE t.deleted_at IS NULL
//!   AND ( (1 = 1 {FILTER})                     -- ← paginate 把 ` AND …` 片段原位注入这里
//!         OR t.id = ANY(?::text::uuid[]) )     -- ← 我参与的 task_id 数组（含负责人/创建人/分派人）
//! ```
//!
//! `{FILTER}` 会被替换成 ` AND <scope 条件>`，于是左半边等价于 `(1 = 1 AND <scope 条件>)`
//! = `<scope 条件>`；整体就是 `scope 条件 OR 参与关系`。片段本身、参数顺序、count 剥离
//! ORDER BY 的能力全部仍由 [`scope::paginate`] 提供 —— 本模块只负责「放在哪儿」。
//!
//! ### 为什么是数组而不是 `OR EXISTS(...)`（D6 的实测依据）
//!
//! 第一版写的是「4 个列 OR + 1 个 `EXISTS` 子查询」。20 万任务下实测：
//!
//! | 写法 | `DeptAndChild` 范围耗时 | 计划 |
//! |---|---|---|
//! | 多 OR + `EXISTS` | **539 ms** | `Seq Scan on biz_task`（5 个 OR 把索引全废） |
//! | **参与 id 数组 `t.id = ANY(?)`** | **40 ms** | `BitmapOr(idx_biz_task_dept, biz_task_pkey)` |
//!
//! 优化器无法在「列 OR 子查询 EXISTS」之上做位图合并，只能退化成顺序扫描；换成
//! 一个「已经算好的 id 集合」后，`t.id = ANY(…)` 是纯粹的 pkey 条件，可以和部门条件
//! 组成 `BitmapOr`。id 集合由 [`my_member_task_ids`] 用一条索引查询算好（一次请求一次），
//! 把「我负责 / 我创建 / 我分派 / 我是协作人」四种身份**全部 OR 进同一个数组**，
//! 于是谓词里只剩一个 `ANY(?)`。
//!
//! ### 上限与降级（D7）
//!
//! 数组不是免费的：每个 id 一次索引探测。实测 6.6 万个 id 时查询 >15 s 被
//! `statement_timeout` 打断。所以 [`my_member_task_ids`] 有 **2000 硬上限**（[`MEMBER_SCOPE_LIMIT`]）：
//! 超过就返回 `truncated = true`，谓词降级成「只按范围」（数组参数传 `{}`，`ANY('{}')`
//! 恒假），并在分页响应里带 `member_scope_truncated: true` 让前端提示
//! 「我参与的任务过多，本视图只显示范围内的；『我参与的』请走独立 tab」。
//!
//! **这条拼法依赖一个前提**：`RowFilter::sql()` 生成的片段一定以 ` AND ` 开头。
//! 该前提由 [`assert_scope_fragment`] 在每次查询前运行时校验：将来若 `scope.rs` 改了
//! 片段形态（比如去掉前导 AND、改成 `OR`），这里会立刻 `500 + 63008` 报警，
//! 而不是静默变成「全表可见」——「静默越权」是这类拼接最危险的失败形态。
//!
//! # 为什么 `biz_task` 不登记进 `SCOPED_TABLES`
//!
//! 登记表清单（`scope.rs` 的 `SCOPED_TABLES`）服务的是「纯 AND 注入」的存量表，且
//! 启动自检会按 `information_schema` 校验列名。任务的可见性不是纯 AND，登记进去只会
//! 让 `filter_for` 的语义和实际用法不一致；本模块用**本地声明的** [`TASK_TARGET`]
//! 锚点（`ScopeTarget::new` 是 `pub const`，不需要改 `scope.rs`），
//! 因此启动自检也不会把 `biz_task` 当成「该注入却没注入」的表。

// ==================== 部件（TD-2 拆分，facade 重导出） ====================
//
// 本文件只做 facade：底座 / 行结构 / 查询 / 写入 / 校验 分居各部件文件，
// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——
// 所有 `use crate::business::<模块>::<本文件>::xxx` 的调用方一行不用改。

pub use super::task_model_ai::*;
pub use super::task_model_base::*;
pub use super::task_model_dep::*;
pub use super::task_model_flow::*;
pub use super::task_model_hierarchy::*;
pub use super::task_model_query::*;
pub use super::task_model_rows::*;
pub use super::task_model_schedule::*;
pub use super::task_model_stats::*;
pub use super::task_model_write::*;

// ==================== 测试（#[path] 挂到各测试部件） ====================
#[cfg(test)]
#[path = "task_model_tests.rs"]
mod tests;

/// P2-9 / DC-19：任务挂业务单据（doc_type / doc_id）的真库用例。
///
/// 覆盖白名单（63022）、单据不存在 / 已软删（63023）、反向查询（`list_by_doc`
/// 的可见性）、落库与详情下发、改任务清单 / 换单。`TASK_DB_URL` 未设置则整段 SKIP。
#[cfg(test)]
#[path = "task_doc_link_tests.rs"]
mod doc_link_tests;

/// E12：附件显示名的纯逻辑回归（不连库，`cargo test` 里必然跑到）。
#[cfg(test)]
#[path = "task_model_display_file_name_tests.rs"]
mod display_file_name_tests;

#[cfg(test)]
#[path = "task_model_depth_tests.rs"]
mod depth_tests;

/// T8-C：任务列表 `root_only`（只返回顶层任务）。
///
/// 分页 + 树形唯一可行的组合：列表只返回顶层，子任务靠展开懒加载。
/// 本模块钉住四件事：
///
/// ① 片段**无绑定参数** —— `scope::paginate` 的「片段参数恒前置」不受影响；
/// ② 片段追加在所有业务过滤**之后** —— SQL 里 `?` 的文本顺序与参数顺序仍一致；
/// ③ 真库：`list` 只含顶层，且 `total` 与 `list` **同口径**（不是「全部条数」，
///    否则分页器会显示「43 条 / 5 页」而实际只有 28 条顶层，第 6 页空白）；
/// ④ 不传 / `false` 时行为与 T8-B 之前完全一致（向后兼容：看板 / 甘特 / 燃尽图都走它）。
#[cfg(test)]
#[path = "task_model_root_only_tests.rs"]
mod root_only_tests;

/// T9-A：任务状态多选筛选（`GET /business/task/page?statuses=TODO,DOING`）。
///
/// 钉住五件事：
///
/// ① `IN (?,?,…)` 的占位符个数 = `statuses` 长度，绑定参数按 vec 顺序逐个 push
///    （参数顺序红线：`build_filters` 的 `params` 追加在 `visible_params` 之后）；
/// ② `Some(空 vec)` → 不产生任何片段（等价不筛，与旧行为逐字相同）；
/// ③ `statuses` 非空时**盖住**单值 `status`（不是两个条件同时生效）；
/// ④ 真库：多选只返回这些状态；单值 `statuses=DONE` 与旧 `status=DONE` 结果一致；
/// ⑤ 真库：`statuses` 与 `root_only` 组合时两个条件都生效。
#[cfg(test)]
#[path = "task_model_statuses_filter_tests.rs"]
mod statuses_filter_tests;

#[cfg(test)]
#[path = "task_model_stats_tests.rs"]
mod stats_tests;

#[cfg(test)]
#[path = "task_model_repair_tests.rs"]
mod repair_tests;

#[cfg(test)]
#[path = "task_model_cascade_tests.rs"]
mod cascade_tests;

#[cfg(test)]
#[path = "task_model_cascade_cancel_tests.rs"]
mod cascade_cancel_tests;
