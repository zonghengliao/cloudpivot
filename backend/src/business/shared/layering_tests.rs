//! 分层护栏（P2-30）：`business::shared` 是**跨域基座**，不许依赖任何业务域。
//!
//! # 为什么是硬护栏
//!
//! `shared`（数据权限 / 状态机 / 用户姓名快照 / 成本开关）被 `system`、`support`、`business/*`
//! 一起用，是「工具层」。它一旦反过来 import 某个业务域，就形成
//! 「工具层 → 业务层 → 工具层」的循环，改一个域会牵连所有人的底座。
//!
//! 历史回边有两条，P2-27~P2-30 已全部清掉：
//!
//! 1. `shared::doc_kind`（业务类型词表）→ 抽成零依赖 crate `cloudpivot-doc`（P2-27/28/29）；
//! 2. `state_machine` 的 `mod tests` 里一条用例调 ERP 的 `voucher_write::update_summary`
//!    → 用例搬回凭证侧 `business/erp/voucher/voucher_write.rs`（P2-30，用例名未改）。
//!
//! # 判据
//!
//! 扫描 `backend/src/business/shared/**/*.rs` 的**全文**（含行注释与文档链接 —— intra-doc link
//! 也是一条 rustdoc 依赖），命中 `business::` + `erp` 这个**模块路径 token** 即判红。
//!
//! 只认模块路径、不认裸 `erp`，因为 `shared` 里合法地存在 `/business/erp/cost-switch` 这类
//! **HTTP 路径**（`cost_switch` 的整张前缀表）与 `erp.cost.enabled` 这类**配置键**——它们是
//! 字符串常量，不是编译依赖。
//!
//! 要放开到别的业务域时，往 [`FORBIDDEN_DOMAINS`] 里加域名的**同时**改掉命中处，
//! 不要加「例外文件」白名单（白名单会让护栏变成装饰）。

use std::path::Path;

/// `shared` 不得引用的业务域（模块路径里的域名，不含 `business::` 前缀）。
///
/// 目前只有 `erp` 一个历史越界者；新增条目请连带把命中点改成文件路径描述
/// （如 `` `business/erp/period.rs::ensure_period_writable` ``），而不是留着链接。
const FORBIDDEN_DOMAINS: [&str; 1] = ["erp"];

/// 扫描面硬断言：这些文件必须真的被扫到（防「路径基准变了 → 扫 0 个文件 → 静默变绿」）。
const REQUIRED_FILES: [&str; 5] = ["mod.rs", "state_machine.rs", "scope.rs", "principal.rs", "guard.rs"];

#[test]
fn shared_never_references_business_domains() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("business")
        .join("shared");
    // 判据 token 在这里拼出来，避免本文件自身成为命中点（本文件也在扫描面里）。
    let forbidden: Vec<String> = FORBIDDEN_DOMAINS
        .iter()
        .map(|domain| format!("business::{domain}"))
        .collect();

    let mut scanned: Vec<String> = Vec::new();
    let mut offenders: Vec<String> = Vec::new();

    let entries = std::fs::read_dir(&dir).unwrap_or_else(|err| panic!("读不到 shared 目录 {}：{err}", dir.display()));
    for entry in entries {
        let path = entry.expect("列 shared 目录失败").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()));
        scanned.push(name);
        for (index, line) in text.lines().enumerate() {
            for token in &forbidden {
                if line.contains(token.as_str()) {
                    offenders.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
                }
            }
        }
    }

    for required in REQUIRED_FILES {
        assert!(
            scanned.iter().any(|name| name == required),
            "扫描面缺 {required}（实测 {} 个 .rs：{:?}）—— 路径基准变了？护栏会静默变空",
            scanned.len(),
            scanned
        );
    }

    assert!(
        offenders.is_empty(),
        "business/shared 是跨域基座，不许依赖业务域（{}）：\n{}",
        forbidden.join(" / "),
        offenders.join("\n")
    );
}
