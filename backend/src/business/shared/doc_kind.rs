//! 业务类型词表 —— **转发垫片**（P2-27 · P2-B 解环）。
//!
//! 本模块**没有内容**：七根轴的枚举、`route()` 拼写规则、`COLUMN_DOMAINS` 登记表
//! 全部在独立 crate `cloudpivot-doc` 里（`backend/crates/cloudpivot-doc/src/lib.rs`）。
//!
//! # 为什么留这层转发，而不是把 141 处调用点全改一遍
//!
//! 1. **依赖方向**：解环要的是「crate 图无环」，不是「路径好看」——
//!    `cloudpivot → cloudpivot-doc`（单向），`cloudpivot-doc` 零依赖（`cargo tree -p cloudpivot-doc`
//!    只有一个根节点）。词表被 ERP 全域引用、词表一致性测试又要 import `business::erp::*`，
//!    词表搬走后这条回边就不存在了。
//! 2. **一致性测试留在应用 crate**：`doc_kind_tests.rs` 比的是「词表 ↔ 前端 TS 常量表 ↔
//!    库内值域 ↔ ERP 写入点字面量」，那是**应用内**的约束，不该塞进零依赖的 crate。
//!    它仍然 `use super::*;`，靠的就是本文件的 glob 转发。
//! 3. **改动面最小**：`crate::business::shared::doc_kind::DocKind` 等 141 处引用一字未动
//!    —— 拆目录卡（P2-01~26）刚做完，再叠一轮全量 import 重写只会放大冲突面。
//!
//! 调用点逐步改到 `cloudpivot_doc::*`（或继续用本路径）都可以；**但不要在本文件里加代码**：
//! 任何新词表项都进 crate，否则「唯一真相」又裂成两半。
//!
//! # 别在这条路径上做文本解析
//!
//! 本文件是转发，**没有常量定义**。按路径抓词表的 4 个地方已经改成指向 crate：
//! `scripts/verify/biz_type_domain.py`、`scripts/verify/regression/erp_invariants_e4_regression.py`、
//! `frontend/tests/unit/doc-kinds-mirror.test.ts`、`frontend/tests/unit/flow-biz-link.test.ts`。
//! 有人再往这里加一个解析器 = 解析到空集还判绿（P2-26 记录过的同一个坑）。

pub use cloudpivot_doc::*;

/// 词表一致性用例（本 crate 的 `doc_kind_tests.rs`，文件仍在本目录）。
///
/// 保留在应用 crate：它要 import `business::erp::*` / `business::shared::state_machine`
/// 做逐字比对，是「应用内跨轴一致」的回归锁，不是词表自身的属性。
#[cfg(test)]
#[path = "doc_kind_tests.rs"]
mod doc_kind_tests;
