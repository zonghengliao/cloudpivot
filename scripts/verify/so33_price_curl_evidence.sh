#!/usr/bin/env bash
# SO-33 取价解析 —— **原样 curl 留证**（卡面「真实 token curl 实测 5 种场景」）
#
# 前置：
#   bash scripts/verify/so33_instance.sh up
#   PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so33 \
#       -v ON_ERROR_STOP=1 -f scripts/verify/so33_price_fixture.sql
#
# 用法：bash scripts/verify/so33_price_curl_evidence.sh | tee docs/evidence/…/curl_transcript.txt
#
# 输出里每条都带**原始 HTTP 响应体**（未经过任何解析），方便人工复核字段名与金额格式；
# 断言式验收在 so33_price_resolve_e2e.py（22 条），本脚本只负责「给人看的现场」。
set -uo pipefail
BASE=${SO33_BASE:-http://127.0.0.1:18239}
RESOLVE="$BASE/api/business/erp/price/resolve"

C1=f0330000-0000-4000-8000-000000000001
C2=f0330000-0000-4000-8000-000000000002
M1=f0330000-0000-4000-8000-000000000011
M2=f0330000-0000-4000-8000-000000000012
M3=f0330000-0000-4000-8000-000000000013
M4=f0330000-0000-4000-8000-000000000014

echo "### 0. 登录（admin / SUPER_ADMIN）"
LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}')
echo "$LOGIN" | grep -o '"code":[0-9]*' | head -1
# access_token 原样取（不在输出里打印完整令牌：只打前 16 位 + 长度）
TOKEN=$(echo "$LOGIN" | grep -o '"access_token":"[^"]*"' | cut -d'"' -f4)
echo "token: ${TOKEN:0:16}…（长度 ${#TOKEN}）"
[ -z "$TOKEN" ] && { echo "登录失败，中止"; exit 1; }

run() {
  local label="$1"; shift
  local url="$1"
  echo
  echo "### $label"
  echo "\$ curl -s -H 'Authorization: Bearer \$TOKEN' '$url'"
  curl -s -H "Authorization: Bearer $TOKEN" "$url"
  echo
}

echo
echo "=========================================================="
echo " A. 5 级各级命中"
echo "=========================================================="
run "① AGREED：C1×M1 qty=150（三条满足起订量，取 min_qty=100 那条 = 9.0000）" \
    "$RESOLVE?customer_id=$C1&material_id=$M1&qty=150&date=2026-01-01"
run "② TIER_CUSTOMER：C1×M2 qty=50（专属 PROMO 6.5 压过同段 TIER 7.0）" \
    "$RESOLVE?customer_id=$C1&material_id=$M2&qty=50&date=2026-01-01"
run "③ TIER_COMMON：C2×M2 qty=50（C2 无专属档位 → 通用 8.0000）" \
    "$RESOLVE?customer_id=$C2&material_id=$M2&qty=50&date=2026-01-01"
run "④ STANDARD：C2×M3 qty=10（物料标准价 12.5000，source_id = 物料 id）" \
    "$RESOLVE?customer_id=$C2&material_id=$M3&qty=10&date=2026-01-01"
run "⑤ NONE：C2×M4 qty=10（无协议价 / 无档位 / 标准价 NULL）" \
    "$RESOLVE?customer_id=$C2&material_id=$M4&qty=10&date=2026-01-01"

echo
echo "=========================================================="
echo " B. 边界（有效期 / 档位区间 / 多条命中 / 停用行）"
echo "=========================================================="
run "有效期：effective_to 当天仍生效（2026-03-31 → 8.5000）" \
    "$RESOLVE?customer_id=$C1&material_id=$M1&qty=250&date=2026-03-31"
run "有效期：effective_to 次日失效并降级（2026-04-01 → 9.0000）" \
    "$RESOLVE?customer_id=$C1&material_id=$M1&qty=250&date=2026-04-01"
run "有效期：未到 effective_from 不参与（2026-05-31 → 旧价 9.8000）" \
    "$RESOLVE?customer_id=$C1&material_id=$M1&qty=60&date=2026-05-31"
run "多条协议价：同 min_qty=50 取最近生效（2026-07-01 → 9.6000）" \
    "$RESOLVE?customer_id=$C1&material_id=$M1&qty=60&date=2026-07-01"
run "档位区间左闭右开：qty == max_qty(100) 不命中档位（→ 通用 8.0000）" \
    "$RESOLVE?customer_id=$C1&material_id=$M2&qty=100&date=2026-01-01"
run "停用行不参与：qty=300 未命中 INACTIVE 的 1.0000（→ 8.5000）" \
    "$RESOLVE?customer_id=$C1&material_id=$M1&qty=300&date=2026-01-01"
run "不传 customer_id：跳过 ①②（→ 通用阶梯 8.0000）" \
    "$RESOLVE?material_id=$M2&qty=50&date=2026-01-01"

echo
echo "=========================================================="
echo " C. 失败路径"
echo "=========================================================="
run "缺 qty（→ 400 + 10001）" \
    "$RESOLVE?customer_id=$C1&material_id=$M1&date=2026-01-01"
run "物料不存在（→ 404 + 10002）" \
    "$RESOLVE?customer_id=$C1&material_id=00000000-0000-4000-8000-0000000000ff&qty=1&date=2026-01-01"
echo
echo "### 无 token（→ 401）"
echo "\$ curl -s '$RESOLVE?material_id=$M1&qty=1'"
curl -s "$RESOLVE?material_id=$M1&qty=1"
echo
