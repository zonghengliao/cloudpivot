//! 审批通知（批次 D · D3c）：4 个模板 + 强制送达分级。
//!
//! # 为什么单独一个文件，而不是散在引擎里拼标题正文
//!
//! 引擎（`flow/engine.rs`）每流转一步就要发通知，标题正文如果直接写在引擎里：
//!
//! 1. **文案不可改**——运营想说「待你审批（超 2 小时未处理会自动提醒）」就得改代码发版；
//! 2. **变量口径漂移**——同一条待办在「发起」「转办」「催办」里拼出的单号/节点名格式
//!    各不相同，前端与审计对不上；
//! 3. **强制/尊重偏好的判断无处收口**——`force` 是安全语义（漏一条待办 = 单子卡死），
//!    绝不能靠每个调用点自觉。
//!
//! 所以本文件是审批通知的**唯一出口**：模板编码、变量表、`force` 分级、幂等键形状
//! 全部在这里定，引擎/定时任务/回调只提供上下文。
//!
//! # 复用 `notice`，不另造通道（红线）
//!
//! 走的是 `business::notice::dispatch` + `sys_notice_template` 那套现成机制：
//! 模板渲染（`notice/render.rs` 的 `{{var}}`）、收件人扇出（`notice/fanout.rs`）、
//! 偏好过滤、幂等键、投递记录——一个都不新写。本文件的产物就是一份 [`NoticeDraft`]。
//! 好处：模板改了不用发版，通知中心/已读/投递失败重试全部免费继承。
//!
//! # 四个模板与 `force` 分级（架构 §1.4 + 修正 8）
//!
//! | 模板 | 触发 | `force` | 理由 |
//! |------|------|---------|------|
//! | [`TEMPLATE_TODO`] | 待你审批 | `true` | 漏了这张待办 = 审批单永久卡死，是**事故**不是打扰 |
//! | [`TEMPLATE_RESULT`] | 通过 / 驳回 | `true` | 发起人必须知道结果（驳回还要去改），漏了会重复追问 |
//! | [`TEMPLATE_CC`] | 抄送你 | `false` | 抄送是「知情」不是「必办」，关掉站内信的人应当可以不看 |
//! | [`TEMPLATE_URGE`] | 催办 / 超时 | `true` | 催办本身就是「你已经漏了」的补救，再尊重偏好就白催 |
//!
//! 一条铁律：`force = true` 只表示**跳过 `notify.inbox` 偏好**，不表示跳过退订/停用账号——
//! 收件人集合由 `notice::fanout` 过滤（`status = 1` 且未软删），本文件不自己算人。
//!
//! # 通知失败不回滚业务
//!
//! [`dispatch`] 吞掉错误只打 `tracing::warn!`（与 `task/collab.rs` 同款）：
//! 审批已经落库、待办已经建好，因为一封站内信写失败就把「同意」回滚掉，
//! 用户点第二次还会失败第二次。

use alun::prelude::*;
use alun::uuid::Uuid;

use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};

/// 待办通知模板（待你审批）。
pub const TEMPLATE_TODO: &str = "FLOW_TODO";
/// 结果通知模板（通过 / 驳回 / 撤回）。
pub const TEMPLATE_RESULT: &str = "FLOW_RESULT";
/// 抄送通知模板。
pub const TEMPLATE_CC: &str = "FLOW_CC";
/// 催办通知模板。
pub const TEMPLATE_URGE: &str = "FLOW_URGE";

/// 业务类型（写进 `sys_notice.biz_type`，通知中心按它分组跳转）。
pub const BIZ_TYPE: &str = "workflow";

/// 跳转链接前缀（前端路由 `/flow/instance/:id`，架构 §11.7 第 2 页）。
pub const LINK_PREFIX: &str = "/flow/instance/";

/// 通知事件（模板 + 送达强度 + 通知类型三者一一绑定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlowEvent {
    /// 待你审批。
    Todo,
    /// 你的单子有结果了。
    Result,
    /// 抄送你。
    Cc,
    /// 催办。
    Urge,
}

