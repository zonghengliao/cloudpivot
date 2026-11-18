//! 标准成本卡 **版本表接口层**（成本核算 · M5-08【M5·成本】标准成本：物料 × 期间）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-standard/...`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-standard` | `…:cost-method:list` | 版本列表（物料 / 状态 / 期间 / 日期 / 关键字） |
//! | 2 | GET | `/business/erp/cost-standard/effective` | `…:cost-method:list` | **某物料某期间生效的是哪一版**（含明细与换版提示） |
//! | 3 | GET | `/business/erp/cost-standard/period-sheet` | `…:cost-method:list` | 期间 × 物料 矩阵（版本表主视图） |
//! | 4 | GET | `/business/erp/cost-standard/{id}` | `…:cost-method:list` | 版本详情（卡头 + 标准 BOM） |
//! | 5 | POST | `/business/erp/cost-standard` | `…:cost-method:create` | 新建一版（含标准 BOM，事务） |
//! | 6 | PUT | `/business/erp/cost-standard/{id}` | `…:cost-method:update` | 改一版（未生效可全改；已生效只许改停止日 / 状态 / 备注） |
//! | 7 | POST | `/business/erp/cost-standard/{id}/status` | `…:cost-method:update` | 生效 / 失效（失效不占生效窗口） |
//! | 8 | DELETE | `/business/erp/cost-standard/{id}` | `…:cost-method:delete` | 软删（已生效过 409，改失效） |
//!
//! # 为什么「复用」cost-method 的四个权限码，而不是新开四个
//!
//! 1. 标准成本卡与「按什么方法核算」是同一类东西：**全公司财务口径配置**（不是部门单据），
//!    维护角色也完全一致（`FINANCE_MANAGER` 改、`FINANCE_DIRECTOR` 只看）。
//!    在 `V0460` 里这四码已经按这个口径授权完毕，新开码不改变任何人能看到什么。
//! 2. 新开四个码在 M5-18（标准成本页）落地前**没有菜单行承载** ⇒ 就是四条
//!    「有码无菜单」的死码，`system::menu::permission_audit_tests::n14` 当场判红；
//!    而先插一条指向不存在组件的菜单行，`n08` 又会红（侧栏点开 404）。
//!    M5-07（批次成本页）遇到的是同一个岔路口，同样选了复用（`cost-object:list`）。
//! 3. 将来真要解耦成 `cost-standard:*`，那是「页面码 / 接口码分离」的整体动作
//!    （AGENTS 约定 14 / PERM-01），届时连同菜单一起改，不在这里留半截。
//!
//! # 三条纪律（与 dto 模块头呼应，这里负责执行）
//!
//! 1. **同物料启用版本不许重叠** ⇒ 事务内 `pg_advisory_xact_lock(物料)` 串行化 → 重查版本
//!    → 判重叠（409 + `66108`）；库层 `ex_biz_cost_std_active_window` 兜「绕过 handler 直插」。
//! 2. **已生效过的版本不许改口径**（`effective_from < 今天`）：只允许改停止日 / 状态 / 备注
//!    （409 + `66109`）。**并且**要求「重算出来的金额 == 库里的金额」，否则拒绝 —— 免得一次
//!    「只改备注」的保存把历史标准悄悄重算成另一个数。
//! 3. **已生效过的版本不许删**（409 + `66110`）：要停用请走 `POST …/{id}/status`。
//!
//! # 事务与「提交后失败」
//!
//! 写路径只有一处提交后动作：**回查详情**（只读）。所以不存在「提交后失败留死单」的面
//! （AGENTS 约定 11）：回查失败也只是「写成功但没读到」，重试是幂等的（重叠会被判 409，
//! 不会再写一版）。本卡不出凭证、不动账 —— 标准成本只用于差异分析（ARCH-9 D-15）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_method_handler` / `cost_batch_finish` 同款：axum handler 要 `AuthClaims` 才跑得起来，
//! 单测调不到。把「校验 + 事务 + 落库」收进 `create_standard` / `update_standard` /
//! `set_status` / `delete_standard` 之后，真库用例走**同一条代码路径**（只跳过认证）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{FixedOffset, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_standard_dto::{
    self as dto, PeriodSheetQuery, PeriodSheetRes, StandardCreateReq, StandardDetailRes, StandardEffectiveQuery,
    StandardEffectiveRes, StandardItemRes, StandardPageQuery, StandardRes, StandardStatusReq, StandardStatusRes,
    StandardUpdateReq,
};
use crate::business::erp::cost::cost_standard_model::{self as model, PeriodSheetFilter, StandardFilter, StandardRow};
use crate::business::erp::model_base::normalize_page;
use cloudpivot_kernel::utils;

// ==================== 权限码（复用 V0460 的四条，理由见模块头） ====================

/// 列表 / 详情 / 生效版本 / 期间矩阵（4 条读接口）。
pub const PERM_LIST: &str = "business:erp:cost-method:list";
/// 新建一版。
pub const PERM_CREATE: &str = "business:erp:cost-method:create";
/// 改版 / 生效 / 失效。
pub const PERM_UPDATE: &str = "business:erp:cost-method:update";
/// 软删。
pub const PERM_DELETE: &str = "business:erp:cost-method:delete";

/// 本模块用到的全部权限码（与 `V0460` 四行菜单逐字一致；**零新增码**）。
pub const COST_STANDARD_PERMISSIONS: [&str; 4] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE];

// ==================== 小件 ====================

