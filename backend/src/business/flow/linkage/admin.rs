//! 跨单据联动规则的**配置管理面**（HTTP）。
//!
//! 内核在 [`super::model`]（`biz_flow_linkage_rule` 的存取与唯一写入口）与
//! [`super::spec`]（11 个白名单动作的入参契约）。**本模块一行校验 / 写库逻辑都不重复实现**，
//! 只做三件事：
//!
//! | # | 做什么 | 落点 |
//! |---|--------|------|
//! | 1 | HTTP 管理面（查 / 增改 / 启停 / 删 / 预览影响） | [`list_rules`] / [`upsert_rule`] / [`toggle_rule`] / [`delete_rule`] / [`preview_impact`] |
//! | 2 | 把白名单动作的入参契约下发给前端（下拉 + 动态参数表单） | [`catalog`] 的 `actions` |
//! | 3 | 把触发点 / 状态 / 实体中文名下发给前端 | [`catalog`] 的 `triggers` |
//!
//! # 为什么「预览影响」由后端算
//!
//! 「这条规则配上去会发生什么」的判据全在代码里：动作的事务归属（`InTx` 还是 `AfterCommit`）、
//! 失败等级（关键动作失败 → 整笔回滚）、复用的是哪段既有实现（`ActionSpec::reuse`）。
//! 前端如果自己拼一份解释，就成了第二份真相 —— 动作改语义时页面文案不会跟着变。
//! 所以 [`preview_impact`] 把「按 seq 排好的执行链 + 回滚风险 + 参数校验结果」一次性算好下发。
//!
//! # 写完规则为什么前端看不到「立即生效」
//!
//! 规则是**下一次**单据状态变更才被引擎读到的（[`super::model::rules_for`] 每次状态变更现查），
//! 不会回放历史单据。预览里的 `next_run_hint` 把这句话明确写给使用者，避免「配了没反应」的误解。
//!
//! # 权限
//!
//! - 读：`business:flow:linkage:list`
//! - 写（增改 / 启停 / 删）：`business:flow:linkage:manage`（迁移只授 `SUPER_ADMIN`）
//!
//! 内核 [`super::model::upsert_rule`] 还会再判一次 `SUPER_ADMIN`（写路径 fail-closed），
//! 注解与内核两层是**同口径**而不是重复：注解管「这个角色能不能进这个接口」，
//! 内核管「这个账号能不能改业务规则」（撤销角色后老令牌也不该能改）。

use axum::Json;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::business::shared::state_machine::Entity;
use crate::shared::error_code;
use crate::utils;

use super::model::{self, LinkageRuleRow, RulePatch};
use super::spec::{self, ActionSpec, ParamKind};

/// 解析 UUID 字段（框架默认提取器反序列化不了 `nik::uuid::Uuid`，统一按字符串收再解析）。
fn parse_uuid_field(raw: &str, label: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("{label}「{raw}」不是合法的 UUID"),
        )
    })
}

/// 读规则的权限码（与 `V0253` 种进 `sys_menu.auth_code` 的值逐字一致）。
pub const PERM_LIST: &str = "business:flow:linkage:list";
/// 改 / 启停 / 删规则的权限码（同上；`V0253` 只授 SUPER_ADMIN）。
pub const PERM_MANAGE: &str = "business:flow:linkage:manage";

// ==================== 请求 ====================

/// `GET /business/flow/linkage` 的查询参数。
#[derive(Debug, Deserialize)]
pub struct RuleListQuery {
    /// 只看某个触发单据类型（空 = 全部）
    pub trigger_entity: Option<String>,
}

/// `PUT /business/flow/linkage` 的请求体（新增 / 编辑共用）。
///
/// 编辑时 `trigger_entity / trigger_status / action_code` 三项**不可变**（它们构成唯一键），
/// 前端把这三项置为只读；要换动作请删掉旧规则再建一条。
#[derive(Debug, Deserialize)]
pub struct RuleUpsertReq {
    /// 规则名（人读）
    pub name: String,
    /// 触发单据类型
    pub trigger_entity: String,
    /// 触发状态
    pub trigger_status: String,
    /// 白名单动作码
    pub action_code: String,
    /// 动作参数（省略 = `{}`）
    #[serde(default)]
    pub action_params: Option<JsonValue>,
    /// 是否启用（省略 = 新建 true / 更新保留原值）
    pub enabled: Option<bool>,
    /// 执行顺序（省略 = 0 / 更新保留原值）
    pub seq: Option<i32>,
    /// 二次确认（**必须** `true`：改联动 = 改业务规则）
    #[serde(default)]
    pub confirm: bool,
    /// 触发点未接线时是否仍然保存（默认 `false` = 拒绝；配置页勾选二次确认后才带 `true`）
    #[serde(default)]
    pub allow_unwired: bool,
}

/// `PUT /business/flow/linkage/toggle` 的请求体（启停）。
#[derive(Debug, Deserialize)]
pub struct RuleToggleReq {
    /// 规则主键（UUID 字符串：框架默认提取器不支持 JSON 里的 `nik::uuid::Uuid`）
    pub id: String,
    /// 目标状态
    pub enabled: bool,
    /// 二次确认（**必须** `true`）
    #[serde(default)]
    pub confirm: bool,
    /// **启用**一条挂在未接线触发点上的规则时是否仍然放行（默认 `false` = 拒绝）
    #[serde(default)]
    pub allow_unwired: bool,
}

