# COST-B1-10 领料单挂成本对象 —— 证据

> 卡面：`stockout_dto` 加字段 + 校验「对象 OPEN」+ 复算出库借方科目（ARCH-9 §4.7.1 / §8.1 B1-10）
> 提交：`434aad42`（本目录随提交一起入库）

## 1. 卡面验收 → 落点 → 实测

| 卡面验收 | 代码落点 | 实测证据 |
|---|---|---|
| 领料单**可选**成本对象 | `stockout_dto::CreateStockOutReq.cost_object_id`（建 / 改共用，改单整单覆盖：不传=清空）；落库 `biz_stock_out.cost_object_id`（V0375 的列，B1-5 已建） | `02_real_db_tests.out` 用例 ①：落库值读回 = 传入值；用例 ④：不挂 → 挂上 → 清空三步都对 |
| 选了就校验**对象 = OPEN** | `stockout_dto::ensure_cost_object_usable`（**纯函数**：不挂放行 / 非 ISSUE 拒 / 不存在 400 / 状态非 OPEN 409+66057） | `01_pure_tests.out` 6 条纯逻辑用例；`02_real_db_tests.out` 用例 ②：CLOSED、CANCELLED 各自被 409+66057 拦下，且**一行都没落库** |
| 复算出库借方科目：挂对象 → `4001.01`，不挂 → 原逻辑 | `stockout_base::debit_account_of_doc(out_type, cost_object_id)`（出库过账**唯一**取借方科目入口）；`voucher::ACCOUNT_PRODUCTION_MATERIAL = "4001.01"`；出参 `StockOutRes.debit_account_code/name` 直接复算 | `01_pure_tests.out`：挂/不挂/报废挂对象三条；`02_real_db_tests.out` 用例 ⑤：**真实凭证**「挂对象 借 4001.01 / 贷 1405」「不挂 借 6401 / 贷 1405」 |
| 单测：挂 / 不挂 / 对象已关闭 | 三种情况各一条（纯逻辑 + 真库各一套） | 见上；`02` 用例 ⑥ 还多一条并发兜底（草稿期间对象被关批 → 过账 409+66057、单仍 DRAFT、账没动） |

## 2. 改动文件

| 文件 | 改什么 |
|---|---|
| `backend/src/business/erp/stockout_base.rs` | `StockOutRow` / `NewStockOut` 加 `cost_object_id`；`HEADER_COLUMNS` 加列；新增 `debit_account_of_doc()`（+3 条纯逻辑单测） |
| `backend/src/business/erp/stockout_dto.rs` | 入参 `cost_object_id`（三态 + UUID 解析）；`ensure_cost_object_usable()` 纯函数 + 状态中文名；出参加 `cost_object_id / cost_object_no / debit_account_code / debit_account_name`；`fill_cost_object_labels()`（整页一条 SQL）；+9 条纯逻辑单测（含 JSON 键名契约） |
| `backend/src/business/erp/stockout_model.rs` | `create` / `update` 写 `cost_object_id`（`NULLIF(?::text,'')::uuid`）；新增 `cost_object_ref()`（窄查询：对象号+状态）与 `cost_object_labels()`（`= ANY(?::text::uuid[])` 批量取名）；挂载 B1-10 真库用例模块 |
| `backend/src/business/erp/stockout_handler.rs` | `plan()` 校验成本对象（查库 → 纯函数判定）并把 id 带进建/改单；过账 ①d「对象仍 OPEN」复检；⑤ 借方科目改用 `debit_account_of_doc`；详情/列表回填对象号 |
| `backend/src/business/erp/voucher.rs` | 新增 `ACCOUNT_PRODUCTION_MATERIAL` / `ACCOUNT_PRODUCTION_MATERIAL_NAME`（4001.01，与 V0377 种子逐字一致） |
| `backend/src/shared/error_code.rs` | 新增 `ERP_COST_OBJECT_NOT_OPEN = 66057`（409）+ 段表 + 码表守卫 |
| `backend/src/business/erp/stockout_cost_object_tests.rs` | **新文件**：6 条真库用例（`ERP_DB_URL` 开关） |
| `stockout_tests.rs` / `stockout_fifo_tests.rs` / `stockin_handler_tests_location_dict.rs` / `shipment_posting.rs` / `receivable_bridge.rs` | 结构体新增字段的机械同步（`cost_object_id: None`） |

## 3. 设计点 / 决策（写下来免得下游猜）

1. **借方分流已接到过账**（不只停在 DTO 上）。
   设计 §8.1 把「`debit_account_of` 加分支」列成 **B3-8**，但 §4.7.1 又写明「改动点唯一：
   `stockout_handler.rs` 的 `debit_account_of(out_type)` 前加一个分支」，且本卡验收要求
   「复算出库借方科目：挂对象 → 4001.01」。本卡按 §4.7.1 落地（`post_out_tx` ⑤ 已改为
   `debit_account_of_doc(&header.out_type, header.cost_object_id)`，真库用例实证凭证分录）。
   ⇒ **B3-8 届时只剩回归**，建议作废或改成「领料借方分流回归」。
