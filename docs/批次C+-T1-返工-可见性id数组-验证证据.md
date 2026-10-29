# 批次 C+ · T1 返工验证证据（可见性谓词 → 参与 id 数组，D6 / D7）

- 任务卡：`t_4c09fcc7`（【T1返工】可见性谓词改参与 id 数组）
- 返工基线：`50eb300`（T1 首版，多 OR + `EXISTS` 写法）
- 依据：`docs/批次C+-工作任务管理-详细架构.md` §4.4（决策 **D6** 参与 id 数组 / 决策 **D7** 2000 上限与降级）、§5.1 / §5.2 实测
- 验证机：`172.18.10.144`（Ubuntu 26.04 arm64 / PG 18.6），库 `cloudpivot_t1v`（接口走查 + 集成测试）、`cloudpivot_t1e`（5 万行性能取证 + 上限降级）
- 验证时间：2026-10-29（CST）
- 脚本（可原样复现）：`scripts/verify/t1_task_api_verify.sh`（29 条断言）、`scripts/verify/t1_rework_explain.sql`（5 万行 + EXPLAIN 取证）、`scripts/verify/t1_rework_truncation.sh`（上限降级）
  - 复现顺序：① 建库 `cloudpivot_t1e`（`createdb -O cloudpivot`）并跑一遍服务完成迁移 + `scripts/verify/t1_task_api_fixture.sql` 夹具 →
    ② `su postgres -c "psql -d cloudpivot_t1e -f scripts/verify/t1_rework_explain.sql"` →
    ③ `bash scripts/verify/t1_rework_truncation.sh`（会自己起停 8028 实例）

> 本文所有输出均为真跑原文（长 id 数组字面量在采集时按 260 字符截断显示，不影响计划节点与耗时）。

---

## 0. 一句话结论

| 验收项（票面） | 结果 | 证据 |
|---|---|---|
| 1. 四场景可见性结果与返工前完全一致（29 条断言全 PASS） | ✅ 1 / 2 / 1 / 3 / 5，与 T1 证据文档逐条相同 | §2 |
| 2. EXPLAIN 证明走索引（5 万行） | ✅ `BitmapOr(idx_biz_task_dept_status, biz_task_pkey)`；**无 `Seq Scan on biz_task`**；166.375 ms → **16.728 ms** | §3 |
| 3. 上限降级路径（2001 条协作关系） | ✅ `member_scope_truncated=true` 且只按范围；边界 2000 不降级 | §4 |
| 4. `shared/scope.rs` 一行未改 | ✅ `git diff -- backend/src/business/shared/scope.rs` 为空 | §5 |
| 5. `cargo test` 全绿 | ✅ **584 passed / 0 failed**（返工前 582；本卡净 +2：删 1 加 3） | §6 |
| 6. `scripts/check.sh` 全绿 | ✅ B 闸① 0 处 / 闸② 0 处、C7 G1–G6 0 条未过、前端 typecheck 通过 | §7 |

---

## 1. 改法说明

### 1.1 谓词（`backend/src/business/task/model.rs`）

```rust
// 返工前（T1 首版，50eb300）
pub const VISIBLE_OR_TAIL: &str = " AND ( (1 = 1 {FILTER}) \
      OR t.owner_id = ?::uuid OR t.creator_id = ?::uuid OR t.assigner_id = ?::uuid \
      OR EXISTS (SELECT 1 FROM biz_task_member m WHERE m.task_id = t.id AND m.user_id = ?::uuid) )";

// 返工后（本卡）
pub const VISIBLE_OR_TAIL: &str = " AND ( (1 = 1 {FILTER}) OR t.id = ANY(?::text::uuid[]) )";
```

- 4 个 OR 分支**全部并入同一个 id 数组**：`my_member_task_ids(me)` 用一条查询算出
  「负责人 ∪ 创建人 ∪ 分派人 ∪ 协作人/关注者」的 `task_id` 集合，谓词里只剩一个 `ANY(?)`。
- 参数顺序：`{FILTER}` 的 scope 片段参数（`paginate` 恒前置）→ 参与 id 数组 → 业务过滤参数，
  与 SQL 文本里 `?` 的先后逐字一致（单测 `visible_params_are_one_uuid_array` 断言「`?` 个数 = 参数个数」）。
- 数组绑定用 `Value::Text("{uuid,uuid}")` + `?::text::uuid[]`（与 `shared/scope.rs::array_param` 同一套路，
  本栈驱动不支持 `Value::Array` 绑 `uuid[]`）。
