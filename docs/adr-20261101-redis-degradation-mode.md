# ADR-20261101：Redis 不可用时的后台形态 —— 令牌吊销落库，缓存退化为加速器

**日期**：2026-11-01
**状态**：accepted（架构拍板，实现另开卡）
**决策**：architect（t_246a5700）
**上游**：BUG-SYS-03 / FIX-03（commit `b3d5ba2`）、CODE-16 B3、CODE-16 B6、BUG-SYS-04
**取代**：无（本文件是对 `docs/会话吊销的耐久性边界.md` §2 末段"查不到即拒绝"取舍的正式复核）

---

## 0. 一句话结论

**选方案 3，但要做到位：把「哪条令牌被吊销」变成 PostgreSQL 侧的事实，认证层读库判定；Redis 从"吊销状态的唯一载体"降级为"读加速器 + 在线列表"。**

这样"Redis 不可用时后台是什么形态"这个问题本身消失了——**没有降级形态，和 Redis 正常时一样可用**。同时三处真实的失败被顺带收紧（见 §4）。

不选 1（现状）的理由不是"不够安全"，而是**它被方案 3 严格支配**：方案 3 的可用性更好，安全性不减，还多堵了两个洞。不选 2 的理由见 §2.2。

---

## 1. 现状复核：证据逐条带行号

本节全部结论来自读源码（含 vendored 框架）与实测，不是推断。

### 1.1 三条关键事实

| # | 事实 | 证据 |
|---|------|------|
| F1 | 项目侧复检层在缓存不可用时 **fail-closed 401**，且这是唯一拦口——框架自带的黑名单检查是 **fail-open** | `middleware/auth_guard.rs:61-74`、`vendor/nik-web/src/middleware/auth.rs:154-158` |
| F2 | 框架写黑名单用 `let _ = ...`，**写失败静默吞掉** | `vendor/nik-web/src/jwt.rs:207-222`（吞错在 219-220） |
| F3 | 会话守卫**逐请求点查** `sys_user`（`token_version` / `must_change_password`），**完全不依赖缓存** | `middleware/session_guard.rs:73-97`、`system/user/model.rs:769-786` |

F1 的框架原文（`vendor/nik-web/src/middleware/auth.rs:154-158`）：

```rust
if !is_ignore && let (Some(c), Some(jti)) = (&cache, &claims.jti) {
    let key = format!("token:blacklist:{}", jti);
    if let Ok(Some(_)) = nik_cache::Cache::get::<serde_json::Value>(c, &key).await {
        // ... 401
    }
}
```

读失败（`Err`）→ 条件不成立 → **放行**；`cache` 为 `None`（Redis 初始化失败）→ 整段跳过 → **放行**。
所以缓存一旦不可用，吊销名单在框架层等于不存在。项目侧 `auth_guard`（`middleware/auth_guard.rs:65-73`）因此把
"查不到"一律判为 `Err`，再由 `:107-116` 落成 401 —— 这就是 BUG-SYS-03 验收 A12 抓到的那条
`[PASS] 无缓存时认证层 fail-closed 401（不是 500）`。

### 1.2 由此推出的可用性结论（实测已证）

`try_cache() == None` 或 `Cache::get` 报错 → 所有受保护接口 401。登录本身能成功，因为登录路径的缓存写入
是**尽力而为**：

- `system/auth/handler.rs:1384`：登记在线会话失败只 `warn`（"登记在线会话失败（不影响登录结果）"）；
- `system/auth/handler.rs:1428`：登记 refresh 失败同样只 `warn`。

于是形成"**能登录、登录后什么都干不了**"的死循环形态。这不是猜测，是 FIX-03 的 A12 实测结果。

### 1.3 一个被现有文档漏记的洞（本次新发现）

`docs/会话吊销的耐久性边界.md` §5.2 主张"`noeviction` 下内存写满时写入会失败，后端把这些失败回传给调用方（5xx），
是可观测的失败而不是静默的降级"。**这条主张对 access 令牌不成立**：

```rust
// system/auth/handler.rs:552（logout 开头）
jwt.logout(&claims).await;          // ← 框架内部 let _ = Cache::set_ex(...)，失败无声
revoke_online_session(&claims).await;  // ← support/online/model.rs:101-108，del 失败仅 warn
```

