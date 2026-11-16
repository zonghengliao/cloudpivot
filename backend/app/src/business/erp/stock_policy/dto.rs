//! 水位设置（M2-13）：物料 × 仓库 的**最低 / 最高水位**——契约 + 纯逻辑 + 「生效水位」取值口径。
//!
//! 卡：t_d997ada8（M2-13【M2·仓库】安全库存：字段（物料 × 仓库）最低/最高水位）；拆卡依据
//! `docs/planning/20261113-新卡清单.md` §M2 的 M2-13 行 —— 水位检查与生成待办（M2-14）、
//! 补货建议（M2-15）、单测扩充（M2-16）、前端设置页（M2-17）都不在本卡。
//!
//! ```text
//! 表            biz_material_stock_policy（V0428，一格一行：物料 × 仓库）
//! 设置接口      PUT /business/erp/stock-policy          ← 本模块（upsert，幂等）
//! 列表接口      GET /business/erp/stock-policy/page     ← 本模块
//! 删除接口      DELETE /business/erp/stock-policy/{id}  ← 本模块（软删 = 该格回退物料级）
//! 读数口径      model::effective_of(物料, 仓库) → EffectiveLevels
//! 取值口径      dto::resolve_min_qty / resolve_max_qty  ← **唯一实现**，M2-14/15 必须调它
//! 结存          永远现算 biz_inventory.qty（设置表里不存、不缓存结存）
//! ```
//!
//! # 两个水位分别回答什么
//!
//! | 列 | 名字 | 回答的问题 | 未设时 |
//! |:--|:--|:--|:--|
//! | `min_qty` | 最低水位（= 该格的安全库存） | 结存掉到多少就该补 | 回退 `biz_material.safety_stock`，都为 0 则不判 |
//! | `max_qty` | 最高水位 | 结存涨到多少就算压货、别再补 | 不判超储 |
//!
//! 这两条**只描述设置本身**：本模块不算「是否低于水位」，也不产生待办（那是 M2-14）。
//! 本模块交付的是「一格的水位是多少」这件事的唯一出口 —— 下游谁要判水位，读的都是
//! [`resolve_min_qty`] / [`resolve_max_qty`]，不许自己在 SQL 或 Rust 里再写一份三级回退
//! （写两份的那天起，「设置页显示 100、待办按 50 报警」就没有任何地方会报错）。
//!
//! # 生效最低水位的三级口径（写死，别再解释第二遍）
//!
//! ```text
//! 有策略行 且 status = 1  → 该行 min_qty（0 = 本格明示不预警，**不**回退物料级）   source = POLICY
//! 有策略行 但 status = 0  → 物料级 safety_stock（> 0 才算，否则不判）              source = MATERIAL / NONE
//! 没有策略行              → 同上                                                  source = MATERIAL / NONE
//! ```
//!
//! 两条容易吵的取舍，理由在 V0428 的建表注释里（那里是唯一版本）：
//!
//! 1. **「停用」退回物料级而不是「不判」**：停用 = 保留数值但不参与，语义上等同「从未设置过」；
//! 2. **策略行的 `0` 会阻断回退**：物料级的 `0` 是「没设」，策略行的 `0` 是**明示**的
//!    「这一格不预警」（寄售仓 / 客户自提仓）。要让一格跟随物料级，正确做法是删掉策略行。
//!
//! # 为什么「最高水位」没有物料级回退
//!
//! `biz_material` 上从来没有最高水位这个字段（V0137），凭空造一个「缺省最高水位」就是拍数。

use nik::prelude::rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// ==================== 常量 ====================

/// 状态：启用（参与水位检查）。
pub const STATUS_ENABLED: i16 = 1;
/// 状态：停用（保留设置数值，退回按物料级 `safety_stock` 判）。
pub const STATUS_DISABLED: i16 = 0;
/// 状态域白名单（与 `ck_biz_msp_status` 逐字一致）。
pub const STATUSES: [i16; 2] = [STATUS_ENABLED, STATUS_DISABLED];

/// 状态是否在域内。
pub fn is_valid_status(status: i16) -> bool {
    STATUSES.contains(&status)
}

