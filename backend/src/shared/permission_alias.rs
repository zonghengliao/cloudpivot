//! 权限码兼容别名（**兼容期双码**）：旧码与规范码在「装载权限」时互扩。
//!
//! # 为什么要有这一层
//!
//! 合同域最初按 `ARCH-2 附录 B #7` 的逐字契约种了 `erp:contract:*`，而 ERP 其余
//! **313 个码**统一是 `business:erp:*`（`business:erp:{资源}:{动作}`）。同一套后台
//! 里出现两种前缀，前端判权与运维排障都要记两套规则 —— 见卡 P2-4。
//!
//! 规范码定为 `business:erp:contract:*`（与 `V0141` 起的 ERP 码同族）。切换时最怕
//! 的不是改代码，而是**把已经配好的授权改没了**：
//!
//! | 风险 | 本次做法 |
//! |------|----------|
//! | 已配授权丢失 | 迁移 `V0241` **就地改 `sys_menu.auth_code`**，菜单 `id` 不动 → `sys_role_menu` 按 `menu_id` 挂的授权**一行不丢**（不需要重新勾） |
//! | 部署瞬间在途 JWT 失效 | 令牌里是**登录那一刻的快照**：老令牌只有旧码，路由换成新码后会 403。装载时互扩 ⇒ 老令牌里的旧码自动也认新码，直到自然过期 |
//! | 前端老包（浏览器缓存）判权失灵 | `/me` 下发的权限列表同样互扩（同一处代码），老包按旧码判权照样命中 |
//! | 第三方 / 脚本还在用旧码调接口 | 旧码仍在令牌权限集合里，`has_permission("erp:contract:list")` 与路由层的 `permissions.contains(旧码)` 都放行 |
//!
//! # 为什么不在库里同时保留两套码
//!
//! 双码写进 `sys_menu` 会让「角色授权」树出现两条同义节点：管理员勾一条就以为授了权，
//! 而代码只认另一条；两边都勾又变成两份可漂移的真值源。所以库里**只留规范码**，
//! 兼容只发生在**读路径的装载阶段**（一个函数、一张表，随兼容期结束整体删除）。
//!
//! # 收口位置（唯一）
//!
//! [`expand`] 只在 [`crate::system::auth::model::load_permissions`] 的非超管分支调用一次，
//! 结果进缓存 + 进 JWT + 进 `/me`。因此下面这些都自动双码可用，**不需要**各自再判：
//!
//! - 框架路由闸 `#[nik::permission(permission = "...")]`（`nik-web` 的 `permissions.contains`）；
//! - 行级判定 [`crate::business::shared::state_machine::AuthSnapshot::has_permission`]；
//! - 前端 `userStore.hasPermission` / `v-privilege`（吃 `/me` 的 permissions）。
//!
//! 超管分支不扩（其权限是 `*:*:*` 通配，无需别名）。
//!
//! # 兼容期结束怎么删
//!
//! 1. 删本文件与 `mod.rs` 里的 `pub mod permission_alias;`；
//! 2. 删 `auth::model::load_permissions_uncached` 末尾的 `expand(...)` 调用；
//! 3. 删 [`LEGACY_ALIASES`] 的 **unit 用例**（`tests` 里会断言表与
//!    `contract_handler::PERMISSIONS` 逐条对应，删一半会红 —— 这是故意的）。
//!
//! 判据：线上已无 `erp:contract:*` 的调用方（WAF / 访问日志 0 命中）且所有令牌都过了
//! 最长有效期。

/// 旧码 → 规范码对照表（`(legacy, canonical)`）。
///
/// **一行一个已切换的码**，不允许前缀/通配式映射（那会让「谁的旧码」变成不可审计的推理）。
/// 表与 [`crate::business::erp::contract_handler::PERMISSIONS`] 由单测钉死：
/// 任一侧改了而另一侧没跟 → `contract_aliases_match_handler_permissions` 红。
pub const LEGACY_ALIASES: [(&str, &str); 5] = [
    ("erp:contract:list", "business:erp:contract:list"),
    ("erp:contract:create", "business:erp:contract:create"),
    ("erp:contract:update", "business:erp:contract:update"),
    ("erp:contract:delete", "business:erp:contract:delete"),
    ("erp:contract:status", "business:erp:contract:status"),
];

/// 取某个权限码的全部别名（**双向**：旧码给出规范码，规范码给出旧码）。
///
/// # 参数
///
/// - `code`：令牌里持有的权限码。
///
/// # 返回值
///
/// 与之等价的对端码；不在对照表里则返回空 `Vec`（绝大多数码都走这条）。
pub fn aliases_of(code: &str) -> Vec<&'static str> {
    let mut aliases = Vec::new();
    for (legacy, canonical) in LEGACY_ALIASES {
        if code == legacy {
            aliases.push(canonical);
        } else if code == canonical {
            aliases.push(legacy);
        }
    }
    aliases
}

