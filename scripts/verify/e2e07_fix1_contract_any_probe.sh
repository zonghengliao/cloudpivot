#!/usr/bin/env bash
# =============================================================================
# e2e07_fix1_contract_any_probe.sh —— 合同审批「角色型审批改或签」的真机验收（145）
#
# 卡：t_8733a7a0 / E2E07-FIX-1 ④
#
# 验什么（真 token、真 HTTP、真库，全部断言打印 PASS/FAIL）
#   1. 创建一张 5 万元采购合同（< 10 万 ⇒ 命中 n_pm 单级）并提交审批
#   2. n_pm 解析出的待办数 = PURCHASE_MANAGER 的可用成员数（改造前 = 3 人全会签）
#   3. 实例节点 n_pm 的 approval_mode 快照是 ANY（V0359 生效的直接证据）
#   4. **只批一张**（不需要凑齐 3 票）→ 节点通过
#   5. 合同从 PENDING_APPROVAL 变 SIGNED、流程实例 APPROVED，n_pm 上再无 PENDING 待办
#      （其余成员的待办被引擎收口成非 PENDING，不再是「等他们几个人」的死结）
#
# 期望：PASS=N FAIL=0，退出码 0。任何一条 FAIL 退出码 1。
# 运行环境：145（BASE 默认 http://127.0.0.1:8033/api），库 cloudpivot_dev。
#   账号口令来自 /etc/cloudpivot/cloudpivot-dev.env 的 NIK_ADMIN_PASSWORD 与历史夹具口令。
# =============================================================================
set -uo pipefail
ENV_FILE="${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"
set -a; . "$ENV_FILE"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK="${WORK:-/tmp/e2e07fix1}"; mkdir -p "$WORK/raw"
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  ❌ %s\n' "$*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
say()  { printf '%s\n' "$*"; }
SQ()   { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -X -q -tA -F'|' -c "$1"; }

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

RUN=$(date +%m%d%H%M)
say "### 0) 登录"
T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD"); [ -n "$T_ADMIN" ] && ok "admin 登录" || { bad "admin 登录失败"; exit 1; }
T_PM=$(login e2e01_appr 'E2e01@Appr123'); [ -n "$T_PM" ] && ok "e2e01_appr（PURCHASE_MANAGER 成员）登录" || { bad "e2e01_appr 登录失败"; exit 1; }
PM_ID=$(SQ "select id from sys_user where username='e2e01_appr' and deleted_at is null")

say "### 1) 流程版本与节点口径（V0359 之后的库内事实）"
VSN=$(SQ "select v.version_no from biz_flow_def d join biz_flow_def_version v on v.id=d.current_version_id where d.flow_code='contract_approve'")
check "contract_approve 当前版本号（V0359 发布后 = 2）" "$VSN" "2"
NPM_MODE=$(SQ "select n.approval_mode from biz_flow_def d join biz_flow_def_version v on v.id=d.current_version_id join biz_flow_node n on n.version_id=v.id where d.flow_code='contract_approve' and n.node_key='n_pm'")
check "n_pm 的 approval_mode" "$NPM_MODE" "ANY"
PM_CANDS=$(SQ "select count(distinct u.id) from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id and d.flow_code='contract_approve' and v.is_current join sys_user_role ur on ur.role_id=a.ref_id join sys_user u on u.id=ur.user_id where a.assignee_type='ROLE' and n.node_key='n_pm' and u.deleted_at is null and u.status=1")
say "     （n_pm 的候选人数 = $PM_CANDS —— 改造前这 N 个人必须全部签）"

say "### 2) 建单 + 提交审批（5 万 → 命中 n_pm）"
SUP_ID=$(SQ "select id from biz_supplier where deleted_at is null and status=1 order by supplier_code limit 1")
OWNER_ID=$(SQ "select id from sys_user where username='admin'")
api create POST /business/erp/contract "$T_ADMIN" \
  "{\"name\":\"E2E07FIX1-ANY-$RUN\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP_ID\",\"amount\":50000,\"sign_date\":\"2026-11-09\",\"start_date\":\"2026-11-09\",\"end_date\":\"2027-11-08\",\"owner_id\":\"$OWNER_ID\"}"
C1=$(data '.data.id')
check "建单 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
[ -z "$C1" ] && { bad "建单没拿到 id"; exit 1; }
api submit PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"PENDING_APPROVAL"}'
check "提交审批 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "合同状态" "$(SQ "select status from biz_contract where id='$C1'")" "PENDING_APPROVAL"

say "### 3) 待办与快照（改造后的关键事实）"
I1=$(SQ "select id from biz_flow_instance where flow_code='contract_approve' and biz_id='$C1' order by created_at desc limit 1")
[ -z "$I1" ] && { bad "没找到流程实例"; exit 1; }
N_TODO=$(SQ "select count(*) from biz_flow_task where instance_id='$I1' and node_key='n_pm' and status='PENDING'")
check "n_pm 生成的待办数（= 候选人数，引擎按人派单）" "$N_TODO" "$PM_CANDS"
check "实例节点 n_pm 的快照 approval_mode" "$(SQ "select approval_mode from biz_flow_instance_node where instance_id='$I1' and node_key='n_pm'")" "ANY"
check "实例状态" "$(SQ "select status from biz_flow_instance where id='$I1'")" "PENDING"

say "### 4) 只批一张（e2e01_appr 那一票）"
TID=$(SQ "select id from biz_flow_task where instance_id='$I1' and node_key='n_pm' and status='PENDING' and assignee_id='$PM_ID' limit 1")
[ -z "$TID" ] && { bad "e2e01_appr 名下没有待办"; exit 1; }
api approve POST "/business/flow/task/$TID/approve" "$T_PM" '{"comment":"E2E07-FIX-1 或签验证：单人同意即通过"}'
check "单人审批 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
if [ "$N_TODO" -gt 1 ]; then
  ok "确实存在 $N_TODO 条待办（改造前需要 $N_TODO 个人都签才算过）"
else
  bad "候选人多于 1 时应有 >1 条待办（实际 $N_TODO）"
fi

say "### 5) 结果：不再卡死"
check "流程实例状态（一票即 APPROVED）" "$(SQ "select status from biz_flow_instance where id='$I1'")" "APPROVED"
check "合同状态（APPROVED → SIGNED）" "$(SQ "select status from biz_contract where id='$C1'")" "SIGNED"
check "n_pm 上剩余 PENDING 待办（应为 0）" "$(SQ "select count(*) from biz_flow_task where instance_id='$I1' and node_key='n_pm' and status='PENDING'")" "0"
say "     · n_pm 每张待办的最终状态："
SQ "select '       ' || coalesce(u.username,'-') || ' → ' || t.status from biz_flow_task t left join sys_user u on u.id=t.assignee_id where t.instance_id='$I1' and t.node_key='n_pm' order by u.username" | sed 's/^ *//;s/^/       /'
say "     · 合同 $C1（E2E07FIX1-ANY-$RUN）留作演练证据，不删除"

say ""
say "== PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
