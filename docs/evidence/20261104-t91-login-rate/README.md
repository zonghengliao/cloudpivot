# t_91a8d427 · 登录限流计数不衰减（无 Redis 永久 429）—— 修复证据

日期：2026-11-04　执行：backend-developer　关联卡：`t_91a8d427`（来源 `t_1d60ed82` VUE-FLOW-3 回归）

## 1. 结论

| 验收项 | 结果 |
|---|---|
| 同一账号 + IP 连续 11 次登录，第 11 次 429 | ✅ 修复前后一致（限流阈值未被放宽） |
| 等 `LOGIN_RATE_WINDOW_SECS`(60) + 2 秒后再登录放行 | ✅ 修复后放行（修复前**永久 429**，见 §3.1） |
| 无 Redis（进程内缓存）/ 有 Redis 两种后端各跑一遍 | ✅ 各一份实机日志（§3.2 / §3.3） |
| 有覆盖该行为的单测 | ✅ 新增 5 条（业务侧）+ 2 条（`nik-cache` 侧）；`cargo test --lib` 2329 passed / 0 failed |

## 2. 根因（两层，缺一不可）

### 2.1 直接根因：`LocalCache::incr` 不看过期时间

`backend/vendor/nik-cache/src/lib.rs` 的 `LocalCache::incr`（修复前）：

```rust
async fn incr(&self, key: &str, delta: i64) -> Result<i64> {
    let mut guard = self.data.write();
    let entry = guard.entry(key).or_insert_with(|| CacheEntry { value: 0, expires_at: None });
    ...
}
```

过期清理只有「读时删（`get`/`exists`）+ 后台扫」两路，`incr` 两者都不走：**TTL 已到但尚未被清掉的条目会被继续累加**，
而它的 `expires_at` 早在过去 ⇒ 计数永久停在阈值之上。这与 Redis 的 `INCR` 语义相反
（Redis 对已过期 key 做 `INCR` 是当它不存在、从 0 开始）。

红测证据：`evidence/red-nik-cache-incr.log` —— 把该修复摘掉后
`test_incr_after_expiry_restarts_from_zero` 立刻失败，`incr` 返回 **6**（过期值 5 + 1），
断言期待 1。

### 2.2 放大条件：原实现只在 `count == 1` 时才写一次 TTL

```rust
let count = Cache::incr(store, &key, 1).await?;
if count == 1 && let Err(e) = Cache::set_ex(store, &key, &1i64, LOGIN_RATE_WINDOW_SECS).await { … }
if count > LOGIN_RATE_MAX_ATTEMPTS { return Err(429) }
```

只要计数键的 TTL 曾经失效（首次写 TTL 失败，或 2.1 的 `incr`）。此后再也不会回到 `count == 1`，
**没有第二次机会补 TTL** —— 账号被锁到进程重启。这正是线上「等 5 分钟也不恢复、重启立刻好」的形态。

## 3. 修复

### 3.1 业务侧：`check_login_rate` 改成「窗口起点键 + 计数键」，窗口到点自愈

`backend/src/system/auth/handler.rs`（`check_login_rate_on`，约 1616 行起）：

- 新增键 `login:rate:{用户名}:{IP}:window`，值是窗口首次尝试的 Unix 时间戳，TTL = `LOGIN_RATE_WINDOW_SECS`；
- 每次检查先 `Cache::get::<i64>(window_key)`：
  - 未命中 / 已过期 → **新窗口**：直接 `set_ex(计数键, 1, window)` + `set_ex(起点键, now, window)`，
    **不再对旧计数键做 `incr`**（残留值被覆盖，而不是继续累加）；
  - 命中 → 窗口内：`incr(计数键, 1)`，`count > 10` → 429；
- 缓存读失败 → 降级为「沿用计数键自身 TTL」；`set_ex` / `incr` 失败 → 放行（限流器故障不能挡登录，与修复前口径一致）。

因此**即使计数键残留成任意值**（无 TTL、超阈值），窗口一到也一定会被重置 —— 账号不可能被永久锁死。

### 3.2 框架侧：`LocalCache::incr` 过期即当不存在

`backend/vendor/nik-cache/src/lib.rs`（`incr`，约 345 行）：自增前先判 `expires_at`，过期则删除并计一次
`expired_cleanups`，再从 0 开始。语义对齐 Redis `INCR`，同时修掉所有「进程内计数 + TTL」场景的同类隐患
（会话缓存、其他限流点以后复用计数时不会再踩）。记录在 `backend/vendor/FORK-NOTES.md` §8。

### 3.3 单测

