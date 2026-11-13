//! 薪资单接口层（`biz_salary_sheet` · HR-36）：设计 §5.2 的 #36~#40 五条接口。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地 |
//! |:--:|---|---|---|---|
//! | 36 | GET | `/business/hr/salary/page` | `business:hr:salary:list` | 列表（期间 / 状态 / 金额合计） |
//! | 37 | GET | `/business/hr/salary/{id}` | `business:hr:salary:detail` | **单头汇总**（单头 + 部门汇总 + 三口径对账） |
//! | 38 | GET | `/business/hr/salary/{id}/detail/page` | `business:hr:salary:detail` | 明细分页（含明细项展开） |
//! | 39 | POST | `/business/hr/salary/calc` | `business:hr:salary:calc` | 试算（不落库） |
//! | 40 | POST | `/business/hr/salary` | `business:hr:salary:calc` | 建单并落库 |
//! | 41 | PUT | `/business/hr/salary/{id}/detail/{detail_id}` | `business:hr:salary:calc` | 明细覆盖（HR-37：改单项 → 重算汇总 → 写状态日志） |
//!
//! 数据层在 [`crate::business::hr::salary_sheet_model`]（唯一 SQL 出口），入参 / 回执契约与
//! 对账纯函数在 [`crate::business::hr::salary_sheet_dto`]，计算本身在
//! [`crate::business::hr::salary_compute`]（HR-34）。
//!
//! # 本卡为什么不写业务编排
//!
//! 「建单」= 引擎的一次 `dry_run = false` 计算，事务边界、恒等式校验、单号取号、状态流转
//! 全在 HR-34 的 [`salary_compute::calc`] 里。本文件**只做三件事**：
//!
//! 1. 把 HTTP 入参解析成 `CalcRequest`（形状 / 域值 / `dry_run` 与接口身份一致）；
//! 2. 调引擎并把 `CalcOutcome` 摊平成回执；
//! 3. 把落库后的东西读出来（列表 / 单头汇总 / 明细分页）。
//!
//! 在 handler 里再算一遍工资，就是「两套公式」——改动一侧漏另一侧，是 HR 模块最贵的 bug。
//!
//! # #37 「单头汇总」为什么不内嵌明细分页
//!
//! 设计 §5.2 #37 的描述是「单头 + 部门汇总 + 明细分页」，但 #38 已经是一条独立的分页接口。
//! 一页 200 人的明细塞进详情响应里，前端要么不用（白传），要么两套分页状态互相打架。
//! 所以本卡把 #37 收成**纯汇总**（单头全列 + 部门汇总 + 三口径对账），明细分页留给 #38，
//! 与设计 §5.3「列表走唯一分页入口」同一取向。
//!
//! # 汇总与明细一致（本卡验收项）
//!
//! [`summary_of`] 每次读都做三方对拍：单头 7 个金额列 vs `SUM(明细)` vs `SUM(明细项按类型)`。
//! 不一致**不报 5xx**：这是**诊断信息**（界面要能打开单据看「哪几个字段漂了」，
//! 而不是整个详情页 500），所以回执里带 `consistent=false` + 逐字段 `mismatches[]`。
//! 但**测试**里必须为 `true` —— 建单后立刻读汇总，三份口径必须一分不差。
//!
//! 为什么不能只靠库层 `CHECK`：`ck_biz_sal_sheet_*` 保证单头内部自洽、`ck_biz_sal_det_*`
//! 保证明细行内部自洽，**跨表**（明细加起来是否等于单头）没有任何约束能管 —— 那是设计
//! §4.9 的巡检 A6/A7，只能在读时（本接口）与离线（HR-45 脚本）各守一道。
//!
//! # 错误出口
//!
//! 只有 `ResResult<T>`：handler 内 `DbError` 直接 `?`（`utils::db_error` 只在要换文案时用），
//! 业务态冲突复用 `HR_SALARY_RULE_BLOCKED(68008)` —— 本卡不新增错误码（加码要动注册表与
//! `docs/`，不在本卡范围）。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::{self, ApprovalGate};
use crate::business::flow::{engine, model as flow_model};
use crate::business::hr::salary_bridge;
use crate::business::hr::salary_compute::{self as compute, CalcRequest};
use crate::business::hr::salary_override_dto::{self as ov, SalaryOverrideReq, SalaryOverrideRes};
use crate::business::hr::salary_sheet_dto::{
    self as dto, CalcMode, SalaryCalcRes, SalaryDetailPageQuery, SalaryDetailRes, SalarySheetCalcReq,
    SalarySheetCardRes, SalarySheetListQuery, SalarySheetSummaryRes, SalaryTotalsRes,
};
use crate::business::hr::salary_sheet_model::{self as model};
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::shared::state_machine::{self, ENTITY_SALARY_SHEET, Operator};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 sys_menu.auth_code 逐字一致） ====================

/// 薪资单**全量列表接口**（`GET /business/hr/salary/page`）。
///
/// ⚠ 自 `V0401`（卡 t_0af7ec46）起，这个码**只**属于接口：它挂在 `sys_menu` 的按钮行
/// （`menu_type=3`，`…e064`），**不再**同时是「薪资管理」页面行的码 ——
/// 页面行改挂 `business:hr:salary:page`。两者分开授予，员工自助拿页面码不会顺带拿到全量列表。
///
/// 页面码在后端源码里**一个字符串都不许出现**（它是纯前端判据，登记在
/// `permission_audit_tests::ROUTE_LEVEL_ONLY`，N18 会检查「菜单里有、源码里 0 引用」）。
pub const PERM_SALARY_LIST: &str = "business:hr:salary:list";

/// 薪资单详情 / 明细分页（设计 §5.2 #37 / #38 共用这一个码）。
pub const PERM_SALARY_DETAIL: &str = "business:hr:salary:detail";

/// 试算 / 建单 / 明细覆盖（设计 §5.2 #39 / #40 共用）。
pub const PERM_SALARY_CALC: &str = "business:hr:salary:calc";

/// 提交审批（设计 §5.2 #42，HR-39）。
pub const PERM_SALARY_SUBMIT: &str = "business:hr:salary:submit";

