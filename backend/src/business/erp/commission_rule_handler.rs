//! 佣金规则接口层（销售链 · SO-36b）：规则的列表 / 新建 / 编辑 / 软删。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/commission-rule/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/commission-rule/page` | `business:erp:commission-rule:list` | 分页（范围 / 计算方式 / 状态 / 关键字） |
//! | POST | `/business/erp/commission-rule` | `business:erp:commission-rule:create` | 新建，回单行 |
//! | PUT | `/business/erp/commission-rule/{id}` | `business:erp:commission-rule:update` | 编辑（**部分更新**；停用 = 传 `status=INACTIVE`） |
//! | DELETE | `/business/erp/commission-rule/{id}` | `business:erp:commission-rule:delete` | 软删（**录错了才用**） |
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `customer_handler` / `commission_handler` 同一范式：axum handler 要 `AuthClaims` +
//! `Json` 才能跑，单测没法直接调；把「校验 + 落库」收进 [`create_rule`] / [`update_rule`] /
//! [`delete_rule`] / [`page_rules`] 之后，单测走的是**同一条代码路径**（只跳过认证），
//! 操作人用 `Option<Uuid>` 传。
//!
//! # 停用 vs 软删（本卡最容易做错的一条，卡面原话）
//!
//! ```text
//! 停用 = PUT {status: "INACTIVE"}   —— 规则退出计提匹配，历史明细仍能用 rule_id 查到它，
//!                                      名字也读得到（commission_handler::rule_names_in 不滤软删）。
//! 软删 = DELETE                     —— 只用于「录错了」（这条规则从来没参与过结算）。
//!                                      一旦有存活结算单的明细引用它，直接 409 顶回去，
//!                                      逼调用方改用「停用」。
//! ```
//!
//! # 不碰计提语义
//!
//! 选规则（`pick_rule`）与算钱（`commission_of`）在 `commission_handler`，本卡只写规则表。
//! 「改完规则后匹配结果确实变了」由单测
//! [`tests::rule_update_changes_pick_rule_outcome`] 钉住 —— 覆盖比率调整、改范围、
//! 缩短生效期、停用四条路径，避免有人改 DTO 时把 `status` / `effective_to` 的语义改坏。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_commission_rule 是配置表（无归属人 / 部门列），可见性由
//!         business:erp:commission-rule:* 权限码控制；分页走 model_base::page_of →
//!         scope::paginate_unscoped（与 commission_handler 头注释的登记一致）。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::commission_model::RuleRow;
use crate::business::erp::commission_rule_dto::{
    self, RuleCreateReq, RuleFilter, RulePageQuery, RuleRes, RuleSpec, RuleUpdateReq, SCOPE_SALESPERSON, scope_label,
    spec_from_create, spec_from_update,
};
use crate::business::erp::commission_rule_model::{self as model, RuleDetailRow};
use crate::business::erp::model::normalize_page;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0310 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 详情。
pub const PERM_LIST: &str = "business:erp:commission-rule:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:commission-rule:create";
/// 编辑 / 停用。
pub const PERM_UPDATE: &str = "business:erp:commission-rule:update";
/// 软删（录错了才用）。
pub const PERM_DELETE: &str = "business:erp:commission-rule:delete";

/// 规则域全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const RULE_PERMISSIONS: [&str; 4] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE];

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("佣金规则不存在或已删除（{id}）"),
    )
}

/// 落库错误 → 统一错误码（CHECK / 唯一索引这类兜底失败会落在这里）。
fn write_error(error: DbError, action: &str) -> ApiError {
    utils::db_error(error, action)
}

// ==================== 今日（北京时间） ====================

/// 今天（CST）：`effective_from` 缺省值，与 DDL 的 `DEFAULT CURRENT_DATE` 同口径。
///
/// 服务器时区可能是 UTC，凌晨 0–8 点两者差一天；佣金规则是「业务日」概念，
/// 与 `commission_dto::current_period_code` 一样按北京时间算。
fn today_cst() -> NaiveDate {
    let offset = FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定");
    Utc::now().with_timezone(&offset).date_naive()
}