| 位置 | 用例 | 钉住什么 |
|---|---|---|
| `system/auth/handler.rs` tests | `login_rate_blocks_after_max_attempts` | 第 11 次必须 429（阈值不放宽） |
| 同上 | `login_rate_resets_after_window_expires` | **窗口过期后放行 + 计数从 1 重算**（本卡核心） |
| 同上 | `login_rate_ignores_stale_counter_without_window_key` | 复刻事故现场：无 TTL 的 57 次计数不得导致永久 429 |
| 同上 | `login_rate_is_keyed_by_username_and_ip` | 换 IP / 换账号不牵连 |
| 同上 | `login_rate_fails_open_without_cache_backend` | 无缓存后端时放行（且证明单测没污染全局缓存） |
| `nik-cache` tests | `test_incr_after_expiry_restarts_from_zero` | `incr` 过期键从 0 开始（旧实现返回 6） |
| 同上 | `test_incr_keeps_window_ttl` | `incr` 不刷新 TTL（固定窗口，不会被高频重试无限延长） |

业务侧单测用**独立 `LocalCache` 实例 + 1 秒窗口**（`check_login_rate_on` 把缓存与窗口参数显式传入），
不调用 `set_cache()` —— 否则 `system/menu/tests.rs` 的「无缓存（无 Redis）」用例会失真。

## 4. 实机验证（两种后端各一遍）

环境：`profile=test`、库 `cloudpivot_vf2`、客户端 IP 维度在直连场景解析为 `unknown`（见 §6）。
每次验证用**不存在的账号 + 错误口令**，只会产生 401 / 429，不触碰任何真实账号的锁定状态。

### 4.1 修复前（进程内缓存，18150，旧二进制）

`evidence/before-fix-local-cache.log`：第 11 次 429 后等 62 秒仍然 429 ⟶ 复现「永久锁死」。

### 4.2 修复后（进程内缓存，18150，新二进制）

`evidence/after-fix-local-cache.log`：

```
attempt#1..#10 http=401
attempt#11 http=429 {"code":10005,...}
after-62s  http=401   <== 放行
new-window#2..#10 http=401 ；new-window#11 http=429  <== 新窗口从 1 重新计数
```

### 4.3 修复后（Redis 后端，18151 → 独立 Redis 16379）

`evidence/after-fix-redis-cache.log`（Redis 是**独立实例**，未触碰生产 Redis）：

```
attempt#3  count=3  ttl=60s | window_start=1793787453 ttl=60s
attempt#11 http=429   count=11 ttl=60s
62s 后          残留键=[]        （Redis 按 TTL 自动删除）
after-62s http=401  count=1 ttl=60s   <== 放行且计数重置
new-window#11 http=429               <== 新窗口同样按 10 次封顶
```

生产（Redis）侧的结论：**修复前 Redis 的行为本来就是对的**（`INCR`+`SETEX`，键按 TTL 过期），
本卡的故障只出现在无 Redis 的进程内缓存；但 2.2 的「只有 count==1 才补 TTL」在 Redis 上同样有
「首次 SETEX 失败即永久锁死」的理论窗口，本次一并消除。

## 5. 回归

| 项 | 命令 | 结果 |
|---|---|---|
| 业务单测（全量） | `cargo test --lib -j 1` | **2329 passed / 0 failed**（基线 2324 + 新增 5） |
| `nik-cache` 单测 | `cd backend/vendor/nik-cache && cargo test` | **12 passed / 0 failed**（含新增 2） |
| clippy | `cargo clippy --all-targets -- -D warnings` | 见 §7 |
| fmt | `cargo fmt -- --check` | 见 §7 |
| 正常登录不受伤 | `POST /api/system/auth/login laoliu_test`，18150 与 18151 | 均 **200** |

## 6. 观察（不在本卡范围，建议另开卡）

1. **IP 维度在直连部署下是 `unknown`**：`utils::client_ip` 走框架 `extract_client_ip`，无 `X-Forwarded-For`
   /`X-Real-IP` 头时返回 `None`，限流键退化成 `login:rate:{用户名}:unknown` ⇒ 实际只按用户名限流。
   反过来，**客户端自带 `X-Forwarded-For`/`X-Real-IP` 就能换桶**（实测 `9.9.9.9` / `8.8.8.8` 各建一把键），
   公网直连部署下等于可绕过 IP 维度。生产在 nginx 后面时需确认「只信任反代写入的头」。
2. 本次验证用 `redis-server`（从 144 拷贝二进制 + `liblzf`）跑在 **16379**，与生产 Redis（144:6379）完全隔离。

## 7. 质量闸门原始输出

见本目录 `gates.log`（clippy `-D warnings` 与 `cargo fmt --check` 的退出码与末尾输出）。
