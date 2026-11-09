#!/usr/bin/env bash
# E2E-09 C：审批降级链（ARCH-8 / P3-02 / P3-03）
#   ① 自审 SKIP → 升级到上级角色（并留痕 escalated_from_role / escalate_reason）
#   ② 升级链二级上溯（角色 0 人 → 上级 0 人 → 再上级命中）
#   ③ 升级链走完仍无人 → 422 + 65018（报文写「升级链已走完」），且不留半截实例
#   ④ 兜底人接管（fallback_user_id）
#   ⑤ 兜底配错 → 422 + 65042（静默当「没配」会让运维以为兜住了）
#   ⑥ 升级链成环 → 422 + 65040
# 全部真 HTTP；DB 断言用 psql。夹具（角色/用户/流程）全部 API 建。
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e09_common.sh"

say "================ E2E-09 C：审批降级链（ARCH-8）================"
T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD") || { bad "admin 登录失败"; exit 1; }
save T_ADMIN "$T_ADMIN"

say ""
say "--- 0) 夹具：角色（含升级链）/ 用户 / 权限码 ---"
mk_role() { # CODE NAME → id
  api "role_$1" POST /system/role "$T_ADMIN" "{\"role_code\":\"$1\",\"role_name\":\"$2\",\"status\":1,\"sort_order\":960,\"data_scope\":1,\"escalation_role_id\":\"\"}"
  data '.data.id'
}
MAKER=$(mk_role E2E09_ESC_MAKER "演练制单人角色")
BOSS=$(mk_role E2E09_ESC_BOSS "演练主管角色")
DEAD=$(mk_role E2E09_ESC_DEAD "演练空角色中继")
EMPTY=$(mk_role E2E09_ESC_EMPTY "演练空角色链中")
DEAD2=$(mk_role E2E09_ESC_DEAD2 "演练死链起点")
EMPTY2=$(mk_role E2E09_ESC_EMPTY2 "演练死链终点")
SUBMIT=$(mk_role E2E09_ESC_SUBMIT "演练提交角色")
save ESC_MAKER "$MAKER"; save ESC_BOSS "$BOSS"; save ESC_DEAD "$DEAD"; save ESC_EMPTY "$EMPTY"
save ESC_DEAD2 "$DEAD2"; save ESC_EMPTY2 "$EMPTY2"; save ESC_SUBMIT "$SUBMIT"
check "7 个降级链角色建成" "$(printf '%s %s %s %s %s %s %s' "$MAKER" "$BOSS" "$DEAD" "$EMPTY" "$DEAD2" "$EMPTY2" "$SUBMIT" | grep -oE '[0-9a-f]{8}-' | wc -l | tr -d ' ')" 7

set_esc() { # ID CODE NAME PARENT → http
  api "esc_$1" PUT "/system/role/$1" "$T_ADMIN" "{\"role_code\":\"$2\",\"role_name\":\"$3\",\"status\":1,\"sort_order\":960,\"data_scope\":1,\"escalation_role_id\":\"$4\"}"
  printf '%s' "$HTTP"
}
check "MAKER 上级 = BOSS" "$(set_esc "$MAKER" E2E09_ESC_MAKER "演练制单人角色" "$BOSS")" 200
check "DEAD 上级 = EMPTY" "$(set_esc "$DEAD" E2E09_ESC_DEAD "演练空角色中继" "$EMPTY")" 200
check "EMPTY 上级 = BOSS（三级链的最后一跳）" "$(set_esc "$EMPTY" E2E09_ESC_EMPTY "演练空角色链中" "$BOSS")" 200
check "DEAD2 上级 = EMPTY2（死链）" "$(set_esc "$DEAD2" E2E09_ESC_DEAD2 "演练死链起点" "$EMPTY2")" 200