`logout` 对 **access jti** 的拉黑完全依赖上面这两条静默路径。Redis 处于 `noeviction` 且内存写满时：
`set_ex` 返回 `Err` → 被框架吞掉 → access 令牌**没有进黑名单** → 客户端拿到 200（refresh 那段可能失败回 500，
但 access 那一半已经丢了）。**这是一个与 Redis 是否彻底宕机无关的、今天就在的静默 fail-open。**

结论：现状的"最安全"是有条件的——它只对"读不到"安全，对"写得进不去"不安全。

---

## 2. 三个候选的对比（按本仓真实约束打分）

| 维度 | 1 保持 fail-closed | 2 只放行读接口 | 3 黑名单落库兜底 |
|------|------------------|---------------|----------------|
| Redis 宕机时可用性 | **全站不可用**（除登录） | 读可用、写 401/503 | **全可用**（读写都行） |
| 已登出令牌在宕机窗口内是否可用 | 不可用（但系统也没了） | **可用 ≤15 min** | 不可用 |
| Redis 被清空/淘汰时已登出令牌 | 可用 ≤15 min（§3 已接受） | 可用 ≤15 min | **不可用**（收紧） |
| Redis 写满 OOM 时登出是否真的生效 | **不生效**（1.3 的洞） | 不生效 | **生效**（收紧） |
| 改密/停用/改角色 | 生效（库 `token_version`） | 生效 | 生效 |
| 代码量 | 0（不写） | +分支，长期维护一个安全语义分裂 | 1 表 + 1 任务 + 2 处读写；**净删**一条 fail-closed 分支 |
| 迁移 | 0 | 0 | V0176（已实测可用，见 §6） |
| 新增运维负担 | 必须新建告警（否则靠用户投诉发现） | 同左 + 读/写白名单要随迭代维护 | 1 个日清任务 + 表增长监控 |

### 2.1 为什么方案 1 被方案 3 严格支配

方案 1 相比方案 3 唯一"多"的东西是**零成本**。安全性上它并不更强：

- 对"Redis **读不到**"这一种情形，方案 3 同样拒绝（查库说得清）；
- 对"Redis **被清空/淘汰**"这一种情形，方案 1 与方案 2 完全一样——都放行已登出的 access ≤15 min
  （`docs/会话吊销的耐久性边界.md` §3 第 1 行明确记载并接受）；
- 对"Redis **写不进**"（OOM），方案 1 是静默失效（§1.3），方案 3 才修掉。

也就是说：**方案 1 用"全站不可用"换来的安全，恰好是方案 3 不用付这个代价就能拿到的部分。**
这正是"被支配"的定义。

### 2.2 为什么否决方案 2（只放行读接口）

1. **读路径才是外泄面。** 本仓的 ERP/财务数据（`biz_erp_payable*`、`biz_erp_stock*`）的主要风险是
   被读走，不是被改坏。方案 2 把"已登出/已被强踢的会话"精确地放进**唯一还开着的门**里。
   §3 早已接受"缓存丢失 → 已登出 access 可读 ≤15 min"，但那是在**缓存丢失（罕见）**前提下接受的；
   方案 2 把这个窗口扩展到**任何一次 Redis 抖动**，接受频次提高了几个量级。
2. **需要一套"读/写"分类器，且它会漂移。** 本仓目前 89 个 POST 端点全是写（无一个 POST 查询，
   实测 `grep`：`#[nik::post("...")]` 89 个、`#[nik::get("...")]` 186 个，POST 中零个 list/page/search），
   所以按 HTTP 方法分类**今天**可行。但降级期"未登记的新读端点"一律不可用——这属于可用性缺口，且
   随迭代单向变差（没人会记得回来补登记）。
3. **它会让验收脚本失去分辨力。** `scripts/verify/fix03_no_redis.sh:267-273` 的 A12 现在同时接受 401 和
   200（"无缓存时业务照常 200（缓存降级形态）"）。若选 2，这条断言把"降级"写成"通过"，
   以后任何一次"该 401 却 200"的回归都会被它盖住。选 3 则可以把 A12 收紧成严格的 200（见 §7）。
4. 它唯一的优势是**成本最低**。但方案 3 的成本已实测为：1 张表 + 1 个清理任务 + 2 处读写改动，
   并且**删掉**了 `auth_guard` 里那段 fail-closed 分支（代码量净减少）。省下的这点工，换不回
   "认证层永久保留一条会放宽安全语义的分支"的维护债。

---

## 3. 决策（DoD）

### 3.1 读路径：库为准，认证层不再以缓存为判据

`middleware/auth_guard.rs::check_revoked()`（当前 `:61-74`）改成：

