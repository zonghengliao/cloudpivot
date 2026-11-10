//! 手工费用台账接口层（成本核算 · ARCH-9 §5.2 #17–#19）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-overhead/...`（约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 17 | GET | `/business/erp/cost-overhead` | `business:erp:cost-overhead:list` | 分页列表（期间 / 对象 / 车间 / 费用项目 / 关键字） |
//! | 18 | POST | `/business/erp/cost-overhead` | `business:erp:cost-overhead:create` | 新增一条费用（科目合法性与金额在服务端校验） |
//! | 19 | DELETE | `/business/erp/cost-overhead/{id}` | `business:erp:cost-overhead:delete` | 软删（已被成本计算归集过则 409） |
//!
//! 设计 §5.2 只给这三个接口、§5.3 只给这三个权限码，V0378 菜单种子也只种了
//! 「新增费用 / 删除费用」两个按钮 ⇒ **没有 PUT**（录错就删掉重录，理由见
//! [`crate::business::erp::cost_overhead_dto`] 模块头）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_labor_handler` / `lot_handler` / `cost_object_handler` 同款：axum handler 要
//! `AuthClaims` 才能跑，单测调不到；把「校验 + 落库」收进 `create_overhead` /
//! `delete_overhead` / `page_overheads` 之后，真库用例走**同一条代码路径**（只跳过认证），
//! 操作人用 `Option<Uuid>` 传。
//!
//! # 三层校验的顺序（先便宜后贵，且先报「能改的」）
//!
//! ```text
//! ① 纯格式（dto）：期间 / UUID / 费用项目 / 借方前缀 4101.* / 金额 > 0 / 备注长度
//! ② 引用存在性：车间（快照来源）→ 成本对象（必须是 OPEN）
//! ③ 账套现状：借方 + 对方科目都必须是「存在 + 末级 + 启用」
//! ④ 落库（map_write_error 兜住绕过入口的写入）
//! ```
//!
//! 第 ③ 层为什么不放在 ①：它要查库，而「金额填了 0」这种纯格式错应该**不碰库**就能报出来
//! （前端一边输一边校验时也不会打库）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_object_dto;
use crate::business::erp::cost_overhead_dto::{
    self as dto, CostOverheadCreateReq, CostOverheadFilter, CostOverheadListRes, CostOverheadPageQuery,
};
use crate::business::erp::cost_overhead_model::{self as model, CostOverheadRow};
use crate::business::erp::model_base::normalize_page;
use crate::business::erp::model_master;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与设计 §5.3 / V0378 菜单种子逐字一致） ====================

/// 列表。
pub const PERM_LIST: &str = "business:erp:cost-overhead:list";
/// 新增。
pub const PERM_CREATE: &str = "business:erp:cost-overhead:create";
/// 删除（软删）。
pub const PERM_DELETE: &str = "business:erp:cost-overhead:delete";

/// 费用台账全部权限码（设计 §5.3 的 3 个；**没有 update** —— 见模块头）。
pub const OVERHEAD_PERMISSIONS: [&str; 3] = [PERM_LIST, PERM_CREATE, PERM_DELETE];

// ==================== 引用校验（建单的查库部分） ====================

/// 成本对象必须存在且是 `OPEN`；返回对象号（4xx 文案要用）。
///
/// 两条不同的错：
/// * 不存在 / 已删 → **400 + `66002`**（入参指错了：换对象）；
/// * 存在但非 `OPEN` → **409 + `66057`**（业务规则拒绝：已关批的成本已结转）。
async fn ensure_cost_object_open(cost_object_id: Uuid) -> std::result::Result<String, ApiError> {
    match model::cost_object_ref(cost_object_id)
        .await
        .map_err(|error| utils::db_error(error, "校验费用的成本对象"))?
    {
        None => Err(ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!("cost_object_id 指向的成本对象 {cost_object_id} 不存在或已删除"),
        )),
        Some((object_no, status)) if status != cost_object_dto::STATUS_OPEN => {
            Err(dto::object_not_open(&object_no, &status))
        }
        Some((object_no, _)) => Ok(object_no),
    }
}

