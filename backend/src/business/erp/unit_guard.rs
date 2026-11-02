//! 单据「计量单位止血」守卫（ARCH-3 §5 DC-09 / §6 R-P0-4，**方案 B**）。
//!
//! # 为什么要有这个守卫
//!
//! `biz_unit` 有 `is_base` / `base_unit_id` / `rate` 三列，主数据页可以配出
//! 「1 箱 = 12 个」这样的折算单位；但**单据链路一处都没读过它们**：
//! 采购 / 收货 / 调拨明细只落 `unit_id + qty`，库存三本账（
//! `biz_inventory` / `biz_inventory_location` / `biz_inventory_txn`）直接拿 `qty` 加减。
//! 于是「按箱下单 5 箱」在库里就是 `qty = 5`，账上只有 5 个而不是 60 个 —— 静默算错账。
//!
//! 完整修法（方案 A：明细存 `qty_base = qty × rate`，三本账只用基本单位）要动 4 张明细表 +
//! 三本账 + 巡检，属专项；**本模块只做止血**：单据明细的单位必须是基本单位，
//! 不是就 400 并告诉用户「1 箱 = 12 个 → 录 12」。
//!
//! # 为什么是「物料的基础单位」而不是请求里的 `unit_id`
//!
//! 三张单据的明细入参**都没有 `unit_id`**（采购 [`crate::business::erp::purchase_dto::PurchaseOrderItemReq`]、
//! 收货 [`crate::business::erp::stockin_dto::StockInItemReq`]、调拨
//! [`crate::business::erp::transfer_dto::TransferItemReq`]）：明细的 `unit_id` 由
//! `biz_material.unit_id` 带出。所以「这一行会记错账」的判据只能落在物料上 ——
//! 物料挂的是折算单位，这张单据就一定会错。
//!
//! # 边界（刻意不做的事）
//!
//! - **不改任何数据**：本模块只读 + 报错，存量非基本单位的明细行由业务确认后再修（见卡面 §4）；
//! - **不碰主数据**：`biz_material.unit_id` 允许指向折算单位这一点本模块不管（改了会牵连
//!   `biz_material` 的既有语义，属另一张卡）；
//! - **不改变合法路径的任何行为**：`is_base = 1` 直接 `Ok(())`，一次多余的库读而已。
//!
//! # 单元（unit）与「未删除」
//!
//! 单位是软删（`deleted_at`）。守卫**不看** `deleted_at`：已停用 / 已软删的单位上挂的
//! 存量单据仍然读得到 `is_base`，而「这个单位是不是基本单位」是身份属性，与删除无关。
//! 真正查不到单位行（数据损坏）才报 [`crate::shared::error_code::ERP_REFERENCE_MISSING`]。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;

/// 取「物料 → 它的单位（含折算基准名称）」。
///
/// 基表是 `biz_material`：物料不存在（或已软删）时整行查不到 → `Ok(None)`，
/// 交给调用方原有的「物料不存在」报错去说（三个调用点都是先查物料再调本守卫）。
const MATERIAL_UNIT_SQL: &str = "\
SELECT m.material_code, u.unit_name, u.is_base, u.rate, b.unit_name AS base_unit_name \
  FROM biz_material m \
  LEFT JOIN biz_unit u ON u.id = m.unit_id \
  LEFT JOIN biz_unit b ON b.id = u.base_unit_id \
 WHERE m.id = ?::uuid AND m.deleted_at IS NULL";

/// 一行的「物料 + 单位」快照。
#[derive(Debug, Clone)]
pub struct MaterialUnitRef {
    /// 物料编码（报错文案里点名用，用户照编码就能找到物料）。
    pub material_code: String,
    /// 单位名称；`None` = 物料的基础单位行已经不存在（数据损坏）。
    pub unit_name: Option<String>,
    /// 是否基本单位；`None` 同上。
    pub is_base: Option<i16>,
    /// 折算率（`is_base = 0` 时才有意义）。
    pub rate: Option<Decimal>,
    /// 折算基准单位名称（`is_base = 0` 时才有意义）。
    pub base_unit_name: Option<String>,
}

impl MaterialUnitRef {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            material_code: row.get_typed("material_code")?,
            unit_name: row.get_typed("unit_name")?,
            is_base: row.get_typed("is_base")?,
            rate: row.get_typed("rate")?,
            base_unit_name: row.get_typed("base_unit_name")?,
        })
    }
}

/// 按物料取「物料编码 + 单位」；物料不存在 → `Ok(None)`。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn material_unit_ref(material_id: Uuid) -> DbResult<Option<MaterialUnitRef>> {
    let rows = db()
        .find_with_params(MATERIAL_UNIT_SQL, vec![Value::from(&material_id)])
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(MaterialUnitRef::from_row(row)?)),
    }
}

/// `Decimal` 转人话：去掉补位的尾零（`12.000000` → `12`，`0.500000` → `0.5`）。
///
/// 文案里 `1 箱 = 12.000000 个` 会让人怀疑系统在算浮点，所以统一在这里收口。
pub fn trim_decimal(value: Decimal) -> String {
    let text = value.normalize().to_string();
    if text == "-0" { "0".to_string() } else { text }
}