```
无 jti                                   → Err（保持现状：无 jti 无法复检，401）
查库 SELECT expire_at FROM sys_token_blacklist WHERE jti = $1
  ├─ 命中且 expire_at > now()            → Ok(true)  → 401「登录状态已失效」
  ├─ 命中但已过期 / 未命中                → Ok(false) → 放行
  └─ 查库失败                             → Err       → 401（fail-closed 语义不变，判据从缓存换成库）
```

**移除**现有的两条缓存分支（`try_cache()` 为 `None`、`Cache::get` 报错）。
于是"Redis 不可用"不再是认证路径的故障形态。

正常路径**不读缓存**：缓存里那份黑名单只服务于框架自带的检查（`vendor/nik-web/.../auth.rs:154`），
读它反而会引入"库有、缓存没有"的判定分裂。这条取舍是本 ADR 的核心，别在实现时"优化"回去。

### 3.2 写路径：先写库（权威），再写缓存（尽力）

唯一写入口收敛到 **`shared/token_revocation.rs`（新增）**：

```rust
pub(crate) async fn revoke(
    jti: &str, user_id: Option<&str>, kind: BlacklistKind, reason: BlacklistReason, exp: i64,
) -> Result<(), Error>
pub(crate) async fn is_revoked(jti: &str) -> Result<bool, DbError>
```

**为什么放 `shared/` 而不是 `system/auth/model.rs`**：写入口的调用方跨两个业务容器——
`support::online::model`（登出 / 强踢 / 轮换）与 `middleware::auth_guard`（读）。
实测 `support/` 目前对 `crate::system::` **只有注释引用、没有代码依赖**（`grep -rn "crate::system::" support/`
仅 2 处，均为文档链接），把实现放 `system/auth/model.rs` 会新开一条 `support → system` 的代码边。
`shared/` 本来就是横切层，且已有"横切代码直连库"的先例（`shared/startup_guard.rs:750-797` 直接
`db().find_with_params(...)`）。放这里的收益：**零新增容器间依赖**。

顺序与失败语义：

1. **先** `INSERT INTO sys_token_blacklist ... ON CONFLICT (jti) DO NOTHING` —— 失败即 `Err`（吊销是安全动作，
   写不进就是没吊销，与今天 `blacklist_token` 的 500 口径一致）；
2. **再** 写 `token:blacklist:{jti}`（TTL = 令牌剩余有效期），失败只 `warn` + 固定字段
   `auth_cache_degraded=true`（沿用 FIX-03 里 `cache_invalidation_pending=true` 的告警口径，见
   `scripts/verify/fix03_no_redis.sh:258`）。**不影响返回码**——库侧已经生效。

改造点（保持既有调用点不变）：

- `support/online/model.rs:567 blacklist_jti` / `:559 blacklist_token` → 委托给 `shared::token_revocation::revoke`；
- `system/auth/handler.rs:552` 的 `jwt.logout(&claims)` **之后补一次显式** `revoke(access jti, ...)`——
  这是 §1.3 那个洞的直接修复；
- `system/auth/handler.rs:488-494`（刷新时作废旧 refresh，注释写明"写失败即 500"）→ 同样走 `revoke(..., kind=refresh, reason=rotate)`。

### 3.3 刷新路径（P1，与 P0 同批交付）

Redis 不可用时 `load_rotated` / `load_replay` 现在返回 500（`system/auth/handler.rs:1150-1182`），
意味着**宕机 15 分钟后所有用户必须重新登录**（access TTL = 900s，`config/config.toml:91`）。

P1 规则：`try_cache()` 为 `None` 时**跳过 `token:replay`（60s 幂等回放）与 `token:rotated`（复用检测）**，
其余照常（验签 → `ver` 比对 → 旧 jti 库侧拉黑 → 签发 → 写新登记尽力而为）。

安全性代价可以精确说出：**"一次性 refresh"退化为"统计保证"**——与
`docs/会话吊销的耐久性边界.md` §4 第 3 条 已经接受的边界**完全同一句话**，没有新增承诺缺口。
不做的代价是"每次 Redis 抖动 15 分钟后全员重登"，这在有 ERP 长表单录入的场景里体验很差。

### 3.4 为什么 `token:rotated` 不落库

卡片原文建议 `token:blacklist:*` 与 `token:rotated:*` **都**落库。**驳回后者**，理由是纯逻辑的：

