//! 成本方法配置契约层（成本核算 · M5-01【M5·成本】品种法 / 分批法 / 标准成本）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//! D-2（不复活 `biz_material.costing_method` 的 FIFO/STANDARD）、
//! D-3（一期只做分批法）、D-15（标准成本二期只做差异分析、不改存货计价）、
//! D-16（品种法三期复用同一套表）、§8 术语表（分批法 / 品种法 / 标准成本）。
//!
//! # 这张配置是「核算方法」，不是「存货计价法」
//!
//! | 事情 | 在哪 | 值域 |
//! |---|---|---|
//! | 存货**计价法**（出库按什么价结转） | `biz_material.costing_method` | 只有 `MOVING_AVG`（V0201 + ARCH-9 D-2 收敛） |
//! | 成本**核算方法**（料工费按什么粒度归集） | 本模块 `biz_cost_method_config.method` | `JOB` / `VARIETY` / `STANDARD` |
//!
//! 两者互不影响：销售出库永远按移动加权结转 6401（ARCH-9 §2），分批法只影响**完工入库**
//! 单价。把这两件事混成一列是历史上踩过的坑（V0201 的 S2 就是替用户把 FIFO/STANDARD
//! 静默改写成 MOVING_AVG），所以本卡**另开一张表**，一个字节都不碰那张物料表。
//!
//! # 判定顺序（`resolve`，纯函数）
//!
//! ```text
//! COST_OBJECT（scope_id = 该对象，启用、生效）
//!   → GLOBAL（启用、生效）
//!     → DEFAULT（代码兜底 = JOB 分批法）
//! ```
//!
//! 同一级里多条版本时取 `effective_from` 最晚的一条（同一天只许一条，库里有唯一索引）。
//! 「最晚生效」而不是「优先级列」：优先级是给人调顺序用的旋钮，方法版本的时间线本身
//! 就是顺序（后生效的覆盖先生效的），多一列只会多一种自相矛盾的组合。
//!
//! # 一期只实现分批法 —— 配置可以选，计算会拦（不静默按分批法算）
//!
//! ARCH-9 D-3：一期只做分批法；D-15：标准成本二期只做差异分析；D-16：品种法三期。
//! 所以三种方法**都可以配**（它们是二 / 三期排期的输入，也是 M5-31 切换策略的对象），
//! 但成本计算入口（[`crate::business::erp::cost::cost_calc_period`]）在开算前会检查：
//! 本期生效范围内出现 `method <> 'JOB'` 的配置 ⇒ `409` + [`CODE_UNIMPLEMENTED`]，
//! 报文点名单据号与「怎么改」。**绝不**按分批法算完却把报表标成标准成本 ——
//! 那样出来的数字是错的，而且没人看得出来（比报错坏得多）。
//!
//! # 错误码（`66xxx` 段，接在 66100 之后）
//!
//! | 码 | 场景 | HTTP |
//! |---|---|---|
//! | [`CODE_UNIMPLEMENTED`] `66101` | 计算入口遇到未实现的核算方法 | 409 |
//! | [`CODE_OVERLAP`] `66102` | 同范围新增版本与既有版本生效期重叠 | 409 |
//! | [`CODE_SCOPE_INVALID`] `66103` | 范围参数不合法（全局带对象 / 对象级没对象 / 对象不存在或已作废） | 400 |
//! | [`CODE_DELETE_IN_USE`] `66104` | 删除已生效过的配置（只能停用） | 409 |
//! | [`CODE_LOCKED_EFFECTIVE`] `66105` | 改写已生效版本的方法 / 范围 / 生效日（只能改停止日 / 状态 / 备注） | 409 |

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::shared::error_code;

// ==================== 值域（与 V0459 的 CHECK 逐字一致） ====================

/// 分批法：按成本对象（生产批）归集与结转，**一期唯一实现**（ARCH-9 D-3）。
pub const METHOD_JOB: &str = "JOB";
/// 品种法：按产品归集料工费，不区分批次（流程型连续生产；三期实现，D-16）。
pub const METHOD_VARIETY: &str = "VARIETY";
/// 标准成本：以标准成本卡为基准做差异分析（二期；不改存货计价，D-15）。
pub const METHOD_STANDARD: &str = "STANDARD";

/// 三种方法（顺序即界面下拉框顺序：先一期实现的，再二三期）。
pub const ALL_METHODS: [&str; 3] = [METHOD_JOB, METHOD_VARIETY, METHOD_STANDARD];

/// 代码兜底方法：一条配置都没有时按分批法算（一期口径，与 V0459 种子行一致）。
pub const DEFAULT_METHOD: &str = METHOD_JOB;

/// 范围：全公司默认。
pub const SCOPE_GLOBAL: &str = "GLOBAL";
/// 范围：指定成本对象。
pub const SCOPE_OBJECT: &str = "COST_OBJECT";

/// 两种范围。
pub const ALL_SCOPES: [&str; 2] = [SCOPE_OBJECT, SCOPE_GLOBAL];

/// 判定来源：代码兜底（没有任何配置命中）。
pub const SOURCE_DEFAULT: &str = "DEFAULT";
/// 判定来源：命中了全公司默认配置。
pub const SOURCE_GLOBAL: &str = "GLOBAL";
/// 判定来源：命中了成本对象级配置。
pub const SOURCE_OBJECT: &str = "COST_OBJECT";

/// 启用。
pub const STATUS_ENABLED: i16 = 1;
/// 停用。
pub const STATUS_DISABLED: i16 = 0;

