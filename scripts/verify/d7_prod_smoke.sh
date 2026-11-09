#!/usr/bin/env bash
# D7 生产只读冒烟（144）：登录后用**不会写库**的入参打批量接口
#   · 空 ids → 400（参数闸）
#   · 不存在的 id → 200 + 1 条失败（逐条报告；不写任何单据）
#   · 批量导出：ids 空且无筛选 → 400（不许全表导出，也不会建任务）
#   ⚠ 现在登录用运维账号（CLOUDPIVOT_OPS_*，不是 SUPER_ADMIN），所以「导出」那条
#     可能先撞权限闸 403（业务码 20002，缺 business:impexp:export:create）——
#     403 = 该账号无此权限（预期内），400 = 参数闸生效；两者都不是 500，都没有写库。
#     要验导出参数闸本身，需 SUPER_ADMIN 会话（人工注入 CLOUDPIVOT_ADMIN_PASSWORD）。卡 t_d4e92b99。
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
# 登录口径：运维账号（CLOUDPIVOT_OPS_*），单次尝试、失败即停 —— 见 docs/运维规程-凭据与自查账号.md
for C in "${OPS_CRED_LIB:-}" /root/ops_cred.sh "$(dirname "$0")/../ops/ops_cred.sh"; do
  [ -n "$C" ] && [ -r "$C" ] && { . "$C"; break; }
done
command -v ops_cred_load >/dev/null || { echo "缺 ops_cred.sh（scripts/ops/ops_cred.sh 或 /root/ops_cred.sh）"; exit 2; }
ops_cred_load "${OPS_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}" || exit 2
BASE=http://127.0.0.1:8023
GHOST=d7e2e0ff-0000-4000-8000-00000000ffff

LOGIN=$(curl -s -m 10 -X POST -H 'Content-Type: application/json' \
  -d "$(jq -nc --arg u "$OPS_CRED_USER" --arg p "$OPS_CRED_PASS" '{username:$u,password:$p}')" "$BASE/api/system/auth/login")
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
