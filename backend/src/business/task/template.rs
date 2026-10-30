//! 任务模板（批次 E · **E11**）：CRUD + 一键建任务（含子任务）+ 周期排期字段。
//!
//! 依据：卡面 `t_834dcc63`「一、任务模板（最简）」「二、周期任务」（表的四列）。
//! 迁移：[`V0134`] 建表 + 菜单，[`V0135`] 加周期四列并扩 `biz_task.source` 值域。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏路径**不写** `/api`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/task/templates` | `business:task:read` | 模板列表（关键词 / 只看启用） |
//! | POST | `/business/task/template` | `business:task:create` | 新建模板 |
//! | PUT | `/business/task/template/{id}` | `business:task:update` | 编辑模板（含周期开关） |
//! | DELETE | `/business/task/template/{id}` | `business:task:delete` | 软删模板 |
//! | POST | `/business/task/from-template/{id}` | `business:task:create` | 一键建任务（含子任务） |
//!
//! # 三条设计口径
//!
//! 1. **不新增权限点**：模板是任务域内的东西，入口沿用 `business:task:*` 四个既有码
//!    （V0103 已授给所有角色）。新增权限点要另开授权迁移，本卡 KPI 里没有它，
//!    而且模板不是权限边界 —— 谁能建任务，谁就能建模板。
//! 2. **`owner_id` 三级兜底**：请求体 → 模板 `owner_id` → 模板 `create_by` → 操作人。
//!    `biz_task.owner_id` 是 `NOT NULL REFERENCES sys_user(id)`，不允许解析出空。
//! 3. **子任务只到第二层**：`biz_task` 的深度上限是 2（父任务自身不能再有 parent_id），
//!    所以模板的子任务一律挂在新建的父任务下，**不再递归**（模板里没有孙任务的概念）。
//!
//! # 与周期任务的分工
//!
//! 本模块只负责「模板长什么样」和「照着模板建一次」；
//! 「什么时候建」在 [`super::recurring`]（cron 解析 + 每 5 分钟扫描）。
//! 两边共用同一个落库内核 [`create_from_template`]，所以
//! 「手动一键建」与「周期自动建」产出的任务**逐字段一致**（只有 `source` 不同）。

// SCOPE-EXEMPT: 任务模板是**全局配置**（不是谁的部门数据）—— 模板列表按「登录用户都能看到全部
// 模板」的口径出参，写权限由 `business:task:create/update/delete` 三个权限码隔离。
// 与流程定义（`business/flow/model.rs`）、任务状态字典（`biz_task_status_def`）同一豁免口径：
// 按部门 / 负责人裁剪模板会让「张三建的周报模板李四用不了」，而模板本身不含任何任务数据
// （生成出来的任务走 `biz_task` 的可见性谓词，一处不少）。

use alun::prelude::chrono::{DateTime, NaiveDate, Utc};
use alun::prelude::*;
use alun::uuid::Uuid;
use axum::Json;
use axum::extract::{Path, Query};
use serde::{Deserialize, Serialize};

use crate::business::task::dto;
use crate::business::task::handler::{current_user, parse_uuid_field};
use crate::business::task::model;
use crate::business::task::recurring;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 模板名最大长度。
pub const MAX_NAME_CHARS: usize = 100;
/// 模板标题最大长度（与 `biz_task.title` 同域）。
pub const MAX_TITLE_CHARS: usize = 200;
/// 模板描述最大长度（与 `biz_task.description` 的截断口径一致）。
pub const MAX_DESCRIPTION_CHARS: usize = 20000;
/// 单个模板的子任务上限。
pub const MAX_SUBTASKS: usize = 20;
/// 列表一次最多返回的模板条数（模板是小数据集，不分页）。
pub const LIST_LIMIT: i64 = 200;
/// 生成任务的来源：一键建 = `SELF`，周期调度 = `RECURRING`。
pub const SOURCE_TEMPLATE: &str = "SELF";
/// 周期调度生成任务的来源。
pub const SOURCE_RECURRING: &str = "RECURRING";

/// 模板列清单（`TemplateRow::from_row` 与所有 SELECT 共用）。
pub const TEMPLATE_COLUMNS: &str = "\
    id, name, title, description, priority, estimate_hours, need_review, subtasks, \
    owner_id, project_id, cron_expr, enabled, next_run_at, last_run_at, \
    created_at, updated_at, create_by";

// ==================== 请求 / 响应 DTO ====================

/// 模板列表查询（`GET /business/task/templates`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskTemplateListQuery {
    /// 关键词（模板名 / 标题模糊匹配）。
    pub keyword: Option<String>,
    /// 只看已开启周期的模板（`true` 时过滤 `enabled = true AND cron_expr IS NOT NULL`）。
    #[serde(default)]
    pub enabled_only: Option<bool>,
}

/// 模板里的子任务（落库形态）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TemplateSubtaskReq {
    /// 子任务标题。
    pub title: String,
    /// 子任务优先级（缺省 = 父任务优先级）。
    #[serde(default)]
    pub priority: Option<String>,
}

