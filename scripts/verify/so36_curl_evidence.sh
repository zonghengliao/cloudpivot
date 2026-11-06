#!/usr/bin/env bash
# SO-36 佣金计提的真实 curl 留证（真实 token，库 cloudpivot_so36 @ :18243）
#
#   runuser -u postgres -- createdb -O alun -T cloudpivot_so35 cloudpivot_so36
#   bash scripts/verify/so36_instance.sh up
#   bash scripts/verify/so36_curl_evidence.sh | tee docs/evidence/20261107-so36-commission/curl_transcript.txt
#
# 覆盖（每条都带断言，断言失败脚本退出码非 0）：
#   ① 登录拿真实 token
#   ② 夹具就位（订单/发货/出库/应收/规则；SQL 见 so36_fixture.sql）
#   ③ POST /commission/settle（不给 salesperson_id → 批跑所有有数据的销售员）
#      · SP1：AR-A 已收 400 → 基数 400、佣金 80（销售员 20% 压过物料 10% 与全局 5%）
#      · SP2：AR-B 已收 1000 → 基数 1000、佣金 60（物料级 10% 被 max_amount=60 封顶）
#      · SO-C 一分未收 → 不出明细（按已收款计佣）
#   ④ 重算幂等：同期间再跑 → 已审批的拒绝 / 无新增数据 → skipped，不重复发钱
#   ⑤ 非法状态迁移 DRAFT → PAID → 409
#   ⑥ PATCH status=APPROVED → 计提凭证（借 5601 / 贷 2241.02，金额 = 佣金合计）
#   ⑦ PATCH status=PAID → 付款凭证（借 2241.02 / 贷 1002）+ paid_at
#   ⑧ 退货 / 退款冲回：AR-B 已收 1000 → 600，下一期间计提 −400 基数 / −40 佣金
#   ⑨ GET /commission/page 列表 + 数据权限（真实 principal）
#   ⑩ 负路径：period_code 乱码 400 / status 乱码 400 / 无 token 401
set -uo pipefail
BASE=${SO36_BASE:-http://127.0.0.1:18243}
DB=${SO36_DB:-cloudpivot_so36}
SP1=e3600000-0000-4000-8000-000000000011
SP2=e3600000-0000-4000-8000-000000000012
FAILED=0

say() { printf '\n### %s\n' "$1"; }
fail() { echo "❌ ASSERT FAIL: $1"; FAILED=$((FAILED + 1)); }
# assert_eq <实际> <期望> <说明>
assert_eq() {
  if [ "$1" = "$2" ]; then echo "✅ $3 = $1"; else fail "$3 期望 [$2] 实际 [$1]"; fi
}
json() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)"; }

say "① 夹具就位（psql 直连隔离库；上游销售链已由 SO-04/10/14 各自的卡验证）"
runuser -u postgres -- psql -q -d "$DB" -f "$(dirname "$0")/so36_fixture.sql" | tail -6

say "② 登录拿 token（admin / SUPER_ADMIN 短路权限）"
TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' \
  | json 'd["data"]["access_token"]')
