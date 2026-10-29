//! AI 执行者（`sys_user.user_type = 'ROBOT'`）的硬性行为约束。
//!
//! 依据：`docs/批次C+-工作任务管理-核心设计.md` §六（AI 作为执行者）、
//! `docs/云枢设计修正补丁-20261029.md` **修正 1**（AI 用户不变式）与
//! **修正 11**（AI 分派按角色授予）。
//!
//! # 一张表说清「硬拒」与「按权限码」
//!
//! | 操作 | 判定方式 | 错误码 | 本模块 |
//! |------|----------|--------|--------|
//! | 网页登录 | **硬拒**（不看密码、不看角色） | `63201` | [`login_forbidden`] |
//! | 删除任务 | **硬拒**（超管角色也救不回来） | `63202` | [`task_delete_forbidden`] |
//! | 改数据权限 | **硬拒**（有 `scope:write` 角色也拒） | `63203` | [`data_scope_forbidden`] |
//! | 管 AI 用户 / 改 profile | **硬拒** | `63204` | [`aiagent_admin_forbidden`] |
//! | 分派 / 转派 / 协作人 | **只查权限码** | `63002` | ⛔ 不在这里判 |
//! | 改任务状态 / 写评论 / 填工时 | 只查权限码 | — | ⛔ 不在这里判 |
//!
//! 最后三行是本模块**故意不提供**的函数。修正 11 把「AI 不能分派」改成
//! 「默认不能、可按角色授予」，实现上就是「分派只查 `business:task:assign`，
//! 谁拿到码谁能派」；再补一个按 `user_type` 硬拒的函数，这条授予路径立刻焊死。
//! 所以本模块只有**永久禁止**的四件事 —— 加第五个之前先问：这件事被授了角色
//! 也不该允许吗？答案是「不该」才配进这里。
//!
//! # 为什么把判定收在 `shared/`
//!
//! 四个判定分散在 `system/auth` / `business/task` / `business/permission` /
//! `business/aiagent` 四个模块，各自写一遍 `user_type == "ROBOT"` 的结果必然是
//! 「有的地方漏了、有的地方文案不一样、有的地方状态码写成 400」。
//! 收成纯函数后：① 四个 HTTP 入口只调一次，漏不掉；② 单测不需要数据库
//! （[`is_robot`] 的入参是 `&str`，不是 `SysUserModel`）；③ 文案只有一份。
//!
//! # 账号类型从哪来
//!
//! `HUMAN` / `ROBOT` 是 `sys_user.user_type` 的库内值（`V0102` 加列，
//! 默认 `HUMAN`），**不在 [`SysUserModel`] 上**（见
//! `system/user/model.rs::UserWithDept::user_type` 的说明：给模型加字段要改所有
//! 构造点）。调用方统一用 `system::user::model::find_user_type(&id)` 单查一列，
//! 再交给本模块判定 —— 本模块不碰数据库，保证判定可单测。

use alun::prelude::*;

use crate::shared::error_code;

/// 人类账号（`sys_user.user_type` 的库内值）。
pub const USER_TYPE_HUMAN: &str = "HUMAN";

/// AI 账号（`sys_user.user_type` 的库内值）。
///
/// 不变式（修正 1）：`user_type = 'ROBOT'` ⇔ `sys_ai_agent` 有该 `user_id` 的记录。
/// 写侧由 `business/aiagent` 的事务保证，读侧由 `shared/startup_guard` 每次启动自检。
pub const USER_TYPE_ROBOT: &str = "ROBOT";

/// 是不是 AI 账号。
///
/// 判定**严格等于** `"ROBOT"`（区分大小写）：库内取值受 `V0102` 的 CHECK 约束，
/// 只可能是 `HUMAN` / `ROBOT`。这里刻意不 `eq_ignore_ascii_case` —— 一旦有人
/// 往库里塞了 `robot`（历史脏数据 / 手工 SQL），宁可它被当成 HUMAN 并在启动
/// 自检里报出来，也不要在运行期「宽容」成一个绕过不变式的洞。
///
/// # 参数
///
/// - `user_type`：`sys_user.user_type` 的读值；`None` / 空串（老库未跑 `V0102`）
///   按人类处理 —— 与 `system/user/model.rs` 列表口径一致。
pub fn is_robot(user_type: Option<&str>) -> bool {
    matches!(user_type, Some(value) if value == USER_TYPE_ROBOT)
}

