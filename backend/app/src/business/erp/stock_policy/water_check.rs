//! 水位检查（M2-14）：结存 < 生效最低水位 → **生成待办**；库存回升 → **自动收口**。
//!
//! 卡：t_df0d7d48（M2-14【M2·仓库】安全库存：水位检查（低于 → 生成待办））。
//! 上游：M2-13（t_d997ada8）交付的 `biz_material_stock_policy` + 生效水位的唯一读数口径。
//!
//! ```text
//! 阈值      读 dto::resolve_min_qty（三级回退的唯一实现：策略启用 > 物料级 > 不判）
//! 结存      现算 biz_inventory.qty（设置表里不存结存；不缓存）
//! 判定      qty < 生效最低水位 → 低水位（负库存照实报，不 GREATEST 抹平）
//! 动作      低水位 → 生成一条 biz_task 待办（负责人 = 仓管主管）
//!           回升 / 不再判水位 → 把那条待办自动关闭（CANCELLED + 原因）
//! 载体      biz_task（doc_type = 'INVENTORY'，doc_id = biz_inventory.id）
//! 入口      ① 定时任务 erp_stock_water_check（每小时 :07 UTC，内置注册 + sys_job 种子）
//!           ② POST /business/erp/stock-policy/check（手动触发 / dry_run 预演）
//! ```
//!
//! # 为什么是「后台任务」而不是「过账时校验」（卡面写的是「后台任务或过账时校验」）
//!
//! 卡面给的是**或**，这里选了后台任务，理由三条：
//!
//! 1. **过账点有 8 处**（收货 / 出库 / 调拨 / 盘点 / 退货 / 生产入库 / 期初 / 红冲），
//!    每处都要在同一个事务里插任务行 —— 任务表的 `task_no` 唯一索引、`owner_id` 外键、
//!    `ck_biz_task_doc_pair` 任何一条不满足，**库存作业本身就会回滚**。
//!    「安全库存提醒」把一个 100 万的收货单打回，是比晚一小时提醒严重得多的事故。
//! 2. **过账后失败必须有补偿**（AGENTS.md 约定 11）：过账事务里做第二件事，就要为它写
//!    失败补偿路径；本文档末尾的「三层幂等」在后台任务里天然成立，在过账里要额外一堆补偿。
//! 3. **时效够**：水位是「补货决策」的信号，不是「过账能不能做」的前置条件。每小时一次
//!    （cron 见 [`CRON`]）+ 手动触发接口，最坏延迟一小时；`dry_run` 还能随时预演。
//!
//! 真要改成「过账即检」时，正确接法是**过账提交后**（不是事务内）异步调
//! [`run_check`] 并把 `material_id` / `warehouse_id` 限定成受影响的那一格 —— 本文件的
//! 过滤参数就是为这个用法留的，别在过账事务里直接插任务。
//!
//! # 待办长什么样
//!
//! | 字段 | 值 |
//! |:--|:--|
//! | `title` | `库存低于最低水位：{物料编码} {物料名称} @{仓库名}` |
//! | `description` | 结存 / 最低水位 / 缺口 / **水位来源**（为什么按 50 报警）/ 口径说明 |
//! | `owner_id` | 仓管主管角色下**确定性第一位**（多个人时不重复生成 N 条） |
//! | `creator_id` | 同 `owner_id`（系统生成、无人分派 → `source = SELF`） |
//! | `priority` | 结存 `<= 0` → `P1`（断料）；否则 `P2` |
//! | `doc_type` / `doc_id` | `INVENTORY` / `biz_inventory.id`（一格一条，机器可查） |
//! | `due_date` | 与手工建任务同一口径：本周五（`task::model::default_due_date`） |
//!
//! **不写** `assigner_id`（没有分派人）、**不写** `parent_id`（不是子任务）、
//! **不写** `approval_instance_id`（水位待办不是审批）。
//!
//! # 三层幂等（为什么同一格不会一天炸出 24 条待办）
//!
//! ```text
//! ① 未完成待办唯一：部分唯一索引 uk_biz_task_open_inventory（V0429）
//!      (doc_type, doc_id) WHERE doc_type = 'INVENTORY' AND deleted_at IS NULL
//!                          AND status IN ('TODO','DOING')
//!      → 同一格同时只能有一条「未完成」待办。这一层是**库层**的，多实例并发抢跑也只有一个
//!        插入成功，其余拿 UniqueViolation（本文件记成 skipped_open，不是失败）。
//! ② 本轮预读：先把这一格所有未完成待办一次读出（`open_todo_ids`），命中即跳过，
//!      避免对每一格都打一次「有没有」的查询（500 格 = 500 次往返）。
//! ③ 重提醒间隔：sys_config `erp.stock_water.realert_days`（缺省 3 天）。
//!      待办被**人**取消/完成（条件可能还在，人判断过）→ 间隔内不重新生成，避免「取消了又
//!      立刻冒出来」；而**系统自动收口**的待办（库存回升过）**不参与**抑制 —— 回升后又跌破
//!      是新事件，必须立刻提醒。区分靠 `cancel_reason` 的前缀 [`AUTO_CLOSE_PREFIX`]。
//! ```
//!
//! # 自动收口（水位「回升」这一半，卡面没写但必须做）
//!
//! 只生成不收回，一个月后每个人手里都是几百条「已经补过了」的待办，红点永远不消失 ——
//! 那比没有提醒更糟（用户会学会忽略红点）。所以每轮检查同时做反向核对：
//!
//! ```text
//! 未完成的水位待办  →  该格现在什么状态？
//!   库存行没了 / 物料停用或删除 / 仓库删除或在途仓 / 结存 >= 最低水位 / 不再判水位
//!     → CANCELLED + cancel_reason = 'AUTO_RECOVERED …'（可追溯、不参与重提醒抑制）
//!   仍然低于水位  → 不动（数量快照不刷新：待办是提醒，不是账）
//! ```
//!
//! 自动收口**只动 `doc_type = 'INVENTORY'` 的待办**，人手建的任务一条都不碰。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 检查是后台/运维动作，扫的是仓库作业台账（biz_inventory 无 dept_id / 归属人列），
//!               与 reminder_stock 的低库存扫描同口径（可见性由 business:erp:stock-policy:check
//!               这个写码控制；接口本身不返回别人的业务数据范围，只返回水位判定结果）。
//! ```

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use nik::prelude::chrono::{DateTime, Duration, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::reminder::{self, users_of_roles};
use crate::business::erp::reminder_stock::WAREHOUSE_ROLES;
use crate::business::erp::stock_policy::dto::{
    self, EffectiveMin, MinSource, PolicyLevels, WaterCheckItem, WaterCheckRes,
};
use crate::business::task::dto as task_dto;
use crate::business::task::model as task_model;
use crate::support::config::model::SysConfigModel;
use crate::task::Scheduler;

// ==================== 常量 ====================

/// 调度名（= `sys_job.invoke_target` = `sys_job_log` 的检索键）。
pub const TASK_NAME: &str = "erp_stock_water_check";

/// cron：每小时第 7 分（6 段写法，第 1 段是秒）。
///
/// 调度器按 **UTC** 字段匹配（`SchedulerPlugin` 的 clock = `Utc::now`），但本表达式的小时段是
/// `*` —— 每个小时都跑，「北京时间几点」这个问题不存在。错开第 7 分是为了避开整点的拥挤点
/// （日志清理 03:00/03:10、导出回收 04:40、通知清理 04:50、业务提醒 07:10~07:40）。
pub const CRON: &str = "0 7 * * * *";

/// 待办挂接的单据类型（`biz_task.doc_type`）：库存行。
///
/// 与 `biz_reminder_log.doc_type` 的 `INVENTORY` 逐字一致（同一个业务对象，两处不许两种写法）。
/// `doc_id` = `biz_inventory.id`（该表无 `deleted_at`，行永不复用）。
pub const DOC_TYPE: &str = "INVENTORY";

/// 一轮最多扫描多少格（物料 × 仓库）。
///
/// 与 `reminder` 的 `SCAN_BATCH` 同取舍：一次补跑不许把任务表/日志打爆。仓库行数
/// （物料 × 仓库）在万级以下，500 是保守值 —— 超出部分下一小时接着扫（排序是全序，
/// 翻轮不会漏也不会重复：重复由①②两层幂等挡住）。
pub const SCAN_LIMIT: i64 = 500;

/// 一轮响应最多带多少条明细（计数仍是全量；明细只给前 N 条，防止响应体过大）。
pub const MAX_ITEMS: usize = 200;

/// 重提醒间隔参数键（`sys_config`，单位：天）。
pub const CONFIG_REALERT_DAYS: &str = "erp.stock_water.realert_days";

/// 重提醒间隔缺省值（天）：参数缺失 / 停用 / 非正数时回退。
pub const DEFAULT_REALERT_DAYS: i64 = 3;

/// 自动收口的 `cancel_reason` 前缀。
///
/// 它是「这条待办是被系统按水位回升关掉的」的唯一标记：重提醒抑制查询靠
/// `NOT (status = 'CANCELLED' AND cancel_reason LIKE 'AUTO_RECOVERED%')` 把它们排除，
/// 于是「回升 → 又跌破」会立刻重新提醒，而「人手动取消」才享受间隔抑制。
pub const AUTO_CLOSE_PREFIX: &str = "AUTO_RECOVERED";

/// 自动收口的完整原因文案（写进 `biz_task.cancel_reason`，人可读）。
pub fn auto_close_reason(detail: &str) -> String {
    format!("{AUTO_CLOSE_PREFIX} {detail}（系统按水位检查自动关闭，库存回升后可重新提醒）")
}

/// 优先级的断料线：结存 `<= 0` 视为断料（负库存也在这里），给 `P1`。
pub const PRIORITY_SHORTAGE: &str = "P1";
/// 普通低水位：`P2`（默认优先级）。
pub const PRIORITY_LOW: &str = "P2";

// ==================== 一格的结构 ====================

/// 一个「物料 × 仓库」格子的判定输入：结存 + 两处水位事实。
///
/// `policy` 直接是**策略行原值**（含停用行），三级回退交给
/// [`dto::resolve_min_qty`] —— 本文件绝不自己判「策略启用还是物料级」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaterCell {
    /// `biz_inventory.id`（待办 `doc_id`）。
    pub inventory_id: Uuid,
    /// 物料 id。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库 id。
    pub warehouse_id: Uuid,
    /// 仓库名称（仓库软删时为「未知仓库」）。
    pub warehouse_name: String,
    /// 计量单位名（物料没配单位时为 `None`）。
    pub unit_name: Option<String>,
    /// 账面结存（`biz_inventory.qty`，可为负）。
    pub qty: Decimal,
    /// 物料级安全库存（`biz_material.safety_stock`，回退值）。
    pub material_safety_stock: Decimal,
    /// 该格策略行（`None` = 没有行；`Some{enabled:false}` = 设过但停用）。
    pub policy: Option<PolicyLevels>,
}

