//! 成本方法切换的接口层（成本核算 · M5-31 —— 存量期间不重算）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-method/...`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-method/switch-plan` | `business:erp:cost-method:list` | 切换预演（只读）：护栏 + 存量期间清单 + 两笔写 + 告警 |
//! | 2 | POST | `/business/erp/cost-method/switch` | `business:erp:cost-method:update` | 执行切换（事务内：收口旧版 + 插入新版） |
//!
//! # 为什么复用 M5-01 的两个权限码，不开新码
//!
//! 切换在数据上就是 M5-01 已经定义好的两笔合法动作：**改一条已生效版本的停止日**
//! （`cost-method:update` 允许，`66105` 明确放行停止日）+ **新增一条版本**
//! （`cost-method:create`）。区别只是「一次点完、且切换日受护栏约束」。
//!
//! ERP 模块还没做 PERM-01 的「页面码 / 接口码解耦」（AGENTS 约定 14 末尾的过渡期条款），
//! 新开一个 `cost-method:switch` 而不配菜单行就是死码（N14 判红）；配菜单行则要动
//! `sys_menu`（本卡只做迁移策略，不夹带菜单）。所以预演挂 `:list`（读），执行挂
//! `:update`（写里最重的那一半：决定历史口径在哪儿收口）。
//!
//! # 事务边界（约定 11：提交后不许留中间态）
//!
//! 整个判定 + 两笔写都在**一个事务**里：
//!
//! ```text
//! begin → 读配置时间线 + 期间台账 → 预演（护栏全过）→ 收口旧版 → 插新版 → commit
//! ```
//!
//! 任何一步失败都 `rollback`，不留「旧版收了、新版没插」的空窗（那会让一个期间
//! 落到代码兜底的方法上，报表口径无缘无故变一次）。提交之后**没有任何**可能失败的
//! 后续动作：出参全部用事务里已经算好的数据拼，不再回读（回读失败会变成「提交成功
//! 但接口报错」，用户重试还撞 66102）。
//!
//! # 历史期间怎么保证不重算
//!
//! 本接口**不碰任何结果表**：只写 `biz_cost_method_config` 一行（收口）和一行（新增）。
//! `biz_cost_period` / `biz_cost_period_item` / 归集 / 分摊 / 还原 / 凭证一行不改 ——
//! 出参 `history_untouched = true` 是「存量期间解析出的方法前一后一致」的自检结论，
//! 不是一句口号（`cost_method_switch_dto::PeriodMethodRow::unchanged`）。

use axum::Json;
use axum::extract::Query;
use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_method_handler as method_handler;
use crate::business::erp::cost::cost_method_model as method_model;
use crate::business::erp::cost::cost_method_switch_dto::{
    self as dto, CostMethodSwitchQuery, CostMethodSwitchReq, SwitchPlan, SwitchResultRes, SwitchTarget,
};
use crate::business::erp::cost::cost_method_switch_model as model;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（复用 M5-01，见模块头） ====================

/// 预演（只读）：复用列表码。
pub const PERM_PLAN: &str = method_handler::PERM_LIST;
/// 执行切换（写）：复用修改码。
pub const PERM_SWITCH: &str = method_handler::PERM_UPDATE;

// ==================== 小件 ====================

/// 事务里算好的一整套预演上下文（出参与落库都从这里取，提交后不再回读）。
#[derive(Debug)]
struct Preview {
    /// 预演。
    plan: SwitchPlan,
    /// 当前配置行（收口时按 `ops[0].config_id` 找它的原规格）。
    configs: Vec<method_model::CostMethodRow>,
    /// 范围展示名（`全公司默认` / `成本对象 CB-…`）。
    scope_label: String,
    /// 成本对象号（GLOBAL 时为空）。
    scope_object_no: Option<String>,
    /// 成本对象 id → 对象号（出参里对象级覆盖清单用）。
    object_nos: std::collections::HashMap<Uuid, String>,
}