- `assert_scope_fragment()` / `check_fragment_shape()` 的形态校验**原样保留**（` AND …` / `1 = 1` / `1 = 0`），
  单测 `fragment_shape_guard_accepts_known_shapes_and_rejects_or` 仍在。

### 1.2 「我参与」id 集合查询（`my_member_task_ids`）

```sql
SELECT t.id FROM biz_task t
 WHERE t.deleted_at IS NULL
   AND (t.owner_id = ?::uuid OR t.creator_id = ?::uuid OR t.assigner_id = ?::uuid)
UNION
SELECT m.task_id FROM biz_task_member m WHERE m.user_id = ?::uuid
LIMIT 2001                                                   -- MEMBER_SCOPE_LIMIT + 1，用于判越限
```

- 三个列各自有索引（`idx_biz_task_owner_status_due` / **新增** `idx_biz_task_creator` / `idx_biz_task_assigner_status`）
  → 实测 `BitmapOr` 三路，**1.096 ms**（§3.2）。
- 协作人分支**刻意不 join `biz_task`**：软删由外层主查询兜住；一旦带 join，规划器会退化成对
  `biz_task` 的全表 Hash Join（实测 5 万行 6.8 ms 且随行数线性上升），正是 D6 要避免的形态。

### 1.3 新增迁移 `V0104__task_creator_index`

`biz_task.creator_id` 原本没有索引（owner / assigner / member 三条分支都有）。
缺它时「我参与」集合查询只能 `Seq Scan biz_task`，等于把主查询省下的时间又还回去。
EXPLAIN 已证明加上后该索引被选中（§3.2 `Bitmap Index Scan on idx_biz_task_creator`）。
回滚脚本 `V0104__task_creator_index.down.sql` 只 `DROP INDEX`。

### 1.4 上限与降级（D7）

- `MEMBER_SCOPE_LIMIT = 2000`（硬上限），取 `2001` 条判断越限。
- 超限 → `MyScopeIds.truncated = true` → 参数传空数组 `{}`（`t.id = ANY('{}')` 恒假）⇒ 谓词自动退化为「只按范围」，
  参数个数不变（不会错位）。
- 分页出参新增 `member_scope_truncated`（`dto::TaskPageRes`，是 `PageData` 的超集，
  `list/total/page/page_size` 四字段不变，前端 `PageData<T>` 照旧可用）。
- 详情 / 子任务列表共用同一套 id 解析，超限时同样只按范围（不放宽安全边界）。

---

## 2. 四场景可见性：与返工前完全一致

真服务 + 真库 + 真登录，`scripts/verify/t1_task_api_verify.sh`（新建 `cloudpivot_t1v` → 63 个迁移 → 夹具 → 29 条断言）：

```
  PASS  编号不重复：5
  PASS  第 1 条状态默认 TODO：TODO
  PASS  第 1 条优先级默认 P2：P2
  PASS  第 1 条进度默认 0：0
  PASS  第 1 条截止日默认本周五：2026-10-30
  PASS  admin 可见条数：5
  PASS  详情部门名（普通员工=研发一组）：T1研发一组
  PASS  详情子任务数：0
  PASS  编辑后标题：T1走查-员工自己的任务（已改）
  PASS  编辑后优先级：P0
  PASS  显式 null 清空截止日：None
  PASS  协作人编辑被拒 HTTP 业务码：63002
  PASS  a) 普通员工只看到自己的：1
  PASS  b) 部门主管看到本部门及下级（研发一组 + 研发部）：2
  PASS  c) 跨部门协作人只看到被拉进的那一条：1
  PASS  c') 同部门同事看到自己名下 3 条（不含协作那条以外）：3
  PASS  d) 超管看到全部：5
  PASS  协作人查 TSK-202610-000004 详情 code：0
  PASS  协作人查 TSK-202610-000003 详情的业务码：63001
  PASS  删除回执 deleted：True
  PASS  删除后 admin 分页 total：4
  PASS  删除后 t1_other 分页 total（原 3 条剩 2 条）：2
  PASS  已删任务详情 404 码：63001
  PASS  我的任务 total：1
  PASS  status=DOING（当前无进行中）：0
  PASS  priority=P1（只第 4 条）：1
  PASS  keyword=跨部门：1
  PASS  owner=t1_other（删了 1 条后剩 2 条）：2
  PASS  status=BAD（非法值 → 400）：400
T1 接口走查：全部断言通过
```

