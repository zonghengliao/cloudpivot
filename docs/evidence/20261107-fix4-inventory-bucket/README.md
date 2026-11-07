# FIX-4 库存两连 · 145 真机验收取证（t_763677cd）

> 卡面：**问题 1** 期初落「未分配桶」、收货落「默认库位桶」→ 同仓两桶并存（分库位库存永久漂移的触发条件）；
> **问题 2** 调拨明细指定库位却被「汇总桶优先」策略静默改扣汇总桶（不报错、不提示）。
>
> 两个问题都按卡面**推荐方案 ①** 修：期初与收货统一落「默认库位桶」；调拨改「精确匹配优先」，
> 指定库位不满足就**明确报错**（绝不静默改扣）。

---

## 一、改了什么（运行时）

| 文件 | 行数 | 内容 |
|---|---|---|
| `backend/src/business/erp/opening.rs` | +359 / -22 | 期初经 `stockin_model::ensure_location_row` 落**默认库位桶**；新增 `opening_location_row`（该仓无默认库位 / 默认库位停用才落未分配桶，与收货兜底逐字一致）、`load_opening_buckets`（重录 / 取消按 `biz_opening_inventory.location_row_id` 冲回**同一个桶**）；模块文档 ② 口径同步；测试夹具 cleanup 按 FK 顺序先删成本账 |
| `backend/src/business/erp/transfer_settle.rs` | +335 / -40 | `resolve_inventory_row` → **`pick_bucket`**（精确匹配优先）：`Exact` / `SpecifiedMissing` / `Unspecified{how}` / `Missing` / `Ambiguous`；新增 `bucket_of_row` / `default_location_bucket` / `bucket_count(_rows)` / `bucket_label` / `bucket_from_row`；新增错误变体 `LocationBucketEmpty` / `LocationAmbiguous`，`Insufficient` 增加 `location` 标签 |
| `backend/src/business/erp/transfer_handler_common.rs` | +44 / -8 | 三个结算错误的 409 报文（点名库位、可用量、汇总桶余量、「不会改扣整仓汇总桶」、多桶要求点名） |
| `backend/src/business/erp/inventory_writer.rs` | +16 / -6 | `lock_cost_bucket` 改**精确匹配优先**（`ORDER BY (location_id IS NOT DISTINCT FROM 明细库位) DESC`），与 `pick_bucket` 同口径 —— 调入侧两本账仍落同一个桶 |
| `backend/migrations/business/V0324__inventory_default_bucket.{up,down}.sql` | +262 / +39 | 存量「未分配桶」并入默认库位桶（引用改指向 + 数量/金额/锁定量合并 + 自检）；`biz_opening_inventory.location_row_id` 新列 + 存量回填 |
| `backend/src/business/erp/transfer_handler_db_tests_settle.rs` | +382 / -30 | P0-03 回归用例按新口径改写（指定库位就扣那个桶）+ 新增 4 条（不足报错 / 指定库位无账报错 / 唯一库位桶 / 多桶要求点名） |
| `backend/src/business/erp/opening.rs`（测试） | 见上 | 新增 2 条：期初落默认库位桶（含重录 / 取消冲同桶）、无默认库位仍落未分配桶 |

> 未改任何**已应用**迁移（V0319 的教训）；`V0324` 是本轮唯一新增迁移，`down` 只回退结构（数据合并不可逆，理由写在文件头）。

### 口径结论（写死在代码注释里）

```text
期初 / 收货：落「仓库默认库位桶」（biz_location.is_default = 1 且启用、未软删）
             该仓没有默认库位 → 落「未分配桶」（与收货兜底逐字一致）

调拨调出（pick_bucket）
  明细指定库位 → ① 该库位有账 → 扣它（不够就 409，绝不改扣别的桶）
                 ② 该库位没账 → 409「调拨不会改扣整仓汇总桶」
  明细未指定   → ③ 整仓汇总桶（历史口径）→ ④ 默认库位桶（新主路径）
                 ⑤ 唯一库位桶 → ⑥ 多桶并存 → 409 要求点名（不猜）
调拨调入：lock_cost_bucket 精确匹配优先（与 ③④⑤ 同一判据），两本账同一个桶
```

## 二、测试结果（本机 99，真库用例连本地 PG）

| 范围 | 命令 | 结果 |
|---|---|---|
| 全量 | `cargo test --features test-hooks`（不设 `ERP_DB_URL`，真库用例 SKIP） | **23 个测试目标 3554 passed / 0 failed**，exit 0 |
| 调拨结算（真库） | `ERP_DB_URL=... cargo test --lib business::erp::transfer_handler::db_settle` | **26 passed / 0 failed**（含改写的 P0-03 回归 + 4 条新增） |
| 库存期初（真库） | `ERP_DB_URL=... cargo test --lib business::erp::opening` | **18 passed / 0 failed**（含 2 条新增） |
| 邻接模块（真库） | stockin_handler / stockout / stocktake / inventory_audit / serial | 见 `raw/`（本机日志） |
| clippy / check | `cargo check --all-targets --features test-hooks` | 零 warning |

## 三、145 真机验收（真实 token curl + DB 断言）

隔离实例：新二进制副本 `target/debug/cloudpivot-fix4`，库 `cloudpivot_fix4n_20261107233725`，端口 `127.0.0.1:8045`；
**8033 服役实例全程未动**；`cloudpivot_dev` 先 `pg_dump -Fc` 备份到 `/root/backup/cloudpivot_dev_FIX4_20261107-233725.dump`（1.1M）。

