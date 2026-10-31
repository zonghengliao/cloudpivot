//! 单品序列号接口契约（批次 D · W2b）：批量生成 / 分页筛选 / 扫码 / 状态流转 的出入参。
//!
//! 与 [`crate::business::erp::dto`] 保持同一套约定：
//! 主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 时间戳 `DateTime<Utc>`（JSON RFC3339）、枚举一律大写字符串且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 与卡面的差异（逐条）
//!
//! 1. 批量生成出参：卡面只写「返回生成的号列表」，这里返回
//!    `{ count, period_key, serial_nos, list }` —— `serial_nos` 给扫码枪 / 贴标机直接用，
//!    `list` 是回读的整行（前端列表要 `id` 才有地方点「状态流转」）。
//! 2. 状态流转入参的 `reason` 只做长度校验、**不落库**：V0138 定稿的 16 列没有
//!    `reason` / `remark`，红线⑤禁止改表结构。字段保留是为了前端契约稳定，
//!    将来加列时前端不用改。
//! 3. `source_type` 白名单取 V0138 列注释的 4 个值（`PURCHASE` / `TRANSFER` /
//!    `PRODUCE` / `RETURN`），落库前显式校验：写错类型会让「按来源追溯」查不到东西。

use std::str::FromStr;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::serial_model::{
    ALL_STATUSES, MAX_BATCH_COUNT, SOURCE_TYPES, SerialRow, SerialScanRow,
};
use crate::business::erp::stockin_model;

/// `unit_cost` 的入参形态：**number 与字符串都收**（前端 `t-input-number` 给 number、
/// 手写 curl / 导入模板给字符串都能过）。
///
/// 为什么不用裸 `Decimal`：裸字段缺省时 axum 直接把缺字段拒成 **422**
/// （`Failed to deserialize the JSON body … missing field`），前端拿不到「400 + 10001」，
/// 就没法按业务码出提示。用 `Option<OptionalDecimal>` 自己判，才能把「没传 / 传了非数字 /
/// 传了 ≤ 0」三种情况分别给出 §2.3 的逐字报文。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OptionalDecimal {
    /// JSON number（`12.34`）。
    Num(Decimal),
    /// JSON 字符串（`"12.34"`），交给 `Decimal::from_str` 判合法性。
    Raw(String),
}

impl OptionalDecimal {
    /// 转成 `Decimal`；非数字返回 `Err(原始文本)`（供调用方拼 §2.3 的报文）。
    pub fn to_decimal(&self) -> std::result::Result<Decimal, String> {
        match self {
            Self::Num(value) => Ok(*value),
            Self::Raw(text) => Decimal::from_str(text.trim()).map_err(|_| text.trim().to_string()),
        }
    }
}

/// 可记入成本账的单价上限（`numeric(18,6)` 能放下，但再大就是录错了）。
///
/// 与规格 v3 §2.2 一致：`unit_cost >= 1e11` 直接 400 —— 一个 6 位小数的
/// `1e11` 单价乘 1000 件就溢出 `numeric(18,4)` 的整数部分（14 位），
/// 让它过就等于把溢出错误留给数据库（500 而不是 400）。
pub const MAX_UNIT_COST: i64 = 100_000_000_000;

/// 批量生成序列号。
#[derive(Debug, Clone, Deserialize)]
pub struct BatchGenerateReq {
    /// 物料 id（**必须** `serial_required = TRUE`，否则 400）。
    pub material_id: String,
    /// 仓库 id（可空）。
    #[serde(default)]
    pub warehouse_id: Option<String>,
    /// 库位库存行 id（可空；指向 `biz_inventory_location.id`）。
    #[serde(default)]
    pub location_id: Option<String>,
    /// 批次号（可空，≤64）。
    #[serde(default)]
    pub batch_no: Option<String>,
    /// 生成条数（1..=1000）。
    pub count: u64,
    /// **成本单价（不含税，必填，> 0，最多 6 位小数）**。
    ///
    /// 批量生成自 ADR-20261031 起等于「无来源入库（期初 / 其他入库）」：必须录成本，
    /// 否则成本账会留下 `qty>0 / avg_cost=0` 的行（巡检 C3），而且不写库位账时
    /// B1（BLOCKER）必红 —— 这条校验就是那次事故的闸门。**不设默认 0**：
    /// 缺省 0 等于把缺陷原样保留。
    #[serde(default)]
    pub unit_cost: Option<OptionalDecimal>,
    /// 来源单据类型（可空；`PURCHASE` / `TRANSFER` / `PRODUCE` / `RETURN`）。
    #[serde(default)]
    pub source_type: Option<String>,
    /// 来源单据 id（可空）。
    #[serde(default)]
    pub source_id: Option<String>,
}

