//! 员工薪资标准接口层（`biz_employee_salary` · HR-16）：设计 §5.2 的 #16 / #17 两条接口。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地卡 |
//! |:--:|---|---|---|---|
//! | 16 | GET | `/business/hr/employee/{id}/salary` | `business:hr:salary:detail` | HR-16 |
//! | 17 | POST | `/business/hr/employee/{id}/salary` | `business:hr:salary:calc` | HR-16 |
//!
//! 数据层在 [`crate::business::hr::salary_model`]（唯一的 SQL 出口 + 区间重叠判定），
//! 入参校验在 [`crate::business::hr::salary_dto`]，出参行投影复用 HR-11 的
//! [`crate::business::hr::employee_dto::EmployeeSalaryRes`]。
//!
//! # #16 的权限码为什么只用 `salary:detail`
//!
//! 设计 §5.2 #16 的权限列写的是「`business:hr:employee:detail` + `business:hr:salary:detail`」。
//! **两个都要求会把财务经理挡在门外**：§3.2 ⑤ 给 `FINANCE_MANAGER` 的是「薪资查看 / 过账 /
//! 导出 4 个码」，其中没有员工档案码（档案里有身份证、银行卡，不该给财务）。所以本卡取
//! **更具体的那个**（`salary:detail`）：
//!
//! * `HR_MANAGER`（36 个 HR 码）→ 能看；
//! * `FINANCE_MANAGER`（薪资 4 码）→ 能看（设计意图）；
//! * `DEPT_LEADER`（只有员工查看）→ 看**不到**工资（设计意图：主管不该看到下属工资）。
//!
//! 数据范围仍由员工侧探针 [`employee_handler::ensure_visible`] 裁剪（DEPT/SELF 范围外 → 404，
//! 不是 403：403 会告诉越权者「这个人真实存在」）。
//!
//! # 调薪为什么是一个事务
//!
//! 一次调薪 = 「插新版本」+「旧开放行 `effective_to` 收口到 `生效日 - 1`」，两件事必须同生同灭
//! （AGENTS.md 约定 11 的同一类问题）：
//!
//! * 只插新不收口 ⇒ 同一区间两条标准 → 工资取数是随机数（A3 脏数据，巡检事后才报）；
//! * 只收口不插新 ⇒ 员工从生效日起没有标准，试算直接少一项，且界面看不出「为什么少了」。
//!
//! 所以整笔在一个事务里，任一步失败全部回滚（旧标准原样「至今」，用户改日期重试即可）。
//! 事务尾部还有一道**写后自检**：把每个薪资项的版本重读一遍、两两判相交
//! （[`salary_model::find_pair_overlap`]），有交集就整笔回滚并点出是哪两行 —— 宁可报错，
//! 也不许把 A3 的脏数据落库。
//!
//! # 不用 `state_machine`
//!
//! `biz_employee_salary` 是**主数据台账**（行版本），没有状态机（设计 §4.2 的四张状态机里没有它）。
//! 调薪审批（`hr_salary_adjust`，三签）通过后**写的就是本接口同一段逻辑**（HR-38 的流程回调
//! 打进来），所以这里不留「审批专用分支」——审批只决定「谁有权调」，不改变「怎么落库」。

use axum::Json;
use axum::extract::{Extension, Path};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::employee_dto::{self, SalaryInput};
use crate::business::hr::employee_handler;
use crate::business::hr::employee_model;
use crate::business::hr::salary_dto::{self, SalaryAdjustReq, SalaryAdjustRes, SalaryListRes, SalaryWrite};
use crate::business::hr::salary_model;
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::shared::state_machine::Operator;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0387 的 sys_menu.auth_code 逐字一致） ====================

/// 读薪资标准（`salary:detail`）。
pub const PERM_SALARY_DETAIL: &str = "business:hr:salary:detail";

/// 调薪 / 登记 / 取消（`salary:calc`：算工资的人才能改标准）。
pub const PERM_SALARY_CALC: &str = "business:hr:salary:calc";

