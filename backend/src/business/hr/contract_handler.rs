//! 劳动合同接口层（`biz_employee_contract` · HR-15）：设计 §5.2 的 #13~#15 三条接口。
//!
//! 数据层在 [`crate::business::hr::contract_model`]，入参校验在
//! [`crate::business::hr::contract_dto`]，出参契约在
//! [`crate::business::hr::employee_dto`]（HR-11 已定稿，本卡不另立一套）。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地卡 |
//! |:--:|---|---|---|---|
//! | 13 | POST | `/business/hr/employee/{id}/contract` | `business:hr:contract:manage` | HR-15 |
//! | 14 | PUT | `/business/hr/contract/{contract_id}` | `business:hr:contract:manage` | HR-15 |
//! | 15 | POST | `/business/hr/contract/{contract_id}/renew` | `business:hr:contract:manage` | HR-15 |
//!
//! **读取**不另开接口：合同是员工详情的子表（`GET /business/hr/employee/{id}` 的
//! `contracts[]` + `current_contract`，HR-12 已落地，SQL 由本卡的
//! [`crate::business::hr::contract_model::list_by_employee`] 供给）。设计 §5.2 只给
//! 了这三条写接口 —— 多开一条「合同详情」就会让「详情吃哪份形状」出现两个答案。
//!
//! # 数据权限（合同跟着员工走）
//!
//! `biz_employee_contract` 本身**没有** `dept_id`，所以不做独立的分页裁剪；
//! 三条接口一律先用员工侧探针 [`employee_handler::ensure_visible`]
//! （= 列表用的同一份 `scope::filter_for` 片段）判可见性，不可见 → **404**（不是 403）：
//! 403 会告诉越权者「这份合同真实存在」。
//!
//! # 续签为什么要一个事务
//!
//! 续签 = 「建新合同（`renewed_from_id` 指回原合同）」+「原合同 `status='RENEWED'`」，
//! 两件事必须同生同灭（AGENTS.md 约定 11 的同一类问题）：
//!
//! * 只建了新合同没封旧合同 ⇒ 两份 `ACTIVE`，员工「现行合同」按 `start_date` 取最新一份，
//!   旧的到期提醒还会继续报（同一段时间报两次）；
//! * 只封了旧合同没建新合同 ⇒ 员工没有生效合同，转正 / 薪资计算全部卡住，且没人能修
//!   （旧合同已是 `RENEWED` 终态）。
//!
//! 所以：**先把新合同插进去，再封旧合同**，任一步失败整笔回滚（旧合同原样 `ACTIVE`，
//! 用户重试即可）。反过来「先封旧再插新」会在撞号时留下「旧合同已封闭、新合同没进来」的死局。
//!
//! # 状态变更为什么不在本文件判边
//!
//! 合法边（`ACTIVE → EXPIRED/TERMINATED/RENEWED`、`EXPIRED → RENEWED`）、
//! `biz_status_log`、CAS 回写全在
//! [`crate::business::shared::state_machine::transition`]（HR-08 已注册
//! `ENTITY_EMPLOYEE_CONTRACT`）。本文件只做**字段级守卫**（`contract_dto` 里那些纯函数）。

use axum::Json;
use axum::extract::{Extension, Path};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::contract_dto::{
    self, ContractCreateReq, ContractRenewReq, ContractRenewRes, ContractUpdateReq,
};
use crate::business::hr::contract_model;
use crate::business::hr::employee_dto::{self, EmployeeContractRes};
use crate::business::hr::{employee_handler, employee_model};
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::shared::state_machine::{self, Operator};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0387 的 sys_menu.auth_code 逐字一致） ====================

/// 合同台账维护（三条接口共用；设计 §5.2 只有这一个码）。
pub const PERM_CONTRACT_MANAGE: &str = "business:hr:contract:manage";

// ==================== 错误原语 ====================

