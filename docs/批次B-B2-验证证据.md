# 批次 B · B2「数据权限基座」验证证据

> 交付步：`B2 · 数据权限基座（business/shared/）`（架构文档 §2 / §5.4 / §7-B2）
> 验证日期：2026-10-28（CST）
> 验证机：`172.18.10.144`（`/opt/cloudpivot`，rustc 1.99.1 / edition 2024）
> 门禁状态：闸① **20** 处、闸② **21** 处（与 B0 基线逐字相同——因为本卡**只做基座、不接入**）

---

## 1. 交付物（文件指纹）

| 文件 | 行数 | 内容 |
|---|---|---|
| `backend/src/business/shared/principal.rs` | 1164 | `ScopedPrincipal`（字段全私有）+ `resolve(user_id)`（**一次往返**：3 表 join + 递归 CTE + `sys_role_dept` + `UNION ALL` 事实行）+ `merge_scope_rows`（纯函数合并/并集/降级）+ Redis cache-aside（键 `perm:user_scope:{epoch}:{uid}`，TTL 复用 `CACHE_PRINCIPAL_TTL_SECS=300`）+ 埋点 `cache_stats()` |
| `backend/src/business/shared/scope.rs` | 1109 | `ScopeKind`（1..5 ↔ 枚举，非法 → `DATA_SCOPE_INVALID`）、`RowFilter`（SQL 与参数同构不可拆）、`ScopeTarget` + `SCOPED_TABLES`（10 张登记表）、`filter_for`、`FILTER_MARKER`、`paginate`（**列表查询唯一入口**，自算 count 并剥 `ORDER BY`） |
| `backend/src/business/shared/guard.rs` | 434 | `bump_epoch()`（`INCR perm:user_scope_epoch`，O(1) 全量失效）、`invalidate_user_scope(s)`、`preflight_permission()`（闸 3 启动自检：10 张登记表的列 + `SUPER_ADMIN` 存在性）、`ensure_scope_grant_allowed()`（范围上限，S14） |
| `backend/src/business/shared/mod.rs` | 45 | 模块契约与接入示例（B3/B5 照抄） |
| `backend/tests/batchB_b2_scope.rs` | 416 | 真库集成测试（5 范围行数、count 可执行、参数顺序、回退查库、自检、S14） |
| `backend/tests/sql/batchb/b2_scope_fixture.sql` | 91 | 真库夹具（3 层部门树 + 5 个范围角色 + 12 用户 + 过期绑定），幂等 |

**附带改动（2 处，均为增量、不改编既有逻辑）**

| 文件 | 改动 |
|---|---|
| `backend/src/business/mod.rs` | `+ pub mod shared;`（1 行模块声明 + 1 行模块表文档） |
| `backend/src/shared/error_code.rs` | `+ 6100x 段 2 个码`：`DATA_SCOPE_INVALID=61001`、`DATA_SCOPE_EXCEED_LIMIT=61002`（B2 实际用到的两个；其余 10 个由 B3/B4/B6/B7 按 §5.3 码表补齐，**码值不改**）。放在 `error_code.rs` 而不是 `business/` 里，是为守住项目铁律「错误码统一在此定义，禁止散落魔法数字」 |

未改：`system/`、`support/`、`bootstrap.rs`、`Cargo.toml`、`scripts/check.sh`、任何既有查询。

---

## 2. 复现命令

