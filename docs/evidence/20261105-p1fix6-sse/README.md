# P1-FIX-6 · SSE 永远 401（每页控制台刷屏）—— 修复与实测

> 卡：`t_12441542`（backend-developer） · 基线：`main` 5e22785 · 环境：99 本机隔离实例
> 目标实例：**18140（后端 / profile=test / 库 `cloudpivot_sse6` = 生产快照副本）+ 5140（vite dev 反代）**
> 生产（144）**未部署**，本卡只到隔离副本（见「偏差」）

## 1. 根因（两层，都必须修）

| 层 | 事实 | 后果 |
|----|------|------|
| **前端** | `stores/notice.ts#connect()` 用 `new EventSource('/api/business/notice/stream?token=…')` —— `EventSource` 不能设置 `Authorization` 头，只能把令牌塞查询串 | 一旦后端不采信查询令牌，**每次页面加载必 401**，且 `EventSource` 会自动重连、反复刷 |
| **后端** | `?token=` 白名单只有环境变量 `NIK_AUTH_QUERY_TOKEN_PATHS` 一个入口，**默认空 = 一律不认**；`config.toml` 里没有对应配置项 | 没有任何环境注入了它 → `/notice/stream?token=` **恒 401**；实时推送从未真正工作过（只剩 30s 轮询兜底，功能层面看不出来） |
| **验收盲区** | 审计脚本 `audit_ui.mjs` 把 `notice/stream` 的 401 **显式过滤掉了**（当时按「已知缺陷」处理） | 39 页逐页刷红却从没进过结论；`frontend/e2e/_pagecheck.ts` 的 `KNOWN_BAD_API` 也把它当已知缺陷豁免 |

证据原文：`scripts/verify/audit_features_20261104/06_ui_run.log`（127 行，其中 39 条 `401 http://127.0.0.1:5173/api/business/notice/stream?token=…`）。

## 2. 改了什么（文件 / 函数）

### 后端：配置项 + 单一解析入口 + 首启自检

| 文件 | 改动 |
|------|------|
| `backend/config/config.toml` | `[middleware.auth]` 新增 **`query_token_paths = ["/business/notice/stream"]`**（默认白名单，相对路径自动拼 `[router] prefix`）。这就是卡面「配默认白名单」的落地位置：**不改代码、不靠环境变量**，所有 profile 继承（test/dev/prod 都从 `config.toml` 合并） |
| `backend/vendor/nik-config/src/lib.rs` | `AuthMiddlewareConfig` 新增字段 `query_token_paths: Vec<String>`（`#[serde(default)]`）；`merge_middleware` 按「非空即覆盖」与 `ignore_paths` 同款语义合并，profile 没写就保留基础配置 |
| `backend/vendor/nik-web/src/middleware/auth.rs` | 新增 `pub const ENV_QUERY_TOKEN_PATHS`、`pub fn join_path_prefix`、**`pub fn resolve_query_token_paths(prefix, from_config)`（唯一实现）**；环境变量优先且完全覆盖配置项。附 3 条单测（前缀拼接 / 配置兜底 / 全空=fail-closed） |
| `backend/vendor/nik-web/src/app.rs` | 装配认证层的白名单改为调 `resolve_query_token_paths`；**白名单为空时打 `WARN`**（点名症状 + 两种修法），不再静默 |
| `backend/vendor/nik/src/lib.rs` | `prelude` 导出上述三件，业务侧自检与框架装配共用一个实现 |
| `backend/src/bootstrap.rs` | 新增启动钩子 **`sse_query_token_guard()`**：用同一份 `resolve_query_token_paths` 判定 `/business/notice/stream` 是否在名单里 → `INFO 自检通过` / `WARN 自检未通过`（带 endpoint / whitelist / env_var）。**只告警不拦启动** |
| `backend/src/business/notice/sse_hub.rs` | `into_sse()` 增加**首帧注释帧 `: connected`**（见 §3 的实测理由）+ 模块文档改「帧序」 |
| `backend/src/business/notice/handler.rs` | SSE 帧表补「首帧」一行 |

### 前端：实时通道不再用 `EventSource`（根治噪声）

