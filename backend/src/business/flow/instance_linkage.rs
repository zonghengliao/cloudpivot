// ============================================================================
// instance_linkage.rs —— 审批实例（FLOW_INSTANCE）终态的跨单据联动接线（LNK-12）
//
// # 这一步之前为什么「先设计再接线」（卡面原文的问题，逐条回答）
//
// ## ① 审批通过时，`FLOW_INSTANCE` 触发 和 业务单据（如 `PURCHASE_ORDER`）触发，谁先谁后
//
// **先审批实例，后业务单据**，而且两者之间隔着**一次事务提交**：
//
// ```text
// handler::approve_task
//   ① db().transaction(engine::approve)      ← 实例落终态（APPROVED），提交
//   ② drain(out)                             ← 引擎自己的通知（事务外）
//   ③ instance_linkage::fire_after_commit    ← 【本模块】FLOW_INSTANCE/APPROVED 触发点
//   ④ purchase_bridge::on_po_settled …       ← 业务桥改单据状态 SUBMITTED→APPROVED
//                                                 └─ 单据自己的触发点 PURCHASE_ORDER/APPROVED
// ```
//
// 理由（三条，缺一条这个顺序都站不住）：
//
// 1. **因果顺序**：实例状态是「原因」，单据状态是「结果」。业务桥是**回调**，
//    回调的输入就是 `out.instance_status`；
// 2. **失败语义**：业务桥的错误一律被吞掉（`on_po_settled` 返回 `Option`，
//    失败只打日志不回滚）。把本触发点放在桥**后面**，等于「桥今天挂了，
//    『你的审批通过了』这条通知和要建的审计待办就永远丢了」；
// 3. **锁与事务**：业务桥各自开事务改单据（还可能再跑一遍自己的联动），
//    本触发点只有 `Phase::AfterCommit` 动作（见下），不需要事务、也不占锁，
//    排在最前面的开销最小。
//
// ## ② 会不会两遍都跑？怎么去重
//
// **同一个触发点绝不会跑两遍**，分三层保证：
//
// | 层 | 机制 | 挡住的形态 |
// |----|------|-----------|
// | 入口唯一 | 只有 [`fire_after_commit`] 一个函数调引擎，四个调用点（approve / reject / withdraw / 超时自动代点）都在事务提交之后调它 | 「同一个处理器里两个地方各调一次」 |
// | 状态闸 | 只认 [`TERMINAL_STATUSES`]，且**实例当前状态必须等于触发状态**（`instance.status == status`） | 中间票（会签第一票仍是 `PENDING`）、传错状态、旧值重放 |
// | 幂等键 | 通知的 `idempotency_key = LNK:FLOW_INSTANCE:<状态>:<实例 id>`（`uk_sys_notice_idem`） | 同一个实例同一个结论被重放 / 重试：只落一条通知 |
//
// **跨触发点**（同一张采购单既配了 `FLOW_INSTANCE/APPROVED` 又配了
// `PURCHASE_ORDER/APPROVED` 的通知）会各发一条，这是**两条不同的业务事实**：
// 「你提交的审批单通过了」vs「采购订单已审批，可以去收货了」。本卡**不做跨触发点的
// 通知合并**（合并要靠「谁去重谁」的全局约定，任一方向都会静默吞掉合法通知）；
// 要做到只发一条，就在一处配。
//
// 真正危险的不是通知而是**落账**，所以本触发点在配置期就把全部 `Phase::InTx` 动作
// 拒掉（[`spec::FLOW_INSTANCE_FORBIDDEN_ACTIONS`]），执行时再用
// [`spec::FLOW_INSTANCE_EXCLUDED_ACTIONS`] 排一遍（兜历史脏行 / 绕过 API 直接写库）。
// 「审批通过 → 改单据状态 / 写库存 / 出凭证」的**唯一实现**是业务桥，
// 因此同一笔账不可能因为多了这条触发点而落两遍。
//
// ## ③ 驳回 / 撤回是否也触发
//
// **都触发**：`REJECTED`（驳回并终止）与 `WITHDRAWN`（发起人撤回）是审批的另外两种
// **结论**，而且是最需要通知发起人的两种（单子被打回了 / 退回来了）。
// 不触发的两个状态，以及理由：
//
// - `PENDING`：不是结论；发起那一刻由流程自己的通知节点（`notify_node`）负责；
// - `SKIPPED`：节点 / 流程任务的状态，**从来不是实例状态**（DDL 的 CHECK 里没有它）；
// - `CANCELLED`：唯一来源是**级联 / 行政终止**（任务被取消、业务单据被删、报销作废），
//   其中「单据被删 → 终止实例」走 `flow_model::cancel_pending_of_biz` 直接 SQL、
//   不经过引擎。接在这里就会「同一张单有时响、有时不响」——比不接更糟。
//   这类通知由各自的业务回调负责（`task_bridge` / `expense_bridge`）。
//
// 于是「可配状态」= 「真的会触发的状态」= `APPROVED / REJECTED / WITHDRAWN`，
// 三张表（`spec::TRIGGER_SPECS` 的 status_labels、`model::known_statuses`、
// 本文件的 [`TERMINAL_STATUSES`]）由单测 `statuses_match_fired_verdicts` 钉住逐字一致：
// 「配得上、永远不响」正是 P1-FIX-5C 那类静默失效。
//
// # 事务边界（铁律 ④）
//
// 本触发点**没有事务内动作**：上面说的禁令让 `run_excluding` 只会把
// `SEND_NOTICE` / `CREATE_TASK` 放进 `run.deferred`，随后 [`fire_after_commit`]
// 自己调 `run_deferred` 执行。所以调用方**不需要**开事务，也不该开：
// 审批已经提交，这里再包一层事务只会让「通知发失败」把额度 / 锁拖长。
// 单测 `allowed_actions_are_all_after_commit` 会检查「FLOW_INSTANCE 上允许的已接管动作
// 必须全部是 `Phase::AfterCommit`」——将来有人把某个 InTx 动作从禁令里放出来，这里立刻红。
//
// # 失败语义
//
// 永不返回 `Err`、永不 panic：审批事务已经提交，这里失败只打 WARN（与
// `run_deferred` 同口径）。「联动配置写错 → 审批接口 500」是把配置问题变成业务故障。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::linkage::{self, spec};
use crate::business::flow::model as flow_model;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeTarget};