/// 判定结果：低于最低水位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowWater {
    /// 生效最低水位 + 来源。
    pub min: EffectiveMin,
    /// 缺口 = 最低水位 − 结存（恒为正）。
    pub gap: Decimal,
}

impl WaterCell {
    /// 生效最低水位（策略启用 > 物料级 > 不判）—— 唯一的取值口径。
    pub fn effective_min(&self) -> EffectiveMin {
        dto::resolve_min_qty(self.policy.as_ref(), self.material_safety_stock)
    }

    /// **判定**：结存 < 生效最低水位 → `Some(LowWater)`；不判水位 / 未跌破 → `None`。
    ///
    /// 边界：`qty == min` **不算**低（「低于」是严格小于 —— 正好等于水位是达标）。
    /// 负库存照实报（`-3 < 50` 成立），不做 `GREATEST(qty, 0)`：账实不符时更要提醒。
    pub fn judge(&self) -> Option<LowWater> {
        let min = self.effective_min();
        let threshold = min.qty?;
        if self.qty < threshold {
            Some(LowWater {
                min,
                gap: threshold - self.qty,
            })
        } else {
            None
        }
    }

    /// 数量 + 单位的人话（例：`12 千克`，没单位时只有数字）。
    pub fn qty_text(&self, value: Decimal) -> String {
        let number = reminder::decimal_text(&value.to_string());
        match self.unit_name.as_deref().filter(|unit| !unit.trim().is_empty()) {
            Some(unit) => format!("{number} {unit}"),
            None => number,
        }
    }
}

/// 水位来源的人话（回显「为什么按 50 报警」）。
pub fn source_label(source: MinSource) -> &'static str {
    match source {
        MinSource::Policy => "该物料在该仓库的水位设置（策略行）",
        MinSource::Material => "物料级安全库存（该格没有启用的水位设置，回退）",
        MinSource::Unset => "不判水位",
    }
}

