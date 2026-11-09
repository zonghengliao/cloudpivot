//! 测试C · P1-3：`business/permission`（数据范围权限）用例集 A01–A12。
//!
//! # 卡片口径与实际的差异（已记入测试报告）
//!
//! 卡片把 `business/permission` 描述成「权限码全集查询 / 权限码格式校验 / 权限树」。
//! 实际这个模块是 **数据范围权限**（`/business/permission/scope/*` 5 个路由 +
//! `scope_model` 的纯逻辑），**权限码**的权威来源是 `sys_menu.auth_code` 与后端
//! `#[nik::permission]` 注解——那部分已由 `system::menu::tests::n10`（注解 ↔ 菜单一致）
//! 与 `system::authz_tests::o14`（权限码格式）覆盖。本文件因此按**实际接口**测：
//! 数据范围的四个不变量。
//!
//! # 四个不变量
//!
//! 1. **不能授出比自己更宽的范围**（`ensure_kind_grantable`）——否则「建个宽角色」就是提权；
//! 2. **配置侧不接受非法组合**（`validate_custom_scope`）：自定义范围必须带部门，非自定义
//!    范围不许带部门集合（静默忽略会让管理员以为生效了）；
//! 3. **越界 / 不存在的部门必须被点名**（`exceeded_dept_ids` / `missing_dept_ids`）；
//! 4. **树裁剪不泄露、不丢节点**（`crop_to_visible`）：父不可见时节点提升为根。
//!
//! 跑法：
//! ```bash
//! SYS_DB_URL="postgres://<user>:***@127.0.0.1:5432/cloudpivot_test" \
//!   cargo test --lib business::permission::tests -- --nocapture
//! ```

use nik::prelude::*;

use crate::business::permission::scope_model::{self, DeptRow};
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope::ScopeKind;
use crate::shared::error_code;

/// 造一个部门行。
fn dept_row(id: uuid::Uuid, parent: Option<uuid::Uuid>, code: &str, sort: i32) -> DeptRow {
    DeptRow {
        id,
        parent_id: parent,
        dept_code: code.to_string(),
        dept_name: format!("部门-{code}"),
        status: 1,
        sort_order: sort,
    }
}

/// 「仅本人 / 无部门」的操作者。
fn self_only_principal() -> ScopedPrincipal {
    ScopedPrincipal::for_test(uuid::Uuid::now_v7(), None, false, ScopeKind::SelfOnly, Vec::new(), true)
}

/// 「本部门及以下」的操作者（可见 1 个部门）。
fn dept_principal(dept: uuid::Uuid) -> ScopedPrincipal {
    ScopedPrincipal::for_test(
        uuid::Uuid::now_v7(),
        Some(dept),
        false,
        ScopeKind::DeptAndChild,
        vec![dept],
        false,
    )
}

// ==================== A01 不能授出「全部数据」 ====================

/// A01：只有「仅本人」范围的管理员把角色设成 `data_scope=1`（全部数据）→ 403。
///
/// 这是最直接的提权路径：集合校验对「不带集合的范围」恒通过，所以必须有形态校验。
#[tokio::test]
async fn a01_cannot_grant_all_scope_from_narrow_operator() {
    let operator = self_only_principal();
    let error =
        scope_model::ensure_kind_grantable(&operator, ScopeKind::All).expect_err("仅本人范围不得授出「全部数据」");
    assert_eq!(error.status, 403, "应 403，实际 {}/{}", error.status, error.code);
    assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);

    // 对照：收紧到「仅本人」永远允许
    assert!(
        scope_model::ensure_kind_grantable(&operator, ScopeKind::SelfOnly).is_ok(),
        "收紧范围不该被拦"
    );
    println!("[A01] 仅本人 → 授出「全部」403（{}）✓", error.msg);
}

// ==================== A02 超管 / 全部范围不受限 ====================

