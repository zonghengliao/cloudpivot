//! ④ 低库存提醒（当前库存 < 安全库存）。
//!
//! # 它解决什么
//!
//! `biz_material.safety_stock` 是 V0137 就有的列，**填了但从不参与任何判定** ——
//! ARCH-3 断链 DC-12 点名的「摆设字段」。本文件是它唯一的消费者：
//!
//! ```text
//! biz_inventory（物料 × 仓库，唯一行）
//!   JOIN biz_material（启用、未删除）
//!   WHERE material.safety_stock > 0
//!     AND inventory.qty < material.safety_stock
//! ```
//!
//! # 「当前库存」的口径（P2-2 ② 定稿，改之前先读完这一段）
//!
//! | 问题 | 口径 | 为什么 |
//! |------|------|--------|
//! | 用哪一列 | **`biz_inventory.qty`**（物料×仓库的已过账结存） | 这是账上真实躺在仓里的货；补货决策看的是「手上有多少」 |
//! | 含在途吗 | **不含** | 在途只存在于 `biz_stock_transfer.transit_warehouse_id`（文档字段），调拨过账（`DONE`）才把数量从调出仓搬到调入仓 —— 在途期间 `biz_inventory` 两边的账都还没变，本来就不该算进任何一仓的结存 |
//! | 含未过账收货吗 | **不含** | `biz_stock_in` 草稿/待过账的单还没进仓库（过账才写 `biz_inventory`），把「在路上/还没验收」的货算进来会让缺料被掩盖 |
//! | 含锁定量 `locked_qty` 吗 | **不含（也不用减）** | `locked_qty` 本版本恒为 0（无占用流程，见 V0215 的列注释）；提醒口径是「仓里有多少」，不是「还能卖多少」—— 后者是出库校验的事 |
//! | 用库位账 `biz_inventory_location` 吗 | **不用** | 库位账是仓库账的细分，且 64% 的行还挂在「未分配库位」桶上；按库位判定会同一物料重复提醒 N 次 |
//! | 负库存怎么算 | **照实数（可以为负）** | 允许负库存的仓出现 `qty = -3` 说明账实已经不符，这种更要提醒，不能 `GREATEST(qty, 0)` 抹平 |
//! | 按物料合计吗 | **不，按物料×仓库** | 见下 |
//!
//! 口径写进 `docs/ERP-摆设字段口径.md`，列注释写在 V0215 —— 三处必须一致，改一处要同步另两处。
//!
//! # 为什么按「物料 × 仓库」而不是按物料合计
//!
//! 补货是按仓库补的（原料仓缺货，成品仓再多也调不过来）；`biz_inventory` 的唯一键
//! 就是 `(material_id, warehouse_id)`，一行天然对应一个可执行的补货动作。
//! 台账 `doc_id` 记的就是这一行的 id（该表无 `deleted_at`，行永不复用）。
//!
//! # 为什么 `safety_stock = 0` 不提醒（= 前端「留空」）
//!
//! 0 的语义是「没设安全库存」（V0137 默认值），把它当阈值会让所有零库存物料天天报 ——
//! 那不是提醒，那是噪音。只有**明确填了正数**的物料才参与判定。
//!
//! 补两条容易踩的：
//!
//! * 列是 `NUMERIC NOT NULL DEFAULT 0`，**没有 NULL 状态** —— 前端「留空」落库就是 `0`，
//!   所以「空值不预警」在本仓的准确说法是「0 不预警」（`dto` 的 `Option<Decimal>` 只是
//!   「本次请求没带这个字段 = 不改」，不是「清空成 NULL」）。
//! * SQL 仍写成 `COALESCE(m.safety_stock, 0) > 0`：将来真把列放开成可空（历史库或二开改表），
//!   NULL 会不满足 `> 0` 而**静默不提醒**，行为与 0 一致 —— 但 `COALESCE` 让它读起来是
//!   「缺省 0」而不是「依赖 SQL 三值逻辑的巧合」，评审时不用再推一遍。
//!
//! # 停用/在途仓不参与
//!
//! * `m.status = 1`：停用物料不提醒（不生产了，提醒也没人处理）。
//! * `w.warehouse_type <> 'IN_TRANSIT'`：在途仓是**记账用的虚拟仓**，不是可补货的实体仓；
//!   万一有人往里收货（或二开造了行），提醒「在途仓低于安全库存」只会制造噪音。
//!
//! # 提醒频率
//!
//! 状态型提醒（补货前一直成立），重提醒间隔由
//! `sys_config.erp.reminder.low_stock.realert_days`（缺省 3 天）控制。
//!
//! # 收件人
//!
//! 仓管主管（`WAREHOUSE_MANAGER` 角色下的启用用户）。仓库主数据里的 `manager` 只是一列
//! 姓名文本（V0137），不是用户引用，无法作为收件人 —— 这是本仓数据模型的既有限制。

