#!/usr/bin/env bash
# E2E-04 期间重置：把 2022 年度从「已年结 + 12 月全封」恢复到「12 月全 OPEN」
# 只动 2022 年度（本卡演练年度）。可重复执行。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
BASE=http://127.0.0.1:8033/api
TOKEN=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token // empty')
[ -n "$TOKEN" ] || { echo "FATAL: admin 登录失败"; exit 1; }
call() { curl -s -m 60 -X POST "$BASE$2" --oauth2-bearer "$TOKEN" -H 'Content-Type: application/json' -d "$3" -w '\n%{http_code}\n'; }

export PGPASSWORD="$NIK_DATABASE_PASSWORD"
Q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off -c "$1"; }

echo "== 当前状态"; Q "select period_code,status from biz_accounting_period where period_code like '2022-%' order by 1;"
YEAR_STATE=$(Q "select to_status from biz_status_log where entity_type='ACCOUNTING_YEAR' and entity_no='2022' order by created_at desc limit 1;")
echo "== 2022 年度状态: ${YEAR_STATE:-none}"
if [ "$YEAR_STATE" = "CLOSED" ]; then
  out=$(call /business/erp/year-end/reopen '{"year":2022,"reason":"E2E-04 演练重置：反年结以恢复演练起点"}')
  echo "== 反年结: $(echo "$out" | tail -1) $(echo "$out" | head -1 | jq -r '.msg // ""' | head -c 120)"
fi
for m in 12 11 10 09 08 07 06 05 04 03 02 01; do
  st=$(Q "select status from biz_accounting_period where period_code='2022-$m';")
  if [ "$st" = "CLOSED" ]; then
    out=$(call "/business/erp/period/$(Q "select id from biz_accounting_period where period_code='2022-$m';")/reopen" '{"reason":"E2E-04 演练重置：解封 2022 年度各月"}')
    echo "== 解封 2022-$m: $(echo "$out" | tail -1)"
  fi
done
echo "== 重置后"; Q "select period_code,status from biz_accounting_period where period_code like '2022-%' order by 1;" | tr '\n' ' '; echo
Q "select 'OPEN='||count(*) from biz_accounting_period where period_code like '2022-%' and status='OPEN';"
