#!/usr/bin/env bash
# 建 FIN-3 演练库：以 cloudpivot_fin2asset（已到 V0238）为模板，补 V0239–V0244。
set -uo pipefail
export PGPASSWORD=alun
DB=${1:-cloudpivot_fin3}
MIG=/opt/cloudpivot/backend/migrations/business

sudo -u postgres psql -d postgres -q -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS $DB" \
  -c "CREATE DATABASE $DB TEMPLATE cloudpivot_fin2asset OWNER alun" || exit 1

for f in V0239__flow_approver_role_grants \
         V0240__erp_budget_mapping_material_type \
         V0241__erp_contract_permission_prefix_unify \
         V0242__erp_expense_tables \
         V0243__erp_expense_approval_flow \
         V0244__erp_expense_voucher_rules; do
  if [ ! -f "$MIG/$f.up.sql" ]; then echo "缺迁移 $f"; exit 1; fi
  echo "── up $f"
  psql -h 127.0.0.1 -U alun -d "$DB" -q -v ON_ERROR_STOP=1 -f "$MIG/$f.up.sql" || { echo "🚨 $f 失败"; exit 1; }
done

echo
echo "== 结果 =="
psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' -c "
select 'tables', count(*) from information_schema.tables where table_schema='public' and table_name like 'biz_expense%'
union all select 'types', count(*) from biz_expense_type where deleted_at is null
union all select 'rules', count(*) from biz_voucher_rule where rule_code like 'EXPENSE%'
union all select 'perm', count(*) from sys_menu where auth_code like 'business:erp:expense%'
union all select 'flow_nodes', count(*) from biz_flow_node where version_id='20000000-0000-4000-8000-0000000d0250'
union all select 'flow_edges', count(*) from biz_flow_edge where version_id='20000000-0000-4000-8000-0000000d0250'
union all select 'config', count(*) from sys_config where config_key='erp.expense.pay_remind_days'"