/// 待办标题：`库存低于最低水位：{编码} {名称} @{仓库}`，超长截断（`biz_task.title` 上限 200 字）。
pub fn task_title(cell: &WaterCell) -> String {
    let raw = format!(
        "库存低于最低水位：{} {} @{}",
        cell.material_code, cell.material_name, cell.warehouse_name
    );
    task_dto::truncate_chars(&raw, task_dto::MAX_TITLE_CHARS)
}

/// 待办描述：四条事实（结存 / 水位 / 缺口 / 来源）+ 该做什么 + 口径说明。
///
/// 三件必需品逐条对上（与业务提醒同一验收口径）：**是什么**（物料 + 仓库）/
/// **数字**（结存 vs 水位）/ **该做什么**（补货或调整水位）。
pub fn task_description(cell: &WaterCell, low: &LowWater) -> String {
    format!(
        "物料 {code} {name} 在仓库「{warehouse}」的账面结存 {qty}，\
         低于最低水位 {min}，缺口 {gap}。\n\
         水位来源：{source}。\n\
         请安排补货（下采购单 / 调拨）或复核这一格的最低水位是否该调整，避免断料停线。\n\
         \n\
         结存口径：biz_inventory.qty 的已过账结存（不含在途调拨、不含未过账收货、不扣锁定量）。\n\
         本待办由系统按水位自动生成：库存回升到最低水位以上时会自动关闭；\
         要停止这一格的提醒，请把水位设为 0 或删除该格的设置。",
        code = cell.material_code,
        name = cell.material_name,
        warehouse = cell.warehouse_name,
        qty = cell.qty_text(cell.qty),
        min = cell.qty_text(low.min.qty.unwrap_or_default()),
        gap = cell.qty_text(low.gap),
        source = source_label(low.min.source),
    )
}

/// 优先级：结存 `<= 0` → `P1`（断料）；否则 `P2`。
pub fn priority_of(cell: &WaterCell) -> &'static str {
    if cell.qty <= Decimal::ZERO {
        PRIORITY_SHORTAGE
    } else {
        PRIORITY_LOW
    }
}

// ==================== 自动收口的判定（纯逻辑） ====================

/// 一条未完成的水位待办，连同它那一格的**现状**（可能已经什么都不剩了）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCandidate {
    /// 待办 id（`biz_task.id`）。
    pub task_id: Uuid,
    /// 待办挂的库存行 id（`biz_task.doc_id`）。
    pub inventory_id: Uuid,
    /// 该格现状（库存行不存在 / 物料查不到时为 `None`）。
    pub cell: Option<WaterCell>,
    /// 物料状态（`None` = 物料不存在或已软删；`1` = 启用）。
    pub material_status: Option<i16>,
    /// 仓库类型（`None` = 仓库不存在或已软删）。
    pub warehouse_type: Option<String>,
}

/// 这条待办**该不该继续挂着**：`None` = 仍然有效；`Some(原因)` = 该自动关闭。
///
/// 判定顺序是「从最根本的开始」：库存行没了 > 物料停了 > 仓库没了/在途仓 > 结存回升。
/// 顺序会影响文案（先命中先返回），但不影响结论（任一条件成立都该关）。
pub fn recovery_reason(candidate: &OpenCandidate) -> Option<&'static str> {
    let Some(cell) = candidate.cell.as_ref() else {
        return Some("库存行已不存在（物料 × 仓库 的账已清）");
    };
    if candidate.material_status != Some(1) {
        return Some("物料已停用或删除");
    }
    match candidate.warehouse_type.as_deref() {
        None => return Some("仓库已删除"),
        Some("IN_TRANSIT") => return Some("在途仓不参与水位检查"),
        Some(_) => {}
    }
    if cell.judge().is_none() {
        return Some("结存已回升到最低水位以上（或该格已不再判水位）");
    }
    None
}

// ==================== 数据访问：扫描 ====================

