//! 定时任务管理接口：任务定义 CRUD、启停、手动执行与执行日志查询。
//!
//! 处理器承担**参数 / cron / 处理器注册校验、业务编排与响应组装**，并直接调用
//! 框架快捷方法（`find_or_none` / `insert` / `update` / `delete` 等）；需要编写
//! SQL 的查询（分页、名称唯一性校验、启停写库）集中在
//! [`crate::support::job::model`]，动态 SQL 全部走 `?` 占位符参数绑定。
//!
//! 新建 / 更新 / 启停 / 删除任务后统一调用 [`crate::task::scheduler`] 的
//! `reload()` 重新装载调度（框架调度器在 `start()` 时对注册表做一次性快照，
//! 运行期变更必须重载）；重载失败只记错误日志，不影响接口成功。
//!
//! 文件顺序遵循项目约定：**路由方法在前，公共辅助函数在后**。

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use nik::prelude::nik_plugin::scheduler::CronSchedule;
use nik::prelude::*;

use std::net::SocketAddr;

use crate::shared::constants::EnableStatus;
use crate::shared::error_code;
use crate::support::job::dto::{
    JobLogPageQuery, JobLogRes, JobPageQuery, JobRes, JobSaveReq, JobStatusReq,
};
use crate::support::job::model::{self, SysJobLogModel, SysJobModel};
use crate::support::log;
use crate::utils;

// ==================== 接口 ====================

/// 分页查询定时任务。
///
/// # 参数
///
/// - `query`：过滤条件（名称模糊 / 分组 / 状态，均可选）与分页参数。
///
/// # 返回值
///
/// 成功返回任务分页列表。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[nik::permission(
    path = "/support/job/page",
    method = "GET",
    permission = "support:job:list"
)]
#[nik::get("/support/job/page")]
pub async fn page(Query(query): Query<JobPageQuery>) -> ResResult<PageData<Vec<JobRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);

    let filter = model::JobFilter {
        job_name: query.job_name,
        job_group: query.job_group,
        status: query.status,
    };
    let paged = model::page_jobs(&filter, page, page_size)
        .await
        .map_err(|e| utils::db_error(e, "查询定时任务列表"))?;
    let list = paged.list.iter().map(JobRes::from).collect::<Vec<_>>();

    Ok(Res::page(list, paged.total, paged.page, paged.size))
}

/// 定时任务详情。
///
/// # 参数
///
/// - `id`：任务 ID。
///
/// # 返回值
///
/// 成功返回任务详情。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：任务不存在。
#[nik::permission(
    path = "/support/job/{id}",
    method = "GET",
    permission = "support:job:list"
)]
#[nik::get("/support/job/{id}")]
pub async fn detail(Path(id): Path<String>) -> ResResult<JobRes> {
    let job = load_job(&id).await?;
    Ok(Res::ok(JobRes::from(&job)))
}

/// 新建定时任务。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（用于审计字段）。
/// - `req`：任务请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`JOB_CREATE`，`after_data` 记录名称 / 分组 / 状态）；
/// 日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回新建任务详情。
///
/// # 异常情况
///
/// - 422：cron 表达式非法或处理器未注册；
/// - 409：任务名称已存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/job",
    method = "POST",
    permission = "support:job:create"
)]
#[nik::post("/support/job")]
pub async fn create(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<JobSaveReq>,
) -> ResResult<JobRes> {
    let job = build_model(&req, None, utils::current_user_uuid(&claims)).await?;
    let mut job = job;
    job.insert(db())
        .await
        .map_err(|e| utils::db_error(e, "新建定时任务"))?;
    reload_scheduler().await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "JOB_CREATE",
            "Job",
            job.id.to_string(),
            None,
            Some(serde_json::json!({
                "job_name": job.job_name,
                "job_group": job.job_group,
                "status": job.status.as_i16(),
            })),
        ),
    );
    Ok(Res::ok(JobRes::from(&job)))
}

/// 更新定时任务（全量）。
///
/// 保留原 `created_at` / `create_by` / `last_run_at` / `next_run_at`，仅更新业务字段。
///
/// # 参数
///
/// - `id`：任务 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明。
/// - `req`：任务请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`JOB_UPDATE`，含名称 / 分组 / 状态的变更前 / 后快照）；
/// 日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回更新后的任务详情。
///
/// # 异常情况
///
/// - 400：ID 或状态非法；
/// - 404：任务不存在；
/// - 422：cron 表达式非法或处理器未注册；
/// - 409：任务名称与其它任务冲突；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/job/{id}",
    method = "PUT",
    permission = "support:job:update"
)]
#[nik::put("/support/job/{id}")]
pub async fn update(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<JobSaveReq>,
) -> ResResult<JobRes> {
    let id = utils::require_uuid(&id, "id")?;
    let existing = SysJobModel::find_or_none(db(), &id)
        .await
        .map_err(|e| utils::db_error(e, "查询待更新定时任务"))?
        .ok_or_else(|| ApiError::new(404, error_code::JOB_NOT_FOUND, "定时任务不存在"))?;

    let mut job = build_model(&req, Some(id), utils::current_user_uuid(&claims)).await?;
    // 保留创建审计字段与运行时间，由执行过程维护
    job.created_at = existing.created_at;
    job.create_by = existing.create_by;
    job.last_run_at = existing.last_run_at;
    job.next_run_at = existing.next_run_at;
    job.update(db())
        .await
        .map_err(|e| utils::db_error(e, "更新定时任务"))?;
    reload_scheduler().await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "JOB_UPDATE",
            "Job",
            job.id.to_string(),
            Some(serde_json::json!({
                "job_name": existing.job_name,
                "job_group": existing.job_group,
                "status": existing.status.as_i16(),
            })),
            Some(serde_json::json!({
                "job_name": job.job_name,
                "job_group": job.job_group,
                "status": job.status.as_i16(),
            })),
        ),
    );
    Ok(Res::ok(JobRes::from(&job)))
}

