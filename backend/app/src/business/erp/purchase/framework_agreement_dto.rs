//! 框架协议契约层（M4-11b）：建单 / 改单 / 删单 / 列表 / 详情 / 提交审批的入参与出参。
//!
//! 与 [`crate::business::erp::purchase::purchase_plan_dto`] 同一套约定：
//! 主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 金额/数量 `Decimal`（**JSON 里是字符串**，4/6 位标度）、
//! 状态与定价方式一律大写字符串且与 `V0450` 的 `CHECK` 逐字一致。
//!
//! # 路由与权限码（与 `V0456` 的 `sys_menu.auth_code` 逐字一致）
//!
//! | 方法 | 路径（前缀 `/api` 由 `[router].prefix` 注入） | 权限码 |
//! |:--|:--|:--|
//! | GET | `/business/erp/framework-agreement/page` | `…:list` |
//! | GET | `/business/erp/framework-agreement/{id}` | `…:list` |
//! | POST | `/business/erp/framework-agreement` | `…:create` |
//! | PUT | `/business/erp/framework-agreement/{id}` | `…:update` |
//! | DELETE | `/business/erp/framework-agreement/{id}` | `…:delete` |
//! | POST | `/business/erp/framework-agreement/{id}/submit` | `…:submit` |
//! | GET | `/business/erp/framework-agreement/execution/page` | `…:execution`（M4-09，已存在） |
//! | GET | `/business/erp/framework-agreement/{id}/execution` | `…:execution`（M4-09，已存在） |
//!
//! 页面显形码（`…:page`，菜单迁移 `V0452` 种下、`V0456` 不动它）**不在这里**：
//! AGENTS 约定 14 / `docs/编码规范.md` §1.7.2 要求后端源码零引用（连常量都不定义），
//! 例外登记在 `permission_audit_tests::ROUTE_LEVEL_ONLY`，N14/N18 会逐条核这条例外是否仍然成立。
//!
//! # 校验口径：与 M4-08（下单引用校验）**同一份判据**，不各写一套
//!
//! | 判据 | 出处 | 为什么必须一致 |
//! |:--|:--|:--|
//! | `start_date <= end_date` | 库层 `ck_biz_fwa_period` + 本文件 [`check_period`] | 有效期决定「今天能不能下单」，两头判据不一致 → 能被引用却是坏协议 |
//! | `FLOATING ⇒ price_adjust_rule` 非空 | 库层 `ck_biz_fwa_adjust` + [`check_adjust_rule`] | 没有规则的「随行就市」= 没有约定，M4-08 无从校验 |
//! | `(agreement_id, material_id)` 唯一 | 库层 `uk_biz_fwai_material` + [`check_materials_unique`] | 两行同物料 = 两个协议价，M4-08 取哪一行都说不清（库层兜底是 500） |
//! | `agreed_price >= 0` | 库层 `ck_biz_fwai_price` + [`check_agreed_price`] | 0 价物料合法（赠品 / 试用料），负数不合法 |
//! | `max_qty / total_max_*` 要么 NULL 要么 > 0 | 库层 `ck_biz_fwai_cap` / `ck_biz_fwa_caps` + [`check_cap`] | `0` 上限 = 禁止采购，**不是**「不限」（想不限就留空） |
//! | 改协议不许把上限改小到低于 `executed_qty` | 本文件 [`check_cap_shrink`] / [`check_missing_material`] | 否则 M4-08 的准入（读现算的已用量）与 M4-09 的报表（超 100%）互相矛盾 |
//!
//! 最后两条是**跨行语义**（库层 CHECK 做不到），必须在 handler 里查已用量后判；
//! 判据本身写成纯函数，单测不连库就能钉住。
//!
//! # 出参的两条硬口径
//!
//! 1. `max_qty` / `total_max_qty` / `total_max_amount` 的 `null` = **不限**，
//!    前端**不许**显示成 0（那会被读成「一点都不剩」，恰好相反）；
//! 2. 单头 `total_executed_*` 是**派生列**（服务端按明细重算），请求体里带了也不读。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::business::shared::doc_lines;
use cloudpivot_kernel::shared::error_code;

pub use crate::business::erp::purchase::framework_agreement_model::{
    PRICE_MODE_FIXED, PRICE_MODE_FLOATING, PRICE_MODES, STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED, STATUSES,
    status_label,
};

// ==================== 域常量 ====================

