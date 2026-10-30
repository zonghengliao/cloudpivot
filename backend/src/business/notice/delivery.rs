//! 通道投递（批次 A · A6）。
//!
//! # 本模块回答一个问题：一条通知该往哪些通道投、每个通道的状态怎么记
//!
//! | 通道 | 掩码 | 粒度 | 已配置 | 未配置 |
//! |------|------|------|--------|--------|
//! | `INBOX` 站内信 | 1 | 每接收人一行（`sys_notice_receiver`） | `DELIVERED`（**必投**） | 不会发生 |
//! | `EMAIL` 邮件 | 2 | 每接收人一行（取 `sys_user.email`） | `PENDING` | `SKIPPED` |
//! | `WEBHOOK` | 4 | 每条通知一行（`sys_config` 的 `notice.webhook.url`） | `PENDING` | `SKIPPED` |
//! | `SMS` 短信 | 8 | 每接收人一行（取 `sys_user.mobile`） | `PENDING` | `SKIPPED` |
//!
//! **未配置的通道记 `SKIPPED`，不是 `FAILED`**：`FAILED` 的语义是「尝试投递了但失败」
//! （要重试），而本批根本没有投递能力（邮件 / 短信 / Webhook 发送器留到后续批次），
//! 记 `FAILED` 会让运维误以为通道坏了并触发重试风暴。这条语义写死在 [`status_for`] 里。
//!
//! # 通道计划（[`plan_channels`]）
//!
//! 生效掩码 = **（模板 `channels` 列的通道 ∪ 调用方显式指定的通道）| 站内信**：
//!
//! - 模板只配 `INBOX`、调用方显式要 `EMAIL` → `INBOX | EMAIL`：站内信照投，邮件落
//!   `SKIPPED` 投递记录（**不报错、不影响站内信送达**）；
//! - 调用方没指定（0）→ 至少 `INBOX`（站内信是「必投」通道，任何通知都得能在站内看到）。
//!
//! 与 A5 的差异（**行为变更，已写进验证证据**）：A5 是「调用方指定了就用调用方的，
//! 模板通道只在调用方未指定时生效」（二选一）；A6 改成**并集**——模板声明的通道
//! 不会因为调用方多写一个通道而被丢掉，符合架构文档 §4.6「按 `template.channels` 投递」。

use nik::prelude::*;

use crate::business::notice::ChannelState;
use crate::business::notice::dto::{
    CHANNEL_EMAIL, CHANNEL_INBOX, CHANNEL_MAX, CHANNEL_SMS, CHANNEL_WEBHOOK,
};
use crate::business::notice::model::{self, ChannelSettings, Executor};

/// 站内信状态：必投且即落库，恒为 `DELIVERED`。
pub const STATUS_DELIVERED: &str = "DELIVERED";

/// 通道已配置、等待发送器（后续批次）投递。
pub const STATUS_PENDING: &str = "PENDING";

/// 通道未配置（**不是失败**）。
pub const STATUS_SKIPPED: &str = "SKIPPED";

/// `FAILED` 只允许由「真的尝试发送并失败」写入，本模块**永不**返回它。
pub const STATUS_FAILED: &str = "FAILED";

/// 通道投递粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    /// 站内信：扇出时已落 `sys_notice_receiver`，只回一条汇总状态。
    Inbox,
    /// 按接收人投递（邮件 / 短信）：`sys_notice_delivery` 每接收人一行。
    PerReceiver,
    /// 通知级投递（Webhook）：一条通知一行。
    NoticeLevel,
}

/// 通道静态定义（位、名字、粒度）。
#[derive(Debug, Clone, Copy)]
pub struct ChannelSpec {
    /// 通道名（落库值，与迁移 CHECK 逐字一致）。
    pub name: &'static str,
    /// 掩码位。
    pub bit: u16,
    /// 投递粒度。
    pub kind: ChannelKind,
}

/// 全部通道（顺序即返回给前端的顺序：站内信永远第一）。
pub const CHANNELS: [ChannelSpec; 4] = [
    ChannelSpec {
        name: "INBOX",
        bit: CHANNEL_INBOX,
        kind: ChannelKind::Inbox,
    },
    ChannelSpec {
        name: "EMAIL",
        bit: CHANNEL_EMAIL,
        kind: ChannelKind::PerReceiver,
    },
    ChannelSpec {
        name: "WEBHOOK",
        bit: CHANNEL_WEBHOOK,
        kind: ChannelKind::NoticeLevel,
    },
    ChannelSpec {
        name: "SMS",
        bit: CHANNEL_SMS,
        kind: ChannelKind::PerReceiver,
    },
];

/// 计算生效通道掩码（模板通道 ∪ 调用方通道，且站内信必投）。
///
/// # 参数
///
/// - `template_mask`：模板 `channels` 列解析出的掩码；未使用模板时传 `None`。
/// - `requested_mask`：调用方显式指定的掩码（0 表示未指定）。
///
/// # 返回值
///
/// 生效掩码，`CHANNEL_INBOX` 位恒为 1。
pub fn plan_channels(template_mask: Option<u16>, requested_mask: u16) -> u16 {
    let mut mask = requested_mask & CHANNEL_MAX;
    if let Some(template) = template_mask {
        mask |= template & CHANNEL_MAX;
    }
    mask | CHANNEL_INBOX
}

/// 通道状态：已配置 `PENDING`，未配置 `SKIPPED`（**永不 `FAILED`**）。
///
/// # 参数
///
/// - `configured`：该通道是否已配置。
///
/// # 返回值
///
/// 状态字符串（落库值）。
pub fn status_for(configured: bool) -> &'static str {
    if configured {
        STATUS_PENDING
    } else {
        STATUS_SKIPPED
    }
}

