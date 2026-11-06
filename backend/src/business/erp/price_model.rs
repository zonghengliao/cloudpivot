//! 取价解析数据层（销售链 · SO-33 / ARCH-7 §15.3.3）：5 级取价的**唯一真相**。
//!
//! 表结构唯一依据：`backend/migrations/business/V0270__sales_price.up.sql`
//! （`biz_customer_price` / `biz_price_tier` / `biz_material.standard_price`）。
//!
//! # 优先级（严格按序，命中即停）
//!
//! | 级 | 来源 | `source` | 条件 |
//! |----|------|----------|------|
//! | ① | `biz_customer_price` | `AGREED` | 客户 + 物料匹配、`min_qty <= qty`、有效期覆盖 `date` |
//! | ② | `biz_price_tier` 且 `customer_id IS NOT NULL` | `TIER_CUSTOMER` | 物料匹配、`min_qty <= qty < max_qty`（`max_qty` 空 = 无上限）、有效期覆盖 |
//! | ③ | `biz_price_tier` 且 `customer_id IS NULL` | `TIER_COMMON` | 同 ② |
//! | ④ | `biz_material.standard_price` | `STANDARD` | 非 NULL |
//! | ⑤ | — | `NONE` | 以上全未命中 → `price = null`，前端让销售员手填 |
//!
//! # 确定性：两个"多条命中"必须各有确定答案
//!
//! 卡面只写了「多条命中 → 取 `min_qty` 最大的」，但**同一 `min_qty` 也可能有多条**：
//! `uk_biz_cust_price` 是 `(customer_id, material_id, min_qty, effective_from)`，
//! 同起订量、不同生效日的两条协议价可以同时覆盖同一天（旧价没失效、新价已生效）。
//! 若不定义二级次序，`LIMIT 1` 的返回就取决于物理行序（VACUUM / 更新后可能变），
//! 同一张单今天算出 10 元、明天算出 12 元。故本文件用**全序比较键**，没有并列：
//!
//! ```text
//! 协议价： min_qty DESC → effective_from DESC（新签的覆盖旧的）→ id ASC（最后兜底）
//! 档位价： PROMO 先于 TIER（促销优先于量价阶梯）
//!          → min_qty DESC（更贴合的高档位优先）→ effective_from DESC → id ASC
//! ```
//!
//! `id ASC` 是纯兜底：两条同 `min_qty` 同 `effective_from` 的协议价被唯一索引挡死，
//! 但 `biz_price_tier` 同一 `min_qty` 的 `TIER` 与 `PROMO` 是**合法并存**的
//! （唯一键含 `tier_type`），所以档位那层的 `PROMO/TIER` 与后续键是**真实会用到**的规则，
//! 不是装饰。`id ASC` 只在唯一索引被绕过（历史脏数据 / 直接改库）时生效，
//! 但它保证「同条件多次调用结果一致」这条卡面硬要求在任何数据下都成立。
//!
//! # 过滤放在 Rust 而不是 SQL：为了让边界可单测
//!
//! SQL 只做**粗筛**（客户 / 物料 / `status = 'ACTIVE'` / `deleted_at IS NULL`，
//! 全走 `uk_biz_cust_price` / `idx_biz_price_tier_lookup` 的索引前缀），
//! 数量 / 日期窗口与排序全部在 [`pick_agreed`] / [`pick_tier`] 里用纯函数判定：
//!
//! * 卡面要求「有效期边界」可测（`effective_to` 是**含**的、档位 `max_qty` 是**不含**的），
//!   这些边界写在 SQL 里就只能靠真库用例验证，而真库用例在本仓是 `ERP_DB_URL` 开关的
//!   （不设则静默 SKIP = 假绿）；写进 Rust 纯函数后 `cargo test --lib` 必跑，边界不再靠环境；
//! * 行数有界：粗筛已按 (客户, 物料) 收敛，一个物料几十条价格是上限量级，
//!   在内存里排全序不构成性能问题；
//! * SQL 与 Rust 不会分叉：窗口条件**只**存在于 [`date_covered`] / [`tier_covers`]，
//!   SQL 里不重复写一遍（写两份必有一份先腐化）。

use std::cmp::Reverse;

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

// ==================== 常量 ====================

/// 档位类型：量价阶梯（`ck_biz_price_tier_type` 域值）。
pub const TIER_TYPE_TIER: &str = "TIER";
/// 档位类型：促销（`ck_biz_price_tier_type` 域值）。
pub const TIER_TYPE_PROMO: &str = "PROMO";
/// 档位类型白名单（与 DDL 的 CHECK 逐字一致，单测钉住）。
pub const TIER_TYPES: [&str; 2] = [TIER_TYPE_TIER, TIER_TYPE_PROMO];

