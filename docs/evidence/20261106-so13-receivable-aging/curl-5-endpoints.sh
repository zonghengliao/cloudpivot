#!/usr/bin/env bash
# ============================================================================
# SO-13 应收后端 API 的 5 接口 curl 实测 + 账龄与应付同口径比对
#
# 用法（先起一个指向**隔离演示库**的实例）：
#   BASE=http://127.0.0.1:18114/api bash docs/evidence/20261106-so13-receivable-aging/curl-5-endpoints.sh
#
# 依赖：curl / jq / psql（默认 alun@127.0.0.1:5432/cloudpivot_so13）
#
# ⚠ 本脚本会**清空**演示库的 biz_receivable / biz_payable 两张台账（账龄报表是全库口径，
#   只有表里没有别人的夹具，才能做「应收 vs 应付各档金额逐个相同」的严格比对）。
#   只对一次性隔离库运行。
#
# 断言分组：
#   ① 账龄同口径（先做：此时库里只有 8 应收 + 8 应付、到期日一一对应）
#   ② 手工挂账 POST /receivable
#   ③ 详情 GET /receivable/{id}
#   ④ 分页 GET /receivable/page（含 payable 域值被拒）
#   ⑤ 账龄 GET /receivable/aging
#   ⑥ 坏账核销 PATCH /receivable/{id}/write-off
#   ⑦ 反向用例（409 / 400 / 404 / 66006）
#   ⑧ 库层留痕 biz_status_log
# ============================================================================
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:18114/api}
DB=${DB:-cloudpivot_so13}
PGHOST=${PGHOST:-127.0.0.1}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}

CUSTOMER='de130000-0000-4000-8000-000000000001'
SUPPLIER='de130000-0000-4000-8000-0000000000f1'
AS_OF='2026-11-05'

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 $3，实际 $2"; fi; }

psql_q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1"; }

echo "==> 0. 夹具（清空两张台账后插入 8 应收 + 8 应付，到期日一一对应）"
psql_q "DELETE FROM biz_receivable" >/dev/null
psql_q "DELETE FROM biz_payable" >/dev/null
psql_q "DELETE FROM biz_customer WHERE id = '$CUSTOMER'::uuid" >/dev/null
psql_q "DELETE FROM biz_supplier WHERE id = '$SUPPLIER'::uuid" >/dev/null
psql_q "INSERT INTO biz_customer (id, code, name, status)
        VALUES ('$CUSTOMER'::uuid, 'ARDEMO-01', '应收演示客户', 'ACTIVE')" >/dev/null
psql_q "INSERT INTO biz_supplier (id, supplier_code, supplier_name, credit_days)
        VALUES ('$SUPPLIER'::uuid, 'ARDEMO-S1', '应收演示供应商', 30)" >/dev/null

# 逾期天数 0 / 1 / 30 / 31 / 60 / 61 / 90 / 91：每个档位边界各一张，应收应付同值
pairs="1:2026-11-05 2:2026-11-04 3:2026-10-06 4:2026-10-05 \
       5:2026-09-06 6:2026-09-05 7:2026-08-07 8:2026-08-06"
for pair in $pairs; do
  n=${pair%%:*}; due=${pair##*:}; tag="a$n"
  psql_q "INSERT INTO biz_receivable
            (id, ar_no, customer_id, source_type, source_id, source_no, currency_code,
             amount, received_amount, unsettled_amount, credit_days, due_date, period_code, status)
          VALUES ('de130000-0000-4000-8000-0000000000$tag'::uuid, 'ARDEMO-$n',
                  '$CUSTOMER'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
                  100, 0, 100, 30, '$due'::date, to_char('$due'::date, 'YYYY-MM'), 'OPEN')" >/dev/null
  psql_q "INSERT INTO biz_payable
            (id, payable_no, supplier_id, source_type, source_id, source_no, currency_code,
             amount, paid_amount, unpaid_amount, credit_days, due_date, period_code, status)
          VALUES ('de130000-0000-4000-8000-0000000000b$n'::uuid, 'APDEMO-$n',
                  '$SUPPLIER'::uuid, 'STOCK_IN', gen_random_uuid(), 'IN-DEMO-$n', 'CNY',
                  100, 0, 100, 30, '$due'::date, to_char('$due'::date, 'YYYY-MM'), 'OPEN')" >/dev/null
done
echo "      应收 8 张 / 应付 8 张（到期日 2026-11-05 …… 2026-08-06）"

echo
echo "==> 1. 登录（首登强制改密会自动处理）"
login() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" \
    "$BASE/system/auth/login"
}
RESP=""
TOKEN=""
# 首登强制改密会把口令换成 ${ADMIN_PASS}_ar13（下面那段），所以第二轮起要能登进新口令
for candidate in "$ADMIN_PASS" "${ADMIN_PASS}_ar13"; do
  RESP=$(login "$candidate")
  TOKEN=$(echo "$RESP" | jq -r '.data.access_token // empty')
  if [ -n "$TOKEN" ]; then ADMIN_PASS="$candidate"; break; fi
