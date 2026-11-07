//! 跨单据联动：规则表存取 + 配置写入口（第 2 层的落点）。
//!
//! # 读路径是 **fail-safe** 的
//!
//! 引擎挂在「单据状态变更之后」这条主链路上：读规则失败（表还没迁移 / 权限不足 /
//! 数据脏）**不能**让业务崩掉。所以 [`rules_for`] 永远返回 `Vec`（读失败 → 空 + 节流 WARN），
//! 与 D1 的 `effective_edges` 同一口径：**配置读不到 = 回到改动前的行为**。
//!
//! 读探测包在 `SAVEPOINT` 里（D1 真机踩过的坑）：PG 里一条语句报错会把整个事务置为
//! `aborted`，而调用方多半正开着事务，于是「读配置失败」会让紧接着的业务写报
//! `current transaction is aborted` —— 本该降级的路径反而把业务写死。
//!
//! # 写路径是 **fail-closed** 的
//!
//! [`upsert_rule`] 是改规则配置的唯一入口：
//!
//! - 非超管 → `403`（联动规则 = 业务规则，能改它的人等于能改全公司的账务联动）；
//! - 未二次确认 → `400`；
//! - 动作码不在白名单 / 参数键不合法 / 动作尚未接管 → `400`（第 1、2 层校验）；
//! - 改前 / 改后各留一行快照写 `biz_status_log`（`entity_type = 'FLOW_LINKAGE_CFG'`）。
//!
//! 为什么「尚未接管的动作」也拒：配置得上但执行时什么都不做，是最坏的一种「看起来配好了」。
//! 宁可写不进去，也不要让人以为联动生效了。

use std::result::Result;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::state_machine;
use crate::shared::constants::SUPER_ADMIN_ROLE_CODE;
use crate::shared::error_code;

use super::spec::{self, ActionSpec};