/// 车间必须是 `sys_dept` 的现有部门；返回名称（快照来源）。
async fn ensure_dept(dept_id: Uuid) -> std::result::Result<String, ApiError> {
    match model::dept_name_of(dept_id)
        .await
        .map_err(|error| utils::db_error(error, "校验费用的车间"))?
    {
        Some(name) => Ok(name),
        None => Err(dto::bad_request(format!(
            "dept_id（{dept_id}）指向的车间（部门）不存在或已删除"
        ))),
    }
}

/// 科目必须是「存在 + 末级 + 启用」（账套现状校验）。
///
/// 只查前缀会放过三种到结算才炸的输入：账套里没有的编码、汇总科目（非末级）、已停用科目。
/// 三者统一 400 + `66060`（动作都是「换科目」），报文点名原因。
async fn ensure_account(code: &str, field: &str) -> std::result::Result<(), ApiError> {
    let account = model_master::account_find_by_code(code)
        .await
        .map_err(|error| utils::db_error(error, "校验费用的科目"))?
        .ok_or_else(|| dto::account_not_found(code, field))?;
    if account.is_leaf != 1 {
        return Err(dto::account_not_leaf(code, &account.account_name, field));
    }
    if account.status != 1 {
        return Err(dto::account_disabled(code, &account.account_name, field));
    }
    Ok(())
}

// ==================== 出参组装 ====================

/// 列表行。
fn to_list_res(row: &CostOverheadRow) -> CostOverheadListRes {
    CostOverheadListRes {
        id: row.id.to_string(),
        period_code: row.period_code.clone(),
        cost_object_id: row.cost_object_id.map(|id| id.to_string()),
        cost_object_no: row.cost_object_no.clone(),
        cost_object_status: row.cost_object_status.clone(),
        dept_id: row.dept_id.to_string(),
        dept_name: Some(row.display_dept_name()),
        expense_item: row.expense_item.clone(),
        expense_item_label: Some(row.display_expense_item()),
        debit_account_code: row.debit_account_code.clone(),
        debit_account_name: row.debit_account_name.clone(),
        credit_account_code: row.credit_account_code.clone(),
        credit_account_name: row.credit_account_name.clone(),
        // 金额一律字符串出参（§5.1：防前端 JS number 精度丢失）
        amount: row.amount.to_string(),
        source_type: row.source_type.clone(),
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}

/// 按 id 组装出参（建单后的回单走它）。
async fn row_of(id: Uuid) -> std::result::Result<CostOverheadListRes, ApiError> {
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询费用台账行"))?
        .ok_or_else(|| dto::not_found(id))?;
    Ok(to_list_res(&row))
}

/// 取活行（不存在 → 404）。
async fn load(id: Uuid) -> std::result::Result<CostOverheadRow, ApiError> {
    model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询费用台账行"))?
        .ok_or_else(|| dto::not_found(id))
}

// ==================== 业务逻辑（单测 / 真库用例直接调这些） ====================

/// 新增一条费用（§5.2 #18）。
///
/// `cost_object_id` 留空 ⇒ **车间费用池**（`biz_cost_collect` 归集时 `cost_object_id IS NULL`，
/// 等分摊规则摊到具体批次，设计 §4.3 / §4.4.3）；非空 ⇒ 直接计入该批（该批必须 `OPEN`）。
pub async fn create_overhead(
    operator: Option<Uuid>,
    req: CostOverheadCreateReq,
) -> std::result::Result<CostOverheadListRes, ApiError> {
    // ② 引用存在性：先拿车间名（快照来源）与成本对象状态
    let dept_id = crate::business::erp::cost_labor_dto::require_uuid(req.dept_id.as_deref(), "dept_id")?;
    let dept_name = ensure_dept(dept_id).await?;
    if let Some(raw) = req
        .cost_object_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let cost_object_id = crate::business::erp::cost_labor_dto::parse_uuid(raw, "cost_object_id")?;
        // 对象号只在错误文案里用；成功时不需要（列表出参的 object_no 由 join 给）
        let _ = ensure_cost_object_open(cost_object_id).await?;
    }
    // ① 纯格式（不碰库）
    let spec = dto::spec_from_create(&req, &dept_name)?;
    // ③ 账套现状：借方 + 对方科目都必须是启用末级科目
    ensure_account(&spec.debit_account_code, "debit_account_code").await?;
    ensure_account(&spec.credit_account_code, "credit_account_code").await?;
    // ④ 落库
    let id = Uuid::now_v7();
    model::insert(id, &spec, operator)
        .await
        .map_err(|error| dto::map_write_error(error, &spec, "新增费用台账"))?;
    row_of(id).await
}

