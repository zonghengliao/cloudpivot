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
//! # ⚠️ 配置红线（自动通过）
//!
//! 若某节点**全部**审批人都是被排除的人，节点会按 `empty_assignee` 策略自动通过
//! （与「角色内 0 人」同一路径），并打 `warn` + 给发起人发一条知会。
//! 这是「流程别卡死」的最低要求，但**它意味着这张单没过任何人的手** ——
//! 出现这种日志说明审批人配置有问题：审批岗位必须由**非制单人 / 非发起人**担任。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::{payable_bridge, purchase_bridge, stockin_bridge};
use crate::business::flow::assignee::Candidate;
use crate::business::flow::model as flow_model;

/// 「制单人不可自审」的留痕原因（写进 `biz_flow_task.comment`，审批详情直接展示）。
pub const CREATOR_REASON: &str = "内控：单据制单人不可审批自己制单的单据（职责分离），已跳过并留痕";
/// 「发起人不可自审」的留痕原因。
pub const INITIATOR_REASON: &str =
    "内控：审批单发起人不可审批自己发起的审批（职责分离），已跳过并留痕";

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
    let mut exclusions: Vec<Exclusion> = Vec::new();
    let biz_type = instance.biz_type.trim().to_ascii_uppercase();
    let governed = if biz_type == purchase_bridge::BIZ_TYPE {
        let creator = purchase_bridge::internal_control_creator(instance.biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == stockin_bridge::POST_BIZ_TYPE || biz_type == stockin_bridge::BIZ_TYPE {
        let creator = stockin_bridge::internal_control_creator(instance.biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == payable_bridge::BIZ_TYPE {
        let creator = payable_bridge::internal_control_creator(instance.biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == payable_bridge::CANCEL_BIZ_TYPE {
        // P-FLOW-3B：应付作废审批的 `biz_id` 是**应付单 id**（不是付款记录 id）
        let creator = payable_bridge::internal_control_creator_of_payable(instance.biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else if biz_type == crate::business::erp::voucher_bridge::BIZ_TYPE {
        // P-FLOW-3B：凭证红冲审批的 `biz_id` 是**凭证 id**
        let creator =
            crate::business::erp::voucher_bridge::internal_control_creator(instance.biz_id).await?;
        push_exclusion(&mut exclusions, creator, CREATOR_REASON);
        true
    } else {
        false
    };
    if governed {
        push_exclusion(
            &mut exclusions,
            Some(instance.initiator_id),
            INITIATOR_REASON,
        );
    }
    Ok(Control {
        governed,
        exclusions,
    })
}

/// 把审批人列表按内控切成「该落 `PENDING` 的」与「该落 `SKIPPED` 留痕的」（**纯函数**）。
///
/// 返回 `(保留的审批人, [(被排除的人, 原因)])`；顺序保持入参顺序（与 `finalize` 的排序一致，
/// 待办 / 留痕行的展示顺序因此可复现）。
pub fn partition(
    approvers: Vec<Candidate>,
    control: &Control,
) -> (Vec<Candidate>, Vec<(Candidate, String)>) {
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
        let (kept, excluded) = partition(
            vec![cand(creator, "制单人"), cand(other, "审批人")],
            &control,
        );
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
// 跑法：
// ```bash
// P0FIX_DB_URL='postgres://p0fixrole:***@127.0.0.1:5432/cloudpivot_p0fix' \
//   cargo test --lib business::flow::internal_control -- --test-threads=1 --nocapture
// ```
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
                if try_db().is_some() {
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

    /// 一致性 uuid（同一 `seed` 永远得到同一个 id，重跑时先删后建）。
    fn uid(seed: u128) -> Uuid {
        Uuid::from_u128(SEED_BASE + seed)
    }

    /// 一条审批 / 会签节点的快照节点（审批人由调用方给）。
    fn approve_spec(
        node_type: &str,
        assignee_type: &str,
        assignees: &[(Uuid, &str)],
    ) -> SnapshotNode {
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
        /// 造场景并**发起实例**。
        async fn start(
            tag: &str,
            seed: u128,
            approvers: &[(Uuid, &str)],
            role_members: &[Uuid],
        ) -> Self {
            let instance_id = uid(seed * 100 + 1);
            let def_id = uid(seed * 100 + 2);
            let version_id = uid(seed * 100 + 3);
            let order_id = uid(seed * 100 + 4);
            let creator = uid(seed * 100 + 5);
            let initiator = uid(seed * 100 + 6);
            let users = vec![
                uid(seed * 100 + 11),
                uid(seed * 100 + 12),
                uid(seed * 100 + 13),
            ];
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

            let scenario = Self {
                tag: tag.to_string(),
                def_id,
                version_id,
                instance_id,
                order_id,
                initiator,
            };
            scenario.run_start().await;
            scenario
        }

        /// `engine::start`（事务内）+ 提交 + flush 副作用。
        async fn run_start(&self) {
            let req = engine::StartReq {
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
            };
            let tx = db().begin().await.expect("开启事务");
            let mut out = engine::start(&tx, &req).await.expect("发起实例");
            assert!(out.rejected.is_none(), "发起不应失败：{:?}", out.rejected);
            tx.commit().await.expect("提交事务");
            engine::flush_effects(out.effects.take()).await;
            engine::flush_drafts(out.effects.take_drafts()).await;
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
                vec![
                    Value::from(&self.instance_id),
                    Value::Text(node_key.to_string()),
                ],
            )
            .await
            .expect("读节点")
            .unwrap_or_default()
        }

        async fn node_approver_count(&self, node_key: &str) -> i32 {
            db().scalar_with_params::<Option<i32>>(
                "SELECT approver_count FROM biz_flow_instance_node \
                  WHERE instance_id = ?::uuid AND node_key = ? AND round = 1 LIMIT 1",
                vec![
                    Value::from(&self.instance_id),
                    Value::Text(node_key.to_string()),
                ],
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
                let _ = db()
                    .execute_with_params(sql, vec![Value::from(&instance_id)])
                    .await;
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
                    .execute_with_params(
                        "DELETE FROM sys_role WHERE id = ?::uuid",
                        vec![Value::from(&role_id)],
                    )
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
        let creator = uid(0 * 100 + 5);
        let keeper = uid(0 * 100 + 11);
        let initiator = uid(0 * 100 + 6);
        let sc = Scenario::start(tag, 0, &[(creator, "USER"), (keeper, "USER")], &[]).await;

        assert_eq!(sc.status_of(creator).await.as_deref(), Some("SKIPPED"));
        assert_eq!(sc.status_of(keeper).await.as_deref(), Some("PENDING"));
        assert!(
            sc.comment_of(creator).await.contains("内控"),
            "SKIPPED 行必须写清原因（实际：{}）",
            sc.comment_of(creator).await
        );

        // 制单人的「我的待办」里查不到这条（status 只认 PENDING）
        let page = crate::business::flow::model::page_my_tasks(
            creator,
            &["PENDING"],
            &["APPROVAL"],
            crate::business::flow::model::MyTaskOrder::Arrived,
            1,
            200,
            true,
        )
        .await
        .expect("读我的待办");
        assert_eq!(page.total, 0, "内控排除的人不该在待办列表里看到它");
        assert!(
            page.list.is_empty(),
            "list 也必须为空（不能出现「total 0 但有行」）"
        );

        // 对照：能批的人看得到 —— 证明查询本身是有效的（不是恒空）
        let page = crate::business::flow::model::page_my_tasks(
            keeper,
            &["PENDING"],
            &["APPROVAL"],
            crate::business::flow::model::MyTaskOrder::Arrived,
            1,
            200,
            true,
        )
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
        let sc = Scenario::start(
            "one_skip",
            seed,
            &[(creator, "USER"), (a, "USER"), (b, "USER")],
            &[],
        )
        .await;

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

    /// ⑦ 会签 3 人**全被 SKIP** → 节点自动通过（并留痕 + warn），流程不卡死。
    #[tokio::test]
    async fn countersign_all_excluded_auto_passes() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let seed = 3u128;
        let creator = uid(seed * 100 + 5);
        let initiator = uid(seed * 100 + 6);
        let sc = Scenario::start(
            "all_skip",
            seed,
            &[(creator, "USER"), (initiator, "USER")],
            &[],
        )
        .await;

        // 两条 SKIPPED 留痕（不删待办行）
        let tasks = sc.tasks().await;
        assert_eq!(tasks.len(), 2, "被排除的人必须有留痕行：{tasks:?}");
        assert!(tasks.iter().all(|(_, status, _)| status == "SKIPPED"));
        assert!(
            tasks.iter().all(|(_, _, comment)| comment.contains("内控")),
            "留痕要写原因：{tasks:?}"
        );
        // 节点按「空审批人」策略跳过，实例继续走到 END
        assert_eq!(sc.node_status("n1").await, "SKIPPED");
        assert_eq!(sc.node_approver_count("n1").await, 0);
        assert_eq!(
            sc.instance_status().await,
            "APPROVED",
            "全部被排除时节点自动通过（同时打 warn：配置有问题）"
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
        let error =
            crate::business::erp::purchase_bridge::guard_self_approval(creator_task, creator)
                .await
                .expect_err("制单人必须被拦");
        assert_eq!(error.status, 403);
        assert_eq!(
            error.code,
            crate::shared::error_code::FLOW_SELF_APPROVAL_FORBIDDEN
        );
        assert!(
            error.msg.contains("不能审批自己提交的采购订单"),
            "{}",
            error.msg
        );

        let initiator_task = sc.task_id_of(initiator).await;
        let error =
            crate::business::erp::purchase_bridge::guard_self_approval(initiator_task, initiator)
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
        assert_eq!(
            sc.instance_status().await,
            before,
            "状态不许因他的「审批」而推进"
        );
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
        assert!(
            tasks
                .iter()
                .all(|(id, _, _)| *id != creator && *id != initiator)
        );
        assert_eq!(sc.node_approver_count("n1").await, 2);
        assert_eq!(sc.instance_status().await, "PENDING");
    }
}
