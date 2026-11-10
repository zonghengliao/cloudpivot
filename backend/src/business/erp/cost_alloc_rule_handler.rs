//! 分摊规则接口层（成本核算 · ARCH-9 §5.2 #20–#24 · §4.4）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-alloc-rule/...`（约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 20 | GET | `/business/erp/cost-alloc-rule` | `business:erp:cost-alloc-rule:list` | 分页列表（期间 / 车间 / 科目 / 标准 / 状态 / 关键字） |
//! | 21 | POST | `/business/erp/cost-alloc-rule` | `business:erp:cost-alloc-rule:create` | 新建一条规则版本 |
//! | 22 | PUT | `/business/erp/cost-alloc-rule/{id}` | `business:erp:cost-alloc-rule:update` | 改规则（已参与分摊的版本只允许改名 / 备注 / 状态） |
//! | 23 | DELETE | `/business/erp/cost-alloc-rule/{id}` | `business:erp:cost-alloc-rule:delete` | 软删（已产生分摊结果则 409） |
//! | 24 | POST | `/business/erp/cost-alloc-rule/preview` | `business:erp:cost-alloc-rule:preview` | **试算**：给期间（+ 可选规则）→ 逐对象分摊额，**不落库** |
//!
//! 设计 §5.3 给的正是这 5 个码，`V0378` 已种页面菜单 + 4 个按钮菜单
//! （「新增规则 / 修改规则 / 删除规则 / 分摊试算」），所以 CRUD 是完整四件套（含 PUT）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_labor_handler` / `cost_overhead_handler` / `cost_object_handler` 同款：
//! axum handler 要 `AuthClaims` 才能跑，单测调不到；把「校验 + 落库 + 试算」收进
//! `create_rule` / `update_rule` / `delete_rule` / `page_rules` / `preview_allocations`
//! 之后，真库用例走**同一条代码路径**（只跳过认证），操作人用 `Option<Uuid>` 传。
//!
//! # 版本化的两条纪律（§4.4.1「改规则不溯及历史」）
//!
//! 1. **同一 (车间, 科目, 生效日) 只能有一条版本**：否则算某期「取哪个版本」没有确定答案
//!    （入口 409 拦；库里由 `effective_from` 排序 + rule_code tiebreak 兜底）。
//! 2. **已参与过分摊的版本不许改口径**：`biz_cost_alloc_result` 里已经有它的分摊结果，
//!    把 `basis` / 科目 / 车间 / 生效期往回改，等于改写历史成本。改口径的正确姿势是
//!    「新建一条 `effective_from = 下期` 的规则」，旧版本继续为历史期间服务。
//!    允许改的只有 `rule_name` / `remark` / `status`（停用不影响历史结果）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_alloc_rule_dto::{
    self as dto, AllocObject, CostAllocPreviewGroup, CostAllocPreviewLine, CostAllocPreviewReq, CostAllocPreviewRes,
    CostAllocPreviewUnallocated, CostAllocRuleCreateReq, CostAllocRuleFilter, CostAllocRuleListRes,
    CostAllocRulePageQuery, CostAllocRuleUpdateReq, ObjectBasis,
};
use crate::business::erp::cost_alloc_rule_model::{self as model, CostAllocRuleRow};
use crate::business::erp::cost_labor_dto;
use crate::business::erp::model_base::normalize_page;
use crate::business::erp::model_master;
use crate::utils;

// ==================== 权限码（与设计 §5.3 / V0378 菜单种子逐字一致） ====================

/// 列表。
pub const PERM_LIST: &str = "business:erp:cost-alloc-rule:list";
/// 新增。
pub const PERM_CREATE: &str = "business:erp:cost-alloc-rule:create";
/// 修改。
pub const PERM_UPDATE: &str = "business:erp:cost-alloc-rule:update";
/// 删除（软删）。
pub const PERM_DELETE: &str = "business:erp:cost-alloc-rule:delete";
/// 试算。
pub const PERM_PREVIEW: &str = "business:erp:cost-alloc-rule:preview";

/// 分摊规则全部权限码（设计 §5.3 的 5 个）。
pub const ALLOC_RULE_PERMISSIONS: [&str; 5] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE, PERM_PREVIEW];

