//! 采购计划 → 采购申请转换契约（M4-03）：按计划生成申请。
//!
//! 与 [`crate::business::erp::purchase::purchase_plan_dto`] /
//! [`crate::business::erp::purchase::purchase_requisition_dto`] 同一套约定：
//! 主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 金额/数量 `Decimal`（**JSON 里是字符串**）、日期 `YYYY-MM-DD`。
//!
//! # 本卡的口径（每条都是纯函数 + 单测钉住，handler 里不许随手写）
//!
//! ```text
//! ① 只有 APPROVED 的计划能转（DRAFT 没批、SUBMITTED 还在批 —— 状态闸在 handler，
//!    这里不判，因为纯函数不该去查库）。
//! ② **一仓一单**：申请单的单头只有一个 `warehouse_id`（收货仓库是必填），
//!    而计划的粒度是「物料 × 仓库」⇒ 一次转换按仓库分组，N 个仓库生成 N 张申请单。
//!    不按仓库拆的话，只能取「第一个仓库」当单头，剩下两种仓库的需求全部记错仓。
//! ③ 转多少：缺省转「剩余量 = 计划量 − 已执行量」；可以逐行指定更小的量（分批转）。
//!    **超过剩余量一律 400**（不是静默截断、也不是库层 500）：计划是基准，
//!    临时加量请直接新建采购申请单 —— 否则执行率报表永远 >100%，计划就白编了。
//! ④ 单价：缺省取计划的 `planned_price`；计划行没有单价（= 0）时**必须**在请求里
//!    给 `items[].est_price` 覆盖，否则 400。为什么不默认填 0：
//!    申请单的 `est_price` 是预算判定的输入，0 元单价等于「不占预算」，
//!    会让「超预算加签」这条内控被一行 0 元明细绕过（与采购申请建单同一口径）。
//! ⑤ 已执行量回写 = 本次转出的数量（同一事务内 `executed_qty += qty`）；
//!    这样「同一行被转两次」在剩余量上自然露头，不靠额外的台账表。
//! ```
//!
//! # 幂等
//!
//! 不是「同参数第二次返回同一张单」那种幂等 —— 转换是**有业务后果**的动作
//! （生成申请单、推进已执行量）。这里的幂等是：
//! * 全量转完之后再调一次 → 409「剩余可转量都是 0」，不会造出空单；
//! * 并发两次转换 → 计划单头 + 明细行锁（`SELECT … FOR UPDATE`）串行化，
//!   第二个请求看到的是已回写的 `executed_qty`，超出剩余量就 400；
//! * 事务中途失败 → 申请单与已执行量一起回滚（不会出现「单造了、量没记」）。

use nik::prelude::chrono::{Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::purchase::purchase_plan_dto::PlanRowRes;
use crate::business::erp::purchase::purchase_plan_model::PlanItemRow;

// ==================== 域常量 ====================

/// 一次转换最多处理多少行（= 计划明细上限，见 `purchase_plan_dto::MAX_LINES`）。
pub const MAX_LINES: usize = 500;
/// 备注上限（与 `biz_purchase_requisition.remark` 的口径一致）。
pub const MAX_REMARK_LEN: usize = 500;
/// 数量标度（与申请明细 `qty NUMERIC(18,4)` 一致）。
pub const QTY_SCALE: u32 = 4;
/// 单价格标度（与申请明细 `est_price NUMERIC(18,6)` 一致）。
pub const PRICE_SCALE: u32 = 6;
/// 税率标度（与申请明细 `tax_rate NUMERIC(9,4)` 一致）。
pub const TAX_SCALE: u32 = 4;

/// 今天（北京时间）的日期。
pub fn today_cst() -> NaiveDate {
    (Utc::now() + Duration::hours(8)).date_naive()
}

// ==================== 入参 ====================

/// 采购计划 → 采购申请。
///
/// `items` 留空 = 把**全部剩余量**转成申请；给了就按给的行转（可以分批）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ToRequisitionReq {
    /// 申请部门：缺省取计划的编制部门；公司级计划（没有编制部门）必须显式给。
    pub dept_id: Option<String>,
    /// 申请人：缺省 = 当前登录人（姓名服务端快照）。
    pub applicant_id: Option<String>,
    /// 申请日期：缺省 = 今天（北京时间）。
    pub req_date: Option<NaiveDate>,
    /// 需求日期（整单口径）：缺省空。
    pub demand_date: Option<NaiveDate>,
    /// 税率（百分数 0–100）：缺省 0。计划不记税率，整单统一取这个值。
    pub tax_rate: Option<Decimal>,
    /// 备注：会追加在「由采购计划 PPL-… 转入」之后。
    pub remark: Option<String>,
    /// 只转这些行（`plan_item_id` + 可选数量 / 单价覆盖）；留空 = 全部剩余量。
    pub items: Vec<ConvertLineReq>,
}

