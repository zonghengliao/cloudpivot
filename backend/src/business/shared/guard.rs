//! 数据权限 · 缓存失效（`epoch`）与启动自检（架构 §2.4 闸 3、§2.7、§2.8 失效矩阵）。
//!
//! # 三件事
//!
//! 1. [`bump_epoch`]：部门树结构变更（增 / 删 / 改 `parent_id` / 移动）后把
//!    `perm:user_scope_epoch` 递增——**O(1) 全量失效**。子树结果对所有用户都变了，
//!    逐用户 `delete_pattern` 在大基数下是阻塞风险（架构 §2.8）。
//! 2. [`invalidate_user_scope`] / [`invalidate_user_scopes`]：授权类变更（用户换角色、
//!    角色 `data_scope` 变、`sys_role_dept` 变、角色删除/停用、用户换部门）后删该用户
//!    的键。B3 会在这些已有写点上各加一行调用。
//! 3. [`preflight_permission`]：启动自检——登记的 `(table, dept_col, owner_col)` 必须真实
//!    存在，`SUPER_ADMIN` 角色必须存在。查不到就是**部署即失败**，而不是运行期 500
//!    （项目既有取向：宁可不启动也不带病上线，见 `bootstrap.rs` 的 prod 缺口 `exit(1)`）。
//!
//! 另外提供 [`ensure_scope_grant_allowed`]：非超管修改数据范围时的「范围上限」校验
//! （架构 §2.7 末条，S14）——防止一个持 `system:data-scope:update` 的管理员把自己的
//! 可见范围扩到全公司（与批次 A 的 delegation cap 同源的提权路径）。

use std::collections::{BTreeMap, BTreeSet};

use nik::prelude::*;
use nik::uuid::Uuid;

use super::principal::{CACHE_KEY_USER_SCOPE_EPOCH, ScopedPrincipal, cache_key, current_epoch};
use super::scope::{SCOPED_TABLES, ScopeKind};
use crate::shared::constants;
use crate::shared::error_code;

// ==================== 失效 ====================

/// 部门树结构变更 → `epoch` 递增（**全量失效**），返回新 epoch。
///
/// # 何时调用（架构 §2.8 失效矩阵最后一行）
///
/// 部门的 `create` / `update`（含 `parent_id` 变更）/ `delete` / `move`——
/// `system/dept/handler.rs` 的 4 个写点，B3 接一条即可（列进底座报批 §5.3-#8）。
///
/// # 返回值
///
/// - `Some(epoch)`：`INCR` 成功，新版本号；
/// - `None`：缓存不可用或 `INCR` 失败。**不是错误**：没有缓存就没有脏读，
///   最坏后果退化为「每次都查库」，不影响正确性（只告警）。
pub async fn bump_epoch() -> Option<u64> {
    let Some(store) = try_cache() else {
        tracing::warn!(
            key = CACHE_KEY_USER_SCOPE_EPOCH,
            "缓存不可用，无法递增数据范围 epoch（本次不做全量失效，靠 TTL 兜底）"
        );
        return None;
    };
    match Cache::incr(store, CACHE_KEY_USER_SCOPE_EPOCH, 1).await {
        Ok(epoch) => {
            tracing::info!(epoch, "部门树变更 → 数据范围缓存 epoch 递增（全量失效）");
            #[allow(clippy::cast_sign_loss)]
            Some(epoch.max(0) as u64)
        }
        Err(error) => {
            tracing::warn!(
                key = CACHE_KEY_USER_SCOPE_EPOCH,
                error = %error,
                "递增数据范围 epoch 失败（靠 TTL 兜底）"
            );
            None
        }
    }
}

/// 删除**一个**用户的数据范围缓存（授权 / 部门归属变更后调用）。
///
/// # 参数
///
/// - `user_id`：用户 ID。
///
/// # 说明
///
/// 键里带 `epoch`，所以删除前要读一次当前 epoch：读不到（缓存不可用）就直接跳过——
/// 没有缓存就没有脏读。
pub async fn invalidate_user_scope(user_id: &Uuid) {
    let Some(store) = try_cache() else {
        return;
    };
    let key = cache_key(current_epoch().await, user_id);
    if let Err(error) = Cache::del(store, &key).await {
        tracing::warn!(key, error = %error, "失效用户数据范围缓存失败");
    }
}

