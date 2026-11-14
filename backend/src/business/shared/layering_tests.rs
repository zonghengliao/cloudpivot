//! 分层护栏（P2-30 建立，P2-31 扩到**全部**业务域）：`business::shared` 是**跨域基座**，
//! 不许依赖任何业务域。
//!
//! # 为什么是硬护栏
//!
//! `shared`（数据权限 / 状态机 / 用户姓名快照 / 成本开关 / 单据行工具）被 `system`、
//! `support`、`business/*` 一起用，是「工具层」。它一旦反过来 import 某个业务域，就形成
//! 「工具层 → 业务层 → 工具层」的循环，改一个域会牵连所有人的底座。
//!
//! 历史上共有三条回边，P2-27~P2-31 已全部清掉：
//!
//! 1. `shared::doc_kind`（业务类型词表）→ 抽成零依赖 crate `cloudpivot-doc`（P2-27/28/29）；
//! 2. `state_machine` 的 `mod tests` 里一条用例调 ERP 的 `voucher_write::update_summary`
//!    → 用例搬回凭证侧 `business/erp/voucher/voucher_write.rs`（P2-30，用例名未改）；
//! 3. `conn.rs` 的文档链接指向 `changelog::record` → 改成文件路径散文（P2-31，
//!    intra-doc link 也是 rustdoc 依赖，注释回边一样算）。
//!
//! # 判据
//!
//! 扫描 `backend/src/business/shared/**/*.rs` 的**全文**（含行注释与文档链接 —— intra-doc link
//! 也是一条 rustdoc 依赖），命中 `business::<业务域>` 这个**模块路径 token** 即判红。
//! 业务域清单**从 `backend/src/business/` 目录动态枚举**（排除 `shared` 自己），
//! 新增业务域自动进入禁区，不用改本文件。
//!
//! 只认模块路径、不认裸域名，因为 `shared` 里合法地存在 `/business/erp/cost-switch` 这类
//! **HTTP 路径**（`cost_switch` 的整张前缀表）与 `erp.cost.enabled` 这类**配置键**——它们是
//! 字符串常量，不是编译依赖。
//!
//! 命中后别加「例外文件」白名单（白名单会让护栏变成装饰），按历史三例的处置方式改命中处：
//! 把链接/引用改成文件路径散文（如 `` `business/erp/period.rs::ensure_period_writable` ``），
//! 或者把被依赖的逻辑搬回依赖方（P2-30 的做法）。

use std::path::{Path, PathBuf};

/// 本域自己的名字：`business::shared` 内部互引是合法的，不进禁区。
const SELF_DOMAIN: &str = "shared";

/// 扫描面硬断言：`backend/src/business/` 下除自己外**至少**有这么多业务域目录。
///
/// P2-31 实测 19 个（aiagent / announcement / attachment / changelog / dashboard / erp /
/// flow / help / hr / impexp / masking / monitor / mutex / notice / pdf / permission /
/// project / storage / task）。留一格余量，但目录读空时必须判红 —— 否则路径基准一变，
/// 护栏就静默变绿。
const MIN_DOMAINS: usize = 18;

/// 扫描面硬断言：这些文件必须真的被扫到（防「路径基准变了 → 扫 0 个文件 → 静默变绿」）。
const REQUIRED_FILES: [&str; 5] = ["mod.rs", "state_machine.rs", "scope.rs", "principal.rs", "guard.rs"];

/// `backend/src/business/` 下除 `shared` 之外的业务域名。
fn business_domains() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("business");
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|err| panic!("读不到 business 目录 {}：{err}", dir.display()));
    let mut domains: Vec<String> = Vec::new();
    for entry in entries {
        let path = entry.expect("列 business 目录失败").path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        if name != SELF_DOMAIN {
            domains.push(name);
        }
    }
    domains.sort();
    assert!(
        domains.len() >= MIN_DOMAINS,
        "业务域枚举异常：只找到 {} 个（期望 ≥{MIN_DOMAINS}）—— 路径基准变了？护栏会静默变空",
        domains.len()
    );
    domains
}

/// 递归收集目录下的 `.rs` 文件（`shared` 目前是平的，递归只是防以后分子目录漏扫）。
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|err| panic!("读不到 shared 目录 {}：{err}", dir.display()));
    for entry in entries {
        let path = entry.expect("列 shared 目录失败").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    out.sort();
}

#[test]
fn shared_never_references_business_domains() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("business")
        .join("shared");
    // 判据 token 在这里拼出来，避免本文件自身成为命中点（本文件也在扫描面里）。
    let forbidden: Vec<String> = business_domains()
        .iter()
        .map(|domain| format!("business::{domain}"))
        .collect();

    let mut files: Vec<PathBuf> = Vec::new();
    rust_files(&dir, &mut files);

    let mut scanned: Vec<String> = Vec::new();
    let mut offenders: Vec<String> = Vec::new();
    for path in &files {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let text = std::fs::read_to_string(path).unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()));
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
        "business/shared 是跨域基座，不许依赖任何业务域（禁区 {} 个：{}）：\n{}",
        forbidden.len(),
        forbidden.join(" / "),
        offenders.join("\n")
    );
}
