//! 销售返利政策接口层（M3-15）。前缀 `/api` 由 `[router].prefix` 注入，宏内写
//! `/business/erp/rebate-policy/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/rebate-policy/page` | `business:erp:rebate-policy:list` | 分页（关键字 / 客户 / 类型 / 粒度 / 状态 / 某日生效） |
//! | GET | `/business/erp/rebate-policy/{id}` | `business:erp:rebate-policy:list` | 详情（政策头 + 规则档位） |
//! | POST | `/business/erp/rebate-policy` | `business:erp:rebate-policy:create` | 新建（取号 `BR-yyyyMMdd-000001` + 规则） |
//! | PUT | `/business/erp/rebate-policy/{id}` | `business:erp:rebate-policy:update` | 改（政策头覆盖 + 规则整单替换） |
//! | DELETE | `/business/erp/rebate-policy/{id}` | `business:erp:rebate-policy:delete` | 软删（历史结算单不受影响） |
//!
//! # 本卡**只做政策**，不算钱
//!
//! 这里一个 `biz_sales_order` / `biz_receipt` 都不查：返利金额 = 期间销量（或销售额）× 比率
//! 是 **M3-16** 的活。本卡交付的是「政策能被正确录入、被正确读回、不会被录成自相矛盾」——
//! 落档口径以纯函数 `rebate_dto::pick_tier` 的形式先给出去（M3-16 直接调，不重写第二份）。
//!
//! # 三条业务闸（卡面「三种规则类型」的落地保障）
//!
//! | 闸 | 在哪 | 拦什么 |
//! |----|------|--------|
//! | ① 形状校验 | [`crate::business::erp::sales::rebate_dto::checked_rules`] | 按量/按额给了多档或区间、阶梯不从 0 起 / 断档 / 末档带上限 / 下限重复 / 只有一档、比率 ≤ 0、百分比 > 100 |
//! | ② 客户可用 | [`require_active_customer`] | 客户不存在 / 已软删 → 404；`FROZEN` → 409（冻结客户不能再谈新返利，旧政策照旧生效到期） |
//! | ③ 政策不重叠 | [`guard_overlap`] | 同一客户 × 同一结算粒度的**启用**政策有效期重叠 → 409 + `66090`（否则一个结算周期能按两份政策各算一次返利） |
//!
//! 闸③ 与写入在**同一事务**、且在 `pg_advisory_xact_lock(hashtextextended(customer_id,0))`
//! 之下 —— 见 [`crate::business::erp::sales::rebate_model::lock_customer`] 的注释。
//!
//! # 为什么编辑是「全量覆盖」而不是 PATCH
//!
//! 政策是**条款**：改了 `rule_type` 却留着旧口径的档位，库里的档位与类型就互相解释不通
//! （按量政策配了三档，M3-16 落档时按 `tier_from` 命中的是「第几档」没有意义的数）。
//! 前端编辑弹窗本来就持有全量字段，整单提交最省事也最不容易出错；`status` 省略时保留原值。
//!
//! # 错误口径
//!
//! | 场景 | 返回 |
//! |------|------|
//! | 参数非法（UUID / 日期 / 政策名 / 粒度 / 状态域外） | 400 + 10001，点名是哪一项 |
//! | 规则档位不合法（三种类型的形状） | 400 + **66089**，点名「第几档、哪个字段」 |
//! | 客户不存在 / 已删；政策不存在 / 已删 | 404 + 10002 |
//! | 客户冻结；政策有效期重叠；政策号撞车 | 409 + 10003 / **66090** |

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::customer::customer_model::{self, STATUS_ACTIVE};
use crate::business::erp::sales::rebate_dto::{
    self, MAX_NAME_LEN, RebatePolicyFilter, RebatePolicyRes, RebatePolicyWriteReq, RebateRuleRes, RuleInput, RuleWrite,
    STATUS_ENABLED, checked_rule_type, checked_rules, checked_settle_period, checked_status, checked_unit,
    settle_label,
};
use crate::business::erp::sales::rebate_model::{
    self as model, NewPolicy, PolicyConflict, PolicyPageQuery, RebatePolicyRow, RebateRuleRow,
};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0438 的 sys_menu.auth_code 逐字一致） ====================