/// 模板里的子任务（下发形态，`priority` 恒有值）。
#[derive(Debug, Clone, Serialize)]
pub struct TemplateSubtaskRes {
    /// 子任务标题。
    pub title: String,
    /// 子任务优先级。
    pub priority: String,
}

/// 新建 / 编辑模板（`POST /business/task/template`、`PUT /business/task/template/{id}`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskTemplateSaveReq {
    /// 模板名（必填，≤100 字符，未软删范围内唯一）。
    pub name: String,
    /// 生成任务的标题（必填，≤200 字符）。
    pub title: String,
    /// 生成任务的描述。
    pub description: Option<String>,
    /// 生成任务的优先级（缺省 `P2`）。
    pub priority: Option<String>,
    /// 生成任务的预估工时。
    pub estimate_hours: Option<f64>,
    /// 生成任务是否需要验收（缺省 `false`）。
    pub need_review: Option<bool>,
    /// 子任务数组（缺省 = 无子任务；显式 `[]` = 清空）。
    #[serde(default)]
    pub subtasks: Option<Vec<TemplateSubtaskReq>>,
    /// 生成任务的负责人（缺省 = 模板创建人）。
    pub owner_id: Option<String>,
    /// 生成任务归属项目（可空）。
    pub project_id: Option<String>,
    /// 标准 cron（5 段；传空串 = 清掉周期）。
    pub cron_expr: Option<String>,
    /// 周期开关（缺省 false；置 true 时必须已给 cron）。
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// 模板响应。
#[derive(Debug, Clone, Serialize)]
pub struct TaskTemplateRes {
    /// 主键。
    pub id: String,
    /// 模板名。
    pub name: String,
    /// 生成任务的标题。
    pub title: String,
    /// 生成任务的描述（空串 = 无）。
    pub description: String,
    /// 生成任务的优先级。
    pub priority: String,
    /// 预估工时。
    pub estimate_hours: Option<f64>,
    /// 是否需要验收。
    pub need_review: bool,
    /// 子任务（含各自优先级）。
    pub subtasks: Vec<TemplateSubtaskRes>,
    /// 生成任务的负责人（空串 = 用创建人兜底）。
    pub owner_id: String,
    /// 归属项目（空串 = 不归入项目）。
    pub project_id: String,
    /// cron 表达式（空串 = 无）。
    pub cron_expr: String,
    /// 周期开关。
    pub enabled: bool,
    /// 下次生成时间（UTC）。
    pub next_run_at: Option<DateTime<Utc>>,
    /// 上次生成时间（UTC）。
    pub last_run_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 模板列表响应。
#[derive(Debug, Clone, Serialize)]
pub struct TaskTemplateListRes {
    /// 模板列表。
    pub list: Vec<TaskTemplateRes>,
    /// 总数（= `list.len()`，模板不分页）。
    pub total: i64,
}

/// 一键建任务（`POST /business/task/from-template/{id}`）。
///
/// 所有字段可缺席：缺席即用模板值。给了就覆盖（这是「从模板创建」的语义 ——
/// 模板是**默认值集合**，不是不可改的硬约束）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskFromTemplateReq {
    /// 覆盖标题。
    pub title: Option<String>,
    /// 覆盖负责人。
    pub owner_id: Option<String>,
    /// 覆盖项目。
    pub project_id: Option<String>,
    /// 计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 截止日（缺省 = 模板无该列 → 本周五口径由 `model::create_task` 之前补）。
    pub due_date: Option<NaiveDate>,
}

/// 一键建任务响应。
#[derive(Debug, Clone, Serialize)]
pub struct TaskFromTemplateRes {
    /// 新任务 id。
    pub id: String,
    /// 新任务编号。
    pub task_no: String,
    /// 新任务标题。
    pub title: String,
    /// 子任务 id 列表。
    pub subtask_ids: Vec<String>,
    /// 来源（`SELF` / `RECURRING`）。
    pub source: String,
}

// ==================== 行结构 ====================

/// 模板行（含周期排期字段）。
#[derive(Debug, Clone)]
pub struct TemplateRow {
    /// 主键。
    pub id: Uuid,
    /// 模板名。
    pub name: String,
    /// 生成任务标题。
    pub title: String,
    /// 生成任务描述。
    pub description: Option<String>,
    /// 优先级。
    pub priority: String,
    /// 预估工时。
    pub estimate_hours: Option<f64>,
    /// 是否需要验收。
    pub need_review: bool,
    /// 子任务 jsonb 原文（`[...]`）。
    pub subtasks: String,
    /// 负责人。
    pub owner_id: Option<Uuid>,
    /// 项目。
    pub project_id: Option<Uuid>,
    /// cron 表达式。
    pub cron_expr: Option<String>,
    /// 周期开关。
    pub enabled: bool,
    /// 下次生成时间。
    pub next_run_at: Option<DateTime<Utc>>,
    /// 上次生成时间。
    pub last_run_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
}