/// 只有启用态参与取价（`INACTIVE` = 停用不删行，保留价格历史）。
pub const STATUS_ACTIVE: &str = "ACTIVE";

// ==================== SQL（粗筛：有效期 / 数量窗口不在这里） ====================

/// ① 客户协议价粗筛：**不带** `min_qty` / 有效期条件（见文件头「过滤放在 Rust」）。
const AGREED_SQL: &str = "SELECT id, price, min_qty, effective_from, effective_to \
     FROM biz_customer_price \
     WHERE customer_id = ?::uuid AND material_id = ?::uuid \
       AND status = 'ACTIVE' AND deleted_at IS NULL";

/// ② 客户专属阶梯价粗筛（`customer_id IS NOT NULL`）。
const CUSTOMER_TIER_SQL: &str = "SELECT id, tier_type, price, min_qty, max_qty, effective_from, effective_to \
     FROM biz_price_tier \
     WHERE customer_id = ?::uuid AND material_id = ?::uuid \
       AND status = 'ACTIVE' AND deleted_at IS NULL";

/// ③ 通用阶梯价粗筛（`customer_id IS NULL`）。
const COMMON_TIER_SQL: &str = "SELECT id, tier_type, price, min_qty, max_qty, effective_from, effective_to \
     FROM biz_price_tier \
     WHERE customer_id IS NULL AND material_id = ?::uuid \
       AND status = 'ACTIVE' AND deleted_at IS NULL";

/// ④ 物料标准售价（软删行不算：删掉的物料给不出价）。
const MATERIAL_SQL: &str =
    "SELECT id, standard_price FROM biz_material WHERE id = ?::uuid AND deleted_at IS NULL";

/// 客户存在性（软删行不算）。
const CUSTOMER_EXISTS_SQL: &str =
    "SELECT COUNT(*) FROM biz_customer WHERE id = ?::uuid AND deleted_at IS NULL";

// ==================== 候选行 ====================

/// 客户协议价候选行（只有取价需要的列）。
#[derive(Debug, Clone, PartialEq)]
pub struct AgreedCandidate {
    /// 主键（回给前端的 `source_id`）。
    pub id: Uuid,
    /// 含税协议单价。
    pub price: Decimal,
    /// 起订量（0 = 不限制）。
    pub min_qty: Decimal,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）；`None` = 长期有效。
    pub effective_to: Option<NaiveDate>,
}

/// 阶梯价候选行（客户专属与通用共用同一形状）。
#[derive(Debug, Clone, PartialEq)]
pub struct TierCandidate {
    /// 主键（回给前端的 `source_id`）。
    pub id: Uuid,
    /// `TIER` / `PROMO`。
    pub tier_type: String,
    /// 含税单价。
    pub price: Decimal,
    /// 区间下界（含）。
    pub min_qty: Decimal,
    /// 区间上界（**不含**）；`None` = 无上限。
    pub max_qty: Option<Decimal>,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）；`None` = 长期有效。
    pub effective_to: Option<NaiveDate>,
}

/// 物料取价信息。
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialPrice {
    /// 物料主键。
    pub id: Uuid,
    /// 标准售价；`None` = 未维护。
    pub standard_price: Option<Decimal>,
}

/// 取价结果（`source` 从 [`crate::business::erp::price_dto::SOURCES`] 里取一个）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveOutcome {
    /// 含税单价；`None` = 五级全未命中。
    pub price: Option<Decimal>,
    /// 命中层级。
    pub source: &'static str,
    /// 命中行主键（`STANDARD` = 物料 id；`NONE` = `None`）。
    pub source_id: Option<Uuid>,
}

// ==================== 窗口判定（纯函数：卡面边界就写在这里） ====================

/// 有效期是否覆盖某日：`effective_from <= date` 且（`effective_to` 为空 或 `date <= effective_to`）。
///
/// **两端都含**：`effective_from` / `effective_to` 同日生效、同日失效（DDL 注释「失效日（含）」）。
pub fn date_covered(from: NaiveDate, to: Option<NaiveDate>, on: NaiveDate) -> bool {
    from <= on && to.is_none_or(|end| end >= on)
}