// ==================== 错误原语 ====================

/// `409 + 68008`：当前状态下不允许这么做（离职员工调薪 / 撤销一条不存在的标准）。
fn rule_blocked(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::HR_SALARY_RULE_BLOCKED, message.into())
}

/// `400 + 10001`：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 路径参数里的 uuid。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    Uuid::parse_str(text).map_err(|_| bad_request(format!("{field} 必须是合法的 uuid（收到 `{text}`）")))
}

// ==================== ⑯ 读：现行 + 历史 ====================

/// 员工薪资标准（设计 §5.2 #16：当前生效标准 + 历史，按 `effective_from DESC`）。
#[nik::permission(
    path = "/business/hr/employee/{id}/salary",
    method = "GET",
    permission = "business:hr:salary:detail"
)]
#[nik::get("/business/hr/employee/{id}/salary")]
pub async fn salary_list(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<SalaryListRes> {
    let employee_id = parse_uuid(&id, "id")?;
    let principal = principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(salary_list_of(&principal, employee_id).await?))
}

/// 读薪资标准的业务实现（接口与真库用例共用同一条路径）。
pub async fn salary_list_of(
    principal: &ScopedPrincipal,
    employee_id: Uuid,
) -> std::result::Result<SalaryListRes, ApiError> {
    employee_handler::ensure_visible(db(), principal, employee_id).await?;
    let employee = employee_model::find(employee_id)
        .await
        .map_err(|error| utils::db_error(error, "查询员工档案"))?
        .ok_or_else(employee_model::not_found_error)?;
    let as_of = employee_dto::today();
    let inputs = salary_model::list_by_employee(db(), employee_id)
        .await
        .map_err(|error| utils::db_error(error, "查询员工薪资标准"))?;
    Ok(SalaryListRes::build(
        employee.id.to_string(),
        employee.emp_no,
        employee.name,
        inputs,
        as_of,
    ))
}

// ==================== ⑰ 写：调薪 / 登记 / 修正 / 取消 ====================

/// 调薪（设计 §5.2 #17）：`items[{item_code, amount, effective_to?, remark?}]` + `effective_from`。
#[nik::permission(
    path = "/business/hr/employee/{id}/salary",
    method = "POST",
    permission = "business:hr:salary:calc"
)]
#[nik::post("/business/hr/employee/{id}/salary")]
pub async fn salary_adjust(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<SalaryAdjustReq>,
) -> ResResult<SalaryAdjustRes> {
    let employee_id = parse_uuid(&id, "id")?;
    let principal = principal::resolve_for_claims(&claims).await?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(adjust_salary(&principal, operator, employee_id, &req).await?))
}

