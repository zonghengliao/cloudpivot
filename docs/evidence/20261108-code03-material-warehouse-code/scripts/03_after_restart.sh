#!/bin/bash
# CODE-03 重启续号验证：实例重启后号段**不重置**（主数据号段 period_key='GLOBAL'）
#   - 前置：01_curl.sh 已跑完（MAT/WH current_no 都到 3）
#   - 期望：重启后再自动取号 = MAT-0004 / WH-04（而不是回到 0001/01）
# 用法：bash 03_after_restart.sh
set -uo pipefail
BASE=${BASE:-http://127.0.0.1:8043}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS2=${ADMIN_PASS2:-Admin@123456x}
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
SUM="$RAW/03_after_restart.txt"
: > "$SUM"
say() { echo "$@" | tee -a "$SUM"; }

FAILED=0
check() { if [ "$2" = "$3" ]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 期望[$2] 实际[$3]"; FAILED=1; fi; }

curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS2\"}" > "$RAW/10_relogin.json"
TOKEN=$(jq -r '.data.access_token // empty' "$RAW/10_relogin.json")
[ -n "$TOKEN" ] || { say "FATAL: 重启后登录失败"; cat "$RAW/10_relogin.json" | tee -a "$SUM"; exit 1; }
say "重启后 login ok（新 JWT 密钥，旧 token 已失效 → 这正是重启的旁证）"
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

UNIT_ID=$(jq -r '.data.id' "$RAW/01_unit.json")
curl -s -o "$RAW/20_mat_after_restart.json" -X POST "$BASE/api/business/erp/material" "${AUTH[@]}" \
  -d "{\"material_name\":\"CODE03 物料（重启后）\",\"unit_id\":\"$UNIT_ID\"}"
MAT=$(jq -r '.data.material_code // empty' "$RAW/20_mat_after_restart.json")
say "重启后自动取号物料 → material_code=$MAT"
check "重启不重置号段（MAT 续到 4）" "MAT-0004" "$MAT"

curl -s -o "$RAW/21_wh_after_restart.json" -X POST "$BASE/api/business/erp/warehouse" "${AUTH[@]}" \
  -d '{"warehouse_name":"CODE03 仓库（重启后）","level":1}'
WH=$(jq -r '.data.warehouse_code // empty' "$RAW/21_wh_after_restart.json")
say "重启后自动取号仓库 → warehouse_code=$WH"
check "重启不重置号段（WH 续到 4）" "WH-04" "$WH"

say ""
[ "$FAILED" = "0" ] && say "RESULT: ALL PASS" || say "RESULT: 有 FAIL"
exit "$FAILED"