/// 指定要转的计划行。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ConvertLineReq {
    /// 计划明细行 id（`biz_purchase_plan_item.id`）。
    pub plan_item_id: Option<String>,
    /// 本次转出的数量：缺省 = 剩余量（计划量 − 已执行量）。
    pub qty: Option<Decimal>,
    /// 覆盖预估单价（计划行没有单价时必填）。
    pub est_price: Option<Decimal>,
}

// ==================== 中间结构（纯逻辑的工作单元） ====================

/// 可转的计划行（从 `PlanItemRow` 折出来的纯数据，单测不需要库）。
#[derive(Debug, Clone)]
pub struct ConvertibleLine {
    /// 计划明细行 id。
    pub plan_item_id: Uuid,
    /// 计划行号（报文用）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库（拆单轴）。
    pub warehouse_id: Uuid,
    /// 仓库名称。
    pub warehouse_name: String,
    /// 计量单位（由计划行带出，物料基础单位）。
    pub unit_id: Uuid,
    /// 计划量。
    pub planned_qty: Decimal,
    /// 已执行量。
    pub executed_qty: Decimal,
    /// 计划单价（不含税）。
    pub planned_price: Decimal,
    /// 行备注（带进申请明细）。
    pub remark: Option<String>,
}

impl ConvertibleLine {
    /// 剩余可转量 = 计划量 − 已执行量（负数按 0 —— `executed_qty` 允许超计划，见 V0446）。
    pub fn remaining(&self) -> Decimal {
        let remaining = self.planned_qty - self.executed_qty;
        if remaining.is_sign_negative() {
            Decimal::ZERO
        } else {
            remaining.round_dp(QTY_SCALE)
        }
    }
}

impl From<&PlanItemRow> for ConvertibleLine {
    fn from(row: &PlanItemRow) -> Self {
        Self {
            plan_item_id: row.id,
            line_no: row.line_no,
            material_id: row.material_id,
            material_code: row.material_code.clone(),
            material_name: row.material_name.clone(),
            warehouse_id: row.warehouse_id,
            warehouse_name: row.warehouse_name.clone(),
            unit_id: row.unit_id,
            planned_qty: row.planned_qty,
            executed_qty: row.executed_qty,
            planned_price: row.planned_price,
            remark: row.remark.clone(),
        }
    }
}

/// 已校验的一行转换结果。
#[derive(Debug, Clone)]
pub struct ConvertLine {
    /// 计划明细行 id（落到申请明细的 `plan_item_id`）。
    pub plan_item_id: Uuid,
    /// 计划行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 仓库名称。
    pub warehouse_name: String,
    /// 计量单位。
    pub unit_id: Uuid,
    /// 本次转出数量。
    pub qty: Decimal,
    /// 预估不含税单价。
    pub est_price: Decimal,
    /// 行金额（`qty × est_price`，4 位舍入）。
    pub amount: Decimal,
    /// 转换后这条计划行还剩多少（给前端直接显示，不用再算一遍）。
    pub remaining_after: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

/// 一个仓库要生成的申请单（单头 + 明细）。
#[derive(Debug, Clone)]
pub struct WarehouseGroup {
    /// 收货仓库。
    pub warehouse_id: Uuid,
    /// 仓库名称。
    pub warehouse_name: String,
    /// 属于这个仓库的转换行（按计划行号升序）。
    pub lines: Vec<ConvertLine>,
}

impl WarehouseGroup {
    /// 预估金额合计（不含税，4 位舍入）。
    pub fn est_amount(&self) -> Decimal {
        self.lines
            .iter()
            .fold(Decimal::ZERO, |sum, line| sum + line.amount)
            .round_dp(QTY_SCALE)
    }