/// `409 + 68006`：当前状态下不允许这么做（不是格式错，也不是撞键）。
fn rule_blocked(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::HR_CONTRACT_RULE_BLOCKED, message.into())
}

/// `409 + 10003`：并发保护 / 原地流转。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 路径参数里的 uuid。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("{field} 必须是合法的 uuid（收到 `{text}`）"),
        )
    })
}

// ==================== ⑬ 新建合同 ====================

/// 给员工登记一份劳动合同（设计 §5.2 #13）。
#[nik::permission(
    path = "/business/hr/employee/{id}/contract",
    method = "POST",
    permission = "business:hr:contract:manage"
)]
#[nik::post("/business/hr/employee/{id}/contract")]
pub async fn contract_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ContractCreateReq>,
) -> ResResult<EmployeeContractRes> {
    let employee_id = parse_uuid(&id, "id")?;
    let principal = principal::resolve_for_claims(&claims).await?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(create_contract(&principal, operator, employee_id, &req).await?))
}

/// 新建合同的业务实现（接口与真库用例共用同一条路径）。
///
/// 事务里的顺序是 `锁员工 → 判员工状态 → 插合同 → 写建单日志 → 读回`：
/// 锁员工是为了不让「员工刚被离职、合同还在往里签」的并发窗口出现。
pub async fn create_contract(
    principal: &ScopedPrincipal,
    operator: Operator,
    employee_id: Uuid,
    req: &ContractCreateReq,
) -> std::result::Result<EmployeeContractRes, ApiError> {
    let (draft, status) = contract_dto::parse_create(req)?;
    // 数据范围探针（事务外先挡一次，避免为越权请求白开事务）。
    employee_handler::ensure_visible(db(), principal, employee_id).await?;

    let id = Uuid::now_v7();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建劳动合同事务"))?;
    let employee = match employee_model::lock_in(&tx, employee_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(employee_model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定员工档案"));
        }
    };
    if employee.status == "RESIGNED" {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "员工 {}（{}）已离职，不能新签劳动合同：返聘请先恢复员工状态（状态流转里「复职」）",
            employee.emp_no, employee.name
        )));
    }
    if let Err(error) = contract_model::insert(&tx, id, employee_id, &draft, &status, None, operator.id).await {
        let _ = tx.rollback().await;
        return Err(contract_model::write_error(
            error,
            "hr_contract_insert",
            &draft.contract_no,
        ));
    }
    // 建单留痕（`null → ACTIVE`）：合同的签订日 / 谁录的，要能从状态日志回溯。
    if let Err(error) = state_machine::log_created(
        &tx,
        state_machine::ENTITY_EMPLOYEE_CONTRACT,
        id,
        &draft.contract_no,
        &status,
        operator.clone(),
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    let row = match contract_model::find_in(&tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(contract_model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回新建的劳动合同"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建劳动合同事务"))?;
    Ok(EmployeeContractRes::from_input(row.to_input(), employee_dto::today()))
}

// ==================== ⑭ 修改合同 ====================

/// 修改合同字段 / 标到期 / 解除（设计 §5.2 #14）。
#[nik::permission(
    path = "/business/hr/contract/{contract_id}",
    method = "PUT",
    permission = "business:hr:contract:manage"
)]
#[nik::put("/business/hr/contract/{contract_id}")]
pub async fn contract_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(contract_id): Path<String>,
    Json(req): Json<ContractUpdateReq>,
) -> ResResult<EmployeeContractRes> {
    let contract_id = parse_uuid(&contract_id, "contract_id")?;
    let principal = principal::resolve_for_claims(&claims).await?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(update_contract(&principal, operator, contract_id, &req).await?))
}

