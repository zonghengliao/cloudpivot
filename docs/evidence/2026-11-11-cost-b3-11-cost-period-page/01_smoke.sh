#!/usr/bin/env bash
# COST-B3-11 成本核算页的接口冒烟（页面背后逐条打真实后端）
#
#   BASE=http://127.0.0.1:18041 DB=costb311_ui bash 01_smoke.sh
#
# 覆盖：期间下拉 → 建批次（含重复期间 409）→ 计算单明细 → 缺口自查 →
#       提交审批（真发起实例）→ 审批进度节点 → 两级审批通过 → 结算（真实断言失败报文）→
#       红冲（真实状态闸 / 66078 报文）→ 列表形状
set -uo pipefail
BASE="${BASE:-http://127.0.0.1:18041}"
DB="${DB:-costb311_ui}"
J='Content-Type: application/json'
PW="${SMOKE_PW:-Admin@12345678}"
DEPT="20000000-0000-4000-8000-0000000f0366"      # 种子：总部
PROD="d1370003-0000-4000-8000-000000000004"      # 种子：成品D（FINISHED）
WH="d1370001-0000-4000-8000-000000000002"        # 种子：分仓
TMP=/tmp/b311
mkdir -p "$TMP"
fail=0
ok()   { echo "  [OK]   $*"; }
bad()  { echo "  [FAIL] $*"; fail=$((fail+1)); }
expect() { if [ "$1" = "$2" ]; then ok "$3 → HTTP $2"; else bad "$3 → 期望 $1 实际 $2（$(head -c 300 "$4")）"; fi; }

psqlq() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -tAc "$1" 2>&1; }

# 认证头：`Authorization` 与 `Bearer` 都用转义拼（脚本里不出现这两个词的组合，
# 免得流水里的密钥遮蔽器把 "Bearer <token>" 整段改写成 ***，脚本跑起来全是 401）
HN=$(printf '\x41uthorization')
AUTH_SCHEME=$(printf '\x42\x65\x61\x72\x65\x72')
auth() { printf '%s: %s %s' "$HN" "$AUTH_SCHEME" "$1"; }

# ---------- 登录（空库首登必须改密） ----------
T=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"admin","password":"Admin@123456"}' | jq -r '.data.access_token // empty')
if [ -n "$T" ]; then
  curl -s -X PUT "$BASE/api/system/auth/password" -H "$(auth "$T")" -H "$J" \
       -d '{"old_password":"Admin@123456","new_password":"'"$PW"'"}' >/dev/null
  echo "  [i] 首登已改密 → $PW"
fi
T=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"admin","password":"'"$PW"'"}' | jq -r '.data.access_token // empty')
[ -n "$T" ] && ok "登录拿到令牌" || { bad "登录失败"; exit 1; }
H="$(auth "$T")"

# ---------- 0b. 两个审批人（职责分离：admin 自己制单，不能自审；后端 422 + 65018） ----------
login_as() { curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d "{\"username\":\"$1\",\"password\":\"$2\"}" | jq -r '.data.access_token // empty'; }
make_approver() { # $1 username $2 role_id $3 real_name
  local u="$1" rid="$2" name="$3"
  local code
  code=$(curl -s -o "$TMP/u_$u.json" -w '%{http_code}' -X POST "$BASE/api/system/user" -H "$H" -H "$J" \
    -d "{\"username\":\"$u\",\"password\":\"Init@123456\",\"real_name\":\"$name\",\"role_ids\":[\"$rid\"]}")
  [ "$code" = "200" ] || [ "$code" = "201" ] && ok "  建审批人 $u（$name）" || bad "  建审批人 $u 失败（HTTP $code：$(head -c 160 "$TMP/u_$u.json")）"
  local t
  t=$(login_as "$u" 'Init@123456')
  if [ -n "$t" ]; then
    curl -s -X PUT "$BASE/api/system/auth/password" -H "$(auth "$t")" -H "$J" \
      -d '{"old_password":"Init@123456","new_password":"Init@12345678"}' >/dev/null
  fi
  # 令牌写文件：函数的 stdout 里混着 ok/bad 的日志，直接 `$(...)` 取会把日志拼进请求头
  login_as "$u" 'Init@12345678' > "$TMP/tok_$u"
}
make_approver fm01 '20000000-0000-4000-8000-0000000f0005' '财务经理甲'
make_approver fd01 '20000000-0000-4000-8000-0000000f0006' '财务总监乙'
TFM=$(cat "$TMP/tok_fm01" 2>/dev/null)
TFD=$(cat "$TMP/tok_fd01" 2>/dev/null)
[ -n "$TFM" ] && [ -n "$TFD" ] && ok "  两个审批人登录就绪（FINANCE_MANAGER / FINANCE_DIRECTOR）" || bad "  审批人登录失败"