/// `DELETE /business/flow/linkage` 的查询参数。
#[derive(Debug, Deserialize)]
pub struct RuleDeleteQuery {
    /// 规则主键（UUID 字符串，理由同上）
    pub id: String,
    /// 二次确认（**必须** `true`）
    #[serde(default)]
    pub confirm: bool,
}

/// `POST /business/flow/linkage/preview` 的请求体。
#[derive(Debug, Deserialize)]
pub struct PreviewReq {
    /// 触发单据类型
    pub trigger_entity: String,
    /// 触发状态
    pub trigger_status: String,
    /// 待保存的草稿（给了就一并校验它；不给 = 只看现状）
    #[serde(default)]
    pub draft: Option<PreviewDraft>,
}

/// 预览时的草稿规则（还没落库的那一条）。
#[derive(Debug, Deserialize)]
pub struct PreviewDraft {
    /// 白名单动作码
    pub action_code: String,
    /// 动作参数
    #[serde(default)]
    pub action_params: Option<JsonValue>,
}

// ==================== 响应 ====================

/// 一条联动规则的视图。
#[derive(Debug, Serialize)]
pub struct RuleRes {
    /// 主键
    pub id: String,
    /// 规则名
    pub name: String,
    /// 触发单据类型
    pub trigger_entity: String,
    /// 触发单据中文名
    pub trigger_entity_label: String,
    /// 触发状态
    pub trigger_status: String,
    /// 触发状态中文名（`POSTED` → 「已过账」；P1-FIX-12A）
    ///
    /// 列表页直接显示这个字段。**为什么行级也下发一份**：列表要显示中文就得把
    /// 「编码 → 中文」的字典 join 到每一行，前端自己算一遍就是第二份映射；
    /// 字典里查不到（脏数据 / 状态已被状态机删掉）时回落成编码本身，不显示空白。
    pub trigger_status_label: String,
    /// 白名单动作码
    pub action_code: String,
    /// 动作中文名（不在白名单时回落成动作码）
    pub action_label: String,
    /// 事务归属（`IN_TX` / `AFTER_COMMIT`）
    pub phase: String,
    /// 失败等级（`CRITICAL` / `NON_CRITICAL`）
    pub severity: String,
    /// 动作是否已接管（`false` = 占位，执行时跳过）
    pub implemented: bool,
    /// 动作参数
    pub action_params: JsonValue,
    /// 是否启用
    pub enabled: bool,
    /// 执行顺序
    pub seq: i32,
    /// 创建时间（RFC 3339）
    pub created_at: String,
    /// 更新时间（RFC 3339）
    pub updated_at: String,
    /// 这一行自身的问题（动作不在白名单 / 未接管 / 参数不合法），空 = 健康
    pub problems: Vec<String>,
}

impl RuleRes {
    /// 行 → 视图（`problems` 只报事实，不修数据）。
    fn of(row: &LinkageRuleRow) -> Self {
        let action = row.spec();
        let problems = rule_problems(row);
        Self {
            id: row.id.to_string(),
            name: row.name.clone(),
            trigger_entity: row.trigger_entity.clone(),
            trigger_entity_label: trigger_label(&row.trigger_entity),
            trigger_status: row.trigger_status.clone(),
            trigger_status_label: trigger_status_label(&row.trigger_entity, &row.trigger_status),
            action_code: row.action_code.clone(),
            action_label: action
                .map(|item| item.label.to_string())
                .unwrap_or_else(|| row.action_code.clone()),
            phase: action
                .map(|item| item.phase.as_str().to_string())
                .unwrap_or_default(),
            severity: action
                .map(|item| item.severity.as_str().to_string())
                .unwrap_or_default(),
            implemented: action.map(|item| item.implemented).unwrap_or(false),
            action_params: row.action_params.clone(),
            enabled: row.enabled,
            seq: row.seq,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
            problems,
        }
    }
}

/// 一个触发状态的中文名（`code` = 落库编码，`label` = 人读中文）。
///
/// P1-FIX-12A：配置页原来拿编码当显示名，管理员看到的是 `POSTED / DOING / DRAFT`。
/// 现在编码与中文成对下发 —— `code` 仍要与库里的 `trigger_status` / 日志对得上，
/// `label` 是给人看的。
#[derive(Debug, Serialize)]
pub struct StatusLabelRes {
    /// 状态编码（与 `biz_flow_linkage_rule.trigger_status` 逐字一致）
    pub code: String,
    /// 中文名（来自 [`spec::TriggerSpec::status_labels`]）
    pub label: String,
}

/// 一个触发点（单据类型 + 允许的状态）。
#[derive(Debug, Serialize)]
pub struct TriggerRes {
    /// 触发单据类型（落 `trigger_entity`）
    pub entity: String,
    /// 中文名（后端下发，前端不再维护映射）
    pub label: String,
    /// 允许的触发状态（升序，与状态机边表同口径）
    pub statuses: Vec<String>,
    /// 允许的触发状态 + 中文名（**顺序与 `statuses` 逐项对齐**，P1-FIX-12A）
    ///
    /// `statuses` 保留是为了向后兼容（老前端 / 脚本仍按编码取值）；
    /// 新前端用 `status_labels` 渲染下拉的 label，`value` 继续用 `code`。
    pub status_labels: Vec<StatusLabelRes>,
    /// **是否已接线**（`false` = 代码里没人调引擎，配了规则永远不触发；前端据此标注并拦截保存）
    pub wired: bool,
    /// 接线位置（`wired = true`）或未接线原因 + 接的时候要动哪里（`wired = false`）。
    /// 文案由后端下发，前端不维护第二份解释。
    pub wiring: String,
}

