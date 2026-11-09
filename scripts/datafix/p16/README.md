# P16 · 全库既有差异清账（E2E-FIX-7）

> 一次性/可重放的数据修复包，不是业务代码。代码侧口径修正见
> `scripts/verify/e2e03_reconcile.sql`（⑤ 聚合口径）与 `scripts/verify/erp_invariants.py` C5。

## 要修的是什么

`e2e03_reconcile.sql` 在 145 `cloudpivot_dev` 上带噪：库存 vs 流水 11 行、逐桶 15 行、一物一码 1 条。
逐条追下来，**没有一条是业务数据错了**：

| 形态 | 例子 | 处置 |
|---|---|---|
| 四张账只写了一半（有成本账没流水 / 只有库位账 / 只跑流水不重算账） | `W2B-M-SN` 流水 2031 vs 账面 2；`W3B-M-PLAIN` 账面 110 无流水；`P2C1-M1` 库位账 633 无成本账 | **A 类：整体删**（夹具残留） |
| 成本账一条「未分配」行 vs 库位账两条具体库位行（B1 绿、B1b 红） | `W1B-M-001` 5 vs 3+2 | A 类：删（夹具）；真实数据走 `p15` 并桶 |
| 主数据已被硬删、库位账行留下 | 5 条 `qty=3` 的孤儿行（11-09 12:00–12:07） | A 类：按 `NOT EXISTS` 主数据形态删 |
| 单据已硬删、流水与账面还在 | `P1F2-M1`（+70）、`SO14-M1`（+20） | A 类：删（补流水等于凭空造 70 件入库） |
| **流水被误删、账面仍保留结果** | `DEMO-INV01/WH-DVA` 差 +2 件 / +20 元（两张盘点单 `ST-20261107-000001/2` 的流水没了） | **C 类：按差额补一条盘盈流水，账面不动** |

## 四个脚本

| 文件 | 作用 |
|---|---|
| `05_target.up.sql` | 目标清单（`datafix_p16_target`，每行带 reason/归属）+ 统计 + `datafix_p16_doomed_rows` 视图（快照源） |
| `10_report.up.sql` | **只读盘点**：① 库存 vs 流水（含金额）、② 逐桶、③ 一物一码（聚合口径）、④ 流水自洽、⑤ 孤儿行、⑥ 过账无流水/负库存 |
| `20_purge_fixture_residue.up.sql` | 清理 A 类（事务内，G1/G2/G3 三道闸：期初库存引用 / 已过账单据明细 / 调拨序列号引用，任一命中即整单回滚）+ 自检归零 |
| `30_restore_missing_txn.up.sql` | C 类补流水（固定 id 幂等；补前断言「账面 − 流水 = 恰好 2 件 / 20.00」，否则回滚） |
| `40_verify.up.sql` | 验收门（任一项非 0 → 抛异常、非 0 退出） |

## 怎么跑

```bash
# 145 上（应用账号 cloudpivot）
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
P16_DB=$NIK_DATABASE_NAME bash scripts/datafix/p16/apply.sh --check
P16_DB=$NIK_DATABASE_NAME bash scripts/datafix/p16/apply.sh --backup
P16_DB=$NIK_DATABASE_NAME bash scripts/datafix/p16/apply.sh --apply
P16_DB=$NIK_DATABASE_NAME bash scripts/datafix/p16/apply.sh --verify
# 收尾（可选，会删掉处理台账）
P16_DB=$NIK_DATABASE_NAME bash scripts/datafix/p16/apply.sh --drop-helper
```

## 安全阀

- **不改账面数字**：`biz_inventory.qty / amount / avg_cost` 一个字节都不 UPDATE；差额只通过 INSERT 流水补。
- **三道门禁**：目标组的库位账行被 `biz_opening_inventory.location_row_id` 引用、目标物料有
  已过账/已完成单据明细、目标序列号被 `biz_stock_transfer_item_serial` 引用 —— 任一成立就整单回滚。
- **删除顺序**（被硬外键钉死）：序列号/调拨明细解引用 → 序列号 → 流水 → 成本账 → 库位账。
- **幂等**：目标集合固定、已清过为 0 行；补流水固定 id，重跑 `跳过（幂等）`。
- **台账**：`datafix_p16_target.done_at` + 每表行数快照；备份含**逐行 jsonb 快照**（`doomed_rows.tsv`）。

## 实跑证据

`docs/evidence/20261109-e2efix7-reconcile-cleanup/`：
前 11 行 / 15 行 / 1 条 → 后 0 / 0 / 0；`erp_invariants.py` 60→67 通过（库存域 B1/B1b/B4/B6/C5/E4 转绿）；
巡检 API `discrepancies=[]`；幂等重跑全 0 行。