/// 今日（CST）—— 与 `invoice::today_cst` / `cost_method_handler::today_cst` 同口径。
fn today_cst() -> NaiveDate {
    Utc::now()
        .with_timezone(&FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"))
        .date_naive()
}

/// 行 → 列表出参。
fn to_res(row: &StandardRow) -> StandardRes {
    StandardRes {
        id: row.id.to_string(),
        material_id: row.material_id.to_string(),
        material_code: row.material_code.clone(),
        material_name: row.material_name.clone(),
        material_type: row.material_type.clone(),
        material_spec: row.material_spec.clone(),
        version_no: row.version_no,
        effective_from: dto::date_text(row.effective_from),
        effective_to: row.effective_to.map(dto::date_text),
        status: row.status,
        status_label: dto::status_label(row.status).to_string(),
        std_material_cost: dto::amount_text(row.std_material_cost),
        std_labor_cost: dto::amount_text(row.std_labor_cost),
        std_overhead_cost: dto::amount_text(row.std_overhead_cost),
        std_total_cost: dto::amount_text(row.std_total_cost),
        std_labor_hours: dto::hours_text(row.std_labor_hours),
        std_labor_rate: dto::amount_text(row.std_labor_rate),
        items_count: row.items_count,
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}

/// 明细行 → 出参。
fn to_item_res(row: &model::ItemRow) -> StandardItemRes {
    StandardItemRes {
        id: row.id.to_string(),
        line_no: row.line_no,
        component_material_id: row.component_material_id.to_string(),
        component_material_code: row.component_material_code.clone(),
        component_material_name: row.component_material_name.clone(),
        std_qty: dto::amount_text(row.std_qty),
        std_price: dto::amount_text(row.std_price),
        std_amount: dto::amount_text(row.std_amount),
    }
}

/// 取活行（不存在 / 已软删 → 404）。
async fn load(id: Uuid) -> std::result::Result<StandardRow, ApiError> {
    model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询标准成本版本"))?
        .ok_or_else(|| dto::not_found(id))
}

/// 组装详情（卡头 + 标准 BOM）。
async fn detail_of(row: &StandardRow) -> std::result::Result<StandardDetailRes, ApiError> {
    let items = model::items_of(db(), row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询标准 BOM"))?;
    Ok(StandardDetailRes {
        head: to_res(row),
        items: items.iter().map(to_item_res).collect(),
    })
}

/// 按 id 组装详情（建 / 改 / 生效后的回单走它，出参不回显请求里的数字）。
async fn detail_by_id(id: Uuid) -> std::result::Result<StandardDetailRes, ApiError> {
    let row = load(id).await?;
    detail_of(&row).await
}

/// 路径参数 → UUID（非法形状也是 404：这个位置本来就该是 id）。
fn parse_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| {
        ApiError::new(
            404,
            cloudpivot_kernel::shared::error_code::RESOURCE_NOT_FOUND,
            format!("标准成本版本 id 不是合法 UUID（收到「{raw}」）"),
        )
    })
}

/// 校验成品 / 半成品物料：必须存在且**启用**。返回其标称（报文用）。
///
/// 为什么要求「启用」：标准成本卡是**现在 / 将来**要拿来算差异的口径，给停用物料建新版本
/// 只会产出一张永远算不到数的卡；要分析历史，请用历史版本（它已经在库里）。
async fn ensure_finished_material(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> std::result::Result<model::MaterialRef, ApiError> {
    let material = model::material_of(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "校验标准成本的物料"))?
        .ok_or_else(|| {
            dto::bad_request(format!(
                "material_id（{id}）指向的物料不存在或已删除：标准成本卡必须挂在真实物料上。\
                 请先在「物料」里建好成品 / 半成品，再回来建标准成本"
            ))
        })?;
    if material.status != 1 {
        return Err(dto::bad_request(format!(
            "物料 {} 已停用（status=0）：停用物料不参与生产与成本核算，给它建标准成本卡没有意义。\
             请先启用该物料，或换一个在用物料",
            material.label()
        )));
    }
    if material.material_type == "SERVICE" {
        return Err(dto::bad_request(format!(
            "物料 {} 是服务类（SERVICE）：标准成本卡针对有物料耗用的成品 / 半成品，服务类没有标准 BOM 可算",
            material.label()
        )));
    }
    Ok(material)
}

/// 校验标准 BOM 里的耗用物料：必须都存在（缺失一律点名第几行）。
async fn ensure_components(ex: &impl SqlExecutor, spec: &dto::StandardSpec) -> std::result::Result<(), ApiError> {
    let mut ids: Vec<Uuid> = Vec::with_capacity(spec.items.len());
    for item in &spec.items {
        if !ids.contains(&item.component_material_id) {
            ids.push(item.component_material_id);
        }
    }
    let found = model::materials_of(ex, &ids)
        .await
        .map_err(|error| utils::db_error(error, "校验标准 BOM 的耗用物料"))?;
    let missing: Vec<String> = ids
        .iter()
        .filter(|id| !found.iter().any(|(existing, _)| existing == *id))
        .map(|id| {
            let line_no = spec
                .items
                .iter()
                .position(|item| item.component_material_id == *id)
                .map(|index| index + 1)
                .unwrap_or(0);
            format!("第 {line_no} 行 {id}")
        })
        .collect();
    if !missing.is_empty() {
        return Err(dto::bad_request(format!(
            "标准 BOM 里有耗用物料不存在或已删除：{}。请先在物料主数据里建好再提交",
            missing.join("、")
        )));
    }
    Ok(())
}

/// 版本视图集合（判定用）。
fn views_of(rows: &[StandardRow]) -> Vec<dto::VersionView> {
    rows.iter().map(StandardRow::to_view).collect()
}

/// 写库错误 → 友好 `ApiError`（唯一出口）。
///
/// - `UniqueViolation`：撞的是 `uk_biz_cost_std`（同物料同生效日）或 `uk_biz_cost_std_ver`
///   （同物料版本号） ⇒ 409 + `66108`（handler 预检在并发下可能双双通过）。
/// - `CheckViolation`：值域 / 算数恒等（V0374 的四条 CHECK）⇒ 400 + `10001`。
/// - `Query` 里带 `ex_biz_cost_std_active_window`：库层区间排除约束被触发（有人绕过 handler
///   直插，或 handler 预检与 commit 之间插进了别的写）⇒ 409 + `66108`。
///   这条**按约束名**判而不是按自然语言猜：nik-db-core 只把 23505/23503/23514 映射成独立
///   变体，`23P01`（exclusion_violation）落 `DbError::Query`；约束名是 DDL 里写死的标识符，
///   比报文稳定。
/// - 其它：500（真实原因只进日志）。
fn map_write_error(err: DbError, ctx: &str) -> ApiError {
    let text = format!("{err}");
    match &err {
        DbError::UniqueViolation { .. } => ApiError::new(
            409,
            dto::CODE_OVERLAP,
            "同一物料在同一生效日 / 同一版本号上已经有另一条启用标准成本卡\
             （并发写入撞了 uk_biz_cost_std_active_start / uk_biz_cost_std_ver）。\
             请刷新后直接改已有那一版，或换一个生效日",
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            cloudpivot_kernel::shared::error_code::PARAM_INVALID,
            format!(
                "数据不满足数据库约束{}（标准成本的四条算数恒等 / 值域：总成本 = 料+工+费、\
                 人工 = round(工时×费率,6)、金额非负、停止日不早于生效日）",
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        _ if text.contains("ex_biz_cost_std_active_window") => ApiError::new(
            409,
            dto::CODE_OVERLAP,
            "同一物料在这一段日期已经有一个启用版本（库层区间约束拒绝写入）。\
             请先把旧版停止日收口到新版生效日前一天，或先失效旧版，再重试",
        ),
        _ => utils::db_error(err, ctx),
    }
}

// ==================== 列表 / 详情 / 生效版本 / 期间矩阵 ====================

/// 版本列表分页（本表 SCOPE-EXEMPT，不需要 `principal`）。
pub async fn page_standards(query: StandardPageQuery) -> std::result::Result<PageData<Vec<StandardRes>>, ApiError> {
    let filter = filter_of(&query)?;
    let (page, page_size) = (filter.page, filter.page_size);
    let (rows, total) = model::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询标准成本版本"))?;
    Ok(PageData {
        list: rows.iter().map(to_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 归一化列表筛选（纯逻辑；单测可直接验）。
pub fn filter_of(query: &StandardPageQuery) -> std::result::Result<StandardFilter, ApiError> {
    let material_id = match cost_labor_dto::clean_optional(query.material_id.clone()) {
        None => None,
        Some(raw) => Some(cost_labor_dto::parse_uuid(&raw, "material_id")?),
    };
    let status = match query.status {
        None => None,
        Some(value) if value == dto::STATUS_ACTIVE || value == dto::STATUS_INACTIVE => Some(value),
        Some(other) => {
            return Err(dto::bad_request(format!(
                "status 筛选只能是 1（启用）或 0（失效），收到 {other}"
            )));
        }
    };
    let period = match cost_labor_dto::clean_optional(query.period_code.clone()) {
        None => None,
        Some(raw) => Some(dto::period_window(&raw)?),
    };
    let on_date = match cost_labor_dto::clean_optional(query.on_date.clone()) {
        None => None,
        Some(raw) => Some(cost_labor_dto::parse_date(&raw, "on_date")?),
    };
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(StandardFilter {
        material_id,
        status,
        period,
        on_date,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        page,
        page_size,
    })
}

/// 版本详情。
pub async fn detail_standard(id_raw: &str) -> std::result::Result<StandardDetailRes, ApiError> {
    let id = parse_id(id_raw)?;
    detail_by_id(id).await
}

/// 「某物料某期间（或某一天）生效的是哪一版」。
///
/// # 异常情况
///
/// - `material_id` 缺失 / 非法、期间与日期都缺失、日期不在期间内 ⇒ 400
/// - 物料不存在 / 已删 ⇒ 400
/// - 该日期没有任何启用版本 ⇒ 404（报文给出建卡引导）
pub async fn effective_of(query: StandardEffectiveQuery) -> std::result::Result<StandardEffectiveRes, ApiError> {
    let raw_material = cost_labor_dto::clean_optional(query.material_id.clone())
        .ok_or_else(|| dto::bad_request("material_id（要查哪个物料的标准成本）不能为空"))?;
    let material_id = cost_labor_dto::parse_uuid(&raw_material, "material_id")?;
    let period = match cost_labor_dto::clean_optional(query.period_code.clone()) {
        None => None,
        Some(raw) => Some((raw.clone(), dto::period_window(&raw)?)),
    };
    let explicit_on = match cost_labor_dto::clean_optional(query.on_date.clone()) {
        None => None,
        Some(raw) => Some(cost_labor_dto::parse_date(&raw, "on_date")?),
    };
    let on_date = match (&period, explicit_on) {
        (Some((code, (start, end))), Some(date)) => {
            if date < *start || date > *end {
                return Err(dto::bad_request(format!(
                    "on_date（{date}）不在 period_code（{code}）所指的 {start} ~ {end} 之内：两个条件互相矛盾"
                )));
            }
            date
        }
        (Some((_, (_, end))), None) => *end,
        (None, Some(date)) => date,
        (None, None) => {
            return Err(dto::bad_request(
                "period_code（yyyy-MM）或 on_date（yyyy-MM-dd）至少给一个：要查哪一天的标准成本",
            ));
        }
    };

    let material = model::material_of(db(), material_id)
        .await
        .map_err(|error| utils::db_error(error, "查询标准成本的物料"))?
        .ok_or_else(|| dto::bad_request(format!("material_id（{material_id}）指向的物料不存在或已删除")))?;

    let row = model::find_covering_in(db(), material_id, on_date)
        .await
        .map_err(|error| utils::db_error(error, "查询期间生效的标准成本版本"))?
        .ok_or_else(|| dto::no_effective_version(&material.label(), on_date))?;

    let version = detail_of(&row).await?;
    let (switch_in_period, other_versions, note) = match &period {
        None => (
            false,
            Vec::new(),
            format!(
                "按 {on_date} 取到 {} 的 v{}（{}）。标准成本只用于差异分析，不改存货计价（ARCH-9 D-15）",
                material.label(),
                row.version_no,
                dto::window_text(row.effective_from, row.effective_to)
            ),
        ),
        Some((code, (start, end))) => {
            let others = model::other_versions_in_window(db(), material_id, *start, *end, row.id)
                .await
                .map_err(|error| utils::db_error(error, "查询期间内切换过的标准成本版本"))?;
            let switch = !others.is_empty();
            let note = if switch {
                format!(
                    "期间 {code} 内换过标准成本：本期按**期末 {end}** 生效的 v{} 取数（差异分析按期末口径），\
                     但本期间内还生效过 {} 版（{}）。若差异报表要按「分开两段」算，属 M5-31 的切换策略，本卡不含",
                    row.version_no,
                    others.len(),
                    others
                        .iter()
                        .map(|other| {
                            format!(
                                "v{}（{}）",
                                other.version_no,
                                dto::window_text(other.effective_from, other.effective_to)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("、")
                )
            } else {
                format!(
                    "期间 {code} 取 {} 的 v{}（{}）：整期一个版本，期末 {end} 与期初 {start} 口径一致",
                    material.label(),
                    row.version_no,
                    dto::window_text(row.effective_from, row.effective_to)
                )
            };
            (switch, others.iter().map(to_res).collect(), note)
        }
    };

    Ok(StandardEffectiveRes {
        period_code: period.map(|(code, _)| code),
        on_date: dto::date_text(on_date),
        version,
        switch_in_period,
        other_versions,
        note,
    })
}

/// 期间 × 物料 矩阵（版本表主视图：一行一个物料，取期间末生效的那一版）。
pub async fn period_sheet(query: PeriodSheetQuery) -> std::result::Result<PageData<Vec<PeriodSheetRes>>, ApiError> {
    let filter = sheet_filter_of(&query)?;
    let (page, page_size) = (filter.page, filter.page_size);
    let (rows, total) = model::period_sheet(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询期间标准成本矩阵"))?;
    Ok(PageData {
        list: rows
            .iter()
            .map(|row| PeriodSheetRes {
                head: to_res(&row.row),
                switch_in_period: row.switch_in_period,
            })
            .collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 归一化矩阵筛选（期间必填）。
pub fn sheet_filter_of(query: &PeriodSheetQuery) -> std::result::Result<PeriodSheetFilter, ApiError> {
    let raw = cost_labor_dto::clean_optional(query.period_code.clone())
        .ok_or_else(|| dto::bad_request("period_code（yyyy-MM）不能为空：期间矩阵按期间取数"))?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(PeriodSheetFilter {
        period: Some(dto::period_window(&raw)?),
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        page,
        page_size,
    })
}

// ==================== 新建 / 修改 ====================

/// 新建一版标准成本（一个事务：锁物料 → 判重叠 → 建卡头 → 写标准 BOM）。
///
/// # 异常情况
///
/// - 值域 / 期间矛盾 / 明细重复物料 / 数值超精度 ⇒ 400（`10001`）
/// - 物料不存在 / 已停用 / 是服务类，或明细里物料不存在 ⇒ 400
/// - 同物料启用版本生效期重叠 ⇒ 409 + `66108`
/// - 同物料同生效日（并发下唯一键兜底）⇒ 409 + `66108`
pub async fn create_standard(
    operator: Option<Uuid>,
    req: StandardCreateReq,
) -> std::result::Result<StandardDetailRes, ApiError> {
    let spec = dto::spec_from_create(&req)?;
    ensure_finished_material(db(), spec.material_id).await?;
    ensure_components(db(), &spec).await?;

    let id = Uuid::now_v7();
    let amounts = spec.amounts();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启标准成本新建事务"))?;
    let outcome = create_in_tx(&tx, id, &spec, &amounts, operator).await;
    match outcome {
        Ok(()) => tx
            .commit()
            .await
            .map_err(|error| utils::db_error(error, "提交标准成本新建事务"))?,
        Err(error) => {
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "标准成本新建回滚失败");
            }
            return Err(error);
        }
    }
    detail_by_id(id).await
}

/// 新建事务体：锁物料 → 取现有版本 → 判重叠 → 取版本号 → 写卡头 + 明细。
async fn create_in_tx(
    tx: &impl SqlExecutor,
    id: Uuid,
    spec: &dto::StandardSpec,
    amounts: &dto::Amounts,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    model::lock_material(tx, spec.material_id)
        .await
        .map_err(|error| utils::db_error(error, "锁定标准成本物料"))?;
    let rows = model::versions_of(tx, spec.material_id)
        .await
        .map_err(|error| utils::db_error(error, "查询同物料已有标准成本版本"))?;
    let views = views_of(&rows);
    if let Some(hit) = dto::find_overlap(&views, spec, None) {
        let label = rows
            .iter()
            .find(|row| row.id == hit.id)
            .map(StandardRow::material_label)
            .unwrap_or_else(|| spec.material_id.to_string());
        return Err(dto::overlap_error(
            &label,
            hit.version_no,
            &dto::window_text(hit.effective_from, hit.effective_to),
            &spec.window_text(),
        ));
    }
    let version_no = model::next_version_no(&rows);
    model::insert_head(tx, id, version_no, spec, amounts, operator)
        .await
        .map_err(|error| map_write_error(error, "新建标准成本版本"))?;
    model::replace_items(tx, id, &spec.items)
        .await
        .map_err(|error| utils::db_error(error, "写标准 BOM"))?;
    Ok(())
}

/// 改一版标准成本。
///
/// # 异常情况
///
/// - 不存在 / 已删 ⇒ 404
/// - 已生效过却改了物料 / 生效起始日 / 工时 / 工资率 / 制造费用 / 标准 BOM ⇒ 409 + `66109`
/// - 已生效过、且库里的金额与「按明细重算的金额」不一致 ⇒ 409 + `66109`（不做静默重算）
/// - 改完与别的启用版本重叠 ⇒ 409 + `66108`
pub async fn update_standard(
    operator: Option<Uuid>,
    id_raw: &str,
    req: StandardUpdateReq,
) -> std::result::Result<StandardDetailRes, ApiError> {
    let id = parse_id(id_raw)?;
    let current = load(id).await?;
    let current_items = model::items_of(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询标准 BOM"))?;
    let current_spec = current.to_spec(current_items.iter().map(model::ItemRow::to_spec).collect());
    let spec = dto::spec_from_update(&req, &current_spec)?;

    if spec.material_id != current_spec.material_id {
        ensure_finished_material(db(), spec.material_id).await?;
    }
    ensure_components(db(), &spec).await?;

    let amounts = spec.amounts();
    let today = today_cst();
    if current_spec.effective_from < today {
        let rewritten = dto::rewritten_fields(&current_spec, &spec);
        if !rewritten.is_empty() {
            return Err(dto::locked_effective(
                &current.material_label(),
                current.version_no,
                current_spec.effective_from,
                today,
                &rewritten.join(" / "),
            ));
        }
        // 只改停止日 / 状态 / 备注：还得要求「重算金额 == 库里金额」，否则一次
        // 「只改备注」的保存会把历史标准悄悄重算成另一个数（库层没有「料 = Σ明细」的 CHECK）。
        let stored = dto::Amounts {
            material: current.std_material_cost,
            labor: current.std_labor_cost,
            overhead: current.std_overhead_cost,
        };
        if stored != amounts || current.std_total_cost != amounts.total() {
            return Err(dto::locked_effective(
                &current.material_label(),
                current.version_no,
                current_spec.effective_from,
                today,
                "标准成本金额（库里的料 / 工 / 费与明细、工时×费率不一致）",
            ));
        }
    }

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启标准成本修改事务"))?;
    let outcome = update_in_tx(&tx, id, &current, &spec, &amounts, req.items.is_some(), operator).await;
    match outcome {
        Ok(()) => tx
            .commit()
            .await
            .map_err(|error| utils::db_error(error, "提交标准成本修改事务"))?,
        Err(error) => {
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "标准成本修改回滚失败");
            }
            return Err(error);
        }
    }
    detail_by_id(id).await
}

/// 修改事务体：按物料取锁（新旧物料都锁，固定顺序防死锁）→ 判重叠 → 改卡头 → 视需要换明细。
async fn update_in_tx(
    tx: &impl SqlExecutor,
    id: Uuid,
    current: &StandardRow,
    spec: &dto::StandardSpec,
    amounts: &dto::Amounts,
    replace_items: bool,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let mut locks = vec![current.material_id, spec.material_id];
    locks.sort_unstable();
    locks.dedup();
    for material_id in locks {
        model::lock_material(tx, material_id)
            .await
            .map_err(|error| utils::db_error(error, "锁定标准成本物料"))?;
    }
    let rows = model::versions_of(tx, spec.material_id)
        .await
        .map_err(|error| utils::db_error(error, "查询同物料已有标准成本版本"))?;
    let views = views_of(&rows);
    if let Some(hit) = dto::find_overlap(&views, spec, Some(id)) {
        let label = rows
            .iter()
            .find(|row| row.id == hit.id)
            .map(StandardRow::material_label)
            .unwrap_or_else(|| spec.material_id.to_string());
        return Err(dto::overlap_error(
            &label,
            hit.version_no,
            &dto::window_text(hit.effective_from, hit.effective_to),
            &spec.window_text(),
        ));
    }
    let affected = model::update_head(tx, id, spec, amounts, operator)
        .await
        .map_err(|error| map_write_error(error, "修改标准成本版本"))?;
    if affected == 0 {
        // 查过存在、改却是 0 行 ⇒ 被并发删掉了
        return Err(dto::not_found(id));
    }
    if replace_items {
        model::replace_items(tx, id, &spec.items)
            .await
            .map_err(|error| utils::db_error(error, "重写标准 BOM"))?;
    }
    Ok(())
}

// ==================== 生效 / 失效 / 删除 ====================

/// 生效 / 失效。
///
/// - **生效**：与同物料其它启用版本重叠 ⇒ 409 + `66108`（先收口 / 先失效旧版）。
/// - **失效**：不占生效窗口，随时可做；但会把「该版本覆盖的期间」从标准成本取数里抽掉，
///   所以出参带 `warnings`（今天起没标准 / 历史差异重算会缺依据），界面要显示出来。
/// - 重复置同一状态 = 幂等返回（不写库）。
///
/// # 异常情况
///
/// 不存在 ⇒ 404；`status` 缺失 / 越界 ⇒ 400；生效时重叠 ⇒ 409 + `66108`。
pub async fn set_status(
    operator: Option<Uuid>,
    id_raw: &str,
    req: StandardStatusReq,
) -> std::result::Result<StandardStatusRes, ApiError> {
    let id = parse_id(id_raw)?;
    let target = req
        .status
        .ok_or_else(|| dto::bad_request("status 不能为空：1 = 生效，0 = 失效"))?;
    let target = dto::normalize_status(Some(target))?;
    let current = load(id).await?;
    let reason = cost_labor_dto::clean_optional(req.reason.clone());
    if let Some(reason) = reason.as_deref()
        && reason.chars().count() > dto::REMARK_MAX
    {
        return Err(dto::bad_request(format!(
            "reason（变更原因）最长 {} 个字符（收到 {} 个）",
            dto::REMARK_MAX,
            reason.chars().count()
        )));
    }

    if current.status == target {
        return Ok(StandardStatusRes {
            version: detail_of(&current).await?,
            message: format!(
                "{} v{} 已经是「{}」状态，本次未做修改",
                current.material_label(),
                current.version_no,
                dto::status_label(target)
            ),
            warnings: Vec::new(),
        });
    }

    let today = today_cst();
    let mut warnings: Vec<String> = Vec::new();
    if target == dto::STATUS_ACTIVE {
        let rows = model::versions_of(db(), current.material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询同物料已有标准成本版本"))?;
        let views = views_of(&rows);
        let spec = dto::StandardSpec {
            material_id: current.material_id,
            effective_from: current.effective_from,
            effective_to: current.effective_to,
            std_labor_hours: current.std_labor_hours,
            std_labor_rate: current.std_labor_rate,
            std_overhead_cost: current.std_overhead_cost,
            status: target,
            remark: current.remark.clone(),
            items: Vec::new(),
        };
        if let Some(hit) = dto::find_overlap(&views, &spec, Some(id)) {
            let label = rows
                .iter()
                .find(|row| row.id == hit.id)
                .map(StandardRow::material_label)
                .unwrap_or_else(|| current.material_label());
            return Err(dto::overlap_error(
                &label,
                hit.version_no,
                &dto::window_text(hit.effective_from, hit.effective_to),
                &dto::window_text(current.effective_from, current.effective_to),
            ));
        }
    } else {
        // 失效：把后果说清楚（不拦 —— 「停用一份标准」是正当操作，拦了就只能改库）
        if dto::window_covers(current.effective_from, current.effective_to, today) {
            warnings.push(format!(
                "今天（{today}）起，物料 {} 在 {} 这段日期不再有生效的标准成本：\
                 差异分析（M5-11 起）按今天取数会找不到依据，直到启用新版或被别的版本覆盖",
                current.material_label(),
                dto::window_text(current.effective_from, current.effective_to)
            ));
        }
        warnings.push(format!(
            "已发生过的历史差异若按本版（v{}，{}）计算，重算时将改用当时生效的其它版本；\
             已落库的差异行（biz_cost_variance.standard_id）保持不变",
            current.version_no,
            dto::window_text(current.effective_from, current.effective_to)
        ));
    }

    let remark = append_status_note(current.remark.as_deref(), target, reason.as_deref(), today)?;
    let affected = model::set_status(db(), id, target, remark.as_deref(), operator)
        .await
        .map_err(|error| map_write_error(error, "置标准成本版本状态"))?;
    if affected == 0 {
        return Err(dto::not_found(id));
    }
    let updated = load(id).await?;
    Ok(StandardStatusRes {
        version: detail_of(&updated).await?,
        message: format!(
            "{} v{} 已置为「{}」（生效区间 {}）",
            updated.material_label(),
            updated.version_no,
            dto::status_label(target),
            dto::window_text(updated.effective_from, updated.effective_to)
        ),
        warnings,
    })
}

/// 备注里追加「状态变更留痕」：`[生效 2026-11-19] 原因`。
///
/// 表里没有状态变更历史表（本卡不建），所以用备注留痕 —— 但**只在给了原因时**追加，
/// 不静默改动业务备注；追加后仍未超长（[`dto::REMARK_MAX`]）。
fn append_status_note(
    current: Option<&str>,
    target: i16,
    reason: Option<&str>,
    today: NaiveDate,
) -> std::result::Result<Option<String>, ApiError> {
    let Some(reason) = reason else {
        return Ok(current.map(str::to_string));
    };
    let line = format!("[{} {today}] {reason}", dto::status_label(target));
    let combined = match current.map(str::trim).filter(|text| !text.is_empty()) {
        None => line,
        Some(existing) => format!("{existing}\n{line}"),
    };
    // 备注列是 TEXT（无长度上限），但接口契约按 REMARK_MAX 收口；留痕追加也守同一条线。
    if combined.chars().count() > dto::REMARK_MAX {
        return Err(dto::bad_request(format!(
            "备注 + 变更原因超过 {} 个字符（当前备注太长了）：请先把旧备注精简，再置{}",
            dto::REMARK_MAX,
            dto::status_label(target)
        )));
    }
    Ok(Some(combined))
}

/// 软删一版。
///
/// 已生效过（`effective_from < 今天`）⇒ 409 + `66110`（版本行是历史差异的依据，改失效即可）。
/// 未生效的版本可以删（录错卡删掉重来的唯一路径）。
pub async fn delete_standard(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_id(id_raw)?;
    let current = load(id).await?;
    let today = today_cst();
    if current.effective_from < today {
        return Err(dto::delete_in_use(
            &current.material_label(),
            current.version_no,
            current.effective_from,
            today,
        ));
    }
    let affected = model::soft_delete(db(), id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除标准成本版本"))?;
    if affected == 0 {
        return Err(dto::not_found(id));
    }
    Ok(())
}

/// 全部权限码（与其余成本模块 `all_permissions` 同形）。
pub fn all_permissions() -> Vec<&'static str> {
    COST_STANDARD_PERMISSIONS.to_vec()
}

// ==================== HTTP 入口 ====================

/// 版本列表分页。
#[nik::permission(
    path = "/business/erp/cost-standard",
    method = "GET",
    permission = "business:erp:cost-method:list"
)]
#[nik::get("/business/erp/cost-standard")]
pub async fn cost_standard_page_api(Query(query): Query<StandardPageQuery>) -> ResResult<PageData<Vec<StandardRes>>> {
    Ok(Res::ok(page_standards(query).await?))
}

/// 某物料某期间（某天）生效的标准成本版本。
#[nik::permission(
    path = "/business/erp/cost-standard/effective",
    method = "GET",
    permission = "business:erp:cost-method:list"
)]
#[nik::get("/business/erp/cost-standard/effective")]
pub async fn cost_standard_effective_api(
    Query(query): Query<StandardEffectiveQuery>,
) -> ResResult<StandardEffectiveRes> {
    Ok(Res::ok(effective_of(query).await?))
}

/// 期间 × 物料 矩阵。
#[nik::permission(
    path = "/business/erp/cost-standard/period-sheet",
    method = "GET",
    permission = "business:erp:cost-method:list"
)]
#[nik::get("/business/erp/cost-standard/period-sheet")]
pub async fn cost_standard_period_sheet_api(
    Query(query): Query<PeriodSheetQuery>,
) -> ResResult<PageData<Vec<PeriodSheetRes>>> {
    Ok(Res::ok(period_sheet(query).await?))
}

/// 版本详情。
#[nik::permission(
    path = "/business/erp/cost-standard/{id}",
    method = "GET",
    permission = "business:erp:cost-method:list"
)]
#[nik::get("/business/erp/cost-standard/{id}")]
pub async fn cost_standard_detail_api(Path(id): Path<String>) -> ResResult<StandardDetailRes> {
    Ok(Res::ok(detail_standard(&id).await?))
}

/// 新建一版。
#[nik::permission(
    path = "/business/erp/cost-standard",
    method = "POST",
    permission = "business:erp:cost-method:create"
)]
#[nik::post("/business/erp/cost-standard")]
pub async fn cost_standard_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<StandardCreateReq>,
) -> ResResult<StandardDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_standard(operator, req).await?))
}

