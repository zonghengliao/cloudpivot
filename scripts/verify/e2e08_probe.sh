#!/usr/bin/env bash
# 只读探针：确认 E2E-08 需要的接口/权限/响应形态（不改数据）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
B=http://127.0.0.1:8033/api
T=$(curl -s -m 20 -X POST "$B/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')
echo "token_len=${#T}"
g() { printf '%-52s ' "$1"; curl -s -m 20 --oauth2-bearer "$T" "$B$1" -o /tmp/p.json -w '%{http_code} '; head -c 320 /tmp/p.json; echo; }
g '/business/erp/customer/page?page=1&page_size=1'
g '/business/erp/supplier/page?page=1&page_size=1'
g '/business/erp/material/page?page=1&page_size=1'
g '/business/erp/warehouse/tree'
g '/business/erp/unit/page?page=1&page_size=1'
g '/business/erp/price/agreed/page?page=1&page_size=1'
g '/business/erp/price/tier/page?page=1&page_size=1'
g '/business/erp/master-data/code-policy'
g '/business/erp/asset-category/next-code'
g '/business/erp/expense-type/next-code'
g '/business/erp/serial/page?page=1&page_size=1'
g '/support/config/page?page=1&page_size=1&keyword=erp.master_data'
echo "--- seq ---"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -c "select seq_type,prefix,separator,pad_width,current_no from biz_seq where deleted_at is null and seq_type in ('SUP','CUS','MAT','WH','AC','ET') order by seq_type"
echo "--- 既有 E2E08 残留 ---"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -c "select 'sup',count(*) from biz_supplier where supplier_code like 'E2E08%' union all select 'cus',count(*) from biz_customer where code like 'E2E08%' union all select 'mat',count(*) from biz_material where material_code like 'E2E08%' union all select 'wh',count(*) from biz_warehouse where warehouse_code like 'E2E08%' union all select 'ac',count(*) from biz_asset_category where category_code like 'E2E08%' union all select 'et',count(*) from biz_expense_type where type_code like 'E2E08%' union all select 'unit',count(*) from biz_unit where unit_code like 'E2E08%'"