// ==================== 错误码 ====================

/// `409`：计算入口遇到一期未实现的核算方法（`66101`，常量定义在 kernel 的 66xxx 码表里）。
pub const CODE_UNIMPLEMENTED: i32 = error_code::ERP_COST_METHOD_UNIMPLEMENTED;
/// `409`：同范围版本生效期重叠（`66102`）。
pub const CODE_OVERLAP: i32 = error_code::ERP_COST_METHOD_OVERLAP;
/// `400`：范围参数不合法（`66103`）。
pub const CODE_SCOPE_INVALID: i32 = error_code::ERP_COST_METHOD_SCOPE_INVALID;
/// `409`：删除已生效过的配置（`66104`）。
pub const CODE_DELETE_IN_USE: i32 = error_code::ERP_COST_METHOD_DELETE_IN_USE;
/// `409`：改写已生效版本的方法 / 范围 / 生效日（`66105`）。
pub const CODE_LOCKED_EFFECTIVE: i32 = error_code::ERP_COST_METHOD_LOCKED_EFFECTIVE;

// ==================== 标签 / 说明 ====================

/// 方法 → 中文名（界面下拉框与列表列共用）。
pub fn method_label(method: &str) -> &'static str {
    match method {
        METHOD_JOB => "分批法",
        METHOD_VARIETY => "品种法",
        METHOD_STANDARD => "标准成本",
        _ => "未知",
    }
}

/// 方法 → 是否已落地（一期只有分批法）。
///
/// 前端据此把未实现的方法标灰 / 加角标，**不阻止选择**（配置是二 / 三期的排期数据），
/// 但要让用户知道「选了它，这期算不出来」。
pub fn method_implemented(method: &str) -> bool {
    method == METHOD_JOB
}

/// 方法 → 一句人话（列表 tooltip / 生效方法查询的说明列）。
pub fn method_note(method: &str) -> &'static str {
    match method {
        METHOD_JOB => "按成本对象（生产批）归集料工费并结转完工：一期已实现，成本计算按它执行",
        METHOD_VARIETY => {
            "按产品归集料工费、不区分批次（流程型连续生产）：一期只可配置，计算引擎未实现（ARCH-9 D-16，三期）"
        }
        METHOD_STANDARD => "以标准成本卡为基准、差异只出报表不入账（ARCH-9 D-15，二期）：一期只可配置，计算引擎未实现",
        _ => "未知方法",
    }
}

/// 范围 → 中文名。
pub fn scope_label(scope: &str) -> &'static str {
    match scope {
        SCOPE_GLOBAL => "全公司默认",
        SCOPE_OBJECT => "指定成本对象",
        _ => "未知范围",
    }
}

/// 判定来源 → 中文名。
pub fn source_label(source: &str) -> &'static str {
    match source {
        SOURCE_OBJECT => "成本对象级配置",
        SOURCE_GLOBAL => "全公司默认配置",
        SOURCE_DEFAULT => "代码兜底（未配置任何方法，默认分批法）",
        _ => "未知来源",
    }
}

/// 状态 → 中文名。
pub fn status_label(status: i16) -> &'static str {
    if status == STATUS_ENABLED { "启用" } else { "停用" }
}

// ==================== 归一化 ====================

/// 方法归一化（去空白 + 大写 + 值域校验）。
pub fn normalize_method(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(
            "method（核算方法）不能为空：JOB 分批法 / VARIETY 品种法 / STANDARD 标准成本",
        ));
    }
    let upper = text.to_ascii_uppercase();
    if !ALL_METHODS.contains(&upper.as_str()) {
        return Err(bad_request(format!(
            "method（核算方法）只能是 JOB / VARIETY / STANDARD（收到「{text}」）：\
             JOB = 分批法（一期已实现）、VARIETY = 品种法（三期）、STANDARD = 标准成本（二期）"
        )));
    }
    Ok(upper)
}

/// 范围归一化。
pub fn normalize_scope(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(format!(
            "scope（配置范围）不能为空：{SCOPE_OBJECT} 指定成本对象 / {SCOPE_GLOBAL} 全公司默认"
        )));
    }
    let upper = text.to_ascii_uppercase();
    if !ALL_SCOPES.contains(&upper.as_str()) {
        return Err(bad_request(format!(
            "scope（配置范围）只能是 {SCOPE_OBJECT} / {SCOPE_GLOBAL}（收到「{text}」）"
        )));
    }
    Ok(upper)
}

/// 状态归一化（只允许 0 / 1）。
pub fn normalize_status(value: Option<i16>) -> std::result::Result<Option<i16>, ApiError> {
    match value {
        None => Ok(None),
        Some(flag) if flag == STATUS_ENABLED || flag == STATUS_DISABLED => Ok(Some(flag)),
        Some(other) => Err(bad_request(format!(
            "status 只能是 {STATUS_ENABLED}（启用）或 {STATUS_DISABLED}（停用），收到 {other}"
        ))),
    }
}

/// `400` 构造（与 `cost_labor_dto::bad_request` 同码同形，模块内自带一份便于独立单测）。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404` 构造。
pub fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("成本方法配置不存在或已删除（{id}）"),
    )
}

// ==================== 领域错误 ====================

/// `409` + `66103`：范围参数不合法（全局带对象 / 对象级没对象 / 对象不存在或已作废）。
pub fn scope_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, CODE_SCOPE_INVALID, message.into())
}