    /// 数量合计。
    pub fn qty(&self) -> Decimal {
        self.lines
            .iter()
            .fold(Decimal::ZERO, |sum, line| sum + line.qty)
            .round_dp(QTY_SCALE)
    }
}

// ==================== 出参 ====================

/// 转换出来的申请明细行。
#[derive(Debug, Clone, Serialize)]
pub struct ConvertedLineRes {
    /// 来源计划行 id。
    pub plan_item_id: String,
    /// 来源计划行号。
    pub plan_line_no: i32,
    /// 物料。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 本次转出数量。
    pub qty: Decimal,
    /// 预估不含税单价。
    pub est_price: Decimal,
    /// 行金额。
    pub amount: Decimal,
    /// 转换后计划行剩余量。
    pub remaining_after: Decimal,
}

/// 转换出来的一张申请单。
#[derive(Debug, Clone, Serialize)]
pub struct ConvertedRequisitionRes {
    /// 申请单 id。
    pub requisition_id: String,
    /// 申请单号（`PRQ-yyyyMMdd-NNNNNN`）。
    pub req_no: String,
    /// 收货仓库。
    pub warehouse_id: String,
    /// 收货仓库名。
    pub warehouse_name: String,
    /// 申请部门。
    pub dept_id: String,
    /// 申请部门名。
    pub dept_name: Option<String>,
    /// 申请日期。
    pub req_date: String,
    /// 预估金额合计（不含税）。
    pub est_amount: Decimal,
    /// 明细行数。
    pub line_count: usize,
    /// 明细（含来源计划行）。
    pub lines: Vec<ConvertedLineRes>,
}

/// 转换结果：计划的最新快照 + 生成的申请单（一仓一张）。
#[derive(Debug, Clone, Serialize)]
pub struct ToRequisitionRes {
    /// 计划的最新状态（已执行量、合计都已经是回写后的值）。
    pub plan: PlanRowRes,
    /// 生成的申请单（按仓库首次出现的顺序）。
    pub requisitions: Vec<ConvertedRequisitionRes>,
    /// 共转出多少行。
    pub converted_lines: usize,
    /// 共转出多少数量。
    pub converted_qty: Decimal,
    /// 共转出多少金额。
    pub converted_amount: Decimal,
}

// ==================== 纯函数 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 解析可选 uuid（空串 → None；非法 → 400）。
pub fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|_| bad_request(format!("{field}={text} 不是合法 uuid"))),
    }
}

/// 数量校验（> 0，≤ 4 位小数）。
pub fn check_qty(raw: Decimal, field: &str) -> std::result::Result<Decimal, ApiError> {
    let qty = raw.round_dp(QTY_SCALE);
    if qty <= Decimal::ZERO {
        return Err(bad_request(format!("{field}={raw} 必须 > 0")));
    }
    Ok(qty)
}

/// 单价校验（> 0，≤ 6 位小数）。
pub fn check_price(raw: Decimal, field: &str) -> std::result::Result<Decimal, ApiError> {
    let price = raw.round_dp(PRICE_SCALE);
    if price <= Decimal::ZERO {
        return Err(bad_request(format!(
            "{field}={raw} 必须 > 0：预估单价是预算判定的依据，0 元等于不占预算"
        )));
    }
    Ok(price)
}

