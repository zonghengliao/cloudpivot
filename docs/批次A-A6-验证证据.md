# 批次 A · A6「SSE 实时推送 + per-user 频道 + 通道投递预留」验证证据

- 任务卡：`t_6f8c4a46`（开发团队 · manong）
- 依据：`cloudpivot-batchA-architecture.md` §4.5（实时推送）+ §6 A6 行
- 基线：A5 已交付（`93f634a` + `cf7e5a9`，202 tests）
- 实测环境：172.18.10.144（ubuntu-svr）/ profile=`test` / 库 **`cloudpivot_test`** / 端口 **8026**
  （独立 worktree `/var/tmp/a6` + 独立 `CARGO_TARGET_DIR=/var/tmp/a6-target`，不碰 prod 8023 与 dev 8024/8025）
- 原始日志：`backend/tests/sql/a6/acceptance_run.log`（本文件中的片段均为其逐字摘录）
- 复跑方式：`bash backend/tests/sql/a6/run_test_instance.sh` → `python3 backend/tests/sql/a6/acceptance.py`

---

## 1. 交付物与变更清单

| 文件 | 说明 |
|------|------|
| `backend/src/business/notice/sse_hub.rs`（**新增** 617 行） | per-user 频道中心：`HashMap<user_id, Arc<Broadcaster<String>>>`、惰性创建、订阅/取消、推送、计数、心跳、空频道回收（`Subscription::drop` + 5 分钟巡逻任务） |
| `backend/src/business/notice/delivery.rs`（**新增** 267 行） | 通道投递计划与投递记录：站内信必投，邮件/Webhook/短信未配置记 `SKIPPED`（**永不 `FAILED`**） |
| `backend/src/business/notice/handler.rs` | 新增 `GET /business/notice/stream`（SSE）与 `GET /business/notice/stream/stats`（排障/取证） |
| `backend/src/business/notice/mod.rs` | `dispatch` 接入：通道计划改为「模板通道 ∪ 调用方通道 \| 站内信」；事务提交后 `push_realtime` 只推「接收人 ∩ 在线」 |
| `backend/src/business/notice/model.rs` | 新增 `online_receivers(notice_id, online_user_ids)`：只查在线用户的接收行（含逐用户 `receiver_id`） |
| `backend/src/business/notice/dto.rs` | 新增 `StreamStatsRes` / `StreamUserStat` |
| `backend/src/task/mod.rs` | 注册 `notice_sse_channel_sweep`（cron `0 */5 * * * *`） |
| `backend/Cargo.toml` | 开启框架 `sse` feature（`nik::sse` 在该 feature 之后才存在）+ 显式化 `futures-util`（**依赖树里已有**，不新增 crate） |
| `backend/tests/sql/a6/{acceptance.py,run_test_instance.sh,acceptance_run.log}` | 验收脚本 + 起实例脚本 + 原始日志 |

> 红线遵守：未改 `system/`、`support/`；`shared/error_code.rs` **零改动**（复用 `AUTH_TOKEN_INVALID`，因此与 A4 无冲突面）；DB 全程只用 `cloudpivot_test`；未新增迁移。

---

## 2. 设计要点（与架构文档逐条对应）

### 2.1 per-user 频道（§4.5「必须按用户拆频道」）

```
HUB: OnceLock<RwLock<HashMap<user_id, ChannelEntry>>>
ChannelEntry { bus: Arc<Broadcaster<String>>, empty_since: Option<Instant> }
```

- 框架的 `Broadcaster` 是**进程级单播广播**，直接用会让 A 用户收到「B 有新的审批待办」（连 `notice_id`、标题都泄露）。
- 这里**每个用户一个独立 broadcast 频道**：`publish(a, event)` 只 `send` 到 A 的频道，**物理上不可能**到达 B 的订阅端。
- 频道**惰性创建**（首个订阅时建），推送时先 `Arc::clone` 出锁再 `send`，不持锁发送。

### 2.2 连接生命周期与断连清理