/// `409` + `66102`：同范围版本生效期重叠。
///
/// 重叠为什么必须拦：同一范围在重叠区间里同时有两条版本，`resolve` 只能按
/// 「取生效最晚的一条」硬挑一个 —— 那不是业务口径，是巧合。口径必须唯一。
pub fn overlap_error(
    scope: &str,
    scope_name: &str,
    existing_method: &str,
    existing_range: &str,
    wanted_range: &str,
) -> ApiError {
    ApiError::new(
        409,
        CODE_OVERLAP,
        format!(
            "{scope_name} 已有一条方法配置覆盖了重叠区间（{scope_kind}：{existing_method_label}，生效期 {existing_range}），\
             新配置的生效期 {wanted_range} 与它重叠。\n\
             同一范围同一时刻只能有一种核算方法（否则「这期按什么算」没有确定答案）。\n\
             切换方法的正确姿势：先把旧版本的停止日改到切换日前一天，再新增一条从切换日生效的版本",
            scope_kind = scope_label(scope),
            existing_method_label = method_label(existing_method),
        ),
    )
}

/// `409` + `66104`：删除已生效过的配置。
pub fn delete_in_use(effective_from: NaiveDate, today: NaiveDate) -> ApiError {
    ApiError::new(
        409,
        CODE_DELETE_IN_USE,
        format!(
            "该配置从 {effective_from} 起已生效（今天 {today}），不能删除：它是「这段期间按哪种方法算成本」的凭据，\
             删掉之后没人能解释历史期间怎么算的。\n\
             要让它不再生效，请把 status 改成 0（停用）；要换方法，请把它的停止日改到切换日前一天，\
             再新增一条从切换日生效的版本"
        ),
    )
}

/// `409` + `66105`：改写已生效版本的方法 / 范围 / 生效日。
///
/// 「已生效」= `effective_from < 今天`（当天生效的当天还能改 —— 它还没覆盖任何一个过去的
/// 核算期间）。允许改的只有停止日 / 状态 / 备注：这三样都不改写「那段期间按什么方法算」。
pub fn locked_effective(scope_name: &str, method: &str, effective_from: NaiveDate, today: NaiveDate) -> ApiError {
    ApiError::new(
        409,
        CODE_LOCKED_EFFECTIVE,
        format!(
            "{scope_name} 的这条配置从 {effective_from} 起已生效（今天 {today}，方法 {}），\
             不能再改「方法 / 范围 / 生效日」——那等于事后改写历史期间的口径。\n\
             能改的只有：停止日（把旧版收到切换日前一天）、状态（停用）、备注。\n\
             换方法请新增一条从切换日生效的版本；配错了请先停用再新增",
            method_label(method)
        ),
    )
}

/// `409` + `66101`：计算入口遇到未实现的方法。
///
/// `entries` 是 `(单据号 / 范围名, 方法)` 列表 —— 报文要把**具体是哪几个对象**点出来，
/// 否则用户在几百个成本对象的期间里找不到是哪条配置挡住了。
pub fn unimplemented_error(period_code: &str, entries: &[(String, String)]) -> ApiError {
    let detail = entries
        .iter()
        .map(|(who, method)| format!("{who} → {}（{method}）", method_label(method)))
        .collect::<Vec<_>>()
        .join("；");
    ApiError::new(
        409,
        CODE_UNIMPLEMENTED,
        format!(
            "期间 {period_code} 的成本计算被拦下：以下范围配置的核算方法在当前引擎里还没实现 —— {detail}。\n\
             一期只实现分批法（ARCH-9 D-3）：品种法属三期（D-16）、标准成本二期只做差异分析不改计价（D-15）。\n\
             三条出路任选其一：① 把这批配置的生效日推到下期或更晚；② 把 status 改成 0 停用；\
             ③ 把方法改回 JOB（分批法）。\n\
             不按分批法静默算完是刻意的：那样出来的成本报表是错的，而且看不出错"
        ),
    )
}

// ==================== 归一化规格 ====================

/// 归一化后的配置规格（dto 与 model 之间唯一的形状）。
#[derive(Debug, Clone, PartialEq)]
pub struct CostMethodSpec {
    /// 核算方法（JOB / VARIETY / STANDARD）。
    pub method: String,
    /// 范围（GLOBAL / COST_OBJECT）。
    pub scope_type: String,
    /// 成本对象（GLOBAL 时必须为 `None`）。
    pub scope_id: Option<Uuid>,
    /// 生效日。
    pub effective_from: NaiveDate,
    /// 停止日（可空 = 长期有效）。
    pub effective_to: Option<NaiveDate>,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
}

/// 新建入参（§ 接口 `POST /business/erp/cost-method`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostMethodCreateReq {
    /// 核算方法（必填）。
    pub method: Option<String>,
    /// 范围（必填：`COST_OBJECT` / `GLOBAL`）。
    pub scope: Option<String>,
    /// 成本对象（`scope = COST_OBJECT` 时必填）。
    pub scope_id: Option<String>,
    /// 生效日（必填 `YYYY-MM-DD`）。
    pub effective_from: Option<String>,
    /// 停止日（可空 = 长期有效）。
    pub effective_to: Option<String>,
    /// 状态（缺省 1 启用）。
    pub status: Option<i16>,
    /// 备注。
    pub remark: Option<String>,
}

