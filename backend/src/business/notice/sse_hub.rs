//! per-user SSE 实时推送频道中心（批次 A · A6）。
//!
//! # 为什么必须 per-user（架构文档 §4.5）
//!
//! 框架的 [`Broadcaster`] 是**进程级单播广播**（`tokio::sync::broadcast`）：一个广播器
//! 的所有订阅者收到完全相同的消息。若全站只用一个广播器，A 用户会收到「B 有新的审批
//! 待办」，`notice_id`、标题甚至链接都会泄露。
//!
//! 因此本模块为**每个用户一个独立频道**：
//!
//! ```text
//! HashMap<user_id, Arc<Broadcaster<String>>>
//!              ├── Sender   (只被 publish(user_id) 使用)
//!              └── Receiver (每个 GET /business/notice/stream 连接一个)
//! ```
//!
//! `publish(a, event)` 只往 A 的广播器 `send`，**物理上不可能**到达 B 的订阅端。
//!
//! # 连接生命周期（防内存泄漏）
//!
//! 频道**惰性创建**（首个订阅时建），随订阅端一起回收：
//!
//! 1. 订阅 → [`Subscription`]（持有 `broadcast::Receiver`）；
//! 2. 客户端断开 → axum 丢弃响应 body → 流被 drop → [`Subscription`] 的 `Drop` 执行；
//! 3. `Drop` 里**先**释放 `Receiver` 再读 `receiver_count()`，计数为 0 时把该用户的
//!    频道从 `HashMap` 摘除（频道占用的 `Arc` 是唯一残留引用，摘除即释放）。
//!
//! `Drop` 里必须先 `take()` 掉 `Receiver`：`Drop::drop` 在字段析构**之前**执行，
//! 不先 take 会读到「还剩自己一个」的计数 1，频道永远回收不掉。
//!
//! 另外每 5 分钟由 [`register_builtin`] 注册的 `notice_sse_channel_sweep` 兜底巡逻，
//! 回收「无人订阅且已空置 30 分钟」的频道（例如 `Drop` 未执行就被 panic 打断的极端情况）。
//!
//! # 帧序
//!
//! 1. **首帧**（建连即刻）：注释帧 `: connected` —— 让浏览器 `fetch()` 的 Promise 立刻
//!    resolve（它等的是首个字节，不是响应头），否则前端要等第一条心跳才开始认「已连上」
//!    （`P1-FIX-6` 实测：那条心跳在 15 s 后才到）；
//! 2. **业务帧**：`event: notice` + `data: {…}`（见 [`EVENT_NAME`]）；
//! 3. **心跳帧**：每 [`HEARTBEAT_SECS`] 秒一条 SSE 注释帧 `: keep-alive`，防反向代理按
//!    空闲超时（常见 60 s）切断长连接。注释帧不触发前端 `onmessage`，也不会被
//!    `utils/notice-stream.ts` 的解析器产出（`data` 行为空的块被丢弃）。
//!
//! # 多副本注意
//!
//! 进程内 `HashMap` 只在**同进程**生效。多副本部署时 A 副本发的通知，连在 B 副本上的
//! 用户收不到——缓解办法是前端仍以轮询「未读数」为权威（SSE 只做立即刷新），
//! B 批次引入 Redis Pub/Sub 时只需替换 [`publish_json`] 的实现（单点已收敛在此）。

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use futures_util::StreamExt;
use futures_util::stream::{once, unfold};
use nik::prelude::*;
use nik::sse::Broadcaster;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::business::notice::dto::{StreamStatsRes, StreamUserStat};
use crate::task::Scheduler;

/// SSE 事件名（前端 `addEventListener("notice", ...)`）。
pub const EVENT_NAME: &str = "notice";

/// 每个频道的缓冲条数（订阅端消费不及会丢最旧的，但**连接不断**）。
pub const CHANNEL_CAPACITY: usize = 64;

/// 心跳间隔（秒）：发 `: keep-alive` 注释帧。
pub const HEARTBEAT_SECS: u64 = 15;

/// 空频道保留时长（秒）：无人订阅超过该时长由巡逻任务回收。
pub const IDLE_RECLAIM_SECS: u64 = 30 * 60;

