#!/bin/bash
# HOME-2 真机造数：3 条逾期 + 9 条今日 = 12 条，验证卡片「最多 10 条 + 逾期优先」
# 全部走真接口 POST /api/business/task，不用 SQL 塞数。
set -euo pipefail
B=http://127.0.0.1:18023
D=/root/.hermes/profiles/frontend-developer/cache/scratch/home2
mkdir -p "$D"

curl -s -X POST "$B/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' > "$D/login-local.json"
T=$(jq -r .data.access_token "$D/login-local.json")
jq -nc --arg at "$T" --arg rt "$(jq -r .data.refresh_token "$D/login-local.json")" '{at:$at,rt:$rt}' > "$D/tokens.json"
AH="Authorization: Bearer $T"
echo "login code=$(jq -r .code "$D/login-local.json")"
UID_=$(curl -s "$B/api/system/auth/me" -H "$AH" | jq -r .data.id)
echo "uid=$UID_"

mk() { # title priority due
  local title="$1" prio="$2" due="$3"
  local body res id code
  body=$(jq -nc --arg t "$title" --arg p "$prio" --arg d "$due" --arg o "$UID_" \
    '{title:$t, priority:$p, due_date:$d, owner_id:$o}')
  res=$(curl -s -X POST "$B/api/business/task" -H "$AH" -H 'Content-Type: application/json' -d "$body")
  id=$(echo "$res" | jq -r '.data.id // empty')
  code=$(echo "$res" | jq -r .code)
  echo "create[$title] code=$code id=$id msg=$(echo "$res" | jq -r '.msg // ""')"
  echo "$id"
}

echo "== 3 条逾期（11-06 ~ 11-08）=="
mk "【HOME2】审核采购订单 PO-20261109-000003" P0 2026-11-08 >/dev/null
mk "【HOME2】处理报销单 EXP-20261109-0001" P1 2026-11-07 >/dev/null
mk "【HOME2】补签上周销售合同" P2 2026-11-06 >/dev/null

echo "== 9 条今日（11-09）=="
TODAY_TASKS=(
  "【HOME2】完成入库单核对|P2"
  "【HOME2】跟进客户回款|P1"
  "【HOME2】更新供应商档案|P3"
  "【HOME2】审核月度报销明细 A|P2"
  "【HOME2】准备周会材料|P2"
  "【HOME2】核对库存盘点差异|P1"
  "【HOME2】催收逾期发票（华东区客户，金额较大需逐张核对）|P2"
  "【HOME2】整理下月采购需求|P3"
  "【HOME2】复核工资表|P2"
)
FIRST_TODAY=""
for item in "${TODAY_TASKS[@]}"; do
  title="${item%%|*}"; prio="${item##*|}"
  out=$(mk "$title" "$prio" 2026-11-09)
  id=$(echo "$out" | tail -1)
  if [ -z "$FIRST_TODAY" ]; then FIRST_TODAY="$id"; fi
done

echo "== 把今日第一条改成进行中（DOING）=="
curl -s -X PATCH "$B/api/business/task/$FIRST_TODAY/status" -H "$AH" -H 'Content-Type: application/json' \
  -d '{"to":"DOING"}' | jq -c '{code, msg, status:.data.status}'

echo "== /business/task/my =="
curl -s "$B/api/business/task/my" -H "$AH" | jq '{code, counts:.data.counts, total:.data.total, overdue:[.data.overdue[].title], today:[.data.today[].title]}' | tee "$D/my-tasks.json"