use std::str::FromStr;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::reminder::{self, ReminderMessage, SCAN_BATCH};

/// 收件人角色：仓管主管。
pub const WAREHOUSE_ROLES: [&str; 1] = ["WAREHOUSE_MANAGER"];

/// 台账档位（低库存没有「几档」，只有一个持续状态）。
pub const STAGE: &str = "LOW";

/// 一行待提醒的低库存（物料 × 仓库）。
#[derive(Debug, Clone)]
pub struct LowStockCandidate {
    /// `biz_inventory.id`（台账 `doc_id`）。
    pub id: Uuid,
    /// 物料编码（台账 `doc_no`）。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库名称。
    pub warehouse_name: String,
    /// 当前库存（NUMERIC 文本）。
    pub qty: String,
    /// 安全库存（NUMERIC 文本）。
    pub safety_stock: String,
}

impl LowStockCandidate {
    /// 缺口 = 安全库存 − 当前库存（解析失败返回 `None`，文案里写「—」而不是 panic）。
    pub fn gap(&self) -> Option<Decimal> {
        let safety = Decimal::from_str(self.safety_stock.trim()).ok()?;
        let qty = Decimal::from_str(self.qty.trim()).ok()?;
        Some(safety - qty)
    }
}

/// 组装一条提醒消息（收件人 = 仓管主管）。
pub fn message_of(candidate: &LowStockCandidate, keepers: &[Uuid]) -> ReminderMessage {
    let recipients = reminder::merge_recipients(keepers.iter().copied().map(Some));
    let current_qty = reminder::decimal_text(&candidate.qty);
    let safety_stock = reminder::decimal_text(&candidate.safety_stock);
    let gap_qty = candidate
        .gap()
        .map(|gap| reminder::decimal_text(&gap.to_string()))
        .unwrap_or_else(|| "—".to_string());
    let title = format!(
        "库存低于安全库存：{} {}",
        candidate.material_code, candidate.material_name
    );
    let content = format!(
        "物料 {} {} 在仓库「{}」的当前库存 {}，低于安全库存 {}，缺口 {}。\n\
         请安排补货（下采购单）或复核安全库存值是否该调整，避免断料停线。",
        candidate.material_code,
        candidate.material_name,
        candidate.warehouse_name,
        current_qty,
        safety_stock,
        gap_qty
    );
    let vars = serde_json::json!({
        "materialCode": candidate.material_code,
        "materialName": candidate.material_name,
        "warehouseName": candidate.warehouse_name,
        "currentQty": current_qty,
        "safetyStock": safety_stock,
        "gapQty": gap_qty,
        "action": "安排补货",
    });
    ReminderMessage {
        doc_id: candidate.id,
        doc_no: candidate.material_code.clone(),
        stage: STAGE.to_string(),
        title,
        content,
        vars,
        recipients,
        recipient_rule: "仓管主管（WAREHOUSE_MANAGER）".to_string(),
    }
}

/// 扫描「低于安全库存」的物料 × 仓库并组装成待发消息（只读查询，不发通知）。
pub async fn scan(now: DateTime<Utc>) -> DbResult<Vec<ReminderMessage>> {
    let _ = now; // 判定只用当前库存与安全库存，不需要业务日（保留签名与其它三类一致）
    let candidates = scan_candidates().await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let keepers = reminder::users_of_roles(&WAREHOUSE_ROLES).await?;
    if keepers.is_empty() {
        tracing::warn!(
            roles = ?WAREHOUSE_ROLES,
            "低库存提醒：仓管主管角色下没有启用用户，本轮无收件人（请补角色成员）"
        );
    }
    Ok(candidates
        .iter()
        .map(|candidate| message_of(candidate, &keepers))
        .collect())
}

