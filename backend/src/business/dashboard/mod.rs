//! 工作台「我的指标」（实名口径的个人统计）。
//!
//! # 接口
//!
//! | 方法 | 路径 | 权限 | 说明 |
//! |------|------|------|------|
//! | GET | `/api/business/dashboard/my-metrics` | **登录即可**（不挂权限码） | 4 张个人统计卡：待办审批 / 本周已办 / 今日任务 / 逾期 |
//!
//! # 为什么和「系统信息」的 overview 分开
//!
//! `support/dashboard/overview` 的 `metrics` 是**系统级**口径（角色总数、菜单总数、
//! 近 7 天操作日志…），只给超管看。个人指标混进那份响应里会让超管视图出现
//! 「角色总数 12 张」和「我的待办 3 张」并列的语义混乱，而且两边的刷新节奏、演进方向
//! 完全不同。所以这里是**独立接口**：各自演进，互不影响（overview 一个字段都没动）。
//!
//! # 为什么落在 `business/` 容器而不是 `support/`
//!
//! 底座（`system/` / `support/`）**不得反向依赖业务模块**：这四张卡的数字全部来自
//! `biz_flow_task` / `biz_task`，只有 `business/` 里的 flow / task 模块知道正确口径。
//! 放在底座会把「底座依赖二开业务表」写死进基础版，专业版 / 企业版复用底座时立刻编译失败。
//!
//! # 口径复用（不许另写一套 SQL）
//!
//! - 待办审批 / 本周已办：直接调 [`crate::business::flow::model::page_my_tasks`]，
//!   与审批中心 `/business/flow/task/todo`、`/done` **同一条 SQL 谓词**
//!   （`MY_TODO_STATUSES` / `MY_DONE_STATUSES` + P11 的「实例还在跑」过滤）；
//! - 今日任务 / 逾期 / 今日已完成：调 [`crate::business::task::model::my_tasks`] 后
//!   用 `bucket_of` 分桶（与 `/business/task/my` 的分组规则同一个函数），外加一条
//!   `count_done_on`（按 `done_at` 的 CST 日计数）。
//!
//! 自己写 count(*) 的后果是「卡片说 8、点进去列表说 7」——这种差异没人能一眼看出
//! 是口径问题还是数据问题，所以这里宁可多调一个函数，也不复制谓词。

pub mod dto;
pub mod handler;
pub mod model;