/// 修改入参：**缺省 = 不改**；`effective_to` / `remark` 传空串 = 清空。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostMethodUpdateReq {
    /// 核算方法。
    pub method: Option<String>,
    /// 范围。
    pub scope: Option<String>,
    /// 成本对象（`scope = COST_OBJECT` 时必填；改成 GLOBAL 时它会被清空）。
    pub scope_id: Option<String>,
    /// 生效日。
    pub effective_from: Option<String>,
    /// 停止日（空串 = 清空 → 长期有效）。
    pub effective_to: Option<String>,
    /// 状态（`1` 启用 / `0` 停用）。
    pub status: Option<i16>,
    /// 备注（空串 = 清空）。
    pub remark: Option<String>,
}

/// 列表 / 生效方法查询入参（`GET /business/erp/cost-method`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostMethodPageQuery {
    /// 范围筛选。
    pub scope: Option<String>,
    /// 方法筛选。
    pub method: Option<String>,
    /// 状态筛选（0 / 1）。
    pub status: Option<i16>,
    /// 成本对象筛选（`scope_id`）。
    pub scope_id: Option<String>,
    /// 关键字（对象号 / 对象名 / 备注）。
    pub keyword: Option<String>,
    /// 页号（缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省由 `normalize_page` 定）。
    pub page_size: Option<u64>,
}

/// 生效方法查询入参（`GET /business/erp/cost-method/effective`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostMethodEffectiveQuery {
    /// 成本对象（可空 = 只查全公司默认口径）。
    pub cost_object_id: Option<String>,
    /// 参照日（`YYYY-MM-DD`，缺省今天）。
    pub on: Option<String>,
}

/// 列表出参。
#[derive(Debug, Clone, Serialize)]
pub struct CostMethodRes {
    /// 主键。
    pub id: String,
    /// 方法（JOB / VARIETY / STANDARD）。
    pub method: String,
    /// 方法中文名。
    pub method_label: String,
    /// 该方法是否已实现（一期只有分批法为 `true`）。
    pub method_implemented: bool,
    /// 方法说明（列表 tooltip）。
    pub method_note: String,
    /// 范围。
    pub scope: String,
    /// 范围中文名。
    pub scope_label: String,
    /// 成本对象 id（GLOBAL 时为空）。
    pub scope_id: Option<String>,
    /// 成本对象号（GLOBAL 时为空；实时 join `biz_cost_object`）。
    pub scope_object_no: Option<String>,
    /// 成本对象状态（OPEN / CLOSED / CANCELLED；GLOBAL 时为空）。
    pub scope_object_status: Option<String>,
    /// 生效日。
    pub effective_from: String,
    /// 停止日（长期有效为空）。
    pub effective_to: Option<String>,
    /// 状态（1 / 0）。
    pub status: i16,
    /// 状态中文名。
    pub status_label: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 更新时间（RFC3339）。
    pub updated_at: String,
}

/// 生效方法查询出参（「这一期 / 这个对象按什么方法算」）。
#[derive(Debug, Clone, Serialize)]
pub struct CostMethodEffectiveRes {
    /// 成本对象 id（查询未指定对象时为空）。
    pub cost_object_id: Option<String>,
    /// 成本对象号（实时 join）。
    pub cost_object_no: Option<String>,
    /// 参照日。
    pub on_date: String,
    /// 命中的方法。
    pub method: String,
    /// 方法中文名。
    pub method_label: String,
    /// 方法是否已实现（`false` 时界面要提示「这期算不出来」）。
    pub method_implemented: bool,
    /// 方法说明。
    pub method_note: String,
    /// 判定来源（COST_OBJECT / GLOBAL / DEFAULT）。
    pub source: String,
    /// 判定来源中文名。
    pub source_label: String,
    /// 命中的配置行（DEFAULT 时为空）。
    pub config_id: Option<String>,
    /// 命中配置的生效日。
    pub effective_from: Option<String>,
    /// 命中配置的停止日。
    pub effective_to: Option<String>,
    /// 给界面的一句结论（含「能不能算」）。
    pub engine_hint: String,
}

// ==================== 判定（纯函数） ====================

/// 判定用的一行配置（model 层 `Row` 的纯数据投影，便于单测不连库）。
#[derive(Debug, Clone, PartialEq)]
pub struct MethodConfigView {
    /// 主键。
    pub id: Uuid,
    /// 方法。
    pub method: String,
    /// 范围。
    pub scope_type: String,
    /// 成本对象。
    pub scope_id: Option<Uuid>,
    /// 生效日。
    pub effective_from: NaiveDate,
    /// 停止日。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: i16,
}

impl MethodConfigView {
    /// 是否在 `on` 这天生效（含两端；`effective_to` 为空 = 长期有效）。
    pub fn covers(&self, on: NaiveDate) -> bool {
        self.status == STATUS_ENABLED && self.effective_from <= on && self.effective_to.is_none_or(|to| to >= on)
    }
}

/// 判定结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    /// 方法。
    pub method: String,
    /// 来源（COST_OBJECT / GLOBAL / DEFAULT）。
    pub source: String,
    /// 命中行（DEFAULT 时为空）。
    pub config: Option<MethodConfigView>,
}