/// 状态筛选文案：`1` / `0`；空白 → `None`（不筛）；其它 → `Err(人话)`。
///
/// 与既有 ERP 查询一致：**不静默忽略**非法筛选（`status=abc` 当没传会把
/// 「筛不出东西」伪装成「返回全部」）。
pub fn parse_status_filter(raw: Option<&str>) -> std::result::Result<Option<i16>, String> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    match text {
        "1" => Ok(Some(STATUS_ENABLED)),
        "0" => Ok(Some(STATUS_DISABLED)),
        other => Err(format!("status 只能是 1（启用）或 0（停用），当前 `{other}`")),
    }
}

/// 备注：空白串一律落 `NULL`（`''` 与 `NULL` 两种「没备注」不该并存）。
pub fn normalize_remark(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 最低水位守卫：`>= 0`（负数水位没有语义；`0` = 本格不预警）。
pub fn min_qty_allowed(value: Decimal) -> bool {
    value >= Decimal::ZERO
}

/// 最高水位守卫：未设（`None`）= 不判超储；设了必须**严格大于**最低水位
/// （相等 = 空区间，`min = max = 100` 既「低于就补」又「高于就停」，自相矛盾）。
pub fn max_qty_allowed(min_qty: Decimal, max_qty: Option<Decimal>) -> bool {
    match max_qty {
        None => true,
        Some(max) => max > min_qty,
    }
}

// ==================== 入参 ====================

/// 水位设置列表筛选（`GET /business/erp/stock-policy/page`）。
///
/// 四个条件都可留空：`material_id` / `warehouse_id` 精确命中（留空 = 全量），
/// `keyword` 命中物料或仓库的编码 / 名称，`status` 见 [`parse_status_filter`]。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PolicyFilter {
    /// 物料 id（精确 UUID；留空 = 全部物料）。
    pub material_id: Option<String>,
    /// 仓库 id（精确 UUID；留空 = 全部仓库）。
    pub warehouse_id: Option<String>,
    /// 状态（`1` / `0`；留空 = 全部）。
    pub status: Option<String>,
    /// 关键字（物料编码 / 名称 / 仓库编码 / 名称）。
    pub keyword: Option<String>,
    /// 页码（从 1 起；`0` 回落第 1 页）。
    pub page: u64,
    /// 每页条数（`0` 回落 20，上限 200）。
    pub page_size: u64,
}

/// 设置水位（`PUT /business/erp/stock-policy`，按 物料 × 仓库 **upsert**）。
///
/// 一次请求 = 一格设置落库，幂等：同一格再发一次就是改；不存在就是建。
/// `material_id` / `warehouse_id` 是**键**（决定了改的是哪一格），编辑时不可换格 ——
/// 换格等于删掉一格再设另一格，语义不同（会让旧格悄悄回退物料级），本接口不提供。
#[derive(Debug, Clone, Deserialize)]
pub struct SavePolicyReq {
    /// 物料 id（必填，必须存在且未软删）。
    pub material_id: String,
    /// 仓库 id（必填，必须存在且未软删）。
    pub warehouse_id: String,
    /// 最低水位（必填；`>= 0`，`0` = 本格不预警）。
    pub min_qty: Decimal,
    /// 最高水位（选填；`null` / 不传 = 未设，不判超储；设了必须 > `min_qty`）。
    #[serde(default)]
    pub max_qty: Option<Decimal>,
    /// 状态（选填；不传 = 1 启用）。
    #[serde(default)]
    pub status: Option<i16>,
    /// 备注（选填；为什么这么设，给下一个人看）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 校验后的设置内容（handler 用，落库前唯一形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyWrite {
    /// 最低水位。
    pub min_qty: Decimal,
    /// 最高水位（`None` = 未设）。
    pub max_qty: Option<Decimal>,
    /// 状态。
    pub status: i16,
    /// 备注（已归一化：空白 → `None`）。
    pub remark: Option<String>,
}

