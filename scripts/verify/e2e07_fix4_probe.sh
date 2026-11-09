#!/usr/bin/env bash
# =============================================================================
# e2e07_fix4_probe.sh —— E2E07-FIX-4 的真机验收（145，真 token / 真 HTTP / 真库）
#
# 卡：t_e0b82129
#
# 逐条验（每条 PASS/FAIL，任一 FAIL 退出码 1）：
#   ① 路径②「登记签订」（PATCH SIGNED）
#        · 合同落 SIGNED，approval_instance_id 保留（审计）
#        · **审批实例终止**：status=CANCELLED、ended_at 非空、result_comment 写明原因
#        · **待办全部 CANCELLED**（PENDING=0，原来挂着的 N 条转 CANCELLED）
#        · 实例节点收口（无 ACTIVE/PENDING）
#        · **状态日志** reason 含「审批实例已终止」
#        · 重复 PATCH SIGNED → 409（不重复终止）
#   ② 归档字面量：target_status=CLOSED → 400 + 10001（合同没有 CLOSED）
#   ④ 归档不硬拦（设计决策 E4）：挂 1 张在途采购单仍可 ACTIVE → COMPLETED (200)，
#      且详情 open_order_count >= 1（前端「归档前提示」的数据源）
#
# 运行环境：145，隔离实例 BASE 默认 http://127.0.0.1:8034/api
#          库名由 DB 环境变量指定（默认 cloudpivot_fix4，owner cloudpivot）
# 夹具：合同 / 采购单 / 通知由本脚本自建，**留作证据不删除**（与 FIX-2/FIX-3 同口径）。
# =============================================================================
set -uo pipefail
ENV_FILE="${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"
set -a; . "$ENV_FILE"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8034/api}"
DB="${DB:-cloudpivot_fix4}"
WORK="${WORK:-/tmp/e2e07fix4}"; mkdir -p "$WORK/raw"
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  ❌ %s\n' "$*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
has()  { case "$2" in *"$3"*) ok "$1 含「$3」";; *) bad "$1 = 「$2」不含「$3」";; esac; }
say()  { printf '%s\n' "$*"; }
SQ()   { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$DB" -X -q -tA -F'|' -c "$1"; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$WORK/raw/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

login() { # login <用户名> <口令>
  api "login_$1" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  [ "$HTTP" = 200 ] && [ "$CODE" = 0 ] && data '.data.access_token'
}

RUN=$(date +%m%d%H%M%S)
say "### 0) 登录（真 token）"
T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD")
[ -n "$T_ADMIN" ] && ok "admin 登录" || { bad "admin 登录失败"; exit 1; }
ADMIN_ID=$(SQ "select id from sys_user where username='admin' and deleted_at is null")
SUP_ID=$(SQ "select id from biz_supplier where deleted_at is null and status=1 order by supplier_code limit 1")
WH_ID=$(SQ "select id from biz_warehouse where deleted_at is null order by warehouse_code limit 1")
MAT_ID=$(SQ "select id from biz_material where deleted_at is null order by material_code limit 1")
[ -n "$SUP_ID" ] && ok "供应商夹具 $SUP_ID" || { bad "库里没有可用供应商"; exit 1; }

# ---------------------------------------------------------------- ① 路径②
say "### 1) 建合同 + 提交审批（起真实例 + 真待办）"
api create POST /business/erp/contract "$T_ADMIN" \
  "{\"name\":\"E2E07FIX4-SIGN-$RUN\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP_ID\",\"amount\":50000,\"sign_date\":\"2026-11-09\",\"start_date\":\"2026-11-09\",\"end_date\":\"2027-11-08\",\"owner_id\":\"$ADMIN_ID\"}"
C1=$(data '.data.id')
check "建单 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
[ -z "$C1" ] || [ "$C1" = "null" ] && { bad "建单没拿到 id"; exit 1; }

api submit PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"PENDING_APPROVAL"}'
check "提交审批 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "合同状态（提交后）" "$(SQ "select status from biz_contract where id='$C1'")" "PENDING_APPROVAL"
INST=$(SQ "select approval_instance_id from biz_contract where id='$C1'")
[ -n "$INST" ] && ok "合同挂上实例 $INST" || { bad "提交后没挂实例"; exit 1; }
TASKS_BEFORE=$(SQ "select count(*) from biz_flow_task where instance_id='$INST' and status='PENDING'")
[ "$TASKS_BEFORE" -gt 0 ] && ok "实例上有 $TASKS_BEFORE 条 PENDING 待办（会签/审批真挂着）" \
  || bad "实例上没有 PENDING 待办，用例前提不成立"