// ==================== 引用校验 ====================

/// 车间必须是 `sys_dept` 的现有部门；返回名称（快照来源）。
async fn ensure_dept(dept_id: Uuid) -> std::result::Result<String, ApiError> {
    match crate::business::erp::cost_overhead_model::dept_name_of(dept_id)
        .await
        .map_err(|error| utils::db_error(error, "校验分摊规则的车间"))?
    {
        Some(name) => Ok(name),
        None => Err(dto::bad_request(format!(
            "dept_id（{dept_id}）指向的车间（部门）不存在或已删除"
        ))),
    }
}

/// 科目必须是「存在 + 末级 + 启用」（账套现状校验，`4101.*` 已在 dto 拦过）。
///
/// 只查前缀会放过三种到结算才炸的输入：账套里没有的编码、汇总科目、已停用科目。
/// 三者统一 400 + `66060`（动作都是「换科目」），报文点名原因。
async fn ensure_account(code: &str) -> std::result::Result<(), ApiError> {
    let account = model_master::account_find_by_code(code)
        .await
        .map_err(|error| utils::db_error(error, "校验分摊规则的科目"))?
        .ok_or_else(|| dto::account_state_invalid(code, None, "在账套（biz_account）里不存在或已删除"))?;
    if account.is_leaf != 1 {
        return Err(dto::account_state_invalid(
            code,
            Some(&account.account_name),
            "不是末级科目（它是汇总科目，下面还有子科目）",
        ));
    }
    if account.status != 1 {
        return Err(dto::account_state_invalid(
            code,
            Some(&account.account_name),
            "已在账套里停用（status=0）",
        ));
    }
    Ok(())
}

// ==================== 出参组装 ====================