grant_codes() { # ROLE_ID CODE... → 覆盖码数
  local role=$1; shift
  local ids="" c
  for c in "$@"; do
    local one; one=$(menu_ids_by_code "$T_ADMIN" "$c")
    [ -n "$one" ] && ids="$ids $one"
  done
  api "grant_$role" PUT "/system/role/$role/menus" "$T_ADMIN" "{\"menu_ids\":$(printf '%s\n' $ids | jq -R . | jq -s -c 'unique')}"
  [ "$HTTP" = 200 ] || say "  ⚠ 授权失败 $role http=$HTTP $(show)"
  SQ "select count(distinct m.auth_code) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where rm.role_id='$role'::uuid"
}
check "MAKER 授 3 码（建/读/审批）" "$(grant_codes "$MAKER" business:flow:instance:create business:flow:instance:read business:flow:approve)" 3
check "SUBMIT 授 2 码（建/读）" "$(grant_codes "$SUBMIT" business:flow:instance:create business:flow:instance:read)" 2
check "BOSS 授 2 码（读/审批）" "$(grant_codes "$BOSS" business:flow:instance:read business:flow:approve)" 2

mk_user() { # USERNAME REAL ROLE_JSON → id
  api "user_$1" POST /system/user "$T_ADMIN" "{\"username\":\"$1\",\"password\":\"E2e09@Pass1\",\"real_name\":\"$2\",\"status\":1,\"role_ids\":$3}"
  data '.data.id'
}
U_MAKER=$(mk_user e2e09_esc_maker "演练制单人" "[\"$MAKER\"]")
U_BOSS1=$(mk_user e2e09_esc_boss1 "演练主管甲" "[\"$BOSS\"]")
U_BOSS2=$(mk_user e2e09_esc_boss2 "演练主管乙" "[\"$BOSS\"]")
U_SUB=$(mk_user e2e09_esc_sub "演练提交人" "[\"$SUBMIT\"]")
save ESC_U_MAKER "$U_MAKER"; save ESC_U_BOSS1 "$U_BOSS1"; save ESC_U_BOSS2 "$U_BOSS2"; save ESC_U_SUB "$U_SUB"
check "4 个降级链用户建成" "$(printf '%s %s %s %s' "$U_MAKER" "$U_BOSS1" "$U_BOSS2" "$U_SUB" | grep -oE '[0-9a-f]{8}-' | wc -l | tr -d ' ')" 4
check "MAKER 角色成员数 = 1（自审 SKIP 的前提）" "$(SQ "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$MAKER'::uuid and u.deleted_at is null")" 1
check "BOSS 角色成员数 = 2（升级后落 2 条待办）" "$(SQ "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$BOSS'::uuid and u.deleted_at is null")" 2
check "DEAD / DEAD2 角色 0 人" "$(SQ "select (select count(*) from sys_user_role where role_id='$DEAD'::uuid)::text||'/'||(select count(*) from sys_user_role where role_id='$DEAD2'::uuid)::text")" "0/0"