/// 过账（设计 §5.2 #43，HR-39）。
pub const PERM_SALARY_POST: &str = "business:hr:salary:post";

/// 红冲（设计 §5.2 #44，HR-40）。
pub const PERM_SALARY_REVERSE: &str = "business:hr:salary:reverse";

// ==================== 错误原语 ====================

/// `404 + 10002`：单不存在（或已软删）。
///
/// 报文里带 id：前端「刷新后仍打不开」时，用户报的 id 能直接被运维对上。
fn not_found(sheet_id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("薪资单 {sheet_id} 不存在（可能已删除，或 id 抄错）"),
    )
}

/// `404 + 10002`：明细行不存在（或不属于这张单）。
///
/// 报文同时点名单号与明细 id：前端「刷新后仍打不开」时，用户报的两个 id 能直接被运维对上。
fn detail_not_found(sheet_no: &str, detail_id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("薪资单 {sheet_no} 里没有明细 {detail_id}（可能单号与明细不匹配，或明细已被重算覆盖）"),
    )
}

/// `500 + 10004`：引擎退化（落库路径没带回单头）。
///
/// 这条理论上不可达（`dry_run=false` 时引擎必回 `Some(sheet)`）。之所以写成显式 500 而不是
/// `unwrap`：真出现时用户看到的是「建单没拿到单号，请重试」，而不是进程 panic 的 502。
fn lost_sheet() -> ApiError {
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        "建单已执行但没拿到单头信息：请刷新列表确认是否已生成，未生成则重试",
    )
}

// ==================== ㊱ 列表 ====================

/// 薪资单列表（设计 §5.2 #36）。
///
/// # 闸只有一道：`business:hr:salary:list`（卡 t_0af7ec46 解耦后）
///
/// HR-47 那轮（t_be54535c）在这里加过第二道数据闸 `ensure_full_list`（要 `salary:detail`），
/// 因为「薪资管理」**页面行**的 `auth_code` 就是 `salary:list` ⇒ 员工自助为了让页面显形，
/// 顺带持有了全量列表码。`V0401` 把页面行改成 `business:hr:salary:page` 之后，
/// 页面显形与全量接口彻底分开：`EMPLOYEE` 手上只有 `salary:page` + `salary:my`，
/// 走到本接口时**框架路由闸**就会用 `salary:list` 判它 403。
///
/// 所以人肉闸已删除（不再需要「每处记得写第二道闸」），保留的只有
/// `#[nik::permission(permission = "business:hr:salary:list")]` 这一道。
/// 判据与回归：`scripts/verify/hr/hr_permission_matrix.sh` 的 `:list` 行（员工 403 / 财务 200）。
#[nik::permission(
    path = "/business/hr/salary/page",
    method = "GET",
    permission = "business:hr:salary:list"
)]
#[nik::get("/business/hr/salary/page")]
pub async fn salary_page(
    Query(query): Query<SalarySheetListQuery>,
) -> ResResult<PageData<Vec<dto::SalarySheetCardRes>>> {
    let (list, total, page, page_size) = list_sheets(&query).await?;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 列表的业务实现（接口与真库用例共用同一条路径）。
pub async fn list_sheets(
    query: &SalarySheetListQuery,
) -> std::result::Result<(Vec<dto::SalarySheetCardRes>, i64, u64, u64), ApiError> {
    let (page, page_size) = dto::normalize_page(query.page, query.page_size);
    let filter = dto::parse_list_query(query)?;
    let (rows, total) = model::page_sheets(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询薪资单"))?;
    Ok((
        rows.iter().map(model::SheetRow::to_card).collect(),
        total,
        page,
        page_size,
    ))
}

// ==================== ㊲ 单头汇总 ====================

/// 单头汇总（设计 §5.2 #37：单头 + 部门汇总 + 三口径对账）。
#[nik::permission(
    path = "/business/hr/salary/{id}",
    method = "GET",
    permission = "business:hr:salary:detail"
)]
#[nik::get("/business/hr/salary/{id}")]
pub async fn salary_summary(Path(id): Path<String>) -> ResResult<SalarySheetSummaryRes> {
    let sheet_id = dto::parse_uuid(&id, "id")?;
    Ok(Res::ok(summary_of(sheet_id).await?))
}