/// 行 → 列表出参。
fn to_list_res(row: &CostAllocRuleRow) -> CostAllocRuleListRes {
    CostAllocRuleListRes {
        id: row.id.to_string(),
        rule_code: row.rule_code.clone(),
        rule_name: row.rule_name.clone(),
        biz_type: row.biz_type.clone(),
        source_dept_id: row.source_dept_id.to_string(),
        source_dept_name: Some(row.display_dept_name()),
        account_code: row.account_code.clone(),
        account_name: row.account_name.clone(),
        basis: row.basis.clone(),
        basis_label: dto::basis_label(&row.basis).to_string(),
        priority: row.priority,
        effective_from: row.effective_from.to_string(),
        effective_to: row.effective_to.map(|date| date.to_string()),
        status: row.status,
        status_label: dto::status_label(row.status).to_string(),
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}

/// 取活行（不存在 / 已软删 → 404）。
async fn load(id: Uuid) -> std::result::Result<CostAllocRuleRow, ApiError> {
    model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询分摊规则"))?
        .ok_or_else(|| dto::not_found(id))
}

/// 按 id 组装出参（建 / 改后的回单走它）。
async fn row_of(id: Uuid) -> std::result::Result<CostAllocRuleListRes, ApiError> {
    Ok(to_list_res(&load(id).await?))
}

// ==================== 业务逻辑 ====================

/// 新建规则（§5.2 #21）。
pub async fn create_rule(
    operator: Option<Uuid>,
    req: CostAllocRuleCreateReq,
) -> std::result::Result<CostAllocRuleListRes, ApiError> {
    // ① 引用存在性：车间名（快照来源）
    let dept_id = cost_labor_dto::require_uuid(req.dept_id.as_deref(), "dept_id")?;
    let dept_name = ensure_dept(dept_id).await?;
    // ② 纯格式（不碰库）
    let spec = dto::spec_from_create(&req, &dept_name)?;
    // ③ 账套现状
    ensure_account(&spec.account_code).await?;
    // ④ 版本唯一性：同 (车间, 科目, 生效日) 只能一条
    let conflict_code = model::version_conflict(spec.source_dept_id, &spec.account_code, spec.effective_from, None)
        .await
        .map_err(|error| utils::db_error(error, "校验规则版本"))?;
    if let Some(code) = conflict_code {
        return Err(dto::version_duplicated(&code, spec.effective_from));
    }
    // ⑤ 落库
    let id = Uuid::now_v7();
    model::insert(id, &spec, operator)
        .await
        .map_err(|error| dto::map_write_error(error, &spec, "新增分摊规则"))?;
    row_of(id).await
}

/// 改规则（§5.2 #22）。
pub async fn update_rule(
    operator: Option<Uuid>,
    id_raw: &str,
    req: CostAllocRuleUpdateReq,
) -> std::result::Result<CostAllocRuleListRes, ApiError> {
    let id = cost_labor_dto::parse_uuid(id_raw, "id")?;
    let current = load(id).await?;
    // 换车间时必须重新取名字快照
    let new_dept_name = match req.dept_id.as_deref() {
        None => None,
        Some(raw) => {
            let dept_id = cost_labor_dto::require_uuid(Some(raw), "dept_id")?;
            Some(ensure_dept(dept_id).await?)
        }
    };
    let spec = dto::spec_from_update(&req, &current.to_spec(), new_dept_name.as_deref())?;
    ensure_account(&spec.account_code).await?;

    // 已参与过分摊的版本：只允许改名字 / 备注 / 状态（见模块头「版本化的两条纪律」）
    if let Some(periods) = model::allocated_periods(id)
        .await
        .map_err(|error| utils::db_error(error, "查询规则的分摊引用"))?
    {
        let current_spec = current.to_spec();
        let changed = spec.source_dept_id != current_spec.source_dept_id
            || spec.account_code != current_spec.account_code
            || spec.basis != current_spec.basis
            || spec.effective_from != current_spec.effective_from
            || spec.effective_to != current_spec.effective_to;
        if changed {
            return Err(dto::rule_in_use(&periods));
        }
    }

    let conflict_code = model::version_conflict(spec.source_dept_id, &spec.account_code, spec.effective_from, Some(id))
        .await
        .map_err(|error| utils::db_error(error, "校验规则版本"))?;
    if let Some(code) = conflict_code {
        return Err(dto::version_duplicated(&code, spec.effective_from));
    }

    let affected = model::update(id, &spec, operator)
        .await
        .map_err(|error| dto::map_write_error(error, &spec, "修改分摊规则"))?;
    if affected == 0 {
        return Err(dto::not_found(id));
    }
    row_of(id).await
}

/// 软删规则（§5.2 #23）。
///
/// 已产生分摊结果的规则不许删：`biz_cost_alloc_result.rule_id` 会指向一条「删掉的规则」，
/// 界面点不进规则、也解释不清当时按什么口径摊的（AGENTS 约定 11 的「死单」形态）。
pub async fn delete_rule(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = cost_labor_dto::parse_uuid(id_raw, "id")?;
    let _row = load(id).await?;
    if let Some(periods) = model::allocated_periods(id)
        .await
        .map_err(|error| utils::db_error(error, "查询规则的分摊引用"))?
    {
        return Err(dto::rule_in_use(&periods));
    }
    let affected = model::soft_delete(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除分摊规则"))?;
    if affected == 0 {
        // 查过存在、删却是 0 行 ⇒ 被并发删掉了
        return Err(dto::not_found(id));
    }
    Ok(())
}

/// 列表分页（本表不参与行级数据权限，故不需要 `principal`）。
pub async fn page_rules(
    query: CostAllocRulePageQuery,
) -> std::result::Result<PageData<Vec<CostAllocRuleListRes>>, ApiError> {
    let filter = filter_of(&query)?;
    let (page, page_size) = (filter.page, filter.page_size);
    let (rows, total) = model::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询分摊规则"))?;
    Ok(PageData {
        list: rows.iter().map(to_list_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 归一化列表筛选（纯逻辑；单测可直接验）。
pub fn filter_of(query: &CostAllocRulePageQuery) -> std::result::Result<CostAllocRuleFilter, ApiError> {
    let (period_code, period_from, period_to) = match cost_labor_dto::clean_optional(query.period_code.clone()) {
        None => (None, None, None),
        Some(raw) => {
            let period = cost_labor_dto::normalize_period(Some(&raw))?;
            let (from, to) = dto::period_bounds(&period)?;
            (Some(period), Some(from), Some(to))
        }
    };
    let account_code = match cost_labor_dto::clean_optional(query.account_code.clone()) {
        None => None,
        Some(raw) => Some(dto::normalize_account_code(Some(&raw))?),
    };
    let basis = match cost_labor_dto::clean_optional(query.basis.clone()) {
        None => None,
        Some(raw) => Some(dto::normalize_basis(Some(&raw))?),
    };
    let status = match query.status {
        None => None,
        Some(value) => Some(dto::normalize_status(Some(value))?),
    };
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(CostAllocRuleFilter {
        period_code,
        period_from,
        period_to,
        dept_id: cost_labor_dto::parse_opt_uuid(query.dept_id.as_deref(), "dept_id")?,
        account_code,
        basis,
        status,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        page,
        page_size,
    })
}

// ==================== 试算（§5.2 #24） ====================

/// 试算：给期间（+ 可选规则）→ 逐规则逐对象的分摊额，**只算不写**。
///
/// 计算链路（§4.4.3）：
/// ```text
/// ① 选规则版本：候选规则按 (车间, 科目) 分组 → 取覆盖该期间且生效最晚的一条
/// ② 池：Σ biz_cost_collect.amount（element=OVERHEAD、不挂对象、三类来源）按 (车间, 科目)
///       —— 科目从来源单据反查（见 model 模块头）
/// ③ 基数：四种标准，按参与分摊的对象逐对象算（§4.4.2）
/// ④ 分摊：最大余数法 + 守恒断言（dto::allocate）
/// ```
pub async fn preview_allocations(req: CostAllocPreviewReq) -> std::result::Result<CostAllocPreviewRes, ApiError> {
    let period_code = cost_labor_dto::normalize_period(req.period_code.as_deref())?;
    let (period_from, period_to) = dto::period_bounds(&period_code)?;
    let wanted = dto::parse_rule_ids(&req.rule_ids)?;

    // ---------- ① 候选规则 + 版本选择 ----------
    let candidates = match wanted {
        Some(ids) => {
            let rows = model::find_by_ids(&ids)
                .await
                .map_err(|error| utils::db_error(error, "查询试算指定的分摊规则"))?;
            for id in &ids {
                if !rows.iter().any(|row| row.id == *id) {
                    return Err(dto::not_found(*id));
                }
            }
            rows
        }
        None => model::active_rules()
            .await
            .map_err(|error| utils::db_error(error, "查询启用的分摊规则"))?,
    };
    let mut families: std::collections::BTreeMap<(Uuid, String), Vec<dto::RuleVersion>> =
        std::collections::BTreeMap::new();
    for row in &candidates {
        if row.status != dto::STATUS_ACTIVE {
            continue;
        }
        families
            .entry((row.source_dept_id, row.account_code.clone()))
            .or_default()
            .push(row.to_version());
    }
    let by_id: std::collections::HashMap<Uuid, &CostAllocRuleRow> =
        candidates.iter().map(|row| (row.id, row)).collect();
    let mut selected: Vec<&CostAllocRuleRow> = Vec::new();
    for versions in families.values() {
        if let Some(version) = dto::pick_latest_version(versions, period_from, period_to)
            && let Some(row) = by_id.get(&version.rule_id)
        {
            selected.push(row);
        }
    }
    // 优先级小的先摊（同优先级按规则编码，保证输出稳定）
    selected.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.rule_code.cmp(&right.rule_code))
    });

    // ---------- ② 池 ----------
    let pools = model::pool_rows(&period_code)
        .await
        .map_err(|error| utils::db_error(error, "汇总待分摊费用池"))?;
    let pool_map: std::collections::HashMap<(Uuid, String), nik::prelude::rust_decimal::Decimal> = pools
        .iter()
        .map(|row| ((row.dept_id, row.account_code.clone()), row.amount))
        .collect();

    // 有池但当期没有生效规则覆盖 → 诊断（不静默吞掉，界面要能看见「这笔钱没人摊」）
    let mut unallocated: Vec<CostAllocPreviewUnallocated> = Vec::new();
    let covered: std::collections::HashSet<(Uuid, String)> = selected
        .iter()
        .map(|row| (row.source_dept_id, row.account_code.clone()))
        .collect();
    let orphan_codes = pools
        .iter()
        .filter(|row| !covered.contains(&(row.dept_id, row.account_code.clone())))
        .map(|row| row.account_code.clone())
        .collect::<Vec<_>>();
    let orphan_accounts = model::account_names(&orphan_codes)
        .await
        .map_err(|error| utils::db_error(error, "取科目名"))?;
    let orphan_depts = model::dept_names(
        &pools
            .iter()
            .filter(|row| !covered.contains(&(row.dept_id, row.account_code.clone())))
            .map(|row| row.dept_id)
            .collect::<Vec<_>>(),
    )
    .await
    .map_err(|error| utils::db_error(error, "取车间名"))?;
    for row in &pools {
        let key = (row.dept_id, row.account_code.clone());
        if covered.contains(&key) {
            continue;
        }
        unallocated.push(CostAllocPreviewUnallocated {
            source_dept_id: Some(row.dept_id.to_string()),
            source_dept_name: orphan_depts.get(&row.dept_id).cloned(),
            account_code: Some(row.account_code.clone()),
            account_name: orphan_accounts.get(&row.account_code).cloned(),
            pool_amount: dto::money4(row.amount),
            reason: format!(
                "该车间 + 科目在 {period_code} 没有生效的分摊规则覆盖（新增一条 effective_from \
                 ≤ 期间末日的规则即可）"
            ),
        });
    }
    // 来源单据被删 / 池行没车间：科目或车间都不知道，规则更匹配不上
    for row in model::unassigned_pool(&period_code)
        .await
        .map_err(|error| utils::db_error(error, "统计摊不出去的池行"))?
    {
        unallocated.push(CostAllocPreviewUnallocated {
            source_dept_id: row.dept_id.map(|id| id.to_string()),
            source_dept_name: row.dept_name,
            account_code: row.account_code,
            account_name: None,
            pool_amount: dto::money4(row.amount),
            reason: row.reason,
        });
    }

    // ---------- ③ 基数 + ④ 分摊 ----------
    let mut groups: Vec<CostAllocPreviewGroup> = Vec::with_capacity(selected.len());
    let mut total_pool = nik::prelude::rust_decimal::Decimal::ZERO;
    let mut total_alloc = nik::prelude::rust_decimal::Decimal::ZERO;
    for rule in selected {
        let dept_name = rule.display_dept_name();
        let pool = pool_map
            .get(&(rule.source_dept_id, rule.account_code.clone()))
            .copied()
            .unwrap_or_default();
        let participants = model::participants(&period_code, rule.source_dept_id)
            .await
            .map_err(|error| utils::db_error(error, "查询参与分摊的成本对象"))?;
        let basis_map = basis_map_of(&period_code, rule).await?;
        let objects = participants
            .iter()
            .map(|participant| AllocObject {
                cost_object_id: participant.cost_object_id,
                cost_object_no: participant.object_no.clone(),
                cost_object_status: participant.status.clone(),
                basis_value: dto::basis_value(
                    &rule.basis,
                    basis_map
                        .get(&participant.cost_object_id)
                        .unwrap_or(&ObjectBasis::default()),
                ),
            })
            .collect::<Vec<_>>();
        // 先给出「带名字」的报错（dto::allocate 的兜底文案没有车间 / 科目）
        if pool > nik::prelude::rust_decimal::Decimal::ZERO {
            if objects.is_empty() {
                return Err(dto::no_cost_object(&dept_name, &rule.account_code, pool));
            }
            let basis_total = objects
                .iter()
                .map(|object| object.basis_value)
                .fold(nik::prelude::rust_decimal::Decimal::ZERO, |acc, value| acc + value);
            if basis_total == nik::prelude::rust_decimal::Decimal::ZERO {
                return Err(dto::basis_total_zero(&rule.basis, &dept_name, &rule.account_code, pool));
            }
        }
        let outcome = dto::allocate(pool, &objects)?;
        let rows = outcome
            .lines
            .iter()
            .map(|line| CostAllocPreviewLine {
                cost_object_id: line.cost_object_id.to_string(),
                cost_object_no: line.cost_object_no.clone(),
                cost_object_status: line.cost_object_status.clone(),
                basis_value: dto::money4(line.basis_value),
                alloc_amount: dto::money4(line.alloc_amount),
                rounding_adj: dto::money4(line.rounding_adj),
            })
            .collect::<Vec<_>>();
        let alloc_total = outcome
            .lines
            .iter()
            .map(|line| line.alloc_amount)
            .fold(nik::prelude::rust_decimal::Decimal::ZERO, |acc, value| acc + value);
        let rounding_total = outcome
            .lines
            .iter()
            .map(|line| line.rounding_adj)
            .fold(nik::prelude::rust_decimal::Decimal::ZERO, |acc, value| acc + value);
        groups.push(CostAllocPreviewGroup {
            rule_id: rule.id.to_string(),
            rule_code: rule.rule_code.clone(),
            rule_name: rule.rule_name.clone(),
            source_dept_id: rule.source_dept_id.to_string(),
            source_dept_name: dept_name,
            account_code: rule.account_code.clone(),
            account_name: rule.account_name.clone(),
            basis: rule.basis.clone(),
            basis_label: dto::basis_label(&rule.basis).to_string(),
            pool_amount: dto::money4(outcome.pool_amount),
            basis_total: dto::money4(outcome.basis_total),
            alloc_total: dto::money4(alloc_total),
            rounding_total: dto::money4(rounding_total),
            rows,
        });
        total_pool += outcome.pool_amount;
        total_alloc += alloc_total;
    }

    Ok(CostAllocPreviewRes {
        period_code,
        groups,
        total_pool_amount: dto::money4(total_pool),
        total_alloc_amount: dto::money4(total_alloc),
        unallocated,
    })
}

/// 取规则所需标准下「逐对象的四个候选基数」。
///
/// 只查当前标准要用的那张表（省一次往返，也让「缺数据」的报错能精确到标准）。
async fn basis_map_of(
    period_code: &str,
    rule: &CostAllocRuleRow,
) -> std::result::Result<std::collections::HashMap<Uuid, ObjectBasis>, ApiError> {
    let mut map: std::collections::HashMap<Uuid, ObjectBasis> = std::collections::HashMap::new();
    match rule.basis.as_str() {
        dto::BASIS_LABOR_HOURS | dto::BASIS_MACHINE_HOURS => {
            for row in model::labor_basis(period_code, rule.source_dept_id)
                .await
                .map_err(|error| utils::db_error(error, "汇总工时基数"))?
            {
                let entry = map.entry(row.cost_object_id).or_default();
                entry.labor_hours = row.labor_hours;
                entry.machine_hours = row.machine_hours;
            }
        }
        dto::BASIS_OUTPUT_QTY => {
            for row in model::completed_qty(period_code)
                .await
                .map_err(|error| utils::db_error(error, "汇总完工数量基数"))?
            {
                map.entry(row.cost_object_id).or_default().completed_qty = row.amount;
            }
        }
        dto::BASIS_MATERIAL_COST => {
            for row in model::material_basis(period_code)
                .await
                .map_err(|error| utils::db_error(error, "汇总材料成本基数"))?
            {
                map.entry(row.cost_object_id).or_default().material_cost = row.amount;
            }
        }
        _ => {}
    }
    Ok(map)
}

// ==================== HTTP 入口 ====================

/// 分摊规则分页。
#[nik::permission(
    path = "/business/erp/cost-alloc-rule",
    method = "GET",
    permission = "business:erp:cost-alloc-rule:list"
)]
#[nik::get("/business/erp/cost-alloc-rule")]
pub async fn cost_alloc_rule_page_api(
    Query(query): Query<CostAllocRulePageQuery>,
) -> ResResult<PageData<Vec<CostAllocRuleListRes>>> {
    Ok(Res::ok(page_rules(query).await?))
}

/// 新增规则。
#[nik::permission(
    path = "/business/erp/cost-alloc-rule",
    method = "POST",
    permission = "business:erp:cost-alloc-rule:create"
)]
#[nik::post("/business/erp/cost-alloc-rule")]
pub async fn cost_alloc_rule_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CostAllocRuleCreateReq>,
) -> ResResult<CostAllocRuleListRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_rule(operator, req).await?))
}