/// 软删一条费用（§5.2 #19）。
///
/// # 为什么先问「有没有被归集」
///
/// 本表没有状态列（台账录错就删）。但若这一行已经被某期 `calc` 归集进
/// `biz_cost_collect`（`source_type='OVERHEAD_TICKET'`），删掉就等于让**已归集的钱失去来源**：
/// 账上还有这笔制造费用、单据却没了，界面上没有可点的按钮，只能改库恢复
/// （AGENTS 约定 11 明令禁止的形态）。所以先查归集，命中就 409 并告诉用户走
/// 「作废/重算该期间」这条正规路径。
pub async fn delete_overhead(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = crate::business::erp::cost_labor_dto::parse_uuid(id_raw, "id")?;
    let _row = load(id).await?;
    if let Some(period_code) = model::collected_period(id)
        .await
        .map_err(|error| utils::db_error(error, "查询费用的归集情况"))?
    {
        return Err(dto::already_collected(&period_code));
    }
    let affected = model::soft_delete(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除费用台账行"))?;
    if affected == 0 {
        // 查过存在、删却是 0 行 ⇒ 期间被别人并发删掉了
        return Err(dto::not_found(id));
    }
    Ok(())
}

/// 列表分页（本表不参与行级数据权限，故不需要 `principal`）。
pub async fn page_overheads(
    query: CostOverheadPageQuery,
) -> std::result::Result<PageData<Vec<CostOverheadListRes>>, ApiError> {
    let filter = filter_of(&query)?;
    let (page, page_size) = (filter.page, filter.page_size);
    let (rows, total) = model::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询费用台账"))?;
    Ok(PageData {
        list: rows.iter().map(to_list_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 归一化列表筛选（纯逻辑；单测可直接验）。
pub fn filter_of(query: &CostOverheadPageQuery) -> std::result::Result<CostOverheadFilter, ApiError> {
    let period_code = match crate::business::erp::cost_labor_dto::clean_optional(query.period_code.clone()) {
        None => None,
        Some(raw) => Some(crate::business::erp::cost_labor_dto::normalize_period(Some(&raw))?),
    };
    // 费用项目筛选：空 = 不筛；给了就必须是 7 个值之一（拼错了返回空列表最误导人）
    let expense_item = match crate::business::erp::cost_labor_dto::clean_optional(query.expense_item.clone()) {
        None => None,
        Some(raw) => Some(dto::normalize_expense_item(Some(&raw))?),
    };
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(CostOverheadFilter {
        period_code,
        cost_object_id: crate::business::erp::cost_labor_dto::parse_opt_uuid(
            query.cost_object_id.as_deref(),
            "cost_object_id",
        )?,
        dept_id: crate::business::erp::cost_labor_dto::parse_opt_uuid(query.dept_id.as_deref(), "dept_id")?,
        expense_item,
        keyword: crate::business::erp::cost_labor_dto::clean_optional(query.keyword.clone()),
        page,
        page_size,
    })
}

// ==================== HTTP 入口 ====================

/// 费用台账分页。
#[nik::permission(
    path = "/business/erp/cost-overhead",
    method = "GET",
    permission = "business:erp:cost-overhead:list"
)]
#[nik::get("/business/erp/cost-overhead")]
pub async fn cost_overhead_page_api(
    Query(query): Query<CostOverheadPageQuery>,
) -> ResResult<PageData<Vec<CostOverheadListRes>>> {
    Ok(Res::ok(page_overheads(query).await?))
}

/// 新增费用。
#[nik::permission(
    path = "/business/erp/cost-overhead",
    method = "POST",
    permission = "business:erp:cost-overhead:create"
)]
#[nik::post("/business/erp/cost-overhead")]
pub async fn cost_overhead_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CostOverheadCreateReq>,
) -> ResResult<CostOverheadListRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_overhead(operator, req).await?))
}