echo "token 长度 = ${#TOKEN}"
[ -n "$TOKEN" ] || { echo "登录失败"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

say "③ 生成 2026-11 结算单（POST /api/business/erp/commission/settle）"
SETTLE=$(curl -s -X POST "$BASE/api/business/erp/commission/settle" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"period_code":"2026-11","remark":"SO-36 计提留证"}')
echo "$SETTLE" | python3 -m json.tool | head -60
assert_eq "$(echo "$SETTLE" | json 'len(d["data"]["settlements"])')" "2" "结算单张数（SP1 + SP2）"
assert_eq "$(echo "$SETTLE" | json 'd["data"]["period_code"]')" "2026-11" "期间"

SP1_ID=$(echo "$SETTLE" | json 'next(s["id"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')
SP2_ID=$(echo "$SETTLE" | json 'next(s["id"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP2"'")')
SP1_NO=$(echo "$SETTLE" | json 'next(s["settlement_no"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')
SP2_NO=$(echo "$SETTLE" | json 'next(s["settlement_no"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP2"'")')
echo "SP1 结算单 = $SP1_NO ($SP1_ID)"
echo "SP2 结算单 = $SP2_NO ($SP2_ID)"

# --- SP1：基数 400 / 佣金 80（20% 优先于物料 10%、全局 5%）---
assert_eq "$(echo "$SETTLE" | json 'next(s["total_sales"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')" "400.0000" "SP1 佣金基数（已收 400，未收 600 不计）"
assert_eq "$(echo "$SETTLE" | json 'next(s["total_commission"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')" "80.0000" "SP1 佣金（400 × 20%）"
assert_eq "$(echo "$SETTLE" | json 'next(s["status"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP1"'")')" "DRAFT" "SP1 初始状态"
# --- SP2：基数 1000 / 佣金 60（物料 10% = 100 → 封顶 60）---
assert_eq "$(echo "$SETTLE" | json 'next(s["total_sales"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP2"'")')" "1000.0000" "SP2 佣金基数"
assert_eq "$(echo "$SETTLE" | json 'next(s["total_commission"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP2"'")')" "60.0000" "SP2 佣金（10% 封顶 60）"

say "③b 明细核对（GET /api/business/erp/commission/{id}）"
D1=$(curl -s "$BASE/api/business/erp/commission/$SP1_ID" -H "$AUTH")
echo "$D1" | python3 -c 'import json,sys;d=json.load(sys.stdin)["data"];[print({k:i[k] for k in ("ar_no","so_no","base_amount","rate","commission","rule_name")}) for i in d["items"]]'
assert_eq "$(echo "$D1" | json 'len(d["data"]["items"])')" "2" "SP1 明细行数（M1 + M2 两个物料桶）"
assert_eq "$(echo "$D1" | json 'sorted(i["ar_no"] for i in d["data"]["items"])')" "['__SO36-AR-A__', '__SO36-AR-A__']" "SP1 明细只落在 AR-A（AR-C 未收款不出现）"
assert_eq "$(echo "$D1" | json 'sorted(i["base_amount"] for i in d["data"]["items"])')" "['160.0000', '240.0000']" "SP1 基数按订单行金额 6:4 分摊（1000 中已收 400）"
assert_eq "$(echo "$D1" | json 'sorted(i["commission"] for i in d["data"]["items"])')" "['32.0000', '48.0000']" "SP1 佣金按 20% 计"
assert_eq "$(echo "$D1" | json 'all(i["rule_name"]=="SO36 销售员甲 20%" for i in d["data"]["items"])')" "True" "命中的是销售员级规则"

say "④ 非法状态迁移 DRAFT → PAID → 409（凭证与单据不能脱钩）"
CODE=$(curl -s -o /tmp/so36_skip.json -w '%{http_code}' -X PATCH "$BASE/api/business/erp/commission/$SP1_ID/status" \
  -H "$AUTH" -H 'Content-Type: application/json' -d '{"status":"PAID"}')
assert_eq "$CODE" "409" "DRAFT → PAID 的 HTTP 码"
head -c 300 /tmp/so36_skip.json; echo

say "⑤ 审批 SP1 → 计提凭证（PATCH status=APPROVED）"
AP1=$(curl -s -X PATCH "$BASE/api/business/erp/commission/$SP1_ID/status" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"status":"APPROVED","reason":"2026-11 佣金审批"}')
echo "$AP1" | python3 -c 'import json,sys;d=json.load(sys.stdin)["data"];print({k:d[k] for k in ("settlement_no","status","total_commission","voucher_id","voucher_no")})'
assert_eq "$(echo "$AP1" | json 'd["data"]["status"]')" "APPROVED" "SP1 审批后状态"
V1=$(echo "$AP1" | json 'd["data"]["voucher_id"]')
[ -n "$V1" ] && [ "$V1" != "None" ] && echo "✅ 计提凭证 id = $V1" || fail "审批后没有生成计提凭证"

