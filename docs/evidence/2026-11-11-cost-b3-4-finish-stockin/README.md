# COST-B3-4【成本】结算 · 完工入库 —— 证据

卡：`t_5f3f2da5`（ARCH-9 §8.1 的 B3-4：「生成 `stock_in(PRODUCTION)` + 明细 + `inventory_writer` 三本账」）
设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md` §3.3 ② / §4.7.4 / §4.7.6 / §4.8 ③

## 交付物

| 文件 | 作用 |
|---|---|
| `backend/src/business/erp/cost_settle_finish.rs` | **结算 ①**：`pending_lines`（待入库计算单）/ `generate`（逐成本对象生成入库单 + 明细 + 三本账 + 计算单回写 + 生产批登记）/ `stock_in_ids_of`（红冲反查）。无 HTTP 入口，供 B3-6 结算事务调用 |
| `backend/src/business/erp/cost_settle_finish_db_tests.rs` | 7 条真库用例（`ERP_DB_URL` 开关） |
| `backend/migrations/business/V0382__stock_in_production_sourceless.{up,down}.sql` | 解除 V0376 与 V0199 两条约束的互斥（V0376 头注释「已知开放问题」点名留给 B3-4） |
| `backend/src/shared/error_code.rs` | 新增 `66074 ERP_COST_FINISH_ALREADY_SETTLED`（+ 66xxx 稳定性数组登记） |
| `backend/src/business/erp/mod.rs` | 模块登记 |

## 卡面三条验收怎么证

| 验收 | 证据 |
|---|---|
| **入库单类型 PRODUCTION** | `01_psql_probe.out` ④（同一条 INSERT 在 V0382 后写入成功）+ `05_real_db_tests.out` 用例 1（`in_type='PRODUCTION'`、`status='POSTED'`、挂 `cost_object_id`/`cost_period_id`、无采购单/无供应商/无应付） |
| **必须走 `inventory_writer`** | `04_gates.out` 的 `inventory_write_gate.py`：**零越界**（本卡代码没有一行 `biz_inventory*` 写 SQL；三本账全部经 `stockin_model::apply_cost` / `apply_location_qty` / `inventory_writer::insert_txn`） |
| **三本账（数量/金额/批次）同时更新** | 用例 1 逐本断言：成本账 `qty=800 / amount=22311.1104 / avg_cost=27.888888`；库位账 `qty=800`；批次账 `biz_lot(source_type='PRODUCTION', source_doc_type='COST_PERIOD', source_doc_id=批次)`；流水 `PRODUCTION_IN / IN / STOCK_IN / period_code=结算期间`。用例 6 另证逐桶累加（同库位两笔移动加权 25.901234、未分配桶独立） |

## 为什么要 V0382（不是「顺手改设计」）

V0376（B1-5）逐字落地了设计 §3.3 的 `ck_biz_si_production`：`PRODUCTION ⇒ 非无源 + 无供应商`；
而 V0199 的 `ck_biz_si_sourceless_pair` 钉死 `is_sourceless = (source_order_id IS NULL)`。
两条叠加 ⇒ 完工入库**必须挂一张采购单**，而 §4.7.4 的完工入库由结算批次直接生成、根本没有采购单。

`01_psql_probe.out` ② 是实测：

```
ERROR:  new row for relation "biz_stock_in" violates check constraint "ck_biz_si_sourceless_pair"
```

V0376 头注释「★ 已知开放问题」已把这条写成「最小修法（留给 B3-4 结算卡或独立迁移）」，
本卡按那段话落地，**不改** §3.3 的 `ck_biz_si_production`。

## 事务 / 补偿（AGENTS 约定 11）

`generate` 只落语句，**不 commit**：调用方（B3-6）在同一个事务里锁批次行、校验状态、
写完三张凭证与批次合计后统一提交。事务提交后本卡没有任何动作 ⇒ 不存在「提交后失败留死单」。

两道幂等闸都是 409 + `66074`：批次级（已有非作废入库单）、计算单级（CAS 影响 0 行）。
`CANCELLED` 不算「已生成」⇒ 红冲作废后同批次可重新结算（用例 3 实证）。

## 已知偏差

1. **一张入库单 = 一个成本对象**（§4.8 ③a「逐成本对象 INSERT」），没用 `source_type='COST_PERIOD'`
   表达来源：`uk_biz_si_source_doc (source_type, source_id)`（V0305）是唯一索引，同期间第二张就撞键；
   来源列用 V0376 给的 `cost_period_id`。
2. **入库单 `invoice_received=false`**（完工入库没有发票；该列 `NOT NULL`，默认值 true 是采购口径）。
3. **写了一条 `null → POSTED` 状态日志**（`state_machine::log_created`，P10「建单留痕」）——
   设计 §4.8 没写这一步，但入库单是直接落 `POSTED` 的，不留痕的话「这单谁建的」查不到。
4. **完工 0 件的成本对象不建入库单**（`ck_biz_si_item_qty` 要求 `qty > 0`）；该行保持 `CALC`，
   由 B3-6 决定批次怎么落（用例 4）。
5. 三张凭证（B3-5）、批次状态与合计回写 + 事务内 6 条断言（B3-6）、红冲（B3-7）都不在本卡。

## 文件清单

- `01_psql_probe.out` —— 库层探针：V0382 前被拦 / 后被放行
- `02_migration_replay.out` —— 空库全链重放 302/302
- `03_full_lib_tests.out` —— 全仓 lib 单测（4687 passed / 0 failed）
- `04_gates.out` —— 库存写入口闸 / fmt / clippy -D warnings / 角色码闸
- `05_real_db_tests.out` —— 真库 7/7（空库重放后的库上跑）
- `06_residue.out` —— 两个演练库残留全 0
