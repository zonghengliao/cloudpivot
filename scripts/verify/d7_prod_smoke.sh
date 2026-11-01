#!/usr/bin/env bash
# D7 生产只读冒烟（144）：登录后用**不会写库**的入参打批量接口
#   · 空 ids → 400（参数闸）
#   · 不存在的 id → 200 + 1 条失败（逐条报告；不写任何单据）
#   · 批量导出：ids 空且无筛选 → 400（不许全表导出，也不会建任务）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
BASE=http://127.0.0.1:8023
GHOST=d7e2e0ff-0000-4000-8000-00000000ffff

LOGIN=$(curl -s -m 10 -X POST -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" "$BASE/api/system/auth/login")
TOKEN=$(printf '%s' "$LOGIN" | python3 -c 'import json,sys
try:
    print(json.load(sys.stdin)["data"]["access_token"])
except Exception:
    print("")')
if [ -z "$TOKEN" ]; then
  echo "LOGIN_FAILED: $(printf '%s' "$LOGIN" | head -c 200)"
  exit 1
fi
echo "login=OK"

post() { # $1=path $2=body
  curl -s -m 15 -o /tmp/d7smoke.out -w '%{http_code}' -X POST \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "$2" "$BASE$1"
  echo "  ← $1"
  head -c 400 /tmp/d7smoke.out; echo
}

echo "-- 1) 收货单批量过账：空 ids（应 400 + 10001）"
post /api/business/erp/stock-in/batch-status '{"ids":[],"status":"POSTED"}'

echo "-- 2) 收货单批量过账：不存在的 id（应 200 + 1 条失败 10002）"
post /api/business/erp/stock-in/batch-status "{\"ids\":[\"$GHOST\"],\"status\":\"POSTED\"}"

echo "-- 3) 批量导出：ids 空且无筛选（应 400，且不建任务）"
post /api/business/erp/stock-in/batch-export '{"ids":[]}'

echo "-- 4) 审批中心批量同意：不存在的待办（应 200 + 1 条失败）"
post /api/business/flow/task/batch-approve "{\"ids\":[\"$GHOST\"],\"decision\":\"APPROVE\"}"

echo "-- 5) 任务批量改状态：不存在的任务（应 200 + 1 条失败）"
post /api/business/task/batch-status "{\"ids\":[\"$GHOST\"],\"status\":\"DONE\"}"