/// 政策列表 / 详情（`list` 与报价单同型：既挂页面节点，也是两个读接口的码）。
pub const PERM_LIST: &str = "business:erp:rebate-policy:list";
/// 新建政策（含规则档位）。
pub const PERM_CREATE: &str = "business:erp:rebate-policy:create";
/// 编辑政策（含规则整单替换）。
pub const PERM_UPDATE: &str = "business:erp:rebate-policy:update";
/// 软删政策。
pub const PERM_DELETE: &str = "business:erp:rebate-policy:delete";

/// 本模块全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const REBATE_POLICY_PERMISSIONS: [&str; 4] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE];

// ==================== 错误出口 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404 + 10002`。
fn not_found(message: impl Into<String>) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, message.into())
}

/// `409 + 10003`：政策号撞车 / 编辑撞并发。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// `409 + 66090`：有效期重叠（前端要能把这条翻成「和哪份政策撞了」）。
fn overlap_conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::ERP_REBATE_POLICY_OVERLAP, message.into())
}

/// 写路径 DB 错误收口：唯一索引 / CHECK 兜底也翻成人话，其余仍是 500。
fn write_error(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::UniqueViolation { constraint, .. } => conflict(format!(
            "{ctx}：政策号或档号冲突{}（并发提交时请刷新后重试）",
            constraint
                .as_deref()
                .map(|name| format!("（{name}）"))
                .unwrap_or_default()
        )),
        DbError::CheckViolation { constraint, .. } => bad_request(format!(
            "{ctx}：数据不满足约束{}（规则类型/计价单位/粒度/有效期/比率域外）",
            constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
        )),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 参数解析 ====================

/// UUID 入参：非法 → 400（点名是哪个参数）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|error| bad_request(format!("{field} 不是合法 UUID：{error}")))
}

/// 可选 UUID（空白 = 不筛）。
fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Ok(Some(parse_uuid(text, field)?)),
    }
}

/// 日期入参（`yyyy-MM-dd`；空白 = 没传）。
fn parse_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| bad_request(format!("{field} 必须是 yyyy-MM-dd（当前 `{text}`）"))),
    }
}

// ==================== 出参装配 ====================

impl From<&RebateRuleRow> for RebateRuleRes {
    fn from(row: &RebateRuleRow) -> Self {
        Self {
            id: row.id.to_string(),
            seq: row.seq,
            tier_from: row.tier_from,
            tier_to: row.tier_to,
            rate: row.rate,
            remark: row.remark.clone(),
        }
    }
}

/// 政策行 + 规则 → 出参。
fn to_res(row: &RebatePolicyRow, rules: &[RebateRuleRow]) -> RebatePolicyRes {
    RebatePolicyRes {
        id: row.id.to_string(),
        policy_no: row.policy_no.clone(),
        policy_name: row.policy_name.clone(),
        customer_id: row.customer_id.to_string(),
        customer_code: row.customer_code.clone(),
        customer_name: row.customer_name.clone(),
        rule_type: row.rule_type.clone(),
        rate_unit: row.rate_unit.clone(),
        settle_period: row.settle_period.clone(),
        effective_from: row.effective_from,
        effective_to: row.effective_to,
        status: row.status,
        remark: row.remark.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        rules: rules.iter().map(RebateRuleRes::from).collect(),
    }
}

// ==================== 编排 ====================

