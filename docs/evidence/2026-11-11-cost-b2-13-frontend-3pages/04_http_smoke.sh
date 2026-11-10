#!/usr/bin/env bash
# COST-B2-13 前端三页的接口冒烟（页面按钮背后逐条打一遍真实后端）
#
#   BASE=http://127.0.0.1:18031 bash 04_http_smoke.sh
#
# 覆盖：
#   1. 三个列表接口（工时 / 费用 / 分摊规则）→ 200 + 分页形状
#   2. 工时模板下载 → 200 + xlsx 字节
#   3. 工时「工资率留空」→ 422 + 66059（页面必须原样展示 + 补"怎么修"）
#   4. 工时「显式费率」→ 200，金额 = round(工时×费率, 4)
#   5. 费用台账「车间池」新增 → 200，cost_object_id 为 null（= 池行）
#   6. 分摊规则新增 → 200，basis_label 回填
#   7. 试算（只算不写）→ 200，Σ分摊额 == 池（守恒）
set -uo pipefail
BASE="${BASE:-http://127.0.0.1:18031}"
J='Content-Type: application/json'
PW="${SMOKE_PW:-Admin@12345678}"
DEPT="20000000-0000-4000-8000-0000000f0366"      # 种子：总部
PROD="d1370003-0000-4000-8000-000000000004"      # 种子：成品D（FINISHED）
WH="d1370001-0000-4000-8000-000000000002"        # 种子：分仓
fail=0
ok()   { echo "  [OK]   $*"; }
bad()  { echo "  [FAIL] $*"; fail=$((fail+1)); }
expect() { # expect <期望状态码> <实际状态码> <说明> <body-file>
  if [ "$1" = "$2" ]; then ok "$3 → HTTP $2"; else bad "$3 → 期望 $1 实际 $2（$(head -c 200 "$4")）"; fi
}

# ---------- 登录（空库首登必须改密，否则除改密外全 403） ----------
T=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"admin","password":"Admin@123456"}' | jq -r '.data.access_token // empty')
if [ -n "$T" ]; then
  curl -s -X PUT "$BASE/api/system/auth/password" -H "Authorization: Bearer $T" -H "$J" \
       -d '{"old_password":"Admin@123456","new_password":"'"$PW"'"}' >/dev/null
  echo "  [i] 首登已改密 → $PW"
fi
T=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"admin","password":"'"$PW"'"}' | jq -r '.data.access_token // empty')
[ -n "$T" ] && ok "登录拿到令牌" || { bad "登录失败"; exit 1; }
H="Authorization: Bearer $T"

# ---------- 1. 三个列表 ----------
for p in cost-labor cost-overhead cost-alloc-rule; do
  code=$(curl -s -o /tmp/b213_$p.json -w '%{http_code}' "$BASE/api/business/erp/$p?page=1&page_size=5" -H "$H")
  expect 200 "$code" "GET /$p" /tmp/b213_$p.json
  jq -e '.data | has("list") and has("total") and has("page") and has("page_size")' /tmp/b213_$p.json >/dev/null \
    && ok "  /$p 出参是 {list,total,page,page_size}" || bad "  /$p 出参形状不对"
done

# ---------- 2. 模板下载 ----------
code=$(curl -s -D /tmp/b213_tpl.h -o /tmp/b213_tpl.xlsx -w '%{http_code}' "$BASE/api/business/erp/cost-labor/template" -H "$H")
expect 200 "$code" "GET /cost-labor/template" /tmp/b213_tpl.h
grep -qi 'spreadsheetml' /tmp/b213_tpl.h && ok "  模板 content-type 是 xlsx（$(stat -c%s /tmp/b213_tpl.xlsx) 字节）" || bad "  模板不是 xlsx"