/// 修改规则。
#[nik::permission(
    path = "/business/erp/cost-alloc-rule/{id}",
    method = "PUT",
    permission = "business:erp:cost-alloc-rule:update"
)]
#[nik::put("/business/erp/cost-alloc-rule/{id}")]
pub async fn cost_alloc_rule_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CostAllocRuleUpdateReq>,
) -> ResResult<CostAllocRuleListRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_rule(operator, &id, req).await?))
}

/// 删除规则（软删）。
#[nik::permission(
    path = "/business/erp/cost-alloc-rule/{id}",
    method = "DELETE",
    permission = "business:erp:cost-alloc-rule:delete"
)]
#[nik::delete("/business/erp/cost-alloc-rule/{id}")]
pub async fn cost_alloc_rule_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_rule(operator, &id).await?;
    Ok(Res::ok(()))
}

/// 分摊试算（**只算不写**）。
#[nik::permission(
    path = "/business/erp/cost-alloc-rule/preview",
    method = "POST",
    permission = "business:erp:cost-alloc-rule:preview"
)]
#[nik::post("/business/erp/cost-alloc-rule/preview")]
pub async fn cost_alloc_rule_preview_api(Json(req): Json<CostAllocPreviewReq>) -> ResResult<CostAllocPreviewRes> {
    Ok(Res::ok(preview_allocations(req).await?))
}

