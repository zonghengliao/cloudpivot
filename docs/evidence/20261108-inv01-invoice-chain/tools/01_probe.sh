#!/usr/bin/env bash
# 只读体检：INV-01 隔离库的种子资源 / 期间 / 库存 / 发票状态机配置
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${INV01_DB:-cloudpivot_inv01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== customers"; q "select id||' | '||code||' | '||name||' | status='||status from biz_customer order by code"
echo "== materials"; q "select id||' | '||material_code||' | '||material_name||' | tax='||tax_rate||' | unit='||coalesce(unit_id::text,'-')||' | status='||status from biz_material order by material_code"
echo "== warehouses"; q "select id||' | '||warehouse_code||' | '||warehouse_name||' | neg='||allow_negative from biz_warehouse order by warehouse_code"
echo "== units"; q "select id||' | '||unit_code||' | '||unit_name from biz_unit order by unit_code limit 8"
echo "== periods"; q "select period_code||' | '||status from biz_accounting_period order by period_code"
echo "== inventory"; q "select count(*) from biz_inventory"
echo "== accounts 2221/5xxx/1122"; q "select account_code||' | '||account_name||' | '||account_type||' | leaf='||is_leaf||' | status='||status from biz_account where account_code in ('1122','2221','2221.01','2221.02','5001','5403','1002','2202') order by account_code"
echo "== invoice cols"; q "select column_name||':'||data_type from information_schema.columns where table_name='biz_invoice' order by ordinal_position"
echo "== invoice checks"; q "select conname||' :: '||pg_get_constraintdef(oid) from pg_constraint where conrelid='biz_invoice'::regclass and contype='c'"
echo "== invoice status log entity"; q "select distinct entity_type from biz_status_log"
echo "== attachment rules for INVOICE"; q "select id||' | '||doc_type||' | '||file_role||' | req_at='||coalesce(required_at,'-')||' | enabled='||enabled||' | cond='||coalesce(condition_sql,'-')||' | exts='||coalesce(allow_exts,'-') from biz_attachment_rule where doc_type in ('INVOICE') order by file_role"
echo "== flow defs"; q "select flow_code||' | '||biz_type||' | status='||status from biz_flow_def order by flow_code"
echo "== sys_user"; q "select username||' | must_change='||must_change_password||' | status='||status from sys_user"
echo "== report items tax"; q "select item_code||' | '||item_name||' | '||direction||' | acct='||coalesce(account_pattern,'-') from biz_report_item where item_code in ('TAX','TAX_SURCHARGE','CASH_TAX') or account_pattern like '%2221%' or account_pattern like '%5403%' order by item_code"
