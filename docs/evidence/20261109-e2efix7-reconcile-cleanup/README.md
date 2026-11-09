# E2E-FIX-7 · 全库既有数据差异清账（145 `cloudpivot_dev`）

> 卡：`t_1246fc6f`（P3 存量）。终态：**库存 vs 流水 0 行 / 逐桶对账 0 行 / 一物一码 0 行**。
> 工具：`scripts/datafix/p16/`（只读报告 → 备份 → 清理/补流水 → 验收门）。
> 口径：`docs/ERP-库存对账口径.md`、`scripts/verify/e2e03_reconcile.sql`、`erp_invariants.py`。

## 0. 一句话结论

11 行「库存 vs 流水」、15 行「逐桶」、1 条「一物一码」全部是**别人卡在 145 dev 库里留下的
夹具/演练残留**（不是业务数据、不是本卡产生）。其中 13 组 (物料, 仓库) 属于「四张账只写了一半」
的夹具 → 整体删除；1 组（`DEMO-INV01/WH-DVA`）是**流水被误删、账面仍保留 +2** → 按差额补一条
盘盈流水（不改账面）；另外修正了 `e2e03_reconcile.sql` ⑤ 的**逐桶假红**（应按 `(物料,仓库)` 聚合）。

## 1. 盘点（动手前，实测）

清账前基线（`cp_fix7_before` 快照库 / `raw-145/before_*.txt`）：

```
① 库存 vs 流水：11 行不一致，数量差 -1807，金额差 -17660
② 逐桶对账：   15 行不一致（10 行真夹具 + 5 行孤儿库位账，后者卡面快照时还不存在）
⑤ 一物一码：   HEAD 版脚本 3 行（E2E03-MS×2 逐桶假红 + W2C2-M-SN 1 条真违例）
               按 (物料,仓库) 聚合 → 只有 1 条，与卡面「1 条（W2C2-M-SN）」一致
```

### ① 11 行逐条归属

| # | 物料 / 仓库 | 账面 | 流水 | 差 | 归属 | 类别 |
|---|---|---|---|---|---|---|
| 1 | `DEMO-INV01` / `WH-DVA` | 92 | 90 | +2 | 11-07 库存域演练（P0-03）的演示物料。当日 7 条流水里的 `STOCK_TAKE_GAIN +5`（`ST-20261107-000001`）与 `STOCK_TAKE_LOSS -3`（`-000002`）**连同两张盘点单被后续夹具清理删掉**，账面仍保留 +5−3=+2（证据：`docs/evidence/20261107-p003-inventory-chain/14_final_reconcile_output.txt` 当日 92=92，共 7 条流水） | **C 类**（补流水） |
| 2 | `P1F2-M1` / `P1F2-WH` | 40 | −30 | +70 | P1F2 出库夹具；单据 `OUT-20261109-000022` 已**硬删**，只剩 2 条 OUT 流水 | A 类 |
| 3 | `SO14-M1` / `SO14-WH` | 16 | −4 | +20 | SO-14 挂账夹具；单据 `OUT-20261109-000011` 已硬删，只剩 1 条 OUT 流水 | A 类 |
| 4 | `W2B-M-SN` / `WH-W2B-1` | 2 | 2031 | −2029 | W2B 序列号夹具：30 条 `SERIAL_*` 流水只跑流水不重算账 | A 类 |
| 5 | `W2C1-M-PLAIN` / `WH-W2C1` | 7 | 0 | +7 | 序列号夹具「按数量物料 7 件」：只写成本账，无流水无库位账（E2E-FIX-6 已给**夹具**补库位账行，旧库不会自愈） | A 类 |
| 6 | `W2C2-M-PLAIN` / `W2C2-WH-A` | 7 | 0 | +7 | 同上形态 | A 类 |
| 7 | `W2C2-M-RT` / `W2C2-WH-B` | 1 | 0 | +1 | 同上形态（+1 件在库序列号） | A 类 |
| 8 | `W2C2-M-SN` / `W2C2-WH-A` | 3 | 0 | +3 | 同上形态；且在库序列号只有 2 → ⑤ 唯一真违例 | A 类 |
| 9 | `W3B-M-PLAIN` / `WH-W3B-1` | 100 | 0 | +100 | W3B 调拨/结算夹具（P0-03 的 `15_purge` 清过同款，后续再跑夹具又长回来） | A 类 |
| 10 | `W3B-M-PLAIN` / `WH-W3B-2` | 10 | 0 | +10 | 同上（调入仓） | A 类 |
| 11 | `W3B-M-SN` / `WH-W3B-1` | 2 | 0 | +2 | 同上形态（+2 件在库序列号） | A 类 |