/// 改一版。
#[nik::permission(
    path = "/business/erp/cost-standard/{id}",
    method = "PUT",
    permission = "business:erp:cost-method:update"
)]
#[nik::put("/business/erp/cost-standard/{id}")]
pub async fn cost_standard_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StandardUpdateReq>,
) -> ResResult<StandardDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_standard(operator, &id, req).await?))
}

/// 生效 / 失效。
#[nik::permission(
    path = "/business/erp/cost-standard/{id}/status",
    method = "POST",
    permission = "business:erp:cost-method:update"
)]
#[nik::post("/business/erp/cost-standard/{id}/status")]
pub async fn cost_standard_status_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StandardStatusReq>,
) -> ResResult<StandardStatusRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(set_status(operator, &id, req).await?))
}

/// 软删一版。
#[nik::permission(
    path = "/business/erp/cost-standard/{id}",
    method = "DELETE",
    permission = "business:erp:cost-method:delete"
)]
#[nik::delete("/business/erp/cost-standard/{id}")]
pub async fn cost_standard_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_standard(operator, &id).await?;
    Ok(Res::ok(()))
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Duration;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    // ---------- 权限 / 路由 ----------

    /// 四个码与 V0460 的菜单行逐字一致（**零新增码**：新开码在 M5-18 之前就是死码）。
    #[test]
    fn permission_codes_are_reused_from_the_cost_method_menu() {
        assert_eq!(
            COST_STANDARD_PERMISSIONS,
            [
                "business:erp:cost-method:list",
                "business:erp:cost-method:create",
                "business:erp:cost-method:update",
                "business:erp:cost-method:delete",
            ]
        );
        assert_eq!(all_permissions().len(), 4);
        for code in COST_STANDARD_PERMISSIONS {
            assert!(code.starts_with("business:erp:cost-method:"), "{code}");
            assert!(!code.contains("cost-standard"), "本卡不新开码：{code}");
        }
    }

    /// 8 条路由由 4 个码承载：读 4 条（列表 / 生效 / 矩阵 / 详情）都挂 list，写 3 条 + 删 1 条。
    #[test]
    fn routes_are_paired_with_permissions() {
        assert_eq!(PERM_LIST, "business:erp:cost-method:list");
        assert_eq!(PERM_CREATE, "business:erp:cost-method:create");
        assert_eq!(PERM_UPDATE, "business:erp:cost-method:update");
        assert_eq!(PERM_DELETE, "business:erp:cost-method:delete");
        // 4 读 + 1 建 + 1 改 + 1 状态 + 1 删 = 8 条路由，4 个码
        assert_eq!(
            COST_STANDARD_PERMISSIONS.len(),
            4,
            "8 条路由由 4 个码承载（list 管 4 条读接口，update 管改 + 生效/失效）"
        );
    }

    // ---------- 筛选归一化 ----------

    /// 列表筛选：空白 = 不筛、状态越界 400、期间与日期都要能解析。
    #[test]
    fn page_filter_normalizes_and_rejects_unknown() {
        let query = StandardPageQuery {
            material_id: None,
            status: Some(0),
            period_code: Some(" 2026-11 ".to_string()),
            on_date: Some("2026-11-30".to_string()),
            keyword: Some("  FG-001  ".to_string()),
            page: Some(0),
            page_size: Some(0),
        };
        let filter = filter_of(&query).expect("合法筛选");
        assert_eq!(filter.status, Some(dto::STATUS_INACTIVE));
        assert_eq!(
            filter.period,
            Some((date("2026-11-01"), date("2026-11-30"))),
            "期间两端都要解析出来"
        );
        assert_eq!(filter.on_date, Some(date("2026-11-30")));
        assert_eq!(filter.keyword.as_deref(), Some("FG-001"), "关键字要 trim");
        assert_eq!(filter.page, 1, "页号 0 → 1");
        assert!(filter.page_size > 0);

        let blank = StandardPageQuery {
            material_id: Some("   ".to_string()),
            period_code: Some("   ".to_string()),
            keyword: Some("   ".to_string()),
            ..Default::default()
        };
        let filter = filter_of(&blank).expect("空白 = 不筛");
        assert!(filter.material_id.is_none() && filter.period.is_none() && filter.keyword.is_none());

        let bad_status = StandardPageQuery {
            status: Some(2),
            ..Default::default()
        };
        assert_eq!(filter_of(&bad_status).expect_err("状态 2").code, 10001);

        let bad_period = StandardPageQuery {
            period_code: Some("2026-13".to_string()),
            ..Default::default()
        };
        assert_eq!(filter_of(&bad_period).expect_err("月份 13").status, 400);
    }

    /// 期间矩阵：期间必填（缺了 400，不是「返回全表」）。
    #[test]
    fn period_sheet_requires_period() {
        let missing = PeriodSheetQuery::default();
        let err = sheet_filter_of(&missing).expect_err("缺期间");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("period_code"), "{}", err.msg);

        let ok = PeriodSheetQuery {
            period_code: Some("2026-02".to_string()),
            keyword: Some(" FG ".to_string()),
            page: Some(2),
            page_size: Some(500),
        };
        let filter = sheet_filter_of(&ok).expect("合法矩阵筛选");
        assert_eq!(filter.period, Some((date("2026-02-01"), date("2026-02-28"))));
        assert_eq!(filter.keyword.as_deref(), Some("FG"));
        assert_eq!(filter.page, 2);
        assert_eq!(filter.page_size, 200, "每页上限 200（model_base::MAX_PAGE_SIZE）");
    }

    // ---------- 生效 / 失效的备注留痕 ----------

    /// 给原因才追加，格式固定；不给原因不动备注。
    #[test]
    fn status_note_is_appended_only_with_reason() {
        let today = date("2026-11-19");
        assert_eq!(
            append_status_note(None, dto::STATUS_INACTIVE, None, today).expect("无原因"),
            None
        );
        assert_eq!(
            append_status_note(Some("旧备注"), dto::STATUS_ACTIVE, None, today).expect("无原因"),
            Some("旧备注".to_string())
        );
        assert_eq!(
            append_status_note(Some("旧备注"), dto::STATUS_INACTIVE, Some("标准过时"), today).expect("有原因"),
            Some("旧备注\n[失效 2026-11-19] 标准过时".to_string())
        );
        assert_eq!(
            append_status_note(None, dto::STATUS_ACTIVE, Some("发布"), today).expect("首条备注"),
            Some("[生效 2026-11-19] 发布".to_string())
        );
    }

    /// 备注 + 原因超长 ⇒ 400（不静默截断：截掉的是留痕本身）。
    #[test]
    fn status_note_rejects_overlong_remark() {
        let today = date("2026-11-19");
        let long = "x".repeat(dto::REMARK_MAX);
        let err = append_status_note(Some(&long), dto::STATUS_INACTIVE, Some("停用"), today).expect_err("超长应 400");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("字符"), "{}", err.msg);
    }

    /// 今天按 CST 取（「已生效」判定与「收口到昨天」都依赖它）。
    #[test]
    fn today_is_cst_not_utc() {
        let today = today_cst();
        let utc_today = Utc::now().date_naive();
        assert!(
            today == utc_today || today == utc_today + Duration::days(1),
            "CST 日期只能是 UTC 今天或 UTC 明天（实测 {today} vs {utc_today}）"
        );
    }
}
