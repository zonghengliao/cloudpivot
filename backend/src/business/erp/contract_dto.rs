//! 合同接口契约（**A2**）：分页筛选 / 新建 / 修改 / 状态流转 / 列表行 / 详情。
//!
//! 与 [`crate::business::erp::purchase_dto`] / [`crate::business::erp::dto`] 同一套约定：
//! 主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 日期 `NaiveDate`（`YYYY-MM-DD`）、金额 `Decimal`（**JSON 里是字符串**）、
//! 枚举一律大写字符串且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 字段名口径：`name` 与 `contract_name` 双写
//!
//! A2 卡面写的是 `name`，DDL（`V0195`）的列是 `contract_name`。两边都得能用：
//!
//! * **入参**：`CreateContractReq` / `UpdateContractReq` / `ContractFilter` 同时收
//!   `name` 与 `contract_name`，取**非空的那个**（[`pick_name`]）；两个都空 = 没传。
//! * **出参**：[`ContractRes`] 同时给 `name`（卡面字段名，前端 A5 直接读）与
//!   `contract_name`（DDL 列名，A3/A4 与 SQL 对照时读）。两个值恒等，
//!   单测 `list_row_exposes_both_name_spellings` 钉住，避免哪天只改一边。
//!
//! # 与卡面的四处出入（都以「已落库的 DDL + ARCH-2」为准）
//!
//! | 卡面 | 实际 | 处置 |
//! |---|---|---|
//! | `status` 含 `CANCELLED` | DDL 白名单是 `DRAFT/SIGNED/ACTIVE/COMPLETED/TERMINATED`（草稿作废走软删） | 拒绝 `CANCELLED`，400 里点名可用状态 |
//! | `amount >= 0` | `ck_biz_contract_amount CHECK (amount > 0)`，DEC-04 明确不支持 0 元 / 不限额框架合同 | `amount <= 0` 一律 400 |
//! | 详情「条款 / 附件按 DDL 有什么给什么」 | DDL **没有** `terms` / `attachment` 列（23 列见 V0195） | 详情不给这两个字段，已在 A2 交付说明里报告 |
//! | 乙方名称「走关联带出，不存名称文本列」 | DDL **有** `party_b_name` 快照列（审计口径：签约时乙方是谁） | 两个都给：`party_b_name`（快照）+ `party_b_current_name`（主档现值，`LEFT JOIN biz_supplier`） |
//!
//! # 校验分工
//!
//! * 只依赖「请求本身」的规则（必填 / 长度 / 枚举 / 金额 / 日期顺序）在本文件，
//!   纯函数、无 DB、可单测（[`CreateContractReq::validate`]）；
//! * 依赖库的规则（乙方存在性、单号唯一）在 `contract_handler`；
//! * 落在 DDL 里的规则（`amount > 0`、日期顺序、`SIGNED` 起日期三件套必填）两边都做：
//!   应用层先拦成 400，DB 的 CHECK 是最后一道闸（漏了会 500，不会静默写脏）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::contract_model::{self, ContractRow, STATUS_DRAFT, STATUSES, USABLE_FOR_ORDER};
use crate::business::shared::state_machine::Entity;
use crate::shared::error_code;

// ==================== 口径常量 ====================

/// 默认每页条数（卡面 20，与其余 `/page` 接口一致）。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 每页条数硬上限（**卡面 100**；比 erp 主数据的 200 更紧，合同行带快照文本且是列表主表）。
pub const MAX_PAGE_SIZE: u64 = 100;

/// 合同名称长度上限（`contract_name VARCHAR(200)`）。
pub const MAX_NAME_LEN: usize = 200;

/// 备注长度上限（列是 `TEXT`，这里按业务口径限 2000 字，防一次塞进整篇文档）。
pub const MAX_REMARK_LEN: usize = 2000;

/// 状态流转原因长度上限（写 `biz_status_log.reason`）。
pub const MAX_REASON_LEN: usize = 500;

/// 合同类型白名单（与 `ck_biz_contract_type` 逐字一致）。
///
/// `PURCHASE` 的乙方是供应商，`SALES` 的乙方是客户（客户主数据 `biz_customer`
/// 已随销售链 SO-02 上线，SO-26 起真正放行）；`SERVICE` / `LEASE` 的乙方**固定供应商**
/// （服务商 / 出租方在系统中登记为 `biz_supplier`，单向强制，应用层校验在
/// [`contract_model::required_party_b_type`]，理由见 `docs/合同类型开放口径-决策.md` §二）。
/// 这两类**不挂**采购单 / 销售单，详情用 `commitment_scope = NOT_APPLICABLE`
/// 与「真没下单」区分（同文档 §四）。DDL 的 `ck_biz_contract_type_party_pair` ELSE 分支
/// 仍允许两值 —— DB 宽容、应用层收窄，本卡零迁移。
pub const CONTRACT_TYPES: [&str; 4] = ["PURCHASE", "SALES", "SERVICE", "LEASE"];

/// 本批次真正放行的合同类型（CTR-02-F1：`SERVICE` / `LEASE` 开放，四类全开）。
///
/// 取值与 [`CONTRACT_TYPES`] 一致，但**保留独立常量与 [`ensure_supported_type`]**：
/// 将来新增枚举（如 `FRAMEWORK`）要先落 `ck_biz_contract_type` 白名单，再在这里放开；
/// 「DDL 有白名单」不等于「业务已开」。
pub const SUPPORTED_TYPES: [&str; 4] = CONTRACT_TYPES;

/// 默认币种（与 DDL 的 `DEFAULT 'CNY'` 一致）。
pub const DEFAULT_CURRENCY: &str = "CNY";

// ==================== 错误构造 ====================

/// 参数错误（400 + `10001`）：合同域的**入参**问题一律走它。
///
/// 业务语义错误另有专用码（`66014`–`66021`，见 `shared/error_code`），
/// 不要为了「看起来更专业」把 `name 不能为空` 也塞进 `66014` ——
/// 前端按错误码做差异化提示，混码等于让提示失效。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 通用小工具 ====================

/// `name` / `contract_name` 双写口径：取第一个非空值（[`pick_name`] 的单一实现）。
fn pick_name<'a>(name: Option<&'a str>, contract_name: Option<&'a str>) -> Option<&'a str> {
    let first = name.map(str::trim).filter(|text| !text.is_empty());
    first.or_else(|| contract_name.map(str::trim).filter(|text| !text.is_empty()))
}

/// 文本字段：必填 + 长度上限（按**字符数**，不是字节数）。
fn require_text(value: Option<&str>, field: &str, max: usize) -> std::result::Result<String, ApiError> {
    let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
        return Err(bad_request(format!("{field} 不能为空")));
    };
    if text.chars().count() > max {
        return Err(bad_request(format!(
            "{field} 最长 {max} 个字符（当前 {} 个）",
            text.chars().count()
        )));
    }
    Ok(text.to_string())
}

/// 选填文本：`None` / 空串放过；有值则限长（超长同样拒，避免被 `VARCHAR(n)` 静默截断）。
fn check_optional_text(value: Option<&str>, field: &str, max: usize) -> std::result::Result<(), ApiError> {
    match value.map(str::trim) {
        Some(text) if !text.is_empty() && text.chars().count() > max => Err(bad_request(format!(
            "{field} 最长 {max} 个字符（当前 {} 个）",
            text.chars().count()
        ))),
        _ => Ok(()),
    }
}