### ② 逐桶差异逐条归属

| 物料 / 仓库 | 桶 | 成本账 | 库位账 | 差 | 归属 |
|---|---|---|---|---|---|
| `P2C1-M1` / `P2C1-WH` | (未分配) | 0 | 633 | −633 | P2C1 收货夹具：只写库位账，成本账/流水都没有 → A |
| `P2C1-M2` / `P2C1-WH` | (未分配) | 0 | 13 | −13 | 同上 → A |
| `W1B-M-001` / `WH-W1B-LEAF` | (未分配) | 5 | 0 | +5 | W1B 分桶夹具：成本账一条「未分配」行 vs 库位账 A-01 3 + A-02 2。**B1（按物料×仓库合计）相等所以长期是绿的，只有 B1b 逐桶红** → A |
| `W1B-M-001` / `WH-W1B-LEAF` | A-01 / A-02 | 0 | 3 / 2 | −3 / −2 | 同上 |
| `W2C1-M-PLAIN` / `W2C2-M-PLAIN` / `W2C2-M-RT` | (未分配) | 7 / 7 / 1 | 0 | +7 / +7 / +1 | 同 ①（只写成本账） → A |
| `W2C2-M-SN` / `W2C2-WH-A` | (未分配) vs `W2C2-A-01` | 3 / 0 | 0 / 3 | +3 / −3 | 成本账落未分配桶、库位账落具体库位 → A |
| （无主数据）×5 | (未分配) | 0 | 3×5 | −3×5 | **孤儿库位账**：物料/仓库在夹具清理时被硬删、库位账行留下。5 条分别在 **11-09 12:00 / 12:02 / 12:05 / 12:05 / 12:07** 生成（每次跑一次夹具漏一行），qty 都是 3 → A（形态判据，不写物料编码） |

### ⑤ 一物一码

- `W2C2-M-SN`：账面 3 / 在库序列号 2 → **真违例**（A 类删除后归零）。
- `E2E03-MS`：账面 5 = 在库序列号 5，但 HEAD 版 `e2e03_reconcile.sql` ⑤ **逐条成本账行**比
  `count(IN_STOCK)`，该物料同仓有两个桶（`E2E03-L01A` 3 + 未分配 2）→ 各报一条 = 2 条假红。
  序列号表只有 `(物料, 仓库)`，正确口径就是聚合（与 `erp_invariants.py` C5、
  `inventory_audit.rs` 的 `AUDIT_CTE` 一致）→ **B 类（修口径）**，详见 §4。

## 2. 处置与影响行数（每一步都有记录）

```bash
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
P16_DB=$NIK_DATABASE_NAME bash scripts/datafix/p16/apply.sh --check    # 只读盘点
P16_DB=$NIK_DATABASE_NAME bash scripts/datafix/p16/apply.sh --backup   # 备份 + 逐行快照
P16_DB=$NIK_DATABASE_NAME bash scripts/datafix/p16/apply.sh --apply    # 清理 + 补流水 + 验收
```

| 步骤 | 动作 | 影响行数 |
|---|---|---|
| 备份 | `pg_dump --data-only` 12 张表 → `evidence/p16-20261109-151525/p16_tables.sql`（159,601 B） | — |
| 备份 | 将被删的每一行导成 jsonb → `doomed_rows.tsv` | **72 行**（成本账 12 / 库位账 13 / 孤儿库位账 6 / 流水 34 / 序列号 7） |
| G1/G2/G3 门禁 | 期初库存引用？已过账单据明细？调拨序列号引用？ | 0 / 0 / 0（通过） |
| 解引用 | `biz_material_serial.location_id` 指向待删库位账行 | **3 行** |
| 解引用 | `biz_stock_transfer_item.from/to_location_id` | 0 / 0 |
| 删除 | `biz_material_serial` | **7 行** |
| 删除 | `biz_inventory_txn` | **34 行** |
| 删除 | `biz_inventory`（成本账，必须先于库位账） | **12 行** |
| 删除 | `biz_inventory_location`（目标组） | **13 行** |
| 删除 | `biz_inventory_location`（孤儿行） | **6 行** |
| 补流水 | `DEMO-INV01/WH-DVA` 盘盈 +2 件 @10.00 = 20.00（`id=f17c0001-…-0001`、`doc_no=ADJ-20261109-FIX7-01`、before 90/1000 → after 92/1020） | **1 行 INSERT**，账面 0 改动 |
| 自检 | 13 组目标全部「成本账/库位账/流水/序列号 = 0」 | 通过 |