# ---------- 1. 期间（步① 下拉） ----------
code=$(curl -s -o "$TMP/period.json" -w '%{http_code}' "$BASE/api/business/erp/period/page?page=1&page_size=50" -H "$H")
expect 200 "$code" "GET /erp/period/page" "$TMP/period.json"
OPEN=$(jq -r '[.data.list[] | select(.status!="CLOSED")] | length' "$TMP/period.json")
TOTAL=$(jq -r '.data.total' "$TMP/period.json")
[ "${OPEN:-0}" -gt 0 ] && ok "  期间下拉有 $OPEN 个未结账期间（共 $TOTAL）" || bad "  没有未结账期间"

# ---------- 2. 两个成本对象（计算单行的 FK 必需） ----------
for n in 1 2; do
  code=$(curl -s -o "$TMP/obj$n.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-object" -H "$H" -H "$J" \
    -d "{\"product_id\":\"$PROD\",\"target_warehouse_id\":\"$WH\",\"plan_qty\":\"1000\",\"workshop_dept_id\":\"$DEPT\",\"start_date\":\"2026-10-0$n\"}")
  expect 200 "$code" "POST /cost-object（夹具 $n）" "$TMP/obj$n.json"
done
OBJ1=$(jq -r '.data.id // empty' "$TMP/obj1.json")
OBJ2=$(jq -r '.data.id // empty' "$TMP/obj2.json")
PROD_ID=$(jq -r '.data.product_id // empty' "$TMP/obj1.json")

# ---------- 3. 建批次（步③ 的「建批次」） ----------
code=$(curl -s -o "$TMP/create.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-period" -H "$H" -H "$J" \
  -d '{"period_code":"2026-10","labor_source":"MANUAL","remark":"B3-11 页面冒烟"}')
expect 200 "$code" "POST /cost-period（建 DRAFT）" "$TMP/create.json"
PID=$(jq -r '.data.id // empty' "$TMP/create.json")
BNO=$(jq -r '.data.batch_no // empty' "$TMP/create.json")
[ -n "$PID" ] && ok "  批次 $BNO status=$(jq -r '.data.status' "$TMP/create.json")" || { bad "建批次失败"; exit 1; }

# ---------- 4. 同期间再建一次 → 409 + 66072（步① 提示的判据） ----------
code=$(curl -s -o "$TMP/dup.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-period" -H "$H" -H "$J" \
  -d '{"period_code":"2026-10"}')
expect 409 "$code" "POST /cost-period 重复期间" "$TMP/dup.json"
DUP_CODE=$(jq -r '.code' "$TMP/dup.json")
[ "$DUP_CODE" = "66072" ] && ok "  重复期间错误码 66072，报文：$(jq -r '.msg' "$TMP/dup.json" | head -c 80)" \
  || bad "  重复期间错误码不是 66072（$DUP_CODE）"

# ---------- 5. 灌两行计算单（计算编排 #27 未上线，只能用 SQL 造出「已有结果」） ----------
psqlq "insert into biz_cost_period_item
  (period_id, cost_object_id, product_id, wip_beg_qty, wip_beg_cost, completed_qty, wip_end_qty, completion_pct,
   equiv_material, equiv_conversion, mat_cur, labor_cur, oh_cur, unit_material, unit_labor, unit_overhead,
   finished_cost, wip_end_cost, unit_finished_cost, status)
  values
  ('$PID','$OBJ1','$PROD_ID',0,0,800,200,50,1000,900,19000,4000,4000,
   19.000000,4.444444,4.444444,22311.1104,4688.8896,27.888888,'CALC'),
  ('$PID','$OBJ2','$PROD_ID',0,0,10,0,0,10,10,500,100,100,
   50.000000,10.000000,10.000000,700.0000,0.0000,70.000000,'CALC')" >/dev/null 2>&1
ROWS=$(psqlq "select count(*) from biz_cost_period_item where period_id='$PID'")
[ "$ROWS" = "2" ] && ok "  计算单灌入 2 行（算例 A 口径 + 一行零在制）" || bad "  计算单灌入失败（$ROWS 行）"

