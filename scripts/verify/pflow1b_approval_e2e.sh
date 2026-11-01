#!/usr/bin/env bash
# ======================================================
# P-FLOW-1B 收尾 · 真接口验收：「审批中（王五）」
#
# 验的是 `GET /business/task/{id}/flow` 的 `approval` 段：
#   ① `on_complete=APPROVE` 的节点，任务完成 → `approval.status=PENDING` + 待办人非空；
#   ② 审批通过 → `status=APPROVED` + `pending_approvers=[]`；
#   ③ `on_complete=DIRECT` 的节点 → `approval=null`。
#
# 跑法（隔离实例已起，库已迁移）：
#   BASE=http://127.0.0.1:18096/api PFLOW_DB_URL=... bash scripts/verify/pflow1b_approval_e2e.sh
#
# 依赖：curl / jq / psql；`PFLOW_DB_URL` 指向本实例用的库（脚本要读任务 id 与审批待办 id）。
# 本脚本只造数据、不清理（库是一次性隔离库）；重跑会自动换一套 flow_code / biz_type。
# ======================================================
set -uo pipefail

BASE="${BASE:-http://127.0.0.1:18096/api}"
: "${PFLOW_DB_URL:?需要导出 PFLOW_DB_URL（本实例的库）}"
ADMIN_USER="${B3F8_ADMIN_USER:-admin}"
ADMIN_PASS_ENV="${B3F8_ADMIN_PASS_ENV:-B3f8-Admin-Pass-1}"
ADMIN_PASS_NEW="${B3F8_ADMIN_PASS_NEW:-B3f8-Admin-Pass-2}"
APPROVER_USER="${B3F8_APPROVER:-b3f8_appr}"
APPROVER_NAME="王五"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$*"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$*"; }
assert_eq() {
  local label="$1" exp="$2" act="$3"
  if [ "$exp" = "$act" ]; then ok "$label = $act"; else bad "$label 期望[$exp] 实际[$act]"; fi
}
assert_ne() {
  local label="$1" nope="$2" act="$3"
  if [ "$act" != "$nope" ]; then ok "$label = $act"; else bad "$label 不应为[$nope]"; fi
}
psql_q() { psql "$PFLOW_DB_URL" -tAc "$1" | head -1; }
jqr() { printf '%s' "$1" | jq -r "$2"; }

post() { # $1 path  $2 json  [$3 token]
  curl -s -X POST "$BASE$1" -H "Authorization: Bearer ${3:-$TOKEN}" \
    -H 'Content-Type: application/json' -d "$2"
}
put() {
  curl -s -X PUT "$BASE$1" -H "Authorization: Bearer ${3:-$TOKEN}" \
    -H 'Content-Type: application/json' -d "$2"
}
patch() {
  curl -s -X PATCH "$BASE$1" -H "Authorization: Bearer ${3:-$TOKEN}" \
    -H 'Content-Type: application/json' -d "$2"
}
get() { curl -s "$BASE$1" -H "Authorization: Bearer ${2:-$TOKEN}"; }

login_raw() { # $1 user $2 pass
  curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}"
}

# ---------- ① 登录 admin（首跑要过「首登强制改密」） ----------
TOKEN=""
BODY=$(login_raw "$ADMIN_USER" "$ADMIN_PASS_NEW")
TOKEN=$(jqr "$BODY" '.data.access_token // empty')
if [ -z "$TOKEN" ]; then
  BODY=$(login_raw "$ADMIN_USER" "$ADMIN_PASS_ENV")
  TOKEN=$(jqr "$BODY" '.data.access_token // empty')
  if [ -z "$TOKEN" ]; then
    bad "admin 登录失败：$(jqr "$BODY" '.msg // .detail // "?"')"
    printf '\n==> 汇总：PASS=%d FAIL=%d\n' "$PASS" "$FAIL"
    exit 1
  fi
  CHG=$(put "/system/auth/password" "{\"old_password\":\"$ADMIN_PASS_ENV\",\"new_password\":\"$ADMIN_PASS_NEW\"}" "$TOKEN")
  assert_eq "首登改密" "0" "$(jqr "$CHG" '.code // 0')"
  BODY=$(login_raw "$ADMIN_USER" "$ADMIN_PASS_NEW")
  TOKEN=$(jqr "$BODY" '.data.access_token // empty')
  assert_ne "改密后用新口令重新登录拿到令牌" "" "$TOKEN"
