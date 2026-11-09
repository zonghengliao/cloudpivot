#!/usr/bin/env bash
# E2E-04 期间重置：把 2022 年度从「已年结 + 12 月全封」恢复到「12 月全 OPEN」。
#
# 只动 2022 年度（本卡演练年度）。可重复执行。
#
# 历史坑（E2E04-FIX-2 已修）：`period::reopen_period` 的「后续已结账期间」查询
#   曾经是**全库无界**的（period_code > 本月 AND status='CLOSED'），所以共享演练库上
#   任何一个更晚的已结账期间（实测 2026-09，历史演练残留）都会让 2022-11 的反结账 409，
#   当时的脚本被迫「先临时解封挡路的后续期间 → 回撤 2022 → 再按原样封回去」。
#   现在判据收敛到同一会计年度：2022 之外的年度的封账状态**不再挡路**，
#   所以本脚本只按 2022 自己从 12 月往回撤，不再碰任何别的年度。
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

echo "== 当前 2022 状态"; Q "select period_code||'='||status from biz_accounting_period where period_code like '2022-%' order by 1;" | tr '\n' ' '; echo
YEAR_STATE=$(Q "select to_status from biz_status_log where entity_type='ACCOUNTING_YEAR' and entity_no='2022' order by created_at desc limit 1;")
echo "== 2022 年度状态: ${YEAR_STATE:-none}"
if [ "$YEAR_STATE" = "CLOSED" ]; then
  out=$(call /business/erp/year-end/reopen '{"year":2022,"reason":"E2E-04 演练重置：反年结以恢复演练起点"}')
  echo "== 反年结 -> $(echo "$out" | tail -1)"
fi
# 只作观测：这些别的年度的封账状态**不该**再挡 2022 的回撤（E2E04-FIX-2）。
# 老版本这里会把它们逐个 reopen 再逐个 close —— 现在不需要，也不再动别人的数据。
echo "== 其它年度的已结账期间（仅观测，不再临时解封）: $(Q "select coalesce(string_agg(period_code, ' ' order by period_code), 'none') from biz_accounting_period where period_code > '2022-12' and status='CLOSED';")"
for m in 12 11 10 09 08 07 06 05 04 03 02 01; do
  [ "$(Q "select status from biz_accounting_period where period_code='2022-$m';")" = "CLOSED" ] && reopen "2022-$m"
done
echo "== 重置后 2022"; Q "select period_code||'='||status from biz_accounting_period where period_code like '2022-%' order by 1;" | tr '\n' ' '; echo
