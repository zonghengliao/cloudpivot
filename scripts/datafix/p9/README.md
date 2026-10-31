# P9 ERP 账务数据修复脚本（2026-10-31）

一次性数据修复，**不是迁移**（不进 `backend/migrations/`，不参与启动自动应用）。
修完的留痕与归因见 [`docs/datafix-20261031.md`](../../../docs/datafix-20261031.md)。

## 文件

| 文件 | 修什么 | 对应不变式 |
|------|--------|-----------|
| `00_backup.sh` | 整库 + 13 张受影响表备份到 `/root/datafix-backup-<ts>/` | — |
| `10_b1_location_ledger.up/down.sql` | 补写库位账（历史单 P6 之前没写） | B1 |
| `20_d1_missing_voucher.up/down.sql` | 给「凭证功能上线前过账」的单补凭证 + 应付 | D1 / B2① |
| `30_e4_cancel_rollback.up/down.sql` | 给「作废没退库存」的单退库存 + 库位账 + 冲销流水 | E4 / B2②③ |
| `40_g2_transfer_stuck.up/down.sql` | 两张无审批实例的 `PENDING` 调拨作废 | G2 |
| `50_b2_fixture_value.up/down.sql` | 夹具带进来的无单据库存价值归零 | B2④ / C3 |
| `apply.sh` | 按 `10→20→30→40→50`（或 `--rollback` 倒序）逐个事务应用 | — |
| `verify_idempotent.sh` | 连跑两遍，第二遍必须零改动 | 单测 ⑫ |
| `verify_rollback.sh` | down 全量回滚 → 三账回到修复前 → 再 up 往返 | 原则 3 |

## 用法（在 144 上）

```bash
cd /opt/cloudpivot

# 0) 备份（必须先做）
bash scripts/datafix/p9/00_backup.sh

# 1) 看一眼现状（不写库）
bash scripts/datafix/p9/apply.sh --check

# 2) 应用到 prod
bash scripts/datafix/p9/apply.sh

# 3) 验收：不变量必须 0 BLOCKER
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
python3 scripts/verify/erp_invariants.py

# 4) 演练库专用：幂等 + 回滚往返
P9_DB=cloudpivot_p9r bash scripts/datafix/p9/verify_idempotent.sh
P9_DB=cloudpivot_p9r bash scripts/datafix/p9/verify_rollback.sh
```

## 约定

- **库名优先读 `P9_DB`**：演练库/隔离库用它覆盖，prod 不传即可（默认取 `NIK_DATABASE_NAME`）。
- **每个脚本必须幂等**：不幂等的写法（无条件 `UPDATE`、不判重的 `INSERT`）在生产上等于「多跑一次多扣一次库存」。
- **不幂等的反例要报错不要沉默**：库存不够、库位账比成本账多、找不到过账流水 → 一律
  `RAISE EXCEPTION` / `NOTICE + 跳过`，交给人判断。
- **号段（`biz_seq`）只前进不回退**：`down` 不回拨号段，宁可留空号也不撞号。
- **plpgsql 变量别跟列名同名**：`period_code` / `credit_days` 这类名字会和目标表列名
  ambiguous（本次实测两次踩到），统一加前缀（`acct_period` / `sup_days` / `period_cd`）。
