//! 告警去重与合并（P1-08）：同一 `(source, title)` 在窗口内只落**一条**站内消息。
//!
//! # 要解决的问题
//!
//! P1-07 把「巡检告警 → 站内消息」打通了，但接口是「来一次落一条」：磁盘越界在
//! cron 里每轮都命中，超管的消息中心就被同一条告警刷屏，真正的第二条异常反而被
//! 埋掉。本模块给出**滑动窗口**去重：
//!
//! ```text
//! t0        发「磁盘 92%」→ 落 1 条通知（窗口开始）
//! t0+5s     同 (source,title) → 不落库，repeat_count = 1
//! t0+10s    …                                      = 2
//! t0+15s    …                                      = 3
//! t0+20s    …                                      = 4
//! t0+11min  同 (source,title) → 落第 2 条通知，正文追加「近 10 分钟已重复 4 次」
//! ```
//!
//! # 为什么判定是纯函数
//!
//! 「发还是不发」是整个卡的核心判据，必须能**脱库**、**脱时钟**单测（卡面要求
//! 「等 11 分钟（用 mock 时间）」）。所以：
//!
//! - [`evaluate`]：纯函数，入参是「当前时刻 + 上次状态 + 窗口长度」，出参是
//!   「这一次该发布还是该抑制」+「该写回什么状态」；
//! - 持久化在 [`crate::business::notice::model::claim_alert_dedup`]（一个事务里
//!   行锁 + 读写），SQL 只负责搬运 [`DedupState`]，不重复实现判据。
//!
//! 两侧判据只有一份，测试与线上跑的是**同一段代码**。
//!
//! # 状态语义（一列都不能随便改，见 `V0404__notice_alert_dedup` 的注释）
//!
//! | 列 | 语义 |
//! |----|------|
//! | `window_start` | 本窗口发布那条通知的时刻（窗口内抑制时不动） |
//! | `repeat_count` | 本窗口**被合并掉**的次数（不含首发） |
//! | `notice_id` | 本窗口实际落下的通知；`NULL` = 还没发成功 |
//!
//! 关键判据：**只有「本窗口已经成功发布过」才允许抑制**（`notice_id IS NOT NULL`）。
//! 若上次发布失败（dispatch 抛错 / 库抖动 / 超管角色缺失），`notice_id` 仍是 `NULL`，
//! 下一次调用会**立刻重发**而不是把后面 10 分钟的告警全部吞掉 ——
//! 这是「事务提交后失败必须有补偿、不许留死单」那条铁律（AGENTS.md 约定 11）
//! 在告警链路上的落地：宁可多响一次，绝不少响。
//!
//! # 与 `sys_notice.idempotency_key` 的分工
//!
//! - `idempotency_key`：**永久**幂等（业务事件唯一键），重放返回首次结果；
//! - 本模块：**滑动窗口**（默认 10 分钟），窗口过后必须能再发。
//!
//! 两者叠加的顺序在 [`crate::business::notice::handler::deliver_internal_alert_at`]：
//! 先查幂等键（命中就直接返回首次结果，不进去重窗口），再去重窗口。

use crate::chrono::{DateTime, Duration, Utc};
use nik::uuid::Uuid;

/// 默认去重窗口（秒）：卡面口径「同一 `(source, title)` 在 10 分钟内只发一次」。
pub const DEFAULT_WINDOW_SECONDS: i64 = 600;

/// 窗口长度的环境变量名（运维可调；未设置 / 非法值一律回落 [`DEFAULT_WINDOW_SECONDS`]）。
///
/// 为什么可调：验收脚本要在秒级把「窗口过期后补发」这一段跑通（真等 11 分钟不现实），
/// 演练时把窗口压到 2 秒即可端到端验证同一条 SQL。生产不设这个变量。
pub const WINDOW_ENV: &str = "NIK_ALERT_DEDUP_WINDOW_SECONDS";