/// 类型闸：`contract_type` 是否在本期放行集里（纯函数，handler 与单测共用同一出口）。
///
/// 与 [`require_one_of`] 的关系：那条判「是不是合法枚举值」（[`CONTRACT_TYPES`]，
/// 与 `ck_biz_contract_type` 逐字一致），本函数判「业务开没开」。CTR-02-F1 起两者取值
/// 一致（四类全开），但闸仍保留：未来新枚举必须两处一起放开 —— 「DDL 允许」不等于
/// 「业务已开」。
///
/// # 异常情况
///
/// * 400 + `10001`：不在 [`SUPPORTED_TYPES`] 里（未来新枚举尚未开放）。
pub fn ensure_supported_type(contract_type: &str) -> std::result::Result<(), ApiError> {
    if SUPPORTED_TYPES.contains(&contract_type) {
        return Ok(());
    }
    Err(bad_request(format!(
        "contract_type `{contract_type}` 不在 ck_biz_contract_type 白名单内（当前白名单与放行集一致：{}）",
        SUPPORTED_TYPES.join(" / ")
    )))
}

/// 乙方类型与合同类型的配对校验（**单向强制**，POST 与 PUT 共用同一出口）。
///
/// 规则（`docs/合同类型开放口径-决策.md` §二 D1）：
/// `PURCHASE` / `SERVICE` / `LEASE` → `SUPPLIER`；`SALES` → `CUSTOMER`。
/// 服务商 / 出租方在系统中登记为**供应商**，不新建主档；DDL 的
/// `ck_biz_contract_type_party_pair` ELSE 分支仍允许两值（DB 宽容、应用层收窄）。
///
/// # 异常情况
///
/// * 400 + `10001`：值与合同类型要求的类型冲突（含 `SERVICE` / `LEASE` 传 `CUSTOMER`）；
/// * 400 + `10001`：不是 `SUPPLIER` / `CUSTOMER` 的第三个值。
pub fn ensure_party_b_type(contract_type: &str, party_b_type: &str) -> std::result::Result<&'static str, ApiError> {
    let required = contract_model::required_party_b_type(contract_type);
    if party_b_type == required {
        return Ok(required);
    }
    if party_b_type == contract_model::PARTY_B_SUPPLIER || party_b_type == contract_model::PARTY_B_CUSTOMER {
        return Err(party_type_conflict(contract_type, required, party_b_type));
    }
    Err(bad_request(format!(
        "party_b_type 只能是 SUPPLIER / CUSTOMER（当前 `{party_b_type}`）"
    )))
}

/// 乙方类型冲突的提示文案（按合同类型说清「为什么必须是它」，别让服务合同的报错说「客户」）。
fn party_type_conflict(contract_type: &str, required: &str, actual: &str) -> ApiError {
    let (label, reason) = match contract_type {
        "SALES" => ("销售（SALES）合同", "乙方是客户（biz_customer）"),
        "SERVICE" => ("服务（SERVICE）合同", "服务商在系统中登记为供应商"),
        "LEASE" => ("租赁（LEASE）合同", "出租方在系统中登记为供应商"),
        _ => ("采购（PURCHASE）合同", "乙方是供应商（biz_supplier）"),
    };
    bad_request(format!(
        "{label}的乙方类型必须是 {required}（{reason}），与 party_b_type=`{actual}` 冲突"
    ))
}

/// 枚举白名单。
fn require_one_of(value: &str, allowed: &[&str], field: &str) -> std::result::Result<(), ApiError> {
    if allowed.contains(&value) {
        return Ok(());
    }
    Err(bad_request(format!(
        "{field} 必须是 {} 之一（当前 `{value}`）",
        allowed.join(" / ")
    )))
}

/// 金额：必须 `> 0`（DDL 是 `CHECK (amount > 0)`，0 元 / 不限额框架合同本批次不支持）。
fn require_positive_amount(value: Decimal) -> std::result::Result<(), ApiError> {
    if value <= Decimal::ZERO {
        return Err(bad_request(format!(
            "amount 必须大于 0（当前 {value}）：DDL 的 ck_biz_contract_amount 是 amount > 0，\
             0 元 / 不限额框架合同本批次不支持（DEC-04）"
        )));
    }
    // NUMERIC(18,4)：整数位最多 14 位，超了 DB 会报 22003（数值越界）→ 提前 400
    if value.trunc().abs() >= Decimal::from(100_000_000_000_000_i64) {
        return Err(bad_request(format!(
            "amount 超出 NUMERIC(18,4) 的整数位上限（14 位，当前 {value}）"
        )));
    }
    if value.normalize().scale() > 4 {
        return Err(bad_request(format!(
            "amount 最多 4 位小数（NUMERIC(18,4)，当前 {value}）"
        )));
    }
    Ok(())
}

/// 日期区间：两端都给了才比较（`end >= start`，与 `ck_biz_contract_date_order` 同口径）。
fn require_date_order(
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    start_field: &str,
    end_field: &str,
) -> std::result::Result<(), ApiError> {
    if let (Some(start), Some(end)) = (start, end)
        && end < start
    {
        return Err(bad_request(format!(
            "{end_field}（{end}）不能早于 {start_field}（{start}）：区间反了"
        )));
    }
    Ok(())
}

/// 签订日期不晚于开始日期（**业务口径，DDL 里没有这条 CHECK**）。
///
/// 卡面 A2「签订日期不能晚于开始日期（按 DDL 的 CHECK 来）」—— 实测 DDL 里
/// **没有**这条约束（`V0195` 的 9 个 CHECK 里只有日期顺序与「SIGNED 起三件套必填」）。
/// 既然卡面要求，就在应用层拦：400 + 文案。
///
/// ⚠️ 代价：**补签（生效日早于签订日）会被拦住**。若业务上允许倒签，
/// 需要总管裁决后放开本函数（已写进 A2 交付说明的「顺带报告」）。
fn require_sign_not_after_start(
    sign: Option<NaiveDate>,
    start: Option<NaiveDate>,
) -> std::result::Result<(), ApiError> {
    if let (Some(sign), Some(start)) = (sign, start)
        && sign > start
    {
        return Err(bad_request(format!(
            "sign_date（{sign}）不能晚于 start_date（{start}）：先签约再生效"
        )));
    }
    Ok(())
}

/// 币种：ISO 4217 三位大写（`ck_biz_contract_currency CHECK (currency_code ~ '^[A-Z]{3}$')`）。
fn require_currency(value: &str) -> std::result::Result<String, ApiError> {
    let text = value.trim().to_uppercase();
    if text.len() != 3 || !text.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(bad_request(format!(
            "currency_code 必须是 3 位大写字母（ISO 4217，当前 `{value}`）"
        )));
    }
    Ok(text)
}

/// `double_option`：区分「没传」（`None`）与「显式传 null」（`Some(None)` = 清空）。
///
/// 与 [`crate::business::erp::dto`] 里的同名函数同源（那边是私有的，各模块各留一份，
/// 避免为了一个 5 行的反序列化助手把 `dto.rs` 的私有件改成 `pub`）。
fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// 分页口径归一化：页码从 1 起，`page_size = 0` 回落 20，**上限 100**（卡面）。
///
/// 放在 dto 而不是 model：这是**对外契约**的一部分（前端要能预测上限），
/// 而 model 只管取数。handler 与 model 都调本函数，两边不会各 clamp 一套。
pub fn normalize_page(page: u64, page_size: u64) -> (u64, u64) {
    let page = page.max(1);
    let size = if page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page_size.min(MAX_PAGE_SIZE)
    };
    (page, size)
}

