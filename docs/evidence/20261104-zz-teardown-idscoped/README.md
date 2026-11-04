# `zz_teardown` 清场改造：payable_bridge / stockin_handler

**一句话结论**：`payable_bridge` 与 `stockin_handler` 两个模块的清场从「按单号前缀 / 全库孤儿扫」
改成「**先收集本模块夹具的 id 集合，再按 id 列表删**」，两条靠 `zz_` 名字排序兜底的收尾用例
拆成「清场自证（不承担清场职责）+ 模块命名空间口径复查」；两模块 `--test-threads=4` 连跑 3 轮全绿。

* 卡：`t_bfa81333`（上游审计 `t_7d480146` 的 R3/R4/R5 改造规范）
* 仓库：`/opt/cloudpivot`
* 隔离库：`cloudpivot_bfa81333`（`bash scripts/verify/erp_clean_db.sh`，`createdb -O alun` + 一次实例自举）
* 只改 `#[cfg(test)]` 测试侧代码，**生产代码 0 行变更**

---

## 1. 全目录 `zz_*` 清场用例清单（`backend/src/business/erp/`）

`grep -rn "async fn zz_" backend/src/business/erp --include=*.rs` = **17 条**（改造前）。
下表按「原清场 SQL 的口径」分类；**本卡范围**只有第 1、2 行（`payable_bridge` / `stockin_handler`），
其余 15 条只做登记，不动代码（归各自模块的卡）。

| # | 文件:行（改造前） | 用例 | 原清场 / 断言口径 | 本卡处置 |
|---|---|---|---|---|
| 1 | `payable_bridge.rs:2200` | `zz_teardown_purges_payment_status_logs` | ① 自造单走「显式跳过审批直付」→ 断言日志有 → 调 `cleanup_fixtures()` → 断言日志 0；② `cleanup_fixtures()` 里 9 条语句全部按 **`payable_no LIKE 'AP-P17-%'`** 前缀链删；③ 末尾「整库 `PAYABLE`/`VOUCHER` 孤儿 = 0」 | **已改**：前缀链 → 显式 id 集合；用例改名并退出清场职责（§2） |
| 2 | `stockin_handler_tests_fixtures.rs:453` | `zz_teardown_purges_post_approval_residue` | ① 自造收货单 → 提交过账审批 → 调 `cleanup_fixtures()`；② `cleanup_fixtures()` 里含 **3 处全库扫**：`entity_id NOT IN (SELECT id FROM biz_stock_in)`（S3）、`entity_no LIKE 'B1-%'`（S4）、`PAYABLE/VOUCHER … NOT IN (…)`（S5）；③ 末尾断言「**全库**无 `STOCK_IN_POST\|PENDING`」 | **已改**：3 处全库扫全部收窄；用例拆成两条（§3） |
| 3 | `serial_handler.rs:2734` | `zz_teardown_purges_serial_vouchers` | 调本模块 `purge_serial_vouchers()` + 按本夹具 id 断言 | 范围外，登记 |
| 4 | `aux_tests.rs:1156` | `zz_p21_aux_teardown_purges_fixtures` | 调 `cleanup()`（组内凭证/科目） | 范围外，登记 |
| 5 | `payable_payment.rs:4033` | `zz_teardown_purges_finance_status_logs` | 调 `cleanup_fixtures()` + `settle_orphans(FINANCE)` | 范围外（同族写法已由 `t_0d7c51f8` 改收敛判定） |
| 6 | `voucher.rs:3614` | `zz_teardown_purges_fixture_accounts` | 用例内 `DELETE … WHERE account_code LIKE 'P2C1%'` | 范围外，登记（仍为前缀删，待本模块卡） |
| 7 | `voucher.rs:3640` | `zz_teardown_purges_finance_status_logs` | 调 `cleanup_fixtures()` + `settle_orphans(FINANCE)` | 范围外，登记 |
| 8 | `stocktake_tests.rs:1140` | `zz_teardown_purges_stocktake_fixtures` | 调本模块 `purge_fixtures()` + 按夹具断言 | 范围外，登记 |
| 9 | `opening.rs:2772` | `zz_teardown_purges_opening_voucher_status_logs` | 调 `cleanup()` + `settle_orphans` | 范围外，登记 |
| 10 | `model_tests.rs:898` | `zz_teardown_purges_fixture_accounts` | 用例内 `DELETE … WHERE account_code LIKE 'W1B%'` | 范围外，登记（前缀删） |
| 11 | `batch.rs:956` | `zz_teardown_purges_finance_status_logs` | 批量生成 + `settle_orphans` | 范围外，登记 |
| 12 | `purchase_handler.rs:3002` | `zz_teardown_purges_po_approval_residue` | 调 `purge_fixture_docs()` + 按 id 断言 | 范围外，登记 |
| 13 | `voucher_write.rs:1702` | `zz_teardown_purges_voucher_status_logs` | 调 `cleanup_fixtures()` + `purge_orphans(FINANCE)`（整库） | 范围外，登记（整库孤儿扫待收口） |
| 14 | `voucher_query.rs:1193` | `zz_teardown_purges_fixture_accounts` | 用例内 `DELETE … WHERE account_code LIKE 'P2D1%'` | 范围外，登记（前缀删） |
| 15 | `stockout_tests.rs:822` | `zz_dc13_fixtures_are_cleaned` | 调 `cleanup_fixtures()` + 按夹具断言 | 范围外，登记 |
| 16 | `transfer_handler.rs:4491` | `zz_teardown_purges_fixture_residue` | 调 `cleanup()` + 按夹具断言 | 范围外，登记 |
| 17 | `contract_approval_tests.rs:864` | `zz_teardown_purges_fixtures` | 按本模块合同 id 链 `purge_fixtures()`（`t_f404258c` 已改 id 集合） | 范围外，已由上游卡改好 |

