//! 业务容器（内核侧）。
//!
//! 只保留 `shared` —— 跨域基座（数据权限 / 状态机 / 内控与成本开关 / 凭证接缝 / 取号引擎）。
//! 其余业务域（`erp` / `flow` / `hr` / `task` …）仍在应用 crate 里，等各自的抽包卡（C3~C15）。
//!
//! 路径刻意与搬包前一致（`business::shared`），所以包内既有的
//! `crate::business::shared::…` 引用一处都不用改。

pub mod shared;