// ==================== 分页筛选 ====================

/// 合同分页筛选（`GET /business/erp/contract/page` 的 query string）。
///
/// 全部字段可缺省；`page` / `page_size` 缺省为 1 / 20。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ContractFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数（上限 100）。
    pub page_size: u64,
    /// 合同号模糊（前缀优先：`LIKE 'HT-2026%'` 走 `uk_biz_contract_no` 的前缀区间）。
    pub contract_no: Option<String>,
    /// 合同名称模糊（卡面字段名）。
    pub name: Option<String>,
    /// 合同名称模糊（DDL 列名，与 `name` 同义；两个都传以 `name` 为准）。
    pub contract_name: Option<String>,
    /// 合同类型（`PURCHASE` / `SALES` / `SERVICE` / `LEASE`）。
    pub contract_type: Option<String>,
    /// 状态（`DRAFT` / `SIGNED` / `ACTIVE` / `COMPLETED` / `TERMINATED`）。
    pub status: Option<String>,
    /// 乙方 id（本期 = 供应商 id）。
    pub party_b_id: Option<String>,
    /// 签订日期 ≥（`YYYY-MM-DD`）。
    pub sign_date_from: Option<String>,
    /// 签订日期 ≤。
    pub sign_date_to: Option<String>,
    /// 到期日期 ≥（落 `end_date` 列）。
    pub expire_date_from: Option<String>,
    /// 到期日期 ≤（落 `end_date` 列）。
    pub expire_date_to: Option<String>,
    /// 关键词：合同号**或**名称任一命中。
    pub keyword: Option<String>,
}

impl ContractFilter {
    /// 名称模糊条件（`name` 优先，其次 `contract_name`）。
    pub fn name_text(&self) -> Option<&str> {
        pick_name(self.name.as_deref(), self.contract_name.as_deref())
    }

    /// 解析并校验成 model 用的 [`ContractQuery`]。
    ///
    /// # 异常情况
    ///
    /// * 400 + `10001`：日期格式不对 / 区间反了 / 枚举不在白名单 / `party_b_id` 不是 UUID。
    pub fn parse(&self) -> std::result::Result<ContractQuery, ApiError> {
        if let Some(contract_type) = self.contract_type.as_deref() {
            require_one_of(contract_type.trim(), &CONTRACT_TYPES, "contract_type")?;
        }
        if let Some(status) = self.status.as_deref() {
            let status = status.trim();
            require_one_of(status, &STATUSES, "status")?;
        }
        let party_b_id = match self.party_b_id.as_deref().map(str::trim) {
            Some(raw) if !raw.is_empty() => Some(parse_uuid_text(raw, "party_b_id")?),
            _ => None,
        };
        let sign_from = parse_date(self.sign_date_from.as_deref(), "sign_date_from")?;
        let sign_to = parse_date(self.sign_date_to.as_deref(), "sign_date_to")?;
        let expire_from = parse_date(self.expire_date_from.as_deref(), "expire_date_from")?;
        let expire_to = parse_date(self.expire_date_to.as_deref(), "expire_date_to")?;
        require_date_order(sign_from, sign_to, "sign_date_from", "sign_date_to")?;
        require_date_order(expire_from, expire_to, "expire_date_from", "expire_date_to")?;
        let (page, page_size) = normalize_page(self.page, self.page_size);
        Ok(ContractQuery {
            page,
            page_size,
            contract_no: non_empty(self.contract_no.as_deref()),
            name: self.name_text().map(|text| text.to_string()),
            contract_type: non_empty(self.contract_type.as_deref()),
            status: non_empty(self.status.as_deref()),
            party_b_id,
            sign_from,
            sign_to,
            expire_from,
            expire_to,
            keyword: non_empty(self.keyword.as_deref()),
        })
    }
}

/// 去空白后为空则视为没传。
fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.to_string())
}

/// `YYYY-MM-DD` → `NaiveDate`；空 = 没传。
fn parse_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|err| bad_request(format!("{field} 不是合法日期 `{text}`（YYYY-MM-DD）：{err}"))),
    }
}

/// 路径 / 请求体里的 UUID 文本 → `Uuid`（非法 → 400，绝不静默当没传）。
pub fn parse_uuid_text(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|err| bad_request(format!("{field} 不是合法 UUID `{text}`：{err}")))
}

/// 选填 UUID：空 / 缺省 → `None`；非法 → 400。
pub fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => parse_uuid_text(text, field).map(Some),
    }
}

/// 校验并解析后的分页查询（model 的入参，避免 model 里再解析字符串）。
#[derive(Debug, Clone, Default)]
pub struct ContractQuery {
    /// 页码。
    pub page: u64,
    /// 每页条数（已 clamp 到 ≤ 100）。
    pub page_size: u64,
    /// 合同号模糊。
    pub contract_no: Option<String>,
    /// 名称模糊。
    pub name: Option<String>,
    /// 类型。
    pub contract_type: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 乙方 id。
    pub party_b_id: Option<Uuid>,
    /// 签订日期下限。
    pub sign_from: Option<NaiveDate>,
    /// 签订日期上限。
    pub sign_to: Option<NaiveDate>,
    /// 到期日期下限。
    pub expire_from: Option<NaiveDate>,
    /// 到期日期上限。
    pub expire_to: Option<NaiveDate>,
    /// 合同号或名称关键词。
    pub keyword: Option<String>,
}

// ==================== 新建 ====================

/// 新建合同请求（`POST /business/erp/contract`）。
///
/// **不接受** `status` / `contract_no`：状态恒为 `DRAFT`、单号由 `biz_seq(CT)` 取，
/// serde 忽略未知字段 ⇒ 客户端硬塞会被静静丢掉（单测 `create_ignores_client_status_and_no` 钉住）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreateContractReq {
    /// 合同名称（卡面字段名；与 `contract_name` 二选一必填）。
    pub name: Option<String>,
    /// 合同名称（DDL 列名）。
    pub contract_name: Option<String>,
    /// 合同类型（`PURCHASE` / `SALES` / `SERVICE` / `LEASE`，必填）。
    pub contract_type: Option<String>,
    /// 甲方 id（缺省 = 本组织）。
    pub party_a_id: Option<String>,
    /// 甲方名称快照。
    pub party_a_name: Option<String>,
    /// 乙方 id（必填；新建时必须指向启用中的主档：
    /// `party_b_type=SUPPLIER` → `biz_supplier`、`CUSTOMER` → `biz_customer`）。
    pub party_b_id: Option<String>,
    /// 乙方类型（缺省按合同类型推导：`SALES` → `CUSTOMER`，其余 → `SUPPLIER`）。
    /// 服务 / 租赁**只能**是 `SUPPLIER`（服务商 / 出租方登记为供应商）。
    pub party_b_type: Option<String>,
    /// 合同金额（必填，> 0）。
    pub amount: Option<Decimal>,
    /// 币种（缺省 `CNY`）。
    pub currency_code: Option<String>,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 生效日。
    pub start_date: Option<NaiveDate>,
    /// 到期日。
    pub end_date: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
    /// 负责人。
    pub owner_id: Option<String>,
    /// 归属部门。
    pub dept_id: Option<String>,
}

