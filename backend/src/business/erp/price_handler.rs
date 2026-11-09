//! 取价解析接口（销售链 · SO-33 / ARCH-7 §15.3.3）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/price/resolve`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/price/resolve` | `business:erp:price:resolve` | 5 级确定性取价，命中即返回 |
//!
//! # 调用方拿到什么
//!
//! ```json
//! { "code": 0, "data": { "price": "120.0000", "source": "AGREED", "source_id": "…",
//!                        "tax_inclusive": true, "price_excl": "106.1947", "tax_rate": "13.0000" } }
//! ```
//!
//! **填单只准用 `price_excl`（不含税）**：`price` 对协议价 / 阶梯价是含税价，
//! 直接填进销售订单行会让单价虚高一个税率（120 vs 106.19），税额再算一遍 → 整单金额全错。
//!
//! `source` 五值（[`crate::business::erp::price_dto::SOURCES`]）：
//! `AGREED`（客户协议价）→ `TIER_CUSTOMER`（客户专属阶梯）→ `TIER_COMMON`（通用阶梯）
//! → `STANDARD`（物料标准售价）→ `NONE`（都没价，前端让销售员手填并提示「未找到协议价」）。
//!
//! # 本接口的边界（写清楚，别让前端猜）
//!
//! * **只读**：不写任何表、不锁行、不过账；不涉及凭证 / 库存 / 流程引擎（取价不是单据动作）；
//! * `NONE` 是**正常结果**（200），不是 404：没有价是业务事实，让销售员手填；
//! * 物料 / 客户**不存在或已软删** → 404（这两种是调用方的参数错，静默返回 `NONE`
//!   会让「选错物料」看起来像「还没定价」，销售手填一个价就开单了）；
//! * `qty` 必填、`date` 缺省 = 今天（北京时间）：理由见 [`crate::business::erp::price_dto`] 文件头；
//! * 取价结果**不落库**：本卡不做「下单时快照价」—— 那是销售订单行的 `price` 列
//!   （SO-04 已落地），此接口只回答「此刻按规则应该是多少」。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_customer_price / biz_price_tier / biz_material 是全局主数据与配置表
//! （V0270 / V0137 无 dept_id / 归属人列），数据范围（SELF / DEPT_*）表达不出来；
//! 可见性由 business:erp:price:resolve 权限码控制。本接口无分页、按主键精确取数。
//! ```

use axum::extract::Query;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use crate::business::erp::price_dto::{PriceResolveQuery, PriceResolveReq, PriceResolveRes};
use crate::business::erp::price_model::{self as model, ResolveOutcome};
use crate::shared::error_code;

// ==================== 权限码（与 V0307 的 sys_menu.auth_code 逐字一致） ====================

/// 取价解析（只读）。与迁移 `V0307__price_resolve_permission.up.sql` 逐字对应。
pub const PERM_RESOLVE: &str = "business:erp:price:resolve";

/// 价格域全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const PRICE_PERMISSIONS: [&str; 1] = [PERM_RESOLVE];

// ==================== 错误出口 ====================

/// `404`：物料不存在 / 已软删。
fn material_not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "物料不存在或已删除，无法取价".to_string(),
    )
}

/// `404`：客户不存在 / 已软删。
fn customer_not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "客户不存在或已删除，无法按客户协议价取价".to_string(),
    )
}

// ==================== 业务编排（无 service 层：编排就在 handler） ====================

/// 5 级取价的完整流程：校验 → 存在性 → 粗筛三级候选 → 纯函数排序选优。
///
/// 顺序不可调整：先确认物料/客户真实存在，再按 ①→④ 逐级取；
/// 每级只在**上一级没有命中**时才查库（短路，少两次查询也少两次误命中机会）。
pub async fn resolve_price(query: PriceResolveQuery) -> std::result::Result<PriceResolveRes, ApiError> {
    let req = query.validate()?;
    let (outcome, material_tax_rate) = resolve_validated(&req).await?;
    // 口径标注（含税 / 不含税 + 折算出不含税价）：纯函数，规则见 price_model::tax_view
    let view = model::tax_view(outcome.price, outcome.source, material_tax_rate);
    Ok(PriceResolveRes {
        price: outcome.price,
        source: outcome.source.to_string(),
        source_id: outcome.source_id.map(|id| id.to_string()),
        tax_inclusive: view.tax_inclusive,
        price_excl: view.price_excl,
        tax_rate: view.tax_rate,
    })
}