/// 物料范围行数上限（一份协议最多多少个物料；框架协议通常几十行）。
pub const MAX_LINES: usize = 500;
/// 协议名称长度上限（与 `biz_framework_agreement.agreement_name VARCHAR(128)` 一致）。
pub const MAX_NAME_LEN: usize = 128;
/// 备注长度上限（与采购计划同口径）。
pub const MAX_REMARK_LEN: usize = 500;
/// 调价规则长度上限（`price_adjust_rule` 是 `TEXT`；给个软上限，避免粘一整份合同进来）。
pub const MAX_RULE_LEN: usize = 2000;

/// 本模块 6 个**接口**权限码（页面显形码 `…:page` 只给前端，按约定 14 不在此列）。
pub const AGREEMENT_PERMISSIONS: [&str; 6] = [
    "business:erp:framework-agreement:list",
    "business:erp:framework-agreement:create",
    "business:erp:framework-agreement:update",
    "business:erp:framework-agreement:delete",
    "business:erp:framework-agreement:submit",
    // M4-09 已有的两个只读接口（执行量）—— 列进来是为了「本域一共有哪些码」可一次对账
    "business:erp:framework-agreement:execution",
];

/// 定价方式中文名（报文用）。
pub fn price_mode_label(price_mode: &str) -> &str {
    match price_mode {
        PRICE_MODE_FIXED => "固定价",
        PRICE_MODE_FLOATING => "随行就市",
        other => other,
    }
}

// ==================== 入参 ====================

/// 新建 / 修改框架协议（`PUT` 为整单替换：单头字段 + 全量物料范围）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreateAgreementReq {
    /// 协议名称（必填，≤128 字）。
    pub agreement_name: Option<String>,
    /// 供应商（必填；必须存在且存活）。
    pub supplier_id: Option<String>,
    /// 经办部门（可空 = 公司级）。
    pub dept_id: Option<String>,
    /// 经办人（缺省 = 当前登录人；姓名一律服务端快照）。
    pub owner_id: Option<String>,
    /// 有效期起（含，`YYYY-MM-DD`，必填）。
    pub start_date: Option<String>,
    /// 有效期止（含，`YYYY-MM-DD`，必填；必须 ≥ `start_date`）。
    pub end_date: Option<String>,
    /// 签订日期（可空；与生效起始日可以不同）。
    pub sign_date: Option<String>,
    /// 定价方式 FIXED / FLOATING（缺省 FIXED）。
    pub price_mode: Option<String>,
    /// 协议价是否含税（缺省 false = 不含税，与采购订单 `price` 列同口径）。
    pub price_includes_tax: Option<bool>,
    /// 协议价币种（ISO 4217 三位大写，缺省 CNY）。
    pub price_currency: Option<String>,
    /// 下单价允许高于协议价的比例（0–1，缺省 0 = 必须 ≤ 协议价）。
    pub price_tolerance: Option<Decimal>,
    /// 调价规则原文（FLOATING 必填）。
    pub price_adjust_rule: Option<String>,
    /// 协议总量上限（空 = 不限；写了必须 > 0）。
    pub total_max_qty: Option<Decimal>,
    /// 协议总额上限（空 = 不限；写了必须 > 0）。
    pub total_max_amount: Option<Decimal>,
    /// 备注。
    pub remark: Option<String>,
    /// 物料范围（可以为空 = 拟稿还没编物料，M4-09 对空范围返 200 + 空列表）。
    pub items: Vec<AgreementItemReq>,
}

/// 物料范围行（一个物料一行；单位由物料带出，客户端不许传）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgreementItemReq {
    /// 物料（必填）。
    pub material_id: Option<String>,
    /// 协议单价（≥ 0，6 位；缺省 0 = 赠品 / 试用料）。
    pub agreed_price: Option<Decimal>,
    /// 进项税率（0–1，缺省 0）。
    pub tax_rate: Option<Decimal>,
    /// 该物料协议期内累计上限（空 = 不限；写了必须 > 0）。
    pub max_qty: Option<Decimal>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 提交审批入参（`skip` 走 M4-02 起就有的显式跳审闸，理由写进备注）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SubmitAgreementReq {
    /// 提交说明（可选；写进备注）。
    pub reason: Option<String>,
    /// 显式跳过审批的请求片段（只有 `business:erp:skip-approval` + 理由 ≥10 字才可能生效）。
    #[serde(flatten)]
    pub skip: crate::business::erp::approval_gate::SkipApprovalReq,
}