/// 默认每页条数（与 `erp::dto` 一致）。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 每页条数硬上限（序列号是「一物一码」，条数比主数据多一个量级，上限放到 500）。
pub const MAX_PAGE_SIZE: u64 = 500;

/// `reason` 的最大长度（超出即 400；不落库，只进日志）。
pub const MAX_REASON_LEN: usize = 200;

/// 可写的目标状态（`IN_STOCK` 不是流转目标：新增一律走批量生成）。
pub const WRITABLE_TARGETS: [&str; 2] = ["OUTBOUND", "SCRAPPED"];

// ==================== ① 批量生成 ====================

impl BatchGenerateReq {
    /// 校验入参（`400 + 10001` / `400 + 66006`），**不碰数据库**。
    ///
    /// UUID 的「格式对不对」也在这里判：非法 UUID 必须 400，
    /// 不能靠后续查询「查不到」来兜（那会变成 404，误导前端以为资源不存在）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_uuid(&self.material_id, "material_id")?;
        check_optional_uuid(self.warehouse_id.as_deref(), "warehouse_id")?;
        check_optional_uuid(self.location_id.as_deref(), "location_id")?;
        check_optional_uuid(self.source_id.as_deref(), "source_id")?;
        if self.count == 0 {
            return Err(ApiError::new(400, error_code_param(), "count 必须 ≥ 1"));
        }
        if self.count > MAX_BATCH_COUNT as u64 {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!("单次最多生成 {MAX_BATCH_COUNT} 条（收到 {}）", self.count),
            ));
        }
        // 成本单价（ADR-20261031 §3 / 规格 v3 §2.2）：**必填、> 0、< 1e11**。
        // 位置刻意放在 `count` 之后、`batch_no` 之前：条数与单价的报文都是「怎么录」的引导，
        // 排在批次号 / 来源类型这些可选字段前面，前端一次就能把两个必填项都提示出来。
        self.parse_unit_cost()?;
        if let Some(batch_no) = trimmed(self.batch_no.as_deref())
            && batch_no.chars().count() > 64
        {
            return Err(ApiError::new(
                400,
                error_code_param(),
                "batch_no 最长 64 个字符",
            ));
        }
        if let Some(source_type) = trimmed(self.source_type.as_deref())
            && !SOURCE_TYPES.contains(&source_type)
        {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!(
                    "source_type 只能是 {} 之一（收到 {source_type}）",
                    SOURCE_TYPES.join(" / ")
                ),
            ));
        }
        Ok(())
    }

    /// 解析并校验成本单价（**必填、> 0、6 位小数、< 1e11**），返回 6 位四舍五入后的值。
    ///
    /// 四舍五入口径与 `stockin_model::unit_price` 完全一致（`MidpointAwayFromZero`，
    /// 与 PG `ROUND(numeric,6)` 同源）—— 落库的 `unit_cost` 与「件数 × 单价」算出来的金额
    /// 必须是同一份舍入结果，否则成本账与流水会差 1 分钱。
    ///
    /// # 报文（前端 `serial-logic.ts` 逐字引用，规格 v3 §2.3）
    ///
    /// | 场景 | 报文 |
    /// |---|---|
    /// | 没传 | `unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）` |
    /// | 非数字 | `unit_cost 不是合法数字：「abc」` |
    /// | `≤ 0` | `unit_cost 必须 > 0（收到 0）：零成本入库会在成本账留下 qty>0/avg_cost=0 的行（巡检 C3），系统拒收` |
    /// | `≥ 1e11` | `unit_cost 超出可记账范围（须 < 100000000000）：100000000000` |
    ///
    /// 注意「收到」后面给的是**6 位舍入之后**的值：`0.0000001` 这类录不进
    /// `numeric(18,6)` 的单价会显示成 `收到 0`，这比让数据库去截断更诚实。
    pub fn parse_unit_cost(&self) -> std::result::Result<Decimal, ApiError> {
        let raw = self.unit_cost.as_ref().ok_or_else(|| {
            ApiError::new(
                400,
                error_code_param(),
                "unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）",
            )
        })?;
        let value = raw.to_decimal().map_err(|text| {
            ApiError::new(
                400,
                error_code_param(),
                format!("unit_cost 不是合法数字：「{text}」"),
            )
        })?;
        let unit_cost = stockin_model::unit_price(value);
        if unit_cost <= Decimal::ZERO {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!(
                    "unit_cost 必须 > 0（收到 {unit_cost}）：零成本入库会在成本账留下 \
                     qty>0/avg_cost=0 的行（巡检 C3），系统拒收"
                ),
            ));
        }
        if unit_cost >= Decimal::from(MAX_UNIT_COST) {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!("unit_cost 超出可记账范围（须 < {MAX_UNIT_COST}）：{unit_cost}"),
            ));
        }
        // 定标到 6 位（`round_dp` 只舍入、不补零：`Decimal::from(10)` 会保持 0 位小数）。
        // 不定标的话响应体给前端的是 `"10"`，与规格 §2.5 的 `"12.340000"` 契约不一致，
        // 前端「本次入库金额」预览与库里的 `numeric(18,6)` 会对不上。
        // 注意 `Decimal::rescale` 是**原地**方法（返回 `()`）。
        let mut unit_cost = unit_cost;
        unit_cost.rescale(6);
        Ok(unit_cost)
    }
}

