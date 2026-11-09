//! 内控（职责分离）：**单据制单人 / 审批发起人不得审批自己那一票**。
//!
//! # 为什么要有这个模块（P0-FIX 的根因）
//!
//! 内控闸此前只做「点的时候 403」（`{purchase,stockin,payable}_bridge::guard_self_approval`）。
//! 待办却照常生成 ⇒ 当事人**看得见、点不动**；在**会签（`approval_mode=ALL`）**节点上更糟：
//! 那一票永远批不出来，节点永远等人齐 ⇒ **流程死锁**。
//!
//! 实测（生产 PO-20261031-000002）：`n_dept` 审批人 = `DEPT_LEADER`（0 人绑定）→
//! `FALLBACK_ADMIN` 回退成 4 个超管 = {admin(发起人), 老六(制单人), 老六测试号, Henry.Liao}，
//! Henry.Liao 已批、admin 与老六被 403 挡住、老六测试号没人点 ⇒ 单据停在「审批中」不动。
//!
//! # 现在怎么修（三层）
//!
//! 1. **生成待办时就排除**（本模块）：「制单人」由业务桥提供（`internal_control_creator`），
//!    「发起人」由实例自带。落在审批人列表里的人**不进 `PENDING`**，改落一条
//!    `SKIPPED` 待办留痕（`completed_at` + `comment` 写明原因）。
//! 2. **会签不再被阻塞**：`Tally.total` 只数 `PENDING/APPROVED/REJECTED`（`model::tally_of_node`），
//!    `SKIPPED` 天然不计入应到人数 ⇒ 剩下的人批完即通过。
//! 3. **403 兜底保留**：`guard_self_approval` 一个字没动（防绕过：转办 / 加签 / 老数据仍会被拦）。
//!
//! # 边界（刻意不做的事）
//!
//! - **只对「受内控约束的业务」生效**：`PO_APPROVE` / `STOCK_IN_POST` / `STOCK_IN_CANCEL` /
//!   `PAYABLE_PAY` / `PAYABLE_CANCEL` / `VOUCHER_REVERSE`。其它 `biz_type`（任务验收、调拨……）
//!   `governed=false`，行为与改动前逐字一致。
//! - **不删任何待办行**：被排除的人一定有 `SKIPPED` 行留痕（审计要能回答「谁没批、为什么」）。
//! - **不碰节点配置**：`self_approval` 仍是 `ALLOW`。内控语义是「这一票没人能点 ⇒ 不算一票」，
//!   与框架的「发起人自审策略」是两件事（后者只有 `SKIP`/`FORBID`，前者还包括**制单人**）。
//!
//! # ⚠️ 配置红线（全部被排除 = 发起即失败）
//!
//! 若某节点**全部**审批人都是被排除的人，本模块给不出任何一个能签字的候选人 ⇒
//! 引擎在落待办之前就以 `422 + FLOW_NO_ASSIGNEE` **整单发起失败**（报文点名节点 +
//! 每个人为什么被排除 + 两条出路：换审批岗位 / 换人提交）。
//!
//! 以前这里会按「空审批人」自动通过（`SKIPPED` + `auto_passed_nodes + 1`），
//! 结果是「配了流程也无人复核就生效」—— 种子 `po_approve` 的
//! `empty_assignee = FALLBACK_ADMIN` 在只有一个超管的库里兜底出来的正好是**发起人本人**
//! （实测见 `docs/evidence/20261103-po-approval-no-approver.md`）。
//! 「真的没有候选」（角色里 0 人 + `empty_assignee=SKIP`）仍按配置自动通过，两者语义不同：
//! 前者是**审批岗位配错了**，后者是**客户明确配置了免审**。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::{payable_bridge, purchase_bridge, stockin_bridge};
use crate::business::flow::assignee::Candidate;
use crate::business::flow::model as flow_model;

/// 「制单人不可自审」的留痕原因（写进 `biz_flow_task.comment`，审批详情直接展示）。
pub const CREATOR_REASON: &str = "内控：单据制单人不可审批自己制单的单据（职责分离），已跳过并留痕";
/// 「发起人不可自审」的留痕原因。
pub const INITIATOR_REASON: &str = "内控：审批单发起人不可审批自己发起的审批（职责分离），已跳过并留痕";

/// 一条内控排除（谁 + 为什么）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclusion {
    /// 被排除的用户。
    pub user_id: Uuid,
    /// 留痕原因（落 `biz_flow_task.comment`）。
    pub reason: String,
}

/// 一次审批人解析要套用的内控规则。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Control {
    /// 本业务是否受内控约束。`false` ⇒ [`partition`] 原样返回，历史行为不变。
    pub governed: bool,
    /// 被排除的人（去重后，制单人优先）。
    pub exclusions: Vec<Exclusion>,
}

impl Control {
    /// 这个人是否被排除；是则返回留痕原因。
    pub fn excluded_reason(&self, user_id: Uuid) -> Option<&str> {
        if !self.governed {
            return None;
        }
        self.exclusions
            .iter()
            .find(|item| item.user_id == user_id)
            .map(|item| item.reason.as_str())
    }

    /// 是否一个人都没排除（详情页 / 日志用）。
    pub fn is_empty(&self) -> bool {
        self.exclusions.is_empty()
    }
}

/// 把一个候选人塞进排除表（**同一个用户只留第一条原因**：制单人排在发起人前面，
/// 「既是制单人又是发起人」时留痕读起来更准）。
fn push_exclusion(exclusions: &mut Vec<Exclusion>, user_id: Option<Uuid>, reason: &str) {
    let Some(user_id) = user_id else {
        return;
    };
    if exclusions.iter().any(|item| item.user_id == user_id) {
        return;
    }
    exclusions.push(Exclusion {
        user_id,
        reason: reason.to_string(),
    });
}

/// 按实例算出内控规则（**只读，可在事务内调**）。
///
/// 「制单人」由业务桥查（各单据的 `create_by`），「发起人」直接取实例字段。
/// 与本业务无关的 `biz_type` → `governed=false`（零影响）。
pub async fn of(instance: &flow_model::InstanceRow) -> DbResult<Control> {
    of_biz(&instance.biz_type, instance.biz_id, instance.initiator_id).await
}