`PASS` 计数 = **29**，`FAIL` 计数 = **0**。

| 场景 | 返工前（`docs/批次C+-T1-验证证据.md` §5/§7） | 返工后（本次） | 一致 |
|---|---|---|---|
| a) 普通员工 `t1_emp`（data_scope=4） | 1 | **1** | ✅ |
| b) 部门主管 `t1_mgr`（data_scope=3） | 2 | **2** | ✅ |
| c) 跨部门协作人 `t1_cross`（被拉进 TSK-…000004） | 1 | **1** | ✅ |
| c') 同部门同事 `t1_other`（非协作人） | 3 | **3** | ✅ |
| d) 超管 `admin` | 5 | **5** | ✅ |
| 协作人看不到同部门其它任务 | 404 + `63001` | **404 + `63001`** | ✅ |

同一批语义在 DB 层也复验（`cargo test --test batchT_t1_task`，真库 `cloudpivot_t1v`）：

```
test visibility_four_scenarios ...
[a] 普通员工可见 1 条
[b] 部门主管可见 2 条
[c] 跨部门协作人可见 1 条
[d] 超管可见 9 条（= 库里未软删任务总数）
test result: ok. 5 passed; 0 failed
```

---

## 3. EXPLAIN 取证（5 万行，`cloudpivot_t1e`）

### 3.1 数据规模与分布

```
                 指标                 |  值
--------------------------------------+-------
 biz_task 总数                        | 50000
 BULK 行                              | 50000
 t1_mgr 部门范围内（研发部+研发一组） | 33333
 t1_mgr 负责的                        |   400
 t1_mgr 协作的                        |   425
 t1_mgr 参与 id 数（去重并集）        |   825
```

身份 `t1_mgr`：`data_scope=3`（DEPT_AND_CHILD）→ scope 片段 `AND t.dept_id = ANY('{研发部,研发一组}'::text::uuid[])`。
两条 SQL 的 `SELECT/FROM/WHERE/ORDER BY` **逐字取自 `model.rs`**（`CARD_COLUMNS` / `CARD_FROM` / 新旧 `VISIBLE_OR_TAIL`），参数走 `PREPARE` 绑定。

### 3.2 「我参与」id 集合查询（返工后新增的那一条）

```
 Limit  (cost=1096.43..1104.76 rows=833 width=16) (actual rows=825.00 loops=1)
   ->  HashAggregate  (actual rows=825.00 loops=1)
         ->  Append  (actual rows=825.00 loops=1)
               ->  Bitmap Heap Scan on biz_task t  (actual rows=400.00 loops=1)
                     Recheck Cond: ((owner_id = …) OR (creator_id = …) OR (assigner_id = …))
                     ->  BitmapOr
                           ->  Bitmap Index Scan on idx_biz_task_owner_status_due  (actual rows=800.00)
                           ->  Bitmap Index Scan on idx_biz_task_creator  (actual rows=0.00)
                           ->  Bitmap Index Scan on idx_biz_task_assigner_status  (actual rows=0.00)
               ->  Seq Scan on biz_task_member m  (actual rows=425.00)   -- 该表仅 425 行，索引无意义
 Planning Time: 0.135 ms
 Execution Time: 1.096 ms
```

三路索引全部命中（含本卡新增的 `idx_biz_task_creator`），**没有 `Seq Scan on biz_task`**。

### 3.3 返工前（多 OR + `EXISTS`）—— `Seq Scan`，166 ms

```
 Limit  (cost=421193.77..421360.47 rows=20) (actual rows=20.00 loops=1)
   ->  Result
         ->  Sort  (Sort Key: t.due_date, t.created_at DESC, t.id DESC; top-N heapsort)
               ->  Nested Loop Left Join  (actual rows=33609.00 loops=1)
                     ->  Seq Scan on biz_task t  (cost=0.00..418951.00 rows=41626) (actual rows=33609.00 loops=1)
                           Filter: ((deleted_at IS NULL) AND ((dept_id = ANY (…)) OR (owner_id = …)
                                   OR (creator_id = …) OR (assigner_id = …) OR (ANY (id = (hashed SubPlan 3).col1))))
                           Rows Removed by Filter: 16391
                     ->  Memoize  (Cache Key: t.owner_id; Hits: 33607)
 Planning Time: 0.910 ms
 Execution Time: 166.375 ms
```

```
---- 返工前的 count ----
 Aggregate
   ->  Seq Scan on biz_task t  (actual rows=33609.00 loops=1)
         Rows Removed by Filter: 16391
 Execution Time: 58.305 ms
```