/// 档位区间是否覆盖某订购量：`min_qty <= qty < max_qty`（**左闭右开**）。
///
/// 右开是 ARCH-7 §15.3.3 ② 的原文：`min_qty <= 订购量 < max_qty`。
/// `max_qty` 为空 = 无上限；`qty == max_qty` **不命中**（该档是下一档的起点）。
/// 注意 DDL 的 `ck_biz_price_tier_qty` 保证 `max_qty > min_qty`（相等即空区间）。
pub fn tier_covers(min_qty: Decimal, max_qty: Option<Decimal>, qty: Decimal) -> bool {
    qty >= min_qty && max_qty.is_none_or(|max| qty < max)
}

/// 协议价的起订量条件：`min_qty <= qty`（契约定的是「达到这个量给这个价」）。
pub fn qty_reaches(min_qty: Decimal, qty: Decimal) -> bool {
    qty >= min_qty
}

// ==================== 全序比较键 ====================

/// 协议价排序键（越小越优先）：`min_qty DESC` → `effective_from DESC` → `id ASC`。
fn agreed_key(row: &AgreedCandidate) -> (Reverse<Decimal>, Reverse<NaiveDate>, Uuid) {
    (Reverse(row.min_qty), Reverse(row.effective_from), row.id)
}

/// 档位优先级：促销（`PROMO`）先于量价阶梯（`TIER`）。
///
/// 依据：促销一定是**更临时、更便宜**的活动价，落在同一个量段里若被阶梯价盖住，
/// 促销就永远生效不了（阶梯是常规价，促销是限时让利）。域外值一律按 `TIER` 处理
/// （DDL 的 CHECK 已经不让它进来，这里是对历史脏数据的兜底而非正常路径）。
fn tier_priority(tier_type: &str) -> u8 {
    if tier_type == TIER_TYPE_PROMO { 0 } else { 1 }
}

/// 档位排序键（越小越优先）：`PROMO` 先 → `min_qty DESC` → `effective_from DESC` → `id ASC`。
fn tier_key(row: &TierCandidate) -> (u8, Reverse<Decimal>, Reverse<NaiveDate>, Uuid) {
    (
        tier_priority(&row.tier_type),
        Reverse(row.min_qty),
        Reverse(row.effective_from),
        row.id,
    )
}

// ==================== 单级选优（纯函数） ====================

/// ① 从协议价候选里挑唯一命中行（全序，绝不并列）。
///
/// 过滤：起订量达标 + 有效期覆盖；排序：见 [`agreed_key`]。
pub fn pick_agreed(
    rows: &[AgreedCandidate],
    qty: Decimal,
    on: NaiveDate,
) -> Option<&AgreedCandidate> {
    rows.iter()
        .filter(|row| qty_reaches(row.min_qty, qty))
        .filter(|row| date_covered(row.effective_from, row.effective_to, on))
        .min_by(|left, right| agreed_key(left).cmp(&agreed_key(right)))
}

/// ②③ 从某一层档位候选里挑唯一命中行（全序，绝不并列）。
///
/// 过滤：区间覆盖 + 有效期覆盖；排序：见 [`tier_key`]。
pub fn pick_tier(rows: &[TierCandidate], qty: Decimal, on: NaiveDate) -> Option<&TierCandidate> {
    rows.iter()
        .filter(|row| tier_covers(row.min_qty, row.max_qty, qty))
        .filter(|row| date_covered(row.effective_from, row.effective_to, on))
        .min_by(|left, right| tier_key(left).cmp(&tier_key(right)))
}

/// 五级取价全链（纯函数：不碰库，输入全部是已经粗筛过的候选行）。
///
/// 命中即返回，不往下走；全未命中 → `NONE` + `price = None`。
/// `customer_tiers` / `common_tiers` 由调用方按客户维度分别取好
/// （无客户时客户专属那一层传空切片，自然落到 ③）。
pub fn resolve_from(
    material_id: Uuid,
    qty: Decimal,
    on: NaiveDate,
    agreed: &[AgreedCandidate],
    customer_tiers: &[TierCandidate],
    common_tiers: &[TierCandidate],
    standard_price: Option<Decimal>,
) -> ResolveOutcome {
    use crate::business::erp::price_dto::{
        SOURCE_AGREED, SOURCE_NONE, SOURCE_STANDARD, SOURCE_TIER_COMMON, SOURCE_TIER_CUSTOMER,
    };

    // ① 客户协议价
    if let Some(hit) = pick_agreed(agreed, qty, on) {
        return ResolveOutcome {
            price: Some(hit.price),
            source: SOURCE_AGREED,
            source_id: Some(hit.id),
        };
    }
    // ② 客户专属阶梯
    if let Some(hit) = pick_tier(customer_tiers, qty, on) {
        return ResolveOutcome {
            price: Some(hit.price),
            source: SOURCE_TIER_CUSTOMER,
            source_id: Some(hit.id),
        };
    }
    // ③ 通用阶梯
    if let Some(hit) = pick_tier(common_tiers, qty, on) {
        return ResolveOutcome {
            price: Some(hit.price),
            source: SOURCE_TIER_COMMON,
            source_id: Some(hit.id),
        };
    }
    // ④ 物料标准售价（只有维护了才给价：NULL 与 0 语义不同，NULL 继续往下走）
    if let Some(price) = standard_price {
        return ResolveOutcome {
            price: Some(price),
            source: SOURCE_STANDARD,
            source_id: Some(material_id),
        };
    }
    // ⑤ 都没有
    ResolveOutcome {
        price: None,
        source: SOURCE_NONE,
        source_id: None,
    }
}