/// 会触发 `FLOW_INSTANCE` 的三个终态（= 审批的三种结论）。
///
/// 与 `spec::TRIGGER_SPECS` 的 `status_labels`、`linkage::model::known_statuses` 逐字一致，
/// 由单测 `statuses_match_fired_verdicts` 钉住。见文件头 ③ 为什么不含
/// `PENDING` / `SKIPPED` / `CANCELLED`。
pub const TERMINAL_STATUSES: &[&str] = &["APPROVED", "REJECTED", "WITHDRAWN"];

/// 通知的幂等键前缀（`biz_flow_instance.instance_no` 不含用户输入，键里不必再转义）。
const NOTICE_KEY_PREFIX: &str = "LNK:FLOW_INSTANCE";

/// 状态是不是「会触发」的终态（大小写不敏感；返回**规范大写**的静态值）。
pub fn canonical_status(status: &str) -> Option<&'static str> {
    let trimmed = status.trim();
    TERMINAL_STATUSES
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(trimmed))
}

/// 是否审批结论（`fire_after_commit` 的状态闸，供调用方 / 单测复用）。
pub fn is_terminal(status: &str) -> bool {
    canonical_status(status).is_some()
}

/// 通知幂等键：**同一实例同一结论**永远同一个键 ⇒ 重放只落一条通知。
///
/// 刻意**不带**操作人 / 时间：「谁在什么时候又点了一次」不改变「这张单的结论是已通过」
/// 这个事实；带时间就退化成「每点一次多一条」。
pub fn notice_key(instance_id: Uuid, status: &str) -> String {
    format!(
        "{NOTICE_KEY_PREFIX}:{}:{instance_id}",
        canonical_status(status).unwrap_or(status.trim())
    )
}

/// 通知标题里的动词（三种结论各一个，不拼字符串）。
fn verb_of(status: &str) -> &'static str {
    match status {
        "APPROVED" => "已通过",
        "REJECTED" => "被驳回",
        "WITHDRAWN" => "已撤回",
        // `canonical_status` 已经在入口挡住；这里兜底成中性词，绝不 panic
        _ => "状态已变更",
    }
}