1. 订阅 → `Subscription { user_id, bus, rx: Option<Receiver> }`；
2. 客户端断开 → axum 丢弃响应 body → 流被 drop → `Subscription::drop` 执行；
3. `drop` 里**先 `drop(self.rx.take())` 再读 `receiver_count()`** —— `Drop::drop` 在字段析构**之前**执行，不先 take 会读到「还剩自己一个」的 1，频道永远回收不掉；
4. 计数为 0 → 从 `HashMap` 摘除（`Arc::ptr_eq` 防止误删已被替换的频道）；
5. 兜底：`notice_sse_channel_sweep` 每 5 分钟回收「无人订阅且空置 ≥ 30 分钟」的频道（覆盖 `Drop` 被 panic 打断的极端情况）。

### 2.3 心跳

`Sse::new(stream).keep_alive(KeepAlive::new().interval(15s).text("keep-alive"))` —— 每 15 s 一条 **SSE 注释帧** `: keep-alive`，不触发前端 `onmessage`，只用于防反向代理按空闲超时（常见 60 s）切断长连接。

### 2.4 通道投递预留（§4.6「按 template.channels 投递」）

| 通道 | 掩码 | 粒度 | 已配置 | 未配置 |
|------|------|------|--------|--------|
| `INBOX` | 1 | 每接收人一行 | `DELIVERED`（**必投**） | 不会发生 |
| `EMAIL` | 2 | 每接收人一行 | `PENDING` | `SKIPPED` |
| `WEBHOOK` | 4 | 每条通知一行 | `PENDING` | `SKIPPED` |
| `SMS` | 8 | 每接收人一行 | `PENDING` | `SKIPPED` |

`SKIPPED` ≠ `FAILED`：本批没有真实发送器，记 `FAILED` 会让运维以为通道坏了并触发重试风暴；`status_for(false) == "SKIPPED"` 由单测钉死。

**与 A5 的行为变更（显式记录）**：A5 是「调用方指定了通道就用调用方的，否则用模板的」（二选一）；A6 改为**并集** `模板通道 ∪ 调用方通道 | 站内信`，这样模板声明的通道不会因为调用方多写一个通道而被丢掉。`sys_notice.channel_mask` 落库值随之变化（例：模板 INBOX + 调用方 EMAIL → 落 `3`，A5 落 `2`）。`DispatchResult.channels` 结构不变，A5 的 34 个 notice 单测全绿。

### 2.5 与 `dispatch` 的衔接

```
事务提交(Created) → push_realtime(notice_id, seed)
   ├─ online = sse_hub::online_user_ids()        // 进程内在线的用户（空则直接返回，不查库）
   ├─ rows = model::online_receivers(notice_id, online)   // 只回「在线 ∩ 接收人」，含逐用户 receiver_id
   └─ 逐用户 publish(user_id, NoticeEvent{ ..., receiver_id })
```

- **推送失败绝不影响发送结果**：查询/推送异常只 `tracing::warn!`，站内信已落库、未读数轮询兜底。
- 幂等重放（`duplicated = true`）**不重复推送**。
- 全员通知 5 万接收人时有 49990 个不在线，SQL 用 `user_id IN (在线集合)` 只回几十行。

### 2.6 SSE 端点鉴权（红线：走现有鉴权）

`GET /api/business/notice/stream` 与其它接口完全一致：`AuthLayer` + `Authorization: Bearer <token>`，用户身份**只从 `AuthClaims` 取**，绝不接受 query 里的 `user_id`/token 透传（否则 A 用户把 `?user_id=B` 塞进 URL 就能收 B 的通知）。权限码复用 `business:notice:mine`（V0045 已授予所有角色）。统计端点用 `business:notice:list`。

> 启动日志中框架会 warn「静态规则 `/api/business/notice/stream` 与模板规则 `/api/business/notice/{id}` 同时命中（已按静态段优先匹配）」——与既有的 `page` / `mine` / `unread-count` 是同一情形，功能实测正常（SSE 返回 200 + `text/event-stream`）。

---

## 3. 三条硬指标实测（逐条留证）

### 硬指标① per-user 隔离：A、B 同时在线，只给 A 发 → 只有 A 收到

两个客户端都是**真实长连接**（`http.client` 独立线程，`Authorization` 头，读 `text/event-stream`）。