impl SavePolicyReq {
    /// 契约校验（纯逻辑，不碰库）：最低水位 >= 0 / 最高水位 > 最低 / 状态在域内。
    ///
    /// 非法值报 400 并**点名是哪一项**（`min_qty` / `max_qty` / `status`），
    /// 不静默夹取（把 `-5` 悄悄改成 0 会让用户以为「我设的就是 -5」）。
    pub fn to_write(&self) -> std::result::Result<PolicyWrite, String> {
        if !min_qty_allowed(self.min_qty) {
            return Err(format!(
                "min_qty 必须 >= 0（当前 {}）：0 = 该物料在该仓库本格不预警",
                self.min_qty
            ));
        }
        if !max_qty_allowed(self.min_qty, self.max_qty) {
            return Err(format!(
                "max_qty（{}）必须严格大于 min_qty（{}）：相等的上下界是空区间，请清空最高水位或提高它",
                self.max_qty
                    .map(|qty| qty.to_string())
                    .unwrap_or_else(|| "空".to_string()),
                self.min_qty
            ));
        }
        let status = self.status.unwrap_or(STATUS_ENABLED);
        if !is_valid_status(status) {
            return Err(format!("status 只能是 1（启用）或 0（停用），当前 {status}"));
        }
        Ok(PolicyWrite {
            min_qty: self.min_qty,
            max_qty: self.max_qty,
            status,
            remark: normalize_remark(self.remark.clone()),
        })
    }
}

// ==================== 出参 ====================

/// 一行水位设置（列表 / 设置回读都用它）。
///
/// 名字（`material_*` / `warehouse_*`）是**主数据关联**（AGENTS.md 约定 13）：
/// 实时取「现在叫什么」，主数据软删后为 `null`，设置行照常可读。
#[derive(Debug, Clone, Serialize)]
pub struct PolicyRes {
    /// 设置行 id。
    pub id: String,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（软删后 `null`）。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 规格型号。
    pub spec: Option<String>,
    /// 计量单位名。
    pub unit_name: Option<String>,
    /// 仓库 id。
    pub warehouse_id: String,
    /// 仓库编码。
    pub warehouse_code: Option<String>,
    /// 仓库名称。
    pub warehouse_name: Option<String>,
    /// 最低水位（= 该格的安全库存阈值）。
    pub min_qty: Decimal,
    /// 最高水位（`null` = 未设）。
    pub max_qty: Option<Decimal>,
    /// 状态（`1` 启用 / `0` 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: nik::prelude::chrono::DateTime<nik::prelude::chrono::Utc>,
    /// 更新时间。
    pub updated_at: nik::prelude::chrono::DateTime<nik::prelude::chrono::Utc>,
}

// ==================== 生效水位（下游唯一入口） ====================

/// 一格的水位设置（从库里读出来的三列，**不含**结存 —— 结存永远现算）。
///
/// `enabled` 由 `status` 折算：下游不需要再记住 `1` / `0` 这两个数字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyLevels {
    /// 最低水位（0 = 本格不预警）。
    pub min_qty: Decimal,
    /// 最高水位（`None` = 未设）。
    pub max_qty: Option<Decimal>,
    /// 是否启用（`status = 1`）。
    pub enabled: bool,
}

impl PolicyLevels {
    /// 从三列原值构造（`status` 只认 `1`；域外的值按停用处理 —— 库层 CHECK 已挡住，
    /// 这里只是不给下游留 panic 的机会）。
    pub fn new(min_qty: Decimal, max_qty: Option<Decimal>, status: i16) -> Self {
        Self {
            min_qty,
            max_qty,
            enabled: status == STATUS_ENABLED,
        }
    }
}

/// 生效最低水位来自哪一层（回显给用户：「这格为什么按 50 报警」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinSource {
    /// 策略行（本格设置）。
    Policy,
    /// 物料级 `biz_material.safety_stock`（没设策略 / 策略停用）。
    Material,
    /// 都没有 → 不判水位。
    Unset,
}

impl MinSource {
    /// 出参字面量（`POLICY` / `MATERIAL` / `NONE`）。
    pub fn as_str(self) -> &'static str {
        match self {
            MinSource::Policy => "POLICY",
            MinSource::Material => "MATERIAL",
            MinSource::Unset => "NONE",
        }
    }
}

/// 生效最低水位的解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveMin {
    /// 生效阈值（`None` = 不判水位）。
    pub qty: Option<Decimal>,
    /// 来源。
    pub source: MinSource,
}