/// 批量删除多个用户的数据范围缓存（角色授权变更 → 反查持有者后批量调用）。
///
/// # 参数
///
/// - `user_ids`：用户 ID 列表（`invalidate_role_holders` 的 `SELECT DISTINCT user_id` 结果）。
///
/// # 说明
///
/// 逐个 `DEL` 而不是 `delete_pattern`：键里带用户 ID，模式匹配会顺带删掉别的用户。
pub async fn invalidate_user_scopes(user_ids: &[Uuid]) {
    for user_id in user_ids {
        invalidate_user_scope(user_id).await;
    }
}

// ==================== 范围上限（S14） ====================

/// 校验「操作者能否把数据范围配成这个部门集合」（非超管的自我扩权保护）。
///
/// # 参数
///
/// - `operator`：操作者的数据范围（`resolve` 得到）；
/// - `new_dept_ids`：**待配置**的部门集合。`SELF` / `ALL` 这类不带部门集合的范围传空切片。
///
/// # 返回值
///
/// 允许 → `Ok(())`。
///
/// # 异常情况
///
/// 任一部门不在操作者的可见集合内 → `403 + DATA_SCOPE_EXCEED_LIMIT(61002)`，
/// 错误消息里列出越界部门 ID（自检 / 排障用，不含数据内容）。
///
/// # 规则
///
/// | 操作者 | 结果 |
/// |---|---|
/// | 超管 | 恒 `Ok`（架构 §2.7：超管不受配置上限约束） |
/// | 范围 = `ALL` 的非超管 | 恒 `Ok`（他的可见集合本来就是全部） |
/// | 其他 | 新部门集合必须 ⊆ 操作者可见部门集合 |
/// | 新部门集合为空 | `Ok`（`SELF` / 未带自定义部门，没有扩权面） |
///
/// 注意最后一行**不是**漏洞：把范围从 `DEPT` 改成 `SELF` 是**收紧**；改成 `ALL` 不带部门
/// 集合时由 B3 的 handler 用 [`ScopeKind::All`] 单独判（本函数只看集合包含关系）。
pub fn ensure_scope_grant_allowed(
    operator: &ScopedPrincipal,
    new_dept_ids: &[Uuid],
) -> std::result::Result<(), ApiError> {
    if operator.is_super() || operator.kind() == ScopeKind::All {
        return Ok(());
    }
    if new_dept_ids.is_empty() {
        return Ok(());
    }
    let visible: BTreeSet<Uuid> = operator.dept_ids().iter().copied().collect();
    let exceeded: Vec<String> = new_dept_ids
        .iter()
        .filter(|dept_id| !visible.contains(dept_id))
        .map(|dept_id| dept_id.to_string())
        .collect();
    if exceeded.is_empty() {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        error_code::DATA_SCOPE_EXCEED_LIMIT,
        format!("新数据范围超出你自身的可见部门范围，越界部门：{}", exceeded.join(", ")),
    ))
}

// ==================== 启动自检（闸 3） ====================

/// 启动自检结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    /// 核对了多少张登记表。
    pub checked_tables: usize,
    /// 缺失的列 / 表（形如 `sys_user.dept_id`；空 = 全部存在）。
    pub missing: Vec<String>,
    /// `SUPER_ADMIN` 角色是否存在（启用 + 未软删）。
    pub super_admin_ok: bool,
}

impl PreflightReport {
    /// 是否健康（无缺失列 且 超管角色存在）。
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty() && self.super_admin_ok
    }

    /// 一行摘要（启动日志用）。
    pub fn summary(&self) -> String {
        if self.is_ok() {
            return format!(
                "数据权限自检通过：{} 张登记表的部门列/归属列均存在，SUPER_ADMIN 角色存在",
                self.checked_tables
            );
        }
        let mut text = format!(
            "数据权限自检失败：登记表 {} 张，缺失项 {} 个",
            self.checked_tables,
            self.missing.len()
        );
        if !self.missing.is_empty() {
            text.push_str(&format!("（{}）", self.missing.join(", ")));
        }
        if !self.super_admin_ok {
            text.push_str("；SUPER_ADMIN 角色不存在或已停用（数据权限短路逻辑会失效）");
        }
        text
    }
}