/// 联动规则（一行 = 触发点上的一个白名单动作）。
#[derive(Debug, Clone)]
pub struct LinkageRuleRow {
    /// 主键。
    pub id: Uuid,
    /// 规则名。
    pub name: String,
    /// 触发单据类型。
    pub trigger_entity: String,
    /// 触发状态。
    pub trigger_status: String,
    /// 白名单动作码。
    pub action_code: String,
    /// 动作参数（JSON 对象）。
    pub action_params: serde_json::Value,
    /// 是否启用。
    pub enabled: bool,
    /// 执行顺序。
    pub seq: i32,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl LinkageRuleRow {
    /// 行 → 结构体（`action_params` 用 `::text` 取出来再解析：jsonb 直接取会被驱动拒）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let params_text: String = row.get_typed("action_params_text")?;
        let action_params = serde_json::from_str(&params_text).unwrap_or_else(|error| {
            tracing::warn!(
                %error,
                "联动规则 action_params 不是合法 JSON，已按空对象处理（该规则会被跳过）"
            );
            serde_json::json!({})
        });
        Ok(Self {
            id: row.get_typed("id")?,
            name: row.get_typed("name")?,
            trigger_entity: row.get_typed("trigger_entity")?,
            trigger_status: row.get_typed("trigger_status")?,
            action_code: row.get_typed("action_code")?,
            action_params,
            enabled: row.get_typed("enabled")?,
            seq: row.get_typed("seq")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 动作声明（不在白名单 → `None`：数据库 CHECK 之外的第二道路径，防历史脏行）。
    pub fn spec(&self) -> Option<&'static ActionSpec> {
        spec::spec_of(&self.action_code)
    }

    /// 人读描述（日志 / 审计用）。
    pub fn describe(&self) -> String {
        format!(
            "{} [{}@{}/{} seq={} enabled={}] params={}",
            self.name,
            self.action_code,
            self.trigger_entity,
            self.trigger_status,
            self.seq,
            self.enabled,
            self.action_params
        )
    }
}

/// 规则表 SELECT 的列清单（读与写共用一份，避免两处列名漂移）。
const RULE_COLUMNS: &str = "id, name, trigger_entity, trigger_status, action_code, \
                            action_params::text AS action_params_text, enabled, seq, \
                            created_at, updated_at";

// ==================== 读路径 ====================

/// 读**全部**存活规则（不按触发点过滤：要能发现 `trigger_entity` 写错这类脏行）。
///
/// 事务里先 `SAVEPOINT`（理由见模块文档）：读失败要能安全降级，不能让事务变 aborted。
pub async fn load_rule_rows(ex: &impl SqlExecutor) -> DbResult<Vec<LinkageRuleRow>> {
    const SAVEPOINT: &str = "d2_linkage_read";
    let in_tx = ex.post_commit_queue().is_some();
    if in_tx {
        ex.exec_sql(format!("SAVEPOINT {SAVEPOINT}"), vec![])
            .await?;
    }
    let result = ex
        .query_sql(
            format!(
                "SELECT {RULE_COLUMNS} FROM biz_flow_linkage_rule \
                  WHERE deleted_at IS NULL ORDER BY trigger_entity ASC, trigger_status ASC, seq ASC, id ASC"
            ),
            vec![],
        )
        .await;
    if in_tx {
        let tail = match &result {
            Ok(_) => format!("RELEASE SAVEPOINT {SAVEPOINT}"),
            Err(_) => format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}"),
        };
        // 收尾语句失败不该盖掉主结果
        let _ = ex.exec_sql(tail, vec![]).await;
    }
    let rows = result?;
    rows.iter().map(LinkageRuleRow::from_row).collect()
}

/// 某个触发点的**启用**规则（按 `seq`、`id` 升序）。
///
/// **永远不返回 Err**：读失败 / 表不存在 → 空 Vec + 节流 WARN（= 不接管任何联动，
/// 业务回到改动前的行为）。这是引擎挂在主链路上唯一可接受的降级方式。
pub async fn rules_for(
    ex: &impl SqlExecutor,
    trigger_entity: &str,
    trigger_status: &str,
) -> Vec<LinkageRuleRow> {
    match load_rule_rows(ex).await {
        Ok(rows) => rows
            .into_iter()
            .filter(|row| {
                row.enabled
                    && row.trigger_entity == trigger_entity
                    && row.trigger_status == trigger_status
            })
            .collect(),
        Err(error) => {
            warn_throttled(
                &format!("{trigger_entity}/{trigger_status}"),
                &format!(
                    "读取 biz_flow_linkage_rule 失败（{error}）：本次联动一律不执行\
                     （业务回退到改动前的行为，配置暂不生效）"
                ),
            );
            Vec::new()
        }
    }
}

/// 配置页用的查询（**含禁用行**，读失败如实报错——管理界面不能静默显示「0 条规则」）。
pub async fn list_rules(
    ex: &impl SqlExecutor,
    trigger_entity: Option<&str>,
) -> DbResult<Vec<LinkageRuleRow>> {
    let rows = match trigger_entity.map(str::trim).filter(|v| !v.is_empty()) {
        Some(entity) => {
            ex.query_sql(
                format!(
                    "SELECT {RULE_COLUMNS} FROM biz_flow_linkage_rule \
                      WHERE deleted_at IS NULL AND trigger_entity = ? \
                      ORDER BY trigger_entity ASC, trigger_status ASC, seq ASC, id ASC"
                ),
                vec![Value::Text(entity.to_string())],
            )
            .await?
        }
        None => {
            ex.query_sql(
                format!(
                    "SELECT {RULE_COLUMNS} FROM biz_flow_linkage_rule \
                      WHERE deleted_at IS NULL \
                      ORDER BY trigger_entity ASC, trigger_status ASC, seq ASC, id ASC"
                ),
                vec![],
            )
            .await?
        }
    };
    rows.iter().map(LinkageRuleRow::from_row).collect()
}

/// 按主键取一条（改前快照 / 单测用）。
pub async fn find_rule(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<LinkageRuleRow>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {RULE_COLUMNS} FROM biz_flow_linkage_rule \
                  WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(LinkageRuleRow::from_row).transpose()
}

/// 一个触发点上该动作是否已有存活配置（唯一键冲突前的可读判断，单测用）。
pub async fn find_rule_by_key(
    ex: &impl SqlExecutor,
    trigger_entity: &str,
    trigger_status: &str,
    action_code: &str,
) -> DbResult<Option<LinkageRuleRow>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {RULE_COLUMNS} FROM biz_flow_linkage_rule \
                  WHERE trigger_entity = ? AND trigger_status = ? AND action_code = ? \
                    AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(trigger_entity.to_string()),
                Value::Text(trigger_status.to_string()),
                Value::Text(action_code.to_string()),
            ],
        )
        .await?;
    rows.first().map(LinkageRuleRow::from_row).transpose()
}

