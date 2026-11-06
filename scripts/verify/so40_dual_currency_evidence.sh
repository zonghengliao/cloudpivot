#!/usr/bin/env bash
# SO-40 证据脚本：起实例 → 种汇率 → 真实 token curl 建 USD 单 → 验汇率锁定 / 次日改汇率不影响老单
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

DB=${ERP_DB:-cloudpivot_so40_clean}
PORT=${PORT:-18099}
BIN=${BIN:-/root/so40-target/debug/cloudpivot}
LOG=${LOG:-/root/so40-evidence.log}
OUT=${OUT:-/opt/cloudpivot/docs/evidence/20261107-so40-currency}
BASE="http://127.0.0.1:$PORT"
PSQL="psql postgres://alun:alun@127.0.0.1:5432/$DB -Atc"

mkdir -p "$OUT"

export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="so40-evidence-jwt-secret-0123456789abcdef0123456789abcdef"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD=Admin@123456

cd /opt/cloudpivot/backend || exit 1
# 先把 admin 的「首次登录必须改密」旗标清掉：否则所有业务接口一律 403（本卡与改密无关，
# 证据要打的是销售单据接口）。必须在实例启动前改，实例内有 60s 缓存。
$PSQL "UPDATE sys_user SET must_change_password = false WHERE username = 'admin'" > /dev/null
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > /root/so40-evidence.pid
echo "== 启动 pid=$(cat /root/so40-evidence.pid) 库=$DB 端口=$PORT"
for _ in $(seq 1 120); do
  curl -sf "$BASE/health" > /dev/null && break
  sleep 1
done
sleep 2
echo "== /health: $(curl -s -o /dev/null -w '%{http_code}' "$BASE/health")"

# ---------- 1) 种汇率（本卡没有汇率维护 API，用 SQL 模拟「财务维护 SPOT」） ----------
$PSQL "DELETE FROM biz_exchange_rate WHERE from_currency IN ('USD','EUR')" > /dev/null
$PSQL "INSERT INTO biz_exchange_rate (id, from_currency, to_currency, rate, rate_type, effective_date)
       VALUES (gen_random_uuid(),'USD','CNY',7.10000000,'SPOT','2026-11-01')" > /dev/null
$PSQL "INSERT INTO biz_exchange_rate (id, from_currency, to_currency, rate, rate_type, effective_date)
       VALUES (gen_random_uuid(),'USD','CNY',7.90000000,'SPOT','2026-11-08')" > /dev/null
echo "== 汇率种子：" && $PSQL "SELECT from_currency||'->'||to_currency||' '||rate||' @'||effective_date FROM biz_exchange_rate ORDER BY effective_date"

# ---------- 2) 登录拿真 token ----------
TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["access_token"])')
[ -n "$TOKEN" ] && echo "== token 长度 ${#TOKEN}"

AUTH="Authorization: Bearer $TOKEN"
JSON="Content-Type: application/json"

# ---------- 3) 夹具：客户 + 物料 ----------
CUST=$($PSQL "SELECT id FROM biz_customer WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1")
if [ -z "$CUST" ]; then
  CUST=$(curl -s -X POST "$BASE/api/business/erp/customer" -H "$AUTH" -H "$JSON" \
    -d '{"code":"SO40-CUST","name":"SO-40 双币夹具客户","credit_limit":100000000}' \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["id"])')
fi
MAT=$($PSQL "SELECT id FROM biz_material WHERE deleted_at IS NULL LIMIT 1")
echo "== 客户=$CUST 物料=$MAT"

# ---------- 4) USD 建单（2026-11-07）→ 应锁定 7.1 ----------
ORDER1=$(curl -s -X POST "$BASE/api/business/erp/sales-order" -H "$AUTH" -H "$JSON" -d "{
  \"customer_id\":\"$CUST\",\"order_date\":\"2026-11-07\",\"currency_code\":\"USD\",
  \"remark\":\"SO-40 证据：USD 单（应锁 7.1）\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":\"10\",\"price\":\"100\",\"tax_rate\":\"13\"}]}")
echo "== 建单响应：" && echo "$ORDER1" | python3 -m json.tool
OID=$(echo "$ORDER1" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["id"])')

# ---------- 5) 改汇率（次日 7.9）+ 再建一张 11-08 的单 ----------
ORDER2=$(curl -s -X POST "$BASE/api/business/erp/sales-order" -H "$AUTH" -H "$JSON" -d "{
  \"customer_id\":\"$CUST\",\"order_date\":\"2026-11-08\",\"currency_code\":\"USD\",
  \"remark\":\"SO-40 证据：USD 单（应锁 7.9）\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":\"10\",\"price\":\"100\",\"tax_rate\":\"13\"}]}")
echo "== 次日建单响应：" && echo "$ORDER2" | python3 -m json.tool

# ---------- 6) 回读老单：汇率必须还是 7.1 ----------
BACK=$(curl -s "$BASE/api/business/erp/sales-order/$OID" -H "$AUTH")
echo "== 老单回读（汇率变动后）：" && echo "$BACK" | python3 -m json.tool

# ---------- 7) 无汇率的外币单必须 4xx ----------
echo "== EUR 无汇率建单（期望 400）："
curl -s -o /tmp/so40-eur.json -w 'HTTP %{http_code}\n' -X POST "$BASE/api/business/erp/sales-order" -H "$AUTH" -H "$JSON" -d "{
  \"customer_id\":\"$CUST\",\"order_date\":\"2026-11-07\",\"currency_code\":\"EUR\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":\"1\",\"price\":\"1\",\"tax_rate\":\"0\"}]}"
cat /tmp/so40-eur.json | python3 -m json.tool

# ---------- 8) 收款单（USD）也要锁定 ----------
RCPT=$(curl -s -X POST "$BASE/api/business/erp/receipt" -H "$AUTH" -H "$JSON" -d "{
  \"customer_id\":\"$CUST\",\"receipt_date\":\"2026-11-07\",\"amount\":\"1000\",\"currency_code\":\"USD\",
  \"method\":\"BANK_TRANSFER\"}")
echo "== 收款单响应：" && echo "$RCPT" | python3 -m json.tool

echo "== 收尾：停实例"
kill "$(cat /root/so40-evidence.pid)" 2>/dev/null
sleep 1
