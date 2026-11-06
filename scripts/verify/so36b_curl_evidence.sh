#!/usr/bin/env bash
# SO-36b 佣金规则 CRUD 的真实 curl 留证（真实 token，隔离库 cloudpivot_so36 @ :18243）
#
#   bash scripts/verify/so36_instance.sh up
#   bash scripts/verify/so36b_curl_evidence.sh | tee docs/evidence/20261107-so36b-commission-rule/curl_transcript.txt
#   bash scripts/verify/so36_instance.sh down
#
# 前置：夹具由 SO-36 的 so36_fixture.sql 提供（幂等重放，清空结算单与规则后重建
#       订单 / 发货 / 出库 / 应收 / 三条夹具规则），本脚本开头会重放一次拿到干净基线。
#
# 覆盖（每条都带断言，断言失败脚本退出码非 0）：
#   A 语义回归（规则**改动**如何影响计提，真库真计提）
#     A1 夹具基线：2026-11 计提 → SP1 基数 400 / 佣金 80（销售员甲 20% 胜出）
#     A2 AR-A 已收 400 → 600，2027-01 计提 → delta 200 / 佣金 40，命中「SO36 销售员甲 20%」
#     A3 PUT 停用该规则（status=INACTIVE）→ 其余字段逐字不变
#     A4 AR-A 已收 600 → 800，2027-02 计提 → 佣金 16 = 物料一 10%（12）+ 全局 5%（4）
#        ⇒ 停用后**回落到物料 / 全局规则**，金额随之变化（规则 CRUD 真的接进了计提）
#     A5 PUT 恢复 ACTIVE
#   B CRUD 四件套
#     B1 POST 新建 4 条（全局 / 物料 / 销售员 / 固定额）→ 回单字段逐个核对
#        · 金额与比率序列化成 4 位小数字符串（"4.0000" / "60.0000"）
#        · scope_name 派生：销售员 → 姓名、物料 → material_name、全局 → null
#     B2 GET /page 分页与筛选（keyword / status / scope_type / calc_type / 排序 / 总数）
#     B3 PUT 部分更新（只发 status）→ 其余字段不变；PUT 清空 max_amount（null）→ 真清空
#     B4 DELETE 未被引用的规则 → 列表消失、再次 DELETE → 404
#     B5 DELETE 被佣金明细引用的规则 → 409（历史可追溯，只能停用）
#   C 负路径：scope 不一致 / 不存在的物料 / PERCENT>100 / 生效期倒挂 / 封顶<起算 /
#            状态乱码 → 400；无 token → 401
set -uo pipefail
BASE=${SO36B_BASE:-http://127.0.0.1:18243}
DB=${SO36B_DB:-cloudpivot_so36}
SP1=e3600000-0000-4000-8000-000000000011
M1=e3600000-0000-4000-8000-000000000021
RULE_SP1=e3600000-0000-4000-8000-000000000073
FAILED=0

say() { printf '\n### %s\n' "$1"; }
fail() { echo "❌ ASSERT FAIL: $1"; FAILED=$((FAILED + 1)); }
# assert_eq <实际> <期望> <说明>
assert_eq() {
  if [ "$1" = "$2" ]; then echo "✅ $3 = $1"; else fail "$3 期望 [$2] 实际 [$1]"; fi
}
json() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)"; }
psql_dbx() { runuser -u postgres -- psql -q -d "$DB" -c "$1"; }

say "⓪ 重放夹具（幂等：清空结算单 / 规则后重建订单→发货→出库→应收与三条规则）"
runuser -u postgres -- psql -q -d "$DB" -f "$(dirname "$0")/so36_fixture.sql" | tail -6
assert_eq "$(runuser -u postgres -- psql -tA -d "$DB" -c "SELECT count(*) FROM biz_commission_rule WHERE deleted_at IS NULL")" "3" "夹具规则条数"
assert_eq "$(runuser -u postgres -- psql -tA -d "$DB" -c "SELECT count(*) FROM biz_commission_settlement")" "0" "夹具清空后的结算单数"

say "① 登录拿 token（admin / SUPER_ADMIN 短路权限）"
TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' \
  | json 'd["data"]["access_token"]')