/// A02：超管（或自身范围已是「全部」的管理员）可以授任意范围（架构 §2.7：超管不受限）。
#[tokio::test]
async fn a02_super_admin_can_grant_any_scope() {
    let sup = ScopedPrincipal::for_test(uuid::Uuid::now_v7(), None, true, ScopeKind::All, Vec::new(), false);
    for kind in [
        ScopeKind::All,
        ScopeKind::Dept,
        ScopeKind::DeptAndChild,
        ScopeKind::SelfOnly,
        ScopeKind::Custom,
    ] {
        assert!(
            scope_model::ensure_kind_grantable(&sup, kind).is_ok(),
            "超管授出 {:?} 不该被拦",
            kind
        );
    }
    // 非超管但范围 = All 的管理员同样可以
    let all_admin = ScopedPrincipal::for_test(uuid::Uuid::now_v7(), None, false, ScopeKind::All, Vec::new(), false);
    assert!(
        scope_model::ensure_kind_grantable(&all_admin, ScopeKind::All).is_ok(),
        "范围=全部的管理员授出全部不该被拦"
    );
    println!("[A02] 超管 / 全部范围管理员可授任意范围 ✓");
}

// ==================== A03 部门级范围：无部门可见即不可授 ====================

/// A03：自身没有任何可见部门（仅本人 / 无部门）时，不能授出部门级范围 → 403。
#[tokio::test]
async fn a03_dept_scope_requires_visible_depts() {
    let operator = self_only_principal();
    for kind in [ScopeKind::Dept, ScopeKind::DeptAndChild] {
        let error = scope_model::ensure_kind_grantable(&operator, kind).expect_err("无部门可见者不得授出部门级范围");
        assert_eq!(error.status, 403, "应 403，实际 {}", error.status);
        assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);
    }

    // 有部门可见的操作者：允许（不宽于自身）
    let dept = uuid::Uuid::now_v7();
    let with_dept = dept_principal(dept);
    assert!(
        scope_model::ensure_kind_grantable(&with_dept, ScopeKind::Dept).is_ok(),
        "有部门可见者授出部门级范围应允许"
    );
    println!("[A03] 无部门可见者授部门范围 → 403；有部门者允许 ✓");
}

// ==================== A04 自定义范围必须带部门 ====================

/// A04：`data_scope=5`（自定义）但没选部门 → 400（不能静默降级成「仅本人」或「全部」）。
#[tokio::test]
async fn a04_custom_scope_requires_depts() {
    let error = scope_model::validate_custom_scope(ScopeKind::Custom, None).expect_err("自定义范围未选部门必须 400");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::DATA_SCOPE_INVALID);

    let empty: Vec<uuid::Uuid> = Vec::new();
    let empty_error =
        scope_model::validate_custom_scope(ScopeKind::Custom, Some(&empty)).expect_err("自定义范围空集合必须 400");
    assert_eq!(empty_error.code, error_code::DATA_SCOPE_INVALID);

    // 正常：带 2 个部门（含重复）→ 去重后 2 个
    let a = uuid::Uuid::now_v7();
    let b = uuid::Uuid::now_v7();
    let ok = scope_model::validate_custom_scope(ScopeKind::Custom, Some(&[a, b, a])).expect("自定义范围带部门应通过");
    assert_eq!(ok.expect("应有值"), vec![a, b], "去重且保序");
    println!("[A04] 自定义范围：空 → 400；带部门 → 去重保序 ✓");
}

// ==================== A05 非自定义范围不许带部门集合 ====================

/// A05：`data_scope≠5` 却提交了部门集合 → 400（静默忽略会让管理员以为生效了）。
#[tokio::test]
async fn a05_non_custom_scope_rejects_dept_set() {
    let dept = uuid::Uuid::now_v7();
    for kind in [
        ScopeKind::All,
        ScopeKind::Dept,
        ScopeKind::DeptAndChild,
        ScopeKind::SelfOnly,
    ] {
        let error =
            scope_model::validate_custom_scope(kind, Some(&[dept])).expect_err("非自定义范围带部门集合必须 400");
        assert_eq!(error.status, 400, "{kind:?} 应 400");
        assert_eq!(error.code, error_code::DATA_SCOPE_INVALID);
    }
    // 空集合 = 清除自定义部门，允许
    let empty: Vec<uuid::Uuid> = Vec::new();
    assert!(
        scope_model::validate_custom_scope(ScopeKind::Dept, Some(&empty))
            .expect("清空应允许")
            .is_some(),
        "非自定义范围 + 空集合语义是「清除」，应返回 Some(空)"
    );
    println!("[A05] 非自定义范围带部门集合 → 400；空集合 = 清除 ✓");
}

// ==================== A06 越界部门点名 ====================

