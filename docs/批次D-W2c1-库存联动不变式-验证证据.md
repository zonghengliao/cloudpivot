# 批次 D · W2c-1 验证证据：库存联动不变式 ①②

> 任务：【W2c-1】库存联动不变式①②（数量由序列号派生 + 同事务流水）
> 前置：W2a（V0138 建 `biz_material_serial`）✅ / W2b（序列号 CRUD 42 单测）✅
> 本卡范围：不变式 ①（数量由序列号派生）与 ②（状态与库存流水同事务）。③巡检 SQL、④调拨同步仓库是 **W2c-2**。

---

## 一、决策：方案 A —— 数量由序列号派生

**结论**：对 `serial_required = TRUE` 的物料，`biz_inventory.qty` **不再手工增减**，
每次序列号状态变化后调用 `recompute_quantity`（`INSERT … ON CONFLICT DO UPDATE SET qty = (SELECT count(*) …)`）重算。

理由（三条，都可验证）：

1. **单一真相**：一物一码的件数只能有一个来源。方案 B（双写 ±1）即使同事务，也允许「两边各自算错」
   （比如漏一笔流水就悄悄累加），巡检只能事后发现、不能阻止脏账；方案 A 里数量是聚合值，**不可能**与序列号分叉。
2. **成本可接受**：一物一码单仓件数量级在千以内，重算子查询走 `biz_material_serial` 的部分索引
   （本卡 EXPLAIN 见 §六：`Index Scan`，无 `Seq Scan`）。读写比约 1:数百，只有写路径付出代价。
3. **不改读路径**：库存列表 / 详情 / 分页 SQL 一字未动，前端与报表无感。

**自动建档**：用 `INSERT … ON CONFLICT (material_id, warehouse_id) DO UPDATE` 而不是纯 `UPDATE` ——
一物一码物料首次批量生成时 `biz_inventory` 还没有行，纯 `UPDATE` 会「改 0 行」而**静默丢账**（单测①专钉这条）。

**`amount` 一起收**：重算同时把 `amount = qty * avg_cost`。数量变了金额不变会让移动加权反算对不上
（`biz_inventory.amount` 是「以本列为准」的成本账）。

## 二、卡面与真实表结构的三处口径差异（照卡面写必然失败）

| 卡面写法 | 真实约束 / 列名 | 落地 |
|---|---|---|
| `biz_inventory.quantity` | V0137 §1.4 列名是 **`qty`** | 全部 SQL 用 `qty` |
| 出库流水 `qty = -1` | `ck_biz_inv_txn_qty CHECK (qty > 0)` | `txn_type='OUT'` + `qty=+1`（方向靠 txn_type） |
| 报废流水 `type = SCRAP` | `ck_biz_inv_txn_type CHECK (txn_type IN ('IN','OUT'))` | `txn_type='OUT'` + `biz_type='SERIAL_SCRAP'` |

另两处业务口径（写在代码注释里，也让流水可追溯）：

- 批量生成 = **一条** `IN` 流水（`qty = N`，`doc_type='SERIAL_BATCH'`，`doc_no` = 本批首号，`doc_id` = 首件 id）；
- 单件出库 / 报废 = 一条 `OUT` 流水（`doc_type='MATERIAL_SERIAL'`，`doc_no` = 序列号本身），
  `before/after` 六列取自**锁读到的旧快照**与**重算后的真实落库值**（不是 `before ± 1`，那样又变双写了）。

## 三、事务边界（C7 G3/G4/G5 的规矩）

```
handler 层（开事务 / 提交 / 回滚）              model 层（吃 &impl SqlExecutor，不开事务）
serial_handler::generate_serials  ── begin ──▶ serial_model::batch_generate(&tx, …)
                                     │            ├─ take_numbers(&tx, …)     取号（同一事务，失败不白吃号）
                                     │            ├─ INSERT N 条 serial
                                     │            ├─ link_inventory(&tx, …)
                                     │            │    ├─ lock_inventory  (SELECT … FOR UPDATE)
                                     │            │    ├─ recompute_quantity（派生数量）
                                     │            │    └─ post_serial_txn（写流水）
                                     │            └─ SELECT 回读
                                     └─ commit / rollback
serial_handler::apply_status      ── begin ──▶ serial_model::transition_status_with(&tx, …)
                                                  ├─ SELECT … FOR UPDATE（行锁，并发第二次必被状态机拒）
                                                  ├─ UPDATE status / outbound_at
                                                  └─ IN_STOCK→OUTBOUND|SCRAPPED 才走 link_inventory
```