impl TemplateRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            name: row.get_typed("name")?,
            title: row.get_typed("title")?,
            description: row.get_typed("description")?,
            priority: row.get_typed("priority")?,
            estimate_hours: row.get_typed("estimate_hours")?,
            need_review: row.get_typed("need_review")?,
            subtasks: row.get_typed("subtasks")?,
            owner_id: row.get_typed("owner_id")?,
            project_id: row.get_typed("project_id")?,
            cron_expr: row.get_typed("cron_expr")?,
            enabled: row.get_typed("enabled")?,
            next_run_at: row.get_typed("next_run_at")?,
            last_run_at: row.get_typed("last_run_at")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
        })
    }

    /// 解析子任务（jsonb → 结构体）。
    ///
    /// 库里脏数据（非数组 / 元素缺 title）→ **跳过该元素**而不是整条接口 500：
    /// 模板是「配置」，一条坏元素不该让整个模板不可用（解析失败的元素进日志）。
    pub fn subtask_list(&self) -> Vec<TemplateSubtaskRes> {
        match parse_subtasks_json(&self.subtasks) {
            Ok(list) => list,
            Err(message) => {
                tracing::warn!(template_id = %self.id, %message, "模板子任务 JSON 解析失败，按空处理");
                Vec::new()
            }
        }
    }
}

// ==================== 纯逻辑（可无库单测） ====================

/// 解析 `subtasks` jsonb 文本 → 子任务列表。
///
/// # 规则
///
/// - 空串 / `null` / `[]` → 空列表；
/// - 元素缺 `title` 或标题全空白 → **跳过该元素**；
/// - 标题截断到 [`MAX_TITLE_CHARS`]，优先级非法 → 落 `P2`；
/// - 元素数量超过 [`MAX_SUBTASKS`] → 只取前 [`MAX_SUBTASKS`] 条。
///
/// # 异常情况
///
/// 文本不是合法 JSON 数组 → `Err(说明)`。
pub fn parse_subtasks_json(raw: &str) -> std::result::Result<Vec<TemplateSubtaskRes>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(Vec::new());
    }
    let parsed: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|error| format!("子任务不是合法 JSON：{error}"))?;
    let array = match parsed {
        serde_json::Value::Array(items) => items,
        _ => return Err("子任务必须是 JSON 数组".to_string()),
    };
    let mut out = Vec::new();
    for item in array {
        if out.len() >= MAX_SUBTASKS {
            break;
        }
        let title = item
            .get("title")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .unwrap_or("");
        if title.is_empty() {
            continue;
        }
        let priority = item
            .get("priority")
            .and_then(|value| value.as_str())
            .and_then(|raw| dto::normalize_priority(Some(raw)))
            .unwrap_or_else(|| "P2".to_string());
        out.push(TemplateSubtaskRes {
            title: truncate_chars(title, MAX_TITLE_CHARS),
            priority,
        });
    }
    Ok(out)
}

/// 把子任务列表编码成 jsonb 文本（落库用）。
///
/// 恒返回 `[...]`（空列表 = `[]`），**不返回 `null`** ——
/// 卡面要求 `subtasks jsonb`，`null` 会让前端每次都要判空。
pub fn subtasks_to_json(items: &[TemplateSubtaskRes]) -> String {
    let payload: Vec<serde_json::Value> = items
        .iter()
        .map(|item| serde_json::json!({ "title": item.title, "priority": item.priority }))
        .collect();
    serde_json::Value::Array(payload).to_string()
}

/// 校验并归一化模板入参（新建 / 编辑共用）。
///
/// # 异常情况
///
/// - 400 + `63502`：模板名 / 标题 / 优先级 / 工时 / 子任务非法；
/// - 400 + `63503`：cron 表达式非法。
pub fn normalize_save_req(
    req: &TaskTemplateSaveReq,
) -> std::result::Result<NormalizedTemplate, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(template_invalid("模板名不能为空".to_string()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(template_invalid(format!(
            "模板名过长：{} 字符（上限 {MAX_NAME_CHARS}）",
            name.chars().count()
        )));
    }
    let title = req.title.trim();
    if title.is_empty() {
        return Err(template_invalid("任务标题不能为空".to_string()));
    }
    let priority = match req.priority.as_deref().map(str::trim) {
        None | Some("") => "P2".to_string(),
        Some(raw) => dto::normalize_priority(Some(raw)).ok_or_else(|| {
            template_invalid(format!("优先级取值非法：{raw}（应为 P0 / P1 / P2 / P3）"))
        })?,
    };
    if let Some(hours) = req.estimate_hours
        && (!hours.is_finite() || hours < 0.0 || hours > 9999.0)
    {
        return Err(template_invalid(format!(
            "预估工时非法：{hours}（应为 0 ~ 9999 小时）"
        )));
    }
    let mut subtasks: Vec<TemplateSubtaskRes> = Vec::new();
    if let Some(items) = req.subtasks.as_ref() {
        if items.len() > MAX_SUBTASKS {
            return Err(template_invalid(format!(
                "子任务过多：{} 条（上限 {MAX_SUBTASKS}）",
                items.len()
            )));
        }
        for item in items {
            let raw_title = item.title.trim();
            if raw_title.is_empty() {
                return Err(template_invalid("子任务标题不能为空".to_string()));
            }
            let child_priority = match item.priority.as_deref().map(str::trim) {
                None | Some("") => priority.clone(),
                Some(raw) => dto::normalize_priority(Some(raw)).ok_or_else(|| {
                    template_invalid(format!(
                        "子任务优先级取值非法：{raw}（应为 P0 / P1 / P2 / P3）"
                    ))
                })?,
            };
            subtasks.push(TemplateSubtaskRes {
                title: truncate_chars(raw_title, MAX_TITLE_CHARS),
                priority: child_priority,
            });
        }
    }
    let cron_expr = match req.cron_expr.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            recurring::parse_cron(raw).map_err(|message| {
                ApiError::new(
                    400,
                    error_code::TASK_TEMPLATE_CRON_INVALID,
                    format!("cron 表达式非法：{message}"),
                )
            })?;
            Some(raw.to_string())
        }
    };
    let enabled = req.enabled.unwrap_or(false);
    if enabled && cron_expr.is_none() {
        return Err(ApiError::new(
            400,
            error_code::TASK_TEMPLATE_CRON_INVALID,
            "开启周期必须同时填写 cron 表达式（例：每天 9 点 = `0 9 * * *`）".to_string(),
        ));
    }
    let description = match req.description.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(text) => Some(truncate_chars(text, MAX_DESCRIPTION_CHARS)),
    };
    Ok(NormalizedTemplate {
        name: name.to_string(),
        title: truncate_chars(title, MAX_TITLE_CHARS),
        description,
        priority,
        estimate_hours: req.estimate_hours,
        need_review: req.need_review.unwrap_or(false),
        subtasks,
        cron_expr,
        enabled,
    })
}