/// 同上，但入参是**裸字段**（P3-04）。
///
/// `assignee.rs` 的委托替换只拿得到 [`crate::business::flow::assignee::InstanceCtx`]
/// （`biz_type` / `biz_id` / 发起人），手里没有实例行；而委托后的「受托人会不会撞内控」
/// 必须与引擎落待办前的那次判定**逐字同源** —— 否则会出现「解析期说没撞、落库前被剔光」
/// 或反过来的分裂口径。所以判据收敛到本函数，两条调用路径共用。
pub async fn of_biz(biz_type: &str, biz_id: Option<Uuid>, initiator_id: Uuid) -> DbResult<Control> {
    // ERP-B2：审批侧的排除同样受「制单与审批分离」开关管辖（默认开 = 改动前行为）。
    // 关掉它 = 小团队允许本人自审：既不排除（不落 SKIPPED），403 闸也同时放行 ——
    // 两处读同一个开关，不存在「待办被 SKIP 了但闸说可以批」的自相矛盾状态。
    if !crate::business::erp::separation::enabled(crate::business::erp::separation::Duty::Approve).await {
        return Ok(Control::default());
    }
    let mut exclusions: Vec<Exclusion> = Vec::new();
    let biz_type = biz_type.trim().to_ascii_uppercase();
    let governed = if biz_type == purchase_bridge::BIZ_TYPE {
        let creator = purchase_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::requisition_bridge::BIZ_TYPE {
        // P2-02：采购申请的 `biz_id` 是**申请单 id**（提单人不能批自己提的需求）
        let creator = crate::business::erp::requisition_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::purchase_return_bridge::BIZ_TYPE {
        // P2-03：采购退货的 `biz_id` 是**退货单 id**（制单人不能批自己的退货）
        let creator = crate::business::erp::purchase_return_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == stockin_bridge::POST_BIZ_TYPE || biz_type == stockin_bridge::BIZ_TYPE {
        let creator = stockin_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == payable_bridge::BIZ_TYPE {
        let creator = payable_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == payable_bridge::CANCEL_BIZ_TYPE {
        // P-FLOW-3B：应付作废审批的 `biz_id` 是**应付单 id**（不是付款记录 id）
        let creator = payable_bridge::internal_control_creator_of_payable(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::receipt_refund_bridge::BIZ_TYPE {
        // SO-14 遗留 D10：收款退款审批的 `biz_id` 是**退款单 id**（制单人不能批自己的退款）
        let creator = crate::business::erp::receipt_refund_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::voucher_bridge::BIZ_TYPE {
        // P-FLOW-3B：凭证红冲审批的 `biz_id` 是**凭证 id**
        let creator = crate::business::erp::voucher_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::contract_bridge::BIZ_TYPE {
        // ARCH-3 R-P1-1：合同审批的 `biz_id` 是**合同 id**（制单人不能批自己拟的合同）
        let creator = crate::business::erp::contract_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::stocktake_bridge::BIZ_TYPE {
        // ARCH-3 DC-14：盘点过账审批的 `biz_id` 是**盘点单 id**（盘的人不能批自己盘的账）
        let creator = crate::business::erp::stocktake_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::budget_bridge::BIZ_TYPE {
        // FIN-1：预算审批的 `biz_id` 是**预算版本 id**（编预算的人不能批自己编的预算）
        let creator = crate::business::erp::budget_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::expense_bridge::BIZ_TYPE {
        // FIN-3：费用报销的 `biz_id` 是**报销单 id**（报销人不能批自己的单）
        let creator = crate::business::erp::expense_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::asset_bridge::BIZ_TYPE {
        // FIN-2：资产处置审批的 `biz_id` 是**资产卡片 id**（建卡的人不能批自己这张卡的处置）
        let creator = crate::business::erp::asset_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::sales_approval_bridge::BIZ_TYPE {
        // SO-06：销售订单审批的 `biz_id` 是**销售订单 id**（制单人不能批自己提交的单）
        let creator = crate::business::erp::sales_approval_bridge::internal_control_creator(biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else {
        false
    };
    if governed {
        push_exclusion(&mut exclusions, Some(initiator_id), INITIATOR_REASON);
    }
    Ok(Control { governed, exclusions })
}

/// 把审批人列表按内控切成「该落 `PENDING` 的」与「该落 `SKIPPED` 留痕的」（**纯函数**）。
///
/// 返回 `(保留的审批人, [(被排除的人, 原因)])`；顺序保持入参顺序（与 `finalize` 的排序一致，
/// 待办 / 留痕行的展示顺序因此可复现）。
pub fn partition(approvers: Vec<Candidate>, control: &Control) -> (Vec<Candidate>, Vec<(Candidate, String)>) {
    if !control.governed || control.exclusions.is_empty() {
        return (approvers, Vec::new());
    }
    let mut kept: Vec<Candidate> = Vec::with_capacity(approvers.len());
    let mut excluded: Vec<(Candidate, String)> = Vec::new();
    for candidate in approvers {
        match control.excluded_reason(candidate.user_id) {
            Some(reason) => excluded.push((candidate, reason.to_string())),
            None => kept.push(candidate),
        }
    }
    (kept, excluded)
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::flow::graph::AssigneeKind;

    /// 造一个候选人。
    fn cand(id: Uuid, name: &str) -> Candidate {
        Candidate::new(id, name, AssigneeKind::User)
    }

    /// 受内控约束、排除 `users` 的规则。
    fn control_of(users: &[(Uuid, &str)]) -> Control {
        Control {
            governed: true,
            exclusions: users
                .iter()
                .map(|(id, reason)| Exclusion {
                    user_id: *id,
                    reason: (*reason).to_string(),
                })
                .collect(),
        }
    }

    /// ① 制单人在审批人列表里 → 该人落 SKIPPED（不是 PENDING），其余照常。
    #[test]
    fn creator_in_approvers_is_skipped_not_pending() {
        let creator = Uuid::from_u128(11);
        let other = Uuid::from_u128(12);
        let control = control_of(&[(creator, CREATOR_REASON)]);
        let (kept, excluded) = partition(vec![cand(creator, "制单人"), cand(other, "审批人")], &control);
        assert_eq!(kept.len(), 1, "非内控人员必须留在 PENDING");
        assert_eq!(kept[0].user_id, other);
        assert_eq!(excluded.len(), 1, "制单人必须被排除");
        assert_eq!(excluded[0].0.user_id, creator);
        assert!(
            excluded[0].1.contains("制单人"),
            "留痕原因要能让人看懂（实际：{}）",
            excluded[0].1
        );
    }

    /// ② 发起人在审批人列表里 → 同样落 SKIPPED。
    #[test]
    fn initiator_in_approvers_is_skipped() {
        let initiator = Uuid::from_u128(21);
        let control = control_of(&[(initiator, INITIATOR_REASON)]);
        let (kept, excluded) = partition(vec![cand(initiator, "发起人")], &control);
        assert!(kept.is_empty());
        assert_eq!(excluded.len(), 1);
        assert!(excluded[0].1.contains("发起人"));
    }

    /// ③ 制单人 + 发起人都不在审批人列表里 → 全部 PENDING（不许误伤）。
    #[test]
    fn nobody_excluded_keeps_everyone_pending() {
        let control = control_of(&[
            (Uuid::from_u128(31), CREATOR_REASON),
            (Uuid::from_u128(32), INITIATOR_REASON),
        ]);
        let approvers = vec![
            cand(Uuid::from_u128(41), "A"),
            cand(Uuid::from_u128(42), "B"),
            cand(Uuid::from_u128(43), "C"),
        ];
        let (kept, excluded) = partition(approvers, &control);
        assert_eq!(kept.len(), 3);
        assert!(excluded.is_empty());
    }

    /// ⑩ 换掉审批人角色（业务岗位人员都不是制单人 / 发起人）后无人被排除 ——
    /// 这正是「配置修复」要达到的效果：`governed` 仍为真，只是一个人都匹配不上。
    #[test]
    fn business_roles_leave_nobody_excluded() {
        let control = control_of(&[
            (Uuid::from_u128(51), CREATOR_REASON),
            (Uuid::from_u128(52), INITIATOR_REASON),
        ]);
        // 采购经理 / 总经理岗位上的人（都不是这张单的制单人 / 发起人）
        let approvers = vec![
            cand(Uuid::from_u128(61), "采购经理"),
            cand(Uuid::from_u128(62), "总经理"),
        ];
        let (kept, excluded) = partition(approvers, &control);
        assert_eq!(kept.len(), 2, "业务岗位人员必须全部是 PENDING");
        assert!(excluded.is_empty(), "配置改对之后不该有人被排除");
        assert!(control.governed, "内控仍生效（只是没命中）");
    }

    /// 不受内控约束的业务（任务验收 / 调拨…）：`governed=false` 一律不干预。
    #[test]
    fn ungovened_business_is_untouched() {
        let anyone = Uuid::from_u128(71);
        let control = Control::default();
        assert!(!control.governed);
        assert_eq!(control.excluded_reason(anyone), None);
        let (kept, excluded) = partition(vec![cand(anyone, "任何人")], &control);
        assert_eq!(kept.len(), 1);
        assert!(excluded.is_empty());
    }

    /// 「既是制单人又是发起人」只留一条留痕（原因取先出现的制单人）。
    #[test]
    fn same_person_keeps_one_reason() {
        let same = Uuid::from_u128(81);
        let mut exclusions = Vec::new();
        push_exclusion(&mut exclusions, Some(same), CREATOR_REASON);
        push_exclusion(&mut exclusions, Some(same), INITIATOR_REASON);
        push_exclusion(&mut exclusions, None, INITIATOR_REASON);
        assert_eq!(exclusions.len(), 1, "同一个人不许出现两条排除");
        assert_eq!(exclusions[0].reason, CREATOR_REASON);
    }

    /// 留痕原因必须写清「为什么」—— 审批详情页直接展示这段文案。
    #[test]
    fn reasons_are_human_readable() {
        for (reason, keyword) in [(CREATOR_REASON, "制单人"), (INITIATOR_REASON, "发起人")] {
            assert!(reason.starts_with("内控"), "原因要以「内控」开头：{reason}");
            assert!(reason.contains(keyword), "原因要点名角色：{reason}");
            assert!(reason.contains("跳过"), "原因要说明结果：{reason}");
        }
    }
}

// ==================== 真库用例（引擎全链路） ====================
//
// 纯逻辑用例证明不了「会签不再被阻塞」：那要经过 `engine::start` → 快照 →
// `assignee::resolve` → 内控 partition → 落 `biz_flow_task` → `vote` → `judge` 的整条链。
// 所以这批用例连真库（`P0FIX_DB_URL`），未设置时**整组 SKIP**（`cargo test --lib` 在没配库的
// 机器上仍然全绿），设置了就真跑。
//
// 跑法（**本组必须独占进程**，原因见下）：
// ```bash
// P0FIX_DB_URL='postgres://p0fixrole:***@127.0.0.1:5432/cloudpivot_p0fix' \
//   cargo test --lib business::flow::internal_control -- --test-threads=1 --nocapture
// ```
//
// # ⚠️ 为什么必须独占进程（`ensure_db` 的冲突守卫）
//
// `set_db()` 是进程级 `OnceLock`（`backend/vendor/nik-web/src/resources.rs`）——一个测试进程
// 只能装**一个**全局连接，先到先得。历史上这里写的是「`try_db().is_some()` 就直接 `return true`」，
// 于是同进程里更早的用例（`ERP_DB_URL` / `TASK_DB_URL` / `SYS_DB_URL` 那一批）一装好连接，
// 本组用例就**静默跑在别人的库上**：`P0FIX_DB_URL` 形同虚设，下面那条「库名必须含 p0fix」的
// 硬断言与全部用例一起被跳过，可 `test result` 仍然是绿的（假绿）。
//
// 实测反例（`docs/evidence/20261104-setdb-reuse-guard/`）：`cargo test --lib 'business::flow::'`
// 一起跑时 7 条 `p0fix_*` 流程定义落在了 ERP 库，`P0FIX_DB_URL` 指向的库一行没写；把
// `P0FIX_DB_URL` 指到一个**根本不存在的库**也照样全绿 —— 证明那个进程压根没去连。
//
// 现在：`try_db()` 已存在时，先问它「你连的到底是哪个库」（`SELECT current_database()`），
// 与 `P0FIX_DB_URL` 里的库名**逐字比对**：
// * 同一个库 → 复用（合法，例如同进程里更早的用例用的就是同一个 P0FIX 库）；
// * 不是同一个库 → **panic 并指路**（不是 SKIP、不是继续跑）。
//
// 选 `panic` 而不是「WARN + SKIP」的理由：本组的价值全在「跑在 P0FIX 库上」这件事上，
// SKIP 会让 `test result` 依旧是 `ok`，正是本卡要灭掉的那类假绿；而冲突只发生在操作者
// **显式设了 `P0FIX_DB_URL`** 的场合（没设 → 仍是老老实实 SKIP），此时失败才是诚实响应。
// 若你确实要跑整段 flow，请把本组排除掉：`-- --skip business::flow::internal_control::db_tests`
// （`scripts/verify/run_lib_tests_by_module.sh` 的 `flow` 模块就是这么做的，本组归 `flow_p0fix` 模块）。
//
// 夹具约定：所有行都带 `p0fix` 前缀（`order_no` / `username` / `role_code`），用例开头先按前缀清场，
// 所以「同一枚举库连跑几轮」不会互相污染；清理只按**本卡前缀**，不碰别人的夹具。
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::business::flow::engine;
    use crate::business::flow::graph::{
        SNAPSHOT_SCHEMA_VERSION, Snapshot, SnapshotAssignee, SnapshotEdge, SnapshotNode,
    };
    use serde_json::json;

    /// 真库用例**串行**（共用一个库 + 固定前缀夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 夹具 id 的固定基址（与本卡其它夹具 / 别的模块都不重叠）。
    const SEED_BASE: u128 = 0x00f0_f100_0000_0000_0000_0000_0000_0000u128;

    /// 初始化全局数据库；`false` = 没配 `P0FIX_DB_URL`，用例直接跳过。
    ///
    /// `try_db()` 已被同进程更早的用例初始化时**不再无条件复用**：先问它连的是哪个库，
    /// 与 `P0FIX_DB_URL` 逐字比对，不同就 panic 并指路（见上方模块注释的「为什么必须独占进程」）。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("P0FIX_DB_URL") else {
                    println!("SKIP: 未设置 P0FIX_DB_URL（内控 / 会签的真库用例需要真库）");
                    return false;
                };
                assert!(
                    url.contains("p0fix") || url.contains("/cloudpivot_test"),
                    "P0FIX_DB_URL 必须指向 cloudpivot_p0fix 或 cloudpivot_test（当前：{url}）——\
                     本套测试会造真实审批实例与待办"
                );
                let wanted = db_name_of(&url);
                if try_db().is_some() {
                    // 进程级 OnceLock 已经装了别人的连接 ⇒ 再也换不了库。过去这里直接 `return true`，
                    // 于是本组用例静默跑在别人的库上。现在只接受「库里也是我要的那一个」。
                    let current = current_db_name().await;
                    if current != wanted {
                        panic!(
                            "\n\
[internal_control::db_tests] 全局 db() 已被同进程更早的用例初始化，且连的不是 P0FIX 库 ——\n\
本组用例拒绝在别人的库上跑（那样 P0FIX_DB_URL 与「库名必须含 p0fix」的断言都不会被执行）。\n\
  · 当前全局连接实际连的库：{current}\n\
  · P0FIX_DB_URL 要求的库    ：{wanted}（{url}）\n\
成因：set_db() 是进程级 OnceLock（先到先得），同进程里更早的用例一旦用 ERP_DB_URL /\n\
TASK_DB_URL / SYS_DB_URL 装好连接，本组就换不了库了。\n\
下一步（任选其一）：\n\
  1) 本组用例独占进程：\n\
     P0FIX_DB_URL='{url}' cargo test --lib business::flow::internal_control -- --test-threads=1\n\
  2) 用分库跑法脚本（推荐，一条命令连库和泄漏校验都做了）：\n\
     MODULES='flow_p0fix' bash scripts/verify/run_lib_tests_by_module.sh\n\
  3) 若你确实要整段跑 flow，把本组排除掉（它属于 flow_p0fix 模块）：\n\
     cargo test --lib 'business::flow::' -- --test-threads=1 --skip business::flow::internal_control::db_tests\n"
                        );
                    }
                    println!("[internal_control] 复用同进程已初始化的全局 db()（库：{current}，与 P0FIX_DB_URL 一致）");
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => {
                            println!("[internal_control] 已连接 {url}");
                            true
                        }
                        Err(error) => panic!("set_db 失败：{error}"),
                    },
                    Err(error) => panic!("连接 {url} 失败：{error}"),
                }
            })
            .await
    }

    /// 从连接串里取库名：`postgres://user:pass@host:port/<db>?params` → `<db>`。
    ///
    /// 只做「够用」的解析 —— 本组要的是「两个连接串指向同一个库吗」，
    /// 因此忽略协议/账号/端口/查询参数，只比路径最后一段。
    fn db_name_of(url: &str) -> String {
        let without_query = url.split(['?', '#']).next().unwrap_or(url);
        without_query.rsplit('/').next().unwrap_or_default().trim().to_string()
    }

    /// 当前全局连接**实际**连的库名（`SELECT current_database()`）。
    ///
    /// 为什么不用「进程里的 `ERP_DB_URL` / `TASK_DB_URL` 是不是同一个串」来判断：
    /// 更早的用例可能压根没设这两个变量（例如走 `SYS_DB_URL`），或者它们与
    /// `P0FIX_DB_URL` 写法不同但指向同一个库。问数据库自己最靠得住。
    async fn current_db_name() -> String {
        match db().scalar::<String>("SELECT current_database()").await {
            Ok(name) => name,
            Err(error) => format!("<查询 current_database() 失败：{error}>"),
        }
    }

    /// 一致性 uuid（同一 `seed` 永远得到同一个 id，重跑时先删后建）。
    fn uid(seed: u128) -> Uuid {
        Uuid::from_u128(SEED_BASE + seed)
    }

    /// 一条审批 / 会签节点的快照节点（审批人由调用方给）。
    fn approve_spec(node_type: &str, assignee_type: &str, assignees: &[(Uuid, &str)]) -> SnapshotNode {
        SnapshotNode {
            key: "n1".to_string(),
            name: "内控审批".to_string(),
            node_type: node_type.to_string(),
            assignee_type: Some(assignee_type.to_string()),
            approval_mode: if node_type == "COUNTERSIGN" {
                Some("ALL".to_string())
            } else {
                None
            },
            approve_ratio: None,
            reject_veto: false,
            join_mode: "AND".to_string(),
            fork_mode: "AND".to_string(),
            form_var_key: None,
            superior_levels: 1,
            empty_assignee: "SKIP".to_string(),
            // 与生产种子同口径：`ALLOW`（自审不拦发起，内控另算）
            self_approval: "ALLOW".to_string(),
            allow_robot: false,
            fallback_user_id: None,
            fallback_role_id: None,
            timeout_hours: None,
            timeout_action: "REMIND".to_string(),
            remind_before_hours: None,
            max_remind_times: 3,
            cc_on_approved: false,
            x: 0,
            y: 0,
            sort_order: 1,
            config: json!({}),
            assignees: assignees
                .iter()
                .enumerate()
                .map(|(index, (id, kind))| SnapshotAssignee {
                    assignee_type: (*kind).to_string(),
                    ref_id: id.to_string(),
                    sort_order: index as i32,
                })
                .collect(),
        }
    }

    /// 一个最简单的基础节点（START / END）。
    fn plain_spec(key: &str, name: &str, node_type: &str, sort_order: i32) -> SnapshotNode {
        SnapshotNode {
            key: key.to_string(),
            name: name.to_string(),
            node_type: node_type.to_string(),
            assignee_type: None,
            approval_mode: None,
            approve_ratio: None,
            reject_veto: false,
            join_mode: "AND".to_string(),
            fork_mode: "AND".to_string(),
            form_var_key: None,
            superior_levels: 1,
            empty_assignee: "SKIP".to_string(),
            self_approval: "SKIP".to_string(),
            allow_robot: false,
            fallback_user_id: None,
            fallback_role_id: None,
            timeout_hours: None,
            timeout_action: "REMIND".to_string(),
            remind_before_hours: None,
            max_remind_times: 3,
            cc_on_approved: false,
            x: 0,
            y: 0,
            sort_order,
            config: json!({}),
            assignees: Vec::new(),
        }
    }

    fn edge(key: &str, from: &str, to: &str) -> SnapshotEdge {
        SnapshotEdge {
            key: key.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            condition: None,
            label: None,
            priority: 0,
            is_default: true,
        }
    }

    /// 一个内控场景：单据（带制单人）+ 流程（START → 审批节点 → END）+ 已发起的实例。
    struct Scenario {
        tag: String,
        def_id: Uuid,
        version_id: Uuid,
        instance_id: Uuid,
        order_id: Uuid,
        initiator: Uuid,
    }

    impl Scenario {
        /// 造场景并**发起实例**（发起被拒 → panic；「发起应当失败」的用例用 [`Self::build`]）。
        async fn start(tag: &str, seed: u128, approvers: &[(Uuid, &str)], role_members: &[Uuid]) -> Self {
            let scenario = Self::build(tag, seed, approvers, role_members).await;
            scenario.run_start().await;
            scenario
        }

        /// 只造场景与流程定义，**不发起**实例。
        async fn build(tag: &str, seed: u128, approvers: &[(Uuid, &str)], role_members: &[Uuid]) -> Self {
            let instance_id = uid(seed * 100 + 1);
            let def_id = uid(seed * 100 + 2);
            let version_id = uid(seed * 100 + 3);
            let order_id = uid(seed * 100 + 4);
            let creator = uid(seed * 100 + 5);
            let initiator = uid(seed * 100 + 6);
            let users = [uid(seed * 100 + 11), uid(seed * 100 + 12), uid(seed * 100 + 13)];
            let role_id = (!role_members.is_empty()).then(|| uid(seed * 100 + 21));
            let suffix = format!("p0fix_{tag}");

            // ---------- 清场（重跑安全） ----------
            Self::purge(&suffix, order_id, instance_id, def_id, version_id, role_id).await;

            // ---------- 单号（`create_by` = 制单人） ----------
            db().execute_with_params(
                "INSERT INTO biz_purchase_order \
                     (id, order_no, supplier_id, order_date, warehouse_id, status, create_by) \
                 VALUES (?::uuid, ?, ?::uuid, current_date, ?::uuid, 'SUBMITTED', ?::uuid)",
                vec![
                    Value::from(&order_id),
                    Value::Text(format!("P0FIX-{tag}")),
                    Value::from(&uid(seed * 100 + 31)),
                    Value::from(&uid(seed * 100 + 32)),
                    Value::from(&creator),
                ],
            )
            .await
            .expect("插采购单");

            // ---------- 用户 ----------
            let all_users: Vec<Uuid> = std::iter::once(creator)
                .chain(std::iter::once(initiator))
                .chain(users.iter().copied())
                .collect();
            for (index, id) in all_users.iter().enumerate() {
                db().execute_with_params(
                    "INSERT INTO sys_user (id, username, password, real_name, status) \
                     VALUES (?::uuid, ?, 'x', ?, 1)",
                    vec![
                        Value::from(id),
                        Value::Text(format!("{suffix}_{index}")),
                        Value::Text(format!("内控夹具{index}")),
                    ],
                )
                .await
                .expect("插用户");
            }

            // ---------- 角色（⑩ 用：业务岗位成员不是制单人 / 发起人） ----------
            if let Some(role_id) = role_id {
                db().execute_with_params(
                    "INSERT INTO sys_role (id, role_code, role_name, status) VALUES (?::uuid, ?, ?, 1)",
                    vec![
                        Value::from(&role_id),
                        Value::Text(format!("P0FIX_{tag}")),
                        Value::Text(format!("内控夹具角色 {tag}")),
                    ],
                )
                .await
                .expect("插角色");
                for member in role_members {
                    db().execute_with_params(
                        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::uuid, ?::uuid)",
                        vec![Value::from(member), Value::from(&role_id)],
                    )
                    .await
                    .expect("插用户角色");
                }
            }

            // ---------- 流程定义 + 版本快照 ----------
            let node = approve_spec(
                "COUNTERSIGN",
                if role_id.is_some() { "ROLE" } else { "USER" },
                approvers,
            );
            let snapshot = Snapshot {
                schema_version: SNAPSHOT_SCHEMA_VERSION,
                nodes: vec![
                    plain_spec("s", "开始", "START", 0),
                    node,
                    plain_spec("e", "结束", "END", 2),
                ],
                edges: vec![edge("e1", "s", "n1"), edge("e2", "n1", "e")],
            };
            let snapshot_text = serde_json::to_string(&snapshot).expect("快照序列化");
            db().execute_with_params(
                "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status, \
                     current_version_id, current_version_no) \
                 VALUES (?::uuid, ?, ?, 'PO_APPROVE', 'PUBLISHED', ?::uuid, 1)",
                vec![
                    Value::from(&def_id),
                    Value::Text(format!("p0fix_{tag}")),
                    Value::Text(format!("P0-FIX {tag}")),
                    Value::from(&version_id),
                ],
            )
            .await
            .expect("插流程定义");
            db().execute_with_params(
                "INSERT INTO biz_flow_def_version \
                     (id, def_id, version_no, version_status, is_current, graph_hash, \
                      graph_snapshot, node_count, edge_count, published_at) \
                 VALUES (?::uuid, ?::uuid, 1, 'PUBLISHED', true, ?, ?::jsonb, ?, ?, now())",
                vec![
                    Value::from(&version_id),
                    Value::from(&def_id),
                    Value::Text("0".repeat(64)),
                    Value::Json(snapshot_text),
                    Value::from(snapshot.nodes.len() as i64),
                    Value::from(snapshot.edges.len() as i64),
                ],
            )
            .await
            .expect("插版本快照");

            Self {
                tag: tag.to_string(),
                def_id,
                version_id,
                instance_id,
                order_id,
                initiator,
            }
        }

        /// 只造 `StartReq`（`run_start` / `try_start` 共用）。
        fn start_req(&self) -> engine::StartReq {
            engine::StartReq {
                instance_id: self.instance_id,
                def_id: self.def_id,
                version_id: self.version_id,
                version_no: 1,
                flow_code: format!("p0fix_{}", self.tag),
                flow_name: "P0-FIX 内控".to_string(),
                biz_type: "PO_APPROVE".to_string(),
                biz_id: Some(self.order_id),
                biz_no: Some(format!("P0FIX-{}", self.tag)),
                title: format!("P0-FIX {}", self.tag),
                initiator_id: self.initiator,
                initiator_name: Some("发起人".to_string()),
                initiator_dept_id: None,
                form_data: json!({}),
            }
        }

        /// `engine::start`（事务内）+ 提交 + flush 副作用。
        async fn run_start(&self) {
            let req = self.start_req();
            let tx = db().begin().await.expect("开启事务");
            let mut out = engine::start(&tx, &req).await.expect("发起实例");
            assert!(out.rejected.is_none(), "发起不应失败：{:?}", out.rejected);
            tx.commit().await.expect("提交事务");
            engine::flush_effects(out.effects.take()).await;
            engine::flush_drafts(out.effects.take_drafts()).await;
        }

        /// 发起实例，**允许失败**：被拒时按业务入口的口径 `rollback`，返回拒绝原因。
        ///
        /// 业务入口（`submit_po_approval` / `submit_post` / `submit_payment`）拿到
        /// `out.rejected` 就是 `Err` + 回滚，所以「发起失败 = 库里不许多出任何行」。
        async fn try_start(&self) -> Option<ApiError> {
            let req = self.start_req();
            let tx = db().begin().await.expect("开启事务");
            let mut out = engine::start(&tx, &req).await.expect("发起实例");
            match out.rejected.take() {
                Some(error) => {
                    tx.rollback().await.expect("回滚事务");
                    Some(error)
                }
                None => {
                    tx.commit().await.expect("提交事务");
                    engine::flush_effects(out.effects.take()).await;
                    engine::flush_drafts(out.effects.take_drafts()).await;
                    None
                }
            }
        }

        /// 走**业务入口的新口径**（[`engine::start_in_tx`]，卡 t_0127714a）。
        ///
        /// 与 [`Self::try_start`] 的区别：这里不再「拿到 `out.rejected` 才自己回滚」，
        /// 而是由引擎把业务拒绝直接翻成 `Err(StartFailure::Rejected)` —— 调用方拿到 `Err`
        /// 就回滚，**没有任何一条代码路径能「先 commit 再判 rejected」**。
        async fn try_start_in_tx(&self) -> Option<ApiError> {
            let req = self.start_req();
            let tx = db().begin().await.expect("开启事务");
            match engine::start_in_tx(&tx, &req).await {
                Ok(mut out) => {
                    tx.commit().await.expect("提交事务");
                    engine::flush_effects(out.effects.take()).await;
                    engine::flush_drafts(out.effects.take_drafts()).await;
                    None
                }
                Err(engine::StartFailure::Rejected(error)) => {
                    tx.rollback().await.expect("回滚事务");
                    Some(error)
                }
                Err(engine::StartFailure::Db(error)) => {
                    let _ = tx.rollback().await;
                    panic!("start_in_tx 不该报数据库错误：{error}");
                }
            }
        }

        /// 实例状态（不存在 → `None`，用来证明「发起失败没留下半截审批单」）。
        async fn instance_status_opt(&self) -> Option<String> {
            let row = db()
                .query_first_sql(
                    "SELECT status FROM biz_flow_instance WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&self.instance_id)],
                )
                .await
                .expect("读实例");
            row.map(|row| row.get_typed::<String>("status").expect("读 status"))
        }

        /// 本实例的全部待办行：`(assignee, status, comment)`。
        async fn tasks(&self) -> Vec<(Uuid, String, String)> {
            let rows = db()
                .find_with_params(
                    "SELECT assignee_id, status, COALESCE(comment,'') AS comment \
                       FROM biz_flow_task WHERE instance_id = ?::uuid",
                    vec![Value::from(&self.instance_id)],
                )
                .await
                .expect("读待办");
            rows.iter()
                .map(|row| {
                    (
                        row.get_typed::<Uuid>("assignee_id").unwrap_or_default(),
                        row.get_typed::<String>("status").unwrap_or_default(),
                        row.get_typed::<String>("comment").unwrap_or_default(),
                    )
                })
                .collect()
        }

        /// 某个人的待办状态（`None` = 没有这一行）。
        async fn status_of(&self, user: Uuid) -> Option<String> {
            self.tasks()
                .await
                .into_iter()
                .find(|(id, _, _)| *id == user)
                .map(|(_, status, _)| status)
        }

        async fn comment_of(&self, user: Uuid) -> String {
            self.tasks()
                .await
                .into_iter()
                .find(|(id, _, _)| *id == user)
                .map(|(_, _, comment)| comment)
                .unwrap_or_default()
        }

        async fn task_id_of(&self, user: Uuid) -> Uuid {
            let row = db()
                .query_first_sql(
                    "SELECT id FROM biz_flow_task WHERE instance_id = ?::uuid \
                      AND assignee_id = ?::uuid LIMIT 1"
                        .to_string(),
                    vec![Value::from(&self.instance_id), Value::from(&user)],
                )
                .await
                .expect("读待办 id")
                .expect("待办必须存在");
            row.get_typed::<Uuid>("id").expect("待办 id")
        }

        async fn instance_status(&self) -> String {
            db().scalar_with_params::<Option<String>>(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&self.instance_id)],
            )
            .await
            .expect("读实例")
            .unwrap_or_default()
        }

        async fn node_status(&self, node_key: &str) -> String {
            db().scalar_with_params::<Option<String>>(
                "SELECT status FROM biz_flow_instance_node \
                  WHERE instance_id = ?::uuid AND node_key = ? AND round = 1 LIMIT 1",
                vec![Value::from(&self.instance_id), Value::Text(node_key.to_string())],
            )
            .await
            .expect("读节点")
            .unwrap_or_default()
        }

        async fn node_approver_count(&self, node_key: &str) -> i32 {
            db().scalar_with_params::<Option<i32>>(
                "SELECT approver_count FROM biz_flow_instance_node \
                  WHERE instance_id = ?::uuid AND node_key = ? AND round = 1 LIMIT 1",
                vec![Value::from(&self.instance_id), Value::Text(node_key.to_string())],
            )
            .await
            .expect("读节点人数")
            .unwrap_or_default()
        }

        /// 以某人身份同意（事务内 `engine::approve` + 提交 + flush）。
        async fn approve(&self, user: Uuid) -> engine::ActionOut {
            let task_id = self.task_id_of(user).await;
            let tx = db().begin().await.expect("开启事务");
            let mut out = engine::approve(&tx, task_id, user, Some("内控用例"))
                .await
                .expect("同意不应报数据库错误");
            tx.commit().await.expect("提交事务");
            engine::flush_effects(out.effects.take()).await;
            engine::flush_drafts(out.effects.take_drafts()).await;
            out
        }

        async fn purge(
            suffix: &str,
            order_id: Uuid,
            instance_id: Uuid,
            def_id: Uuid,
            version_id: Uuid,
            role_id: Option<Uuid>,
        ) {
            for sql in [
                "DELETE FROM biz_flow_task WHERE instance_id = ?::uuid",
                "DELETE FROM biz_flow_instance_node WHERE instance_id = ?::uuid",
                "DELETE FROM biz_flow_instance WHERE id = ?::uuid",
            ] {
                let _ = db().execute_with_params(sql, vec![Value::from(&instance_id)]).await;
            }
            let _ = db()
                .execute_with_params(
                    "DELETE FROM biz_purchase_order WHERE id = ?::uuid",
                    vec![Value::from(&order_id)],
                )
                .await;
            let _ = db()
                .execute_with_params(
                    "DELETE FROM biz_flow_def_version WHERE id = ?::uuid",
                    vec![Value::from(&version_id)],
                )
                .await;
            let _ = db()
                .execute_with_params(
                    "DELETE FROM biz_flow_def WHERE id = ?::uuid",
                    vec![Value::from(&def_id)],
                )
                .await;
            let _ = db()
                .execute_with_params(
                    "DELETE FROM sys_user_role WHERE user_id IN \
                         (SELECT id FROM sys_user WHERE username LIKE ?)",
                    vec![Value::Text(format!("{suffix}_%"))],
                )
                .await;
            let _ = db()
                .execute_with_params(
                    "DELETE FROM sys_user WHERE username LIKE ?",
                    vec![Value::Text(format!("{suffix}_%"))],
                )
                .await;
            if let Some(role_id) = role_id {
                let _ = db()
                    .execute_with_params("DELETE FROM sys_role WHERE id = ?::uuid", vec![Value::from(&role_id)])
                    .await;
            }
        }
    }

    /// ④ SKIPPED 待办**不出现在「我的待办」里**（不是「看得到点不动」）。
    #[tokio::test]
    async fn skipped_task_is_not_in_my_todo() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let tag = "todo";
        let seed = 7u128;
        let creator = uid(seed * 100 + 5);
        let keeper = uid(seed * 100 + 11);
        let initiator = uid(seed * 100 + 6);
        let sc = Scenario::start(tag, seed, &[(creator, "USER"), (keeper, "USER")], &[]).await;

        assert_eq!(sc.status_of(creator).await.as_deref(), Some("SKIPPED"));
        assert_eq!(sc.status_of(keeper).await.as_deref(), Some("PENDING"));
        assert!(
            sc.comment_of(creator).await.contains("内控"),
            "SKIPPED 行必须写清原因（实际：{}）",
            sc.comment_of(creator).await
        );

        // 制单人的「我的待办」里查不到这条（status 只认 PENDING）
        let page = crate::business::flow::model::page_my_tasks(crate::business::flow::model::MyTaskQueryParams {
            user_id: creator,
            statuses: &["PENDING"],
            kinds: &["APPROVAL"],
            order: crate::business::flow::model::MyTaskOrder::Arrived,
            page: 1,
            page_size: 200,
            live_only: true,
            completed_since: None,
        })
        .await
        .expect("读我的待办");
        assert_eq!(page.total, 0, "内控排除的人不该在待办列表里看到它");
        assert!(page.list.is_empty(), "list 也必须为空（不能出现「total 0 但有行」）");

        // 对照：能批的人看得到 —— 证明查询本身是有效的（不是恒空）
        let page = crate::business::flow::model::page_my_tasks(crate::business::flow::model::MyTaskQueryParams {
            user_id: keeper,
            statuses: &["PENDING"],
            kinds: &["APPROVAL"],
            order: crate::business::flow::model::MyTaskOrder::Arrived,
            page: 1,
            page_size: 200,
            live_only: true,
            completed_since: None,
        })
        .await
        .expect("读我的待办");
        assert_eq!(page.total, 1, "能批的人必须看到 1 条待办");
        let _ = (sc.initiator, initiator);
    }

    /// ⑤ 会签 3 人、1 人被内控 SKIP → 剩余 2 人批完即通过（不卡死）。
    #[tokio::test]
    async fn countersign_one_excluded_then_two_approve_passes() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let seed = 1u128;
        let creator = uid(seed * 100 + 5);
        let a = uid(seed * 100 + 11);
        let b = uid(seed * 100 + 12);
        let sc = Scenario::start("one_skip", seed, &[(creator, "USER"), (a, "USER"), (b, "USER")], &[]).await;

        assert_eq!(sc.status_of(creator).await.as_deref(), Some("SKIPPED"));
        assert_eq!(
            sc.node_approver_count("n1").await,
            2,
            "应到人数只数 PENDING/APPROVED/REJECTED"
        );

        let out = sc.approve(a).await;
        assert_eq!(
            out.node_status.as_deref(),
            Some("ACTIVE"),
            "还有 1 人没批，节点仍审批中"
        );
        assert_eq!(sc.instance_status().await, "PENDING");

        let out = sc.approve(b).await;
        assert!(out.rejected.is_none());
        assert_eq!(out.node_status.as_deref(), Some("APPROVED"));
        assert_eq!(sc.node_status("n1").await, "APPROVED");
        assert_eq!(sc.instance_status().await, "APPROVED", "剩下的人批完即通过");
    }

    /// ⑥ 会签 3 人、2 人（制单人 + 发起人）被 SKIP → 剩余 1 人批完即通过。
    #[tokio::test]
    async fn countersign_two_excluded_then_one_approves() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let seed = 2u128;
        let creator = uid(seed * 100 + 5);
        let initiator = uid(seed * 100 + 6);
        let a = uid(seed * 100 + 11);
        let sc = Scenario::start(
            "two_skip",
            seed,
            &[(creator, "USER"), (initiator, "USER"), (a, "USER")],
            &[],
        )
        .await;

        assert_eq!(sc.status_of(creator).await.as_deref(), Some("SKIPPED"));
        assert_eq!(sc.status_of(initiator).await.as_deref(), Some("SKIPPED"));
        assert_eq!(sc.node_approver_count("n1").await, 1);

        let out = sc.approve(a).await;
        assert!(out.rejected.is_none());
        assert_eq!(out.node_status.as_deref(), Some("APPROVED"));
        assert_eq!(sc.instance_status().await, "APPROVED");
    }

    /// ⑦ 会签 2 人**全被内控排除** → 发起即失败（422 + 65018），**不许**静默自动通过。
    ///
    /// 这条用例以前断言「节点自动通过、实例 APPROVED」—— 那正是 t_72e95761 报的缺陷
    /// （配了流程也无人复核就生效）。新语义：审批岗位配错时在**发起那一刻**就挡住，
    /// 库里不留实例 / 节点 / 待办（回滚），报文点名节点 + 为什么被排除 + 两条出路。
    #[tokio::test]
    async fn countersign_all_excluded_fails_closed() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let seed = 3u128;
        let creator = uid(seed * 100 + 5);
        let initiator = uid(seed * 100 + 6);
        let sc = Scenario::build("all_skip", seed, &[(creator, "USER"), (initiator, "USER")], &[]).await;

        let error = sc.try_start().await.expect("全部被内控排除必须发起失败");
        assert_eq!(error.status, 422, "发起应当 422：{}", error.msg);
        assert_eq!(
            error.code,
            crate::shared::error_code::FLOW_NO_ASSIGNEE,
            "码必须是 65018（没有可用审批人）：{}",
            error.msg
        );
        assert!(
            error.msg.contains("内控") && error.msg.contains("全部是制单人"),
            "报文要说清是内控排除导致的：{}",
            error.msg
        );
        assert!(error.msg.contains("内控审批"), "报文要点名节点名：{}", error.msg);
        assert!(
            error.msg.contains("指派给非制单人"),
            "报文要给可执行的出路：{}",
            error.msg
        );

        // 回滚：没有实例、没有待办（不留半截审批单）
        assert_eq!(sc.instance_status_opt().await, None, "发起失败后不许留下实例行");
        assert!(sc.tasks().await.is_empty(), "发起失败后不许留下待办 / 留痕行");
    }

    /// ⑨ `engine::start_in_tx`（业务入口的新口径）：**业务拒绝 = 硬错误 + 整笔回滚**。
    ///
    /// # 这条用例钉的是什么（卡 t_0127714a）
    ///
    /// `engine::start` 把「派不出审批人（65018）/ 自审被禁（65019）」放进
    /// `ActionOut::rejected` 之后**正常返回**，而此时它已经写了实例行。业务入口若
    /// 「先 commit、再判 rejected」，库里就是「单据已置 SUBMITTED + 一张 PENDING 实例 +
    /// 0 行待办」—— 单据永久卡死（批不了 / 付不了 / 重提不了）。实测就是报销人 = 部门负责人
    /// 本人时提交报销单：`422 / 65018` 之后三张单全卡在 `SUBMITTED`。
    ///
    /// 新口径 `start_in_tx` 在**事务内**就把拒绝翻成 `Err(StartFailure::Rejected)`，
    /// 调用方（报销 / 处置 / 调拨 / 验收即发起）拿到 `Err` 只能回滚。本用例断言三件事：
    /// ① 拒绝码与结构化载荷原样保留（前端「指派审批人」引导框只认它）；
    /// ② 回滚后实例 / 节点 / 待办**一行都不留**；
    /// ③ 正常路径（有非制单人可签）照旧 `Ok` + 1 条 PENDING 待办（不能把好路径一起拦死）。
    #[tokio::test]
    async fn start_in_tx_turns_business_reject_into_rollback() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        // ---------- ① 拒绝路径：会签 2 人全是当事人 → 内控全剔掉 → 65018 ----------
        let seed = 8u128;
        let creator = uid(seed * 100 + 5);
        let initiator = uid(seed * 100 + 6);
        let sc = Scenario::build("start_tx_reject", seed, &[(creator, "USER"), (initiator, "USER")], &[]).await;

        let error = sc
            .try_start_in_tx()
            .await
            .expect("全部被内控排除时 start_in_tx 必须返回 Err（不许正常返回）");
        assert_eq!(error.status, 422, "发起应当 422：{}", error.msg);
        assert_eq!(
            error.code,
            crate::shared::error_code::FLOW_NO_ASSIGNEE,
            "码必须是 65018（没有可用审批人）：{}",
            error.msg
        );
        assert!(
            error.data.is_some(),
            "65018 的结构化载荷（node_key / role_code / fix_hint）必须原样带出，\
             否则前端「指派审批人」引导框只剩一句红字：{}",
            error.msg
        );
        // 回滚后：实例 / 节点 / 待办全部为 0（这正是「单据卡死」的反面）
        assert_eq!(
            sc.instance_status_opt().await,
            None,
            "start_in_tx 拒绝后不许留下实例行（否则单据会被实例占住、批不动也重提不了）"
        );
        assert!(sc.tasks().await.is_empty(), "start_in_tx 拒绝后不许留下待办 / 留痕行");
        let nodes: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_flow_instance_node WHERE instance_id = ?::uuid",
                vec![Value::from(&sc.instance_id)],
            )
            .await
            .expect("读实例节点");
        assert_eq!(nodes, 0, "start_in_tx 拒绝后不许留下实例节点行");

        // ---------- ② 正常路径：有非制单人可签 → Ok + 实例 PENDING + 1 条待办 ----------
        let ok_seed = 9u128;
        let approver = uid(ok_seed * 100 + 11);
        let ok = Scenario::build("start_tx_ok", ok_seed, &[(approver, "USER")], &[]).await;
        assert!(
            ok.try_start_in_tx().await.is_none(),
            "有非制单人可签时 start_in_tx 必须成功（不能把正常路径一起拦死）"
        );
        assert_eq!(ok.instance_status().await, "PENDING");
        assert_eq!(
            ok.tasks().await.len(),
            1,
            "成功路径必须留下 1 条待办（否则就是本卡要修的「实例在跑、0 待办」）"
        );
    }

    /// ⑧ 绕过前端直接审批（内控当事人）→ 仍被 403 兜底拦住；引擎侧也不会替他批。
    #[tokio::test]
    async fn excluded_user_cannot_bypass_by_calling_approve() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let seed = 4u128;
        let creator = uid(seed * 100 + 5);
        let initiator = uid(seed * 100 + 6);
        let a = uid(seed * 100 + 11);
        let sc = Scenario::start(
            "bypass",
            seed,
            &[(creator, "USER"), (initiator, "USER"), (a, "USER")],
            &[],
        )
        .await;

        // ① 事务前的内控闸（handler 的入口）对当事人一律 403
        let creator_task = sc.task_id_of(creator).await;
        let error = crate::business::erp::purchase_bridge::guard_self_approval(creator_task, creator)
            .await
            .expect_err("制单人必须被拦");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, crate::shared::error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert!(error.msg.contains("不能审批自己提交的采购订单"), "{}", error.msg);

        let initiator_task = sc.task_id_of(initiator).await;
        let error = crate::business::erp::purchase_bridge::guard_self_approval(initiator_task, initiator)
            .await
            .expect_err("发起人必须被拦");
        assert_eq!(error.status, 403);

        // ② 与内控无关的审批人：闸放行
        let keeper_task = sc.task_id_of(a).await;
        crate::business::erp::purchase_bridge::guard_self_approval(keeper_task, a)
            .await
            .expect("非当事人不该被内控闸拦");

        // ③ 就算绕过 handler 直接调引擎，那票也早就不是 PENDING → 不产生「他批了」的效果
        let before = sc.instance_status().await;
        let out = sc.approve(creator).await;
        assert!(out.idempotent, "已 SKIPPED 的票只能幂等返回");
        assert_eq!(sc.status_of(creator).await.as_deref(), Some("SKIPPED"));
        assert_eq!(sc.instance_status().await, before, "状态不许因他的「审批」而推进");
    }

    /// ⑨ 内控排除后，能批的人正常批（票数 / 意见 / 状态都对）。
    #[tokio::test]
    async fn remaining_approver_votes_normally() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let seed = 5u128;
        let creator = uid(seed * 100 + 5);
        let a = uid(seed * 100 + 11);
        let sc = Scenario::start("normal_vote", seed, &[(creator, "USER"), (a, "USER")], &[]).await;

        let out = sc.approve(a).await;
        assert!(out.rejected.is_none());
        assert_eq!(out.approver_count, 1, "应到人数 = 1（被内控排除的人不算）");
        assert_eq!(out.approved_count, 1);
        assert_eq!(out.rejected_count, 0);
        assert_eq!(sc.status_of(a).await.as_deref(), Some("APPROVED"));
        assert_eq!(
            sc.status_of(creator).await.as_deref(),
            Some("SKIPPED"),
            "制单人的留痕行不许被这次投票改写"
        );
    }

    /// ⑩ 审批人换成业务角色（成员既非制单人、也非发起人）→ 没有人被排除。
    #[tokio::test]
    async fn role_members_are_not_excluded() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let seed = 6u128;
        let u1 = uid(seed * 100 + 11);
        let u2 = uid(seed * 100 + 12);
        let role_id = uid(seed * 100 + 21);
        let creator = uid(seed * 100 + 5);
        let initiator = uid(seed * 100 + 6);
        let sc = Scenario::start("role_fix", seed, &[(role_id, "ROLE")], &[u1, u2]).await;

        let tasks = sc.tasks().await;
        assert_eq!(tasks.len(), 2, "角色里 2 个人都要有待办：{tasks:?}");
        assert!(
            tasks.iter().all(|(_, status, _)| status == "PENDING"),
            "配置改对之后不该有人被排除：{tasks:?}"
        );
        assert!(tasks.iter().all(|(id, _, _)| *id != creator && *id != initiator));
        assert_eq!(sc.node_approver_count("n1").await, 2);
        assert_eq!(sc.instance_status().await, "PENDING");
    }

    /// 收尾：把本模块的 `p0fix_*` 夹具**从库里彻底清掉**（模块跑完的最后一条用例）。
    ///
    /// # 为什么必须是模块级收尾（而不是每例 `setup` 自清）
    ///
    /// 这些夹具的 `biz_flow_def` 是 `biz_type = 'PO_APPROVE'` + `status = 'PUBLISHED'`，
    /// 还会留下 `PENDING` 的 `PO_APPROVE` 实例、且 7 个版本的 `graph_hash` 是 `0…0`（夹具不写哈希）。
    /// 同一个库连跑第二轮时：
    /// * `purchase_handler`（erp 段，排在 flow 段之前）的「没配流程必须 409」「额度内可显式跳过」
    ///   「模块收尾不许剩 PENDING 采购审批实例」会先跑，被上一轮的残留打成假红（实测 4 条）；
    /// * `stockin` 的全局「审批图自检」（`flow::maintain::run_once`）会把这 7 个零哈希版本
    ///   报成 7 条 `RowsEdited` 漂移（实测 1 条）。
    ///
    /// 本用例名以 `zz_` 开头：串行跑（`--test-threads=1`）时排在本模块最后，
    /// 删掉的正好是本模块自己留下的行；断言是**库侧复查**（不信 self-report）。
    #[tokio::test]
    async fn zz_purge_module_fixtures() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        // 删除顺序按外键依赖：状态日志（无外键，先删）→ 实例子表 → 实例 → 采购单 → 流程图 → 定义 → 人。
        let stmts = [
            "DELETE FROM biz_status_log WHERE entity_id IN \
             (SELECT id FROM biz_flow_instance WHERE biz_no LIKE 'P0FIX-%')",
            "DELETE FROM biz_status_log WHERE entity_id IN \
             (SELECT id FROM biz_purchase_order WHERE order_no LIKE 'P0FIX-%')",
            "DELETE FROM biz_flow_task WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE biz_no LIKE 'P0FIX-%')",
            "DELETE FROM biz_flow_instance_node WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE biz_no LIKE 'P0FIX-%')",
            "DELETE FROM biz_flow_cc WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE biz_no LIKE 'P0FIX-%')",
            // 自引用外键（prev_instance_id）先解开，再删本体
            "UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE biz_no LIKE 'P0FIX-%'",
            "DELETE FROM biz_flow_instance WHERE biz_no LIKE 'P0FIX-%'",
            "DELETE FROM biz_purchase_order_item WHERE order_id IN \
             (SELECT id FROM biz_purchase_order WHERE order_no LIKE 'P0FIX-%')",
            "DELETE FROM biz_purchase_order WHERE order_no LIKE 'P0FIX-%'",
            "DELETE FROM biz_flow_edge WHERE version_id IN \
             (SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id \
               WHERE d.flow_code LIKE 'p0fix\\_%')",
            "DELETE FROM biz_flow_node_assignee WHERE node_id IN \
             (SELECT n.id FROM biz_flow_node n JOIN biz_flow_def_version v ON v.id = n.version_id \
                JOIN biz_flow_def d ON d.id = v.def_id WHERE d.flow_code LIKE 'p0fix\\_%')",
            "DELETE FROM biz_flow_node WHERE version_id IN \
             (SELECT v.id FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id \
               WHERE d.flow_code LIKE 'p0fix\\_%')",
            "DELETE FROM biz_flow_def_version WHERE def_id IN \
             (SELECT id FROM biz_flow_def WHERE flow_code LIKE 'p0fix\\_%')",
            "DELETE FROM biz_flow_def WHERE flow_code LIKE 'p0fix\\_%'",
            "DELETE FROM sys_user_role WHERE user_id IN \
             (SELECT id FROM sys_user WHERE username LIKE 'p0fix\\_%')",
            "DELETE FROM sys_user_role WHERE role_id IN \
             (SELECT id FROM sys_role WHERE role_code LIKE 'P0FIX\\_%')",
            "DELETE FROM sys_role WHERE role_code LIKE 'P0FIX\\_%'",
            "DELETE FROM sys_user WHERE username LIKE 'p0fix\\_%'",
        ];
        for sql in stmts {
            if let Err(error) = db().execute_with_params(sql, vec![]).await {
                panic!("模块收尾失败：{error}\nSQL: {sql}");
            }
        }
        // 库侧复查（不信 self-report）：残留必须为 0
        for (label, sql) in [
            (
                "p0fix 流程定义",
                "SELECT count(*) FROM biz_flow_def WHERE flow_code LIKE 'p0fix\\_%'",
            ),
            (
                "P0FIX 采购单",
                "SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'P0FIX-%'",
            ),
            (
                "P0FIX 审批实例",
                "SELECT count(*) FROM biz_flow_instance WHERE biz_no LIKE 'P0FIX-%'",
            ),
            (
                "p0fix 夹具用户",
                "SELECT count(*) FROM sys_user WHERE username LIKE 'p0fix\\_%'",
            ),
        ] {
            let left: i64 = db()
                .scalar_with_params(sql, vec![])
                .await
                .unwrap_or_else(|error| panic!("复查{label}失败：{error}"));
            assert_eq!(left, 0, "模块收尾后{label}必须为 0（否则同库第二轮会假红）");
        }
        println!("[internal_control] 模块收尾：p0fix 夹具已从库里清空");
    }
}
