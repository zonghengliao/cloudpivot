//! 薪资项接口层（`biz_salary_item` · HR-31）：设计 §5.2 的 #32~#35 四条接口。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地卡 |
//! |:--:|---|---|---|---|
//! | 32 | GET | `/business/hr/salary-item/page` | `business:hr:salary-item:list` | HR-31 |
//! | 33 | POST | `/business/hr/salary-item` | `business:hr:salary-item:create` | HR-31 |
//! | 34 | PUT | `/business/hr/salary-item/{id}` | `business:hr:salary-item:update` | HR-31 |
//! | 35 | POST | `/business/hr/salary-item/validate` | `business:hr:salary-item:update` | HR-31 |
//!
//! 数据层在 [`crate::business::hr::salary_item_model`]（唯一 SQL 出口），入参 / 校验 /
//! 回执契约在 [`crate::business::hr::salary_item_dto`]，公式的结构检查与依赖图在
//! [`crate::business::hr::salary_item_formula`]。
//!
//! # 为什么没有 DELETE
//!
//! 设计 §5.2 的 46 条里没有「删除薪资项」。设计 §6.2.4 的删除保护（`is_system=1` 不可删 /
//! 不可停用、被薪资标准引用的项删前要 409）**落在停用路径上**：配置表的生命周期是
//! 「停用」而不是「删行」——历史薪资单的 `detail_item.item_id` 指向它（`ON DELETE SET NULL`），
//! 删了行等于把历史工资条上的项名抹掉。权限码 `salary-item:delete` 由 V0387 种下了但本卡不注册路由，
//! 发现这一点的下游（HR-45 收口）可以决定是补接口还是删权限码。
//!
//! # 三道守卫（都落在 `PUT` 上）
//!
//! | 守卫 | 为什么 |
//! |---|---|
//! | `item_code` 不可改 | 公式按 `{编码}` 引用、`biz_employee_salary.item_code` 是弱引用（不建 FK）；改名 = 引用它的公式与标准一起失效，且**没有报错** |
//! | `is_system` 项不可停用 / 不可改类型 | `GROSS` / `NET` 是试算与凭证的恒等式基线（`biz_salary_detail` 的 CHECK 靠它们），停用后账算不出来 |
//! | 被引用的项不可停用 | 引用它的员工薪资标准（条数）与别的项公式（编码清单）会变成「指向不存在的东西」；试算时才炸，界面上看不出 |
//!
//! # 事务边界
//!
//! `PUT` 在**一个事务**里做「锁行 → 全部校验（编码 / 顺序占用、公式依赖、停用引用）→ 改行」。
//! 校验如果在事务外做，两个并发请求能同时通过「顺序 40 没人占」再一前一后写，第二个撞
//! `uk_biz_sal_item_sort` 拿到的是 500 级译文而不是「40 被谁占了」。写后不再有外部副作用
//! （不写日志表、不发通知），所以**不需要 AGENTS.md 约定 11 的补偿路径**。
//!
//! # 检查按钮（#35）为什么不报 400
//!
//! 「公式有错」是**查询结论**，不是请求错误：前端要拿 `errors[]` 逐条显示、拿
//! `dependency_order` 画顺序。所以 `ok=false` 也返回 200；只有「没传 formula」「id 不存在」
//! 这类请求本身的问题才 400 / 404。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::salary_item_dto::{
    self, SalaryItemPageQuery, SalaryItemRes, SalaryItemValidateReq, SalaryItemValidateRes, SalaryItemWriteReq,
};
use crate::business::hr::salary_item_model;
use crate::business::shared::state_machine::Operator;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0387 的 sys_menu.auth_code 逐字一致） ====================

/// 配置列表（含公式）。
pub const PERM_SALARY_ITEM_LIST: &str = "business:hr:salary-item:list";

/// 新建薪资项。
pub const PERM_SALARY_ITEM_CREATE: &str = "business:hr:salary-item:create";

/// 修改薪资项 / 公式检查（设计 §5.2 #35 复用 `update` 码：能改的人才能检查）。
pub const PERM_SALARY_ITEM_UPDATE: &str = "business:hr:salary-item:update";

// ==================== 错误原语 ====================

/// `409 + 68008`：当前数据状态下不允许这么做（不是格式错，也不是撞键）。
fn rule_blocked(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::HR_SALARY_RULE_BLOCKED, message.into())
}