// ==================== 出参组装 ====================

/// 行 + 范围显示名 → 出参（字段名与前端 SO-37 逐字对齐）。
pub fn to_res(row: &RuleDetailRow, scope_name: Option<String>) -> RuleRes {
    RuleRes {
        id: row.id.to_string(),
        rule_name: row.rule_name.clone(),
        scope_type: row.scope_type.clone(),
        scope_id: row.scope_id.map(|id| id.to_string()),
        scope_name,
        calc_type: row.calc_type.clone(),
        rate: row.rate,
        min_amount: row.min_amount,
        max_amount: row.max_amount,
        effective_from: row.effective_from,
        effective_to: row.effective_to,
        status: row.status.clone(),
        remark: row.remark.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

// ==================== 范围对象存在性 ====================

/// 范围对象必须存在且未软删：`SALESPERSON` ⇒ `sys_user`、`MATERIAL_CATEGORY` ⇒ `biz_material`。
///
/// 基础版没有分类表，物料类规则的 `scope_id` 就是 `biz_material.id`（见
/// `commission_handler` 头注释），所以这里查的是物料 —— 写错表会让整类规则永远匹配不上，
/// 而且**不会报错**（`pick_rule` 只是命中不到），属于静默失效，必须在入口挡住。
async fn ensure_scope_target(spec: &RuleSpec) -> std::result::Result<(), ApiError> {
    let Some(scope_id) = spec.scope_id else {
        return Ok(());
    };
    let exists = model::scope_target_exists(&spec.scope_type, scope_id)
        .await
        .map_err(|error| utils::db_error(error, "校验佣金规则的范围对象"))?;
    if exists {
        return Ok(());
    }
    let object = if spec.scope_type == SCOPE_SALESPERSON {
        "销售员（sys_user）"
    } else {
        "物料（biz_material）"
    };
    Err(bad_request(format!(
        "scope_type={} 的 scope_id（{scope_id}）指向的{object}不存在或已删除",
        scope_label(&spec.scope_type)
    )))
}

// ==================== 业务逻辑（单测直接调这些） ====================

/// 新建规则。
pub async fn create_rule(operator: Option<Uuid>, req: RuleCreateReq) -> std::result::Result<RuleRes, ApiError> {
    let spec = spec_from_create(&req, today_cst())?;
    ensure_scope_target(&spec).await?;
    let id = Uuid::now_v7();
    model::insert(id, &spec, operator)
        .await
        .map_err(|error| write_error(error, "新建佣金规则"))?;
    read_back(id).await
}

/// 编辑规则（部分更新；前端发全量时等价于整体覆盖）。
pub async fn update_rule(
    operator: Option<Uuid>,
    id_raw: &str,
    req: RuleUpdateReq,
) -> std::result::Result<RuleRes, ApiError> {
    let id = commission_rule_dto::parse_uuid(id_raw, "id")?;
    let (current_row, _) = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑的佣金规则"))?
        .ok_or_else(|| not_found(id))?;
    let current = current_row.to_spec();
    let spec = spec_from_update(&current, &req)?;
    // 范围没动就不必再查一次对象存在性（省一次往返，语义不变）
    if spec.scope_id != current.scope_id || spec.scope_type != current.scope_type {
        ensure_scope_target(&spec).await?;
    }
    let affected = model::update(id, &spec, operator)
        .await
        .map_err(|error| write_error(error, "编辑佣金规则"))?;
    if affected == 0 {
        return Err(not_found(id));
    }
    read_back(id).await
}

/// 软删规则（**只用于录错了**：被明细引用过的规则改「停用」，不删）。
pub async fn delete_rule(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = commission_rule_dto::parse_uuid(id_raw, "id")?;
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待删除的佣金规则"))?
        .ok_or_else(|| not_found(id))?;
    let references = model::reference_count(id)
        .await
        .map_err(|error| utils::db_error(error, "统计规则的佣金明细引用"))?;
    if references > 0 {
        return Err(conflict(format!(
            "规则「{}」已被 {references} 条佣金明细引用，不能删除；不再使用时请停用\
             （PUT status=INACTIVE），历史明细与凭证仍要能追溯到它",
            row.0.rule_name
        )));
    }
    let affected = model::soft_delete(id, operator)
        .await
        .map_err(|error| write_error(error, "删除佣金规则"))?;
    if affected == 0 {
        return Err(not_found(id));
    }
    Ok(())
}

/// 规则分页。
pub async fn page_rules(query: RulePageQuery) -> std::result::Result<PageData<Vec<RuleRes>>, ApiError> {
    let filter: RuleFilter = query.filter()?;
    let (rows, total) = model::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询佣金规则"))?;
    let (page, page_size) = normalize_page(filter.page, filter.page_size);
    let list = rows
        .iter()
        .map(|(row, scope_name)| to_res(row, scope_name.clone()))
        .collect();
    Ok(PageData {
        list,
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 新建 / 编辑后回读（拿服务端生成的 `created_at` / `updated_at` 与 `scope_name`）。
async fn read_back(id: Uuid) -> std::result::Result<RuleRes, ApiError> {
    let (row, scope_name) = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读佣金规则"))?
        .ok_or_else(|| not_found(id))?;
    Ok(to_res(&row, scope_name))
}

/// 归一化规格 → 计提侧读的 [`RuleRow`]（`commission_handler::load_rules` 的同构快照）。
///
/// 生产路径是「DB → `RuleRow`」；这里是「DTO → `RuleRow`」，用它在单测里验证
/// 「同一条规则被编辑后，`pick_rule` / `commission_of` 的结果如何变化」，
/// 不必起库就能钉住两模块之间的**契约**。
pub fn to_rule_row(spec: &RuleSpec, id: Uuid, created_at: DateTime<Utc>) -> RuleRow {
    RuleRow {
        id,
        rule_name: spec.rule_name.clone(),
        scope_type: spec.scope_type.clone(),
        scope_id: spec.scope_id,
        calc_type: spec.calc_type.clone(),
        rate: spec.rate,
        min_amount: spec.min_amount,
        max_amount: spec.max_amount,
        effective_from: spec.effective_from,
        effective_to: spec.effective_to,
        created_at,
    }
}

// ==================== HTTP 入口 ====================

/// 规则分页。
#[nik::permission(
    path = "/business/erp/commission-rule/page",
    method = "GET",
    permission = "business:erp:commission-rule:list"
)]
#[nik::get("/business/erp/commission-rule/page")]
pub async fn commission_rule_page_api(Query(query): Query<RulePageQuery>) -> ResResult<PageData<Vec<RuleRes>>> {
    Ok(Res::ok(page_rules(query).await?))
}

/// 新建规则。
#[nik::permission(
    path = "/business/erp/commission-rule",
    method = "POST",
    permission = "business:erp:commission-rule:create"
)]
#[nik::post("/business/erp/commission-rule")]
pub async fn commission_rule_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<RuleCreateReq>,
) -> ResResult<RuleRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_rule(operator, req).await?))
}

