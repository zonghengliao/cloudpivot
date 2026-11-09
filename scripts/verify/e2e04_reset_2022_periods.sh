#!/usr/bin/env bash
# E2E-04 期间重置：把 2022 年度从「已年结 + 12 月全封」恢复到「12 月全 OPEN」。
#
# 只动 2022 年度（本卡演练年度）。可重复执行。
# 坑（实测）：`period::reopen_period` 的「后续已结账期间」查询是**全库无界**的
#   （period_code > 本月 AND status='CLOSED'），所以共享演练库上任何一个更晚的
#   已结账期间（实测 2026-09，历史演练残留）都会让 2022-11 的反结账 409。
#   解法：先把挡路的后续期间临时解封，回撤完 2022 再按原样封回去。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE=http://127.0.0.1:8033/api
Q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off -c "$1"; }
TOKEN=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token // empty')
[ -n "$TOKEN" ] || { echo "FATAL: admin 登录失败"; exit 1; }
call() { curl -s -m 60 -X POST "$BASE$1" --oauth2-bearer "$TOKEN" -H 'Content-Type: application/json' -d "$2" -w '\n%{http_code}\n'; }
reopen() { local out; out=$(call "/business/erp/period/$(Q "select id from biz_accounting_period where period_code='$1';")/reopen" "{\"reason\":\"E2E-04 演练重置：解封 $1\"}"); echo "  reopen $1 -> $(echo "$out" | tail -1)"; }
closep() { local out; out=$(call "/business/erp/period/$(Q "select id from biz_accounting_period where period_code='$1';")/close" "{\"reason\":\"E2E-04 演练重置：恢复 $1 封账\"}"); echo "  close  $1 -> $(echo "$out" | tail -1)"; }

echo "== 当前 2022 状态"; Q "select period_code||'='||status from biz_accounting_period where period_code like '2022-%' order by 1;" | tr '\n' ' '; echo
YEAR_STATE=$(Q "select to_status from biz_status_log where entity_type='ACCOUNTING_YEAR' and entity_no='2022' order by created_at desc limit 1;")
echo "== 2022 年度状态: ${YEAR_STATE:-none}"
if [ "$YEAR_STATE" = "CLOSED" ]; then
  out=$(call /business/erp/year-end/reopen '{"year":2022,"reason":"E2E-04 演练重置：反年结以恢复演练起点"}')
  echo "== 反年结 -> $(echo "$out" | tail -1)"
fi
BLOCK=$(Q "select period_code from biz_accounting_period where period_code > '2022-12' and status='CLOSED' order by period_code desc;")
echo "== 挡回撤的后续已结账期间: ${BLOCK:-none}"
for b in $BLOCK; do reopen "$b"; done
for m in 11 10 09 08 07 06 05 04 03 02 01; do
  [ "$(Q "select status from biz_accounting_period where period_code='2022-$m';")" = "CLOSED" ] && reopen "2022-$m"
done
for b in $(echo "$BLOCK" | tac); do closep "$b"; done
echo "== 重置后 2022"; Q "select period_code||'='||status from biz_accounting_period where period_code like '2022-%' order by 1;" | tr '\n' ' '; echo
echo "== 后续期间已恢复："; for b in $BLOCK; do echo "  $b=$(Q "select status from biz_accounting_period where period_code='$b';")"; done