say ""
say "--- 0b) 建 5 张流程（START → APPROVAL(ROLE) → END，或签）---"
mk_flow() { # CODE NAME BIZ ROLE_ID SELF FALLBACK_USER → def_id
  local code=$1 name=$2 biz=$3 role=$4 self=$5 fb=${6:-}
  local def vid fbfrag=""
  api "def_$code" POST /business/flow/def "$T_ADMIN" "{\"flow_code\":\"$code\",\"flow_name\":\"$name\",\"biz_type\":\"$biz\",\"is_default\":true}"
  [ "$HTTP" = "200" ] || { say "  [curl] create def $code → http=$HTTP $(show)" >&2; return 1; }
  def=$(data '.data.id'); vid=$(data '.data.version_id')
  [ -n "$fb" ] && fbfrag=",\"fallback_user_id\":\"$fb\""
  api "graph_$code" PUT "/business/flow/version/$vid/graph" "$T_ADMIN" "{
    \"change_note\":\"E2E-09 降级链夹具（self=$self）\",
    \"nodes\":[
      {\"key\":\"s\",\"name\":\"开始\",\"type\":\"START\",\"x\":0,\"y\":0,\"sort_order\":1},
      {\"key\":\"n_approve\",\"name\":\"审批（E2E-09）\",\"type\":\"APPROVAL\",\"x\":220,\"y\":0,\"sort_order\":2,
       \"assignee_type\":\"ROLE\",\"assignees\":[{\"type\":\"ROLE\",\"ref_id\":\"$role\",\"sort_order\":0}],
       \"approval_mode\":\"ANY\",\"self_approval\":\"$self\",\"empty_assignee\":\"ERROR\"$fbfrag,
       \"join_mode\":\"AND\",\"fork_mode\":\"AND\",\"reject_veto\":true},
      {\"key\":\"e\",\"name\":\"结束\",\"type\":\"END\",\"x\":440,\"y\":0,\"sort_order\":3}
    ],
    \"edges\":[
      {\"key\":\"e_start\",\"from\":\"s\",\"to\":\"n_approve\"},
      {\"key\":\"e_end\",\"from\":\"n_approve\",\"to\":\"e\"}
    ]}"
  [ "$HTTP" = "200" ] || { say "  [curl] save graph $code → http=$HTTP $(show)" >&2; return 1; }
  api "pub_$code" POST "/business/flow/def/$def/publish" "$T_ADMIN" ""
  [ "$HTTP" = "200" ] || { say "  [curl] publish $code → http=$HTTP $(show)" >&2; return 1; }
  printf '%s' "$def"
}
F_ESC=$(mk_flow e2e09_esc "E2E9 自审跳过升级" E2E09_ESC "$MAKER" SKIP)
F_CHAIN=$(mk_flow e2e09_chain "E2E9 二级上溯" E2E09_CHAIN "$DEAD" SKIP)
F_DEAD=$(mk_flow e2e09_deadend "E2E9 到顶无人" E2E09_DEADEND "$DEAD2" SKIP)
F_FB=$(mk_flow e2e09_fallback "E2E9 兜底人接管" E2E09_FALLBACK "$DEAD2" SKIP "$U_BOSS1")
F_FORBID=$(mk_flow e2e09_forbid "E2E9 禁止自审（回归）" E2E09_FORBID "$MAKER" FORBID)
save F_ESC "$F_ESC"; save F_CHAIN "$F_CHAIN"; save F_DEAD "$F_DEAD"; save F_FB "$F_FB"; save F_FORBID "$F_FORBID"
check "5 张流程建成并发布" "$(printf '%s %s %s %s %s' "$F_ESC" "$F_CHAIN" "$F_DEAD" "$F_FB" "$F_FORBID" | grep -oE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' | wc -l | tr -d ' ')" 5

T_MAKER=$(login e2e09_esc_maker 'E2e09@Pass1') || bad "制单人登录失败"
T_BOSS1=$(login e2e09_esc_boss1 'E2e09@Pass1') || bad "主管甲登录失败"
T_SUB=$(login e2e09_esc_sub 'E2e09@Pass1') || bad "提交人登录失败"
save ESC_T_MAKER "$T_MAKER"; save ESC_T_BOSS1 "$T_BOSS1"; save ESC_T_SUB "$T_SUB"
ok "3 个业务令牌就绪"

start_of() { # TOKEN FLOW_CODE TITLE → 设置全局 IID / HTTP（不用命令替换：subshell 会丢状态）
  local tok=$1 code=$2 title=$3
  api "start_$code" POST /business/flow/instance "$tok" "{\"flow_code\":\"$code\",\"title\":\"$title\",\"form_data\":{}}"
  IID=$(data '.data.instance_id // .data.id')
}

say ""
say "--- ① 自审 SKIP → 升级到上级角色（ARCH-8 第 ② 层）---"
start_of "$T_MAKER" e2e09_esc "E2E09-自审跳过升级"
I1=$IID
check "提交 HTTP" "$HTTP" 200
matches "拿到实例 id" "$I1" '^[0-9a-f]{8}-'
save ESC_I1 "$I1"
check "实例状态 PENDING" "$(SQ "select status from biz_flow_instance where id='$I1'::uuid")" "PENDING"
check "待办 2 行（升级到 BOSS 的 2 人）" "$(SQ "select count(*) from biz_flow_task where instance_id='$I1'::uuid and status='PENDING'")" 2
check "待办 escalated_from_role = E2E09_ESC_MAKER" "$(SQ "select distinct escalated_from_role from biz_flow_task where instance_id='$I1'::uuid and status='PENDING'")" "E2E09_ESC_MAKER"
check "待办 escalate_reason = SKIP_SELF_APPROVAL" "$(SQ "select distinct escalate_reason from biz_flow_task where instance_id='$I1'::uuid and status='PENDING'")" "SKIP_SELF_APPROVAL"
check "2 条待办的审批人属于 BOSS 角色" "$(SQ "select count(*) from biz_flow_task t join sys_user_role ur on ur.user_id=t.assignee_id where t.instance_id='$I1'::uuid and t.status='PENDING' and ur.role_id='$BOSS'::uuid")" 2
check "制单人留 SKIPPED 行（不是静默跳过）" "$(SQ "select count(*) from biz_flow_task where instance_id='$I1'::uuid and status='SKIPPED' and assignee_id='$U_MAKER'::uuid")" 1
contains "SKIPPED 行原因写 self_approval=SKIP" "$(SQ "select comment from biz_flow_task where instance_id='$I1'::uuid and status='SKIPPED' limit 1")" "self_approval=SKIP"
TASK1=$(SQ "select id from biz_flow_task where instance_id='$I1'::uuid and status='PENDING' and assignee_id='$U_BOSS1'::uuid")
api esc_approve POST "/business/flow/task/$TASK1/approve" "$T_BOSS1" '{"comment":"E2E-09 上级角色审批通过"}'
check "上级角色审批 → HTTP" "$HTTP" 200
check "实例 APPROVED" "$(SQ "select status from biz_flow_instance where id='$I1'::uuid")" "APPROVED"
check "升级留痕行仍在（2 行 escalated_from_role）" "$(SQ "select count(*) from biz_flow_task where instance_id='$I1'::uuid and escalated_from_role='E2E09_ESC_MAKER'")" 2
api esc_detail GET "/business/flow/instance/$I1" "$T_BOSS1"
check "时间线 API 带 escalate_reason" "$(printf '%s' "$BODY" | jq -r '[.data.timeline[]? | select(.escalate_reason=="SKIP_SELF_APPROVAL")] | length > 0')" true
check "时间线带 escalated_to_role（升级目标角色）" "$(printf '%s' "$BODY" | jq -r '[.data.timeline[]? | select(.escalated_to_role!=null and .escalated_to_role!="")] | length > 0')" true

say ""
say "--- ② 升级链二级上溯（0 人 → 上级 0 人 → 再上级命中）---"
start_of "$T_SUB" e2e09_chain "E2E09-二级上溯"
I2=$IID
check "提交 HTTP" "$HTTP" 200
matches "拿到实例 id" "$I2" '^[0-9a-f]{8}-'
save ESC_I2 "$I2"
check "待办 2 行（命中最上层的 BOSS）" "$(SQ "select count(*) from biz_flow_task where instance_id='$I2'::uuid and status='PENDING'")" 2
check "escalate_reason = NO_MEMBER（角色 0 人触发）" "$(SQ "select distinct escalate_reason from biz_flow_task where instance_id='$I2'::uuid and status='PENDING'")" "NO_MEMBER"
check "escalated_from_role = E2E09_ESC_DEAD（链起点）" "$(SQ "select distinct escalated_from_role from biz_flow_task where instance_id='$I2'::uuid and status='PENDING'")" "E2E09_ESC_DEAD"
check "assigned_by_rule = ESCALATED" "$(SQ "select distinct assigned_by_rule from biz_flow_task where instance_id='$I2'::uuid and status='PENDING'")" "ESCALATED"

say ""
say "--- ③ 升级链走完仍无人 → 422 + 65018（不留半截实例）---"
api dead_start POST /business/flow/instance "$T_SUB" '{"flow_code":"e2e09_deadend","title":"E2E09-死链","form_data":{}}'
check "死链提交 → HTTP" "$HTTP" 422
check "死链 → 65018（FLOW_NO_ASSIGNEE）" "$CODE" 65018
MSG=$(data '.msg')
contains "报文写明升级链已走完" "$MSG" "升级链已走完"
contains "报文点名链尾角色" "$MSG" "演练死链终点"
check "结构化诊断 escalation_chain 有 1 级" "$(printf '%s' "$BODY" | jq -r '(.data.escalation_chain // []) | length')" 1
check "死链没留下半截实例" "$(SQ "select count(*) from biz_flow_instance where title='E2E09-死链'")" 0

say ""
say "--- ④ 兜底人接管（升级链走完仍无人 → fallback_user_id）---"
start_of "$T_SUB" e2e09_fallback "E2E09-兜底人接管"
I4=$IID
check "提交 HTTP" "$HTTP" 200
matches "拿到实例 id" "$I4" '^[0-9a-f]{8}-'
save ESC_I4 "$I4"
check "实例 PENDING" "$(SQ "select status from biz_flow_instance where id='$I4'::uuid")" "PENDING"
check "待办 1 行（兜底人 boss1）" "$(SQ "select count(*) from biz_flow_task where instance_id='$I4'::uuid and status='PENDING'")" 1
check "待办审批人 = boss1" "$(SQ "select assignee_id from biz_flow_task where instance_id='$I4'::uuid and status='PENDING'")" "$U_BOSS1"
check "assigned_by_rule = FALLBACK" "$(SQ "select assigned_by_rule from biz_flow_task where instance_id='$I4'::uuid and status='PENDING'")" "FALLBACK"
TASK4=$(SQ "select id from biz_flow_task where instance_id='$I4'::uuid and status='PENDING'")
api fb_approve POST "/business/flow/task/$TASK4/approve" "$T_BOSS1" '{"comment":"E2E-09 兜底人审批通过"}'
check "兜底人审批 → HTTP" "$HTTP" 200
check "实例 APPROVED" "$(SQ "select status from biz_flow_instance where id='$I4'::uuid")" "APPROVED"

say ""
say "--- ⑤ 兜底配错 → 422 + 65042（不许静默当成「没配」）---"
# 注：流程定义保存 / 发布**不校验** fallback_user_id 是否存在（见问题清单 P3），
# 因此这里用 API 就能造出「兜底人指向不存在账号」的流程，运行期必须吵 65042。
BOGUS="00000000-0000-4000-8000-00000000beef"
api bad_def POST /business/flow/def "$T_ADMIN" '{"flow_code":"e2e09_badfb","flow_name":"E2E9 兜底配错","biz_type":"E2E09_BADFB","is_default":true}'
check "建「兜底配错」流程 → HTTP" "$HTTP" 200
BAD_DEF=$(data '.data.id'); BAD_VER=$(data '.data.version_id')
api bad_graph PUT "/business/flow/version/$BAD_VER/graph" "$T_ADMIN" "{\"change_note\":\"bogus fallback\",\"nodes\":[
  {\"key\":\"s\",\"name\":\"开始\",\"type\":\"START\"},
  {\"key\":\"n\",\"name\":\"审批\",\"type\":\"APPROVAL\",\"assignee_type\":\"ROLE\",
   \"assignees\":[{\"type\":\"ROLE\",\"ref_id\":\"$DEAD2\",\"sort_order\":0}],
   \"approval_mode\":\"ANY\",\"self_approval\":\"SKIP\",\"empty_assignee\":\"ERROR\",
   \"fallback_user_id\":\"$BOGUS\",\"join_mode\":\"AND\",\"fork_mode\":\"AND\",\"reject_veto\":true},
  {\"key\":\"e\",\"name\":\"结束\",\"type\":\"END\"}],
  \"edges\":[{\"key\":\"e1\",\"from\":\"s\",\"to\":\"n\"},{\"key\":\"e2\",\"from\":\"n\",\"to\":\"e\"}]}"
check "保存含非法兜底人的图 → HTTP（保存期不校验存在性，见问题清单）" "$HTTP" 200
api bad_pub POST "/business/flow/def/$BAD_DEF/publish" "$T_ADMIN" ""
check "发布 → HTTP" "$HTTP" 200
api fb_bad POST /business/flow/instance "$T_SUB" '{"flow_code":"e2e09_badfb","title":"E2E09-兜底配错","form_data":{}}'
check "兜底配错提交 → HTTP" "$HTTP" 422
check "兜底配错 → 65042（FLOW_FALLBACK_INVALID）" "$CODE" 65042
contains "65042 报文点名不存在的兜底人 id" "$(data '.msg')" "$BOGUS"
check "兜底配错没留半截实例" "$(SQ "select count(*) from biz_flow_instance where title='E2E09-兜底配错'")" 0

say ""
say "--- ⑥ 升级链成环 → 422 + 65040（运行期兜底，防历史脏数据/直接改库）---"
SQ "update sys_role set escalation_role_id='$DEAD2'::uuid where id='$EMPTY2'::uuid" >/dev/null
api cyc_start POST /business/flow/instance "$T_SUB" '{"flow_code":"e2e09_deadend","title":"E2E09-环检测","form_data":{}}'
check "成环提交 → HTTP" "$HTTP" 422
check "成环 → 65040（FLOW_ESCALATION_INVALID）" "$CODE" 65040
contains "成环报文" "$(data '.msg')" "成环"
SQ "update sys_role set escalation_role_id=null where id='$EMPTY2'::uuid" >/dev/null
check "链条已复原（EMPTY2 无上级）" "$(SQ "select coalesce(escalation_role_id::text,'NULL') from sys_role where id='$EMPTY2'::uuid")" "NULL"

say ""
say "--- ⑦ 回归：self_approval=FORBID 仍 65019（降级链不越权改语义）---"
api forbid_start POST /business/flow/instance "$T_MAKER" '{"flow_code":"e2e09_forbid","title":"E2E09-禁止自审","form_data":{}}'
check "FORBID 提交 → HTTP" "$HTTP" 422
check "FORBID → 65019" "$CODE" 65019
contains "FORBID 报文" "$(data '.msg')" "self_approval=FORBID"
check "FORBID 没留下半截实例" "$(SQ "select count(*) from biz_flow_instance where title='E2E09-禁止自审'")" 0

say ""
say "--- ⑧ 收尾：把 ② 的待办审掉，避免给演练账号留悬空待办 ---"
TASK2=$(SQ "select id from biz_flow_task where instance_id='$I2'::uuid and status='PENDING' and assignee_id='$U_BOSS1'::uuid")
api chain_approve POST "/business/flow/task/$TASK2/approve" "$T_BOSS1" '{"comment":"E2E-09 上溯审批通过"}'
check "② 实例审批后 APPROVED" "$(SQ "select status from biz_flow_instance where id='$I2'::uuid")" "APPROVED"
check "所有 E2E09 实例无 PENDING 待办残留" "$(SQ "select count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.title like 'E2E09-%' and t.status='PENDING'")" 0

say ""
summary