/// 巡逻任务名（同时用作调度名与日志标识）。
pub const SWEEP_TASK_NAME: &str = "notice_sse_channel_sweep";

/// 巡逻任务 cron（6 段，每 5 分钟）。
pub const SWEEP_TASK_CRON: &str = "0 */5 * * * *";

/// 累计 `publish` 调用次数（观测用）。
static PUBLISHED_TOTAL: AtomicU64 = AtomicU64::new(0);
/// 累计**成功入队到订阅端**的消息条数（一次 publish 命中 N 个连接即 +N）。
static DELIVERED_TOTAL: AtomicU64 = AtomicU64::new(0);
/// 累计建立的订阅连接数。
static SUBSCRIBE_TOTAL: AtomicU64 = AtomicU64::new(0);

/// 推送给单个用户的一条通知事件（**按接收人定制**：`receiver_id` 是「我的那条接收行」）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NoticeEvent {
    /// 通知主键（字符串，`uuid` crate 未开 serde feature）。
    pub notice_id: String,
    /// 通知编号（前端可展示 / 排障）。
    pub notice_no: String,
    /// 通知类型（`SYSTEM` / `TODO` / `ALERT` / `WORKFLOW` / `CIRCULAR`）。
    pub notice_type: String,
    /// 级别（`INFO` / `WARN` / `URGENT`）。
    pub level: String,
    /// 标题。
    pub title: String,
    /// 跳转链接（可空）。
    pub link_url: Option<String>,
    /// 业务类型（可空）。
    pub biz_type: Option<String>,
    /// 业务主键（可空）。
    pub biz_id: Option<String>,
    /// **该用户自己的**接收行主键（前端据此调 `PUT /notice/read/{receiver_id}`）。
    pub receiver_id: String,
    /// 通知落库时间（RFC 3339）。
    pub created_at: String,
}

/// 一个用户的频道（广播器 + 空置起点）。
struct ChannelEntry {
    /// 该用户独占的广播器。
    bus: Arc<Broadcaster<String>>,
    /// 从什么时候开始没人订阅（`None` 表示有订阅者或刚创建）。
    empty_since: Option<Instant>,
}

/// 全局频道表（进程级单例）。
fn hub() -> &'static RwLock<HashMap<uuid::Uuid, ChannelEntry>> {
    static HUB: OnceLock<RwLock<HashMap<uuid::Uuid, ChannelEntry>>> = OnceLock::new();
    HUB.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 读锁（中毒时取回内部数据——推送链路不该因某处 panic 永久瘫痪）。
fn read_hub() -> RwLockReadGuard<'static, HashMap<uuid::Uuid, ChannelEntry>> {
    hub().read().unwrap_or_else(|poison| poison.into_inner())
}

/// 写锁（同上）。
fn write_hub() -> RwLockWriteGuard<'static, HashMap<uuid::Uuid, ChannelEntry>> {
    hub().write().unwrap_or_else(|poison| poison.into_inner())
}

/// 取某用户的频道（**惰性创建**，同一用户永远拿到同一个广播器）。
///
/// # 参数
///
/// - `user_id`：用户主键。
///
/// # 返回值
///
/// 该用户独占的广播器（`Arc` 可直接 clone 出锁外使用，避免持锁发送）。
pub fn channel(user_id: uuid::Uuid) -> Arc<Broadcaster<String>> {
    if let Some(entry) = read_hub().get(&user_id) {
        return entry.bus.clone();
    }
    let mut guard = write_hub();
    // 双重检查：读锁释放到写锁获取之间可能已被别的线程插入
    if let Some(entry) = guard.get(&user_id) {
        return entry.bus.clone();
    }
    let bus = Arc::new(Broadcaster::new(CHANNEL_CAPACITY));
    guard.insert(
        user_id,
        ChannelEntry {
            bus: bus.clone(),
            empty_since: None,
        },
    );
    bus
}