/// 一个动作参数的类型声明（动态表单用）。
#[derive(Debug, Serialize)]
pub struct ParamKindRes {
    /// 类型：`text` / `uuid` / `enum` / `bool` / `int` / `decimal` / `uuid_list`
    pub kind: &'static str,
    /// `text` 的长度上限
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_len: Option<usize>,
    /// `int` 的下界
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    /// `int` 的上界
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    /// `uuid_list` 的条数上限
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
    /// `enum` 的候选值（逐字命中）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

impl ParamKindRes {
    /// 从内核的 `ParamKind` 投影。
    fn of(kind: &ParamKind) -> Self {
        match kind {
            ParamKind::Text { max } => Self {
                kind: "text",
                max_len: Some(*max),
                min: None,
                max: None,
                max_items: None,
                values: Vec::new(),
            },
            ParamKind::Uuid => Self {
                kind: "uuid",
                max_len: None,
                min: None,
                max: None,
                max_items: None,
                values: Vec::new(),
            },
            ParamKind::Enum(values) => Self {
                kind: "enum",
                max_len: None,
                min: None,
                max: None,
                max_items: None,
                values: values.iter().map(|item| item.to_string()).collect(),
            },
            ParamKind::Bool => Self {
                kind: "bool",
                max_len: None,
                min: None,
                max: None,
                max_items: None,
                values: Vec::new(),
            },
            ParamKind::Int { min, max } => Self {
                kind: "int",
                max_len: None,
                min: Some(*min),
                max: Some(*max),
                max_items: None,
                values: Vec::new(),
            },
            ParamKind::Decimal => Self {
                kind: "decimal",
                max_len: None,
                min: None,
                max: None,
                max_items: None,
                values: Vec::new(),
            },
            ParamKind::UuidList { max_items } => Self {
                kind: "uuid_list",
                max_len: None,
                min: None,
                max: None,
                max_items: Some(*max_items),
                values: Vec::new(),
            },
        }
    }
}

/// 一个动作参数的声明（动态表单用）。
#[derive(Debug, Serialize)]
pub struct ParamRes {
    /// 键名（落 `action_params`）
    pub key: &'static str,
    /// 类型
    pub kind: ParamKindRes,
    /// 是否必填
    pub required: bool,
    /// 人读说明
    pub note: &'static str,
}

/// 一个白名单动作的声明（动作下拉 + 影响说明）。
#[derive(Debug, Serialize)]
pub struct ActionRes {
    /// 动作码（落 `action_code`）
    pub code: &'static str,
    /// 中文名
    pub label: &'static str,
    /// 事务归属
    pub phase: &'static str,
    /// 失败等级
    pub severity: &'static str,
    /// 是否已接管（`false` = 占位：**不允许配置**，选了下拉也保存不了）
    pub implemented: bool,
    /// 参数声明
    pub params: Vec<ParamRes>,
    /// 复用的既有实现（预览里的「影响」说明）
    pub reuse: &'static str,
}

impl ActionRes {
    /// 从内核的 `ActionSpec` 投影。
    fn of(action: &'static ActionSpec) -> Self {
        Self {
            code: action.code,
            label: action.label,
            phase: action.phase.as_str(),
            severity: action.severity.as_str(),
            implemented: action.implemented,
            params: action
                .params
                .iter()
                .map(|param| ParamRes {
                    key: param.key,
                    kind: ParamKindRes::of(&param.kind),
                    required: param.required,
                    note: param.note,
                })
                .collect(),
            reuse: action.reuse,
        }
    }
}

/// `GET /business/flow/linkage` 的响应：规则列表 + 配置字典。
#[derive(Debug, Serialize)]
pub struct RuleListRes {
    /// 规则（含禁用行；按触发点 / seq 升序）
    pub rules: Vec<RuleRes>,
    /// `rules` 长度
    pub total: usize,
    /// 触发点字典（前端「源单据类型 + 触发状态」两级下拉）
    pub triggers: Vec<TriggerRes>,
    /// 白名单动作字典（前端「目标动作」下拉 + 动态参数表单）
    pub actions: Vec<ActionRes>,
    /// 参数里**禁止出现**的键（禁配 SQL 的黑名单；前端文案直接引用）
    pub forbidden_keys: Vec<&'static str>,
}

/// 预览里的一步（一个会执行的动作）。
#[derive(Debug, Serialize)]
pub struct PreviewStepRes {
    /// 规则主键
    pub rule_id: String,
    /// 规则名
    pub name: String,
    /// 执行顺序
    pub seq: i32,
    /// 动作码
    pub action_code: String,
    /// 动作中文名
    pub action_label: String,
    /// 事务归属
    pub phase: String,
    /// 失败等级
    pub severity: String,
    /// 动作参数
    pub action_params: JsonValue,
    /// 复用的既有实现（「这条会动到什么」）
    pub effect: String,
    /// 这一行的问题（有问题 = 运行时会被跳过 / 被拒）
    pub problems: Vec<String>,
}

/// 草稿规则的校验结果。
#[derive(Debug, Serialize)]
pub struct DraftCheckRes {
    /// 校验是否通过（`false` = 保存会被 400 拒绝）
    pub ok: bool,
    /// 失败原因（`ok = true` 时为空串）
    pub message: String,
    /// 动作中文名
    pub action_label: String,
    /// 事务归属
    pub phase: String,
    /// 失败等级
    pub severity: String,
    /// 复用的既有实现
    pub effect: String,
    /// 同一个触发点 + 同一个动作是否已有存活规则（`true` = 保存会覆盖它）
    pub overrides_existing: bool,
}

/// `POST /business/flow/linkage/preview` 的响应。
#[derive(Debug, Serialize)]
pub struct PreviewRes {
    /// 触发单据类型
    pub trigger_entity: String,
    /// 触发单据中文名
    pub trigger_entity_label: String,
    /// 触发状态
    pub trigger_status: String,
    /// **该触发点是否已接线**（`false` = 引擎在这个状态变更点不会被调用，配了也不触发）
    pub trigger_wired: bool,
    /// 该触发点当前**启用**的规则数
    pub enabled_count: usize,
    /// 按 seq 排好的执行链
    pub steps: Vec<PreviewStepRes>,
    /// 是否存在「关键动作 + 同事务」：任一失败整笔回滚
    pub rollback_risk: bool,
    /// 提交后执行的动作数
    pub after_commit_count: usize,
    /// 给人看的结论与提醒（空 = 没有额外要说的事）
    pub warnings: Vec<String>,
    /// 「什么时候才会跑」的说明（规则不回放历史单据）
    pub next_run_hint: String,
    /// 草稿校验结果（请求没带草稿时为 `null`）
    pub draft: Option<DraftCheckRes>,
}

// ==================== HTTP：查 ====================

/// 查联动规则 + 配置字典（`GET /business/flow/linkage?trigger_entity=STOCK_IN`）。
///
/// 返回**含禁用行**的规则列表：配置页要能把已停用的规则也列出来（否则「配过什么」不可追）。
///
/// # 异常情况
///
/// - 400 + `65005`：`trigger_entity` 不在白名单里（不静默返回空列表）；
/// - 500：数据库异常（管理界面读失败必须如实报错，不能显示「0 条规则」）。
#[nik::permission(
    path = "/business/flow/linkage",
    method = "GET",
    permission = "business:flow:linkage:list"
)]
#[nik::get("/business/flow/linkage")]
pub async fn list_rules(Query(query): Query<RuleListQuery>) -> ResResult<RuleListRes> {
    let entity = match query
        .trigger_entity
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
    {
        None => None,
        Some(raw) => Some(parse_trigger(raw)?),
    };
    let rows = model::list_rules(db(), entity)
        .await
        .map_err(|error| utils::db_error(error, "查询联动规则"))?;
    let rules: Vec<RuleRes> = rows.iter().map(RuleRes::of).collect();
    Ok(Res::ok(RuleListRes {
        total: rules.len(),
        rules,
        triggers: catalog_triggers(),
        actions: catalog_actions(),
        forbidden_keys: spec::FORBIDDEN_KEYS.to_vec(),
    }))
}

