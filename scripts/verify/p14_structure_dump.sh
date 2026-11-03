#!/usr/bin/env bash
# P1-4 证据：表结构 / 约束 / 索引 / 权限节点（psql 侧只读导出）。
#   bash scripts/verify/p14_structure_dump.sh
# 输出到 docs/evidence/20261103-p14-doc-attachment/raw/01-seed-and-structure.txt
set -uo pipefail
ROOT=${REPO:-/opt/cloudpivot}
DB=${P14_DB_NAME:-cp_p14_e2e}
OUT="$ROOT/docs/evidence/20261103-p14-doc-attachment/raw/01-seed-and-structure.txt"
URL="postgres://alun:alun@127.0.0.1:5432/$DB"

mkdir -p "$(dirname "$OUT")"
{
  echo "== 库：$DB（隔离实例 18098）=="
  echo "== 导出时间：$(date -Is) =="
  echo
  echo "---- ① 表结构 biz_doc_attachment ----"
  psql "$URL" -c "\d biz_doc_attachment"
  echo
  echo "---- ② 列清单（含注释）----"
  psql "$URL" -Atc "select a.attname || ' | ' || format_type(a.atttypid, a.atttypmod)
      || ' | notnull=' || a.attnotnull
      || ' | ' || coalesce(col_description(a.attrelid, a.attnum), '')
    from pg_attribute a
   where a.attrelid = 'biz_doc_attachment'::regclass and a.attnum > 0 and not a.attisdropped
   order by a.attnum"
  echo
  echo "---- ③ 约束（含 CHECK 定义）----"
  psql "$URL" -Atc "select conname || ' :: ' || pg_get_constraintdef(oid)
      from pg_constraint where conrelid = 'biz_doc_attachment'::regclass order by conname"
  echo
  echo "---- ④ 索引 ----"
  psql "$URL" -Atc "select indexdef from pg_indexes
     where schemaname='public' and tablename='biz_doc_attachment' order by indexname"
  echo
  echo "---- ⑤ 权限节点（3 个）与授权行数 ----"
  psql "$URL" -Atc "select m.id || ' | menu_type=' || m.menu_type || ' | ' || m.auth_code
      || ' | visible=' || m.is_visible
      || ' | 授权角色数=' || (select count(*) from sys_role_menu rm where rm.menu_id = m.id)
    from sys_menu m
   where m.auth_code like 'business:attachment:%'
   order by m.auth_code"
  echo
  echo "---- ⑥ 迁移记录 ----"
  psql "$URL" -Atc "select version, name, to_char(applied_at, 'YYYY-MM-DD HH24:MI:SS')
      from schema_migrations where version = '212'"
  echo
  echo "---- ⑦ 五类单据 \`doc_type\` 与真实表的对应（存在性校验口径）----"
  psql "$URL" -Atc "select 'CONTRACT|biz_contract|' || count(*) from biz_contract
    union all select 'PURCHASE_ORDER|biz_purchase_order|' || count(*) from biz_purchase_order
    union all select 'STOCK_IN|biz_stock_in|' || count(*) from biz_stock_in
    union all select 'PAYABLE|biz_payable|' || count(*) from biz_payable
    union all select 'PAYMENT|biz_payment|' || count(*) from biz_payment"
} > "$OUT" 2>&1
echo "已写 $OUT ($(wc -l < "$OUT") 行)"
