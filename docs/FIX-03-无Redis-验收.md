# FIX-03 · BUG-SYS-03「无 Redis 时 500 但业务已写入」验收证据

> 卡面：t_a4fbaefe（P2，来源：测试C 系统管理测试）
> 结论：**当前 main 已不存在「写库成功却回 500」的路径**（74712c2 已把缓存侧清理降级为 warn）；
> 本次补掉该缺陷的**第三段**——「失效失败 → 旧缓存留到 TTL 3600 s」并通过新增回归用例与
> 可复跑的 e2e 脚本把三条行为钉死。

---

## 1. 缺陷三段拆解（为什么只改一半不算修完）

| 段 | 症状 | 修复前 | 现在 |
|----|------|--------|------|
| ① 写库后缓存侧「强制下线」失败 | 菜单删除/角色授权回 **500**，而数据已提交 → 客户端重试 → 重复删/重复授权 | `force_logout_all_except()` 的 `Err` 被 `?` 抛出 | 74712c2：`revoke_after_permission_change()` 只 `warn`（`pending_compensation=true`），库侧 `token_version` 在同一事务里推进 |
| ② 缓存失效（`Cache::del`）失败 | 业务回 200，但**旧值留在缓存里**，TTL 3600 s | 只 `warn`，**没有任何补偿** → 删掉/改掉的菜单在缓存恢复后仍返回最长一小时 | **本次**：`shared/cache_gate` 记「失效欠账」，读缓存前先补失效，补不成直接查库 |
| ③ 完全无 Redis | 认证层 `auth_guard` fail-closed → 401，业务不会被执行 | 已如此 | 已如此（见 §4 的边界说明） |

段 ③ 是刻意保留的安全取向（`middleware/auth_guard.rs` 注释「CODE-16 B3：查不到即拒绝」）：
缓存不可用时**宁可让管理员重登，也不放行无法判定吊销状态的令牌**。因此今天在 HTTP 层
**不可能**再观察到「500 但业务已写入」——要么 200（缓存还活着），要么 401（缓存彻底不可用，
连业务都没进）。

---

## 2. 关键取证：只让 DEL 失败（缺陷最难复现的形态）

本机没有 `redis-server`，而缺陷真正可复现的形态不是「Redis 全停」（那会被认证层挡在 handler
之前），而是 **Redis 活着、只有这一次 `DEL` 失败**：

```
业务 UPDATE/DELETE 提交 → Cache::del 超时/报错 → 旧值仍在缓存 → 接口若回 500 就是「看到失败、实际成功」
```

`scripts/verify/fix03_fake_redis.py` 是一个可控假 Redis（内存键值 + 控制口
`die / live / del_fail / del_ok / dump / flush`），`del_fail` 只让 `DEL` 返回错误，其余命令正常——
精确定点到这一刻。

没有这层注入，「恢复后不会读到旧值」这条断言就是空跑（缓存里根本没有旧值）。脚本里的
A7 专门证明「注入有效、旧值确实还在缓存里」，A8 才证明「读路径绕过了它」。

---

## 3. 改动清单

| 文件 | 改动 |
|------|------|
| `backend/src/shared/cache_gate.rs`（新增） | `InvalidationGate`：`AtomicBool` 记账「上次失效失败了」，纯逻辑可单测；附 5 条用例 |
| `backend/src/shared/mod.rs` | 注册 `pub mod cache_gate;` |
| `backend/src/system/menu/model.rs` | 菜单树缓存：失效失败 → 记欠账 + `warn(cache_invalidation_pending=true)`；读缓存前补失效，补不成查库 |
| `backend/src/system/role/model.rs` | 启用角色下拉缓存：同款 |
| `backend/src/system/dept/model.rs` | 部门树缓存：同款（同族缺陷，顺手收口） |
| `backend/src/system/menu/tests.rs` | 新增 `n14_writes_succeed_without_cache`（建/改/删在无缓存下全成功 + 读路径回退查库） |
| `backend/src/system/role/tests.rs` | 新增 `r08_assign_menus_succeeds_without_cache`（授权/清空两次都成功且落库） |
| `scripts/verify/fix03_no_redis.sh`（新增） | 验收脚本：23 条断言，自带假 Redis 与隔离库/端口 | 
| `scripts/verify/fix03_fake_redis.py`（新增） | 可控假 Redis（RESP 子集 + 控制口） |

**缓存层接口签名一个都没动**（`Cache::get/set/set_ex/del` 原样），只改调用点与读路径判定。

---

## 4. 验收证据（原始输出）

### 4.1 单测：18 passed（含本卡新增 7 条）

```bash
SYS_DB_URL="postgres://alun:***@127.0.0.1:5432/cloudpivot_fix03" \
  cargo test --lib -j 2 -- --test-threads=1 \
    system::menu::tests::n1 system::role::tests::r0 shared::cache_gate
```

```
running 18 tests
test shared::cache_gate::tests::failed_invalidation_blocks_cache_reads ... ok
test shared::cache_gate::tests::mark_dirty_helper_matches_record_false ... ok
test shared::cache_gate::tests::new_gate_is_clean ... ok
test shared::cache_gate::tests::repeated_failures_keep_gate_dirty_then_clear ... ok
test shared::cache_gate::tests::successful_retry_restores_reads ... ok
test system::menu::tests::n13_permission_change_invalidates_tokens_in_db ... ok
test system::menu::tests::n14_writes_succeed_without_cache ... ok
test system::role::tests::r07_assign_menus_bumps_holders_token_version ... ok
test system::role::tests::r08_assign_menus_succeeds_without_cache ... ok
（其余 9 条为既有 n10–n12 / r01–r06）
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 1480 filtered out
```