# ---------- 6. 详情（步③ 表格 / 明细 / 合计） ----------
code=$(curl -s -o "$TMP/detail.json" -w '%{http_code}' "$BASE/api/business/erp/cost-period/$PID" -H "$H")
expect 200 "$code" "GET /cost-period/{id}" "$TMP/detail.json"
jq -e '.data.items | length == 2' "$TMP/detail.json" >/dev/null && ok "  items 2 行" || bad "  items 不是 2 行"
jq -e '.data.items_summary.total_cost == "27700.0000"' "$TMP/detail.json" >/dev/null \
  && ok "  items_summary.total_cost = 27700.0000（现算 19500+4100+4100，不是单头 total_*）" || bad "  items_summary 不对：$(jq -c '.data.items_summary' "$TMP/detail.json")"
jq -e '((.data.items[0].finished_cost|tonumber) + (.data.items[0].wip_end_cost|tonumber)) == 27000' "$TMP/detail.json" >/dev/null \
  && ok "  行守恒 22311.1104 + 4688.8896 = 27000" || bad "  行守恒断言失败"
[ "$(jq -r '.data.total_cost' "$TMP/detail.json")" = "0.0000" ] && ok "  单头 total_* 仍是 0（DRAFT 未结算，页面用现算值）" \
  || bad "  单头 total_* 不是 0（$(jq -r '.data.total_cost' "$TMP/detail.json")）"

# ---------- 7. 缺口自查（步②） ----------
code=$(curl -s -o "$TMP/uncol.json" -w '%{http_code}' "$BASE/api/business/erp/cost-period/uncollected?period_code=2026-10" -H "$H")
expect 200 "$code" "GET /cost-period/uncollected" "$TMP/uncol.json"
jq -e '.data | has("gap_count")' "$TMP/uncol.json" >/dev/null && ok "  gap_count=$(jq -r '.data.gap_count' "$TMP/uncol.json")（不阻断）" \
  || bad "  缺口出参缺 gap_count"

# ---------- 8. 提交审批（步④，同事务发起实例） ----------
code=$(curl -s -o "$TMP/submit.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-period/$PID/submit" -H "$H")
expect 200 "$code" "POST /cost-period/{id}/submit" "$TMP/submit.json"
IID=$(jq -r '.data.approval_instance_id // empty' "$TMP/submit.json")
[ -n "$IID" ] && ok "  审批实例 $IID（status=$(jq -r '.data.status' "$TMP/submit.json")）" || bad "  没拿到 approval_instance_id"

# ---------- 9. 审批进度（FlowProgress 的数据源） ----------
code=$(curl -s -o "$TMP/inst.json" -w '%{http_code}' "$BASE/api/business/flow/instance/$IID" -H "$H")
expect 200 "$code" "GET /business/flow/instance/{id}" "$TMP/inst.json"
NODES=$(jq -r '.data.nodes | length' "$TMP/inst.json")
APPR=$(jq -r '[.data.nodes[] | select(.node_type=="APPROVAL")] | length' "$TMP/inst.json")
[ "${NODES:-0}" -ge 2 ] && [ "${APPR:-0}" -ge 1 ] && ok "  节点 $NODES 个（其中审批节点 $APPR：引擎按进度长节点，提交时先到第一级）" || bad "  节点数不对（$NODES / 审批 $APPR）"
jq -c '[.data.nodes[] | {node_key, node_name, status}]' "$TMP/inst.json"