/// 分页列出返利政策（每行带全部规则档位：一次 SQL 批量取，不做 N+1）。
pub async fn page_policies_of(
    filter: RebatePolicyFilter,
) -> std::result::Result<PageData<Vec<RebatePolicyRes>>, ApiError> {
    let customer_id = parse_optional_uuid(filter.customer_id.as_deref(), "customer_id")?;
    let rule_type = match filter
        .rule_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(raw) => Some(checked_rule_type(raw)?),
    };
    let settle_period = match filter
        .settle_period
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(raw) => Some(checked_settle_period(raw)?),
    };
    let status = filter.status.map(checked_status).transpose()?;
    let active_on = parse_date(filter.active_on.as_deref(), "active_on")?;
    let query = PolicyPageQuery {
        keyword: filter.keyword.as_deref(),
        customer_id,
        rule_type: rule_type.as_deref(),
        settle_period: settle_period.as_deref(),
        status,
        active_on,
        page: filter.page,
        page_size: filter.page_size,
    };
    let (rows, total) = model::policy_page(&query)
        .await
        .map_err(|error| utils::db_error(error, "分页查询返利政策"))?;
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let grouped = model::rules_of_many(&ids)
        .await
        .map_err(|error| utils::db_error(error, "批量查询返利规则"))?;
    let lookup = |id: Uuid| -> Vec<RebateRuleRow> {
        grouped
            .iter()
            .find(|(policy_id, _)| *policy_id == id)
            .map(|(_, rules)| rules.clone())
            .unwrap_or_default()
    };
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    Ok(PageData {
        list: rows.iter().map(|row| to_res(row, &lookup(row.id))).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 政策详情（政策头 + 规则档位）。不存在 / 已删 → 404。
pub async fn detail_policy_of(id_raw: &str) -> std::result::Result<RebatePolicyRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    detail_of(id).await
}

/// 内部详情（回读用：创建/编辑成功后走同一条路）。
async fn detail_of(id: Uuid) -> std::result::Result<RebatePolicyRes, ApiError> {
    let row = model::find_policy(id)
        .await
        .map_err(|error| utils::db_error(error, "查询返利政策"))?
        .ok_or_else(|| not_found("返利政策不存在或已删除"))?;
    let rules = model::rules_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询返利规则"))?;
    Ok(to_res(&row, &rules))
}

/// 已校验的写入内容（政策头 + 规则，全部 owned，方便跨 await）。
#[derive(Debug, Clone)]
struct CheckedPolicy {
    policy_name: String,
    customer_id: Uuid,
    rule_type: String,
    rate_unit: String,
    settle_period: String,
    effective_from: NaiveDate,
    effective_to: Option<NaiveDate>,
    /// `None` = 调用方没给：新建用 [`STATUS_ENABLED`]，编辑保留原值。
    status: Option<i16>,
    remark: Option<String>,
    rules: Vec<RuleWrite>,
}

/// 校验一份写入内容（政策头 + 三种规则类型的形状）。
///
/// 纯校验，不碰库：客户存在性 / 重叠 / 取号都在事务里做。
fn checked_policy(req: &RebatePolicyWriteReq) -> std::result::Result<CheckedPolicy, ApiError> {
    let policy_name = req.policy_name.trim();
    if policy_name.is_empty() {
        return Err(bad_request("policy_name（政策名称）不能为空"));
    }
    if policy_name.chars().count() > MAX_NAME_LEN {
        return Err(bad_request(format!(
            "policy_name 最多 {MAX_NAME_LEN} 个字（当前 {} 个）",
            policy_name.chars().count()
        )));
    }
    let customer_id = parse_uuid(&req.customer_id, "customer_id")?;
    let rule_type = checked_rule_type(&req.rule_type)?;
    let rate_unit = checked_unit(&rule_type, req.rate_unit.as_deref())?;
    let settle_period = checked_settle_period(&req.settle_period)?;
    let effective_from = parse_date(Some(&req.effective_from), "effective_from")?
        .ok_or_else(|| bad_request("effective_from（生效日）必填：政策从哪天开始算"))?;
    let effective_to = parse_date(req.effective_to.as_deref(), "effective_to")?;
    if let Some(to) = effective_to
        && to < effective_from
    {
        return Err(bad_request(format!(
            "effective_to（{to}）不能早于 effective_from（{effective_from}）：\
             建单当天就失效的政策只会在结算时才发现"
        )));
    }
    let status = req.status.map(checked_status).transpose()?;
    let rules = checked_rules(&rule_type, &rate_unit, &rule_inputs(&req.rules)?)?;
    Ok(CheckedPolicy {
        policy_name: policy_name.to_string(),
        customer_id,
        rule_type,
        rate_unit,
        settle_period,
        effective_from,
        effective_to,
        status,
        remark: req
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
        rules,
    })
}

/// 把入参档位转成纯校验类型（缺 `rate` → 400 点名第几档）。
fn rule_inputs(rules: &[rebate_dto::RebateRuleReq]) -> std::result::Result<Vec<RuleInput>, ApiError> {
    rules
        .iter()
        .enumerate()
        .map(|(index, rule)| rule.to_input(index))
        .collect()
}

/// 客户必须存在、未软删、`ACTIVE`（冻结客户 → 409）。
async fn require_active_customer(customer_id: Uuid) -> std::result::Result<customer_model::CustomerRow, ApiError> {
    let customer = customer_model::customer_find_by_id(customer_id)
        .await
        .map_err(|error| utils::db_error(error, "校验客户存在性"))?
        .ok_or_else(|| not_found(format!("客户 {customer_id} 不存在或已删除，无法维护返利政策")))?;
    if customer.status != STATUS_ACTIVE {
        return Err(conflict(format!(
            "客户「{}」已冻结（{}），不能再新建或修改返利政策：\
             解冻后可继续，已有政策的有效期不受影响",
            customer.name, customer.status
        )));
    }
    Ok(customer)
}

/// 有效期重叠闸：同一客户 × 同一结算粒度已有启用政策与本次重叠 → 409 + 66090。
///
/// **必须在已取 `model::lock_customer` 的那个事务里调用**（见模块注释「三条业务闸」）。
async fn guard_overlap(
    tx: &Transaction,
    policy: &CheckedPolicy,
    exclude_id: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    // 只有「启用」的政策才参与比较：停用的政策不结算，允许与新的重叠（这正是先停用再改的路径）
    let enabled = policy.status.unwrap_or(STATUS_ENABLED) == STATUS_ENABLED;
    if !enabled {
        return Ok(());
    }
    let found = model::overlap_of(
        tx,
        policy.customer_id,
        &policy.settle_period,
        policy.effective_from,
        policy.effective_to,
        exclude_id,
    )
    .await
    .map_err(|error| utils::db_error(error, "检查返利政策有效期重叠"))?;
    match found {
        None => Ok(()),
        Some(PolicyConflict {
            policy_no,
            policy_name,
            effective_from,
            effective_to,
            ..
        }) => Err(overlap_conflict(format!(
            "同一客户已有生效中的{}返利政策 {policy_no}（{policy_name}，{} ~ {}）与本次有效期重叠：\
             一个结算周期只能按一份政策算返利，否则返利会算两遍。\
             请改有效期错开、或先停用/改短旧政策的失效日、或软删旧政策后新建",
            settle_label(&policy.settle_period),
            effective_from,
            effective_to
                .map(|day| day.to_string())
                .unwrap_or_else(|| "长期".to_string())
        ))),
    }
}

/// 新建返利政策（取号 + 政策头 + 规则，一个事务）。
pub async fn create_policy_of(
    operator: Option<Uuid>,
    req: RebatePolicyWriteReq,
) -> std::result::Result<RebatePolicyRes, ApiError> {
    let checked = checked_policy(&req)?;
    require_active_customer(checked.customer_id).await?;
    let new = NewPolicy {
        policy_name: &checked.policy_name,
        customer_id: checked.customer_id,
        rule_type: &checked.rule_type,
        rate_unit: &checked.rate_unit,
        settle_period: &checked.settle_period,
        effective_from: checked.effective_from,
        effective_to: checked.effective_to,
        status: checked.status.unwrap_or(STATUS_ENABLED),
        remark: checked.remark.as_deref(),
    };

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建政策事务"))?;
    if let Err(error) = model::lock_customer(&tx, checked.customer_id).await {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "锁定客户（返利政策串行化）"));
    }
    if let Err(error) = guard_overlap(&tx, &checked, None).await {
        let _ = tx.rollback().await;
        return Err(error);
    }
    let created = model::create_policy(&tx, &new, &checked.rules, operator, Utc::now()).await;
    let (id, policy_no) = match created {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "新建返利政策"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交建政策事务"))?;
    let res = detail_of(id).await?;
    debug_assert_eq!(res.policy_no, policy_no, "回读单号必须与取号结果一致");
    Ok(res)
}