### 4.2 e2e：23 PASS / 0 FAIL

```bash
REPO=/opt/cloudpivot FIX03_BIN=backend/target/debug/cloudpivot \
FIX03_DB=cloudpivot_fix03 FIX03_PORT=18150 FIX03_REDIS_PORT=6495 \
  bash scripts/verify/fix03_no_redis.sh
```

```
=== A1 健康检查 ===            [PASS] 健康检查（200）
=== A2 登录 ===                [PASS] 登录成功（access_token 长度 461）
=== A3 预热菜单树缓存 ===       [PASS] 菜单树读取（200） / [PASS] 缓存里出现 menu:tree:all
=== A4 建菜单 ===              [PASS] 新建菜单（200） / [PASS] 缓存里仍是含该菜单的旧树（键存在）
=== A5-A7 DEL 失败下删除 ===    [PASS] 删除菜单（缓存失效失败）必须 200 而不是 500（200）
                              [PASS] 库里已软删（deleted_at 非空）（0）
                              [PASS] 缓存里的旧树还在（注入有效，A8 不是空跑）
=== A8 恢复后不读旧值 ===       [PASS] 恢复后读菜单树（200）
                              [PASS] 已删菜单不再出现（旧缓存被绕过）
=== A9 DEL 失败下授权 ===       [PASS] 角色授权（缓存失效失败）必须 200（200）
                              [PASS] 授权绑定已落库（1）
=== A10 回归 ===               [PASS] 角色下拉（200） / [PASS] 菜单树（200）
=== A11 红线② warn ===         [PASS] 日志出现 cache_invalidation_pending 共 1 次
=== A12 完全无 Redis ===        [PASS] 无缓存时认证层 fail-closed 401（不是 500）
PASS=23 FAIL=0
```

红线②的原始 warn 行（`cache/gate` 不是静默）：

```
WARN cloudpivot::system::menu::model: 失效菜单树缓存失败（业务已提交，不影响本次结果）：
     下次读缓存前补失效，补不成则跳过缓存 error=ResponseError: simulated DEL failure (fix03)
     key="menu:tree:all" cache_invalidation_pending=true
```

### 4.3 卡面四条验收的对应关系

| 卡面 | 证据 |
|------|------|
| ① 停 Redis → 删除菜单 → **200**，数据正常删除 | A5/A6。注：完全停掉缓存时认证层会 fail-closed 401（§1 段③），因此「200」在 **Redis 活着但失效失败** 这一刻验证（更严格：业务已提交、缓存打不掉，正是缺陷本体） |
| ② 停 Redis → 角色授权 → **200**，权限正常写入 | A9（`sys_role_menu` 行数 1）+ 单测 `r08` |
| ③ 恢复 Redis → 缓存能读到新数据（不是旧缓存） | A7（旧值确实还在）→ A8（读路径绕过它） |
| ④ 有 Redis 时行为不变（回归） | A3/A10（正常热缓存命中 + 恢复后仍 200）+ 既有 n12/r01–r06 全绿 |

---

## 5. 复跑方式

```bash
# 1) 起隔离库（owner 必须是应用账号，否则迁移只有一行无细节的报错）
su postgres -c 'createdb -O alun cloudpivot_fix03'
# 2) 编 + 起一次让迁移跑完（看到 /health 200 再停，bootstrap 才会种 admin）
cd backend && cargo build && NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_fix03 \
  NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=*** \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  NIK_SERVER_LISTEN=127.0.0.1:18141 ./target/debug/cloudpivot
# 3) 单测
SYS_DB_URL="postgres://alun:***@127.0.0.1:5432/cloudpivot_fix03" \
  cargo test --lib -j 2 -- --test-threads=1 system::menu::tests::n1 system::role::tests::r0 shared::cache_gate
# 4) e2e（脚本自带假 Redis 与实例，端口/库名可覆盖）
REPO=/opt/cloudpivot FIX03_BIN=backend/target/debug/cloudpivot \
FIX03_DB=cloudpivot_fix03 FIX03_PORT=18150 FIX03_REDIS_PORT=6495 \
  bash scripts/verify/fix03_no_redis.sh
```

---

## 6. 遗留与边界（如实记录）

1. **无缓存时认证层 fail-closed 401**（`middleware/auth_guard.rs`）。这是 CODE-16 B3 的安全取向，
   本次**没有动**它：把黑名单查询失败改成放行，等于让「已吊销令牌」在缓存抖动时继续可用。
   代价是 Redis 彻底不可用时整个后台不可用（可登录，但登录后所有接口 401）。是否要改成
   「缓存失败 → 只放行白名单接口 / 只降级读接口」是一个**架构决策**，建议由架构师评估后单独出卡。
2. **`business/task/template.rs:398`、`business/task/handler.rs:2987`、`business/erp/stockin_handler.rs:3410`
   三处 clippy 红点（`manual_range_contains` / `question_mark`）是既有代码**，不在本卡改动范围内，
   本卡 4 个改动文件 0 告警（`cargo clippy --all-targets -- -D warnings` 的全部诊断路径见上）。
3. `cache_gate` 的欠账标记是**进程内**的：多实例部署时每个实例各自记账，各自跳过各自的旧命中，
   语义仍然正确（不会把旧值当成新值）；但「A 实例失效失败」不会让 B 实例跳过缓存——B 实例读到的
   是 Redis 里那份旧值。要根治需要把「失效失败」上报到 Redis 之外的共用介质，成本远高于收益
   （TTL 已从「最长 1 小时」降到「本实例下一次读」）。
