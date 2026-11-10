#!/usr/bin/env bash
# COST-B2-5 分摊规则 · HTTP 冒烟（真实例 + admin 真登录）
#
# 覆盖：5 个接口全通（GET / POST / PUT / DELETE / preview）+ 卡面验收项①
# 「规则按生效期选最新版本」（同一 (车间,科目) 两条版本，11 月试算必须取 11-01 那条）
# + 鉴权闸（无 token 401）+ 未知路径 404 + 科目不合法 400/66060 + 期间不合法 400。
#
# 依赖：BASE 指向的实例已起（见 00_up.sh），库里有 4101.02 / 1002 科目种子。
#
# 用法：BASE=http://127.0.0.1:18028 bash 03_http_smoke.sh
set -uo pipefail

BASE="${BASE:-http://127.0.0.1:18028}"
API="$BASE/api"
SUF="$(date +%s)$RANDOM"
SUF6="${SUF: -6}"
NEW_PW='Admin@123456b25'

py() { python3 -c "$1"; }

echo "1) 登录 admin"
login_with() {
  curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$1\"}"
}
LOGIN=$(login_with 'Admin@123456')
CODE=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["code"])')
if [ "$CODE" != "0" ]; then
  LOGIN=$(login_with "$NEW_PW")
  CODE=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["code"])')
fi
TOKEN=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["data"]["access_token"])')
MUST=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["data"]["must_change_password"])')
echo "   login code=$CODE token 长度 ${#TOKEN}  must_change_password=$MUST"
if [ -z "$TOKEN" ]; then echo "登录失败：$LOGIN"; exit 1; fi

if [ "$MUST" = "True" ]; then
  CHANGED=$(curl -s -X PUT "$API/system/auth/password" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"Admin@123456\",\"new_password\":\"$NEW_PW\"}")
  echo "   改密 code=$(echo "$CHANGED" | py 'import sys,json;print(json.load(sys.stdin)["code"])')"
  LOGIN=$(login_with "$NEW_PW")
  TOKEN=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["data"]["access_token"])')
  echo "   新 token 长度 ${#TOKEN}"
fi
AUTH="Authorization: Bearer $TOKEN"

DEPT="${DEPT:-}"
if [ -z "$DEPT" ]; then
  DEPT=$(su - postgres -c "psql -d ${DB:-costb25_http} -Atc \"select id from sys_dept where deleted_at is null order by created_at limit 1\"" 2>/dev/null)
fi
if [ -z "$DEPT" ]; then echo "取不到部门 id"; exit 1; fi
echo "   车间（部门）id = $DEPT"

echo "2) GET /business/erp/cost-alloc-rule（空列表）"
LIST0=$(curl -s -G "$API/business/erp/cost-alloc-rule" -H "$AUTH")
echo "$LIST0" | py 'import sys,json;d=json.load(sys.stdin);print("   code=%s total=%s page_size=%s"%(d["code"],d["data"]["total"],d["data"]["page_size"]))'

echo "3) POST 建旧版本（2026-01-01 生效，按人工工时）"
R1=$(curl -s -X POST "$API/business/erp/cost-alloc-rule" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"rule_code\":\"WL-$SUF6-V1\",\"rule_name\":\"一车间水电按工时（旧）\",\"dept_id\":\"$DEPT\",\"account_code\":\"4101.02\",\"basis\":\"LABOR_HOURS\",\"effective_from\":\"2026-01-01\"}")
echo "$R1" | py 'import sys,json;d=json.load(sys.stdin);r=d["data"];print("   code=%s rule=%s basis=%s/%s 生效=%s~%s 车间=%s 科目=%s/%s 状态=%s"%(d["code"],r["rule_code"],r["basis"],r["basis_label"],r["effective_from"],r["effective_to"],r["source_dept_name"],r["account_code"],r["account_name"],r["status_label"]))'
ID1=$(echo "$R1" | py 'import sys,json;print(json.load(sys.stdin)["data"]["id"])')

echo "4) POST 建新版本（2026-11-01 生效，按机器工时）"
R2=$(curl -s -X POST "$API/business/erp/cost-alloc-rule" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"rule_code\":\"WL-$SUF6-V2\",\"rule_name\":\"一车间水电按机器工时（新）\",\"dept_id\":\"$DEPT\",\"account_code\":\"4101.02\",\"basis\":\"MACHINE_HOURS\",\"priority\":20,\"effective_from\":\"2026-11-01\"}")
echo "$R2" | py 'import sys,json;d=json.load(sys.stdin);r=d["data"];print("   code=%s rule=%s basis=%s/%s priority=%s 生效=%s"%(d["code"],r["rule_code"],r["basis"],r["basis_label"],r["priority"],r["effective_from"]))'
ID2=$(echo "$R2" | py 'import sys,json;print(json.load(sys.stdin)["data"]["id"])')

echo "5) GET 列表（period_code=2026-11 + 车间 + 科目）"
LIST=$(curl -s -G "$API/business/erp/cost-alloc-rule" \
  --data-urlencode "period_code=2026-11" --data-urlencode "dept_id=$DEPT" \
  --data-urlencode "account_code=4101.02" -H "$AUTH")
echo "$LIST" | py 'import sys,json;d=json.load(sys.stdin);rows=d["data"]["list"];print("   code=%s total=%s 规则=%s"%(d["code"],d["data"]["total"],[ (r["rule_code"],r["basis"],r["effective_from"]) for r in rows]))'