/// 归一化后的模板字段（[`normalize_save_req`] 的产物）。
#[derive(Debug, Clone)]
pub struct NormalizedTemplate {
    /// 模板名。
    pub name: String,
    /// 生成任务标题。
    pub title: String,
    /// 生成任务描述。
    pub description: Option<String>,
    /// 优先级。
    pub priority: String,
    /// 预估工时。
    pub estimate_hours: Option<f64>,
    /// 是否需要验收。
    pub need_review: bool,
    /// 子任务。
    pub subtasks: Vec<TemplateSubtaskRes>,
    /// cron（`None` = 无周期）。
    pub cron_expr: Option<String>,
    /// 周期开关。
    pub enabled: bool,
}

/// 400 + `63502`（模板字段非法）。
pub fn template_invalid(message: String) -> ApiError {
    ApiError::new(400, error_code::TASK_TEMPLATE_INVALID, message)
}

/// 404 + `63501`（模板不存在 / 已软删）。
pub fn template_not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::TASK_TEMPLATE_NOT_FOUND,
        "任务模板不存在或已删除",
    )
}

/// 按字符数截断（不切坏 UTF-8）。
pub fn truncate_chars(raw: &str, limit: usize) -> String {
    if raw.chars().count() <= limit {
        return raw.to_string();
    }
    raw.chars().take(limit).collect()
}

// ==================== 数据访问 ====================