刷新只有在**缓存可用**时才可能发生（轮换要写 `token:replay` / `token:rotated` / `online:refresh`）。
若缓存不可用，刷新在 P0 形态下直接 500、在 P1 形态下按 §3.3 跳过复用检测——**两种情况都不会写、也不会读
`token:rotated`**。所以库侧的 rotated 表在降级窗口里**永远没有读者**，是纯成本。
（它的价值只在"缓存被清空"的场景，而那个场景已被 §4 第 3 条 明确接受为可退化。）

---

## 4. 对 CODE-16 B3 安全承诺的影响：逐条，无一处放宽

B3 的承诺是"**查不到即拒绝**，绝不放行已吊销令牌"。下表逐条对照"今天"与"本决策后"：

| 攻击面 | 今天的行为 | 本决策后 | 变化 |
|--------|-----------|---------|------|
| 已登出 access，缓存正常 | 401 | 401（查库） | 不变 |
| 已登出 access，**Redis 读不到** | 401（全站也 401） | 401（查库），系统其余功能正常 | **可用性改善，安全性不变** |
| 已登出 access，**Redis 被清空/淘汰** | **可用 ≤15 min**（§3 已接受） | **401** | **收紧** |
| 已登出 access，**Redis 写满 OOM** | **可用**（框架 `let _=` 吞错，§1.3） | **401** | **收紧** |
| 强制下线（`force_logout_user`） | 依赖缓存枚举 + 拉黑 | 库侧有 `revoke` 行 → 401 | 收紧（同一机制） |
| 改密 / 停用 / 改角色 | 401（`token_version` 库侧） | 401（不变） | 不变 |
| 已删除账号的令牌 | 401（`sys_user` 行缺失） | 401（不变） | 不变 |
| refresh 复用（一次性保证） | 缓存 `token:rotated` 拦住 | 缓存正常时不变；缓存不可用时退化为 §4 第 3 条 已接受边界 | 不变（不新增承诺缺口） |
| JWT 密钥泄漏 → 自签 `*:*:*` | 401（`session_guard` 通配复核查库） | 不变 | 不变 |
| 登出接口本身 | 可能 200 但没生效（OOM） | 写库失败 → 500；成功则真生效 | **收紧** |

**结论：0 处放宽，3 处收紧。** B3 的"查不到即拒绝"语义原样保留，只是"查"的位置从 Redis 换成了
PostgreSQL —— 后者本来就已经是每个请求的硬依赖（F3），不是新增依赖。

### 4.1 唯一需要如实记账的代价

| 代价 | 量级 | 依据 |
|------|------|------|
| 每受保护请求 +1 次主键点查 | 实测 **0.024 ms / 3 shared buffers**（4.2 万行表） | §6 实测 |
| 每次吊销 +1 次 INSERT | 登出 / 强踢 / 轮换，量级 = 会话数 × (刷新频率) | 单行 60 字节级 |
| 表增长 | 有界：行只活到令牌 `exp`，日清任务回收 | §5.5 |
| DB 不可用时 | 401 fail-closed（今天 `session_guard` 本来就 500/401，整站已不可用） | `session_guard.rs:73-79` |

关于"+1 次点查"是否违背架构：**不违背**。`session_guard` 的设计注释（`middleware/session_guard.rs:27-35`）
已经明确选择了"每请求读库，安全性优先于一次索引查询"，理由是"不存在缓存未失效导致旧令牌仍可用的时间窗"。
本决策只是把同一条判据补到同一件事的另一半上——**per-jti 撤销是这套"读库换正确性"设计里唯一漏掉的一项**。

---

## 5. 落点（实现清单，可直接照做）

### 5.1 表

`backend/migrations/system/V0176__create_sys_token_blacklist.up.sql` / `.down.sql`（**本卡已交付并实测**，见 §6）。

编号依据：全仓共用一个号段（`migrations/system/V0105__create_sys_ai_agent.up.sql:13` 明确记载
"编号是全仓共享资源，`migrations/{system,business}` 共用一个号段"）；当前最大值是
`business/V0175__erp_payable_payment_approval`，**V0176 空闲**（实测 `find` 无 `V0176*`/`V0177*`）。

设计要点（每条都在迁移文件头写了"为什么"）：

- `jti varchar(64) PRIMARY KEY` —— 吊销查询的唯一入口，等值探测走主键；
- `user_id uuid` **故意不加外键** —— 吊销记录的寿命必须长于账号，否则物理删号会让仍在有效期内的
  令牌在故障窗口里"复活"；
- `reason` / `token_kind` **故意不加 CHECK** —— 诊断字段，给 CHECK 等于把"枚举没同步"升级成"登出 500"，
  正是 FIX-03 教训的反面；