### 3.1 全新库主链路（`raw/verify-145-fresh.log`）**PASS=33 FAIL=0**

| 验收 | 断言（真实输出） |
|---|---|
| ① 新库 → 期初入库 → 只有默认库位桶 | `biz_inventory_location` 该物料×仓 **1 行**、`location_id IS NULL` **0 行**、桶 = 默认库位 `F4D-*`、qty `100.0000`、成本账同一行 id、成本账未分配桶 0 行 |
| ② 调拨指定库位 A | 结算 DONE 200；库位账 `100 → 70`；调出仓仍只有 1 个桶、无汇总桶；**逐桶两本账 0 差异** |
| ③ 库位不足 | DONE **409**、`code=66001`、报文：`调出仓库存不足：物料 F4M-… 本次需要 200.0000，当前可用 70.0000（库位 F4D-…）`（点名库位与可用量）；单据仍 `IN_TRANSIT`；账一分不动（仍 70）；流水 0 条 |
| ④ 迁移幂等（本库） | 重跑 up.sql：`并入 0 个`；总数量 / 总金额不变 |
| ⑤ 全量对账 | 库存表 = 流水累计（(物料,仓库) 聚合）**0 差异**；逐桶两本账 **0 差异** |

### 3.2 存量数据升级路径（`raw/verify-145-legacy.log`）**PASS=10 FAIL=0**

库 = `cloudpivot_fix4legacy_20261107234033`（`cloudpivot_dev` 的**完整拷贝**，停在 V0323 前；共享库不动）。

```text
迁移前：cost_qty=714.0000  cost_amount=23580.0000  loc_qty=714.0000
        未分配桶（全库）8 行，其中落在「有默认库位的仓库」里的 **4 行**（其余 4 行属于没有默认库位的仓，按口径不动）
迁移日志：V0324：并入 4 个「未分配桶」到默认库位桶（原未分配桶 8 条），回填期初桶 2 行；
        总数量 714.0000 / 总金额 23580.0000 / 库位账合计 714.0000 与迁移前一致，两本账 0 差异
迁移后：cost_qty=714.0000  cost_amount=23580.0000  loc_qty=714.0000
        有默认库位仓库里的未分配桶：4 → **0**；两本账 0 差异；库存 vs 流水 0 差异
```

- `up.sql` 连跑 **3 次**：第 2/3 次都是「并入 0 个」，账目快照与第 1 次之后**完全一致**。
- P0-03 对账脚本 `docs/evidence/20261107-p003-inventory-chain/13_final_reconcile.sql` 在迁移后重跑：
  总量 `714.0000 = 714.0000`（diff 0）、逐 (物料, 仓库) 全 0、**逐桶 `(物料, 仓库, 库位字典)` 0 行差异**、负库存 0 行。

## 四、脚本（可重跑）

| 脚本 | 作用 |
|---|---|
| `30_prepare_145.sh` | 拉代码 → 建二进制副本 → **备份 cloudpivot_dev** → 建隔离库（全新 / 存量拷贝） |
| `31_start_instance.sh` | 全新库起隔离实例（8045） |
| `32_e2e_145.sh` | 全新库主链路验收（33 条断言，`TAG` 控制主数据编码，可反复跑） |
| `33_legacy_migration.sh` | 存量库升级路径（V0324 在真实数据上跑 + 幂等 + P0-03 对账） |
| `34_reset_legacy_copy.sh` | 重新造一份「迁移前」拷贝（迁移一跑就被并掉了，取证要重置） |
| `20_seed_legacy.sql` / `21_assert.sql` | 本机造「同期初+收货两桶」老数据 → 迁移 → 7 条断言（先在本机跑通再上 145） |

## 五、上线注意

1. **迁移会改数据（只改桶归属，不改总量）**：`V0324` 把「有默认库位仓库」里的未分配桶并进该默认库位桶。
   上线前先备份（`pg_dump -Fc`）；合并后无法逐条还原（`down.sql` 只删列，理由见文件头）。
2. **升级后「未指定库位」的调拨行为变化**：现在按「汇总桶 → 默认库位桶 → 唯一库位桶」兜底；
   多桶并存且明细未指定库位 → **409 要求点名库位**（以前是静默扣汇总桶）。
   前端调拨表单建议默认把「调出库位」带出来（该仓唯一/默认库位桶），避免人工遇到 409。
3. **8033 服役实例仍是旧二进制**（`source` 已到本卡 commit）：要生效应 `cargo build --bin cloudpivot`
   + `systemctl restart cloudpivot-test`；本卡按惯例没动，避免干扰其他 worker。
4. 遗留隔离库：`cloudpivot_fix4n_20261107233725`、`cloudpivot_fix4legacy_20261107*`（人工/策略放行后清理）。

## 六、与 P0-03（ce548eb）的关系

P0-03 修的是「调拨结算时库位账与成本账必须落**同一个桶**」（不变式），本卡修的是「**该落哪个桶**」
（用户意图）。两者不冲突且互补：本卡改口径后，两本账仍然逐桶相等 —— P0-03 的回归用例
`p003_settle_deducts_the_same_bucket_in_both_books` 已按新口径改写（断言 `100 → 70` 落在
**明细指定的库位桶**、汇总桶不动），断言本身（逐桶两本账 0 差异）原样保留。
