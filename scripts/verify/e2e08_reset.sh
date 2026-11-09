#!/usr/bin/env bash
# 145 上重置本卡演练工作目录（不删目录，只清状态与日志）并清理 E2E08 前缀残留
set -uo pipefail
mkdir -p /tmp/e2e08/raw
: > /tmp/e2e08/state.sh
: > /tmp/e2e08/asserts.log
find /tmp/e2e08/raw -maxdepth 1 -name '*.json' -delete 2>/dev/null
. /opt/cloudpivot/scripts/verify/e2e08_master_common.sh
cleanup_own
echo "-- 清理后残留计数 --"
SQ "select 'sup='||count(*) from biz_supplier where supplier_code like 'E2E08%' union all select 'cus='||count(*) from biz_customer where code like 'E2E08%' union all select 'mat='||count(*) from biz_material where material_code like 'E2E08%' union all select 'wh='||count(*) from biz_warehouse where warehouse_code like 'E2E08%' union all select 'ac='||count(*) from biz_asset_category where category_code like 'E2E08%' union all select 'et='||count(*) from biz_expense_type where type_code like 'E2E08%' union all select 'unit='||count(*) from biz_unit where unit_code like 'E2E08%'"