# ---------- 造一个在产成本对象（工时必需） ----------
code=$(curl -s -o /tmp/b213_obj.json -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-object" -H "$H" -H "$J" \
  -d "{\"product_id\":\"$PROD\",\"target_warehouse_id\":\"$WH\",\"plan_qty\":\"100\",\"workshop_dept_id\":\"$DEPT\",\"start_date\":\"2026-11-05\"}")
expect 200 "$code" "POST /cost-object（造夹具）" /tmp/b213_obj.json
OBJ=$(jq -r '.data.id // empty' /tmp/b213_obj.json)

# ---------- 3. 工时：工资率留空 → 422 + 66059 ----------
code=$(curl -s -o /tmp/b213_lb0.json -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-labor" -H "$H" -H "$J" \
  -d "{\"period_code\":\"2026-11\",\"work_date\":\"2026-11-05\",\"cost_object_id\":\"$OBJ\",\"employee_name\":\"张三\",\"dept_id\":\"$DEPT\",\"hours\":\"8\"}")
expect 422 "$code" "POST /cost-labor（rate 留空）" /tmp/b213_lb0.json
[ "$(jq -r '.code' /tmp/b213_lb0.json)" = "66059" ] && ok "  业务码 = 66059（费率三级全空，不按 0 计）" || bad "  业务码不是 66059"
jq -r '.msg' /tmp/b213_lb0.json | grep -q '怎么修' && ok "  报文含「怎么修」" || bad "  报文缺「怎么修」"

# ---------- 4. 工时：显式费率 → 200 + 金额 ----------
code=$(curl -s -o /tmp/b213_lb1.json -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-labor" -H "$H" -H "$J" \
  -d "{\"period_code\":\"2026-11\",\"work_date\":\"2026-11-05\",\"cost_object_id\":\"$OBJ\",\"employee_name\":\"张三\",\"dept_id\":\"$DEPT\",\"hours\":\"8\",\"rate\":\"50\",\"machine_hours\":\"2\"}")
expect 200 "$code" "POST /cost-labor（rate=50）" /tmp/b213_lb1.json
[ "$(jq -r '.data.amount' /tmp/b213_lb1.json)" = "400.0000" ] && ok "  金额 = 400.0000（8 × 50）" || bad "  金额不是 400.0000（$(jq -r '.data.amount' /tmp/b213_lb1.json)）"
LB=$(jq -r '.data.id' /tmp/b213_lb1.json)
code=$(curl -s -o /tmp/b213_lb2.json -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-labor/$LB/confirm" -H "$H")
expect 200 "$code" "POST /cost-labor/{id}/confirm" /tmp/b213_lb2.json
[ "$(jq -r '.data.status' /tmp/b213_lb2.json)" = "CONFIRMED" ] && ok "  状态 DRAFT → CONFIRMED" || bad "  状态没变"

# ---------- 5. 费用台账：车间池行 ----------
code=$(curl -s -o /tmp/b213_oh.json -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-overhead" -H "$H" -H "$J" \
  -d "{\"period_code\":\"2026-11\",\"dept_id\":\"$DEPT\",\"expense_item\":\"UTILITY\",\"credit_account_code\":\"1002\",\"amount\":\"1000.00\"}")
expect 200 "$code" "POST /cost-overhead（不挂对象）" /tmp/b213_oh.json
jq -e '.data.cost_object_id == null' /tmp/b213_oh.json >/dev/null && ok "  cost_object_id = null（= 车间费用池）" || bad "  不是池行"
[ "$(jq -r '.data.expense_item_label' /tmp/b213_oh.json)" = "水电费" ] && ok "  费用项目名来自字典（水电费）" || bad "  费用项目名不对"

# ---------- 6. 分摊规则 ----------
code=$(curl -s -o /tmp/b213_rule.json -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-alloc-rule" -H "$H" -H "$J" \
  -d "{\"rule_code\":\"smoke_utility\",\"rule_name\":\"冒烟-水电费按工时分摊\",\"dept_id\":\"$DEPT\",\"account_code\":\"4101.03\",\"basis\":\"LABOR_HOURS\",\"effective_from\":\"2026-11-01\"}")
expect 200 "$code" "POST /cost-alloc-rule（rule_code 小写）" /tmp/b213_rule.json
[ "$(jq -r '.data.rule_code' /tmp/b213_rule.json)" = "SMOKE_UTILITY" ] && ok "  rule_code 归一为大写 SMOKE_UTILITY" || bad "  rule_code 没归一"
[ "$(jq -r '.data.basis_label' /tmp/b213_rule.json)" = "按人工工时分摊" ] && ok "  basis_label 回填" || bad "  basis_label 不对"

# ---------- 7. 试算（只算不写） ----------
code=$(curl -s -o /tmp/b213_pv.json -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-alloc-rule/preview" -H "$H" -H "$J" -d '{"period_code":"2026-11"}')
expect 200 "$code" "POST /cost-alloc-rule/preview" /tmp/b213_pv.json
if [ "$(jq -r '.code' /tmp/b213_pv.json)" = "0" ]; then
  pool=$(jq -r '.data.total_pool_amount' /tmp/b213_pv.json)
  alloc=$(jq -r '.data.total_alloc_amount' /tmp/b213_pv.json)
  [ "$pool" = "$alloc" ] && ok "  守恒：池 $pool == 分摊 $alloc" || bad "  不守恒：池 $pool ≠ 分摊 $alloc"
  echo "  [i] 组数 $(jq -r '.data.groups|length' /tmp/b213_pv.json) / 未分摊 $(jq -r '.data.unallocated|length' /tmp/b213_pv.json)（本期未跑 calc，池为 0 属预期）"
fi

echo
if [ "$fail" -eq 0 ]; then echo "ALL SMOKE CHECKS PASSED"; else echo "$fail CHECK(S) FAILED"; fi
exit "$fail"