```bash
# ① 单元测试（S1–S14 纯逻辑 + 缓存语义 + 5 范围 SQL 打印）
cd /opt/cloudpivot/backend
cargo test --lib business::shared -- --nocapture --test-threads=1

# ② 真 Redis（键形态 / 负载序列化 / TTL / epoch INCR）
B2_REDIS_URL="redis://127.0.0.1:6379/5" \
  cargo test --lib business::shared::principal::tests::real_redis_cache_roundtrip -- --nocapture

# ③ 真库（先在一个**专用**库上跑一次迁移建表 + 种子，本卡用 cloudpivot_b2v）
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export NIK_DATABASE_NAME=cloudpivot_b2v NIK_SERVER_LISTEN=127.0.0.1:8099
export NIK_REDIS_URL=redis://127.0.0.1:6379/5
timeout 30 ./target/debug/cloudpivot        # 只需看到「数据库迁移完成: 应用 42 个迁移」
export B2_DB_URL="postgres://cloudpivot:${NIK_DATABASE_PASSWORD}@127.0.0.1:5432/cloudpivot_b2v"
cargo test --test batchB_b2_scope -- --nocapture --test-threads=1

# ④ 门禁（闸①/闸② 口径与 scripts/check.sh 的 batchb_gates 逐字相同；
#    check.sh 会先跑 fmt/clippy/前端 typecheck，前端缺 pnpm 时会提前退出，
#    所以这里只复刻那两段循环）
cd /opt/cloudpivot
src=backend/src
BATCHB_SCOPED_TABLES="sys_user sys_dept sys_import_task sys_export_task sys_notice \
sys_file operation_logs audit_logs auth_logs security_logs"
# 闸①
grep -rn "paginate_with_params" "$src" --include=*.rs \
  | grep -v "business/shared/scope.rs" \
  | grep -v -E "^[^:]+:[0-9]+: *(//|\*)" >/tmp/_gate1.txt || true
echo "闸①命中：$(wc -l < /tmp/_gate1.txt)"
# 闸②
gate2=$(mktemp)
for t in $BATCHB_SCOPED_TABLES; do
  for f in $(grep -rl "FROM $t" "$src" --include=*.rs 2>/dev/null || true); do
    grep -q -e "scope::" -e "SCOPE-EXEMPT" "$f" \
      || echo "FAIL: ${f#"$src"/} 查询 $t" >>"$gate2"
  done
done
echo "闸②命中：$(wc -l < "$gate2")"
```

---

## 3. S1–S14 逐条结果（架构 §5.4）

单测 31 个：`cargo test --lib business::shared` → **31 passed; 0 failed**。

| # | 断言 | 结果 | 证据原文（实测输出） |
|---|---|---|---|
| S1 | `ScopeKind::from_i16`：1..5 正确，0/6/-1 → `DATA_SCOPE_INVALID` | ✅ | `[S1] 1..5 → All/Dept/DeptAndChild/SelfOnly/Custom；0/6/-1/MAX → 61001 ✓`（断言 `error.code == 61001 && error.status == 400`） |
| S2 | `filter_for(All)`：含 `1 = 1`、参数空、`is_noop()` | ✅ | `[S2] All → ` AND 1 = 1` params=0 is_noop=true ✓` |
| S3 | `filter_for(Dept)`：**恰好** 1 个 `?` | ✅ | `[S3] Dept → ` AND u.dept_id = ?` params=1 ✓` |
| **S4**（硬门槛） | `filter_for(DeptAndChild)` 是 `= ANY(…)` 形态、**不含 `WITH RECURSIVE`** | ✅ | `[S4] DeptAndChild → ` AND u.dept_id = ANY(?::text::uuid[])`（无 WITH RECURSIVE）✓`；另断言 SQL 里不出现 `sys_dept` |
| S5 | `SelfOnly` 用 `owner_col`：`sys_user`→`id`、`sys_import_task`→`create_by` | ✅ | `[S5] SelfOnly：sys_user → ` AND u.id = ?` / sys_import_task → ` AND t.create_by = ?` ✓` |
| S6 | `Dept ∪ Custom`：并集**去重**、SQL 仍 1 个 `?` | ✅ | `[S6] Dept ∪ Custom → dept_set=3 个（去重），片段 ` AND u.dept_id = ANY(?::text::uuid[])` ✓`（6 行事实里有 1 个重复部门 → 3 个） |
| S7 | `All ∪ 任意` → `All`（短路） | ✅ | `[S7] All ∪ 任意 → kind=ALL，片段 ` AND 1 = 1` ✓` |
| **S8**（硬门槛） | `SelfOnly ∪ Dept`：形如 `(dept = ANY(?) OR owner = ?)`、**恰好 2 个 `?`**、参数顺序 `[dept_array, user_id]` | ✅ | `[S8] SelfOnly ∪ Dept → ` AND (u.dept_id = ANY(?::text::uuid[]) OR u.id = ?)`，params=[数组(00000000-…-6301), user_id] ✓` |
| S9 | 超管短路（即使另有 `data_scope=4` 角色） | ✅ | `[S9] 超管 → 5 张登记表全部 `AND 1 = 1` ✓` |
| S10 | `data_scope=5` 但未配部门 → 降级 `SELF`，**不放行** | ✅ | `[S10] CUSTOM 空集合 → SELF，片段 ` AND u.id = ?` ✓` |
| S11 | 用户无部门 + `data_scope=3` → 降级 `SELF` | ✅ | `[S11] 无部门 + 本部门及以下 → SELF，片段 ` AND u.id = ?` ✓`（并断言 SQL 不含 `dept_id`，不生成 `dept_id = NULL`） |
| **S12**（硬门槛） | count 剥 `ORDER BY`，数据页保留 | ✅ | `[S12] count → `SELECT COUNT(*) FROM (SELECT u.* FROM sys_user u WHERE u.deleted_at IS NULL AND u.dept_id = ANY(?::text::uuid[])) AS _scope_count``<br>`[S12] data  → `SELECT u.* FROM sys_user u WHERE u.deleted_at IS NULL AND u.dept_id = ANY(?::text::uuid[]) ORDER BY u.created_at DESC LIMIT 20 OFFSET 0`` |
| S13 | 参数顺序契约：SQL 里第 1 个 `?` 对应 `RowFilter.params[0]` | ✅ | `[S13] sql=`SELECT u.* FROM sys_user u WHERE u.deleted_at IS NULL AND (u.dept_id = ANY(?::text::uuid[]) OR u.id = ?) AND u.username LIKE ? AND u.status = ? LIMIT 10 OFFSET 10` params[0]=数组 params[1]=user_id params[2..]=业务参数 ✓`（断言 `?` 个数 == 合并后参数个数，且片段出现在业务条件之前） |
| S14 | `ensure_scope_grant_allowed`：子集 → `Ok`；越界 → `DATA_SCOPE_EXCEED_LIMIT` | ✅ | `[S14] 子集→Ok；越界→403/61002（点名 00000000-…-9199）；超管与 ALL 不受限 ✓` |

