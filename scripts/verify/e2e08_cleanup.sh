#!/usr/bin/env bash
# E2E-08 演练数据自清（只删 E2E08 前缀；不 DROP / TRUNCATE / 不动他人数据）
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e08_master_common.sh"
cleanup_own
echo "-- 清理后本卡存活行 --"
SQ "select 'sup='||count(*) from biz_supplier where supplier_code ilike 'E2E08%' and deleted_at is null
 union all select 'cus='||count(*) from biz_customer where code ilike 'E2E08%' and deleted_at is null
 union all select 'mat='||count(*) from biz_material where material_code ilike 'E2E08%' and deleted_at is null
 union all select 'wh='||count(*) from biz_warehouse where warehouse_code ilike 'E2E08%' and deleted_at is null
 union all select 'ac='||count(*) from biz_asset_category where category_code ilike 'E2E08%' and deleted_at is null
 union all select 'et='||count(*) from biz_expense_type where type_code ilike 'E2E08%' and deleted_at is null
 union all select 'unit='||count(*) from biz_unit where unit_code ilike 'E2E08%' and deleted_at is null
 union all select 'price_rows='||count(*) from biz_price_tier where material_id in (select id from biz_material where material_code ilike 'E2E08%')
 union all select 'agreed_rows='||count(*) from biz_customer_price where material_id in (select id from biz_material where material_code ilike 'E2E08%')
 union all select 'serial_rows='||count(*) from biz_material_serial where material_id in (select id from biz_material where material_code ilike 'E2E08%')
 union all select 'user='||count(*) from sys_user where username ilike 'e2e08%'"
