# ERP-R6 证据：补 12 个外键索引（V0186）

> 卡：`t_4b18bf5f`【ERP-R6】补 12 个外键索引（快卡，30 分钟）
> 提交：`768981e` perf(db): 补 12 个外键缺失的索引 ERP-R6（V0186，幂等部分索引）
> 生产：172.18.10.144 / `cloudpivot` 库 / `cloudpivot.service` 重启后自动迁移
> 状态：已部署、已验收（`PASS=51 FAIL=0`），ERP 不变量 31/32 未退化

---

## 一、清单是怎么来的（不照抄卡面）

卡面写 10 个 + 「另有 2 个，先跑查询确认」。实际执行卡面 §先确认清单 的 SQL，生产库返回 **12 行**，
差的 2 个是 `biz_task_template.project_id` 与 `biz_task_worklog.user_id`：

```
biz_flow_instance|prev_instance_id
biz_flow_instance|version_id
biz_flow_instance_node|node_id
biz_flow_task|transferred_from
biz_stock_transfer|transit_warehouse_id
biz_stock_transfer_item|from_location_id
biz_stock_transfer_item|to_location_id
biz_task_comment|parent_comment_id
biz_task_comment|user_id
biz_task_template|owner_id
biz_task_template|project_id        ← 卡面未列
biz_task_worklog|user_id            ← 卡面未列
```

列的 NOT NULL 属性也一并查了（决定能不能做部分索引）：

| 可空（→ 部分索引 `WHERE col IS NOT NULL`） | NOT NULL（→ 全表索引） |
|---|---|
| prev_instance_id / node_id / transferred_from / transit_warehouse_id / from_location_id / to_location_id / parent_comment_id / owner_id / project_id | version_id / biz_task_comment.user_id / biz_task_worklog.user_id |

---

## 二、部分索引会不会让 JOIN 用不上？（先证明再动手）

卡面要求「可空列加 `WHERE xxx IS NOT NULL`」，但如果规划器不能从 `child.fk = parent.id`
推出 `fk IS NOT NULL`，部分索引对 JOIN 就是废的。所以先在 PG 18.6 上做了探针
（1000/2000 行 + `enable_seqscan=off`）：

| 场景 | 结果 |
|---|---|
| `where fk = 'uuid'` | Index Scan using idx_child_fk_partial ✅ |
| `join ... on c.fk = p.id`（外层 p.id = const） | Index Only Scan using idx_child_fk_partial ✅ |
| 同表 `var = var` 等值 | 两侧都用该索引 ✅ |
| 范围 `fk > 'uuid'` | Index Scan ✅ |
| 反连接 `NOT EXISTS (... c.fk = p.id)` | Hash Anti Join，子侧 Index Only Scan ✅ |

结论：uuid 的 `=` 是严格算子，PG 的 `predicate_implied_by` 能证明蕴含，**部分索引在 JOIN/反连接下照常命中**。
（探针全程 `begin ... rollback`，临时表，不落任何库。）

---

## 三、迁移

- `backend/migrations/business/V0186__fk_indexes.up.sql` —— 12 条 `CREATE INDEX IF NOT EXISTS`；
  迁移内置 `DO $$` 自检：12 个索引缺失即 `RAISE EXCEPTION`，复查「剩余无索引外键 > 0」时
  `RAISE WARNING` 并点名表.列。
- `backend/migrations/business/V0186__fk_indexes.down.sql` —— 12 条 `DROP INDEX IF EXISTS`，
  不删表、不动数据、不动其它索引。
- 不用 `CONCURRENTLY`（迁移在单事务内，PG 报 25001）；表最大 947 行，普通 CREATE INDEX 锁窗口毫秒级。
- 迁移文件无 psql 元命令（`grep -nE '^\s*\\'` 无匹配）。

### 干跑（先在生产 schema 副本上验，再碰生产）

用 144 生产库 `pg_dump -s` 的 schema-only dump 在本机建 `cloudpivot_r6v`：

| 步骤 | 结果 |
|---|---|
| 迁移前无索引外键 | 12 行（与生产一致）|
| 跑 up.sql | `NOTICE: V0186 自检通过：新增外键索引 12 个，剩余无索引外键 0 个` |
| 复查 | 0 行（卡面验收口径）|
| EXPLAIN（12 列 + 反连接 + join） | 26 处命中新索引，0 处落空 |
| 重跑 up.sql（幂等） | 无报错，索引数仍 12 |
| 跑 down.sql | 目标索引 0，全库索引 393（只掉这 12 个）|
| 再重放 up.sql | 12 个，闭环 |

