# 批次 A · SSE 实时推送鉴权口径（定稿）

> 端点：`GET /api/business/notice/stream`（权限码 `business:notice:mine`）
> 代码：`backend/src/business/notice/handler.rs::stream` + `backend/src/business/notice/sse_hub.rs`
> 框架侧：`backend/vendor/nik-web/src/middleware/auth.rs`（CODE-10 补丁，已内联进 vendored 源码）
> 状态：**定稿**（A7 定口径；**2026-11-05 P1-FIX-6 修订**：白名单改为配置项默认带值 + 首启自检 + 前端只走带头流）

## 1. 一句话结论

**实时通道只认 `Authorization: Bearer <token>`（请求头）。** `?token=<token>` 作为
**浏览器 `EventSource` 的兼容退路**保留，且**只**对白名单里的端点生效；白名单默认值就是
`/business/notice/stream`（配置项 `[middleware.auth].query_token_paths`），漏配时启动即告警。

前端两种姿势都受支持，**推荐并在仓内实际使用**的是前者：

| 场景 | 姿势 | 令牌位置 | 仓内实现 |
|------|------|----------|----------|
| **推荐（现网唯一通道）** | `fetch('/api/business/notice/stream', {headers:{Authorization:'Bearer …'}})` + `ReadableStream` 读流 | 请求头 | `frontend/src/utils/notice-stream.ts` → `stores/notice.ts#connect()` |
| 兼容（第三方 / 不支持自定义头的客户端） | `new EventSource('/api/business/notice/stream?token=' + encodeURIComponent(token))` | 查询串（需白名单） | 仓内**已不再使用**（P1-FIX-6 移除） |

## 2. 六条硬约束

| # | 约束 | 实现位置 | 说明 |
|---|------|----------|------|
| 1 | **仅白名单端点** | 配置项 `[middleware.auth].query_token_paths`；环境变量 `NIK_AUTH_QUERY_TOKEN_PATHS`（逗号分隔、**含** `/api` 前缀、**完全覆盖**配置项） | 解析唯一实现：`resolve_query_token_paths(prefix, from_config)`。默认白名单 = `["/business/notice/stream"]`（相对路径自动拼 `[router] prefix`，`!` 开头为绝对路径）。**两边都空 = 完全不接受查询令牌（fail-closed）** |
| 2 | **与头认证完全等价** | `nik-web/src/middleware/auth.rs`（CODE-10 补丁） | 两条来源汇入同一个 `validate_and_extract_claims`：签名 / `exp` / 令牌类型 / 登出黑名单全部照常生效，不是「另一套放行逻辑」 |
| 3 | **短期有效** | `[middleware.auth].access_token_expire_secs` | 只用 **access token**（refresh token 的 `typ` 校验过不了）；access token 过期即重连失败，前端此时刷新令牌后重连 |
| 4 | **日志脱敏** | 认证层只记来源；`request_log` 只记 `path` | 认证层不打令牌；`request_log` 打的是 `path=…`（不含 query）；业务侧 `stream` 也不打 `query.token`。**nginx 侧仍会按 `$request` 记录 query**，生产建议对 SSE 路径单独关掉 access_log 或改用 `$uri`（运维项，见 §5） |
| 5 | **不可伪造他人频道** | `stream` handler | 频道归属**只**取自令牌声明（`AuthClaims`）。请求里出现 `user_id` / `receiver_id` → **400 直接拒绝**（不是静默忽略），避免「以为订阅成功」与排障歧义 |
| 6 | **令牌形态受控** | `AuthService::query_token_of` | 只认 `&token=` 的**未编码**取值；含 `%`（疑似编码/二次封装）一律不认，避免歧义注入 |

## 3. 首启自检（P1-FIX-6 新增）

漏配白名单的症状是**静默**的：每个页面加载都刷一条 `401 GET /api/business/notice/stream?token=…`，
实时推送退化成 30 秒轮询（功能层面几乎看不出来）。所以把判定搬到启动日志里，两处：

| 落点 | 何时打 | 内容 |
|------|--------|------|
| `nik_web::app`（框架装配期） | 每次启动 | 白名单非空 → `INFO 查询参数令牌白名单 endpoints=[…]`；**为空 → `WARN 查询参数令牌白名单为空：浏览器 EventSource 类端点（SSE）将返回 401`** + 两种修法 |
| `cloudpivot::bootstrap::sse_query_token_guard`（业务自检） | 每次启动 | 判定用的是**与装配层同一个** `resolve_query_token_paths`，所以「日志说了什么」与「运行期采信什么」不会分叉。通过 → `INFO SSE 鉴权自检通过`；不通过 → `WARN SSE 鉴权自检未通过`（带 `endpoint=…`、`whitelist=…`、`env_var=…`） |