// ==================== HTTP：写 ====================

/// 新增 / 编辑一条联动规则（`PUT /business/flow/linkage`）。
///
/// 唯一键是 `(trigger_entity, trigger_status, action_code)`：同一触发点上的同一动作只有一行，
/// 保存即覆盖（`ON CONFLICT DO UPDATE`，软删行会被复活）。所以编辑时三项键不可变。
///
/// 校验全部由内核 [`model::upsert_rule`] 完成（白名单 / 状态 / 参数键与类型 / 二次确认 / 超管），
/// 本层只做解析与错误透传。
///
/// # 异常情况
///
/// - 400 + `10001`：二次确认缺失 / 名称空 / 未知触发点或状态 / 动作不在白名单 /
///   动作未接管 / 参数键或类型不合法（报文里点名是哪一条）；
/// - 403：不是超管 / 没有 `manage` 码；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/linkage",
    method = "PUT",
    permission = "business:flow:linkage:manage"
)]
#[nik::put("/business/flow/linkage")]
pub async fn upsert_rule(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<RuleUpsertReq>,
) -> ResResult<RuleRes> {
    let patch = RulePatch {
        name: req.name,
        trigger_entity: req.trigger_entity,
        trigger_status: req.trigger_status,
        action_code: req.action_code,
        action_params: req.action_params.unwrap_or_else(|| serde_json::json!({})),
        enabled: req.enabled,
        seq: req.seq,
        confirm: req.confirm,
        allow_unwired: req.allow_unwired,
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启联动规则事务"))?;
    match model::upsert_rule(&tx, &claims, &patch).await {
        Ok(row) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交联动规则事务"))?;
            Ok(Res::ok(RuleRes::of(&row)))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 启停一条联动规则（`PUT /business/flow/linkage/toggle`）。
///
/// 语义 = 把该行原样再 upsert 一遍、只改 `enabled`：复用同一个写入口，
/// 参数校验与审计留痕全部照走（停用不是「绕过校验的后门」）。
///
/// # 异常情况
///
/// - 400 + `10001`：二次确认缺失；
/// - 403：不是超管；
/// - 404 + `65005`：规则不存在（或已删除）。
#[nik::permission(
    path = "/business/flow/linkage/toggle",
    method = "PUT",
    permission = "business:flow:linkage:manage"
)]
#[nik::put("/business/flow/linkage/toggle")]
pub async fn toggle_rule(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<RuleToggleReq>,
) -> ResResult<RuleRes> {
    if !req.confirm {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "启停联动规则需要二次确认（confirm = true）：规则决定「单据状态变了以后还会动哪些账」"
                .to_string(),
        ));
    }
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启联动规则事务"))?;
    let result = async {
        let rule_id = parse_uuid_field(&req.id, "规则主键")?;
        let row = model::find_rule(&tx, rule_id)
            .await
            .map_err(|error| utils::db_error(error, "读联动规则"))?
            .ok_or_else(|| {
                ApiError::new(
                    404,
                    error_code::RESOURCE_NOT_FOUND,
                    format!("联动规则 {rule_id} 不存在（或已删除）"),
                )
            })?;
        let patch = RulePatch {
            name: row.name,
            trigger_entity: row.trigger_entity,
            trigger_status: row.trigger_status,
            action_code: row.action_code,
            action_params: row.action_params,
            enabled: Some(req.enabled),
            seq: Some(row.seq),
            confirm: true,
            // 停用永远放行；「启用一条未接线触发点上的规则」要显式 allow_unwired
            allow_unwired: req.allow_unwired,
        };
        model::upsert_rule(&tx, &claims, &patch).await
    }
    .await;
    match result {
        Ok(row) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交联动规则事务"))?;
            Ok(Res::ok(RuleRes::of(&row)))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 软删一条联动规则（`DELETE /business/flow/linkage?id=&confirm=true`）。
///
/// 软删 = 这条联动不再执行；行保留（唯一键上的历史仍可追，重新保存同一动作会复活它）。
///
/// # 异常情况
///
/// - 400 + `10001`：二次确认缺失；
/// - 403：不是超管；
/// - 404 + `65005`：规则不存在（或已被删除）。
#[nik::permission(
    path = "/business/flow/linkage",
    method = "DELETE",
    permission = "business:flow:linkage:manage"
)]
#[nik::delete("/business/flow/linkage")]
pub async fn delete_rule(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<RuleDeleteQuery>,
) -> ResResult<()> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启联动规则事务"))?;
    let rule_id = parse_uuid_field(&query.id, "规则主键")?;
    match model::delete_rule(&tx, &claims, rule_id, query.confirm).await {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交联动规则事务"))?;
            Ok(Res::ok(()))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== HTTP：预览影响 ====================

/// 预览一个触发点的执行链（`POST /business/flow/linkage/preview`）。
///
/// 算三份东西：
/// 1. 该触发点**当前启用**的规则，按 `seq` 排好的执行链（含事务归属与失败等级）；
/// 2. 回滚风险：有「关键动作 + 同事务」时，任一失败整笔回滚（这是使用者最需要知道的事）；
/// 3. 草稿校验：请求带了 `draft` 就按内核同一份判据（白名单 / 已接管 / 参数键与类型）先验一遍，
///    保存前就能看到能不能过，而不是点保存才收 400。
///
/// # 异常情况
///
/// - 400：`trigger_entity` 不在白名单 / `trigger_status` 不在该单据的状态集里；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/flow/linkage/preview",
    method = "POST",
    permission = "business:flow:linkage:list"
)]
#[nik::post("/business/flow/linkage/preview")]
pub async fn preview_impact(Json(req): Json<PreviewReq>) -> ResResult<PreviewRes> {
    let entity = parse_trigger(req.trigger_entity.trim())?.to_string();
    let status = req.trigger_status.trim().to_string();
    ensure_known_status(&entity, &status)?;

    let rows = model::list_rules(db(), Some(entity.as_str()))
        .await
        .map_err(|error| utils::db_error(error, "查询联动规则"))?;
    let mut steps: Vec<PreviewStepRes> = Vec::new();
    for row in rows
        .iter()
        .filter(|row| row.enabled && row.trigger_status == status)
    {
        let action = row.spec();
        steps.push(PreviewStepRes {
            rule_id: row.id.to_string(),
            name: row.name.clone(),
            seq: row.seq,
            action_code: row.action_code.clone(),
            action_label: action
                .map(|item| item.label.to_string())
                .unwrap_or_else(|| row.action_code.clone()),
            phase: action
                .map(|item| item.phase.as_str().to_string())
                .unwrap_or_default(),
            severity: action
                .map(|item| item.severity.as_str().to_string())
                .unwrap_or_default(),
            action_params: row.action_params.clone(),
            effect: action
                .map(|item| item.reuse.to_string())
                .unwrap_or_default(),
            problems: rule_problems(row),
        });
    }

    let rollback_risk = steps
        .iter()
        .any(|step| step.phase == "IN_TX" && step.severity == "CRITICAL");
    let after_commit_count = steps
        .iter()
        .filter(|step| step.phase == "AFTER_COMMIT")
        .count();
    let mut warnings = Vec::new();
    let trigger_wired = spec::is_wired_trigger(&entity);
    if !trigger_wired {
        warnings.push(format!(
            "【未接线】{entity} 的「{status}」变更点还没有调用联动引擎：这里的规则配了也不会触发。{}",
            spec::trigger_spec(&entity)
                .map(|item| item.wiring)
                .unwrap_or("")
        ));
    }
    if steps.is_empty() {
        warnings.push(format!(
            "{entity} 走到 {status} 时当前没有任何启用的联动规则：这次配置保存后，\
             下一条该单据走到这个状态才会开始执行"
        ));
    }
    if rollback_risk {
        warnings.push(
            "执行链里有「同事务 + 关键动作」：任一动作失败，本次单据状态变更整笔回滚（不会半执行）"
                .to_string(),
        );
    }
    if after_commit_count > 0 {
        warnings.push(format!(
            "有 {after_commit_count} 个动作在事务提交后执行：失败只记日志，不会回滚已经过账的单据"
        ));
    }
    let draft = req.draft.as_ref().map(|draft| {
        check_draft(
            &entity,
            &status,
            &draft.action_code,
            draft
                .action_params
                .clone()
                .unwrap_or_else(|| serde_json::json!({})),
            &rows,
        )
    });

    Ok(Res::ok(PreviewRes {
        trigger_entity: entity.clone(),
        trigger_entity_label: trigger_label(&entity),
        trigger_status: status.clone(),
        trigger_wired,
        enabled_count: steps.len(),
        steps,
        rollback_risk,
        after_commit_count,
        warnings,
        next_run_hint: format!(
            "规则不会回放历史单据：只有之后发生的「{entity} → {status}」才会被联动引擎执行，\
             已存在的单据需要重新走一次状态变更"
        ),
        draft,
    }))
}