else
  ok "admin 用已改过的口令登录"
fi

# ---------- ② 审批人账号（王五）：同一个口令哈希 + 超管角色 ----------
APPR_ID=$(psql_q "SELECT id FROM sys_user WHERE username='$APPROVER_USER' AND deleted_at IS NULL")
if [ -z "$APPR_ID" ]; then
  APPR_ID=$(psql_q "INSERT INTO sys_user (id, username, password, real_name, status, user_type) \
                    VALUES (gen_random_uuid(), '$APPROVER_USER', \
                            (SELECT password FROM sys_user WHERE username='$ADMIN_USER'), \
                            '$APPROVER_NAME', 1, 'HUMAN') RETURNING id")
  psql -q "$PFLOW_DB_URL" -c "INSERT INTO sys_user_role (id, user_id, role_id) \
      SELECT gen_random_uuid(), '$APPR_ID'::uuid, role_id FROM sys_user_role ur \
        JOIN sys_user u ON u.id = ur.user_id WHERE u.username='$ADMIN_USER' LIMIT 1" > /dev/null
  ok "造审批人 $APPROVER_USER（id=$APPR_ID）"
else
  ok "审批人 $APPROVER_USER 已存在（id=$APPR_ID）"
fi
APPR_TOKEN=$(jqr "$(login_raw "$APPROVER_USER" "$ADMIN_PASS_NEW")" '.data.access_token // empty')
assert_ne "审批人登录拿到令牌" "" "$APPR_TOKEN"

# ---------- ③ 目标审批流程（biz_type 独立、已发布且默认） ----------
SUF=$(date +%H%M%S)
APPR_BIZ="PFLOW_APPR_E2E_$SUF"
DEF=$(post "/business/flow/def" "{\"flow_code\":\"pflow_e2e_appr_$SUF\",\"flow_name\":\"E2E 节点审批（$SUF）\",\"biz_type\":\"$APPR_BIZ\",\"is_default\":true,\"allow_auto_approve\":false}")
APPR_DEF_ID=$(jqr "$DEF" '.data.id // empty')
APPR_VER_ID=$(jqr "$DEF" '.data.version_id // empty')
assert_ne "建审批流程 def" "" "$APPR_DEF_ID"

APPR_GRAPH=$(cat <<JSON
{"nodes":[
  {"key":"n_start","name":"开始","type":"START"},
  {"key":"n_appr","name":"主管审批","type":"APPROVAL","assignee_type":"USER",
   "assignees":[{"type":"USER","ref_id":"$APPR_ID","sort_order":1}],
   "empty_assignee":"ERROR","timeout_action":"REMIND"},
  {"key":"n_end","name":"结束","type":"END"}
],"edges":[{"key":"e1","from":"n_start","to":"n_appr"},{"key":"e2","from":"n_appr","to":"n_end"}]}
JSON
)
SAVED=$(put "/business/flow/version/$APPR_VER_ID/graph" "$APPR_GRAPH")
assert_eq "保存审批流程图" "0" "$(jqr "$SAVED" '.code // 0')"
PUB=$(post "/business/flow/def/$APPR_DEF_ID/publish" '{}')
assert_eq "发布审批流程" "0" "$(jqr "$PUB" '.code // 0')"

# ---------- ④ 任务流程：on_complete=APPROVE ----------
TDEF=$(post "/business/flow/def" "{\"flow_code\":\"pflow_e2e_task_$SUF\",\"flow_name\":\"E2E 催货流程（$SUF）\",\"biz_type\":\"PFLOW_E2E_TASK_$SUF\",\"is_default\":true,\"allow_auto_approve\":false}")
TASK_DEF_ID=$(jqr "$TDEF" '.data.id // empty')
TASK_VER_ID=$(jqr "$TDEF" '.data.version_id // empty')
assert_ne "建任务流程 def" "" "$TASK_DEF_ID"