/// 窗口下限（秒）。禁止 0：窗口为 0 等于「去重关掉了」，那应该是删掉调用而不是悄悄失效。
pub const MIN_WINDOW_SECONDS: i64 = 1;

/// 窗口上限（秒）：一天。再长就不是"去重"而是"静音"了。
pub const MAX_WINDOW_SECONDS: i64 = 86_400;

/// 去重窗口的持久状态（一行 = 一个 `(source, title)`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupState {
    /// 当前窗口起点（本窗口发布那条通知的时刻）。
    pub window_start: DateTime<Utc>,
    /// 本窗口内被合并掉的次数（不含首发）。
    pub repeat_count: i32,
    /// 本窗口实际落下的通知主键；`None` = 本窗口还没发成功（下次必须重发）。
    pub notice_id: Option<Uuid>,
}

/// 单次调用的处置结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertAction {
    /// 需要落一条新通知。
    Publish {
        /// 上一窗口被合并掉的次数（> 0 时写进正文「近 N 分钟已重复 X 次」）。
        merged: i32,
    },
    /// 窗口内重复：不落库，只累加计数。
    Suppress {
        /// 本窗口累计合并次数（**含本次**，即「第几次被合并」）。
        merged: i32,
    },
}

impl AlertAction {
    /// 本次是否要落库（抑制时 `false`）。
    pub fn is_publish(&self) -> bool {
        matches!(self, AlertAction::Publish { .. })
    }

    /// 与本次处置关联的合并次数（发布时是上一窗口的，抑制时是本窗口累计的）。
    pub fn merged(&self) -> i32 {
        match self {
            AlertAction::Publish { merged } | AlertAction::Suppress { merged } => *merged,
        }
    }
}

/// 一次判定的完整结论：**该做什么** + **该写回什么状态**。
///
/// 两者出自同一次计算，调用方不可能只写一半（比如"记了抑制但没加计数"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupVerdict {
    /// 本次处置。
    pub action: AlertAction,
    /// 判定之后应落库的状态。
    pub next: DedupState,
}

/// 窗口长度（读环境变量，非法值回落默认）。
///
/// # 返回值
///
/// 合法区间内的窗口；`NIK_ALERT_DEDUP_WINDOW_SECONDS` 未设置 / 不是整数 / 越界
/// （< [`MIN_WINDOW_SECONDS`] 或 > [`MAX_WINDOW_SECONDS`]）时回落 [`DEFAULT_WINDOW_SECONDS`]。
pub fn window() -> Duration {
    window_of(std::env::var(WINDOW_ENV).ok().as_deref())
}

/// [`window`] 的纯函数版本（便于单测，不读环境变量）。
///
/// # 参数
///
/// - `raw`：环境变量的原值（`None` = 未设置）。
pub fn window_of(raw: Option<&str>) -> Duration {
    let seconds = raw
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|seconds| (MIN_WINDOW_SECONDS..=MAX_WINDOW_SECONDS).contains(seconds))
        .unwrap_or(DEFAULT_WINDOW_SECONDS);
    Duration::seconds(seconds)
}

/// 窗口长度的人话（写进告警正文）：整分钟用「N 分钟」，否则用「N 秒」。
///
/// 默认窗口 600 秒 → `"10 分钟"`，正文里读作「近 10 分钟已重复 4 次」。
pub fn humanize_window(window: Duration) -> String {
    let seconds = window.num_seconds();
    if seconds > 0 && seconds % 60 == 0 {
        format!("{} 分钟", seconds / 60)
    } else {
        format!("{seconds} 秒")
    }
}

/// 重复次数的提示语（不含内容前缀）：`近 10 分钟已重复 4 次`。
pub fn repeat_note(merged: i32, window: Duration) -> String {
    format!("近 {}已重复 {} 次", humanize_window(window), merged)
}

