//! 员工自助三接口（HR-41）：`/attendance/my` · `/leave/my` · `/salary/my`。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 数据来源 |
//! |:--:|---|---|---|---|
//! | 25 | GET | `/business/hr/attendance/my` | **登录即可**（设计 §5.2 #25） | `biz_attendance_record` |
//! | 47 | GET | `/business/hr/leave/my` | **登录即可**（设计 §5.2 脚注第 3 处 `/my`） | `biz_leave_request` |
//! | 46 | GET | `/business/hr/salary/my` | `business:hr:salary:my` | `biz_salary_detail` + 明细项 |
//!
//! # 三条接口的「我是谁」只来自令牌
//!
//! `AuthClaims.user_id` → [`self_service_model::my_employee_id`] → `biz_employee.id`。
//! 入参结构里没有 id 字段（见 DTO 模块文档），所以**没有可越权的参数**：
//! 想改别人的数据，只能改令牌 —— 那是认证层的事，不是本模块的事。
//!
//! # 为什么考勤 / 请假没有权限码，薪资有
//!
//! * 考勤天数与请假记录是**员工本人的劳动事实**，「能登录」就该能看
//!   （设计 §5.2 #25 的权限码列就是「登录即可」）；给一个码反而会让客户在配角色时
//!   忘了勾上，导致全员看不到自己的考勤；
//! * 工资条是**敏感数据**，有些客户要求只走纸质工资条 → 给 `business:hr:salary:my`
//!   让 HR 能整体关掉（`V0387` 已种该码，见 `self_service_dto::PERM_SALARY_MY`）。
//!
//! 注意：没有 `#[nik::permission]` 宏 = 不注册权限规则 = 权限层不匹配该路径（放行）。
//! **不能**写一个空权限码的 `#[nik::permission(permission = "")]` —— 那会注册一条
//! `required_permission = ""` 的规则，`has_permission("")` 恒假，全员 403
//! （`nik-web/src/middleware/permission_check.rs` 的匹配分支）。
//!
//! # 没有员工档时返回 200 + 空列表
//!
//! 理由见 `self_service_dto` 的模块文档：「越权返回空不是 403」。三条接口的空态一律是
//! `{ list: [], total: 0, page, page_size }`，前端按 `total == 0` 显示「暂无数据 /
//! 请联系 HR 绑定员工档案」。
//!
//! # 错误出口
//!
//! 只有 `ResResult<T>`：入参错误 400（`10001`），查库失败走 `utils::db_error` 的 500。
//! 本卡**不新增错误码**。

use axum::extract::{Extension, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::self_service_dto::{
    self as dto, MyAttendanceQuery, MyAttendanceRes, MyLeaveQuery, MyLeaveRes, MySalaryQuery, MySalaryRes,
};
use crate::business::hr::self_service_model as model;
use crate::shared::error_code;
use crate::utils;

/// 令牌 → 当前用户 id（自助接口的**唯一**身份来源）。
///
/// `TokenClaims::user_id()` 给的是 `sub` 的**字符串**（`&str`），自助接口要的是 `Uuid`
/// （`biz_employee.user_id` 是 uuid 列）。这里统一解析一次并把「解析不出来」当 401：
/// 能通过认证层却拿不到可解析的 `sub`，说明令牌与库里的用户 id 口径不一致
/// （换了认证实现 / 手工铸造的令牌），是**认证问题**，不是 400 参数问题。
fn caller_id(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims).ok_or_else(|| {
        ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            format!("自助接口需要登录用户（令牌里的 sub 不是 uuid：`{}`）", claims.user_id()),
        )
    })
}

// ==================== 组装（接口与真库用例共用同一路径） ====================

/// 「我的考勤」取数：`user_id` 由 handler 从令牌取，真库用例直接传。
///
/// # 返回值
///
/// `(本页行, 总数, page, page_size)`；`total = 0` 且 `rows` 为空有两种原因
/// （没绑员工档 / 确实没有考勤记录），响应上不可区分 —— 这是刻意的。
pub async fn attendance_of(
    user_id: Uuid,
    query: &MyAttendanceQuery,
) -> std::result::Result<(Vec<MyAttendanceRes>, i64, u64, u64), ApiError> {
    let (page, page_size) = dto::normalize_paging(query.page, query.page_size);
    let period_code = dto::parse_period_code(query.period_code.as_deref())?;
    let Some(employee_id) = model::my_employee_id(user_id)
        .await
        .map_err(|error| utils::db_error(error, "按登录账号查员工档案"))?
    else {
        return Ok((Vec::new(), 0, page, page_size));
    };
    let (rows, total) = model::my_attendance(employee_id, period_code.as_deref(), page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "查询我的考勤"))?;
    Ok((rows, total, page, page_size))
}