/// 批量生成出参。
#[derive(Debug, Clone, Serialize)]
pub struct SerialBatchRes {
    /// 实际生成条数。
    pub count: usize,
    /// 本次号段所属的周期键（`yyyyMMdd`，北京时间）。
    pub period_key: String,
    /// 号列表（按号升序，可直接打印/贴标）。
    pub serial_nos: Vec<String>,
    /// 回读的整行（与 `serial_nos` 一一对应）。
    pub list: Vec<SerialRes>,
    /// 本次成本单价（不含税，6 位）—— `Decimal` 序列化成**字符串**（`"12.340000"`）。
    pub unit_cost: Decimal,
    /// 本次入账金额 = 该成本桶 `amount` 的真实增量（4 位，`"61.7000"`）。
    ///
    /// **不是** Rust 里算的 `N × unit_cost`：它取自 `apply_cost` 的
    /// `RETURNING amount`（`after − before`），与账上、凭证上的是同一个数。
    pub amount: Decimal,
    /// 记账后该成本桶的快照（`apply_cost` 的 after）。
    pub inventory: BatchInventoryRes,
}

/// 记账后成本桶快照（金额单位与库表一致：qty 4 位 / amount 4 位 / avg_cost 6 位）。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct BatchInventoryRes {
    /// 结存数量（`biz_inventory.qty numeric(18,4)` → 字符串）。
    pub qty: Decimal,
    /// 结存金额（`biz_inventory.amount numeric(18,4)` → 字符串）。
    pub amount: Decimal,
    /// 结存移动加权单价（`biz_inventory.avg_cost numeric(18,6)` → 字符串）。
    pub avg_cost: Decimal,
}

impl SerialBatchRes {
    /// 由数据层的行集合 + 本次入账口径组装。
    ///
    /// 「回显金额」是 ADR 验收判据 5 的要求（提交成功后页面显示的库存数量 / 金额与库一致），
    /// 也是操作人当场知道「这次入账多少钱」的唯一途径 —— 会计凭据不该让人再点一次查询。
    pub fn new(
        period_key: &str,
        rows: Vec<SerialRow>,
        unit_cost: Decimal,
        amount: Decimal,
        inventory: (Decimal, Decimal, Decimal),
    ) -> Self {
        let serial_nos = rows.iter().map(|row| row.serial_no.clone()).collect();
        let count = rows.len();
        let (qty, amount_after, avg_cost) = inventory;
        Self {
            count,
            period_key: period_key.to_string(),
            serial_nos,
            list: rows.iter().map(SerialRes::from).collect(),
            unit_cost,
            amount,
            inventory: BatchInventoryRes {
                qty,
                amount: amount_after,
                avg_cost,
            },
        }
    }
}

// ==================== ② 分页筛选 ====================