- `expire_at timestamptz` = 令牌绝对 `exp`。语义比缓存更准：缓存 TTL 是"从写入起算剩余期"，
  库里记的是**绝对到期时刻**，所以行天然只活到令牌失效那一刻，不随写入时机漂移。

### 5.2 读写

| 文件 | 改动 |
|------|------|
| `backend/src/shared/token_revocation.rs`（新增） | `revoke(...)`（先库后缓存）与 `is_revoked(jti) -> Result<bool, DbError>`；`mod.rs` 加一行声明 |
| `backend/src/middleware/auth_guard.rs` | `check_revoked()`（`:61-74`）改查库；删掉 `try_cache()` / `Cache::get` 两条分支；`Err → 401` 保留（`:107-116`） |
| `backend/src/support/online/model.rs` | `blacklist_jti`（`:567`）与 `blacklist_token`（`:559`）委托 `revoke(...)`；调用点签名不变 |
| `backend/src/system/auth/handler.rs` | `:552` 后补显式 access-jti `revoke(...)`（修 §1.3 的洞）；`:488-494` 轮换作废改走 `revoke` |
| `backend/src/system/auth/handler.rs` | §3.3：`load_rotated` / `load_replay`（`:1150-1182`）在 `try_cache() == None` 时返回 `Ok(None)` 而非 500 |
| `backend/src/support/health.rs` + `lib.rs` | 新增 `GET /health/deps`（§5.4）；`/health` 不动 |
| `backend/Cargo.toml` + `config/config-{dev,prod}.toml` | 开 `metrics-prometheus` feature 与 `[metrics]` 段（§5.4 第一层告警） |

### 5.3 清理任务

`backend/src/task/mod.rs` 的 `Scheduler::register_builtin`（`:235` 起）里追加，写法照
`business/storage/cleanup.rs` 的内置任务模式（`TASK_NAME` / `TASK_CRON` / `register_builtin`）：

```rust
// 令牌吊销表日清：每天 03:50，与五张日志表（03:20/03:30/03:40）错峰
crate::system::auth::cleanup::register_builtin(self);
```

- `TASK_NAME = "token_blacklist_cleanup"`
- `TASK_CRON = "0 50 3 * * *"`
- 任务体：`DELETE FROM sys_token_blacklist WHERE expire_at < now() - interval '1 hour'`
  —— 留 1 小时宽限期吸收时钟偏差，**不要**用 `expire_at < now()`（刚过期就删，边界抖动会让
  "已过期"与"已删除"之间出现一段 401）。
- 注册点必须在 `register_builtin` 内，**不能**放 `bootstrap.rs`（`business/storage/cleanup.rs:9-12`
  记载的实测坑：框架调度器对注册表做一次性快照，`bootstrap.rs` 末尾追加会被随后的 `reload()` 清空）。
- 同步在 `task/mod.rs` 顶部的任务表注释里加一行（该表是运维查"有哪些定时任务"的唯一入口）。

### 5.4 健康检查与告警（无论选哪个方案都必须做）

现状：`GET /health` 只有进程存活（`support/health.rs:1-4` 明确"不鉴权、不查库，只反映进程是否存活，
避免下游依赖抖动导致探针误判并触发无谓重启"），且 `scripts/dev-*.sh` 把它当"应保持 200"用
（`scripts/dev-down.sh:65`、`scripts/dev-up.sh:165-168`）。

**决策：`/health` 保持 liveness，不反映缓存状态。** 理由不是"懒得做"，而是三个具体后果：

1. 它会变成重启风暴的扳机——一旦有人给 backend 加 `healthcheck`（compose 里 backend 目前没有），
   Redis 抖动 = 容器被反复重启 = **把可用性事故放大成数据不可用**；
2. `dev-status.sh` / `dev-down.sh` 依赖它判断"后端活着"，混合依赖状态后这些脚本的语义失效；
3. 只有一个端点的探针无法区分"进程挂了"与"依赖挂了"，而这恰恰是要区分的两件事。

**新增只读依赖探针 `GET /health/deps`**（根路径，与 `/health` 一样豁免前缀、免认证）：

```json
{ "status": "degraded",
  "db":   { "ok": true,  "latency_ms": 1.2 },
  "cache":{ "ok": false, "error": "connection refused" },
  "auth": { "revocation_source": "db", "cache_degraded": true } }
```