impl CreateContractReq {
    /// 名称（`name` 优先，其次 `contract_name`）。
    pub fn display_name(&self) -> Option<&str> {
        pick_name(self.name.as_deref(), self.contract_name.as_deref())
    }

    /// 只依赖请求本身的校验（无 DB）。
    ///
    /// # 异常情况
    ///
    /// * 400 + `10001`：缺必填 / 超长 / 枚举非法 / 类型不在放行集（见
    ///   [`ensure_supported_type`]，当前四类全放行）/
    ///   金额 ≤ 0 / 日期区间反了 / 签订晚于生效 / 币种非法。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(self.display_name(), "name", MAX_NAME_LEN)?;
        let contract_type = require_text(self.contract_type.as_deref(), "contract_type", 16)?;
        require_one_of(&contract_type, &CONTRACT_TYPES, "contract_type")?;
        // 类型闸（业务开没开）：CTR-02-F1 起四类全放行，但闸保留给未来新枚举
        ensure_supported_type(&contract_type)?;
        if self.party_b_id.as_deref().map(str::trim).unwrap_or("").is_empty() {
            return Err(bad_request("party_b_id 不能为空"));
        }
        let Some(amount) = self.amount else {
            return Err(bad_request("amount 不能为空"));
        };
        require_positive_amount(amount)?;
        if let Some(currency) = self.currency_code.as_deref() {
            require_currency(currency)?;
        }
        require_date_order(self.start_date, self.end_date, "start_date", "end_date")?;
        require_sign_not_after_start(self.sign_date, self.start_date)?;
        check_optional_text(self.remark.as_deref(), "remark", MAX_REMARK_LEN)?;
        check_optional_text(self.party_a_name.as_deref(), "party_a_name", MAX_NAME_LEN)?;
        Ok(())
    }

    /// 合同类型（已 trim）。
    pub fn contract_type_value(&self) -> &str {
        self.contract_type.as_deref().map(str::trim).unwrap_or_default()
    }

    /// 乙方类型：客户端显式给了就校验，没给按合同类型推导。
    ///
    /// * `SALES` → `CUSTOMER`（`party_b_id` 指向 `biz_customer.id`）
    /// * 其余（`PURCHASE` / `SERVICE` / `LEASE`）→ `SUPPLIER`
    ///   （服务商 / 出租方在系统中登记为供应商，**单向强制**，见
    ///   `docs/合同类型开放口径-决策.md` §二 D1；DDL 的 ELSE 分支仍宽容两值）
    ///
    /// # 异常情况
    ///
    /// * 400 + `10001`：传了非 `SUPPLIER` / `CUSTOMER` 的第三个值；
    ///   或显式值与本类型要求的乙方主档类型冲突（`SALES` 传 `SUPPLIER`、
    ///   `SERVICE` / `LEASE` 传 `CUSTOMER`）。
    pub fn resolve_party_b_type(&self) -> std::result::Result<&'static str, ApiError> {
        let derived = contract_model::required_party_b_type(self.contract_type_value());
        match self.party_b_type.as_deref().map(str::trim) {
            None | Some("") => Ok(derived),
            Some(text) => ensure_party_b_type(self.contract_type_value(), text),
        }
    }

    /// 拼成 model 的写入结构（乙方 id / 名称都用**服务端解析到的值**，不用客户端传的名称）。
    ///
    /// 甲方 / 负责人 / 部门三个可选弱引用由调用方解析后回填（`party_a_id` / `owner_id` /
    /// `dept_id`），避免本函数为了三个 `Option<Uuid>` 再收三个参数。
    pub fn into_new(
        &self,
        party_b_id: Uuid,
        party_b_name: String,
        party_b_type: &str,
        operator: Option<Uuid>,
    ) -> contract_model::NewContract {
        contract_model::NewContract {
            contract_name: self.display_name().unwrap_or_default().trim().to_string(),
            contract_type: self.contract_type_value().to_string(),
            party_a_id: None,
            party_a_name: non_empty(self.party_a_name.as_deref()),
            party_b_type: party_b_type.to_string(),
            party_b_id,
            party_b_name,
            amount: self.amount.unwrap_or(Decimal::ZERO),
            currency_code: self
                .currency_code
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| text.to_uppercase())
                .unwrap_or_else(|| DEFAULT_CURRENCY.to_string()),
            sign_date: self.sign_date,
            start_date: self.start_date,
            end_date: self.end_date,
            remark: non_empty(self.remark.as_deref()),
            owner_id: None,
            dept_id: None,
            operator,
        }
    }
}

// ==================== 修改 ====================

/// 修改合同请求（`PUT /business/erp/contract/{id}`）。
///
/// 只收**可改字段**（卡面 + `ARCH-2 §5.3`）：名称 / 乙方 / 金额 / 三个日期 / 备注。
/// `contract_no` / `status` / `create_by` / `created_at` 不在这里 —— 传了也被 serde 丢掉，
/// 状态只能走 `PATCH /{id}/status`。
///
/// 嵌套 `Option` 的语义（与 erp 其它 `UpdateXxxReq` 一致）：
/// * 字段**没出现** → `None`（保持原值）
/// * 字段**显式传 null** → `Some(None)`（清空；日期与备注可清空）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpdateContractReq {
    /// 合同名称（卡面字段名）。
    pub name: Option<String>,
    /// 合同名称（DDL 列名）。
    pub contract_name: Option<String>,
    /// 乙方 id。
    pub party_b_id: Option<String>,
    /// 乙方类型（校验用，且必须与合同类型配对。
    /// `contract_type` 在草稿阶段也不可改，所以这里只做**一致性校验**：
    /// `SERVICE` / `LEASE` 传 `CUSTOMER` → 400）。
    pub party_b_type: Option<String>,
    /// 合同金额（> 0）。
    pub amount: Option<Decimal>,
    /// 签订日期（`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub sign_date: Option<Option<NaiveDate>>,
    /// 生效日（`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub start_date: Option<Option<NaiveDate>>,
    /// 到期日（`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub end_date: Option<Option<NaiveDate>>,
    /// 备注（`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdateContractReq {
    /// 名称（`name` 优先）。
    pub fn display_name(&self) -> Option<&str> {
        pick_name(self.name.as_deref(), self.contract_name.as_deref())
    }

    /// 只校验「给了的字段」（合并后的整体校验在 handler 里做）。
    ///
    /// # 异常情况
    ///
    /// * 400 + `10001`：字段超长 / 空白名称 / 金额 ≤ 0 / 日期区间反了 / 签订晚于生效。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if self.name.is_some() || self.contract_name.is_some() {
            require_text(self.display_name(), "name", MAX_NAME_LEN)?;
        }
        if let Some(amount) = self.amount {
            require_positive_amount(amount)?;
        }
        if let Some(remark) = self.remark.as_ref().and_then(|value| value.as_deref()) {
            check_optional_text(Some(remark), "remark", MAX_REMARK_LEN)?;
        }
        Ok(())
    }
}

// ==================== 状态流转 ====================