| 文件 | 改动 |
|------|------|
| `frontend/src/stores/notice.ts` | **删掉 `EventSource` 整条通道**（`source` 变量、`SSE_EVENT_NAME`、`handleMessageEvent`、`onerror` 降级逻辑）；`connect()` 只留带头 `fetch` + `ReadableStream`（`openNoticeStream`），先挂 30s 轮询兜底、`onOpen` 后停轮询 |
| `frontend/src/api/business/notice/index.ts` | 删除 `buildNoticeSseUrl()`（唯一使用 `?token=` 的地方）与不再使用的 `TOKEN_KEY` 引入；`noticeApi` 同步去掉该成员 |
| `frontend/e2e/_pagecheck.ts` | `KNOWN_BAD_API` 清空（原「通知 SSE 401」条目删掉并写清为什么），52 页探针不再豁免它 |
| `frontend/e2e/34-error-boundary.spec.ts` | 「已知缺陷留证：SSE 恒 401」改为 **`P1-FIX-6：通知实时通道 200 连通，页面加载不再刷 401`**：断言 `200 + text/event-stream`、页内**零** 4xx、未读数兜底仍 200 |
| `frontend/e2e/11-erp-master-data.spec.ts`、`e2e/20-pages-home-error-task-flow.spec.ts` | 去掉「notice/stream 豁免」分支（原来把它当已知缺陷不计失败），现在它 4xx 就算本页坏 |
| `scripts/verify/e2e_isolated_instance.sh` | 修脚本自身缺陷：复用已有副本库时**跳过 pg_dump**（原来会往已有数据的库里再灌一份 prod 快照 → `pg_dump 失败（rc=255）`，整条流水线中断） |
| `docs/批次A-SSE鉴权口径.md` | 口径文档按新事实重写（配置项默认值 / 首启自检 / 帧序 / 前后端可独立回滚） |

### 为什么「两条腿」都要有

- 只修后端（配白名单）：`EventSource` 不再 401，但**每个页面加载仍会建一条 SSE 连接**，且浏览器对 SSE 有 6 连接/域名上限、断线自动重连，噪声源仍在；
- 只修前端（改带头流）：`?token=` 的 401 消失，但**第三方客户端 / 未来再有人用 `EventSource`** 仍会踩，而且没人知道白名单是空的；
- 两个都修 + 首启自检，才是「症状消失 + 根因有护栏 + 漏配可发现」。

## 3. 新发现：`fetch` 只认「首个字节」，所以必须补首帧（实测）

排查 39 页巡检时发现每页 `stream` 状态是 `null`（`waitForResponse` 15s 超时），但服务端日志明明有
`SSE 订阅请求已受理`。用 CDP 诊断（`scripts/diag_stream_events.mjs`）确认：

```
0.6s request GET http://127.0.0.1:5140/api/business/notice/stream
0.6s CDP.requestWillBeSent GET
（12 秒内没有 Network.responseReceived / response / requestfinished）
```

原因：**浏览器 `fetch()` 的 Promise 要等首个响应体字节才 resolve**（不是等响应头），而 axum 的
`KeepAlive` 第一条 `: keep-alive` 要等 `HEARTBEAT_SECS = 15` 秒才发 → 这 15 秒里前端 `onOpen` 不触发
（`connected` 一直是 false、轮询白跑），验收侧也「看不见这条 200」。

修法：`sse_hub::Subscription::into_sse()` 在业务流前串一个 `once(comment("connected"))` 首帧。
修完 `curl` TTFB ≈ **5ms** 就能拿到 `: connected`，巡检里 39/39 页都稳定观察到 `200`。

## 4. 验收项逐条对照