// ==================== 内部 ====================

/// 草稿校验（与内核同一份判据：白名单 → 已接管 → 参数）。
fn check_draft(
    entity: &str,
    status: &str,
    action_code: &str,
    params: JsonValue,
    rows: &[LinkageRuleRow],
) -> DraftCheckRes {
    let code = action_code.trim();
    let Some(action) = spec::spec_of(code) else {
        return DraftCheckRes {
            ok: false,
            message: format!(
                "动作码「{code}」不在白名单里：联动只允许预定义动作（{}），不接受任何自定义语句（禁配 SQL）",
                spec::codes().join(" / ")
            ),
            action_label: code.to_string(),
            phase: String::new(),
            severity: String::new(),
            effect: String::new(),
            overrides_existing: false,
        };
    };
    let overrides_existing = rows.iter().any(|row| {
        row.trigger_entity == entity && row.trigger_status == status && row.action_code == code
    });
    let base = |ok: bool, message: String| DraftCheckRes {
        ok,
        message,
        action_label: action.label.to_string(),
        phase: action.phase.as_str().to_string(),
        severity: action.severity.as_str().to_string(),
        effect: action.reuse.to_string(),
        overrides_existing,
    };
    // 触发点未接线 = 这条草稿存下去也永远不触发（P1-FIX-5C）。与 `model::upsert_rule`
    // 同一份判据：这里给配置页一个**当场可见**的拒绝原因，而不是等保存时才 400。
    if !spec::is_wired_trigger(entity) {
        return base(
            false,
            format!(
                "触发点 {entity}/{status} 尚未接线：引擎在这个状态变更点不会被调用，规则保存后\
                 永远不会触发。{}",
                spec::trigger_spec(entity)
                    .map(|item| item.wiring)
                    .unwrap_or("")
            ),
        );
    }
    if !action.implemented {
        return base(
            false,
            format!(
                "动作 {}（{}）尚未接管：入参契约已定，执行实现要等第 3 步复用既有 handler 后再开放配置",
                action.code, action.label
            ),
        );
    }
    match spec::validate_params(action, &params) {
        Ok(()) => base(true, String::new()),
        Err(reason) => base(false, reason),
    }
}