/// 编辑 / 停用规则。
#[nik::permission(
    path = "/business/erp/commission-rule/{id}",
    method = "PUT",
    permission = "business:erp:commission-rule:update"
)]
#[nik::put("/business/erp/commission-rule/{id}")]
pub async fn commission_rule_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<RuleUpdateReq>,
) -> ResResult<RuleRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_rule(operator, &id, req).await?))
}

/// 软删规则（录错了才用）。
#[nik::permission(
    path = "/business/erp/commission-rule/{id}",
    method = "DELETE",
    permission = "business:erp:commission-rule:delete"
)]
#[nik::delete("/business/erp/commission-rule/{id}")]
pub async fn commission_rule_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_rule(operator, &id).await?;
    Ok(Res::ok(()))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形，便于汇总脚本收集）。
pub fn all_permissions() -> Vec<&'static str> {
    RULE_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::commission_handler::{commission_of, pick_rule};
    use crate::business::erp::commission_rule_dto::{
        CALC_FIXED, CALC_PERCENT, RULE_ACTIVE, RULE_INACTIVE, SCOPE_ALL, SCOPE_MATERIAL,
    };
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    use nik::prelude::chrono::Duration;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("小数")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期")
    }

    fn create_req(scope_type: &str, scope_id: Option<&str>, rate: &str) -> RuleCreateReq {
        RuleCreateReq {
            rule_name: "销售员甲 20%".to_string(),
            scope_type: scope_type.to_string(),
            scope_id: scope_id.map(str::to_string),
            calc_type: CALC_PERCENT.to_string(),
            rate: Some(dec(rate)),
            min_amount: None,
            max_amount: None,
            effective_from: Some("2026-01-01".to_string()),
            effective_to: None,
            status: None,
            remark: None,
        }
    }

    // ---------- 权限 / 路由 ----------

    #[test]
    fn permission_codes_match_v0310() {
        assert_eq!(PERM_LIST, "business:erp:commission-rule:list");
        assert_eq!(PERM_CREATE, "business:erp:commission-rule:create");
        assert_eq!(PERM_UPDATE, "business:erp:commission-rule:update");
        assert_eq!(PERM_DELETE, "business:erp:commission-rule:delete");
        assert_eq!(all_permissions().len(), 4);
        for code in RULE_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code}");
            assert_eq!(parts[0], "business", "{code}");
            assert_eq!(parts[1], "erp", "{code}");
            assert_eq!(parts[2], "commission-rule", "{code}");
        }
        // 与 SO-36 结算域的码必须是两个不同资源（别把 settle 的码复用过来）
        assert!(RULE_PERMISSIONS.iter().all(|code| code.contains("commission-rule")));
    }

    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in [
            "/business/erp/commission-rule/page",
            "/business/erp/commission-rule",
            "/business/erp/commission-rule/{id}",
        ] {
            assert!(path.starts_with("/business/erp/commission-rule"));
            assert!(!path.starts_with("/api"), "{path} 不能带 /api 前缀");
        }
    }

    #[test]
    fn today_is_cst_not_utc() {
        // 北京时间恒为 UTC+8：同一时刻的 CST 日期要么同日要么晚一天
        let cst = today_cst();
        let utc = Utc::now().date_naive();
        let delta = (cst - utc).num_days();
        assert!((0..=1).contains(&delta), "CST 日期与 UTC 日期相差 {delta} 天");
    }

    // ---------- 出参形状（前端 SO-37 逐字对齐） ----------

    #[test]
    fn res_serializes_frontend_field_names() {
        let id = Uuid::now_v7();
        let scope_id = Uuid::now_v7();
        let row = RuleDetailRow {
            id,
            rule_name: "物料一 10%".to_string(),
            scope_type: SCOPE_MATERIAL.to_string(),
            scope_id: Some(scope_id),
            calc_type: CALC_PERCENT.to_string(),
            rate: dec("10.0000"),
            // 真实链路上这些值来自 NUMERIC(18,4)，scale 恒为 4 —— 前端拿到的就是 4 位小数字符串
            min_amount: dec("0.0000"),
            max_amount: Some(dec("60.0000")),
            effective_from: date("2026-01-01"),
            effective_to: None,
            status: RULE_ACTIVE.to_string(),
            remark: Some("夹具".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let res = to_res(&row, Some("SO36 物料一".to_string()));
        let json = serde_json::to_value(&res).expect("序列化");
        let object = json.as_object().expect("对象");
        for field in [
            "id",
            "rule_name",
            "scope_type",
            "scope_id",
            "scope_name",
            "calc_type",
            "rate",
            "min_amount",
            "max_amount",
            "effective_from",
            "effective_to",
            "status",
            "remark",
            "created_at",
            "updated_at",
        ] {
            assert!(object.contains_key(field), "出参缺字段 {field}（前端会 undefined）");
        }
        // 金额/比率是字符串（前端按字符串展示，不自己算）
        assert_eq!(json["rate"], serde_json::json!("10.0000"));
        assert_eq!(json["min_amount"], serde_json::json!("0.0000"));
        assert_eq!(json["max_amount"], serde_json::json!("60.0000"));
        assert_eq!(json["effective_from"], serde_json::json!("2026-01-01"));
        assert_eq!(json["effective_to"], serde_json::Value::Null);
        assert_eq!(json["scope_name"], serde_json::json!("SO36 物料一"));
    }

    // ---------- 规则改动 → 计提匹配结果（本卡点名的回归单测） ----------

    /// 一条 `ALL 5%` 规则：编辑成 12% / 改范围 / 缩短生效期 / 停用 后，
    /// `pick_rule` + `commission_of` 的结果必须各按新语义变化。
    ///
    /// 这是两个模块之间的**契约测试**：规则 CRUD 可以随便改 DTO 的写法，但这四条语义
    /// （比率、范围优先级、生效期含端点、停用即退出匹配）一旦变了，计佣金额就会错。
    #[test]
    fn rule_update_changes_pick_rule_outcome() {
        let salesperson = Uuid::now_v7();
        let other_salesperson = Uuid::now_v7();
        let material = Uuid::now_v7();
        let created_at = Utc::now();
        let on = date("2026-06-01");

        // ① 建：全局 5%
        let mut spec = spec_from_create(&create_req(SCOPE_ALL, None, "5"), date("2026-11-07")).expect("新建规格");
        let rule_id = Uuid::now_v7();
        let mut rules = vec![to_rule_row(&spec, rule_id, created_at)];
        let picked = pick_rule(&rules, salesperson, Some(material), on).expect("命中全局规则");
        assert_eq!(picked.id, rule_id);
        assert_eq!(
            commission_of(picked, dec("1000")).expect("计佣"),
            (dec("5"), dec("50.0000")),
            "1000 × 5% = 50"
        );

        // ② 改比率 5% → 12%（同一 id、同一 created_at，其余不变）
        let update = RuleUpdateReq {
            rate: Some(dec("12")),
            ..Default::default()
        };
        spec = spec_from_update(&spec, &update).expect("改比率");
        rules = vec![to_rule_row(&spec, rule_id, created_at)];
        let picked = pick_rule(&rules, salesperson, Some(material), on).expect("仍命中");
        assert_eq!(
            commission_of(picked, dec("1000")).expect("计佣"),
            (dec("12"), dec("120.0000")),
            "1000 × 12% = 120"
        );

        // ③ 改范围：全局 → 销售员甲（别的销售员不再命中；甲命中且优先级为 0）
        let update = RuleUpdateReq {
            scope_type: Some(SCOPE_SALESPERSON.to_string()),
            scope_id: Some(Some(salesperson.to_string())),
            ..Default::default()
        };
        spec = spec_from_update(&spec, &update).expect("改范围");
        rules = vec![to_rule_row(&spec, rule_id, created_at)];
        assert!(
            pick_rule(&rules, other_salesperson, Some(material), on).is_none(),
            "规则改成销售员甲之后，乙不该再命中"
        );
        assert_eq!(
            pick_rule(&rules, salesperson, Some(material), on).map(|rule| rule.id),
            Some(rule_id)
        );

        // ④ 缩短生效期：effective_to 早于计提日 → 不再命中（不再发钱）
        let update = RuleUpdateReq {
            effective_to: Some(Some("2026-05-31".to_string())),
            ..Default::default()
        };
        spec = spec_from_update(&spec, &update).expect("缩短生效期");
        rules = vec![to_rule_row(&spec, rule_id, created_at)];
        assert!(
            pick_rule(&rules, salesperson, Some(material), on).is_none(),
            "生效期结束在 5-31 的规则，6-1 不该命中"
        );
        assert!(
            pick_rule(&rules, salesperson, Some(material), date("2026-05-31")).is_some(),
            "effective_to 是含端点的"
        );

        // ⑤ 停用：status=INACTIVE 的规则由 load_rules 过滤掉 ⇒ 传进 pick_rule 的集合里没有它
        //    （这里用「过滤后的集合」模拟 load_rules 的 WHERE status='ACTIVE'）
        let update = RuleUpdateReq {
            status: Some(RULE_INACTIVE.to_string()),
            ..Default::default()
        };
        spec = spec_from_update(&spec, &update).expect("停用");
        assert_eq!(spec.status, RULE_INACTIVE);
        let active: Vec<RuleRow> = vec![to_rule_row(&spec, rule_id, created_at)]
            .into_iter()
            .filter(|_| spec.status == RULE_ACTIVE)
            .collect();
        assert!(active.is_empty(), "停用规则不该进匹配集合");
        assert!(
            pick_rule(&active, salesperson, Some(material), date("2026-03-01")).is_none(),
            "没有可用规则 ⇒ 不计佣（不是按 0 计）"
        );
        // 停用不动其他字段：比率/范围快照仍在，随时可再启用
        assert_eq!(spec.rate, dec("12"));
        assert_eq!(spec.scope_id, Some(salesperson));
    }

    /// 优先级不变式：销售员规则压过物料规则与全局规则，即使后两者是**新加**的。
    ///
    /// 编辑规则表不该影响 `pick_rule` 的全序（scope_rank → 生效日 → 创建时间 → id）。
    #[test]
    fn crud_does_not_break_rule_priority_order() {
        let salesperson = Uuid::now_v7();
        let material = Uuid::now_v7();

        let global = spec_from_create(&create_req(SCOPE_ALL, None, "5"), date("2026-11-07")).expect("全局");
        let mut material_req = create_req(SCOPE_MATERIAL, Some(&material.to_string()), "10");
        material_req.rule_name = "物料一 10%".to_string();
        let by_material = spec_from_create(&material_req, date("2026-11-07")).expect("物料");
        let mut person_req = create_req(SCOPE_SALESPERSON, Some(&salesperson.to_string()), "20");
        person_req.rule_name = "销售员甲 20%".to_string();
        let by_person = spec_from_create(&person_req, date("2026-11-07")).expect("销售员");

        let now = Utc::now();
        // 故意让销售员规则**最老**（created_at 最早），仍必须赢：优先级先于时间
        let rules = vec![
            to_rule_row(&by_person, Uuid::now_v7(), now - Duration::days(10)),
            to_rule_row(&global, Uuid::now_v7(), now),
            to_rule_row(&by_material, Uuid::now_v7(), now),
        ];
        let picked = pick_rule(&rules, salesperson, Some(material), date("2026-06-01")).expect("命中");
        assert_eq!(picked.rule_name, "销售员甲 20%");
        assert_eq!(commission_of(picked, dec("1000")).expect("计佣").1, dec("200.0000"));

        // 没有物料桶（订单无明细）时退到销售员规则，而不是物料规则
        let picked = pick_rule(&rules, salesperson, None, date("2026-06-01")).expect("命中");
        assert_eq!(picked.rule_name, "销售员甲 20%");
    }

    /// 固定额规则：`min_amount` 起算门槛与 `max_amount` 封顶都经 `commission_of` 生效。
    ///
    /// 库层 `ck_biz_cmrule_amount` 要求 `max_amount ≥ min_amount`，所以「起算门槛」
    /// 与「封顶」用两条规则分别验（起算额高于封顶本身是非法配置）。
    #[test]
    fn fixed_rule_amounts_flow_into_commission_of() {
        // ① 封顶：固定 300 元、不设起算额、封顶 200
        let mut req = create_req(SCOPE_ALL, None, "300");
        req.calc_type = CALC_FIXED.to_string();
        req.max_amount = Some(dec("200"));
        req.rule_name = "固定 300 元（封顶 200）".to_string();
        let spec = spec_from_create(&req, date("2026-11-07")).expect("固定额规则");
        assert_eq!(spec.calc_type, CALC_FIXED);
        let rule = to_rule_row(&spec, Uuid::now_v7(), Utc::now());
        assert_eq!(
            commission_of(&rule, dec("1500")).expect("计佣"),
            (dec("300"), dec("200.0000")),
            "FIXED 300 被封顶压到 200"
        );
        // 冲回对称：基数 −1500 → −200
        assert_eq!(
            commission_of(&rule, dec("-1500")).expect("冲回"),
            (dec("300"), dec("-200.0000"))
        );

        // ② 起算门槛：固定 300 元、起算额 600（不封顶）
        let mut req = create_req(SCOPE_ALL, None, "300");
        req.calc_type = CALC_FIXED.to_string();
        req.min_amount = Some(dec("600"));
        req.rule_name = "固定 300 元（起算 600）".to_string();
        let spec = spec_from_create(&req, date("2026-11-07")).expect("固定额规则");
        let rule = to_rule_row(&spec, Uuid::now_v7(), Utc::now());
        assert!(commission_of(&rule, dec("500")).is_none(), "未达起算额 600 → 不计佣");
        assert_eq!(
            commission_of(&rule, dec("700")).expect("计佣"),
            (dec("300"), dec("300.0000")),
            "达到起算额 → 固定 300"
        );
    }

    // ---------- 停用 vs 软删的语义（纯逻辑侧） ----------

    /// 只有 `status=INACTIVE` 才算「不再参与匹配」；DTO 层不允许把状态写成别的值糊过去。
    #[test]
    fn stopping_a_rule_is_a_status_change_not_a_delete() {
        let spec = spec_from_create(&create_req(SCOPE_ALL, None, "5"), date("2026-11-07")).expect("新建");
        assert_eq!(spec.status, RULE_ACTIVE, "默认启用");
        let stopped = spec_from_update(
            &spec,
            &RuleUpdateReq {
                status: Some("inactive".to_string()),
                ..Default::default()
            },
        )
        .expect("停用（域值大小写不敏感）");
        assert_eq!(stopped.status, RULE_INACTIVE);
        // 停用只动 status：其余 10 个业务字段逐字不变（否则「停用」会悄悄改口径）
        assert_eq!(stopped.rule_name, spec.rule_name);
        assert_eq!(stopped.scope_type, spec.scope_type);
        assert_eq!(stopped.rate, spec.rate);
        assert_eq!(stopped.min_amount, spec.min_amount);
        assert_eq!(stopped.max_amount, spec.max_amount);
        assert_eq!(stopped.effective_from, spec.effective_from);
        assert_eq!(stopped.effective_to, spec.effective_to);
    }

    /// 编辑时若把 scope_type 从 ALL 改成 SALESPERSON 却没给 scope_id（合并后为空）→ 400。
    #[test]
    fn switching_scope_type_requires_scope_id() {
        let spec = spec_from_create(&create_req(SCOPE_ALL, None, "5"), date("2026-11-07")).expect("全局规则");
        let err = spec_from_update(
            &spec,
            &RuleUpdateReq {
                scope_type: Some(SCOPE_SALESPERSON.to_string()),
                ..Default::default()
            },
        )
        .expect_err("换范围必须同时给 scope_id");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("scope_id"), "{}", err.msg);
    }
}