```
② 登录 A / B 并同时建立 SSE 长连接（Authorization 头，走现有 AuthLayer）
  A 响应：HTTP 200，Content-Type=text/event-stream
  B 响应：HTTP 200，Content-Type=text/event-stream
  连接后 stats：total_users=2 total_receivers=2
    - a6000000-0000-0000-0000-0000000000a1 → receivers=1
    - a6000000-0000-0000-0000-0000000000b1 → receivers=1
  [PASS] A 的 receiver_count = 1：实测 1
  [PASS] B 的 receiver_count = 1：实测 1

③ 硬指标①：只给 A 发通知 → 只有 A 收到
  POST /business/notice → HTTP 200，响应：{"code": 0, "msg": "ok", "data": {"notice_id": "01a18280-7a6d-760b-b350-35518d5346f4", "notice_no": "NTS202610281233408d5346f4", "receiver_count": 1, "duplicated": false, "channels": [{"channel": "INBOX", "status": "DELIVERED", "rows": 1}]}}
  [PASS] 发送接口返回 200

  --- A 实际收到的原始帧 ---
  A| event: notice
  A| data: {"notice_id":"01a18280-7a6d-760b-b350-35518d5346f4","notice_no":"NTS202610281233408d5346f4","notice_type":"SYSTEM","level":"INFO","title":"A6 隔离验证：只发给 A","link_url":null,"biz_type":null,"biz_id":null,"receiver_id":"c8674278-a7c3-4153-aeb3-b757b4b08a13","created_at":"2026-10-28T12:33:40.461883550+00:00"}
  --- B 实际收到的原始帧 ---
  B| （无任何帧 —— 隔离成立）
  [PASS] A 收到 1 条事件：实际 1 条
  [PASS] A 收到的正是给 A 的那条：{"notice_id":"01a18280-7a6d-760b-b350-35518d5346f4",…
  [PASS] B 一条事件都没收到：实际 0 条
  [PASS] 载荷带该用户自己的 receiver_id：c8674278-a7c3-4153-aeb3-b757b4b08a13
  [PASS] 载荷 notice_id 与发送返回一致
```

补充：**心跳**（`: keep-alive`）在等待 14.1 s 后采集到，通道在无业务消息时仍然活着。

```
④ 心跳：等待服务端 `: keep-alive` 注释帧（间隔 15 s）
  等待 14.1 s 后采集到的心跳帧：: keep-alive
  [PASS] 采集到 : keep-alive 注释帧
```

### 硬指标② 断连清理：`receiver_count` 1 → 0 且频道被回收

```
⑤ 硬指标②：A 断开 → receiver_count 1 → 0，频道被回收
  断开后 stats：
    total_users=1 total_receivers=1 subscribe_total=6 published_total=3 delivered_total=3
    - a6000000-0000-0000-0000-0000000000b1 → receivers=1
SQL 侧接收行仍在（SSE 只影响内存频道，不动库）：1
  [PASS] A 的 receiver_count 归零：实测 0
  [PASS] A 已从频道表中消失
  [PASS] B 仍在在线（不受 A 断连影响）：实测 1

⑥ B 断连后的收尾 + 服务端日志（断连清理的原始日志）
  最终 stats：total_users=0 total_receivers=0
    2026-10-28T12:34:10.487530Z  INFO cloudpivot::business::notice::sse_hub: SSE 订阅端断开：receiver_count 归零，频道已回收 user_id=a6000000-0000-0000-0000-0000000000a1 receiver_count=0
    2026-10-28T12:34:24.864093Z  INFO cloudpivot::business::notice::sse_hub: SSE 订阅端断开：receiver_count 归零，频道已回收 user_id=a6000000-0000-0000-0000-0000000000b1 receiver_count=0
  [PASS] 日志出现「receiver_count 归零，频道已回收」
  [PASS] 全部连接断开后频道表为空：实测 0
```

`stats` 里 A **整个条目消失**（不是 `receivers=0` 挂在那里），即频道已从 `HashMap` 摘除，不存在内存泄漏。

### 硬指标③ 通道未配置记 `SKIPPED`（不是失败）

模板 `A6_INBOX_ONLY` 的 `channels = INBOX`，调用方显式投 `EMAIL`（`channel_mask=2`）：

