//! 取价解析契约（销售链 · SO-33 / ARCH-7 §15.3.3）：入参校验 + 出参形状。
//!
//! 接口：`GET /api/business/erp/price/resolve?customer_id=&material_id=&qty=&date=`
//!
//! | 参数 | 必填 | 口径 |
//! |------|------|------|
//! | `material_id` | ✅ | 物料 UUID；非法 / 缺省 → **400**（`10001`） |
//! | `qty` | ✅ | 订购量（`Decimal`，≥ 0）。**必填**：缺省时若按 0 处理会静默取到「无起订量」这一档的价，等于给销售一个偏低的价 —— 价格错比 400 更贵 |
//! | `customer_id` | ❌ | 客户 UUID；不传 / 空串 = **跳过 ①② 两级**，只走 ③ 通用阶梯 → ④ 标准价 → ⑤ 无价。非法 → 400 |
//! | `date` | ❌ | 取价基准日 `YYYY-MM-DD`；缺省 = **今天（北京时间）**。非法 → 400 |
//!
//! 出参（**只有这三个字段**，字段名与 ARCH-7 §15.3.3 逐字对齐，前端直接读）：
//!
//! ```json
//! { "price": "12.5000", "source": "AGREED", "source_id": "…uuid…" }
//! { "price": null,      "source": "NONE",   "source_id": null }
//! ```
//!
//! * `price`：`Decimal`（**JSON 里是字符串**，与全仓金额口径一致），`18,4`；
//! * `source`：`AGREED` / `TIER_CUSTOMER` / `TIER_COMMON` / `STANDARD` / `NONE` 五值之一
//!   （[`SOURCES`] 与 ARCH-7 的枚举逐字对应，单测钉住）；
//! * `source_id`：命中行的主键（协议价 / 档位价 = 那一行的 `id`；`STANDARD` = **物料 id**
//!   本身，前端据此能定位到哪个物料给的价；`NONE` = `null`）。
//!
//! # 校验分工
//!
//! * 只依赖请求本身的规则（必填 / UUID 格式 / 数量非负 / 日期格式）在本文件，
//!   纯函数、无 DB、可单测（[`PriceResolveQuery::validate`]）；
//! * 依赖库的规则（物料 / 客户是否存在）在 [`crate::business::erp::price_handler`]；
//! * 取价优先级与并列打破在 [`crate::business::erp::price_model`]（纯函数，同样是可单测的）。
//!
//! # 为什么 `qty` 不做「缺省 0」
//!
//! `min_qty` 语义是「订购量 ≥ 本值才命中」（0 = 不限制）。把缺省的 `qty` 当 0，
//! 恰好会命中 `min_qty = 0` 那一行 —— 往往是**最低起订档的价**。销售员忘了传量，
//! 系统回一个偏低的价、还不报错，这是最难查的一类事故。故 `qty` 必填，
//! 真要「不限量取底价」就显式写 `qty=0`。

use std::str::FromStr;

use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== source 域值（与 ARCH-7 §15.3.3 逐字对应） ====================

/// ① 客户协议价（`biz_customer_price`）。
pub const SOURCE_AGREED: &str = "AGREED";
/// ② 客户专属阶梯价（`biz_price_tier` 且 `customer_id IS NOT NULL`）。
pub const SOURCE_TIER_CUSTOMER: &str = "TIER_CUSTOMER";
/// ③ 通用阶梯价（`biz_price_tier` 且 `customer_id IS NULL`）。
pub const SOURCE_TIER_COMMON: &str = "TIER_COMMON";
/// ④ 物料标准售价（`biz_material.standard_price`）。
pub const SOURCE_STANDARD: &str = "STANDARD";
/// ⑤ 五级全未命中 → 让销售员手填。
pub const SOURCE_NONE: &str = "NONE";

/// `source` 白名单（顺序 = 优先级从高到低，单测钉住）。
pub const SOURCES: [&str; 5] = [
    SOURCE_AGREED,
    SOURCE_TIER_CUSTOMER,
    SOURCE_TIER_COMMON,
    SOURCE_STANDARD,
    SOURCE_NONE,
];

/// 是否为合法 `source` 域值。
pub fn is_valid_source(value: &str) -> bool {
    SOURCES.contains(&value)
}

// ==================== 错误出口 ====================

/// `400 + 10001`（入参不合法）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 入参 ====================