/// 汇总的业务实现。
///
/// 三次读（明细合计 / 明细项合计 / 部门汇总）+ 一次单头读，全部按 `sheet_id` 走索引
/// （`uk_biz_sal_det` 的前缀、`idx_biz_sal_ditem_sheet`、`idx_biz_sal_det_dept`）。
pub async fn summary_of(sheet_id: Uuid) -> std::result::Result<SalarySheetSummaryRes, ApiError> {
    let sheet = model::find_sheet(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "查询薪资单"))?
        .ok_or_else(|| not_found(sheet_id))?;

    let detail_calc = model::totals_of_details(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总薪资明细"))?;
    let item_totals = model::item_totals(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总薪资明细项"))?;
    let dept_summary = model::dept_summary(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总薪资单的部门合计"))?;

    let header_totals = sheet.totals();
    let detail_totals = SalaryTotalsRes::of_details(&detail_calc);
    let mut mismatches = dto::diff_totals(&header_totals, &detail_totals);
    mismatches.extend(dto::diff_item_totals(&detail_totals, &item_totals));

    Ok(SalarySheetSummaryRes {
        sheet: sheet.to_header(),
        detail_totals,
        item_totals,
        consistent: mismatches.is_empty(),
        mismatches,
        dept_summary,
    })
}

// ==================== ㊳ 明细分页 ====================

/// 明细分页（设计 §5.2 #38）。
#[nik::permission(
    path = "/business/hr/salary/{id}/detail/page",
    method = "GET",
    permission = "business:hr:salary:detail"
)]
#[nik::get("/business/hr/salary/{id}/detail/page")]
pub async fn salary_detail_page(
    Path(id): Path<String>,
    Query(query): Query<SalaryDetailPageQuery>,
) -> ResResult<PageData<Vec<SalaryDetailRes>>> {
    let sheet_id = dto::parse_uuid(&id, "id")?;
    let (list, total, page, page_size) = detail_page_of(sheet_id, &query).await?;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 明细分页的业务实现（接口与真库用例共用同一条路径）。
///
/// 单头不存在 → 404，**不给空页**：空页会让用户以为「这单一个人都没有」，
/// 而真实原因是 id 错了（或者单被软删了）。
pub async fn detail_page_of(
    sheet_id: Uuid,
    query: &SalaryDetailPageQuery,
) -> std::result::Result<(Vec<SalaryDetailRes>, i64, u64, u64), ApiError> {
    model::find_sheet(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "查询薪资单"))?
        .ok_or_else(|| not_found(sheet_id))?;

    let filter = dto::parse_detail_query(sheet_id, query)?;
    let (page, page_size) = dto::normalize_page(query.page, query.page_size);
    let (rows, total) = model::page_details(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询薪资明细"))?;

    // 明细项按本页的 detail_id 一次查完（不发 N 次）
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let grouped = if filter.with_items {
        model::items_of_details(sheet_id, &ids)
            .await
            .map_err(|error| utils::db_error(error, "查询薪资明细项"))?
    } else {
        std::collections::HashMap::new()
    };

    let list = rows
        .iter()
        .map(|row| {
            let items = grouped
                .get(&row.id)
                .map(|items| items.iter().map(model::DetailItemRow::to_res).collect())
                .unwrap_or_default();
            row.to_res(items)
        })
        .collect();
    Ok((list, total, page, page_size))
}

// ==================== ㊴ 试算 ====================

/// 试算（设计 §5.2 #39：算完回滚，库里零行、号段不推进）。
#[nik::permission(
    path = "/business/hr/salary/calc",
    method = "POST",
    permission = "business:hr:salary:calc"
)]
#[nik::post("/business/hr/salary/calc")]
pub async fn salary_calc(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SalarySheetCalcReq>,
) -> ResResult<SalaryCalcRes> {
    let operator: Operator = (&claims).into();
    Ok(Res::ok(preview(operator, &req).await?))
}

/// 试算的业务实现（接口与真库用例共用同一条路径）。
pub async fn preview(operator: Operator, req: &SalarySheetCalcReq) -> std::result::Result<SalaryCalcRes, ApiError> {
    let request = dto::parse_calc_req(req, CalcMode::Preview)?;
    let outcome = compute::calc(operator, &request).await?;
    // 试算回执带出单头就是 bug（说明引擎没回滚或调用方传错了 dry_run）
    if outcome.sheet.is_some() {
        return Err(lost_sheet());
    }
    Ok(SalaryCalcRes::of(&outcome))
}

// ==================== ㊵ 建单 ====================

/// 建单并落库（设计 §5.2 #40：同一次计算的 `dry_run = false`）。
#[nik::permission(
    path = "/business/hr/salary",
    method = "POST",
    permission = "business:hr:salary:calc"
)]
#[nik::post("/business/hr/salary")]
pub async fn salary_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SalarySheetCalcReq>,
) -> ResResult<SalaryCalcRes> {
    let operator: Operator = (&claims).into();
    Ok(Res::ok(persist(operator, &req).await?))
}

/// 建单的业务实现（接口与真库用例共用同一条路径）。
///
/// 事务边界、单号、状态流转、恒等式校验全部在 [`compute::calc`] 里（HR-34）；
/// 这里只保证「落库路径必须回单头」。
pub async fn persist(operator: Operator, req: &SalarySheetCalcReq) -> std::result::Result<SalaryCalcRes, ApiError> {
    let request: CalcRequest = dto::parse_calc_req(req, CalcMode::Persist)?;
    let outcome = compute::calc(operator, &request).await?;
    if outcome.sheet.is_none() {
        return Err(lost_sheet());
    }
    Ok(SalaryCalcRes::of(&outcome))
}

// ==================== ㊶ 明细覆盖 ====================

/// 明细覆盖（设计 §5.2 #41：改项金额 + `reason`）—— HR-37。
///
/// 权限码 `business:hr:salary:calc`（与试算 / 建单同一个码：能算工资的人才能改算出来的数）。
/// 另外还有一道**数据范围**闸：见 [`override_detail`]。
#[nik::permission(
    path = "/business/hr/salary/{id}/detail/{detail_id}",
    method = "PUT",
    permission = "business:hr:salary:calc"
)]
#[nik::put("/business/hr/salary/{id}/detail/{detail_id}")]
pub async fn salary_detail_override(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, detail_id)): Path<(String, String)>,
    Json(req): Json<SalaryOverrideReq>,
) -> ResResult<SalaryOverrideRes> {
    let sheet_id = dto::parse_uuid(&id, "id")?;
    let target = dto::parse_uuid(&detail_id, "detail_id")?;
    let operator: Operator = (&claims).into();
    // 数据范围必须按**令牌里的用户**解析（业务代码不得用请求体里的 user_id 代替）。
    let principal = principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(
        override_detail(operator, &principal, sheet_id, target, &req).await?,
    ))
}