echo "6) POST /preview（2026-11 —— 版本选择：必须取 11-01 那条 = $ID2）"
PREVIEW=$(curl -s -X POST "$API/business/erp/cost-alloc-rule/preview" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"period_code":"2026-11"}')
echo "$PREVIEW" | py 'import sys,json;d=json.load(sys.stdin);p=d["data"];g=p["groups"];print("   code=%s groups=%s total_pool=%s total_alloc=%s unallocated=%s"%(d["code"],len(g),p["total_pool_amount"],p["total_alloc_amount"],len(p["unallocated"])));print("   选中规则=%s basis=%s/%s pool=%s basis_total=%s rows=%s"%(g[0]["rule_code"] if g else "-",g[0]["basis"] if g else "-",g[0]["basis_label"] if g else "-",g[0]["pool_amount"] if g else "-",g[0]["basis_total"] if g else "-",len(g[0]["rows"]) if g else 0))'
GOT=$(echo "$PREVIEW" | py 'import sys,json;g=json.load(sys.stdin)["data"]["groups"];print(g[0]["rule_id"] if g else "")')
if [ "$GOT" = "$ID2" ]; then
  echo "   ✓ 版本选择正确：11 月取 2026-11-01 的版本（$GOT）"
else
  echo "   ✗ 版本选择错误：期望 $ID2，实际 $GOT"; exit 1
fi

echo "7) POST /preview（2026-10 —— 必须回落旧版本 = $ID1）"
PREVIEW_OLD=$(curl -s -X POST "$API/business/erp/cost-alloc-rule/preview" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"period_code":"2026-10"}')
GOT_OLD=$(echo "$PREVIEW_OLD" | py 'import sys,json;g=json.load(sys.stdin)["data"]["groups"];print(g[0]["rule_id"] if g else "")')
echo "   选中规则 id=$GOT_OLD（basis=$(echo "$PREVIEW_OLD" | py 'import sys,json;g=json.load(sys.stdin)["data"]["groups"];print(g[0]["basis"] if g else "-")')）"
if [ "$GOT_OLD" = "$ID1" ]; then
  echo "   ✓ 历史期间不被新版本改口径"
else
  echo "   ✗ 10 月应取旧版本 $ID1，实际 $GOT_OLD"; exit 1
fi

echo "8) PUT /business/erp/cost-alloc-rule/{id}（改名 + 停用）"
UPD=$(curl -s -X PUT "$API/business/erp/cost-alloc-rule/$ID2" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"rule_name\":\"一车间水电（改名）\",\"status\":0}")
echo "$UPD" | py 'import sys,json;d=json.load(sys.stdin);r=d["data"];print("   code=%s name=%s status=%s/%s"%(d["code"],r["rule_name"],r["status"],r["status_label"]))'

echo "9) POST /preview（停用新版本后，11 月回落到旧版本）"
PREVIEW2=$(curl -s -X POST "$API/business/erp/cost-alloc-rule/preview" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"period_code":"2026-11"}')
GOT2=$(echo "$PREVIEW2" | py 'import sys,json;g=json.load(sys.stdin)["data"]["groups"];print(g[0]["rule_id"] if g else "")')
[ "$GOT2" = "$ID1" ] && echo "   ✓ 停用后回落到 $ID1" || { echo "   ✗ 期望 $ID1，实际 $GOT2"; exit 1; }

echo "10) DELETE /business/erp/cost-alloc-rule/{id}（软删两条）"
for id in "$ID1" "$ID2"; do
  DEL=$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$API/business/erp/cost-alloc-rule/$id" -H "$AUTH")
  echo "   DELETE $id → HTTP $DEL"
done
LIST2=$(curl -s -G "$API/business/erp/cost-alloc-rule" --data-urlencode "dept_id=$DEPT" -H "$AUTH")
echo "$LIST2" | py 'import sys,json;d=json.load(sys.stdin);print("   软删后列表 total=%s"%d["data"]["total"])'

echo "11) 反证：科目不在 4101.* → HTTP 400 + 66060"
BAD=$(curl -s -o /tmp/b25_bad_acc.json -w '%{http_code}' -X POST "$API/business/erp/cost-alloc-rule" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"rule_code\":\"BAD-$SUF6\",\"rule_name\":\"坏科目\",\"dept_id\":\"$DEPT\",\"account_code\":\"5602.01\",\"basis\":\"LABOR_HOURS\",\"effective_from\":\"2026-01-01\"}")
echo "   HTTP $BAD"
py 'import json;d=json.load(open("/tmp/b25_bad_acc.json"));print("   code=%s msg=%s"%(d["code"],d["msg"].splitlines()[0]))'

echo "12) 反证：期间不合法 → HTTP 400"
BADP=$(curl -s -o /tmp/b25_bad_period.json -w '%{http_code}' -X POST "$API/business/erp/cost-alloc-rule/preview" \
  -H "$AUTH" -H 'Content-Type: application/json' -d '{"period_code":"2026-13"}')
echo "   HTTP $BADP"
py 'import json;d=json.load(open("/tmp/b25_bad_period.json"));print("   code=%s msg=%s"%(d["code"],d["msg"].splitlines()[0]))'

echo "13) 反证：无 token → HTTP 401"
echo "   GET list → HTTP $(curl -s -o /dev/null -w '%{http_code}' "$API/business/erp/cost-alloc-rule")"
echo "   POST preview → HTTP $(curl -s -o /dev/null -w '%{http_code}' -X POST "$API/business/erp/cost-alloc-rule/preview" -H 'Content-Type: application/json' -d '{"period_code":"2026-11"}')"

echo "14) 反证：未知路径 → HTTP 404"
echo "   GET /business/erp/cost-alloc-rule-none → HTTP $(curl -s -o /dev/null -w '%{http_code}' "$API/business/erp/cost-alloc-rule-none" -H "$AUTH")"

echo
echo "冒烟完成"