/// 模板列表（未软删；关键词模糊匹配 name / title）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn list_templates(
    keyword: Option<&str>,
    enabled_only: bool,
) -> DbResult<Vec<TemplateRow>> {
    let mut sql =
        format!("SELECT {TEMPLATE_COLUMNS} FROM biz_task_template WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(raw) = keyword.map(str::trim).filter(|text| !text.is_empty()) {
        let pattern = format!("%{}%", model::escape_like(raw));
        sql.push_str(" AND (name ILIKE ? OR title ILIKE ?)");
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    if enabled_only {
        sql.push_str(" AND enabled AND cron_expr IS NOT NULL AND cron_expr <> ''");
    }
    sql.push_str(" ORDER BY updated_at DESC, id DESC LIMIT ?");
    params.push(Value::I64(LIST_LIMIT));
    let rows = db().find_with_params(&sql, params).await?;
    rows.iter().map(TemplateRow::from_row).collect()
}

/// 按 id 查模板（未软删）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_template(id: Uuid) -> DbResult<Option<TemplateRow>> {
    let sql = format!(
        "SELECT {TEMPLATE_COLUMNS} FROM biz_task_template \
          WHERE id = ?::uuid AND deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    match rows.first() {
        Some(row) => Ok(Some(TemplateRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 模板名是否已被占用（未软删范围内唯一）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn name_conflict(name: &str, exclude: Option<Uuid>) -> DbResult<bool> {
    let rows = match exclude {
        Some(id) => {
            db().find_with_params(
                "SELECT 1 AS hit FROM biz_task_template \
                  WHERE deleted_at IS NULL AND name = ? AND id <> ?::uuid LIMIT 1",
                vec![Value::Text(name.to_string()), Value::from(&id)],
            )
            .await?
        }
        None => {
            db().find_with_params(
                "SELECT 1 AS hit FROM biz_task_template \
                  WHERE deleted_at IS NULL AND name = ? LIMIT 1",
                vec![Value::Text(name.to_string())],
            )
            .await?
        }
    };
    Ok(!rows.is_empty())
}

/// 新建模板。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn insert_template(
    id: Uuid,
    fields: &NormalizedTemplate,
    owner_id: Option<Uuid>,
    project_id: Option<Uuid>,
    next_run_at: Option<DateTime<Utc>>,
    operator: Uuid,
) -> DbResult<()> {
    let subtasks = subtasks_to_json(&fields.subtasks);
    db().execute_with_params(
        "INSERT INTO biz_task_template \
             (id, name, title, description, priority, estimate_hours, need_review, subtasks, \
              owner_id, project_id, cron_expr, enabled, next_run_at, last_run_at, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, ?, ?::numeric, ?, ?::jsonb, \
                 ?::uuid, ?::uuid, ?, ?, ?::timestamptz, NULL, \
                 now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&id),
            Value::Text(fields.name.clone()),
            Value::Text(fields.title.clone()),
            Value::from(fields.description.clone()),
            Value::Text(fields.priority.clone()),
            Value::from(fields.estimate_hours),
            Value::Bool(fields.need_review),
            Value::Json(subtasks),
            Value::from(owner_id.as_ref()),
            Value::from(project_id.as_ref()),
            Value::from(fields.cron_expr.clone()),
            Value::Bool(fields.enabled),
            Value::from(next_run_at),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(())
}

/// 编辑模板（全量覆盖可编辑字段）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn update_template(
    id: Uuid,
    fields: &NormalizedTemplate,
    owner_id: Option<Uuid>,
    project_id: Option<Uuid>,
    next_run_at: Option<DateTime<Utc>>,
    operator: Uuid,
) -> DbResult<u64> {
    let subtasks = subtasks_to_json(&fields.subtasks);
    db().execute_with_params(
        "UPDATE biz_task_template \
            SET name = ?, title = ?, description = ?, priority = ?, estimate_hours = ?::numeric, \
                need_review = ?, subtasks = ?::jsonb, owner_id = ?::uuid, project_id = ?::uuid, \
                cron_expr = ?, enabled = ?, next_run_at = ?::timestamptz, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::Text(fields.name.clone()),
            Value::Text(fields.title.clone()),
            Value::from(fields.description.clone()),
            Value::Text(fields.priority.clone()),
            Value::from(fields.estimate_hours),
            Value::Bool(fields.need_review),
            Value::Json(subtasks),
            Value::from(owner_id.as_ref()),
            Value::from(project_id.as_ref()),
            Value::from(fields.cron_expr.clone()),
            Value::Bool(fields.enabled),
            Value::from(next_run_at),
            Value::from(&operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删模板。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn soft_delete_template(id: Uuid, operator: Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_task_template \
            SET deleted_at = now(), updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&operator), Value::from(&id)],
    )
    .await
}

// ==================== 落库内核：照模板建任务 ====================

/// 照模板建一次任务（含子任务）——**手动一键建与周期调度共用**。
///
/// # 参数
///
/// - `tpl`：模板行；
/// - `actor`：操作人（手动 = 当前登录人；周期 = 模板 `create_by` 兜底、再兜超级管理员外的
///   第一个可用人？见她下面的说明：周期调度拿不到登录态，用 `tpl.create_by`）；
/// - `req`：覆盖项（周期调度传空结构体）；
/// - `source`：生成任务的来源标记（`SELF` / `RECURRING`）。
///
/// # 返回值
///
/// 新任务 id / 编号 / 标题 + 子任务 id 列表。
///
/// # 异常情况
///
/// - `DbError::InvalidValue`：负责人 / 项目 / 子任务落库前置条件不满足；
/// - 数据库异常 → [`DbError`]。
pub async fn create_from_template(
    tpl: &TemplateRow,
    actor: Uuid,
    req: &TaskFromTemplateReq,
    source: &str,
) -> DbResult<TaskFromTemplateRes> {
    let title = match req.title.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => truncate_chars(raw, MAX_TITLE_CHARS),
        _ => tpl.title.clone(),
    };
    // 负责人三级兜底：请求体 → 模板 owner_id → 模板创建人 → 操作人
    let owner_id = match req.owner_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Uuid::parse_str(raw)
            .map_err(|_| DbError::InvalidValue(format!("负责人 ID 不是合法 UUID：{raw}")))?,
        _ => tpl.owner_id.or(tpl.create_by).unwrap_or(actor),
    };
    let project_id = match req.project_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(
            Uuid::parse_str(raw)
                .map_err(|_| DbError::InvalidValue(format!("项目 ID 不是合法 UUID：{raw}")))?,
        ),
        _ => tpl.project_id,
    };
    if !model::user_exists(owner_id).await? {
        return Err(DbError::InvalidValue(format!(
            "负责人不存在或已停用：{owner_id}"
        )));
    }
    let dept_id = model::user_dept(owner_id).await?;
    let today = model::today_cst();
    let due_date = req
        .due_date
        .or_else(|| Some(model::default_due_date(today)));
    let start_date = req.start_date;

    let parent_id = Uuid::now_v7();
    let month = model::month_key(today);
    let template_id = tpl.id;
    let subtasks = tpl.subtask_list();
    // 事务闭包要 `'static`，所以把模板字段先克隆成**自有值**再 move 进去
    let description = tpl.description.clone();
    let priority = tpl.priority.clone();
    let need_review = tpl.need_review;
    let estimate_hours = tpl.estimate_hours;
    let source_owned = source.to_string();
    let title_out = title.clone();

    let result = db()
        .transaction(|tx| async move {
            let new = model::NewTask {
                id: parent_id,
                title: title.clone(),
                description: description.clone(),
                priority: priority.clone(),
                need_review,
                owner_id,
                creator_id: actor,
                assigner_id: (owner_id != actor).then_some(actor),
                dept_id,
                parent_id: None,
                project_id,
                start_date,
                due_date,
                estimate_hours,
                source: source_owned.clone(),
                add_creator_as_collaborator: owner_id != actor,
            };
            let task_no = model::create_task(&tx, &new, actor, &month).await?;
            let mut subtask_ids: Vec<String> = Vec::with_capacity(subtasks.len());
            for item in &subtasks {
                let child_id = Uuid::now_v7();
                let child = model::NewTask {
                    id: child_id,
                    title: item.title.clone(),
                    description: None,
                    priority: item.priority.clone(),
                    need_review: false,
                    owner_id,
                    creator_id: actor,
                    assigner_id: None,
                    dept_id,
                    parent_id: Some(parent_id),
                    project_id,
                    start_date: None,
                    due_date,
                    estimate_hours: None,
                    source: source_owned.clone(),
                    add_creator_as_collaborator: false,
                };
                model::create_task(&tx, &child, actor, &month).await?;
                subtask_ids.push(child_id.to_string());
            }
            Ok::<(String, Vec<String>), DbError>((task_no, subtask_ids))
        })
        .await?;

    tracing::info!(
        template_id = %template_id,
        task_id = %parent_id,
        source = %source,
        subtasks = result.1.len(),
        "已按模板创建任务"
    );
    Ok(TaskFromTemplateRes {
        id: parent_id.to_string(),
        task_no: result.0,
        title: title_out,
        subtask_ids: result.1,
        source: source.to_string(),
    })
}