/// 序列号分页筛选。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SerialFilter {
    /// 物料 id。
    pub material_id: Option<String>,
    /// 仓库 id。
    pub warehouse_id: Option<String>,
    /// 库位库存行 id。
    pub location_id: Option<String>,
    /// 状态（`IN_STOCK` / `OUTBOUND` / `SCRAPPED`）。
    pub status: Option<String>,
    /// 序列号（模糊匹配，扫码枪输入半截号也能搜）。
    pub serial_no: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for SerialFilter {
    fn default() -> Self {
        Self {
            material_id: None,
            warehouse_id: None,
            location_id: None,
            status: None,
            serial_no: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

impl SerialFilter {
    /// 归一化并校验（非法 UUID / 非法 status → `400`，绝不静默忽略条件）。
    ///
    /// 三个 id 筛选都在这里先判 UUID 格式：把非法 UUID 放给数据层会在 SQL 绑定阶段
    /// 变 `500`（`DbError::InvalidValue` 被翻译成「服务器内部错误」），
    /// 而它本质上是**调用方参数错**，必须 400 + `66006`。
    pub fn validated(self) -> std::result::Result<Self, ApiError> {
        check_optional_uuid(self.material_id.as_deref(), "material_id")?;
        check_optional_uuid(self.warehouse_id.as_deref(), "warehouse_id")?;
        check_optional_uuid(self.location_id.as_deref(), "location_id")?;
        if let Some(status) = trimmed(self.status.as_deref())
            && !ALL_STATUSES.contains(&status)
        {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!(
                    "status 只能是 {} 之一（收到 {status}）",
                    ALL_STATUSES.join(" / ")
                ),
            ));
        }
        Ok(self)
    }
}

// ==================== ③ 状态流转 ====================

/// 状态流转入参。
#[derive(Debug, Clone, Deserialize)]
pub struct SerialStatusReq {
    /// 目标状态：只接受 `OUTBOUND` / `SCRAPPED`。
    pub status: String,
    /// 原因（≤200 字；**不落库**，只进日志留痕）。
    #[serde(default)]
    pub reason: Option<String>,
}

impl SerialStatusReq {
    /// 校验并返回目标状态（`IN_STOCK` 不是合法目标，`400 + 66008`）。
    pub fn target_status(&self) -> std::result::Result<&str, ApiError> {
        let status = self.status.trim();
        if !WRITABLE_TARGETS.contains(&status) {
            let msg = if ALL_STATUSES.contains(&status) {
                format!(
                    "状态流转的目标只能是 OUTBOUND / SCRAPPED（收到 {status}）：\
                     IN_STOCK 只能由批量生成产生，出库后要重新入库请生成新的序列号"
                )
            } else {
                format!("status 只能是 OUTBOUND / SCRAPPED（收到 {status}）")
            };
            return Err(ApiError::new(400, error_code_status(), msg));
        }
        if let Some(reason) = self.reason.as_deref()
            && reason.trim().chars().count() > MAX_REASON_LEN
        {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!("reason 最长 {MAX_REASON_LEN} 个字符"),
            ));
        }
        Ok(status)
    }
}

// ==================== 出参 ====================