/// 校验范围指向的成本对象，并返回对象号（`COST_OBJECT` 时）。
async fn scope_object_no(target: &SwitchTarget) -> std::result::Result<Option<String>, ApiError> {
    match target.scope_id {
        None => Ok(None),
        Some(id) => method_handler::ensure_object(id).await.map(Some),
    }
}

/// 读时间线 + 台账 → 预演（同一个 `SqlExecutor`：预演与执行读到同一份快照）。
async fn preview_in(
    ex: &impl SqlExecutor,
    switch_on_raw: Option<&str>,
    to_method_raw: Option<&str>,
    scope_raw: Option<&str>,
    scope_id_raw: Option<&str>,
    today: NaiveDate,
) -> std::result::Result<Preview, ApiError> {
    let periods = model::period_ledgers(ex)
        .await
        .map_err(|error| utils::db_error(error, "查询成本期间台账"))?;
    let suggested = dto::suggested_switch_on(today, &periods);
    let target = dto::parse_target(switch_on_raw, to_method_raw, scope_raw, scope_id_raw, suggested)?;
    let scope_object_no = scope_object_no(&target).await?;
    let configs = method_model::configs_in(ex)
        .await
        .map_err(|error| utils::db_error(error, "查询成本方法配置"))?;
    let views = configs
        .iter()
        .map(method_model::CostMethodRow::to_view)
        .collect::<Vec<_>>();
    let plan = dto::build_plan(&views, &periods, &target, today)?;
    let scope_label = match (&scope_object_no, target.scope_id) {
        (Some(object_no), _) => format!("成本对象 {object_no}"),
        (None, None) => "全公司默认".to_string(),
        (None, Some(id)) => format!("成本对象 {id}"),
    };
    let override_ids = plan.object_overrides.iter().map(|row| row.scope_id).collect::<Vec<_>>();
    let object_nos = model::object_nos(ex, &override_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象号"))?;
    Ok(Preview {
        plan,
        configs,
        scope_label,
        scope_object_no,
        object_nos,
    })
}

/// 预演出参（纯映射，不再查库）。
fn preview_res(preview: &Preview) -> dto::SwitchPlanRes {
    dto::plan_res(
        &preview.plan,
        &preview.scope_label,
        preview.scope_object_no.as_deref(),
        &preview.object_nos,
    )
}

/// 结论一句话（执行结果显示在页面上）。
pub fn switch_message(plan: &SwitchPlan, scope_label: &str) -> String {
    let history = if plan.history.is_empty() {
        "切换日之前没有算过的期间".to_string()
    } else {
        format!(
            "切换日之前的 {} 个期间（{} 个批次）口径不变、不重算",
            plan.history.len(),
            plan.history_batches
        )
    };
    let engine = if plan.engine_ready {
        "新期间可以正常计算".to_string()
    } else {
        "注意：目标方法一期还没实现，新期间的「计算成本」会被拒绝（66101）".to_string()
    };
    format!(
        "从 {} 起 {} 按「{}」核算；{}；{}",
        plan.switch_on,
        scope_label,
        crate::business::erp::cost::cost_method_dto::method_label(&plan.to_method),
        history,
        engine
    )
}

/// 写库错误 → `ApiError`（唯一出口；只处理并发撞唯一键的情形）。
fn map_switch_write_error(err: DbError, switch_on: NaiveDate, scope_label: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { .. } => ApiError::new(
            409,
            dto::CODE_OVERLAP,
            format!(
                "{scope_label} 从 {switch_on} 起已经有一条配置（并发写入撞了唯一键）：\
                 切换可能刚被别的会话执行过，请刷新预演后确认当前口径"
            ),
        ),
        _ => utils::db_error(err, "执行成本方法切换"),
    }
}

// ==================== 预演 / 执行（业务函数） ====================