echo "token 长度 = ${#TOKEN}"
[ -n "$TOKEN" ] || { echo "登录失败"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

# ============================================================
# A. 语义回归：规则改动 → 计提结果变化（真库）
# ============================================================

say "A1 夹具基线：2026-11 计提（SP1 命中销售员甲 20%）"
BASE_SETTLE=$(curl -s -X POST "$BASE/api/business/erp/commission/settle" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"period_code":"2026-11","remark":"SO-36b 基线"}')
assert_eq "$(echo "$BASE_SETTLE" | json 'next(s["total_commission"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')" "80.0000" "基线佣金（400 × 20%）"

say "A2 AR-A 已收 400 → 600，2027-01 计提 → delta 200 / 佣金 40（仍命中销售员甲 20%）"
psql_dbx "UPDATE biz_receivable SET received_amount = 600, unsettled_amount = 400, status='PARTIAL' WHERE ar_no='__SO36-AR-A__'" >/dev/null
JAN=$(curl -s -X POST "$BASE/api/business/erp/commission/settle" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"period_code":"2027-01"}')
JAN_ID=$(echo "$JAN" | json 'next(s["id"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')
assert_eq "$(echo "$JAN" | json 'next(s["total_sales"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')" "200.0000" "2027-01 基数（600 − 已认领 400）"
assert_eq "$(echo "$JAN" | json 'next(s["total_commission"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')" "40.0000" "2027-01 佣金（200 × 20%）"
JAN_DETAIL=$(curl -s "$BASE/api/business/erp/commission/$JAN_ID" -H "$AUTH")
assert_eq "$(echo "$JAN_DETAIL" | json 'sorted({i["rule_name"] for i in d["data"]["items"]})')" "['SO36 销售员甲 20%']" "命中规则（销售员级）"

say "A3 PUT 停用销售员规则（只发 status=INACTIVE）→ 其余字段逐字不变"
STOP=$(curl -s -X PUT "$BASE/api/business/erp/commission-rule/$RULE_SP1" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"status":"INACTIVE"}')
echo "$STOP" | python3 -m json.tool
assert_eq "$(echo "$STOP" | json 'd["data"]["status"]')" "INACTIVE" "停用后的状态"
assert_eq "$(echo "$STOP" | json 'd["data"]["rate"]')" "20.0000" "停用不改比率"
assert_eq "$(echo "$STOP" | json 'd["data"]["scope_type"]')" "SALESPERSON" "停用不改范围"
assert_eq "$(echo "$STOP" | json 'd["data"]["scope_name"]')" "SO36 销售员甲" "停用后仍能读出范围名"

say "A4 AR-A 已收 600 → 800，2027-02 计提 → 16 = 物料一 10%（12）+ 全局 5%（4）"
psql_dbx "UPDATE biz_receivable SET received_amount = 800, unsettled_amount = 200, status='PARTIAL' WHERE ar_no='__SO36-AR-A__'" >/dev/null
FEB=$(curl -s -X POST "$BASE/api/business/erp/commission/settle" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"period_code":"2027-02"}')
FEB_ID=$(echo "$FEB" | json 'next(s["id"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')
assert_eq "$(echo "$FEB" | json 'next(s["total_sales"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')" "200.0000" "2027-02 基数"
assert_eq "$(echo "$FEB" | json 'next(s["total_commission"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')" "16.0000" "2027-02 佣金（停用后回落到物料 10% + 全局 5%）"
FEB_DETAIL=$(curl -s "$BASE/api/business/erp/commission/$FEB_ID" -H "$AUTH")
echo "$FEB_DETAIL" | python3 -c 'import json,sys;d=json.load(sys.stdin)["data"];print([{k:i[k] for k in ("base_amount","rate","commission","rule_name")} for i in d["items"]])'
assert_eq "$(echo "$FEB_DETAIL" | json 'sorted(i["commission"] for i in d["data"]["items"])')" "['12.0000', '4.0000']" "两行佣金：物料一 12 + 其余 4"
assert_eq "$(echo "$FEB_DETAIL" | json 'sorted({i["rule_name"] for i in d["data"]["items"]})')" "['SO36 全局 5%', 'SO36 物料一 10% 封顶 60']" "停用后命中物料级 + 全局级"
assert_eq "$(echo "$FEB_DETAIL" | json 'sorted(i["rate"] for i in d["data"]["items"])')" "['10.0000', '5.0000']" "两行比率快照"

say "A5 PUT 恢复销售员规则为 ACTIVE（停用是可逆的，不是删除）"
BACK=$(curl -s -X PUT "$BASE/api/business/erp/commission-rule/$RULE_SP1" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"status":"ACTIVE"}')
assert_eq "$(echo "$BACK" | json 'd["data"]["status"]')" "ACTIVE" "恢复启用"

# ============================================================
# B. CRUD 四件套
# ============================================================

say "B1 新建 4 条规则（全局 / 物料 / 销售员 / 固定额）"
NEW_ALL=$(curl -s -X POST "$BASE/api/business/erp/commission-rule" -H "$AUTH" \
  -H 'Content-Type: application/json' \
  -d '{"rule_name":"SO36B 全局 4%","scope_type":"ALL","scope_id":null,"calc_type":"PERCENT",
       "rate":"4","min_amount":"0","max_amount":null,"effective_from":"2030-01-01",
       "effective_to":null,"status":"ACTIVE","remark":"SO-36b 留证：全局兜底"}')
echo "$NEW_ALL" | python3 -m json.tool
ALL_ID=$(echo "$NEW_ALL" | json 'd["data"]["id"]')
assert_eq "$(echo "$NEW_ALL" | json 'd["data"]["rate"]')" "4.0000" "比率序列化成 4 位小数字符串"
assert_eq "$(echo "$NEW_ALL" | json 'd["data"]["min_amount"]')" "0.0000" "起算额 4 位小数"
assert_eq "$(echo "$NEW_ALL" | json 'd["data"]["max_amount"]')" "None" "不封顶 = null"
assert_eq "$(echo "$NEW_ALL" | json 'd["data"]["scope_id"]')" "None" "ALL 的 scope_id 为 null"
assert_eq "$(echo "$NEW_ALL" | json 'd["data"]["scope_name"]')" "None" "ALL 的 scope_name 为 null"
assert_eq "$(echo "$NEW_ALL" | json 'd["data"]["status"]')" "ACTIVE" "默认启用"
assert_eq "$(echo "$NEW_ALL" | json 'd["data"]["effective_from"]')" "2030-01-01" "生效起 YYYY-MM-DD"
assert_eq "$(echo "$NEW_ALL" | json 'd["data"]["effective_to"]')" "None" "长期有效 = null"

NEW_MAT=$(curl -s -X POST "$BASE/api/business/erp/commission-rule" -H "$AUTH" \
  -H 'Content-Type: application/json' \
  -d '{"rule_name":"SO36B 物料一 10% 封顶 60","scope_type":"MATERIAL_CATEGORY","scope_id":"'"$M1"'",
       "calc_type":"PERCENT","rate":"10","min_amount":"0","max_amount":"60",
       "effective_from":"2030-01-01","status":"ACTIVE"}')
MAT_ID=$(echo "$NEW_MAT" | json 'd["data"]["id"]')
assert_eq "$(echo "$NEW_MAT" | json 'd["data"]["scope_name"]')" "SO36 物料一" "物料级 scope_name = biz_material.material_name"
assert_eq "$(echo "$NEW_MAT" | json 'd["data"]["max_amount"]')" "60.0000" "封顶 4 位小数"

NEW_SP=$(curl -s -X POST "$BASE/api/business/erp/commission-rule" -H "$AUTH" \
  -H 'Content-Type: application/json' \
  -d '{"rule_name":"SO36B 甲专属 25%","scope_type":"SALESPERSON","scope_id":"'"$SP1"'",
       "calc_type":"PERCENT","rate":"25","effective_from":"2030-01-01"}')
SP_ID=$(echo "$NEW_SP" | json 'd["data"]["id"]')
assert_eq "$(echo "$NEW_SP" | json 'd["data"]["scope_name"]')" "SO36 销售员甲" "销售员级 scope_name = sys_user.real_name"

NEW_FIXED=$(curl -s -X POST "$BASE/api/business/erp/commission-rule" -H "$AUTH" \
  -H 'Content-Type: application/json' \
  -d '{"rule_name":"SO36B 固定 150 元","scope_type":"ALL","calc_type":"FIXED","rate":"150",
       "min_amount":"500","effective_from":"2030-01-01","remark":"SO-36b 留证：固定额"}')
FIXED_ID=$(echo "$NEW_FIXED" | json 'd["data"]["id"]')
assert_eq "$(echo "$NEW_FIXED" | json 'd["data"]["calc_type"]')" "FIXED" "固定额计算方式"
assert_eq "$(echo "$NEW_FIXED" | json 'd["data"]["rate"]')" "150.0000" "FIXED 的 rate 是金额，同样 4 位小数"

say "B2 分页与筛选（GET /api/business/erp/commission-rule/page）"
PAGE=$(curl -s "$BASE/api/business/erp/commission-rule/page?page=1&page_size=20&keyword=SO36B" -H "$AUTH")
echo "$PAGE" | python3 -c 'import json,sys;d=json.load(sys.stdin)["data"];print({"total":d["total"],"rows":[{k:r[k] for k in ("rule_name","scope_type","scope_name","rate","status")} for r in d["list"]]})'
assert_eq "$(echo "$PAGE" | json 'd["data"]["total"]')" "4" "SO36B 规则总数"
assert_eq "$(echo "$PAGE" | json 'd["data"]["page"]')" "1" "页码回显"
assert_eq "$(echo "$PAGE" | json 'd["data"]["page_size"]')" "20" "每页条数回显"
assert_eq "$(echo "$PAGE" | json 'd["data"]["list"][0]["status"]')" "ACTIVE" "启用规则排在前面（排序首键 status）"
assert_eq "$(curl -s "$BASE/api/business/erp/commission-rule/page?keyword=SO36B&scope_type=MATERIAL_CATEGORY" -H "$AUTH" | json 'd["data"]["total"]')" "1" "scope_type 过滤"
assert_eq "$(curl -s "$BASE/api/business/erp/commission-rule/page?keyword=SO36B&calc_type=FIXED" -H "$AUTH" | json 'd["data"]["total"]')" "1" "calc_type 过滤"
# 新规则名「SO36B 甲专属 25%」里没有「销售员甲」：命中数是 2 说明 keyword 也扫了 scope_name
assert_eq "$(curl -s "$BASE/api/business/erp/commission-rule/page?keyword=销售员甲&scope_type=SALESPERSON" -H "$AUTH" | json 'd["data"]["total"]')" "2" "keyword 匹配范围名（销售员姓名）"
assert_eq "$(curl -s "$BASE/api/business/erp/commission-rule/page?keyword=甲专属" -H "$AUTH" | json 'd["data"]["total"]')" "1" "keyword 匹配规则名"

say "B3 PUT 部分更新：只发 rate → 其余不变；再发 max_amount=null → 真清空"
PATCH=$(curl -s -X PUT "$BASE/api/business/erp/commission-rule/$MAT_ID" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"rate":"12"}')
assert_eq "$(echo "$PATCH" | json 'd["data"]["rate"]')" "12.0000" "比率改成 12%"
assert_eq "$(echo "$PATCH" | json 'd["data"]["rule_name"]')" "SO36B 物料一 10% 封顶 60" "规则名未被改"
assert_eq "$(echo "$PATCH" | json 'd["data"]["max_amount"]')" "60.0000" "封顶未被改"
assert_eq "$(echo "$PATCH" | json 'd["data"]["min_amount"]')" "0.0000" "起算额未被改"
assert_eq "$(echo "$PATCH" | json 'd["data"]["scope_name"]')" "SO36 物料一" "范围未被改"
CLEAR=$(curl -s -X PUT "$BASE/api/business/erp/commission-rule/$MAT_ID" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"max_amount":null,"effective_to":null}')
assert_eq "$(echo "$CLEAR" | json 'd["data"]["max_amount"]')" "None" "显式 null 清空封顶"
assert_eq "$(echo "$CLEAR" | json 'd["data"]["effective_to"]')" "None" "显式 null 清空生效止"

say "B4 DELETE 未被引用的规则（录错了才用）"
CODE=$(curl -s -o /tmp/so36b_del.json -w '%{http_code}' -X DELETE "$BASE/api/business/erp/commission-rule/$FIXED_ID" -H "$AUTH")
assert_eq "$CODE" "200" "软删未被引用的规则"
assert_eq "$(curl -s "$BASE/api/business/erp/commission-rule/page?keyword=SO36B" -H "$AUTH" | json 'd["data"]["total"]')" "3" "软删后列表少一条"
assert_eq "$(curl -s "$BASE/api/business/erp/commission-rule/page?keyword=SO36B&status=INACTIVE" -H "$AUTH" | json 'd["data"]["total"]')" "0" "软删行不出现在任何状态筛选里"
CODE=$(curl -s -o /tmp/so36b_del2.json -w '%{http_code}' -X DELETE "$BASE/api/business/erp/commission-rule/$FIXED_ID" -H "$AUTH")
assert_eq "$CODE" "404" "重复删除 → 404"

say "B5 DELETE 被佣金明细引用的规则 → 409（历史明细要能追溯，只能停用）"
REFS=$(runuser -u postgres -- psql -tA -d "$DB" -c "SELECT count(*) FROM biz_commission_item i JOIN biz_commission_settlement s ON s.id=i.settlement_id WHERE i.rule_id='$RULE_SP1' AND s.deleted_at IS NULL")
echo "夹具规则 073 的存活明细引用数 = $REFS"
[ "$REFS" -gt 0 ] || fail "夹具规则 073 应该有明细引用（A2 计提产生），否则 409 用例无效"
CODE=$(curl -s -o /tmp/so36b_del3.json -w '%{http_code}' -X DELETE "$BASE/api/business/erp/commission-rule/$RULE_SP1" -H "$AUTH")
assert_eq "$CODE" "409" "被引用的规则拒绝删除"
head -c 300 /tmp/so36b_del3.json; echo

# ============================================================
# C. 负路径
# ============================================================

say "C 负路径（全部 400，文案指名到字段）"
bad_code() { # <说明> <JSON>
  local label=$1 payload=$2
  curl -s -o /tmp/so36b_bad.json -w '%{http_code}' -X POST "$BASE/api/business/erp/commission-rule" \
    -H "$AUTH" -H 'Content-Type: application/json' -d "$payload" > /tmp/so36b_bad.code
  assert_eq "$(cat /tmp/so36b_bad.code)" "400" "$label"
  head -c 200 /tmp/so36b_bad.json; echo
}
bad_code "ALL 却给了 scope_id" '{"rule_name":"坏规则","scope_type":"ALL","scope_id":"'"$M1"'","calc_type":"PERCENT","rate":"5"}'
bad_code "SALESPERSON 没给 scope_id" '{"rule_name":"坏规则","scope_type":"SALESPERSON","calc_type":"PERCENT","rate":"5"}'
bad_code "MATERIAL_CATEGORY 指向不存在的物料" '{"rule_name":"坏规则","scope_type":"MATERIAL_CATEGORY","scope_id":"e3600000-0000-4000-8000-00000000ffff","calc_type":"PERCENT","rate":"5","effective_from":"2030-01-01"}'
bad_code "PERCENT 的 rate=120" '{"rule_name":"坏规则","scope_type":"ALL","calc_type":"PERCENT","rate":"120"}'
bad_code "生效期倒挂" '{"rule_name":"坏规则","scope_type":"ALL","calc_type":"PERCENT","rate":"5","effective_from":"2030-06-01","effective_to":"2030-05-31"}'
bad_code "封顶小于起算额" '{"rule_name":"坏规则","scope_type":"ALL","calc_type":"PERCENT","rate":"5","min_amount":"100","max_amount":"50"}'
bad_code "status 乱码" '{"rule_name":"坏规则","scope_type":"ALL","calc_type":"PERCENT","rate":"5","status":"DELETED"}'
bad_code "rate 缺失" '{"rule_name":"坏规则","scope_type":"ALL","calc_type":"PERCENT"}'

say "C2 列表筛选乱码 → 400"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/business/erp/commission-rule/page?status=STOPPED" -H "$AUTH")" "400" "status 乱码"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/business/erp/commission-rule/page?scope_type=DEPARTMENT" -H "$AUTH")" "400" "scope_type 乱码"

say "C3 无 token → 401（权限不是只靠前端藏按钮）"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/business/erp/commission-rule/page")" "401" "列表无 token"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/business/erp/commission-rule" -H 'Content-Type: application/json' -d '{"rule_name":"x","scope_type":"ALL","calc_type":"PERCENT","rate":"5"}')" "401" "新建无 token"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' -X PUT "$BASE/api/business/erp/commission-rule/$ALL_ID" -H 'Content-Type: application/json' -d '{"status":"INACTIVE"}')" "401" "编辑无 token"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/api/business/erp/commission-rule/$ALL_ID")" "401" "删除无 token"

say "D 落库态核对（psql 直查，确认 4 位小数与审计列）"
runuser -u postgres -- psql -q -d "$DB" -c \
  "SELECT rule_name, scope_type, scope_id, calc_type, rate, min_amount, max_amount,
          effective_from, effective_to, status, create_by IS NOT NULL AS has_create_by,
          deleted_at IS NOT NULL AS deleted
     FROM biz_commission_rule ORDER BY rule_name"

say "结果"
if [ "$FAILED" -eq 0 ]; then echo "ALL CHECKS PASSED"; else echo "FAILED CHECKS: $FAILED"; exit 1; fi