/// A06：提交别人部门的 id → 被判越界，且**保序去重**（错误消息要点名，顺序要稳定）。
#[tokio::test]
async fn a06_exceeded_dept_ids_pointed_out() {
    let mine = uuid::Uuid::now_v7();
    let other_1 = uuid::Uuid::now_v7();
    let other_2 = uuid::Uuid::now_v7();
    let operator = dept_principal(mine);

    let exceeded = scope_model::exceeded_dept_ids(&operator, &[other_1, mine, other_2, other_1]);
    assert_eq!(
        exceeded,
        vec![other_1, other_2],
        "越界部门必须点名且保序去重，实际 {exceeded:?}"
    );
    assert!(
        scope_model::exceeded_dept_ids(&operator, &[mine]).is_empty(),
        "本部门不该被判越界"
    );

    // 点名文案：已知部门给「名称（ID）」，未知只给 ID（解析失败不改判 500）
    let rows = vec![dept_row(other_1, None, "B1", 0)];
    let text = scope_model::describe_depts(&exceeded, &rows);
    assert!(text.contains("部门-B1"), "已知部门应显示名称，实际 {text}");
    assert!(text.contains(&other_2.to_string()), "未知部门应显示 ID，实际 {text}");
    println!("[A06] 越界点名：{text} ✓");
}

// ==================== A07 不存在的部门单独报 ====================

/// A07：库里不存在（或已软删）的部门 id 与「越界」分开报，管理员才知道该补哪一步。
#[tokio::test]
async fn a07_missing_dept_ids_separated_from_exceeded() {
    let a = uuid::Uuid::now_v7();
    let b = uuid::Uuid::now_v7();
    let missing = scope_model::missing_dept_ids(&[a, b, a], &[a]);
    assert_eq!(missing, vec![b], "只有库里没有的才算缺失，实际 {missing:?}");
    assert!(
        scope_model::missing_dept_ids(&[a], &[a]).is_empty(),
        "存在的部门不该被判缺失"
    );
    println!("[A07] 缺失部门与越界分开报（{missing:?}）✓");
}

// ==================== A08 树裁剪不丢节点、不泄露父 ====================

/// A08：自定义范围下「父部门不可见」时，子部门必须**提升为根**：
/// 既不能悄悄丢掉（用户会以为没授权），也不能把父节点补进来（等于泄露一个不可见部门）。
#[tokio::test]
async fn a08_crop_keeps_orphans_as_roots() {
    let parent = uuid::Uuid::now_v7();
    let child = uuid::Uuid::now_v7();
    let grandchild = uuid::Uuid::now_v7();
    let rows = vec![
        dept_row(child, Some(parent), "C", 1),
        dept_row(grandchild, Some(child), "G", 2),
    ];
    let visible: std::collections::BTreeSet<uuid::Uuid> = [child, grandchild].into_iter().collect();

    let cropped = scope_model::crop_to_visible(rows, &visible);
    assert_eq!(cropped.len(), 2, "节点一个都不能少");
    let by_id = |id: uuid::Uuid| cropped.iter().find(|r| r.id == id).expect("节点应存在");
    assert!(by_id(child).parent_id.is_none(), "父不可见 → 提升为根");
    assert_eq!(by_id(grandchild).parent_id, Some(child), "父可见则保持挂载");
    assert!(cropped.iter().all(|r| r.id != parent), "不可见的父节点绝不能被补进结果");

    let tree = scope_model::build_tree(cropped);
    assert_eq!(tree.len(), 1, "提升为根后应只有 1 个根节点");
    assert_eq!(scope_model::count_nodes(&tree), 2, "树内节点总数应仍为 2");
    println!("[A08] 父不可见 → 子提升为根；树节点数与输入一致 ✓");
}

// ==================== A09 树排序与层级 ====================

/// A09：`build_tree` 同级按 `sort_order` 升序（与前端渲染顺序一致）。
#[tokio::test]
async fn a09_tree_sorted_by_sort_order() {
    let root = uuid::Uuid::now_v7();
    let rows = vec![
        dept_row(root, None, "R", 0),
        dept_row(uuid::Uuid::now_v7(), Some(root), "C2", 20),
        dept_row(uuid::Uuid::now_v7(), Some(root), "C1", 10),
    ];
    let tree = scope_model::build_tree(rows);
    assert_eq!(tree.len(), 1);
    let children = &tree[0].children;
    assert_eq!(children.len(), 2, "根下应有 2 个子部门");
    assert!(
        children[0].dept_name.ends_with("C1") && children[1].dept_name.ends_with("C2"),
        "同级必须按 sort_order 升序，实际 {:?}",
        children.iter().map(|c| c.dept_name.clone()).collect::<Vec<_>>()
    );
    println!("[A09] 同级按 sort_order 升序（C1 在 C2 前）✓");
}