/// 删除定时任务（软删）。
///
/// # 参数
///
/// - `id`：任务 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（审计操作者）。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`JOB_DELETE`，`before_data` 记录被删任务的名称 / 分组 / 状态）；
/// 日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：任务不存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/job/{id}",
    method = "DELETE",
    permission = "support:job:delete"
)]
#[nik::delete("/support/job/{id}")]
pub async fn delete(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<()> {
    let job = load_job(&id).await?;
    SysJobModel::of(job.id)
        .delete(db())
        .await
        .map(|_| ())
        .map_err(|e| utils::db_error(e, "删除定时任务"))?;
    reload_scheduler().await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "JOB_DELETE",
            "Job",
            job.id.to_string(),
            Some(serde_json::json!({
                "job_name": job.job_name,
                "job_group": job.job_group,
                "status": job.status.as_i16(),
            })),
            None,
        ),
    );
    Ok(Res::ok_empty())
}

/// 启停定时任务。
///
/// # 参数
///
/// - `id`：任务 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明。
/// - `req`：启停请求体（`status`：1 启用 / 0 暂停）。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`JOB_STATUS`，含名称 / 分组 / 状态的变更前 / 后快照）；
/// 日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 或状态非法；
/// - 404：任务不存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/job/{id}/status",
    method = "PATCH",
    permission = "support:job:update"
)]
#[nik::patch("/support/job/{id}/status")]
pub async fn change_status(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<JobStatusReq>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let status = EnableStatus::from_i16(req.status)?;
    let Some(existing) = SysJobModel::find_or_none(db(), &id)
        .await
        .map_err(|e| utils::db_error(e, "查询待启停定时任务"))?
    else {
        return Err(ApiError::new(
            404,
            error_code::JOB_NOT_FOUND,
            "定时任务不存在",
        ));
    };

    model::update_job_status(&id, status, utils::current_user_uuid(&claims))
        .await
        .map_err(|e| utils::db_error(e, "启停定时任务"))?;
    reload_scheduler().await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "JOB_STATUS",
            "Job",
            id.to_string(),
            Some(serde_json::json!({
                "job_name": existing.job_name,
                "job_group": existing.job_group,
                "status": existing.status.as_i16(),
            })),
            Some(serde_json::json!({
                "job_name": existing.job_name,
                "job_group": existing.job_group,
                "status": status.as_i16(),
            })),
        ),
    );
    Ok(Res::ok_empty())
}

/// 立即执行一次定时任务（同步等待执行完成）。
///
/// # 参数
///
/// - `id`：任务 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（审计操作者）。
///
/// # 日志
///
/// 执行成功后写入 `audit_logs`（`JOB_TRIGGER`，`after_data` 记录被触发任务的名称 /
/// 分组 / 状态，`before_data` 为空）；日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：任务不存在；
/// - 422：处理器未注册；
/// - 500：数据库异常或处理器执行失败。
#[nik::permission(
    path = "/support/job/{id}/trigger",
    method = "POST",
    permission = "support:job:trigger"
)]
#[nik::post("/support/job/{id}/trigger")]
pub async fn trigger(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<()> {
    let job = load_job(&id).await?;
    crate::task::scheduler().run_job(&job).await?;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "JOB_TRIGGER",
            "Job",
            job.id.to_string(),
            None,
            Some(serde_json::json!({
                "job_name": job.job_name,
                "job_group": job.job_group,
                "status": job.status.as_i16(),
            })),
        ),
    );
    Ok(Res::ok_empty())
}

/// 分页查询任务执行日志。
///
/// # 参数
///
/// - `query`：过滤条件（任务 ID / 名称模糊 / 执行状态，均可选）与分页参数。
///
/// # 返回值
///
/// 成功返回执行日志分页列表。
///
/// # 异常情况
///
/// - 400：`job_id` 格式非法；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/job/log/page",
    method = "GET",
    permission = "support:job:list"
)]
#[nik::get("/support/job/log/page")]
pub async fn log_page(Query(query): Query<JobLogPageQuery>) -> ResResult<PageData<Vec<JobLogRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);

    // `job_id` 为可选过滤：提供但格式非法时按参数错误拦截
    let job_id = match query
        .job_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(raw) => Some(utils::require_uuid(raw, "job_id")?),
        None => None,
    };
    let filter = model::JobLogFilter {
        job_id,
        job_name: query.job_name,
        status: query.status,
    };
    let paged = model::page_job_logs(&filter, page, page_size)
        .await
        .map_err(|e| utils::db_error(e, "查询任务执行日志"))?;
    let list = paged.list.iter().map(JobLogRes::from).collect::<Vec<_>>();

    Ok(Res::page(list, paged.total, paged.page, paged.size))
}