/// 明细覆盖的业务实现（接口与真库用例共用同一条路径）。
///
/// # 事务里的顺序（每一步都有理由）
///
/// 1. `parse` 入参：形状错不该占数据库连接；
/// 2. **锁单头**（`FOR UPDATE`）：设计 §4.8.3 的并发口径 —— 后到的等锁，
///    避免「读明细 → 算汇总 → 写汇总」之间被人插一笔（丢失更新）；
/// 3. **锁明细行**：同一行的两项被两个人同时改时，后改的人看到的是前一个人的结果；
/// 4. **越权闸**（[`model::detail_in_scope`]）：设计 D-12 —— 部门 / 本人范围之外的行
///    403（不是 404：行是存在的，是你不该动它，报文要能自查）；**超管短路**；
/// 5. **只读闸**（`state_machine::ensure_writable`）：`SUBMITTED/APPROVED/POSTED/REVERSED`
///    四态冻结（`SALARY_SHEET_LOCKED`）→ 409 并说明怎么更正；
/// 6. 覆盖金额 → **重算明细 11 列** → 写明细项 → 写明细行 → **重算单头 7 列 + 人数**；
/// 7. 写 `biz_status_log`（`action = AMOUNT_OVERRIDE`，`from = to = 当前状态`，
///    `reason` 里逐项写「旧值→新值 + 差额」）—— **与业务改动同事务**：
///    日志写不进去就整笔回滚，绝不允许「钱改了但没有痕」。
///
/// # 为什么越权闸在只读闸之前
///
/// 反过来的话，一个只能看本部门的 HR 对别的部门的已过账单会拿到 409「已入账不可改」——
/// 那是在告诉他不存在的选择。先 403 才是真话：「这行本来就不归你」。
///
/// # 幂等
///
/// 传入金额与库里一致 → `changed = false`、不写状态日志、不回 4xx（网络重试不该报错）。
pub async fn override_detail(
    operator: Operator,
    principal: &ScopedPrincipal,
    sheet_id: Uuid,
    detail_id: Uuid,
    req: &SalaryOverrideReq,
) -> std::result::Result<SalaryOverrideRes, ApiError> {
    let input = ov::parse_override_req(req)?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启薪资明细覆盖事务"))?;

    // ② 锁单头
    let sheet = match model::lock_sheet(&tx, sheet_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found(sheet_id));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定薪资单"));
        }
    };

    // ③ 锁明细行
    let detail = match model::lock_detail(&tx, sheet_id, detail_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(detail_not_found(&sheet.sheet_no, detail_id));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定薪资明细"));
        }
    };

    // ④ 越权闸
    let who = format!("{}（{}）", detail.employee_name, detail.employee_no);
    match model::detail_in_scope(&tx, sheet_id, detail_id, principal).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.rollback().await;
            return Err(ov::forbidden(&sheet.sheet_no, &who));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "校验薪资明细的数据范围"));
        }
    }

    // ⑤ 只读闸
    if let Err(error) = state_machine::ensure_writable(ENTITY_SALARY_SHEET, &sheet.status, &sheet.sheet_no) {
        let _ = tx.rollback().await;
        return Err(error);
    }

    // ⑥ 覆盖 → 重算 → 写回
    let rows = match model::items_for_override(&tx, sheet_id, detail_id).await {
        Ok(rows) => rows,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读取薪资明细项"));
        }
    };
    let current: Vec<ov::ItemAmount> = rows.iter().map(model::OverrideItemRow::to_amount).collect();
    let (next, changes) = match ov::override_amounts(&current, &input, &who) {
        Ok(pair) => pair,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    // 重算汇总（负数工资 / 归不了桶的单位项在这里就被拦下，不落到库层 23514）
    let amounts = match ov::summarize(&next, &who) {
        Ok(amounts) => amounts,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };

    let mut log_id: Option<Uuid> = None;
    if !changes.is_empty() {
        // 明细项：金额 + 「人工覆盖 原→新」说明
        for change in &changes {
            let Some(row) = rows.iter().find(|row| row.item_code == change.item_code) else {
                let _ = tx.rollback().await;
                return Err(ov::rule_blocked(format!(
                    "{} 的明细项 {} 在写回时消失了（并发改动），请刷新后重试",
                    who, change.item_code
                )));
            };
            let note = ov::override_note(change, &input.reason);
            if let Err(error) = model::update_item_amount(&tx, row.id, change.new_amount, &note).await {
                let _ = tx.rollback().await;
                return Err(utils::db_error(error, "写回被覆盖的薪资项金额"));
            }
        }
        if let Err(error) = model::update_detail_amounts(&tx, detail_id, &amounts, input.remark.as_deref()).await {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "重写薪资明细的汇总金额"));
        }
        if let Err(error) = model::recalc_sheet_totals(&tx, sheet_id).await {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "重算薪资单的单头合计"));
        }
        let reason = ov::log_reason(&who, &changes, &input.reason);
        let biz_ref = (model::TABLE_DETAIL, detail.employee_no.as_str());
        match state_machine::log_event(
            &tx,
            ENTITY_SALARY_SHEET,
            sheet_id,
            &sheet.sheet_no,
            &sheet.status,
            Some(ov::ACTION_AMOUNT_OVERRIDE),
            operator,
            Some(&reason),
            Some(biz_ref),
        )
        .await
        {
            Ok(id) => log_id = Some(id),
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        }
    }

    // ⑦ 事务内读回（单头合计已重算；明细行金额已重写）
    let fresh_sheet = match model::find_sheet_in(&tx, sheet_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found(sheet_id));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回薪资单"));
        }
    };
    let fresh_detail = match model::find_detail_in(&tx, sheet_id, detail_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(detail_not_found(&sheet.sheet_no, detail_id));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回薪资明细"));
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交薪资明细覆盖事务"))?;

    // ⑧ 回执：覆盖后的明细项（金额/说明用新值）+ 明细行 11 列 + 单头 7 列
    let overridden: std::collections::HashMap<&str, &ov::AmountChange> = changes
        .iter()
        .map(|change| (change.item_code.as_str(), change))
        .collect();
    let lines = rows
        .iter()
        .map(|row| match overridden.get(row.item_code.as_str()) {
            Some(change) => row.to_line(change.new_amount, &ov::override_note(change, &input.reason)),
            None => row.to_line(row.amount, &row.calc_note),
        })
        .collect();

    Ok(SalaryOverrideRes {
        changed: !changes.is_empty(),
        sheet_id: fresh_sheet.id.to_string(),
        sheet_no: fresh_sheet.sheet_no.clone(),
        sheet_status: fresh_sheet.status.clone(),
        sheet_status_label: dto::status_label(&fresh_sheet.status),
        detail_id: fresh_detail.id.to_string(),
        employee_no: fresh_detail.employee_no.clone(),
        employee_name: fresh_detail.employee_name.clone(),
        changes: changes.iter().map(ov::SalaryAmountChangeRes::of).collect(),
        detail: fresh_detail.to_res(lines),
        sheet: fresh_sheet.to_header(),
        log_id: log_id.map(|id| id.to_string()),
        reason: input.reason,
    })
}