/// **生效最低水位**（唯一实现，M2-14 / M2-15 必须调它）。
///
/// 口径见模块头注释的三行表；`material_safety_stock` 传 `biz_material.safety_stock` 原值
/// （物料软删 / 查不到时传 `Decimal::ZERO`，效果与「物料级没设」相同）。
///
/// 边界：策略启用且 `min_qty = 0` → `qty = None` 但 `source = POLICY`（明示不预警，
/// 语义与「没设」可区分 —— 这正是排查「这格为什么不报警」时要看的那一列）。
pub fn resolve_min_qty(policy: Option<&PolicyLevels>, material_safety_stock: Decimal) -> EffectiveMin {
    if let Some(levels) = policy.filter(|levels| levels.enabled) {
        return EffectiveMin {
            qty: Some(levels.min_qty).filter(|qty| *qty > Decimal::ZERO),
            source: MinSource::Policy,
        };
    }
    let material = material_safety_stock.max(Decimal::ZERO);
    if material > Decimal::ZERO {
        EffectiveMin {
            qty: Some(material),
            source: MinSource::Material,
        }
    } else {
        EffectiveMin {
            qty: None,
            source: MinSource::Unset,
        }
    }
}

/// **生效最高水位**：只来自启用中的策略行（物料级没有这个字段，见模块头注释）。
pub fn resolve_max_qty(policy: Option<&PolicyLevels>) -> Option<Decimal> {
    policy.filter(|levels| levels.enabled).and_then(|levels| levels.max_qty)
}

// ==================== 水位检查（M2-14）的入参 / 出参 ====================
//
// 卡：t_df0d7d48（M2-14【M2·仓库】安全库存：水位检查（低于 → 生成待办））。
// 检查本体（扫描 / 判定 / 待办生成与收口）在 [`crate::business::erp::stock_policy::water_check`]，
// 本处只放**契约**：一个入参、一组出参字段。分开的理由与 M2-13 一致 ——
// dto 回答「长什么样」，water_check 回答「怎么算」。

/// 触发一次水位检查（`POST /business/erp/stock-policy/check`，M2-14）。
///
/// 三个字段都可留空：**默认全库扫**。这是刻意的默认 —— 「检查水位」是运维/仓管的一句话动作，
/// 让他先挑物料再检查等于多一步；限定范围是排查用的（「这个物料怎么没生成待办」）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaterCheckReq {
    /// 只查这个物料（留空 = 全部物料）。
    pub material_id: Option<String>,
    /// 只查这个仓库（留空 = 全部仓库）。
    pub warehouse_id: Option<String>,
    /// 只算不写（留空 = `false`）。
    ///
    /// `true` 时**不创建也不关闭**任何待办，只在响应里给出「本来会做什么」——
    /// 用于验收、排障（「为什么这一格没有待办」），以及上线前先看一眼会炸出多少条。
    pub dry_run: bool,
}

/// 一行「低于最低水位」的明细（出参）。数量一律走**文本**（与 ERP 其余数量出参同口径：
/// `NUMERIC` 直接序列化成 JSON number 会在前端丢精度）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WaterCheckItem {
    /// 库存行 id（= 待办的 `doc_id`）。
    pub inventory_id: String,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库 id。
    pub warehouse_id: String,
    /// 仓库名称（仓库被删时为「未知仓库」）。
    pub warehouse_name: String,
    /// 账面结存（`biz_inventory.qty`，已过账；可为负）。
    pub qty: String,
    /// 生效最低水位。
    pub min_qty: String,
    /// 缺口 = 最低水位 − 结存（正数）。
    pub gap_qty: String,
    /// 水位来源：`POLICY`（该格设置）/ `MATERIAL`（物料级安全库存）/ `NONE`。
    pub min_source: String,
    /// 本格在这一轮发生了什么：`RAISED` 新建待办 / `EXISTS` 已有未完成待办 /
    /// `RECENT` 重提醒间隔内刚提醒过 / `NO_OWNER` 定位不到负责人 / `DRY_RUN` 只算不写。
    pub action: String,
    /// 新建的待办编号（只有 `RAISED` 有）。
    pub task_no: Option<String>,
}