/// `409 + 10003`：占用冲突（编码 / 计算顺序已被别的项占了）。
fn occupied(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

// ==================== ㉜ 列表 ====================

/// 薪资项配置列表（设计 §5.2 #32：含 `formula`）。
#[nik::permission(
    path = "/business/hr/salary-item/page",
    method = "GET",
    permission = "business:hr:salary-item:list"
)]
#[nik::get("/business/hr/salary-item/page")]
pub async fn salary_item_page(Query(query): Query<SalaryItemPageQuery>) -> ResResult<PageData<Vec<SalaryItemRes>>> {
    let (list, total, page, page_size) = list_items(&query).await?;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 列表的业务实现（接口与真库用例共用同一条路径）。
pub async fn list_items(
    query: &SalaryItemPageQuery,
) -> std::result::Result<(Vec<SalaryItemRes>, i64, u64, u64), ApiError> {
    let (page, page_size) = salary_item_dto::normalize_page(query.page, query.page_size);
    let filter = salary_item_dto::filter_of(query)?;
    let (rows, total) = salary_item_model::page(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询薪资项"))?;
    Ok((
        rows.iter().map(SalaryItemRes::from_row).collect(),
        total,
        page,
        page_size,
    ))
}

// ==================== ㉝ 新建 ====================

/// 新建薪资项（设计 §5.2 #33：校验公式可解析 + 变量可达 + 无环）。
#[nik::permission(
    path = "/business/hr/salary-item",
    method = "POST",
    permission = "business:hr:salary-item:create"
)]
#[nik::post("/business/hr/salary-item")]
pub async fn salary_item_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SalaryItemWriteReq>,
) -> ResResult<SalaryItemRes> {
    let operator: Operator = (&claims).into();
    Ok(Res::ok(create_item(operator, &req).await?))
}

/// 新建的业务实现（接口与真库用例共用同一条路径）。
///
/// 顺序：`parse_write`（形状 / 列宽 / 域值 / 科目分档）→ 公式依赖检查（结构 + 未定义变量 + 环）
/// → 编码 / 顺序占用预检 → 插入（唯一索引兜底）。
pub async fn create_item(operator: Operator, req: &SalaryItemWriteReq) -> std::result::Result<SalaryItemRes, ApiError> {
    // 未指定排序时才查「下一个可用序号」（省一次查询）
    let fallback_sort = match req.sort_order {
        Some(value) => value,
        None => salary_item_model::next_sort_order(db())
            .await
            .map_err(|error| utils::db_error(error, "读取薪资项最大计算顺序"))?,
    };
    let raw_code = req.item_code.as_deref().unwrap_or_default();
    let draft = salary_item_dto::parse_write(req, raw_code, fallback_sort, None)?;

    let snapshots = salary_item_model::list_snapshots(db())
        .await
        .map_err(|error| utils::db_error(error, "读取薪资项依赖图"))?;
    if let Some(text) = draft.formula.as_deref() {
        let check = salary_item_dto::check_formula(text, Some(&draft.item_code), &snapshots);
        if !check.ok() {
            return Err(salary_item_dto::formula_error(&check));
        }
    }

    if let Some(owner) = salary_item_model::code_owner(db(), &draft.item_code, None)
        .await
        .map_err(|error| utils::db_error(error, "查询薪资项编码是否重复"))?
    {
        return Err(occupied(format!(
            "薪资项编码「{}」已被占用（同一公司内编码唯一，未删除的项都算）：请换一个编码，或直接改那一项",
            owner
        )));
    }
    if let Some(owner) = salary_item_model::sort_owner(db(), draft.sort_order, None)
        .await
        .map_err(|error| utils::db_error(error, "查询计算顺序是否被占用"))?
    {
        return Err(occupied(format!(
            "计算顺序 {} 已被薪资项「{}」占用（同一公司内顺序唯一）：换个序号（建议 10 递增，插空用中间值如 15/25）",
            draft.sort_order, owner
        )));
    }

    let id = Uuid::now_v7();
    salary_item_model::insert(db(), id, &draft, operator.id)
        .await
        .map_err(|error| salary_item_model::write_error(error, "新建薪资项"))?;
    let row = salary_item_model::find(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "读回新建的薪资项"))?
        .ok_or_else(salary_item_model::not_found_error)?;
    Ok(SalaryItemRes::from_row(&row))
}

// ==================== ㉞ 修改 ====================

/// 修改薪资项（设计 §5.2 #34：改公式后**旧薪资单不受影响** —— 靠 `expr_snapshot`，不靠禁改）。
#[nik::permission(
    path = "/business/hr/salary-item/{id}",
    method = "PUT",
    permission = "business:hr:salary-item:update"
)]
#[nik::put("/business/hr/salary-item/{id}")]
pub async fn salary_item_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<SalaryItemWriteReq>,
) -> ResResult<SalaryItemRes> {
    let item_id = salary_item_dto::parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(update_item(operator, item_id, &req).await?))
}

