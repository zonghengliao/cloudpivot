//! 考勤期间接口层（`biz_attendance_period` · HR-19）：设计 §5.2 的 #18 / #19 / #20
//! + 卡面「期间 CRUD / 汇总重算」补的三条。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地卡 |
//! |:--:|---|---|---|---|
//! | 18 | GET | `/business/hr/attendance/period/page` | `business:hr:attendance:list` | HR-19 |
//! | 19 | POST | `/business/hr/attendance/period` | `business:hr:attendance:period` | HR-19 |
//! | 20 | POST | `/business/hr/attendance/period/{id}/status` | `business:hr:attendance:lock` | HR-19（原排 HR-23） |
//! | ★ | PUT | `/business/hr/attendance/period/{id}` | `business:hr:attendance:period` | HR-19 |
//! | ★ | DELETE | `/business/hr/attendance/period/{id}` | `business:hr:attendance:period` | HR-19 |
//! | ★ | POST | `/business/hr/attendance/period/{id}/recalc` | `business:hr:attendance:period` | HR-19 |
//!
//! 数据层在 [`crate::business::hr::attendance_period_model`]，入参校验在
//! [`crate::business::hr::attendance_period_dto`]。
//!
//! # ★ 三条为什么必须在本卡落地（偏差说明）
//!
//! 设计 §5.2 的考勤组只有 8 条，其中期间相关的只有 #18 列表 / #19 建期间 / #20 状态。
//! 但 **HR-19 的卡面验收是「期间 CRUD + 状态机正确 + 汇总重算幂等」**，这三件都缺接口：
//!
//! | 缺什么 | 不补的后果 |
//! |---|---|
//! | 改（PUT） | 建错日期（把「26-25 计薪周期」建成了自然月）只能删了重建，而删又要先清空记录 —— 用户被卡在「改不了也删不了」 |
//! | 删（DELETE） | 建错的期间永久占着 `uk_biz_att_period`（一公司一月一批），下个月月初就会被自己的笔误堵死 |
//! | 汇总重算（recalc） | 卡面验收项无处触发；HR-22 的「导入后重算」也只能抄一段 SQL 进 impexp 钩子，出现第二个写这四个列的地方 |
//!
//! 三条都**没有新增权限码**（复用 `V0387` 已种下的 `attendance:period` / `attendance:list`），
//! 所以 `contract_permissions_are_seeded_by_v0387` 仍然全绿；`ROUTE_CONTRACT` 相应从 46 条
//! 长到 49 条，并在表头注明了「设计 46 + HR-19 补 3」。
//!
//! # 状态机（本卡验收项）
//!
//! ```text
//! DRAFT ──提交──▶ SUBMITTED ──锁定──▶ LOCKED ──解锁（必须给原因）──▶ DRAFT
//! ```
//!
//! 三条边由 `shared::state_machine::ATTENDANCE_PERIOD_EDGES`（HR-08 注册）唯一判定，
//! 本文件**不写** `match (from, to)`：边是否合法、是否要原因、`biz_status_log` 怎么落，
//! 全在 [`state_machine::transition`] 里；本文件只做「锁行 → 判期间态 → 走状态机 → CAS 回写」。
//! `SUBMITTED → LOCKED` 之后再 `LOCKED → DRAFT` 是**唯一**的回位路径（没有 `SUBMITTED → DRAFT`：
//! 设计 §4.2.3 的图只有这两条回位边，提交态可以直接重跑导入，不需要撤回）。
//!
//! # 为什么要开事务、并且先 `FOR UPDATE` 锁期间
//!
//! 每个写操作都是「读当前状态 → 判守卫 → 写」。不锁的话，两个人同时点「锁定」会各读到
//! `SUBMITTED` 再各写一次 `LOCKED`：第二次的 `state_machine::transition` 会把
//! `SUBMITTED → LOCKED` 再记一条 `biz_status_log`（轨迹里出现两次锁定），而且
//! `updated_at` / `locked_at` 后写覆盖先写。锁行 + 状态 CAS（`WHERE status = 旧值`）
//! 两层一起，只有一次能成功，另一次拿到 `rows = 0` → 409「已被他人处理，请刷新」。
//!
//! # 锁定后（`LOCKED`）拦什么
//!
//! | 操作 | 结果 | 理由 |
//! |---|---|---|
//! | 改（PUT） | 409 + `68010` | 锁定期间是薪资试算的基线，改日期等于改口径 |
//! | 删（DELETE） | 409 + `68010` | 同上，而且删除会连带考勤记录 |
//! | 重算（recalc） | 409 + `68010` | 记录冻结 ⇒ 快照也必须冻（库层 `RECALC_SQL` 也带 `status <> 'LOCKED'`） |
//! | 状态（status） | 允许 `LOCKED → DRAFT` | 解锁是设计 §4.2.3 给的唯一出口，要原因 |
//!
//! 记录层的「锁定后不许增删改」属 HR-23（锁定时把期间的写保护接到 `biz_attendance_record` 上），
//! 本卡只封期间头。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::attendance_period_dto::{
    self, AttendancePeriodRes, PeriodCreateReq, PeriodPageQuery, PeriodRecalcRes, PeriodStatusReq, PeriodUpdateReq,
    STATUS_DRAFT,
};
use crate::business::hr::attendance_period_model as model;
use crate::business::shared::state_machine::{self, Operator};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0387 的 sys_menu.auth_code 逐字一致） ====================