say "⑤b 凭证分录核对（库里真账：借 5601 / 贷 2241.02，金额 = 80.00）"
runuser -u postgres -- psql -d "$DB" -c \
  "SELECT v.voucher_no, v.source_type, v.source_no, e.line_no, e.account_code, e.summary, e.debit_amount, e.credit_amount
     FROM biz_voucher v JOIN biz_voucher_entry e ON e.voucher_id = v.id
    WHERE v.id = '$V1'::uuid ORDER BY e.line_no"
assert_eq "$(runuser -u postgres -- psql -tA -d "$DB" -c "SELECT count(*) FROM biz_voucher_entry WHERE voucher_id='$V1'::uuid AND account_code='5601' AND debit_amount=80")" "1" "借方 5601 销售费用 80"
assert_eq "$(runuser -u postgres -- psql -tA -d "$DB" -c "SELECT count(*) FROM biz_voucher_entry WHERE voucher_id='$V1'::uuid AND account_code='2241.02' AND credit_amount=80")" "1" "贷方 2241.02 其他应付款-佣金 80"
assert_eq "$(runuser -u postgres -- psql -tA -d "$DB" -c "SELECT sign(sum(debit_amount)-sum(credit_amount))::int FROM biz_voucher_entry WHERE voucher_id='$V1'::uuid")" "0" "凭证借贷平衡"

say "⑥ 审批 SP2（封顶 60）→ 凭证金额 60"
AP2=$(curl -s -X PATCH "$BASE/api/business/erp/commission/$SP2_ID/status" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"status":"APPROVED"}')
V2=$(echo "$AP2" | json 'd["data"]["voucher_id"]')
assert_eq "$(runuser -u postgres -- psql -tA -d "$DB" -c "SELECT debit_amount::text FROM biz_voucher_entry WHERE voucher_id='$V2'::uuid AND account_code='5601'")" "60.0000" "SP2 计提凭证借方金额（封顶生效）"

say "⑦ 发放 SP1 → 付款凭证（借 2241.02 / 贷 1002）"
PAY=$(curl -s -X PATCH "$BASE/api/business/erp/commission/$SP1_ID/status" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"status":"PAID","reason":"银行转账发放"}')
echo "$PAY" | python3 -c 'import json,sys;d=json.load(sys.stdin)["data"];print({k:d[k] for k in ("settlement_no","status","paid_at")})'
assert_eq "$(echo "$PAY" | json 'd["data"]["status"]')" "PAID" "SP1 发放后状态"
assert_eq "$(echo "$PAY" | json 'd["data"]["paid_at"] is not None')" "True" "发放时间已落库（PAID ⇔ paid_at 非空）"
assert_eq "$(runuser -u postgres -- psql -tA -d "$DB" -c "SELECT count(*) FROM biz_voucher v JOIN biz_voucher_entry e ON e.voucher_id=v.id WHERE v.source_type='COMMISSION' AND v.source_no='$SP1_NO' AND e.account_code='2241.02' AND e.debit_amount=80")" "1" "付款凭证借 2241.02 80（冲平应付佣金）"
assert_eq "$(runuser -u postgres -- psql -tA -d "$DB" -c "SELECT count(*) FROM biz_voucher v JOIN biz_voucher_entry e ON e.voucher_id=v.id WHERE v.source_type='COMMISSION' AND v.source_no='$SP1_NO' AND e.account_code='1002' AND e.credit_amount=80")" "1" "付款凭证贷 1002 银行存款 80"

say "⑧ 重算幂等：同期间再跑 → 已审批的拒绝重算（不重复发钱）"
AGAIN=$(curl -s -X POST "$BASE/api/business/erp/commission/settle" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"period_code":"2026-11"}')
echo "$AGAIN" | python3 -m json.tool | head -30
assert_eq "$(echo "$AGAIN" | json 'len(d["data"]["settlements"])')" "0" "无新建结算单"
assert_eq "$(echo "$AGAIN" | json 'len(d["data"]["skipped"])')" "2" "两个销售员都被跳过"
assert_eq "$(echo "$AGAIN" | json 'all("已" in s["reason"] for s in d["data"]["skipped"])')" "True" "跳过原因是「已审批入账不能重算」"

