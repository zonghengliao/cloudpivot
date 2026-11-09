# N-4 证据：日志加 request_id / trace_id 贯穿

- 卡：`t_831f972c`（NEAR-FIX-4【N-4】日志加 request_id / trace_id 贯穿）
- 评审来源：评审 5.8「journald 结构化文本（时间/级别/target/message），**无 trace_id / 无请求 ID 贯穿**，跨服务定位靠时间戳」
- 主机：145 = 172.18.10.145（`/opt/cloudpivot`，临时实例 `n4-verify.service` @ `127.0.0.1:8036`，库 `cloudpivot_dev`，redis db 4）
- 采集时间：2026-11-10 03:42 CST
- 落地提交：`c395cc3c`（功能）→ 合并 `origin/main` 后的 `0422eff2`（已 push）

## 病（两条，互相关联）

| # | 现象 | 真因 |
|---|------|------|
| ① | 访问日志里 `"request_id":"-"` 恒为占位符 | `nik-web` 的中间件装配顺序是 `RequestLog` **在 `RequestId` 之外层**（`app.rs`：先 `layer(RequestIdLayer)` 再 `layer(RequestLogLayer)`，后加者更靠外）→ 访问日志读请求头时，RequestId 还没往里写 |
| ② | handler / SQL / 下游日志完全没有请求维度 | 框架 `RequestIdLayer` 只写请求头 + 响应头，**不建 tracing span**；`tracing_subscriber` 不会凭空给事件加字段 |

同时：框架生成的 ID 是 UUID v4 截断 16 字符，且响应头在 `X-Request-Id` 之外没有统一出口。

## 修法（不动 vendored 框架）

新增 `backend/src/middleware/request_context.rs`，用 `App::layer_from_fn` 注册为**链最外层**
（`router_extensions` 按注册顺序包裹，最后注册者最靠外）：

1. 解析 `X-Request-Id`：只采信 ≤128 字节、`[A-Za-z0-9._:-]` 的值，否则丢弃重生成 **UUID v7**
   （v7 前 48 位是时间戳 ⇒ 字典序即时间序，与仓内 `utils::request_id` / 各单据同口径）；
2. 把最终 ID **写回请求头** ⇒ 框架 `RequestLog`、`utils::request_id`、`operation_logs`、`sys_change_log`
   全链路同一个值（这就是 `-` 的修法，不必改 vendored 的中间件顺序）；
3. 开 span `request{request_id, method, path}` 并 `instrument` 后续整条链 ⇒ handler / model /
   SQL（`tokio_postgres`）/ 下游调用的事件全部带上同一个 ID；
4. 回写响应头 `X-Request-Id`；收尾日志带 `status` / `duration_ms`（探针端点 `/health`、
   `/health/deps`、`/metrics` 降级 DEBUG，避免健康检查刷屏）。

前端：`api/request-id.ts`（纯逻辑）+ `api/request.ts` 接线 —— 每个请求写 `X-Request-Id`
（`crypto.randomUUID()`，内网 http 无安全上下文时退化为 `web-<ts36>-<rand>`），失败时
`ApiError.requestId` 取**响应头那个值**并追加到 `message`（`…（请求 ID：xxx）`），
仓内 175 处 `showError(err.message)` 无需逐个改。

## 证据清单（全部为 145 上实跑输出）

| 文件 | 验的是什么 | 结论 |
|---|---|---|
| `00_summary.txt` | 本次实测的四个 ID + 命中行数 + 结论 | 全绿 |
| `01_login_headers.txt` / `01_login_body.json` | 真实 admin 登录，请求带 `X-Request-Id: n4-gw-20261110034219-1524434` | HTTP 200，响应头**原样回传同一个值**，拿到 492 字符的真实 access_token（响应体里的令牌**已脱敏**，只留长度与结构） |
| `02_headers_generated.txt` | 不带头访问 `/api/system/user/page`（带真实 token） | 响应头 `01a1c1d5-3aae-71fd-9a6b-3af824f19301`（UUID v7，**不是 `-`**） |
| `03_grep_client_id.jsonl` | `grep n4-gw-…` 该请求的全部日志 | **17 行**：`nik_web::middleware::request_log`（访问日志，`fields.request_id` 已非 `-`）、`tokio_postgres::prepare/query`（SQL 11 条）、`cloudpivot::system::auth::handler`（业务）、本层收尾、span close |
| `03_all_logs.jsonl` | 整个实例的全部日志（未过滤） | 无任何 `"request_id":"-"` |
| `04_id_consistency.txt` | 17 行里 `request_id` 的取值分布 | 只有 1 种取值；`"-"` 出现 **0** 次；日志来源 target 6 类 |
| `05_operation_logs.txt` | `operation_logs` 按该 ID 反查 | 命中 1 行，`request_id` 与响应头一致（登录 `200`），可与审计 join |
| `06_headers_malformed.txt` | 脏头 `X-Request-Id: bad id with space` | 被丢弃，换成服务端 ID `01a1c1d5-3abe-…`（防日志/头注入） |
| `07_backend_unit_tests.txt` | 本模块 8 条单测（采信/空白/注入/唯一性/探针判定） | 8 passed |
| `08_backend_full_lib.txt` | `cargo test --lib` 全量 | 4048 passed / 0 failed |
| `09_frontend_unit_n4.txt` | 前端 10 条单测（生成、解析、文案、接线契约） | 10 passed |
| `10_frontend_suite_and_build.txt` | 前端全量单测 + `pnpm build` | 1307 passed / 0 failed；build 成功 |

复现：`OUT=/opt/cloudpivot/docs/evidence/20261110-n4-request-id bash scripts/verify/n4_request_id.sh`
（脚本自己起临时实例、跑六条断言、结束时停掉实例；退出码 0 = 全绿）

## 明确的边界（没做 / 做不到）

| 项 | 说明 |
|---|---|
| 跨服务 trace（W3C `traceparent`） | 只做了**单服务内的 request_id 贯穿**。目前没有第二个服务需要串联（前端是静态站 + 同源反代）；将来接外部服务时，把 `X-Request-Id` 透传下去即可，ID 字符集已兼容 `traceparent` 的 trace-id 段 |
| SSE（`/api/business/notice/stream`） | `EventSource` 不允许自定义头，该连接的日志无客户端 ID（服务端仍生成自己的 ID） |
| 定时任务 / 启动钩子 | 无 HTTP 请求 ⇒ 无 request span，按任务名检索 |
| 跨域读取响应头 | 生产同源，无需 `Access-Control-Expose-Headers`；分域部署时才需要（框架 `CorsConfig` 无该键，属改 vendored 框架的范围，本卡不做） |
| 144 生产 | 本卡**未碰** 144（禁止项）。新二进制要等下一次 `scripts/deploy/deploy.sh` 才会让 144 的日志带上 ID；在那之前 144 行为与改造前一致（`-`），不会更差 |
