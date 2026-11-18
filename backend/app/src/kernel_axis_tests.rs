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
/// 这是「双码」的真正守门人。判据分三条（**只加不减**，见卡 `t_a5eca17f`）：
///
/// 1. **别名不许悬空** —— `LEGACY_ALIASES` 每一行指向的规范码都必须真实存在于
///    `PERMISSIONS`：只加别名而 handler 没切 → 别名指向不存在的码，等于没扩；
/// 2. **已切换码一个不能少** —— 旧码列必须与 `contract_handler::LEGACY_PERMISSIONS`
///    逐条一致：handler 改了某个已切换码、别名表没跟 → 红；
/// 3. **新增码必须登记** —— `PERMISSIONS` 里比别名表多出来的码，只许是
///    `ADDED_AFTER_PREFIX_UNIFICATION` 登记过的「P2-4 前缀统一**之后**新增、
///    从来没有旧码」的码：忘了登记 = 忘了在别名表补它的旧码映射 → 红。
///
/// # 为什么不用「两个集合完全相等」
///
/// `execution`（M4-19 执行量统计）是前缀统一**之后**新增的码，从来没有
/// `erp:contract:execution` 这个旧码。往 `LEGACY_ALIASES` 塞一条
/// `("erp:contract:execution", "business:erp:contract:execution")` 也能换绿，
/// 但那会污染「一行一个**已切换**码」的语义：线上永不命中的假映射 +
/// 兼容期收尾（整表删除）时说不清哪些能删。所以「新增码」走白名单，
/// 「已切换码」走别名表，两种语义不混。
#[test]
fn contract_aliases_match_handler_permissions() {
    use cloudpivot_kernel::shared::permission_alias::{LEGACY_ALIASES, aliases_of};

    use crate::business::erp::contract::contract_handler::{LEGACY_PERMISSIONS, PERMISSIONS};

    /// P2-4（`erp:contract:*` → `business:erp:contract:*`）**之后新增、从来没有旧码**的码。
    ///
    /// 逐条登记是刻意留的一次人工确认：这类码**不该**有别名（给了就是放行一个从没存在过
    /// 的权限），但它必然不在别名表里 —— 只有登记在这里，上面的 ①③ 两条才不会把
    /// 「新增码」误报成「表格腐烂」。
    ///
    /// 以后加码时按语义分流：**真做过改名**的进 `LEGACY_ALIASES` + `LEGACY_PERMISSIONS`；
    /// **全新**的码登记到这里。
    const ADDED_AFTER_PREFIX_UNIFICATION: [&str; 1] = ["business:erp:contract:execution"];

    let canonical_in_table: BTreeSet<&str> = LEGACY_ALIASES.iter().map(|(_, c)| *c).collect();
    let canonical_in_code: BTreeSet<&str> = PERMISSIONS.iter().copied().collect();

    // ① 别名不许悬空（悬空别名 = 表格腐烂）。
    let dangling: BTreeSet<&str> = canonical_in_table.difference(&canonical_in_code).copied().collect();
    assert!(
        dangling.is_empty(),
        "LEGACY_ALIASES 指向了 contract_handler::PERMISSIONS 里不存在的码：{dangling:?}"
    );

    // ② handler 多出来的码只许是登记过的「新增码」。
    let only_in_code: BTreeSet<&str> = canonical_in_code.difference(&canonical_in_table).copied().collect();
    assert_eq!(
        only_in_code,
        ADDED_AFTER_PREFIX_UNIFICATION.into_iter().collect::<BTreeSet<&str>>(),
        "contract_handler::PERMISSIONS 里出现了别名表没有的码：要么它是「P2-4 之后新增、\
         从来没有旧码」的码（登记进 ADDED_AFTER_PREFIX_UNIFICATION），要么你忘了在 \
         LEGACY_ALIASES 补它的旧码映射"
    );

    // ③ 旧码列与 handler 的 `LEGACY_PERMISSIONS` 一对一（顺序也一致：
    //    `PERMISSIONS[i]` 与 `LEGACY_PERMISSIONS[i]` 是同一个动作位）。
    //    长度不再写死 5：由这条与 handler 的已切换码清单钉死（当前 5 条）。
    let legacy_in_table: Vec<&str> = LEGACY_ALIASES.iter().map(|(l, _)| *l).collect();
    assert_eq!(
        legacy_in_table, LEGACY_PERMISSIONS,
        "LEGACY_ALIASES 的旧码列必须与 contract_handler::LEGACY_PERMISSIONS 一致\
         （handler 换了已切换码而别名表没跟）"
    );

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