/// 一行规则的「自身问题」（触发点未接线 / 不在白名单 / 未接管 / 参数不合法）。
fn rule_problems(row: &LinkageRuleRow) -> Vec<String> {
    let mut problems = Vec::new();
    // 触发点未接线 = 这条规则永远不会触发（P1-FIX-5C：以前这里什么都不报）
    if !spec::is_wired_trigger(&row.trigger_entity) {
        problems.push(format!(
            "触发点 {} 未接线：联动引擎不会在 {} 的「{}」变更点执行 —— 这条规则{}不会触发",
            row.trigger_entity,
            row.trigger_entity,
            row.trigger_status,
            if row.enabled {
                ""
            } else {
                "（且已停用）"
            }
        ));
    }
    match row.spec() {
        None => problems.push(format!(
            "动作码 {} 不在白名单里：运行时会被跳过（历史脏行）",
            row.action_code
        )),
        Some(action) => {
            if !action.implemented {
                problems.push(format!(
                    "动作 {}（{}）尚未接管：运行时会被跳过",
                    action.code, action.label
                ));
            }
            if let Err(reason) = spec::validate_params(action, &row.action_params) {
                problems.push(format!("参数不合法：{reason}"));
            }
        }
    }
    problems
}

/// 触发点白名单校验（不在白名单直接 400，不静默返回空）。
fn parse_trigger(raw: &str) -> std::result::Result<&'static str, ApiError> {
    spec::TRIGGERS
        .iter()
        .copied()
        .find(|item| *item == raw)
        .ok_or_else(|| {
            ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!(
                    "未知的触发单据类型「{raw}」：只认 {}",
                    spec::TRIGGERS.join(" / ")
                ),
            )
        })
}

/// 触发状态校验（写错状态的规则永远不触发，必须当场拒）。
fn ensure_known_status(entity: &str, status: &str) -> std::result::Result<(), ApiError> {
    if status.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "触发状态不能为空".to_string(),
        ));
    }
    let known = model::known_statuses(entity);
    if !known.is_empty() && !known.contains(&status) {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "{entity} 没有状态「{status}」：可用 {}（写错状态的规则永远不触发）",
                known.join(" / ")
            ),
        ));
    }
    Ok(())
}