/// 修改的业务实现（接口与真库用例共用同一条路径）。
///
/// 锁行 → 编码不可改 → 系统项守卫 → 停用守卫 → 公式检查 → 顺序占用 → 改行 → 提交。
pub async fn update_item(
    operator: Operator,
    id: Uuid,
    req: &SalaryItemWriteReq,
) -> std::result::Result<SalaryItemRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启修改薪资项事务"))?;

    // ---- 锁行（两个并发 PUT 不会各读一份旧值）----
    let existing = match salary_item_model::lock_in(&tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(salary_item_model::not_found_error());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定薪资项"));
        }
    };

    // ---- 编码不可改（传了就必须与原值一致）----
    if let Some(raw) = req.item_code.as_deref().map(str::trim).filter(|text| !text.is_empty())
        && raw != existing.item_code
    {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "薪资项编码不可修改（{} → {}）：公式按 `{{编码}}` 引用它、员工薪资标准的 item_code 也是弱引用，\
             改名会让引用它的公式与标准一起失效且不报错。要换编码请新建一项、把引用改过去、再把旧项停用",
            existing.item_code, raw
        )));
    }

    // ---- 形状校验（缺省的开关保持原值）----
    let draft = match salary_item_dto::parse_write(
        req,
        &existing.item_code,
        existing.sort_order,
        Some(salary_item_dto::ExistingFlags {
            is_taxable: existing.is_taxable,
            is_active: existing.is_active,
        }),
    ) {
        Ok(draft) => draft,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };

    // ---- 系统项守卫（GROSS / NET 是恒等式基线）----
    if existing.is_system == 1 {
        if draft.is_active == 0 {
            let _ = tx.rollback().await;
            return Err(rule_blocked(format!(
                "系统项「{}」不许停用：它是薪资恒等式（应发 / 实发）的基线，停用后试算与凭证都对不上",
                existing.item_code
            )));
        }
        if draft.item_type != existing.item_type || draft.calc_type != existing.calc_type {
            let _ = tx.rollback().await;
            return Err(rule_blocked(format!(
                "系统项「{}」的类型 / 算法不可改（{} / {} → {} / {}）：它由迁移种下，是所有薪资单的基准口径",
                existing.item_code, existing.item_type, existing.calc_type, draft.item_type, draft.calc_type
            )));
        }
    }

    // ---- 停用守卫：被引用的项不许停用（否则引用它的地方静默失效）----
    if draft.is_active == 0 && existing.is_active == 1 {
        let salary_refs = salary_item_model::count_employee_refs(&tx, &existing.item_code)
            .await
            .map_err(|error| utils::db_error(error, "统计引用该薪资项的员工薪资标准"))?;
        let formula_refs = salary_item_model::formula_referrers(&tx, &existing.item_code, Some(id))
            .await
            .map_err(|error| utils::db_error(error, "统计公式引用该薪资项的其它项"))?;
        if salary_refs > 0 || !formula_refs.is_empty() {
            let _ = tx.rollback().await;
            let mut detail = Vec::new();
            if salary_refs > 0 {
                detail.push(format!("{salary_refs} 条员工薪资标准用它取数"));
            }
            if !formula_refs.is_empty() {
                detail.push(format!("薪资项 {} 的公式引用它", formula_refs.join(" / ")));
            }
            return Err(rule_blocked(format!(
                "薪资项「{}」正在被使用，不能停用（{}）：先把这些引用处理掉（改标准 / 改公式），再停用",
                existing.item_code,
                detail.join("；")
            )));
        }
    }

    // ---- 公式依赖检查（引用 / 环 / 拓扑）----
    let snapshots = salary_item_model::list_snapshots(&tx)
        .await
        .map_err(|error| utils::db_error(error, "读取薪资项依赖图"))?;
    if let Some(text) = draft.formula.as_deref() {
        let check = salary_item_dto::check_formula(text, Some(&existing.item_code), &snapshots);
        if !check.ok() {
            let _ = tx.rollback().await;
            return Err(salary_item_dto::formula_error(&check));
        }
    }

    // ---- 顺序占用（改号时）----
    if draft.sort_order != existing.sort_order
        && let Some(owner) = salary_item_model::sort_owner(&tx, draft.sort_order, Some(id))
            .await
            .map_err(|error| utils::db_error(error, "查询计算顺序是否被占用"))?
    {
        let _ = tx.rollback().await;
        return Err(occupied(format!(
            "计算顺序 {} 已被薪资项「{}」占用（同一公司内顺序唯一）：换个序号（建议 10 递增，插空用中间值如 15/25）",
            draft.sort_order, owner
        )));
    }

    let affected = match salary_item_model::update(&tx, id, &draft, operator.id).await {
        Ok(count) => count,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(salary_item_model::write_error(error, "修改薪资项"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(salary_item_model::not_found_error());
    }
    if let Err(error) = tx.commit().await {
        // 只有一条 UPDATE，提交失败 = 什么都没落库（无外部副作用，不需要补偿路径）
        return Err(utils::db_error(error, "提交修改薪资项事务"));
    }

    let row = salary_item_model::find(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "读回修改后的薪资项"))?
        .ok_or_else(salary_item_model::not_found_error)?;
    Ok(SalaryItemRes::from_row(&row))
}

