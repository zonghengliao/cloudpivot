#!/usr/bin/env bash
# SO-35 迁移往返探针：在**一个空克隆库**上验 V0309 / V0310 的 up → up(幂等) → down → down(幂等) → up
#
#   runuser -u postgres -- createdb -O alun -T cloudpivot_so34 cloudpivot_so35p
#   bash scripts/verify/so35_migration_probe.sh | tee docs/evidence/20261107-so35-salesperson/migration_probe.txt
#
# 只用 psql 直连（不走 Spring/Rust 迁移器）：验的是 SQL 本身幂等 + 自检块能拦住坏数据。
# 「迁移器真的按 versioned/executed 记账」这一条由 so35_instance.sh 启动时实测（见 README）。
set -uo pipefail
DB=${SO35_PROBE_DB:-cloudpivot_so35p}
URL=${SO35_PROBE_URL:-postgresql://alun:alun@127.0.0.1:5432/$DB}
DIR=/opt/cloudpivot/backend/migrations/business
export PGPASSWORD=alun
PSQL=(psql "$URL" -v ON_ERROR_STOP=1 -q)

say() { printf '\n### %s\n' "$1"; }
run() {
  local file="$1"
  echo "-- psql -f $file"
  if "${PSQL[@]}" -f "$DIR/$file" 2>&1 | grep -E "NOTICE|ERROR|FATAL" | sed 's/^psql:[^ ]* //'; then :; fi
}
state() {
  "${PSQL[@]}" -Atc "
    SELECT 'tables=' || count(*) FROM pg_class
     WHERE relname IN ('biz_commission_rule','biz_commission_settlement','biz_commission_item')
       AND relkind = 'r';
    SELECT 'salesperson_col=' || count(*) FROM information_schema.columns
     WHERE table_name = 'biz_sales_order' AND column_name = 'salesperson_id';
    SELECT 'salesperson_fk=' || count(*) FROM pg_constraint
     WHERE conname = 'fk_biz_sales_order_salesperson';
    SELECT 'commission_menu_rows=' || count(*) FROM sys_menu
     WHERE auth_code LIKE 'business:erp:commission-%';
    SELECT 'commission_grants=' || count(*) FROM sys_role_menu rm
      JOIN sys_menu m ON m.id = rm.menu_id
     WHERE m.auth_code LIKE 'business:erp:commission-%';"
}

say "库：$DB"
say "① up（第一轮，全新）"
run V0309__sales_commission.up.sql
run V0310__commission_permissions.up.sql
state

say "② up（第二轮，必须幂等：0 行新增 / 无 ERROR）"
run V0309__sales_commission.up.sql
run V0310__commission_permissions.up.sql
state

say "③ 结构 + 域值抽检"
"${PSQL[@]}" -Atc "
  SELECT 'rule_cols=' || count(*) FROM information_schema.columns WHERE table_name='biz_commission_rule';
  SELECT 'settle_cols=' || count(*) FROM information_schema.columns WHERE table_name='biz_commission_settlement';
  SELECT 'item_cols=' || count(*) FROM information_schema.columns WHERE table_name='biz_commission_item';
  SELECT 'settle_fks=' || count(*) FROM pg_constraint WHERE conrelid='biz_commission_settlement'::regclass AND contype='f';
  SELECT 'item_fks=' || count(*) FROM pg_constraint WHERE conrelid='biz_commission_item'::regclass AND contype='f';
  SELECT 'uk_period=' || indexname FROM pg_indexes
   WHERE tablename='biz_commission_settlement' AND indexname='uk_biz_cmset_period';
  SELECT 'roles=' || string_agg(role_code, ',' ORDER BY role_code) FROM sys_role
   WHERE role_code IN ('SALES_SUPERVISOR','SALES_CLERK') AND deleted_at IS NULL;"

say "④ down（两轮，必须都幂等）"
run V0310__commission_permissions.down.sql
run V0309__sales_commission.down.sql
state
run V0310__commission_permissions.down.sql
run V0309__sales_commission.down.sql

say "⑤ up（第三轮，从 down 状态恢复）"
run V0309__sales_commission.up.sql
run V0310__commission_permissions.up.sql
state