- 全部健康 → 200；任一依赖不健康 → **503**（这样它可以直接给 LB 当 readiness，而 `/health` 留给 liveness）；
- `revocation_source` 恒为 `db`——它存在的意义是让运维一眼确认"降级时吊销判定没有旁路"。

**告警落点（两层，都不需要新建监控系统）**

**第一层：框架自带的 Prometheus 导出，只是没开。** 实测发现 `nik-web` 内置了完整指标子系统
（`vendor/nik-web/src/metrics.rs:7-8`：`metrics` = HTTP 自动埋点，"无 recorder 时是 no-op"；
`metrics-prometheus` = 装 recorder 并挂 `/metrics`），配置面也已齐全
（`vendor/nik-config/src/lib.rs:348-368`：`[metrics] enabled / path / allow_ips`，`allow_ips` 默认空 = 仅回环）。
**但本项目两处都没开**：`backend/Cargo.toml:36-64` 的 nik features 里没有 `metrics-prometheus`，
`backend/config/*.toml` 里没有 `[metrics]` 段。

所以落点很小：

1. `backend/Cargo.toml`：nik features 加 `"metrics-prometheus"`；
2. `config/config-{dev,prod}.toml`：加 `[metrics] enabled = true`（`path` 默认 `/metrics`，
   `allow_ips` 填抓取端 IP；空 = 仅回环，符合"默认不对外暴露"）；
3. 代码侧只发 gauge，全部经 `metrics` crate 全局注册表，**未开 feature 时自动 no-op**（框架自己的注释就是这么写的），
   因此这几行即使 feature 关着也零风险：

```rust
metrics::gauge!("cloudpivot_auth_cache_degraded").set(if degraded { 1.0 } else { 0.0 });
metrics::gauge!("cloudpivot_token_blacklist_rows").set(rows as f64);
metrics::counter!("cloudpivot_token_blacklist_cleanup_deleted").increment(deleted);
```

`cloudpivot_auth_cache_degraded` 持续 60s = 1 → 告警。这条指标比"用户投诉"早，也比 grep 日志可靠。

**第二层：日志兜底（零依赖，feature 关着也生效）**：所有缓存写失败 / `try_cache()` 为 `None` 的位置
统一打固定字段 `auth_cache_degraded=true`。沿用 FIX-03 已建立的告警口径——
`scripts/verify/fix03_no_redis.sh:258` 就是靠 `grep -m1 "cache_invalidation_pending"` 断言"不许静默吞"的。
于是最简告警规则：`grep auth_cache_degraded=true`。

**既有欠账必须记账**：`docs/会话吊销的耐久性边界.md` §5.3 定的四条 Redis 侧阈值
（`used_memory/maxmemory` >70% 预警、>85% 严重、OOM 错误 >0 告警、`evicted_keys` 恒为 0）
**在本仓没有任何实现**——实测无采集端配置、无 exporter、无规则文件。它们是"§1.3 那个洞"的
唯一外部预警，所以不能一直空着：

- `docker/docker-compose.prod.yml:50` 引用的 `docs/session-revocation-durability.md` **不存在**
  （实际文件是 `docs/会话吊销的耐久性边界.md`），是断链，实现时顺手修掉；
- 四条阈值需要 `redis_exporter` + 规则文件，**另开运维卡**，不算在本决策内（本决策的 P0 已让
  "Redis 写着写着写不进去"不再影响吊销正确性，所以运维卡的紧迫性从 P0 降到 P1）。

### 5.5 表增长估算（给运维的容量口径）

吊销行只活到令牌 `exp`。稳态行数 ≈ 活跃会话数 × (1 access + 1 refresh) ≈ **30–40 MB 量级对应的行数**
（与 §5.3 的 Redis 估算同源），远小于 Postgres 任意一张业务表。日清一次即可。

---

## 6. 已实测的数据库证据

在 `cloudpivot_test` 的独立 schema（`t246a_scratch*`，用完已 `DROP SCHEMA ... CASCADE` 清理）上真跑，
PostgreSQL 18.6：

**① 迁移可执行、结构正确**

```
UP OK
 jti        | character varying(64)    | not null |
 user_id    | uuid                     |          |
 token_kind | character varying(8)     | not null | 'access'::character varying
 reason     | character varying(32)    | not null | 'unspecified'::character varying
 expire_at  | timestamp with time zone | not null |
 created_at | timestamp with time zone | not null | now()
Indexes: "sys_token_blacklist_pkey" PRIMARY KEY, btree (jti)
         "idx_sys_token_blacklist_expire" btree (expire_at)
```