/// 税率校验（0–100，≤ 4 位小数；缺省 0）。
pub fn check_tax(raw: Option<Decimal>) -> std::result::Result<Decimal, ApiError> {
    let tax = raw.unwrap_or(Decimal::ZERO).round_dp(TAX_SCALE);
    if tax < Decimal::ZERO || tax > Decimal::from(100) {
        return Err(bad_request(format!("tax_rate={tax} 必须在 0–100 之间")));
    }
    Ok(tax)
}

/// 备注清洗（trim / 空串折 None / 超长 400）。
pub fn clean_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > MAX_REMARK_LEN {
        return Err(bad_request(format!(
            "remark 最长 {MAX_REMARK_LEN} 个字符（收到 {}）",
            text.chars().count()
        )));
    }
    Ok(Some(text.to_string()))
}

/// 行金额（`qty × est_price`，4 位舍入）。
pub fn line_amount(qty: Decimal, price: Decimal) -> Decimal {
    (qty * price).round_dp(QTY_SCALE)
}

/// 申请单备注：把来源写清楚（审计与对账靠这句话，别只放在日志里）。
pub fn compose_head_remark(
    plan_no: &str,
    warehouse_name: &str,
    extra: Option<&str>,
) -> std::result::Result<String, ApiError> {
    let mut text = format!("由采购计划 {plan_no} 转采购申请（仓库：{warehouse_name}）");
    if let Some(extra) = extra.map(str::trim).filter(|text| !text.is_empty()) {
        text.push('\n');
        text.push_str(extra);
    }
    if text.chars().count() > MAX_REMARK_LEN {
        return Err(bad_request(format!(
            "remark 与「由采购计划…转入」合并后超过 {MAX_REMARK_LEN} 个字符（收到 {}），请缩短备注",
            text.chars().count()
        )));
    }
    Ok(text)
}

/// **核心纯函数**：把「计划的全部行 + 请求」「折成」要落库的转换行。
///
/// 返回顺序 = 计划行号顺序（与请求里的顺序无关 —— 前端点选的顺序不该决定申请明细的行号）。
/// 请求里没提到、或剩余量为 0 的行不会被转。
pub fn plan_conversion(
    lines: &[ConvertibleLine],
    requested: &[ConvertLineReq],
) -> std::result::Result<Vec<ConvertLine>, ApiError> {
    if lines.is_empty() {
        return Err(bad_request("这张采购计划没有明细，不能转采购申请"));
    }
    if requested.len() > MAX_LINES {
        return Err(bad_request(format!(
            "一次最多转 {MAX_LINES} 行（收到 {} 行）",
            requested.len()
        )));
    }

    // 选择集：plan_item_id → (数量, 单价覆盖)
    let mut selection: std::collections::BTreeMap<Uuid, (Decimal, Option<Decimal>)> = std::collections::BTreeMap::new();

    if requested.is_empty() {
        // 全转：只转还有剩余量的行
        for line in lines {
            let remaining = line.remaining();
            if remaining > Decimal::ZERO {
                selection.insert(line.plan_item_id, (remaining, None));
            }
        }
        if selection.is_empty() {
            return Err(conflict(
                "这张采购计划的明细已全部转为采购申请（剩余可转量都是 0）：\
                 如需追加需求请直接新建采购申请单",
            ));
        }
    } else {
        for (index, item) in requested.iter().enumerate() {
            let id = parse_optional_uuid(item.plan_item_id.as_deref(), &format!("items[{index}].plan_item_id"))?
                .ok_or_else(|| bad_request(format!("items[{index}].plan_item_id 不能为空")))?;
            if selection.contains_key(&id) {
                return Err(bad_request(format!(
                    "items[{index}].plan_item_id={id} 重复出现：同一计划行一次只能转一遍\
                     （要分批请分多次调用）"
                )));
            }
            let Some(line) = lines.iter().find(|line| line.plan_item_id == id) else {
                return Err(bad_request(format!(
                    "items[{index}].plan_item_id={id} 不属于这张采购计划"
                )));
            };
            let remaining = line.remaining();
            let qty = match item.qty {
                Some(qty) => check_qty(qty, &format!("items[{index}].qty"))?,
                None => remaining,
            };
            if qty <= Decimal::ZERO {
                return Err(bad_request(format!(
                    "items[{index}]（计划第 {} 行 {} {}）剩余可转量是 0，没有可转的数量",
                    line.line_no, line.material_code, line.material_name
                )));
            }
            if qty > remaining {
                return Err(bad_request(format!(
                    "items[{index}]（计划第 {} 行 {} {}）本次转 {qty}，超过剩余可转量 {remaining}：\
                     计划是基准，超出的部分请直接新建采购申请单",
                    line.line_no, line.material_code, line.material_name
                )));
            }
            let price = match item.est_price {
                Some(price) => Some(check_price(price, &format!("items[{index}].est_price"))?),
                None => None,
            };
            selection.insert(id, (qty, price));
        }
    }

    // 按计划行号顺序落结果
    let mut out = Vec::with_capacity(selection.len());
    for line in lines {
        let Some((qty, override_price)) = selection.get(&line.plan_item_id) else {
            continue;
        };
        let est_price = match override_price {
            Some(price) => *price,
            None => {
                if line.planned_price <= Decimal::ZERO {
                    return Err(bad_request(format!(
                        "计划第 {} 行（{} {} @ {}）没有预估单价（0）：\
                         转申请前请在计划里补单价，或在请求里用 items[].est_price 覆盖\
                         （申请单价是预算判定的依据，不能是 0）",
                        line.line_no, line.material_code, line.material_name, line.warehouse_name
                    )));
                }
                line.planned_price.round_dp(PRICE_SCALE)
            }
        };
        let remaining_after = (line.remaining() - *qty).round_dp(QTY_SCALE);
        out.push(ConvertLine {
            plan_item_id: line.plan_item_id,
            line_no: line.line_no,
            material_id: line.material_id,
            material_code: line.material_code.clone(),
            material_name: line.material_name.clone(),
            warehouse_id: line.warehouse_id,
            warehouse_name: line.warehouse_name.clone(),
            unit_id: line.unit_id,
            qty: *qty,
            est_price,
            amount: line_amount(*qty, est_price),
            remaining_after: if remaining_after.is_sign_negative() {
                Decimal::ZERO
            } else {
                remaining_after
            },
            remark: line.remark.clone(),
        });
    }
    Ok(out)
}