// ==================== 写路径（配置唯一入口） ====================

/// 写规则配置的入参。
#[derive(Debug, Clone)]
pub struct RulePatch {
    /// 规则名（人读）。
    pub name: String,
    /// 触发单据类型。
    pub trigger_entity: String,
    /// 触发状态。
    pub trigger_status: String,
    /// 白名单动作码。
    pub action_code: String,
    /// 动作参数。
    pub action_params: serde_json::Value,
    /// 是否启用（`None` = 新建默认 true / 更新保留原值）。
    pub enabled: Option<bool>,
    /// 执行顺序（`None` = 0 / 更新保留原值）。
    pub seq: Option<i32>,
    /// 二次确认（改联动 = 改业务规则）。
    pub confirm: bool,
    /// **触发点未接线时是否仍然保存**（P1-FIX-5C）。
    ///
    /// 默认 `false` ⇒ 在未接线的触发点上建/启用规则一律 `400`。这不是「限制功能」，
    /// 而是「不许静默保存一条永远不会触发的规则」：白名单 15 个触发点里目前只有 3 个真的
    /// 调了引擎（见 [`spec::TRIGGER_SPECS`]），其余 12 个上配规则会让管理员以为生效了。
    /// 确认要预置配置时可以显式传 `true`（配置页会弹二次确认并要求勾选）。
    pub allow_unwired: bool,
}

/// 某个触发点允许的状态（第 2 层校验：写错状态 = 规则永远不触发，必须当场拒）。
///
/// - 状态机实体：取 `base_edges()` 的 from/to 全集（代码里存在的状态就这些）；
/// - `TASK`：任务状态机的 5 态；
/// - `FLOW_INSTANCE`：审批实例状态（`PENDING/APPROVED/REJECTED/SKIPPED/CANCELLED`，
///   与 `flow::model` 的终态判定同口径）。
pub fn known_statuses(trigger_entity: &str) -> Vec<&'static str> {
    if trigger_entity == spec::TRIGGER_TASK {
        return vec!["TODO", "DOING", "REVIEW", "DONE", "CANCELLED"];
    }
    if trigger_entity == spec::TRIGGER_FLOW_INSTANCE {
        return vec!["PENDING", "APPROVED", "REJECTED", "SKIPPED", "CANCELLED"];
    }
    match state_machine::Entity::parse(trigger_entity) {
        Some(entity) => {
            let mut statuses: Vec<&'static str> = Vec::new();
            for edge in entity.base_edges() {
                if !statuses.contains(&edge.from) {
                    statuses.push(edge.from);
                }
                if !statuses.contains(&edge.to) {
                    statuses.push(edge.to);
                }
            }
            statuses
        }
        None => Vec::new(),
    }
}