// ==================== A10 摘要与文案 ====================

/// A10：`scope_summary` 的取值与 `ScopeKind` 的稳定编码一致（前端按 code 渲染标签）。
#[tokio::test]
async fn a10_scope_summary_codes() {
    for (kind, code, data_scope) in [
        (ScopeKind::All, "ALL", 1_i16),
        (ScopeKind::Dept, "DEPT", 2),
        (ScopeKind::DeptAndChild, "DEPT_AND_CHILD", 3),
        (ScopeKind::SelfOnly, "SELF", 4),
        (ScopeKind::Custom, "CUSTOM", 5),
    ] {
        let summary = scope_model::scope_summary(kind);
        assert_eq!(summary.data_scope, data_scope);
        assert_eq!(summary.code, code, "{kind:?} 的稳定编码应为 {code}");
        assert!(!summary.label.is_empty(), "标签不能为空");
        assert_eq!(kind.as_i16(), data_scope, "as_i16 与摘要必须一致");
        assert_eq!(ScopeKind::from_i16(data_scope).expect("合法值"), kind);
    }
    println!("[A10] 5 种范围的 code / data_scope / label 全部一致 ✓");
}

// ==================== A11 数据范围接线覆盖（防漏接） ====================

/// A11：**每张纳入数据范围管控的表都必须有接线描述**，未接线的表要能一眼查出。
///
/// 「新加一张业务表却忘了接数据范围」是本系统最静默的越权：列表照常返回全量数据。
/// 本用例锁住 `table_coverage()` 与 `uncovered_tables()` 的自洽性。
#[tokio::test]
async fn a11_table_coverage_self_consistent() {
    let coverage = scope_model::table_coverage();
    assert!(!coverage.is_empty(), "覆盖清单不能为空");

    let mut wired = 0usize;
    for item in &coverage {
        assert!(!item.table.is_empty(), "表名不能为空");
        assert!(
            item.wired || !item.status.is_empty(),
            "未接线的表必须给出状态说明：{}",
            item.table
        );
        assert!(
            !item.reason.is_empty(),
            "每张表都要写清楚接线位置/原因（否则无从复核）：{}",
            item.table
        );
        if item.wired {
            wired += 1;
            assert!(
                scope_model::is_wired(&item.table),
                "{} 标称已接线，但 is_wired() 说没有",
                item.table
            );
            assert!(
                scope_model::wiring_of(&item.table).is_some(),
                "{} 标称已接线，但 wiring_of() 查不到",
                item.table
            );
        }
    }
    assert!(wired > 0, "至少要有一张表已接线");

    let uncovered = scope_model::uncovered_tables();
    for table in &uncovered {
        assert!(
            !scope_model::is_wired(table),
            "{table} 同时出现在「未接线」和「已接线」两个口径里"
        );
    }
    let exempted = scope_model::exempted_tables();
    for table in &exempted {
        assert!(
            !scope_model::is_wired(table),
            "{table} 既声明豁免又接了线，口径自相矛盾"
        );
    }
    println!(
        "[A11] 覆盖 {} 张表（已接线 {wired}，未接线 {}，豁免 {}）自洽 ✓",
        coverage.len(),
        uncovered.len(),
        exempted.len()
    );
}

// ==================== A12 核心表的接线事实 ====================

/// A12：人 / 部门 / 单据三类核心表的接线状态必须符合事实（抽样钉死，防止被误改成「未接线」）。
#[tokio::test]
async fn a12_core_tables_are_wired() {
    for table in ["sys_user", "sys_dept"] {
        assert!(scope_model::is_wired(table), "{table} 是数据权限最核心的表，必须已接线");
        let wiring = scope_model::wiring_of(table).expect("应能查到接线描述");
        assert_eq!(wiring.table, table);
        assert!(!wiring.anchor.is_empty(), "{table} 必须给出 SQL 锚点（否则接线是假的）");
    }
    // 部门树不分页（没有 {FILTER} 契约），必须显式声明
    let dept_wiring = scope_model::wiring_of("sys_dept").expect("sys_dept 应已接线");
    assert!(dept_wiring.no_paginate, "部门树不分页，必须标记 no_paginate");
    println!("[A12] sys_user / sys_dept 均已接线且锚点完备 ✓");
}