/// 切换预演（只读）。
///
/// # 异常情况
///
/// - 切换日不合法（非期间首日 / 追溯 / 该期间已有批次）⇒ `409` + `66113`
/// - 同一切换日已有启用版本 ⇒ `409` + `66102`
/// - 范围参数不合法 / 对象不存在 ⇒ `400` + `66103`
/// - 查库失败 ⇒ 500
pub async fn switch_plan(query: CostMethodSwitchQuery) -> std::result::Result<dto::SwitchPlanRes, ApiError> {
    let today = method_handler::today_cst();
    let preview = preview_in(
        db(),
        query.switch_on.as_deref(),
        query.to_method.as_deref(),
        query.scope.as_deref(),
        query.scope_id.as_deref(),
        today,
    )
    .await?;
    Ok(preview_res(&preview))
}

/// 执行切换（事务内：收口旧版 + 插入新版）。
///
/// # 异常情况
///
/// - 预演的全部护栏（`66113` / `66102` / `66103`）
/// - 目标方法一期未实现且未显式确认 ⇒ `409` + `66101`
/// - 收口时版本已被并发删掉（受影响 0 行）⇒ 404
/// - 写库失败 ⇒ 500（事务已回滚，配置时间线回到执行前）
pub async fn switch_method(
    operator: Option<Uuid>,
    req: CostMethodSwitchReq,
) -> std::result::Result<SwitchResultRes, ApiError> {
    let today = method_handler::today_cst();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启成本方法切换事务"))?;

    // ① 预演（读的是事务快照）
    let preview = match preview_in(
        &tx,
        req.switch_on.as_deref(),
        req.to_method.as_deref(),
        req.scope.as_deref(),
        req.scope_id.as_deref(),
        today,
    )
    .await
    {
        Ok(preview) => preview,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };

    // ② 目标方法没实现：必须显式确认（否则下个期间算不出来）
    if !preview.plan.to_method_implemented && !req.confirm_unimplemented {
        let error = dto::unimplemented_target_needs_confirm(
            &preview.scope_label,
            &preview.plan.to_method,
            preview.plan.switch_on,
        );
        let _ = tx.rollback().await;
        return Err(error);
    }

    // ③ 收口旧版（有旧版才收）
    let closing = preview.plan.ops.iter().find(|op| op.action == dto::OP_CLOSE).cloned();
    if let Some(op) = &closing {
        let id = op
            .config_id
            .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "预演里的收口动作缺少 config_id"))?;
        let row = preview.configs.iter().find(|row| row.id == id).ok_or_else(|| {
            ApiError::new(
                409,
                dto::CODE_OVERLAP,
                "要收口的旧版本在事务里已经找不到了（并发删除）：请刷新预演后重试",
            )
        })?;
        let mut spec = row.to_spec();
        spec.effective_to = op.effective_to;
        let affected = match method_model::update_in(&tx, id, &spec, operator).await {
            Ok(affected) => affected,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(map_switch_write_error(
                    error,
                    preview.plan.switch_on,
                    &preview.scope_label,
                ));
            }
        };
        if affected == 0 {
            let _ = tx.rollback().await;
            return Err(ApiError::new(
                409,
                dto::CODE_OVERLAP,
                "要收口的旧版本刚被删掉了：请刷新预演后重试",
            ));
        }
    }

    // ④ 插入新版
    let new_id = Uuid::now_v7();
    let spec = dto::new_version_spec(
        &preview.plan,
        crate::business::erp::cost::cost_labor_dto::clean_optional(req.remark.clone()),
    );
    if let Err(error) = method_model::insert_in(&tx, new_id, &spec, operator).await {
        let _ = tx.rollback().await;
        return Err(map_switch_write_error(
            error,
            preview.plan.switch_on,
            &preview.scope_label,
        ));
    }

    // ⑤ 提交（此后不再有任何可能失败的后续动作）
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交成本方法切换事务"))?;

    Ok(SwitchResultRes {
        plan: preview_res(&preview),
        closed: closing.as_ref().map(dto::op_res),
        created_config_id: new_id.to_string(),
        created_effective_from: preview.plan.switch_on.to_string(),
        message: switch_message(&preview.plan, &preview.scope_label),
    })
}