say "⑨ 退货 / 退款冲回：AR-B 已收 1000 → 600（模拟收款退款单把已收打回），下一期间计提 −40"
runuser -u postgres -- psql -q -d "$DB" -c \
  "UPDATE biz_receivable SET received_amount = 600, unsettled_amount = 400, status='PARTIAL', settled_at=NULL
    WHERE ar_no = '__SO36-AR-B__'"
DEC=$(curl -s -X POST "$BASE/api/business/erp/commission/settle" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"period_code":"2026-12"}')
echo "$DEC" | python3 -m json.tool | head -40
assert_eq "$(echo "$DEC" | json 'next(s["total_commission"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP2"'")')" "-40.0000" "冲回佣金 −40（−400 × 10%）"
assert_eq "$(echo "$DEC" | json 'next(s["total_sales"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP2"'")')" "0.0000" "纯冲回期间 total_sales 按 0 兜底（库层 CHECK）"
DEC_ID=$(echo "$DEC" | json 'next(s["id"] for s in d["data"]["settlements"] if s["salesperson_id"]=="'"$SP2"'")')
curl -s "$BASE/api/business/erp/commission/$DEC_ID" -H "$AUTH" \
  | python3 -c 'import json,sys;d=json.load(sys.stdin)["data"];print([{k:i[k] for k in ("base_amount","rate","commission","remark")} for i in d["items"]])'

say "⑩ 分页（GET /api/business/erp/commission/page，数据权限走 scope::paginate）"
curl -s "$BASE/api/business/erp/commission/page?page=1&page_size=10" -H "$AUTH" \
  | python3 -c 'import json,sys;d=json.load(sys.stdin)["data"];print({"total":d["total"],"rows":[{k:r[k] for k in ("settlement_no","period_code","status","total_commission")} for r in d["list"]]})'
assert_eq "$(curl -s "$BASE/api/business/erp/commission/page?period_code=2026-11&page=1&page_size=10" -H "$AUTH" | json 'd["data"]["total"]')" "2" "2026-11 期间结算单数"
assert_eq "$(curl -s "$BASE/api/business/erp/commission/page?status=PAID&page=1&page_size=10" -H "$AUTH" | json 'd["data"]["total"]')" "1" "PAID 状态筛选"
assert_eq "$(curl -s "$BASE/api/business/erp/commission/page?keyword=CM-202611&page=1&page_size=10" -H "$AUTH" | json 'd["data"]["total"]')" "2" "关键字（单号）筛选"

say "⑪ 负路径：period_code 乱码 → 400"
CODE=$(curl -s -o /tmp/so36_badperiod.json -w '%{http_code}' -X POST "$BASE/api/business/erp/commission/settle" \
  -H "$AUTH" -H 'Content-Type: application/json' -d '{"period_code":"2026-13"}')
assert_eq "$CODE" "400" "非法期间 HTTP 码"
head -c 200 /tmp/so36_badperiod.json; echo

say "⑫ 负路径：status 乱码 → 400"
CODE=$(curl -s -o /tmp/so36_badstatus.json -w '%{http_code}' -X PATCH "$BASE/api/business/erp/commission/$SP1_ID/status" \
  -H "$AUTH" -H 'Content-Type: application/json' -d '{"status":"CANCELLED"}')
assert_eq "$CODE" "400" "非法状态 HTTP 码"
head -c 200 /tmp/so36_badstatus.json; echo

say "⑬ 无 token → 401（权限不是只靠前端藏按钮）"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/business/erp/commission/page?page=1&page_size=1")" "401" "列表无 token"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/business/erp/commission/settle" -H 'Content-Type: application/json' -d '{}')" "401" "计提无 token"

say "结果"
if [ "$FAILED" -eq 0 ]; then echo "ALL CHECKS PASSED"; else echo "FAILED CHECKS: $FAILED"; exit 1; fi