/// 调薪的业务实现（接口与真库用例共用同一条路径）。
///
/// 事务里的顺序：`锁员工 → 判员工状态 → 逐项校验薪资项 → 逐项写（锁版本行）→
/// 写后自检（A3）→ 读回三份清单`。
pub async fn adjust_salary(
    principal: &ScopedPrincipal,
    operator: Operator,
    employee_id: Uuid,
    req: &SalaryAdjustReq,
) -> std::result::Result<SalaryAdjustRes, ApiError> {
    let (effective_from, writes) = salary_dto::parse_adjust(req)?;
    // 数据范围探针（事务外先挡一次，避免为越权请求白开事务）。
    employee_handler::ensure_visible(db(), principal, employee_id).await?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启调薪事务"))?;
    let employee = match adjust_in_tx(&tx, operator.clone(), employee_id, effective_from, &writes).await {
        Ok(res) => res,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    if let Err(error) = tx.commit().await {
        return Err(utils::db_error(error, "提交调薪事务"));
    }
    Ok(employee)
}

/// 事务内的调薪主体（拆出来是为了「失败一定回滚」只有一个出口）。
async fn adjust_in_tx(
    tx: &impl SqlExecutor,
    operator: Operator,
    employee_id: Uuid,
    effective_from: nik::prelude::chrono::NaiveDate,
    writes: &[SalaryWrite],
) -> std::result::Result<SalaryAdjustRes, ApiError> {
    let employee = employee_model::lock_in(tx, employee_id)
        .await
        .map_err(|error| utils::db_error(error, "锁定员工档案"))?
        .ok_or_else(employee_model::not_found_error)?;
    if employee.status == "RESIGNED" {
        return Err(rule_blocked(format!(
            "员工 {}（{}）已离职，不能调薪：离职员工的标准是历史数据，返聘后（状态流转里「复职」）再调整",
            employee.emp_no, employee.name
        )));
    }

    // 薪资项存在性：弱引用不建 FK，拼错的编码会变成「永远取不到标准的孤儿行」。
    for write in writes {
        let found = salary_model::item_name_of(tx, &write.item_code)
            .await
            .map_err(|error| utils::db_error(error, "校验薪资项"))?;
        if found.is_none() {
            return Err(bad_request(format!(
                "薪资项「{}」不存在或已停用：薪资标准必须挂在已定义的薪资项上（薪资项在「薪资项」页维护）",
                write.item_code
            )));
        }
    }

    let mut written_ids: Vec<(String, Uuid)> = Vec::new();
    let mut closed_ids: Vec<(String, Uuid)> = Vec::new();
    let mut canceled: Vec<String> = Vec::new();
    for write in writes {
        let outcome = adjust_one(tx, employee_id, write, &operator).await?;
        if let Some(id) = outcome.written {
            written_ids.push((write.item_code.clone(), id));
        }
        for id in outcome.closed {
            closed_ids.push((write.item_code.clone(), id));
        }
        if write.is_cancel() {
            canceled.push(write.item_code.clone());
        }
    }

    // 写后自检（A3）：每个薪资项重读版本，两两判相交。有交集 = 上面的收口逻辑漏了，
    // 整笔回滚并点出两行区间（不许把脏数据落库）。
    for write in writes {
        let versions = salary_model::find_versions(tx, employee_id, &write.item_code)
            .await
            .map_err(|error| utils::db_error(error, "读回薪资标准版本"))?;
        if let Some((left, right)) = salary_model::find_pair_overlap(&versions) {
            return Err(ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!(
                    "薪资项「{}」写入后仍有区间重叠（{} 与 {}）：本笔已整体回滚，请把这次操作连同生效日 {effective_from} 报给管理员",
                    write.item_code,
                    salary_model::range_text(left.effective_from, left.effective_to),
                    salary_model::range_text(right.effective_from, right.effective_to)
                ),
            ));
        }
    }

    // 读回：三份清单 + 最新全量（前端一次刷新）。
    let all = salary_model::list_by_employee(tx, employee_id)
        .await
        .map_err(|error| utils::db_error(error, "读回调薪后的薪资标准"))?;
    let mut written = Vec::with_capacity(written_ids.len());
    let mut closed = Vec::with_capacity(closed_ids.len());
    for (item_code, id) in &written_ids {
        written.push(pick(tx, employee_id, item_code, *id).await?);
    }
    for (item_code, id) in &closed_ids {
        closed.push(pick(tx, employee_id, item_code, *id).await?);
    }

    Ok(SalaryAdjustRes::build(
        employee.id.to_string(),
        employee.emp_no,
        employee.name,
        effective_from,
        employee_dto::today(),
        written,
        closed,
        canceled,
        all,
    ))
}

/// 从某个薪资项的版本里挑出指定 id 的行（回执要带「收口后的样子」）。
async fn pick(
    tx: &impl SqlExecutor,
    employee_id: Uuid,
    item_code: &str,
    id: Uuid,
) -> std::result::Result<SalaryInput, ApiError> {
    let versions = salary_model::find_versions(tx, employee_id, item_code)
        .await
        .map_err(|error| utils::db_error(error, "读回薪资标准行"))?;
    versions
        .into_iter()
        .find(|row| row.id == id)
        .map(|row| row.to_input())
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("薪资项「{item_code}」的第 {id} 行写入后读不回来，本笔已回滚"),
            )
        })
}