/// 原始查询串（全部按字符串收，校验在 [`PriceResolveQuery::validate`]）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PriceResolveQuery {
    /// 客户 id（选填：不传 = 跳过协议价与客户专属阶梯）。
    pub customer_id: Option<String>,
    /// 物料 id（必填）。
    pub material_id: Option<String>,
    /// 订购量（必填，≥ 0）。
    pub qty: Option<String>,
    /// 取价基准日 `YYYY-MM-DD`（选填，缺省 = 今天北京时间）。
    pub date: Option<String>,
}

/// 校验并解析后的入参（model 层不再碰字符串）。
#[derive(Debug, Clone, PartialEq)]
pub struct PriceResolveReq {
    /// 客户 id；`None` = 只走通用阶梯及以下。
    pub customer_id: Option<Uuid>,
    /// 物料 id。
    pub material_id: Uuid,
    /// 订购量（≥ 0）。
    pub qty: Decimal,
    /// 取价基准日。
    pub date: NaiveDate,
}

impl PriceResolveQuery {
    /// 校验并解析；任一不合法 → `400 + 10001`（点名哪个参数、错在哪）。
    pub fn validate(&self) -> std::result::Result<PriceResolveReq, ApiError> {
        let material_id = match non_empty(self.material_id.as_deref()) {
            None => return Err(bad_request("material_id 必填")),
            Some(text) => parse_uuid(&text, "material_id")?,
        };
        let customer_id = match non_empty(self.customer_id.as_deref()) {
            None => None,
            Some(text) => Some(parse_uuid(&text, "customer_id")?),
        };
        let qty = match non_empty(self.qty.as_deref()) {
            None => return Err(bad_request("qty 必填（不限量取底价请显式传 qty=0）")),
            Some(text) => parse_qty(&text)?,
        };
        let date = match non_empty(self.date.as_deref()) {
            None => crate::business::erp::voucher::today_cst(Utc::now()),
            Some(text) => NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                .map_err(|err| bad_request(format!("date 不是合法日期 `{text}`（YYYY-MM-DD）：{err}")))?,
        };
        Ok(PriceResolveReq {
            customer_id,
            material_id,
            qty,
            date,
        })
    }
}

/// 去空白后为空 = 没传（`?qty=` 这种空串等同缺省，判定见调用点）。
fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.to_string())
}

/// UUID 文本 → `Uuid`；非法 → 400（绝不静默当作「没传」）。
fn parse_uuid(text: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(text).map_err(|err| bad_request(format!("{field} 不是合法 UUID `{text}`：{err}")))
}

/// 数量：定点数，**负数一律拒**（负的订购量在取价里没有意义，且会让 `min_qty` 过滤变成全放行）。
fn parse_qty(text: &str) -> std::result::Result<Decimal, ApiError> {
    let qty = Decimal::from_str(text).map_err(|err| bad_request(format!("qty 不是合法数字 `{text}`：{err}")))?;
    if qty < Decimal::ZERO {
        return Err(bad_request(format!("qty 不能为负 `{text}`")));
    }
    Ok(qty)
}

// ==================== 出参 ====================

/// 取价结果。
///
/// `price` / `source_id` 都是 `Option`：五级全未命中时给 `null`（**不是** 0 元，
/// 也不是 404 —— 「没有价」是合法业务结果，前端据此让销售员手填）。
#[derive(Debug, Clone, Serialize)]
pub struct PriceResolveRes {
    /// 含税单价（`18,4`，JSON 里是字符串）；`None` = 未取到价。
    pub price: Option<Decimal>,
    /// 命中层级（[`SOURCES`] 之一）。
    pub source: String,
    /// 命中行主键；`STANDARD` 时是物料 id；`NONE` 时 `None`。
    pub source_id: Option<String>,
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 合法物料 id（固定串，避免随机 id 让断言不可读）。
    const MAT: &str = "5a000000-0000-4000-8000-000000000001";
    /// 合法客户 id。
    const CUS: &str = "5a000000-0000-4000-8000-000000000002";

    fn query(
        customer: Option<&str>,
        material: Option<&str>,
        qty: Option<&str>,
        date: Option<&str>,
    ) -> PriceResolveQuery {
        PriceResolveQuery {
            customer_id: customer.map(str::to_string),
            material_id: material.map(str::to_string),
            qty: qty.map(str::to_string),
            date: date.map(str::to_string),
        }
    }