- `db()` 只出现在 handler 层（`apply_status` / `generate_serials`）与**只读**查询里；
  写路径不在模型层开事务、不在事务里开新事务。
- `take_numbers` 由 `db()` 改为吃 `&impl SqlExecutor` 后产生一个**额外收益**：
  落库失败会连号段一起回滚（不再「白吃一段号」）。
- `reason` 从 W2b 的「只进日志」升级为落流水 `remark`（V0138 序列号表仍不加列，不动表结构）。

## 四、单测：11 条（卡面要求 ≥8）

`backend/src/business/erp/serial_handler.rs` 的 `tests`（真库 `ERP_DB_URL`，缺库则 SKIP）：

| # | 用例 | 断言要点 |
|---|---|---|
| ① | `batch_generate_derives_inventory_quantity` | 生成 5 → `qty = 5`（首次入库 upsert 建档，生成前无账面行） |
| ② | `batch_generate_writes_single_in_txn` | 只有 1 条 `IN` 流水：qty=5 / before=0 / after=5 |
| ③ | `outbound_decrements_derived_quantity` | 出库 1 件 → `qty = 4` |
| ④ | `outbound_writes_out_txn_with_reason` | 1 条 `OUT` 流水：qty=+1 / before=5 / after=4，`remark` = 接口 `reason` |
| ⑤ | `scrap_from_stock_decrements_and_writes_scrap_txn` | 在库报废 → `qty -1` + 1 条 `SERIAL_SCRAP` |
| ⑥ | `scrap_from_outbound_keeps_quantity_and_writes_no_txn` | 出库后报废 → 数量不变、无新流水 |
| ⑦ | `txn_failure_rolls_back_serial_status` | remark 超 255 字符 → 流水 INSERT 失败 → 回滚：状态仍 `IN_STOCK`、`outbound_at` 为空、qty 与流水条数不变 |
| ⑧ | `invariant_holds_after_every_operation` | 生成→出库→在库报废→出库后报废→再生成，**每一步** `qty == count(IN_STOCK)` |
| ⑨ | `plain_material_keeps_quantity_management` | 非 `serial_required`：生成 400+66007，账面 7 件不变、无流水、无序列号 |
| ⑩ | `concurrent_outbound_only_decrements_once` | 并发两次转 OUTBOUND 同一件 → 一成功一 400+66008，qty 只减 1、流水只 1 条 |
| ⑪ | `period_code_uses_china_time` | 会计期间码 `yyyy-MM` 按 CST 翻月 |

**实测（全量 lib 单测，真库 `cloudpivot_w2c1`）**

```
test result: ok. 1077 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 37.50s
```

（基线 1066 → 本卡 +11 = 1077；W2b 的 42 条全绿，未被事务下沉破坏。）

```
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 25.52s
CLIPPY-EXIT: 0

$ cargo fmt -- --check
FMT-CHECK: OK
```

`bash scripts/check.sh`（仓根，含后端 4 步 + 前端 typecheck + 批次B 闸①② + C7 G1–G6）：

```
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处
==> 批次B 门禁：全部通过
==> C7 门禁汇总：G1–G6 共 6 条，未通过 0 条
==> 偏好字典与色板一致性  PASS
==> 全部检查通过
CHECK-EXIT: 0
```

## 五、真机验收：`scripts/verify/w2c1_invariant.sh`（40 项断言，全 PASS）

```
$ REPO=/root/w2c1a bash scripts/verify/w2c1_invariant.sh
[PASS] 隔离实例就绪（/health=200）
[PASS] 生成前无账面行（首次入库要 upsert 建档）
[PASS] POST /business/erp/serial/batch-generate count=3
[PASS] 生成 3 件 → 账面数量 = 3（由序列号派生）
[PASS] 不变式①：quantity(3) == count(IN_STOCK)(3) —— 批量生成 3 件后
[PASS] 批量生成只写 1 条流水（不是 3 条）
[PASS]   流水方向/业务类型 = IN / SERIAL_IN
[PASS]   流水 qty=3 / before=0 / after=3
[PASS] PATCH /serial/{id}/status → OUTBOUND
[PASS] 出库 1 件 → 账面数量 3 → 2
[PASS] 不变式①：quantity(2) == count(IN_STOCK)(2) —— 出库 1 件后
[PASS] 出库追加 1 条流水（共 2 条）
[PASS]   流水方向/业务类型 = OUT / SERIAL_OUT
[PASS]   流水 qty=+1（方向看 txn_type）/ before=3 / after=2
[PASS]   reason 落流水 remark
[PASS] PATCH 重复出库 → 400+66008
[PASS]   重复出库未扣减、未写流水（qty=2 / txn=2）
[PASS] 在库报废 1 件 → 账面 1
[PASS]   报废写 1 条 SERIAL_SCRAP 流水（共 3 条）
[PASS] 已出库件报废：数量不变（1）、不写流水（3 条）
[PASS] 终局巡检：quantity == count(IN_STOCK)（true~1.0000~1）
[PASS] POST batch-generate 按数量物料 → 400+66007
[PASS]   按数量物料没有账面行、没有流水（不受序列号影响）
[PASS]   重算子查询命中索引（无 Seq Scan）
===== 汇总：PASS=40 FAIL=0 =====
```