// ==================== handler ====================

/// 模板列表。
///
/// # 异常情况
///
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/task/templates",
    method = "GET",
    permission = "business:task:read"
)]
#[alun::get("/business/task/templates")]
pub async fn list(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
    Query(query): Query<TaskTemplateListQuery>,
) -> ResResult<TaskTemplateListRes> {
    let rows = list_templates(
        query.keyword.as_deref(),
        query.enabled_only.unwrap_or(false),
    )
    .await
    .map_err(|error| utils::db_error(error, "查询任务模板"))?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(template_res(row));
    }
    Ok(Res::ok(TaskTemplateListRes {
        total: list.len() as i64,
        list,
    }))
}

/// 新建模板。
///
/// # 异常情况
///
/// - 400 + `63502` / `63503`：字段 / cron 非法，或模板名重复；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/task/template",
    method = "POST",
    permission = "business:task:create"
)]
#[alun::post("/business/task/template")]
pub async fn create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<TaskTemplateSaveReq>,
) -> ResResult<TaskTemplateRes> {
    let me = current_user(&claims)?;
    let fields = normalize_save_req(&req)?;
    if name_conflict(&fields.name, None)
        .await
        .map_err(|error| utils::db_error(error, "校验模板名"))?
    {
        return Err(template_invalid(format!(
            "模板名已被占用：{}（同名的模板未删除前不能再建）",
            fields.name
        )));
    }
    let owner_id = parse_optional_uuid(req.owner_id.as_deref(), "负责人")?;
    let project_id = parse_optional_uuid(req.project_id.as_deref(), "项目")?;
    let next_run_at = match fields.cron_expr.as_deref() {
        Some(expr) if fields.enabled => recurring::next_run_after(expr, Utc::now()),
        _ => None,
    };
    let id = Uuid::now_v7();
    insert_template(id, &fields, owner_id, project_id, next_run_at, me)
        .await
        .map_err(|error| utils::db_error(error, "新建任务模板"))?;
    let row = find_template(id)
        .await
        .map_err(|error| utils::db_error(error, "回读任务模板"))?
        .ok_or_else(template_not_found)?;
    tracing::info!(template_id = %id, name = %row.name, enabled = row.enabled, "任务模板已创建");
    Ok(Res::ok(template_res(&row)))
}

