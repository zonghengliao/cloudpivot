//! 云枢内核（cloudpivot-kernel）—— 跨域基座。
//!
//! # 这个 crate 是什么
//!
//! 从应用 crate 里整体搬出来的三块**跨域基座**（P2-59 · C1，2026-11-15）：
//!
//! | 模块 | 原路径 | 内容 |
//! |------|--------|------|
//! | [`shared`] | `backend/crates/cloudpivot-kernel/src/shared/` | 错误码（326 个 5 位码）/ 常量 / 枚举 / 启动守卫 / 会话缓存 / 令牌吊销 / 指标目录 / 权限码别名 / 空闲超时 / 缓存闸门 / 测试夹具 |
//! | [`utils`] | `backend/crates/cloudpivot-kernel/src/utils/` | 无状态纯函数（UUID / 分页 / 树组装 / 客户端 IP / 统一的 500 出口） |
//! | [`business::shared`] | `backend/crates/cloudpivot-kernel/src/business/shared/` | 数据权限基座（`principal` / `scope` / `guard`）、状态机、内控开关、成本开关、凭证接缝、取号引擎、Markdown 管线 |
//!
//! # 谁依赖它
//!
//! **所有域 crate 与应用 crate 都依赖它，它不依赖任何域**。依赖方向只有一条向下箭头：
//!
//! ```text
//! cloudpivot-doc (L0)  ←  cloudpivot-kernel (L1)  ←  各域 crate (L2)  ←  cloudpivot-server (L3)
//! ```
//!
//! 内核里有 10 处曾经指向业务域的引用，落包时全部倒转成接缝（清单见 `Cargo.toml` 顶部注释与
//! `docs/architecture/P2-59-kernel抽crate收口.md`）。
//!
//! # 包内模块路径刻意保持原样
//!
//! `shared` / `utils` / `business::shared` 三个模块名与层级**和搬进来之前一模一样**，
//! 所以包内 21 826 行代码里的 `crate::shared::…` / `crate::business::shared::…`
//! **一行都不用改**；只有包外的调用点改成 `cloudpivot_kernel::…`。
//!
//! # 没有路由、没有权限码、没有迁移
//!
//! 本 crate 零 `#[nik::route]` / 零 `inventory::submit` / 零 `#[nik::permission]` / 零迁移文件
//! —— 「抽包后路由不许静默少」这条门禁对它天然成立。反过来，后面 14 张域包卡**不要照抄**
//! 这条验收口径（它们的路由条数必须「拆前 − 0」）。

pub mod business;
pub mod shared;
pub mod utils;