/// 向**指定用户**推送一条事件（只推给该用户，不影响任何人）。
///
/// # 参数
///
/// - `user_id`：接收人主键。
/// - `event`：按接收人定制的事件（含该用户自己的 `receiver_id`）。
///
/// # 返回值
///
/// 成功入队的订阅端数量；该用户不在线（或序列化失败）时为 0——**不是错误**，
/// SSE 是「加速器」，未读数以轮询为权威。
pub fn publish(user_id: uuid::Uuid, event: &NoticeEvent) -> usize {
    let payload = match serde_json::to_string(event) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(user_id = %user_id, error = %error, "SSE 事件序列化失败，已跳过");
            return 0;
        }
    };
    publish_json(user_id, payload)
}

/// 向指定用户推送一条**已序列化**的 JSON 字符串（Redis Pub/Sub 等外部来源的入口）。
///
/// # 参数
///
/// - `user_id`：接收人主键。
/// - `payload`：JSON 字符串（原样作为 SSE `data`）。
///
/// # 返回值
///
/// 成功入队的订阅端数量（不在线返回 0）。
pub fn publish_json(user_id: uuid::Uuid, payload: String) -> usize {
    // 先把 Arc clone 出锁：send 不持锁，避免「慢订阅端」拖住整个频道表
    let bus = match read_hub().get(&user_id) {
        Some(entry) => entry.bus.clone(),
        None => return 0,
    };
    let sent = bus.send(payload);
    PUBLISHED_TOTAL.fetch_add(1, Ordering::Relaxed);
    DELIVERED_TOTAL.fetch_add(sent as u64, Ordering::Relaxed);
    if sent > 0 {
        tracing::debug!(user_id = %user_id, subscribers = sent, "SSE 事件已推送");
    }
    sent
}

/// 建立订阅（每次 `GET /business/notice/stream` 调用一次）。
///
/// # 参数
///
/// - `user_id`：当前登录用户（来自 `AuthClaims`，**绝不来自 query / body**）。
///
/// # 返回值
///
/// [`Subscription`]：持有接收端与频道引用，drop 即退订并触发回收。
pub fn subscribe(user_id: uuid::Uuid) -> Subscription {
    let bus = channel(user_id);
    let rx = bus.subscribe();
    SUBSCRIBE_TOTAL.fetch_add(1, Ordering::Relaxed);
    tracing::info!(
        user_id = %user_id,
        receiver_count = bus.receiver_count(),
        "SSE 订阅建立"
    );
    Subscription {
        user_id,
        bus,
        rx: Some(rx),
    }
}

/// 某用户当前的订阅端数量（断连后应回到 0）。
pub fn subscriber_count(user_id: uuid::Uuid) -> usize {
    read_hub()
        .get(&user_id)
        .map(|entry| entry.bus.receiver_count())
        .unwrap_or(0)
}

/// 全部用户的订阅端总数。
pub fn total_receivers() -> usize {
    read_hub()
        .values()
        .map(|entry| entry.bus.receiver_count())
        .sum()
}

/// 当前**在线**（至少一个订阅端）的用户主键（推送时据此只查在线用户的接收行）。
pub fn online_user_ids() -> Vec<uuid::Uuid> {
    let guard = read_hub();
    let mut ids: Vec<uuid::Uuid> = guard
        .iter()
        .filter(|(_, entry)| entry.bus.receiver_count() > 0)
        .map(|(user_id, _)| *user_id)
        .collect();
    ids.sort_unstable();
    ids
}

/// 频道表快照（运维排障 / 验收用）。
///
/// # 返回值
///
/// 各用户订阅端数量 + 汇总计数与累计推送量。
pub fn stats() -> StreamStatsRes {
    let guard = read_hub();
    let mut users: Vec<StreamUserStat> = guard
        .iter()
        .map(|(user_id, entry)| StreamUserStat {
            user_id: user_id.to_string(),
            receivers: entry.bus.receiver_count(),
        })
        .collect();
    users.sort_by(|left, right| left.user_id.cmp(&right.user_id));
    StreamStatsRes {
        total_users: users.len(),
        total_receivers: users.iter().map(|user| user.receivers).sum(),
        users,
        published_total: PUBLISHED_TOTAL.load(Ordering::Relaxed),
        delivered_total: DELIVERED_TOTAL.load(Ordering::Relaxed),
        subscribe_total: SUBSCRIBE_TOTAL.load(Ordering::Relaxed),
        heartbeat_seconds: HEARTBEAT_SECS,
    }
}