/// 触发点的通知基线（`SEND_NOTICE` 的 `LEGACY` 口径：配置不覆盖时逐字沿用它）。
///
/// 收件人 = **发起人**（「你提交的单子有结论了」），`creator` 也填发起人，
/// 这样配置里 `recipient = CREATOR` 同样能落到人身上。
fn draft_of(instance: &flow_model::InstanceRow, status: &str) -> notice::NoticeDraft {
    let label = spec::status_label_of(spec::TRIGGER_FLOW_INSTANCE, status).unwrap_or(status);
    let mut content = format!(
        "审批单 {}（{}）当前状态：{}。",
        instance.instance_no, instance.flow_name, label
    );
    if let Some(biz_no) = instance.biz_no.as_deref().filter(|text| !text.is_empty()) {
        content.push_str(&format!("关联单据：{biz_no}。"));
    }
    if let Some(comment) = instance
        .result_comment
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        content.push_str(&format!("审批意见：{comment}"));
    }
    let mut draft = notice::NoticeDraft::text(
        NoticeType::Workflow,
        NoticeLevel::Info,
        format!("审批{}：{}", verb_of(status), instance.title),
        content,
        NoticeTarget::User(vec![instance.initiator_id]),
    );
    draft.link_url = Some(format!("/business/flow/instance?id={}", instance.id));
    // 审批单是「哪张单的结论」的载体：biz_type 用实例自己的类型，biz_id 用实例 id，
    // 站内信的跳转 / 排障都指得回这张审批单（不冒充业务单据 —— 那张单另有自己的通知）。
    draft.biz_type = Some("FLOW_INSTANCE".to_string());
    draft.biz_id = Some(instance.id.to_string());
    draft.sender_name = Some("审批实例".to_string());
    // 知情类：尊重 notify.inbox（结论在审批中心 / 详情时间线里都查得到，
    // 漏看一条站内信不至于造成损失），与期间 / 年结 / 折旧同口径。
    draft.force = false;
    draft.idempotency_key = Some(notice_key(instance.id, status));
    draft
}

