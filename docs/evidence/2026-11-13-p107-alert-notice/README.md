# P1-07 证据：告警通道 `alert_check.sh` → 站内消息（`POST /internal/alert`）

> 卡面验收：**造一条异常 → 超管在「消息中心」看到该条**；单测落 1 条通知且收件人正确；
> 脚本加调用逻辑且失败不阻断主流程。**全部达成**，实测见本目录 00~09。

## 实测环境

| 项 | 值 |
|----|----|
| 主机 | 172.18.10.99（开发机，非 144/145） |
| 后端 A（验收口） | `./target/debug/cloudpivot`，监听 `0.0.0.0:18097`，profile `p107`（空库首启，**auth 关**，与 `config.toml` 默认一致） |
| 后端 B（消息中心） | 监听 `127.0.0.1:18098`，profile `p107auth`（临时把 `[middleware.auth] enabled` 打开，**跑完即删**） |
| 库 | 新建空库 `cloudpivot_p107_test`（`create database … owner alun`），首启自动跑完 **335 条迁移**，`admin` 账号 + `SUPER_ADMIN` 角色绑定 1 行 |
| 内网 token | 服务端注入 `NIK_ALERT_INTERNAL_TOKEN=p107-internal-token`（只为验「非回环 + token」分支） |
| 压测 / 触发 | `alert_check.sh --only disk --force`，`NIK_ALERT_DISK_THRESHOLD=1`（1% 必然触发） |

## 改了什么、为什么

| 文件 | 改动 | 原因 |
|------|------|------|
| `backend/src/business/notice/dto.rs` | 新增 `InternalAlertReq`（`level/title/content/source/idempotency_key`，`validator` 长度校验）+ `alert_level_of()`（`critical→ERROR` / `warning→WARN`）+ `DEFAULT_ALERT_SOURCE` / `ALERT_BIZ_TYPE` 常量 | 级别口径是纯函数，可脱库单测；长度校验前置，避免先落库再撞列宽 |
| `backend/src/business/notice/model.rs` | 新增 `super_admin_role_id()`（`role_code='SUPER_ADMIN'` 且未软删） | 收件人用 **ROLE 目标**由 `fanout` 在一条 `INSERT … SELECT` 里算（排除软删 / 停用 / 授权过期），这里是「角色码 → 角色主键」的唯一换算点 |
| `backend/src/business/notice/handler.rs` | 新增 `internal_alert` 处理器（`#[nik::no_auth("!/internal/alert")]`）+ 准入纯函数 `internal_caller_allowed`（定长比较）+ `deliver_internal_alert`（组 `NoticeDraft` 调 `dispatch`）+ 3 条单测（级别 / 准入 / 真库落库） | **复用 `dispatch`**（落库 + 扇出 + 计数 + 投递记录一个事务），不新写一套 `sys_notice` SQL；准入与落库分开，准入可脱库测 |
| `backend/src/lib.rs` | `App::post("/internal/alert", …)` 手动注册 | 宏路由会被 `[router].prefix` 嵌套成 `/api/internal/alert`；放在根路径才与 nginx「只反代 `/api/` 与 `/metrics`」配合，把「只允许本机」变成**部署侧也成立**（见下「为什么不放 /api」） |
| `scripts/ops/alert_check.sh` | 新增 `sink_notice()`（python3 生成 JSON → curl POST → 解析 `notice_id` / `receiver_count`）+ 派发链里排在 `ALERT.log` 之后、kanban 之前 + `--list-checks` 打印通道状态 + 自检加 2 条「失败不阻断」断言 | 卡面要求「调用内部接口而不是直接写库」；脚本不持有库口令，也不复制 `sys_notice` 的四张表约束 |
| `scripts/verify/p107_alert_notice_acceptance.sh`（新增） | 8 组 22 条断言 + 证据落盘（含脚本端到端） | 验收脚本化，换机器可复跑 |
| `docs/运维规程-指标端点与抓取.md` | 新增 §8「告警通道：`alert_check.sh` → 站内消息」 | 接口契约 / 准入 / 脚本开关 / 纪律必须留档，不能只活在代码注释里 |

**没有**改框架 `vendor/nik-*`：`no_auth` 的 `"!"` 绝对路径语义与 `App::post` 都是现成的。

### 三个刻意的决定

| 决定 | 为什么不选另一种 |
|------|------------------|
| **根路径 `/internal/alert`**（不是 `/api/internal/alert`） | 卡面写的是 `/internal/alert`。更关键：nginx 只反代 `location /api/` 与 `location /metrics`，放进 `/api/` 就挂到公网；而准入闸判的是「对端是否回环」，请求经 nginx 后对端恒为 `127.0.0.1` ⇒ **闸直接失效**。根路径的 `/internal/*` 在 nginx 上是 SPA 回退，压根到不了后端 |
| **只看对端地址，不看 `X-Forwarded-For`/`X-Real-IP`** | 那两个头客户端自己能写：任何能直连 8023 的人加一行 `X-Forwarded-For: 127.0.0.1` 就绕过。对端地址由内核给 |
| **`force = true`（不受 `notify.inbox` 偏好约束）** | 「关掉站内信」的收件人恰恰是最不想错过告警的运维 / 超管；被偏好静默吃掉等于通道失效 |
| **不带默认 `idempotency_key`** | 若默认按「source+title」生成，同一条告警恢复后再犯会被 `uk_sys_notice_idem` 永久吞掉。去重交给脚本的状态目录（窗口过后重试），接口只在调用方显式给键时保证幂等 |

