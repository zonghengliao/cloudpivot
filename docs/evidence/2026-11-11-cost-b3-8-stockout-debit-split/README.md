# COST-B3-8 领料凭证借方分流 —— 复核（无代码改动）

- 卡号：`t_550d5d89`（COST-B3-8，来自 ARCH-9 §8.1 B3-8）
- 日期：2026-11-11
- 结论：**卡面要求的功能已由前置卡 B1-10（`434aad42`）落地并在过账路径实测通过**。本卡执行的是「验收复核 + 取证」，**未新增/修改任何业务代码**；只回填了设计 §4.7.1 的「实现口径」并归档证据。
- 出库借方取科目**全仓唯一入口**：`backend/src/business/erp/stockout_base.rs:191 debit_account_of_doc(out_type, cost_object_id)`；**唯一调用点**：`stockout_handler.rs:782`（⑤ 成本结转凭证）。`4001.01` 常量只在 `voucher.rs:211` 定义一次。

## 验收对照

| 卡面验收 | 实测 | 证据 |
|---|---|---|
| 挂了成本对象 → 借方 `4001.01` | `OUT-20261111-000036`（ISSUE + `cost_object_id` 非空）凭证 `PZ-20261111-000028` 借 `4001.01 生产成本-直接材料` 100.0000 / 贷 `1405 库存商品` 100.0000 | `03_db_voucher_entries.out`、`02_posting_nocapture.out` |
| 没挂 → 原逻辑（制造费用或期间费用） | `OUT-20261111-000037`（ISSUE，无对象）凭证 `PZ-20261111-000029` 借 `6401 主营业务成本` 100.0000 / 贷 `1405` 100.0000；报废/盘亏恒借 `1901 待处理财产损溢` | 同上 + `01_stockout_lib_tests.out` |
| 单测覆盖两种 | 纯逻辑 3 条（`issue_with_cost_object_debits_production_material` / `no_cost_object_keeps_legacy_debit_account` / `scrap_and_loss_never_debit_production_material`）+ 真库 1 条（`posting_splits_debit_by_cost_object`，两单对照读 `biz_voucher_entry`） | `04_source_refs.out` §4；`01`/`02` |

> 注：卡面写「没挂 → 制造费用或期间费用」，实际口径是 `6401 主营业务成本`（领用/其他）与 `1901 待处理财产损溢`（报废/盘亏）—— 这是**改造前就有的存量行为**（`debit_account_of`），设计 §4.7.1 表格口径亦为 `6401`。本卡沿用设计口径，未改存量科目。

## 实测命令与结果

```bash
# 1) 出库全量（纯逻辑 + 真库），串行，真库 = mig_replay_b110cost（已到 V0377 之后）
cd /opt/cloudpivot/backend
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/mig_replay_b110cost \
  cargo test --lib stockout -- --test-threads=1
# → test result: ok. 62 passed; 0 failed; 4680 filtered out; finished in 48.60s
#   日志里没有任何 "SKIP: 未设置 ERP_DB_URL" ⇒ 真库用例确实跑了，不是假绿

# 2) 两单对照逐字输出（--nocapture）
ERP_DB_URL=... cargo test --lib \
  stockout_model::cost_object_tests::posting_splits_debit_by_cost_object -- --nocapture --test-threads=1
# → [B1-10] 挂对象 过账凭证：借 4001.01 / 贷 1405 100.0000
# → [B1-10] 不挂   过账凭证：借 6401    / 贷 1405 100.0000
# → test result: ok. 1 passed; 0 failed

# 3) 落库凭证分录（独立于测试代码的 SQL 复核）
psql ... -c "SELECT so.out_no, ..., e.account_code, e.debit_amount, e.credit_amount
             FROM biz_stock_out so JOIN biz_voucher v ... JOIN biz_voucher_entry e ..."
# → 000036 借 4001.01 / 贷 1405；000037 借 6401 / 贷 1405（见 03_db_voucher_entries.out）

# 4) 门禁
cargo fmt --check  → FMT_EXIT=0（05_gates.out）
```

## 与卡面/设计的已知偏差

1. **分支位置**：卡面/设计写作「`debit_account_of` 前加分支」。实现是在 `debit_account_of` **外面**新增包装 `debit_account_of_doc`（`debit_account_of` 保持纯 `out_type` 映射、零语义变化）—— 因为 `debit_account_of` 是出库类型的公共映射，直接加参数会波及 `biz_type_of` 一类的共用调用。行为与设计表格逐字一致，`git show 434aad42 -- stockout_handler.rs` 中过账路径仅该一行改动（金额/拆行/尾差未动）。
2. **非 ISSUE 兜底**：报废/盘亏即使挂了对象也**不**返回 `4001.01`（写入侧 `ensure_cost_object_usable` 已拒；记账侧再兜一层）。设计 §4.3 只说料归集只扫 `ISSUE`，这里是记账侧更强的一层保护。
3. **迁移**：本卡无 schema 变更 ⇒ 卡面「迁移幂等 + `.down.sql`」N/A（`git status backend/migrations` 零差异）。
4. **前端**：无前端改动（领料单「成本对象」选择器属 B1-12）⇒ `pnpm typecheck/build` N/A。

## 复跑提示 / 泄漏

- 演练库 `mig_replay_b110cost` 保留（含 `4001.01` 末级科目与 `biz_stock_out.cost_object_id`），可直接 `ERP_DB_URL` 指向它复跑。
- 夹具生命周期：`stockout_cost_object_tests::setup()` 是**每轮先清后建**（`cleanup_fixtures()` 在 setup 里，不在收尾），所以本次跑完库里还留着 `d2b10c00-*` 夹具单 —— `OUT-20261111-000036/000037` 与两张凭证正是**跑完就能用 SQL 直接查**的原因（`03_db_voucher_entries.out` 即由此取得）；下一轮 `setup()` 会先清掉，不会累积。
- 文件清单：`01_stockout_lib_tests.out`、`02_posting_nocapture.out`、`03_db_voucher_entries.out`、`04_source_refs.out`、`05_gates.out`。