/// 修改合同的业务实现。
///
/// # 三件事在**一个事务**里
///
/// 1. 字段更新（`contract_model::UPDATE_SQL` 的列白名单）；
/// 2. 状态动作（`to_status`：`EXPIRED` 手工标到期 / `TERMINATED` 解除）——
///    先 `state_machine::transition`（写 `biz_status_log` + 边校验 + 原因校验），
///    再 CAS 回写这一行；
/// 3. 读回出参。
///
/// 已解除 / 已续签的合同直接 409（[`contract_dto::update_blocked_reason`]）：
/// 它们是审计凭据，允许改字段就等于允许篡改历史（`state_machine::EMPLOYEE_CONTRACT_LOCKED`）。
pub async fn update_contract(
    principal: &ScopedPrincipal,
    operator: Operator,
    contract_id: Uuid,
    req: &ContractUpdateReq,
) -> std::result::Result<EmployeeContractRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启修改劳动合同事务"))?;
    let old = match contract_model::lock_in(&tx, contract_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(contract_model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定劳动合同"));
        }
    };
    if let Err(error) = employee_handler::ensure_visible(&tx, principal, old.employee_id).await {
        let _ = tx.rollback().await;
        return Err(error);
    }
    if let Some(reason) = contract_dto::update_blocked_reason(&old) {
        let _ = tx.rollback().await;
        return Err(rule_blocked(reason));
    }
    let draft = match contract_dto::parse_update(req, &old) {
        Ok(draft) => draft,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let action = match contract_dto::parse_status_action(req, &old) {
        Ok(action) => action,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    if !contract_dto::update_changes_anything(req, &old, &draft) {
        let _ = tx.rollback().await;
        return Err(conflict(
            "本次请求没有任何改动：合同字段与状态都与库里一致（没有改动就不写库、不写状态日志）",
        ));
    }
    if let Err(error) = contract_model::update(&tx, contract_id, &draft, operator.id).await {
        let _ = tx.rollback().await;
        return Err(contract_model::write_error(
            error,
            "hr_contract_update",
            &draft.contract_no,
        ));
    }
    if let Some(action) = action {
        if let Err(error) = state_machine::transition(
            &tx,
            state_machine::ENTITY_EMPLOYEE_CONTRACT,
            contract_id,
            &old.contract_no,
            &state_machine::EMPLOYEE_CONTRACT_EDGES,
            &old.status,
            &action.to_status,
            operator.clone(),
            action.terminate_reason.as_deref(),
            None,
        )
        .await
        {
            let _ = tx.rollback().await;
            return Err(error);
        }
        let affected = match contract_model::set_status(
            &tx,
            contract_id,
            &old.status,
            &action.to_status,
            action.terminated_date,
            action.terminate_reason.as_deref(),
            operator.id,
        )
        .await
        {
            Ok(affected) => affected,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(contract_model::write_error(
                    error,
                    "hr_contract_status",
                    &draft.contract_no,
                ));
            }
        };
        if affected == 0 {
            let _ = tx.rollback().await;
            return Err(conflict(
                "合同状态在本次流转期间已被他人改动，请刷新后重试（并发保护：状态 CAS 只允许一个成功）",
            ));
        }
    }
    let row = match contract_model::find_in(&tx, contract_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(contract_model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回修改后的劳动合同"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交修改劳动合同事务"))?;
    Ok(EmployeeContractRes::from_input(row.to_input(), employee_dto::today()))
}

// ==================== ⑮ 续签 ====================

/// 续签合同（设计 §5.2 #15）：建新合同 + 把旧合同封成 `RENEWED` + 接上续签链。
#[nik::permission(
    path = "/business/hr/contract/{contract_id}/renew",
    method = "POST",
    permission = "business:hr:contract:manage"
)]
#[nik::post("/business/hr/contract/{contract_id}/renew")]
pub async fn contract_renew(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(contract_id): Path<String>,
    Json(req): Json<ContractRenewReq>,
) -> ResResult<ContractRenewRes> {
    let contract_id = parse_uuid(&contract_id, "contract_id")?;
    let principal = principal::resolve_for_claims(&claims).await?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(renew_contract(&principal, operator, contract_id, &req).await?))
}