## 验证结果

| 证据 | 命令 | 结论 |
|------|------|------|
| `00_health.json` | `curl :18097/health` | 实例存活（空库首启 335 迁移、`admin` + `SUPER_ADMIN` 已就绪） |
| `01_loopback.json` | `POST /internal/alert`（127.0.0.1，critical） | `200` + `code=0`，`notice_no=NTA…`、`receiver_count=1`、`channels=[{INBOX,DELIVERED,1}]` |
| `02_notice_row.txt` | `psql` 直查落库行 | `ALERT\|ERROR\|ROLE\|alert_check\|【验收】磁盘使用率 99%（P1-07）`；`sys_notice` 恰好 1 行；`sys_notice_receiver` 1 行 = `receiver_count`，且按 `role_code='SUPER_ADMIN'` 反查命中 1 |
| `03_lan_without_token.json` | `POST http://172.18.10.99:18097/internal/alert`（不带 token） | `403` + `code=30001`，且**库里 0 行**（被拒的请求没有副作用） |
| `04_lan_with_token.json` | 非回环 + `X-Internal-Token`（对 / 错各一次） | 对 → `200`；错 → `403` |
| `05_replay.json` | 同一 `idempotency_key` 重放 | `duplicated=true`，`sys_notice` 仍 1 行 |
| `06_bad_level.json` | `{"level":"fatal"}` | `400` + `code=10001`（文案点名 `critical / warning`） |
| `07_alert_check.out` | `alert_check.sh --only disk --force`（阈值 1%） | 退出码 0；日志 `站内消息: 已送达超管（notice=… 收件人=1）`；库里有该条 |
| `08_admin_message_center.out` | 超管登录 → `GET /api/business/notice/mine` | `unread_count=3`、`total=3`，3 行全是 `ALERT ERROR`（`alert_check` 的三次触发）⇒ **卡面「超管在消息中心看到该条」成立** |
| `09_acceptance_full.out` | `bash scripts/verify/p107_alert_notice_acceptance.sh 18097 …` | **22 PASS / 0 FAIL，exit=0** |
| `10_auth_enabled_loopback.json` | **auth 打开**的实例（`jwt_auth=true`）上 `POST /internal/alert` | `200` + `code=0`，`receiver_count=1` ⇒ `#[nik::no_auth("!/internal/alert")]` 在生产 profile 下确实免认证；同实例 `/api/internal/alert` 返回 **404**（经 nginx 的 `/api` 反代打不到它） |
| `11_edge_cases.out` | 超长 / 空 / 空白标题、缺字段、非法级别 | 超长与空标题 `422 + 422`（`#[validate]` 先拦）、空白标题 `400 + 10001`、缺字段 `400 + 400`、非法级别 `400 + 10001` |

### 单测

```
cargo test --lib business::notice                       → 67 passed / 0 failed（含新增 2 条纯函数用例）
ERP_DB_URL='postgres://alun:***@…/cloudpivot_p107_test' \
  cargo test --lib business::notice::handler::tests::internal_alert -- --test-threads=1 --nocapture
                                                        → 1 passed / 0 failed（真库：1 条 sys_notice、
                                                          收件人含超管账号、幂等重放不新增行）
```

> 真库用例的「不假绿」自查：把 `ERP_DB_URL` 指向不存在的库再跑，`--nocapture` 会打印
> `SKIP: ERP_DB_URL 连接失败…`；所以上面那次「ok 且无 SKIP」确实是跑过库的。

## 已知边界

- **`144/145 现网需要下次 deploy 才有这个接口**（脚本里已默认调用，接口不在 = 站内消息通道
  `warn` 一行后继续走 ALERT.log / kanban，不影响巡检）。
- 卡面写的 `biz_notice` 在本仓库不存在：`notice` 模块的表是 **`sys_notice` / `sys_notice_receiver`**
  （`migrations/business/V0043__create_notice.up.sql`），本条按实际表名验收。
- 「超管在消息中心」这一环是在**临时打开的 auth 实例**（`config-p107auth.toml`，跑完已删）上验的：
  基础 `config.toml` 的 `[middleware.auth] enabled = false` 是 dev 默认，prod profile 打开；
  auth 关闭时 `AuthClaims` 不存在，所有 `/api/...` 业务接口一律 403，与本次改动无关。
- 验收库 `cloudpivot_p107_test` 留在 99 供复跑。