/// 按仓库分组（申请单单头只有一个收货仓库 ⇒ 一仓一单）。首次出现的顺序即输出顺序。
pub fn group_by_warehouse(lines: &[ConvertLine]) -> Vec<WarehouseGroup> {
    let mut groups: Vec<WarehouseGroup> = Vec::new();
    for line in lines {
        match groups.iter_mut().find(|group| group.warehouse_id == line.warehouse_id) {
            Some(group) => group.lines.push(line.clone()),
            None => groups.push(WarehouseGroup {
                warehouse_id: line.warehouse_id,
                warehouse_name: line.warehouse_name.clone(),
                lines: vec![line.clone()],
            }),
        }
    }
    groups
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn line(id: u128, line_no: i32, warehouse: u128, planned: &str, executed: &str, price: &str) -> ConvertibleLine {
        ConvertibleLine {
            plan_item_id: uuid(id),
            line_no,
            material_id: uuid(9000 + id),
            material_code: format!("MAT-{id}"),
            material_name: format!("物料 {id}"),
            warehouse_id: uuid(warehouse),
            warehouse_name: format!("仓库 {warehouse}"),
            unit_id: uuid(1),
            planned_qty: dec(planned),
            executed_qty: dec(executed),
            planned_price: dec(price),
            remark: None,
        }
    }

    fn take(id: u128) -> ConvertLineReq {
        ConvertLineReq {
            plan_item_id: Some(uuid(id).to_string()),
            qty: None,
            est_price: None,
        }
    }

    /// ① 缺省全转：只转还有剩余量的行，数量 = 计划 − 已执行。
    #[test]
    fn default_converts_remaining_only() {
        let lines = vec![
            line(1, 1, 100, "10", "0", "2.5"),
            line(2, 2, 100, "5", "5", "2.5"),
            line(3, 3, 101, "3", "1", "4"),
        ];
        let out = plan_conversion(&lines, &[]).expect("应成功");
        assert_eq!(out.len(), 2, "已执行满的行不该再转");
        assert_eq!(out[0].plan_item_id, uuid(1));
        assert_eq!(out[0].qty, dec("10"));
        assert_eq!(out[0].remaining_after, Decimal::ZERO);
        assert_eq!(out[1].qty, dec("2"), "3 − 1 = 2");
        assert_eq!(out[1].remaining_after, Decimal::ZERO);
        assert_eq!(out[0].amount, dec("25"));
    }

    /// ② 全转完再转 → 409（不是造空单、也不是 200 空响应）。
    #[test]
    fn fully_consumed_plan_refuses() {
        let lines = vec![line(1, 1, 100, "2", "2", "1")];
        let err = plan_conversion(&lines, &[]).expect_err("必须拒");
        assert_eq!(err.code, error_code::RESOURCE_CONFLICT);
        assert!(err.msg.contains("剩余可转量都是 0"), "报文要说清原因：{}", err.msg);
    }

    /// ③ 超出剩余量 → 400，且报文点出剩余量与行号（不是静默截断）。
    #[test]
    fn over_remaining_is_rejected_with_numbers() {
        let lines = vec![line(1, 7, 100, "5", "3", "1")];
        let err = plan_conversion(
            &lines,
            &[ConvertLineReq {
                plan_item_id: Some(uuid(1).to_string()),
                qty: Some(dec("3")),
                est_price: None,
            }],
        )
        .expect_err("超出剩余量必须拒");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("超过剩余可转量 2"), "要有剩余量：{}", err.msg);
        assert!(err.msg.contains("计划第 7 行"), "要有行号：{}", err.msg);
    }

    /// ④ 计划行没单价 → 400；请求里覆盖单价后放行（预算是硬约束）。
    #[test]
    fn zero_plan_price_needs_override() {
        let lines = vec![line(1, 1, 100, "4", "0", "0")];
        let err = plan_conversion(&lines, &[]).expect_err("0 单价必须拒");
        assert!(err.msg.contains("没有预估单价"), "{}", err.msg);
        let out = plan_conversion(
            &lines,
            &[ConvertLineReq {
                plan_item_id: Some(uuid(1).to_string()),
                qty: None,
                est_price: Some(dec("8.5")),
            }],
        )
        .expect("覆盖单价后应成功");
        assert_eq!(out[0].est_price, dec("8.5"));
        assert_eq!(out[0].amount, dec("34"));
    }

    /// ⑤ 请求里的行必须属于这张计划 / 不许重复 / 不许空 id。
    #[test]
    fn request_lines_are_validated() {
        let lines = vec![line(1, 1, 100, "4", "0", "1"), line(2, 2, 100, "4", "0", "1")];
        let foreign = plan_conversion(
            &lines,
            &[ConvertLineReq {
                plan_item_id: Some(uuid(99).to_string()),
                qty: None,
                est_price: None,
            }],
        )
        .expect_err("别的计划的行走不通");
        assert!(foreign.msg.contains("不属于这张采购计划"), "{}", foreign.msg);

        let dup = plan_conversion(&lines, &[take(1), take(1)]).expect_err("重复必须拒");
        assert!(dup.msg.contains("重复出现"), "{}", dup.msg);

        let empty = plan_conversion(
            &lines,
            &[ConvertLineReq {
                plan_item_id: None,
                qty: None,
                est_price: None,
            }],
        )
        .expect_err("缺 plan_item_id 必须拒");
        assert!(empty.msg.contains("plan_item_id 不能为空"), "{}", empty.msg);
    }

    /// ⑥ 部分转：剩余量正确递减，且输出按**计划行号**排序（与请求顺序无关）。
    #[test]
    fn partial_conversion_keeps_plan_order() {
        let lines = vec![line(1, 1, 100, "10", "0", "2"), line(2, 2, 101, "10", "9", "3")];
        let out = plan_conversion(
            &lines,
            &[
                ConvertLineReq {
                    plan_item_id: Some(uuid(2).to_string()),
                    qty: Some(dec("1")),
                    est_price: None,
                },
                ConvertLineReq {
                    plan_item_id: Some(uuid(1).to_string()),
                    qty: Some(dec("4")),
                    est_price: None,
                },
            ],
        )
        .expect("应成功");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].plan_item_id, uuid(1), "计划第 1 行必须排在前");
        assert_eq!(out[0].qty, dec("4"));
        assert_eq!(out[0].remaining_after, dec("6"));
        assert_eq!(out[1].plan_item_id, uuid(2));
        assert_eq!(out[1].remaining_after, Decimal::ZERO);
    }

    /// ⑦ 一仓一单：同仓多行合并到一张单，不同仓拆成多张，顺序按仓库首次出现。
    #[test]
    fn groups_are_one_requisition_per_warehouse() {
        let lines = vec![
            line(1, 1, 100, "1", "0", "1"),
            line(2, 2, 101, "2", "0", "2"),
            line(3, 3, 100, "3", "0", "3"),
        ];
        let out = plan_conversion(&lines, &[]).expect("应成功");
        let groups = group_by_warehouse(&out);
        assert_eq!(groups.len(), 2, "两个仓库 → 两张申请单");
        assert_eq!(groups[0].warehouse_id, uuid(100));
        assert_eq!(groups[0].lines.len(), 2);
        assert_eq!(groups[0].qty(), dec("4"), "1 + 3");
        assert_eq!(groups[0].est_amount(), dec("10"), "1 + 9");
        assert_eq!(groups[1].warehouse_id, uuid(101));
        assert_eq!(groups[1].est_amount(), dec("4"));
    }

    /// ⑧ 数量 / 单价 / 税率闸：0 与负值都拒，标度按域收敛。
    #[test]
    fn numeric_guards() {
        assert!(check_qty(dec("0"), "qty").is_err());
        assert!(check_qty(dec("-1"), "qty").is_err());
        assert_eq!(check_qty(dec("1.23456"), "qty").expect("应舍入"), dec("1.2346"));
        assert!(check_price(dec("0"), "price").is_err());
        assert_eq!(check_price(dec("1.1234567"), "price").expect("应舍入"), dec("1.123457"));
        assert_eq!(check_tax(None).expect("缺省 0"), Decimal::ZERO);
        assert!(check_tax(Some(dec("101"))).is_err());
        assert!(check_tax(Some(dec("-1"))).is_err());
        // 行金额 4 位舍入：3 × 0.333333 = 0.999999 → 1.0000（申请单的金额列是 NUMERIC(18,4)）
        assert_eq!(line_amount(dec("3"), dec("0.333333")), dec("1.0000"));
    }

    /// ⑨ 备注：来源句必须在，用户备注追加，合并超长 400（不静默截断）。
    #[test]
    fn head_remark_keeps_source_and_warns_when_too_long() {
        let text = compose_head_remark("PPL-20261117-000001", "成品仓", Some("  急件  ")).expect("正常长度");
        assert!(text.contains("PPL-20261117-000001"), "来源单号必须在：{text}");
        assert!(text.contains("成品仓"), "仓库必须在：{text}");
        assert!(text.ends_with("急件"), "用户备注要追加：{text}");
        let long = "备".repeat(MAX_REMARK_LEN);
        assert!(
            compose_head_remark("PPL-20261117-000001", "成品仓", Some(&long)).is_err(),
            "合并超长必须 400"
        );
    }

    /// ⑩ 剩余量：已执行超计划时按 0 处理（V0446 刻意允许超计划，别让这里算出负数）。
    #[test]
    fn remaining_never_goes_negative() {
        let line = line(1, 1, 100, "5", "7", "1");
        assert_eq!(line.remaining(), Decimal::ZERO);
    }
}