/// 序列号详情 / 列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct SerialRes {
    /// 主键。
    pub id: String,
    /// 序列号。
    pub serial_no: String,
    /// 物料 id。
    pub material_id: String,
    /// 仓库 id。
    pub warehouse_id: Option<String>,
    /// 库位库存行 id。
    pub location_id: Option<String>,
    /// 批次号。
    pub batch_no: Option<String>,
    /// 状态。
    pub status: String,
    /// 入库时间。
    pub inbound_at: Option<DateTime<Utc>>,
    /// 出库 / 报废时间。
    pub outbound_at: Option<DateTime<Utc>>,
    /// 来源单据类型。
    pub source_type: Option<String>,
    /// 来源单据 id。
    pub source_id: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&SerialRow> for SerialRes {
    fn from(row: &SerialRow) -> Self {
        Self {
            id: row.id.to_string(),
            serial_no: row.serial_no.clone(),
            material_id: row.material_id.to_string(),
            warehouse_id: row.warehouse_id.map(|id| id.to_string()),
            location_id: row.location_id.map(|id| id.to_string()),
            batch_no: row.batch_no.clone(),
            status: row.status.clone(),
            inbound_at: row.inbound_at,
            outbound_at: row.outbound_at,
            source_type: row.source_type.clone(),
            source_id: row.source_id.map(|id| id.to_string()),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 扫码出参（卡面 §二 ③ 的 10 个字段逐条对应）。
#[derive(Debug, Clone, Serialize)]
pub struct SerialScanRes {
    /// 主键。
    pub id: String,
    /// 序列号。
    pub serial_no: String,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库 id。
    pub warehouse_id: Option<String>,
    /// 仓库名称。
    pub warehouse_name: Option<String>,
    /// 库位库存行 id。
    pub location_id: Option<String>,
    /// 库位名称。
    pub location_name: Option<String>,
    /// 批次号。
    pub batch_no: Option<String>,
    /// 状态。
    pub status: String,
    /// 入库时间。
    pub inbound_at: Option<DateTime<Utc>>,
    /// 出库 / 报废时间。
    pub outbound_at: Option<DateTime<Utc>>,
}

impl From<&SerialScanRow> for SerialScanRes {
    fn from(row: &SerialScanRow) -> Self {
        Self {
            id: row.id.to_string(),
            serial_no: row.serial_no.clone(),
            material_id: row.material_id.to_string(),
            material_code: row.material_code.clone(),
            material_name: row.material_name.clone(),
            warehouse_id: row.warehouse_id.map(|id| id.to_string()),
            warehouse_name: row.warehouse_name.clone(),
            location_id: row.location_id.map(|id| id.to_string()),
            location_name: row.location_name.clone(),
            batch_no: row.batch_no.clone(),
            status: row.status.clone(),
            inbound_at: row.inbound_at,
            outbound_at: row.outbound_at,
        }
    }
}

// ==================== 内部辅助 ====================

/// 参数错误码（`10001`，与其余模块一致）。
fn error_code_param() -> i32 {
    crate::shared::error_code::PARAM_INVALID
}

/// 状态流转错误码（`66008`）。
fn error_code_status() -> i32 {
    crate::shared::error_code::ERP_SERIAL_STATUS_INVALID
}

/// 去空白 + 空串归一成 `None`。
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

/// 必填 UUID 字段校验。
fn require_uuid(value: &str, field: &str) -> std::result::Result<(), ApiError> {
    let text = value.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code_param(),
            format!("{field} 不能为空"),
        ));
    }
    if nik::uuid::Uuid::parse_str(text).is_err() {
        return Err(ApiError::new(
            400,
            crate::shared::error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        ));
    }
    Ok(())
}