impl FlowEvent {
    /// 四个事件（顺序即 [`FlowEvent::template_code`] 的书写顺序）。
    pub const ALL: &'static [FlowEvent] = &[
        FlowEvent::Todo,
        FlowEvent::Result,
        FlowEvent::Cc,
        FlowEvent::Urge,
    ];

    /// 对应模板编码（必须与迁移种子里的 `sys_notice_template.template_code` 逐字一致）。
    pub fn template_code(self) -> &'static str {
        match self {
            FlowEvent::Todo => TEMPLATE_TODO,
            FlowEvent::Result => TEMPLATE_RESULT,
            FlowEvent::Cc => TEMPLATE_CC,
            FlowEvent::Urge => TEMPLATE_URGE,
        }
    }

    /// 是否强制送达（跳过 `notify.inbox` 偏好）。
    ///
    /// **只有抄送可以尊重偏好**：其余三个漏了都有业务后果（卡单 / 不知道结果 / 催了白催）。
    pub fn force(self) -> bool {
        match self {
            FlowEvent::Todo | FlowEvent::Result | FlowEvent::Urge => true,
            FlowEvent::Cc => false,
        }
    }

    /// 通知类型（`TODO` 走待办角标，`WORKFLOW` 走流程消息）。
    pub fn notice_type(self) -> NoticeType {
        match self {
            FlowEvent::Todo | FlowEvent::Urge => NoticeType::Todo,
            FlowEvent::Result | FlowEvent::Cc => NoticeType::Workflow,
        }
    }

    /// 默认级别（模板里的 `default_level` 与此一致；调用方显式改了以调用方为准）。
    pub fn level(self) -> NoticeLevel {
        match self {
            FlowEvent::Todo | FlowEvent::Cc => NoticeLevel::Info,
            FlowEvent::Result | FlowEvent::Urge => NoticeLevel::Warn,
        }
    }

    /// 幂等键里的事件段（重试同一事件必须命中同一个键）。
    pub fn code(self) -> &'static str {
        match self {
            FlowEvent::Todo => "TODO",
            FlowEvent::Result => "RESULT",
            FlowEvent::Cc => "CC",
            FlowEvent::Urge => "URGE",
        }
    }
}

/// 一条审批通知的上下文（**引擎/定时任务提供，本模块不查库**）。
///
/// 字段全是审批场景里「用户需要知道的三件事」的原料：
/// **流程名**（`flow_name`）、**实例号**（`instance_no`）、**当前节点名**（`node_name`）。
#[derive(Debug, Clone)]
pub struct FlowNotice<'a> {
    /// 事件。
    pub event: FlowEvent,
    /// 实例主键（跳转链接 + `biz_id` + 幂等键）。
    pub instance_id: Uuid,
    /// 实例号（`FLW-202610-000123`）。
    pub instance_no: &'a str,
    /// 流程名（「任务验收」）。
    pub flow_name: &'a str,
    /// 单据标题（「完成批次 D 架构文档」）。
    pub title: &'a str,
    /// 当前节点 key（幂等键的一部分，一轮里唯一）。
    pub node_key: &'a str,
    /// 当前节点名（「主管审批」）。
    pub node_name: &'a str,
    /// 轮次（退回重走 → round+1，幂等键必须跟着变）。
    pub round: i32,
    /// 发起人姓名。
    pub initiator_name: &'a str,
    /// 结果事件用：`Some(true)` 通过 / `Some(false)` 驳回；其余事件 `None`。
    pub result: Option<bool>,
    /// 审批意见（可为空串；**不可为 `None`**，渲染器缺变量会 422）。
    pub comment: &'a str,
    /// 催办事件用：第几次提醒。
    pub urge_count: i32,
    /// 收件人（**只用 `User`**：审批人已在 `biz_flow_task` 冻结成具体的人，
    /// 不再让通知模块二次展开角色，架构 §1.4）。
    pub recipients: Vec<Uuid>,
}

impl<'a> FlowNotice<'a> {
    /// 构造一条通知（结果 / 催办专有字段给默认值，用 `with_*` 覆盖）。
    pub fn new(
        event: FlowEvent,
        instance_id: Uuid,
        instance_no: &'a str,
        flow_name: &'a str,
        title: &'a str,
        node_key: &'a str,
        node_name: &'a str,
    ) -> Self {
        FlowNotice {
            event,
            instance_id,
            instance_no,
            flow_name,
            title,
            node_key,
            node_name,
            round: 1,
            initiator_name: "",
            result: None,
            comment: "",
            urge_count: 1,
            recipients: Vec::new(),
        }
    }

    /// 设置收件人（调用方负责去重：同一人可能同时是负责人/创建人/协作人）。
    pub fn with_recipients(mut self, recipients: Vec<Uuid>) -> Self {
        self.recipients = recipients;
        self
    }