/// 巡逻一次：回收「无人订阅且空置超 [`IDLE_RECLAIM_SECS`]」的频道。
///
/// # 参数
///
/// - `now`：当前时刻（测试可注入未来时刻，无需 sleep 30 分钟）。
///
/// # 返回值
///
/// 本次回收的频道数。
pub fn sweep_idle(now: Instant) -> usize {
    let mut removed = 0usize;
    let mut guard = write_hub();
    let mut reclaim: Vec<uuid::Uuid> = Vec::new();
    for (user_id, entry) in guard.iter_mut() {
        if entry.bus.receiver_count() > 0 {
            entry.empty_since = None;
            continue;
        }
        match entry.empty_since {
            None => entry.empty_since = Some(now),
            Some(since) => {
                // `now` 可能早于 `since`（测试注入历史时刻）：saturating 到 0，不回收
                let idle = now.duration_since(since).as_secs();
                if idle >= IDLE_RECLAIM_SECS {
                    reclaim.push(*user_id);
                }
            }
        }
    }
    for user_id in reclaim {
        guard.remove(&user_id);
        removed += 1;
        tracing::info!(user_id = %user_id, "SSE 空频道已回收（巡逻任务）");
    }
    removed
}

/// 在调度器上注册空频道巡逻任务（由 `task::Scheduler::register_builtin` 调用）。
///
/// # 参数
///
/// - `scheduler`：全局调度器。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        SWEEP_TASK_NAME,
        SWEEP_TASK_CRON,
        "系统任务：SSE 空频道回收",
        || {
            tokio::spawn(async move {
                let removed = sweep_idle(Instant::now());
                if removed > 0 {
                    tracing::info!(removed, "SSE 空频道回收完成");
                }
            })
        },
    );
}

/// 一次 SSE 订阅（生命周期与 HTTP 连接一致）。
///
/// `Drop` 是「断连清理」的唯一实现点：无论客户端正常关闭、网络中断还是服务端关停，
/// 只要响应 body 被丢弃，这里就会被调用。
pub struct Subscription {
    /// 订阅者。
    user_id: uuid::Uuid,
    /// 该用户的频道。
    bus: Arc<Broadcaster<String>>,
    /// 接收端（`Option` 只为在 `Drop` 里**先**释放它再读计数）。
    rx: Option<broadcast::Receiver<String>>,
}

impl Subscription {
    /// 订阅者主键。
    pub fn user_id(&self) -> uuid::Uuid {
        self.user_id
    }