**② 幂等 upsert**：同一 `jti` 连插两次 → `upsert_idempotent_rows=1`（登出重试 / 轮换重放不会报错）。

**③ 降级读路径的查询计划（4.2 万行，真实分布：4 万有效 + 2 千最近过期）**

```
Index Scan using sys_token_blacklist_pkey on sys_token_blacklist
  Index Cond: ((jti)::text = 'live-777'::text)
  Filter: (expire_at > now())
  Buffers: shared hit=3
  Execution Time: 0.027 ms          ← 命中且有效
未命中：Index Cond: jti='nobody-000'  Execution Time: 0.015 ms   Buffers: shared hit=2
```

**④ 过期行的判定语义**（`expire_at > now()` 必须是查询的一部分，否则删过期的执法会变成 401）：

```
 alive-1   | counts_as_revoked = t
 expired-1 | counts_as_revoked = f     ← 已过期 60s，不算吊销
 stale-1   | counts_as_revoked = f
```

**⑤ 清理任务真正只删该删的**

```
DELETE FROM sys_token_blacklist WHERE expire_at < now() - interval '1 hour';
DELETE 1                              ← 只删了 stale-1（过期 2 小时）
剩余：alive-1, expired-1              ← 未过期 + 刚过期（宽限期内）都保留
```

**⑥ 清理走索引（`SET enable_seqscan=off` 证明可用）**

```
Delete on sys_token_blacklist
  -> Index Scan using idx_sys_token_blacklist_expire on sys_token_blacklist
       Index Cond: (expire_at < (now() - '01:00:00'::interval))
       Execution Time: 0.061 ms
```

**⑦ 回滚零残留**：`-f ...down.sql` → `DOWN OK`，`pg_class` 计数 = 0。

> 一个实测注意点：若表里**过期行大量堆积**（我第一次用 50% 过期的脏数据测），规划器会改走
> `idx_sys_token_blacklist_expire` 做范围扫描 + `jti` 过滤。真实分布下（日清正常跑）**稳定走主键**。
> 因此：**清理任务是这条查询计划的隐式前提**，它挂了就必须有人知道 —— 这也是 §7 A19 要断言清理生效的原因。

---

## 7. 验收口径（可直接落成 `scripts/verify/` 断言）

前置：沿用 `scripts/verify/fix03_no_redis.sh` 的夹具（隔离实例 + `scripts/verify/fix03_fake_redis.py` 可控假 Redis，
`die` / `live` / `flush` 三个控制动词已经现成）。

**A12 必须收紧**（现在是 `:267-273` 的"401 或 200 都算过"）：

```bash
redis_ctl die >/dev/null
api GET "/system/menu/tree" - "$TOKEN"     # 真实存在的 GET（system/menu/handler.rs:41）
expect_code 200 "无 Redis 时受保护接口正常可用（不再是 fail-closed 401）"
redis_ctl live >/dev/null
```

新增：

| 编号 | 断言 | 判定 |
|------|------|------|
| A13 | 无 Redis 时 `GET`（读）与 `POST/PUT/DELETE`（写）各打一次 | 全部 **200**，且写操作在库里真的查得到 |
| A14 | 无 Redis 时登出 → 用同一 access 再请求 | 登出 **200**；再请求 **401**（库侧吊销生效） |
| A15 | 故障**前**已登出的令牌，Redis 挂掉后再用 | **401**（这是方案 2 会失败、方案 3 必须通过的那条） |
| A16 | 无 Redis 时 `POST /system/auth/refresh` | P0：**500 且未签发新令牌**；P1 完成后：**200 且旧 refresh 再用即 401** |
| A17 | 恢复 Redis 后 `redis-cli GET token:blacklist:<jti>` | 存在（缓存侧与库一致），且接口行为不变 |
| A18 | 停掉 PostgreSQL，带令牌请求 | **401 / 500**（fail-closed），**绝不 200** |
| A19 | 清理任务 SQL 断言 | 过期 2h 的行被删；未过期行与刚过期（<1h）的行保留 |
| A20 | Redis `die` 期间打 `/health` 与 `/health/deps` | `/health` **仍 200**；`/health/deps` **503** 且 `cache.ok=false`、`auth.revocation_source="db"` |
| A21 | Redis `die` 期间抓 `/metrics` | `cloudpivot_auth_cache_degraded` = **1**（开了 metrics 才断言；未开时跳过，不算失败） |

A18 是这套改动的安全底线：**读路径换库之后，库挂了不能变成放行**。