/// 单项写入的结局（回执要区分「写了哪一行」与「收口了哪些行」）。
struct Outcome {
    /// 新增 / 原地修正的行 id。
    written: Option<Uuid>,
    /// 被收口的旧行 id。
    closed: Vec<Uuid>,
}

/// 一个薪资项的写入（三种意图：现行标准 / 明确区间 / 取消）。
///
/// 这就是「区间不重叠」的全部判定：冲突检测（[`salary_model::first_conflict`]）先跑，
/// 通过之后才动库 —— 顺序反了会在拒绝时留下「旧行已收口、新行没进来」的半截状态
/// （虽然事务会回滚，但报错文案会变成「区间重叠」而不是「什么都没发生」）。
async fn adjust_one(
    tx: &impl SqlExecutor,
    employee_id: Uuid,
    write: &SalaryWrite,
    operator: &Operator,
) -> std::result::Result<Outcome, ApiError> {
    let versions = salary_model::lock_versions(tx, employee_id, &write.item_code)
        .await
        .map_err(|error| utils::db_error(error, "锁定薪资标准版本"))?;
    let item_name = versions.first().and_then(|row| row.item_name.clone());

    // ---- 取消：自生效日起不再有这一项 ----
    if write.is_cancel() {
        let last_day = write.last_day_before().ok_or_else(|| {
            bad_request(format!(
                "生效日 {} 早于可表示的最早日期，无法收口旧标准",
                write.effective_from
            ))
        })?;
        let covering: Vec<Uuid> = versions
            .iter()
            .filter(|row| row.effective_from < write.effective_from && row.covers(write.effective_from))
            .map(|row| row.id)
            .collect();
        let doomed: Vec<Uuid> = versions
            .iter()
            .filter(|row| row.effective_from >= write.effective_from)
            .map(|row| row.id)
            .collect();
        if covering.is_empty() && doomed.is_empty() {
            return Err(rule_blocked(format!(
                "薪资项「{}」（{}）在 {} 及以后没有标准可取消：本次什么都没改",
                item_name.as_deref().unwrap_or(&write.item_code),
                write.item_code,
                write.effective_from
            )));
        }
        salary_model::close_before(
            tx,
            employee_id,
            &write.item_code,
            write.effective_from,
            last_day,
            false,
            operator.id,
        )
        .await
        .map_err(|error| salary_model::write_error(error, "收口员工薪资标准"))?;
        salary_model::soft_delete_from(tx, employee_id, &write.item_code, write.effective_from, operator.id)
            .await
            .map_err(|error| salary_model::write_error(error, "删除员工薪资标准"))?;
        return Ok(Outcome {
            written: None,
            closed: covering,
        });
    }

    let amount = write.amount.ok_or_else(|| bad_request("amount 缺失"))?;

    // ---- 同生效日：原地修正（`uk_biz_emp_salary` 决定同一天只能有一行）----
    if let Some(same) = versions.iter().find(|row| row.effective_from == write.effective_from) {
        if !write.is_open_ended() || !same.is_open() {
            return Err(salary_model::overlap_error(
                &write.item_code,
                same.item_name.as_deref(),
                (write.effective_from, write.effective_to),
                (same.effective_from, same.effective_to),
            ));
        }
        salary_model::update_amount(tx, same.id, amount, write.remark.as_deref(), operator.id)
            .await
            .map_err(|error| salary_model::write_error(error, "修正员工薪资标准"))?;
        return Ok(Outcome {
            written: Some(same.id),
            closed: Vec::new(),
        });
    }

    // ---- 冲突检测：现行标准可以收口「开放行」，明确区间一条都不许相交 ----
    let closable: Vec<Uuid> = if write.is_open_ended() {
        versions
            .iter()
            .filter(|row| row.effective_from < write.effective_from && row.is_open())
            .map(|row| row.id)
            .collect()
    } else {
        Vec::new()
    };
    if let Some(conflict) = salary_model::first_conflict(write.effective_from, write.effective_to, &versions, &closable)
    {
        return Err(salary_model::overlap_error(
            &write.item_code,
            conflict.item_name.as_deref(),
            (write.effective_from, write.effective_to),
            (conflict.effective_from, conflict.effective_to),
        ));
    }

    let id = Uuid::now_v7();
    salary_model::insert(
        tx,
        id,
        employee_id,
        &salary_model::NewVersion {
            item_code: write.item_code.clone(),
            amount,
            effective_from: write.effective_from,
            effective_to: write.effective_to,
            remark: write.remark.clone(),
        },
        operator.id,
    )
    .await
    .map_err(|error| salary_model::write_error(error, "新增员工薪资标准"))?;

    let mut closed = Vec::new();
    if !closable.is_empty() {
        let last_day = write.last_day_before().ok_or_else(|| {
            bad_request(format!(
                "生效日 {} 早于可表示的最早日期，无法收口旧标准",
                write.effective_from
            ))
        })?;
        salary_model::close_before(
            tx,
            employee_id,
            &write.item_code,
            write.effective_from,
            last_day,
            true,
            operator.id,
        )
        .await
        .map_err(|error| salary_model::write_error(error, "收口员工薪资标准"))?;
        closed = closable;
    }
    Ok(Outcome {
        written: Some(id),
        closed,
    })
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 菜单 / 权限种子原文（编译期嵌入）：本文件的权限码必须真在 `V0387` 里。
    const V0387_SEED: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");

    /// 两个权限码都必须是 `V0387` 种下的（写错不会有编译错误，只会在线上全 403）。
    #[test]
    fn permission_codes_are_seeded() {
        for code in [PERM_SALARY_DETAIL, PERM_SALARY_CALC] {
            assert!(V0387_SEED.contains(code), "权限码 {code} 不在 V0387 的菜单种子里");
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "权限码必须 4 段：{code}");
            assert_eq!(&parts[0..3], ["business", "hr", "salary"], "码段固定：{code}");
        }
    }

    /// 契约表里 #16 / #17 的权限码就是本文件的两个常量（改一处漏一处 = 前端按钮与后端闸不一致）。
    #[test]
    fn route_contract_permissions_match_this_file() {
        use crate::business::hr::handler::ROUTE_CONTRACT;
        let spec = |method: &str| {
            ROUTE_CONTRACT
                .iter()
                .find(|spec| spec.method == method && spec.path == "/business/hr/employee/{id}/salary")
                .unwrap_or_else(|| panic!("契约里没有 {method} /business/hr/employee/{{id}}/salary"))
        };
        assert_eq!(spec("GET").permission, Some(PERM_SALARY_DETAIL));
        assert_eq!(spec("POST").permission, Some(PERM_SALARY_CALC));
        assert_eq!(spec("GET").card, "HR-16");
        assert_eq!(spec("POST").card, "HR-16");
    }

    /// uuid 路径参数：坏值 400 而不是 500。
    #[test]
    fn path_uuid_is_validated() {
        let error = parse_uuid("not-a-uuid", "id").expect_err("坏 uuid");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("uuid"));
        let id = Uuid::now_v7();
        assert_eq!(parse_uuid(&id.to_string(), "id").expect("好 uuid"), id);
    }

    /// 两个错误原语的状态码 / 业务码固定（前端按码分段出文案）。
    #[test]
    fn error_primitives_are_stable() {
        let blocked = rule_blocked("x");
        assert_eq!(blocked.status, 409);
        assert_eq!(blocked.code, error_code::HR_SALARY_RULE_BLOCKED);
        let invalid = bad_request("y");
        assert_eq!(invalid.status, 400);
        assert_eq!(invalid.code, error_code::PARAM_INVALID);
    }
}