/// 考勤期间维护（建 / 改 / 删 / 重算）。
pub const PERM_PERIOD: &str = "business:hr:attendance:period";

/// 考勤期间列表（页面码，设计 §5.2 #18）。
pub const PERM_LIST: &str = "business:hr:attendance:list";

/// 提交 / 锁定 / 解锁（设计 §4.2.3「需 `attendance:lock` 权限码」）。
pub const PERM_LOCK: &str = "business:hr:attendance:lock";

// ==================== 错误原语 ====================

/// `409 + 68010`：当前状态下不允许这么做（已锁定 / 期间下有记录 / 非草稿）。
fn rule_blocked(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED, message.into())
}

/// `409 + 10003`：并发保护 / 原地重放（不是入参格式错）。
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

// ==================== ⑱ 列表 ====================

/// 考勤期间列表（设计 §5.2 #18）。
///
/// 本表没有部门 / 归属人列，不做数据范围裁剪（设计 §4.7 的登记表清单里只有
/// `biz_attendance_record` 是范围表）；可见性由权限码 `attendance:list` 控制。
#[nik::permission(
    path = "/business/hr/attendance/period/page",
    method = "GET",
    permission = "business:hr:attendance:list"
)]
#[nik::get("/business/hr/attendance/period/page")]
pub async fn period_page(Query(query): Query<PeriodPageQuery>) -> ResResult<PageData<Vec<AttendancePeriodRes>>> {
    let (page, page_size) = attendance_period_dto::normalize_page(query.page, query.page_size);
    let filter = attendance_period_dto::filter_of(&query)?;
    let (list, total) = page_periods(&filter, page, page_size).await?;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 列表组装（接口与真库用例共用同一条路径）。
pub async fn page_periods(
    filter: &attendance_period_dto::PeriodFilter,
    page: u64,
    page_size: u64,
) -> std::result::Result<(Vec<AttendancePeriodRes>, i64), ApiError> {
    let (rows, total) = model::page(filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询考勤期间"))?;
    Ok((rows.iter().map(|row| row.to_res()).collect(), total))
}

// ==================== ⑲ 建期间 ====================

/// 建考勤期间（设计 §5.2 #19：`period_code` + `period_start/end`）。
#[nik::permission(
    path = "/business/hr/attendance/period",
    method = "POST",
    permission = "business:hr:attendance:period"
)]
#[nik::post("/business/hr/attendance/period")]
pub async fn period_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<PeriodCreateReq>,
) -> ResResult<AttendancePeriodRes> {
    let operator: Operator = (&claims).into();
    Ok(Res::ok(create_period(operator, &req).await?))
}

