//! 考勤台账接口层（`biz_attendance_record` · HR-20）：设计 §5.2 的 #21 / #22。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地卡 |
//! |:--:|---|---|---|---|
//! | 21 | GET | `/business/hr/attendance/record/page` | `business:hr:attendance:record` | HR-20 |
//! | 22 | PUT | `/business/hr/attendance/record/{id}` | `business:hr:attendance:record` | HR-20 |
//!
//! 数据层在 [`crate::business::hr::attendance_record_model`]，入参校验在
//! [`crate::business::hr::attendance_record_dto`]。
//!
//! # 三个验收项各自落在哪
//!
//! | 验收 | 落点 | 证据 |
//! |---|---|---|
//! | **台账按人一行** | `uk_biz_att_rec`（库）+ `page` 只出本表行（不来 join 员工表补空行） | 真库用例 ①：两人两行、`employee_no` 唯一、`total=2` |
//! | **单行修正写入审计** | `changelog::record_in(&tx, …)` 与业务写**同事务**（D11） | 真库用例 ②：`sys_change_log` 一条、`changed_fields` 只含改动列、`b`/`a` 成对 |
//! | **锁定期间不可改** | `attendance_lock::lock_for_write`（锁期间 → 判 `LOCKED`，HR-23 的唯一判据）+ `UPDATE_SQL` 自带 `NOT EXISTS(…status='LOCKED')` + `V0392` 触发器 | 真库用例 ③：锁后 409 + `68010`，且库里的值**逐列未变** |
//!
//! # 单行修正为什么要开事务、并且**先锁期间再锁记录**
//!
//! 一次修正动三样东西：记录行、`sys_change_log`（审计）、期间头的四个汇总列（重算）。
//! 三者必须同生同灭 —— 只改记录不留痕 = 审计空洞；改了记录没重算汇总 = 台账合计与
//! 各行之和对不上（设计 §4.9 的口径：`employee_count / should_days_total …
//! 是「导入/修改后重算」的快照）。
//!
//! 加锁顺序固定为**期间 → 记录**：期间的 `recalc` 路径只锁期间（HR-19），导入链路
//! （HR-21/22）也会先锁期间再插记录。所有人同一个顺序，才不会互相等成死锁
//! （PG 检测到死锁会杀掉其中一个，用户看到 500）。
//!
//! # 「没有变化」返回 200 而不是 409（与 HR-19 的期间 PUT 不同）
//!
//! 台账是**单元格失焦保存**（设计 §6.2.3）：用户点进格子、发现没错、点别处失焦，
//! 前端就可能原样提交一次。这种请求回 409 会弹一个毫无意义的「冲突」框；
//! 回 `200 + changed=false` 才是「保存成功，什么也没变」。此时**不写库、不写审计**
//! （`sys_change_log` 的 `Skipped` 语义：值没变就不该有「变更」）。
//! 期间 PUT（HR-19）是整单编辑表单，用户点「保存」时确实期望有改动，所以那里 409 是对的。
//!
//! # 锁定期间拦什么
//!
//! | 操作 | 结果 | 理由 |
//! |---|---|---|
//! | 台账列表 | 允许（`editable=false`） | 锁定期间的台账正是「发工资前该核对的那份」，必须能看 |
//! | 单行修正 | 409 + `68010` | 设计 §4.2.3：`LOCKED` 之后记录不许增删改 |
//!
//! 「锁定后写保护」的完整接线属 **HR-23**：本卡在自己的入口上封死，并在
//! `attendance_record_model::UPDATE_SQL` 里留了语句级守卫；HR-23 把判据收到
//! [`crate::business::hr::attendance_lock`]（本文件的 ② 步已经改为调它），并加了库层触发器
//! `V0392` 兜住「绕过 handler 的裸 SQL」（导入 upsert / bridge / 手工脚本）。

