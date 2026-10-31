#!/usr/bin/env bash
# ============================================================
# P7 验收脚本：应付付款（部分付款 → 结清）+ 凭证红冲（红字凭证 + 反向指针）
#
# 用法（在 144 上、隔离实例已起）：
#   P7_DB=cloudpivot_p7 P7_PORT=18099 bash scripts/verify/p7_payable_voucher.sh
#
# 前置：
#   1. 隔离库已把迁移跑到 V0158（`select max(version::int) from schema_migrations` ≥ 158）
#   2. 隔离实例监听 $P7_PORT，profile=dev（NIK_ADMIN_PASSWORD 可登录）
#
# 口径：本脚本**只动** `P7V-` / `PM-` 前缀的夹具（每条用例前先清自己上一轮的行），
#      不改任何既有业务数据。每个断言打印 [PASS]/[FAIL] 与真实值，结尾非 0 退出。
# ============================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
P7_DB=${P7_DB:-cloudpivot_p7}
P7_PORT=${P7_PORT:-18099}
ENV_FILE=${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}
BASE="http://127.0.0.1:${P7_PORT}/api"

PASS=0
FAIL=0

ok()   { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
info() { printf '       %s\n' "$1"; }

# 断言：相等
expect() { # expect <说明> <期望> <实际>
  if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 [$2] 实际 [$3]"; fi
}
# 断言：数值相等（NUMERIC 经 JSON 回来带标度，逐字比会假红）
expect_num() { # expect_num <说明> <期望> <实际>
  if [ -z "$3" ]; then
    bad "$1：实际值是空的（接口没返回 / SQL 报错）"
  elif awk -v a="$2" -v b="$3" 'BEGIN{exit !(a+0==b+0)}'; then
    ok "$1（$3）"
  else
    bad "$1：期望 [$2] 实际 [$3]"
  fi
}
# 断言：包含子串
expect_has() { # expect_has <说明> <子串> <文本>
  if [ -z "$3" ]; then
    bad "$1：实际文本是空的"
  elif [ -z "$2" ]; then
    bad "$1：断言子串为空（脚本写错了）"
  else
    case "$3" in
      *"$2"*) ok "$1" ;;
      *) bad "$1：期望包含 [$2]，实际 [$3]" ;;
    esac
  fi
}

# ---------- 0. 环境 ----------
set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:?需要 NIK_DATABASE_PASSWORD}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"

psql_q() { # psql_q <sql>  只取第一行（去掉命令标签）
  psql -h 127.0.0.1 -U "$DB_USER" -d "$P7_DB" -Atc "$1" 2>&1 | head -1
}
psql_n() { # psql_n <sql>  取第一行第一列的数值
  psql_q "$1"
}

echo "=== 0. 环境"
echo "DB=$P7_DB  PORT=$P7_PORT  BASE=$BASE"
MIG=$(psql_n "select max(version::int) from schema_migrations where version is not null")
expect "迁移已到 V0158" "158" "$MIG"
TBL=$(psql_q "select count(*) from information_schema.tables where table_name='biz_payment'")
expect "biz_payment 表存在" "1" "$TBL"

# ---------- 1. 登录 ----------
login() { # login <password>
  curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$1\"}"
}
json_get() { # json_get <json> <key>  （只取 data.<key> 或顶层 <key>）
  printf '%s' "$1" | python3 -c '
import sys, json
raw = sys.stdin.read()
try:
    d = json.loads(raw)
except Exception:
    print(""); raise SystemExit
data = d.get("data") or {}
print(data.get(sys.argv[1]) or d.get(sys.argv[1]) or "")
' "$2"
}

TOKEN=""
LOGIN_RESP=""
USED_PW=""
for pw in "${P7_ADMIN_PASSWORD:-}" "${NIK_ADMIN_PASSWORD:-}" "P7Admin#2026"; do
  [ -n "$pw" ] || continue
  RESP=$(login "$pw")
  TOKEN=$(json_get "$RESP" access_token)
  [ -n "$TOKEN" ] && { USED_PW="$pw"; LOGIN_RESP="$RESP"; echo "       登录成功"; break; }
done
if [ -z "$TOKEN" ]; then
  bad "登录失败：三种口令都不通"
  echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
  exit 1
fi
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