改造后本卡范围内：`payable_bridge` **0 条** `zz_`，`stockin_handler` **1 条**（只做命名空间复查，不做清场）。

---

## 2. payable_bridge：改造对照

清场入口 `cleanup_fixtures()`，每条用例的 `setup()` 都会先调它（自清上一轮）。

| 项 | 原清理条件 | 新清理条件 |
|---|---|---|
| 应付单 | `DELETE … WHERE payable_no LIKE 'AP-P17-%'` | 先 `SELECT id FROM biz_payable WHERE supplier_id = 'e7e70001-…f170'::uuid`（**本模块独占的夹具供应商主数据行**），再 `DELETE … WHERE id IN (<显式 uuid 列表>)` |
| 付款单 | `payable_id IN (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-P17-%')` | `id IN (<上一步收集的付款单 uuid 列表>)` |
| 凭证 / 分录 | `source_id IN (…前缀链…)` | `id IN (<收集的凭证 uuid 列表>)` / `voucher_id IN (同上)` |
| 状态日志 `PAYABLE`/`VOUCHER` | `entity_id IN (…前缀链…)` | `entity_id IN (<收集的应付/凭证 uuid 列表>)`，仍在删单据**之前**执行 |
| 审批实例 | `biz_type='PAYABLE_PAY' AND biz_id IN (…前缀链…)` | 先收集实例 id，`UPDATE prev_instance_id=NULL` → `DELETE task/cc` → `DELETE instance WHERE id IN (…)` |
| 通知 / 通知接收人 | `biz_id IN (前缀链的实例 id::text)` | `biz_id IN (<收集的实例 id::text 列表>)`，并显式先删 `sys_notice_receiver` |
| 夹具用户 | `username IN ('p17_creator','p17_ap1','p17_ap2')`（3 处子查询） | 三个**固定 uuid** `IN (…)`（`CREATOR`/`APPROVER1`/`APPROVER2`） |
| 收尾用例 ① | `p17_zzz_fixture_users_are_removed`（注释写明「靠名字排最后跑」） | `p17_fixture_users_are_removed_by_cleanup`：`setup()` 断言 3 人存在 → 调 `cleanup_fixtures()` → 断言按 uuid 为 0 |
| 收尾用例 ② | `zz_teardown_purges_payment_status_logs`（承担「模块跑完库干净」的清场职责） | `p17_skip_approval_payment_logs_are_purged_by_id`：只证明清场按 id 有效；「整库无孤儿」保留 `settle_orphans(FINANCE)` 收敛判定（真孤儿仍红） |

新增两个夹具工具（模块内私有）：`fixture_ids(sql)`（跑 `SELECT id` 收集 id 集合）、
`id_list(ids)`（`Uuid` 列表 → `IN (…)` 字面量，空集给零 uuid 而不是 `IN ()`）。

**为什么能直接这么改而不放宽**：`new_payable()` 是模块里唯一造应付单的入口，且**每张单都写
`SUPPLIER_ID` `e7e70001-0000-4000-8000-00000000f170`**（全库只有本模块用它）——所以
「按 `supplier_id` 选 id 集合」与「按 `AP-P17-%` 前缀选」覆盖的行**完全一致**，
但前者跨轮次/跨进程也成立，且不再依赖单号格式。

---

## 3. stockin_handler：改造对照

清场入口 `cleanup_fixtures()` + `purge_post_approval_traces()`，同样在每例 `setup()` 前置执行。

