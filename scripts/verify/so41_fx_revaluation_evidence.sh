#!/usr/bin/env bash
# SO-41 证据脚本：起实例 → 种 SPOT 汇率 → 真实 token curl
#   ① USD 手工挂账应收（锁 7.1）
#   ② 从应收开票 → ISSUED ⇒ 收入凭证按**本位币**入账（借 1122 8023 / 贷 5001 7100 / 贷 2221.02 923）
#   ③ 月末结账（期末汇率 7.9）⇒ 自动生成汇兑重估凭证（借 1122 904 / 贷 5603 904）
#   ④ 本位币应收（CNY）不产生任何重估行
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

DB=${ERP_DB:-cloudpivot_so41_api}
PORT=${PORT:-18142}
BIN=${BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}
LOG=${LOG:-/root/so41-evidence.log}
OUT=${OUT:-/opt/cloudpivot/docs/evidence/20261107-so41-fx}
BASE="http://127.0.0.1:$PORT"
PSQL() { psql "postgres://alun:alun@127.0.0.1:5432/$DB" -Atc "$1"; }

mkdir -p "$OUT"

export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="so41-evidence-jwt-secret-0123456789abcdef0123456789abcdef"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD=Admin@123456

cd /opt/cloudpivot/backend || exit 1
# 「首次登录必须改密」旗标必须在实例启动前清掉（实例内有 60s 缓存）
PSQL "UPDATE sys_user SET must_change_password = false WHERE username = 'admin'" > /dev/null
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > /root/so41-evidence.pid
echo "== 启动 pid=$(cat /root/so41-evidence.pid) 库=$DB 端口=$PORT"
for _ in $(seq 1 120); do
  curl -sf "$BASE/health" > /dev/null && break
  sleep 1
done
sleep 2
echo "== /health: $(curl -s -o /dev/null -w '%{http_code}' "$BASE/health")"

# ---------- 1) 种汇率（汇率维护 API 属 SO-39 后续卡，这里用 SQL 模拟财务维护 SPOT）----------
PSQL "DELETE FROM biz_exchange_rate WHERE from_currency IN ('USD','EUR')" > /dev/null
PSQL "INSERT INTO biz_exchange_rate (id, from_currency, to_currency, rate, rate_type, effective_date)
      VALUES (gen_random_uuid(),'USD','CNY',7.10000000,'SPOT','2026-11-01')" > /dev/null
PSQL "INSERT INTO biz_exchange_rate (id, from_currency, to_currency, rate, rate_type, effective_date)
      VALUES (gen_random_uuid(),'USD','CNY',7.90000000,'SPOT','2026-11-30')" > /dev/null
echo "== 汇率种子："
PSQL "SELECT from_currency||'->'||to_currency||' '||rate||' @'||effective_date FROM biz_exchange_rate ORDER BY effective_date"

# ---------- 2) 真 token ----------
TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["access_token"])')
[ -n "$TOKEN" ] && echo "== token 长度 ${#TOKEN}"
AUTH="Authorization: Bearer $TOKEN"
JSON="Content-Type: application/json"

# ---------- 3) 夹具：客户 ----------
CUST=$(PSQL "SELECT id FROM biz_customer WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1")
if [ -z "$CUST" ]; then
  CUST=$(curl -s -X POST "$BASE/api/business/erp/customer" -H "$AUTH" -H "$JSON" \
    -d '{"code":"SO41-CUST","name":"SO-41 汇兑夹具客户","credit_limit":100000000}' \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["id"])')
fi
echo "== 客户=$CUST"

# ---------- 4) USD 手工挂账应收（2026-11 期间，锁 7.1）----------
AR=$(curl -s -X POST "$BASE/api/business/erp/receivable" -H "$AUTH" -H "$JSON" -d "{
  \"customer_id\":\"$CUST\",\"amount\":\"1130\",\"period_code\":\"2026-11\",
  \"currency_code\":\"USD\",\"remark\":\"SO-41 证据：USD 应收 1130（锁 7.1）\"}")
echo "== 建 USD 应收响应：" && echo "$AR" | python3 -m json.tool
AR_ID=$(echo "$AR" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["id"])')
echo "== 库内该应收的双币列（原币 / 汇率 / 本位币含税）："
PSQL "SELECT currency_code||' | rate='||exchange_rate||' | base_amount='||base_total_amount||' | base_with_tax='||base_total_with_tax
        FROM biz_receivable WHERE id = '$AR_ID'"

# ---------- 5) 从应收开票 → ISSUED ⇒ 收入凭证按本位币入账 ----------
INV=$(curl -s -X POST "$BASE/api/business/erp/invoice" -H "$AUTH" -H "$JSON" -d "{
  \"receivable_id\":\"$AR_ID\",\"invoice_date\":\"2026-11-07\",
  \"amount\":\"1000\",\"tax_amount\":\"130\",\"remark\":\"SO-41 证据：USD 发票\"}")
echo "== 开票响应：" && echo "$INV" | python3 -m json.tool
INV_ID=$(echo "$INV" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["id"])')
echo "== 库内发票双币列："
PSQL "SELECT currency_code||' | rate='||exchange_rate||' | base_amount='||base_total_amount||' | base_tax='||base_total_tax||' | base_with_tax='||base_total_with_tax
        FROM biz_invoice WHERE id = '$INV_ID'"

