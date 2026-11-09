# ARCH-9 DDL 可执行性验证（2026-11-10）

对应设计：`docs/architecture/ARCH-9-多组织与合并报表设计.md` §3.1（新增 7 张表）与 §3.2（改造 42 张表）。

## 为什么有这份证据

设计卡要求「DDL 完整」。DDL 写在 markdown 里是**未经执行的文本**——列名写错、索引名记错、
CHECK 写不出它声称的语义，都不会报错。所以这里把文档里的 DDL 原样拿去真库跑一遍。

## 怎么跑的（可复现）

```bash
# 01 新增表：临时 schema，整段 ROLLBACK，不落任何对象
psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 -f <( \
  echo "BEGIN;"; echo "CREATE SCHEMA arch9_check;"; echo "SET search_path TO arch9_check;"; \
  cat 01_new_tables_check.sql; echo "ROLLBACK;" )

# 02 改造表：在 cloudpivot_test 的 public schema 上跑，整段 ROLLBACK
psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 -f <( \
  echo "BEGIN;"; cat 02_alter_tables_check.sql; echo "ROLLBACK;" )
```

环境：PostgreSQL 18.6（127.0.0.1:5432），库 `cloudpivot_test`，用户 `alun`。

## 结果

| 文件 | RC | 结论 |
|---|---:|---|
| `01_new_tables.out` | 0 | 7 张新表 + 索引 + 种子全部建成；**12 条约束齿口断言全过**（非法 status / 管理节点带信用代码 / 底稿借贷不等 / 重复底稿 / 非法期间 / MANUAL 无理由 / 双边非零 / AUTO 无规则 / 范围无原因 / 双默认公司 各被拒；CANCELLED 允许、合法 AUTO 行可写） |
| `02_alter_tables.out` | 0 | 文档 §3.2 的 ALTER 语句在真实 schema 上全部执行成功（含 3 条 `biz_voucher` 唯一键/部分唯一索引重建、`biz_accounting_period` 2 条、`biz_seq`、`biz_opening_balance`/`biz_opening_inventory` 表约束、`sys_dept`/`sys_user`/`biz_flow_instance` 回填 + `SET NOT NULL`）；**8 条语义断言全过**：两家公司可同号凭证/同 period_code/同 seq_type，同公司重复被拒；`ck_biz_stock_in_ic`、`ck_biz_supplier_ic` 真的拦得住 |

关键索引形状（`pg_indexes.indexdef` 实测，见 02_alter_tables.out 末段）：

```
uk_biz_voucher_no              (org_id, voucher_word, period, voucher_no) WHERE deleted_at IS NULL
uk_biz_voucher_opening_period  (org_id, period) WHERE voucher_type='OPENING' AND deleted_at IS NULL
uk_biz_voucher_year_end_period (org_id, period) WHERE voucher_type='YEAR_END' AND deleted_at IS NULL
uk_biz_period_code             (org_id, period_code)
uk_biz_period_ym               (org_id, period_year, period_month)
uk_biz_seq_type                (org_id, seq_type) WHERE deleted_at IS NULL
```

## 已验证过程改掉的两个真错误

1. `period_code` 的正则原来写 `^[0-9]{4}-[0-9]{2}$` → `2026-13` 能过。改成 `^[0-9]{4}-(0[1-9]|1[0-2])$`（月份也拦）。
2. `biz_inventory_txn` 的索引原来写 `(org_id, txn_date DESC)` —— **该表没有 `txn_date` 列**（实测列清单：`id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no, qty, unit_cost, amount, before_*, after_*, period_code, operator_id, remark, created_at`），改为 `(org_id, created_at DESC)`。

这两处若不跑就发现不了。

## 未覆盖（诚实说明）

`cloudpivot_test` 里不存在的表，其 ALTER 只能在迁移首次执行时验证，已由设计 §7.1 要求「每个迁移末尾带 `DO $$` 自检块」兜底：

`biz_depreciation_batch` · `biz_fx_revaluation` · `biz_customer` · `biz_inventory` · `biz_inventory_location` · `biz_depreciation` · `biz_stock_out` · `biz_stock_transfer` · `biz_stock_take` · `biz_sales_order` · `biz_shipment` · `biz_sales_return` · `biz_purchase_*` · `biz_receivable` · `biz_receipt*` · `biz_payable` · `biz_payment` · `biz_invoice` · `biz_expense_*` · `biz_fixed_asset*` · `biz_budget` · `biz_contract` · `biz_commission_settlement`。

这些表的 ALTER 语句形状与已验过的 `biz_stock_in` 样板完全一致（三条语句 + 一条索引），风险集中在「表名/列名笔误」，实现时由迁移自检块 + `\d` 实测校对。
