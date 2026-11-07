#!/bin/bash
# SO-44 原始 curl 取证（真实 token + 真实 HTTP，逐条落证据文件）
# 前置：bash scripts/verify/so44_start.sh（隔离实例 127.0.0.1:18044 / 库 cloudpivot_so44）
#       PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so44 -f scripts/verify/so44_seed.sql
set -u
BASE=http://127.0.0.1:18044
SALES_A=aa440000-0000-4000-8000-000000000002
OUT=/opt/cloudpivot/docs/evidence/20261107-so44-sales-target/curl_evidence.txt
mkdir -p "$(dirname "$OUT")"
: > "$OUT"

say() { echo -e "\n$*" | tee -a "$OUT"; }

# e2e 会把首次登录的口令改成 Admin@123456so44，两个都试
TOKEN=""
for PASS in Admin@123456so44 Admin@123456; do
  TOKEN=$(curl -sS -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$PASS\"}" \
    | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
  [ -n "$TOKEN" ] && break
done
say "\$ TOKEN=\$(curl -sS -X POST \$BASE/api/system/auth/login -d '{\"username\":\"admin\",...}')   # 长度 ${#TOKEN}"
[ -n "$TOKEN" ] || { echo "登录失败" | tee -a "$OUT"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

# ---------- ① 达成率看板：两个口径 ----------
say '$ curl -sS "$BASE/api/business/erp/sales-target/achievement?period_code=2026-11&scope_type=SALESPERSON&basis=ORDER" -H "Authorization: Bearer ***"'
curl -sS "$BASE/api/business/erp/sales-target/achievement?period_code=2026-11&scope_type=SALESPERSON&basis=ORDER" \
  -H "$AUTH" | python3 -m json.tool | tee -a "$OUT"

say '$ curl -sS ".../achievement?period_code=2026-11&scope_type=SALESPERSON&basis=RECEIPT" -H "Authorization: Bearer ***"'
curl -sS "$BASE/api/business/erp/sales-target/achievement?period_code=2026-11&scope_type=SALESPERSON&basis=RECEIPT" \
  -H "$AUTH" | python3 -m json.tool | tee -a "$OUT"

say '$ curl -sS ".../achievement?period_code=2026-11&scope_type=ALL&basis=ORDER" -H "Authorization: Bearer ***"'
curl -sS "$BASE/api/business/erp/sales-target/achievement?period_code=2026-11&scope_type=ALL&basis=ORDER" \
  -H "$AUTH" | python3 -m json.tool | tee -a "$OUT"

say '$ curl -sS ".../achievement?period_code=2026-Q4&scope_type=ALL&basis=ORDER" -H "Authorization: Bearer ***"'
curl -sS "$BASE/api/business/erp/sales-target/achievement?period_code=2026-Q4&scope_type=ALL&basis=ORDER" \
  -H "$AUTH" | python3 -m json.tool | tee -a "$OUT"

# ---------- ② 目标维护 ----------
say '$ curl -sS -X POST "$BASE/api/business/erp/sales-target" -d {"scope_type":"SALESPERSON","scope_id":...,"period_code":"2026-12","target_amount":"30000.00"} -H "Authorization: Bearer ***"'
CREATE=$(curl -sS -X POST "$BASE/api/business/erp/sales-target" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"scope_type\":\"SALESPERSON\",\"scope_id\":\"$SALES_A\",\"period_code\":\"2026-12\",\"target_amount\":\"30000.00\",\"target_qty\":\"50\",\"remark\":\"curl 取证\"}")
echo "$CREATE" | python3 -m json.tool | tee -a "$OUT"
TARGET_ID=$(echo "$CREATE" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1)

say '$ curl -sS "$BASE/api/business/erp/sales-target/page?period_code=2026-12" -H "Authorization: Bearer ***"'
curl -sS "$BASE/api/business/erp/sales-target/page?period_code=2026-12" \
  -H "$AUTH" | python3 -m json.tool | tee -a "$OUT"

# ---------- ③ 预测维护（含版本号 / 同维度唯一） ----------
say '$ curl -sS -X POST "$BASE/api/business/erp/sales-forecast" -d {"period_code":"2026-12","forecast_amount":"18800.00","confidence":"HIGH","version_no":2} -H "Authorization: Bearer ***"'
curl -sS -X POST "$BASE/api/business/erp/sales-forecast" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"period_code":"2026-12","forecast_amount":"18800.00","confidence":"HIGH","version_no":2,"remark":"curl 取证 全局预测"}' \
  | python3 -m json.tool | tee -a "$OUT"

say '$ curl -sS "$BASE/api/business/erp/sales-forecast/page?period_code=2026-12" -H "Authorization: Bearer ***"'
curl -sS "$BASE/api/business/erp/sales-forecast/page?period_code=2026-12" \
  -H "$AUTH" | python3 -m json.tool | tee -a "$OUT"

# ---------- ④ 负向：参数 / 鉴权 ----------
say '# 非法 basis（只认 ORDER / RECEIPT）'
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' \
  "$BASE/api/business/erp/sales-target/achievement?period_code=2026-11&scope_type=ALL&basis=CASH" \
  -H "$AUTH" | tee -a "$OUT"

say '# 非法期间 2026-13'
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' \
  "$BASE/api/business/erp/sales-target/achievement?period_code=2026-13&scope_type=ALL" \
  -H "$AUTH" | tee -a "$OUT"

say '# ALL 带 scope_id（400）'
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' -X POST "$BASE/api/business/erp/sales-target" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"scope_type\":\"ALL\",\"scope_id\":\"$SALES_A\",\"period_code\":\"2026-12\",\"target_amount\":\"1.00\"}" | tee -a "$OUT"

say '# 版本号 0（400，不能靠库侧 CHECK 变成 500）'
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' -X POST "$BASE/api/business/erp/sales-forecast" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"period_code":"2026-12","forecast_amount":"1.00","version_no":0}' | tee -a "$OUT"

say '# 无 token（401）'
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' \
  "$BASE/api/business/erp/sales-target/achievement?period_code=2026-11&scope_type=ALL" | tee -a "$OUT"

# ---------- ⑤ 清理 curl 造的两条 12 月数据 ----------
if [ -n "${TARGET_ID:-}" ]; then
  say "# 清理：DELETE /sales-target/$TARGET_ID"
  curl -sS -o /dev/null -w 'HTTP %{http_code}\n' -X DELETE \
    "$BASE/api/business/erp/sales-target/$TARGET_ID" -H "$AUTH" | tee -a "$OUT"
fi
FORECAST_ID=$(curl -sS "$BASE/api/business/erp/sales-forecast/page?period_code=2026-12" -H "$AUTH" \
  | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1)
if [ -n "${FORECAST_ID:-}" ]; then
  say "# 清理：DELETE /sales-forecast/$FORECAST_ID"
  curl -sS -o /dev/null -w 'HTTP %{http_code}\n' -X DELETE \
    "$BASE/api/business/erp/sales-forecast/$FORECAST_ID" -H "$AUTH" | tee -a "$OUT"
fi

echo "证据已写入 $OUT"