**附加断言（架构未列但属于同一防线，均已覆盖）**

- 用户不存在（无 `ME` 行）→ `DbError::NotFound`：`[附加] 无 ME 行 → DbError::NotFound ✓`
- 无角色用户 → `SELF`（安全侧默认，不是 ALL）：`[附加] 无角色 → SELF（安全侧默认）✓`
- 表别名可替换且片段只锚定主体表：`[S5b] 别名 usr → ` AND usr.dept_id = ?`；无部门列的表降级 → ` AND l.user_id = ?` degraded=true ✓`
- `sys_dept` 上的 `SELF` 用「本人所在部门」这一行；无部门 → `AND 1 = 0`：`[S5c] sys_dept + SelfOnly → ` AND d.id = ?`；无部门 → ` AND 1 = 0`（fail-closed）✓`
- 空部门集合 + 部门范围：有归属列 → 仅本人；无归属列 → 拒绝全部：`[降级] Custom 空集合 → ` AND u.id = ?`（仅本人）/ sys_dept 无部门 → ` AND 1 = 0`（拒绝全部）✓`
- `base_sql` 里出现 `ORDER BY` / `LIMIT` → `DbError::InvalidValue`：`[守卫] base_sql 里出现 ORDER BY / LIMIT → DbError::InvalidValue ✓`
- `{FILTER}` 标记三条规则：`[守卫] FILTER 标记三条注入规则：有标记→替换；无标记无参数→追加；无标记有参数→报错 ✓`
- `SCOPED_TABLES` 自洽（无重复、至少一列可注入、标识符纯小写）：`[清单] SCOPED_TABLES 10 张表自洽 ✓`

---

## 4. 五种范围生成的 SQL 原文（评审证据，`--nocapture`）

基础查询（`sys_user` 列表，别名 `u`）：

```sql
SELECT u.*, d.dept_name FROM sys_user u
LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL
WHERE u.deleted_at IS NULL
```

| 范围 | 片段 | binds | count（**无 ORDER BY**） | data（含 ORDER BY） |
|---|---|---|---|---|
| ① ALL | `AND 1 = 1` | 0 | `SELECT COUNT(*) FROM (… WHERE u.deleted_at IS NULL AND 1 = 1) AS _scope_count` | `… AND 1 = 1 ORDER BY u.created_at DESC LIMIT 20 OFFSET 0` |
| ② DEPT | `AND u.dept_id = ?` | 1 | `… AND u.dept_id = ?) AS _scope_count` | `… AND u.dept_id = ? ORDER BY … LIMIT 20 OFFSET 0` |
| ③ DEPT_AND_CHILD | `AND u.dept_id = ANY(?::text::uuid[])` | 1 | `… AND u.dept_id = ANY(?::text::uuid[])) AS _scope_count` | `… ANY(?::text::uuid[]) ORDER BY … LIMIT 20 OFFSET 0` |
| ④ SELF | `AND u.id = ?` | 1 | `… AND u.id = ?) AS _scope_count` | `… AND u.id = ? ORDER BY … LIMIT 20 OFFSET 0` |
| ⑤ CUSTOM | `AND u.dept_id = ANY(?::text::uuid[])` | 1 | 同 ③ | 同 ③ |