/// 网页登录是否被硬拒（`63201`）。
///
/// # 参数
///
/// - `user_type`：命中账号的 `sys_user.user_type`。
///
/// # 返回值
///
/// AI 账号 → `Some(403 + 63201)`；其余 → `None`（继续走密码校验）。
pub fn login_forbidden(user_type: Option<&str>) -> Option<ApiError> {
    if !is_robot(user_type) {
        return None;
    }
    Some(ApiError::new(
        403,
        error_code::AI_LOGIN_FORBIDDEN,
        "AI 用户不能登录网页",
    ))
}

/// 删除任务是否被硬拒（`63202`）。
///
/// 与分派不同：**没有**「授个角色就能删」的路径（见模块文档）。
pub fn task_delete_forbidden(user_type: Option<&str>) -> Option<ApiError> {
    if !is_robot(user_type) {
        return None;
    }
    Some(ApiError::new(
        403,
        error_code::AI_TASK_DELETE_FORBIDDEN,
        "AI 用户不能删除任务（如需停止执行，请把任务转派给人类用户或改为「已取消」）",
    ))
}

/// 修改数据权限是否被硬拒（`63203`）。
pub fn data_scope_forbidden(user_type: Option<&str>) -> Option<ApiError> {
    if !is_robot(user_type) {
        return None;
    }
    Some(ApiError::new(
        403,
        error_code::AI_DATA_SCOPE_FORBIDDEN,
        "AI 用户不能修改数据权限（数据范围只能由人类管理员调整）",
    ))
}

/// 管理 AI 用户 / 修改 profile 是否被硬拒（`63204`）。
pub fn aiagent_admin_forbidden(user_type: Option<&str>) -> Option<ApiError> {
    if !is_robot(user_type) {
        return None;
    }
    Some(ApiError::new(
        403,
        error_code::AI_AGENT_ADMIN_FORBIDDEN,
        "AI 用户不能管理 AI 用户或修改 profile（只有人类管理员可以操作）",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 人类账号：四条硬拒**全部**不生效（不能把人类误伤）。
    ///
    /// `None` / `""`（老库未跑 `V0102`）也按人类处理：宁可放行让业务逻辑自己判，
    /// 也不要因为读不到一列就让所有人类用户登录不了。
    #[test]
    fn human_is_never_blocked() {
        for value in [Some(USER_TYPE_HUMAN), None, Some("")] {
            assert!(!is_robot(value), "{value:?} 不该判成 AI");
            assert!(login_forbidden(value).is_none());
            assert!(task_delete_forbidden(value).is_none());
            assert!(data_scope_forbidden(value).is_none());
            assert!(aiagent_admin_forbidden(value).is_none());
        }
    }

    /// AI 账号：四条硬拒逐条给出 403 + 对应码 + 各自文案。
    ///
    /// 断言 HTTP 状态与业务码**两个**都对：只对码不对状态时，前端拿到 200/400
    /// 也「看起来像拒了」；只对状态不对码时，前端文案会串到别的业务错误上。
    #[test]
    fn robot_is_blocked_with_expected_codes() {
        let value = Some(USER_TYPE_ROBOT);
        for (error, expected_code, message_keyword) in [
            (login_forbidden(value), 63201, "不能登录网页"),
            (task_delete_forbidden(value), 63202, "不能删除任务"),
            (data_scope_forbidden(value), 63203, "不能修改数据权限"),
            (aiagent_admin_forbidden(value), 63204, "不能管理 AI 用户"),
        ] {
            let error = error.unwrap_or_else(|| panic!("{expected_code} 应当硬拒 AI 用户"));
            assert_eq!(error.status, 403, "{expected_code} 的 HTTP 状态必须是 403");
            assert_eq!(error.code, expected_code, "{expected_code} 业务码不对");
            assert!(
                error.msg.contains(message_keyword),
                "{expected_code} 文案应含「{message_keyword}」，实际：{}",
                error.msg
            );
        }
    }

    /// 判定区分大小写：库里只可能有 `HUMAN` / `ROBOT`。
    ///
    /// 这条测试是**故意的严格性声明**：`"robot"` 被判成人类时，启动自检
    /// (`check_ai_agent_invariant`) 会把这个不一致报出来；如果这里改了宽容匹配，
    /// 不一致就永远不报警 —— 那才是真漏洞。
    #[test]
    fn user_type_match_is_case_sensitive() {
        assert!(!is_robot(Some("robot")));
        assert!(!is_robot(Some("Robot")));
        assert!(is_robot(Some(USER_TYPE_ROBOT)));
    }
}
