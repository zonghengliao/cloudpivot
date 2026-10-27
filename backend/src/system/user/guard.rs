//! 用户管理写操作的**权限上限（delegation cap）**守卫。
//!
//! # 为什么需要
//!
//! 框架的 `#[alun::permission]` 只回答「操作者有没有这个权限码」，不回答
//! 「操作者有没有资格动这个对象 / 授予这个角色」。两者在用户管理里被压成同一级
//! 权限，于是存在三条提权路径（安全审计 P1）：
//!
//! 1. **建号即提权**：`POST /system/user` 只要 `system:user:create` 就能挂
//!    `SUPER_ADMIN`（原 `parse_and_check_roles` 仅校验角色存在性）；
//! 2. **改超管密码即接管**：`PATCH /system/user/{id}/password` 对内置超管无拦截；
//! 3. **自我提权**：`PUT /system/user/{id}/roles` 只拦「目标用户名是 admin」，
//!    不拦「操作者给自己授予 SUPER_ADMIN」。
//!
//! # SUPER_ADMIN 的判定依据（按实际 schema，非猜测）
//!
//! - `sys_role`（`migrations/system/V0002__create_sys_role.up.sql`）中标识超管的
//!   语义列是 **`role_code`**；种子 `migrations/seed/S__role_data.sql` 写入
//!   `role_code = 'SUPER_ADMIN'`，对应常量 [`constants::SUPER_ADMIN_ROLE_CODE`]。
//! - 该表**没有** `is_super` 之类布尔列；`id` 是 `gen_random_uuid()` 随机值，
//!   不能拿固定 ID 判定。
//! - 与既有口径一致：`system/auth/model.rs::is_super_admin` 正是按角色编码短路
//!   全部权限校验，`shared/constants.rs` 的注释亦明确「模型侧以
//!   `role_code == SUPER_ADMIN_ROLE_CODE` 短路全部权限校验」。
//! - 内置超管账号的用户名是 [`constants::ADMIN_USERNAME`]（`admin`）；除角色判定外
//!   再叠加用户名判定，避免角色绑定被误删后内置账号失去保护。
//!
//! # 三条规则
//!
//! | # | 规则 | 错误 |
//! |---|------|------|
//! | 1 | 非超管不得对「内置超管账号 / 持有 SUPER_ADMIN 角色的用户」做任何写操作 | 403 `PERMISSION_DENIED` |
//! | 2 | 非超管不得在 `role_ids` 中携带 SUPER_ADMIN 角色 | 403 `PERMISSION_DENIED` |
//! | 3 | 非超管不得给自己新增原本未持有的角色（自我提权） | 403 `PERMISSION_DENIED` |
//!
//! 超管自身不受上限约束（超管本就最高），因此「超管建超管号 / 改超管密码 /
//! 调整任意用户角色」全部保持可用。
//!
//! # 用法
//!
//! 各写接口在完成**原有**的角色存在性校验之后调用
//! [`ensure_role_grant_allowed`]：改角色 / 建号传 `role_ids`，
//! 改密 / 改资料 / 启停 / 删除传空切片 + `Some(target_id)`。

use alun::prelude::*;

use crate::shared::{constants, error_code};
use crate::system::user::model::{self, SysUserModel, SysUserRoleModel};
use crate::utils;

/// 非超管操作超管账号时的提示。
const MSG_TARGET_SUPER_ADMIN: &str = "无权操作超级管理员账号";
/// 非超管授予超管角色时的提示。
const MSG_GRANT_SUPER_ADMIN: &str = "无权授予超级管理员角色";
/// 非超管给自己新增未持有角色时的提示。
const MSG_SELF_ESCALATION: &str = "无权为自己新增未持有的角色";
/// 操作者账号本身不存在时的提示（令牌有效但用户已被删除）。
const MSG_OPERATOR_MISSING: &str = "操作者账号不存在，拒绝操作";

/// 统一的「无权限」错误：403 + `PERMISSION_DENIED`（复用 `shared/error_code.rs`）。
fn forbidden(message: &str) -> ApiError {
    ApiError::new(403, error_code::PERMISSION_DENIED, message)
}