/// **本触发点唯一的引擎入口**：审批实例落终态之后（事务已提交）跑
/// `FLOW_INSTANCE` 触发点的联动。
///
/// # 参数
///
/// - `instance_id`：引擎出参里的实例 id（`ActionOut::instance_id`），`None` = 没碰实例；
/// - `status`：动作**之后**的实例状态（`ActionOut::instance_status`）。中间票传 `PENDING`，
///   这里直接返回 0（不是错误）；
/// - `operator`：本次操作人（审批人 / 发起人本人 / 系统代点时的审批人）。
///
/// # 返回
///
/// 实际执行的规则条数（通知 + 建任务）。**重复调用返回 0**（幂等键让通知不重复落库）。
///
/// # 失败
///
/// 永不返回 `Err`：审批已提交，配置 / 引擎问题只记 WARN，不把审批接口变成 500。
pub async fn fire_after_commit(
    instance_id: Option<Uuid>,
    status: &str,
    operator: Option<Uuid>,
) -> usize {
    let Some(status) = canonical_status(status) else {
        return 0;
    };
    let Some(instance_id) = instance_id else {
        return 0;
    };

    let instance = match flow_model::find_instance(instance_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            tracing::warn!(
                %instance_id,
                status,
                "FLOW_INSTANCE 联动跳过：审批单查不到（已删 / id 不对）"
            );
            return 0;
        }
        Err(error) => {
            tracing::warn!(
                %instance_id,
                status,
                error = %error,
                "FLOW_INSTANCE 联动跳过：读审批单失败"
            );
            return 0;
        }
    };
    // 状态闸：实例**当前**状态必须就是触发状态。
    // 这一条挡的是「传错 / 传旧值 / 中间票」：审批单早已被别的动作推进时，
    // 用旧出参再跑一遍联动就是重复通知。
    if !instance.status.eq_ignore_ascii_case(status) {
        tracing::debug!(
            %instance_id,
            expected = status,
            actual = %instance.status,
            "FLOW_INSTANCE 联动跳过：实例当前状态与触发状态不一致"
        );
        return 0;
    }

    let ctx = linkage::LinkageContext::new(
        spec::TRIGGER_FLOW_INSTANCE,
        status,
        instance.id,
        &instance.instance_no,
        operator,
    )
    // owner 留空：审批单没有「负责人」概念；creator = 发起人（收到结论的人）。
    .with_people(None, Some(instance.initiator_id), None)
    .with_notice(draft_of(&instance, status));

    let run = match linkage::engine::run_excluding(db(), &ctx, spec::FLOW_INSTANCE_EXCLUDED_ACTIONS)
        .await
    {
        Ok(run) => run,
        Err(error) => {
            // 关键动作失败才会到这里；本触发点在配置期已禁止全部 InTx 动作，
            // 走到这里说明是绕过 API 写进去的脏规则 —— 记 ERROR 但不影响审批结果。
            tracing::error!(
                %instance_id,
                status,
                code = error.code,
                msg = %error.msg,
                "FLOW_INSTANCE 联动执行失败（审批已提交，不回滚）：检查是否有绕过 API 的脏规则"
            );
            return 0;
        }
    };

    for item in &run.runs {
        tracing::info!(
            %instance_id,
            status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "FLOW_INSTANCE 联动：事务内动作执行结果"
        );
    }

    let deferred = run.deferred;
    if deferred.is_empty() {
        return run.runs.len();
    }
    let executed = linkage::engine::run_deferred(&ctx, deferred).await;
    for item in &executed {
        tracing::info!(
            %instance_id,
            status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "FLOW_INSTANCE 联动：提交后动作执行结果"
        );
    }
    run.runs.len() + executed.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::flow::linkage::spec::{Phase, Severity};

    /// 三个可配状态 = 三个会触发的终态 = 三个中文名，三处逐字一致。
    ///
    /// 这张表是「配得上就一定响」的唯一保证：任何一处多写一个状态，
    /// 就会造出「能保存、能启用、永远不触发」的规则（P1-FIX-5C 的病）。
    #[test]
    fn statuses_match_fired_verdicts() {
        let decl =
            spec::trigger_spec(spec::TRIGGER_FLOW_INSTANCE).expect("FLOW_INSTANCE 必须在接线表里");
        let labels: Vec<&str> = decl.status_labels.iter().map(|(code, _)| *code).collect();
        assert_eq!(
            labels, TERMINAL_STATUSES,
            "spec 的可配状态必须与 fire_after_commit 真的会触发的状态逐字一致"
        );
        assert_eq!(
            linkage::model::known_statuses(spec::TRIGGER_FLOW_INSTANCE),
            TERMINAL_STATUSES,
            "写入口的 known_statuses 必须与触发状态同集（否则能配出永远不响的规则）"
        );
        for (code, label) in decl.status_labels {
            assert!(!label.trim().is_empty(), "{code} 缺中文名");
            assert_ne!(label, code, "{code} 的中文名回落成了编码（字典缺项）");
        }
        assert!(decl.wired, "接线完成后 wired 必须为 true");
        assert!(
            !decl.proofs.is_empty(),
            "wired = true 必须给 proofs（tests_wiring 会扫）"
        );
    }

    /// 入口状态闸：只认三种结论，大小写不敏感，其它一律不触发。
    #[test]
    fn only_verdicts_are_terminal() {
        for status in TERMINAL_STATUSES {
            assert!(is_terminal(status), "{status} 必须触发");
            assert!(is_terminal(&status.to_ascii_lowercase()), "大小写不敏感");
            assert!(is_terminal(&format!("  {status}  ")), "首尾空白要 trim");
        }
        for status in ["PENDING", "SKIPPED", "CANCELLED", "", "APPROVED2", "DRAFT"] {
            assert!(!is_terminal(status), "{status} 不该触发");
        }
        assert_eq!(canonical_status("approved"), Some("APPROVED"));
        assert_eq!(canonical_status("nope"), None);
    }

    /// 幂等键：同一实例同一结论恒定；不同结论 / 不同实例必须不同。
    #[test]
    fn notice_key_is_stable_per_instance_and_verdict() {
        let instance = Uuid::now_v7();
        let a = notice_key(instance, "APPROVED");
        let b = notice_key(instance, "approved");
        assert_eq!(a, b, "状态大小写不能改变幂等键");
        assert_eq!(a, notice_key(instance, "APPROVED"), "重放必须同一个键");
        assert_ne!(a, notice_key(instance, "REJECTED"), "不同结论必须不同键");
        assert_ne!(
            a,
            notice_key(Uuid::now_v7(), "APPROVED"),
            "不同实例必须不同键"
        );
        assert!(a.starts_with("LNK:FLOW_INSTANCE:APPROVED:"));
    }

    /// 三种结论的标题动词各不相同（驳回 / 撤回也要发得出去，且文案不混）。
    #[test]
    fn titles_distinguish_verdicts() {
        assert_eq!(verb_of("APPROVED"), "已通过");
        assert_eq!(verb_of("REJECTED"), "被驳回");
        assert_eq!(verb_of("WITHDRAWN"), "已撤回");
    }

    /// **最重要的守卫**：FLOW_INSTANCE 上允许的已接管动作必须全部是 `Phase::AfterCommit`。
    ///
    /// 本触发点在审批事务提交之后执行，进程里没有事务。任何一个 `Phase::InTx` 动作
    /// 被放进来（从禁令 / 排除清单里漏出去），就是无事务裸写账 —— 将来有人实现新的
    /// InTx 动作时，要么补进禁令，要么把接线点挪回事务里，二选一，不能悄悄放着。
    #[test]
    fn allowed_actions_are_all_after_commit() {
        for action in spec::ACTIONS.iter().filter(|action| action.implemented) {
            let allowed = !spec::FLOW_INSTANCE_FORBIDDEN_ACTIONS.contains(&action.code)
                && !spec::FLOW_INSTANCE_EXCLUDED_ACTIONS.contains(&action.code);
            if allowed {
                assert_eq!(
                    action.phase,
                    Phase::AfterCommit,
                    "动作 {} 在 FLOW_INSTANCE 上被允许，但它是 {} —— 本触发点没有事务，\
                     InTx 动作在这里就是无事务裸写；请把它补进 \
                     spec::FLOW_INSTANCE_EXCLUDED_ACTIONS / FORBIDDEN_ACTIONS",
                    action.code,
                    action.phase.as_str()
                );
                assert_eq!(
                    action.severity,
                    Severity::NonCritical,
                    "提交后动作不该是 Critical：审批已提交，失败只能记日志",
                );
            }
        }
        // 两个已接管的 InTx 动作必须确实被禁（配置期 + 运行时各一道）
        for code in [
            spec::ACTION_GENERATE_VOUCHER,
            spec::ACTION_UPDATE_RECEIVED_QTY,
        ] {
            assert!(
                spec::FLOW_INSTANCE_EXCLUDED_ACTIONS.contains(&code),
                "运行时排除清单漏了 {code}：它与业务桥的专用实现重复落账"
            );
            assert!(
                spec::FLOW_INSTANCE_FORBIDDEN_ACTIONS.contains(&code),
                "配置期禁令漏了 {code}：能配得上但永远不执行"
            );
        }
        // 通知 / 建任务不能被排除：本触发点的唯一落点就是它们
        for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
            assert!(
                !spec::FLOW_INSTANCE_EXCLUDED_ACTIONS.contains(&code),
                "{code} 被排除等于接线了但永远不响"
            );
        }
    }

    /// 配置期禁令必须给出**说人话的理由**（配置页 / 保存接口都读这一份文案）。
    #[test]
    fn forbidden_reason_is_human_readable() {
        let reason = spec::forbidden_action_reason(
            spec::TRIGGER_FLOW_INSTANCE,
            spec::ACTION_GENERATE_VOUCHER,
        )
        .expect("FLOW_INSTANCE + GENERATE_VOUCHER 必须被禁");
        assert!(
            reason.contains("两遍") && reason.contains("凭证"),
            "理由要点明「同一笔账落两遍」并给出后果：{reason}"
        );
        assert!(
            spec::forbidden_action_reason(spec::TRIGGER_FLOW_INSTANCE, spec::ACTION_SEND_NOTICE)
                .is_none(),
            "SEND_NOTICE 不能被禁"
        );
        assert!(
            spec::forbidden_action_reason(spec::TRIGGER_TASK, spec::ACTION_SEND_NOTICE).is_none(),
            "其它触发点不受 FLOW_INSTANCE 禁令影响"
        );
    }
}