// ==================== DB 读（粗筛） ====================

/// 物料 + 标准售价；物料不存在 / 已软删 → `None`（handler 转 404）。
pub async fn load_material(material_id: Uuid) -> DbResult<Option<MaterialPrice>> {
    let rows = db()
        .find_with_params(MATERIAL_SQL, vec![Value::from(&material_id)])
        .await?;
    rows.first()
        .map(|row| {
            Ok(MaterialPrice {
                id: row.get_typed("id")?,
                standard_price: row.get_typed("standard_price")?,
            })
        })
        .transpose()
}

/// 客户是否存在（软删不算：删了的客户不该再按它的协议价报价）。
pub async fn customer_exists(customer_id: Uuid) -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(CUSTOMER_EXISTS_SQL, vec![Value::from(&customer_id)])
        .await?;
    Ok(count > 0)
}

/// 某客户对某物料的协议价候选（只含 `ACTIVE` + 未软删行）。
pub async fn load_agreed(customer_id: Uuid, material_id: Uuid) -> DbResult<Vec<AgreedCandidate>> {
    let rows = db()
        .find_with_params(
            AGREED_SQL,
            vec![Value::from(&customer_id), Value::from(&material_id)],
        )
        .await?;
    rows.iter().map(agreed_of_row).collect()
}

/// 客户专属阶梯候选（`customer_id IS NOT NULL`）。
pub async fn load_customer_tiers(
    customer_id: Uuid,
    material_id: Uuid,
) -> DbResult<Vec<TierCandidate>> {
    let rows = db()
        .find_with_params(
            CUSTOMER_TIER_SQL,
            vec![Value::from(&customer_id), Value::from(&material_id)],
        )
        .await?;
    rows.iter().map(tier_of_row).collect()
}

/// 通用阶梯候选（`customer_id IS NULL`）。
pub async fn load_common_tiers(material_id: Uuid) -> DbResult<Vec<TierCandidate>> {
    let rows = db()
        .find_with_params(COMMON_TIER_SQL, vec![Value::from(&material_id)])
        .await?;
    rows.iter().map(tier_of_row).collect()
}

/// 协议价行 → 候选结构。
fn agreed_of_row(row: &Row) -> DbResult<AgreedCandidate> {
    Ok(AgreedCandidate {
        id: row.get_typed("id")?,
        price: row.get_typed("price")?,
        min_qty: row.get_typed("min_qty")?,
        effective_from: row.get_typed("effective_from")?,
        effective_to: row.get_typed("effective_to")?,
    })
}

