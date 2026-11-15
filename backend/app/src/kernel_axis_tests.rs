//! 跨轴一致性用例（P2-59 · C1）：内核基座 ⇄ 业务实现的契约断言。
//!
//! # 为什么这两条用例在应用侧而不在内核里
//!
//! 它们比的是**两个轴**：内核里的「目录 / 别名表」对业务里的「采样器实现 / 权限码常量」。
//!
//! - `every_sampler_metric_is_registered`：`observability::CATALOG`（内核）⇄
//!   `task::metrics_sampler::Snapshot::gauges()`（task 域）；
//! - `contract_aliases_match_handler_permissions`：`permission_alias::LEGACY_ALIASES`（内核）⇄
//!   `business::erp::contract::contract_handler::PERMISSIONS`（erp 域）。
//!
//! 放在内核里就是**包级环**：test 配置下 `cloudpivot-kernel → task/erp`，而生产配置下
//! `task/erp → cloudpivot-kernel`（`cargo` 的 cyclic package dependency 直接拒编）。
//! 这种「实现 vs 登记表」的断言本来就属于装配层（bin 同时看得到两侧），所以搬到这里。
//!
//! 搬动前它们在 `shared/observability.rs` 与 `shared/permission_alias.rs` 的 `mod tests`
//! 里，用例名与断言一字未改（引用同一份常量，任一侧改动仍然立刻变红）。

use std::collections::BTreeSet;

/// 采样器真正写入的每一个名字都必须在内核的指标目录里 —— 否则「暴露出来了但没有说明」。
#[test]
fn every_sampler_metric_is_registered() {
    use cloudpivot_kernel::shared::observability;

    let emitted = crate::task::metrics_sampler::Snapshot::default().gauges();
    assert!(!emitted.is_empty(), "采样器不应一个指标都不写");
    for (name, _) in emitted {
        assert!(
            observability::is_registered(name),
            "采样器写入了未登记的指标 {name}：请补到 observability::CATALOG"
        );
    }
}

/// 权限码兼容别名表 ⇄ ERP 合同处理器的常量：任一侧改动即红。
///
/// 这是「双码」的真正守门人：只改 handler 的 5 个常量而不加别名 → 老令牌 403；
/// 只加别名而 handler 没切 → 别名指向不存在的码，等于没扩。
#[test]
fn contract_aliases_match_handler_permissions() {
    use cloudpivot_kernel::shared::permission_alias::{LEGACY_ALIASES, aliases_of};

    let canonical_in_table: BTreeSet<&str> = LEGACY_ALIASES.iter().map(|(_, c)| *c).collect();
    let canonical_in_code: BTreeSet<&str> = crate::business::erp::contract::contract_handler::PERMISSIONS
        .iter()
        .copied()
        .collect();
    assert_eq!(
        canonical_in_table, canonical_in_code,
        "LEGACY_ALIASES 的规范码必须与 contract_handler::PERMISSIONS 完全相同"
    );
    assert_eq!(LEGACY_ALIASES.len(), 5, "合同域 5 个码，一个不能多一个不能少");

    for (legacy, canonical) in LEGACY_ALIASES {
        assert!(
            canonical.starts_with("business:erp:contract:"),
            "{canonical} 必须以 business:erp:contract: 开头（与 ERP 其余 313 码同族）"
        );
        assert!(
            legacy.starts_with("erp:contract:"),
            "{legacy} 是历史码，必须是 erp:contract: 前缀"
        );
        assert_eq!(
            legacy.rsplit(':').next(),
            canonical.rsplit(':').next(),
            "别名只能换前缀，动作段必须一致（{legacy} vs {canonical}）"
        );
        assert_eq!(
            aliases_of(canonical),
            vec![legacy],
            "{canonical} 的对端码应只有 {legacy}"
        );
    }
}

/// 全量扩一次：5 个规范码 → 10 个码，且旧码 / 新码各 5 个。
#[test]
fn expanding_all_contract_codes_yields_both_families() {
    use cloudpivot_kernel::shared::permission_alias::{LEGACY_ALIASES, expand};

    let mut codes: Vec<String> = LEGACY_ALIASES
        .iter()
        .map(|(_, canonical)| (*canonical).to_string())
        .collect();
    expand(&mut codes);
    assert_eq!(codes.len(), 10, "5 个规范码 + 5 个别名：{codes:?}");
    assert_eq!(codes.iter().filter(|c| c.starts_with("erp:contract:")).count(), 5);
    assert_eq!(
        codes.iter().filter(|c| c.starts_with("business:erp:contract:")).count(),
        5
    );
}