// ==================== ㉟ 公式检查 ====================

/// 公式检查（设计 §5.2 #35 + §6.2.4 的「检查」按钮：语法 + 变量可达 + 依赖图）。
#[nik::permission(
    path = "/business/hr/salary-item/validate",
    method = "POST",
    permission = "business:hr:salary-item:update"
)]
#[nik::post("/business/hr/salary-item/validate")]
pub async fn salary_item_validate(Json(req): Json<SalaryItemValidateReq>) -> ResResult<SalaryItemValidateRes> {
    Ok(Res::ok(validate_formula(&req).await?))
}

/// 检查的业务实现（接口与真库用例共用同一条路径）。
///
/// `id` 优先（改已有项时后端自己查编码，免得前端把编码传错）；否则用 `item_code`；
/// 两者都没有时按「新建时还没编码」处理（不参与拓扑序，只判引用可达性）。
pub async fn validate_formula(req: &SalaryItemValidateReq) -> std::result::Result<SalaryItemValidateRes, ApiError> {
    let text = req.formula.as_deref().map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(salary_item_dto::param_invalid(
            "formula 必填：检查按钮要把当前编辑框里的公式原文传上来（空公式无法检查）",
        ));
    }

    let snapshots = salary_item_model::list_snapshots(db())
        .await
        .map_err(|error| utils::db_error(error, "读取薪资项依赖图"))?;

    let self_code = match (req.id.as_deref(), req.item_code.as_deref()) {
        (Some(raw), _) => {
            let id = salary_item_dto::parse_uuid(raw, "id")?;
            let row = salary_item_model::find(db(), id)
                .await
                .map_err(|error| utils::db_error(error, "查询待检查的薪资项"))?
                .ok_or_else(salary_item_model::not_found_error)?;
            Some(row.item_code)
        }
        (None, Some(code)) => {
            let code = code.trim();
            if code.is_empty() { None } else { Some(code.to_string()) }
        }
        (None, None) => None,
    };

    let check = salary_item_dto::check_formula(text, self_code.as_deref(), &snapshots);
    Ok(salary_item_dto::validate_res(check, &snapshots))
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 权限码与 V0387 的种子逐字一致（码写错 = 谁都没有这个权限，接口永远 403）。
    #[test]
    fn permission_codes_are_seeded_by_v0387() {
        let seed = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");
        for code in [PERM_SALARY_ITEM_LIST, PERM_SALARY_ITEM_CREATE, PERM_SALARY_ITEM_UPDATE] {
            assert!(seed.contains(code), "V0387 没有种下权限码 {code}");
        }
        // 设计 §5.2 #35 复用 update 码 → validate 不许有独立的码
        assert!(
            !seed.contains("business:hr:salary-item:validate"),
            "validate 复用 update 码（设计 §5.2）"
        );
    }

    /// 四条路由的路径必须与设计 §5.2 / HR-09 的 ROUTE_CONTRACT 一致。
    #[test]
    fn route_paths_match_design() {
        let source = include_str!("salary_item_handler.rs");
        for path in [
            "\"/business/hr/salary-item/page\"",
            "\"/business/hr/salary-item\"",
            "\"/business/hr/salary-item/{id}\"",
            "\"/business/hr/salary-item/validate\"",
        ] {
            assert!(source.contains(path), "缺路由 {path}");
        }
        // 路径不带 /api 前缀（AGENTS.md 约定 8：前缀由 [router] prefix 注入）。
        // 拼接出 needle 而不是写字面量：字面量会出现在本文件源码里，include_str! 自测必然命中。
        let needle = format!("nik::get(\"{}\"", "/api");
        assert!(!source.contains(&needle), "路由宏路径不许带 /api 前缀");
    }
}