# 首登强制改密：不改密时所有业务接口都是 403「首次登录必须先修改初始密码」。
# 顺序必须是「带旧 token 改密 → 用新口令重登」（改密会递增 token_version，旧 token 立刻失效）。
if [ "$(json_get "$LOGIN_RESP" must_change_password)" = "True" ]; then
  echo "       首登强制改密 → 改密并重登"
  curl -s -o /dev/null -X PUT "$BASE/system/auth/password" "${AUTH[@]}" \
    -d "{\"old_password\":\"$USED_PW\",\"new_password\":\"P7Admin#2026\"}"
  RESP=$(login "P7Admin#2026")
  TOKEN=$(json_get "$RESP" access_token)
  if [ -z "$TOKEN" ]; then
    bad "改密后重登失败"
    echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
    exit 1
  fi
  AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')
  ok "登录并完成首登改密"
fi

# ---------- 2. 清理本卡上一轮夹具 ----------
# ⚠️ 只删本卡自己造的行（应付单 `AP-P7V-` + 它们的付款记录 + 付款凭证 + `P7V-`/`P7PAY-` 凭证），
#    绝不按 `source_no LIKE 'PM-%'` 全局删 —— 同一个库里可能有真实业务的付款凭证。
psql_q "delete from biz_voucher_entry where voucher_id in (
          select id from biz_voucher
           where source_no like 'P7V-%' or source_no like 'P7PAY-%'
              or source_id in (select id from biz_payment
                                where payable_id in (select id from biz_payable where payable_no like 'AP-P7V-%')))" >/dev/null
psql_q "delete from biz_voucher
         where source_no like 'P7V-%' or source_no like 'P7PAY-%'
            or source_id in (select id from biz_payment
                              where payable_id in (select id from biz_payable where payable_no like 'AP-P7V-%'))" >/dev/null
psql_q "delete from biz_payment where payable_id in (select id from biz_payable where payable_no like 'AP-P7V-%')" >/dev/null
psql_q "delete from biz_payable where payable_no like 'AP-P7V-%'" >/dev/null

# ---------- 3. 造一张 OPEN 应付单 ----------
SUPPLIER="e7e70002-0000-4000-8000-0000000000a1"
psql_q "insert into biz_supplier (id, supplier_code, supplier_name, status, credit_days)
        values ('$SUPPLIER'::uuid, 'P7V-SUP', 'P7 验收供应商', 1, 30)
        on conflict do nothing" >/dev/null
psql_q "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
        values (2026, 10, '2026-10', date '2026-10-01', date '2026-10-31', 'OPEN')
        on conflict (period_code) do update set status = 'OPEN'" >/dev/null

AP_NO="AP-P7V-$(date +%H%M%S)$RANDOM"
AP_ID=$(psql_q "insert into biz_payable (payable_no, supplier_id, source_type, source_id, source_no,
                                        currency_code, amount, paid_amount, unpaid_amount, credit_days,
                                        due_date, period_code, status)
                values ('$AP_NO', '$SUPPLIER'::uuid, 'STOCK_IN', gen_random_uuid(), 'P7V-IN-0001',
                        'CNY', 500.0000, 0, 500.0000, 30, current_date + 30, '2026-10', 'OPEN')
                returning id")
echo
echo "=== 1. 应付全链路（$AP_NO 应付 500.00 / 未付 500.00）"
if [ -z "$AP_ID" ] || [ "${#AP_ID}" -lt 32 ]; then
  bad "造应付单失败：$AP_ID"
  echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
  exit 1
fi
info "payable_id=$AP_ID"

# ① 记下未付金额
UNPAID0=$(psql_n "select unpaid_amount from biz_payable where id = '$AP_ID'::uuid")
expect_num "① 初始未付金额 = 500.00" "500.00" "$UNPAID0"

# ② 付一半
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/payable/$AP_ID/pay" "${AUTH[@]}" \
  -d '{"amount":"250.00","pay_method":"银行转账","remark":"P7 验收：首付一半"}')
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "② 付一半 HTTP 200" "200" "$CODE"
STATUS=$(json_get "$BODY" status)
PAID=$(json_get "$BODY" paid_amount)
UNPAID=$(json_get "$BODY" unpaid_amount)
expect "② 状态转 PARTIAL_PAID" "PARTIAL_PAID" "$STATUS"
expect_num "② 已付 = 250.00" "250.00" "$PAID"
info "$(printf '%s' "$BODY" | head -c 200)"