---

## 四、生产部署

```
[0] 部署前：12 个无索引外键；全库索引 393；schema_migrations max=185
    行数 flow_instance=145 node=408 flow_task=141 transfer=8 transfer_item=8 comment=947 template=0 worklog=99
[1] git pull → HEAD=768981e
[2] systemctl restart cloudpivot → /health=200（第 2 次尝试）
[3] journal: NOTICE: V0186 自检通过：新增外键索引 12 个，剩余无索引外键 0 个
[4] schema_migrations: name=fk_indexes success=true applied_via=executed ms=8
    12 个目标索引 = 12；全库索引 405（+12）
[5] 行数逐表对比：与部署前完全一致 ✅（只加索引，不动数据）
[6] scripts/verify/erpr6_fk_indexes.sh → PASS=51 FAIL=0
[7] erp_invariants.py → 31/32 通过，BLOCKER 0，WARN 1（F2 历史数据，卡前基线相同）
```

12 个索引总体积 **136 kB**（表合计：flow_instance 145 / node 408 / task 141 / transfer 8 /
transfer_item 8 / comment 947 / template 0 / worklog 99 行）。

---

## 五、EXPLAIN 实测（含一条**必须照实说**的偏差）

### 5.1 卡面原始验收查询

```sql
explain select * from biz_flow_task where transferred_from = '00000000-0000-0000-0000-000000000001';
-- 生产实际：Seq Scan on biz_flow_task  (cost=0.00..6.76 rows=1 width=414)
--            Filter: (transferred_from = '...'::uuid)
```

**生产上是 Seq Scan，不是卡面期望的 Index Scan。** 原因不是索引没建好，而是
`biz_flow_task` 只有 141 行：全表扫代价 6.76 < 索引扫 8.14，规划器理性地选 Seq Scan。
`set enable_seqscan=off` 后立刻变成：

```
Index Scan using idx_biz_flow_task_transferred_from on biz_flow_task  (cost=0.12..8.14 rows=1 width=414)
  Index Cond: (transferred_from = '...'::uuid)
```

即「索引存在且可被采用」，只是这个数据量下规划器不划算去用。

### 5.2 生产上已经自然命中新索引的真实 SQL

```sql
explain select t.id from biz_flow_task t join biz_flow_instance_node n on n.node_id = t.id;
-- Hash Join
--   ->  Index Only Scan using idx_biz_flow_instance_node_node_id on biz_flow_instance_node n
```

### 5.3 规模验证（副本库灌到 5 万行，**不关 seqscan**）

副本库 `cloudpivot_r6v.biz_task_comment` 灌 5 万行（heap 6560 kB，idx_user 336 kB，idx_parent 64 kB）：

```sql
select * from biz_task_comment where user_id = '...07';
-- Bitmap Heap Scan → Bitmap Index Scan on idx_biz_task_comment_user_id
--   Index Cond: (user_id = '...'::uuid)   Buffers: shared hit=3

select * from biz_task_comment where parent_comment_id = '...17';
-- Index Scan using idx_biz_task_comment_parent_comment_id（部分索引，自然命中）

-- 对照（临时关索引，模拟迁移前）：
-- Seq Scan on biz_task_comment
--   Rows Removed by Filter: 47500
```

即：数据量起来后规划器自己会切回索引 —— 本卡补的正是「将来那条路」。

---

## 六、偏差与已知缺口（照实说）

1. **卡面 `explain` 验收在生产上仍是 Seq Scan**（141 行，代价更优），见 §5.1。验收口径改为
   「索引存在 + 可被采用（`enable_seqscan=off` 命中）+ 5 万行规模下自然命中」，三条都留了证据。
2. 生产表都很小（最大 947 行），索引的**收益**当前无法在生产上量化；本卡只保证
   「路修好了、规划器需要时能用上」。
3. `biz_task_template` 当前 0 行 → 它的两个索引暂时是纯成本（16 kB），等模板功能上线才有收益；
   仍按卡面补，避免以后上线再补迁移。