/// 启动自检：登记的 `(table, dept_col, owner_col)` 必须真实存在 + `SUPER_ADMIN` 必须存在。
///
/// # 为什么必须启动期查（架构 §2.4 闸 3）
///
/// 表被改名 / 删列后，注入点会在**运行期**抛 `column does not exist`——那时是 500 与用户
/// 报障。启动期查 = 部署即失败。超管角色同理：短路逻辑失效等于超管被自己锁在外面。
///
/// # 返回值
///
/// [`PreflightReport`]（不健康的形态也返回 `Ok`，由调用方决定是否 `exit(1)`——
/// 与 `bootstrap` 现有的「缺角色记 error」口径一致，B3 把它升级成 prod 下 `exit(1)`）。
///
/// # 异常情况
///
/// 查库本身失败（连不上 / 权限不足）→ 原样上抛 [`DbError`]。
pub async fn preflight_permission() -> DbResult<PreflightReport> {
    let tables: Vec<&str> = SCOPED_TABLES.iter().map(|target| target.table).collect();
    let columns = load_columns(&tables).await?;
    let mut missing = collect_missing_tables(SCOPED_TABLES, &columns);
    missing.sort();

    let super_admin_ok = super_admin_role_exists().await?;

    let report = PreflightReport {
        checked_tables: SCOPED_TABLES.len(),
        missing,
        super_admin_ok,
    };
    if report.is_ok() {
        tracing::info!("{}", report.summary());
    } else {
        tracing::error!("{}", report.summary());
    }
    Ok(report)
}

/// 读 `information_schema.columns`：返回 `表名 → 列名集合`。
async fn load_columns(tables: &[&str]) -> DbResult<BTreeMap<String, BTreeSet<String>>> {
    if tables.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = tables.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT table_name, column_name FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name IN ({placeholders})"
    );
    let params: Vec<Value> = tables.iter().map(|table| Value::from(*table)).collect();
    let rows = db().find_with_params(&sql, params).await?;
    let mut columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        let table: String = row.get_typed("table_name")?;
        let column: String = row.get_typed("column_name")?;
        columns.entry(table).or_default().insert(column);
    }
    Ok(columns)
}

/// 纯函数：按「表 → 列集合」算出缺失项（单测的断言点，不连库）。
fn collect_missing_tables(
    targets: &[super::scope::ScopeTarget],
    columns: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<String> {
    let mut missing = Vec::new();
    for target in targets {
        let Some(existing) = columns.get(target.table) else {
            missing.push(format!("{}(表不存在)", target.table));
            continue;
        };
        for column in [target.dept_col, target.owner_col].into_iter().flatten() {
            if !existing.contains(column) {
                missing.push(format!("{}.{}", target.table, column));
            }
        }
    }
    missing
}

/// `SUPER_ADMIN` 角色是否存在（启用 + 未软删）。
async fn super_admin_role_exists() -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM sys_role \
             WHERE role_code = ? AND status = 1 AND deleted_at IS NULL",
            vec![Value::from(constants::SUPER_ADMIN_ROLE_CODE)],
        )
        .await?;
    Ok(count > 0)
}

