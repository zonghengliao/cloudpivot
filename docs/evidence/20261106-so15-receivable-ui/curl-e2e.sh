#!/usr/bin/env bash
# ============================================================================
# SO-15 应收台账前端页面 —— 真实 token 的接口实测（页面读的每个字段都断言一遍）
#
# 用法（先起一个指向**隔离演示库**的实例）：
#   BASE=http://127.0.0.1:18119/api DB=cloudpivot_so15ui \
#     bash docs/evidence/20261106-so15-receivable-ui/curl-e2e.sh
#
# 为什么前端卡要跑这个：`index.vue` 不自己算账，它读什么字段完全取决于这几个接口
# 的真实 JSON。字段名漂一个字母，`vue-tsc` 抓不到（编译期两边都是 string），
# 运行时就是空单元格。所以这里逐个断言页面用到的键确实存在且类型对。
#
# 链路：
#   POST /customer（credit_days=30）
#     → POST /receivable ×2（一笔 30 天前到期=逾期，一笔 10 天后到期=未到期）
#     → GET  /receivable/page      （列表 8 列 + 分页 + status/ar_no 过滤 + 非法状态 400）
#     → GET  /receivable/{id}      （详情：客户名 / unsettled / voucher_no 键存在）
#     → GET  /receivable/aging     （账龄：5 档表头 / 汇总 / 明细行）
#     → PATCH /receivable/{id}/write-off（原因过短 400 → 正常核销 → 重复核销 409）
#     → GET  /receivable/{id}      （remark 里的 `[坏账核销]` 留痕 = 前端「核销记录」的数据源）
#     → GET  /receivable/aging     （核销后不再计入应收资产）
#
# 脚本自带清场（只清 `SO15UI%` 前缀的客户与其应收），可连跑多轮。
# ============================================================================
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:18119/api}
DB=${DB:-cloudpivot_so15ui}
PGHOST=${PGHOST:-127.0.0.1}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}

CCODE='SO15UI-CUST'
TODAY=$(date +%Y-%m-%d)
DUE_OLD=$(date -d "$TODAY -30 days" +%Y-%m-%d)
DUE_NEW=$(date -d "$TODAY +10 days" +%Y-%m-%d)