done
if [ -z "$TOKEN" ]; then echo "  !! 登录失败：$RESP"; exit 1; fi
MUST=$(echo "$RESP" | jq -r '.data.must_change_password // false')
if [ "$MUST" = "true" ]; then
  NEW_PASS="${ADMIN_PASS}_ar13"
  curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
    "$BASE/system/auth/password"
  ADMIN_PASS="$NEW_PASS"
  TOKEN=$(login "$ADMIN_PASS" | jq -r '.data.access_token // empty')
  [ -n "$TOKEN" ] && ok "首登改密后重新登录" || { bad "改密后登录失败"; exit 1; }
fi
[ -n "$TOKEN" ] && ok "拿到 access_token（admin）"
AUTH="Authorization: Bearer $TOKEN"

echo
echo "==> 2. 账龄同口径（应收 vs 应付，库内只有上面 8+8 张）"
RJSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/aging?as_of=$AS_OF")
PJSON=$(curl -s -H "$AUTH" "$BASE/business/erp/report/payable-aging?as_of=$AS_OF")
check "应收账龄 code" "$(echo "$RJSON" | jq -r '.code')" "0"
check "应付账龄 code" "$(echo "$PJSON" | jq -r '.code')" "0"
check "报表编码" "$(echo "$RJSON" | jq -r '.data.report_code')" "RECEIVABLE_AGING"
check "档位键与应付逐字相同" \
  "$(echo "$RJSON" | jq -r '[.data.buckets[].key] | join(",")')" \
  "$(echo "$PJSON" | jq -r '[.data.buckets[].key] | join(",")')"
RSUM=$(echo "$RJSON" | jq -c '[.data.summary[] | {key, unpaid}]')
PSUM=$(echo "$PJSON" | jq -c '[.data.summary[] | {key, unpaid}]')
echo "      应收各档：$RSUM"
echo "      应付各档：$PSUM"
check "各档金额逐个相同" "$RSUM" "$PSUM"
check "应收未收合计" "$(echo "$RJSON" | jq -r '.data.total_unsettled')" "800.0000"
check "应收逾期合计（除未到期）" "$(echo "$RJSON" | jq -r '.data.overdue_unsettled')" "700.0000"
# 明细侧（Rust 现算档位）与汇总侧（SQL CASE 聚合）必须逐档相等 —— 分档同源的硬证明
JQ_BUCKET_PROOF='
def bof(d): if d <= 0 then "NOT_DUE" elif d <= 30 then "D0_30"
            elif d <= 60 then "D31_60" elif d <= 90 then "D61_90" else "OVER_90" end;
([.data.details[] | {k: bof(.overdue_days), v: (.unsettled_amount | tonumber)}]
  | group_by(.k) | map({key: .[0].k, unpaid: (map(.v) | add)}) | sort_by(.key))
== ([.data.summary[] | {key, unpaid: (.unpaid | tonumber)}] | sort_by(.key))'
check "明细档位聚合 == 汇总档位（Rust/SQL 同源）" \
  "$(echo "$RJSON" | jq -r "$JQ_BUCKET_PROOF")" "true"
check "明细按到期日升序" \
  "$(echo "$RJSON" | jq -r '[.data.details[].due_date] as $d | $d == ($d | sort)')" "true"

echo
echo "==> 3. POST /business/erp/receivable（手工挂账）"
BODY=$(curl -s -w '\n%{http_code}' -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUSTOMER\",\"amount\":1234.56,\"credit_days\":30,
                  \"remark\":\"SO13 curl 演示：手工挂账\"}" \
  "$BASE/business/erp/receivable")
CODE=$(echo "$BODY" | tail -1); JSON=$(echo "$BODY" | sed '$d')
AR_ID=$(echo "$JSON" | jq -r '.data.id // empty')
AR_NO=$(echo "$JSON" | jq -r '.data.ar_no // empty')
check "HTTP 状态" "$CODE" "200"
check "响应 code" "$(echo "$JSON" | jq -r '.code')" "0"
check "状态" "$(echo "$JSON" | jq -r '.data.status')" "OPEN"
check "未收 = 应收（恒等式）" "$(echo "$JSON" | jq -r '.data.unsettled_amount')" "1234.56"
check "账期快照" "$(echo "$JSON" | jq -r '.data.credit_days')" "30"
case "$AR_NO" in
  AR-[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-*) ok "单号格式 AR-yyyyMMdd-000001（$AR_NO）" ;;
  *) bad "单号格式不对：$AR_NO" ;;
esac