/// 编辑模板。
///
/// 周期语义：`enabled` 由 false 翻成 true（或 cron 变了）时**重算 `next_run_at`**；
/// 停用则把 `next_run_at` 清空（停用期间不排期，重新启用时从「现在」起算下一次）。
///
/// # 异常情况
///
/// - 404 + `63501`：模板不存在；
/// - 400 + `63502` / `63503`：字段 / cron 非法，或模板名与他人重复；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/task/template/{id}",
    method = "PUT",
    permission = "business:task:update"
)]
#[alun::put("/business/task/template/{id}")]
pub async fn update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskTemplateSaveReq>,
) -> ResResult<TaskTemplateRes> {
    let me = current_user(&claims)?;
    let template_id = parse_uuid_field(&id, "模板")?;
    let existing = find_template(template_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务模板"))?
        .ok_or_else(template_not_found)?;
    let fields = normalize_save_req(&req)?;
    if name_conflict(&fields.name, Some(template_id))
        .await
        .map_err(|error| utils::db_error(error, "校验模板名"))?
    {
        return Err(template_invalid(format!(
            "模板名已被占用：{}（同名的模板未删除前不能重名）",
            fields.name
        )));
    }
    let owner_id = parse_optional_uuid(req.owner_id.as_deref(), "负责人")?;
    let project_id = parse_optional_uuid(req.project_id.as_deref(), "项目")?;
    let next_run_at = match fields.cron_expr.as_deref() {
        Some(expr) if fields.enabled => {
            let cron_changed = existing.cron_expr.as_deref() != Some(expr);
            if cron_changed || !existing.enabled || existing.next_run_at.is_none() {
                recurring::next_run_after(expr, Utc::now())
            } else {
                existing.next_run_at
            }
        }
        _ => None,
    };
    let affected = update_template(template_id, &fields, owner_id, project_id, next_run_at, me)
        .await
        .map_err(|error| utils::db_error(error, "编辑任务模板"))?;
    if affected == 0 {
        return Err(template_not_found());
    }
    let row = find_template(template_id)
        .await
        .map_err(|error| utils::db_error(error, "回读任务模板"))?
        .ok_or_else(template_not_found)?;
    tracing::info!(
        template_id = %template_id,
        enabled = row.enabled,
        next_run_at = ?row.next_run_at,
        "任务模板已更新"
    );
    Ok(Res::ok(template_res(&row)))
}

/// 软删模板。
///
/// # 异常情况
///
/// - 404 + `63501`：模板不存在；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/task/template/{id}",
    method = "DELETE",
    permission = "business:task:delete"
)]
#[alun::delete("/business/task/template/{id}")]
pub async fn delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<serde_json::Value> {
    let me = current_user(&claims)?;
    let template_id = parse_uuid_field(&id, "模板")?;
    let affected = soft_delete_template(template_id, me)
        .await
        .map_err(|error| utils::db_error(error, "删除任务模板"))?;
    if affected == 0 {
        return Err(template_not_found());
    }
    tracing::info!(template_id = %template_id, operator = %me, "任务模板已软删");
    Ok(Res::ok(
        serde_json::json!({ "id": template_id.to_string() }),
    ))
}

