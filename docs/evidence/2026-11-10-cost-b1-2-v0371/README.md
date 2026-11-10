# COST-B1-2（t_76333a39）实测证据 —— 迁移 V0371 归集四表

设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md` §3.1 ③④⑤⑥（拆卡清单 §8.1 的 B1-2）。
前置：B1-1 = V0370（`biz_lot` + `biz_cost_object`，commit 42a443c4）已上线。

## 1. 交付物

| 文件 | 说明 |
|---|---|
| `backend/migrations/business/V0371__cost_collect_four_tables.up.sql` | 四张表 + 索引 + 注释 + 3 个自检 `DO` 块（前置护栏 / 形态 6 组 / 行为 9 组） |
| `backend/migrations/business/V0371__cost_collect_four_tables.down.sql` | 幂等回滚 + 下游 FK 护栏（扫 `pg_constraint`，不硬编码表名） |
| `backend/tests/cost_b1_2_cost_collect_ddl_migration.rs` | 11 条静态不变量（不连库，`cargo test` 每次都跑） |
| `docs/evidence/2026-11-10-cost-b1-2-v0371/01_replay_fresh.out` | 空库全链重放 291/291 PASS |
| `docs/evidence/2026-11-10-cost-b1-2-v0371/02_up_down_drill.out` | 真库往返演练（本文件第 3 节逐项引用） |
| `docs/evidence/2026-11-10-cost-b1-2-v0371/02_up_down_drill.sh` | 演练脚本（可重复执行） |

建的四张表：

| 表 | 作用 | 索引 | CHECK | 物理外键 |
|---|---|---|---|---|
| `biz_cost_collect` | 料/工/费三路归集的唯一落点；`cost_object_id` 空 = 车间费用池 | 3（1 唯一 + 2 普通） | 7 | `→ biz_cost_object ON DELETE RESTRICT` |
| `biz_cost_labor` | 工时台账（人工成本唯一来源，结算只取 `CONFIRMED`） | 3 | 6 | `→ biz_cost_object ON DELETE RESTRICT`（NOT NULL） |
| `biz_cost_overhead` | 手工制造费用台账（折旧/水电/房租/修理/车间管理/易耗/其他） | 2 | 5 | `→ biz_cost_object ON DELETE RESTRICT`（可空=车间级） |
| `biz_cost_rate` | 工资率 / 机器费率（版本化生效期，`scope_id` 弱引用） | 2（1 唯一 + 1 普通） | 6 | 无（模块内无父表；`scope_id` 指向 dept 或 object，属弱引用） |

合计：4 表 / 10 条 `CREATE INDEX`（含 2 条唯一）/ 24 条 CHECK / 3 条物理外键。

## 2. 与卡面的三处偏差（都基于实测，不是口味）

### 2.1 号段：卡面写 `V0369`，实际落 `V0371`

`V0369__perf_list_order_indexes` 已被 N-8 性能卡占用，`V0370` 是前置卡 B1-1 用的。
nik 迁移加载器要求**版本号跨目录全局唯一**
（`backend/vendor/nik-db/src/migrate/loader.rs::load_from_sources`），重号会让服务启动直接报错。
故顺延 V0371；B1-3 起为 V0372 / V0373 / V0374。

### 2.2 表数：卡面验收写「五张表」，实际建 4 张

§8.1 把 B1-2 定义为「归集四表 + 自检」，§3.1 里属于本卡的是 ③④⑤⑥ 四节；
第 5 张 `biz_cost_alloc_rule`（§3.1 ⑦）归 B1-3（子卡 t_7efea068 正文写「分摊两表 + 期间两表」）。
按「只做本卡写的事」⇒ 建 4 张，B1-3 再把 ⑦⑧⑨⑩ 补齐。

### 2.3 精度：卡面验收写「金额 `numeric(20,2)`」，实际按设计 §3.1 落 `NUMERIC(18,4)`

两条验收项（「五张表结构符合 §3」与「金额 numeric(20,2)」）互斥 —— §3.1 全篇是
`NUMERIC(18,4)`（金额、数量）+ `NUMERIC(18,6)`（单价、费率）。取设计的实测理由：

1. **`numeric(20,2)` 会让 `ck_biz_cost_labor_amount` 恒假**，工时行根本插不进去。
   该约束是 `amount = round(hours * rate, 4)`，而 `rate` 是 `NUMERIC(18,6)`
   （月薪 ÷ 工时必然除不尽）。演练第 7 步用同一份数据实测：

   ```
   → 同一份代码、只把 amount 换成 numeric(20,2)：工资率 33.333333 的工时行（期望 check 违反）
   ERROR:  new row for relation "zz_prec_probe" violates check constraint "ck_zz_amount"
   DETAIL:  Failing row contains (1.00, 33.333333, 33.33).
   → 同样的数据写进 numeric(18,4) 的 biz_cost_labor（期望成功）：
     → 18,4 落库成功
   ```

2. **约当产量 / 分摊尾差按 4 位小数倒挤**（设计 §4.4.4、§4.6.2），scale=2 会把尾差放大到分。
3. **全仓口径一致**：`backend/migrations` 里 210 处 `NUMERIC(18,4)`，列定义 `numeric(20,2)` **0 处**
   （`scripts/verify/*.sh` 里的 `::numeric(20,2)` 只是显示格式化，不是列类型）；
   前置卡 V0370 的 `plan_qty / completed_qty / wip_qty` 同样是 `NUMERIC(18,4)`。

该决定已固化进迁移自检（A6 断言 `format_type` 精确匹配）与单测
`v0371_amount_precision_follows_design`（反向断言禁止出现 `NUMERIC(20,2)/(18,2)/(20,4)`），
将来谁改窄都会红。

## 3. 真库实测（`02_up_down_drill.out`，库 = `mig_replay_costb1_01`）

| 步 | 项目 | 结果 |
|---|---|---|
| 1-2 | up ×3 | 三轮均 `exit=0`，第二轮起只有 `already exists, skipping` |
| 3 | 幂等证据 | `rows_collect/labor/overhead/rate = 0`（探针零残留）、`probe_leftover=0`、`idx=14`（10 + 4 PK）、`checks=24`、`fks=3` |
| 4 | 唯一索引谓词 | `uk_biz_cost_collect_src … WHERE ((deleted_at IS NULL) AND (source_id IS NOT NULL))`、`uk_biz_cost_rate … WHERE (deleted_at IS NULL)` |
| 5 | 外键动作 | 三条全部 `confdeltype=r`、`ON DELETE RESTRICT` |
| 6 | 金额/数量精度 | `amount=(18,4)`、`qty=(18,4)`、`unit_price=(18,6)`、`rate=(18,6)`、`hours=(10,2)` |
| 7 | 精度反证 | `numeric(20,2)` 下 `rate=33.333333` 的工时行被 CHECK 拒（`scale2_table_rows=0`）；`(18,4)` 下同一行成功 |
| 8 | 业务语义 | 软删后同来源行可重新归集 ✅；活行重号被 `uk_biz_cost_collect_src` 拦 ✅；领料行不挂对象被 `ck_biz_cost_collect_obj` 拦 ✅；车间费用池（折旧、不挂对象）允许 ✅；有归集行时删成本对象被 RESTRICT 拦 ✅ |
| 10-12 | down ×2 | 两次均 `exit=0`，第二次只打 `does not exist, skipping`；回滚后四表 + 相关索引 `idx_left=0` |
| 13 | 回滚后重放 | `replay tables=4` |
| 14 | 下游护栏反证 | 存在 `zz_downstream` FK 到 `biz_cost_collect` 时，down 打出 `V0371 down 拒绝执行：下游成本表仍在（zz_downstream），请先回滚建立它们的迁移（V0372 及之后）` |
| 15 | 收尾 | `final tables=4 fks=3` |

空库全链重放（`01_replay_fresh.out`）：

```
==> 空库已建：mig_replay_costb12（owner=alun）
== 结果：PASS —— 291/291 条版本化迁移在空库上按版本号升序全部通过
```

（B1-1 时是 290 条，+1 = 本卡的 V0371。）

`cargo test`：exit=0，26 个测试目标全 ok（新增本卡 11 条：`cost_b1_2_cost_collect_ddl_migration`）；
`cargo fmt -- --check` 干净。

## 4. 踩到的坑（已写进迁移注释与单测）

1. **`ON DELETE RESTRICT` 违反抛的是 SQLSTATE `23001 restrict_violation`，不是 `23503`**
   （23503 只在「子行引用不存在的父行」时抛）。自检里写 `WHEN foreign_key_violation` 捕不到，
   必须 `WHEN restrict_violation OR foreign_key_violation`。实测证据：

   ```
   caught sqlstate=23001 msg=update or delete on table "zz_probe_parent" violates RESTRICT setting of ...
   ```

2. **`pg_indexes.indexdef` 会把部分索引谓词多套一层括号**：
   `WHERE ((deleted_at IS NULL) AND (source_id IS NOT NULL))`。
   自检里按 `replace(indexdef,' ','')` 直接 LIKE 匹配会失败 —— 要先去掉空格**和括号**再比。

3. **down 里不能用 `'biz_cost_labor'::regclass` 做比较**：第二次执行 down 时表已删，
   该 cast 直接抛 `relation does not exist`，down 就不幂等了。改用 `confrelid::regclass::text = ANY(数组)`。