/// 权限码自检出口（与其余成本模块 `all_permissions` 同形）。
pub fn all_permissions() -> Vec<&'static str> {
    ALLOC_RULE_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 权限 / 路由 ----------

    #[test]
    fn permission_codes_match_design_5_3() {
        assert_eq!(PERM_LIST, "business:erp:cost-alloc-rule:list");
        assert_eq!(PERM_CREATE, "business:erp:cost-alloc-rule:create");
        assert_eq!(PERM_UPDATE, "business:erp:cost-alloc-rule:update");
        assert_eq!(PERM_DELETE, "business:erp:cost-alloc-rule:delete");
        assert_eq!(PERM_PREVIEW, "business:erp:cost-alloc-rule:preview");
        assert_eq!(all_permissions().len(), 5, "设计 §5.3 给了 5 个码");
        for code in ALLOC_RULE_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code}");
            assert_eq!(parts[0], "business", "{code}");
            assert_eq!(parts[1], "erp", "{code}");
            assert_eq!(parts[2], "cost-alloc-rule", "{code}");
        }
    }

    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in [
            "/business/erp/cost-alloc-rule",
            "/business/erp/cost-alloc-rule/{id}",
            "/business/erp/cost-alloc-rule/preview",
        ] {
            assert!(path.starts_with("/business/erp/cost-alloc-rule"), "{path}");
            assert!(!path.starts_with("/api"), "{path} 不能带 /api 前缀（约定 8）");
        }
    }

    // ---------- 列表筛选归一化 ----------

    #[test]
    fn filter_normalizes_period_basis_and_paging() {
        let query = CostAllocRulePageQuery {
            period_code: Some(" 2026-11 ".to_string()),
            account_code: Some("4101.02".to_string()),
            basis: Some("labor_hours".to_string()),
            status: Some(1),
            page: None,
            page_size: Some(0),
            ..Default::default()
        };
        let filter = filter_of(&query).expect("筛选");
        assert_eq!(filter.period_code.as_deref(), Some("2026-11"));
        assert_eq!(
            filter.period_from.map(|date| date.to_string()).as_deref(),
            Some("2026-11-01")
        );
        assert_eq!(
            filter.period_to.map(|date| date.to_string()).as_deref(),
            Some("2026-11-30")
        );
        assert_eq!(filter.account_code.as_deref(), Some("4101.02"));
        assert_eq!(filter.basis.as_deref(), Some("LABOR_HOURS"));
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20);

        let query = CostAllocRulePageQuery {
            page_size: Some(10_000),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect("筛选").page_size, 200, "上限 200");
    }

    #[test]
    fn filter_rejects_bad_values() {
        for bad in ["2026-13", "26-11"] {
            let query = CostAllocRulePageQuery {
                period_code: Some(bad.to_string()),
                ..Default::default()
            };
            let error = filter_of(&query).expect_err(bad);
            assert_eq!(error.status, 400, "{bad}");
        }
        let query = CostAllocRulePageQuery {
            basis: Some("STEP_TRANSFER".to_string()),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect_err("未知标准").status, 400);

        let query = CostAllocRulePageQuery {
            account_code: Some("5602.01".to_string()),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("非制造费用科目");
        assert_eq!(error.code, crate::shared::error_code::ERP_COST_OVERHEAD_ACCOUNT);

        let query = CostAllocRulePageQuery {
            status: Some(7),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect_err("非法状态").status, 400);
    }

    // ---------- 出参映射 ----------

    fn sample_row() -> CostAllocRuleRow {
        let now = nik::prelude::chrono::Utc::now();
        CostAllocRuleRow {
            id: Uuid::now_v7(),
            rule_code: "WL-001".to_string(),
            rule_name: "一车间水电按工时分摊".to_string(),
            biz_type: "OVERHEAD".to_string(),
            source_dept_id: Uuid::now_v7(),
            source_dept_name: "一车间（旧名）".to_string(),
            account_code: "4101.02".to_string(),
            basis: dto::BASIS_LABOR_HOURS.to_string(),
            priority: 10,
            effective_from: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 1).expect("日期"),
            effective_to: None,
            status: dto::STATUS_ACTIVE,
            remark: Some("11 月起改口径".to_string()),
            created_at: now,
            updated_at: now,
            dept_name_live: Some("一车间（新名）".to_string()),
            account_name: Some("制造费用-水电费".to_string()),
        }
    }

    #[test]
    fn list_res_carries_names_and_labels() {
        let res = to_list_res(&sample_row());
        assert_eq!(res.rule_code, "WL-001");
        assert_eq!(res.source_dept_name.as_deref(), Some("一车间（新名）"), "出参用实时名");
        assert_eq!(res.account_name.as_deref(), Some("制造费用-水电费"));
        assert_eq!(res.basis, dto::BASIS_LABOR_HOURS);
        assert_eq!(res.basis_label, "按人工工时分摊", "前端下拉要一句解释（§6.2.3）");
        assert_eq!(res.status_label, "启用");
        assert_eq!(res.effective_from, "2026-11-01");
        assert!(res.effective_to.is_none(), "长期有效");
        assert!(res.created_at.contains('T'), "{}", res.created_at);
    }

    #[test]
    fn list_res_shows_closed_effective_range() {
        let res = to_list_res(&CostAllocRuleRow {
            effective_to: Some(nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 10, 31).expect("日期")),
            ..sample_row()
        });
        assert_eq!(res.effective_to.as_deref(), Some("2026-10-31"));
    }

    /// 试算里「池有金额但无对象 / 基数全 0」的报错必须点名车间 + 科目 + 金额
    /// （否则用户在高亮之前根本不知道是哪条规则卡住）。
    #[test]
    fn preview_errors_name_dept_account_and_amount() {
        let pool = nik::prelude::rust_decimal::Decimal::new(40000000, 4);
        let no_object = dto::no_cost_object("一车间", "4101.02", pool);
        assert_eq!(no_object.status, 409);
        assert_eq!(no_object.code, crate::shared::error_code::ERP_COST_ALLOC_NO_OBJECT);
        assert!(no_object.msg.contains("一车间"), "{}", no_object.msg);
        assert!(no_object.msg.contains("4101.02"), "{}", no_object.msg);
        assert!(no_object.msg.contains("4000"), "{}", no_object.msg);

        let zero = dto::basis_total_zero(dto::BASIS_MACHINE_HOURS, "一车间", "4101.02", pool);
        assert_eq!(zero.code, crate::shared::error_code::ERP_COST_ALLOC_BASIS_ZERO);
        assert!(zero.msg.contains("机器工时"), "{}", zero.msg);
        assert!(zero.msg.contains("4101.02"), "{}", zero.msg);
    }

    /// 版本化纪律的报错要点名「哪些期间用过了」「正确做法是新建版本」。
    #[test]
    fn rule_in_use_message_guides_to_new_version() {
        let error = dto::rule_in_use("2026-10 / 2026-11");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("2026-10 / 2026-11"), "{}", error.msg);
        assert!(error.msg.contains("新建一条规则"), "{}", error.msg);
    }
}