use axum::Json;
use axum::extract::{ConnectInfo, Extension, Path, Query};
use axum::http::HeaderMap;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::changelog::{self, Action as ChangeAction, ChangeTarget, DiffInput};
use crate::business::hr::attendance_lock;
use crate::business::hr::attendance_period_dto::PeriodTotals;
use crate::business::hr::attendance_period_model as period_model;
use crate::business::hr::attendance_record_dto::{
    self, AttendanceRecordRes, RecordPageQuery, RecordPatchReq, RecordPatchRes,
};
use crate::business::hr::attendance_record_model as model;
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::shared::state_machine::Operator;
use crate::shared::error_code;
use crate::utils;

/// 考勤记录维护（台账列表 + 单行修正）。与 `V0387` 的 `sys_menu.auth_code` 逐字一致。
pub const PERM_RECORD: &str = "business:hr:attendance:record";

// ==================== 错误原语 ====================

/// `409 + 10003`：并发保护（期间在读与写之间被锁定 / 记录被他人先改）。
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

// ==================== ㉑ 台账列表 ====================

/// 考勤台账（设计 §5.2 #21）：按期间出「一人一行」，`dept_id` / `keyword` 可选。
///
/// 数据权限走 `attendance_record_model::page` → `scope::paginate`：
/// `DEPT` / `DEPT_AND_CHILD` 只出本部门（及下级）的考勤，`ALL` / 超管全出
/// （设计 §4.7 的登记表清单里 `biz_attendance_record` 是范围表，`dept_col = dept_id`）。
#[nik::permission(
    path = "/business/hr/attendance/record/page",
    method = "GET",
    permission = "business:hr:attendance:record"
)]
#[nik::get("/business/hr/attendance/record/page")]
pub async fn record_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<RecordPageQuery>,
) -> ResResult<PageData<Vec<AttendanceRecordRes>>> {
    let (page, page_size) = attendance_record_dto::normalize_paging(query.page, query.page_size);
    let filter = attendance_record_dto::filter_of(&query)?;
    let principal = principal::resolve_for_claims(&claims).await?;
    let (list, total) = page_records(&principal, &filter, page, page_size).await?;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 列表组装（接口与真库用例共用同一条路径）。
pub async fn page_records(
    principal: &ScopedPrincipal,
    filter: &attendance_record_dto::RecordFilter,
    page: u64,
    page_size: u64,
) -> std::result::Result<(Vec<AttendanceRecordRes>, i64), ApiError> {
    let (rows, total) = model::page(filter, page, page_size, principal)
        .await
        .map_err(|error| utils::db_error(error, "分页查询考勤台账"))?;
    Ok((rows.iter().map(|row| row.to_res()).collect(), total))
}

// ==================== ㉒ 单行修正 ====================

/// 单行修正（设计 §5.2 #22）：手工改这一行的天数 / 加班小时 / 迟到早退次数 / 备注。
///
/// 回执带三样：改后的行、`changed`、以及**同事务重算后的期间汇总**（详见 DTO 文档）。
#[nik::permission(
    path = "/business/hr/attendance/record/{id}",
    method = "PUT",
    permission = "business:hr:attendance:record"
)]
#[nik::put("/business/hr/attendance/record/{id}")]
pub async fn record_update(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<std::net::SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<RecordPatchReq>,
) -> ResResult<RecordPatchRes> {
    let record_id = parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    let request_ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    Ok(Res::ok(patch_record(operator, &request_ctx, record_id, &req).await?))
}