/// 任务执行日志详情。
///
/// # 参数
///
/// - `id`：日志 ID。
///
/// # 返回值
///
/// 成功返回日志详情。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：日志不存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/job/log/{id}",
    method = "GET",
    permission = "support:job:list"
)]
#[nik::get("/support/job/log/{id}")]
pub async fn log_detail(Path(id): Path<String>) -> ResResult<JobLogRes> {
    let id = utils::require_uuid(&id, "id")?;
    let log = SysJobLogModel::find_or_none(db(), &id)
        .await
        .map_err(|e| utils::db_error(e, "查询任务执行日志"))?
        .ok_or_else(|| ApiError::new(404, error_code::LOG_NOT_FOUND, "任务执行日志不存在"))?;
    Ok(Res::ok(JobLogRes::from(&log)))
}

// ==================== 公共辅助 ====================

/// 重新装载调度（新建 / 更新 / 启停 / 删除后调用）。
///
/// 失败只记错误日志，不使业务接口失败。
async fn reload_scheduler() {
    if let Err(e) = crate::task::scheduler().reload().await {
        tracing::error!(error = %e.msg, "重新装载定时任务调度失败");
    }
}

/// 把请求中的 `params` 文本转换为 JSON 值。
///
/// 非空且可解析为 JSON 时存解析结果，否则按 JSON 字符串存；空串视为未提供。
///
/// # 参数
///
/// - `raw`：请求中的 `params` 文本。
///
/// # 返回值
///
/// 空串返回 `None`，否则返回对应的 JSON 值。
fn parse_params(raw: Option<&str>) -> Option<JsonValue> {
    let text = raw?.trim();
    if text.is_empty() {
        return None;
    }
    match serde_json::from_str::<JsonValue>(text) {
        Ok(json) => Some(json),
        Err(_) => Some(JsonValue::String(text.to_string())),
    }
}

/// 按主键读取任务，不存在时返回 404。
///
/// # 参数
///
/// - `raw_id`：路径中的任务 ID 文本。
///
/// # 返回值
///
/// 任务模型。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：任务不存在；
/// - 500：数据库异常。
async fn load_job(raw_id: &str) -> std::result::Result<SysJobModel, ApiError> {
    let id = utils::require_uuid(raw_id, "id")?;
    SysJobModel::find_or_none(db(), &id)
        .await
        .map_err(|e| utils::db_error(e, "查询定时任务"))?
        .ok_or_else(|| ApiError::new(404, error_code::JOB_NOT_FOUND, "定时任务不存在"))
}

/// 从请求构造任务模型字段（新建 / 更新共用）。
///
/// # 参数
///
/// - `req`：任务请求体。
/// - `id`：主键（更新时为既有 ID，新建时为 `None`）。
/// - `operator`：当前操作人 ID。
///
/// # 返回值
///
/// 可直接写入的任务模型（`last_run_at` / `next_run_at` 由调用方决定）。
///
/// # 异常情况
///
/// - 422：cron 表达式非法或处理器未注册；
/// - 409：任务名称已存在；
/// - 400：状态值非法；
/// - 500：数据库异常。
async fn build_model(
    req: &JobSaveReq,
    id: Option<uuid::Uuid>,
    operator: Option<uuid::Uuid>,
) -> std::result::Result<SysJobModel, ApiError> {
    let job_name = req.job_name.trim().to_string();
    let cron_expr = req.cron_expr.trim().to_string();
    let invoke_target = req.invoke_target.trim().to_string();

    // cron 表达式合法性（与调度器同一解析器，避免「能存不能跑」）
    if CronSchedule::parse(&cron_expr).is_err() {
        return Err(ApiError::new(
            422,
            error_code::JOB_CRON_INVALID,
            "cron 表达式非法",
        ));
    }
    // 处理器必须已注册，否则任务永远无法执行
    if !crate::task::is_registered_target(&invoke_target) {
        return Err(ApiError::new(
            422,
            error_code::JOB_TARGET_UNREGISTERED,
            format!("未注册的任务处理器: {invoke_target}"),
        ));
    }
    // 任务名称唯一（软删表，仅比较未删除行）
    let duplicate = model::find_job_by_name(&job_name)
        .await
        .map_err(|e| utils::db_error(e, "校验任务名称唯一性"))?;
    if let Some(existing) = duplicate
        && Some(existing.id) != id
    {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            "任务名称已存在",
        ));
    }

    let status = match req.status {
        Some(value) => EnableStatus::from_i16(value)?,
        None => EnableStatus::enabled,
    };
    let job_group = req
        .job_group
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("DEFAULT")
        .to_string();
    let now = chrono::Utc::now();
    Ok(SysJobModel {
        id: id.unwrap_or_else(uuid::Uuid::now_v7),
        job_name,
        job_group,
        cron_expr,
        invoke_target,
        params: parse_params(req.params.as_deref()),
        status,
        last_run_at: None,
        next_run_at: None,
        remark: req
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        created_at: now,
        updated_at: now,
        create_by: operator,
        update_by: operator,
        deleted_at: None,
    })
}