/// 把「已合并 N 次」追加到正文末尾；`merged <= 0` 时**原样返回**（首发不加噪音）。
///
/// # 参数
///
/// - `content`：调用方给的正文（调用方已 trim，非空由 handler 保证）；
/// - `merged`：上一窗口被合并掉的次数；
/// - `window`：窗口长度（决定文案里写「10 分钟」还是「2 秒」）。
pub fn with_repeat_note(content: &str, merged: i32, window: Duration) -> String {
    if merged <= 0 {
        return content.to_string();
    }
    format!(
        "{content}\n\n—— {}（同类告警已在窗口内合并，本条为合并后首次推送）",
        repeat_note(merged, window)
    )
}

/// 核心判据：给定「现在」与「上次状态」，决定这次发布还是抑制，以及该写回什么状态。
///
/// # 参数
///
/// - `now`：本次调用的时刻（生产传 `Utc::now()`，测试传 mock 时间）；
/// - `prev`：库里已有的窗口状态（`None` = 这个 `(source, title)` 第一次出现）；
/// - `window`：窗口长度（默认 [`DEFAULT_WINDOW_SECONDS`]）。
///
/// # 处置规则（顺序即优先级）
///
/// 1. 没有历史状态 → **发布**（`merged = 0`）；
/// 2. 历史窗口里 `notice_id IS NULL`（上次发布失败） → **发布**：
///    宁可多响一次，也不能因为一次失败把后面整个窗口的告警吞掉；
/// 3. `now` 落在窗口内（`0 <= now - window_start < window`） → **抑制**，
///    `repeat_count + 1`，`window_start` / `notice_id` 保持不变；
/// 4. 否则（窗口已过，或系统时钟被往回拨过 → 差值 < 0） → **发布**，
///    并把**上一窗口**的 `repeat_count` 作为 `merged` 带出去；状态重置为新窗口。
///
/// 时钟回拨单独处理成「发布」：抑制依赖 `now - window_start < window`，
/// 若时钟跳回过去，`差值 < 0` 会让抑制窗口变成"无限长"，一直到时钟追上为止 ——
/// 那正好是要避免的静默期。
///
/// # 返回值
///
/// [`DedupVerdict`]：`action` 决定这次要不要落库，`next` 是该写回库的状态。
pub fn evaluate(now: DateTime<Utc>, prev: Option<&DedupState>, window: Duration) -> DedupVerdict {
    let Some(prev) = prev else {
        return published(now, 0);
    };
    let elapsed = now.signed_duration_since(prev.window_start);
    let in_window = prev.notice_id.is_some() && elapsed >= Duration::zero() && elapsed < window;
    if in_window {
        let merged = prev.repeat_count.saturating_add(1);
        DedupVerdict {
            action: AlertAction::Suppress { merged },
            next: DedupState {
                window_start: prev.window_start,
                repeat_count: merged,
                notice_id: prev.notice_id,
            },
        }
    } else {
        // 窗口过期 / 上次没发成 / 时钟回拨：都要立刻发布。
        // 上一窗口的合并次数带进正文，让收件人知道「这段时间它一直在犯」。
        published(now, prev.repeat_count.max(0))
    }
}