/// 权限上限裁决所需的输入快照。
///
/// 拆成纯数据 + 纯函数，既让 DB 查询与判定解耦，也让三条规则的每个分支都能被
/// 单测直接覆盖（无需连库）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GrantCheckInput {
    /// 操作者账号在库中存在（令牌有效但账号已被删 → false）。
    pub operator_exists: bool,
    /// 操作者当前持有 SUPER_ADMIN 角色。
    pub operator_is_super_admin: bool,
    /// 本次操作是否显式指定了目标用户（建号时为 false）。
    pub has_target_user: bool,
    /// 目标用户在库中存在。
    pub target_user_exists: bool,
    /// 目标用户是超管（用户名 = admin 或持有 SUPER_ADMIN 角色）。
    pub target_user_is_super_admin: bool,
    /// 目标用户就是操作者本人。
    pub target_user_is_operator: bool,
    /// 请求的角色集合里含 SUPER_ADMIN 角色。
    pub grants_super_admin_role: bool,
    /// 请求的角色集合里有操作者当前不具备的角色。
    pub adds_role_operator_lacks: bool,
}

/// 依据三条规则做最终裁决（无 IO，纯函数）。
///
/// 判定顺序（先保护对象、再保护角色、最后保护本人）：
/// 1. 操作者账号不存在 → 403（令牌有效但账号已删，直接拒绝）；
/// 2. 指定了目标用户但目标不存在 → 404 `USER_NOT_FOUND`（与各接口原语义一致）；
/// 3. 操作者是超管 → 放行（不受上限约束）；
/// 4. 目标是超管 → 403（规则 1）；
/// 5. 请求含超管角色 → 403（规则 2）；
/// 6. 给自己新增未持有角色 → 403（规则 3）。
pub(crate) fn evaluate_grant(input: GrantCheckInput) -> std::result::Result<(), ApiError> {
    if !input.operator_exists {
        return Err(forbidden(MSG_OPERATOR_MISSING));
    }
    if input.has_target_user && !input.target_user_exists {
        return Err(ApiError::new(404, error_code::USER_NOT_FOUND, "用户不存在"));
    }
    if input.operator_is_super_admin {
        return Ok(());
    }
    if input.target_user_is_super_admin {
        return Err(forbidden(MSG_TARGET_SUPER_ADMIN));
    }
    if input.grants_super_admin_role {
        return Err(forbidden(MSG_GRANT_SUPER_ADMIN));
    }
    if input.target_user_is_operator && input.adds_role_operator_lacks {
        return Err(forbidden(MSG_SELF_ESCALATION));
    }
    Ok(())
}

/// 请求的角色集合是否含超管角色。
///
/// `super_admin_role_id` 为 `None` 表示库中不存在 `SUPER_ADMIN` 角色（未初始化），
/// 此时不存在「授予超管」的可能，返回 false。
pub(crate) fn grants_super_admin_role(
    role_ids: &[uuid::Uuid],
    super_admin_role_id: Option<uuid::Uuid>,
) -> bool {
    match super_admin_role_id {
        Some(id) => role_ids.contains(&id),
        None => false,
    }
}

/// 请求的角色里是否有操作者当前不具备的角色（自我提权判定）。
///
/// `requested` 为空（改密 / 改资料 / 删除等场景）时恒为 false。
pub(crate) fn adds_role_operator_lacks(held: &[uuid::Uuid], requested: &[uuid::Uuid]) -> bool {
    requested.iter().any(|id| !held.contains(id))
}