/// 建期间的业务实现（接口与真库用例共用同一条路径）。
///
/// 事务里的顺序是 `插期间 → 写建单日志 → 读回`：`biz_status_log` 的建单行与期间本身
/// 同生同灭 —— 没有建单日志的期间在「这张期间是谁建的、什么时候建的」上就是空白
/// （设计 §4.2.3 要求锁定写日志，建期同理）。
pub async fn create_period(
    operator: Operator,
    req: &PeriodCreateReq,
) -> std::result::Result<AttendancePeriodRes, ApiError> {
    let draft = attendance_period_dto::parse_create(req)?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建考勤期间事务"))?;
    let id = Uuid::now_v7();
    if let Err(error) = model::insert(&tx, id, &draft, operator.id).await {
        let _ = tx.rollback().await;
        return Err(model::write_error(
            error,
            "hr_attendance_period_create",
            &draft.period_code,
        ));
    }
    if let Err(error) = state_machine::log_created(
        &tx,
        state_machine::ENTITY_ATTENDANCE_PERIOD,
        id,
        &draft.period_code,
        STATUS_DRAFT,
        operator,
        Some("新建考勤期间"),
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    let row = match model::find_in(&tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回新建的考勤期间"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交建考勤期间事务"))?;
    Ok(row.to_res())
}

// ==================== ★ 改期间 ====================

/// 改考勤期间（起 / 止 / 备注）。
#[nik::permission(
    path = "/business/hr/attendance/period/{id}",
    method = "PUT",
    permission = "business:hr:attendance:period"
)]
#[nik::put("/business/hr/attendance/period/{id}")]
pub async fn period_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<PeriodUpdateReq>,
) -> ResResult<AttendancePeriodRes> {
    let period_id = parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(update_period(operator, period_id, &req).await?))
}

/// 改期间的业务实现。
///
/// 先锁行再判「是否已锁定」：否则「读到 DRAFT → 别人锁定 → 本请求照旧写」会把
/// 已冻结的基线改掉。改完必须读回 —— 库里 `updated_at` 等列是数据库生成的值。
pub async fn update_period(
    operator: Operator,
    period_id: Uuid,
    req: &PeriodUpdateReq,
) -> std::result::Result<AttendancePeriodRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改考勤期间事务"))?;
    let current = match model::lock_in(&tx, period_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定待修改的考勤期间"));
        }
    };
    if current.is_locked() {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "考勤期间 {} 已锁定，不能修改（锁定期间是薪资试算的基线）：\
             先「解锁」（要填原因）回到草稿，再改日期",
            current.period_code
        )));
    }
    let patch = match attendance_period_dto::parse_update(
        req,
        (current.period_start, current.period_end, current.remark.as_deref()),
    ) {
        Ok(patch) => patch,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    if !patch.changed {
        let _ = tx.rollback().await;
        return Err(conflict(format!(
            "考勤期间 {} 的起止日期与备注都与库里一致：本次没有任何改动（不写库、不写状态日志）",
            current.period_code
        )));
    }
    match model::update(&tx, period_id, &patch, operator.id).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Ok(_) => {}
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(model::write_error(
                error,
                "hr_attendance_period_update",
                &current.period_code,
            ));
        }
    }
    let row = match model::find_in(&tx, period_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回修改后的考勤期间"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交改考勤期间事务"))?;
    Ok(row.to_res())
}

// ==================== ⑳ 状态流转 ====================

/// 提交 / 锁定 / 解锁（设计 §5.2 #20）。
///
/// 权限码取 `attendance:lock`（设计 §4.2.3：三条边都要这个码 —— 提交之后紧接着就是锁定，
/// 分成两个码会让「能提交不能锁」的账号把期间停在半路）。
#[nik::permission(
    path = "/business/hr/attendance/period/{id}/status",
    method = "POST",
    permission = "business:hr:attendance:lock"
)]
#[nik::post("/business/hr/attendance/period/{id}/status")]
pub async fn period_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<PeriodStatusReq>,
) -> ResResult<AttendancePeriodRes> {
    let period_id = parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(change_status(operator, period_id, &req).await?))
}