```
⑦ 硬指标③：模板只配 INBOX + 投递 EMAIL → 记录 SKIPPED（不是失败）
  DB 中的模板通道：channels=INBOX
  发通知（channel_mask=2 → EMAIL）→ HTTP 200
  响应：{"code": 0, "msg": "ok", "data": {"notice_id": "01a18281-292d-748c-8fc0-9b4b1906c264", "notice_no": "NTS202610281234251906c264", "receiver_count": 1, "duplicated": false, "channels": [{"channel": "INBOX", "status": "DELIVERED", "rows": 1}, {"channel": "EMAIL", "status": "SKIPPED", "rows": 1}]}}
  [PASS] 发送接口 200（未配置通道不导致失败）
    - 通道 EMAIL：status=SKIPPED rows=1
    - 通道 INBOX：status=DELIVERED rows=1
  [PASS] INBOX 为 DELIVERED
  [PASS] EMAIL 为 SKIPPED

  通知主体（生效掩码 = INBOX|EMAIL = 3）：
  NTS202610281234251906c264 | channel_mask=3 | title=模板通知：张三 的待办
  投递记录（DB 实查 sys_notice_delivery）：
EMAIL | SKIPPED | rows=1
  [PASS] EMAIL 投递记录 status=SKIPPED：1 行
  [PASS] 没有任何 FAILED 记录：FAILED=0

  离线语义（A 已断连，只落库不推送）：
    published_total：3 → 3（无在线订阅端，未发生推送）
    接收行=1，未读=1
  [PASS] 离线用户不消耗推送（published_total 未增长）

结论：✅ 三条硬指标全部通过（隔离帧 / 计数归零 / SKIPPED 记录）
```

DB 原始查询口径（脚本内执行）：

```sql
SELECT channel || ' | ' || status || ' | rows=' || count(*)
FROM sys_notice_delivery WHERE notice_id = '<notification>' GROUP BY channel, status ORDER BY channel;
-- EMAIL | SKIPPED | rows=1
```

---

## 4. 三检查原样输出（`backend/` 下，`CARGO_TARGET_DIR=/var/tmp/a6-target`）