| 卡面验收项 | 结果 | 证据 |
|------------|------|------|
| ☑ **配置修好，贴配置** | `backend/config/config.toml`：`query_token_paths = ["/business/notice/stream"]`（默认生效，无需环境变量）；启动日志 `查询参数令牌白名单 endpoints=["/api/business/notice/stream"]`、`SSE 鉴权自检通过 endpoint=/api/business/notice/stream` | `probe_round1.txt` 第 ⑤ 段、`/tmp/p1fix6-iso/backend.log` L417/L529 |
| ☑ **39 个页面走一遍，控制台无 401（贴截图）** | **两轮**都是 `pages=39 / stream200=39 / stream401=0 / console401=0 / api4xxPages=0 / navErr=0` | `walk_round1.txt`、`walk_round2.txt`、`shots-r1/walk_r1.json`、`shots-r2/walk_r2.json`、`shots-r1/*.jpg`（39 张 ×2 轮，**零豁免**：`>=400` 全记） |
| ☑ **SSE 真的连上了（服务端有连接日志）** | 每轮探针 + 78 次页面加载都在服务端留下 `SSE 订阅请求已受理 user_id=…`（`cumulative 89 条`）；`curl` 侧 18s 窗口收到 `: keep-alive`；首帧 `: connected` TTFB 5ms | `probe_round1.txt` ⑥⑦、`probe_round2.txt` ⑥⑦ |
| ☑ **首启自检（卡面第 2 条）** | 把临时配置里的白名单删掉后启动：`WARN nik_web::app 查询参数令牌白名单为空…` + `WARN cloudpivot::bootstrap SSE 鉴权自检未通过…`（4/4 断言 PASS）；再把 `NIK_AUTH_QUERY_TOKEN_PATHS` 注入回去 → `自检通过`（环境变量覆盖生效） | `guard_missing_config.txt`、`env_override_proof.txt` |
| ☑ **git commit** | 见卡内汇报的 commit id | — |

额外（未在卡面、但用来证明「没把攻击面放大」）：`?token=` 打**非白名单**端点
（`/notice/unread-count`、`/system/auth/me`、`/notice/page`）**全部 401**；`/notice/stream` 无凭证 401 —— fail-closed 保持原样。

## 5. 复跑方式（全部可复现）

```bash
# 1) 隔离实例（生产快照副本，端口 18140/5140）
cd /opt/cloudpivot
E2E_ISO_DB=cloudpivot_sse6 E2E_ISO_BACKEND_PORT=18140 E2E_ISO_FRONTEND_PORT=5140 \
E2E_ISO_RUN_DIR=/tmp/p1fix6-iso \
E2E_ISO_BACKEND_BIN=/root/p1fix5c-target/debug/cloudpivot \
bash scripts/verify/e2e_isolated_instance.sh up

# 2) 鉴权探针（10 条断言）
bash docs/evidence/20261105-p1fix6-sse/scripts/sse_probe.sh r1

# 3) 39 页真机巡检（零豁免口径，两轮）
node docs/evidence/20261105-p1fix6-sse/scripts/page_walk.mjs "$PWD/shots-r1" r1

# 4) 首启自检实证（删掉白名单再启动）
bash docs/evidence/20261105-p1fix6-sse/scripts/guard_missing_config.sh

# 5) 改了断言的那条 e2e
cd frontend && E2E_BASE_URL=http://127.0.0.1:5140 E2E_USERNAME=admin E2E_PASSWORD="$(cat /tmp/p1fix6-iso/admin.pw)" \
  pnpm exec playwright test e2e/34-error-boundary.spec.ts --reporter=list
```

## 6. 偏差与遗留（不假完成）

1. **生产（144）未部署**：本卡所有实测都在 99 的隔离副本（`cloudpivot_sse6` = 生产库快照 + 独立端口）。
   发版后需要在 144 复核一次：`grep "SSE 鉴权自检通过"`（journalctl）+ 登录后浏览器控制台零 401。
   本次没有对生产做任何变更（不重启、不改 env、不写库）。
2. **39 页巡检是「逐页整页加载」**：等价于用户逐页点开（每页一次 `GET /notice/stream`）。SPA 内部跳转
   （不刷新）不会重建连接，实际噪声只会更少。
3. **截图是 JPEG（质量 55）**：39×2 张 PNG 会让仓里多十几 MB，证据目录用 JPEG 存；如需 PNG 可重跑脚本改 `type`。
4. **`?token=` 能力仍保留**（第三方客户端用）：本卡没有删后端这条通道，只是前端不再用 + 加了白名单与自检。
5. **单测**：`cargo test --lib business::notice::`（含 `sse_hub` 的频道/心跳/回收用例）见
   `backend_notice_tests.log`；前端 `pnpm typecheck` 见 `typecheck_round1.txt`（EXIT=0）。