2. **只有 `ISSUE`（领用）单允许挂对象**。依据两条：§4.3 的料归集只扫
   `out_type='ISSUE' AND cost_object_id IS NOT NULL`（挂到报废单上永远归集不到，是死数据）；
   报废 / 盘亏是资产损失，必须进 `1901`。因此两层防线：写入侧 400 拒绝；
   记账侧 `debit_account_of_doc` 对非 ISSUE **永不**返回 4001.01（历史数据 / 人工改库兜底）。
3. **过账前多一道「对象仍 OPEN」复检**（`post_out_tx` ①d）。关批侧的前置 ④ 会拦
   「还有未过账领料」，但那是关批时的检查，并发下过账必须自己再看一眼 —— 否则料会记到
   已结转的对象上。被拦 → 409 + 66057，整笔回滚（用例 ⑥ 实证单仍 DRAFT、库存没动）。
4. **66057 不复用 66055**：66055 是「关批被拦，逐条列 5 条前置」，用户动作是补归集；
   66057 的动作是「换个在制对象 / 新建一批」，前端按码给不同引导。
5. **对象号是「名称」**：`biz_cost_object` 没有独立名称列，对象号（`CB-yyyyMMdd-NNNNNN`）
   就是用户认的名字 ⇒ 出参补 `cost_object_no`（实时取，约定 13），列表整页一条 SQL
   （`= ANY(?::text::uuid[])`，不逐行查）。
6. **改单三态**：`cost_object_id` 不传 / 空串 = 清空（与 `reason` / `remark` 同一套「整单覆盖」口径，
   不一致会让「清不掉」变成 bug）。

## 4. 实测输出（原始文件）

| 文件 | 内容 |
|---|---|
| `01_pure_tests.out` | 纯逻辑：`stockout_base` 3 条 + `stockout_dto` 9 条 + 66057 码表守卫 —— 15 passed / 0 failed |
| `02_real_db_tests.out` | 真库 6 条（`--nocapture`，报文与凭证分录都在里面）—— 6 passed / 0 failed |
| `04_isolated_gate.log` | 隔离工作树 `@434aad42`：`cargo fmt --check` FMT_EXIT=0 / `cargo clippy --all-targets -D warnings` 0 告警（5m22s）/ **全量 `cargo test` TEST_EXIT=0，31 个目标 4494 passed / 0 failed** |
| `05_isolated_real_db.out` | 同一隔离工作树里再跑一遍真库 6 条（`ERP_DB_URL=mig_replay_b110cost`）—— 6 passed / 0 failed |
| `03_full_test_raw.log` | **共享工作区**跑全量 test 的原始日志：红在并发卡（B2-1 工时台账）未提交的 `cost_labor_db_tests.rs`（`&str` vs `String`），**不是本卡**；本卡的全绿证据看 `04` |

真库怎么跑（`mig_replay_b110cost` = 空库全链重放 300/300 得到的干净库）：

```bash
bash scripts/verify/mig_replay_fresh.sh b110cost        # 300/300 PASS
ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/mig_replay_b110cost" \
  cargo test --lib cost_object_tests -- --test-threads=1
```

`02_real_db_tests.out` 里的关键几行：

```text
[B1-10] 挂 OPEN 对象建单 OK：借方 4001.01
[B1-10] CLOSED 对象被拦：66057 成本对象 CB-B110-000002 当前是「已关批」（CLOSED），不能投料 …
[B1-10] CANCELLED 对象被拦：66057 …
[B1-10] 报废单挂对象被拦：10001 只有领用单（out_type=ISSUE）可以挂成本对象 …
[B1-10] 挂对象 过账凭证：借 4001.01 / 贷 1405 100.0000
[B1-10] 不挂 过账凭证：借 6401 / 贷 1405 100.0000
[B1-10] 关批后过账被拦且回滚：66057 …（单仍 DRAFT、库存 50 未变）
```

## 5. 已知偏差 / 遗留

1. **无迁移**：`biz_stock_out.cost_object_id` 在 B1-5 的 `V0375`，末级科目 `4001.01` 在
   B1-6 的 `V0377` —— 本卡一张迁移都不需要（⇒ 卡面「迁移幂等 + down.sql」N/A）。
2. **前端未改**：出参新增 4 个字段（`cost_object_id / cost_object_no / debit_account_code /
   debit_account_name`），选择器与「借方科目提示会变」由 **B1-12** 做（JSON 键名已用单测钉住）。
3. **过账复检多一次查询**：挂了对象的单过账时多一条 `biz_cost_object` 窄查询（按主键），
   相对整个过账事务（多行锁 + 三本账 + 凭证）可忽略。
4. **遗留库** `mig_replay_b110cost`（本机，供复跑；不含生产数据），隔离工作树 `/root/wt-b110`
   与 `/root/wt-b110-target` 用完已 `git worktree remove`。
5. **共享工作区门禁**：`cargo test` 全仓会红在并发卡（B2-1 工时台账）的未提交文件；
   本卡全绿证据在隔离工作树 `@434aad42`（`04_isolated_gate.log`：31 目标 4494 passed / 0 failed，
   fmt / clippy 全绿，`05_isolated_real_db.out` 真库 6/6）。pre-push 同理用
   `SKIP_PREPUSH=1` 跳过工作区级 L1（原因同 B1-5）。
