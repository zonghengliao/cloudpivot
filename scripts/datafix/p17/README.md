# P17 · 孤儿库位账 / 无主批次凭证清账（t_45bda7ad）

> 打谁：145 `cloudpivot_dev`（**144 生产禁止**）
> 为什么：`scripts/verify/e2e08_master_common.sh::cleanup_own()` 原来删物料/仓库时不删库位账，
> 也不删 `batch-generate` 写出的 `SERIAL_BATCH` 凭证 ⇒ 每跑一次 E2E-08 就留 1 行 qty=3 的孤儿库位账
> + 1 张 30 元的无主凭证 + 1 条 VOUCHER 状态日志。清场根因已在同一提交修好，本目录只清**存量**。

## 判据（形态判据，不写死物料编码 / 凭证号）

| 类别 | 判据 |
|---|---|
| 孤儿库位账 | `biz_inventory_location` 的 `material_id` 或 `warehouse_id` 在 `biz_material` / `biz_warehouse` 里不存在 |
| 无主批次凭证 | `biz_voucher.source_type='SERIAL_BATCH'` 且 `txn.doc_id = voucher.source_id` 的流水已不存在 |

## 门禁（任一命中整单回滚）

- **G1** 目标凭证不得有红冲/反冲关系（`is_red<>0` / `reversed_by` / `reverse_of`）；
- **G2** 目标凭证不得被任何业务表的 `*_voucher_id` 引用（动态扫 `information_schema`，白名单只有 `biz_voucher_entry`）；
- **G3** 目标凭证分录科目只能 1405/1901（batch-generate 固定口径）；目标库位账不得被
  `biz_inventory` / `biz_material_serial` / `biz_stock_transfer_item` 引用。

## 用法（在 145 上、仓库根目录）

```bash
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
P17_DB=$NIK_DATABASE_NAME bash scripts/datafix/p17/apply.sh --check    # 只读盘点
P17_DB=$NIK_DATABASE_NAME bash scripts/datafix/p17/apply.sh --backup   # 4 张表 dump + 逐行 jsonb 快照
P17_DB=$NIK_DATABASE_NAME bash scripts/datafix/p17/apply.sh --apply    # 清理 + 验收
P17_DB=$NIK_DATABASE_NAME bash scripts/datafix/p17/apply.sh --verify   # 只跑验收门
```

删的每一行都写进 `datafix_p17_doomed`（`tbl / row jsonb / doomed_at`），可逐行回滚或复查。
幂等：复跑 `--apply` 所有删除 0 行、验收仍全绿。

## 实测（t_45bda7ad，2026-11-09）

删除 `biz_inventory_location` 2 行 / `biz_voucher` 7 张 / `biz_voucher_entry` 14 行 /
`biz_status_log` 7 行；1405 余额 530628 → 530448（−180 = 6 张 × 30.00，与明细对得上）。
有行情的 `PZ-20261109-000501`（有批次流水）不动。证据：`docs/evidence/20261109-t45bda7ad-e2e08-fixture-cleanup/`。