**没有对 `biz_inventory.qty/amount` 做任何 UPDATE**；`datafix_p16_target` 留处理台账（`raw-145/04_target_ledger.txt`）。
幂等重跑实测：所有删除语句 **0 行**、补流水 `跳过（幂等）`、验收仍全绿（`raw-145/after_apply_idempotent_rerun.txt`）。

## 3. 清账后基线（实测原始输出见 `raw-145/`）

| 验收项 | 前 | 后 | 证据 |
|---|---|---|---|
| ① 库存 vs 流水 | 11 行 / −1807 件 / −17660 元 | **0 行 / 0 / 0** | `before_e2e03_reconcile_FIXED.txt` / `after_e2e03_reconcile.txt` |
| ② 逐桶对账 | 15 行 | **0 行** | 同上（`②-2`） |
| ⑤ 一物一码（聚合口径） | 1 条（`W2C2-M-SN`） | **0 行** | 同上 |
| `erp_invariants.py` | 60/80 通过，BLOCKER 12 / WARN 8 | **67/80 通过，BLOCKER 8 / WARN 5** | `before_erp_invariants.txt` / `after_erp_invariants.txt` |
| 库存域 6 条不变量 | B1 / B1b / B4 / B6 / C5 / E4 全红 | **全部转绿** | `raw-145/05_invariants_delta.txt` |
| 巡检 API `GET /business/erp/inventory/audit?dry_run=true` | — | HTTP 200 / `code=0` / `checked=1` / **`discrepancies=[]`** | `raw-145/after_audit_api.json` |

`erp_invariants.py` 剩下的 8 条 BLOCKER **不在本卡口径内**，且清账前就是红的（不是本次引入）：
`A2 凭证头金额=分录取和`、`B2 库存金额=1405 余额`、`B5 出库单必结转凭证`、`E4b biz_type 未登记`、
`H6 预置流程种子 is_system`、`S1`、`S5`、`S6`（销售/审批/凭证域的演练数据问题）。

## 4. 代码侧改动（只改口径，不改业务语义）

`scripts/verify/e2e03_reconcile.sql` ⑤：逐条成本账行比 `count(IN_STOCK)` → **按 `(物料, 仓库)`
聚合后比**。理由：

- `biz_material_serial` 只有 `(material, warehouse, status)`，**没有库位维度**；
- `erp_invariants.py` C5 与 `inventory_audit.rs::AUDIT_CTE` 早已是聚合口径（两处独立实现）；
- 逐桶比会在「同物料同仓多桶」时必然假红（E2E-FIX-6 之前批量生成落未分配桶造成多桶），
  实测把 `E2E03-MS`（5=5，完全正确）误报成 2 条违例。
  证据：`raw-145/before_e2e03_reconcile_HEAD.txt`（3 行）vs `before_e2e03_reconcile_FIXED.txt`（1 行）。

## 5. 需要下游注意的两件事

1. **夹具清理有漏**：11-09 12:00–12:07 连续 5 次出现「物料/仓库被硬删、库位账行留下」（qty 均为 3）。
   本卡按**形态判据**（`NOT EXISTS` 主数据）清理，不写死物料编码；但只要那条夹具清理逻辑不改，
   每跑一次就会重新长出一行。建议夹具清理统一走 `datafix_p16` 的形态判据或补一句
   `delete from biz_inventory_location where material_id = …`。
2. **`DEMO-INV01` 的补录流水 `ADJ-20261109-FIX7-01` 没有对应业务单据**（故意如此：原始两张盘点单
   已不存在，硬造单据会连带要求凭证/明细）。因此 `10_report` 的「流水单据已不存在」计数由 5 → 6，
   属于留痕而非脏数据。

## 6. 复跑

```bash
# 只读盘点（任何库都能跑，改了不改数据）
P16_DB=<库名> bash scripts/datafix/p16/apply.sh --check
# 验收门（有差异就非 0 退出）
P16_DB=<库名> bash scripts/datafix/p16/apply.sh --verify
```

> 注意：`05_target.up.sql` 的目标清单是**针对 145 dev 库这批夹具**写死的（每条都带 reason 与归属）；
> 别的库/新形态请先跑 `--check` 拿到差异清单再决定是否加目标，不要直接 `--apply`。