/// 按「对象级 > 全局级 > 代码默认」判定某成本对象在 `on` 这天用的核算方法。
///
/// 同级多条版本取 `effective_from` 最晚的一条（同一天只许一条，库里有唯一索引）；
/// `effective_from` 相同时按 `id` 倒序稳定取一条（纯函数也必须是确定性的）。
pub fn resolve(rows: &[MethodConfigView], cost_object_id: Option<Uuid>, on: NaiveDate) -> Resolution {
    let latest = |mut candidates: Vec<&MethodConfigView>| -> Option<MethodConfigView> {
        candidates.sort_by(|a, b| a.effective_from.cmp(&b.effective_from).then_with(|| a.id.cmp(&b.id)));
        candidates.pop().cloned()
    };

    if let Some(object_id) = cost_object_id {
        let object_rows = rows
            .iter()
            .filter(|row| row.scope_type == SCOPE_OBJECT && row.scope_id == Some(object_id) && row.covers(on))
            .collect::<Vec<_>>();
        if let Some(hit) = latest(object_rows) {
            return Resolution {
                method: hit.method.clone(),
                source: SOURCE_OBJECT.to_string(),
                config: Some(hit),
            };
        }
    }

    let global_rows = rows
        .iter()
        .filter(|row| row.scope_type == SCOPE_GLOBAL && row.covers(on))
        .collect::<Vec<_>>();
    if let Some(hit) = latest(global_rows) {
        return Resolution {
            method: hit.method.clone(),
            source: SOURCE_GLOBAL.to_string(),
            config: Some(hit),
        };
    }

    Resolution {
        method: DEFAULT_METHOD.to_string(),
        source: SOURCE_DEFAULT.to_string(),
        config: None,
    }
}

/// 两个生效区间是否相交（`None` = 开端到无穷）。
pub fn ranges_overlap(a_from: NaiveDate, a_to: Option<NaiveDate>, b_from: NaiveDate, b_to: Option<NaiveDate>) -> bool {
    let a_end_ok = a_to.is_none_or(|to| to >= b_from);
    let b_end_ok = b_to.is_none_or(|to| to >= a_from);
    a_end_ok && b_end_ok
}

/// 在候选行里找与 `spec` 同范围、**启用中**且生效期重叠的既有版本（`exclude` = 修改时排除自己）。
///
/// 停用（`status = 0`）的行不参与：它不参与判定，也就不会造成「这期按什么算」的歧义；
/// 把它算进重叠会让用户陷入死结（想启用新口径，却因为一条自己刚停用的旧行而建不出来）。
pub fn find_overlap(
    rows: &[MethodConfigView],
    spec: &CostMethodSpec,
    exclude: Option<Uuid>,
) -> Option<MethodConfigView> {
    rows.iter()
        .find(|row| {
            Some(row.id) != exclude
                && row.status == STATUS_ENABLED
                && row.scope_type == spec.scope_type
                && row.scope_id == spec.scope_id
                && ranges_overlap(
                    row.effective_from,
                    row.effective_to,
                    spec.effective_from,
                    spec.effective_to,
                )
        })
        .cloned()
}

/// 生效区间的文字形式（报文用）：`2026-01-01 ~ 长期`。
pub fn range_text(from: NaiveDate, to: Option<NaiveDate>) -> String {
    match to {
        None => format!("{from} ~ 长期"),
        Some(end) => format!("{from} ~ {end}"),
    }
}

// ==================== 规格构造 ====================

/// 新建规格：校验值域 + 范围成对（对象存在性由 handler 查库后另行确认）。
pub fn spec_from_create(req: &CostMethodCreateReq) -> std::result::Result<CostMethodSpec, ApiError> {
    let method = normalize_method(req.method.as_deref())?;
    let scope_type = normalize_scope(req.scope.as_deref())?;
    let scope_id = match scope_type.as_str() {
        SCOPE_OBJECT => {
            let text = req
                .scope_id
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    scope_invalid(
                        "scope = COST_OBJECT 时必须给 scope_id（指定成本对象 id）；\
                         要配全公司默认请用 scope = GLOBAL",
                    )
                })?;
            Some(crate::business::erp::cost::cost_labor_dto::parse_uuid(
                text, "scope_id",
            )?)
        }
        _ => {
            if req
                .scope_id
                .as_deref()
                .map(str::trim)
                .is_some_and(|text| !text.is_empty())
            {
                return Err(scope_invalid(
                    "scope = GLOBAL 时不能给 scope_id（全公司默认没有具体对象）；\
                     要给某个成本对象配方法请用 scope = COST_OBJECT",
                ));
            }
            None
        }
    };
    let effective_from =
        crate::business::erp::cost::cost_labor_dto::require_date(req.effective_from.as_deref(), "effective_from")?;
    let effective_to = match crate::business::erp::cost::cost_labor_dto::clean_optional(req.effective_to.clone()) {
        None => None,
        Some(raw) => Some(crate::business::erp::cost::cost_labor_dto::parse_date(
            &raw,
            "effective_to",
        )?),
    };
    if let Some(end) = effective_to
        && end < effective_from
    {
        return Err(bad_request(format!(
            "effective_to（{end}）不能早于 effective_from（{effective_from}）"
        )));
    }
    let status = normalize_status(req.status)?.unwrap_or(STATUS_ENABLED);
    Ok(CostMethodSpec {
        method,
        scope_type,
        scope_id,
        effective_from,
        effective_to,
        status,
        remark: crate::business::erp::cost::cost_labor_dto::clean_optional(req.remark.clone()),
    })
}