/// 全部权限码（与 M5-01 完全一致：本卡零新增码）。
pub fn all_permissions() -> Vec<&'static str> {
    vec![PERM_PLAN, PERM_SWITCH]
}

// ==================== HTTP 入口 ====================

/// 切换预演（只读）。
#[nik::permission(
    path = "/business/erp/cost-method/switch-plan",
    method = "GET",
    permission = "business:erp:cost-method:list"
)]
#[nik::get("/business/erp/cost-method/switch-plan")]
pub async fn cost_method_switch_plan_api(Query(query): Query<CostMethodSwitchQuery>) -> ResResult<dto::SwitchPlanRes> {
    Ok(Res::ok(switch_plan(query).await?))
}

/// 执行方法切换。
#[nik::permission(
    path = "/business/erp/cost-method/switch",
    method = "POST",
    permission = "business:erp:cost-method:update"
)]
#[nik::post("/business/erp/cost-method/switch")]
pub async fn cost_method_switch_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CostMethodSwitchReq>,
) -> ResResult<SwitchResultRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(switch_method(operator, req).await?))
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost::cost_method_dto::{METHOD_JOB, MethodConfigView, SCOPE_GLOBAL, STATUS_ENABLED};
    use crate::business::erp::cost::cost_method_switch_dto::PeriodLedger;

    /// 两个码都是 M5-01 已经种好的码，本卡不新增（ERP 未做 PERM-01 解耦，新码即死码）。
    #[test]
    fn permissions_are_reused_from_m501() {
        assert_eq!(PERM_PLAN, "business:erp:cost-method:list");
        assert_eq!(PERM_SWITCH, "business:erp:cost-method:update");
        assert_eq!(all_permissions(), vec![PERM_PLAN, PERM_SWITCH]);
        for code in all_permissions() {
            assert!(code.starts_with("business:erp:cost-method:"), "{code}");
        }
    }

    /// 预演与执行是两条路由、两个码（读 / 写分开）。
    #[test]
    fn plan_is_read_and_switch_is_write() {
        assert_ne!(PERM_PLAN, PERM_SWITCH);
        assert!(PERM_PLAN.ends_with(":list"));
        assert!(PERM_SWITCH.ends_with(":update"));
    }

    /// 结论文案：带上切换日、范围、新方法、存量期间数、能不能算。
    #[test]
    fn message_names_switch_day_scope_and_history() {
        let rows = [MethodConfigView {
            id: Uuid::from_u128(1),
            method: METHOD_JOB.to_string(),
            scope_type: SCOPE_GLOBAL.to_string(),
            scope_id: None,
            effective_from: NaiveDate::from_ymd_opt(1970, 1, 1).expect("日期"),
            effective_to: None,
            status: STATUS_ENABLED,
        }];
        let periods = [PeriodLedger {
            period_code: "2026-11".to_string(),
            batch_count: 2,
            statuses: vec!["DRAFT".to_string()],
        }];
        let today = NaiveDate::from_ymd_opt(2026, 11, 20).expect("日期");
        let target = dto::parse_target(
            None,
            Some("VARIETY"),
            None,
            None,
            dto::suggested_switch_on(today, &periods),
        )
        .expect("目标");
        let plan = dto::build_plan(&rows, &periods, &target, today).expect("预演");
        let message = switch_message(&plan, "全公司默认");
        assert!(message.contains("2026-12-01"), "{message}");
        assert!(message.contains("全公司默认"), "{message}");
        assert!(message.contains("品种法"), "{message}");
        assert!(message.contains("不重算"), "{message}");
        assert!(message.contains("66101"), "未实现方法要提醒：{message}");
    }

    /// 今天按 CST 取（跨日边界不许把切换日算成前一天）。
    #[test]
    fn today_is_cst() {
        let today = method_handler::today_cst();
        let utc_today = nik::prelude::chrono::Utc::now().date_naive();
        assert!(
            today == utc_today || today == utc_today + nik::prelude::chrono::Duration::days(1),
            "CST 与 UTC 最多差一天"
        );
    }
}
