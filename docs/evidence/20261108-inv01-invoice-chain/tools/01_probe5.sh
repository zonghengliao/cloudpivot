#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${INV01_DB:-cloudpivot_inv01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== 2221 subtree"; q "select account_code||' | '||account_name||' | leaf='||is_leaf from biz_account where account_code like '2221%' or account_code like '5403%' or account_code like '1002%' order by account_code"
echo "== report codes"; q "select distinct report_code from biz_report_item order by report_code"
echo "== cash flow tax items"; q "select report_code||' | '||item_code||' | '||item_name||' | pattern='||coalesce(account_pattern,'-')||' | formula='||coalesce(formula,'-') from biz_report_item where report_code='CASH_FLOW' and (item_name like '%税%' or coalesce(account_pattern,'') like '%2221%') order by sort_order"
echo "== income tax items"; q "select item_code||' | '||item_name||' | pattern='||coalesce(account_pattern,'-') from biz_report_item where report_code='INCOME_STATEMENT' order by sort_order" | head -25