// ==================== 单测 ====================
//
// 定时任务管理在补测前是 **0 条** `#[test]`（评审实测），而它是「运维安全阀」：
// cron 表达式能存不能跑、参数被静默降级、状态 0/1 写反，界面上都看不出来。
//
// 本模块全部是**纯逻辑用例**（不连库、不需要 `SYS_DB_URL`），钉住四类契约：
//
// 1. `params` 文本 → JSONB 的转换规则（[`parse_params`]）；
// 2. 响应字段的空值口径（`None` → 空串，前端表格不需要判空）；
// 3. 请求校验的边界（1-64 / 64 / 255 / 500 字，按**字符**计）；
// 4. cron 的**解析 + 下一次触发时刻**（与调度器同一个 `CronSchedule`，避免「能存不能跑」）。
//
// 跑法（不需要数据库）：
//
// ```bash
// cargo test --lib support::job
// ```
#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::job::model::JobLogStatus;
    use validator::Validate;

    // ---------- 夹具 ----------

    /// 固定时钟（用例不依赖 `Utc::now()`，避免跨天抖动）。
    fn utc(text: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(text)
            .expect("固定时钟格式合法")
            .with_timezone(&chrono::Utc)
    }

    /// 一条启用中的任务定义。
    fn job_model() -> SysJobModel {
        let now = utc("2026-11-05T01:00:00Z");
        SysJobModel {
            id: uuid::Uuid::now_v7(),
            job_name: "清理操作日志".to_string(),
            job_group: "LOG".to_string(),
            cron_expr: "0 3 * * *".to_string(),
            invoke_target: "clean_operation_log".to_string(),
            params: None,
            status: EnableStatus::enabled,
            last_run_at: None,
            next_run_at: None,
            remark: None,
            created_at: now,
            updated_at: now,
            create_by: None,
            update_by: None,
            deleted_at: None,
        }
    }

    /// 一份合法请求（各用例只改自己关心的字段）。
    fn save_req() -> JobSaveReq {
        JobSaveReq {
            job_name: "清理操作日志".to_string(),
            job_group: None,
            cron_expr: "0 3 * * *".to_string(),
            invoke_target: "clean_operation_log".to_string(),
            params: None,
            status: None,
            remark: None,
        }
    }

    // ---------- ① params 文本 → JSONB ----------

    /// 空串 / 纯空白 = 「没有参数」（落库 `NULL`），不能存成 JSON 字符串 `""`。
    #[test]
    fn parse_params_treats_none_and_blank_as_absent() {
        assert_eq!(parse_params(None), None);
        assert_eq!(parse_params(Some("")), None);
        assert_eq!(parse_params(Some("   \n\t ")), None);
    }

    /// 合法 JSON 原样保留 —— 对象 / 数组 / 数字 / 字符串 / `null` 都不能被降级成字符串。
    #[test]
    fn parse_params_keeps_valid_json_of_every_shape() {
        let object = parse_params(Some(r#"{"batch":200,"dry_run":false}"#)).expect("对象");
        assert_eq!(object["batch"], serde_json::json!(200));
        assert_eq!(object["dry_run"], serde_json::json!(false));

        assert_eq!(
            parse_params(Some("[1,2,3]")),
            Some(serde_json::json!([1, 2, 3])),
            "数组不能被包成字符串"
        );
        assert_eq!(parse_params(Some("123")), Some(serde_json::json!(123)));
        assert_eq!(
            parse_params(Some("\"topic\"")),
            Some(serde_json::json!("topic"))
        );
        assert_eq!(parse_params(Some("null")), Some(JsonValue::Null));
    }

    /// 首尾空白剥掉后再解析（前端输入框常带空格，不能因此降级成字符串）。
    #[test]
    fn parse_params_trims_outer_whitespace_before_parsing() {
        assert_eq!(
            parse_params(Some("  {\"a\": 1}  ")),
            Some(serde_json::json!({"a": 1}))
        );
    }

    /// 非 JSON 文本按「JSON 字符串」存，且保留用户原文（只去首尾空白）。
    #[test]
    fn parse_params_falls_back_to_plain_string() {
        assert_eq!(
            parse_params(Some("  not-a-json  ")),
            Some(JsonValue::String("not-a-json".to_string()))
        );
        assert_eq!(
            parse_params(Some("{broken")),
            Some(JsonValue::String("{broken".to_string())),
            "半截 JSON 不能丢，要原样留给排查"
        );
    }

    // ---------- ② 响应映射 ----------

    /// 无参数 / 无备注 → 空串；从未执行 → `None`（前端显示「未执行」）。
    #[test]
    fn job_res_maps_absent_params_and_remark_to_empty_string() {
        let model = job_model();
        let res = JobRes::from(&model);
        assert_eq!(res.id, model.id.to_string());
        assert_eq!(res.job_name, "清理操作日志");
        assert_eq!(res.params, "", "None 必须回空串，不能是字符串 \"null\"");
        assert_eq!(res.remark, "", "None 必须回空串");
        assert_eq!(res.status, 1);
        assert_eq!(res.last_run_at, None);
        assert_eq!(res.next_run_at, None);
        assert_eq!(res.created_at, model.created_at);
    }

    /// 暂停任务 `status = 0`；params 是 JSON 文本；备注原样。
    #[test]
    fn job_res_keeps_params_json_and_paused_status() {
        let mut model = job_model();
        model.status = EnableStatus::disabled;
        model.params = Some(serde_json::json!({"batch": 200}));
        model.remark = Some("凌晨跑".to_string());
        model.last_run_at = Some(utc("2026-11-04T19:00:00Z"));

        let res = JobRes::from(&model);
        assert_eq!(res.status, 0, "暂停任务必须回 0（sys_job.status=0）");
        assert_eq!(res.params, r#"{"batch":200}"#);
        assert_eq!(res.remark, "凌晨跑");
        assert_eq!(res.last_run_at, Some(utc("2026-11-04T19:00:00Z")));
    }

    /// 日志响应：`message` / `error` / `duration_ms` 缺省回空串与 0（前端表格不判空）。
    #[test]
    fn job_log_res_maps_nulls_to_empty_values() {
        let started = utc("2026-11-04T19:00:00Z");
        let log = SysJobLogModel {
            id: uuid::Uuid::now_v7(),
            job_id: uuid::Uuid::now_v7(),
            job_name: "清理操作日志".to_string(),
            invoke_target: "clean_operation_log".to_string(),
            params: None,
            status: JobLogStatus::success,
            message: None,
            error: None,
            started_at: started,
            finished_at: None,
            duration_ms: None,
            created_at: started,
        };
        let res = JobLogRes::from(&log);
        assert_eq!(res.status, 1);
        assert_eq!(res.message, "");
        assert_eq!(res.error, "");
        assert_eq!(res.duration_ms, 0);
        assert_eq!(res.finished_at, None);
        assert_eq!(res.job_id, log.job_id.to_string());
    }

    /// 执行状态与库值一一对应（1 成功 / 0 失败）；写反会让前端把成功画成红叉。
    #[test]
    fn job_log_status_maps_to_documented_integers() {
        assert_eq!(JobLogStatus::success.as_i16(), 1, "成功必须是 1");
        assert_eq!(JobLogStatus::failed.as_i16(), 0, "失败必须是 0");

        let started = utc("2026-11-04T19:00:00Z");
        let log = SysJobLogModel {
            id: uuid::Uuid::now_v7(),
            job_id: uuid::Uuid::now_v7(),
            job_name: "清理审计日志".to_string(),
            invoke_target: "clean_audit_log".to_string(),
            params: None,
            status: JobLogStatus::failed,
            message: None,
            error: Some("表锁超时".to_string()),
            started_at: started,
            finished_at: Some(started),
            duration_ms: Some(1200),
            created_at: started,
        };
        let res = JobLogRes::from(&log);
        assert_eq!(res.status, 0, "失败日志必须回 0");
        assert_eq!(res.error, "表锁超时");
        assert_eq!(res.duration_ms, 1200);
        assert!(res.finished_at.is_some());
    }

    // ---------- ③ 请求校验边界 ----------

    /// 边界内的最长值全部放行：名称 64 / 分组 64 / cron 64 / 目标 255 / 备注 500（按**字符**计）。
    #[test]
    fn job_save_req_accepts_boundary_lengths() {
        let mut req = save_req();
        req.job_name = "名".repeat(64);
        req.job_group = Some("组".repeat(64));
        req.cron_expr = "*".repeat(64);
        req.invoke_target = "t".repeat(255);
        req.remark = Some("备".repeat(500));
        req.params = Some("{}".to_string());
        req.status = Some(1);
        assert!(
            req.validate().is_ok(),
            "64/64/64/255/500 字必须放行（汉字按字符计）：{:?}",
            req.validate().unwrap_err()
        );
    }

    /// 必填项为空 → 校验失败，且报文点名是哪个字段（前端据此定位高亮）。
    #[test]
    fn job_save_req_rejects_blank_required_fields() {
        let mut req = save_req();
        req.job_name = String::new();
        let error = req.validate().unwrap_err().to_string();
        assert!(error.contains("任务名称"), "必须点名 job_name：{error}");

        let mut req = save_req();
        req.cron_expr = String::new();
        let error = req.validate().unwrap_err().to_string();
        assert!(error.contains("cron"), "必须点名 cron：{error}");

        let mut req = save_req();
        req.invoke_target = String::new();
        let error = req.validate().unwrap_err().to_string();
        assert!(
            error.contains("调用目标"),
            "必须点名 invoke_target：{error}"
        );
    }

    /// 超长 1 个字就拒：名称 65 / 分组 65 / cron 65 / 目标 256 / 备注 501。
    #[test]
    fn job_save_req_rejects_over_length_fields() {
        let mut req = save_req();
        req.job_name = "名".repeat(65);
        assert!(req.validate().is_err(), "65 字名称必须拒");

        let mut req = save_req();
        req.job_group = Some("组".repeat(65));
        assert!(req.validate().is_err(), "65 字分组必须拒");

        let mut req = save_req();
        req.cron_expr = "*".repeat(65);
        assert!(req.validate().is_err(), "65 字 cron 必须拒");

        let mut req = save_req();
        req.invoke_target = "t".repeat(256);
        assert!(req.validate().is_err(), "256 字目标必须拒");

        let mut req = save_req();
        req.remark = Some("备".repeat(501));
        assert!(req.validate().is_err(), "501 字备注必须拒");
    }

    /// 可选字段可以整段省略（老前端 / curl 只传必填项也必须能用）。
    #[test]
    fn job_save_req_deserializes_without_optional_fields() {
        let req: JobSaveReq = serde_json::from_str(
            r#"{"job_name":"n","cron_expr":"0 3 * * *","invoke_target":"clean_audit_log"}"#,
        )
        .expect("只传必填项必须能反序列化");
        assert!(req.job_group.is_none());
        assert!(req.params.is_none());
        assert!(req.status.is_none());
        assert!(req.remark.is_none());
        assert!(req.validate().is_ok());
    }

    /// 启停请求：0 / 1 都可反序列化；`status` 缺失必须报错（不能默认成启用）。
    #[test]
    fn job_status_req_requires_explicit_status() {
        let paused: JobStatusReq = serde_json::from_str(r#"{"status":0}"#).expect("暂停");
        assert_eq!(paused.status, 0);
        let running: JobStatusReq = serde_json::from_str(r#"{"status":1}"#).expect("启用");
        assert_eq!(running.status, 1);
        assert!(
            serde_json::from_str::<JobStatusReq>(r#"{"status":"0"}"#).is_err(),
            "字符串 \"0\" 不能悄悄当成 0"
        );
        assert!(
            serde_json::from_str::<JobStatusReq>("{}").is_err(),
            "缺 status 必须报错，不能默认成启用"
        );
    }

    /// 状态值只认 0 / 1，其它一律 400（这里是 `sys_job.status` 的唯一还原入口）。
    #[test]
    fn enable_status_accepts_only_zero_and_one() {
        assert_eq!(EnableStatus::from_i16(1).expect("启用").as_i16(), 1);
        assert_eq!(EnableStatus::from_i16(0).expect("暂停").as_i16(), 0);
        for bad in [2, -1, 127, i16::MIN] {
            let error = EnableStatus::from_i16(bad).expect_err("非法状态必须拒");
            assert_eq!(error.status, 400, "状态 {bad} 必须是 400");
            assert!(
                error.msg.contains("0") && error.msg.contains("1"),
                "报文要说明只接受 0/1：{}",
                error.msg
            );
        }
    }

    // ---------- ④ 处理器注册表（「能存不能跑」的另一半） ----------

    /// 五张日志表清理 + 不变量巡检必须都算「已注册」，未注册的处理器不能被存进 `sys_job`。
    #[test]
    fn registered_targets_cover_builtin_jobs_and_reject_unknown() {
        for target in [
            "clean_operation_log",
            "clean_audit_log",
            "clean_auth_log",
            "clean_security_log",
            "clean_password_log",
            crate::support::invariant::runner::TASK_NAME,
        ] {
            assert!(
                crate::task::is_registered_target(target),
                "内置处理器 {target} 必须算已注册"
            );
        }
        for bad in [
            "not_registered",
            "",
            "clean_operation_log ",
            "CLEAN_AUDIT_LOG",
        ] {
            assert!(
                !crate::task::is_registered_target(bad),
                "未注册处理器「{bad}」必须拒（大小写 / 空格都不做模糊匹配）"
            );
        }
    }

    // ---------- ⑤ cron：解析 + 下一次触发时刻 ----------

    /// 5 段（分 时 日 月 周）的天级任务：`0 3 * * *` 的下一次是当天 03:00:00。
    #[test]
    fn cron_five_field_next_after_is_three_am() {
        let schedule = CronSchedule::parse("0 3 * * *").expect("5 段必须能解析");
        let after = utc("2026-11-05T00:00:00Z");
        assert_eq!(
            schedule.next_after(after),
            Some(utc("2026-11-05T03:00:00Z")),
            "03:00 必须命中"
        );
        // 恰好 03:00:00 之后是第二天 03:00（严格晚于）。
        assert_eq!(
            schedule.next_after(utc("2026-11-05T03:00:00Z")),
            Some(utc("2026-11-06T03:00:00Z")),
            "next_after 是严格晚于，不能返回同一时刻"
        );
    }

    /// 6 段（秒 分 时 日 月 周）的秒级任务：`*/30 * * * * *` 下一次是 30 秒后。
    #[test]
    fn cron_six_field_supports_second_precision() {
        let schedule = CronSchedule::parse("*/30 * * * * *").expect("6 段必须能解析");
        assert_eq!(
            schedule.next_after(utc("2026-11-05T00:00:00Z")),
            Some(utc("2026-11-05T00:00:30Z"))
        );
        assert_eq!(
            schedule.next_after(utc("2026-11-05T00:00:30Z")),
            Some(utc("2026-11-05T00:01:00Z"))
        );
    }

    /// 跨月：`0 0 1 * *`（每月 1 日 0 点）从 10-05 出发必须算到 11-01。
    #[test]
    fn cron_month_rollover_is_handled() {
        let schedule = CronSchedule::parse("0 0 1 * *").expect("每月 1 日");
        assert_eq!(
            schedule.next_after(utc("2026-10-05T12:00:00Z")),
            Some(utc("2026-11-01T00:00:00Z"))
        );
    }

    /// 闰年：`0 0 29 2 *` 从 2026-03-01 出发，下一个是 2028-02-29。
    #[test]
    fn cron_leap_day_is_handled() {
        let schedule = CronSchedule::parse("0 0 29 2 *").expect("2 月 29 日");
        assert_eq!(
            schedule.next_after(utc("2026-03-01T00:00:00Z")),
            Some(utc("2028-02-29T00:00:00Z"))
        );
    }

    /// 永远无解的日子（2 月 30 日）必须返回 `None`，而不是死循环 / panic。
    #[test]
    fn cron_impossible_date_yields_no_next_time() {
        let schedule = CronSchedule::parse("0 0 30 2 *").expect("语法本身合法");
        assert_eq!(schedule.next_after(utc("2026-01-01T00:00:00Z")), None);
    }

    /// 段数不对（4 段 / 7 段 / 空串）必须拒 —— 这正是 `build_model` 判 422 的入口。
    #[test]
    fn cron_rejects_wrong_field_count() {
        for bad in ["", "   ", "0 3 * *", "0 0 3 * * * *"] {
            assert!(
                CronSchedule::parse(bad).is_err(),
                "「{bad}」段数不合法，必须拒"
            );
        }
    }

    /// 越界取值必须拒（分 60 / 时 24 / 日 32 / 月 13），否则「存进去也永远不跑」。
    #[test]
    fn cron_rejects_out_of_range_values() {
        for bad in ["60 * * * *", "0 24 * * *", "0 0 32 * *", "0 0 * 13 *"] {
            assert!(
                CronSchedule::parse(bad).is_err(),
                "越界表达式「{bad}」必须拒"
            );
        }
    }

    /// 列表 / 区间 / 步长三种常见写法都要认（运维在 `sys_job` 里就是这么填的）。
    #[test]
    fn cron_accepts_lists_ranges_and_steps() {
        for ok in [
            "0 3,15 * * *",
            "0 9-17 * * 1-5",
            "*/15 * * * *",
            "0 0-23/2 * * *",
            "30 2 * * 0",
        ] {
            assert!(
                CronSchedule::parse(ok).is_ok(),
                "常见写法「{ok}」必须能解析"
            );
        }
    }
}

// ==================== 真库用例（`SYS_DB_URL`）====================
//
// `sys_job` / `sys_job_log` 在补测前**一行 SQL 都没被测过**：分页过滤、名称唯一性
// （软删表）、启停写库这三条最容易被改坏的 SQL 全靠人工点页面。
//
// 跑法：
//
// ```bash
// export SYS_DB_URL='postgres://…/cloudpivot_test_xxx'
// cargo test --lib support::job::db_tests -- --test-threads=1
// ```
//
// 隔离手法：
// - 夹具用 `JOBT-` 前缀，开工先清、收尾再清（只删自己的行）；
// - 进用例前用 `SELECT current_database()` 与本用例期望的库名比对：`set_db` 是进程级
//   `OnceLock`（先到先得），若同进程里更早的用例装了别的库，这里**必须 SKIP 而不是假绿**
//   （教训见 `docs/测试卫生-真库连库入口清单.md`）。
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::support::job::model::{JobFilter, JobLogStatus, find_job_by_name, page_jobs};

    /// 夹具名称前缀（清场只认它）。
    const TAG: &str = "JOBT-";

    /// 本模块的真实库用例必须串行：三条用例都按 `JOBT-%` 前缀清场，并行时
    /// A 的开场 `DELETE` 会把 B 刚种下的行删掉（默认 4 线程下必然自撞）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 取锁 + 连库；`None` = 跳过（未配 `SYS_DB_URL` / 连不上 / 全局连接指向了别的库）。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        Some(guard)
    }

    /// 连库；`false` = 跳过（未配 `SYS_DB_URL` / 连不上 / 全局连接指向了别的库）。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("SYS_DB_URL") else {
                    println!("SKIP: 未设置 SYS_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_none() {
                    let mut config = DatabaseConfig::from_url(&url);
                    config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                    config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                    match Db::connect_with(&url, config).await {
                        Ok(connection) => {
                            if set_db(connection).is_err() {
                                println!("SKIP: set_db 失败（已被其它用例占用）");
                                return false;
                            }
                        }
                        Err(error) => {
                            println!("SKIP: SYS_DB_URL 连接失败（{error}）");
                            return false;
                        }
                    }
                }
                // 全局连接可能由更早的用例装成别的库（set_db 先到先得）→ 比对库名，别假绿。
                let expected = url.rsplit('/').next().unwrap_or_default().to_string();
                match db()
                    .scalar_with_params("SELECT current_database()", vec![])
                    .await
                {
                    Ok::<String, _>(actual) if actual == expected => true,
                    Ok(actual) => {
                        println!("SKIP: 全局连接指向 {actual}，本用例要求 {expected}");
                        false
                    }
                    Err(error) => {
                        println!("SKIP: 取 current_database() 失败（{error}）");
                        false
                    }
                }
            })
            .await
    }

    /// 清掉本前缀的夹具行（**只删自己造的**）。
    async fn cleanup() {
        if let Err(error) = db()
            .execute_with_params(
                "DELETE FROM sys_job WHERE job_name LIKE ?",
                vec![Value::from(format!("{TAG}%"))],
            )
            .await
        {
            panic!("清场失败：{error}");
        }
    }

    /// 造一条已落库的任务（`job_name` 带前缀 + 唯一后缀）。
    async fn seed(suffix: &str, group: &str, status: EnableStatus) -> SysJobModel {
        let mut job = SysJobModel {
            id: uuid::Uuid::now_v7(),
            job_name: format!("{TAG}{suffix}"),
            job_group: group.to_string(),
            cron_expr: "0 3 * * *".to_string(),
            invoke_target: "clean_operation_log".to_string(),
            params: None,
            status,
            last_run_at: None,
            next_run_at: None,
            remark: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            create_by: None,
            update_by: None,
            deleted_at: None,
        };
        job.insert(db()).await.expect("插入 sys_job 夹具");
        job
    }

    fn filter_name(name: &str) -> JobFilter {
        JobFilter {
            job_name: Some(name.to_string()),
            job_group: None,
            status: None,
        }
    }

    /// 新增 → 按名称查回 → 启停写库 → 软删后名称可复用（软删表的部分唯一索引口径）。
    #[tokio::test]
    async fn job_name_lookup_and_status_roundtrip() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup().await;

        let job = seed("A-回环", "LOG", EnableStatus::enabled).await;
        let found = find_job_by_name(&job.job_name)
            .await
            .expect("按名称查")
            .expect("新建的任务必须查得到");
        assert_eq!(found.id, job.id);
        assert_eq!(found.job_group, "LOG");
        assert_eq!(found.status.as_i16(), 1);

        // 停用 → 落库值必须是 0（不是 NULL、不是 2）。
        model::update_job_status(&job.id, EnableStatus::disabled, None)
            .await
            .expect("启停写库");
        let after = SysJobModel::find_or_none(db(), &job.id)
            .await
            .expect("按 id 查")
            .expect("任务还在");
        assert_eq!(after.status.as_i16(), 0, "停用后 sys_job.status 必须是 0");

        // 软删后：名称查不到（否则同名的第二个任务永远建不了）。
        SysJobModel::of(job.id).delete(db()).await.expect("软删");
        assert!(
            find_job_by_name(&job.job_name)
                .await
                .expect("查名称")
                .is_none(),
            "软删行不得参与名称唯一性校验"
        );
        let gone = SysJobModel::find_or_none(db(), &job.id)
            .await
            .expect("按 id 查");
        assert!(gone.is_none(), "软删后按 id 应查不到");

        cleanup().await;
        println!("[P2-3A·定时任务] 名称唯一性 / 启停写库 / 软删可复用 ✓");
    }

    /// 分页过滤：名称模糊 / 分组精确 / 状态三项都要真的进 SQL（写错就是「筛选无效」）。
    #[tokio::test]
    async fn page_jobs_applies_name_group_and_status_filters() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup().await;

        let enabled = seed("B-分页-启用", "LOG", EnableStatus::enabled).await;
        let _paused = seed("B-分页-暂停", "MAINT", EnableStatus::disabled).await;

        // 名称模糊
        let page = page_jobs(&filter_name(&format!("{TAG}B-分页")), 1, 50)
            .await
            .expect("按名称分页");
        assert_eq!(page.total, 2, "两条 JOBT-B-分页 都必须命中");

        // 分组精确 + 状态过滤
        let page = page_jobs(
            &JobFilter {
                job_name: Some(TAG.to_string()),
                job_group: Some("MAINT".to_string()),
                status: Some(0),
            },
            1,
            50,
        )
        .await
        .expect("分组 + 状态分页");
        assert_eq!(page.total, 1, "分组 MAINT + 暂停 只应命中 1 条");
        assert_eq!(page.list[0].job_group, "MAINT");
        assert_eq!(page.list[0].status.as_i16(), 0);

        // 分组两端的空白要按「未提供」处理（不参与过滤）。
        let page = page_jobs(
            &JobFilter {
                job_name: Some(TAG.to_string()),
                job_group: Some("   ".to_string()),
                status: None,
            },
            1,
            50,
        )
        .await
        .expect("空白分组 = 不过滤");
        assert_eq!(page.total, 2, "空白分组不能当成「等于空串」");
        assert!(
            page.list.iter().any(|row| row.id == enabled.id),
            "启用那条必须在本页里"
        );

        cleanup().await;
        println!("[P2-3A·定时任务] 名称模糊 / 分组精确 / 状态过滤 ✓");
    }

    /// 执行日志分页：按 `job_id` / 状态过滤，并验证「任务删了日志还在」（快照字段独立）。
    #[tokio::test]
    async fn page_job_logs_filters_by_job_and_keeps_snapshot() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup().await;
        let job = seed("C-日志", "LOG", EnableStatus::enabled).await;
        db().execute_with_params(
            "DELETE FROM sys_job_log WHERE job_name LIKE ?",
            vec![Value::from(format!("{TAG}%"))],
        )
        .await
        .expect("清日志夹具");

        let now = chrono::Utc::now();
        for (status, message) in [
            (JobLogStatus::success, Some("清理 12 行".to_string())),
            (JobLogStatus::failed, None),
        ] {
            let mut log = SysJobLogModel {
                id: uuid::Uuid::now_v7(),
                job_id: job.id,
                job_name: job.job_name.clone(),
                invoke_target: job.invoke_target.clone(),
                params: None,
                status,
                message,
                error: None,
                started_at: now,
                finished_at: Some(now),
                duration_ms: Some(7),
                created_at: now,
            };
            log.insert(db()).await.expect("插入 sys_job_log 夹具");
        }

        let all = model::page_job_logs(
            &model::JobLogFilter {
                job_id: Some(job.id),
                job_name: None,
                status: None,
            },
            1,
            50,
        )
        .await
        .expect("按任务 id 分页");
        assert_eq!(all.total, 2, "两条日志都必须命中 job_id 过滤");

        let failed = model::page_job_logs(
            &model::JobLogFilter {
                job_id: Some(job.id),
                job_name: Some(TAG.to_string()),
                status: Some(0),
            },
            1,
            50,
        )
        .await
        .expect("按状态分页");
        assert_eq!(failed.total, 1, "status=0 只应命中失败那条");
        assert_eq!(failed.list[0].status.as_i16(), 0);
        assert_eq!(
            failed.list[0].job_name, job.job_name,
            "日志带任务名快照，任务改名前也能读懂历史"
        );

        // 任务被软删后，历史日志仍在（弱引用、无外键，这是设计）。
        SysJobModel::of(job.id)
            .delete(db())
            .await
            .expect("软删任务");
        let still = model::page_job_logs(
            &model::JobLogFilter {
                job_id: Some(job.id),
                job_name: None,
                status: None,
            },
            1,
            50,
        )
        .await
        .expect("任务软删后查日志");
        assert_eq!(still.total, 2, "历史执行日志不能随任务删除而消失");

        db().execute_with_params(
            "DELETE FROM sys_job_log WHERE job_name LIKE ?",
            vec![Value::from(format!("{TAG}%"))],
        )
        .await
        .expect("清日志夹具");
        cleanup().await;
        println!("[P2-3A·定时任务] 执行日志分页 / 快照独立保留 ✓");
    }
}