/// 把权限集合按 [`LEGACY_ALIASES`] 双向补齐（原地追加，去重，幂等）。
///
/// # 参数
///
/// - `codes`：`load_permissions` 查库得到的权限码列表，按需补齐对端码。
///
/// # 行为
///
/// - 顺序：原有码不动，别名按对照表顺序追加在末尾（令牌里码的顺序无人依赖，但**稳定**
///   能让「同一用户的 JWT 字节一致」，便于排障时对比）；
/// - 重复：已在集合里的别名不再追加；重复调用无副作用；
/// - 复杂度：`O(n × 5)`，`n` 是用户权限数（几十到几百），在登录 / 刷新路径上可忽略。
pub fn expand(codes: &mut Vec<String>) {
    let mut appended: Vec<String> = Vec::new();
    for code in codes.iter() {
        for alias in aliases_of(code) {
            let already_held = codes.iter().any(|held| held == alias);
            let already_appended = appended.iter().any(|held| held == alias);
            if !already_held && !already_appended {
                appended.push(alias.to_string());
            }
        }
    }
    codes.extend(appended);
}

// ==================== 单元测试（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::contract_handler;
    use std::collections::BTreeSet;

    /// ① 双向：旧码补齐规范码，规范码补齐旧码；无关码不动。
    #[test]
    fn expand_is_bidirectional() {
        let mut legacy_only = vec![
            "erp:contract:list".to_string(),
            "business:erp:budget:list".to_string(),
        ];
        expand(&mut legacy_only);
        assert!(
            legacy_only.contains(&"business:erp:contract:list".to_string()),
            "旧码应补齐规范码：{legacy_only:?}"
        );
        assert_eq!(legacy_only.len(), 3, "只追加 1 个别名：{legacy_only:?}");
        assert!(
            !legacy_only
                .iter()
                .any(|c| c.starts_with("erp:contract:create")),
            "不能凭前缀猜其它动作: {legacy_only:?}"
        );

        let mut canonical_only = vec!["business:erp:contract:status".to_string()];
        expand(&mut canonical_only);
        assert_eq!(
            canonical_only,
            vec![
                "business:erp:contract:status".to_string(),
                "erp:contract:status".to_string()
            ],
            "规范码应补齐旧码"
        );
    }

    /// ② 幂等 + 不产生重复：连扩两次结果不变。
    #[test]
    fn expand_is_idempotent() {
        let mut codes = vec![
            "erp:contract:list".to_string(),
            "business:erp:contract:list".to_string(),
        ];
        expand(&mut codes);
        let once = codes.clone();
        assert_eq!(once.len(), 2, "两码互为别名，不应再追加：{once:?}");
        expand(&mut codes);
        assert_eq!(codes, once, "第二次 expand 必须无副作用");
    }

    /// ③ 对照表 ↔ handler 常量逐条对应（任一侧改动即红）。
    ///
    /// 这是「双码」的真正守门人：只改 handler 的 5 个常量而不加别名 → 老令牌 403；
    /// 只加别名而 handler 没切 → 别名指向不存在的码，等于没扩。
    #[test]
    fn contract_aliases_match_handler_permissions() {
        let canonical_in_table: BTreeSet<&str> = LEGACY_ALIASES.iter().map(|(_, c)| *c).collect();
        let canonical_in_code: BTreeSet<&str> =
            contract_handler::PERMISSIONS.iter().copied().collect();
        assert_eq!(
            canonical_in_table, canonical_in_code,
            "LEGACY_ALIASES 的规范码必须与 contract_handler::PERMISSIONS 完全相同"
        );
        assert_eq!(
            LEGACY_ALIASES.len(),
            5,
            "合同域 5 个码，一个不能多一个不能少"
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

    /// ④ 全量扩一次：5 个规范码 → 10 个码，且旧码 / 新码各 5 个。
    #[test]
    fn expanding_all_contract_codes_yields_both_families() {
        let mut codes: Vec<String> = contract_handler::PERMISSIONS
            .iter()
            .map(|code| (*code).to_string())
            .collect();
        expand(&mut codes);
        assert_eq!(codes.len(), 10, "5 个规范码 + 5 个别名：{codes:?}");
        assert_eq!(
            codes
                .iter()
                .filter(|c| c.starts_with("erp:contract:"))
                .count(),
            5
        );
        assert_eq!(
            codes
                .iter()
                .filter(|c| c.starts_with("business:erp:contract:"))
                .count(),
            5
        );
    }
}