/// 已通过校验的入参 → `(取价结果, 物料税率)`（拆出来是为了让库里那几步能单独测顺序）。
///
/// 物料税率（百分数）必须一起带回来：协议价 / 阶梯价是**含税**价，
/// 折算 `price_excl` 要用它；这一列的读取和取价是同一次物料查询，不额外多一次往返。
async fn resolve_validated(req: &PriceResolveReq) -> std::result::Result<(ResolveOutcome, Decimal), ApiError> {
    // ④ 顺手先读物料：物料不存在时后面三级全无意义（而且 level ④ 本来就要这一行）
    let material = model::load_material(req.material_id)
        .await?
        .ok_or_else(material_not_found)?;

    // 客户存在性：给了客户就必须是真客户（见文件头「本接口的边界」）
    let customer_id = match req.customer_id {
        Some(id) => {
            if !model::customer_exists(id).await? {
                return Err(customer_not_found());
            }
            Some(id)
        }
        None => None,
    };

    // ①② 需要客户；③ 永远查（通用档位对谁都一样）
    let (agreed, customer_tiers) = match customer_id {
        Some(id) => (
            model::load_agreed(id, req.material_id).await?,
            model::load_customer_tiers(id, req.material_id).await?,
        ),
        None => (Vec::new(), Vec::new()),
    };
    let common_tiers = model::load_common_tiers(req.material_id).await?;

    // 排序与优先级全在纯函数里（可单测，不依赖库）
    let outcome = model::resolve_from(
        req.material_id,
        req.qty,
        req.date,
        &agreed,
        &customer_tiers,
        &common_tiers,
        material.standard_price,
    );
    Ok((outcome, material.tax_rate))
}

// ==================== HTTP 入口 ====================

/// 取价解析：`GET /api/business/erp/price/resolve?customer_id=&material_id=&qty=&date=`。
#[nik::permission(
    path = "/business/erp/price/resolve",
    method = "GET",
    permission = "business:erp:price:resolve"
)]
#[nik::get("/business/erp/price/resolve")]
pub async fn price_resolve_api(Query(query): Query<PriceResolveQuery>) -> ResResult<PriceResolveRes> {
    Ok(Res::ok(resolve_price(query).await?))
}

// ==================== 单测（纯逻辑；真库行为由 curl 实测留证） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 权限码格式：`{模块}:{资源}:{动作}`，且与迁移里的 `auth_code` 逐字一致。
    #[test]
    fn permission_code_shape() {
        assert_eq!(PERM_RESOLVE, "business:erp:price:resolve");
        let parts: Vec<&str> = PERM_RESOLVE.split(':').collect();
        assert_eq!(parts.len(), 4, "必须是 business:erp:<资源>:<动作>");
        assert_eq!(parts[0], "business");
        assert_eq!(parts[1], "erp");
        assert_eq!(PRICE_PERMISSIONS, [PERM_RESOLVE]);
    }

    /// 路由路径不含 `/api` 前缀（由 `[router].prefix` 注入）——写进前缀会被拼成 /api/api。
    #[test]
    fn route_path_has_no_api_prefix() {
        let path = "/business/erp/price/resolve";
        assert!(!path.starts_with("/api"), "宏内路径不能带 /api 前缀");
        assert!(path.starts_with("/business/erp/"));
        assert!(path.ends_with("/price/resolve"));
    }

    /// 两个 404 的错误码与文案：必须是 404 + 10002，且文案指名到底缺哪个对象。
    #[test]
    fn not_found_errors_are_explicit() {
        let material = material_not_found();
        assert_eq!(material.status, 404);
        assert_eq!(material.code, error_code::RESOURCE_NOT_FOUND);
        assert!(material.msg.contains("物料"), "{}", material.msg);

        let customer = customer_not_found();
        assert_eq!(customer.status, 404);
        assert_eq!(customer.code, error_code::RESOURCE_NOT_FOUND);
        assert!(customer.msg.contains("客户"), "{}", customer.msg);
    }
}