/// 一轮水位检查的结果。
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct WaterCheckRes {
    /// 本轮扫描的格数（物料 × 仓库）。
    pub scanned: u64,
    /// 低于最低水位的格数。
    pub low: u64,
    /// 本轮**新建**的待办数。
    pub raised: u64,
    /// 已有未完成待办而跳过的格数。
    pub skipped_open: u64,
    /// 重提醒间隔内刚提醒过而跳过的格数。
    pub skipped_recent: u64,
    /// 定位不到负责人而跳过的格数（已打 WARN，不静默丢弃）。
    pub skipped_no_owner: u64,
    /// 库存回升而**自动关闭**的待办数（`dry_run` 时是「将会关闭」的条数）。
    pub closed: u64,
    /// 单格处理失败（数据库异常等，下一轮自动重试；已打 WARN）。
    pub failed: u64,
    /// 本次是否只算不写。
    pub dry_run: bool,
    /// 本轮使用的负责人（UUID 文本；`None` = 没人可派）。
    pub owner: Option<String>,
    /// 负责人从哪来：`ROLE`（仓管主管角色）/ `OPERATOR`（手动触发人兜底）/ `NONE`。
    pub owner_source: String,
    /// 低于水位的明细（上限 [`crate::business::erp::stock_policy::water_check::MAX_ITEMS`] 条）。
    pub items: Vec<WaterCheckItem>,
    /// 一句给人看的结论（定时任务页 / 日志 / 接口回显共用）。
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点字面量")
    }

    fn levels(min: &str, max: Option<&str>, status: i16) -> PolicyLevels {
        PolicyLevels::new(dec(min), max.map(dec), status)
    }

    /// 状态域：只有 1 / 0；筛选参数非法必须报错（不静默当没传）。
    #[test]
    fn status_domain_and_filter_are_strict() {
        assert!(is_valid_status(STATUS_ENABLED));
        assert!(is_valid_status(STATUS_DISABLED));
        for bad in [-1, 2, 9] {
            assert!(!is_valid_status(bad), "{bad} 不在域内");
        }
        assert_eq!(parse_status_filter(None).expect("空"), None);
        assert_eq!(parse_status_filter(Some("  ")).expect("空白"), None);
        assert_eq!(parse_status_filter(Some("1")).expect("启用"), Some(STATUS_ENABLED));
        assert_eq!(parse_status_filter(Some("0")).expect("停用"), Some(STATUS_DISABLED));
        for bad in ["2", "abc", "-1", "enabled"] {
            let error = parse_status_filter(Some(bad)).expect_err("必须拒");
            assert!(error.contains("status"), "文案要点名字段：{error}");
        }
        println!("[M2-13] 状态域 1/0；筛选参数 2/abc/-1 一律 400（不静默当没传）✓");
    }

    /// 契约校验：最低 >= 0、最高必须严格大于最低；报错点名是哪一项。
    #[test]
    fn write_validation_names_the_offending_field() {
        let ok = SavePolicyReq {
            material_id: "m".to_string(),
            warehouse_id: "w".to_string(),
            min_qty: dec("100"),
            max_qty: Some(dec("500")),
            status: None,
            remark: Some("  备件仓  ".to_string()),
        };
        let write = ok.to_write().expect("合法");
        assert_eq!(write.status, STATUS_ENABLED, "不传 status = 启用");
        assert_eq!(write.remark.as_deref(), Some("备件仓"), "备注两端空白被去掉");

        // 最低水位负数
        let mut bad = ok.clone();
        bad.min_qty = dec("-1");
        let error = bad.to_write().expect_err("负数必须拒");
        assert!(error.contains("min_qty"), "{error}");

        // 最高水位 == 最低水位（空区间）
        let mut bad = ok.clone();
        bad.max_qty = Some(dec("100"));
        let error = bad.to_write().expect_err("相等必须拒");
        assert!(error.contains("max_qty"), "{error}");

        // 最高水位 < 最低水位
        let mut bad = ok.clone();
        bad.max_qty = Some(dec("99.9999"));
        assert!(bad.to_write().is_err(), "最高 < 最低必须拒");

        // 状态域外
        let mut bad = ok.clone();
        bad.status = Some(2);
        let error = bad.to_write().expect_err("状态域外必须拒");
        assert!(error.contains("status"), "{error}");

        // 最高水位留空 = 不判超储
        let mut no_max = ok.clone();
        no_max.max_qty = None;
        assert_eq!(no_max.to_write().expect("无上限").max_qty, None);

        // 备注空串 → None（'' 与 NULL 不并存）
        let mut blank = ok;
        blank.remark = Some("   ".to_string());
        assert_eq!(blank.to_write().expect("合法").remark, None);
        println!("[M2-13] 校验：min<0 / max<=min / status 域外 全部 400 并点名字段 ✓");
    }

    /// 生效最低水位三级：策略启用 > 物料级 > 不判。
    #[test]
    fn resolve_min_prefers_policy_then_material_then_unset() {
        // ① 策略启用 → 用策略（哪怕物料级也设了）
        let resolved = resolve_min_qty(Some(&levels("120", None, STATUS_ENABLED)), dec("50"));
        assert_eq!(resolved.qty, Some(dec("120")));
        assert_eq!(resolved.source, MinSource::Policy);
        assert_eq!(resolved.source.as_str(), "POLICY");

        // ② 没有策略 → 物料级
        let resolved = resolve_min_qty(None, dec("50"));
        assert_eq!(resolved.qty, Some(dec("50")));
        assert_eq!(resolved.source, MinSource::Material);

        // ③ 策略停用 → 回退物料级（不是「不判」）
        let resolved = resolve_min_qty(Some(&levels("120", None, STATUS_DISABLED)), dec("50"));
        assert_eq!(resolved.qty, Some(dec("50")), "停用 = 等同没设置");
        assert_eq!(resolved.source, MinSource::Material);

        // ④ 都没有 → 不判
        let resolved = resolve_min_qty(None, Decimal::ZERO);
        assert_eq!(resolved.qty, None);
        assert_eq!(resolved.source, MinSource::Unset);
        assert_eq!(resolved.source.as_str(), "NONE");

        // ⑤ 策略启用但 min=0 → 明示不预警（source 仍是 POLICY，可区分于「没设」）
        let resolved = resolve_min_qty(Some(&levels("0", None, STATUS_ENABLED)), dec("50"));
        assert_eq!(resolved.qty, None, "策略行的 0 阻断回退物料级");
        assert_eq!(resolved.source, MinSource::Policy);

        // ⑥ 物料级负数（脏数据）按 0 处理，不给下游负数阈值
        let resolved = resolve_min_qty(None, dec("-5"));
        assert_eq!(resolved.qty, None);
        assert_eq!(resolved.source, MinSource::Unset);
        println!("[M2-13] 三级取值：策略启用 > 物料级 > 不判；停用回退、0 阻断回退 ✓");
    }

    /// 生效最高水位：只来自启用中的策略行。
    #[test]
    fn resolve_max_only_from_enabled_policy() {
        assert_eq!(
            resolve_max_qty(Some(&levels("100", Some("800"), STATUS_ENABLED))),
            Some(dec("800"))
        );
        assert_eq!(
            resolve_max_qty(Some(&levels("100", Some("800"), STATUS_DISABLED))),
            None
        );
        assert_eq!(resolve_max_qty(Some(&levels("100", None, STATUS_ENABLED))), None);
        assert_eq!(resolve_max_qty(None), None);
        println!("[M2-13] 最高水位：只认启用中的策略行；停用 / 未设 / 无行 → 不判超储 ✓");
    }

    /// `PolicyLevels::new` 把 status 折成 enabled；域外值按停用（不 panic）。
    #[test]
    fn levels_fold_status_into_enabled() {
        assert!(PolicyLevels::new(dec("1"), None, STATUS_ENABLED).enabled);
        assert!(!PolicyLevels::new(dec("1"), None, STATUS_DISABLED).enabled);
        assert!(!PolicyLevels::new(dec("1"), None, 7).enabled, "域外值按停用，不 panic");
        println!("[M2-13] status → enabled 折算：1=启用、0/域外=停用 ✓");
    }
}