/// 状态流转请求（`PATCH /business/erp/contract/{id}/status`）。
///
/// ```json
/// { "target_status": "PENDING_APPROVAL", "reason": "可选，两条终止边必填" }
/// ```
///
/// # `target_status = PENDING_APPROVAL` 就是「提交审批」（R-P1-1）
///
/// 不另开一个 `/submit-approval` 端点，理由：**合同的每一次状态变化都必须过状态机**
/// （ARCH-2 DEC-10「状态变更只有一个入口」）。多一个端点 = 多一条能绕开边表的写库路径。
/// 于是「提交审批」在协议层就是一条普通流转，只是这条流转在事务里多做两件事：
/// 起 `contract_approve` 审批实例 + 把实例 id 回写到合同。
///
/// 没配流程时的「显式跳过审批」也复用同一入口的 `skip_approval` / `skip_reason`
/// 两个字段（口径与采购单 / 收货单的 `approval_gate::SkipApprovalReq` 逐字一致）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ContractStatusReq {
    /// 目标状态（必填）。
    pub target_status: Option<String>,
    /// 目标状态（兼容卡面 `status` 拼写；两者都传以 `target_status` 为准）。
    pub status: Option<String>,
    /// 原因（`SIGNED→TERMINATED` / `ACTIVE→TERMINATED` 必填）。
    pub reason: Option<String>,
    /// 「显式跳过审批」片段（只有「没发布 contract_approve 流程」时才可能生效）。
    #[serde(flatten)]
    pub gate: crate::business::erp::approval_gate::SkipApprovalReq,
}

impl ContractStatusReq {
    /// 目标状态（已 trim、已过白名单）。
    ///
    /// # 异常情况
    ///
    /// * 400 + `10001`：缺 target_status / 不在 5 态白名单里（含卡面写错的 `CANCELLED`）。
    pub fn target(&self) -> std::result::Result<&str, ApiError> {
        let raw = self
            .target_status
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .or_else(|| self.status.as_deref().map(str::trim).filter(|text| !text.is_empty()))
            .ok_or_else(|| bad_request("target_status 不能为空"))?;
        require_one_of(raw, &STATUSES, "target_status")?;
        Ok(raw)
    }

    /// 原因（去空白、空串视为没给）。
    pub fn reason_text(&self) -> Option<&str> {
        self.reason.as_deref().map(str::trim).filter(|text| !text.is_empty())
    }
}

// ==================== 出参 ====================

/// 列表行（`GET /page` 的 `list` 元素）。
///
/// `name` 与 `contract_name` 同值（见模块文档的「双写」）；`party_b_name` 是**签约快照**，
/// `party_b_current_name` 是**主档现值**（供应商改名后两者会不同，审计要前者）。
#[derive(Debug, Clone, Serialize)]
pub struct ContractRes {
    /// 主键。
    pub id: String,
    /// 合同号（`HT-yyyyMMdd-000001`，只用于显示与模糊查，不参与关联）。
    pub contract_no: String,
    /// 合同名称（卡面字段名）。
    pub name: String,
    /// 合同名称（DDL 列名，与 `name` 同值）。
    pub contract_name: String,
    /// 合同类型。
    pub contract_type: String,
    /// 乙方类型。
    pub party_b_type: String,
    /// 乙方 id（弱引用，无外键）。
    pub party_b_id: String,
    /// 乙方名称快照（签约时）。
    pub party_b_name: String,
    /// 乙方名称现值（`LEFT JOIN biz_supplier`；主档已删 / 不存在时为 `null`）。
    pub party_b_current_name: Option<String>,
    /// 合同金额。
    pub amount: Decimal,
    /// 币种。
    pub currency_code: String,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 生效日。
    pub start_date: Option<NaiveDate>,
    /// 到期日。
    pub end_date: Option<NaiveDate>,
    /// 状态。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 提交审批的实例 id（R-P1-1；`null` = 从未提交过审批）。
    pub approval_instance_id: Option<String>,
    /// 审批实例单号（`FLW-…`；与 `approval_instance_id` 同源，列表直接展示
    /// 「审批中（FLW-xxx）」；实例查不到时为 `null`）。
    pub approval_instance_no: Option<String>,
    /// 最后更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&ContractRow> for ContractRes {
    fn from(row: &ContractRow) -> Self {
        Self {
            id: row.id.to_string(),
            contract_no: row.contract_no.clone(),
            name: row.contract_name.clone(),
            contract_name: row.contract_name.clone(),
            contract_type: row.contract_type.clone(),
            party_b_type: row.party_b_type.clone(),
            party_b_id: row.party_b_id.to_string(),
            party_b_name: row.party_b_name.clone(),
            party_b_current_name: row.party_b_current_name.clone(),
            amount: row.amount,
            currency_code: row.currency_code.trim().to_string(),
            sign_date: row.sign_date,
            start_date: row.start_date,
            end_date: row.end_date,
            status: row.status.clone(),
            status_label: contract_model::status_label(&row.status).to_string(),
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            // 单号要查一次实例表：`From<&ContractRow>` 只拿得到 id。
            // 列表 / 详情都由 `contract_handler::fill_approval_no` 批量回填。
            approval_instance_no: None,
            updated_at: row.updated_at,
        }
    }
}