check "实例状态（提交后）" "$(SQ "select status from biz_flow_instance where id='$INST'")" "PENDING"

say "### 2) 路径② 登记签订：PATCH {target_status:SIGNED}"
api sign PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"SIGNED"}'
check "登记签订 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "合同状态（签订后）" "$(SQ "select status from biz_contract where id='$C1'")" "SIGNED"
check "合同仍指向原实例（审计）" "$(SQ "select approval_instance_id from biz_contract where id='$C1'")" "$INST"

check "① 审批实例状态" "$(SQ "select status from biz_flow_instance where id='$INST'")" "CANCELLED"
check "① 实例 ended_at 已写" "$(SQ "select count(*) from biz_flow_instance where id='$INST' and ended_at is not null")" "1"
has "① 实例 result_comment" "$(SQ "select coalesce(result_comment,'') from biz_flow_instance where id='$INST'")" "审批中心外登记签订"
check "① 待办 PENDING（应为 0）" "$(SQ "select count(*) from biz_flow_task where instance_id='$INST' and status='PENDING'")" "0"
check "① 待办 CANCELLED（应=原条数）" "$(SQ "select count(*) from biz_flow_task where instance_id='$INST' and status='CANCELLED'")" "$TASKS_BEFORE"
check "① 实例节点未收口数（应为 0）" "$(SQ "select count(*) from biz_flow_instance_node where instance_id='$INST' and status in ('ACTIVE','PENDING')")" "0"
has "① 状态日志 reason" "$(SQ "select coalesce(reason,'') from biz_status_log where entity_type='CONTRACT' and entity_id='$C1' and to_status='SIGNED' order by created_at desc limit 1")" "审批实例已终止"

say "### 3) 已签再签（幂等闸）：应 409，且不重复终止"
api resign PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"SIGNED"}'
check "重复登记签订 HTTP" "$HTTP" "409"
check "合同状态没被改回" "$(SQ "select status from biz_contract where id='$C1'")" "SIGNED"

# ---------------------------------------------------------------- ② CLOSED
say "### 4) ② 归档字面量：CLOSED 不存在（应 400 + 10001）"
api closed PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"CLOSED"}'
check "CLOSED HTTP/CODE" "$HTTP/c$CODE" "400/c$CODE"
check "CLOSED 业务码" "$CODE" "10001"
check "合同状态没被改" "$(SQ "select status from biz_contract where id='$C1'")" "SIGNED"

# ---------------------------------------------------------------- ④ 归档提示源
say "### 5) ④ 归档不硬拦（E4）：挂 1 张在途采购单 → SIGNED/ACTIVE → COMPLETED 仍 200"
api activate PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check "生效 HTTP/CODE" "$HTTP/c$CODE" "200/c0"

PO_OK=0
if [ -n "$WH_ID" ] && [ -n "$MAT_ID" ]; then
  api po POST /business/erp/purchase-order "$T_ADMIN" \
    "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C1\",\"warehouse_id\":\"$WH_ID\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":2,\"price_excl\":100,\"tax_rate\":13}]}"
  if [ "$HTTP" = 200 ] && [ "$CODE" = 0 ]; then
    PO_OK=1
    ok "在途采购单已建（contract_id=$C1）"
  else
    bad "建采购单失败（HTTP=$HTTP code=$CODE）：$(printf '%s' "$BODY" | head -c 200)"
  fi
else
  bad "缺少仓库 / 物料夹具，无法造在途单据"
fi

api detail GET "/business/erp/contract/$C1" "$T_ADMIN"
check "详情 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
OPEN_CNT=$(data '.data.open_order_count')
if [ "$PO_OK" = 1 ]; then
  check "④ 详情 open_order_count（前端提示数据源）" "$OPEN_CNT" "1"
  check "④ 详情 open_order_nos 点名" "$(data '.data.open_order_nos | length')" "1"
else
  has "④ 详情返回 open_order_count 字段" "$(printf '%s' "$BODY" | jq -r '.data | has("open_order_count")')" "true"
fi
check "④ 库内未关闭单据数（与接口同口径）" \
  "$(SQ "select count(*) from biz_purchase_order where contract_id='$C1' and deleted_at is null and status not in ('RECEIVED','CLOSED','CANCELLED')")" "$OPEN_CNT"

api archive PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"COMPLETED"}'
check "④ 归档（COMPLETED）HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "④ 合同状态" "$(SQ "select status from biz_contract where id='$C1'")" "COMPLETED"

say ""
say "================ 结果：PASS=$PASS FAIL=$FAIL ================"
[ "$FAIL" -eq 0 ] || exit 1