/// 扫描候选格（`biz_inventory` ⨝ 物料 ⨝ 仓库 ⨝ 策略行），按「最缺的在前」排序。
///
/// # 口径（逐条都有出处，改之前先读）
///
/// | 条件 | 为什么 |
/// |:--|:--|
/// | `m.deleted_at IS NULL AND m.status = 1` | 停用 / 删除的物料不提醒（不生产了，提醒没人处理）—— 与 `reminder_stock` 一致 |
/// | `w.warehouse_type IS DISTINCT FROM 'IN_TRANSIT'` | 在途仓是虚拟记账仓，不是可补货的实体仓（用 `IS DISTINCT FROM` 而不是 `<>`：仓库软删时 `w.*` 全 NULL，`<>` 会把行静默丢掉） |
/// | `p.deleted_at IS NULL`（在 JOIN 里） | 策略行的唯一键只看未删行；软删行若被 join 进来会覆盖物料级回退 |
/// | 不筛 `qty` | 「低于水位」要拿真实结存判，负数更要报；阈值判定交给 [`WaterCell::judge`] |
///
/// # 为什么不在 SQL 里判「低于水位」
///
/// 三级回退（策略启用 > 物料级 > 不判；停用回退；策略行 `min = 0` 阻断回退）的唯一实现在
/// [`dto::resolve_min_qty`]。SQL 只负责把**两处事实**（策略行的三列 + 物料的 `safety_stock`）
/// 取出来，判定在 Rust 里调那个函数 —— 一旦 SQL 里也写一份 `COALESCE(NULLIF(...))`，
/// 「设置页显示 100、待办按 50 报警」就再没有任何地方会报错。
pub async fn scan_cells(material_id: Option<Uuid>, warehouse_id: Option<Uuid>) -> DbResult<Vec<WaterCell>> {
    // SCOPE-EXEMPT: 见模块头「数据权限」——仓库作业台账无 dept / 归属人列，后台检查无请求者。
    let mut sql = String::from(
        "SELECT i.id AS inventory_id, i.material_id AS material_id, \
                m.material_code AS material_code, m.material_name AS material_name, \
                i.warehouse_id AS warehouse_id, \
                COALESCE(w.warehouse_name, '未知仓库') AS warehouse_name, \
                u.unit_name AS unit_name, \
                i.qty::text AS qty, COALESCE(m.safety_stock, 0)::text AS safety_stock, \
                p.min_qty::text AS policy_min, p.max_qty::text AS policy_max, \
                p.status::text AS policy_status \
           FROM biz_inventory i \
           JOIN biz_material m ON m.id = i.material_id AND m.deleted_at IS NULL AND m.status = 1 \
           LEFT JOIN biz_unit u ON u.id = m.unit_id \
           LEFT JOIN biz_warehouse w ON w.id = i.warehouse_id AND w.deleted_at IS NULL \
           LEFT JOIN biz_material_stock_policy p \
                  ON p.material_id = i.material_id AND p.warehouse_id = i.warehouse_id \
                 AND p.deleted_at IS NULL \
          WHERE w.warehouse_type IS DISTINCT FROM 'IN_TRANSIT'",
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(id) = material_id {
        sql.push_str(" AND i.material_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(id) = warehouse_id {
        sql.push_str(" AND i.warehouse_id = ?::uuid");
        params.push(Value::from(&id));
    }
    // 全序（物料编码 → 仓库 → 库存行）：翻轮不换行、不重复；最缺的在前（缺口 = 结存 - 水位
    // 由小到大 —— 不判水位的格缺口算不出来，但它们本来也不该出现在前面，故用结存兜底排序）。
    sql.push_str(" ORDER BY i.qty ASC, m.material_code ASC, i.warehouse_id ASC, i.id ASC LIMIT ?");
    params.push(Value::I64(SCAN_LIMIT));

    let rows = db().query_sql(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        out.push(WaterCell {
            inventory_id: row.get_typed("inventory_id")?,
            material_id: row.get_typed("material_id")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            warehouse_name: row.get_typed("warehouse_name")?,
            unit_name: row.get_typed("unit_name")?,
            qty: parse_decimal(row.get_typed::<String>("qty")?, "qty")?,
            material_safety_stock: parse_decimal(row.get_typed::<String>("safety_stock")?, "safety_stock")?,
            policy: levels_of(
                row.get_typed::<Option<String>>("policy_min")?,
                row.get_typed::<Option<String>>("policy_max")?,
                row.get_typed::<Option<String>>("policy_status")?,
            )?,
        });
    }
    Ok(out)
}

/// 构造策略行三列（`LEFT JOIN` 未命中时三列都是 NULL → `None`）。
fn levels_of(min: Option<String>, max: Option<String>, status: Option<String>) -> DbResult<Option<PolicyLevels>> {
    let (Some(min), Some(status)) = (min, status) else {
        return Ok(None);
    };
    let min_qty = parse_decimal(min, "policy.min_qty")?;
    let max_qty = match max.filter(|text| !text.trim().is_empty()) {
        Some(text) => Some(parse_decimal(text, "policy.max_qty")?),
        None => None,
    };
    let status: i16 = status
        .trim()
        .parse()
        .map_err(|error| DbError::InvalidValue(format!("policy.status 不是合法 smallint：{error}")))?;
    Ok(Some(PolicyLevels::new(min_qty, max_qty, status)))
}

/// `NUMERIC::text` → `Decimal`（驱动不吃 `Decimal` 直接绑/取，本工程一律走文本中转）。
fn parse_decimal(raw: String, field: &str) -> DbResult<Decimal> {
    Decimal::from_str(raw.trim())
        .map_err(|error| DbError::InvalidValue(format!("{field} 不是合法定点数（{raw}）：{error}")))
}

// ==================== 数据访问：未完成待办 ====================

/// 读该范围下**所有未完成**的水位待办，连同那一格的现状（自动收口的输入）。
///
/// SQL 全部用 `LEFT JOIN`：库存行被物理删、物料被软删、仓库被删时**行还在**（`NULL` 列），
/// 这正是「该自动关闭」的证据 —— 用 `JOIN` 会让这些行静默消失，待办就永远挂着。
pub async fn open_candidates(material_id: Option<Uuid>, warehouse_id: Option<Uuid>) -> DbResult<Vec<OpenCandidate>> {
    let mut sql = format!(
        "SELECT t.id AS task_id, t.doc_id AS inventory_id, \
                i.material_id AS material_id, i.warehouse_id AS warehouse_id, \
                m.material_code AS material_code, m.material_name AS material_name, \
                m.status::text AS material_status, m.deleted_at AS material_deleted_at, \
                COALESCE(w.warehouse_name, '未知仓库') AS warehouse_name, \
                w.warehouse_type AS warehouse_type, w.deleted_at AS warehouse_deleted_at, \
                u.unit_name AS unit_name, i.qty::text AS qty, \
                COALESCE(m.safety_stock, 0)::text AS safety_stock, \
                p.min_qty::text AS policy_min, p.max_qty::text AS policy_max, \
                p.status::text AS policy_status \
           FROM biz_task t \
           LEFT JOIN biz_inventory i ON i.id = t.doc_id \
           LEFT JOIN biz_material m ON m.id = i.material_id \
           LEFT JOIN biz_unit u ON u.id = m.unit_id \
           LEFT JOIN biz_warehouse w ON w.id = i.warehouse_id \
           LEFT JOIN biz_material_stock_policy p \
                  ON p.material_id = i.material_id AND p.warehouse_id = i.warehouse_id \
                 AND p.deleted_at IS NULL \
          WHERE t.doc_type = '{DOC_TYPE}' AND t.doc_id IS NOT NULL \
            AND t.deleted_at IS NULL AND t.status IN ('TODO', 'DOING')"
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(id) = material_id {
        sql.push_str(" AND i.material_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(id) = warehouse_id {
        sql.push_str(" AND i.warehouse_id = ?::uuid");
        params.push(Value::from(&id));
    }
    sql.push_str(" ORDER BY t.id ASC");

    let rows = db().query_sql(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let cell = match row.get_typed::<Option<String>>("qty")? {
            None => None,
            Some(qty) => {
                let deleted = row.get_typed::<Option<DateTime<Utc>>>("material_deleted_at")?;
                let code: Option<String> = row.get_typed("material_code")?;
                match (code, deleted) {
                    (Some(material_code), None) => Some(WaterCell {
                        inventory_id: row.get_typed("inventory_id")?,
                        material_id: row.get_typed("material_id")?,
                        material_code,
                        material_name: row.get_typed::<Option<String>>("material_name")?.unwrap_or_default(),
                        warehouse_id: row.get_typed("warehouse_id")?,
                        warehouse_name: row.get_typed("warehouse_name")?,
                        unit_name: row.get_typed("unit_name")?,
                        qty: parse_decimal(qty, "qty")?,
                        material_safety_stock: parse_decimal(row.get_typed::<String>("safety_stock")?, "safety_stock")?,
                        policy: levels_of(
                            row.get_typed::<Option<String>>("policy_min")?,
                            row.get_typed::<Option<String>>("policy_max")?,
                            row.get_typed::<Option<String>>("policy_status")?,
                        )?,
                    }),
                    // 物料软删：`material_code` 还是 JOIN 出来的（没过滤 deleted_at），
                    // 但 `deleted_at` 非空 → 交给 recovery_reason 判「物料已删除」
                    (Some(material_code), Some(_)) => Some(WaterCell {
                        inventory_id: row.get_typed("inventory_id")?,
                        material_id: row.get_typed("material_id")?,
                        material_code,
                        material_name: row.get_typed::<Option<String>>("material_name")?.unwrap_or_default(),
                        warehouse_id: row.get_typed("warehouse_id")?,
                        warehouse_name: row.get_typed("warehouse_name")?,
                        unit_name: row.get_typed("unit_name")?,
                        qty: parse_decimal(qty, "qty")?,
                        material_safety_stock: Decimal::ZERO,
                        policy: None,
                    }),
                    (None, _) => None,
                }
            }
        };
        let status = match row.get_typed::<Option<String>>("material_status")? {
            None => None,
            Some(raw) => Some(raw.trim().parse::<i16>().map_err(|error| {
                DbError::InvalidValue(format!("material.status 不是合法 smallint（{raw}）：{error}"))
            })?),
        };
        let warehouse_deleted = row.get_typed::<Option<DateTime<Utc>>>("warehouse_deleted_at")?;
        let warehouse_type = if warehouse_deleted.is_some() {
            None
        } else {
            row.get_typed::<Option<String>>("warehouse_type")?
        };
        out.push(OpenCandidate {
            task_id: row.get_typed("task_id")?,
            inventory_id: row.get_typed("inventory_id")?,
            cell,
            material_status: status,
            warehouse_type,
        });
    }
    Ok(out)
}

/// 每格「上一次提醒」的时间（用于重提醒间隔）。
///
/// 排除被**系统自动收口**的行（[`AUTO_CLOSE_PREFIX`]）：库存回升过再跌破是新事件，
/// 不被间隔抑制；被人取消 / 完成的才抑制。
pub async fn last_raise_map(
    material_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
) -> DbResult<HashMap<Uuid, DateTime<Utc>>> {
    let mut sql = format!(
        "SELECT t.doc_id AS inventory_id, max(t.created_at) AS last_at \
           FROM biz_task t \
          WHERE t.doc_type = '{DOC_TYPE}' AND t.doc_id IS NOT NULL AND t.deleted_at IS NULL \
            AND NOT (t.status = 'CANCELLED' \
                     AND COALESCE(t.cancel_reason, '') LIKE '{AUTO_CLOSE_PREFIX}%')"
    );
    let mut params: Vec<Value> = Vec::new();
    if material_id.is_some() || warehouse_id.is_some() {
        sql.push_str(" AND t.doc_id IN (SELECT i.id FROM biz_inventory i WHERE TRUE");
        if let Some(id) = material_id {
            sql.push_str(" AND i.material_id = ?::uuid");
            params.push(Value::from(&id));
        }
        if let Some(id) = warehouse_id {
            sql.push_str(" AND i.warehouse_id = ?::uuid");
            params.push(Value::from(&id));
        }
        sql.push(')');
    }
    sql.push_str(" GROUP BY t.doc_id");

    let rows = db().query_sql(sql, params).await?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        out.insert(
            row.get_typed::<Uuid>("inventory_id")?,
            row.get_typed::<DateTime<Utc>>("last_at")?,
        );
    }
    Ok(out)
}

/// 批量关闭待办（一条 `UPDATE … WHERE id IN (…)`：单语句原子，不需要事务）。
///
/// `status IN ('TODO','DOING')` 是防呆：并发下若有人刚好把这单完成/取消，不会把它改成
/// 「已取消」覆盖掉人的操作。
pub async fn close_todos(task_ids: &[Uuid], reason: &str, operator: Option<Uuid>) -> DbResult<u64> {
    if task_ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?::uuid"; task_ids.len()].join(", ");
    let mut params: Vec<Value> = Vec::with_capacity(task_ids.len() + 2);
    params.push(Value::Text(reason.to_string()));
    params.push(Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()));
    for id in task_ids {
        params.push(Value::from(id));
    }
    db().execute_with_params(
        &format!(
            "UPDATE biz_task SET status = 'CANCELLED', cancel_reason = ?, \
                    updated_at = now(), update_by = NULLIF(?::text, '')::uuid \
              WHERE id IN ({placeholders}) AND status IN ('TODO', 'DOING') AND deleted_at IS NULL"
        ),
        params,
    )
    .await
}

// ==================== 数据访问：建待办 ====================

/// 建一条水位待办（事务内取号 + 落库，返回 `task_no`）。
///
/// # 为什么复用 `task::model::create_task`
///
/// 任务编号 `TSK-YYYYMM-NNNNNN` 的取号（`pg_advisory_xact_lock` + 月内 MAX+1）是任务模块的
/// **唯一实现**。在这里另写一份 `MAX+1` 会在并发下算出同一个号（撞 `uk_biz_task_task_no`），
/// 或者在不同模块里排出两套号段。
///
/// # 异常情况
///
/// - 数据库异常（含 `uk_biz_task_open_inventory` 冲突，调用方翻成「已有待办」）→ [`DbError`]。
pub async fn raise_todo(cell: &WaterCell, low: &LowWater, owner: Uuid, operator: Option<Uuid>) -> DbResult<String> {
    let today = task_model::today_cst();
    let month = task_model::month_key(today);
    let dept_id = task_model::user_dept(owner).await?;
    let new = task_model::NewTask {
        id: Uuid::now_v7(),
        title: task_title(cell),
        description: Some(task_description(cell, low)),
        priority: priority_of(cell).to_string(),
        need_review: false,
        owner_id: owner,
        // 系统生成 = 负责人自己「领到」的任务：creator = owner，于是 source 恒为 SELF、
        // assigner 为空（这不是「别人派给你的」，任务列表按 SELF 分组才不会出现假分派）
        creator_id: owner,
        assigner_id: None,
        dept_id,
        parent_id: None,
        project_id: None,
        doc_type: Some(DOC_TYPE.to_string()),
        doc_id: Some(cell.inventory_id),
        start_date: None,
        due_date: Some(task_model::default_due_date(today)),
        estimate_hours: None,
        source: task_dto::SOURCE_SELF.to_string(),
        add_creator_as_collaborator: false,
    };
    // 审计列 `create_by` 记真实触发人（后台任务没有触发人 → 退回负责人）
    let audit_operator = operator.unwrap_or(owner);
    db().transaction(|tx| async move { task_model::create_task(&tx, &new, audit_operator, &month).await })
        .await
}

// ==================== 负责人 ====================

/// 解析本轮的待办负责人。
///
/// ```text
/// 仓管主管角色下有启用用户  → 确定性第一位（uuid 升序），source = ROLE
/// 角色下没人 + 手动触发     → 触发人自己，source = OPERATOR
/// 角色下没人 + 后台任务     → 无人可派，source = NONE（上层跳过并 WARN，不静默丢弃）
/// ```
///
/// 多个仓管主管时取**一位**而不是每人一条：同一格的缺料是**一件事**，不是 N 件事；
/// 每人一条会把待办列表撑成 N 倍，还会出现「A 补了货、B 的那条还挂着」。
/// 确定性排序保证多实例/多轮之间选到同一个人（否则每轮换人，谁都不觉得自己该管）。
pub async fn resolve_owner(operator: Option<Uuid>) -> DbResult<(Option<Uuid>, &'static str)> {
    let mut keepers = users_of_roles(&WAREHOUSE_ROLES).await?;
    keepers.sort_unstable();
    if let Some(owner) = keepers.first() {
        return Ok((Some(*owner), "ROLE"));
    }
    match operator {
        Some(id) => Ok((Some(id), "OPERATOR")),
        None => Ok((None, "NONE")),
    }
}

/// 重提醒间隔（天）：`sys_config` 读不到 / 非正数 → 缺省值。
async fn realert_days() -> i64 {
    SysConfigModel::read_i64(CONFIG_REALERT_DAYS)
        .await
        .filter(|days| *days > 0)
        .unwrap_or(DEFAULT_REALERT_DAYS)
}

/// 距上次提醒是否还在抑制窗口内（`last` 为空 = 从没提醒过 → 不在窗口内）。
pub fn within_realert_window(last: Option<DateTime<Utc>>, now: DateTime<Utc>, days: i64) -> bool {
    match last {
        None => false,
        Some(at) => now.signed_duration_since(at) < Duration::days(days),
    }
}

// ==================== 编排：跑一轮检查 ====================

/// 跑一轮水位检查（后台任务与手动接口**共用这一个函数**）。
///
/// # 顺序（先收后发，不能换）
///
/// ```text
/// ① 收口：未完成待办中「已经不该挂」的 → CANCELLED（先把噪音清掉）
/// ② 扫描：候选格（biz_inventory ⨝ 物料 ⨝ 仓库 ⨝ 策略行）
/// ③ 判定：结存 < 生效最低水位 → 低水位
/// ④ 发单：没有未完成待办、不在重提醒窗口、有负责人 → 建待办
/// ```
///
/// 先收后发的理由：回升的格子在 ② 里本来就不会被判低，顺序不影响结论，但先收能让
/// ①-④ 之间的中间态是「更干净」的那一半（异常中断时最多漏发，不会多发）。
///
/// # 参数
///
/// - `material_id` / `warehouse_id`：范围（`None` = 全库）；
/// - `dry_run`：只算不写（不建也不关，响应里给「本来会做什么」）；
/// - `operator`：真实触发人（后台任务为 `None`）。它只影响两件事：
///   待办的 `create_by` 审计列、以及角色下没人时的负责人兜底。
///
/// # 异常情况
///
/// - 读库失败 → [`DbError`]（整轮失败，下一轮重试）；
/// - 单格建单/关闭失败 → 计入 `failed` 并打 WARN，**不影响其余格子**（一轮不许因一格崩掉）。
pub async fn run_check(
    material_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    dry_run: bool,
    operator: Option<Uuid>,
) -> DbResult<WaterCheckRes> {
    let now = Utc::now();
    let days = realert_days().await;
    let (owner, owner_source) = resolve_owner(operator).await?;

    let mut res = WaterCheckRes {
        dry_run,
        owner: owner.map(|id| id.to_string()),
        owner_source: owner_source.to_string(),
        ..WaterCheckRes::default()
    };

    // ① 收口
    let candidates = open_candidates(material_id, warehouse_id).await?;
    let mut open_ids: HashSet<Uuid> = HashSet::with_capacity(candidates.len());
    // 按「为什么该关」分组：一条 UPDATE 一个原因，待办的 cancel_reason 才能说清是哪一条
    // （把五种原因糊成一句「库存已回到安全区间」会让「物料停用」的待办显示成「库存回升」）
    let mut close_batches: HashMap<&'static str, Vec<Uuid>> = HashMap::new();
    for candidate in &candidates {
        open_ids.insert(candidate.inventory_id);
        if let Some(reason) = recovery_reason(candidate) {
            if dry_run {
                res.closed += 1;
            } else {
                close_batches.entry(reason).or_default().push(candidate.task_id);
            }
        }
    }
    for (reason, task_ids) in close_batches {
        match close_todos(&task_ids, &auto_close_reason(reason), operator).await {
            Ok(closed) => res.closed += closed,
            Err(error) => {
                res.failed += 1;
                tracing::warn!(
                    error = %error,
                    count = task_ids.len(),
                    reason,
                    "水位检查：自动收口失败（下一轮重试）"
                );
            }
        }
    }

    // ②③④ 扫描 → 判定 → 发单
    let cells = scan_cells(material_id, warehouse_id).await?;
    res.scanned = usize_to_u64(cells.len());
    let last_map = last_raise_map(material_id, warehouse_id).await?;

    for cell in &cells {
        let Some(low) = cell.judge() else {
            continue;
        };
        res.low += 1;
        let mut item = item_of(cell, &low, "DRY_RUN", None);
        if dry_run {
            push_item(&mut res, item);
            continue;
        }
        if open_ids.contains(&cell.inventory_id) {
            res.skipped_open += 1;
            item.action = "EXISTS".to_string();
            push_item(&mut res, item);
            continue;
        }
        let last = last_map.get(&cell.inventory_id).copied();
        if within_realert_window(last, now, days) {
            res.skipped_recent += 1;
            item.action = "RECENT".to_string();
            push_item(&mut res, item);
            continue;
        }
        let Some(owner) = owner else {
            res.skipped_no_owner += 1;
            item.action = "NO_OWNER".to_string();
            push_item(&mut res, item);
            continue;
        };
        match raise_todo(cell, &low, owner, operator).await {
            Ok(task_no) => {
                res.raised += 1;
                open_ids.insert(cell.inventory_id);
                item.action = "RAISED".to_string();
                item.task_no = Some(task_no);
                push_item(&mut res, item);
            }
            Err(error) => {
                // 并发下另一实例先建了同格待办：唯一索引把它吃掉，这不算失败
                if is_open_todo_conflict(&error) {
                    res.skipped_open += 1;
                    item.action = "EXISTS".to_string();
                    push_item(&mut res, item);
                } else {
                    res.failed += 1;
                    tracing::warn!(
                        inventory_id = %cell.inventory_id,
                        material_code = %cell.material_code,
                        error = %error,
                        "水位检查：生成待办失败（下一轮重试）"
                    );
                }
            }
        }
    }

    if res.skipped_no_owner > 0 {
        tracing::warn!(
            roles = ?WAREHOUSE_ROLES,
            count = res.skipped_no_owner,
            "水位检查：仓管主管角色下没有启用用户，这些格子未能生成待办（请补角色成员，或用带操作人的手动触发）"
        );
    }
    res.message = summary(&res);
    Ok(res)
}

/// 后台任务入口（`run_handler` 与内置 cron 共用）：全库扫，无操作人。
pub async fn run_background() -> DbResult<String> {
    let res = run_check(None, None, false, None).await?;
    tracing::info!(message = %res.message, "水位检查完成");
    Ok(res.message)
}

fn push_item(res: &mut WaterCheckRes, item: WaterCheckItem) {
    if res.items.len() < MAX_ITEMS {
        res.items.push(item);
    }
}

fn item_of(cell: &WaterCell, low: &LowWater, action: &str, task_no: Option<String>) -> WaterCheckItem {
    WaterCheckItem {
        inventory_id: cell.inventory_id.to_string(),
        material_id: cell.material_id.to_string(),
        material_code: cell.material_code.clone(),
        material_name: cell.material_name.clone(),
        warehouse_id: cell.warehouse_id.to_string(),
        warehouse_name: cell.warehouse_name.clone(),
        qty: reminder::decimal_text(&cell.qty.to_string()),
        min_qty: reminder::decimal_text(&low.min.qty.unwrap_or(Decimal::ZERO).to_string()),
        gap_qty: reminder::decimal_text(&low.gap.to_string()),
        min_source: low.min.source.as_str().to_string(),
        action: action.to_string(),
        task_no,
    }
}

/// 唯一索引 `uk_biz_task_open_inventory` 的冲突 = 该格已有未完成待办（并发下的正常结果）。
fn is_open_todo_conflict(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } => constraint
            .as_deref()
            .map(|name| name.contains("uk_biz_task_open_inventory"))
            .unwrap_or(false),
        _ => false,
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// 一行可读结论（写日志 / `sys_job_log` 摘要 / 接口 `message` 共用，字段名给人 grep）。
pub fn summary(res: &WaterCheckRes) -> String {
    format!(
        "{TASK_NAME}: scanned={} low={} raised={} skipped_open={} skipped_recent={} \
         skipped_no_owner={} closed={} failed={}{}",
        res.scanned,
        res.low,
        res.raised,
        res.skipped_open,
        res.skipped_recent,
        res.skipped_no_owner,
        res.closed,
        res.failed,
        if res.dry_run { " (dry_run)" } else { "" }
    )
}

// ==================== 注册 ====================

/// 在调度器上注册水位检查任务（由 `task::Scheduler::register_builtin` 调用）。
///
/// 内置注册是**兜底**：`sys_job` 里的那一行（V0429 种子，`status = 1`）会被调度器动态注册，
/// 同名的后注册者覆盖前注册者（`SchedulerPlugin` 的 `insert` 是 HashMap 覆盖语义），
/// 所以一天只跑一次、并且每轮都会落 `sys_job_log`。任务行被删 / 被停用时内置的这份照跑。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        CRON,
        "系统任务：安全库存水位检查（低于 → 生成待办）",
        || {
            tokio::spawn(async move {
                match run_background().await {
                    Ok(summary) => tracing::info!(summary = %summary, "水位检查任务完成"),
                    Err(error) => tracing::error!(error = %error, "水位检查任务失败"),
                }
            })
        },
    );
    tracing::info!(invoke_target = TASK_NAME, cron = CRON, "水位检查任务已注册");
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点字面量")
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 一格（默认：策略启用、最低 50、结存 12、物料级 100）。
    fn cell(qty: &str, policy: Option<(String, Option<String>, i16)>, safety: &str) -> WaterCell {
        WaterCell {
            inventory_id: uuid(1),
            material_id: uuid(2),
            material_code: "M214-1".to_string(),
            material_name: "水位物料".to_string(),
            warehouse_id: uuid(3),
            warehouse_name: "原料仓".to_string(),
            unit_name: Some("千克".to_string()),
            qty: dec(qty),
            material_safety_stock: dec(safety),
            policy: policy.map(|(min, max, status)| PolicyLevels::new(dec(&min), max.map(|text| dec(&text)), status)),
        }
    }

    /// ① 判定边界：`qty < 阈值` 才报；`==` 达标；`>` 不报；负库存照实报。
    #[test]
    fn judge_is_strictly_less_than_threshold() {
        let enabled = Some(("50".to_string(), None, 1));
        assert!(cell("50", enabled.clone(), "0").judge().is_none(), "等于水位 = 达标");
        assert!(cell("50.0001", enabled.clone(), "0").judge().is_none());
        let low = cell("49.9999", enabled.clone(), "0").judge().expect("低于水位");
        assert_eq!(low.gap, dec("0.0001"));
        let negative = cell("-3", enabled, "0").judge().expect("负库存必须报");
        assert_eq!(negative.gap, dec("53"));
        println!("[M2-14] 判定：< 才报 / == 达标 / 负库存照实报 ✓");
    }

    /// ② 三级回退原封不动来自 M2-13：策略启用 > 物料级 > 不判；停用/`min=0` 的行为照抄。
    #[test]
    fn thresholds_come_from_the_single_resolver() {
        // 策略启用 → 用 50（物料级 100 被覆盖）
        let enabled = cell("60", Some(("50".to_string(), None, 1)), "100");
        assert_eq!(enabled.effective_min().qty, Some(dec("50")));
        assert_eq!(enabled.effective_min().source, MinSource::Policy);
        assert!(enabled.judge().is_none(), "60 >= 50 不报");

        // 策略停用 → 回退物料级 100 → 60 < 100 要报
        let disabled = cell("60", Some(("50".to_string(), None, 0)), "100");
        assert_eq!(disabled.effective_min().source, MinSource::Material);

        // 策略行 min = 0 → 明示不预警，**不**回退物料级
        let blocked = cell("1", Some(("0".to_string(), None, 1)), "100");
        assert_eq!(blocked.effective_min().qty, None);
        assert!(blocked.judge().is_none(), "min=0 阻断回退");

        // 都没有 → 不判
        assert!(cell("0", None, "0").judge().is_none());
        println!("[M2-14] 阈值只走 resolve_min_qty（启用/停用/min=0/不判 四态）✓");
    }

    /// ③ 标题与描述带齐三件必需品：是什么 / 数字 / 该做什么；来源写人话。
    #[test]
    fn task_text_carries_facts_and_action() {
        let target = cell("12", Some(("50".to_string(), None, 1)), "0");
        let low = target.judge().expect("低于水位");
        let title = task_title(&target);
        assert!(title.contains("M214-1"), "{title}");
        assert!(title.contains("原料仓"), "{title}");
        for fact in ["M214-1", "原料仓", "12", "50", "38", "补货", "水位来源", "策略行"] {
            assert!(
                task_description(&target, &low).contains(fact),
                "描述缺 `{fact}`：{}",
                task_description(&target, &low)
            );
        }
        assert!(task_title(&target).chars().count() <= task_dto::MAX_TITLE_CHARS);
        println!("[M2-14] 待办文案：编码/仓库/结存/水位/缺口/来源/动作 全在 ✓");
    }

    /// ④ 超长物料名不会突破 `biz_task.title` 的 200 字上限。
    #[test]
    fn long_title_is_truncated() {
        let mut long = cell("1", Some(("50".to_string(), None, 1)), "0");
        long.material_name = "水".repeat(400);
        let title = task_title(&long);
        assert_eq!(title.chars().count(), task_dto::MAX_TITLE_CHARS, "{title}");
        println!("[M2-14] 标题按 200 字截断 ✓");
    }

    /// ⑤ 优先级：断料（<= 0）P1，普通低水位 P2。
    #[test]
    fn priority_splits_shortage_from_low() {
        let policy = Some(("50".to_string(), None, 1));
        assert_eq!(priority_of(&cell("0", policy.clone(), "0")), PRIORITY_SHORTAGE);
        assert_eq!(priority_of(&cell("-2", policy.clone(), "0")), PRIORITY_SHORTAGE);
        assert_eq!(priority_of(&cell("1", policy, "0")), PRIORITY_LOW);
        println!("[M2-14] 优先级：<=0 断料 P1 / 其余 P2 ✓");
    }

    /// ⑥ 自动收口的四个理由（顺序与文案都要对），以及「仍低于水位时不关」。
    #[test]
    fn recovery_reason_covers_every_stale_case() {
        let policy = Some(("50".to_string(), None, 1));
        let low_cell = cell("10", policy.clone(), "0");

        // 仍然低 → 不关
        assert_eq!(
            recovery_reason(&OpenCandidate {
                task_id: uuid(9),
                inventory_id: uuid(1),
                cell: Some(low_cell.clone()),
                material_status: Some(1),
                warehouse_type: Some("NORMAL".to_string()),
            }),
            None
        );
        // 库存行没了
        assert!(
            recovery_reason(&OpenCandidate {
                task_id: uuid(9),
                inventory_id: uuid(1),
                cell: None,
                material_status: Some(1),
                warehouse_type: Some("NORMAL".to_string()),
            })
            .expect("该关")
            .contains("库存行已不存在")
        );
        // 物料停用 / 删除
        for status in [None, Some(0)] {
            assert_eq!(
                recovery_reason(&OpenCandidate {
                    task_id: uuid(9),
                    inventory_id: uuid(1),
                    cell: Some(low_cell.clone()),
                    material_status: status,
                    warehouse_type: Some("NORMAL".to_string()),
                }),
                Some("物料已停用或删除")
            );
        }
        // 仓库没了 / 在途仓
        assert_eq!(
            recovery_reason(&OpenCandidate {
                task_id: uuid(9),
                inventory_id: uuid(1),
                cell: Some(low_cell.clone()),
                material_status: Some(1),
                warehouse_type: None,
            }),
            Some("仓库已删除")
        );
        assert_eq!(
            recovery_reason(&OpenCandidate {
                task_id: uuid(9),
                inventory_id: uuid(1),
                cell: Some(low_cell),
                material_status: Some(1),
                warehouse_type: Some("IN_TRANSIT".to_string()),
            }),
            Some("在途仓不参与水位检查")
        );
        // 结存回升
        assert!(
            recovery_reason(&OpenCandidate {
                task_id: uuid(9),
                inventory_id: uuid(1),
                cell: Some(cell("999", policy, "0")),
                material_status: Some(1),
                warehouse_type: Some("NORMAL".to_string()),
            })
            .expect("该关")
            .contains("回升")
        );
        println!("[M2-14] 自动收口：行没了/物料停/仓库没了/在途仓/回升 五种都判 ✓");
    }

    /// ⑦ 重提醒窗口：从没提醒过不抑制；窗口内抑制；窗口外不抑制。
    #[test]
    fn realert_window_is_inclusive_of_now() {
        let now = Utc::now();
        assert!(!within_realert_window(None, now, 3));
        assert!(within_realert_window(Some(now - Duration::hours(1)), now, 3));
        assert!(!within_realert_window(
            Some(now - Duration::days(3) - Duration::seconds(1)),
            now,
            3
        ));
        println!("[M2-14] 重提醒窗口：无历史不抑制 / 窗口内抑制 / 过期放行 ✓");
    }

    /// ⑧ 自动收口的 reason 前缀必须与抑制查询里的 LIKE 逐字一致（两处写死同一个前缀）。
    #[test]
    fn auto_close_prefix_matches_suppression_filter() {
        let reason = auto_close_reason("库存已回到安全区间");
        assert!(reason.starts_with(AUTO_CLOSE_PREFIX), "{reason}");
        assert_eq!(AUTO_CLOSE_PREFIX, "AUTO_RECOVERED");
        assert!(reason.contains("自动关闭"), "{reason}");
        println!("[M2-14] AUTO_RECOVERED 前缀：写库文案与抑制查询同一常量 ✓");
    }

    /// ⑨ 常量钉死：调度名 / cron 可被调度器解析 / 单据类型与提醒台账口径一致。
    #[test]
    fn schedule_and_doc_type_are_pinned() {
        assert_eq!(TASK_NAME, "erp_stock_water_check");
        assert_eq!(CRON, "0 7 * * * *");
        assert!(CRON.split_whitespace().count() == 6, "6 段 cron（第 1 段是秒）");
        assert_eq!(DOC_TYPE, "INVENTORY", "与 biz_reminder_log.doc_type 逐字一致");
        assert_eq!(SCAN_LIMIT, 500);
        println!("[M2-14] 调度名 / cron(6 段) / doc_type=INVENTORY 钉死 ✓");
    }
}