---

## 8. 非目标 / 后续

1. **不把 `token:rotated` 落库**（§3.4）。
2. **不动框架**（`vendor`/crates.io 的 `nik-web`）：它的 fail-open 由项目侧复检层覆盖，改框架会把
   "一个依赖"变成"一份 fork"，代价远大于收益。
3. **`online:*` 在线列表**在无 Redis 时仍然不可用（在线用户页为空 / 强踢枚举不到）。
   这不影响安全（强踢还可以靠 `token_version` 自增），属可接受的降级，**本次不修**。
   若将来要做，落点是 `sys_user_session` 表 —— 那是另一个决策，别和本 ADR 混。
4. **Redis 侧四条内存阈值告警的采集实现**另开运维卡（§5.4 已记账）。
5. **可选优化 P2**：把 `auth_guard` 的点查与 `session_guard` 的点查（`system/user/model.rs:769-786`）
   合并成一条 SQL，把每请求 2 次往返压成 1 次。仅在实测证明它是瓶颈时再做——当前 0.024 ms，
   **不在本轮做**（合并两个安全中间件是独立风险，不该搭在本次改动上）。

---

## 9. 复现命令（本文件所有实测数字）

```bash
# ① 现象：Redis 全挂 → 认证层 fail-closed 401（FIX-03 A12）
repo=/opt/cloudpivot FIX03_BIN=$repo/backend/target/debug/cloudpivot \
  bash $repo/scripts/verify/fix03_no_redis.sh 2>&1 | sed -n '/A12/,/汇总/p'

# ② 框架 fail-open 原文
sed -n '154,158p' /opt/cloudpivot/backend/vendor/nik-web/src/middleware/auth.rs
# ③ 框架写黑名单吞错原文
sed -n '207,222p' /opt/cloudpivot/backend/vendor/nik-web/src/jwt.rs

# ④ 本 ADR 的 DDL 实测（独立 schema，用完即删）
export PGPASSWORD=alun; SC=t246a_scratch
psql -h 127.0.0.1 -U alun -d cloudpivot_test -q -c "CREATE SCHEMA IF NOT EXISTS $SC;"
PGOPTIONS="-c search_path=$SC" psql -h 127.0.0.1 -U alun -d cloudpivot_test \
  -f V0176__create_sys_token_blacklist.up.sql
PGOPTIONS="-c search_path=$SC" psql -h 127.0.0.1 -U alun -d cloudpivot_test \
  -c "EXPLAIN (ANALYZE, COSTS OFF) SELECT expire_at FROM sys_token_blacklist WHERE jti='x';"
psql -h 127.0.0.1 -U alun -d cloudpivot_test -q -c "DROP SCHEMA $SC CASCADE;"

# ⑤ 编号占用核查
find /opt/cloudpivot/backend/migrations -name "V017[5-9]*"
```

---

## 附录 A：迁移全文（已实测，直接落 `backend/migrations/system/`）

> 本卡**只出决策与规格，不落实现**，因此这两个文件没有放进 `backend/migrations/`——
> 一旦放进去，`config.toml` 的 `auto_migrate = true`（`backend/config/config.toml:51-54`）会在
> 下一次后端重启时**自动建表**，而消费它的代码还没进来。实现卡落地时把这几个文件拷进去即可。
> 两份文件已在本机 PostgreSQL 18.6 上真跑过（up → 读写 → 清理 → down → 零残留，见 §6）。

`V0176__create_sys_token_blacklist.up.sql`：

```sql
CREATE TABLE IF NOT EXISTS sys_token_blacklist (
    jti        varchar(64)  PRIMARY KEY,
    user_id    uuid,
    token_kind varchar(8)   NOT NULL DEFAULT 'access',
    reason     varchar(32)  NOT NULL DEFAULT 'unspecified',
    expire_at  timestamptz  NOT NULL,
    created_at timestamptz  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_sys_token_blacklist_expire
    ON sys_token_blacklist (expire_at);
```

（完整文件带 20 余行"为什么这么设计"的表注释 `COMMENT ON`，落盘时以交付的 `.sql` 为准。）

`V0176__create_sys_token_blacklist.down.sql`：

```sql
DROP TABLE IF EXISTS sys_token_blacklist;
```

回滚注意：回滚本迁移等于**放弃**"Redis 不可用时仍能判吊销"的能力，认证层会退回全站 fail-closed 401。
回滚前必须先移除 `token_blacklist_cleanup` 任务代码，否则任务会持续报错。