// ==================== ㊷ 提交审批 ====================

/// 提交审批（设计 §5.2 #42 `hr_salary`）—— HR-39。
///
/// 权限码 `business:hr:salary:submit`（与 V0387 的 `e043` 按钮逐字一致）。
#[nik::permission(
    path = "/business/hr/salary/{id}/submit",
    method = "POST",
    permission = "business:hr:salary:submit"
)]
#[nik::post("/business/hr/salary/{id}/submit")]
pub async fn salary_submit(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<SalarySheetCardRes> {
    let sheet_id = dto::parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(submit_sheet(operator, sheet_id).await?))
}

/// 提交审批的业务实现（接口与真库用例共用同一条路径）。
///
/// # 闸门顺序（全部在同一个事务里）
///
/// 1. `status = CALCULATED`（`DRAFT` 得先试算，`SUBMITTED` 已在审批中，
///    `APPROVED/POSTED/REVERSED` 不能再提交）；
/// 2. 同一单已有在跑的审批 → 409（避免撞 `uk_biz_flow_instance_biz_active`）；
/// 3. 没有已发布的 `hr_salary` 流程 → `approval_gate::authorize_missing_flow`
///    （409 点名流程编码）。**不做「没流程就直接生效」**（设计 §4.3 第 3 条）；
/// 4. 起实例 → 状态机 `CALCULATED → SUBMITTED` → CAS 回写 `status` + `submitted_at`
///    + `approval_instance_id`（一条语句，不留中间窗口）；
/// 5. 免审直过（流程里没有审批节点）：引擎在同事务里直接 `APPROVED`，本函数立刻把单据
///    推到 `APPROVED` —— **绝不留「实例已通过、单据还在审批中且无待办」的死单**
///    （`AGENTS.md` 约定 11）。
///
/// # 表单快照里的 `gross_total_num` 是**条件边的契约**
///
/// `hr_salary` 的「应发 ≥ 50 万 → 总经理审批」是一条**条件边**
/// （`V0394` 的 `condition_expr = {"field": "form.gross_total_num", "cmp": "gte", "value": 500000}`），
/// 条件求值器对 `gte` 走 `Value::as_f64()`：左值是 JSON **字符串**就 `TypeMismatch`，
/// 引擎按「条件不成立」**静默走兜底边** —— 大额工资单会绕过总经理签字，且不报任何错。
/// 所以这里冻结**两个**字段：`gross_total`（字符串，展示口径）+ `gross_total_num`
/// （JSON number，供求值）。两边的钉子：本文件的单测 + `tests/hr_38_*` 的种子断言。
pub async fn submit_sheet(operator: Operator, sheet_id: Uuid) -> std::result::Result<SalarySheetCardRes, ApiError> {
    let Some(operator_id) = operator.id else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启提交薪资审批事务"))?;
    // ① 锁单 + 状态闸
    let sheet = match model::lock_sheet(&tx, sheet_id).await {
        Ok(Some(sheet)) => sheet,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found(sheet_id));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定薪资单"));
        }
    };
    if sheet.status != dto::STATUS_CALCULATED {
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            409,
            error_code::HR_SALARY_RULE_BLOCKED,
            format!(
                "薪资单 {} 当前是「{}」（{}），只有已算出（CALCULATED）的单可以提交审批{}",
                sheet.sheet_no,
                dto::status_label(&sheet.status),
                sheet.status,
                match sheet.status.as_str() {
                    dto::STATUS_DRAFT => "：请先试算（建单）",
                    dto::STATUS_SUBMITTED => "：这张单已经在审批中了",
                    dto::STATUS_APPROVED => "：已审批通过，可以直接过账",
                    dto::STATUS_POSTED => "：已过账（凭证已生成）",
                    _ => "：已红冲的单是终态，要重发请新开一张",
                }
            ),
        ));
    }
    // ② 幂等：已有在跑实例
    match flow_model::biz_instance_running_in(&tx, salary_bridge::BIZ_TYPE, sheet_id).await {
        Ok(false) => {}
        Ok(true) => {
            let _ = tx.rollback().await;
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!(
                    "薪资单 {} 已在审批中：请等审批结束，或让审批人驳回后再提交",
                    sheet.sheet_no
                ),
            ));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询薪资单在跑的审批"));
        }
    }
    // ③ 流程必须已发布（fail-closed）
    let flow = match published_salary_flow().await {
        Ok(flow) => flow,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询薪资审批流程"));
        }
    };
    let Some(flow) = flow else {
        let _ = tx.rollback().await;
        return Err(approval_gate::authorize_missing_flow(
            salary_bridge::BIZ_TYPE,
            salary_bridge::FLOW_CODE,
            &ApprovalGate::none(),
        )
        .expect_err("没有已发布流程时 authorize_missing_flow 必须返回错误（fail-closed）"));
    };
    let (initiator_name, initiator_dept_id) = match flow_model::user_profile(operator_id).await {
        Ok(profile) => profile.unwrap_or((String::new(), None)),
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询提交人"));
        }
    };
    // ④ 起实例
    let instance_id = Uuid::now_v7();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(sheet_id),
        biz_no: Some(sheet.sheet_no.clone()),
        title: format!(
            "{} {} 薪资单（{} 人 / 应发 {} 元）",
            sheet.sheet_no, sheet.period_code, sheet.employee_count, sheet.gross_total
        )
        .chars()
        .take(200)
        .collect::<String>(),
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: salary_form_data(&sheet),
    };
    let mut out = match engine::start(&tx, &start).await {
        Ok(out) => out,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(model::write_error(error, "发起薪资审批单", &sheet.sheet_no));
        }
    };
    if let Some(rejected) = out.rejected.take() {
        let _ = tx.rollback().await;
        let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
            "（内控要求：审批人不能是发起人本人。请让其他人事 / 财务账号提交）"
        } else {
            ""
        };
        return Err(rejected.appended(hint));
    }
    let started = match out.instance_id {
        Some(started) => started,
        None => {
            let _ = tx.rollback().await;
            return Err(ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "审批单发起后没有实例 id",
            ));
        }
    };
    // ⑤ 状态机（唯一入口：边校验 + 写 biz_status_log）
    if let Err(error) = state_machine::transition(
        &tx,
        ENTITY_SALARY_SHEET,
        sheet_id,
        &sheet.sheet_no,
        &state_machine::SALARY_SHEET_EDGES,
        dto::STATUS_CALCULATED,
        dto::STATUS_SUBMITTED,
        operator.clone(),
        Some("提交审批（薪资单）"),
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    // ⑥ CAS 回写（状态 + submitted_at/by + 实例 id 一条语句）
    match model::set_submitted(
        &tx,
        sheet_id,
        dto::STATUS_CALCULATED,
        dto::STATUS_SUBMITTED,
        started,
        operator.id,
    )
    .await
    {
        Ok(1) => {}
        Ok(_) => {
            let _ = tx.rollback().await;
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("薪资单 {} 的状态在提交期间已被改动，请刷新后重试", sheet.sheet_no),
            ));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(model::write_error(error, "回写薪资单提交结果", &sheet.sheet_no));
        }
    }
    // ⑦ 免审直过：实例已在同事务里 APPROVED → 单据必须同步推到 APPROVED
    let auto_finished = out.instance_status.eq_ignore_ascii_case(dto::STATUS_APPROVED);
    if auto_finished {
        if let Err(error) = state_machine::transition(
            &tx,
            ENTITY_SALARY_SHEET,
            sheet_id,
            &sheet.sheet_no,
            &state_machine::SALARY_SHEET_EDGES,
            dto::STATUS_SUBMITTED,
            dto::STATUS_APPROVED,
            Operator::named(Some(operator_id), operator.name.clone().unwrap_or_default()),
            Some("审批免点（流程未配置审批节点）：同事务直接通过"),
            None,
        )
        .await
        {
            let _ = tx.rollback().await;
            return Err(error);
        }
        if let Err(error) =
            model::set_status(&tx, sheet_id, dto::STATUS_SUBMITTED, dto::STATUS_APPROVED, operator.id).await
        {
            let _ = tx.rollback().await;
            return Err(model::write_error(error, "回写薪资单免审通过", &sheet.sheet_no));
        }
    }
    let updated = match model::find_sheet_in(&tx, sheet_id).await {
        Ok(Some(updated)) => updated,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found(sheet_id));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回提交后的薪资单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交薪资审批事务"))?;
    // ⑧ 锁外副作用（审批通知 / 客户自配的通知节点）：提交之后才发，失败只 WARN
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    tracing::info!(
        sheet_id = %sheet_id,
        sheet_no = %sheet.sheet_no,
        instance_id = %started,
        auto_finished,
        gross_total = %sheet.gross_total,
        "薪资单已提交审批"
    );
    Ok(updated.to_card())
}