> 五条 SQL 都**不含** `WITH RECURSIVE`（测试内逐条 `assert!`），也都不含 `sys_dept` 子查询或 `IN (SELECT …)`（部门集合已在应用层解析完）。

---

## 5. 缓存：命中 / epoch 失效 / 回退查库

### 5.1 逻辑行为（注入计数假实现，走的是**生产同一段代码** `resolve_with`）

```
[缓存] 1st miss→db(1), 2nd hit(hits=1), bump_epoch → 3rd miss(db_loads=2)
       ✓ stats=ScopeCacheStats { hits: 1, misses: 2, db_loads: 2, cache_unavailable: 0 }
[回退] 缓存不可用 → 查库成功，kind=DEPT dept_set=1 ✓
       stats=ScopeCacheStats { hits: 0, misses: 0, db_loads: 1, cache_unavailable: 1 }
[epoch] 缓存不可用时 bump_epoch() → None（告警 + TTL 兜底）✓
[键] perm:user_scope:{epoch}:{uid} ✓
[失效] 缓存不可用 → invalidate_* 静默跳过 ✓
```

断言细节：

- 第 1 次：`misses=1, db_loads=1`，数据源被调用 1 次，缓存写入 1 次；
- 第 2 次：`hits=1`，`db_loads` **仍为 1**、数据源**仍只被调用 1 次**（真命中，不是「命中但重算」）；
- `bump_epoch`（等价 `INCR perm:user_scope_epoch`）后第 3 次：`misses=2, db_loads=2`（键里的 epoch 变了）；
- 缓存不可用：`hits=0, misses=0`（根本没问缓存）、`db_loads=1`、`cache_unavailable=1`，**结果正确**（`kind=DEPT`、`dept_set=1`）——Redis 故障不会让列表报错。

### 5.2 真 Redis（`redis://127.0.0.1:6379/5`）

```
[真Redis] INCR epoch 1→2；键 `b2:test:526517:user_scope:1:00000000-0000-0000-0000-000000007401`
          TTL=300s；读回 serde 一致；新键未命中 ✓
```

覆盖：`epoch` 键不存在 → 0；`INCR` 1→2；`SETEX` 写入后 `TTL ∈ (0, 300]`；读回 `CachedScope` 的 `kind/dept_ids/self_only` 与写入一致；epoch 递增后新键必然未命中（旧键靠 TTL 回收）。

> **诚实说明**：本卡无法在测试进程里初始化**全局** Redis 缓存（`nik` 门面只 re-export `Cache`/`LocalCache`，而 `set_cache()` 需要 `SharedCache` 类型，未被转发；`create_cache` 亦未导出）。所以「真 Redis 命中 → 0 次查库」的**端到端**验证留给 B3/B4：它们把 `resolve` 接到 HTTP 路径后，用 `App::start` 初始化缓存即可复验。B2 交付的是「命令语义 + 键/负载/TTL 约定」（本段）+「命中/失效/回退逻辑」（§5.1）+「回退路径在真库上正确」（§5.3）。

### 5.3 真库上的回退查库（测试进程**没有**缓存 → 正是生产「Redis 挂了」的分支）

```
[回退查库] 埋点 ScopeCacheStats { hits: 0, misses: 0, db_loads: 10, cache_unavailable: 10 }
```

10 次 `resolve` 全部查库、全部记录「缓存不可用」，且 5 种范围 + 并集 + 3 条降级 + `sys_dept` 裁剪的行数断言**全部通过**（见 §6）。

---

## 6. 真库验收（`cloudpivot_b2v`，夹具 `tests/sql/batchb/b2_scope_fixture.sql`）

部门树：`B2-D1` → `B2-D1-1` →（`B2-D1-1-1`, `B2-D1-1-2`）；另一棵树 `B2-D2` → `B2-D2-1`。
用户 12 个（11 个夹具 + 种子 `admin`）。