/// 状态流转的业务实现（边由状态机判，见模块文档「状态机」）。
pub async fn change_status(
    operator: Operator,
    period_id: Uuid,
    req: &PeriodStatusReq,
) -> std::result::Result<AttendancePeriodRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启考勤期间状态流转事务"))?;
    let current = match model::lock_in(&tx, period_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定待流转的考勤期间"));
        }
    };
    let change = match attendance_period_dto::parse_status(req, &current.status) {
        Ok(change) => change,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_ATTENDANCE_PERIOD,
        period_id,
        &current.period_code,
        &state_machine::ATTENDANCE_PERIOD_EDGES,
        &current.status,
        &change.to_status,
        operator.clone(),
        change.reason.as_deref(),
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    let affected = match model::set_status(&tx, period_id, &current.status, &change.to_status, operator.id).await {
        Ok(affected) => affected,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "回写考勤期间状态"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(conflict(
            "考勤期间状态在本次流转期间已被他人改动，请刷新后重试（并发保护：状态 CAS 只允许一个成功）",
        ));
    }
    let row = match model::find_in(&tx, period_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回流转后的考勤期间"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交考勤期间状态流转事务"))?;
    Ok(row.to_res())
}

// ==================== ★ 汇总重算 ====================

/// 汇总重算（把四个快照列按期间内考勤记录全量覆盖，幂等）。
///
/// 权限码取 `attendance:period`（维护期间头的人才能改期间上的数字），不取 `attendance:import`：
/// 导入是 HR-21 的接口，重算是期间头的维护动作 —— 两者可以由不同角色持有。
#[nik::permission(
    path = "/business/hr/attendance/period/{id}/recalc",
    method = "POST",
    permission = "business:hr:attendance:period"
)]
#[nik::post("/business/hr/attendance/period/{id}/recalc")]
pub async fn period_recalc(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<PeriodRecalcRes> {
    let period_id = parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(recalc_period(operator, period_id).await?))
}

/// 汇总重算的业务实现（可见 `PeriodRecalcRes` 的 `before` / `after` / `changed`）。
///
/// 事务里：`锁期间 → 判锁定 → 记旧快照 → 全量覆盖 → 读回 → 比对新旧`。
/// 「幂等」在回执里是**可观测的**：第二次调用 `changed = false` 且四个数与第一次相同。
pub async fn recalc_period(operator: Operator, period_id: Uuid) -> std::result::Result<PeriodRecalcRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启考勤期间汇总重算事务"))?;
    let current = match model::lock_in(&tx, period_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定待重算的考勤期间"));
        }
    };
    if current.is_locked() {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "考勤期间 {} 已锁定，不能重算汇总：锁定期间是薪资试算的基线，\
             解锁（要填原因）之后再重算",
            current.period_code
        )));
    }
    let before = current.totals();
    let scanned = match model::count_records(&tx, period_id).await {
        Ok(count) => count,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "统计考勤期间内的记录条数"));
        }
    };
    match model::recalc_totals(&tx, period_id, operator.id).await {
        // `0` 行只可能来自「期间在这几毫秒里被锁定」（锁行已挡住并发写），按并发冲突报
        Ok(0) => {
            let _ = tx.rollback().await;
            return Err(conflict(
                "考勤期间在重算期间被锁定，汇总未更新：刷新后确认期间状态再重试",
            ));
        }
        Ok(_) => {}
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "重算考勤期间汇总"));
        }
    }
    let row = match model::find_in(&tx, period_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回重算后的考勤期间"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交考勤期间汇总重算事务"))?;
    let after = row.totals();
    Ok(PeriodRecalcRes {
        scanned,
        before,
        after,
        changed: !before.same_as(&after),
        period: row.to_res(),
    })
}

// ==================== ★ 删期间 ====================