// ==================== 单元测试（S14 + 自检的纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::shared::scope::ScopeTarget;
    use std::collections::{BTreeMap, BTreeSet};

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn principal(kind: ScopeKind, dept_ids: Vec<Uuid>, self_only: bool, is_super: bool) -> ScopedPrincipal {
        ScopedPrincipal::for_test(
            uid(0x9001),
            dept_ids.first().copied(),
            is_super,
            kind,
            dept_ids,
            self_only,
        )
    }

    /// S14：新部门集合 ⊆ 操作者可见集合 → `Ok`；任一越界 → `DATA_SCOPE_EXCEED_LIMIT(61002)`。
    #[test]
    fn s14_ensure_scope_grant_allowed() {
        let visible = vec![uid(0x9101), uid(0x9102)];
        let operator = principal(ScopeKind::DeptAndChild, visible.clone(), false, false);

        // 子集 → 允许（含「空集合」= SELF/收紧）
        assert!(ensure_scope_grant_allowed(&operator, &[visible[0]]).is_ok());
        assert!(ensure_scope_grant_allowed(&operator, &visible).is_ok());
        assert!(ensure_scope_grant_allowed(&operator, &[]).is_ok());

        // 越界一个 → 403 / 61002，且消息里点名越界部门
        let intruder = uid(0x9199);
        let error = ensure_scope_grant_allowed(&operator, &[visible[0], intruder]).expect_err("越界必须拒绝");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);
        assert!(
            error.msg.contains(&intruder.to_string()),
            "消息应点名越界部门：{}",
            error.msg
        );

        // 超管 / 范围=ALL 的非超管 不受限
        let super_admin = principal(ScopeKind::SelfOnly, vec![uid(0x9103)], true, true);
        assert!(ensure_scope_grant_allowed(&super_admin, &[intruder]).is_ok());
        let all_scope = principal(ScopeKind::All, vec![], false, false);
        assert!(ensure_scope_grant_allowed(&all_scope, &[intruder]).is_ok());

        // 只有 SELF 范围的操作者：任何非空部门集合都越界（最需要堵住的形态）
        let self_only = principal(ScopeKind::SelfOnly, vec![], true, false);
        let error = ensure_scope_grant_allowed(&self_only, &[intruder]).expect_err("必须拒绝");
        assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);

        println!(
            "[S14] 子集→Ok；越界→403/61002（点名 {}）；超管与 ALL 不受限 ✓",
            intruder
        );
    }

    /// 附加：自检的缺失项计算（纯函数，不连库）。
    #[test]
    fn preflight_missing_detection() {
        let mut columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        // sys_user 齐全
        columns.insert(
            "sys_user".to_string(),
            ["id", "dept_id"].iter().map(|c| c.to_string()).collect(),
        );
        // sys_dept 缺 id（模拟被改名）
        columns.insert(
            "sys_dept".to_string(),
            ["dept_name"].iter().map(|c| c.to_string()).collect(),
        );
        // 其余表故意不提供（模拟表被删）
        let missing = collect_missing_tables(SCOPED_TABLES, &columns);
        assert!(
            missing.contains(&"sys_dept.id".to_string()),
            "缺列必须被发现：{missing:?}"
        );
        assert!(
            missing.iter().any(|item| item.starts_with("operation_logs(表不存在)")),
            "缺表必须被发现：{missing:?}"
        );
        assert!(
            !missing.iter().any(|item| item.starts_with("sys_user")),
            "齐备的表不应报缺失：{missing:?}"
        );

        // 全部存在 → 无缺失
        let mut complete: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for target in SCOPED_TABLES {
            let entry = complete.entry(target.table.to_string()).or_default();
            for column in [target.dept_col, target.owner_col].into_iter().flatten() {
                entry.insert(column.to_string());
            }
        }
        assert!(collect_missing_tables(SCOPED_TABLES, &complete).is_empty());

        let report = PreflightReport {
            checked_tables: SCOPED_TABLES.len(),
            missing: vec!["sys_user.dept_id".to_string()],
            super_admin_ok: false,
        };
        assert!(!report.is_ok());
        assert!(report.summary().contains("sys_user.dept_id"));
        assert!(report.summary().contains("SUPER_ADMIN"));
        println!("[自检] 缺列/缺表被发现；齐全时不报缺；报告摘要 = {}", report.summary());
    }

    /// 附加：`invalidate_user_scope` 在缓存不可用时是 no-op（不 panic）。
    #[tokio::test]
    async fn invalidate_without_cache_is_noop() {
        invalidate_user_scope(&uid(0x9201)).await;
        invalidate_user_scopes(&[uid(0x9202), uid(0x9203)]).await;
        println!("[失效] 缓存不可用 → invalidate_* 静默跳过 ✓");
    }

    /// 附加：登记表清单与自检函数用的是同一份清单（避免两处清单漂移）。
    #[test]
    fn preflight_uses_the_registry() {
        // 10 张（B2 基线）+ `password_change_logs`（B5 按 §2.5 的 ✅(P1) 行补登记）
        // + `biz_commission_settlement`（SO-36：佣金结算单按销售员注入 SELF）= 12 张
        assert_eq!(
            SCOPED_TABLES.len(),
            12,
            "架构 §2.5 登记表 10 张 + B5 补登记的 password_change_logs + SO-36 的佣金结算单"
        );
        assert!(SCOPED_TABLES.contains(&ScopeTarget::SYS_USER));
        assert!(SCOPED_TABLES.contains(&ScopeTarget::SYS_FILE));
        assert!(SCOPED_TABLES.contains(&ScopeTarget::PASSWORD_CHANGE_LOGS));
        assert!(SCOPED_TABLES.contains(&ScopeTarget::BIZ_COMMISSION_SETTLEMENT));
        println!("[清单] preflight 直接遍历 SCOPED_TABLES（{} 张）✓", SCOPED_TABLES.len());
    }
}