| 项 | 原清理条件 | 新清理条件 |
|---|---|---|
| 过账/作废实例（S7 的 where 口径） | `biz_type IN ('STOCK_IN_POST','STOCK_IN_CANCEL') AND (biz_id IS NULL OR biz_id NOT IN (SELECT id FROM biz_stock_in) OR biz_id IN (…夹具供应商…))` —— **全库孤儿扫** | `biz_type IN (…) AND biz_id IN (SELECT id FROM biz_stock_in WHERE supplier_id IN (<夹具供应商 uuid>))`，仅本模块 id 集合 |
| `STOCK_IN` 状态日志（S3） | `entity_type='STOCK_IN' AND (entity_id IN (夹具收货单) OR entity_id NOT IN (SELECT id FROM biz_stock_in))` | `entity_type='STOCK_IN' AND entity_id IN (SELECT id FROM biz_stock_in WHERE supplier_id IN (<夹具供应商 uuid>))` |
| `B1-%` 单号前缀删（S4） | `DELETE … WHERE entity_no LIKE 'B1-%'` | **删除该语句**（本轮日志已按 id 先删；历史孤儿只在 `zz_` 收口） |
| `PAYABLE`/`VOUCHER` 日志（S5） | `entity_type IN ('PAYABLE','VOUCHER') AND entity_id NOT IN (SELECT id FROM biz_payable) AND entity_id NOT IN (SELECT id FROM biz_voucher)` —— **跨模块整库孤儿扫**，且位置在删凭证/应付**之后**（永远删不到本轮的行） | 两条：`entity_id IN (SELECT id FROM biz_voucher WHERE source_id IN (<夹具收货单 id 链>))`、`entity_id IN (SELECT id FROM biz_payable WHERE source_id IN (<同上>))`，并**移到删凭证/应付之前** |
| 收尾用例 ①（S7） | `zz_teardown_purges_post_approval_residue`：自造单 → 调 `cleanup_fixtures()` → 断言本用例 id 干净 → 再断言「**全库**无 `STOCK_IN_POST\|PENDING`」 | `post_approval_residue_is_purged_by_cleanup`：前三条断言原样保留；「全库」那条移出（拆到 ①） |
| 收尾用例 ②（S8） | （同上，「全库无 PENDING」） | 新增 `zz_module_namespace_has_no_residue`：**只复查不清场**——① 夹具供应商名下收货单 = 0；② `STOCK_IN_POST/CANCEL` 命名空间内 `PENDING` 实例 = 0；③ `STOCK_IN` 日志孤儿 = 0（`fixture_hygiene::settle_orphans`，收敛 5s） |

新增：`fixture_suppliers()`（夹具供应商 id 列表，删除集的根）、`purge_orphan_post_instances()`
（**只在 `zz_` 复查里**用的孤儿实例收口，判据先按本模块私有的两种 `biz_type` 收口）。

**为什么不直接用「删全库孤儿」兜底**：`biz_stock_in` / `biz_voucher` / `biz_payable` 是跨模块共享表，
`NOT IN (SELECT id FROM …)` 在并行时会把别的模块「单据已删、日志还没删」的中间态当垃圾删掉
（审计 S3/S5 的原判）。本模块**本轮**产生的日志先于单据按 id 删，一条都不会变孤儿；
历史孤儿（改造前的遗留）由 `zz_` 的收敛判定一次性收干，收干后再断言 0。

**孤儿实例为什么移出每例清场**：`uk_biz_flow_instance_biz_active` 只对
`status='PENDING' AND biz_id IS NOT NULL` 生效，且按 `(biz_type, biz_id)` 唯一 —— 孤儿实例的
`biz_id` 指向已删单据，不会顶住任何新提交；它只在「整库/命名空间计数」类断言里碍事。
所以把它从「每例 setup 扫全库」降级为「`zz_` 收尾按命名空间收」。

---

## 4. 验收实测

隔离库 `cloudpivot_bfa81333`（干净库，迁移 219 / 163 条）。基线（改造前同一库）：

| 模块 | 命令 | 结果 |
|---|---|---|
| `payable_bridge` | `cargo test --lib business::erp::payable_bridge -- --test-threads=4` | **23 passed / 0 failed**（36.10s） |
| `stockin_handler` | `cargo test --lib business::erp::stockin_handler -- --test-threads=4` | **132 passed / 0 failed**（406.29s） |

改造后（`bash /root/bfa8-run.sh`，两模块 × 3 轮，同一库连跑，第 2/3 轮跑在上一轮残留上）：