/// 发起薪资审批时冻结的表单变量（`biz_flow_instance.form_data`）。
///
/// `gross_total` 是**字符串**（与出参口径一致，前端直接显示），
/// `gross_total_num` 是**同值同源的 JSON number**（条件边求值用，见 [`submit_sheet`] 的文档）。
fn salary_form_data(sheet: &model::SheetRow) -> serde_json::Value {
    let mut form = serde_json::json!({
        "sheet_id": sheet.id.to_string(),
        "sheet_no": sheet.sheet_no,
        "period_code": sheet.period_code,
        "period_start": sheet.period_start.to_string(),
        "period_end": sheet.period_end.to_string(),
        "employee_count": sheet.employee_count,
        "earning_total": sheet.earning_total.to_string(),
        "pre_deduct_total": sheet.pre_deduct_total.to_string(),
        "gross_total": sheet.gross_total.to_string(),
        "withhold_total": sheet.withhold_total.to_string(),
        "net_total": sheet.net_total.to_string(),
        "si_company_total": sheet.si_company_total.to_string(),
        "hf_company_total": sheet.hf_company_total.to_string(),
        "accrual_total": (sheet.gross_total + sheet.si_company_total + sheet.hf_company_total).to_string(),
        "remark": sheet.remark,
    });
    if let Ok(value) = sheet.gross_total.to_string().parse::<f64>()
        && let Some(number) = serde_json::Number::from_f64(value)
    {
        form["gross_total_num"] = serde_json::Value::Number(number);
    }
    form
}

/// 已发布且默认的 `hr_salary` 流程（`biz_type = HR_SALARY` 或 `flow_code = hr_salary`）。
async fn published_salary_flow() -> DbResult<Option<SalaryFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = ? OR lower(d.flow_code) = ?) \
              ORDER BY (d.biz_type = ?) DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![
                Value::Text(salary_bridge::BIZ_TYPE.to_string()),
                Value::Text(salary_bridge::FLOW_CODE.to_string()),
                Value::Text(salary_bridge::BIZ_TYPE.to_string()),
            ],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(SalaryFlow {
        def_id: row.get_typed("def_id")?,
        flow_code: row.get_typed("flow_code")?,
        flow_name: row.get_typed("flow_name")?,
        biz_type: row.get_typed("biz_type")?,
        version_id: row.get_typed("version_id")?,
        version_no: row.get_typed("version_no")?,
    }))
}

/// 已发布的薪资流程（`published_salary_flow` 的投影）。
struct SalaryFlow {
    def_id: Uuid,
    flow_code: String,
    flow_name: String,
    biz_type: String,
    version_id: Uuid,
    version_no: i32,
}

// ==================== ㊸ 过账 ====================

/// 过账（设计 §5.2 #43：生成计提 + 发放两张凭证）—— HR-39。
///
/// 权限码 `business:hr:salary:post`（与 V0387 的 `e044` 按钮逐字一致）。
/// 业务实现全在 [`salary_bridge::post_salary`]（事务 + 净额法 + 两张凭证 + 状态流转）。
#[nik::permission(
    path = "/business/hr/salary/{id}/post",
    method = "POST",
    permission = "business:hr:salary:post"
)]
#[nik::post("/business/hr/salary/{id}/post")]
pub async fn salary_post(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<SalarySheetCardRes> {
    let sheet_id = dto::parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(salary_bridge::post_salary(operator, sheet_id).await?))
}

