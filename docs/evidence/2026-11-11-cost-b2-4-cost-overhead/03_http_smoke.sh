#!/usr/bin/env bash
# COST-B2-4 手工费用台账 · HTTP 冒烟（真实例 + admin 真登录）
#
# 覆盖卡面验收三条：CRUD（C/R/D）/ 科目不在 4101.* → 拒绝 / 金额必须 > 0，
# 外加「鉴权闸」与「列表筛选」两条真实链路验证。
#
# 依赖：BASE 指向的实例已起（见 00_up.sh），库里有 4101.02 / 1002 科目种子。
#
# 用法：BASE=http://127.0.0.1:18027 bash 03_http_smoke.sh
set -uo pipefail

BASE="${BASE:-http://127.0.0.1:18027}"
API="$BASE/api"
SUF="$(date +%s)$RANDOM"
NEW_PW='Admin@123456b24'

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
  echo "   首次登录必须先改密：PUT /system/auth/password"
  CHANGED=$(curl -s -X PUT "$API/system/auth/password" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"Admin@123456\",\"new_password\":\"$NEW_PW\"}")
  echo "   改密结果 code=$(echo "$CHANGED" | py 'import sys,json;print(json.load(sys.stdin)["code"])')"
  LOGIN=$(login_with "$NEW_PW")
  TOKEN=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["data"]["access_token"])')
  echo "   新 token 长度 ${#TOKEN}"
fi
AUTH="Authorization: Bearer $TOKEN"

DEPT="${DEPT:-}"
if [ -z "$DEPT" ]; then
  # 直接取库里的部门（`/system/dept` 的返回形状随页面口味变，冒烟不该依赖它）
  DEPT=$(su - postgres -c "psql -d ${DB:-costb24_http} -Atc \"select id from sys_dept where deleted_at is null order by created_at limit 1\"" 2>/dev/null)
fi
if [ -z "$DEPT" ]; then echo "取不到部门 id，无法继续"; exit 1; fi
echo "   车间（部门）id = $DEPT"

echo "2) POST /business/erp/cost-overhead（新建：车间费用池行，4101.02 水电费 4000）"
CREATE=$(curl -s -X POST "$API/business/erp/cost-overhead" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"dept_id\":\"$DEPT\",\"expense_item\":\"UTILITY\",\"debit_account_code\":\"4101.02\",\"credit_account_code\":\"1002\",\"amount\":4000,\"remark\":\"B2-4 冒烟 $SUF\"}")
echo "$CREATE" | py 'import sys,json;d=json.load(sys.stdin);r=d["data"];print("   code=%s amount=%s debit=%s/%s credit=%s/%s item=%s/%s dept=%s object=%s source=%s"%(d["code"],r["amount"],r["debit_account_code"],r["debit_account_name"],r["credit_account_code"],r["credit_account_name"],r["expense_item"],r["expense_item_label"],r["dept_name"],r["cost_object_id"],r["source_type"]))'
ROW_ID=$(echo "$CREATE" | py 'import sys,json;print(json.load(sys.stdin)["data"]["id"])')
echo "   费用行 id = $ROW_ID"

echo "3) POST（借方留空 ⇒ 缺省 4101.09）"
DEFAULT_DEBIT=$(curl -s -X POST "$API/business/erp/cost-overhead" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"dept_id\":\"$DEPT\",\"expense_item\":\"OTHER\",\"credit_account_code\":\"2241.01\",\"amount\":120.5,\"remark\":\"B2-4 冒烟缺省科目 $SUF\"}")
echo "$DEFAULT_DEBIT" | py 'import sys,json;d=json.load(sys.stdin);r=d["data"];print("   code=%s debit=%s/%s amount=%s"%(d["code"],r["debit_account_code"],r["debit_account_name"],r["amount"]))'
DEFAULT_ID=$(echo "$DEFAULT_DEBIT" | py 'import sys,json;print(json.load(sys.stdin)["data"]["id"])')

echo "4) GET /business/erp/cost-overhead（列表：期间 + 费用项目 + 关键字）"
LIST=$(curl -s -G "$API/business/erp/cost-overhead" \
  --data-urlencode "period_code=2026-11" --data-urlencode "expense_item=UTILITY" \
  --data-urlencode "keyword=4101.02" -H "$AUTH")
echo "$LIST" | py 'import sys,json;d=json.load(sys.stdin);rows=d["data"]["list"];print("   code=%s total=%s page=%s page_size=%s 首行=%s/%s/%s"%(d["code"],d["data"]["total"],d["data"]["page"],d["data"]["page_size"],rows[0]["period_code"] if rows else "-",rows[0]["debit_account_code"] if rows else "-",rows[0]["amount"] if rows else "-"))'