    /// 设置轮次。
    pub fn with_round(mut self, round: i32) -> Self {
        self.round = round.max(1);
        self
    }

    /// 设置发起人姓名。
    pub fn with_initiator(mut self, name: &'a str) -> Self {
        self.initiator_name = name;
        self
    }

    /// 设置审批结果（仅 `Result` 事件有意义）。
    pub fn with_result(mut self, passed: bool, comment: &'a str) -> Self {
        self.result = Some(passed);
        self.comment = comment;
        self
    }

    /// 设置催办次数（仅 `Urge` 事件有意义）。
    pub fn with_urge_count(mut self, count: i32) -> Self {
        self.urge_count = count.max(1);
        self
    }

    /// 结果文案（模板变量 `resultText`）。
    fn result_text(&self) -> &'static str {
        match self.result {
            Some(true) => "通过",
            Some(false) => "驳回",
            None => "",
        }
    }

    /// 模板变量表。
    ///
    /// **八个键恒定存在**（不用的给空串）：渲染器对缺失变量是 `422 + 60023` 硬失败，
    /// 「这个事件用不到所以不传」会让模板改一个字就整条通知发不出去。
    pub fn template_vars(&self) -> serde_json::Value {
        serde_json::json!({
            "flowName": self.flow_name,
            "instanceNo": self.instance_no,
            "nodeName": self.node_name,
            "initiatorName": self.initiator_name,
            "title": self.title,
            "resultText": self.result_text(),
            "comment": self.comment,
            "urgeCount": self.urge_count.to_string(),
        })
    }

    /// 幂等键：`FLOW:<事件>:<实例>:<节点>:<轮次>`。
    ///
    /// 不含收件人：一条事件 = 一次 `dispatch`（`NoticeTarget::User` 多收件人），
    /// 重试 / 网络重发命中同一个键，不会给同一个人发两遍。
    pub fn idempotency_key(&self) -> String {
        format!(
            "FLOW:{}:{}:{}:{}",
            self.event.code(),
            self.instance_id,
            self.node_key,
            self.round
        )
    }

    /// 跳转链接（前端审批详情页）。
    pub fn link_url(&self) -> String {
        format!("{LINK_PREFIX}{}", self.instance_id)
    }

    /// 组装成 `notice::dispatch` 的入参。
    pub fn draft(&self) -> NoticeDraft {
        let mut draft = NoticeDraft::text(
            self.event.notice_type(),
            self.event.level(),
            String::new(),
            String::new(),
            NoticeTarget::User(self.recipients.clone()),
        );
        draft.template_code = Some(self.event.template_code().to_string());
        draft.template_vars = self.template_vars();
        draft.link_url = Some(self.link_url());
        draft.biz_type = Some(BIZ_TYPE.to_string());
        draft.biz_id = Some(self.instance_id.to_string());
        draft.idempotency_key = Some(self.idempotency_key());
        draft.sender_name = Some("工作流引擎".to_string());
        // 分级在这里收口：Todo/Result/Urge 强制，Cc 尊重偏好（见 FlowEvent::force）
        draft.force = self.event.force();
        draft
    }

    /// 发送（**尽力而为**：失败只告警，不向上抛，绝不回滚业务事务）。
    ///
    /// # 返回值
    ///
    /// `true` = 已发出（或命中幂等键）；`false` = 发送失败 / 没有收件人（已告警）。
    pub async fn dispatch(&self) -> bool {
        if self.recipients.is_empty() {
            return false;
        }
        match notice::dispatch(self.draft()).await {
            Ok(result) => {
                tracing::debug!(
                    notice_id = %result.notice_id,
                    event = self.event.code(),
                    instance = %self.instance_id,
                    receivers = result.receiver_count,
                    "审批通知已发送"
                );
                true
            }
            Err(error) => {
                tracing::warn!(
                    event = self.event.code(),
                    instance = %self.instance_id,
                    node = self.node_key,
                    error = %error,
                    "审批通知发送失败（不回滚业务）"
                );
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn notice(event: FlowEvent) -> FlowNotice<'static> {
        FlowNotice::new(
            event,
            uuid(0x1234),
            "FLW-202610-000123",
            "任务验收",
            "完成批次 D 架构文档",
            "n_leader",
            "主管审批",
        )
        .with_initiator("武鹏")
        .with_recipients(vec![uuid(1), uuid(2)])
    }

    /// 强制分级：待办/结果/催办强制，抄送尊重偏好（卡面红线）。
    #[test]
    fn force_policy() {
        assert!(FlowEvent::Todo.force(), "FLOW_TODO 必须 force=true");
        assert!(FlowEvent::Result.force());
        assert!(FlowEvent::Urge.force());
        assert!(!FlowEvent::Cc.force(), "FLOW_CC 必须尊重偏好");
        for event in FlowEvent::ALL {
            assert_eq!(notice(*event).draft().force, event.force());
        }
    }

    /// 模板编码必须能过库侧 CHECK：`^[A-Za-z][A-Za-z0-9_]{1,63}$`（逐字符校验）。
    #[test]
    fn template_codes_fit_db_check() {
        let mut seen: Vec<&str> = Vec::new();
        for event in FlowEvent::ALL {
            let code = event.template_code();
            let mut chars = code.chars();
            let first = chars.next().expect("模板编码不能为空");
            assert!(first.is_ascii_alphabetic(), "{code} 首字符必须字母");
            assert!(
                code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{code} 含非法字符"
            );
            assert!((2..=64).contains(&code.chars().count()), "{code} 长度越界");
            assert!(!seen.contains(&code), "{code} 重复");
            seen.push(code);
        }
        assert_eq!(
            seen,
            vec!["FLOW_TODO", "FLOW_RESULT", "FLOW_CC", "FLOW_URGE"]
        );
    }

    /// 待办通知：走模板、强制、含流程名 + 实例号 + 当前节点名、链到实例详情。
    #[test]
    fn todo_draft_carries_context() {
        let draft = notice(FlowEvent::Todo).draft();
        assert_eq!(draft.template_code.as_deref(), Some(TEMPLATE_TODO));
        assert!(draft.force);
        let vars = &draft.template_vars;
        assert_eq!(vars["flowName"], "任务验收");
        assert_eq!(vars["instanceNo"], "FLW-202610-000123");
        assert_eq!(vars["nodeName"], "主管审批");
        assert_eq!(draft.biz_type.as_deref(), Some(BIZ_TYPE));
        assert_eq!(
            draft.biz_id.as_deref(),
            Some("00000000-0000-0000-0000-000000001234")
        );
        assert_eq!(
            draft.link_url.as_deref(),
            Some("/flow/instance/00000000-0000-0000-0000-000000001234")
        );
        assert!(matches!(draft.target, NoticeTarget::User(_)));
        if let NoticeTarget::User(users) = &draft.target {
            assert_eq!(users.len(), 2);
        }
    }

    /// 变量表八个键恒定存在（渲染器缺变量是 422 硬失败）。
    #[test]
    fn template_vars_are_complete() {
        for event in FlowEvent::ALL {
            let vars = notice(*event).template_vars();
            for key in [
                "flowName",
                "instanceNo",
                "nodeName",
                "initiatorName",
                "title",
                "resultText",
                "comment",
                "urgeCount",
            ] {
                assert!(vars.get(key).is_some(), "{} 缺变量 {key}", event.code());
                assert!(!vars[key].is_null(), "{} 的 {key} 不能是 null", event.code());
            }
        }
    }

    /// 结果通知：通过/驳回文案不同，级别 WARN 保底。
    #[test]
    fn result_text_reflects_outcome() {
        let passed = notice(FlowEvent::Result).with_result(true, "同意");
        assert_eq!(passed.template_vars()["resultText"], "通过");
        let rejected = notice(FlowEvent::Result).with_result(false, "凭证缺失");
        assert_eq!(rejected.template_vars()["resultText"], "驳回");
        assert_eq!(rejected.template_vars()["comment"], "凭证缺失");
        assert!(rejected.draft().force);
    }

    /// 幂等键：同事件同轮次稳定（重试不重发），轮次/节点变则变（退回重走要重发）。
    #[test]
    fn idempotency_key_is_scoped() {
        let base = notice(FlowEvent::Todo);
        let again = notice(FlowEvent::Todo);
        assert_eq!(base.idempotency_key(), again.idempotency_key());
        assert_ne!(
            base.idempotency_key(),
            base.clone().with_round(2).idempotency_key()
        );
        assert_ne!(base.idempotency_key(), notice(FlowEvent::Urge).idempotency_key());
        assert!(base.idempotency_key().starts_with("FLOW:TODO:"));
        // 幂等键长度：`sys_notice.idempotency_key` 一列最多 128 字符
        assert!(base.idempotency_key().chars().count() <= 128);
    }
}