**只告警、不拦启动**：漏配只影响实时通道（站内信本身仍可用），不该成为部署阻塞项。
实证（把临时配置里的白名单删掉后启动）：`docs/evidence/20261105-p1fix6-sse/guard_missing_config.txt`。

## 4. 帧序（P1-FIX-6 新增首帧）

| 帧 | 内容 |
|----|------|
| **首帧**（建连即刻） | 注释帧 `: connected` —— 浏览器 `fetch()` 的 Promise 等的是**首个字节**而不是响应头，没有首帧就要等 15 秒后的第一条心跳才开始认「已连上」（`onOpen` 迟 15s、轮询白停 15s、验收侧也看不到这条 200） |
| 数据帧 | `event: notice` + `data: {notice_id, notice_no, notice_type, level, title, link_url, biz_type, biz_id, receiver_id, created_at}` |
| 心跳帧 | 每 `HEARTBEAT_SECS`（15s）一条注释帧 `: keep-alive`，防反向代理按空闲超时（常见 60s）切流 |

注释帧不会产出业务行为（`utils/notice-stream.ts` 的解析器把没有 `data` 行的块直接丢弃）。

## 5. 实测结论（2026-11-05 · P1-FIX-6 复测，见 `docs/evidence/20261105-p1fix6-sse/`）

| 用例 | 结果 |
|------|------|
| 无凭证 | `401 未授权访问，请先登录` |
| 合法 token（query）订阅 `/notice/stream` | `200 text/event-stream`，TTFB ≈ 5ms，首帧 `: connected`（**修复前 401**） |
| 合法 token（header）订阅 | `200 text/event-stream`（与 query 等价） |
| `?token=` 打非白名单端点（`/notice/unread-count`、`/system/auth/me`、`/notice/page`） | 全部 `401`（fail-closed，未放大攻击面） |
| 18s 订阅窗口 | 收到 `: keep-alive`（长连接存活） |
| 39 个页面逐页真机巡检（连跑两轮） | `stream=200` ×39、`stream401=0`、控制台 401 ×0、页面 4xx ×0 |

历史用例（A7 定口径时的原始证据，含伪造令牌 / 频道归属 400 / 日志脱敏）见
`docs/批次A-遗留修复-验证证据.md` 项⑥。

## 6. 部署要求（laoliu 统一做）

1. **不需要额外环境变量**：默认白名单已随 `config.toml` 下发（`query_token_paths = ["/business/notice/stream"]`）。
   若某个环境想收紧或覆盖，再注入 `NIK_AUTH_QUERY_TOKEN_PATHS=/api/business/notice/stream`（**含** `/api` 前缀）；
   注入即**完全覆盖**配置文件，写错（少前缀 / 多空格）会退回 401 —— 但此时启动日志会有 `WARN`，
   不再需要人肉逐页看控制台。
2. **前后端可独立回滚**：旧前端 + 新后端能用（新后端对 `?token=` 放行）；新前端 + 旧后端也能用
   （走请求头，不依赖白名单）。
3. **可选加固（nginx）**：对 `/api/business/notice/stream` 使用不含 query 的日志格式，或直接 `access_log off;`。
4. **框架补丁**：vendored `nik-web` 已是补丁后的源码（`scripts/patch-alun-web.sh` 仅作文档保留），
   改 `vendor/` 后 `cargo build` 即生效；若切回 registry 依赖，需重跑该脚本并 `cargo clean -p nik-web`。

## 7. 前端约定

- `frontend/src/utils/notice-stream.ts`：带头流读取器（`fetch` + `ReadableStream` + SSE 分帧解析）；
- `frontend/src/stores/notice.ts#connect()`：**只有这一条**实时通道（先挂轮询兜底，`onOpen` 后停轮询），
  断开/被拒则轮询并在一个间隔后重连；
- `frontend/src/api/business/notice/index.ts`：`buildNoticeStreamUrl()`（**不带令牌**）；`buildNoticeSseUrl()`
  已在 P1-FIX-6 删除 —— `EventSource` 姿势不再出现在仓内，只作为后端能力保留给第三方客户端。