/// 非基本单位的统一报错文案（三处单据共用，任何一个字改动都算契约变更）。
///
/// 形如：
/// `items[0] 物料 W1B-M-003：单位「箱」不是基本单位：本版本不支持多单位换算，
/// 请按基本单位录入（1 箱 = 12 个 → 录 12）`
///
/// 末段的「录 N」是**按本行数量算好的建议值**（`qty × rate`，4 位四舍五入），
/// 用户照抄就能过，不用自己拿计算器乘。
fn not_base_message(
    line_no: usize,
    material_code: &str,
    unit_name: &str,
    rate: Decimal,
    base_unit_name: &str,
    qty: Decimal,
) -> String {
    let suggested = trim_decimal((qty * rate).round_dp(4));
    format!(
        "items[{line_no}] 物料 {material_code}：单位「{unit_name}」不是基本单位：\
         本版本不支持多单位换算，请按基本单位录入\
         （1 {unit_name} = {} {base_unit_name} → 录 {suggested}）",
        trim_decimal(rate)
    )
}

/// 单据明细的**单位止血闸**：物料的基础单位必须是基本单位（`biz_unit.is_base = 1`）。
///
/// # 入参
///
/// - `material_id`：明细行的物料（调用方已校验存在性）；
/// - `line_no`：明细行下标（0 起，与请求体 `items[i]` 对齐，报错文案里点出来）；
/// - `qty`：本行数量（只用于把折算建议算出来，不参与落库）。
///
/// # 返回
///
/// - `Ok(())`：基本单位 / 物料在本查询里已消失（调用方的存在性校验会兜住）；
/// - `Err(400 + ERP_UNIT_NOT_BASE)`：单位是折算单位，文案带「本版本不支持多单位换算」；
/// - `Err(400 + ERP_REFERENCE_MISSING)`：物料在、单位行却查不到（数据损坏，要人看）。
///
/// # 异常情况
///
/// 数据库异常 → [`ApiError`]（`500`，走 `?` 的统一出口）。
pub async fn ensure_base_unit(
    material_id: Uuid,
    line_no: usize,
    qty: Decimal,
) -> std::result::Result<(), ApiError> {
    let Some(reference) = material_unit_ref(material_id).await? else {
        // 物料查不到：三个调用点都在本守卫之前校验过存在性，走到这里只能是并发删除，
        // 交给调用方原来的文案去说，不在这里抢答。
        return Ok(());
    };
    let Some(unit_name) = reference.unit_name.as_deref() else {
        return Err(ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!(
                "items[{line_no}] 物料 {} 的基础单位在 biz_unit 里查不到（unit_id 指向的行已不存在）：\
                 请先在「物料」主数据里重选计量单位",
                reference.material_code
            ),
        ));
    };
    if reference.is_base == Some(1) {
        return Ok(());
    }
    // 折算基准缺失（`base_unit_id` 为空）只可能是脏数据：文案里用中性词，不编一个单位名出来。
    let base_unit_name = reference.base_unit_name.as_deref().unwrap_or("基本单位");
    Err(ApiError::new(
        400,
        error_code::ERP_UNIT_NOT_BASE,
        not_base_message(
            line_no,
            &reference.material_code,
            unit_name,
            reference.rate.unwrap_or(Decimal::ONE),
            base_unit_name,
            qty,
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试数量字面量非法")
    }

    /// ① 文案逐字锁死：`1 箱 = 12 个 → 录 12` 里的三个数字都是算出来的，不是拼死的。
    ///
    /// 这条用例的作用是**防止后续有人把文案改短**（前端 `error-code.ts` 与验收脚本都按它断言）。
    #[test]
    fn not_base_message_is_exact() {
        let text = not_base_message(0, "W1B-M-003", "箱", dec("12"), "个", dec("1"));
        assert_eq!(
            text,
            "items[0] 物料 W1B-M-003：单位「箱」不是基本单位：本版本不支持多单位换算，\
             请按基本单位录入（1 箱 = 12 个 → 录 12）"
        );
        assert!(
            text.contains("单位「箱」不是基本单位：本版本不支持多单位换算，请按基本单位录入（1 箱 = 12 个 → 录 12）"),
            "{text}"
        );
    }

    /// ② 建议数量 = 本行数量 × 折算率（4 位四舍五入）：3 箱 × 12 = 36；0.5 箱 × 12 = 6。
    #[test]
    fn suggestion_is_qty_times_rate() {
        let text = not_base_message(2, "M-1", "箱", dec("12"), "个", dec("3"));
        assert!(text.starts_with("items[2] 物料 M-1："), "{text}");
        assert!(text.ends_with("→ 录 36）"), "{text}");

        let half = not_base_message(0, "M-1", "打", dec("12.000000"), "个", dec("0.5"));
        assert!(half.contains("1 打 = 12 个 → 录 6）"), "{half}");

        // 4 位以外的尾巴四舍五入，不出现 `7.50000` 这种浮点感
        let odd = not_base_message(0, "M-1", "桶", dec("2.5"), "千克", dec("3"));
        assert!(odd.contains("1 桶 = 2.5 千克 → 录 7.5）"), "{odd}");
    }

    /// ③ `trim_decimal` 只去尾零，不去有效小数；`-0` 归一成 `0`。
    #[test]
    fn trim_decimal_strips_trailing_zeros_only() {
        assert_eq!(trim_decimal(dec("12.000000")), "12");
        assert_eq!(trim_decimal(dec("0.500000")), "0.5");
        assert_eq!(trim_decimal(dec("1.2500")), "1.25");
        assert_eq!(trim_decimal(dec("-0.000000")), "0");
    }
}
