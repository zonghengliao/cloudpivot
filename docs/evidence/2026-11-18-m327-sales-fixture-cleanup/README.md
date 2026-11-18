# M3-27 · 销售域级单测夹具清场收口（证据包）

> 卡：`t_20e958fc`（M3-27【M3·销售】域级单测夹具清场收口，56 个 `M3xx-` 客户残留）
> 上游证据：`docs/evidence/2026-11-17-m3-26-sales-gap-closure/07_fixture_residue.txt`
> 演练库：`cp_m326_sales_gap_e2e_*`（脚本自己 `CREATE … TEMPLATE cloudpivot_test` + `DROP`，PG `172.18.10.145:5432`）

## 一、改了什么（只动测试代码）

同一条病根、10 个文件：**`cleanup()` 只在每条用例开头跑，客户行故意留着复用 ⇒ 最后一条用例的客户永久留在库里**。

| 文件 | 夹具命名空间 | 原来留下的 | 现在 |
|:--|:--|:--|:--|
| `sales/quotation_model.rs`（`mod tests`，M3-02） | `M302-CUST` | 客户 + 报价单 + 状态日志 | 新增 `zz_teardown_purges_fixtures`：造一张报价单 → 清场 → 按 **id** 断言 0 |
| `sales/quotation_handler.rs`（`mod tests`，M3-03） | `M303-CUST` | 客户 + 报价单 + 转单订单 + 状态日志 | 新增 `zz_teardown_purges_fixtures`：真转单一次 → 清场 → 按 **id** 断言 0 |
| `credit_release_tests.rs`（M3-13） | `M312-CUST` | 客户 | `purge_fixtures` 拆出 `purge_customer`（setup/收尾用），新增收尾用例 |
| `credit_concurrency_tests.rs`（M3-13） | `M313-CUST` | 客户 | 同上，新增收尾用例 |
| `sales/rebate_db_tests.rs`（M3-15） | `M315-` | 12 客户 | `cleanup()` 末尾 `purge_customers`，收尾用例加客户断言 |
| `sales/rebate_settle_db_tests.rs`（M3-16） | `M316-` | 8 客户 | 同上 |
| `sales/rebate_invoice_db_tests.rs`（M3-17） | `M317-` | 8 客户 | 同上 |
| `sales/sales_forecast_db_tests.rs`（M3-20） | `M320-` | 10 客户 | 同上 |
| `sales/sales_variance_db_tests.rs`（M3-21/22 共用） | `M321-` `M322-` | 3 + 4 客户 | `cleanup()` 末尾按两个命名空间各清一遍 |
| `sales/sales_variance_summary_db_tests.rs`（M3-22） | 复用上一条 | — | 收尾用例加客户断言 |

统一判据只有一处实现：`test_seed::purge_customers(prefix)` / `customer_count(prefix)`。

- 为什么先删三张子表：`biz_customer_contact` / `biz_customer_price` / `biz_price_tier` 对
  `biz_customer` 是 `ON DELETE RESTRICT`（实测 `pg_constraint.confdeltype='r'`），不先清子行删客户直接 23503。
- 为什么删客户不漂移断言：`ensure_customer` 的 id 由 `customer:<code>` 经 `fixture_uuid` 派生（确定性），
  下一轮重建还是同一行 —— 删掉不影响任何按 id 的断言。
- `quotation_state_tests.rs`（`M305-`）/ `quotation_convert_tests.rs`（`M308-`）本来就有 `cleanup()` + `zz_teardown`
  并且会删客户行，**未改动**（也与 M3-26 的残留清单一致：这两个前缀没在残留里）。

## 二、验收 1（真跑）：演练全绿且净增量为 0

```
bash scripts/verify/m326_sales_gap_closure_drill.sh
```

完整输出见 `01_drill_green.out`。关键三行：

```
  PASS 整个演练净增量为 0（跑完与基线一致）
  ---- 基线：cust=0 order=0 quote=0 fc=0 settle=0 policy=0 inst=0 log=0
  ---- 收尾：cust=0 order=0 quote=0 fc=0 settle=0 policy=0 inst=0 log=0
== 汇总：PASS=65 FAIL=0 ==
ALL PASS
```

各组「夹具净增量」行（全部 `= 0`）与用例数：

| 组 | 用例数 | 夹具净增量 |
|:--|--:|:--|
| 报价域 `business::erp::sales::quotation` | 85 | 0 |
| 返利域 `business::erp::sales::rebate` | 104 | 0 |
| 预测域 `sales_forecast_db_tests` | 11 | 0 |
| 预测域 `sales_variance`（偏差 + 汇总口径） | 13 | 0 |
| 信用域 `business::erp::credit` | 46 | 0 |
| E2E M3-24 / M3-25 | 2 / 2 | 0（strict） |
| 销售域整组回归 `business::erp::sales` | 363 | 0 |

> 用例数比卡面写的 361 / 44 各多 2 条，原因是**新增了 4 条 `zz_teardown_purges_fixtures` 收尾用例**
> （quotation_model / quotation_handler 各 1 条影响销售域整组，credit_release / credit_concurrency 各 1 条影响信用域）。
> 原有断言口径一条未改。

## 三、验收 2（真跑）：SKIP 守卫仍有牙

```
M326_BAD_URL=1 bash scripts/verify/m326_sales_gap_closure_drill.sh
→ PASS=55 FAIL=10，DRILL_RC=1（预期红）
```

完整输出见 `02_drill_badurl_red.out`。红在「本组出现 N 行 SKIP（真库用例被跳过 = 假绿）」与
「缺正向标记 [M3-24] / [M3-25 全链通过]」，不是被新加的清场动作掩盖。

## 四、验收 3（真跑）：静态门禁

```
cd backend && cargo fmt --check          → FMT_OK
cargo clippy -p cloudpivot --lib -- -D warnings → Finished（0 告警，rc=0）
```

## 五、踩到的两个真坑（写在这里，别再踩）

1. **清场函数不能顺手删客户行**：`credit_*_tests` 的 `purge_fixtures()` 同时被
   `db_concurrent_submit_is_admission_only` 的**用例中段**调用（每轮重来 `for round in 1..=3`）。
   第一版把 `DELETE FROM biz_customer` 塞进 `purge_fixtures()`，第 2 轮起提交全部 404
   「客户不存在」—— 手里那个 `customer` 主键悬空了。
   → 修法：业务行清场（`purge_fixtures`）与客户清场（`purge_customer`）分开，只有 `setup()` 和收尾用例调后者。
2. **`so_id` 是 DTO 的字符串**，`WHERE id = ?::uuid` 直接绑 `Value::Text` 会被驱动拒掉
   （`cannot bind parameter to column type uuid`）。收尾用例里先 `Uuid::parse_str` 再绑。

## 六、边界

- 零迁移、零权限码、零前端改动（`git status` 只有测试文件 + 本证据目录）。
- 物料 / 期间 / 科目等**其它主数据不删**：它们是共享夹具、id 确定性（`test_seed::fixture_uuid`），
  重复跑不会增长；本卡只收「客户行」这一类真正的残留源。