/// 触发单据类型的中文名（后端下发，前端不再维护映射）。
fn trigger_label(entity: &str) -> String {
    if entity == spec::TRIGGER_TASK {
        return "任务".to_string();
    }
    if entity == spec::TRIGGER_FLOW_INSTANCE {
        return "审批实例".to_string();
    }
    Entity::parse(entity)
        .map(|item| item.label().to_string())
        .unwrap_or_else(|| entity.to_string())
}

/// 触发状态的中文名（P1-FIX-12A）。
///
/// 字典只有一份：[`spec::TriggerSpec::status_labels`]。查不到时**回落成编码本身**
/// 而不是「未知状态」——脏数据 / 状态机里已删掉的老状态，回显编码才能对着
/// `biz_flow_linkage_rule.trigger_status` 和日志排障；空白或「未知状态」都没法查。
fn trigger_status_label(entity: &str, status: &str) -> String {
    spec::status_label_of(entity, status)
        .unwrap_or(status)
        .to_string()
}

/// 某触发点的「状态编码 + 中文名」清单（**顺序与 `statuses` 逐项对齐**）。
///
/// 顺序刻意跟着 `statuses`（= [`model::known_statuses`] 的顺序）走，两数组同下标即同一状态：
/// 前端按下标兜底也不会错位，老前端只读 `statuses` 也不受任何影响。
fn status_labels_of(entity: &str) -> Vec<StatusLabelRes> {
    model::known_statuses(entity)
        .into_iter()
        .map(|status| StatusLabelRes {
            code: status.to_string(),
            label: trigger_status_label(entity, status),
        })
        .collect()
}

/// 触发点字典（单据类型 + 该单据允许的状态）。
fn catalog_triggers() -> Vec<TriggerRes> {
    spec::TRIGGERS
        .iter()
        .map(|entity| TriggerRes {
            entity: entity.to_string(),
            label: trigger_label(entity),
            statuses: model::known_statuses(entity)
                .into_iter()
                .map(str::to_string)
                .collect(),
            status_labels: status_labels_of(entity),
            wired: spec::is_wired_trigger(entity),
            wiring: spec::trigger_spec(entity)
                .map(|item| item.wiring.to_string())
                .unwrap_or_default(),
        })
        .collect()
}