/// **改联动规则配置的唯一入口**：超管 + 二次确认 + 三层校验的前两层 + 写审计。
///
/// # 错误
///
/// - 非超管 → `403`
/// - `confirm != true` / 名称空 / 未知触发点 / 未知状态 / 动作不在白名单 /
///   动作尚未接管 / 参数键或类型不合法 → `400`
pub async fn upsert_rule(
    ex: &impl SqlExecutor,
    claims: &TokenClaims,
    patch: &RulePatch,
) -> Result<LinkageRuleRow, ApiError> {
    ensure_config_admin(claims, "修改")?;
    if !patch.confirm {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "改联动规则需要二次确认（confirm = true）：规则决定「单据状态变了以后还会动哪些账」"
                .to_string(),
        ));
    }

    let name = patch.name.trim();
    if name.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "规则名不能为空（配置页要靠它认出这条联动是干什么的）".to_string(),
        ));
    }
    if name.chars().count() > 128 {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("规则名最长 128 个字符（当前 {}）", name.chars().count()),
        ));
    }

    // 第 1 层（应用侧）：触发点必须存在
    let trigger_entity = patch.trigger_entity.trim();
    if !spec::is_known_trigger(trigger_entity) {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "未知的触发单据类型「{trigger_entity}」：只认 {}",
                spec::TRIGGERS.join(" / ")
            ),
        ));
    }

    // 第 1 层（应用侧）：触发状态必须在该单据的状态集里
    let trigger_status = patch.trigger_status.trim();
    if trigger_status.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "触发状态不能为空".to_string(),
        ));
    }
    let known = known_statuses(trigger_entity);
    if !known.is_empty() && !known.contains(&trigger_status) {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "{trigger_entity} 没有状态「{trigger_status}」：可用 {}（写错状态的规则永远不触发）",
                known.join(" / ")
            ),
        ));
    }

    // 第 1 层（应用侧）：触发点必须**真的接线**（P1-FIX-5C）
    //
    // 白名单校验只回答「允许不允许」，不回答「有没有人调引擎」。15 个白名单触发点里
    // 只有 3 个真接线，其余 12 个上配规则「能保存、能启用、永远不触发」——这是最坏的
    // 一种看起来配好了。所以启用态的规则必须落在已接线的触发点上，否则要求显式
    // `allow_unwired`（配置页做成二次确认勾选，curl 直连也必须显式传）。
    let will_enable = patch.enabled.unwrap_or(true);
    if will_enable && !spec::is_wired_trigger(trigger_entity) && !patch.allow_unwired {
        let detail = spec::trigger_spec(trigger_entity)
            .map(|item| item.wiring)
            .unwrap_or("该触发点不在白名单里");
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "触发点 {trigger_entity}/{trigger_status} 尚未接线：引擎在这个状态变更点不会被执行，\
                 规则保存后永远不会触发。{detail}。确认要预置配置（例如为接线做准备）请显式传 \
                 allow_unwired = true；当前可用（已接线）的触发点：{}",
                spec::wired_triggers().join(" / ")
            ),
        ));
    }

    // 第 1 层（应用侧）：动作码必须在白名单里
    let action_code = patch.action_code.trim();
    let action = spec::spec_of(action_code).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "动作码「{action_code}」不在白名单里：联动只允许预定义动作（{}），\
                 不接受任何自定义语句（禁配 SQL）",
                spec::codes().join(" / ")
            ),
        )
    })?;

    // 尚未接管的动作不许配置（配得上但执行时什么都不做是最坏的一种「看起来配好了」）
    if !action.implemented {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "动作 {action_code}（{}）尚未接管：入参契约已定，执行实现要等第 3 步复用既有 handler 后再开放配置",
                action.label
            ),
        ));
    }

    // 第 1.5 层：实体级禁令（LNK-06）。有的实体上某些动作是**结构性**成立的死循环，
    // 不是「配了不触发」而是「一触发就无限递归」—— 这类动作在保存时就 400，
    // 不靠运行时排除兜底（运行时排除只负责历史脏行 / 绕过 API 直接写库的行）。
    if let Some(reason) = spec::forbidden_action_reason(trigger_entity, action_code) {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("动作 {action_code} 不能配置在触发点 {trigger_entity} 上：{reason}"),
        ));
    }

    // 第 2 层：参数键 / 必填 / 类型
    spec::validate_params(action, &patch.action_params)
        .map_err(|reason| ApiError::new(400, error_code::PARAM_INVALID, reason))?;

    let enabled = patch.enabled.unwrap_or(true);
    let seq = patch.seq.unwrap_or(0);
    if seq < 0 {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("seq 不能为负（当前 {seq}）"),
        ));
    }

    // 改前快照（审计用）
    let before = find_rule_by_key(ex, trigger_entity, trigger_status, action_code)
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("读联动规则失败（改配置前必须留改前快照）：{error}"),
            )
        })?;

    let now = Utc::now();
    let operator = state_machine::Operator::from(claims);
    let operator_id = operator
        .id
        .map(|value| value.to_string())
        .unwrap_or_default();
    let id: Uuid = ex
        .query_first_sql(
            "INSERT INTO biz_flow_linkage_rule \
                 (id, name, trigger_entity, trigger_status, action_code, action_params, \
                  enabled, seq, created_at, updated_at, create_by, update_by, deleted_at) \
             VALUES (?, ?, ?, ?, ?, ?::jsonb, ?, ?, ?, ?, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, NULL) \
             ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE \
                SET name = EXCLUDED.name, \
                    action_params = EXCLUDED.action_params, \
                    enabled = EXCLUDED.enabled, \
                    seq = EXCLUDED.seq, \
                    updated_at = EXCLUDED.updated_at, \
                    update_by = EXCLUDED.update_by, \
                    deleted_at = NULL \
             RETURNING id"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::Text(name.to_string()),
                Value::Text(trigger_entity.to_string()),
                Value::Text(trigger_status.to_string()),
                Value::Text(action_code.to_string()),
                Value::Json(patch.action_params.to_string()),
                Value::from(enabled),
                Value::I32(seq),
                Value::from(now),
                Value::from(now),
                Value::Text(operator_id.clone()),
                Value::Text(operator_id),
            ],
        )
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("写联动规则失败：{error}"),
            )
        })?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "写联动规则没有返回主键（upsert 异常）".to_string(),
            )
        })?
        .get_typed("id")
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("读联动规则主键失败：{error}"),
            )
        })?;

    let after = find_rule(ex, id)
        .await
        .map_err(|error| crate::utils::db_error(error, "读回联动规则"))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "联动规则写入后读不回来（事务内可见性异常）".to_string(),
            )
        })?;

    write_config_audit(
        ex,
        &operator,
        id,
        trigger_entity,
        trigger_status,
        action_code,
        before.as_ref(),
        Some(&after),
        now,
    )
    .await?;

    Ok(after)
}