/// 开一个新窗口：`window_start = now`、计数归零、`notice_id` 置空（发布成功后再回填）。
fn published(now: DateTime<Utc>, merged: i32) -> DedupVerdict {
    DedupVerdict {
        action: AlertAction::Publish { merged },
        next: DedupState {
            window_start: now,
            repeat_count: 0,
            notice_id: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定时间基准（不依赖真实时钟：整套用例都是 mock 时间）。
    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-13T12:00:00Z")
            .expect("常量应可解析")
            .with_timezone(&Utc)
    }

    fn ten_minutes() -> Duration {
        Duration::seconds(DEFAULT_WINDOW_SECONDS)
    }

    /// 一个「发布成功」后的状态（notice_id 已回填 —— 只有这种状态才允许抑制）。
    fn published_state(window_start: DateTime<Utc>, repeat_count: i32) -> DedupState {
        DedupState {
            window_start,
            repeat_count,
            notice_id: Some(Uuid::now_v7()),
        }
    }

    /// 卡面验收 ①：连续发 5 条同 `(source,title)` → 只落 1 条，其余 4 条计入 count。
    ///
    /// 走的是线上同一段判据（[`evaluate`]）：每轮把 `next` 当作下一轮的 `prev`，
    /// 发布后模拟「回填 notice_id」（线上由 `bind_alert_dedup_notice` 做）。
    #[test]
    fn five_same_alerts_publish_once_and_count_the_rest() {
        let window = ten_minutes();
        let start = t0();
        let mut state: Option<DedupState> = None;
        let mut published = 0;
        let mut suppressed = 0;

        for i in 0..5 {
            let now = start + Duration::seconds(i);
            let verdict = evaluate(now, state.as_ref(), window);
            match verdict.action {
                AlertAction::Publish { merged } => {
                    published += 1;
                    assert_eq!(merged, 0, "第一条不该带合并提示");
                    assert_eq!(verdict.next.window_start, now, "窗口从发布时刻开始");
                    assert!(verdict.next.notice_id.is_none(), "发布后等回填 notice_id");
                    // 模拟 dispatch 成功 → 回填
                    state = Some(DedupState {
                        notice_id: Some(Uuid::now_v7()),
                        ..verdict.next
                    });
                }
                AlertAction::Suppress { merged } => {
                    suppressed += 1;
                    assert_eq!(merged, i as i32, "第 {} 次重复应累计到 {} 次", i, i);
                    assert_eq!(verdict.next.window_start, start, "窗口内不许挪窗口起点");
                    assert!(verdict.next.notice_id.is_some(), "抑制必须保留已发布的通知");
                    state = Some(verdict.next);
                }
            }
        }

        assert_eq!(published, 1, "5 条同源同标题只允许落 1 条");
        assert_eq!(suppressed, 4, "其余 4 条必须被合并");
        let final_state = state.expect("应有状态");
        assert_eq!(final_state.repeat_count, 4, "合并计数 = 4");
    }

    /// 卡面验收 ②：等 11 分钟（mock 时间）再发 1 条 → 再落 1 条，正文含「已重复 4 次」。
    #[test]
    fn eleven_minutes_later_republishes_with_repeat_note() {
        let window = ten_minutes();
        let start = t0();
        // 上一窗口：1 次发布 + 4 次合并
        let prev = published_state(start, 4);

        // 第 11 分钟：窗口已过 → 发布
        let now = start + Duration::minutes(11);
        let verdict = evaluate(now, Some(&prev), window);
        assert!(verdict.action.is_publish(), "窗口过期后必须发布，不能继续抑制");
        assert_eq!(verdict.action.merged(), 4, "带出上一窗口的合并次数");
        assert_eq!(verdict.next.window_start, now, "新窗口从现在开始");
        assert_eq!(verdict.next.repeat_count, 0, "新窗口计数归零");
        assert!(verdict.next.notice_id.is_none(), "新窗口等回填 notice_id");

        let content = with_repeat_note("磁盘使用率 92%（/ 已用 92%）", 4, window);
        assert!(content.contains("已重复 4 次"), "正文必须带出合并次数：{content}");
        assert!(content.contains("近 10 分钟"), "默认窗口是 10 分钟：{content}");
        assert!(content.starts_with("磁盘使用率 92%"), "原文必须原样保留在开头");
    }

    /// 窗口边界：恰好等于窗口长度时**发布**（抑制条件是严格小于）。
    ///
    /// 为什么边界要明确：`t0+10:00` 到底算"第 10 分钟之内"还是"第 11 分钟"，
    /// 两种理解都能自圆其说；这里钉死「< 窗口才抑制」，与卡面「第 11 分钟再发」一致。
    #[test]
    fn window_boundary_publishes_at_exactly_window_length() {
        let window = ten_minutes();
        let start = t0();
        let prev = published_state(start, 0);

        let at_limit = start + window;
        assert!(
            evaluate(at_limit, Some(&prev), window).action.is_publish(),
            "恰好窗口长度 → 发布"
        );

        let one_nano_before = at_limit - Duration::nanoseconds(1);
        assert!(
            !evaluate(one_nano_before, Some(&prev), window).action.is_publish(),
            "窗口内最后 1 ns → 抑制"
        );
    }

    /// 上次发布**失败**（`notice_id IS NULL`）时不许抑制：下一次立刻重发。
    ///
    /// 这是本卡最容易写错、后果最重的一条：如果把"开窗口"和"发出去"当成一件事，
    /// dispatch 失败就会留下一个「空窗口」——之后 10 分钟的告警全被吞掉，而消息
    /// 中心一条都没有（无待办、无补偿，只能改库）。判据把 `notice_id` 作为
    /// 「本窗口是否真的发出去了」的唯一凭据。
    #[test]
    fn failed_publish_is_retried_immediately() {
        let window = ten_minutes();
        let start = t0();
        for offset_seconds in [1, 30, 599] {
            let broken = DedupState {
                window_start: start,
                repeat_count: 0,
                notice_id: None,
            };
            let now = start + Duration::seconds(offset_seconds);
            let verdict = evaluate(now, Some(&broken), window);
            assert!(
                verdict.action.is_publish(),
                "上次没发成功（notice_id = NULL）时 t+{offset_seconds}s 必须重发"
            );
            assert_eq!(verdict.next.window_start, now, "重发时窗口重新开始");
        }
    }

    /// 首次出现 → 发布，且正文不加任何噪音。
    #[test]
    fn first_alert_publishes_without_note() {
        let verdict = evaluate(t0(), None, ten_minutes());
        assert!(verdict.action.is_publish());
        assert_eq!(verdict.action.merged(), 0);
        assert_eq!(verdict.next.repeat_count, 0);
        assert_eq!(verdict.next.window_start, t0());
        assert_eq!(with_repeat_note("原始正文", 0, ten_minutes()), "原始正文");
    }

    /// 时钟回拨（`now < window_start`）→ 发布，绝不能把抑制期拖成"无限长"。
    #[test]
    fn clock_skew_backwards_publishes() {
        let window = ten_minutes();
        let start = t0();
        let prev = published_state(start, 3);
        let now = start - Duration::minutes(5);
        let verdict = evaluate(now, Some(&prev), window);
        assert!(
            verdict.action.is_publish(),
            "时钟被往回拨时必须发布（否则会静默到时钟追上为止）"
        );
    }

    /// 窗口环境变量：缺省 / 非法 / 越界一律回落 600 秒，合法值原样生效。
    #[test]
    fn window_env_parsing_is_fail_safe() {
        assert_eq!(window_of(None), ten_minutes());
        assert_eq!(window_of(Some("")), ten_minutes());
        assert_eq!(window_of(Some("abc")), ten_minutes());
        assert_eq!(window_of(Some("0")), ten_minutes(), "0 = 非法（去重不能悄悄关掉）");
        assert_eq!(window_of(Some("-5")), ten_minutes());
        assert_eq!(window_of(Some("99999999")), ten_minutes(), "超过一天不合规");
        assert_eq!(window_of(Some("2")), Duration::seconds(2));
        assert_eq!(window_of(Some("  120  ")), Duration::seconds(120), "前后空白应被忽略");
    }

    /// 文案里的时间单位：整分钟用「分钟」，否则用「秒」（验收脚本把窗口压到 2 秒）。
    #[test]
    fn window_humanized_text() {
        assert_eq!(humanize_window(ten_minutes()), "10 分钟");
        assert_eq!(humanize_window(Duration::seconds(120)), "2 分钟");
        assert_eq!(humanize_window(Duration::seconds(2)), "2 秒");
        assert_eq!(repeat_note(4, ten_minutes()), "近 10 分钟已重复 4 次");
        assert_eq!(repeat_note(1, Duration::seconds(2)), "近 2 秒已重复 1 次");
    }
}