/// 删考勤期间（**软删**，只允许「草稿 + 期间下没有记录」）。
///
/// 为什么不是硬删：`biz_attendance_record.period_id` 是 `ON DELETE CASCADE`，硬删会连带
/// 把考勤记录抹掉；而记录是工资扣款的计算依据（`biz_salary_detail.attendance_period_id`
/// 还是 `ON DELETE RESTRICT`，硬删会被库直接拒）。软删保留数据、可恢复，
/// 且让 `uk_biz_att_period` 立刻释放（唯一索引带 `WHERE deleted_at IS NULL`）。
#[nik::permission(
    path = "/business/hr/attendance/period/{id}",
    method = "DELETE",
    permission = "business:hr:attendance:period"
)]
#[nik::delete("/business/hr/attendance/period/{id}")]
pub async fn period_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<AttendancePeriodRes> {
    let period_id = parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(delete_period(operator, period_id).await?))
}

/// 删期间的业务实现（回执是删除前的快照，前端可直接把它从列表里摘掉）。
pub async fn delete_period(operator: Operator, period_id: Uuid) -> std::result::Result<AttendancePeriodRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启删考勤期间事务"))?;
    let current = match model::lock_in(&tx, period_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定待删除的考勤期间"));
        }
    };
    if current.status != STATUS_DRAFT {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "考勤期间 {} 当前是「{}」，只有草稿可以删除：先按状态机走回 DRAFT\
             （SUBMITTED 锁定后解锁）再删；已产生工资单据的期间要保留作为历史依据",
            current.period_code,
            attendance_period_dto::status_label(&current.status)
        )));
    }
    let records = match model::count_records(&tx, period_id).await {
        Ok(count) => count,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "统计待删考勤期间的记录条数"));
        }
    };
    if records > 0 {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "考勤期间 {} 下已有 {records} 条考勤记录，不能删除：\
             直接把这一期留着（改日期即可复用），或先按约定清理记录再删",
            current.period_code
        )));
    }
    let snapshot = current.to_res();
    match model::soft_delete(&tx, period_id, operator.id).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            return Err(conflict("考勤期间在删除期间被他人改动（或已删除），请刷新后重试"));
        }
        Ok(_) => {}
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "软删考勤期间"));
        }
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交删考勤期间事务"))?;
    Ok(snapshot)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::handler::{ROUTE_CONTRACT, RouteSpec};

    /// 本卡注册的 6 条路由必须都在契约表里（方法 / 路径 / 权限码三件都对得上）。
    ///
    /// 只靠 `every_macro_path_in_module_is_in_contract` 不够：那条只比**路径**，
    /// 权限码写错（`attendance:period` 写成 `attendance:periods`）不会红，
    /// 只会在线上变成「HR 全 403」。
    #[test]
    fn period_routes_match_contract() {
        let expected = [
            ("GET", "/business/hr/attendance/period/page", PERM_LIST),
            ("POST", "/business/hr/attendance/period", PERM_PERIOD),
            ("POST", "/business/hr/attendance/period/{id}/status", PERM_LOCK),
            ("PUT", "/business/hr/attendance/period/{id}", PERM_PERIOD),
            ("DELETE", "/business/hr/attendance/period/{id}", PERM_PERIOD),
            ("POST", "/business/hr/attendance/period/{id}/recalc", PERM_PERIOD),
        ];
        for (method, path, permission) in expected {
            let spec: Vec<&RouteSpec> = ROUTE_CONTRACT
                .iter()
                .filter(|spec| spec.method == method && spec.path == path)
                .collect();
            assert_eq!(spec.len(), 1, "契约表里应有且只有一条 {method} {path}");
            assert_eq!(
                spec[0].permission,
                Some(permission),
                "{method} {path} 的权限码与契约表不一致"
            );
            assert_eq!(spec[0].card, "HR-19", "{method} {path} 的落地卡应是 HR-19");
        }
    }

    /// 权限码必须 4 段（`business:hr:{资源}:{动作}`），与 `V0387` 种子同口径。
    #[test]
    fn period_permissions_are_four_segments() {
        for permission in [PERM_PERIOD, PERM_LIST, PERM_LOCK] {
            let parts: Vec<&str> = permission.split(':').collect();
            assert_eq!(parts.len(), 4, "权限码必须 4 段：{permission}");
            assert_eq!(parts[0], "business");
            assert_eq!(parts[1], "hr");
            assert_eq!(parts[2], "attendance");
        }
    }
}