/// 档位行 → 候选结构（客户专属 / 通用共用）。
fn tier_of_row(row: &Row) -> DbResult<TierCandidate> {
    Ok(TierCandidate {
        id: row.get_typed("id")?,
        tier_type: row.get_typed("tier_type")?,
        price: row.get_typed("price")?,
        min_qty: row.get_typed("min_qty")?,
        max_qty: row.get_typed("max_qty")?,
        effective_from: row.get_typed("effective_from")?,
        effective_to: row.get_typed("effective_to")?,
    })
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点字面量")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn uid(last: u8) -> Uuid {
        Uuid::parse_str(&format!("5b000000-0000-4000-8000-0000000000{last:02}")).unwrap()
    }

    fn agreed(
        id: Uuid,
        price: &str,
        min_qty: &str,
        from: &str,
        to: Option<&str>,
    ) -> AgreedCandidate {
        AgreedCandidate {
            id,
            price: dec(price),
            min_qty: dec(min_qty),
            effective_from: date(from),
            effective_to: to.map(date),
        }
    }

    fn tier(
        id: Uuid,
        tier_type: &str,
        price: &str,
        min_qty: &str,
        max_qty: Option<&str>,
        from: &str,
        to: Option<&str>,
    ) -> TierCandidate {
        TierCandidate {
            id,
            tier_type: tier_type.to_string(),
            price: dec(price),
            min_qty: dec(min_qty),
            max_qty: max_qty.map(dec),
            effective_from: date(from),
            effective_to: to.map(date),
        }
    }

    // ---------- 有效期边界 ----------

    /// 有效期两端**都含**：`from` 当天命中、`to` 当天命中，前后一天各差一步。
    #[test]
    fn date_window_is_inclusive_on_both_ends() {
        let from = date("2026-01-01");
        let to = Some(date("2026-03-31"));

        assert!(
            date_covered(from, to, date("2026-01-01")),
            "生效日当天必须命中"
        );
        assert!(
            date_covered(from, to, date("2026-03-31")),
            "失效日当天必须命中"
        );
        assert!(date_covered(from, to, date("2026-02-15")), "区间内命中");
        assert!(
            !date_covered(from, to, date("2025-12-31")),
            "生效日前一天不命中"
        );
        assert!(
            !date_covered(from, to, date("2026-04-01")),
            "失效日后一天不命中"
        );
        assert!(
            date_covered(from, None, date("2099-12-31")),
            "无失效日 = 长期有效"
        );
    }

    // ---------- 档位区间边界（左闭右开） ----------

    #[test]
    fn tier_window_is_left_closed_right_open() {
        let min = dec("10");
        let max = Some(dec("100"));

        assert!(tier_covers(min, max, dec("10")), "qty == min 命中");
        assert!(!tier_covers(min, max, dec("9.9999")), "qty < min 不命中");
        assert!(tier_covers(min, max, dec("99.9999")), "qty < max 命中");
        assert!(
            !tier_covers(min, max, dec("100")),
            "qty == max 不命中（右开）"
        );
        assert!(tier_covers(min, None, dec("999999")), "max 为空 = 无上限");
    }

    #[test]
    fn agreed_qty_floor_is_inclusive() {
        assert!(qty_reaches(dec("10"), dec("10")));
        assert!(qty_reaches(dec("10"), dec("10.0001")));
        assert!(!qty_reaches(dec("10"), dec("9.9999")));
        assert!(qty_reaches(dec("0"), dec("0")), "min_qty = 0 不限制");
    }

    // ---------- 5 级各级命中 ----------

    /// ① 协议价命中：价与 source_id 都来自那一行。
    #[test]
    fn level1_agreed_hits() {
        let rows = vec![agreed(uid(1), "10.0000", "0", "2026-01-01", None)];
        let out = resolve_from(
            uid(9),
            dec("5"),
            date("2026-02-01"),
            &rows,
            &[],
            &[],
            Some(dec("99")),
        );
        assert_eq!(out.source, "AGREED");
        assert_eq!(out.price, Some(dec("10.0000")));
        assert_eq!(out.source_id, Some(uid(1)));
    }

    /// ② 客户专属阶梯命中（协议价层为空）。
    #[test]
    fn level2_customer_tier_hits() {
        let tiers = vec![tier(
            uid(2),
            "TIER",
            "8.0000",
            "0",
            Some("100"),
            "2026-01-01",
            None,
        )];
        let out = resolve_from(
            uid(9),
            dec("50"),
            date("2026-02-01"),
            &[],
            &tiers,
            &[],
            Some(dec("99")),
        );
        assert_eq!(out.source, "TIER_CUSTOMER");
        assert_eq!(out.price, Some(dec("8.0000")));
        assert_eq!(out.source_id, Some(uid(2)));
    }

    /// ③ 通用阶梯命中（协议价 / 客户专属都没有）。
    #[test]
    fn level3_common_tier_hits() {
        let common = vec![tier(
            uid(3),
            "TIER",
            "9.5000",
            "0",
            None,
            "2026-01-01",
            None,
        )];
        let out = resolve_from(
            uid(9),
            dec("50"),
            date("2026-02-01"),
            &[],
            &[],
            &common,
            Some(dec("99")),
        );
        assert_eq!(out.source, "TIER_COMMON");
        assert_eq!(out.price, Some(dec("9.5000")));
        assert_eq!(out.source_id, Some(uid(3)));
    }

    /// ④ 标准售价命中：`source_id` 是**物料 id**，不是某条价格行。
    #[test]
    fn level4_standard_price_hits() {
        let out = resolve_from(
            uid(9),
            dec("50"),
            date("2026-02-01"),
            &[],
            &[],
            &[],
            Some(dec("11.0000")),
        );
        assert_eq!(out.source, "STANDARD");
        assert_eq!(out.price, Some(dec("11.0000")));
        assert_eq!(out.source_id, Some(uid(9)), "标准价的 source_id 是物料 id");
    }

    /// ⑤ 全未命中：`price = None` + `NONE` + `source_id = None`。
    #[test]
    fn level5_all_miss_returns_none() {
        let out = resolve_from(uid(9), dec("50"), date("2026-02-01"), &[], &[], &[], None);
        assert_eq!(out.source, "NONE");
        assert_eq!(out.price, None);
        assert_eq!(out.source_id, None);
    }

    /// 优先级严格：五级同时可用时，永远取最高的那一级（不是最便宜的那一级）。
    #[test]
    fn priority_is_strict_and_short_circuits() {
        let agreed_rows = vec![agreed(uid(1), "20.0000", "0", "2026-01-01", None)];
        let customer_tiers = vec![tier(
            uid(2),
            "PROMO",
            "5.0000",
            "0",
            None,
            "2026-01-01",
            None,
        )];
        let common_tiers = vec![tier(
            uid(3),
            "PROMO",
            "4.0000",
            "0",
            None,
            "2026-01-01",
            None,
        )];

        // ① 命中 → 即使 ②③ 更便宜也取 ①（协议价是谈定的契约价，不能被活动价挤掉）
        let out = resolve_from(
            uid(9),
            dec("10"),
            date("2026-02-01"),
            &agreed_rows,
            &customer_tiers,
            &common_tiers,
            Some(dec("1.0000")),
        );
        assert_eq!(out.source, "AGREED");
        assert_eq!(out.price, Some(dec("20.0000")));

        // ② 命中 → 不落 ③
        let out = resolve_from(
            uid(9),
            dec("10"),
            date("2026-02-01"),
            &[],
            &customer_tiers,
            &common_tiers,
            Some(dec("1.0000")),
        );
        assert_eq!(out.source, "TIER_CUSTOMER");
        assert_eq!(out.price, Some(dec("5.0000")));

        // ③ 命中 → 不落 ④
        let out = resolve_from(
            uid(9),
            dec("10"),
            date("2026-02-01"),
            &[],
            &[],
            &common_tiers,
            Some(dec("1.0000")),
        );
        assert_eq!(out.source, "TIER_COMMON");
        assert_eq!(out.price, Some(dec("4.0000")));
    }

    // ---------- 有效期边界导致的降级 ----------

    /// 协议价已过期（`effective_to` 前一天可以，当天前后一步之差决定降级）。
    #[test]
    fn expired_agreed_falls_through_to_next_level() {
        let rows = vec![agreed(
            uid(1),
            "10.0000",
            "0",
            "2026-01-01",
            Some("2026-03-31"),
        )];

        // 失效日当天仍命中
        let out = resolve_from(
            uid(9),
            dec("1"),
            date("2026-03-31"),
            &rows,
            &[],
            &[],
            Some(dec("12")),
        );
        assert_eq!(out.source, "AGREED");

        // 失效日次日 → 掉到 ④
        let out = resolve_from(
            uid(9),
            dec("1"),
            date("2026-04-01"),
            &rows,
            &[],
            &[],
            Some(dec("12")),
        );
        assert_eq!(out.source, "STANDARD");
        assert_eq!(out.price, Some(dec("12")));
    }

    /// 未到生效日 → 不命中（不能拿未来才生效的价开单）。
    #[test]
    fn future_agreed_is_not_used() {
        let rows = vec![agreed(uid(1), "10.0000", "0", "2026-06-01", None)];
        let out = resolve_from(uid(9), dec("1"), date("2026-05-31"), &rows, &[], &[], None);
        assert_eq!(out.source, "NONE");
    }

    /// 订购量不达起订量 → 降级；达到 → 命中。
    #[test]
    fn qty_below_min_qty_falls_through() {
        let rows = vec![agreed(uid(1), "10.0000", "100", "2026-01-01", None)];

        let out = resolve_from(
            uid(9),
            dec("99.9999"),
            date("2026-02-01"),
            &rows,
            &[],
            &[],
            None,
        );
        assert_eq!(out.source, "NONE");

        let out = resolve_from(
            uid(9),
            dec("100"),
            date("2026-02-01"),
            &rows,
            &[],
            &[],
            None,
        );
        assert_eq!(out.source, "AGREED");
    }

    // ---------- 多条协议价 ----------

    /// 卡面主规则：多条命中取 `min_qty` 最大（最贴合本次订购量的那一档）。
    #[test]
    fn agreed_picks_largest_min_qty() {
        let rows = vec![
            agreed(uid(1), "10.0000", "0", "2026-01-01", None),
            agreed(uid(2), "9.0000", "10", "2026-01-01", None),
            agreed(uid(3), "8.0000", "100", "2026-01-01", None),
        ];
        // qty = 100：三条都满足 min_qty <= 100，取 min_qty 最大的 uid(3)
        let hit = pick_agreed(&rows, dec("100"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(3));
        assert_eq!(hit.price, dec("8.0000"));

        // qty = 50：只有 0 与 10 两档满足 → 取 uid(2)
        let hit = pick_agreed(&rows, dec("50"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(2));

        // qty = 5：只有 0 档 → uid(1)
        let hit = pick_agreed(&rows, dec("5"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(1));
    }

    /// 同 `min_qty` 多条（旧价未失效 + 新价已生效）→ 取**最近生效**的那条。
    #[test]
    fn agreed_tie_on_min_qty_picks_newest_effective_from() {
        let rows = vec![
            agreed(uid(1), "10.0000", "0", "2025-01-01", None),
            agreed(uid(2), "12.0000", "0", "2026-01-01", None),
            agreed(uid(3), "11.0000", "0", "2025-07-01", None),
        ];
        let hit = pick_agreed(&rows, dec("1"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(2), "同起订量取最近生效的协议价");
        assert_eq!(hit.price, dec("12.0000"));
    }

    /// 三级键全同（历史脏数据绕过唯一索引）→ 按 `id ASC` 兜底，仍是确定答案。
    #[test]
    fn agreed_full_tie_falls_back_to_id() {
        let rows = vec![
            agreed(uid(7), "10.0000", "0", "2026-01-01", None),
            agreed(uid(3), "10.0000", "0", "2026-01-01", None),
            agreed(uid(5), "10.0000", "0", "2026-01-01", None),
        ];
        let hit = pick_agreed(&rows, dec("1"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(3), "同键取 id 最小，保证确定性");
    }

    /// 候选顺序被打乱不影响结果（卡面「同条件多次调用结果一致」）。
    #[test]
    fn candidate_order_does_not_change_result() {
        let base = vec![
            agreed(uid(1), "10.0000", "0", "2026-01-01", None),
            agreed(uid(2), "9.0000", "10", "2026-01-01", None),
            agreed(uid(3), "9.5000", "10", "2025-06-01", None),
        ];
        let expected = pick_agreed(&base, dec("20"), date("2026-02-01"))
            .expect("应命中")
            .id;

        // 6 种排列全试：uid(2) 与 uid(3) 同为 min_qty=10，取 effective_from 更新的 uid(2)
        assert_eq!(expected, uid(2));
        let permutations: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for order in permutations {
            let rows: Vec<AgreedCandidate> = order.iter().map(|&i| base[i].clone()).collect();
            let hit = pick_agreed(&rows, dec("20"), date("2026-02-01")).expect("应命中");
            assert_eq!(hit.id, expected, "排列 {order:?} 结果必须一致");
        }
    }

    // ---------- 多条档位 ----------

    /// 促销优先于量价阶梯：同区间两条并存时取 `PROMO`。
    #[test]
    fn promo_beats_tier_in_same_window() {
        let rows = vec![
            tier(uid(1), "TIER", "10.0000", "0", None, "2026-01-01", None),
            tier(uid(2), "PROMO", "8.0000", "0", None, "2026-01-01", None),
        ];
        let hit = pick_tier(&rows, dec("5"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(2));
        assert_eq!(hit.tier_type, "PROMO");
    }

    /// 区间重叠时取 `min_qty` 最大的那一档（更贴合本次数量）。
    #[test]
    fn tier_overlap_picks_highest_min_qty() {
        let rows = vec![
            tier(
                uid(1),
                "TIER",
                "10.0000",
                "0",
                Some("100"),
                "2026-01-01",
                None,
            ),
            tier(
                uid(2),
                "TIER",
                "9.0000",
                "50",
                Some("200"),
                "2026-01-01",
                None,
            ),
            tier(
                uid(3),
                "TIER",
                "7.0000",
                "150",
                Some("500"),
                "2026-01-01",
                None,
            ),
        ];
        // qty = 160：1 不命中（>100）、2 命中、3 命中 → 取 min_qty 更大的 uid(3)
        let hit = pick_tier(&rows, dec("160"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(3));

        // qty = 60：1 与 2 命中 → uid(2)
        let hit = pick_tier(&rows, dec("60"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(2));

        // qty = 150：2 命中（<200）、3 命中（>=150）→ uid(3)；右开边界 100 见下
        let hit = pick_tier(&rows, dec("150"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(3));

        // qty = 100：1 不命中（右开），2 命中 → uid(2)
        let hit = pick_tier(&rows, dec("100"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(2));
    }

    /// 客户专属阶梯全不命中 → 落通用阶梯（两层各自独立判定）。
    #[test]
    fn customer_tier_miss_falls_to_common() {
        // 客户专属档位只覆盖 100+
        let customer_tiers = vec![tier(
            uid(1),
            "TIER",
            "5.0000",
            "100",
            None,
            "2026-01-01",
            None,
        )];
        let common_tiers = vec![tier(
            uid(2),
            "TIER",
            "9.0000",
            "0",
            None,
            "2026-01-01",
            None,
        )];

        let out = resolve_from(
            uid(9),
            dec("10"),
            date("2026-02-01"),
            &[],
            &customer_tiers,
            &common_tiers,
            None,
        );
        assert_eq!(out.source, "TIER_COMMON");
        assert_eq!(out.price, Some(dec("9.0000")));
    }

    /// 已过期的档位即使价格更低、`min_qty` 更大也不能用。
    #[test]
    fn expired_tier_is_ignored() {
        let rows = vec![
            tier(
                uid(1),
                "TIER",
                "1.0000",
                "100",
                None,
                "2025-01-01",
                Some("2025-12-31"),
            ),
            tier(uid(2), "TIER", "9.0000", "0", None, "2026-01-01", None),
        ];
        let hit = pick_tier(&rows, dec("500"), date("2026-02-01")).expect("应命中");
        assert_eq!(hit.id, uid(2), "过期档位不参与取价");
    }

    // ---------- 确定性 ----------

    /// 同条件反复调用（含乱序输入）结果恒等 —— 卡面「边界必须确定性」。
    #[test]
    fn repeated_resolution_is_stable() {
        let agreed_rows = vec![
            agreed(uid(1), "10.0000", "0", "2025-01-01", Some("2026-12-31")),
            agreed(uid(2), "9.0000", "100", "2025-01-01", Some("2026-12-31")),
        ];
        let customer_tiers = vec![
            tier(uid(3), "TIER", "8.0000", "0", None, "2026-01-01", None),
            tier(uid(4), "PROMO", "8.0000", "0", None, "2026-01-01", None),
        ];
        let common_tiers = vec![tier(
            uid(5),
            "TIER",
            "9.0000",
            "0",
            None,
            "2026-01-01",
            None,
        )];

        let first = resolve_from(
            uid(9),
            dec("100"),
            date("2026-02-01"),
            &agreed_rows,
            &customer_tiers,
            &common_tiers,
            Some(dec("11")),
        );
        for _ in 0..20 {
            let again = resolve_from(
                uid(9),
                dec("100"),
                date("2026-02-01"),
                &agreed_rows,
                &customer_tiers,
                &common_tiers,
                Some(dec("11")),
            );
            assert_eq!(again, first, "同条件多次调用必须完全一致");
        }
        assert_eq!(first.source, "AGREED");
        assert_eq!(
            first.source_id,
            Some(uid(2)),
            "qty=100 命中 min_qty=100 那条"
        );
    }

    // ---------- SQL 口径守卫（防止后人把过滤/条件改孬） ----------

    #[test]
    fn sql_guards_alive_rows_and_status() {
        for statement in [AGREED_SQL, CUSTOMER_TIER_SQL, COMMON_TIER_SQL] {
            assert!(
                statement.contains("deleted_at IS NULL"),
                "价格查询必须排除软删行：{statement}"
            );
            assert!(
                statement.contains("status = 'ACTIVE'"),
                "价格查询只认启用态：{statement}"
            );
            assert!(
                statement.contains("material_id = ?::uuid"),
                "必须按物料收窄：{statement}"
            );
        }
        assert!(
            COMMON_TIER_SQL.contains("customer_id IS NULL"),
            "通用档位按 customer_id IS NULL 取"
        );
        assert!(CUSTOMER_TIER_SQL.contains("customer_id = ?::uuid"));
        assert!(MATERIAL_SQL.contains("deleted_at IS NULL"));
        assert!(CUSTOMER_EXISTS_SQL.contains("deleted_at IS NULL"));
    }

    /// 档位域值与 DDL 的 `ck_biz_price_tier_type` 逐字一致。
    #[test]
    fn tier_type_domain_matches_ddl() {
        assert_eq!(TIER_TYPES, ["TIER", "PROMO"]);
        assert_eq!(tier_priority(TIER_TYPE_PROMO), 0);
        assert_eq!(tier_priority(TIER_TYPE_TIER), 1);
        assert_eq!(tier_priority("LEGACY"), 1, "域外值按 TIER 兜底");
        assert_eq!(STATUS_ACTIVE, "ACTIVE");
    }
}