/// 校验操作者是否有资格授予 `role_ids` / 操作 `target_user_id`（权限上限检查）。
///
/// # 参数
///
/// - `claims`：认证中间件注入的令牌声明（`sub` 为操作者用户 ID）。
/// - `role_ids`：本次请求要授予的角色 ID（已通过存在性校验；改密 / 改资料 /
///   启停 / 删除场景传空切片）。
/// - `target_user_id`：被操作的用户；建号场景传 `None`。
///
/// # 返回值
///
/// 通过上限检查返回 `Ok(())`。
///
/// # 异常情况
///
/// - 400：令牌 `sub` 不是合法 UUID；
/// - 403：命中三条规则之一（`PERMISSION_DENIED`）；
/// - 404：指定了目标用户但用户不存在（`USER_NOT_FOUND`）；
/// - 500：数据库异常。
pub(crate) async fn ensure_role_grant_allowed(
    claims: &TokenClaims,
    role_ids: &[uuid::Uuid],
    target_user_id: Option<uuid::Uuid>,
) -> std::result::Result<(), ApiError> {
    let operator_id = utils::require_uuid(&claims.sub, "sub")?;

    let operator = SysUserModel::find_or_none(db(), &operator_id)
        .await
        .map_err(|e| utils::db_error(e, "查询操作者账号"))?;
    if operator.is_none() {
        return evaluate_grant(GrantCheckInput {
            operator_exists: false,
            ..Default::default()
        });
    }

    // 操作者是否为超管：以**库里当前的角色绑定**为准（令牌里的权限是签发时快照，
    // 不作为授权依据，避免降权后旧令牌仍可授予超管）。
    let operator_is_super_admin = model::user_holds_super_admin_role(&operator_id)
        .await
        .map_err(|e| utils::db_error(e, "查询操作者角色"))?;
    if operator_is_super_admin {
        // 超管不受上限约束；目标用户是否存在由各接口自身的 load 保证（404 语义不变）
        return Ok(());
    }

    let super_admin_role_id = model::find_super_admin_role_id()
        .await
        .map_err(|e| utils::db_error(e, "查询超级管理员角色"))?;

    let mut input = GrantCheckInput {
        operator_exists: true,
        operator_is_super_admin: false,
        grants_super_admin_role: grants_super_admin_role(role_ids, super_admin_role_id),
        ..Default::default()
    };

    if let Some(target_id) = target_user_id {
        input.has_target_user = true;
        let target = SysUserModel::find_or_none(db(), &target_id)
            .await
            .map_err(|e| utils::db_error(e, "查询目标用户"))?;
        match target {
            None => input.target_user_exists = false,
            Some(user) => {
                input.target_user_exists = true;
                // 内置超管（用户名 admin）即使角色绑定被误删也一律保护
                input.target_user_is_super_admin = user.username == constants::ADMIN_USERNAME
                    || model::user_holds_super_admin_role(&target_id)
                        .await
                        .map_err(|e| utils::db_error(e, "查询目标用户角色"))?;
                input.target_user_is_operator = target_id == operator_id;
                if input.target_user_is_operator {
                    let held = SysUserRoleModel::find_by(db(), "user_id", (&target_id,))
                        .await
                        .map_err(|e| utils::db_error(e, "查询操作者角色绑定"))?
                        .into_iter()
                        .map(|binding| binding.role_id)
                        .collect::<Vec<_>>();
                    input.adds_role_operator_lacks = adds_role_operator_lacks(&held, role_ids);
                }
            }
        }
    }

    evaluate_grant(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 所有危险标记为 false 的基线：非超管、无目标用户、不授予超管。
    fn baseline() -> GrantCheckInput {
        GrantCheckInput {
            operator_exists: true,
            ..Default::default()
        }
    }

    /// 断言为 403 + `PERMISSION_DENIED` 且消息匹配。
    fn assert_forbidden(result: std::result::Result<(), ApiError>, expected_msg: &str) {
        match result {
            Ok(()) => panic!("应被拒绝，但返回了 Ok"),
            Err(err) => {
                assert_eq!(err.status, 403, "HTTP 状态应为 403，实际 {}", err.status);
                assert_eq!(
                    err.code,
                    error_code::PERMISSION_DENIED,
                    "业务码应为 PERMISSION_DENIED({})，实际 {}",
                    error_code::PERMISSION_DENIED,
                    err.code
                );
                assert_eq!(err.msg, expected_msg);
            }
        }
    }

    // ── 用例 1：非超管 + 授予 SUPER_ADMIN 角色 → 拒绝（路径 1 建号即提权）──
    #[test]
    fn non_super_admin_cannot_grant_super_admin_role() {
        let input = GrantCheckInput {
            grants_super_admin_role: true,
            ..baseline()
        };
        assert_forbidden(evaluate_grant(input), MSG_GRANT_SUPER_ADMIN);
    }

    // ── 用例 2：非超管 + 修改超管密码 → 拒绝（路径 2 改密接管）──
    #[test]
    fn non_super_admin_cannot_write_super_admin_account() {
        // 改密接口不带角色，只有目标用户
        let input = GrantCheckInput {
            has_target_user: true,
            target_user_exists: true,
            target_user_is_super_admin: true,
            ..baseline()
        };
        assert_forbidden(evaluate_grant(input), MSG_TARGET_SUPER_ADMIN);
    }

    // ── 用例 3：非超管 + 给自己加更高角色 → 拒绝（路径 3 自我提权）──
    #[test]
    fn non_super_admin_cannot_escalate_self() {
        let input = GrantCheckInput {
            has_target_user: true,
            target_user_exists: true,
            target_user_is_operator: true,
            adds_role_operator_lacks: true,
            ..baseline()
        };
        assert_forbidden(evaluate_grant(input), MSG_SELF_ESCALATION);
    }

    // ── 用例 4：超管 + 授予 SUPER_ADMIN / 改超管账号 → 允许（不误伤超管）──
    #[test]
    fn super_admin_is_not_restricted() {
        let input = GrantCheckInput {
            operator_is_super_admin: true,
            has_target_user: true,
            target_user_exists: true,
            target_user_is_super_admin: true,
            target_user_is_operator: true,
            grants_super_admin_role: true,
            adds_role_operator_lacks: true,
            ..baseline()
        };
        assert!(
            evaluate_grant(input).is_ok(),
            "超管必须仍能建超管号 / 改超管密码"
        );
    }

    // ── 用例 5：非超管 + 授予普通角色 → 允许（不误伤正常业务）──
    #[test]
    fn non_super_admin_can_grant_normal_role() {
        let input = GrantCheckInput {
            has_target_user: true,
            target_user_exists: true,
            ..baseline()
        };
        assert!(evaluate_grant(input).is_ok(), "非超管授予普通角色不得被拦");
        // 建号场景（无目标用户）同样放行
        assert!(
            evaluate_grant(baseline()).is_ok(),
            "非超管建普通账号不得被拦"
        );
    }

    // ── 用例 6a：边界——role_ids 为空 ──
    #[test]
    fn empty_role_ids_never_grants_super_admin() {
        let super_role = uuid::Uuid::now_v7();
        assert!(
            !grants_super_admin_role(&[], Some(super_role)),
            "空角色集合不可能授予超管"
        );
        let input = GrantCheckInput {
            has_target_user: true,
            target_user_exists: true,
            grants_super_admin_role: grants_super_admin_role(&[], Some(super_role)),
            ..baseline()
        };
        assert!(
            evaluate_grant(input).is_ok(),
            "清空角色（分配给普通用户）应放行"
        );
    }

    // ── 用例 6b：边界——目标用户不存在 ──
    #[test]
    fn missing_target_user_returns_not_found() {
        let input = GrantCheckInput {
            has_target_user: true,
            target_user_exists: false,
            target_user_is_super_admin: true, // 即使其余标记异常，也先报 404
            ..baseline()
        };
        match evaluate_grant(input) {
            Ok(()) => panic!("目标用户不存在应报 404"),
            Err(err) => {
                assert_eq!(err.status, 404);
                assert_eq!(err.code, error_code::USER_NOT_FOUND);
            }
        }
    }

    // ── 用例 6c：边界——操作者不存在 ──
    #[test]
    fn missing_operator_is_rejected() {
        let input = GrantCheckInput {
            operator_exists: false,
            operator_is_super_admin: true, // 令牌自称超管也不算数
            ..Default::default()
        };
        assert_forbidden(evaluate_grant(input), MSG_OPERATOR_MISSING);
    }

    // ── 附加：自我提权判定的集合比较逻辑 ──
    #[test]
    fn adds_role_operator_lacks_compares_sets() {
        let a = uuid::Uuid::now_v7();
        let b = uuid::Uuid::now_v7();
        let c = uuid::Uuid::now_v7();
        let held = vec![a, b];

        assert!(
            !adds_role_operator_lacks(&held, &[a, b]),
            "原样保留不得判为提权"
        );
        assert!(!adds_role_operator_lacks(&held, &[b]), "移除角色不算提权");
        assert!(!adds_role_operator_lacks(&held, &[]), "清空角色不算提权");
        assert!(
            adds_role_operator_lacks(&held, &[a, c]),
            "新增未持有角色应判为提权"
        );
    }

    // ── 附加：超管角色判定（含库中未初始化 SUPER_ADMIN 的兜底）──
    #[test]
    fn grants_super_admin_role_matches_role_id() {
        let super_role = uuid::Uuid::now_v7();
        let other = uuid::Uuid::now_v7();
        assert!(grants_super_admin_role(
            &[other, super_role],
            Some(super_role)
        ));
        assert!(!grants_super_admin_role(&[other], Some(super_role)));
        assert!(
            !grants_super_admin_role(&[other], None),
            "库中无超管角色时不得误判"
        );
    }
}
