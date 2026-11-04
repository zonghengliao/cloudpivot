//! 账务不变量巡检的**结果落库 + 看板**（P2-7）。
//!
//! # 它解决什么
//!
//! `scripts/verify/erp_invariants.py` 早就能把「账是不是错的」查清楚，但结论只打在终端里：
//! 跑完即散，没有历史，于是「账务健康度这 30 天是变好还是变坏」无人能答；也没法在新窗口里
//! 回答「现在有哪几条红着」。本模块把每次运行**落成一行**，并提供只读看板。
//!
//! # 分工（不重写检查，只搬结果）
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`runner`] | 起子进程跑既有脚本 → 解析末尾 JSON → 写 `sys_invariant_result`；内置 cron `invariant_audit` 每天 02:20 触发 |
//! | [`model`] | `sys_invariant_result` 强类型模型 + 分页 / 最新一行 / 30 天趋势 / 连续失败次数 |
//! | [`dto`] | 看板与分页的请求 / 响应结构 |
//! | [`handler`] | `GET /support/invariant/overview`、`/page`、`/{id}`、`POST /support/invariant/run` |
//!
//! **为什么直接跑脚本而不是在 Rust 里重写检查**：闸③（`scripts/check.sh`）判红用的就是
//! 这个脚本，重写一份 Rust 版迟早两套口径分叉 —— 看板说绿、闸③说红，比没有看板更糟。
//! 落库的是**同一次运行的同一份输出**，两边只有一份真相。
//!
//! # 写库失败 / 脚本失败怎么区分
//!
//! - 脚本跑成但有失败项 → `status = FAIL`（这是账务结论，`blockers / warns` 分级）；
//! - 脚本没跑成（找不到脚本 / python 缺失 / 连不上库 / 超时 / 输出解析失败）→
//!   `status = ERROR` + `error` 写明原因。**绝不写成 OK**：静默假绿会让看板替错误的账背书。
//! - 只有**写库本身**失败才把 `DbError` 往上抛（此时没有可信的落库结论）。
//!
//! # 接口清单
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/api/support/invariant/overview` | `support:invariant:list` | 看板：最近一次结论 + 近 N 天（默认 30）趋势 + 当前失败项 |
//! | GET | `/api/support/invariant/page` | `support:invariant:list` | 运行历史分页（`page` / `page_size` / `status?`） |
//! | GET | `/api/support/invariant/{id}` | `support:invariant:list` | 单次运行全量明细（`detail`） |
//! | POST | `/api/support/invariant/run` | `support:invariant:run` | 立即跑一次并落库（同步等待，最长 300s） |
//!
//! 看板是**只读**的：没有增删改接口，结果行只能由 [`runner`] 写入。

pub mod dto;
pub mod handler;
pub mod model;
pub mod runner;