PASS=0
FAIL=0
ok()    { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad()   { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 $3，实际 $2"; fi }
# 键必须存在（jq 的 `has` 认键名不认值；value 为 null 也算存在）
has()   { if [ "$2" = "true" ]; then ok "$1（键存在）"; else bad "$1：响应里没有这个键"; fi; }

psql_q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1"; }

echo "==> 0. 清场（只碰 SO15UI 前缀的夹具）"
psql_q "DELETE FROM biz_status_log WHERE entity_id IN
          (SELECT id FROM biz_receivable WHERE customer_id IN
             (SELECT id FROM biz_customer WHERE code LIKE 'SO15UI%'))" >/dev/null
psql_q "DELETE FROM biz_receivable WHERE customer_id IN
          (SELECT id FROM biz_customer WHERE code LIKE 'SO15UI%')" >/dev/null
psql_q "DELETE FROM biz_customer WHERE code LIKE 'SO15UI%'" >/dev/null

echo
echo "==> 1. 登录（admin）"
login() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" \
    "$BASE/system/auth/login"
}
RESP=""; TOKEN=""
for candidate in "$ADMIN_PASS" "${ADMIN_PASS}_so15"; do
  for attempt in 1 2 3; do
    RESP=$(login "$candidate")
    TOKEN=$(echo "$RESP" | jq -r '.data.access_token // empty')
    [ -n "$TOKEN" ] && break
    if [ "$(echo "$RESP" | jq -r '.code')" = "10005" ]; then sleep 25; else break; fi
  done
  if [ -n "$TOKEN" ]; then ADMIN_PASS="$candidate"; break; fi
done
if [ -z "$TOKEN" ]; then echo "  !! 登录失败：$RESP"; exit 1; fi
MUST=$(echo "$RESP" | jq -r '.data.must_change_password // false')
if [ "$MUST" = "true" ]; then
  NEW_PASS="${ADMIN_PASS}_so15"
  curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
    "$BASE/system/auth/password"
  ADMIN_PASS="$NEW_PASS"
  TOKEN=$(login "$ADMIN_PASS" | jq -r '.data.access_token // empty')
fi
[ -n "$TOKEN" ] && ok "拿到 admin access_token" || { bad "登录失败"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"
JSON='Content-Type: application/json'

echo
echo "==> 2. 建夹具客户 + 两笔应收（一笔逾期 30 天，一笔未到期）"
CJSON=$(curl -s -X POST -H "$AUTH" -H "$JSON" \
  --data-binary "{\"code\":\"$CCODE\",\"name\":\"SO-15 台账演示客户\",\"credit_days\":30}" \
  "$BASE/business/erp/customer")
CUST=$(echo "$CJSON" | jq -r '.data.id // empty')
[ -n "$CUST" ] && ok "建客户成功（credit_days=30）" || { bad "建客户失败：$CJSON"; exit 1; }

R1JSON=$(curl -s -X POST -H "$AUTH" -H "$JSON" \
  --data-binary "{\"customer_id\":\"$CUST\",\"amount\":1130,\"due_date\":\"$DUE_OLD\",\"remark\":\"SO-15 逾期演示\"}" \
  "$BASE/business/erp/receivable")
R1=$(echo "$R1JSON" | jq -r '.data.id // empty')
[ -n "$R1" ] && ok "手工挂账 #1（到期 $DUE_OLD，逾期 30 天）" || { bad "挂账 #1 失败：$R1JSON"; exit 1; }
R1NO=$(echo "$R1JSON" | jq -r '.data.ar_no')

R2JSON=$(curl -s -X POST -H "$AUTH" -H "$JSON" \
  --data-binary "{\"customer_id\":\"$CUST\",\"amount\":500,\"due_date\":\"$DUE_NEW\",\"remark\":\"SO-15 未到期演示\"}" \
  "$BASE/business/erp/receivable")
R2=$(echo "$R2JSON" | jq -r '.data.id // empty')
[ -n "$R2" ] && ok "手工挂账 #2（到期 $DUE_NEW，未到期）" || { bad "挂账 #2 失败：$R2JSON"; exit 1; }
R2NO=$(echo "$R2JSON" | jq -r '.data.ar_no')

echo
echo "==> 3. 列表：页面读的 8 列 + 分页 + 过滤（GET /receivable/page）"
PAGE=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?page=1&page_size=20&ar_no=SO15UI")
# ar_no 是模糊键，同时匹配来源单号；这里用客户反查更稳
PAGE=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?page=1&page_size=20&customer_id=$CUST")
check "分页 total = 2" "$(echo "$PAGE" | jq -r '.data.total')" "2"
check "分页 page = 1" "$(echo "$PAGE" | jq -r '.data.page')" "1"
check "分页 page_size = 20" "$(echo "$PAGE" | jq -r '.data.page_size')" "20"
ROW=$(echo "$PAGE" | jq -c '.data.list[0]')
has "列表行有 ar_no"        "$(echo "$ROW" | jq 'has("ar_no")')"
has "列表行有 customer_name" "$(echo "$ROW" | jq 'has("customer_name")')"
has "列表行有 source_no"     "$(echo "$ROW" | jq 'has("source_no")')"
has "列表行有 amount"        "$(echo "$ROW" | jq 'has("amount")')"
has "列表行有 received_amount"  "$(echo "$ROW" | jq 'has("received_amount")')"
has "列表行有 unsettled_amount" "$(echo "$ROW" | jq 'has("unsettled_amount")')"
has "列表行有 due_date"      "$(echo "$ROW" | jq 'has("due_date")')"
has "列表行有 status"        "$(echo "$ROW" | jq 'has("status")')"
has "列表行有 status_label"  "$(echo "$ROW" | jq 'has("status_label")')"
has "列表行有 overdue"       "$(echo "$ROW" | jq 'has("overdue")')"
check "客户名回填（LEFT JOIN biz_customer）" "$(echo "$ROW" | jq -r '.customer_name')" "SO-15 台账演示客户"
check "来源类型 = MANUAL（手工挂账）" "$(echo "$ROW" | jq -r '.source_type')" "MANUAL"
check "来源单号 = MANUAL（uk_biz_ar_source 的唯一例外）" "$(echo "$ROW" | jq -r '.source_no')" "MANUAL"
check "状态中文由后端下发" "$(echo "$ROW" | jq -r '.status_label')" "未收"

OVD=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?page=1&page_size=20&customer_id=$CUST&status=OPEN" \
  | jq -r '[.data.list[] | select(.ar_no=="'"$R1NO"'")][0].overdue')
check "30 天前到期的单据 overdue = true（列表红标的数据源）" "$OVD" "true"

NOD=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?page=1&page_size=20&customer_id=$CUST" \
  | jq -r '[.data.list[] | select(.ar_no=="'"$R2NO"'")][0].overdue')
check "10 天后到期的单据 overdue = false" "$NOD" "false"

# 前端的状态域必须是应收那 5 个值：传应付的 PARTIAL_PAID 后端 400（不是静默忽略）
BADST=$(curl -s -o /dev/null -w '%{http_code}' -H "$AUTH" \
  "$BASE/business/erp/receivable/page?page=1&page_size=20&status=PARTIAL_PAID")
check "传应付状态 PARTIAL_PAID → 400（前端状态域抄错会立刻暴露）" "$BADST" "400"

# 日期区间过滤（页面的「到期日 起~止」）
RANGE=$(curl -s -H "$AUTH" \
  "$BASE/business/erp/receivable/page?page=1&page_size=20&customer_id=$CUST&due_date_from=$DUE_OLD&due_date_to=$DUE_OLD" \
  | jq -r '.data.total')
check "到期日区间过滤 total = 1" "$RANGE" "1"

echo
echo "==> 4. 详情（GET /receivable/{id}）：金额 / 已收 / 未收 / voucher_no"
D1=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/$R1")
has "详情有 voucher_no（关联凭证号）" "$(echo "$D1" | jq '.data | has("voucher_no")')"
has "详情有 settled_at"              "$(echo "$D1" | jq '.data | has("settled_at")')"
has "详情有 credit_days"             "$(echo "$D1" | jq '.data | has("credit_days")')"
has "详情有 period_code"             "$(echo "$D1" | jq '.data | has("period_code")')"
check "详情 amount = 1130" "$(echo "$D1" | jq -r '.data.amount | tonumber | floor')" "1130"
check "详情 received_amount = 0" "$(echo "$D1" | jq -r '.data.received_amount | tonumber | floor')" "0"
check "详情 unsettled_amount = 1130" "$(echo "$D1" | jq -r '.data.unsettled_amount | tonumber | floor')" "1130"
check "详情凭证号为 null（收入确认凭证属 SO-22）" "$(echo "$D1" | jq -r '.data.voucher_no // "null"')" "null"

echo
echo "==> 5. 账龄看板（GET /receivable/aging）：5 档表头 + 汇总 + 明细"
AG=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/aging?as_of=$TODAY")
check "账龄基准日 = 传入的 as_of" "$(echo "$AG" | jq -r '.data.as_of')" "$TODAY"
check "as_of_source = QUERY（手动指定）" "$(echo "$AG" | jq -r '.data.as_of_source')" "QUERY"
check "档位数 = 5（未到期 / 1-30 / 31-60 / 61-90 / 90+）" "$(echo "$AG" | jq -r '.data.buckets | length')" "5"
check "档位 key 顺序" "$(echo "$AG" | jq -r '[.data.buckets[].key] | join(",")')" "NOT_DUE,D0_30,D31_60,D61_90,OVER_90"
check "汇总格与档位同长同序" "$(echo "$AG" | jq -r '[.data.summary[].key] | join(",")')" "NOT_DUE,D0_30,D31_60,D61_90,OVER_90"
CELL=$(echo "$AG" | jq -r '.data.summary[] | select(.key=="D0_30") | .unpaid')
check "逾期 30 天落在 D0_30 档，金额 1130" "$(echo "$CELL" | jq -r 'tonumber | floor')" "1130"
check "未到期档 500" "$(echo "$AG" | jq -r '.data.summary[] | select(.key=="NOT_DUE") | .unpaid | tonumber | floor')" "500"
has "账龄有 details（单据明细）" "$(echo "$AG" | jq '.data | has("details")')"
has "账龄有 overdue_ratio" "$(echo "$AG" | jq '.data | has("overdue_ratio")')"
check "明细行数 = 2" "$(echo "$AG" | jq -r '.data.detail_count')" "2"
check "明细行带 bucket_label（页面标签的数据源）" \
  "$(echo "$AG" | jq -r '[.data.details[] | select(.ar_no=="'"$R1NO"'")][0].bucket_label')" "1-30 天"

echo
echo "==> 6. 坏账核销（PATCH /receivable/{id}/write-off）"
SHORT=$(curl -s -X PATCH -H "$AUTH" -H "$JSON" --data-binary '{"reason":"短"}' \
  "$BASE/business/erp/receivable/$R1/write-off" | jq -r '.code')
check "原因过短被拒（前端 writeOffReasonError 同口径）" "$SHORT" "10001"

WO=$(curl -s -X PATCH -H "$AUTH" -H "$JSON" \
  --data-binary '{"reason":"SO-15 演示：客户已注销，经理批准核销"}' \
  "$BASE/business/erp/receivable/$R1/write-off")
check "核销后状态 = WRITTEN_OFF" "$(echo "$WO" | jq -r '.data.status')" "WRITTEN_OFF"
check "核销后状态中文 = 坏账核销" "$(echo "$WO" | jq -r '.data.status_label')" "坏账核销"
check "核销后 settled_at 非空" "$(echo "$WO" | jq -r 'if .data.settled_at == null then "null" else "set" end')" "set"
check "核销后金额不变（只改状态，不冲金额）" "$(echo "$WO" | jq -r '.data.amount | tonumber | floor')" "1130"

AGAIN=$(curl -s -X PATCH -H "$AUTH" -H "$JSON" --data-binary '{"reason":"再核销一次不该成功"}' \
  "$BASE/business/erp/receivable/$R1/write-off" | jq -r '.code')
check "重复核销返回业务错误（不是 200）" "$AGAIN" "10003"

echo
echo "==> 7. 核销记录留痕（前端「核销记录」面板的数据源 = remark 前缀）"
D2=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/$R1")
REMARK=$(echo "$D2" | jq -r '.data.remark')
case "$REMARK" in
  *"[坏账核销]"*) ok "remark 含 [坏账核销] 前缀（前端按这个前缀拆行）" ;;
  *) bad "remark 缺 [坏账核销] 前缀：$REMARK" ;;
