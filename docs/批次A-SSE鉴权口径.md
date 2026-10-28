# 批次 A · SSE 实时推送鉴权口径（定稿）

> 端点：`GET /api/business/notice/stream`（权限码 `business:notice:mine`）
> 代码：`backend/src/business/notice/handler.rs::stream` + 框架补丁 `scripts/patch-alun-web.sh`（CODE-10）
> 状态：**定稿**（A7 提出问题⑥：header token 与 query token 口径未与前端统一）

## 1. 一句话结论

**优先 `Authorization: Bearer <token>`；浏览器原生 `EventSource` 无法设置请求头时，才允许
`?token=<token>`，且只对通过环境变量显式声明的端点生效。**

前端两种姿势都受支持，推荐前者：

| 场景 | 姿势 | 令牌位置 |
|------|------|----------|
| 推荐 | `fetch('/api/business/notice/stream', {headers:{Authorization:'Bearer …'}})` + `ReadableStream` 读流 | 请求头 |
| 兼容（EventSource） | `new EventSource('/api/business/notice/stream?token=' + encodeURIComponent(token))` | 查询串 |

## 2. 六条硬约束

| # | 约束 | 实现位置 | 说明 |
|---|------|----------|------|
| 1 | **仅此端点** | `ALUN_AUTH_QUERY_TOKEN_PATHS` 环境变量（逗号分隔、含 `/api` 前缀） | 默认空 = 完全不接受查询令牌（fail-closed，与打补丁前行为一致）。生产 `/etc/cloudpivot/cloudpivot.env` 需加：`ALUN_AUTH_QUERY_TOKEN_PATHS=/api/business/notice/stream` |
| 2 | **与头认证完全等价** | `alun-web/src/middleware/auth.rs`（CODE-10 补丁） | 两条来源汇入同一个 `validate_and_extract_claims`：签名 / `exp` / 令牌类型 / 登出黑名单全部照常生效，不是「另一套放行逻辑」 |
| 3 | **短期有效** | `[middleware.auth].access_token_expire_secs` | 只用 **access token**（refresh token 的 `typ` 校验过不了）；access token 过期即重连失败，前端此时刷新令牌后重连 |
| 4 | **日志脱敏** | 认证层只记来源；`request_log` 只记 `path` | 认证层不打令牌；`request_log` 打的是 `path=…`（不含 query）；业务侧 `stream` 也不打 `query.token`。**nginx 侧仍会按 `$request` 记录 query**，生产建议对 SSE 路径单独关掉 access_log 或改用 `$uri`（运维项，见 §5） |
| 5 | **不可伪造他人频道** | `stream` handler | 频道归属**只**取自令牌声明（`AuthClaims`）。请求里出现 `user_id` / `receiver_id` → **400 直接拒绝**（不是静默忽略），避免「以为订阅成功」与排障歧义 |
| 6 | **令牌形态受控** | `AuthService::query_token_of` | 只认 `&token=` 的**未编码**取值；含 `%`（疑似编码/二次封装）一律不认，避免歧义注入 |

## 3. 实测结论（详见 `docs/批次A-遗留修复-验证证据.md` 项⑥）

| 用例 | 结果 |
|------|------|
| 无 token | `401 {"code":401,"msg":"未授权访问，请先登录"}` |
| 伪造 token（header） | `401 Token 无效或已过期` |
| 伪造 token（query） | `401 Token 无效或已过期` |
| 合法 token（header）订阅 → 发送通知 | `200 text/event-stream`，收到 `event: notice` 数据帧 |
| 合法 token（query）订阅 → 发送通知 | `200 text/event-stream`，收到 `event: notice` 数据帧（与 header 等价） |
| query token + `user_id=<别人的真实 userId>` | `400 不接受客户端指定频道归属（user_id / receiver_id），用户身份只取自令牌` |
| query token + `receiver_id=…` | 同上 `400` |
| 日志泄漏 | `run.log` 中令牌串出现 0 次、`token=` 出现 0 次；`request_log` 只记 `/api/business/notice/stream` |

## 4. 部署要求（laoliu 统一做）

1. **环境变量**（三套 env 都要加，值相同）：
   ```bash
   ALUN_AUTH_QUERY_TOKEN_PATHS=/api/business/notice/stream
   ```
   - 不加 → 查询令牌一律 401，前端自动退化为 30 秒轮询（功能不坏、实时性下降）；
   - 加错（少了 `/api` 前缀或多空格）→ 同样 401；启动日志会打印实际生效清单：
     `查询参数令牌白名单 endpoints=["/api/business/notice/stream"]（仅 EventSource 类端点；令牌不落日志）`。
2. **框架补丁**（`bash scripts/patch-alun-web.sh`）必须在构建前执行；`cargo` **不会**跟踪 registry 源码
   的 mtime，打完补丁要 `cargo clean -p alun-web` 再 build，否则跑的还是旧 rlib（实测踩过）。
3. **可选加固（nginx）**：对 `/api/business/notice/stream` 使用不含 query 的日志格式，或直接 `access_log off;`。

## 5. 前端约定

- `frontend/src/api/business/notice/index.ts::buildNoticeSseUrl()` 目前用 `?token=`（A7 实现，已被后端支持）；
- 若后端未配置白名单（返回 401），`stores/notice.ts` 会自动降级为轮询，**不需要**前端改代码；
- 后续如改成 `fetch` + `ReadableStream`，把令牌放回请求头即可，后端无需任何改动。