/// 软删一条规则（= 这条联动不再执行）。超管 + 二次确认 + 留痕。
pub async fn delete_rule(
    ex: &impl SqlExecutor,
    claims: &TokenClaims,
    id: Uuid,
    confirm: bool,
) -> Result<(), ApiError> {
    ensure_config_admin(claims, "删除")?;
    if !confirm {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "删联动规则需要二次确认（confirm = true）".to_string(),
        ));
    }
    let before = find_rule(ex, id)
        .await
        .map_err(|error| crate::utils::db_error(error, "读联动规则"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("联动规则 {id} 不存在（或已删除）"),
            )
        })?;
    let now = Utc::now();
    let operator = state_machine::Operator::from(claims);
    let affected = ex
        .exec_sql(
            "UPDATE biz_flow_linkage_rule \
                SET deleted_at = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(now),
                Value::from(now),
                Value::Text(
                    operator
                        .id
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(&id),
            ],
        )
        .await
        .map_err(|error| crate::utils::db_error(error, "软删联动规则"))?;
    if affected == 0 {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            format!("联动规则 {id} 已被删除（并发），本次未生效"),
        ));
    }
    write_config_audit(
        ex,
        &operator,
        id,
        &before.trigger_entity,
        &before.trigger_status,
        &before.action_code,
        Some(&before),
        None,
        now,
    )
    .await
}

// ==================== 内部 ====================