/// 修改规格：在当前规格上叠加（缺省 = 不改）。
pub fn spec_from_update(
    req: &CostMethodUpdateReq,
    current: &CostMethodSpec,
) -> std::result::Result<CostMethodSpec, ApiError> {
    let method = match req.method.as_deref() {
        None => current.method.clone(),
        Some(raw) => normalize_method(Some(raw))?,
    };
    let scope_type = match req.scope.as_deref() {
        None => current.scope_type.clone(),
        Some(raw) => normalize_scope(Some(raw))?,
    };
    // 范围改写时 scope_id 跟着重算：换成 GLOBAL 就清空；COST_OBJECT 则必须给（或沿用）
    let scope_id = if scope_type == SCOPE_GLOBAL {
        None
    } else {
        match req.scope_id.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
            Some(raw) => Some(crate::business::erp::cost::cost_labor_dto::parse_uuid(raw, "scope_id")?),
            None => Some(
                current
                    .scope_id
                    .ok_or_else(|| scope_invalid("scope = COST_OBJECT 时必须给 scope_id"))?,
            ),
        }
    };
    let effective_from = match req.effective_from.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        None => current.effective_from,
        Some(raw) => crate::business::erp::cost::cost_labor_dto::parse_date(raw, "effective_from")?,
    };
    let effective_to = match req.effective_to.as_ref() {
        None => current.effective_to,
        Some(raw) => match crate::business::erp::cost::cost_labor_dto::clean_optional(Some(raw.clone())) {
            None => None, // 空串 = 清空（改成长期有效）
            Some(text) => Some(crate::business::erp::cost::cost_labor_dto::parse_date(
                &text,
                "effective_to",
            )?),
        },
    };
    if let Some(end) = effective_to
        && end < effective_from
    {
        return Err(bad_request(format!(
            "effective_to（{end}）不能早于 effective_from（{effective_from}）"
        )));
    }
    let status = normalize_status(req.status)?.unwrap_or(current.status);
    let remark = match req.remark.as_ref() {
        None => current.remark.clone(),
        Some(raw) => crate::business::erp::cost::cost_labor_dto::clean_optional(Some(raw.clone())),
    };
    Ok(CostMethodSpec {
        method,
        scope_type,
        scope_id,
        effective_from,
        effective_to,
        status,
        remark,
    })
}

