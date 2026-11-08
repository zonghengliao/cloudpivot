#!/usr/bin/env bash
# CLS-01 环境探针 v2：真实列名
set -uo pipefail
DB=${CLS01_DB:-cloudpivot_cls01}
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
P() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }

echo "== 期间"
P "select period_code||' '||status||' '||start_date||'~'||end_date from biz_accounting_period order by period_code"
echo "== 费用类型"
P "select type_code||' '||type_name||' acct='||coalesce(account_code,'-')||' deductible='||tax_deductible||' need_inv='||need_invoice_no||' limit='||amount_limit||' st='||status from biz_expense_type order by type_code"
echo "== 资产分类"
P "select category_code||' '||category_name||' life='||useful_life_months||' salvage='||salvage_rate||' method='||depreciation_method||' accum='||(select account_code from biz_account where id=accum_account_id)||' exp='||(select account_code from biz_account where id=expense_account_id) from biz_asset_category order by category_code"
echo "== 客户"
P "select code||' '||name from biz_customer order by code"
echo "== 物料"
P "select material_code||' '||material_name||' tax='||tax_rate||' costing='||costing_method||' serial='||serial_required from biz_material order by material_code"
echo "== 报表项（资产负债表，非标题行）"
P "select item_code||' '||item_name||' side='||coalesce(side,'-')||' level='||item_level from biz_report_item where report_code='BALANCE_SHEET' and item_level>0 order by sort_order"
echo "== 期初台账行数"
P "select count(*) from biz_opening_balance"
echo "== 已存在凭证"
P "select count(*) from biz_voucher"