/// 详情（`GET /business/erp/contract/{id}`、以及三个写接口的回读）。
///
/// 列表字段全集 + DDL 里剩下的可读列（甲方便照、负责人、部门、备注、审计四件）。
/// **没有** `terms` / `attachment`：DDL 没有这两列（见模块文档）。
#[derive(Debug, Clone, Serialize)]
pub struct ContractDetailRes {
    /// 列表行的全部字段。
    #[serde(flatten)]
    pub base: ContractRes,
    /// 甲方 id（`NULL` = 本组织）。
    pub party_a_id: Option<String>,
    /// 甲方名称快照。
    pub party_a_name: Option<String>,
    /// 负责人。
    pub owner_id: Option<String>,
    /// 负责人姓名（AUD-02：实时 join `sys_user` 取现值，改名后这里跟着变）。
    ///
    /// `owner_id` 为空 / 账号已被物理清理 → `None`（前端显示 `—`，别回退 id）。
    pub owner_name: Option<String>,
    /// 归属部门。
    pub dept_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<String>,
    /// 创建人姓名（AUD-02：实时口径，见 `business/shared/user_name.rs`）。
    pub create_by_name: Option<String>,
    /// 最后修改人。
    pub update_by: Option<String>,
    /// 最后修改人姓名（AUD-02）。
    pub update_by_name: Option<String>,
    /// 能否编辑（= 状态为 DRAFT；`PUT` 的唯一放行条件，给前端置灰用）。
    pub can_edit: bool,
    /// 能否被采购单引用（`SIGNED` / `ACTIVE`；A3 的选择器用这个字段，别自己判状态）。
    pub usable_for_order: bool,
    /// 当前状态的合法去向（**由状态机给**，前端别硬编码边）。
    pub next_statuses: Vec<String>,
    /// **履约口径**（CTR-02-F1）：告诉前端「下面四个履约字段该怎么读」。
    ///
    /// * `ORDER_LINKED` —— `PURCHASE` / `SALES`：合同会挂采购单 / 销售单，
    ///   `committed_amount` 等是**真值**；
    /// * `NOT_APPLICABLE` —— `SERVICE` / `LEASE`：按口径**不挂**订单（订单侧类型闸只收
    ///   `PURCHASE` / `SALES`），因此 `committed_amount=0` / `remaining_amount=amount` /
    ///   `open_order_count=0` / `open_order_nos=[]` 是**正确值，不是「还没执行」**。
    ///   前端必须按本字段渲染（不得显示 `0.00` / 0% 履约率），报表按
    ///   `committed_amount` 算履约率前必须先过滤 `ORDER_LINKED`。
    ///
    /// 预留取值 `PAYMENT_PLAN`：合同付款计划成为履约单据后启用，见
    /// 《合同类型开放口径-决策.md》§四 D3 与需求池 RP-2026-005。
    ///
    /// 取值由 `contract_model::commitment_scope_of` 单点决定（纯函数，别在 handler 里散字面量）。
    pub commitment_scope: String,
    /// **已执行金额**（CTR-02 F6）：关联采购单 + 销售单的 `SUM(total_with_tax)`，现算。
    ///
    /// 与下单额度守卫（`purchase_model::committed_amount` /
    /// `sales_order_model::committed_amount`）**同一个口径**，所以「详情页的履约进度」
    /// 与「能不能再下单」永远对得上，前端别自己再 SUM 一遍。
    /// 口径细节（含作废 / 软删如何排除）见 `contract_model::ContractCommitment`。
    ///
    /// ⚠️ `commitment_scope = NOT_APPLICABLE`（服务 / 租赁）时**恒为 0**，这是正确值
    /// （不挂订单），不是「尚未执行」；机器可读值照常返回，显示由前端按口径决定。
    pub committed_amount: Decimal,
    /// **剩余额度** = `amount - committed_amount`，现算。
    ///
    /// **允许为负**：合同额被改小到低于已下单额时，负数就是「超签了多少」，
    /// 夹到 0 会把异常藏起来。
    pub remaining_amount: Decimal,
    /// 未关闭的下游单据数（CTR-02 F3）：采购单 ≠ `RECEIVED/CLOSED/CANCELLED`、
    /// 销售单 ≠ `OUTBOUND/CLOSED/CANCELLED` 的条数。
    ///
    /// 用途：归档（`ACTIVE → COMPLETED`）前给人一句「还有 N 张单没走完」。
    /// ⚠️ **它不是拦截条件**：按 ARCH-2 §4.1 E4「不硬拦」，
    /// `COMPLETED` 只表示**归档**（不再接新单），不代表履约完成；
    /// 报表要判「已履行」用 `open_order_count == 0`。
    pub open_order_count: i64,
    /// 未关闭下游单据的单号（最多 `contract_model::OPEN_ORDER_LIST_LIMIT` 个，按单号升序）。
    pub open_order_nos: Vec<String>,
}

impl ContractDetailRes {
    /// 把履约口径（现算结果）灌进详情。
    ///
    /// 为什么不在 `From<&ContractRow>` 里做：那是**纯函数**（不查库），
    /// 而这两个金额必须现算 SUM —— 放在 handler 的 [`crate::business::erp::contract_handler`]
    /// 里由 `detail_of` 统一调用，保证四个写接口的回读体口径一致。
    pub fn apply_commitment(&mut self, commitment: &contract_model::ContractCommitment) {
        self.committed_amount = commitment.committed_amount();
        self.remaining_amount = commitment.remaining_amount(self.base.amount);
        self.open_order_count = commitment.open_order_count;
        self.open_order_nos = commitment
            .open_orders
            .iter()
            .map(|order| order.order_no.clone())
            .collect();
    }
}

impl From<&ContractRow> for ContractDetailRes {
    fn from(row: &ContractRow) -> Self {
        let entity = Entity::Contract;
        Self {
            base: ContractRes::from(row),
            party_a_id: row.party_a_id.map(|id| id.to_string()),
            party_a_name: row.party_a_name.clone(),
            owner_id: row.owner_id.map(|id| id.to_string()),
            owner_name: None,
            dept_id: row.dept_id.map(|id| id.to_string()),
            remark: row.remark.clone(),
            created_at: row.created_at,
            create_by: row.create_by.map(|id| id.to_string()),
            create_by_name: None,
            update_by: row.update_by.map(|id| id.to_string()),
            update_by_name: None,
            can_edit: row.status == STATUS_DRAFT,
            usable_for_order: USABLE_FOR_ORDER.contains(&row.status.as_str()),
            next_statuses: entity
                .next_statuses(&row.status)
                .into_iter()
                .map(|status| status.to_string())
                .collect(),
            // CTR-02 F6 / F3：履约口径必须现算 SUM（本函数是纯函数、不查库），
            // 这里只给「一张单都没挂」的自洽默认值（已执行 0、剩余 = 合同额），
            // 真值由 `contract_handler::detail_of` 调 [`ContractDetailRes::apply_commitment`] 灌入。
            // CTR-02-F1：口径本身是纯函数（按下单能力分叉），这里就能定，
            // 服务 / 租赁 → NOT_APPLICABLE（`committed_amount=0` 是正确值，不是未执行）。
            commitment_scope: contract_model::commitment_scope_of(&row.contract_type).to_string(),
            committed_amount: Decimal::ZERO,
            remaining_amount: row.amount,
            open_order_count: 0,
            open_order_nos: Vec::new(),
        }
    }
}

/// AUD-02：详情里三个人字段（负责人 / 制单人 / 修改人）的姓名回填，
/// 由 `business/shared/user_name.rs` 的批量入口统一驱动（一页/一单恒定 1 条 SQL）。
impl crate::business::shared::user_name::PersonNameRow for ContractDetailRes {
    fn person_ids(&self, out: &mut Vec<Uuid>) {
        for raw in [
            self.owner_id.as_deref(),
            self.create_by.as_deref(),
            self.update_by.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Ok(id) = Uuid::parse_str(raw) {
                out.push(id);
            }
        }
    }

    fn apply_names(&mut self, names: &std::collections::HashMap<Uuid, String>) {
        let name_of = |raw: Option<&str>| -> Option<String> {
            raw.and_then(|value| Uuid::parse_str(value).ok())
                .and_then(|id| names.get(&id).cloned())
        };
        self.owner_name = name_of(self.owner_id.as_deref());
        self.create_by_name = name_of(self.create_by.as_deref());
        self.update_by_name = name_of(self.update_by.as_deref());
    }
}