/// 编辑返利政策（政策头覆盖 + 规则整单替换，一个事务）。
pub async fn update_policy_of(
    operator: Option<Uuid>,
    id_raw: &str,
    req: RebatePolicyWriteReq,
) -> std::result::Result<RebatePolicyRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let current = model::find_policy(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑返利政策"))?
        .ok_or_else(|| not_found("返利政策不存在或已删除"))?;
    let mut checked = checked_policy(&req)?;
    // `status` 省略 = 保留原值（编辑弹窗不必每次都回填开关）
    if checked.status.is_none() {
        checked.status = Some(current.status);
    }
    require_active_customer(checked.customer_id).await?;
    let new = NewPolicy {
        policy_name: &checked.policy_name,
        customer_id: checked.customer_id,
        rule_type: &checked.rule_type,
        rate_unit: &checked.rate_unit,
        settle_period: &checked.settle_period,
        effective_from: checked.effective_from,
        effective_to: checked.effective_to,
        status: checked.status.unwrap_or(STATUS_ENABLED),
        remark: checked.remark.as_deref(),
    };

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改政策事务"))?;
    match model::lock_policy(&tx, id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.rollback().await;
            return Err(not_found("返利政策不存在或已被删除"));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定返利政策行"));
        }
    }
    if let Err(error) = model::lock_customer(&tx, checked.customer_id).await {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "锁定客户（返利政策串行化）"));
    }
    if let Err(error) = guard_overlap(&tx, &checked, Some(id)).await {
        let _ = tx.rollback().await;
        return Err(error);
    }
    let updated = model::update_policy(&tx, id, &new, &checked.rules, operator, Utc::now()).await;
    match updated {
        Ok(affected) if affected > 0 => {}
        Ok(_) => {
            let _ = tx.rollback().await;
            return Err(conflict("返利政策在保存期间被删除或改走，请刷新后重试"));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "更新返利政策"));
        }
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交改政策事务"))?;
    detail_of(id).await
}

