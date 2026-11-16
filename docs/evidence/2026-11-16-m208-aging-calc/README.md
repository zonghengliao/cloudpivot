# M2-08 库龄：计算（按批次入库时间）—— 验收证据

卡：`t_397dc77c`（M2-08【M2·仓库】库龄：计算（按批次入库时间））
日期：2026-11-16 ｜ 分支：`main` ｜ 环境：99（开发机）→ PG 145:5432

## 1. 交付了什么

| 层 | 文件 | 内容 |
|:--|:--|:--|
| DB（唯一算法） | `backend/app/migrations/business/V0424__inventory_aging_calc.{up,down}.sql` | 库函数 `biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)`：**FIFO 批次库龄层的唯一实现**（业务日期 + 累计 + 夹 0）；+ 权限码 `business:erp:inventory-aging:list`（menu_type=3，挂「即时库存」页下、sort 40） |
| 后端四件套 | `backend/app/src/business/erp/inventory_aging/{mod,dto,model,handler}.rs` | 批次层分页接口 `GET /business/erp/inventory-aging/page`（物料/仓库/批号/库龄区间/基准日/是否含已吃满层）+ 主数据批量补名（一页 2 条 SQL） |
| 互证实现 | `.../inventory_aging/fifo.rs` | FIFO 的 Rust 纯逻辑参考实现（只用于和 SQL 逐层对拍，生产路径不跑） |
| 用例 | `.../inventory_aging/{aging_tests,aging_db_tests}.rs` + 各文件 `mod tests` | 36 条（25 纯逻辑 + 11 真库） |
| 验收脚本 | `scripts/verify/m208_inventory_aging_drill.sh` | V0424 迁移幂等 / 回滚 / 业务数据不动（18 断言） |
| 夹具 | `scripts/verify/m208_inventory_aging_fixture.sql` | 幂等夹具（3 层入库 + 1 笔补录出库） |
| 门禁补登记 | `scripts/lint/scope_gate.py` | `SKIP_MANIFEST` 补 11 条（本卡 2 条 + 前序卡漏登记 9 条，见 §6） |

**列口径**：`layer_id / 物料(编码/名称/规格/单位) / 仓库(编码/名称) / lot_no / doc_type / doc_no / line_no /
biz_type / inbound_date / age_days / qty_in / qty_consumed / qty_remaining / unit_cost / amount_remaining /
posted_at / as_of`。

## 2. 核心口径（为什么这样算）

- **层 = `biz_inventory_txn` 的每一笔 `IN`**。`biz_inventory_txn` 里没有「层」表，也不该有
  （另存层账要做出库时的层消耗记账，是另一个量级改造）⇒ 库龄层是**推导**出来的。
- **FIFO**：`层剩余 = GREATEST(0, LEAST(层入库量, 该层累计入库 − 该格出库总量))`
  ⇒ 最早入库的层先被吃满，结存落在较新的层上。
- **入库日期取业务日期，不取过账时间**：流水 `created_at` 是过账墙钟（`Utc::now()`），
  入库单的 `in_date` 才是「货哪天进仓」。COALESCE 顺序：
  `STOCK_IN.in_date → STOCK_OUT.out_date → STOCK_TRANSFER.transfer_date → STOCK_TAKE.take_date →
  OPENING 的期初月初 → created_at(CST)`（最后一档是明写的已知近似）。
- **基准日 `as_of` 之前的判据也用业务日期** ⇒ 期末库龄能还原「那天账上是什么样」。
  补录单据（业务日期早、过账晚）照样算进它所属的期间。
- **出库 > 入库（脏数据 / 允许负库存的仓）⇒ 所有层剩余 0，不编造负数层**；
  `Σ剩余 < biz_inventory.qty` 就是「这格账不干净」的信号。
- **算法只有一份**：FIFO 住在库函数里，M2-09（分段报表）/ M2-10（呆滞料）直接复用；
  Rust 取数层禁止出现窗口聚合（`aging_tests::fifo_formula_exists_only_in_the_database_function` 静态断言）。

## 3. 证据清单

| 文件 | 是什么 | 结果 |
|:--|:--|:--|
| `01_cargo_test.out` | `cargo test --lib business::erp::inventory_aging`（真库） | **36 passed / 0 failed**，无 SKIP |
| `02_migration_drill.out` | `bash scripts/verify/m208_inventory_aging_drill.sh cp_m208_aging_test` | **PASS=18 FAIL=0 / ALL PASS**（up→up→down→down→up 幂等；`biz_inventory_txn` 行数不变） |
| `03_function_probe.out` | 库函数直查（psql，事务内 ROLLBACK） | 三层剩余 `0 / 120 / 50`；10-31 只剩两层；10-18 两层都满；`Σ剩余 = 170` |
| `04_api_check.out` | 真 HTTP（隔离实例 `127.0.0.1:18086`，profile=test） | **PASS=37 FAIL=0 / ALL PASS** |
| `05_menu_and_perm_tests.out` | `cargo test --lib system::menu::`（真库） | **22 passed**（含 `permission_audit_tests` n14–n21 全绿） |
| `m208_api_check.py` | 上面 04 的断言脚本（可重跑） | 37 条断言逐条 PASS/FAIL |