/// 扫描候选行（`safety_stock > 0` 且 `qty < safety_stock`）。
///
/// 口径逐条见文件头注释。三处 `WHERE` 条件都不能少：
/// ① `COALESCE(m.safety_stock,0) > 0` —— 0（= 前端留空）不预警；
/// ② `i.qty < m.safety_stock` —— 负库存也要报（不 `GREATEST` 抹平）；
/// ③ `w.warehouse_type IS DISTINCT FROM 'IN_TRANSIT'` —— 在途仓是虚拟记账仓，不参与补货提醒。
///    用 `IS DISTINCT FROM` 而不是 `<>`：仓库被删时 `w.*` 全是 NULL，`<>` 会把这类行
///    **静默丢掉**（三值逻辑），而我们要的是「照常提醒，仓库名显示未知仓库」。
pub async fn scan_candidates() -> DbResult<Vec<LowStockCandidate>> {
    // SCOPE-EXEMPT: 库存是仓库作业台账（biz_inventory 无 dept_id / 归属人列），
    // 可见性由 business:erp:inventory:list 控制；提醒收件人由仓管主管角色决定。
    let rows = db()
        .query_sql(
            "SELECT i.id AS id, m.material_code AS material_code, m.material_name AS material_name, \
                    COALESCE(w.warehouse_name, '未知仓库') AS warehouse_name, \
                    i.qty::text AS qty, m.safety_stock::text AS safety_stock \
               FROM biz_inventory i \
               JOIN biz_material m ON m.id = i.material_id \
                                  AND m.deleted_at IS NULL AND m.status = 1 \
               LEFT JOIN biz_warehouse w ON w.id = i.warehouse_id AND w.deleted_at IS NULL \
              WHERE COALESCE(m.safety_stock, 0) > 0 \
                AND i.qty < COALESCE(m.safety_stock, 0) \
                AND w.warehouse_type IS DISTINCT FROM 'IN_TRANSIT' \
              ORDER BY (i.qty - m.safety_stock) ASC, m.material_code ASC \
              LIMIT ?"
                .to_string(),
            vec![Value::I64(SCAN_BATCH)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        out.push(LowStockCandidate {
            id: row.get_typed("id")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            warehouse_name: row.get_typed("warehouse_name")?,
            qty: row.get_typed("qty")?,
            safety_stock: row.get_typed("safety_stock")?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(qty: &str, safety: &str) -> LowStockCandidate {
        LowStockCandidate {
            id: Uuid::parse_str("0199ffff-0000-7000-8000-000000000001").expect("固定 uuid"),
            material_code: "M-0001".to_string(),
            material_name: "冷轧钢板".to_string(),
            warehouse_name: "原料仓".to_string(),
            qty: qty.to_string(),
            safety_stock: safety.to_string(),
        }
    }

    /// ① 文案三件套：物料编码 + 数量事实 + 该做什么；缺口算得对。
    #[test]
    fn message_carries_stock_facts() {
        let keepers = vec![Uuid::parse_str("0199ffff-0000-7000-8000-000000000011").unwrap()];
        let message = message_of(&candidate("12.0000", "50.0000"), &keepers);
        assert_eq!(message.stage, "LOW");
        assert!(message.title.contains("M-0001"), "{}", message.title);
        for fact in ["M-0001", "冷轧钢板", "原料仓", "12", "50", "38", "补货"] {
            assert!(
                message.content.contains(fact),
                "正文缺 `{fact}`：{}",
                message.content
            );
        }
        assert_eq!(message.vars["gapQty"], "38");
        assert_eq!(message.vars["currentQty"], "12");
        assert_eq!(message.recipients.len(), 1);
    }

    /// ② 小数库存：缺口保留有效小数（`12.5` vs `50` → `37.5`）。
    #[test]
    fn gap_keeps_decimals() {
        let message = message_of(&candidate("12.5000", "50.0000"), &[]);
        assert_eq!(message.vars["gapQty"], "37.5");
    }

    /// ③ 数量解析失败时缺口写「—」而不是 panic（脏数据不能让整轮扫描崩）。
    #[test]
    fn bad_numeric_does_not_panic() {
        let message = message_of(&candidate("abc", "50"), &[]);
        assert_eq!(message.vars["gapQty"], "—");
        assert!(message.content.contains('—'), "{}", message.content);
    }

    /// ④ 台账 `doc_id` 是库存行 id、`doc_no` 是物料编码（同物料不同仓库各算一份）。
    #[test]
    fn doc_id_is_inventory_row() {
        let message = message_of(&candidate("1", "2"), &[]);
        assert_eq!(message.doc_id, candidate("1", "2").id);
        assert_eq!(message.doc_no, "M-0001");
    }

    /// ⑤ 没有仓管主管 → 空收件人（上层 WARN + 跳过，不静默丢弃）。
    #[test]
    fn no_keeper_marks_empty() {
        assert!(message_of(&candidate("1", "2"), &[]).recipients.is_empty());
    }
}