/// 续签的业务实现（见模块文档「续签为什么要一个事务」）。
///
/// # 返回
///
/// [`ContractRenewRes`]：新合同 + 原合同（已 `RENEWED`）+ 完整链（原合同在前）。
/// 链由 [`contract_model::chain_of`] 一条递归 CTE 查出，前端时间线一次拿全。
pub async fn renew_contract(
    principal: &ScopedPrincipal,
    operator: Operator,
    contract_id: Uuid,
    req: &ContractRenewReq,
) -> std::result::Result<ContractRenewRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启续签劳动合同事务"))?;
    let old = match contract_model::lock_in(&tx, contract_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(contract_model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定待续签的劳动合同"));
        }
    };
    if let Err(error) = employee_handler::ensure_visible(&tx, principal, old.employee_id).await {
        let _ = tx.rollback().await;
        return Err(error);
    }
    if let Some(reason) = contract_dto::renew_blocked_reason(&old.status) {
        let _ = tx.rollback().await;
        return Err(rule_blocked(reason));
    }
    let draft = match contract_dto::parse_renew(req, &old, employee_dto::today()) {
        Ok(draft) => draft,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };

    // ① 先插新合同（撞号时旧合同还是 ACTIVE，用户改编号重试即可，不留死局）。
    let new_id = Uuid::now_v7();
    if let Err(error) = contract_model::insert(
        &tx,
        new_id,
        old.employee_id,
        &draft,
        "ACTIVE",
        Some(old.id),
        operator.id,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(contract_model::write_error(
            error,
            "hr_contract_renew_insert",
            &draft.contract_no,
        ));
    }
    if let Err(error) = state_machine::log_created(
        &tx,
        state_machine::ENTITY_EMPLOYEE_CONTRACT,
        new_id,
        &draft.contract_no,
        "ACTIVE",
        operator.clone(),
        Some("续签新建"),
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }

    // ② 再封旧合同（状态机写日志 + CAS 回写，两件事都在本事务里）。
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_EMPLOYEE_CONTRACT,
        old.id,
        &old.contract_no,
        &state_machine::EMPLOYEE_CONTRACT_EDGES,
        &old.status,
        "RENEWED",
        operator.clone(),
        None,
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    let affected = match contract_model::set_status(&tx, old.id, &old.status, "RENEWED", None, None, operator.id).await
    {
        Ok(affected) => affected,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(contract_model::write_error(
                error,
                "hr_contract_renew_close",
                &old.contract_no,
            ));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(conflict(
            "原合同状态在本次续签期间已被他人改动，请刷新后重试（并发保护：状态 CAS 只允许一个成功）",
        ));
    }

    // ③ 读回三份视图（新合同 / 旧合同 / 完整链），再提交。
    let chain_rows = match contract_model::chain_of(&tx, new_id).await {
        Ok(rows) => rows,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询劳动合同续签链"));
        }
    };
    let new_row = match contract_model::find_in(&tx, new_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(contract_model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回续签后的新合同"));
        }
    };
    let old_row = match contract_model::find_in(&tx, old.id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(contract_model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回续签后的原合同"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交续签劳动合同事务"))?;

    let as_of = employee_dto::today();
    Ok(ContractRenewRes {
        contract: EmployeeContractRes::from_input(new_row.to_input(), as_of),
        previous: EmployeeContractRes::from_input(old_row.to_input(), as_of),
        chain: chain_rows
            .into_iter()
            .map(|input| EmployeeContractRes::from_input(input, as_of))
            .collect(),
    })
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 本文件源码（编译期嵌入）：下面两条断言直接扫 `#[nik::*]` 宏声明。
    const SOURCE: &str = include_str!("contract_handler.rs");
    /// 权限种子迁移原文。
    const V0387: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");

    /// 本文件声明的每条 `#[nik::*]` 路径 + 方法，都必须是 `ROUTE_CONTRACT` 里登记给
    /// **HR-15** 且权限码为 `business:hr:contract:manage` 的那三条。
    ///
    /// 宏路径写错（少个 `business`、`/contract` 写成 `/contracts`）不会编译报错，
    /// 线上表现是 404 / 被 SPA fallback 返回 HTML —— 这条测试是唯一能在 CI 抓住它的地方。
    #[test]
    fn every_macro_route_is_in_the_contract_for_hr15() {
        use crate::business::hr::handler::ROUTE_CONTRACT;

        let mut declared: Vec<(&str, &str)> = Vec::new();
        for line in SOURCE.lines() {
            let text = line.trim();
            if let Some(rest) = text.strip_prefix("#[nik::post(\"") {
                declared.push(("POST", rest.trim_end_matches("\")]")));
            } else if let Some(rest) = text.strip_prefix("#[nik::put(\"") {
                declared.push(("PUT", rest.trim_end_matches("\")]")));
            }
        }
        // needle 用 concat! 拼：源码里写完整的 get 宏开头 + 引号，会被 `handler.rs` 的目录扫描
        // 正则（`#[nik::get(` + 引号 + 路径）误当成一条真实路由声明（它会把 `))` 当成路径）。
        let get_macro = concat!("#[nik::", "get(");
        assert!(
            !SOURCE.lines().any(|line| line.trim_start().starts_with(get_macro)),
            "本卡不该有 GET 路由：合同读取走员工详情的 contracts[]"
        );
        assert_eq!(declared.len(), 3, "本卡正好三条写接口");

        for (verb, path) in &declared {
            let spec = ROUTE_CONTRACT
                .iter()
                .find(|spec| spec.method == *verb && spec.path == *path)
                .unwrap_or_else(|| panic!("{verb} {path} 不在 ROUTE_CONTRACT 里 —— 先登记契约再实现"));
            assert_eq!(spec.card, "HR-15", "{verb} {path} 的落地卡不是 HR-15");
            assert_eq!(
                spec.permission,
                Some(PERM_CONTRACT_MANAGE),
                "{verb} {path} 的权限码与实现不一致"
            );
        }
        for expected in [
            ("POST", "/business/hr/employee/{id}/contract"),
            ("PUT", "/business/hr/contract/{contract_id}"),
            ("POST", "/business/hr/contract/{contract_id}/renew"),
        ] {
            assert!(declared.contains(&expected), "少声明了 {expected:?}");
        }
    }

    /// 权限码必须逐字出现在 `V0387` 的种子里（否则线上 403，代码层看不出来）。
    #[test]
    fn permission_code_is_seeded_by_v0387() {
        assert_eq!(PERM_CONTRACT_MANAGE, "business:hr:contract:manage");
        assert!(
            V0387.contains(&format!("'{PERM_CONTRACT_MANAGE}'")),
            "权限码没在 V0387 的 sys_menu 种子里"
        );
    }

    /// 路径参数拼错时给的是 400 + 字段名，而不是 panic / 500。
    #[test]
    fn bad_uuid_path_is_a_400() {
        let error = parse_uuid("not-a-uuid", "contract_id").expect_err("非法 uuid");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("contract_id"));
    }

    /// 两条 409 的语义分开：规则拦截用 68006，并发/原地流转用 10003。
    #[test]
    fn conflict_codes_are_distinguishable() {
        assert_eq!(rule_blocked("已离职").status, 409);
        assert_eq!(rule_blocked("已离职").code, error_code::HR_CONTRACT_RULE_BLOCKED);
        assert_eq!(conflict("已被他人改动").code, error_code::RESOURCE_CONFLICT);
        assert_ne!(
            error_code::HR_CONTRACT_RULE_BLOCKED,
            error_code::RESOURCE_CONFLICT,
            "规则拦截与并发冲突必须能分开（前端文案不同）"
        );
    }
}
