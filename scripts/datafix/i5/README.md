# I-5 · 演练库清污（5 张孤儿应收 + 11/12 组「库存差异」）+ 干净验收基线

> 打谁：145 `cloudpivot_dev`（只软删 5 张孤儿应收）/ `cloudpivot_dev_clean`（冒烟夹具清场）
> **144 生产一行不动**。卡：`REVIEW-FIX-3【I-5】`（E2E-14 评审 ③ 数据一致性）
> 证据：`docs/evidence/2026-11-09-i5-clean-drill-db/`

## 目录内容

| 文件 | 干什么 | 幂等 |
|---|---|---|
| `00_start_instance.sh` | 用 `cloudpivot_dev_clean` 起隔离实例（:18059），并核对端口监听 pid 就是自己 | 是（重启自己的实例） |
| `01_orphan_ar_softdel.up.sql` | 把 5 张「出库单已不存在」的销售应收**软删**（`deleted_at`），保留审计痕迹 | 是（第二遍 ROW_COUNT=0；非 0/5 整单回滚） |
| `02_smoke_fixture_cleanup.sql` | 删掉 `I5-SMOKE` 前缀的冒烟夹具（子表→主表），清完回到 0 业务行 | 是 |
| `03_smoke.sh` | 起实例 → 登录 → 建主数据 → 序列号入库 3 件 → 出库 1 件 → 账实对账 → 巡检 → 清场 → 再巡检 | 是（清场后重跑） |

## 用法（在 145、仓库根目录）

```bash
# ① 孤儿应收软删（cloudpivot_dev）
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
PGPASSWORD=$NIK_DATABASE_PASSWORD psql -h 127.0.0.1 -U $NIK_DATABASE_USER -d cloudpivot_dev \
  -v ON_ERROR_STOP=1 -f scripts/datafix/i5/01_orphan_ar_softdel.up.sql

# ② 干净库冒烟（cloudpivot_dev_clean，:18059）
bash scripts/datafix/i5/03_smoke.sh

# ③ 验收闸（巡检全绿才退出 0）
bash scripts/ops/acceptance_gate.sh
```

## 判据说明（为什么不是卡面那两条 SQL 直接跑）

1. **孤儿应收**：卡面 SQL 漏了 `source_type='SALES_OUT'` 过滤。`MANUAL` 应收的 `source_id`
   是挂账时服务端生成的占位 UUID（`source_no='MANUAL'`），永远对不上 `biz_stock_out`，
   于是「5 张孤儿」会被跑出 **21 行**。正确判据 = `source_type='SALES_OUT'` + 出库单不存在。
2. **库存 vs 流水**：卡面 SQL 是 `biz_inventory LEFT JOIN biz_inventory_txn` 再 `SUM(i.qty)`。
   `biz_inventory` 是 **per-bucket 桶表**（同一物料×仓库可能多行：库位/未分配），流水会被
   每个桶行重复计入 ⇒ 差异量正好是 `库存合计 − 流水×桶数`，全是**假差异**。
   正确口径：两侧先聚合（`GROUP BY material_id, warehouse_id`）再 `FULL JOIN`。
   修口径后 `cloudpivot_dev` 现测 **0 组真差异**（逐桶成本账 vs 库位账也是 0）。

## 清场时踩到的坑

- 序列号入库/出库会各写一张凭证：`SERIAL_BATCH`（source_id = 流水 `doc_id`）和
  `SERIAL_OUT`（source_id = **序列号 id**）。只按 `SERIAL_BATCH` 清会漏掉 `SERIAL_OUT`，
  残留的 1405 贷方会让 `B2 库存金额 = 存货科目余额` 变红 —— 这正是清场脚本里
  「无序列号业务时兜底清无主 SERIAL_* 凭证」那段存在的原因。
- 冒烟会改 admin 口令（首登 `must_change_password=true`）：跑完记得用
  `PUT /api/system/auth/password` 改回服务 env 的口令，否则运维拿 env 口令登不进去。