    /// 转成 axum SSE 响应（自动挂心跳）。
    ///
    /// # 返回值
    ///
    /// `Sse`：事件名 [`EVENT_NAME`]，每 [`HEARTBEAT_SECS`] 秒一条 `: keep-alive` 注释帧。
    pub fn into_sse(self) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
        let event_name = EVENT_NAME;
        let stream = unfold(Some(self), move |state| async move {
            let mut subscription = state?;
            loop {
                let received = match subscription.rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => return None,
                };
                match received {
                    Ok(payload) => {
                        let event = Event::default().event(event_name).data(payload);
                        return Some((Ok(event), Some(subscription)));
                    }
                    // 订阅端消费不及：跳过被丢弃的旧消息，**断开不是选项**
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            user_id = %subscription.user_id,
                            skipped,
                            "SSE 订阅端消费不及，已跳过积压消息（连接保持）"
                        );
                        continue;
                    }
                    // 频道关闭（正常关停）：结束流
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        // 首帧（`P1-FIX-6` 实测结论，别删）：
        //
        // axum 的 `KeepAlive` 第一条 `: keep-alive` 要等 `HEARTBEAT_SECS`（15s）才发，
        // 在那之前**响应体一个字节都没有**。而浏览器 `fetch()` 的 Promise 要等**首个字节**
        // 才 resolve（不是等响应头！）—— 于是前端 `openNoticeStream` 的 `onOpen` 被推迟
        // 整整 15 秒：这 15 秒里前端以为「还没连上」、继续 30s 轮询，验收侧（Playwright /
        // CDP `Network.responseReceived`）也**根本看不到这条 200**，只能看到请求发出去。
        // 一条注释帧把「握手已完成」立刻兑现：解析器会丢弃注释帧（不影响业务帧），
        // 但字节一到，`fetch` 立刻 resolve、`onOpen` 立刻把 `connected` 置真、轮询立刻停。
        let hello = once(async { Ok::<Event, Infallible>(Event::default().comment("connected")) });
        Sse::new(hello.chain(stream)).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(HEARTBEAT_SECS))
                .text("keep-alive"),
        )
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // 关键顺序：先释放接收端（`Drop::drop` 在字段析构之前执行，不 take 会读到 1）
        drop(self.rx.take());
        let remaining = self.bus.receiver_count();
        if remaining > 0 {
            tracing::info!(
                user_id = %self.user_id,
                remaining,
                "SSE 订阅端断开（该用户仍有其它连接）"
            );
            return;
        }
        let mut guard = write_hub();
        let same_channel = guard
            .get(&self.user_id)
            .map(|entry| Arc::ptr_eq(&entry.bus, &self.bus))
            .unwrap_or(false);
        if same_channel {
            guard.remove(&self.user_id);
            tracing::info!(
                user_id = %self.user_id,
                receiver_count = 0,
                "SSE 订阅端断开：receiver_count 归零，频道已回收"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一条给某用户的事件。
    fn event_for(title: &str) -> NoticeEvent {
        NoticeEvent {
            notice_id: uuid::Uuid::now_v7().to_string(),
            notice_no: "NTS20260101000000ABCDEFGH".to_string(),
            notice_type: "SYSTEM".to_string(),
            level: "INFO".to_string(),
            title: title.to_string(),
            link_url: Some("/business/notice".to_string()),
            biz_type: None,
            biz_id: None,
            receiver_id: uuid::Uuid::now_v7().to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// **硬指标 ①**：A 用户订阅后收到的只有推给 A 的事件，B 用户一个字节都收不到。
    #[tokio::test]
    async fn a_user_never_receives_b_events() {
        let user_a = uuid::Uuid::now_v7();
        let user_b = uuid::Uuid::now_v7();
        let mut rx_a = channel(user_a).subscribe();
        let mut rx_b = channel(user_b).subscribe();

        let event_a = event_for("给 A 的通知");
        let event_b = event_for("给 B 的通知");

        assert_eq!(publish(user_a, &event_a), 1, "A 的推送应命中 1 个订阅端");
        assert_eq!(publish(user_b, &event_b), 1, "B 的推送应命中 1 个订阅端");

        // A 先收到自己的（频道隔离意味着 B 的事件不会串进来）
        let first = rx_a.recv().await.expect("A 应收到事件");
        assert!(first.contains("给 A 的通知"), "A 收到：{first}");
        assert!(
            !first.contains("给 B 的通知"),
            "A 不得收到 B 的事件：{first}"
        );

        let second = rx_b.recv().await.expect("B 应收到事件");
        assert!(second.contains("给 B 的通知"), "B 收到：{second}");
        assert!(
            !second.contains("给 A 的通知"),
            "B 不得收到 A 的事件：{second}"
        );

        // 交叉断言：各自队列里再无第二条（A 只收到 1 条）
        assert!(
            rx_a.try_recv().is_err(),
            "A 的队列里不应有第二条消息（跨用户串消息即隔离失败）"
        );
        assert!(rx_b.try_recv().is_err(), "B 的队列里不应有第二条消息");
    }

    /// **硬指标 ②**：订阅 → `receiver_count = 1`；drop 订阅 → 归零且频道被摘除。
    #[test]
    fn disconnecting_drops_receiver_count_to_zero() {
        let user_id = uuid::Uuid::now_v7();
        let subscription = subscribe(user_id);
        assert_eq!(subscriber_count(user_id), 1, "订阅后应有 1 个接收端");
        assert!(online_user_ids().contains(&user_id));

        drop(subscription);
        assert_eq!(
            subscriber_count(user_id),
            0,
            "断连后 receiver_count 必须归零"
        );
        assert!(
            !online_user_ids().contains(&user_id),
            "空频道必须从 HUB 摘除（否则是内存泄漏）"
        );
        assert!(
            stats()
                .users
                .iter()
                .all(|user| user.user_id != user_id.to_string()),
            "stats 里不应再出现该用户"
        );
    }

    /// 同一用户多连接：只断开一个时计数递减但不回收，全部断开才回收。
    #[test]
    fn multiple_connections_are_counted_until_last_one_closes() {
        let user_id = uuid::Uuid::now_v7();
        let first = subscribe(user_id);
        let second = subscribe(user_id);
        assert_eq!(subscriber_count(user_id), 2);

        drop(first);
        assert_eq!(subscriber_count(user_id), 1, "还剩一个连接");
        assert!(online_user_ids().contains(&user_id), "仍在线，不应摘除频道");

        drop(second);
        assert_eq!(subscriber_count(user_id), 0);
        assert!(!online_user_ids().contains(&user_id));
    }

    /// 给不在线的用户推事件返回 0，且**不会**创建频道（避免「推一次就留一个空频道」）。
    #[test]
    fn publishing_to_offline_user_is_a_noop() {
        let user_id = uuid::Uuid::now_v7();
        let event = event_for("离线用户的通知");
        assert_eq!(publish(user_id, &event), 0);
        assert_eq!(subscriber_count(user_id), 0);
        assert!(
            stats()
                .users
                .iter()
                .all(|user| user.user_id != user_id.to_string())
        );
    }

    /// 巡逻任务：空频道超过 30 分钟才回收，有订阅者的频道一律不动。
    #[test]
    fn sweep_reclaims_only_idle_empty_channels() {
        let idle_user = uuid::Uuid::now_v7();
        let live_user = uuid::Uuid::now_v7();
        let _idle_bus = channel(idle_user); // 无订阅端的空频道
        let live = subscribe(live_user); // 有订阅端

        let now = Instant::now();
        // 第一轮：只记下空置起点，不回收
        sweep_idle(now);
        assert_eq!(subscriber_count(live_user), 1);
        assert!(hub_contains(idle_user), "第一轮不应回收空频道");

        // 29 分钟后：仍未到期
        sweep_idle(now + Duration::from_secs(IDLE_RECLAIM_SECS - 60));
        assert!(hub_contains(idle_user), "未到 30 分钟不回收");

        // 31 分钟后：回收空频道，但不碰有订阅者的频道
        sweep_idle(now + Duration::from_secs(IDLE_RECLAIM_SECS + 60));
        assert!(!hub_contains(idle_user), "空置超时的频道应被回收");
        assert!(hub_contains(live_user), "有订阅者的频道绝不能被回收");
        assert_eq!(subscriber_count(live_user), 1, "有订阅者的频道绝不能被回收");

        drop(live);
    }

    /// 频道表里是否还有某用户的条目（测试内部用；全局表被并行测试共享，
    /// 因此**只断言自己的 user_id**，不断言全局回收条数）。
    fn hub_contains(user_id: uuid::Uuid) -> bool {
        read_hub().contains_key(&user_id)
    }

    /// 心跳间隔常量与事件名是前端契约，不得随意改动。
    #[test]
    fn contract_constants_are_stable() {
        assert_eq!(EVENT_NAME, "notice");
        assert_eq!(HEARTBEAT_SECS, 15);
        assert_eq!(CHANNEL_CAPACITY, 64);
        assert_eq!(SWEEP_TASK_CRON, "0 */5 * * * *");
        // 空置回收期不短于 30 分钟（编译期断言，避免被改成 1 秒后频道表抖动）
        const { assert!(IDLE_RECLAIM_SECS >= 60 * 30) };
    }

    /// 事件 JSON 载荷自描述（前端只依赖这几个字段名）。
    #[test]
    fn event_payload_shape_is_stable() {
        let event = event_for("标题");
        let json = serde_json::to_value(&event).expect("序列化失败");
        for key in [
            "notice_id",
            "notice_no",
            "notice_type",
            "level",
            "title",
            "link_url",
            "receiver_id",
            "created_at",
        ] {
            assert!(json.get(key).is_some(), "载荷缺少字段 {key}：{json}");
        }
    }
}