echo
echo "==> 4. GET /business/erp/receivable/{id}（详情）"
JSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/$AR_ID")
check "HTTP code 字段" "$(echo "$JSON" | jq -r '.code')" "0"
check "详情单号回显" "$(echo "$JSON" | jq -r '.data.ar_no')" "$AR_NO"
check "客户名 JOIN" "$(echo "$JSON" | jq -r '.data.customer_name')" "应收演示客户"
check "状态标签" "$(echo "$JSON" | jq -r '.data.status_label')" "未收"
check "凭证号（手工挂账无凭证）" "$(echo "$JSON" | jq -r '.data.voucher_no // "null"')" "null"

echo
echo "==> 5. GET /business/erp/receivable/page（分页 + 状态筛选）"
JSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?page=1&page_size=5&status=OPEN")
check "HTTP code 字段" "$(echo "$JSON" | jq -r '.code')" "0"
TOTAL=$(echo "$JSON" | jq -r '.data.total')
INPAGE=$(echo "$JSON" | jq -r '.data.list | length')
[ "$TOTAL" -ge 1 ] && ok "总数 total=$TOTAL（>=1）" || bad "总数异常：$TOTAL"
[ "$INPAGE" -le 5 ] && ok "page_size=5 生效（本页 $INPAGE 条）" || bad "page_size 未生效：$INPAGE"
check "全部 OPEN" "$(echo "$JSON" | jq -r '[.data.list[].status] | unique | join(",")')" "OPEN"
JSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?status=PARTIAL_PAID")
check "payable 域值被拒" "$(echo "$JSON" | jq -r '.code')" "10001"

echo
echo "==> 6. GET /business/erp/receivable/aging（挂账后再看一次，未到期档 +1234.56）"
JSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/aging?as_of=$AS_OF")
check "HTTP code 字段" "$(echo "$JSON" | jq -r '.code')" "0"
check "未到期档 = 100 + 1234.56" "$(echo "$JSON" | jq -r '.data.summary[0].unpaid')" "1334.5600"
check "基准日来源" "$(echo "$JSON" | jq -r '.data.as_of_source')" "QUERY"

echo
echo "==> 7. PATCH /business/erp/receivable/{id}/write-off（坏账核销）"
BODY=$(curl -s -w '\n%{http_code}' -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"reason":"客户已注销，经理批准核销（SO13 curl 演示）"}' \
  "$BASE/business/erp/receivable/$AR_ID/write-off")
CODE=$(echo "$BODY" | tail -1); JSON=$(echo "$BODY" | sed '$d')
check "HTTP 状态" "$CODE" "200"
check "状态 → WRITTEN_OFF" "$(echo "$JSON" | jq -r '.data.status')" "WRITTEN_OFF"
check "状态标签" "$(echo "$JSON" | jq -r '.data.status_label')" "坏账核销"
[ "$(echo "$JSON" | jq -r '.data.settled_at // "null"')" != "null" ] \
  && ok "核销时间已落 settled_at" || bad "settled_at 未落值"
echo "$JSON" | jq -r '.data.remark' | grep -q '客户已注销' \
  && ok "核销原因写进 remark" || bad "remark 未记录原因"
check "核销后未收金额不变（坏账不改金额）" \
  "$(echo "$JSON" | jq -r '.data.unsettled_amount')" "1234.5600"

echo
echo "==> 8. 反向用例"
JSON=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"reason":"重复核销"}' "$BASE/business/erp/receivable/$AR_ID/write-off")
check "终态再核销 → 409(10003)" "$(echo "$JSON" | jq -r '.code')" "10003"
JSON=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"reason":""}' "$BASE/business/erp/receivable/$AR_ID/write-off")
check "核销缺原因 → 400(10001)" "$(echo "$JSON" | jq -r '.code')" "10001"
JSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/not-a-uuid")
check "非法 UUID → 400(66006)" "$(echo "$JSON" | jq -r '.code')" "66006"
JSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/00000000-0000-4000-8000-00000000dead")
check "不存在的应收 → 404(10002)" "$(echo "$JSON" | jq -r '.code')" "10002"
JSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUSTOMER\",\"amount\":0}" "$BASE/business/erp/receivable")
check "amount=0 → 400(10001)" "$(echo "$JSON" | jq -r '.code')" "10001"

echo
echo "==> 9. 库层留痕（状态日志）"
LOG_ROWS=$(psql_q "SELECT count(*) FROM biz_status_log
                    WHERE entity_type = 'RECEIVABLE' AND entity_id = '$AR_ID'::uuid")
check "biz_status_log 行数（建单 + 核销）" "$LOG_ROWS" "2"
psql_q "SELECT from_status || '->' || to_status || ' | ' || coalesce(reason,'-')
          FROM biz_status_log WHERE entity_type = 'RECEIVABLE'
           AND entity_id = '$AR_ID'::uuid ORDER BY created_at" | sed 's/^/      /'

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
