# P10 ERP 残留收口 · 数据修复脚本（2026-10-31）

一次性数据修复，**不是迁移**（不进 `backend/migrations/`，不参与启动自动应用）。
留痕与证据链见 [`docs/datafix-20261031-p10.md`](../../../docs/datafix-20261031-p10.md)。

## 文件

| 文件 | 做什么 | 对应验收项 |
|------|--------|-----------|
| `00_backup.sh` | 整库 + 7 张受影响表备份到 `/root/datafix-backup-p10-<ts>/`，另存 M-W1A-004 的逐列快照 | 原则 1（先备份） |
| `10_m_w1a_004_fixture_residue.up.sql` | 清理 M-W1A-004 的无单据夹具残留：序列号软删、流水/库位账/成本账物理删（带两道安全阀） | C3 WARN → 0；W2c 口径 |
| `10_m_w1a_004_fixture_residue.down.sql` | 按快照原值把 7 行写回（幂等） | 原则 3（可回滚） |
| `apply.sh` | 应用（`--rollback` 倒序回滚 / `--check` 只看现状） | — |
| `verify_idempotent.sh` | 连跑两遍，第二遍必须零改动 | 原则 4（幂等） |
| `verify_rollback.sh` | down 全量回滚 → 回到修复前快照 → 再 up 往返 | 原则 3 |

## 用法（在 144 上）

```bash
cd /opt/cloudpivot

# 0) 备份（必须先做；apply.sh 会检查最近一份备份）
bash scripts/datafix/p10/00_backup.sh

# 1) 看现状（不写库）
bash scripts/datafix/p10/apply.sh --check

# 2) 演练库先跑（prod 的副本）
P10_DB=cloudpivot_p10r bash scripts/datafix/p10/apply.sh
P10_DB=cloudpivot_p10r bash scripts/datafix/p10/verify_idempotent.sh
P10_DB=cloudpivot_p10r bash scripts/datafix/p10/verify_rollback.sh
NIK_DATABASE_NAME=cloudpivot_p10r python3 scripts/verify/erp_invariants.py

# 3) 上 prod
bash scripts/datafix/p10/apply.sh
python3 scripts/verify/erp_invariants.py     # 必须 BLOCKER 0 / WARN 0
```

## 约定

- **库名优先读 `P10_DB`**（演练库/隔离库用它覆盖），prod 不传即可（默认 `NIK_DATABASE_NAME`）。
- **安全阀优先于清理**：该物料一旦出现收货/采购明细，或库存金额 ≠ 0，脚本
  `RAISE EXCEPTION` 中止 —— 那说明货有业务来源，应补做收货单，而不是删数据。
- **号段（`biz_seq`）只前进不回退**：down 不回拨 `SN` 号段。
- **软删优先于物理删**：序列号只置 `deleted_at`（`biz_stock_transfer_item_serial.serial_id`
  是硬外键，且审计要看「谁的号、哪天作废」）。