/// 红冲（设计 §5.2 #44 `POST /business/hr/salary/{id}/reverse`）—— HR-40。
///
/// 权限码 `business:hr:salary:reverse`（与 V0387 的 `e045` 按钮逐字一致；`hr_04` 的
/// 权限矩阵把这一条**刻意不授给 FINANCE_MANAGER**——已入账的钱倒回去必须 HR + 财务双签）。
///
/// # 两道校验的分工
///
/// * [`dto::parse_reverse_reason`]：**原因必填**（trim 后 ≥5 字、≤255 字）—— 本卡验收项，
///   在开事务之前就拦住（报文点名 `reason`）；
/// * [`salary_bridge::reverse_salary`]：状态机 / 两张凭证 / 事务边界 —— 全在 bridge 里。
///
/// handler 不写业务编排（与 #39~#43 同一取向）。
#[nik::permission(
    path = "/business/hr/salary/{id}/reverse",
    method = "POST",
    permission = "business:hr:salary:reverse"
)]
#[nik::post("/business/hr/salary/{id}/reverse")]
pub async fn salary_reverse(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::SalaryReverseReq>,
) -> ResResult<SalarySheetCardRes> {
    let sheet_id = dto::parse_uuid(&id, "id")?;
    let reason = dto::parse_reverse_reason(&req)?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(
        salary_bridge::reverse_salary(operator, sheet_id, &reason).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::handler::ROUTE_CONTRACT;

    /// 本卡落地的五条路由必须在 `ROUTE_CONTRACT`（设计 §5.2 的冻结表）里逐字存在，
    /// 且权限码与声明一致 —— 路径 / 权限码写错的直接后果是「菜单给了、点进去 403/404」。
    #[test]
    fn routes_and_permissions_match_the_frozen_contract() {
        let expected = [
            ("GET", "/business/hr/salary/page", PERM_SALARY_LIST),
            ("GET", "/business/hr/salary/{id}", PERM_SALARY_DETAIL),
            ("GET", "/business/hr/salary/{id}/detail/page", PERM_SALARY_DETAIL),
            ("POST", "/business/hr/salary/calc", PERM_SALARY_CALC),
            ("POST", "/business/hr/salary", PERM_SALARY_CALC),
            // HR-37：明细覆盖（改单项 → 重算汇总 → 写状态日志）
            ("PUT", "/business/hr/salary/{id}/detail/{detail_id}", PERM_SALARY_CALC),
            // HR-39：提交审批 / 过账（两张凭证）
            ("POST", "/business/hr/salary/{id}/submit", PERM_SALARY_SUBMIT),
            ("POST", "/business/hr/salary/{id}/post", PERM_SALARY_POST),
            // HR-40：红冲（两张红字凭证）
            ("POST", "/business/hr/salary/{id}/reverse", PERM_SALARY_REVERSE),
        ];
        for (method, path, permission) in expected {
            let spec = ROUTE_CONTRACT
                .iter()
                .find(|spec| spec.method == method && spec.path == path)
                .unwrap_or_else(|| panic!("契约表里没有 {method} {path}"));
            assert_eq!(
                spec.permission,
                Some(permission),
                "{method} {path} 的权限码与契约表不一致"
            );
        }
    }

    /// 明细覆盖的契约行必须挂在 HR-37（改卡号会让「谁该实现它」在板上失联）。
    #[test]
    fn override_route_belongs_to_hr37() {
        let spec = ROUTE_CONTRACT
            .iter()
            .find(|spec| spec.method == "PUT" && spec.path == "/business/hr/salary/{id}/detail/{detail_id}")
            .expect("契约表缺明细覆盖");
        assert_eq!(spec.card, "HR-37");
        assert_eq!(spec.permission, Some(PERM_SALARY_CALC));
    }

    /// 冻结的四个只读态一律 409（覆盖不许改已提交 / 已审批 / 已入账 / 已红冲的单）。
    ///
    /// 这条闸来自 `state_machine::ensure_writable` —— 它按 `Entity::SalarySheet` 的
    /// `SALARY_SHEET_LOCKED` 判定，所以「哪些状态能动」只有一处定义。
    #[test]
    fn override_is_rejected_on_locked_statuses() {
        for status in ["SUBMITTED", "APPROVED", "POSTED", "REVERSED"] {
            let error = state_machine::ensure_writable(ENTITY_SALARY_SHEET, status, "SAL-202701-000001")
                .expect_err("冻结态必须拒");
            assert_eq!(
                (error.status, error.code),
                (409, error_code::RESOURCE_CONFLICT),
                "{status} 必须 409"
            );
            assert!(error.msg.contains("SAL-202701-000001"), "报文点名单号：{}", error.msg);
        }
        for status in ["DRAFT", "CALCULATED"] {
            state_machine::ensure_writable(ENTITY_SALARY_SHEET, status, "SAL-202701-000001")
                .unwrap_or_else(|error| panic!("{status} 是草稿态，必须放行：{}", error.msg));
        }
    }

    /// 明细不存在的 404 报文同时点名单号与明细 id。
    #[test]
    fn detail_not_found_names_sheet_and_detail() {
        let detail_id = Uuid::now_v7();
        let error = detail_not_found("SAL-202701-000001", detail_id);
        assert_eq!((error.status, error.code), (404, error_code::RESOURCE_NOT_FOUND));
        assert!(error.msg.contains("SAL-202701-000001"), "报文：{}", error.msg);
        assert!(error.msg.contains(&detail_id.to_string()), "报文：{}", error.msg);
    }

    /// 三条权限码必须与 `V0387` 种进 `sys_menu.auth_code` 的字符串逐字一致。
    #[test]
    fn permission_codes_exist_in_the_menu_seed() {
        const V0387: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");
        for code in [
            PERM_SALARY_LIST,
            PERM_SALARY_DETAIL,
            PERM_SALARY_CALC,
            PERM_SALARY_SUBMIT,
            PERM_SALARY_POST,
        ] {
            assert!(V0387.contains(code), "V0387 没有种权限码 {code}");
        }
    }

    /// 提交 / 过账两条路由必须挂在 HR-39（改卡号会让「谁该实现它」在板上失联）。
    #[test]
    fn submit_and_post_belong_to_hr39() {
        for path in ["/business/hr/salary/{id}/submit", "/business/hr/salary/{id}/post"] {
            let spec = ROUTE_CONTRACT
                .iter()
                .find(|spec| spec.method == "POST" && spec.path == path)
                .unwrap_or_else(|| panic!("契约表缺 {path}"));
            assert_eq!(spec.card, "HR-39", "{path} 的落地卡必须是 HR-39");
        }
    }

    /// #44 红冲的契约行必须挂在 HR-40，且权限码逐字是 `business:hr:salary:reverse`
    /// （`V0387` 的 `e045` 按钮、`hr_04` 的权限矩阵都按这个字符串对齐）。
    #[test]
    fn reverse_route_belongs_to_hr40() {
        let spec = ROUTE_CONTRACT
            .iter()
            .find(|spec| spec.method == "POST" && spec.path == "/business/hr/salary/{id}/reverse")
            .expect("契约表缺红冲路由");
        assert_eq!(spec.card, "HR-40", "红冲的落地卡必须是 HR-40");
        assert_eq!(spec.permission, Some(PERM_SALARY_REVERSE));
        assert_eq!(PERM_SALARY_REVERSE, "business:hr:salary:reverse");
    }

    /// ★ 条件边契约：`form.gross_total_num` 必须是 **JSON number**。
    ///
    /// `hr_salary` 的「应发 ≥ 50 万 → 总经理审批」条件边（V0394）对 `gte` 走
    /// `Value::as_f64()`：字符串会 `TypeMismatch` → 引擎**静默走兜底边**（大额单绕过总经理）。
    /// 所以这条断言直接钉住「提交时冻结下来的表单长什么样」。
    #[test]
    fn form_data_freezes_gross_total_as_number() {
        let sheet = crate::business::hr::salary_sheet_model::SheetRow {
            id: Uuid::now_v7(),
            sheet_no: "SAL-202611-000001".to_string(),
            period_code: "2026-11".to_string(),
            period_start: "2026-11-01".parse().expect("日期"),
            period_end: "2026-11-30".parse().expect("日期"),
            status: dto::STATUS_CALCULATED.to_string(),
            attendance_period_id: None,
            employee_count: 12,
            earning_total: "520000".parse().expect("金额"),
            pre_deduct_total: "0".parse().expect("金额"),
            gross_total: "500000".parse().expect("金额"),
            withhold_total: "80000".parse().expect("金额"),
            net_total: "420000".parse().expect("金额"),
            si_company_total: "1680".parse().expect("金额"),
            hf_company_total: "960".parse().expect("金额"),
            accrual_voucher_id: None,
            accrual_voucher_no: None,
            payment_voucher_id: None,
            payment_voucher_no: None,
            reversal_voucher_id: None,
            approval_instance_id: None,
            calculated_at: None,
            calculated_by: None,
            submitted_at: None,
            posted_at: None,
            reversed_at: None,
            reverse_reason: None,
            remark: None,
            created_at: None,
            updated_at: None,
        };
        let form = salary_form_data(&sheet);
        let gross = &form["gross_total_num"];
        assert!(
            gross.is_number(),
            "gross_total_num 必须是 JSON number（字符串会让条件边静默走兜底）：{gross}"
        );
        assert_eq!(gross.as_f64(), Some(500_000.0));
        // 展示口径仍然是字符串（与出参一致，前端直接显示）
        assert_eq!(form["gross_total"], serde_json::json!("500000"));
        assert_eq!(form["period_code"], serde_json::json!("2026-11"));
        // 计提合计 = 应发 + 单位社保 + 单位公积金（设计 §4.4.1 第四条恒等式）
        assert_eq!(form["accrual_total"], serde_json::json!("502640"));
    }

    /// 404 报文点名 id（用户报障时能直接对上单）。
    #[test]
    fn not_found_names_the_sheet() {
        let id = Uuid::now_v7();
        let error = not_found(id);
        assert_eq!(error.status, 404);
        assert_eq!(error.code, error_code::RESOURCE_NOT_FOUND);
        assert!(error.msg.contains(&id.to_string()), "报文：{}", error.msg);
    }

    /// 造一份只带给定权限码的令牌声明（真库用例不参与，纯判断用）。
    fn claims_with(permissions: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: Uuid::now_v7().to_string(),
            username: Some("hr47-matrix".to_string()),
            roles: Vec::new(),
            permissions: permissions.iter().map(|p| (*p).to_string()).collect(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    /// 页面码与全量接口码**解耦**后的口径（卡 t_0af7ec46 / 迁移 `V0401`）：
    ///
    /// 员工自助的后端判据里只有自助码 `salary:my` —— 页面显形码
    /// （`business:hr:salary:page`）**不进后端**，所以「有页面」不再等于「有全量」。
    /// 本接口因此只认 `salary:list` 一道闸，handler 里不再需要第二道数据闸。
    /// 这条用例守的是「别又把页面码塞进接口判据」：一旦有人拿页面码当行级判据，
    /// 员工就会重新拿到全公司薪资单。
    #[test]
    fn full_list_is_denied_for_a_token_without_salary_list() {
        // 员工自助的真实形状：只有自助码（页面码不进后端判据）
        let employee = claims_with(&["business:hr:salary:my"]);
        assert!(
            !employee.has_permission(PERM_SALARY_LIST),
            "员工自助不该持有 salary:list（否则框架路由闸会放行全量列表）"
        );
        assert!(
            !employee.has_permission(PERM_SALARY_DETAIL),
            "员工自助不该持有 salary:detail"
        );

        // 允许态：人事 / 财务 / 超管
        let finance = claims_with(&[PERM_SALARY_LIST, PERM_SALARY_DETAIL]);
        assert!(finance.has_permission(PERM_SALARY_LIST));
        let super_admin = claims_with(&["*:*:*"]);
        assert!(super_admin.has_permission(PERM_SALARY_LIST));
    }
}