# ③ 详情未付减半
DET=$(curl -s "$BASE/business/erp/payable/$AP_ID" "${AUTH[@]}")
UNPAID_GET=$(json_get "$DET" unpaid_amount)
expect_num "③ GET 详情未付 = 250.00" "250.00" "$UNPAID_GET"
PAY_COUNT=$(printf '%s' "$DET" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(len((d.get("data") or {}).get("payments") or []))')
expect "③ 详情带 1 条付款记录" "1" "$PAY_COUNT"

# ④ 付剩余 → SETTLED
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/payable/$AP_ID/pay" "${AUTH[@]}" \
  -d '{"amount":"250.00","pay_method":"银行转账","remark":"P7 验收：付清"}')
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "④ 付剩余 HTTP 200" "200" "$CODE"
STATUS=$(json_get "$BODY" status)
expect "④ 状态转 SETTLED" "SETTLED" "$STATUS"
expect_num "④ 未付 = 0" "0" "$(json_get "$BODY" unpaid_amount)"
SETTLED_AT=$(psql_q "select settled_at is not null from biz_payable where id = '$AP_ID'::uuid")
expect "④ settled_at 已写入" "t" "$SETTLED_AT"

# ⑤ 两次付款 → 2 张凭证，各自借贷平衡（借 2202 / 贷 1002）
PAYMENT_NOS=$(psql_q "select string_agg(payment_no, ',') from biz_payment where payable_id = '$AP_ID'::uuid")
info "付款单号：$PAYMENT_NOS"
VC=$(psql_n "select count(*) from biz_voucher where source_type = 'PAYMENT'
              and source_no in (select payment_no from biz_payment where payable_id = '$AP_ID'::uuid)")
expect "⑤ 两次付款生成 2 张凭证" "2" "$VC"
UNBAL=$(psql_n "select count(*) from biz_voucher where source_type = 'PAYMENT'
                 and source_no in (select payment_no from biz_payment where payable_id = '$AP_ID'::uuid)
                 and total_debit <> total_credit")
expect "⑤ 2 张凭证借贷平衡（不平衡 0 张）" "0" "$UNBAL"
ENTRY2202=$(psql_n "select count(*) from biz_voucher_entry e join biz_voucher v on v.id = e.voucher_id
                     where v.source_type = 'PAYMENT'
                       and v.source_no in (select payment_no from biz_payment where payable_id = '$AP_ID'::uuid)
                       and e.account_code = '2202' and e.debit_amount > 0 and e.credit_amount = 0")
ENTRY1002=$(psql_n "select count(*) from biz_voucher_entry e join biz_voucher v on v.id = e.voucher_id
                     where v.source_type = 'PAYMENT'
                       and v.source_no in (select payment_no from biz_payment where payable_id = '$AP_ID'::uuid)
                       and e.account_code = '1002' and e.credit_amount > 0 and e.debit_amount = 0")
expect "⑤ 借方 2202 应付账款 2 行" "2" "$ENTRY2202"
expect "⑤ 贷方 1002 银行存款 2 行" "2" "$ENTRY1002"
psql_q "select voucher_no || ' 借' || total_debit || ' 贷' || total_credit
          from biz_voucher where source_type = 'PAYMENT'
         and source_no in (select payment_no from biz_payment where payable_id = '$AP_ID'::uuid)
         order by voucher_no" | while read -r line; do info "凭证 $line"; done

# ⑥ 已结清再付款 → 409
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/payable/$AP_ID/pay" "${AUTH[@]}" \
  -d '{"amount":"1.00"}')
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "⑥ 已结清再付款 HTTP 409" "409" "$CODE"
expect_has "⑥ 文案说明已结清" "已结清" "$BODY"

# ---------- 4. 凭证红冲 ----------
echo
echo "=== 2. 凭证红冲"
ORIG_ID=$(psql_q "select id from biz_voucher where source_type = 'PAYMENT'
                   and source_no in (select payment_no from biz_payment where payable_id = '$AP_ID'::uuid)
                   order by voucher_no limit 1")
ORIG_NO=$(psql_q "select voucher_no from biz_voucher where id = '$ORIG_ID'::uuid")
ORIG_AMT=$(psql_q "select total_debit from biz_voucher where id = '$ORIG_ID'::uuid")
info "原凭证 $ORIG_NO（借贷合计 $ORIG_AMT）"

RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/voucher/$ORIG_ID/reverse" "${AUTH[@]}" \
  -d '{"reason":"录错科目"}')
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "⑦ 红冲 HTTP 200" "200" "$CODE"
RED_NO=$(json_get "$BODY" voucher_no)
IS_RED=$(json_get "$BODY" is_red)
expect "⑦ 返回的是红字凭证（is_red=true）" "True" "$IS_RED"
expect_has "⑦ 红字凭证走独立号段 PZR-" "PZR-" "$RED_NO"
info "$(printf '%s' "$BODY" | head -c 220)"

# ⑧ reverse_of 指向原凭证，金额等额
RED_OF=$(psql_q "select reverse_of::text from biz_voucher where reverse_of = '$ORIG_ID'::uuid")
expect "⑧ 红字凭证 reverse_of 指向原凭证" "$ORIG_ID" "$RED_OF"
RED_COUNT=$(psql_n "select count(*) from biz_voucher where reverse_of = '$ORIG_ID'::uuid")
expect "⑧ 只有 1 张红字凭证" "1" "$RED_COUNT"
RED_AMT=$(psql_n "select total_debit from biz_voucher where reverse_of = '$ORIG_ID'::uuid")
expect_num "⑧ 红字凭证金额与原凭证等额" "$ORIG_AMT" "$RED_AMT"
RED_BAL=$(psql_n "select count(*) from biz_voucher where reverse_of = '$ORIG_ID'::uuid and total_debit <> total_credit")
expect "⑧ 红字凭证自身借贷平衡" "0" "$RED_BAL"
# 借贷互换：原「借 2202」在红字里变「贷 2202」
SWAP=$(psql_n "select count(*) from biz_voucher_entry e where e.voucher_id = (select id from biz_voucher where reverse_of = '$ORIG_ID'::uuid)
                 and e.account_code = '2202' and e.credit_amount > 0 and e.debit_amount = 0")
expect "⑧ 红字凭证里 2202 变贷方（借贷互换）" "1" "$SWAP"

# ⑨ 原凭证 reversed_by 回写、status 仍 POSTED
REVERSED_BY=$(psql_q "select reversed_by::text from biz_voucher where id = '$ORIG_ID'::uuid")
RED_ID=$(psql_q "select id from biz_voucher where reverse_of = '$ORIG_ID'::uuid")
expect "⑨ 原凭证 reversed_by 指向红字凭证" "$RED_ID" "$REVERSED_BY"
ORIG_STATUS=$(psql_q "select status from biz_voucher where id = '$ORIG_ID'::uuid")
expect "⑨ 原凭证状态仍 POSTED" "POSTED" "$ORIG_STATUS"

# ⑩ 再红冲 → 409
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/voucher/$ORIG_ID/reverse" "${AUTH[@]}" \
  -d '{"reason":"再来一次"}')
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "⑩ 重复红冲 HTTP 409" "409" "$CODE"
expect_has "⑩ 文案提到已红冲" "已经红冲" "$BODY"
expect_has "⑩ 文案点名红字凭证号" "$RED_NO" "$BODY"

# ⑪ 红字凭证本身不能再红冲 → 409
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/voucher/$RED_ID/reverse" "${AUTH[@]}" \
  -d '{"reason":"冲红字"}')
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "⑪ 红冲红字凭证 HTTP 409" "409" "$CODE"
expect_has "⑪ 文案说明红字凭证不能再红冲" "红字凭证" "$BODY"

# ---------- 5. 手工凭证（借贷平衡 / 差额） ----------
echo
echo "=== 3. 手工凭证"
ACC_A=$(psql_q "select id from biz_account where account_code = '1405' and deleted_at is null limit 1")
ACC_B=$(psql_q "select id from biz_account where account_code = '2202' and deleted_at is null limit 1")

python3 - "$ACC_A" "$ACC_B" > /tmp/p7_unbalanced.json <<'PY'
import json, sys
a, b = sys.argv[1], sys.argv[2]
print(json.dumps({
    "summary": "P7 验收 不平衡",
    "entries": [
        {"account_id": a, "debit": "100.00", "credit": "0", "summary": "借 100"},
        {"account_id": b, "debit": "0", "credit": "99.50", "summary": "贷 99.5"},
    ],
}, ensure_ascii=False))
PY
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/voucher" "${AUTH[@]}" -d @/tmp/p7_unbalanced.json)
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "⑫ 不平衡手工凭证 HTTP 400" "400" "$CODE"
expect_has "⑫ 文案点名差额 0.5" "差额 0.5" "$BODY"

python3 - "$ACC_A" "$ACC_B" > /tmp/p7_balanced.json <<'PY'
import json, sys
a, b = sys.argv[1], sys.argv[2]
print(json.dumps({
    "summary": "P7 验收 手工凭证",
    "entries": [
        {"account_id": a, "debit": "123.45", "credit": "0", "summary": "借 存货"},
        {"account_id": b, "debit": "0", "credit": "123.45", "summary": "贷 应付"},
    ],
}, ensure_ascii=False))
PY
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/voucher" "${AUTH[@]}" -d @/tmp/p7_balanced.json)
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "⑬ 平衡手工凭证 HTTP 200" "200" "$CODE"
MV_STATUS=$(json_get "$BODY" status)
MV_TYPE=$(json_get "$BODY" voucher_type)
MV_COUNT=$(json_get "$BODY" entry_count)
expect "⑬ 手工凭证直接 POSTED" "POSTED" "$MV_STATUS"
expect "⑬ 凭证类型 MANUAL" "MANUAL" "$MV_TYPE"
expect "⑬ 分录行数 = 2" "2" "$MV_COUNT"
MV_NO=$(json_get "$BODY" voucher_no)
info "手工凭证 $MV_NO"

# ---------- 5. 应付作废 ----------
echo
echo "=== 4. 应付作废"
AP2_NO="AP-P7V-C$(date +%H%M%S)$RANDOM"
AP2_ID=$(psql_q "insert into biz_payable (payable_no, supplier_id, source_type, source_id, source_no,
                                         currency_code, amount, paid_amount, unpaid_amount, credit_days,
                                         due_date, period_code, status)
                 values ('$AP2_NO', '$SUPPLIER'::uuid, 'STOCK_IN', gen_random_uuid(), 'P7V-IN-0002',
                         'CNY', 88.0000, 0, 88.0000, 30, current_date + 30, '2026-10', 'OPEN')
                 returning id")
# 作废原因必填 → 400
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/payable/$AP2_ID/cancel" "${AUTH[@]}" -d '{}')
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "⑭ 缺原因作废 HTTP 400" "400" "$CODE"
expect_has "⑭ 文案说明 reason 必填" "reason 必填" "$BODY"

# 正常作废 → CANCELLED
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/payable/$AP2_ID/cancel" "${AUTH[@]}" \
  -d '{"reason":"P7 验收：重复入库"}')
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "⑭ 作废 HTTP 200" "200" "$CODE"
expect "⑭ 状态转 CANCELLED" "CANCELLED" "$(json_get "$BODY" status)"
REASON_LOGGED=$(psql_q "select (remark like '%重复入库%')::text from biz_payable where id = '$AP2_ID'::uuid")
expect "⑭ 作废原因留痕" "true" "$REASON_LOGGED"

# 已付款的单作废 → 409（用本卡第一张已结清的应付单）
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/erp/payable/$AP_ID/cancel" "${AUTH[@]}" \
  -d '{"reason":"P7 验收：已付款还能作废吗"}')
CODE=$(printf '%s' "$RESP" | tail -1)
BODY=$(printf '%s' "$RESP" | sed '$d')
expect "⑭ 已付款的单作废 HTTP 409" "409" "$CODE"
expect_has "⑭ 文案说明不能作废" "不能作废" "$BODY"

# ---------- 汇总 ----------
echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
echo "（本次夹具：应付单 $AP_NO / 付款单 $PAYMENT_NOS / 红字凭证 $RED_NO / 手工凭证 $MV_NO）"
if [ "$FAIL" -ne 0 ]; then
  exit 1
fi
exit 0