// ==================== 单测（纯函数，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn row(method: &str, scope_type: &str, scope_id: Option<Uuid>, from: &str, to: Option<&str>) -> MethodConfigView {
        MethodConfigView {
            id: Uuid::now_v7(),
            method: method.to_string(),
            scope_type: scope_type.to_string(),
            scope_id,
            effective_from: date(from),
            effective_to: to.map(date),
            status: STATUS_ENABLED,
        }
    }

    // ---------- 值域 / 标签 ----------

    /// 三种方法、两种范围的值域与标签必须一一对上（改一个字母界面就说错话）。
    #[test]
    fn value_domains_and_labels_are_stable() {
        assert_eq!(ALL_METHODS, ["JOB", "VARIETY", "STANDARD"]);
        assert_eq!(ALL_SCOPES, ["COST_OBJECT", "GLOBAL"]);
        assert_eq!(method_label(METHOD_JOB), "分批法");
        assert_eq!(method_label(METHOD_VARIETY), "品种法");
        assert_eq!(method_label(METHOD_STANDARD), "标准成本");
        assert_eq!(scope_label(SCOPE_GLOBAL), "全公司默认");
        assert_eq!(scope_label(SCOPE_OBJECT), "指定成本对象");
        assert_eq!(DEFAULT_METHOD, METHOD_JOB);
    }

    /// 一期只有分批法算得出来；另外两种必须如实标「未实现」。
    #[test]
    fn only_job_is_implemented_in_phase_one() {
        assert!(method_implemented(METHOD_JOB));
        assert!(!method_implemented(METHOD_VARIETY));
        assert!(!method_implemented(METHOD_STANDARD));
        assert!(method_note(METHOD_VARIETY).contains("三期"));
        assert!(method_note(METHOD_STANDARD).contains("二期"));
        assert!(method_note(METHOD_JOB).contains("已实现"));
    }

    /// 方法 / 范围的归一化：大小写与空白都吃，值域外一律 400。
    #[test]
    fn normalize_accepts_case_and_rejects_unknown() {
        assert_eq!(normalize_method(Some(" job ")).expect("小写合法"), "JOB");
        assert_eq!(normalize_method(Some("Variety")).expect("混合大小写合法"), "VARIETY");
        assert!(normalize_method(Some("FIFO")).is_err());
        assert!(normalize_method(None).is_err());
        assert_eq!(normalize_scope(Some("global")).expect("小写合法"), "GLOBAL");
        assert!(normalize_scope(Some("PRODUCT")).is_err());
        assert_eq!(normalize_status(Some(0)).expect("0 合法"), Some(0));
        assert!(normalize_status(Some(2)).is_err());
    }

    // ---------- 判定 ----------

    /// 什么都没有 ⇒ 代码兜底分批法（一期默认口径），来源标成 DEFAULT。
    #[test]
    fn empty_config_falls_back_to_job() {
        let resolution = resolve(&[], None, date("2026-11-30"));
        assert_eq!(resolution.method, METHOD_JOB);
        assert_eq!(resolution.source, SOURCE_DEFAULT);
        assert!(resolution.config.is_none());
    }

    /// 对象级压过全局级。
    #[test]
    fn object_level_beats_global() {
        let object = Uuid::now_v7();
        let rows = vec![
            row(METHOD_STANDARD, SCOPE_GLOBAL, None, "2020-01-01", None),
            row(METHOD_VARIETY, SCOPE_OBJECT, Some(object), "2026-01-01", None),
        ];
        let resolution = resolve(&rows, Some(object), date("2026-11-30"));
        assert_eq!(resolution.method, METHOD_VARIETY);
        assert_eq!(resolution.source, SOURCE_OBJECT);

        // 别的对象看不见这条对象级配置 → 落到全局
        let other = resolve(&rows, Some(Uuid::now_v7()), date("2026-11-30"));
        assert_eq!(other.method, METHOD_STANDARD);
        assert_eq!(other.source, SOURCE_GLOBAL);

        // 不指名对象（只问全公司默认）→ 全局
        let global_only = resolve(&rows, None, date("2026-11-30"));
        assert_eq!(global_only.source, SOURCE_GLOBAL);
    }

    /// 生效期两端都是「含」；过期 / 未生效的版本都不算命中。
    #[test]
    fn effective_window_is_inclusive_on_both_ends() {
        let rows = vec![row(
            METHOD_VARIETY,
            SCOPE_GLOBAL,
            None,
            "2026-03-01",
            Some("2026-05-31"),
        )];
        assert_eq!(resolve(&rows, None, date("2026-02-28")).source, SOURCE_DEFAULT);
        assert_eq!(resolve(&rows, None, date("2026-03-01")).method, METHOD_VARIETY);
        assert_eq!(resolve(&rows, None, date("2026-05-31")).method, METHOD_VARIETY);
        assert_eq!(resolve(&rows, None, date("2026-06-01")).source, SOURCE_DEFAULT);
    }

    /// 同级多条版本：取生效最晚的一条（后生效覆盖先生效）。
    #[test]
    fn newest_effective_version_wins() {
        let rows = vec![
            row(METHOD_JOB, SCOPE_GLOBAL, None, "2020-01-01", None),
            row(METHOD_STANDARD, SCOPE_GLOBAL, None, "2026-07-01", None),
        ];
        let resolution = resolve(&rows, None, date("2026-11-30"));
        assert_eq!(resolution.method, METHOD_STANDARD);
        assert_eq!(resolution.config.expect("命中行").effective_from, date("2026-07-01"));

        // 参照日退回到新版本生效之前 ⇒ 老版本
        assert_eq!(resolve(&rows, None, date("2026-06-30")).method, METHOD_JOB);
    }

    /// 停用的配置不参与判定（停用 = 「以后别再用这条」，不是删除）。
    #[test]
    fn disabled_rows_are_ignored() {
        let mut disabled = row(METHOD_STANDARD, SCOPE_GLOBAL, None, "2020-01-01", None);
        disabled.status = STATUS_DISABLED;
        let resolution = resolve(&[disabled], None, date("2026-11-30"));
        assert_eq!(resolution.method, METHOD_JOB);
        assert_eq!(resolution.source, SOURCE_DEFAULT);
    }

    // ---------- 区间重叠 ----------

    /// 区间相交的四种形态（含开端）。
    #[test]
    fn overlap_detection_covers_open_ended_ranges() {
        // 完全包含
        assert!(ranges_overlap(
            date("2026-01-01"),
            Some(date("2026-12-31")),
            date("2026-05-01"),
            Some(date("2026-06-30"))
        ));
        // 同一天起
        assert!(ranges_overlap(date("2026-05-01"), None, date("2026-05-01"), None));
        // 相邻不重叠（旧版止于 4-30，新版 5-1 起）
        assert!(!ranges_overlap(
            date("2026-01-01"),
            Some(date("2026-04-30")),
            date("2026-05-01"),
            None
        ));
        // 都在过去且不相交
        assert!(!ranges_overlap(
            date("2025-01-01"),
            Some(date("2025-06-30")),
            date("2025-07-01"),
            Some(date("2025-12-31"))
        ));
    }

    /// `find_overlap` 认范围（对象级 vs 全局互不影响）与排除自己。
    #[test]
    fn find_overlap_respects_scope_and_exclusion() {
        let object = Uuid::now_v7();
        let existing = row(METHOD_JOB, SCOPE_OBJECT, Some(object), "2026-01-01", Some("2026-06-30"));
        let rows = vec![existing.clone()];
        let spec = CostMethodSpec {
            method: METHOD_VARIETY.to_string(),
            scope_type: SCOPE_OBJECT.to_string(),
            scope_id: Some(object),
            effective_from: date("2026-06-01"),
            effective_to: None,
            status: STATUS_ENABLED,
            remark: None,
        };
        assert!(find_overlap(&rows, &spec, None).is_some(), "重叠必须被找出来");
        assert!(
            find_overlap(&rows, &spec, Some(existing.id)).is_none(),
            "改自己不算重叠"
        );
        // 全局范围与对象级不同范围，不算重叠
        let global_spec = CostMethodSpec {
            scope_type: SCOPE_GLOBAL.to_string(),
            scope_id: None,
            ..spec.clone()
        };
        assert!(find_overlap(&rows, &global_spec, None).is_none());

        // 停用的行不算重叠（否则「停用旧的再建新的」这条路会被自己堵死）
        let mut disabled = existing.clone();
        disabled.status = STATUS_DISABLED;
        assert!(find_overlap(&[disabled], &spec, None).is_none(), "停用行不该拦住新版本");
    }

    // ---------- 入参 ----------

    /// 新建：全局不能带对象、对象级必须给对象、停止日不能早于生效日。
    #[test]
    fn create_spec_enforces_scope_pairing() {
        let base = CostMethodCreateReq {
            method: Some("job".to_string()),
            scope: Some("global".to_string()),
            scope_id: None,
            effective_from: Some("2026-11-01".to_string()),
            effective_to: None,
            status: None,
            remark: None,
        };
        let spec = spec_from_create(&base).expect("全局默认合法");
        assert_eq!(spec.scope_type, SCOPE_GLOBAL);
        assert!(spec.scope_id.is_none());
        assert_eq!(spec.status, STATUS_ENABLED, "状态缺省 = 启用");

        let with_object = CostMethodCreateReq {
            scope_id: Some(Uuid::now_v7().to_string()),
            ..base.clone()
        };
        assert!(spec_from_create(&with_object).is_err(), "全局不能带对象");

        let object_without_id = CostMethodCreateReq {
            scope: Some("COST_OBJECT".to_string()),
            ..base.clone()
        };
        assert!(spec_from_create(&object_without_id).is_err(), "对象级必须给对象");

        let bad_range = CostMethodCreateReq {
            effective_to: Some("2026-10-01".to_string()),
            ..base.clone()
        };
        assert!(spec_from_create(&bad_range).is_err(), "停止日早于生效日必须拒");

        let missing_from = CostMethodCreateReq {
            effective_from: None,
            ..base
        };
        assert!(spec_from_create(&missing_from).is_err(), "生效日必填");
    }

    /// 修改：缺省不改；`effective_to` 空串 = 清空（改回长期有效）。
    #[test]
    fn update_spec_treats_blank_as_clear() {
        let current = CostMethodSpec {
            method: METHOD_JOB.to_string(),
            scope_type: SCOPE_GLOBAL.to_string(),
            scope_id: None,
            effective_from: date("2026-01-01"),
            effective_to: Some(date("2026-06-30")),
            status: STATUS_ENABLED,
            remark: Some("旧备注".to_string()),
        };
        let noop = spec_from_update(&CostMethodUpdateReq::default(), &current).expect("空改动合法");
        assert_eq!(noop, current);

        let cleared = spec_from_update(
            &CostMethodUpdateReq {
                effective_to: Some(String::new()),
                remark: Some(String::new()),
                ..Default::default()
            },
            &current,
        )
        .expect("清空合法");
        assert!(cleared.effective_to.is_none(), "空串 = 长期有效");
        assert!(cleared.remark.is_none(), "空串 = 清空备注");

        let disabled = spec_from_update(
            &CostMethodUpdateReq {
                status: Some(STATUS_DISABLED),
                ..Default::default()
            },
            &current,
        )
        .expect("停用合法");
        assert_eq!(disabled.status, STATUS_DISABLED);
        assert_eq!(disabled.effective_to, current.effective_to);
    }

    // ---------- 错误构造 ----------

    /// 计算入口的拦截面报文必须点名对象、方法与三条出路（用户照着能修，不用问工程师）。
    #[test]
    fn unimplemented_error_names_subject_method_and_ways_out() {
        let error = unimplemented_error(
            "2026-11",
            &[
                ("CB-20261101-000001".to_string(), METHOD_STANDARD.to_string()),
                ("全公司默认".to_string(), METHOD_VARIETY.to_string()),
            ],
        );
        assert_eq!(error.status, 409);
        let text = format!("{error:?}");
        assert!(text.contains("66101"), "必须带业务码 66101：{text}");
        assert!(text.contains("CB-20261101-000001"), "必须点名成本对象：{text}");
        assert!(text.contains("标准成本"), "必须点名方法：{text}");
        assert!(text.contains("停用"), "必须给替换动作：{text}");
    }

    /// 删除拦截的文案要点名「哪一天起生效」与「改停用」。
    #[test]
    fn delete_in_use_explains_the_way_out() {
        let error = delete_in_use(date("2026-01-01"), date("2026-11-18"));
        assert_eq!(error.status, 409);
        let text = format!("{error:?}");
        assert!(text.contains("66104"));
        assert!(text.contains("2026-01-01"));
        assert!(text.contains("停用"));
    }

    /// 改写已生效版本：报文要点名「能改的三样」与「换方法怎么做」。
    #[test]
    fn locked_effective_points_at_the_three_allowed_fields() {
        let error = locked_effective(
            "CB-20261101-000001",
            METHOD_STANDARD,
            date("2026-01-01"),
            date("2026-11-18"),
        );
        assert_eq!(error.status, 409);
        let text = format!("{error:?}");
        assert!(text.contains("66105"), "{text}");
        assert!(text.contains("标准成本"), "必须点名现有方法：{text}");
        assert!(text.contains("停止日"), "必须说清能改什么：{text}");
        assert!(text.contains("停用"), "必须给配错时的出路：{text}");
    }

    /// 5 个业务码的值不许漂（前端 `error-code.ts` 文案按值映射）。
    #[test]
    fn business_codes_are_stable() {
        assert_eq!(CODE_UNIMPLEMENTED, 66101);
        assert_eq!(CODE_OVERLAP, 66102);
        assert_eq!(CODE_SCOPE_INVALID, 66103);
        assert_eq!(CODE_DELETE_IN_USE, 66104);
        assert_eq!(CODE_LOCKED_EFFECTIVE, 66105);
    }

    /// `range_text` 两种形态（报文里「长期」不能显示成 `None`）。
    #[test]
    fn range_text_is_human_readable() {
        assert_eq!(range_text(date("2026-01-01"), None), "2026-01-01 ~ 长期");
        assert_eq!(
            range_text(date("2026-01-01"), Some(date("2026-12-31"))),
            "2026-01-01 ~ 2026-12-31"
        );
    }
}