/// 软删返利政策。
///
/// 物理行与规则行都留着（审计）：列表 / 详情查不到，M3-16 的结算单会存政策快照，
/// 因此删政策不会让已结算的返利「无从解释」。已被结算引用时的硬拦在 M3-16 实现
/// （那时才有引用表可查）；本卡只保证「删了不再出现在政策列表里」。
pub async fn delete_policy_of(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let affected = model::soft_delete(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除返利政策"))?;
    if affected == 0 {
        return Err(not_found("返利政策不存在或已删除"));
    }
    Ok(())
}

// ==================== HTTP 入口 ====================

/// 返利政策分页。
#[nik::permission(
    path = "/business/erp/rebate-policy/page",
    method = "GET",
    permission = "business:erp:rebate-policy:list"
)]
#[nik::get("/business/erp/rebate-policy/page")]
pub async fn rebate_policy_page_api(
    Query(filter): Query<RebatePolicyFilter>,
) -> ResResult<PageData<Vec<RebatePolicyRes>>> {
    Ok(Res::ok(page_policies_of(filter).await?))
}

/// 返利政策详情（含规则档位）。
#[nik::permission(
    path = "/business/erp/rebate-policy/{id}",
    method = "GET",
    permission = "business:erp:rebate-policy:list"
)]
#[nik::get("/business/erp/rebate-policy/{id}")]
pub async fn rebate_policy_detail_api(Path(id): Path<String>) -> ResResult<RebatePolicyRes> {
    Ok(Res::ok(detail_policy_of(&id).await?))
}

/// 新建返利政策。
#[nik::permission(
    path = "/business/erp/rebate-policy",
    method = "POST",
    permission = "business:erp:rebate-policy:create"
)]
#[nik::post("/business/erp/rebate-policy")]
pub async fn rebate_policy_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<RebatePolicyWriteReq>,
) -> ResResult<RebatePolicyRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_policy_of(operator, req).await?))
}