/// 按生效掩码逐通道写投递记录（**在 `dispatch` 的事务内调用**）。
///
/// # 参数
///
/// - `ex`：执行器（事务内传 [`Executor::Tx`]，与通知落库同事务）。
/// - `notice_id`：通知主键。
/// - `mask`：生效通道掩码（见 [`plan_channels`]）。
/// - `settings`：通道配置（是否已配置 + 真实 Webhook 地址）。
/// - `inbox_rows`：站内信扇出的接收行数（`INBOX` 通道的 `rows`）。
///
/// # 返回值
///
/// 每个通道一条 [`ChannelState`]（顺序与 [`CHANNELS`] 一致）。
///
/// # 异常情况
///
/// 数据库异常向上抛（调用方在事务内会整体回滚，不会留下「通知存在但通道记录缺失」）。
pub async fn deliver(
    ex: &Executor<'_>,
    notice_id: &uuid::Uuid,
    mask: u16,
    settings: &ChannelSettings,
    inbox_rows: u64,
) -> DbResult<Vec<ChannelState>> {
    let mut states: Vec<ChannelState> = Vec::with_capacity(CHANNELS.len());
    for spec in CHANNELS.iter() {
        if mask & spec.bit == 0 {
            continue;
        }
        let configured = settings.config.is_configured(spec.name);
        match spec.kind {
            ChannelKind::Inbox => states.push(ChannelState {
                channel: spec.name.to_string(),
                status: STATUS_DELIVERED.to_string(),
                rows: inbox_rows,
            }),
            ChannelKind::PerReceiver => {
                let rows =
                    model::insert_receiver_deliveries(ex, notice_id, spec.name, configured).await?;
                states.push(ChannelState {
                    channel: spec.name.to_string(),
                    status: status_for(configured).to_string(),
                    rows,
                });
            }
            ChannelKind::NoticeLevel => {
                let rows = model::insert_notice_delivery(
                    ex,
                    notice_id,
                    spec.name,
                    &settings.webhook_url,
                    configured,
                )
                .await?;
                states.push(ChannelState {
                    channel: spec.name.to_string(),
                    status: status_for(configured).to_string(),
                    rows,
                });
            }
        }
    }
    Ok(states)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 站内信必投：调用方什么都不指定也有 `INBOX`。
    #[test]
    fn inbox_is_always_planned() {
        assert_eq!(plan_channels(None, 0), CHANNEL_INBOX);
        assert_eq!(plan_channels(Some(0), 0), CHANNEL_INBOX);
    }

    /// **硬指标 ③ 的计划侧**：模板只配 `INBOX`，调用方投 `EMAIL` → 两者都在计划里
    /// （站内信照投，邮件按「未配置」落 `SKIPPED`，绝不因为邮件通道没配而拒发整条通知）。
    #[test]
    fn template_inbox_plus_requested_email_keeps_both() {
        let mask = plan_channels(Some(CHANNEL_INBOX), CHANNEL_EMAIL);
        assert_eq!(mask & CHANNEL_INBOX, CHANNEL_INBOX, "站内信必投");
        assert_eq!(
            mask & CHANNEL_EMAIL,
            CHANNEL_EMAIL,
            "调用方要的邮件要在计划里"
        );
        assert_eq!(mask, CHANNEL_INBOX | CHANNEL_EMAIL);
    }

    /// 并集语义：模板通道不会因为调用方指定了别的通道而被丢掉。
    #[test]
    fn template_channels_are_unioned_not_replaced() {
        let mask = plan_channels(Some(CHANNEL_WEBHOOK | CHANNEL_SMS), CHANNEL_EMAIL);
        assert_eq!(
            mask,
            CHANNEL_INBOX | CHANNEL_EMAIL | CHANNEL_WEBHOOK | CHANNEL_SMS
        );
    }

    /// 越界位被裁掉（掩码只会来自 `ensure_channel_mask` 校验过的值，这里兜底）。
    #[test]
    fn out_of_range_bits_are_masked_out() {
        assert_eq!(plan_channels(Some(0xFFFF), 0xFFFF), CHANNEL_MAX);
    }

    /// **硬指标 ③**：未配置 → `SKIPPED`，且绝不等同 `FAILED`（语义不同，`FAILED` 会触发重试）。
    #[test]
    fn unconfigured_channel_is_skipped_never_failed() {
        assert_eq!(status_for(false), STATUS_SKIPPED);
        assert_eq!(status_for(true), STATUS_PENDING);
        assert_ne!(status_for(false), STATUS_FAILED);
        assert_ne!(STATUS_SKIPPED, STATUS_FAILED);
    }

    /// 通道表与迁移 CHECK 一致：4 个通道、位互不重叠、并集等于 `CHANNEL_MAX`。
    #[test]
    fn channel_table_covers_all_bits_exactly_once() {
        assert_eq!(CHANNELS.len(), 4);
        let mut union = 0u16;
        for spec in CHANNELS.iter() {
            assert_eq!(union & spec.bit, 0, "通道位重复：{}", spec.name);
            union |= spec.bit;
        }
        assert_eq!(union, CHANNEL_MAX);
        assert_eq!(CHANNELS[0].name, "INBOX", "站内信必须是第一个通道");
        assert_eq!(CHANNELS[0].kind, ChannelKind::Inbox);
    }

    /// 通道名必须与 `sys_notice_delivery.channel` 的 CHECK 逐字一致（大写）。
    #[test]
    fn channel_names_match_check_constraint() {
        let names: Vec<&str> = CHANNELS.iter().map(|spec| spec.name).collect();
        assert_eq!(names, vec!["INBOX", "EMAIL", "WEBHOOK", "SMS"]);
        for spec in CHANNELS.iter() {
            assert_eq!(spec.name, spec.name.to_uppercase(), "通道名必须大写");
        }
    }
}