```
  admin      kind=ALL            dept_set=0  self_only=false own_dept=NULL
  u_all      kind=ALL            dept_set=0  self_only=false own_dept=b2000000-…-0001
  u_dept     kind=DEPT           dept_set=1  self_only=false own_dept=b2000000-…-0002
  u_child    kind=DEPT_AND_CHILD dept_set=3  self_only=false own_dept=b2000000-…-0002
  u_self     kind=SELF           dept_set=0  self_only=true  own_dept=b2000000-…-0005
  u_custom   kind=CUSTOM         dept_set=2  self_only=false own_dept=b2000000-…-0006
  u_multi    kind=DEPT           dept_set=1  self_only=true  own_dept=b2000000-…-0002
  u_nodept   kind=SELF           dept_set=0  self_only=true  own_dept=NULL
  u_norole   kind=SELF           dept_set=0  self_only=true  own_dept=b2000000-…-0001
  u_branch   kind=SELF           dept_set=0  self_only=true  own_dept=b2000000-…-0005
```

`sys_user` 可见行数（`scope::paginate` 的 `total`，精确相等断言）：

| 用户 | 期望 | 实测 | 说明 |
|---|---|---|---|
| `admin` | 12 | 12 | 超管短路 |
| `u_all` | 12 | 12 | `ALL` 短路（且 `dept_set` 被清空） |
| `u_dept` | 3 | 3 | 本部门 `B2-D1-1` |
| `u_child` | 5 | 5 | 子树 3 个部门内的 5 个用户 |
| `u_self` | 1 | 1 | 仅本人 |
| `u_custom` | 3 | 3 | `B2-D2` + `B2-D2-1` |
| `u_multi` | 3 | 3 | `DEPT ∪ SELF` 并集（本人在部门内，不放大） |
| `u_nodept` | 1 | 1 | 无部门 → 降级 SELF |
| `u_norole` | 1 | 1 | 无角色 → 降级 SELF |
| `u_branch` | 1 | 1 | **过期绑定不算持有** → 降级 SELF |

其它真库断言：

- `sys_dept` 也按范围裁剪：`u_child` → 3 个部门；`u_self`（SELF）→ 1 个（本人所在部门那一行）；
- count 语句**可直接执行**且与 `paginate().total` 相等（原文）：
  ```
  [S12] 真库 count 语句（无 ORDER BY，可执行）：
    SELECT COUNT(*) FROM (SELECT u.*, d.dept_name AS dept_name FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL WHERE u.deleted_at IS NULL AND u.dept_id = ANY(?::text::uuid[])) AS _scope_count
  [S12] 片段：` AND u.dept_id = ANY(?::text::uuid[])` binds=1
  ```
- 参数顺序契约在真驱动上成立（`{FILTER}` + 业务参数）：
  ```
  [S8/S13] FILTER 标记 + 业务参数 `AND u.username LIKE ?` → total=2 rows=["b2_u_leaf1", "b2_u_leaf2"] ✓
  [S13] 缺 {FILTER} → invalid value: base_sql 带 1 个业务参数但没有 {FILTER} 标记：
         片段追加到末尾会让 `?` 顺序与参数顺序错位（架构 §2.6 参数顺序契约） ✓
  ```
  > 这是最硬的一条：若片段参数与业务参数错位，`text` 参数会去绑 `uuid[]`，驱动当场报错。
- 启动自检（闸 3）：`[自检] 数据权限自检通过：10 张登记表的部门列/归属列均存在，SUPER_ADMIN 角色存在`
- 范围上限（S14）真库版：`[S14] 子树内→Ok；跨树 B2-D2 → 新数据范围超出你自身的可见部门范围，越界部门：b2000000-…-0005 ✓`

---

## 7. 回归与门禁

```
cargo fmt --check                     → 通过（无 diff）
cargo clippy --all-targets -- -D warnings → Finished（0 warning / 0 error）
cargo test                            → 289 passed（单元）+ 1（batchB_b2_scope）+ 7（impexp_worker）
                                        + 2（s3_minio）= 299 passed / 0 failed
                                          基线 258 → 289 = 新增 31 个单测（C7 要求）
```

批次B 两条门禁（口径与 `scripts/check.sh` 的 `batchb_gates` 逐字相同）：