    /// source 五值必须与 ARCH-7 §15.3.3 逐字一致（差一个字前端就静默不认）。
    #[test]
    fn source_domain_matches_arch7() {
        assert_eq!(SOURCES, ["AGREED", "TIER_CUSTOMER", "TIER_COMMON", "STANDARD", "NONE"]);
        for code in SOURCES {
            assert!(is_valid_source(code), "{code} 应在白名单内");
        }
        assert!(!is_valid_source("AGREED_PRICE"));
        assert!(!is_valid_source(""));
    }

    /// 全参数合法 → 原样解析。
    #[test]
    fn validate_full_query() {
        let req = query(Some(CUS), Some(MAT), Some("12.5"), Some("2026-03-01"))
            .validate()
            .expect("全参应通过");
        assert_eq!(req.material_id, Uuid::parse_str(MAT).unwrap());
        assert_eq!(req.customer_id, Some(Uuid::parse_str(CUS).unwrap()));
        assert_eq!(req.qty, Decimal::from_str("12.5").unwrap());
        assert_eq!(req.date, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());
    }

    /// 客户可选：不传 / 空串 → `None`（只走 ③④⑤）。
    #[test]
    fn customer_is_optional() {
        for raw in [None, Some(""), Some("   ")] {
            let req = query(raw, Some(MAT), Some("1"), None)
                .validate()
                .expect("客户缺省应通过");
            assert_eq!(req.customer_id, None, "raw={raw:?}");
        }
    }

    /// 物料必填：缺省 / 空串 → 400 且点名 material_id。
    #[test]
    fn material_is_required() {
        for raw in [None, Some(""), Some("  ")] {
            let error = query(None, raw, Some("1"), None).validate().unwrap_err();
            assert_eq!(error.status, 400);
            assert!(error.msg.contains("material_id"), "{}", error.msg);
        }
    }

    /// 数量必填且非负：缺省 / 空串 / 负数 / 非数字 → 400 点名 qty。
    #[test]
    fn qty_is_required_and_non_negative() {
        for raw in [None, Some(""), Some("-1"), Some("-0.0001"), Some("abc")] {
            let error = query(Some(CUS), Some(MAT), raw, None).validate().unwrap_err();
            assert_eq!(error.status, 400, "raw={raw:?}");
            assert!(error.msg.contains("qty"), "{}", error.msg);
        }
        // 0 是合法的「不限量」口径。
        let zero = query(Some(CUS), Some(MAT), Some("0"), None)
            .validate()
            .expect("qty=0 应通过");
        assert_eq!(zero.qty, Decimal::ZERO);
    }

    /// 日期缺省 = 今天（北京时间）；非法格式 → 400 点名 date。
    #[test]
    fn date_defaults_to_today_cst() {
        let req = query(Some(CUS), Some(MAT), Some("1"), None)
            .validate()
            .expect("日期可缺省");
        assert_eq!(req.date, crate::business::erp::voucher::today_cst(Utc::now()));

        for raw in ["2026-13-01", "2026/03/01", "20260301", "not-a-date"] {
            let error = query(Some(CUS), Some(MAT), Some("1"), Some(raw))
                .validate()
                .unwrap_err();
            assert_eq!(error.status, 400, "raw={raw}");
            assert!(error.msg.contains("date"), "{}", error.msg);
        }
    }

    /// 非法 UUID → 400 且点名字段（绝不静默当「没传」）。
    #[test]
    fn bad_uuid_is_rejected() {
        let error = query(None, Some("not-a-uuid"), Some("1"), None).validate().unwrap_err();
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("material_id"), "{}", error.msg);

        let error = query(Some("xyz"), Some(MAT), Some("1"), None).validate().unwrap_err();
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("customer_id"), "{}", error.msg);
    }

    /// 出参形状：三个字段、名字与 ARCH-7 一致，未命中时 price / source_id 都是 null。
    #[test]
    fn response_shape_is_three_fields() {
        let hit = PriceResolveRes {
            price: Some(Decimal::from_str("12.5000").unwrap()),
            source: SOURCE_AGREED.to_string(),
            source_id: Some(MAT.to_string()),
        };
        let json = serde_json::to_value(&hit).expect("序列化");
        let object = json.as_object().expect("应是对象");
        assert_eq!(object.len(), 3, "出参只能有三个字段：{json}");
        assert_eq!(json["source"], "AGREED");
        assert_eq!(json["price"], "12.5000");

        let miss = PriceResolveRes {
            price: None,
            source: SOURCE_NONE.to_string(),
            source_id: None,
        };
        let json = serde_json::to_value(&miss).expect("序列化");
        assert!(json["price"].is_null());
        assert!(json["source_id"].is_null());
        assert_eq!(json["source"], "NONE");
    }
}