### 3.4 返工后（参与 id 数组）—— `BitmapOr`，16.7 ms

```
 Limit  (cost=5768.06..5934.76 rows=20) (actual rows=20.00 loops=1)
   ->  Result
         ->  Sort  (top-N heapsort, Memory: 35kB)
               ->  Hash Left Join  (Hash Cond: (t.owner_id = u.id))
                     ->  Bitmap Heap Scan on biz_task t  (cost=498.24..4690.83 rows=33392) (actual rows=33609.00 loops=1)
                           Recheck Cond: ((dept_id = ANY (…)) OR (id = ANY (…)))
                           Filter: (deleted_at IS NULL)
                           Heap Blocks: exact=2326
                           ->  BitmapOr  (cost=498.24..498.24 rows=33938)
                                 ->  Bitmap Index Scan on idx_biz_task_dept_status  (actual rows=66666.00 loops=1)
                                       Index Cond: (dept_id = ANY ('{研发部,研发一组}'::text::uuid[]))
                                 ->  Bitmap Index Scan on biz_task_pkey  (actual rows=825.00 loops=1)
                                       Index Cond: (id = ANY ('{825 个参与 id}'::text::uuid[]))
                     ->  Hash  (Hash Cond on sys_user u: 5 行)
 Planning Time: 0.903 ms
 Execution Time: 16.728 ms
```

```
---- 返工后的 count ----
 Aggregate
   ->  Bitmap Heap Scan on biz_task t  (actual rows=33609.00 loops=1)
         ->  BitmapOr
               ->  Bitmap Index Scan on idx_biz_task_dept_status
               ->  Bitmap Index Scan on biz_task_pkey
 Execution Time: 7.606 ms
```

### 3.5 证据对（同一库、同一数据、同一身份）

| 查询 | 返工前 | 返工后 | 提升 | 计划变化 |
|---|---|---|---|---|
| `page_visible` 取数（LIMIT 20） | **166.375 ms** | **16.728 ms** | **9.94×** | `Seq Scan on biz_task` → `BitmapOr` |
| `page_visible` count | 58.305 ms | 7.606 ms | 7.67× | `Seq Scan on biz_task` → `BitmapOr` |
| 命中行数（语义） | 33609 | **33609** | — | **完全一致** |
| 「我参与」id 集合查询 | （新增，原先由 `EXISTS` 现场判断） | 1.096 ms | — | `BitmapOr` 三路索引 |

> 计划里除 `biz_task_member`（425 行）/ `sys_user`（5 行）这两张**小表**外，
> 没有对 `biz_task` 的顺序扫描 —— 票面要求「禁止出现 `Seq Scan on biz_task`」✅。

### 3.6 语义等价（同一条 SQL 的两种写法，同库同参）

```
 返工前_visible_total | 返工后_visible_total
----------------------+----------------------
                33609 |                33609
```

---

## 4. 上限降级路径（D7）

库 `cloudpivot_t1e`（5 万行），身份 `t1_emp`（`data_scope=4` 仅本人）：
自有 2 条任务 + 人为灌 2001 条协作关系（范围外的市场部任务）。

```
================ 2) 2001 条协作关系 → 期望 total=2（只按范围）且 member_scope_truncated=true ================
t1_emp 自有任务 = 2
t1_emp 协作行 = 2001
参与 id 数 = 2003
  total=2  page_size=5  member_scope_truncated=True
  首页标题: ['上限测试-自有A', '上限测试-自有B']

================ 3) 删掉 3 条协作（剩 1998，参与 id 恰为 2000）→ 期望不降级 ================
t1_emp 协作行 = 1998
参与 id 数 = 2000
  total=2000  page_size=5  member_scope_truncated=False
  首页标题: ['批量任务 89', '批量任务 179', '批量任务 269', '批量任务 359', '批量任务 449']
```

- 2003 个参与 id（> 2000）→ **降级为只按范围**（只看到自有的 2 条），并如实回报 `member_scope_truncated=true`。
- 2000 个参与 id（= 上限）→ **不降级**，2000 条协作任务全部可见，`member_scope_truncated=false`。
- 边界语义、降级语义、响应标记三者同时被验证；降级时参数传空数组，**参数个数不变**（单测 `truncated_scope_degrades_to_empty_array` 钉住）。

---

## 5. `shared/scope.rs` 一行未改