```
round=1 module=payable_bridge  exit=0 test result: ok. 23 passed; 0 failed; … finished in 45.13s
round=1 module=stockin_handler exit=0 test result: ok. 133 passed; 0 failed; … finished in 418.41s
round=2 module=payable_bridge  exit=0 test result: ok. 23 passed; 0 failed; … finished in 44.91s
round=2 module=stockin_handler exit=0 test result: ok. 133 passed; 0 failed; … finished in 419.96s
round=3 module=payable_bridge  exit=0 test result: ok. 23 passed; 0 failed; … finished in 45.83s
round=3 module=stockin_handler exit=0 test result: ok. 133 passed; 0 failed; … finished in 420.16s
```

`stockin` 从 132 → **133** 条：原 `zz_teardown_purges_post_approval_residue` 拆成
`post_approval_residue_is_purged_by_cleanup`（清场自证）+ `zz_module_namespace_has_no_residue`（命名空间复查）。
`payable_bridge` 仍 23 条（两条收尾用例改名，未增删）。

库侧残留（`bash /root/bfa8-residue.sh`，3 轮结束后只读复查；`setup()` 只清「上一轮」，
所以最后一次运行留下的行会留到下次运行 —— 关键看**孤儿与累积**）：

```
stockin 夹具收货单 = 1        ← 最后一次运行最后一条用例的产物（每轮 setup 会清掉）
stockin 夹具采购单 = 0
P17 应付单 (AP-P17-*) = 1      ← 同上
PENDING STOCK_IN_POST/CANCEL 实例 = 0
STOCK_IN 日志孤儿 = 0
PAYABLE 日志孤儿 = 0
VOUCHER 日志孤儿 = 0
```

即：**跨轮次不累积**（第 2/3 轮都跑在上一轮残留上仍全绿，改造前该场景会因
`uk_biz_flow_instance_biz_active` / 种子流程行互写而随机假红），且三类状态日志孤儿都为 0。

### 跨模块并发对照（审计 S5 的直接验证）

`bash /root/bfa8-concurrent.sh`：**两个 `cargo test` 进程同时**跑 `stockin_handler`（133 条）
与 `payable_payment`（P7/D5，29 条），同一库、各 4 线程。这正是旧 `S5` 语句
（`DELETE … WHERE entity_type IN ('PAYABLE','VOUCHER') AND entity_id NOT IN (…)`）会误删的现场：

```
stockin exit=0 test result: ok. 133 passed; 0 failed; … finished in 430.81s
p7      exit=0 test result: ok. 29 passed; 0 failed; … finished in 20.44s
```

并发后再看库侧残留（`/root/bfa8-residue.sh`）：夹具收货单 1、P17 应付单 1（都是「最后一次运行
最后一条用例」的产物，每轮 `setup()` 会清），**`PENDING` 实例 0、三类日志孤儿 0**。

门禁 grep（本卡范围两个文件）：

```
$ grep -n "LIKE '" payable_bridge.rs stockin_handler_tests_fixtures.rs
stockin_handler_tests_fixtures.rs:290:        // ⚠️ 这里**不再**有 `entity_no LIKE 'B1-%'`（审计 S4）…   ← 仅注释
$ grep -n "NOT IN (SELECT id FROM" payable_bridge.rs stockin_handler_tests_fixtures.rs
stockin_handler_tests_fixtures.rs:596:  … AND biz_id NOT IN (SELECT id FROM biz_stock_in)   ← 仅 zz_ 命名空间孤儿收口
$ grep -niE "period[[:space:]]*(=|LIKE)" payable_bridge.rs stockin_handler_tests_fixtures.rs
（无清场语句，仅 new_payable() 里给 INSERT 算 period 字符串）
```

---

## 5. 已知边界 / 后续

1. **本卡只改两个模块**。§1 里 #6/#10/#14（`voucher`/`model_tests`/`voucher_query` 的
   `account_code LIKE '…%'`）与 #13（`voucher_write` 的整库 `purge_orphans`）仍是旧口径，
   归各自模块的卡处理。
2. `payable_bridge` 的「按 `supplier_id` 选 id 集合」依赖一条约定：**本模块的应付单只用
   `SUPPLIER_ID`**。若将来加别的供应商夹具，必须同时把新供应商 id 加进 `cleanup_fixtures()` 的
   收集条件（否则会漏清）。
3. `stockin` 的孤儿实例收口（`purge_orphan_post_instances`）仍是「模块命名空间内的弱引用扫描」，
   不是纯 id 集合 —— 因为孤儿实例的单据行已经删了，无法反查 id。这是 R4 允许的
   「收敛式判定 + 写明理由」的用法，判据先按本模块私有 `biz_type` 收口。