// ==================== 出参 ====================

/// 协议单头（列表 / 详情共用一份，避免两条赋值路径漂移）。
#[derive(Debug, Clone, Serialize)]
pub struct AgreementRowRes {
    /// 主键。
    pub id: String,
    /// 协议号 `FWA-yyyyMMdd-000001`。
    pub agreement_no: String,
    /// 协议名称。
    pub agreement_name: String,
    /// 供应商 id。
    pub supplier_id: String,
    /// 供应商名**快照**（签约主体历史事实，约定 13）。
    pub supplier_name: Option<String>,
    /// 供应商**现名**（实时 join；快照为空时回退到它）。
    pub supplier_display_name: String,
    /// 经办部门 id。
    pub dept_id: Option<String>,
    /// 经办部门名快照。
    pub dept_name: Option<String>,
    /// 经办人 id。
    pub owner_id: Option<String>,
    /// 经办人姓名快照。
    pub owner_name: Option<String>,
    /// 有效期起（含）。
    pub start_date: String,
    /// 有效期止（含）。
    pub end_date: String,
    /// 签订日期。
    pub sign_date: Option<String>,
    /// 定价方式。
    pub price_mode: String,
    /// 定价方式中文。
    pub price_mode_label: String,
    /// 协议价是否含税。
    pub price_includes_tax: bool,
    /// 协议价币种。
    pub price_currency: String,
    /// 允许高于协议价的比例上限。
    pub price_tolerance: Decimal,
    /// 调价规则原文。
    pub price_adjust_rule: Option<String>,
    /// 协议总量上限（`null` = 不限）。
    pub total_max_qty: Option<Decimal>,
    /// 协议总额上限（`null` = 不限）。
    pub total_max_amount: Option<Decimal>,
    /// 已执行量合计（派生）。
    pub total_executed_qty: Decimal,
    /// 已执行金额合计（派生）。
    pub total_executed_amount: Decimal,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 审批实例 id。
    pub approval_instance_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 创建人。
    pub create_by: Option<String>,
    /// 物料范围行数。
    pub item_count: i64,
    /// 是否可改 / 可删 / 可提交（只有 `DRAFT`）—— 前端**用它**判按钮，别自己写状态机。
    pub editable: bool,
}

/// 物料范围行（详情用：改单表单要靠它回填「已用量」以免把上限改小到低于已用）。
#[derive(Debug, Clone, Serialize)]
pub struct AgreementItemRes {
    /// 行主键（改单时服务端按 `material_id` 就地对齐，**不重建**行 id）。
    pub id: String,
    /// 行号（1 起）。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 单位 id。
    pub unit_id: String,
    /// 单位名。
    pub unit_name: String,
    /// 协议单价。
    pub agreed_price: Decimal,
    /// 进项税率（0–1 分数）。
    pub tax_rate: Decimal,
    /// 该物料数量上限（`null` = 不限）。
    pub max_qty: Option<Decimal>,
    /// 已执行量（回写快照；改单时的新上限**不允许**小于它）。
    pub executed_qty: Decimal,
    /// 已执行金额。
    pub executed_amount: Decimal,
    /// 剩余可下单量（`null` = 不限；负数 = 超上限）。
    pub remaining_qty: Option<Decimal>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 详情：单头 + 物料范围。
#[derive(Debug, Clone, Serialize)]
pub struct AgreementDetailRes {
    /// 单头。
    pub agreement: AgreementRowRes,
    /// 物料范围（按行号升序；拟稿还没编物料时是 `[]`，不是 404）。
    pub items: Vec<AgreementItemRes>,
}

// ==================== 解析 / 清洗 ====================

/// 400。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409（状态守卫）。
pub fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 解析必填 uuid 字段（空串 / 缺省 → 400）。
pub fn parse_uuid_field(raw: Option<&str>, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|_| bad_request(format!("{field}={text} 不是合法 uuid")))
}

/// 解析可选 uuid 字段（空串 → None；非法 → 400）。
pub fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|_| bad_request(format!("{field}={text} 不是合法 uuid"))),
    }
}

/// 解析日期字段（`YYYY-MM-DD`；空串 → None；非法 → 400 并点名字段）。
pub fn parse_date_field(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| bad_request(format!("{field}={text} 不是合法日期（要 YYYY-MM-DD）"))),
    }
}