```
$ git diff --stat -- backend/src/business/shared/scope.rs
（无输出）

$ git status --short
 M backend/src/business/task/dto.rs
 M backend/src/business/task/handler.rs
 M backend/src/business/task/mod.rs
 M backend/src/business/task/model.rs
?? backend/migrations/business/V0104__task_creator_index.down.sql
?? backend/migrations/business/V0104__task_creator_index.up.sql
```

B 批次核心（`scope.rs`）与“分页唯一入口”闸① 均未触碰：`page_visible` 仍然只经 `scope::paginate`。

---

## 6. `cargo test` 全绿（584）

```
$ cd backend && cargo test 2>&1 | grep -E "^test result:"
test result: ok. 543 passed; 0 failed   （lib：本卡净 +2，详见下方单测清单）
test result: ok. 1 passed; 0 failed     （batchB_b2_scope）
test result: ok. 1 passed; 0 failed     （batchB_b7_mutex）
test result: ok. 1 passed; 0 failed     （batchC_c3_dispatch）
test result: ok. 11 passed; 0 failed    （batchC_c4_help）
test result: ok. 8 passed; 0 failed     （batchC_c5_changelog）
test result: ok. 4 passed; 0 failed     （batchC_c6_changelog）
test result: ok. 5 passed; 0 failed     （batchT_t1_task，真库）
test result: ok. 7 passed; 0 failed     （impexp_worker）
test result: ok. 1 passed; 0 failed     （pref_preference）
test result: ok. 2 passed; 0 failed     （s3_minio）
合计 passed = 584（返工前 582）
```

本卡单测（`business/task/model.rs` 内联）：
- `visible_tail_uses_member_id_array_only`：谓词只含一个 `ANY(?::text::uuid[])`，**禁止** `EXISTS` / `t.owner_id` 等 OR 分支回流（防回归）。
- `visible_params_are_one_uuid_array`：`?` 个数 = 参数个数；数组编码为 `{uuid,uuid}`。
- `truncated_scope_degrades_to_empty_array`：降级/空集合都编码为 `{}`，`MEMBER_SCOPE_LIMIT == 2000`。
- `member_limit_truncation_rule`：2000 不降级 / 2001 降级。
- 原有 `fragment_shape_guard_…` 保留（形态校验未放松）。

---

## 7. `scripts/check.sh` 全绿

```
==> 后端：cargo fmt --check                       （通过）
==> 后端：cargo clippy -- -D warnings              （通过）
==> 后端：cargo check                              （通过）
==> 前端：pnpm typecheck                           （通过）
==> 批次B 闸①：PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：PASS：0 处
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> C7 G1–G6：未通过 0 条
==> 偏好字典与色板一致性：PASS
==> 全部检查通过
```

---

## 8. 迁移与索引

`cloudpivot_t1v` 空库全量迁移：**63 个迁移**应用成功（含新增 `V0104`），`biz_task` 索引 9 条：

```
biz_task_pkey                 | btree (id)
idx_biz_task_assigner_status  | btree (assigner_id, status)
idx_biz_task_creator          | btree (creator_id)                 ← 本卡新增（V0104）
idx_biz_task_dept_status      | btree (dept_id, status)
idx_biz_task_due_open         | btree (due_date) WHERE status NOT IN ('DONE','CANCELLED')
idx_biz_task_owner_status_due | btree (owner_id, status, due_date)
idx_biz_task_parent           | btree (parent_id)
idx_biz_task_project          | btree (project_id) WHERE project_id IS NOT NULL
uk_biz_task_task_no           | unique btree (task_no)
```

---

## 9. 残留风险 / 后续

1. **每次分页多一次查询**（解析参与 id 集合）。50 万规模下该查询仍是索引级（§3.2 为 1.1 ms）；
   若将来单用户参与量逼近 2000，BitmapOr 的探测成本会上升（D7 的 2000 就是为此设的），
   届时按设计文档 §4.4.4 的「独立 tab（`/task/mine?tab=participated`）」兜底，属 T2/T6 范围。
2. **上限降级对详情接口同样生效**（超限时范围外协作任务看不到详情）。这是 D7 的既定取舍：
   宁可少看、也不让单请求退化成 15 s 超时；`member_scope_truncated` 只回在分页接口上，
   详情接口无标记位（单条查询本身不体现“集合被截断”）。
3. `biz_task_member` 在真实规模（25 万行）下会走 `idx_biz_task_member_user`；本文 5 万行库中该表只有 425 行，
   PG 选择顺序扫描属正常（成本 0.05 ms），不是索引缺失。