```
==> 批次B 闸①：分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
    闸①命中：20 处（B0 基线 20）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
    闸②命中：21 处（B0 基线 21）
==> 门禁汇总：闸①=20 处  闸②=21 处
```

**两个数都没变，这正是本卡的验收点**：门禁数下降才代表「接入发生了」，那是 B3/B5 的工作。本卡新增的 `business/shared/*.rs` 与 `tests/batchB_b2_scope.rs` 都不在闸② 的 FAIL 清单里（它们含 `scope::`），也没有为了让门禁通过而改 `scripts/check.sh`。

---

## 8. 与架构文字的 4 处实现说明（都写在代码注释里，逐条可查）

1. **`= ANY(?)` 的实际形态是 `= ANY(?::text::uuid[])`**（架构 §2.1 表格写的是 `= ANY(?)`）。
   原因：本栈 PG 驱动**不支持数组参数绑定**——`nik-db-driver-postgres-0.4.2/src/convert.rs`
   的 `value_to_pg_param` 对 `Value::Array` 直接返回 `unsupported feature: array binding`。
   实测探针（B2 现场跑过）：
   ```
   A(array-literal double cast) -> Some(Ok(12))     ← dept_id = ANY(?::text::uuid[]) + 文本数组字面量
   B(string_to_array)           -> Some(Ok(12))
   C(Value::Array)              -> unsupported feature `array binding` for dialect `postgres`
   ```
   采用 A：应用层把部门集合编码成 PG 文本数组字面量 `{uuid,uuid}`（排序后编码，保证同一集合字节级相同），
   `::text::uuid[]` 让占位符类型推断落在 `text`。**不违反**「不在列上做函数包装」的反模式
   （§5.5 禁止的是 `dept_id::text = ?`，本模块是参数侧 cast，索引照常可用）。

2. **`paginate` 的注入位置用 `{FILTER}` 标记**（架构 §2.6 的 `page_users` 形态里已有这个标记）。
   片段参数**恒前置**于业务参数，所以片段必须在业务条件之前；带业务参数却没写标记 → 直接报错，
   而**不是**把片段追加到末尾（那会造成 SQL 里 `?` 顺序与参数顺序静默错位）。

3. **`CachedScope` 里的 ID 用 `String`**：本栈 `uuid::Uuid` 未开 serde feature
   （项目里所有缓存 DTO 都这样，例如 `system/dept/dto.rs::DeptNode` 的 `id: String`）。
   内存形态 `ResolvedScope` 保持强类型 `Uuid`，只在缓存边界转换一次。

4. **登记表的真实列名以 `information_schema` 实测为准**（架构 §2.5 的注要求如此）：
   `operation_logs` / `audit_logs` 的归属列是 **`user_id`**（架构写的 `operate_by` /
   `operator_id` 是笔误）；`sys_notice` 用 `sender_id`；`sys_file` 用 `uploaded_by` 且**无部门列**
   （部门形态需 join `sys_user`，P1/B5）。`preflight_permission()` 会把这 10 张表的列在启动期再校一遍。

---

## 9. 本卡没做的事（边界）

- **不接入任何既有查询**：`page_users`、部门树裁剪、日志/导入导出/公告/文件列表仍是原状
  —— 这是 B3（底座接入）与 B5（其它登记表）的工作，也是门禁数保持 20/21 的原因。
- 不加 `bootstrap` 自检调用（`preflight_permission` 已就绪，B3 接 1 行）、不改 `Cargo.toml`（无需新依赖）。
- 缓存失效矩阵的 8 条**写点接线**（用户角色变更、角色 `data_scope` 变更、部门树 4 个写点）
  需要改 `system/`，属 B3；B2 已提供 `bump_epoch` / `invalidate_user_scope(s)` 三个函数。

---

## 10. 三方同步（提交后核对）

流程（与 B0/B1 同）：`commit` → `push origin main`（bare `/opt/git/cloudpivot.git`）→
回 `/opt/cloudpivot` 再 `git merge --ff-only origin/main` → 核对 `git rev-parse HEAD origin/main` 相等。

- 功能 + 测试提交：**`577c070`** `feat(permission): B2 数据权限基座 —— ScopedPrincipal + scope::paginate + epoch 缓存`
  （8 文件 +3274 行）
- 本证据文档提交：紧随其后（提交后实测的 sha 与 `HEAD == origin/main` 输出记录在本卡完成摘要里）