/// 改配置的门：**只有超管**。
fn ensure_config_admin(claims: &TokenClaims, action: &str) -> Result<(), ApiError> {
    let is_super = claims
        .roles
        .iter()
        .any(|role| role == SUPER_ADMIN_ROLE_CODE)
        || claims
            .permissions
            .iter()
            .any(|code| code == "*" || code == "*:*:*");
    if is_super {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        error_code::PERMISSION_DENIED,
        format!(
            "{action}联动规则需要超级管理员（SUPER_ADMIN）：规则决定「单据状态变了以后还会动哪些账」，\
             只能由超管在「流程配置」里操作"
        ),
    ))
}

/// 写一行配置审计（`biz_status_log`，只增不改；`entity_type = FLOW_LINKAGE_CFG`）。
///
/// 这里直接写 INSERT 而不是复用 `state_machine::insert_log`（那是模块私有函数），
/// 列清单与它逐字一致；`biz_ref_type/biz_ref_no` 留空串（配置变更不挂业务引用）。
///
/// # 列宽口径（照 `V0159` 的 DDL，别照直觉写）
///
/// - `entity_no` 是 **varchar(64)** → 超长会 500，所以按字符截断；
/// - `from_status` / `to_status` 是 **varchar(24)** → 只放短标记
///   （`CONFIG` / `UPSERT` / `DELETED`），人读的改前改后快照全部进 `reason`（text）。
#[allow(clippy::too_many_arguments)]
async fn write_config_audit(
    ex: &impl SqlExecutor,
    operator: &state_machine::Operator,
    rule_id: Uuid,
    trigger_entity: &str,
    trigger_status: &str,
    action_code: &str,
    before: Option<&LinkageRuleRow>,
    after: Option<&LinkageRuleRow>,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let entity_no: String = format!("{trigger_entity}/{trigger_status} {action_code}")
        .chars()
        .take(64)
        .collect();
    let describe = |row: Option<&LinkageRuleRow>| match row {
        None => "无规则".to_string(),
        Some(row) => format!(
            "name={} seq={} enabled={} params={}",
            row.name, row.seq, row.enabled, row.action_params
        ),
    };
    let reason = match after {
        Some(_) => format!(
            "改前 {{{}}} → 改后 {{{}}}",
            describe(before),
            describe(after)
        ),
        None => format!("删除规则 {{ {} }}", describe(before)),
    };
    let to_status = if after.is_some() { "UPSERT" } else { "DELETED" };
    ex.exec_sql(
        "INSERT INTO biz_status_log \
            (id, entity_type, entity_id, entity_no, from_status, to_status, \
             operator_id, operator_name, reason, biz_ref_type, biz_ref_no, created_at) \
         VALUES (?, ?, ?::uuid, ?, ?, ?, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,''), NULLIF(?::text,''), '', '', ?)"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(spec::AUDIT_ENTITY_TYPE.to_string()),
            Value::from(&rule_id),
            Value::Text(entity_no),
            Value::Text("CONFIG".to_string()),
            Value::Text(to_status.to_string()),
            Value::Text(operator.id.map(|id| id.to_string()).unwrap_or_default()),
            Value::Text(operator.name.clone().unwrap_or_default()),
            Value::Text(reason),
            Value::from(now),
        ],
    )
    .await
    .map_err(|error| crate::utils::db_error(error, "写联动配置审计"))?;
    Ok(())
}

/// 同一问题的 WARN 只打一次（60 秒节流）：状态变更是高频路径，别把日志刷爆。
fn warn_throttled(scope: &str, detail: &str) {
    static SEEN: OnceLock<Mutex<std::collections::HashMap<String, Instant>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let now = Instant::now();
    let should_log = match seen.lock() {
        Ok(mut guard) => match guard.get(scope) {
            Some(last) if now.duration_since(*last) < Duration::from_secs(60) => false,
            _ => {
                guard.insert(scope.to_string(), now);
                true
            }
        },
        Err(_) => true,
    };
    if should_log {
        tracing::warn!(scope, "{detail}");
    }
}