## 六、EXPLAIN（重算子查询不许 Seq Scan）

```
Aggregate
  ->  Index Scan using uk_biz_material_serial_no on biz_material_serial
        Filter: ((material_id = '…'::uuid) AND (warehouse_id = '…'::uuid)
                 AND ((status)::text = 'IN_STOCK'::text))
```

验收库里只有个位数行，planner 挑了 `uk_biz_material_serial_no` 的全索引扫描（仍不是 `Seq Scan`）；
生产量级下会走 `idx_biz_material_serial_material (material_id) WHERE deleted_at IS NULL`。
两个选择都不读整表，符合「一物一码重算不许全表扫」的红线。

## 七、测试库口径（与卡面的差异）

卡面写「测试库只用 `cloudpivot_test`」，但实测该库 `schema_migrations.max(version::int) = 88`，
**`biz_material_serial` 表根本不存在**（`to_regclass` 为 NULL），跑 W2c-1 必然报
`relation "biz_material_serial" does not exist`（技能库里 §6.26 已记过这条）。本卡按既有惯例自建隔离库：

- 单测：`cloudpivot_w2c1`（`createdb -O cloudpivot`，起一次实例自动跑完 97 条迁移到 V0142）
- 真机验收脚本：每次运行**重建** `cloudpivot_w2c1_verify`（`W2C1_KEEP=0` 可跑完删除）

**未动 `cloudpivot_test` / `cloudpivot_dev` / 生产库。**

## 八、接口契约变化（对 W2b 的唯一影响）

- 5 个接口的**路径、入参、出参全部不变**（红线④）。
- 一处**校验收紧**：`POST /business/erp/serial/batch-generate` 对 `serial_required=TRUE` 的物料
  **必须**给 `warehouse_id`，否则 `400 + 66002`（「一物一码物料批量生成必须指定 warehouse_id：… 不变式①」）。
  原因：V0137 的 `biz_inventory.warehouse_id` 是 `NOT NULL`，没有仓库维度的序列号记不进任何仓账，
  不变式①就无法成立。W2b 的 e2e 脚本本来就带仓库，不受影响。
- `PATCH /serial/{id}/status` 的 `reason` 现在落流水 `remark`（限 200 字，由 dto 校验；超 255 会整单回滚而不是悄悄截断）。

## 九、未做（W2c-2 范围，避免越界）

- ③ 巡检 SQL（「账面 5 件 / 序列号 3 个」对账）
- ④ 调拨 / 出库时同步改 `biz_material_serial.warehouse_id`
- 库存流水与库位明细（`biz_inventory_location`）的联动

## 十、改动文件

```
backend/src/business/erp/serial_model.rs     + 仓储联动段（recompute / 流水 / 事务内联动），
                                             transition_status / batch_generate / take_numbers 改吃 &impl SqlExecutor
backend/src/business/erp/serial_handler.rs   事务边界下沉到接口层 + warehouse_id 校验 + 11 条单测
scripts/verify/w2c1_invariant.sh             40 项真机验收（自建/自毁隔离库）
docs/批次D-W2c1-库存联动不变式-验证证据.md     本文件
```

**未改**：V0138 / V0137（迁移已应用，改一个字符就会 checksum 冲突）、`inventory_model.rs`（仓里没有这个文件，
库存模型住在 `erp/model.rs` 的 115KB 大文件里；W2c-1 的库存逻辑只服务序列号联动，放在 `serial_model.rs`
可以避免动 `erp/mod.rs` 这个共享文件 → 少一个同仓冲突点）。