/// 可空 UUID 字段校验（空串 / 缺席都放过）。
fn check_optional_uuid(value: Option<&str>, field: &str) -> std::result::Result<(), ApiError> {
    if let Some(text) = trimmed(value)
        && nik::uuid::Uuid::parse_str(text).is_err()
    {
        return Err(ApiError::new(
            400,
            crate::shared::error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch_req(count: u64) -> BatchGenerateReq {
        BatchGenerateReq {
            material_id: "b2b00004-0000-4000-8000-000000000001".to_string(),
            warehouse_id: None,
            location_id: None,
            batch_no: None,
            count,
            unit_cost: Some(OptionalDecimal::Num(Decimal::from(10))),
            source_type: None,
            source_id: None,
        }
    }

    /// ⑰ 成本单价：**缺 / 非数字 / ≤ 0 / ≥ 1e11 各自 400 + 10001，报文与规格 §2.3 逐字一致**。
    ///
    /// 这是 2026-10-31 11:34 prod 事故（零成本入库 → B1 BLOCKER + C3 WARN）的第一道闸门，
    /// 所以文案要按「前端直接显示」的标准钉死，改动任何一处都会打到前端单测。
    #[test]
    fn batch_unit_cost_validation_messages() {
        let missing = batch_req(1);
        let mut req = missing;
        req.unit_cost = None;
        let err = req.validate().expect_err("缺 unit_cost 必须 400");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code_param());
        assert_eq!(
            err.msg,
            "unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）"
        );

        let mut req = batch_req(1);
        req.unit_cost = Some(OptionalDecimal::Raw("abc".to_string()));
        let err = req.validate().expect_err("非数字必须 400");
        assert_eq!(err.code, error_code_param());
        assert_eq!(err.msg, "unit_cost 不是合法数字：「abc」");

        for (input, shown) in [
            (Decimal::ZERO, "0"),
            (Decimal::from_str("-1.5").unwrap(), "-1.5"),
        ] {
            let mut req = batch_req(1);
            req.unit_cost = Some(OptionalDecimal::Num(input));
            let err = req.validate().expect_err("≤0 必须 400");
            assert_eq!(err.code, error_code_param());
            assert_eq!(
                err.msg,
                format!(
                    "unit_cost 必须 > 0（收到 {shown}）：零成本入库会在成本账留下 \
                     qty>0/avg_cost=0 的行（巡检 C3），系统拒收"
                )
            );
        }

        let mut req = batch_req(1);
        req.unit_cost = Some(OptionalDecimal::Num(Decimal::from(MAX_UNIT_COST)));
        let err = req.validate().expect_err("≥1e11 必须 400");
        assert_eq!(err.code, error_code_param());
        assert_eq!(
            err.msg,
            "unit_cost 超出可记账范围（须 < 100000000000）：100000000000"
        );

        // 字符串形态（"12.34"）与 number 形态等价，且 6 位四舍五入（与 PG ROUND 同口径）
        let mut req = batch_req(5);
        req.unit_cost = Some(OptionalDecimal::Raw("12.3456789".to_string()));
        assert_eq!(
            req.parse_unit_cost().expect("合法单价"),
            Decimal::from_str("12.345679").unwrap(),
            "6 位四舍五入（MidpointAwayFromZero）"
        );
        let mut req = batch_req(5);
        req.unit_cost = Some(OptionalDecimal::Num(
            Decimal::from_str("0.0000004").unwrap(),
        ));
        assert!(
            req.parse_unit_cost().is_err(),
            "6 位下舍成 0 的单价必须拒（否则等于零成本入库）"
        );
    }

    /// ⑱ `unit_cost` 同时接受 number 与字符串（`#[serde(untagged)]` 的两种形态）。
    #[test]
    fn batch_unit_cost_accepts_number_and_string() {
        let base = serde_json::json!({
            "material_id": "b2b00004-0000-4000-8000-000000000001",
            "count": 2
        });
        for raw in [serde_json::json!(12.34), serde_json::json!("12.34")] {
            let mut body = base.clone();
            body["unit_cost"] = raw.clone();
            let req: BatchGenerateReq =
                serde_json::from_value(body).unwrap_or_else(|e| panic!("{raw} 解析失败：{e}"));
            assert_eq!(
                req.parse_unit_cost().expect("合法"),
                Decimal::from_str("12.34").unwrap()
            );
        }
        let req: BatchGenerateReq = serde_json::from_value(base).expect("缺字段也要能反序列化");
        assert!(
            req.unit_cost.is_none(),
            "缺字段 = None（交给 validate 出 400，而不是 422）"
        );
    }

    /// ⑩ 批量生成条数边界：1 与 1000 放行，0 与 1001 拒绝（文案给出上限）。
    #[test]
    fn batch_count_bounds() {
        assert!(batch_req(1).validate().is_ok());
        assert!(batch_req(MAX_BATCH_COUNT as u64).validate().is_ok());
        let zero = batch_req(0).validate().expect_err("0 条必须拒");
        assert_eq!(zero.status, 400);
        assert!(zero.msg.contains("≥ 1"), "{}", zero.msg);
        let too_many = batch_req(MAX_BATCH_COUNT as u64 + 1)
            .validate()
            .expect_err("超上限必须拒");
        assert_eq!(too_many.status, 400);
        assert!(too_many.msg.contains("1000"), "{}", too_many.msg);
    }

    /// ⑪ material_id 必填且必须是 UUID；空串与乱码都 400。
    #[test]
    fn batch_requires_valid_material_uuid() {
        let mut req = batch_req(1);
        req.material_id = "  ".to_string();
        let err = req.validate().expect_err("空 material_id 必须拒");
        assert!(err.msg.contains("不能为空"), "{}", err.msg);

        let mut req = batch_req(1);
        req.material_id = "not-a-uuid".to_string();
        let err = req.validate().expect_err("非法 UUID 必须拒");
        assert_eq!(err.code, crate::shared::error_code::ERP_FILTER_INVALID);
        assert!(err.msg.contains("不是合法的 UUID"), "{}", err.msg);

        let mut req = batch_req(1);
        req.warehouse_id = Some("bad".to_string());
        assert!(req.validate().is_err(), "可空字段传了非法 UUID 也要拒");
    }

    /// ⑫ source_type 白名单：4 个枚举值放行，写错 400（写错会让来源追溯查不到）。
    #[test]
    fn batch_source_type_whitelist() {
        for kind in SOURCE_TYPES {
            let mut req = batch_req(1);
            req.source_type = Some(kind.to_string());
            assert!(req.validate().is_ok(), "{kind} 应放行");
        }
        let mut req = batch_req(1);
        req.source_type = Some("GIFT".to_string());
        let err = req.validate().expect_err("白名单外必须拒");
        assert!(err.msg.contains("source_type"), "{}", err.msg);
    }

    /// ⑬ 状态流转：只接受 OUTBOUND / SCRAPPED；IN_STOCK 的拒绝文案要指引「生成新号」。
    #[test]
    fn status_target_only_accepts_writable_states() {
        let outbound = SerialStatusReq {
            status: "OUTBOUND".to_string(),
            reason: None,
        };
        assert_eq!(outbound.target_status().expect("OUTBOUND 合法"), "OUTBOUND");
        let scrapped = SerialStatusReq {
            status: " SCRAPPED ".to_string(),
            reason: Some("外壳破损".to_string()),
        };
        assert_eq!(scrapped.target_status().expect("SCRAPPED 合法"), "SCRAPPED");

        let in_stock = SerialStatusReq {
            status: "IN_STOCK".to_string(),
            reason: None,
        };
        let err = in_stock.target_status().expect_err("IN_STOCK 不是合法目标");
        assert_eq!(err.status, 400);
        assert_eq!(
            err.code,
            crate::shared::error_code::ERP_SERIAL_STATUS_INVALID
        );
        assert!(err.msg.contains("新的序列号"), "{}", err.msg);

        let bogus = SerialStatusReq {
            status: "SCRAP".to_string(),
            reason: None,
        };
        assert!(bogus.target_status().is_err(), "错别字状态也必须拒");
    }

    /// ⑭ reason 超长 400（不落库，但入参仍要收口）。
    #[test]
    fn status_reason_length_is_limited() {
        let long = SerialStatusReq {
            status: "SCRAPPED".to_string(),
            reason: Some("报".repeat(MAX_REASON_LEN + 1)),
        };
        let err = long.target_status().expect_err("超长 reason 必须拒");
        assert!(err.msg.contains("reason"), "{}", err.msg);
    }

    /// ⑮ 分页筛选默认值：第 1 页 / 20 条 / 无条件。
    #[test]
    fn filter_defaults_match_module_convention() {
        let filter = SerialFilter::default();
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, DEFAULT_PAGE_SIZE);
        assert!(filter.material_id.is_none() && filter.status.is_none());
    }

    /// ⑯ 分页筛选的 status 白名单（非法值 400，不静默忽略）。
    #[test]
    fn filter_rejects_unknown_status() {
        let filter = SerialFilter {
            status: Some("INVALID".to_string()),
            ..SerialFilter::default()
        };
        let err = filter.validated().expect_err("非法 status 必须拒");
        assert_eq!(err.status, 400);

        let filter = SerialFilter {
            status: Some("IN_STOCK".to_string()),
            ..SerialFilter::default()
        };
        assert!(filter.validated().is_ok());
    }

    /// ⑯b 分页筛选的 id 参数：非法 UUID 必须 400 + 66006（不许被数据层翻成 500）。
    #[test]
    fn filter_rejects_bad_uuid() {
        let filter = SerialFilter {
            material_id: Some("not-a-uuid".to_string()),
            ..SerialFilter::default()
        };
        let err = filter.validated().expect_err("非法 UUID 必须拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, crate::shared::error_code::ERP_FILTER_INVALID);

        let filter = SerialFilter {
            warehouse_id: Some("  ".to_string()),
            material_id: Some("b2b00004-0000-4000-8000-000000000001".to_string()),
            ..SerialFilter::default()
        };
        assert!(filter.validated().is_ok(), "空串 = 未传，不算错");
    }
}
