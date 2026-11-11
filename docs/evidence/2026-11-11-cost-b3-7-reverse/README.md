# COST-B3-7 红冲 —— 实测证据（2026-11-11）

卡面（ARCH-9-3 §8.1 · B3 批次）：

```
红冲：逐凭证 `reverse_posted` + 库存回冲 + 状态 → REVERSED
```

设计出处：`docs/architecture/ARCH-9-成本核算升级设计.md`
§4.8（结算事务与失败补偿 → 「反向场景（红冲）」四段 + 库存回冲失败的边界）、
§5.2 #30（`POST /business/erp/cost-period/{id}/reverse`，`cost-period:reverse`，`{reason}` ≥10 字）、
§4.1（`uk_biz_cost_period` 排除 `REVERSED`，红冲后同期间可重建）。

## 本卡落地

| 段 | 实现 | 文件 |
|---|---|---|
| ① 锁批次 + 状态/期间两闸 | `FOR UPDATE` + 锁内复核 `SETTLED`（状态机 `SETTLED→REVERSED` 是唯一红冲边）+ 期间已结账 409 人话 | `cost_reverse_run::lock_period` / `ensure_period_open` |
| ② 逐凭证红冲 | `voucher::reverse_posted`（红字 `PZR-` 号段，原凭证保持 `POSTED` 只打 `reversed_by`）；红字凭证日期 = **被红冲期间末日** | `cost_reverse_run::reverse_vouchers` |
| ③ 库存回冲 | `inventory_writer::subtract_cost` / `subtract_location_qty` / `insert_txn(OUT, PRODUCE_CANCEL)`，金额取**过账流水实际写入值** | `cost_reverse_run::reverse_stock_ins` / `reverse_plan_of` |
| ④ 生产入库单作废 | 状态机 `POSTED→CANCELLED`（必填原因）+ CAS 写 `cancelled_at/by/reason` + `biz_status_log` | 同上 |
| ⑤ 计算单回未结算 | `status='CALC'`、`stock_in_id=NULL`（`ck_biz_cost_item_settled` 要求两列同时动） | `cost_reverse_run::reset_items` |
| ⑥ 批次 → `REVERSED` | 状态机 + CAS 写 `reversed_at/by/reason`；**保留** `settled_at` 与三个 `*_voucher_id` | `cost_reverse_run::write_back` |
| HTTP | `POST /business/erp/cost-period/{id}/reverse`（权限 `business:erp:cost-period:reverse`，入参只有 `reason`） | `cost_period_handler::cost_period_reverse_api` |

## 实测输出（真库 `mig_replay_b34`，`ERP_DB_URL` 开关）

```
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_b34 \
  cargo test --lib cost_reverse_run -- --test-threads=1
→ 15 passed; 0 failed（8 纯逻辑单测 + 7 真库用例），21.22s
```

真库用例逐条（`01_real_db_tests.out`）：

| # | 用例 | 卡面验收 | 实测 |
|---|---|---|---|
| 1 | `reverse_end_to_end_in_one_transaction` | 三张凭证全部红冲 / 库存回冲走 inventory_writer / 入库单作废 / 状态 REVERSED | 4 张蓝字 → 4 张红字（`PZ-…000144…147` → `PZR-…000013…016`，合计 4000/4000/4000/22311.1104）；原凭证仍 `POSTED` 且 `reversed_by` 指向红字；库存 800→0（数量与金额）、库位账 0、流水 `OUT+PRODUCE_CANCEL` 1 条；入库单 `CANCELLED` + 原因；计算单 `CALC` + `stock_in_id=NULL`；批次 `REVERSED` + `reversed_at/reason`；`4001.02/.03` 与 `4101.01/.02` 本期净发生归零（蓝红同期互抵），`4001.01` 只剩夹具领料凭证 19000（不属于本批次） |
| 2 | `reverse_then_recalc_does_not_hit_the_unique_key` | 单测：红冲后再算一次，唯一键不冲突 | 同期间新建 `CS37-R02` 成功（`uk_biz_cost_period` 排除 `REVERSED`）；**真跑 `calc_items`** 写出 1 条计算单（Σ料 19000）；再插第二张活跃批次仍被唯一键拒（证明不是把键删了） |
| 3 | `reverse_is_rejected_when_stock_already_shipped` | 库存回冲失败的边界（设计 §4.8） | 先出库 700（成本账剩 100）⇒ 红冲 409 + `66078`，报文含「已出库」「销售退货」；**整笔零写入**（无红字凭证 / 入库单仍 `POSTED` / 批次仍 `SETTLED` / 计算单仍绑单 / 无反向流水 / 状态日志仍只有结算那条 / 库存 100 未动） |
| 4 | `reverse_requires_settled_status` | 状态闸 | `APPROVED` 批次 ⇒ 409 + `66073`，报文点名允许态 `SETTLED`，零写入 |
| 5 | `reverse_requires_reason_of_at_least_10_chars` | 设计 #30 `{reason}` ≥10 字 | `None` 与 9 字均 400 `10001`，零写入；够 10 字放行 |
| 6 | `second_reverse_is_rejected` | 幂等 | 再红冲 ⇒ 409 + `66073`（`REVERSED`），红字凭证 / 反向流水 / 入库单条数不变 |

全量单测（`02_full_lib_tests.out`）：`cargo test --lib` → **4738 passed; 0 failed**
（B3-6 收口时 4722，本卡新增 16 条：8 纯逻辑 + 7 真库 + 1 路由注解）。

门禁（`03_gates.out`）：`cargo fmt --check` PASS、`cargo clippy --all-targets -- -D warnings` PASS。

夹具残留（`04_residue.out`）：`CS37/CB37/FIX37` 前缀 12 项全 0（本卡夹具全部自清）。

## 已知偏差（详见模块头与卡内汇报）

1. 设计 §4.8 ① 允许 `SETTLED/CLOSED` 都能红冲，但状态机 `COST_PERIOD_EDGES` 只有
   `SETTLED → REVERSED` 一条边，且 B3 批次内没有代码把批次置 `CLOSED`（关期卡未实施）⇒
   本卡只接受 `SETTLED`；`CLOSED → REVERSED` 待关期/反关期卡补边（边表是 B3-1 定稿，本卡不动）。
2. 单头 `labor/overhead/finish_voucher_id` 与 `settled_at` **保留不置空**：
   `ck_biz_cost_period_settle` 对 `REVERSED` 同样生效，且审计要能顺藤找到「红了哪几张蓝字」。
3. 红冲不审批（设计 §4.8 反向路径没有审批环节，只要 `cost-period:reverse` + 原因 ≥10 字）。
4. `cost_settle_voucher::voucher_ids_of` 补上 `AND reversed_by IS NULL`（B3-5 留给 B3-7 的
   辅助函数，原 SQL 只过滤 `is_red = 0` —— 被红冲过的蓝字仍是 `is_red = 0`，会让
   「还剩几张可红」永远数不对；不影响 B3-5 的既有断言，已复跑 6/6 绿）。
5. 新增错误码 `66078`（`ERP_COST_REVERSE_BLOCKED`）三个触发点：库存退不回来 / 流水缺失 /
   无蓝字可红 —— 都是「修数据后能从界面继续」的边界，不是 500。