/// 动作字典（11 个白名单动作 + 参数声明）。
fn catalog_actions() -> Vec<ActionRes> {
    spec::ACTIONS.iter().map(ActionRes::of).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 字典必须覆盖内核白名单的全部动作码与触发点（漏一个 = 前端下拉少一项）。
    #[test]
    fn catalog_covers_every_action_and_trigger() {
        let actions = catalog_actions();
        assert_eq!(
            actions.iter().map(|item| item.code).collect::<Vec<_>>(),
            spec::codes(),
            "动作字典顺序与白名单一致"
        );
        let triggers = catalog_triggers();
        assert_eq!(
            triggers
                .iter()
                .map(|item| item.entity.as_str())
                .collect::<Vec<_>>(),
            spec::TRIGGERS.to_vec(),
            "触发点字典顺序与白名单一致"
        );
        for trigger in &triggers {
            assert!(!trigger.statuses.is_empty(), "每个触发点都要有状态可选");
            assert!(!trigger.label.is_empty(), "中文名不能为空");
        }
    }

    /// 触发点字典必须带「状态编码 → 中文名」，且与 `statuses` 逐项对齐（P1-FIX-12A）。
    ///
    /// 用户报的就是这条：下拉和列表都显示 `POSTED / DOING / DRAFT`，看不出是什么状态。
    #[test]
    fn catalog_triggers_carry_status_labels() {
        let triggers = catalog_triggers();
        for trigger in &triggers {
            assert_eq!(
                trigger.status_labels.len(),
                trigger.statuses.len(),
                "{}：status_labels 必须与 statuses 一一对应",
                trigger.entity
            );
            for (status, pair) in trigger.statuses.iter().zip(&trigger.status_labels) {
                assert_eq!(
                    status, &pair.code,
                    "{}：两个数组同下标必须是同一状态（前端会按下标兜底）",
                    trigger.entity
                );
                assert!(
                    !pair.label.trim().is_empty(),
                    "{} / {status}：中文名不能为空",
                    trigger.entity
                );
                assert_ne!(
                    pair.label, pair.code,
                    "{} / {status}：中文名回落成了编码（字典缺项，等于没修）",
                    trigger.entity
                );
            }
        }

        // 抽查用户实际点名的三个：POSTED / DOING / DRAFT
        let pair_of = |entity: &str, code: &str| -> String {
            triggers
                .iter()
                .find(|item| item.entity == entity)
                .and_then(|item| item.status_labels.iter().find(|pair| pair.code == code))
                .map(|pair| pair.label.clone())
                .unwrap_or_else(|| panic!("{entity} 缺状态 {code}"))
        };
        assert_eq!(pair_of("STOCK_IN", "POSTED"), "已过账");
        assert_eq!(pair_of("TASK", "DOING"), "进行中");
        assert_eq!(pair_of("STOCK_IN", "DRAFT"), "草稿");
        assert_eq!(pair_of("FLOW_INSTANCE", "REJECTED"), "已驳回");

        // 向后兼容：老字段一行不少（老前端 / 脚本仍按编码取值）
        assert!(
            triggers.iter().all(|trigger| !trigger.statuses.is_empty()),
            "statuses 必须继续下发"
        );
    }

    /// 规则行也要带触发状态中文名（列表页直接显示，前端不再维护第二份映射）。
    #[test]
    fn rule_res_carries_trigger_status_label() {
        use nik::prelude::chrono::Utc;

        let row = LinkageRuleRow {
            id: Uuid::now_v7(),
            name: "收货过账后发通知".to_string(),
            trigger_entity: "STOCK_IN".to_string(),
            trigger_status: "POSTED".to_string(),
            action_code: "SEND_NOTICE".to_string(),
            action_params: serde_json::json!({"mode": "TAKEOVER"}),
            enabled: true,
            seq: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let res = RuleRes::of(&row);
        assert_eq!(res.trigger_status, "POSTED", "编码必须继续下发（向后兼容）");
        assert_eq!(res.trigger_status_label, "已过账");

        // 状态机里已不存在的历史状态：回落成编码本身（不空白、不改数据）
        let mut legacy = row.clone();
        legacy.trigger_status = "LEGACY_STATE".to_string();
        let res = RuleRes::of(&legacy);
        assert_eq!(
            res.trigger_status_label, "LEGACY_STATE",
            "查不到就回显编码，便于排障"
        );
    }

    /// 触发点字典必须带上「是否接线」与其说明（P1-FIX-5C：配置页靠它拦误配）。
    #[test]
    fn catalog_triggers_carry_wired_flag() {
        let triggers = catalog_triggers();
        let wired: Vec<&str> = triggers
            .iter()
            .filter(|item| item.wired)
            .map(|item| item.entity.as_str())
            .collect();
        assert_eq!(
            wired,
            vec![
                "STOCK_IN",
                "STOCK_OUT",
                "STOCK_TRANSFER",
                "PURCHASE_ORDER",
                "PAYABLE",
                "STOCK_TAKE",
                "TASK"
            ],
            "已接线的触发点：STOCK_IN / STOCK_OUT（P1-FIX-5C 新接）/ \
             STOCK_TRANSFER（LNK-01 新接）/ PURCHASE_ORDER（LNK-02 新接）/ \
             PAYABLE（LNK-03 新接）/ STOCK_TAKE（LNK-04 新接）/ TASK"
        );
        for trigger in &triggers {
            assert!(
                !trigger.wiring.trim().is_empty(),
                "触发点 {} 必须下发「接线位置 / 未接线原因」",
                trigger.entity
            );
            assert_eq!(
                trigger.wired,
                spec::is_wired_trigger(&trigger.entity),
                "{} 的 wired 必须逐字来自 spec::TRIGGER_SPECS",
                trigger.entity
            );
        }
    }

    /// 「已接管」标记必须与内核逐条一致：前端据此禁用下拉项。
    #[test]
    fn catalog_implemented_flags_match_spec() {
        for action in catalog_actions() {
            let spec_item = spec::spec_of(action.code).expect("动作必须在白名单里");
            assert_eq!(action.implemented, spec_item.implemented, "{}", action.code);
            assert_eq!(action.phase, spec_item.phase.as_str());
            assert_eq!(action.severity, spec_item.severity.as_str());
            assert_eq!(action.params.len(), spec_item.params.len());
        }
    }

    /// 草稿校验：未接管 / 非白名单 / 参数不合法都必须给出可读原因。
    #[test]
    fn draft_check_rejects_bad_input() {
        let rows: Vec<LinkageRuleRow> = Vec::new();
        let unknown = check_draft(
            "STOCK_IN",
            "POSTED",
            "DROP_TABLE",
            serde_json::json!({}),
            &rows,
        );
        assert!(!unknown.ok);
        assert!(unknown.message.contains("不在白名单"));

        let placeholder = check_draft(
            "STOCK_IN",
            "POSTED",
            "LOCK_DOCUMENT",
            serde_json::json!({}),
            &rows,
        );
        assert!(!placeholder.ok);
        assert!(placeholder.message.contains("尚未接管"));

        let params = check_draft(
            "STOCK_IN",
            "POSTED",
            "GENERATE_VOUCHER",
            serde_json::json!({"biz_type": "STOCK_IN", "sql": "select 1"}),
            &rows,
        );
        assert!(!params.ok, "禁配 SQL 的键必须被拒");
    }

    /// 未接线触发点的草稿必须在**预览阶段**就被拒（P1-FIX-5C：不许「看起来配好了」）。
    #[test]
    fn draft_check_rejects_unwired_trigger() {
        let rows: Vec<LinkageRuleRow> = Vec::new();
        let unwired = spec::unwired_triggers();
        assert!(!unwired.is_empty(), "当前应有未接线的触发点");
        let res = check_draft(
            unwired[0],
            "DRAFT",
            "SEND_NOTICE",
            serde_json::json!({"mode": "TAKEOVER"}),
            &rows,
        );
        assert!(!res.ok, "未接线触发点上的草稿必须 ok=false");
        assert!(
            res.message.contains("尚未接线"),
            "拒绝原因必须点名「尚未接线」：{}",
            res.message
        );
    }

    /// 触发点 / 状态解析：未知值报 400，不静默。
    #[test]
    fn unknown_trigger_and_status_are_rejected() {
        assert!(parse_trigger("NOT_A_DOC").is_err());
        assert!(ensure_known_status("STOCK_IN", "NOT_A_STATUS").is_err());
        assert!(ensure_known_status("STOCK_IN", "POSTED").is_ok());
    }
}
