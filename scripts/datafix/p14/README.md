# P14 数据修复脚本（清理 prod 零成本测试数据）

> 一次性的生产数据清理，不是可复用的业务脚本。执行留痕见
> `docs/datafix-20261101-p14.md`（含修前/修后快照与全部原始输出）。

## 这是什么

清理 2026-10-31 16:17:24 由 `POST /business/erp/serial/batch-generate`（旧代码不校验 `unit_cost`）
造出来的一批零成本数据（老板确认是测试用）：

```
物料 M-W1A-004 / 仓库 WH01 / 10 件 / 金额 0 / 均价 0
序列号 SN-20261031-000011 ~ 000020（source_type='PRODUCE'、source_id IS NULL、create_by=henry.liao）
流水 SN-20261031-000011（IN / SERIAL_IN / 10 件 / unit_cost=0）
库位账 0 行（批量生成只写成本账 ⇒ 不变式 B1 红）
凭证 0 张（金额 0 ⇒ 不需要红冲）
```

## 怎么跑（在 prod 主机 144，`/opt/cloudpivot`）

```bash
set -a; . /etc/cloudpivot/cloudpivot.env; set +a

bash scripts/datafix/p14/drill.sh           # ① 强烈建议先演练（建 pg_dump 副本库 → up×2 → down → 重放 → drop）
bash scripts/datafix/p14/00_backup.sh       # ② 备份 6 张表 + MANIFEST
bash scripts/datafix/p14/apply.sh           # ③ 应用（10 → 20 → 30 自检，任何 FAIL 退出码 1）
bash scripts/datafix/p14/apply.sh --check   # 只看现状，不写库
bash scripts/datafix/p14/apply.sh --rollback# ④ 回滚（20 down → 10 down）
```

环境变量：

| 变量 | 默认 | 用途 |
|------|------|------|
| `P14_DB` | `NIK_DATABASE_NAME`（= `cloudpivot`） | 目标库（演练库 / 隔离库用） |
| `FORCE=1` | 关 | 跳过「必须存在本库备份」的保护 |
| `CLOUDPIVOT_ENV_FILE` | `/etc/cloudpivot/cloudpivot.env` | 凭据来源 |
| `P14_SRC_DB` / `P14_DRILL_DB` | `cloudpivot` / `cloudpivot_p14drill` | 演练的源库与副本库 |

## 脚本

| 文件 | 作用 |
|------|------|
| `00_backup.sh` | 逐表 `pg_dump` + custom-format + `MANIFEST.txt`（表名/行数/md5/库名/迁移版本/目标快照/全库基线） |
| `10_void_serials.up.sql` / `.down.sql` | 软删 10 个序列号（整行快照进 `datafix_p14_snapshot`）/ 按快照写回 |
| `20_reverse_inventory.up.sql` / `.down.sql` | 成本账归零 + 写 `OUT / PRODUCE_CANCEL` 冲销流水（固定主键）/ 删流水 + 成本账写回 |
| `30_verify.sql` | 10 项自检（①~⑩），输出 `PASS/FAIL/INFO` |
| `apply.sh` | `apply` / `--rollback` / `--check`；每个 SQL 一个事务 |
| `drill.sh` | 副本库全流程演练：up 两遍（幂等）+ down（比对基线）+ 重放 + drop |

## 设计要点（踩过的坑）

1. **软删而不是物理删序列号**：`biz_stock_transfer_item_serial.serial_id` 是硬外键；
   `uk_biz_material_serial_no` 是 `WHERE deleted_at IS NULL` 的部分唯一索引 ⇒ 软删后同号可重发、审计不丢。
2. **必须写冲销流水**：只改 `qty` 会把 B1 换成 B4（流水累计 ≠ 库存）。
3. **`qty` 写出库正数 10**：`ck_biz_inv_txn_qty CHECK (qty > 0)`，负数违约；方向由 `txn_type='OUT'` 表达。
4. **`biz_type` 没有白名单**（只有 `txn_type` 有 CHECK）⇒ `PRODUCE_CANCEL` 可直接落库，不需要加迁移。
5. **`doc_id` 是 NOT NULL uuid**，卡面的 insert 列表里没有它；本脚本用固定 `doc_id` + 固定主键
   （幂等靠主键 `on conflict`、回滚靠主键精确删，不做 `doc_no LIKE` 模糊匹配）。
6. **严格前置校验**：命中总数 ≠ 10、存活数既不是 10 也不是 0、`qty ≠ 10`、`amount ≠ 0`、
   `avg_cost ≠ 0`、库位账 ≠ 0、被调拨明细引用 —— 任一成立就 `RAISE EXCEPTION` 整脚本回滚。
7. **不写凭证**：金额 0，原功能也没生成凭证（`source_no` 关联 0 张，实测）；有意外凭证应停下报告、不自行红冲。
8. **`operator_id` 留 NULL**：不伪造操作人，身份由 `doc_no=DATAFIX-20261101-P14` + `remark` 表达。

## 幂等 / 可逆

* 幂等：第 2 遍两步都打 `NOTICE: … 幂等跳过（本步 0 行受影响）`，`30_verify.sql` 仍全 PASS
  （prod 与演练库都实测过）。
* 可逆：`--rollback` 后与清理前基线**逐项相同**（演练库实测：序列号 10 存活 / qty 10 / 冲销流水 0 /
  全库合计与凭证数不变），回滚后还能再次 apply 且全绿。
* 快照表 `datafix_p14_snapshot` 保留给 down 用；确认不再回滚后可 `DROP TABLE datafix_p14_snapshot;`。