## 4. 真接口证据摘要（04）

- 200 + `{list,total,page,page_size}`；`code=0`（响应是 JSON，不是 SPA fallback 的 HTML）
- FIFO：默认 2 行（A 层吃满不返回）→ **B 剩 120（库龄 32 天）+ C 剩 50（库龄 6 天）**
- `include_consumed=true` → 3 行，A 层剩 0 / 已消耗 100 / 库龄 76 天
- `as_of=2026-10-31`：C 层（业务日期 11-10）不出现；B 仍剩 120 —— 出库业务日期 10-20 已算进来
  （它的**过账时间**是 11-12，说明快照判据确实用的是业务日期）
- `as_of=2026-10-18`：A 100 + B 200（出库还没发生）
- 库龄区间：`min_age_days=10`→只剩 B；`max_age_days=10`→只剩 C（边界含当天）
- 批号：`lot_no=M208H-LOT-C`→只剩 C
- 分页：`page_size=1` 第 1 页 B、第 2 页 C，`total` 恒为 2
- 非法参数 5 组（非 UUID / 日期格式 / 区间反了 / 布尔不认识 / 负库龄界）→ **400 + 66006**，文案点名字段
- 无令牌 → **401**（权限闸真的挂着）

## 5. 门禁

| 门禁 | 结果 |
|:--|:--|
| `cargo fmt -p cloudpivot -- --check` | rc=0（无 Diff） |
| `cargo clippy --lib --all-targets -- -D warnings` | **0 warning**（2m58s） |
| `cargo clippy --lib -- -D warnings` | 0 warning |
| `cargo check --lib` | rc=0 |
| `python3 scripts/lint/scope_gate.py --root .` | **全绿**（跳过清单 169 条逐条一致；生产面 0 处违纪） |
| `python3 scripts/verify/perm_menu_gate.py`（连演练库） | `missing_in_menu = 0` → **PASS** |
| `bash scripts/verify/menu_invariants.sh cp_m208_aging_test` | **8 条全绿**，菜单总数 **517**（= 516 + 本卡 1 个按钮行） |
| 迁移加载器（nik loader，不是 psql 回放） | 实例启动实测应用 `V0405…V0424` 共 20 个迁移，`V0424__inventory_aging_calc.up.sql` 成功记录 |
| `cargo test --lib system::menu::` | 22 passed（n14–n21 + menu 14 条） |

## 6. 顺带修的既有红（非本卡引入）

`scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST` 逐条断言在 main 上本来就是红的（实测 11 条）：
本卡新增 `inventory_aging/{aging_tests,aging_db_tests}.rs` 2 条，另有前序卡漏登记 9 条
（`cashflow_chain_tests` / `fx_cycle_tests` / `inventory_transit/{transit_flow,transit_query,transit_reconcile}_tests`
/ `receivable/aging_partial_tests` / `report/fx_report_tests` / `shipment/shipment_receipt_tests` / `test_seed.rs`）。
只补登记「它们本来就是测试面文件」这个事实，**不动任何文件内容与判据**。补完闸② 全绿。

## 7. 复现步骤

```bash
# ① 克隆演练库（99 上跑，PG 在 145）
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m208_aging_test

# ② 迁移幂等 / 回滚
bash scripts/verify/m208_inventory_aging_drill.sh cp_m208_aging_test

# ③ 真库用例（36 条）
cd backend && ERP_DB_URL="postgres://cloudpivot:...@172.18.10.145:5432/cp_m208_aging_test" \
  cargo test --lib business::erp::inventory_aging -- --test-threads=1

# ④ 真接口（需要先起实例把 V0405…V0424 补齐，再灌夹具 + 自签超管 token）
psql -h 172.18.10.145 -U cloudpivot -d cp_m208_aging_test -v ON_ERROR_STOP=1 \
  -f scripts/verify/m208_inventory_aging_fixture.sql
python3 docs/evidence/2026-11-16-m208-aging-calc/m208_api_check.py

# ⑤ 清场
dropdb -h 172.18.10.145 -U cloudpivot cp_m208_aging_test
```

## 8. 已知缺口（明写，不在本卡解）

1. **没有层表**：库龄层是每次查询推导（窗口函数），大库（千万级流水）需要预聚合/物化 —— 
   目前实测在 181 表模板库上毫秒级；量级上来后由报表卡（M2-09）决定是否加物化视图。
2. **业务日期兜底档**：`MATERIAL_SERIAL` / `SERIAL_BATCH` 这类没有业务日期列的单据回落
   `created_at`（CST）。补法已写进模块头注释（加一条 `LEFT JOIN`），不需要改表。
3. **没有入库流水的库存不计入**：直接写 `biz_inventory` 而无流水的行在这里看不见
   （合规路径 —— 库存期初录入会写 `OPENING` 流水）。
4. **报表 / 呆滞料 / 前端页**分别是 M2-09 / M2-10 / M2-12，本卡不含。
