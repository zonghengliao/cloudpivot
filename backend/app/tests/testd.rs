//! 测试D · 其余业务模块测试入口（公告/通知/帮助/存储/项目/日志/互斥/导入导出 + 8 条联动）。
//!
//! `cargo test --test testd -- --test-threads=1 --nocapture`
//!
//! 用例按领域分文件；夹具与连库细节见 [`testd::common`]。
//! 数据库只允许 `cloudpivot_test`（`common::ensure_db` 有硬校验）。

#[path = "testd/mod.rs"]
mod testd;