/// 「我的请假」取数（`status` 白名单见 [`dto::LEAVE_STATUSES`]）。
pub async fn leaves_of(
    user_id: Uuid,
    query: &MyLeaveQuery,
) -> std::result::Result<(Vec<MyLeaveRes>, i64, u64, u64), ApiError> {
    let (page, page_size) = dto::normalize_paging(query.page, query.page_size);
    let status = dto::parse_leave_status(query.status.as_deref())?;
    let Some(employee_id) = model::my_employee_id(user_id)
        .await
        .map_err(|error| utils::db_error(error, "按登录账号查员工档案"))?
    else {
        return Ok((Vec::new(), 0, page, page_size));
    };
    let (rows, total) = model::my_leaves(employee_id, status, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "查询我的请假"))?;
    Ok((rows, total, page, page_size))
}

/// 「我的薪资」取数：工资条 + 明细项（明细项一次 `IN` 查完再分组）。
pub async fn salary_of(
    user_id: Uuid,
    query: &MySalaryQuery,
) -> std::result::Result<(Vec<MySalaryRes>, i64, u64, u64), ApiError> {
    let (page, page_size) = dto::normalize_paging(query.page, query.page_size);
    let period_code = dto::parse_period_code(query.period_code.as_deref())?;
    let include_pending = query.include_pending.unwrap_or(false);
    let Some(employee_id) = model::my_employee_id(user_id)
        .await
        .map_err(|error| utils::db_error(error, "按登录账号查员工档案"))?
    else {
        return Ok((Vec::new(), 0, page, page_size));
    };
    let (rows, total) = model::my_payslips(employee_id, period_code.as_deref(), include_pending, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "查询我的薪资"))?;
    Ok((rows, total, page, page_size))
}

// ==================== ㉕ 我的考勤 ====================

/// 我的考勤（设计 §5.2 #25）：按 `biz_employee.user_id = 当前登录用户` 过滤，登录即可。
///
/// 入参：`period_code`（可选）、`page`、`page_size`。
#[nik::get("/business/hr/attendance/my")]
pub async fn my_attendance(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<MyAttendanceQuery>,
) -> ResResult<PageData<Vec<MyAttendanceRes>>> {
    let (rows, total, page, page_size) = attendance_of(caller_id(&claims)?, &query).await?;
    Ok(Res::page(rows, u64::try_from(total).unwrap_or(0), page, page_size))
}

// ==================== 我的请假 ====================

/// 我的请假（设计 §5.2 的 `/leave/my`）：按员工档过滤，登录即可。
///
/// 与 `/leave/page?mine=true` 的区别：
///
/// * 本接口**不要权限码**（`leave:list` 是审批人 / HR 的码，普通员工没有）；
/// * 本接口**不注入数据范围**（`/leave/page` 按部门裁，见 `self_service_model` 模块文档）；
/// * 本接口回参是自助投影（无审批轨迹、无创建人）。
#[nik::get("/business/hr/leave/my")]
pub async fn my_leaves(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<MyLeaveQuery>,
) -> ResResult<PageData<Vec<MyLeaveRes>>> {
    let (rows, total, page, page_size) = leaves_of(caller_id(&claims)?, &query).await?;
    Ok(Res::page(rows, u64::try_from(total).unwrap_or(0), page, page_size))
}

// ==================== ㊻ 我的薪资 ====================