// ==================== 纯逻辑单测（无需数据库） ====================
#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期夹具")
    }

    fn base_req() -> CreateContractReq {
        CreateContractReq {
            name: Some("A2 采购合同".to_string()),
            contract_type: Some("PURCHASE".to_string()),
            party_b_id: Some("018f4b2c-1111-7222-8333-444455556666".to_string()),
            amount: Some(Decimal::new(100_000, 2)),
            sign_date: Some(date("2026-01-01")),
            start_date: Some(date("2026-01-02")),
            end_date: Some(date("2026-12-31")),
            ..Default::default()
        }
    }

    /// ① 缺名称 → 400 + 10001（卡面 A2 验收第 2 条）。
    #[test]
    fn create_without_name_is_rejected() {
        let req = CreateContractReq {
            name: None,
            contract_name: Some("   ".to_string()),
            ..base_req()
        };
        let err = req.validate().expect_err("空名称必须拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("name"), "文案要点名字段：{}", err.msg);
    }

    /// ② 缺类型 / 类型非法 → 400。
    #[test]
    fn create_with_bad_contract_type_is_rejected() {
        let missing = CreateContractReq {
            contract_type: None,
            ..base_req()
        };
        assert_eq!(missing.validate().expect_err("缺类型").status, 400);

        let unknown = CreateContractReq {
            contract_type: Some("FRAMEWORK".to_string()),
            ..base_req()
        };
        let err = unknown.validate().expect_err("未知类型必须拒");
        assert!(err.msg.contains("contract_type"), "{}", err.msg);
    }

    /// ③ 负金额 / 0 元 → 400（卡面 A2 验收第 4 条；DDL 是 `amount > 0`）。
    #[test]
    fn create_with_non_positive_amount_is_rejected() {
        for amount in [Decimal::new(-1, 0), Decimal::ZERO] {
            let req = CreateContractReq {
                amount: Some(amount),
                ..base_req()
            };
            let err = req.validate().expect_err("非正金额必须拒");
            assert_eq!(err.status, 400, "amount={amount}");
            assert!(err.msg.contains("amount"), "{}", err.msg);
        }
    }

    /// ④ 金额小数位 > 4 → 400（NUMERIC(18,4) 会静默四舍五入，宁可拒）。
    #[test]
    fn create_with_too_many_decimals_is_rejected() {
        let req = CreateContractReq {
            amount: Some(Decimal::new(1_000_001, 5)), // 10.00001
            ..base_req()
        };
        let err = req.validate().expect_err("5 位小数必须拒");
        assert!(err.msg.contains("4 位小数"), "{}", err.msg);
    }

    /// ⑤ 日期区间反了 → 400（卡面 A2 验收第 5 条）。
    #[test]
    fn create_with_reversed_date_range_is_rejected() {
        let req = CreateContractReq {
            start_date: Some(date("2026-12-31")),
            end_date: Some(date("2026-01-01")),
            ..base_req()
        };
        let err = req.validate().expect_err("区间反了必须拒");
        assert!(err.msg.contains("不能早于"), "{}", err.msg);
    }

    /// ⑥ 签订日期晚于生效日 → 400（卡面要求；DDL 无此 CHECK，应用层拦）。
    #[test]
    fn create_with_sign_date_after_start_is_rejected() {
        let req = CreateContractReq {
            sign_date: Some(date("2026-02-01")),
            start_date: Some(date("2026-01-01")),
            ..base_req()
        };
        let err = req.validate().expect_err("倒签必须拒");
        assert!(err.msg.contains("sign_date"), "{}", err.msg);
    }

    /// ⑦ 合法请求通过 + 乙方名称用服务端快照（客户端传什么都不算数）。
    #[test]
    fn create_with_legal_payload_passes_and_uses_server_snapshot() {
        let req = base_req();
        req.validate().expect("合法请求必须过");
        let party_b_id = Uuid::now_v7();
        let new = req.into_new(
            party_b_id,
            "服务端读到的供应商名".to_string(),
            contract_model::PARTY_B_SUPPLIER,
            None,
        );
        assert_eq!(new.party_b_name, "服务端读到的供应商名", "名称必须是服务端读到的快照");
        assert_eq!(new.party_b_id, party_b_id);
        assert_eq!(new.contract_type, "PURCHASE");
        assert_eq!(new.currency_code, DEFAULT_CURRENCY, "币种缺省 CNY");
        assert_eq!(new.contract_name, "A2 采购合同");
    }

    /// ⑧ `SALES`（客户类）的乙方类型推导为 `CUSTOMER`（SO-26 起 party_b_id 指向 biz_customer）。
    #[test]
    fn sales_contract_derives_customer_party_type() {
        let req = CreateContractReq {
            contract_type: Some("SALES".to_string()),
            ..base_req()
        };
        assert_eq!(
            req.resolve_party_b_type().expect("推导"),
            contract_model::PARTY_B_CUSTOMER,
            "销售合同的乙方是客户 → party_b_id 指向 biz_customer.id"
        );
    }

    /// ⑧-1 CTR-02-F1：四类合同**全部放行**（`SERVICE` / `LEASE` 由类型闸放开），
    /// 乙方类型各自推导正确（服务 / 租赁固定供应商）。
    #[test]
    fn all_four_types_are_supported() {
        assert_eq!(
            SUPPORTED_TYPES,
            ["PURCHASE", "SALES", "SERVICE", "LEASE"],
            "放行集与 CONTRACT_TYPES / ck_biz_contract_type 一致"
        );
        assert_eq!(SUPPORTED_TYPES, CONTRACT_TYPES, "放行集与枚举白名单同值");

        for (contract_type, party_b_type) in [
            ("PURCHASE", contract_model::PARTY_B_SUPPLIER),
            ("SALES", contract_model::PARTY_B_CUSTOMER),
            ("SERVICE", contract_model::PARTY_B_SUPPLIER),
            ("LEASE", contract_model::PARTY_B_SUPPLIER),
        ] {
            let req = CreateContractReq {
                contract_type: Some(contract_type.to_string()),
                ..base_req()
            };
            req.validate()
                .unwrap_or_else(|error| panic!("{contract_type} 必须放行：{}", error.msg));
            assert_eq!(
                req.resolve_party_b_type().expect("推导"),
                party_b_type,
                "{contract_type} 的乙方主档类型"
            );
            assert!(
                ensure_supported_type(contract_type).is_ok(),
                "{contract_type} 在放行集里"
            );
        }
    }

    /// ⑧-1b 类型闸保留：未来新枚举（不在 `ck_biz_contract_type` 白名单）仍被拒，
    /// 文案指向白名单而不是「本批次未开放」。
    #[test]
    fn unknown_future_type_still_rejected_by_type_gate() {
        let err = ensure_supported_type("FRAMEWORK").expect_err("未来新枚举仍要拦");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(
            err.msg.contains("ck_biz_contract_type"),
            "文案要点明白名单：{}",
            err.msg
        );

        let req = CreateContractReq {
            contract_type: Some("FRAMEWORK".to_string()),
            ..base_req()
        };
        let err = req.validate().expect_err("枚举闸先拦");
        assert!(err.msg.contains("contract_type"), "{}", err.msg);
    }

    /// ⑧-2 `SALES` 显式传 `SUPPLIER` 乙方类型 → 400（推导与显式必须一致）。
    #[test]
    fn sales_with_supplier_party_type_is_rejected() {
        let req = CreateContractReq {
            contract_type: Some("SALES".to_string()),
            party_b_type: Some(contract_model::PARTY_B_SUPPLIER.to_string()),
            ..base_req()
        };
        let err = req.resolve_party_b_type().expect_err("冲突必须拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("CUSTOMER"), "{}", err.msg);
    }

    /// ⑧-3 CTR-02-F1：`SERVICE` / `LEASE` 传 `CUSTOMER` → 400（单向强制走供应商主档）。
    /// 文案按类型说清「服务商 / 出租方登记为供应商」，且不出现 SQLSTATE。
    #[test]
    fn service_and_lease_reject_customer_party_type() {
        for (contract_type, label) in [("SERVICE", "服务"), ("LEASE", "租赁")] {
            let req = CreateContractReq {
                contract_type: Some(contract_type.to_string()),
                party_b_type: Some(contract_model::PARTY_B_CUSTOMER.to_string()),
                ..base_req()
            };
            let err = req.resolve_party_b_type().expect_err("服务/租赁的乙方固定供应商");
            assert_eq!(err.status, 400, "{contract_type}");
            assert_eq!(err.code, error_code::PARAM_INVALID, "{contract_type}");
            assert!(
                err.msg.contains("SUPPLIER") && err.msg.contains(label),
                "文案要说清为什么必须是 SUPPLIER：{}",
                err.msg
            );
            assert!(
                !err.msg.contains("SQLSTATE") && !err.msg.contains("23514"),
                "不许把 DDL 错误透给前端：{}",
                err.msg
            );

            // 纯函数出口同样拒（PUT 走的就是它）
            let pure = ensure_party_b_type(contract_type, contract_model::PARTY_B_CUSTOMER).expect_err("纯函数同样拒");
            assert_eq!(pure.status, 400);
            assert_eq!(
                ensure_party_b_type(contract_type, contract_model::PARTY_B_SUPPLIER).expect("供应商放行"),
                contract_model::PARTY_B_SUPPLIER
            );
            // 第三个值也不许过
            assert!(ensure_party_b_type(contract_type, "VENDOR").is_err());
        }
        // 销售侧反向：CUSTOMER 才是对的
        assert!(ensure_party_b_type("SALES", contract_model::PARTY_B_CUSTOMER).is_ok());
        assert!(ensure_party_b_type("SALES", contract_model::PARTY_B_SUPPLIER).is_err());
    }

    /// ⑨ 分页口径：默认 1/20，上限 100。
    #[test]
    fn page_size_is_capped_at_one_hundred() {
        assert_eq!(normalize_page(0, 0), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(3, 500), (3, MAX_PAGE_SIZE));
        let filter = ContractFilter {
            page: 2,
            page_size: 1000,
            ..Default::default()
        };
        let query = filter.parse().expect("解析");
        assert_eq!((query.page, query.page_size), (2, 100));
    }

    /// ⑩ 筛选里的 `CANCELLED` / 非法日期 / 非法 UUID → 400（不许静默返回空列表）。
    #[test]
    fn filter_rejects_unknown_status_and_bad_values() {
        let cancelled = ContractFilter {
            status: Some("CANCELLED".to_string()),
            ..Default::default()
        };
        let err = cancelled.parse().expect_err("CANCELLED 不是合法状态");
        assert!(err.msg.contains("TERMINATED"), "文案要点名可用状态：{}", err.msg);

        let bad_date = ContractFilter {
            sign_date_from: Some("2026/01/01".to_string()),
            ..Default::default()
        };
        assert_eq!(bad_date.parse().expect_err("日期格式").status, 400);

        let bad_uuid = ContractFilter {
            party_b_id: Some("not-a-uuid".to_string()),
            ..Default::default()
        };
        assert_eq!(bad_uuid.parse().expect_err("UUID").status, 400);
    }

    /// ⑪ 状态流转请求：`target_status` / `status` 两个拼写都认；缺省 → 400。
    #[test]
    fn status_req_accepts_both_spellings() {
        let canonical = ContractStatusReq {
            target_status: Some(" SIGNED ".to_string()),
            ..Default::default()
        };
        assert_eq!(canonical.target().expect("解析"), "SIGNED");

        let legacy = ContractStatusReq {
            status: Some("TERMINATED".to_string()),
            reason: Some(" 供应商违约 ".to_string()),
            ..Default::default()
        };
        assert_eq!(legacy.target().expect("解析"), "TERMINATED");
        assert_eq!(legacy.reason_text(), Some("供应商违约"), "reason 去空白");

        let missing = ContractStatusReq::default();
        assert_eq!(missing.target().expect_err("缺目标态").status, 400);
    }

    /// 造一行 `ContractRow`（纯结构，不连库）：`contract_type` 可变，其余固定。
    fn contract_row_with_type(contract_type: &str) -> ContractRow {
        ContractRow {
            id: Uuid::now_v7(),
            contract_no: "HT-20261102-000001".to_string(),
            contract_name: "A2 双写合同".to_string(),
            contract_type: contract_type.to_string(),
            party_a_id: None,
            party_a_name: None,
            party_b_type: contract_model::required_party_b_type(contract_type).to_string(),
            party_b_id: Uuid::now_v7(),
            party_b_name: "乙方快照".to_string(),
            party_b_current_name: Some("乙方现值".to_string()),
            amount: Decimal::new(100, 0),
            currency_code: "CNY".to_string(),
            sign_date: Some(date("2026-01-01")),
            start_date: Some(date("2026-01-01")),
            end_date: Some(date("2026-12-31")),
            status: STATUS_DRAFT.to_string(),
            owner_id: None,
            dept_id: None,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
            deleted_at: None,
            approval_instance_id: None,
        }
    }

    /// ⑫ 列表行两个名称字段恒等（防「只改一边」的漂移）。
    #[test]
    fn list_row_exposes_both_name_spellings() {
        let row = contract_row_with_type("PURCHASE");
        let res = ContractRes::from(&row);
        assert_eq!(res.name, res.contract_name, "name 与 contract_name 必须同值");
        assert_eq!(res.status_label, "草稿");
        assert_eq!(res.party_b_name, "乙方快照", "快照列");
        assert_eq!(
            res.party_b_current_name.as_deref(),
            Some("乙方现值"),
            "关联带出的是主档现值"
        );

        let detail = ContractDetailRes::from(&row);
        assert!(detail.can_edit, "DRAFT 可改");
        assert!(!detail.usable_for_order, "DRAFT 不能被采购单引用");
        assert_eq!(
            detail.next_statuses,
            vec!["PENDING_APPROVAL".to_string()],
            "去向由状态机给（R-P1-1：草稿只能提交审批）"
        );
        assert_eq!(
            detail.commitment_scope,
            contract_model::COMMITMENT_SCOPE_ORDER_LINKED,
            "采购合同挂采购单 → 履约字段是真值"
        );
    }

    /// ⑫-1 CTR-02-F1：详情按合同类型给 `commitment_scope`；服务 / 租赁的 0 是**正确值**，
    /// 前端靠这个字段决定「不渲染 0.00」（不是靠 `committed_amount == 0` 猜）。
    #[test]
    fn detail_commitment_scope_is_not_applicable_for_service_and_lease() {
        for contract_type in ["SERVICE", "LEASE"] {
            let detail = ContractDetailRes::from(&contract_row_with_type(contract_type));
            assert_eq!(
                detail.commitment_scope,
                contract_model::COMMITMENT_SCOPE_NOT_APPLICABLE,
                "{contract_type} 不挂订单 → 口径不适用"
            );
            assert_eq!(detail.committed_amount, Decimal::ZERO, "不适用时 0 是正确值");
            assert_eq!(detail.remaining_amount, detail.base.amount, "剩余 = 合同额");
            assert_eq!(detail.open_order_count, 0);
            assert!(detail.open_order_nos.is_empty());
        }
        for contract_type in ["PURCHASE", "SALES"] {
            assert_eq!(
                ContractDetailRes::from(&contract_row_with_type(contract_type)).commitment_scope,
                contract_model::COMMITMENT_SCOPE_ORDER_LINKED,
                "{contract_type} 挂订单 → 履约字段是真值"
            );
        }
    }
}