/// 一键建任务（含子任务）。
///
/// # 异常情况
///
/// - 404 + `63501`：模板不存在；
/// - 400 + `63009`：负责人不存在；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/task/from-template/{id}",
    method = "POST",
    permission = "business:task:create"
)]
#[alun::post("/business/task/from-template/{id}")]
pub async fn from_template(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskFromTemplateReq>,
) -> ResResult<TaskFromTemplateRes> {
    let me = current_user(&claims)?;
    let template_id = parse_uuid_field(&id, "模板")?;
    let tpl = find_template(template_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务模板"))?
        .ok_or_else(template_not_found)?;
    let created = create_from_template(&tpl, me, &req, SOURCE_TEMPLATE)
        .await
        .map_err(|error| match error {
            DbError::InvalidValue(message) => {
                ApiError::new(400, error_code::TASK_OWNER_INVALID, message)
            }
            other => utils::db_error(other, "按模板创建任务"),
        })?;
    Ok(Res::ok(created))
}

// ==================== 内部工具 ====================

/// 模板行 → 响应。
fn template_res(row: &TemplateRow) -> TaskTemplateRes {
    TaskTemplateRes {
        id: row.id.to_string(),
        name: row.name.clone(),
        title: row.title.clone(),
        description: row.description.clone().unwrap_or_default(),
        priority: row.priority.clone(),
        estimate_hours: row.estimate_hours,
        need_review: row.need_review,
        subtasks: row.subtask_list(),
        owner_id: row.owner_id.map(|id| id.to_string()).unwrap_or_default(),
        project_id: row.project_id.map(|id| id.to_string()).unwrap_or_default(),
        cron_expr: row.cron_expr.clone().unwrap_or_default(),
        enabled: row.enabled,
        next_run_at: row.next_run_at,
        last_run_at: row.last_run_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// 可空 UUID 入参解析（空串 / 缺省 → `None`）。
///
/// # 异常情况
///
/// 非法 UUID → 400 + `10001`。
fn parse_optional_uuid(
    raw: Option<&str>,
    label: &str,
) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => Ok(Some(parse_uuid_field(value, label)?)),
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 子任务 JSON 解析：正常 + 跳过坏元素 + 优先级兜底 + 截断。
    #[test]
    fn subtasks_json_parses_and_skips_bad_items() {
        let raw = r#"[
            {"title":"写迁移","priority":"P1"},
            {"title":"   "},
            {"priority":"P0"},
            {"title":"补单测","priority":"PX"}
        ]"#;
        let list = parse_subtasks_json(raw).expect("合法 JSON 应解析成功");
        assert_eq!(list.len(), 2, "缺标题的元素必须被跳过");
        assert_eq!(list[0].title, "写迁移");
        assert_eq!(list[0].priority, "P1");
        assert_eq!(list[1].title, "补单测");
        assert_eq!(list[1].priority, "P2", "非法优先级落 P2");
    }

    /// 子任务 JSON 解析：空值 / 非数组 / 非 JSON 三种边界。
    #[test]
    fn subtasks_json_rejects_non_array_and_handles_empty() {
        assert!(parse_subtasks_json("").expect("空串 → 空列表").is_empty());
        assert!(
            parse_subtasks_json("null")
                .expect("null → 空列表")
                .is_empty()
        );
        assert!(parse_subtasks_json("[]").expect("[] → 空列表").is_empty());
        assert!(
            parse_subtasks_json(r#"{"title":"x"}"#).is_err(),
            "对象不是数组"
        );
        assert!(parse_subtasks_json("not-json").is_err(), "非 JSON 必须报错");
    }

    /// 子任务编码：空列表落 `[]`，元素逐字保留。
    #[test]
    fn subtasks_encode_always_array() {
        assert_eq!(subtasks_to_json(&[]), "[]");
        let encoded = subtasks_to_json(&[TemplateSubtaskRes {
            title: "巡检".to_string(),
            priority: "P1".to_string(),
        }]);
        assert!(encoded.contains("\"title\":\"巡检\""));
        assert!(encoded.contains("\"priority\":\"P1\""));
    }

    /// 保存入参归一化：正常路径（含子任务继承父优先级）。
    #[test]
    fn normalize_save_ok() {
        let req = TaskTemplateSaveReq {
            name: "  周报模板  ".to_string(),
            title: " 写周报 ".to_string(),
            description: Some("  ".to_string()),
            priority: Some("P1".to_string()),
            estimate_hours: Some(2.5),
            need_review: Some(true),
            subtasks: Some(vec![
                TemplateSubtaskReq {
                    title: "收集数据".to_string(),
                    priority: None,
                },
                TemplateSubtaskReq {
                    title: "写总结".to_string(),
                    priority: Some("P0".to_string()),
                },
            ]),
            owner_id: None,
            project_id: None,
            cron_expr: Some("0 9 * * *".to_string()),
            enabled: Some(true),
        };
        let fields = normalize_save_req(&req).expect("合法入参应通过");
        assert_eq!(fields.name, "周报模板");
        assert_eq!(fields.title, "写周报");
        assert!(fields.description.is_none(), "空白描述落 None");
        assert_eq!(fields.priority, "P1");
        assert_eq!(fields.subtasks.len(), 2);
        assert_eq!(fields.subtasks[0].priority, "P1", "子任务缺省继承父优先级");
        assert_eq!(fields.subtasks[1].priority, "P0");
        assert_eq!(fields.cron_expr.as_deref(), Some("0 9 * * *"));
        assert!(fields.enabled);
    }

    /// 保存入参归一化：错误 / 边界分支。
    #[test]
    fn normalize_save_rejects_invalid() {
        // 空名
        let mut req = TaskTemplateSaveReq {
            name: " ".to_string(),
            title: "t".to_string(),
            ..Default::default()
        };
        let error = normalize_save_req(&req).expect_err("空模板名必须拒绝");
        assert_eq!(error.code, error_code::TASK_TEMPLATE_INVALID);

        // 空标题
        req.name = "n".to_string();
        req.title = "   ".to_string();
        assert!(normalize_save_req(&req).is_err());

        // 优先级非法
        req.title = "t".to_string();
        req.priority = Some("P9".to_string());
        assert!(normalize_save_req(&req).is_err());

        // 工时越界
        req.priority = None;
        req.estimate_hours = Some(-1.0);
        assert!(normalize_save_req(&req).is_err());

        // cron 非法
        req.estimate_hours = None;
        req.cron_expr = Some("每天九点".to_string());
        let error = normalize_save_req(&req).expect_err("自然语言 cron 必须拒绝");
        assert_eq!(error.code, error_code::TASK_TEMPLATE_CRON_INVALID);

        // 开启周期但没给 cron
        req.cron_expr = None;
        req.enabled = Some(true);
        let error = normalize_save_req(&req).expect_err("启用但无 cron 必须拒绝");
        assert_eq!(error.code, error_code::TASK_TEMPLATE_CRON_INVALID);

        // 子任务过多
        req.enabled = None;
        req.subtasks = Some(
            (0..MAX_SUBTASKS + 1)
                .map(|index| TemplateSubtaskReq {
                    title: format!("子任务{index}"),
                    priority: None,
                })
                .collect(),
        );
        assert!(normalize_save_req(&req).is_err());
    }

    /// 按字符截断不切坏 UTF-8，且短串原样返回。
    #[test]
    fn truncate_keeps_chars() {
        assert_eq!(truncate_chars("abc", 5), "abc");
        assert_eq!(truncate_chars("任务模板", 2), "任务");
    }
}