/// 单行修正的业务实现（接口与真库用例共用同一条路径）。
///
/// 事务里的顺序：
///
/// ```text
/// 读记录（取 period_id） → 锁期间 + 判 LOCKED → 锁记录并复读
///   → 合并入参（部分更新） → changed? 否则回滚并原样返回
///   → 整行改写（库层带锁定守卫） → 写 sys_change_log（同事务，D11）
///   → 期间汇总重算（合计 = 各行之和） → 复读记录与期间 → 提交
/// ```
///
/// # 异常情况
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | `id` 不是 uuid | 400 | `10001` |
/// | 入参一个字段都没给 / 越界 / 备注超长 | 400 | `10001` |
/// | 记录不存在或已删除 | 404 | `10005` |
/// | 期间已锁定 | 409 | `68010` |
/// | 期间在读与写之间被锁定 / 记录被并发改掉 | 409 | `10003` |
pub async fn patch_record(
    operator: Operator,
    request_ctx: &changelog::RequestCtx,
    record_id: Uuid,
    req: &RecordPatchReq,
) -> std::result::Result<RecordPatchRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启考勤单行修正事务"))?;

    // ① 先读一次拿 `period_id`（不加锁：加锁顺序必须是「期间 → 记录」，见模块文档）
    let peek = match model::find_in(&tx, record_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询待修正的考勤记录"));
        }
    };

    // ② 锁期间行 + 判 `LOCKED`：唯一判据在 `attendance_lock`（HR-23），这里不再自己写一遍
    //    （判据散在多处 = 有人加状态忘了改一处就漏拦；单测 `writable_statuses_are_complement_of_locked`
    //     把白名单与 `state_machine::ATTENDANCE_PERIOD_LOCKED` 对拍）。
    //    加锁顺序固定「期间 → 记录」：HR-19 的锁定/解锁、HR-21/22 的导入抢同一把锁。
    let period = match attendance_lock::lock_for_write(&tx, peek.period_id).await {
        Ok(row) => row,
        Err(error) => {
            // 期间已 LOCKED → 409 + 68010，报文点名期间与下一步；回滚前没有任何写入，库里的值不会变
            let _ = tx.rollback().await;
            return Err(error);
        }
    };

    // ④ 锁记录并**复读**（拿到锁之前的值可能已被别人改过；用旧值当 before 会写假 diff）
    let current = match model::lock_in(&tx, record_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定待修正的考勤记录"));
        }
    };

    // ⑤ 合并 + 校验（部分更新；一个字段都没给 → 400）
    let patch = match req.apply(&current.numbers, current.remark.as_deref()) {
        Ok(patch) => patch,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };

    // ⑥ 值没变：不写库、不写审计，原样回执（单元格失焦保存的常态）
    if !patch.changed {
        let totals = period.totals();
        let _ = tx.rollback().await;
        return Ok(RecordPatchRes {
            warnings: attendance_record_dto::consistency_warnings(&current.numbers),
            record: current.to_res(),
            changed: false,
            period_totals: totals,
        });
    }

    // ⑦ 整行改写（`UPDATE_SQL` 自带「期间未锁定」守卫：并发锁定的最后一道闸）
    let before_snapshot = model::diff_snapshot_before(&current);
    let after_snapshot = model::diff_snapshot_after(&patch);
    let affected = match model::update(&tx, record_id, &patch, operator.id).await {
        Ok(rows) => rows,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(model::write_error(
                error,
                "hr_attendance_record_update",
                &current.employee_no,
                &period.period_code,
            ));
        }
    };
    if affected == 0 {
        // 期间在这几毫秒里被锁定（或被软删）
        let _ = tx.rollback().await;
        return Err(conflict(format!(
            "考勤期间 {} 在本次修改期间被锁定，修改未保存：刷新后确认期间状态（已锁定的记录不能改）",
            period.period_code
        )));
    }

    // ⑧ 审计留痕与业务写**同事务**（D11）。失败 → 整笔回滚：宁可这次改单失败，
    // 也不留「考勤改了、审计没记」的半截事实。
    let audit = changelog::record_in(
        &tx,
        &request_ctx.ctx(),
        ChangeTarget::of(
            model::TABLE,
            record_id.to_string(),
            ChangeAction::Update,
            "修正考勤记录",
        ),
        DiffInput::update(&before_snapshot, &after_snapshot),
    )
    .await;
    if let Err(error) = audit {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "写考勤记录变更日志"));
    }

    // ⑨ 期间汇总重算（全量覆盖，幂等）：改了明细就必须让「合计 = 各行之和」继续成立，
    // 否则台账底部合计与各行之和对不上（设计 §4.9 的口径），而 HR-19 的 recalc 是
    // 另一个接口，用户不会想到去点它。
    match period_model::recalc_totals(&tx, current.period_id, operator.id).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            return Err(conflict(format!(
                "考勤期间 {} 在本次修改期间被锁定，汇总未重算：本次修改已撤销，刷新后重试",
                period.period_code
            )));
        }
        Ok(_) => {}
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "重算考勤期间汇总"));
        }
    }

    // ⑩ 复读（回执必须是库里的最终值，而不是内存里拼的）
    let updated = match model::find_in(&tx, record_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回修改后的考勤记录"));
        }
    };
    let totals: PeriodTotals = match period_model::find_in(&tx, current.period_id).await {
        Ok(Some(row)) => row.totals(),
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(period_model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回修正后的考勤期间汇总"));
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交考勤单行修正事务"))?;

    Ok(RecordPatchRes {
        warnings: attendance_record_dto::consistency_warnings(&updated.numbers),
        record: updated.to_res(),
        changed: true,
        period_totals: totals,
    })
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::attendance_record_dto::RecordNumbers;
    use nik::prelude::chrono::Utc;
    use nik::prelude::rust_decimal::Decimal;

    fn numbers() -> RecordNumbers {
        RecordNumbers {
            should_days: Decimal::new(2200, 2),
            actual_days: Decimal::new(2000, 2),
            leave_paid_days: Decimal::ZERO,
            leave_unpaid_days: Decimal::new(200, 2),
            sick_days: Decimal::ZERO,
            absent_days: Decimal::ZERO,
            ot_hours_workday: Decimal::new(1000, 2),
            ot_hours_weekend: Decimal::ZERO,
            ot_hours_holiday: Decimal::ZERO,
            late_count: 0,
            early_leave_count: 0,
        }
    }

    /// 路径参数必须是 uuid（非 uuid 400 而不是 404：这是形状错，不是「不存在」）。
    #[test]
    fn path_uuid_shape_is_validated() {
        assert!(parse_uuid(&Uuid::nil().to_string(), "id").is_ok());
        let error = parse_uuid("att-1", "id").expect_err("非 uuid 必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("att-1"), "{}", error.msg);
    }

    /// 锁定期间的报错必须**点名期间码 + 给出下一步**（解锁 / 重新锁定并试算），
    /// 并带 `68010`（前端据此禁用单元格编辑态后的提示）。
    /// 报文由 `attendance_lock`（HR-23 的唯一守卫面）出，本处只钉住「改了来源也仍要点名期间」。
    #[test]
    fn locked_period_error_is_actionable() {
        let error = attendance_lock::locked_error("2026-11");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);
        assert!(error.msg.contains("2026-11"));
        assert!(
            error.msg.contains(attendance_lock::LOCK_PERMISSION),
            "报文要指出下一步权限码：{}",
            error.msg
        );
    }

    /// 修正后的行必须自带「能不能改」与期间状态（前端据此置灰单元格）。
    #[test]
    fn res_carries_period_state() {
        let row = model::AttendanceRecordRow {
            id: Uuid::nil(),
            period_id: Uuid::nil(),
            period_code: "2026-11".to_string(),
            period_status: "LOCKED".to_string(),
            employee_id: Uuid::nil(),
            employee_no: "EMP-0001".to_string(),
            employee_name: "张三".to_string(),
            dept_id: None,
            dept_name: None,
            numbers: numbers(),
            source: "IMPORT".to_string(),
            remark: None,
            updated_at: Utc::now(),
            update_by: None,
            updated_by_name: None,
        };
        let res = row.to_res();
        assert!(!res.editable, "锁定期间的行不可编辑");
        assert_eq!(res.period_status_label, "已锁定");
        assert_eq!(res.ot_hours_total, Decimal::new(1000, 2), "加班合计 = 三列之和");
        assert_eq!(res.source_label, "导入");
    }
}