/// 编辑返利政策（全量覆盖：政策头 + 规则整单替换）。
#[nik::permission(
    path = "/business/erp/rebate-policy/{id}",
    method = "PUT",
    permission = "business:erp:rebate-policy:update"
)]
#[nik::put("/business/erp/rebate-policy/{id}")]
pub async fn rebate_policy_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<RebatePolicyWriteReq>,
) -> ResResult<RebatePolicyRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_policy_of(operator, &id, req).await?))
}

/// 软删返利政策。
#[nik::permission(
    path = "/business/erp/rebate-policy/{id}",
    method = "DELETE",
    permission = "business:erp:rebate-policy:delete"
)]
#[nik::delete("/business/erp/rebate-policy/{id}")]
pub async fn rebate_policy_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_policy_of(operator, &id).await?;
    Ok(Res::ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::sales::rebate_dto::{default_unit_of, unit_label};

    /// 权限码与 V0438 逐字一致；四个码互不相同。
    #[test]
    fn permission_codes_match_migration() {
        assert_eq!(
            REBATE_POLICY_PERMISSIONS,
            [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE]
        );
        assert_eq!(PERM_LIST, "business:erp:rebate-policy:list");
        assert_eq!(PERM_CREATE, "business:erp:rebate-policy:create");
        assert_eq!(PERM_UPDATE, "business:erp:rebate-policy:update");
        assert_eq!(PERM_DELETE, "business:erp:rebate-policy:delete");
        let unique: std::collections::BTreeSet<&str> = REBATE_POLICY_PERMISSIONS.into_iter().collect();
        assert_eq!(unique.len(), 4, "四个码必须互不相同");
        // 写码不能是读码（system::authz_tests::o13）
        for code in [PERM_CREATE, PERM_UPDATE, PERM_DELETE] {
            assert!(!code.ends_with(":list"), "{code} 是写接口的码，不能是 :list");
        }
        println!("[M3-15] 权限码 list/create/update/delete（四码互不相同、写码非读码）✓");
    }

    /// 非法 UUID / 空 id 必须 400 + 10001，且文案点名字段。
    #[test]
    fn invalid_uuid_is_400_and_names_the_field() {
        let error = parse_uuid("not-a-uuid", "customer_id").expect_err("非法 UUID 必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("customer_id"), "文案要点名字段：{}", error.msg);
        assert!(
            parse_optional_uuid(Some("  "), "customer_id")
                .expect("空白不报错")
                .is_none()
        );
        println!("[M3-15] 非法/空 UUID → 400 + 10001 并点名字段；空白可选参数 = 不筛 ✓");
    }

    /// 日期严格解析（不许 `2026/11/17`、`20261117`）。
    #[test]
    fn parse_date_is_strict() {
        assert_eq!(
            parse_date(Some("2026-11-17"), "effective_from").expect("合法日期"),
            NaiveDate::from_ymd_opt(2026, 11, 17)
        );
        assert!(parse_date(Some("   "), "effective_to").expect("空串 = 没传").is_none());
        for bad in ["2026/11/17", "20261117", "2026-13-01", "tomorrow"] {
            let error = parse_date(Some(bad), "effective_from").expect_err("非法日期必须被拒");
            assert_eq!(error.status, 400);
            assert!(error.msg.contains("effective_from"), "{}", error.msg);
        }
        println!("[M3-15] 日期只认 yyyy-MM-dd（4 种非法形状全拒）✓");
    }

    /// 政策头校验：名称必填 / 超长、生效日必填、失效日不早于生效日、状态域外。
    #[test]
    fn header_validation() {
        let base = valid_req();
        checked_policy(&base).expect("合法按量政策");

        let mut empty_name = base.clone();
        empty_name.policy_name = "   ".to_string();
        assert!(
            checked_policy(&empty_name)
                .expect_err("空名称")
                .msg
                .contains("policy_name")
        );

        let mut long_name = base.clone();
        long_name.policy_name = "返".repeat(MAX_NAME_LEN + 1);
        assert!(checked_policy(&long_name).expect_err("超长名称").msg.contains("200"));

        let mut no_from = base.clone();
        no_from.effective_from = String::new();
        assert!(
            checked_policy(&no_from)
                .expect_err("缺生效日")
                .msg
                .contains("effective_from")
        );

        let mut inverted = base.clone();
        inverted.effective_from = "2026-11-17".to_string();
        inverted.effective_to = Some("2026-11-16".to_string());
        assert!(
            checked_policy(&inverted)
                .expect_err("失效日早于生效日")
                .msg
                .contains("不能早于")
        );

        let mut bad_status = base;
        bad_status.status = Some(7);
        assert_eq!(
            checked_policy(&bad_status).expect_err("状态域外").code,
            error_code::PARAM_INVALID
        );
        println!("[M3-15] 政策头：名称必填/≤200、生效日必填、失效日≥生效日、状态 1/0 ✓");
    }

    /// 三种规则类型经 HTTP 契约全链路校验：按量/按额可省单位、阶梯必须给单位。
    #[test]
    fn three_rule_types_end_to_end() {
        let by_qty = valid_req();
        let checked = checked_policy(&by_qty).expect("按量");
        assert_eq!(checked.rate_unit, "PER_UNIT");
        assert_eq!(checked.rules.len(), 1);

        let by_amount = RebatePolicyWriteReq {
            rule_type: " by_amount ".to_string(),
            rate_unit: None,
            rules: vec![rebate_dto::RebateRuleReq {
                tier_from: Some(rust_decimal::Decimal::ZERO),
                tier_to: None,
                rate: Some(rust_decimal::Decimal::from(2)),
                remark: None,
            }],
            ..valid_req()
        };
        let checked = checked_policy(&by_amount).expect("按额");
        assert_eq!(checked.rate_unit, "PERCENT");
        assert_eq!(checked.settle_period, "MONTH");

        let tiered = RebatePolicyWriteReq {
            rule_type: "TIERED".to_string(),
            rate_unit: None,
            rules: vec![
                rebate_dto::RebateRuleReq {
                    tier_from: Some(rust_decimal::Decimal::from(100)),
                    tier_to: None,
                    rate: Some(rust_decimal::Decimal::from(2)),
                    remark: None,
                },
                rebate_dto::RebateRuleReq {
                    tier_from: Some(rust_decimal::Decimal::ZERO),
                    tier_to: Some(rust_decimal::Decimal::from(100)),
                    rate: Some(rust_decimal::Decimal::from(1)),
                    remark: None,
                },
            ],
            ..valid_req()
        };
        let error = checked_policy(&tiered).expect_err("阶梯没给单位");
        assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);
        println!("[M3-15] 三种类型：按量/按额省单位即默认、阶梯省单位 400 + 66089 ✓");
    }

    /// 默认单位与类型配对（`default_unit_of` 只服务 TIERED 之外的两种）。
    #[test]
    fn default_unit_only_for_flat_types() {
        assert_eq!(default_unit_of("BY_QTY"), Some("PER_UNIT"));
        assert_eq!(default_unit_of("BY_AMOUNT"), Some("PERCENT"));
        assert_eq!(default_unit_of("TIERED"), None);
        assert_eq!(unit_label("PER_UNIT"), "元/件");
        assert_eq!(settle_label("QUARTER"), "季度");
        println!("[M3-15] 默认单位只给按量/按额；阶梯必须显式 ✓");
    }

    /// 测试夹具：一份能通过校验的「按量」政策入参。
    fn valid_req() -> RebatePolicyWriteReq {
        RebatePolicyWriteReq {
            policy_name: "2026 年度返利（按量）".to_string(),
            customer_id: "018f0000-0000-7000-8000-000000000001".to_string(),
            rule_type: "BY_QTY".to_string(),
            rate_unit: None,
            settle_period: "month".to_string(),
            effective_from: "2026-01-01".to_string(),
            effective_to: Some("2026-12-31".to_string()),
            status: None,
            remark: Some("  谈定：每件返 0.5 元  ".to_string()),
            rules: vec![rebate_dto::RebateRuleReq {
                tier_from: Some(rust_decimal::Decimal::ZERO),
                tier_to: None,
                rate: Some(rust_decimal::Decimal::new(5, 1)),
                remark: None,
            }],
        }
    }
}