/// 删除费用（软删）。
#[nik::permission(
    path = "/business/erp/cost-overhead/{id}",
    method = "DELETE",
    permission = "business:erp:cost-overhead:delete"
)]
#[nik::delete("/business/erp/cost-overhead/{id}")]
pub async fn cost_overhead_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_overhead(operator, &id).await?;
    Ok(Res::ok(()))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形，便于汇总脚本收集）。
pub fn all_permissions() -> Vec<&'static str> {
    OVERHEAD_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    // ---------- 权限 / 路由 ----------

    #[test]
    fn permission_codes_match_design_5_3() {
        assert_eq!(PERM_LIST, "business:erp:cost-overhead:list");
        assert_eq!(PERM_CREATE, "business:erp:cost-overhead:create");
        assert_eq!(PERM_DELETE, "business:erp:cost-overhead:delete");
        assert_eq!(all_permissions().len(), 3, "设计 §5.3 给了 3 个码");
        for code in OVERHEAD_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code}");
            assert_eq!(parts[0], "business", "{code}");
            assert_eq!(parts[1], "erp", "{code}");
            assert_eq!(parts[2], "cost-overhead", "{code}");
        }
    }

    /// 没有 update 码：设计 §5.2/§5.3 与 V0378 菜单种子都不给「编辑费用」。
    /// 若将来要加 PUT，必须先有承载它的权限码迁移（否则 `perm_menu_gate` 直接红）。
    #[test]
    fn there_is_no_update_permission_on_purpose() {
        assert_eq!(OVERHEAD_PERMISSIONS.len(), 3);
        assert!(
            !OVERHEAD_PERMISSIONS.iter().any(|code| code.ends_with(":update")),
            "{OVERHEAD_PERMISSIONS:?}"
        );
    }

    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in ["/business/erp/cost-overhead", "/business/erp/cost-overhead/{id}"] {
            assert!(path.starts_with("/business/erp/cost-overhead"), "{path}");
            assert!(!path.starts_with("/api"), "{path} 不能带 /api 前缀（约定 8）");
        }
    }

    // ---------- 列表筛选归一化 ----------

    #[test]
    fn filter_normalizes_period_item_and_paging() {
        let query = CostOverheadPageQuery {
            period_code: Some(" 2026-11 ".to_string()),
            expense_item: Some("utility".to_string()),
            dept_id: Some("018f4b2c-1111-7222-8333-444455556666".to_string()),
            page: None,
            page_size: Some(0),
            ..Default::default()
        };
        let filter = filter_of(&query).expect("筛选");
        assert_eq!(filter.period_code.as_deref(), Some("2026-11"));
        assert_eq!(filter.expense_item.as_deref(), Some("UTILITY"));
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20, "page_size=0 回落到默认 20");
        assert!(filter.dept_id.is_some());

        let query = CostOverheadPageQuery {
            page_size: Some(10_000),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect("筛选").page_size, 200, "上限 200");
    }

    #[test]
    fn filter_rejects_unknown_period_item_and_uuid() {
        for bad in ["2026-13", "26-11", "2026/11"] {
            let query = CostOverheadPageQuery {
                period_code: Some(bad.to_string()),
                ..Default::default()
            };
            let error = filter_of(&query).expect_err(bad);
            assert_eq!(error.status, 400, "{bad}");
            assert!(error.msg.contains("period_code"), "{bad}: {}", error.msg);
        }

        // 费用项目拼错 → 400（返回空列表会让用户以为这个月没录费用）
        let query = CostOverheadPageQuery {
            expense_item: Some("ELECTRICITY".to_string()),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("未知费用项目");
        assert!(error.msg.contains("expense_item"), "{}", error.msg);
        assert!(error.msg.contains("UTILITY"), "要列出合法值：{}", error.msg);

        let query = CostOverheadPageQuery {
            cost_object_id: Some("not-a-uuid".to_string()),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("非法 UUID");
        assert!(error.msg.contains("cost_object_id"), "{}", error.msg);
    }

    // ---------- 出参映射 ----------

    fn sample_row() -> CostOverheadRow {
        let now = nik::prelude::chrono::Utc::now();
        CostOverheadRow {
            id: Uuid::now_v7(),
            period_code: "2026-11".to_string(),
            cost_object_id: None,
            dept_id: Uuid::now_v7(),
            dept_name: "一车间（旧名）".to_string(),
            expense_item: "UTILITY".to_string(),
            debit_account_code: "4101.02".to_string(),
            credit_account_code: "1002".to_string(),
            amount: dec("4000.0000"),
            source_type: "MANUAL".to_string(),
            remark: Some("10 月电费".to_string()),
            created_at: now,
            updated_at: now,
            cost_object_no: None,
            cost_object_status: None,
            dept_name_live: Some("一车间（新名）".to_string()),
            expense_item_label_live: Some("水电费".to_string()),
            debit_account_name: Some("制造费用-水电费".to_string()),
            credit_account_name: Some("银行存款".to_string()),
        }
    }

    #[test]
    fn list_res_carries_names_and_string_amounts() {
        let res = to_list_res(&sample_row());
        assert_eq!(res.expense_item, "UTILITY");
        assert_eq!(res.expense_item_label.as_deref(), Some("水电费"), "字典实时名");
        assert_eq!(res.debit_account_code, "4101.02");
        assert_eq!(res.debit_account_name.as_deref(), Some("制造费用-水电费"));
        assert_eq!(res.credit_account_name.as_deref(), Some("银行存款"));
        assert_eq!(res.dept_name.as_deref(), Some("一车间（新名）"), "出参用实时名");
        // 金额是字符串（§5.1：防 JS number 精度丢失）
        assert_eq!(res.amount, "4000.0000");
        assert!(res.created_at.contains('T'), "{}", res.created_at);
    }

    /// 车间费用池行：`cost_object_id` 为空是**合法且常见**的形态，不许当异常处理。
    #[test]
    fn pool_row_has_null_object_and_keeps_object_fields_empty() {
        let res = to_list_res(&sample_row());
        assert!(res.cost_object_id.is_none());
        assert!(res.cost_object_no.is_none());
        assert!(res.cost_object_status.is_none());

        let direct = to_list_res(&CostOverheadRow {
            cost_object_id: Some(Uuid::now_v7()),
            cost_object_no: Some("CB-20261101-000001".to_string()),
            cost_object_status: Some("OPEN".to_string()),
            ..sample_row()
        });
        assert_eq!(direct.cost_object_no.as_deref(), Some("CB-20261101-000001"));
        assert_eq!(direct.cost_object_status.as_deref(), Some("OPEN"));
    }

    /// 金额只回显库里的定点值，出参不重新算（台账记的是账上金额，不是公式）。
    #[test]
    fn amount_is_echoed_verbatim() {
        let res = to_list_res(&CostOverheadRow {
            amount: dec("1234.5678"),
            ..sample_row()
        });
        assert_eq!(res.amount, "1234.5678", "4 位小数原样回显");
        assert_eq!(
            to_list_res(&CostOverheadRow {
                amount: dec("0.0001"),
                ..sample_row()
            })
            .amount,
            "0.0001",
            "最小正数不许被四舍五入成 0"
        );
    }
}