esac
case "$REMARK" in
  *"客户已注销"*) ok "remark 保留了核销原因（可审计）" ;;
  *) bad "remark 丢掉原因：$REMARK" ;;
esac

echo
echo "==> 8. 核销后账龄不再计入应收资产"
AG2=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/aging?as_of=$TODAY")
check "核销后逾期 30 天档清零" "$(echo "$AG2" | jq -r '.data.summary[] | select(.key=="D0_30") | .unpaid | tonumber | floor')" "0"
check "核销后明细只剩 1 张" "$(echo "$AG2" | jq -r '.data.detail_count')" "1"

echo
echo "==> 9. 库层复核（页面之外的事实）"
check "biz_receivable 该客户 2 张（1 张已核销）" \
  "$(psql_q "SELECT count(*) FROM biz_receivable WHERE customer_id='$CUST'::uuid")" "2"
check "状态留痕写进 biz_status_log（RECEIVABLE / WRITTEN_OFF）" \
  "$(psql_q "SELECT count(*) FROM biz_status_log WHERE entity_id='$R1'::uuid AND to_status='WRITTEN_OFF'")" "1"
check "sys_menu 应收页已开（V0291 收尾）" \
  "$(psql_q "SELECT path || '|' || component || '|' || is_visible FROM sys_menu WHERE id='20000000-0000-0000-0000-000000002880'::uuid")" \
  "/business/erp/receivable|business/erp/receivable/index|true"
check "V0291 在 schema_migrations 里 versioned/executed" \
  "$(psql_q "SELECT kind || '|' || applied_via || '|' || success::text FROM schema_migrations WHERE version::text='291'")" \
  "versioned|executed|true"

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