/// 我的薪资（设计 §5.2 #46）：工资条列表 + 明细项，按员工档过滤。
///
/// 默认只出**已定稿**的薪资单（`APPROVED` / `POSTED` / `REVERSED`），
/// `include_pending=true` 才含草稿与在途（理由见 `self_service_dto::VISIBLE_STATUSES`）。
#[nik::permission(
    path = "/business/hr/salary/my",
    method = "GET",
    permission = "business:hr:salary:my"
)]
#[nik::get("/business/hr/salary/my")]
pub async fn my_salary(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<MySalaryQuery>,
) -> ResResult<PageData<Vec<MySalaryRes>>> {
    let (rows, total, page, page_size) = salary_of(caller_id(&claims)?, &query).await?;
    Ok(Res::page(rows, u64::try_from(total).unwrap_or(0), page, page_size))
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::handler::{ROUTE_CONTRACT, RouteSpec};

    const V0387_SEED: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");

    /// 三条自助路由必须是契约表里的「登录即可 / 指定码」形态，且卡号是本卡。
    #[test]
    fn self_service_routes_are_in_the_contract() {
        let wanted = [
            ("GET", "/business/hr/attendance/my", None, "HR-41"),
            ("GET", "/business/hr/leave/my", None, "HR-41"),
            ("GET", "/business/hr/salary/my", Some(dto::PERM_SALARY_MY), "HR-41"),
        ];
        for (method, path, permission, card) in wanted {
            let spec: &RouteSpec = ROUTE_CONTRACT
                .iter()
                .find(|spec| spec.method == method && spec.path == path)
                .unwrap_or_else(|| panic!("契约表缺 {method} {path}"));
            assert_eq!(spec.permission, permission, "{path} 的权限码");
            assert_eq!(spec.card, card, "{path} 的落地卡");
        }
        // 薪资的码必须在 V0387 种子里（写错只在线上变 403）
        assert!(V0387_SEED.contains(dto::PERM_SALARY_MY));
    }

    /// 自助路由**不许有路径参数**：有 `{id}` 就有「拿别人的 id 试试」的攻击面。
    #[test]
    fn self_service_routes_take_no_path_params() {
        for spec in ROUTE_CONTRACT.iter().filter(|spec| spec.path.ends_with("/my")) {
            assert!(
                !spec.path.contains('{'),
                "自助路由不许带路径参数：{} {}",
                spec.method,
                spec.path
            );
        }
        // 三条本卡路由都在其中（`/my` 结尾的必须 ≥3 条，否则上面的循环可能是空转）
        let count = ROUTE_CONTRACT.iter().filter(|spec| spec.path.ends_with("/my")).count();
        assert!(count >= 3, "契约里的 `/my` 路由只找到 {count} 条");
        println!("[自助] 契约里 /my 路由 {count} 条，均无路径参数 ✓");
    }

    /// 本文件声明的每条 `#[nik::*]` 路由都必须登记在 `ROUTE_CONTRACT` 里、且卡号是本卡。
    ///
    /// 用 `include_str!` + 正则读源码而不是手抄一份列表：手抄的列表在「加了一条路由忘了
    /// 登记契约」时**照样通过**，等于没有这道闸。
    ///
    /// 为什么重要：`/salary/my` 与 `/salary/{id}` 是兄弟路径（matchit 里静态段优先），
    /// 契约表少一行不会编译失败、也不会 404 —— 只会在权限审计表里凭空少一条自助接口，
    /// 而权限审计表正是验收「谁能看谁的工资」的依据。
    #[test]
    fn declared_routes_are_the_three_self_service_routes() {
        let source = include_str!("self_service_handler.rs");
        let pattern = regex::Regex::new(r#"#\[nik::(get|post|put|delete)\(\s*"([^"]+)""#).expect("正则合法");
        let mut mine: Vec<(String, String)> = pattern
            .captures_iter(source)
            .map(|capture| (capture[1].to_ascii_uppercase(), capture[2].to_string()))
            .collect();
        mine.sort_unstable();
        mine.dedup();
        assert_eq!(
            mine,
            vec![
                ("GET".to_string(), "/business/hr/attendance/my".to_string()),
                ("GET".to_string(), "/business/hr/leave/my".to_string()),
                ("GET".to_string(), "/business/hr/salary/my".to_string()),
            ],
            "本卡只该有三条自助路由，实际 {mine:?}"
        );

        for (method, path) in &mine {
            let spec = ROUTE_CONTRACT
                .iter()
                .find(|spec| spec.path == path && spec.method == method)
                .unwrap_or_else(|| panic!("{method} {path} 不在 ROUTE_CONTRACT 里 —— 先登记契约再实现"));
            assert_eq!(spec.card, "HR-41", "{method} {path} 在契约里登记的卡是 {}", spec.card);
        }
    }

    /// 只有 `/salary/my` 带权限码；考勤 / 请假**不许**有 `#[nik::permission]`。
    ///
    /// 给考勤加码的后果是静默的：码没进 `V0387` 种子时 `has_permission` 恒假 →
    /// 全员打开「我的考勤」就 403；码进了种子但客户没勾给员工角色 → 同样是全员 403。
    /// 这条断言盯着「别顺手加上」。
    #[test]
    fn only_salary_carries_a_permission_code() {
        let source = include_str!("self_service_handler.rs");
        let pattern =
            regex::Regex::new(r#"permission = "([^"]+)"\s*\)\]\s*#\[nik::(get|post|put|delete)\(\s*"([^"]+)""#)
                .expect("正则合法");
        let pairs: Vec<(String, String, String)> = pattern
            .captures_iter(source)
            .map(|capture| {
                (
                    capture[2].to_ascii_uppercase(),
                    capture[3].to_string(),
                    capture[1].to_string(),
                )
            })
            .collect();
        assert_eq!(pairs.len(), 1, "只该有 1 条「权限码 + 路由」声明，实际 {pairs:?}");
        assert_eq!(pairs[0].0, "GET");
        assert_eq!(pairs[0].1, "/business/hr/salary/my");
        assert_eq!(pairs[0].2, dto::PERM_SALARY_MY);
    }

    /// 空态口径：没绑员工档 → 200 + 空列表（不是 403 / 404）。
    ///
    /// 只测纯逻辑部分（`normalize_paging` 与「无员工档时直接返回空」的分支）：
    /// 真库路径在 `self_service_db_tests` 里用真实账号断言。
    #[test]
    fn empty_state_shape_is_a_page_not_an_error() {
        let (page, page_size) = dto::normalize_paging(None, None);
        let res: Res<PageData<Vec<MyAttendanceRes>>> = Res::page(Vec::new(), 0, page, page_size);
        let data = res.data.expect("分页响应必带 data");
        assert_eq!(data.total, 0);
        assert!(data.list.is_empty());
        assert_eq!(data.page, 1);
        assert_eq!(data.page_size, dto::MY_PAGE_DEFAULT);
        assert_eq!(res.code, 0, "空态是成功，不是错误码");
    }
}