/// 清洗备注：trim → 空串折成 `None` → 超长 400。
pub fn clean_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim) else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > MAX_REMARK_LEN {
        return Err(bad_request(format!(
            "备注最长 {MAX_REMARK_LEN} 个字符（收到 {}）",
            text.chars().count()
        )));
    }
    Ok(Some(text.to_string()))
}

// ==================== 单头判据（纯函数） ====================

/// 校验协议名称（trim 后非空，≤128）。
pub fn check_name(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request("agreement_name 不能为空"));
    }
    if text.chars().count() > MAX_NAME_LEN {
        return Err(bad_request(format!(
            "agreement_name 最长 {MAX_NAME_LEN} 个字符（收到 {}）",
            text.chars().count()
        )));
    }
    Ok(text.to_string())
}

/// 有效期：起止都必填，且 `start_date <= end_date`（与 `ck_biz_fwa_period` 同口径）。
///
/// 起止日都是**含**边界：`end_date` 当天仍可下单（与 `is_effective_on` 一致）。
pub fn check_period(
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> std::result::Result<(NaiveDate, NaiveDate), ApiError> {
    let Some(start) = start else {
        return Err(bad_request("start_date 不能为空（有效期起）"));
    };
    let Some(end) = end else {
        return Err(bad_request("end_date 不能为空（有效期止）"));
    };
    if start > end {
        return Err(bad_request(format!(
            "有效期起（{start}）不能晚于有效期止（{end}）：起止日是**含**边界，写反了这份协议一天都不会生效"
        )));
    }
    Ok((start, end))
}

/// 定价方式闭集（大小写容错，与 `ck_biz_fwa_price_mode` 同一份）。
pub fn check_price_mode(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default().to_ascii_uppercase();
    let text = if text.is_empty() {
        PRICE_MODE_FIXED.to_string()
    } else {
        text
    };
    if !PRICE_MODES.contains(&text.as_str()) {
        return Err(bad_request(format!(
            "price_mode={text} 不是合法定价方式（{}）",
            PRICE_MODES.join(" / ")
        )));
    }
    Ok(text)
}

/// 币种：三位大写 ISO 4217（与 `ck_biz_fwa_currency` 同口径；空串 → CNY）。
pub fn check_currency(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default().to_ascii_uppercase();
    let text = if text.is_empty() { "CNY".to_string() } else { text };
    let ok = text.len() == 3 && text.chars().all(|ch| ch.is_ascii_uppercase());
    if !ok {
        return Err(bad_request(format!(
            "price_currency={text} 不是合法币种（ISO 4217 三位大写，例如 CNY / USD）"
        )));
    }
    Ok(text)
}

/// 容差：0–1（与 `ck_biz_fwa_tolerance` 同口径；缺省 0 = 必须 ≤ 协议价）。
pub fn check_tolerance(raw: Option<Decimal>) -> std::result::Result<Decimal, ApiError> {
    let value = raw.unwrap_or(Decimal::ZERO).round_dp(4);
    if value < Decimal::ZERO || value > Decimal::ONE {
        return Err(bad_request(format!(
            "price_tolerance={value} 超范围（0–1；0 = 下单价必须 ≤ 协议价，0.05 = 允许高 5%）"
        )));
    }
    Ok(value)
}

/// 调价规则：`FLOATING` **必须**有非空规则；`FIXED` 可以留空（留了也保留）。
///
/// 与 `ck_biz_fwa_adjust` 同口径。库层那条 CHECK 的 `btrim(...) <> ''` 不能省，
/// 这里同理 —— 只判 `is_none()` 会放过「一串空格」。
pub fn check_adjust_rule(price_mode: &str, raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if price_mode == PRICE_MODE_FLOATING && text.is_empty() {
        return Err(bad_request(
            "定价方式是「随行就市」（FLOATING）时必须写调价规则（price_adjust_rule）：\
             没有规则的浮动价 = 没有约定，下单校验无从判「高多少算超」",
        ));
    }
    if text.chars().count() > MAX_RULE_LEN {
        return Err(bad_request(format!(
            "price_adjust_rule 最长 {MAX_RULE_LEN} 个字符（收到 {}）",
            text.chars().count()
        )));
    }
    Ok(if text.is_empty() { None } else { Some(text.to_string()) })
}

/// 上限（单头总量 / 总额 / 明细量共用）：`None` = **不限**，写了必须 `> 0`。
///
/// `0` 不是「不限」—— 0 上限等于禁止采购，想不限就留空（语义只有一种）。
pub fn check_cap(raw: Option<Decimal>, field: &str) -> std::result::Result<Option<Decimal>, ApiError> {
    match raw {
        None => Ok(None),
        Some(value) => {
            let value = value.round_dp(4);
            if value <= Decimal::ZERO {
                return Err(bad_request(format!(
                    "{field}={value} 必须 > 0；「不限量」请留空（null），不要写 0（0 上限等于禁止采购）"
                )));
            }
            Ok(Some(value))
        }
    }
}

// ==================== 明细判据（纯函数） ====================

/// 协议单价：`>= 0`（0 = 赠品 / 试用料合法），6 位舍入。
pub fn check_agreed_price(raw: Option<Decimal>, field: &str) -> std::result::Result<Decimal, ApiError> {
    let price = raw.unwrap_or(Decimal::ZERO).round_dp(6);
    if price < Decimal::ZERO {
        return Err(bad_request(format!(
            "{field}={price} 不能为负（0 是合法的：赠品 / 试用料）"
        )));
    }
    Ok(price)
}

/// 进项税率：0–1（与 `ck_biz_fwai_tax` 同口径），4 位舍入。
pub fn check_tax_rate(raw: Option<Decimal>, field: &str) -> std::result::Result<Decimal, ApiError> {
    let rate = raw.unwrap_or(Decimal::ZERO).round_dp(4);
    if rate < Decimal::ZERO || rate > Decimal::ONE {
        return Err(bad_request(format!(
            "{field}={rate} 超范围（0–1 分数：13% 写 0.13，不是 13）"
        )));
    }
    Ok(rate)
}

/// 物料范围行数闸（可以为 0：拟稿还没编物料；≤ `MAX_LINES`）。
pub fn check_items(items: &[AgreementItemReq]) -> std::result::Result<(), ApiError> {
    if items.len() > MAX_LINES {
        return Err(bad_request(doc_lines::too_many_lines(MAX_LINES, items.len())));
    }
    Ok(())
}

/// 同一份协议里「同物料只能一行」（库层 `uk_biz_fwai_material` 兜底是 500，这里给人话）。
pub fn check_materials_unique(materials: &[Uuid]) -> std::result::Result<(), ApiError> {
    let mut seen: std::collections::BTreeMap<Uuid, usize> = std::collections::BTreeMap::new();
    for (index, material_id) in materials.iter().enumerate() {
        let line_no = index + 1;
        if let Some(first) = seen.get(material_id) {
            return Err(bad_request(format!(
                "第 {line_no} 行与第 {first} 行是同一个物料：同一份框架协议里一个物料只能有一行 \
                 （两行就是两个协议价，下单校验取哪一行都说不清）"
            )));
        }
        seen.insert(*material_id, line_no);
    }
    Ok(())
}

/// 改单时「新上限不许小于已用量」（跨行语义：库层 CHECK 做不到）。
///
/// 为什么必须拦：M4-08 的下单准入**读现算的已用量**判上限，M4-09 的报表按同一口径露头；
/// 一旦允许把 `max_qty` 调到小于 `executed_qty`，同一份协议在「准入」侧是「早就超了」、
/// 在「台账」侧却是「上限本来就小」—— 两个入口给出互相矛盾的结论，且没有任何人知道
/// 那笔已经发生的采购该不该退。
pub fn check_cap_shrink(
    field: &str,
    label: &str,
    new_cap: Option<Decimal>,
    executed: Decimal,
) -> std::result::Result<(), ApiError> {
    if let Some(new_cap) = new_cap
        && new_cap < executed
    {
        return Err(conflict(format!(
            "{label}的新上限（{field}={new_cap}）小于已执行量（{executed}）：\
             已发生的采购不会因为改单而消失，把上限调到低于已用量会让下单准入与执行量报表互相矛盾。\
             要收紧额度请先处理已有采购单，或把上限留在 ≥ {executed}"
        )));
    }
    Ok(())
}

/// 改单时「不许删掉已经有执行量的物料行」。
///
/// 为什么不删：`biz_purchase_order_item.framework_agreement_item_id` 是**弱引用**（V0451，
/// 刻意不加外键），删掉协议行之后那些采购明细仍指向一个不存在的 id ——
/// 执行量报表（现算 FROM 采购单）会**静默少掉**这些量，而回写列还在，
/// 两边差一个说不清的数。所以有执行量的行必须在，只能改价 / 改上限。
pub fn check_missing_material(
    material_id: Uuid,
    material_name: &str,
    executed: Decimal,
) -> std::result::Result<(), ApiError> {
    if executed > Decimal::ZERO {
        return Err(conflict(format!(
            "物料「{material_name}」（{material_id}）已有执行量 {executed}，不能从物料范围里删掉：\
             已下的采购单还引着这一行，删了执行量报表会静默少掉这些量（弱引用没有外键兜底）。\
             真要停供请把 max_qty 调到等于已用量并另签新协议"
        )));
    }
    Ok(())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    /// ① 权限码清单必须与 V0456 的 `sys_menu.auth_code` 逐字一致。
    #[test]
    fn permission_codes_match_v0456() {
        assert_eq!(AGREEMENT_PERMISSIONS.len(), 6);
        for expected in [
            "business:erp:framework-agreement:list",
            "business:erp:framework-agreement:create",
            "business:erp:framework-agreement:update",
            "business:erp:framework-agreement:delete",
            "business:erp:framework-agreement:submit",
            "business:erp:framework-agreement:execution",
        ] {
            assert!(
                AGREEMENT_PERMISSIONS.contains(&expected),
                "缺 {expected}（与迁移的 auth_code 对不上就是「码有了但没人有」，按钮点了 403）"
            );
        }
        // 页面显形码**不在**接口码清单里（约定 14：只给前端判显隐）。
        //
        // 判据写成「前缀 + 动作白名单」，而不是把页面码字面量写死进来做负断言：
        // 页面码本身就是本文件**必须避免出现**的字符串（N18 扫源码字面量），
        // 写死进来等于「反证断言自己踩了铁律」—— 2026-11-18 卡 t_23aad8fd 修的就是这个。
        // 白名单只有 6 个动作，`page` 不在其中 ⇒ 语义等价，且后端源码里 0 引用。
        let prefix = "business:erp:framework-agreement:";
        let interface_actions = ["list", "create", "update", "delete", "submit", "execution"];
        for code in AGREEMENT_PERMISSIONS {
            let action = code
                .strip_prefix(prefix)
                .unwrap_or_else(|| panic!("{code} 不以本资源的权限码前缀开头（借了别的资源的前缀）"));
            assert!(
                interface_actions.contains(&action),
                "{code} 的动作位「{action}」不在接口动作白名单 {interface_actions:?} 里：\
                 接口码只取这 6 个动作，页面显形码（`…:page`）不属于接口码"
            );
        }
    }

    /// ② 有效期：起止含边界；起晚于止必须拒（与 ck_biz_fwa_period 同口径）。
    #[test]
    fn period_rejects_reversed_dates() {
        let (start, end) = check_period(Some(date("2026-01-01")), Some(date("2026-12-31"))).expect("正序合法");
        assert_eq!(start, date("2026-01-01"));
        assert_eq!(end, date("2026-12-31"));
        // 同一天：有效期 1 天是合法的
        assert!(check_period(Some(date("2026-05-01")), Some(date("2026-05-01"))).is_ok());
        // 起晚于止：拒，且报文点出「一天都不会生效」
        let error = check_period(Some(date("2026-12-31")), Some(date("2026-01-01"))).expect_err("倒序必须拒");
        assert!(error.msg.contains("不能晚于"), "报文要说清是日期反了：{}", error.msg);
        // 缺一不可
        assert!(check_period(None, Some(date("2026-01-01"))).is_err());
        assert!(check_period(Some(date("2026-01-01")), None).is_err());
    }

    /// ③ 浮动作价必须有调价规则；空白串也算「没有」（3VL / 空串陷阱）。
    #[test]
    fn floating_requires_adjust_rule() {
        assert!(check_adjust_rule(PRICE_MODE_FLOATING, None).is_err());
        assert!(check_adjust_rule(PRICE_MODE_FLOATING, Some("   ")).is_err());
        assert_eq!(
            check_adjust_rule(PRICE_MODE_FLOATING, Some(" 随钢材指数月度均价上浮不超过 3% "))
                .expect("有规则的浮动价合法"),
            Some("随钢材指数月度均价上浮不超过 3%".to_string())
        );
        // 固定价不需要规则；留了也保留
        assert_eq!(
            check_adjust_rule(PRICE_MODE_FIXED, None).expect("固定价不要求规则"),
            None
        );
        assert_eq!(
            check_adjust_rule(PRICE_MODE_FIXED, Some("按季调价")).expect("固定价留规则也允许"),
            Some("按季调价".to_string())
        );
    }

    /// ④ 上限：null = 不限；0 不是「不限」而是「禁止采购」→ 必须拒。
    #[test]
    fn cap_allows_null_but_rejects_zero_and_negative() {
        assert_eq!(check_cap(None, "max_qty").expect("null = 不限"), None);
        assert_eq!(
            check_cap(Some(dec("0.0001")), "max_qty").expect("最小正数合法"),
            Some(dec("0.0001"))
        );
        for bad in ["0", "-1", "-0.0001"] {
            let error = check_cap(Some(dec(bad)), "max_qty").expect_err("0 / 负数必须拒");
            assert!(
                error.msg.contains("不限量") || error.msg.contains("> 0"),
                "报文要教用户「想不限就留空」：{}",
                error.msg
            );
        }
    }

    /// ⑤ 定价方式 / 币种 / 容差：缺省值与闭集。
    #[test]
    fn price_mode_currency_tolerance_defaults_and_bounds() {
        assert_eq!(check_price_mode(None).expect("缺省 FIXED"), PRICE_MODE_FIXED);
        assert_eq!(
            check_price_mode(Some(" floating ")).expect("大小写容错"),
            PRICE_MODE_FLOATING
        );
        assert!(
            check_price_mode(Some("MARKET")).is_err(),
            "域外的 MARKET 必须拒（库层 CHECK 同口径）"
        );

        assert_eq!(check_currency(None).expect("缺省 CNY"), "CNY");
        assert_eq!(check_currency(Some("usd")).expect("自动大写"), "USD");
        // 判据是**形状**（与 ck_biz_fwa_currency 的 `^[A-Z]{3}$` 同口径），不查 ISO 全表：
        // 库里拦得住的就是「不是三个大写字母」，多一个在库层能过的码这里也放行。
        assert_eq!(check_currency(Some(" rmb ")).expect("小写自动大写"), "RMB");
        assert!(check_currency(Some("CN")).is_err(), "两位必须拒");
        assert!(check_currency(Some("人民币")).is_err(), "非 ASCII 必须拒");

        assert_eq!(check_tolerance(None).expect("缺省 0"), Decimal::ZERO);
        assert_eq!(check_tolerance(Some(dec("0.05"))).expect("5% 容差合法"), dec("0.05"));
        assert_eq!(check_tolerance(Some(dec("1"))).expect("1 = 100% 是上界"), Decimal::ONE);
        assert!(check_tolerance(Some(dec("1.5"))).is_err());
        assert!(check_tolerance(Some(dec("-0.1"))).is_err());
    }

    /// ⑥ 明细：单价 ≥ 0（0 合法）、税率 0–1、同物料不许两行。
    #[test]
    fn item_checks() {
        assert_eq!(
            check_agreed_price(Some(dec("0")), "agreed_price").expect("0 价合法"),
            Decimal::ZERO
        );
        assert!(check_agreed_price(Some(dec("-0.01")), "agreed_price").is_err());
        // 6 位标度：12.3456789 → 12.345679
        assert_eq!(
            check_agreed_price(Some(dec("12.3456789")), "agreed_price").expect("正常价"),
            dec("12.345679")
        );

        assert_eq!(check_tax_rate(Some(dec("0.13")), "tax_rate").expect("13%"), dec("0.13"));
        assert!(check_tax_rate(Some(dec("13")), "tax_rate").is_err(), "13 不是 13%");
        assert!(check_tax_rate(Some(dec("1.01")), "tax_rate").is_err());

        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        assert!(check_materials_unique(&[a, b]).is_ok());
        assert!(check_materials_unique(&[]).is_ok(), "空物料范围合法（拟稿还没编）");
        let error = check_materials_unique(&[a, b, a]).expect_err("同物料两行必须拒");
        assert!(
            error.msg.contains("第 3 行与第 1 行"),
            "报文要点出是哪两行：{}",
            error.msg
        );

        assert!(check_items(&[]).is_ok());
        let too_many: Vec<AgreementItemReq> = (0..=MAX_LINES).map(|_| AgreementItemReq::default()).collect();
        assert!(check_items(&too_many).is_err());
    }

    /// ⑦ 改单收紧额度的两条守卫：新上限 < 已用 → 409；删掉有执行量的物料 → 409。
    #[test]
    fn shrink_guards() {
        // 上限没写（不限）→ 不构成收紧
        assert!(check_cap_shrink("max_qty", "物料「钢卷」", None, dec("120")).is_ok());
        // 新上限 == 已用：允许（「用满了」是合法的收紧终点）
        assert!(check_cap_shrink("max_qty", "物料「钢卷」", Some(dec("120")), dec("120")).is_ok());
        // 新上限 > 已用：允许
        assert!(check_cap_shrink("max_qty", "物料「钢卷」", Some(dec("150")), dec("120")).is_ok());
        // 新上限 < 已用：拒，且报文说清后果
        let error = check_cap_shrink("max_qty", "物料「钢卷」", Some(dec("100")), dec("120"))
            .expect_err("改小到低于已用量必须拒");
        assert_eq!(error.code, error_code::RESOURCE_CONFLICT);
        assert!(error.msg.contains("小于已执行量"), "{}", error.msg);

        // 删掉没有执行量的物料行：允许（还没下过单）
        assert!(check_missing_material(Uuid::now_v7(), "垫片", Decimal::ZERO).is_ok());
        // 删掉有执行量的行：拒
        let error = check_missing_material(Uuid::now_v7(), "钢卷", dec("30")).expect_err("有执行量不许删");
        assert_eq!(error.code, error_code::RESOURCE_CONFLICT);
        assert!(error.msg.contains("钢卷"), "报文要点出是哪个物料：{}", error.msg);
    }

    /// ⑧ 解析：空串 → None；非法日期 / uuid → 400 且字段名在报文里。
    #[test]
    fn parsers_report_field_names() {
        assert_eq!(parse_date_field(Some(""), "start_date").expect("空串 = 未填"), None);
        assert_eq!(
            parse_date_field(Some("2026-01-05"), "start_date").expect("正常日期"),
            Some(date("2026-01-05"))
        );
        let error = parse_date_field(Some("2026/01/05"), "start_date").expect_err("斜杠不是合法格式");
        assert!(error.msg.contains("start_date"), "{}", error.msg);

        assert_eq!(parse_optional_uuid(Some("  "), "dept_id").expect("空串 = 不传"), None);
        let error = parse_optional_uuid(Some("abc"), "dept_id").expect_err("非 uuid");
        assert!(error.msg.contains("dept_id"), "{}", error.msg);
        let error = parse_uuid_field(None, "supplier_id").expect_err("必填缺失");
        assert!(error.msg.contains("supplier_id"), "{}", error.msg);
    }

    /// ⑨ 名称 / 备注：trim 后非空、超长拒绝。
    #[test]
    fn name_and_remark_are_trimmed_and_bounded() {
        assert_eq!(
            check_name(Some("  2026 年度钢材框架采购协议  ")).expect("正常名"),
            "2026 年度钢材框架采购协议"
        );
        assert!(
            check_name(Some("   ")).is_err(),
            "空白名必须拒（库层 ck_biz_fwa_name 同口径）"
        );
        assert!(check_name(Some(&"字".repeat(MAX_NAME_LEN + 1))).is_err());
        assert_eq!(clean_remark(Some("   ")).expect("空串折 None"), None);
        assert_eq!(
            clean_remark(Some(" 一票一议 ")).expect("正常备注"),
            Some("一票一议".to_string())
        );
        assert!(clean_remark(Some(&"字".repeat(MAX_REMARK_LEN + 1))).is_err());
    }

    /// ⑩ 状态中文名：三个值全译，未知名原样返回（绝不 panic）。
    #[test]
    fn labels_cover_all_states() {
        assert_eq!(status_label(STATUS_DRAFT), "草稿");
        assert_eq!(status_label(STATUS_SUBMITTED), "审批中");
        assert!(status_label(STATUS_APPROVED).contains("生效"));
        assert_eq!(status_label("WEIRD"), "WEIRD");
        assert_eq!(price_mode_label(PRICE_MODE_FIXED), "固定价");
        assert_eq!(price_mode_label(PRICE_MODE_FLOATING), "随行就市");
        assert_eq!(price_mode_label("WEIRD"), "WEIRD");
    }
}