echo "== ISSUED（开票）响应摘要："
curl -s -X PATCH "$BASE/api/business/erp/invoice/$INV_ID/status" -H "$AUTH" -H "$JSON" \
  -d '{"status":"ISSUED"}' | python3 -c 'import sys,json; d=json.load(sys.stdin); print(json.dumps({"code":d.get("code"),"msg":d.get("msg"),"status":d["data"]["status"] if d.get("data") else None,"voucher_id":d["data"].get("voucher_id") if d.get("data") else None}, ensure_ascii=False))'

V=$(PSQL "SELECT voucher_id FROM biz_invoice WHERE id = '$INV_ID'")
echo "== 收入凭证 ${V} 的分录（借 1122 应 8023 / 贷 5001 7100 / 贷 2221.02 923）："
PSQL "SELECT line_no||' | '||account_code||' | '||account_name||' | debit='||debit_amount||' | credit='||credit_amount
        FROM biz_voucher_entry WHERE voucher_id = '$V' ORDER BY line_no"
echo "== 借贷合计与恒等式："
PSQL "SELECT 'debit='||SUM(debit_amount)||' credit='||SUM(credit_amount)||' balanced='||(SUM(debit_amount)=SUM(credit_amount))
        FROM biz_voucher_entry WHERE voucher_id = '$V'"

# ---------- 6) 本位币对照：CNY 应收（不该被重估）----------
AR_CNY=$(curl -s -X POST "$BASE/api/business/erp/receivable" -H "$AUTH" -H "$JSON" -d "{
  \"customer_id\":\"$CUST\",\"amount\":\"500\",\"period_code\":\"2026-11\",
  \"remark\":\"SO-41 证据：CNY 应收（不重估）\"}")
echo "== 建 CNY 应收响应（只打 code/ar_no/currency）："
echo "$AR_CNY" | python3 -c 'import sys,json; d=json.load(sys.stdin); x=d.get("data") or {}; print(json.dumps({"code":d.get("code"),"ar_no":x.get("ar_no"),"currency_code":x.get("currency_code"),"amount":x.get("amount")}, ensure_ascii=False))'

# ---------- 7) 月末结账 → 汇兑重估 ----------
PID=$(PSQL "SELECT id FROM biz_accounting_period WHERE period_code = '2026-11'")
echo "== 期间 2026-11 id=$PID 状态=$(PSQL "SELECT status FROM biz_accounting_period WHERE id='$PID'")"
echo "== 结账预检："
curl -s "$BASE/api/business/erp/period/$PID/close-check" -H "$AUTH" \
  | python3 -c 'import sys,json; d=json.load(sys.stdin)["data"]; print(json.dumps({"can_close":d["can_close"],"blockers":[b.get("message") for b in d["blockers"]],"warnings":len(d.get("warnings") or [])}, ensure_ascii=False))'

echo "== 结账响应（code / status）："
curl -s -X POST "$BASE/api/business/erp/period/$PID/close" -H "$AUTH" -H "$JSON" \
  -d '{"reason":"SO-41 证据：月末结账触发汇兑重估"}' \
  | python3 -c 'import sys,json; d=json.load(sys.stdin); x=d.get("data") or {}; print(json.dumps({"code":d.get("code"),"msg":d.get("msg"),"status":x.get("status"),"closed_at":x.get("closed_at")}, ensure_ascii=False))'

FX=$(PSQL "SELECT id FROM biz_voucher WHERE source_type='FX_REVALUATION' AND source_no='2026-11' AND is_red=0")
echo "== 重估凭证 ${FX} 摘要与分录（借 1122 904 / 贷 5603 904）："
PSQL "SELECT voucher_no||' | '||summary||' | date='||voucher_date||' | source_type='||source_type
        FROM biz_voucher WHERE id = '$FX'"
PSQL "SELECT line_no||' | '||account_code||' | '||account_name||' | summary='||summary||' | debit='||debit_amount||' | credit='||credit_amount
        FROM biz_voucher_entry WHERE voucher_id = '$FX' ORDER BY line_no"
echo "== 重估台账（只应有 USD 那一行；CNY 不许出现）："
PSQL "SELECT period_code||' | '||ar_no||' | '||currency_code||' | open='||open_amount||' | book='||book_base||' | reval='||revalued_base||' | diff='||diff_base
        FROM biz_fx_revaluation WHERE period_code='2026-11' ORDER BY ar_no"
echo "== CNY 应收是否被重估（期望 0）："
PSQL "SELECT count(*) FROM biz_fx_revaluation f JOIN biz_receivable r ON r.id = f.receivable_id WHERE r.currency_code = 'CNY'"

# ---------- 8) 幂等：再跑一次结账（期间已 CLOSED ⇒ 409，不会二次计提）----------
echo "== 重复结账响应（期望非 0 业务码 + 文案含已结账）："
curl -s -X POST "$BASE/api/business/erp/period/$PID/close" -H "$AUTH" -H "$JSON" -d '{}' \
  | python3 -c 'import sys,json; d=json.load(sys.stdin); print(json.dumps({"code":d.get("code"),"msg":d.get("msg")}, ensure_ascii=False))'
echo "== 重估凭证条数（期望 1）："
PSQL "SELECT count(*) FROM biz_voucher WHERE source_type='FX_REVALUATION' AND source_no='2026-11'"

echo "== 完（实例仍在跑，pid=$(cat /root/so41-evidence.pid)；收工：kill \$(cat /root/so41-evidence.pid)）"