echo "5) POST（借方 5602.01 —— 不在 4101.*）→ 期望 HTTP 400 + 66060"
BAD_ACC=$(curl -s -o /tmp/b24_bad_acc.json -w '%{http_code}' -X POST "$API/business/erp/cost-overhead" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"dept_id\":\"$DEPT\",\"expense_item\":\"UTILITY\",\"debit_account_code\":\"5602.01\",\"credit_account_code\":\"1002\",\"amount\":100,\"remark\":\"B2-4 冒烟坏科目 $SUF\"}")
echo "   HTTP $BAD_ACC"
py 'import json;d=json.load(open("/tmp/b24_bad_acc.json"));print("   code=%s msg=%s"%(d["code"],d["msg"].splitlines()[0]))'

echo "6) POST（金额 0）→ 期望 HTTP 400 + 66061"
BAD_AMT=$(curl -s -o /tmp/b24_bad_amt.json -w '%{http_code}' -X POST "$API/business/erp/cost-overhead" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"dept_id\":\"$DEPT\",\"expense_item\":\"UTILITY\",\"debit_account_code\":\"4101.02\",\"credit_account_code\":\"1002\",\"amount\":0,\"remark\":\"B2-4 冒烟零金额 $SUF\"}")
echo "   HTTP $BAD_AMT"
py 'import json;d=json.load(open("/tmp/b24_bad_amt.json"));print("   code=%s msg=%s"%(d["code"],d["msg"].splitlines()[0]))'

echo "7) POST（费用项目 TRAVEL 不在字典）→ 期望 HTTP 400"
BAD_ITEM=$(curl -s -o /tmp/b24_bad_item.json -w '%{http_code}' -X POST "$API/business/erp/cost-overhead" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"dept_id\":\"$DEPT\",\"expense_item\":\"TRAVEL\",\"debit_account_code\":\"4101.02\",\"credit_account_code\":\"1002\",\"amount\":100,\"remark\":\"B2-4 冒烟坏项目 $SUF\"}")
echo "   HTTP $BAD_ITEM"
py 'import json;d=json.load(open("/tmp/b24_bad_item.json"));print("   code=%s msg=%s"%(d["code"],d["msg"].splitlines()[0]))'

echo "8) POST（成本对象不存在）→ 期望 HTTP 400 + 66002"
BAD_OBJ=$(curl -s -o /tmp/b24_bad_obj.json -w '%{http_code}' -X POST "$API/business/erp/cost-overhead" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"cost_object_id\":\"018f4b2c-1111-7222-8333-444455556666\",\"dept_id\":\"$DEPT\",\"expense_item\":\"UTILITY\",\"debit_account_code\":\"4101.02\",\"credit_account_code\":\"1002\",\"amount\":100,\"remark\":\"B2-4 冒烟坏对象 $SUF\"}")
echo "   HTTP $BAD_OBJ"
py 'import json;d=json.load(open("/tmp/b24_bad_obj.json"));print("   code=%s msg=%s"%(d["code"],d["msg"].splitlines()[0]))'

echo "9) DELETE /business/erp/cost-overhead/{id}（两行都删）"
for id in "$ROW_ID" "$DEFAULT_ID"; do
  DEL=$(curl -s -X DELETE "$API/business/erp/cost-overhead/$id" -H "$AUTH")
  echo "   del $id → $(echo "$DEL" | py 'import sys,json;d=json.load(sys.stdin);print("code=%s"%d["code"])')"
done

echo "10) DELETE 同一行两次 → 期望 HTTP 404"
AGAIN=$(curl -s -o /tmp/b24_del_again.json -w '%{http_code}' -X DELETE "$API/business/erp/cost-overhead/$ROW_ID" -H "$AUTH")
echo "   HTTP $AGAIN code=$(py 'import json;print(json.load(open("/tmp/b24_del_again.json"))["code"])')"

echo "11) GET 列表（删完应为 0 条）"
AFTER=$(curl -s -G "$API/business/erp/cost-overhead" --data-urlencode "period_code=2026-11" \
  --data-urlencode "keyword=$SUF" -H "$AUTH")
echo "$AFTER" | py 'import sys,json;d=json.load(sys.stdin);print("   code=%s total=%s"%(d["code"],d["data"]["total"]))'

echo "12) 鉴权闸：无 token"
for req in "GET /business/erp/cost-overhead" "POST /business/erp/cost-overhead" "DELETE /business/erp/cost-overhead/$ROW_ID"; do
  method=${req%% *}; path=${req#* }
  code=$(curl -s -o /dev/null -w '%{http_code}' -X "$method" "$API$path" -H 'Content-Type: application/json' -d '{}')
  echo "   $method $path → HTTP $code"
done
echo "   未知路径 → HTTP $(curl -s -o /dev/null -w '%{http_code}' "$API/business/erp/cost-overhead-not-exist" -H "$AUTH")"
