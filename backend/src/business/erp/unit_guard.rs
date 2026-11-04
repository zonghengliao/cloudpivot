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
//! - **不改变合法路径的任何行为**：`is_base = 1` 直接 `Ok(())`，一次多余的库读而已；
//! - 完整换算（方案 A）不做：本模块只保证「折算单位进不了账」。
//!
//! # 三道闸（DC-09 + 本卡补口）
//!
//! ```text
//! ① 建单 / 改单期   ensure_base_unit（判「物料现在的单位」）  采购 / 收货 / 调拨
//! ② 过账 / 结算期   ensure_unit_row_is_base（判「明细快照的 unit_id」）
//!                   · stockin_handler::post_stock_in / post_after_approval
//!                   · transfer_settle::settle_transfer（调拨明细无 unit_id ⇒ 用 ensure_base_unit_in）
//! ③ 物料主数据      ensure_material_unit_is_base（unit_id 只能指向基本单位）
//! ```
//!
//! ① 挡不住「绕过接口 UPDATE 明细 / 物料」与「升级前建的草稿」，所以写账的两个入口要按
//! **快照**再判一次；③ 则是把「建出来的物料任何单据都 400」这种死路堵在源头。
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
    material_unit_ref_in(db(), material_id).await
}

/// 同 [`material_unit_ref`]，但**吃调用方的执行器**（事务内复检用：与写账同一连接）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn material_unit_ref_in(
    ex: &impl SqlExecutor,
    material_id: Uuid,
) -> DbResult<Option<MaterialUnitRef>> {
    let rows = ex
        .query_sql(
            MATERIAL_UNIT_SQL.to_string(),
            vec![Value::from(&material_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(MaterialUnitRef::from_row(row)?)),
    }
}

/// 取「一个单位行 +（可选）物料的编码」。
///
/// 第一条 `?` 是**物料**（空串 = 不关心物料编码），第二条是单位。
/// 物料用 `LEFT JOIN` + `NULLIF(?::text,'')::uuid`：不传物料时 `m.id = NULL` 恒假，
/// 结果仍是「单位行一行」，只是 `material_code` 为 NULL —— 不会把整行 join 掉。
const UNIT_LINE_SQL: &str = "\
SELECT u.unit_name, u.is_base, u.rate, b.unit_name AS base_unit_name, m.material_code \
  FROM biz_unit u \
  LEFT JOIN biz_unit b ON b.id = u.base_unit_id \
  LEFT JOIN biz_material m ON m.id = NULLIF(?::text, '')::uuid AND m.deleted_at IS NULL \
 WHERE u.id = ?::uuid";

/// **单据明细自带单位**时的单位快照（`unit_id` 来自明细行，不是物料）。
///
/// 与 [`MaterialUnitRef`] 的区别只有一处：`material_code` 可空 ——
/// 明细只带了 `unit_id` 时也能判「是不是基本单位」，只是报错文案里点不出物料编码。
#[derive(Debug, Clone)]
pub struct UnitLineRef {
    /// 物料编码（`None` = 该单位不是从物料上取的 / 物料已删除）。
    pub material_code: Option<String>,
    /// 单位名称；`None` = 单位行已不存在（数据损坏）。
    pub unit_name: Option<String>,
    /// 是否基本单位；`None` 同上。
    pub is_base: Option<i16>,
    /// 折算率（`is_base = 0` 时才有意义）。
    pub rate: Option<Decimal>,
    /// 折算基准单位名称（`is_base = 0` 时才有意义）。
    pub base_unit_name: Option<String>,
}

impl UnitLineRef {
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

/// 按**单位主键**取单位行（可选带出物料编码）；单位不存在 → `Ok(None)`。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn unit_line_ref(
    ex: &impl SqlExecutor,
    unit_id: Uuid,
    material_id: Option<Uuid>,
) -> DbResult<Option<UnitLineRef>> {
    let material = material_id.map(|id| id.to_string()).unwrap_or_default();
    let rows = ex
        .query_sql(
            UNIT_LINE_SQL.to_string(),
            vec![Value::Text(material), Value::from(&unit_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(UnitLineRef::from_row(row)?)),
    }
}

/// `Decimal` 转人话：去掉补位的尾零（`12.000000` → `12`，`0.500000` → `0.5`）。
///
/// 文案里 `1 箱 = 12.000000 个` 会让人怀疑系统在算浮点，所以统一在这里收口。
pub fn trim_decimal(value: Decimal) -> String {
    let text = value.normalize().to_string();
    if text == "-0" { "0".to_string() } else { text }
}

/// 报错文案里的「主体」：有物料编码就点名物料，否则只说「明细」。
///
/// 结算 / 过账期复检按的是**明细快照的单位**，某些调用点（如调拨明细）拿不到物料编码，
/// 文案也只能退化成「明细」——不编一个假编码出来。
fn line_subject(material_code: Option<&str>) -> String {
    match material_code {
        Some(code) => format!("物料 {code}"),
        None => "明细".to_string(),
    }
}

/// 非基本单位的统一报错文案（**唯一实现**，所有入口共用）。
///
/// 形如：
/// `items[0] 物料 W1B-M-003：单位「箱」不是基本单位：本版本不支持多单位换算，
/// 请按基本单位录入（1 箱 = 12 个 → 录 12）`
///
/// 末段的「录 N」是**按本行数量算好的建议值**（`qty × rate`，4 位四舍五入），
/// 用户照抄就能过，不用自己拿计算器乘。
fn not_base_line_message(
    line_no: usize,
    material_code: Option<&str>,
    unit_name: &str,
    rate: Decimal,
    base_unit_name: &str,
    qty: Decimal,
) -> String {
    let suggested = trim_decimal((qty * rate).round_dp(4));
    let subject = line_subject(material_code);
    format!(
        "items[{line_no}] {subject}：单位「{unit_name}」不是基本单位：\
         本版本不支持多单位换算，请按基本单位录入\
         （1 {unit_name} = {} {base_unit_name} → 录 {suggested}）",
        trim_decimal(rate)
    )
}

/// 同 [`not_base_line_message`]，物料编码必有（建单期三个入口都查过物料）。
fn not_base_message(
    line_no: usize,
    material_code: &str,
    unit_name: &str,
    rate: Decimal,
    base_unit_name: &str,
    qty: Decimal,
) -> String {
    not_base_line_message(
        line_no,
        Some(material_code),
        unit_name,
        rate,
        base_unit_name,
        qty,
    )
}

/// 「单位行查不到」的统一报错文案（数据损坏，要人看，不是用户操作问题）。
fn unit_missing_message(line_no: usize, material_code: Option<&str>) -> String {
    let subject = match material_code {
        Some(code) => format!("物料 {code}："),
        None => String::new(),
    };
    format!(
        "items[{line_no}] {subject}单位在 biz_unit 里查不到（明细的 unit_id 指向的行已不存在）：\
         请先在「物料」主数据里重选计量单位"
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
    ensure_base_unit_in(db(), material_id, line_no, qty).await
}

/// 同 [`ensure_base_unit`]，但**吃调用方的执行器**（结算在事务内 ⇒ 与写账同一连接）。
///
/// # 异常情况
///
/// 同 [`ensure_base_unit`]。
pub async fn ensure_base_unit_in(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    line_no: usize,
    qty: Decimal,
) -> std::result::Result<(), ApiError> {
    let Some(reference) = material_unit_ref_in(ex, material_id).await? else {
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

/// **写账入口的复检闸**：按**明细自带的 `unit_id`** 判「这一行是不是基本单位」。
///
/// 与 [`ensure_base_unit`] 的分工（缺一不可）：
///
/// ```text
/// 建单 / 改单期  ensure_base_unit        —— 判「物料现在的单位」
/// 过账 / 结算期  ensure_unit_row_is_base —— 判「明细里落下来的那个单位」
/// ```
///
/// 为什么写账期必须判明细而不是物料：
///
/// 1. 单据明细把 `unit_id` **快照**下来了（`biz_stock_in_item.unit_id` 是 `NOT NULL`），
///    过账只认快照；绕过接口 `UPDATE` 明细的单位（或升级前建的折算单位草稿）在建单期
///    校验之后才出现，物料侧再怎么看都看不出问题；
/// 2. 反过来，物料建单后被改成基本单位也不代表这行的 `qty` 是按基本单位录的。
///
/// # 入参
///
/// - `ex`：执行器（事务内传 `&tx`，事务外传 `db()`）；
/// - `unit_id`：**明细行的** `unit_id`（不是物料当前的 `unit_id`）；
/// - `material_id`：明细行的物料（只用于把编码写进文案；`None` = 不点名物料）；
/// - `line_no`：明细行下标（0 起，与 `items[i]` 对齐）；
/// - `qty`：本行数量（只用于算折算建议）。
///
/// # 返回
///
/// - `Ok(())`：基本单位；
/// - `Err(400 + ERP_UNIT_NOT_BASE)`：明细单位是折算单位（文案带折算建议）；
/// - `Err(400 + ERP_REFERENCE_MISSING)`：`unit_id` 指向的单位行已不存在（数据损坏）。
///
/// # 异常情况
///
/// 数据库异常 → [`ApiError`]（`500`）。
pub async fn ensure_unit_row_is_base(
    ex: &impl SqlExecutor,
    unit_id: Uuid,
    material_id: Option<Uuid>,
    line_no: usize,
    qty: Decimal,
) -> std::result::Result<(), ApiError> {
    let Some(reference) = unit_line_ref(ex, unit_id, material_id).await? else {
        return Err(ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            unit_missing_message(line_no, None),
        ));
    };
    if let Some(unit_name) = reference.unit_name.as_deref()
        && reference.is_base != Some(1)
    {
        let base_unit_name = reference.base_unit_name.as_deref().unwrap_or("基本单位");
        return Err(ApiError::new(
            400,
            error_code::ERP_UNIT_NOT_BASE,
            not_base_line_message(
                line_no,
                reference.material_code.as_deref(),
                unit_name,
                reference.rate.unwrap_or(Decimal::ONE),
                base_unit_name,
                qty,
            ),
        ));
    }
    Ok(())
}

/// **物料主数据的单位闸**：`biz_material.unit_id` 只能指向基本单位。
///
/// # 为什么主数据也要拦
///
/// 单据链路的判据是「物料的基础单位必须是基本单位」（[`ensure_base_unit`]）。
/// 物料一旦挂在折算单位（`is_base = 0`）上，**它的任何单据都会被 400 拒**——
/// 用户看到的只是「这个物料的单建不了」，完全不知道根因在主数据。
/// 所以在建 / 改物料时就把这条路堵掉（本卡：DC-09 缺口 2）。
///
/// # 存量
///
/// 已经挂在折算单位上的历史物料会因此**不能再保存**（要先把单位改回基本单位）。
/// 生产实测 0 行，命中就出清单给业务确认，**不自动改数据**。
///
/// # 入参
///
/// - `is_base`：单位行的 `is_base`（`1` = 基本单位）；
/// - `unit_name`：单位名称（写进文案，用户一眼知道是哪个单位）。
///
/// # 返回
///
/// - `Ok(())`：基本单位；
/// - `Err(400 + ERP_UNIT_NOT_BASE)`：折算单位。
pub fn ensure_material_unit_is_base(
    is_base: i16,
    unit_name: &str,
) -> std::result::Result<(), ApiError> {
    if is_base == 1 {
        return Ok(());
    }
    Err(ApiError::new(
        400,
        error_code::ERP_UNIT_NOT_BASE,
        format!(
            "物料的计量单位必须是基本单位：折算单位（箱/打）只能用于显示，\
             本版本单据不支持多单位换算（当前「{unit_name}」）"
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

    /// ④ 明细自带单位（拿不到物料编码）时，文案退化成「明细」而不是编一个假编码。
    ///
    /// 过账 / 结算期复检走的就是这条：`biz_stock_in_item.unit_id` 说得清「哪个单位」，
    /// 物料编码只是附加信息。
    #[test]
    fn not_base_line_message_without_material_code_says_line() {
        let text = not_base_line_message(1, None, "箱", dec("12"), "个", dec("2"));
        assert_eq!(
            text,
            "items[1] 明细：单位「箱」不是基本单位：本版本不支持多单位换算，\
             请按基本单位录入（1 箱 = 12 个 → 录 24）"
        );

        // 带物料编码时与建单期口径**逐字一致**（同一个实现）
        let with_code =
            not_base_line_message(1, Some("W1B-M-003"), "箱", dec("12"), "个", dec("2"));
        assert_eq!(
            with_code,
            not_base_message(1, "W1B-M-003", "箱", dec("12"), "个", dec("2"))
        );
        assert!(with_code.starts_with("items[1] 物料 W1B-M-003：单位「箱」"));
    }

    /// ⑤ 物料主数据闸：折算单位 400 + 66030，文案点名当前单位；基本单位直接放行。
    #[test]
    fn material_unit_must_be_base() {
        assert!(ensure_material_unit_is_base(1, "个").is_ok());

        let error = ensure_material_unit_is_base(0, "箱").expect_err("折算单位必须被拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE);
        assert_eq!(
            error.msg,
            "物料的计量单位必须是基本单位：折算单位（箱/打）只能用于显示，\
             本版本单据不支持多单位换算（当前「箱」）"
        );
        // 换一个折算单位名，只有「当前」那处跟着变（不是拼死的「箱」）
        let other = ensure_material_unit_is_base(0, "打").expect_err("折算单位必须被拒");
        assert!(other.msg.ends_with("（当前「打」）"), "{}", other.msg);
    }

    /// ⑥ 单位行查不到（数据损坏）的文案：点名 biz_unit 与「重选计量单位」，别让用户以为是自己填错。
    #[test]
    fn unit_missing_message_points_at_master_data() {
        let with_code = unit_missing_message(2, Some("W1B-M-003"));
        assert!(
            with_code.starts_with("items[2] 物料 W1B-M-003：单位在 biz_unit 里查不到"),
            "{with_code}"
        );
        assert!(with_code.contains("重选计量单位"), "{with_code}");

        let plain = unit_missing_message(0, None);
        assert!(
            plain.starts_with("items[0] 单位在 biz_unit 里查不到"),
            "{plain}"
        );
    }

    /// ⑦ 折算建议保留 4 位并四舍五入：`12.123456 × 1` → `12.1235`（不是浮点尾巴一堆 0）。
    #[test]
    fn suggestion_rounds_to_four_decimals() {
        let text = not_base_message(0, "M-9", "箱", dec("12.123456"), "千克", dec("1"));
        assert!(text.contains("1 箱 = 12.123456 千克"), "{text}");
        assert!(text.ends_with("→ 录 12.1235）"), "{text}");
        assert!(!text.contains("12.12350"), "尾零必须去掉：{text}");
    }

    /// ⑧ 数量为 0 时建议值就是 0（不能出现空串或 `-0`）。
    #[test]
    fn zero_qty_suggests_zero() {
        let text = not_base_message(0, "M-1", "箱", dec("12"), "个", dec("0"));
        assert!(text.ends_with("→ 录 0）"), "{text}");
        assert!(!text.contains("-0"), "{text}");
    }

    /// ⑨ 建议值是「按本行数量」算的：同一个折算率，2 箱 → 24、5 箱 → 60。
    #[test]
    fn suggestion_is_per_line_not_shared() {
        let two = not_base_message(0, "M-1", "箱", dec("12"), "个", dec("2"));
        let five = not_base_message(1, "M-1", "箱", dec("12"), "个", dec("5"));
        assert!(two.ends_with("→ 录 24）"), "{two}");
        assert!(five.ends_with("→ 录 60）"), "{five}");
        assert!(five.starts_with("items[1] "), "行号跟着行走：{five}");
    }

    /// ⑩ 行号原样回显（含两位下标），调用方按 `items[i]` 定位不用再算。
    #[test]
    fn line_no_is_echoed_verbatim() {
        for line_no in [0usize, 9, 12, 999] {
            let text = not_base_message(line_no, "M-1", "箱", dec("12"), "个", dec("1"));
            assert!(
                text.starts_with(&format!("items[{line_no}] 物料 M-1：")),
                "{text}"
            );
        }
    }

    /// ⑪ 物料主数据闸：只有 `is_base == 1` 放行，其它值（0 / 2 / -1 / 异常码）一律拒。
    #[test]
    fn material_unit_gate_accepts_only_is_base_one() {
        assert!(ensure_material_unit_is_base(1, "个").is_ok());
        for is_base in [0i16, 2, -1, 9] {
            let error = match ensure_material_unit_is_base(is_base, "箱") {
                Ok(()) => panic!("is_base={is_base} 必须被拒"),
                Err(error) => error,
            };
            assert_eq!(error.status, 400);
            assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE);
            assert!(error.msg.contains("「箱」"), "{}", error.msg);
        }
    }

    /// ⑫ `trim_decimal` 的性能边界：大数不丢精度、负数与整数值不出现 `.0` / `-0`。
    #[test]
    fn trim_decimal_handles_large_negative_and_integer_values() {
        assert_eq!(
            trim_decimal(dec("1234567890123.120000")),
            "1234567890123.12"
        );
        assert_eq!(trim_decimal(dec("12000006.000000")), "12000006");
        assert_eq!(trim_decimal(dec("-1.500000")), "-1.5");
        assert_eq!(trim_decimal(dec("0.000000")), "0");
        assert_eq!(
            trim_decimal(dec("1.000001")),
            "1.000001",
            "有效小数不许被裁掉"
        );
    }

    /// ⑬ 单位查不到文案的行号与主体：给了物料编码点名物料，没给就只说「单位」。
    #[test]
    fn unit_missing_message_respects_line_and_subject() {
        let with_code = unit_missing_message(7, Some("M-7"));
        assert!(
            with_code.starts_with("items[7] 物料 M-7：单位在 biz_unit 里查不到"),
            "{with_code}"
        );
        let plain = unit_missing_message(7, None);
        assert!(
            plain.starts_with("items[7] 单位在 biz_unit 里查不到"),
            "{plain}"
        );
        assert!(
            !plain.contains("物料 "),
            "没有编码时不许编一个物料出来：{plain}"
        );
    }

    /// ⑭ 折算率恰好 1 时也照原文渲染（`1 个 = 1 个`），不特判、不改口径。
    #[test]
    fn rate_one_is_rendered_as_is() {
        let text = not_base_message(0, "M-1", "个", dec("1"), "个", dec("7"));
        assert!(text.contains("（1 个 = 1 个 → 录 7）"), "{text}");
        assert_eq!(
            text,
            not_base_line_message(0, Some("M-1"), "个", dec("1"), "个", dec("7")),
            "两条实现必须逐字一致"
        );
    }

    /// ⑮ 大数量 × 大折算率仍然精确（不失精度、不出科学计数法）。
    #[test]
    fn large_qty_times_rate_stays_exact() {
        let text = not_base_message(0, "M-1", "吨", dec("12"), "千克", dec("1000000.5"));
        assert!(text.ends_with("→ 录 12000006）"), "{text}");
        // 只看建议值那一段：Decimal → String 不许退化成科学计数法
        let suggested = text
            .rsplit_once("→ 录 ")
            .map(|(_, tail)| tail.trim_end_matches('）').to_string())
            .expect("文案必须带建议值");
        assert_eq!(suggested, "12000006", "建议值必须是纯整数：{suggested}");
        assert!(
            suggested.chars().all(|ch| ch.is_ascii_digit()),
            "建议值不许出现 e/E 等非数字字符：{suggested}"
        );
    }

    /// ⑯ 小数量（< 1）× 折算率：0.25 箱 × 4 = 1，建议值取整后仍带 0 位小数。
    #[test]
    fn fractional_qty_rounds_to_integer_when_exact() {
        let text = not_base_message(0, "M-1", "箱", dec("4"), "个", dec("0.25"));
        assert!(text.ends_with("→ 录 1）"), "{text}");
        let text = not_base_message(0, "M-1", "箱", dec("4"), "个", dec("0.375"));
        assert!(text.ends_with("→ 录 1.5）"), "{text}");
    }
}