TASK_GRAPH=$(cat <<JSON
{"nodes":[
  {"key":"n_start","name":"开始","type":"START"},
  {"key":"n_task","name":"催货","type":"TASK_NODE","assignee_type":"INITIATOR",
   "empty_assignee":"ERROR","timeout_action":"REMIND",
   "config":{"task":{"title_template":"{{instance.biz_no}} 催货（第1轮）",
                     "on_complete":"APPROVE","approve_biz_type":"$APPR_BIZ",
                     "on_reject":"BACK_TO_PREV","on_cancel":"TERMINATE"}}},
  {"key":"n_end","name":"结束","type":"END"}
],"edges":[{"key":"e1","from":"n_start","to":"n_task"},{"key":"e2","from":"n_task","to":"n_end"}]}
JSON
)
SAVED=$(put "/business/flow/version/$TASK_VER_ID/graph" "$TASK_GRAPH")
assert_eq "保存任务流程图" "0" "$(jqr "$SAVED" '.code // 0')"
PUB=$(post "/business/flow/def/$TASK_DEF_ID/publish" '{}')
assert_eq "发布任务流程（R18 通过）" "0" "$(jqr "$PUB" '.code // 0')"

# ---------- ⑤ 发起 → 生成任务 ----------
STARTED=$(post "/business/flow/instance" "{\"def_id\":\"$TASK_DEF_ID\",\"title\":\"E2E 审批进度 $SUF\",\"biz_no\":\"PO-E2E-$SUF\",\"form_data\":{}}")
assert_eq "发起流程" "0" "$(jqr "$STARTED" '.code // 0')"
INST=$(psql_q "SELECT id FROM biz_flow_instance WHERE def_id='$TASK_DEF_ID'::uuid AND deleted_at IS NULL ORDER BY started_at DESC LIMIT 1")
TASK_ID=$(psql_q "SELECT id FROM biz_task WHERE flow_instance_id='$INST'::uuid AND deleted_at IS NULL ORDER BY created_at DESC LIMIT 1")
assert_ne "生成的任务 id" "" "$TASK_ID"

FLOW=$(get "/business/task/$TASK_ID/flow")
assert_eq "完成前 approval" "null" "$(jqr "$FLOW" '.data.flow.approval')"
assert_eq "on_complete" "APPROVE" "$(jqr "$FLOW" '.data.flow.on_complete')"
assert_eq "node_status" "ACTIVE" "$(jqr "$FLOW" '.data.flow.node_status')"

# ---------- ⑥ 任务 TODO → DOING → DONE ----------
R1=$(patch "/business/task/$TASK_ID/status" '{"to":"DOING"}')
assert_eq "任务 → DOING" "0" "$(jqr "$R1" '.code // 0')"
R2=$(patch "/business/task/$TASK_ID/status" '{"to":"DONE"}')
assert_eq "任务 → DONE（节点保持 ACTIVE 并发起审批）" "0" "$(jqr "$R2" '.code // 0')"
assert_eq "库里任务状态" "DONE" "$(psql_q "SELECT status FROM biz_task WHERE id='$TASK_ID'::uuid")"

FLOW=$(get "/business/task/$TASK_ID/flow")
printf '\n---- curl 回执 ①：任务已完成、等审批 ----\n'
printf '%s\n' "$FLOW" | jq '.data.flow | {node_key, node_name, node_status, round, on_complete, approval}'
APPR_STATUS=$(jqr "$FLOW" '.data.flow.approval.status')
APPR_NO=$(jqr "$FLOW" '.data.flow.approval.instance_no')
APPR_IID=$(jqr "$FLOW" '.data.flow.approval.instance_id')
APPR_PEOPLE=$(jqr "$FLOW" '.data.flow.approval.pending_approvers | join(",")')
assert_eq "approval.status" "PENDING" "$APPR_STATUS"
assert_eq "approval.pending_approvers" "$APPROVER_NAME" "$APPR_PEOPLE"
assert_eq "approval.instance_no 非空" "0" "$([ -z "$APPR_NO" ] && echo 1 || echo 0)"
assert_eq "flow.node_status（等审批时仍是 ACTIVE）" "ACTIVE" "$(jqr "$FLOW" '.data.flow.node_status')"

# ---------- ⑦ 审批通过 → APPROVED + 待办清空 ----------
FT=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$APPR_IID'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
assert_ne "审批待办 id" "" "$FT"
APPROVED=$(post "/business/flow/task/$FT/approve" '{"comment":"同意"}' "$APPR_TOKEN")
assert_eq "审批同意" "0" "$(jqr "$APPROVED" '.code // 0')"

