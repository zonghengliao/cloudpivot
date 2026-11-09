//! 「我的指标」出参契约。
//!
//! 复用系统信息 Tab 的 [`MetricCardRes`]：字段完全一致
//! （`key / label / value / extra / unit / icon / theme / tip`），前端已有同一套渲染逻辑，
//! 不需要第二份卡片结构 —— 两套 DTO 只会在字段演进时互相漂移。
//!
//! 区别只在**语义**：overview 的卡片是系统级口径，本接口的卡片是「我自己」的口径，
//! 两者的 `key` 不重叠（`my_todo_count` / `my_done_week` / `my_task_today` / `my_overdue`），
//! 前端可以按 `key` 决定点击跳转。

pub use crate::support::dashboard::dto::MetricCardRes;