```
### cargo fmt -- --check
FMT_OK

### cargo clippy --all-targets -- -D warnings
    Checking cloudpivot v0.1.0 (/var/tmp/a6/backend)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.46s

### cargo test
running 217 tests
…
test result: ok. 217 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
running 0 tests
test result: ok. 0 passed; …
running 2 tests
test s3_minio_real_roundtrip ... ok
test s3_minio_multipart_upload_matches ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 4 tests
test result: ok. 0 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

**测试数对账**：基线 202 → 本次 **217**（**+15**），连跑 3 次均 217 passed / 0 failed（全局频道表被并行用例共享，因此相关用例只断言「自己的 user_id」，不断言全局回收条数）。

新增单测清单（15 个，全绿）：

| 模块 | 用例 | 断言要点 |
|------|------|----------|
| `sse_hub` | `a_user_never_receives_b_events` | A 只收到推给 A 的事件，B 队列**空**（硬指标①） |
| `sse_hub` | `disconnecting_drops_receiver_count_to_zero` | 订阅→1；drop 订阅→0 且从 HUB 摘除（硬指标②） |
| `sse_hub` | `multiple_connections_are_counted_until_last_one_closes` | 多连接计数递减，最后一个断开才回收 |
| `sse_hub` | `publishing_to_offline_user_is_a_noop` | 离线推送返回 0 且不创建空频道 |
| `sse_hub` | `sweep_reclaims_only_idle_empty_channels` | 空置 < 30 min 不回收；≥30 min 回收；有订阅者的频道绝不动 |
| `sse_hub` | `contract_constants_are_stable` | 事件名/心跳/容量/巡逻 cron 是前端契约 |
| `sse_hub` | `event_payload_shape_is_stable` | 载荷字段名稳定（前端只依赖这几个 key） |
| `delivery` | `inbox_is_always_planned` | 站内信必投 |
| `delivery` | `template_inbox_plus_requested_email_keeps_both` | 模板 INBOX + 调用方 EMAIL → 两者都在计划（硬指标③计划侧） |
| `delivery` | `template_channels_are_unioned_not_replaced` | 并集语义 |
| `delivery` | `out_of_range_bits_are_masked_out` | 越界位裁剪 |
| `delivery` | `unconfigured_channel_is_skipped_never_failed` | 未配置 = `SKIPPED` ≠ `FAILED` |
| `delivery` | `channel_table_covers_all_bits_exactly_once` | 4 通道、位不重叠、并集 = 15 |
| `delivery` | `channel_names_match_check_constraint` | 通道名与 DB CHECK 逐字一致 |
| `notice` | `dispatch_channel_plan_is_union` | `dispatch` 入口的通道计划同样是并集 |

---

## 5. 踩坑记录（给后续卡片省时间）

1. **`nik` 的 `sse` feature 不开就没有 `nik::sse`**：`Cargo.toml` 的 `nik` features 里加了 `"sse"`（`nik-web/sse`），否则 `unresolved import alum::sse`。
2. **驱动参数类型坑（踩了两轮）**：`?::jsonb` 绑 `Value::Json(String)` 和 `Value::from(String)` 都会报
   `error serializing parameter 1`（驱动按语句描述推断出 jsonb，而 `Value` 是 text）。
   最终写法：`jsonb_array_elements_text((?::text)::jsonb)` + `Value::Text(json)` —— 参数类型钉成 `text` 再由 PG 转 `jsonb`。
   注意：扇出（`fanout.rs`）里的 `Value::Json(?::jsonb)` 在 `tx.execute_with_params` 路径上没触发，**查询路径**（`find_with_params`，语句预编译带 ParameterDescription）会触发。
3. **起停脚本别按工作目录 pkill**：`pkill -f a6-run` 匹配不到 `…/cloudpivot profile=test`（cmdline 里没有工作目录），旧进程没死、新进程绑不上端口，health 检查打到旧进程上，**看起来重启成功其实是旧二进制**（白排查一轮「SQL 没生效」）。改为按**可执行文件绝对路径**匹配并轮询端口。
4. **`cargo test` 依赖仓库里的本地配置**：`shared::startup_guard::tests::merge_semantics_or_base_and_profile` 读 `config/config-{dev,test}.toml`（`.gitignore` 里的本地文件）。**新建 worktree 时这两个文件不存在 → 该用例必失败**（不是代码问题）。从 `/opt/cloudpivot/backend/config/` 拷一份过去即可。
5. **工作副本 `--ff-only` 同步前提是干净**：A4 卡片正在同一个 `/opt/cloudpivot` 里改 `impexp/*`、`shared/error_code.rs`、`task/mod.rs`，本卡全程在独立 worktree（`/var/tmp/a6`）开发，收尾才尝试快进同步；两边都改 `task/mod.rs` 的那一行插入点是唯一的冲突面（保留双方）。

---

## 6. 前端接入指引（A7 用）

```ts
// EventSource 不能自定义请求头 → 用 fetch 读流（鉴权与其它接口一致）
const controller = new AbortController();
const resp = await fetch("/api/business/notice/stream", {
  headers: { Authorization: `Bearer ${token}` },
  signal: controller.signal,
});
const reader = resp.body!.getReader();
const decoder = new TextDecoder();
let buffer = "";
for (;;) {
  const { value, done } = await reader.read();
  if (done) break;
  buffer += decoder.decode(value, { stream: true });
  const chunks = buffer.split("\n\n");      // SSE 以空行分隔帧
  buffer = chunks.pop() ?? "";
  for (const chunk of chunks) {
    if (chunk.startsWith(":")) continue;    // 心跳注释帧，忽略
    const data = chunk.split("\n").find((l) => l.startsWith("data:"));
    if (!data) continue;
    const notice = JSON.parse(data.slice(5).trim());   // { notice_id, receiver_id, title, link_url, ... }
    // 未读数以轮询为权威（SSE 只做「立即刷新」加速）；断线由浏览器/上层重连
  }
}
```

- **轮询兜底必须保留**：SSE 只是加速器，`GET /notice/unread-count` 仍是权威口径。
- **多副本限制**：进程内频道只在同进程生效（A 副本发的通知，连在 B 副本上的用户收不到）。B 批次用 Redis Pub/Sub 替换 `sse_hub::publish_json` 即可（单点已收敛在该函数）。
- 断线重连建议：`retry` 交给上层退避（`/stream` 401 说明 token 过期，先刷新 token 再重连）。

---

## 7. 补充验证：SSE 端点的鉴权不可穿透（红线：走现有鉴权）

脚本 `backend/tests/sql/a6/auth_probe.py`，原始输出 `backend/tests/sql/a6/auth_probe.log`：

```
① 不带 Authorization 访问 SSE 端点
  HTTP 401 {"code": 401, "msg": "未授权访问，请先登录"}
  [PASS] 未登录访问 SSE 返回 401

② 用 B 的 token 拼 ?user_id=A（试图抢 A 的频道）
  响应：HTTP 200，Content-Type=text/event-stream
  stats：total_users=1 total_receivers=1
    - a6000000-0000-0000-0000-0000000000b1 → receivers=1
  [PASS] 频道归属 B（token 的真实身份）：1
  [PASS] A 的频道没有被 query 参数建出来：0

③ 推给 A 的通知不会到达这条（B 的）连接
  POST /business/notice → HTTP 200，notice_id=01a18283-2b4b-76ae-9c4f-00170ec902e9
  B 的连接收到的数据帧数：0
  [PASS] B 的连接一帧都没收到：0 帧
  断开后 stats：total_users=0 total_receivers=0
  [PASS] 断开后频道表为空：0

✅ 鉴权不可穿透：无 token 401、query 不能伪造频道归属、A 的通知不会到 B 的连接
```

结论：用户身份**只**来自 `AuthClaims`；`?user_id=` / `?userId=` 一律被忽略（频道按 token 的真实身份建立）。

---

## 8. 结论

- 三条硬指标**全部实测通过**（隔离帧 / 计数归零 / `SKIPPED` 记录），21 条断言全 PASS；
- `cargo fmt -- --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test`（217 passed）全绿；
- `system/`、`support/` 零改动，`shared/error_code.rs` 零改动，无新增迁移；
- 对外契约：`GET /api/business/notice/stream`（`business:notice:mine`）、`GET /api/business/notice/stream/stats`（`business:notice:list`）、`dispatch` 签名不变（C/D 批次照旧调用）。

---

## 9. 生产工作副本同步核对（A5 踩过的坑，本卡收尾必查）

A6 全程在独立 worktree `/var/tmp/a6` 开发（A4 卡当时正在 `/opt/cloudpivot` 里改 `impexp/*` 与 `task/mod.rs`），
收尾时 A4 已 rebase 到 A6 之上并推送，生产工作副本 `/opt/cloudpivot` 已快进到同一 tip：

```console
$ cd /opt/cloudpivot && git rev-parse HEAD origin/main
d7c2db954f49f154e9b836f3688255da1f72c5a0
d7c2db954f49f154e9b836f3688255da1f72c5a0
$ git status --short
（空 → 工作区干净）
$ git log --oneline -6
d7c2db9 docs(impexp): 归档 A4 验收证据（四条硬指标实测输出 + 两个实测踩坑）
d7be183 feat(impexp): A4 异步任务 —— SKIP LOCKED 抢占 + 进度 + 重试 + 僵尸回收 + 过期 410
2c59f97 fix(impexp): 导出定义 after_id 绑定 uuid 类型 —— 10 万行导出的第 2 页必然失败
215cef0 test(notice): A6 补充 SSE 鉴权穿透验证（无 auth 401 / query 不能伪造频道归属 / A 的通知不到 B 的连接）
dddb22c docs(notice): 归档 A6 验证证据（三条硬指标实测输出 + 三检查日志 + 4 条踩坑记录）
dee5488 feat(notice): A6 SSE 实时推送 + per-user 频道 + 通道投递预留
```

在**生产工作副本**（`/opt/cloudpivot/backend`，已含 A6 + A4 合并后的代码）复跑三检查：

```console
### cargo fmt -- --check
FMT_OK
### cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.60s
### cargo test
running 248 tests
test result: ok. 248 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
running 7 tests   → test result: ok. 7 passed
running 2 tests   → test result: ok. 2 passed (s3 minio)
running 4 tests   → 4 ignored
```

248 = A5 基线 202 + A6 新增 15 + A4 新增 31：**合并后的主分支全绿**，`task/mod.rs` 里双方的注册行并存
（`notice::sse_hub::register_builtin` 与 `impexp::worker/cleanup::register_builtin` 都在，启动日志可见
`定时任务 notice_sse_channel_sweep [0 */5 * * * *] 已排程`）。