FLOW=$(get "/business/task/$TASK_ID/flow")
printf '\n---- curl 回执 ②：审批已通过 ----\n'
printf '%s\n' "$FLOW" | jq '.data.flow | {node_status, on_complete, approval}'
assert_eq "approval.status" "APPROVED" "$(jqr "$FLOW" '.data.flow.approval.status')"
assert_eq "approval.pending_approvers（终态清空）" "[]" "$(jqr "$FLOW" '.data.flow.approval.pending_approvers | tostring')"
assert_eq "同一个任务仍指向同一张审批单" "$APPR_IID" "$(jqr "$FLOW" '.data.flow.approval.instance_id')"
assert_eq "库里审批实例状态" "APPROVED" "$(psql_q "SELECT status FROM biz_flow_instance WHERE id='$APPR_IID'::uuid")"

# ---------- ⑧ 反例：on_complete=DIRECT → approval=null ----------
DDEF=$(post "/business/flow/def" "{\"flow_code\":\"pflow_e2e_direct_$SUF\",\"flow_name\":\"E2E 直完成流程（$SUF）\",\"biz_type\":\"PFLOW_E2E_DIRECT_$SUF\",\"is_default\":true,\"allow_auto_approve\":false}")
D_DEF_ID=$(jqr "$DDEF" '.data.id // empty')
D_VER_ID=$(jqr "$DDEF" '.data.version_id // empty')
D_GRAPH=$(cat <<JSON
{"nodes":[
  {"key":"n_start","name":"开始","type":"START"},
  {"key":"n_task","name":"直接完成","type":"TASK_NODE","assignee_type":"INITIATOR",
   "empty_assignee":"ERROR","timeout_action":"REMIND",
   "config":{"task":{"title_template":"{{instance.biz_no}} 直接完成",
                     "on_complete":"DIRECT","on_reject":"BACK_TO_PREV","on_cancel":"TERMINATE"}}},
  {"key":"n_end","name":"结束","type":"END"}
],"edges":[{"key":"e1","from":"n_start","to":"n_task"},{"key":"e2","from":"n_task","to":"n_end"}]}
JSON
)
SAVED=$(put "/business/flow/version/$D_VER_ID/graph" "$D_GRAPH")
assert_eq "保存 DIRECT 流程图" "0" "$(jqr "$SAVED" '.code // 0')"
PUB=$(post "/business/flow/def/$D_DEF_ID/publish" '{}')
assert_eq "发布 DIRECT 流程" "0" "$(jqr "$PUB" '.code // 0')"
STARTED=$(post "/business/flow/instance" "{\"def_id\":\"$D_DEF_ID\",\"title\":\"E2E DIRECT $SUF\",\"biz_no\":\"PO-E2E-D-$SUF\",\"form_data\":{}}")
assert_eq "发起 DIRECT 流程" "0" "$(jqr "$STARTED" '.code // 0')"
D_INST=$(psql_q "SELECT id FROM biz_flow_instance WHERE def_id='$D_DEF_ID'::uuid AND deleted_at IS NULL ORDER BY started_at DESC LIMIT 1")
D_TASK=$(psql_q "SELECT id FROM biz_task WHERE flow_instance_id='$D_INST'::uuid AND deleted_at IS NULL ORDER BY created_at DESC LIMIT 1")
assert_ne "DIRECT 任务 id" "" "$D_TASK"
patch "/business/task/$D_TASK/status" '{"to":"DOING"}' > /dev/null
R=$(patch "/business/task/$D_TASK/status" '{"to":"DONE"}')
assert_eq "DIRECT 任务 → DONE" "0" "$(jqr "$R" '.code // 0')"

FLOW=$(get "/business/task/$D_TASK/flow")
printf '\n---- curl 回执 ③：DIRECT 节点（无审批） ----\n'
printf '%s\n' "$FLOW" | jq '.data.flow | {node_status, on_complete, approval}'
assert_eq "approval（DIRECT → null）" "null" "$(jqr "$FLOW" '.data.flow.approval')"
assert_eq "DIRECT 完成后节点推进" "APPROVED" "$(jqr "$FLOW" '.data.flow.node_status')"
assert_eq "库里任务状态" "DONE" "$(psql_q "SELECT status FROM biz_task WHERE id='$D_TASK'::uuid")"

printf '\n==> 汇总：PASS=%d FAIL=%d\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