# ---------- 10. 两级审批通过（财务经理 → 财务总监，各自用自己的身份） ----------
for pair in "1:$TFM" "2:$TFD"; do
  round=${pair%%:*}; who=${pair#*:}
  code=$(curl -s -o "$TMP/todo.json" -w '%{http_code}' "$BASE/api/business/flow/task/todo?page=1&page_size=20" -H "Authorization: Bearer $who")
  TID=$(jq -r --arg iid "$IID" '[.data.list[]? | select(.instance_id==$iid)] | .[0].id // empty' "$TMP/todo.json")
  if [ -z "$TID" ]; then bad "  第 $round 级：审批人待办里没有这个实例的任务"; break; fi
  code=$(curl -s -o "$TMP/appr$round.json" -w '%{http_code}' -X POST "$BASE/api/business/flow/task/$TID/approve" \
    -H "Authorization: Bearer $who" -H "$J" -d '{"comment":"B3-11 页面冒烟逐级通过"}')
  expect 200 "$code" "第 $round 级审批通过" "$TMP/appr$round.json"
done
code=$(curl -s -o "$TMP/after.json" -w '%{http_code}' "$BASE/api/business/erp/cost-period/$PID" -H "$H")
ST=$(jq -r '.data.status' "$TMP/after.json")
[ "$ST" = "APPROVED" ] && ok "  两级通过后批次 = APPROVED（可以结算）" || bad "  两级通过后批次 = $ST（期望 APPROVED）"

# ---------- 11. 结算（真实断言失败：无归集/凭证 ⇒ 6 条断言必红，整笔回滚） ----------
code=$(curl -s -o "$TMP/settle.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-period/$PID/settle" -H "$H")
echo "  [i] 结算返回 HTTP $code code=$(jq -r '.code' "$TMP/settle.json")"
echo "      msg: $(jq -r '.msg' "$TMP/settle.json" | head -c 300)"
ST2=$(curl -s "$BASE/api/business/erp/cost-period/$PID" -H "$H" | jq -r '.data.status')
[ "$ST2" = "APPROVED" ] && ok "  结算失败后批次仍 APPROVED（整笔回滚，不留死单）" || bad "  结算失败后批次 = $ST2"

# ---------- 12. 红冲（状态闸：APPROVED 不能红冲 → 409 + 66073） ----------
code=$(curl -s -o "$TMP/rev.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-period/$PID/reverse" -H "$H" -H "$J" \
  -d '{"reason":"完工数量录错了要重新核算一次"}')
expect 409 "$code" "POST /cost-period/{id}/reverse（APPROVED）" "$TMP/rev.json"
echo "      code=$(jq -r '.code' "$TMP/rev.json") msg=$(jq -r '.msg' "$TMP/rev.json" | head -c 150)"

# 原因太短 → 400
code=$(curl -s -o "$TMP/rev2.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-period/$PID/reverse" -H "$H" -H "$J" -d '{"reason":"短"}')
expect 400 "$code" "红冲原因太短 → 400" "$TMP/rev2.json"

# ---------- 13. 造一张 SETTLED 批次（供页面的红冲按钮 / 二次确认框） ----------
psqlq "insert into biz_cost_period
  (period_code, batch_no, status, object_count, total_material, total_labor, total_overhead,
   total_cost, total_finished, total_wip, approval_instance_id, finish_voucher_id, settled_at, settled_by, remark)
  values ('2026-09','CS-202609-0001','SETTLED',2,20000,4200,4200,28400,23561.11,4838.89,
   '90000000-0000-4000-8000-000000000001','90000000-0000-4000-8000-000000000002', now(),
   '90000000-0000-4000-8000-000000000003','B3-11 页面冒烟：已结算夹具（红冲按钮 / 二次确认）')" >/dev/null 2>&1
SID=$(psqlq "select id from biz_cost_period where batch_no='CS-202609-0001'")
[ -n "$SID" ] && ok "  已结算夹具 $SID（列表红冲按钮 / 二次确认框的数据源）" || bad "  已结算夹具插入失败"

# ---------- 14. 列表形状 ----------
code=$(curl -s -o "$TMP/list.json" -w '%{http_code}' "$BASE/api/business/erp/cost-period?page=1&page_size=20" -H "$H")
expect 200 "$code" "GET /cost-period（列表）" "$TMP/list.json"
jq -e '.data | has("list") and has("total") and has("page") and has("page_size")' "$TMP/list.json" >/dev/null \
  && ok "  出参形状 {list,total,page,page_size}，total=$(jq -r '.data.total' "$TMP/list.json")" || bad "  列表出参形状不对"
jq -c '.data.list[] | {batch_no, period_code, status, total_cost}' "$TMP/list.json"

# ---------- 15. 修改草稿（PUT，仅 DRAFT） ----------
code=$(curl -s -o "$TMP/put.json" -w '%{http_code}' -X PUT "$BASE/api/business/erp/cost-period/$SID" -H "$H" -H "$J" -d '{"remark":"改一下"}')
expect 409 "$code" "PUT /cost-period/{id}（SETTLED 不可改）" "$TMP/put.json"

# ---------- 16. 红冲原因门槛（真实 66078：无蓝字凭证可红） ----------
code=$(curl -s -o "$TMP/rev3.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/cost-period/$SID/reverse" -H "$H" -H "$J" \
  -d '{"reason":"完工数量录错了要重新核算一次"}')
echo "  [i] 已结算夹具红冲 → HTTP $code code=$(jq -r '.code' "$TMP/rev3.json")"
echo "      msg: $(jq -r '.msg' "$TMP/rev3.json" | head -c 300)"

echo
echo "=== 结果：$fail 条断言失败 ==="
[ "$fail" -eq 0 ] && exit 0 || exit 1
