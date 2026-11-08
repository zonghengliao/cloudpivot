#!/usr/bin/env bash
# ============================================================================
# P3-08 验收：子流程节点（SUBPROCESS）+ 父子实例 + 结算回调
#
# 卡面验收逐条对照：
#   S1 正常完成：父流程「报销审批」中间插 SUBPROCESS(→「预算调整」) → 父实例进节点 ACTIVE +
#      子实例 PENDING（两行，parent_instance_id 正确）→ 子流程 APPROVED → 父节点自动 APPROVED
#      并继续到 END（父实例 APPROVED）
#   S2 fail_action=ABORT：子流程被驳回 → 父实例 REJECTED
#   S3 CONTINUE：子流程被驳回，父节点仍 APPROVED 并继续（对照 S2）
#   S4 自递归 def 发布被 65046 拒；S5 深度 4 层被拒；S6 指向未发布 def 被 65045 拒
#   S7 幂等：重复回调父节点不重复落状态、不刷行
#   S8 回归：不含 SUBPROCESS 的既有流程 po_approve / pr_approve 完整走通
#   S9 spawn_task：父侧 biz_task 从 TODO → DONE（通过）/ CANCELLED（驳回）
#
# 图 JSON 一律先落到 $RAW/*.json 再 `cat` 进请求体：既是证据，也避免
# 「命令替换里嵌 heredoc」这种解析坑（本目录 20_e2e.sh.broken 就是那一版的残骸）。
#
# 环境：库 cloudpivot_p308_ut / 端口 18312
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

BASE=${BASE:-http://127.0.0.1:18312}
DB=${DB:-cloudpivot_p308_ut}
OUT="$RAW/20_e2e_subprocess.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

ENVF=${ENVF:-/root/p307/instance.env}
ADMIN_PW=${ADMIN_PW:-$(grep '^NIK_ADMIN_PASSWORD=' "$ENVF" | cut -d= -f2-)}
ADMIN_NEW=${ADMIN_NEW:-P308new@2026}
U_PW=${U_PW:-P308@2026}

say "================ P3-08 子流程节点实测（$BASE / $DB）================"
curl -s -o /dev/null "$BASE/health" || { bad "实例没起来"; summary; }

# ---------- 0) 迁移后结构就位 ----------
eq "V0341 已记录（1 条）" \
  "$(q "$DB" "select count(*) from schema_migrations where kind='versioned' and version='341'")" "1"
eq "node_type CHECK 含 SUBPROCESS" \
  "$(q "$DB" "select count(*) from pg_constraint where conrelid='biz_flow_node'::regclass
      and conname='biz_flow_node_node_type_check' and pg_get_constraintdef(oid) like '%SUBPROCESS%'")" "1"
eq "分区 CHECK 同步扩了 sub" \
  "$(q "$DB" "select count(*) from pg_constraint where conrelid='biz_flow_node'::regclass
      and conname='ck_biz_flow_node_config_partition' and pg_get_constraintdef(oid) like '%sub%'")" "1"
eq "biz_flow_instance 父子两列" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_instance'
      and column_name in ('parent_instance_id','parent_node_key')")" "2"
eq "父子部分索引" \
  "$(q "$DB" "select count(*) from pg_indexes where indexname='idx_biz_flow_instance_parent'
      and indexdef like '%WHERE (parent_instance_id IS NOT NULL)%'")" "1"

# ---------- 1) 管理员登录 ----------
RC=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"admin\",\"password\":\"$ADMIN_PW\"}")
if [ "$RC" = "200" ]; then
  A=$(body | jget data.access_token); MUST=$(body | jget data.must_change_password | tr 'A-Z' 'a-z')
else
  RC=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"admin\",\"password\":\"$ADMIN_NEW\"}")
  A=$(body | jget data.access_token); MUST=false
fi
[ -n "$A" ] || { bad "管理员登录失败：$(body)"; summary; }
if [ "$MUST" = "true" ]; then
  http PUT "$BASE/api/system/auth/password" "$A" \
    "{\"old_password\":\"$ADMIN_PW\",\"new_password\":\"$ADMIN_NEW\"}" >/dev/null
  RC=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"admin\",\"password\":\"$ADMIN_NEW\"}")
  A=$(body | jget data.access_token)
fi
[ -n "$A" ] && ok "管理员令牌就绪" || { bad "管理员登录失败"; summary; }

# ---------- 2) 角色 / 用户 / 权限 ----------
mk_role() {
  local code=$1 name=$2 rc
  rc=$(http POST "$BASE/api/system/role" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"status\":1,\"sort_order\":980}")
  if [ "$rc" = "200" ]; then body | jget data.id; return 0; fi
  # 409 角色编码已存在（脚本可重复执行）→ 直接取库里那条
  local existing
  existing=$(q "$DB" "select id from sys_role where role_code='$code' and deleted_at is null limit 1")
  [ -n "$existing" ] && { printf '%s' "$existing"; return 0; }
  say "  [curl] role $code http=$rc $(body)" >&2
  return 1
}
mk_user() {
  local uname=$1 real=$2 role=$3 rc existing
  rc=$(http POST "$BASE/api/system/user" "$A" \
    "{\"username\":\"$uname\",\"password\":\"$U_PW\",\"real_name\":\"$real\",\"status\":1,\"role_ids\":[\"$role\"]}")
  if [ "$rc" = "200" ]; then body | jget data.id; return 0; fi
  existing=$(q "$DB" "select id from sys_user where username='$uname' and deleted_at is null limit 1")
  [ -n "$existing" ] && { printf '%s' "$existing"; return 0; }
  say "  [curl] user $uname http=$rc $(body)" >&2
  return 1
}
grant() { # grant ROLE_ID CODES... -> 实际持有的码数
  local role=$1; shift
  local list
  list=$(printf "'%s'," "$@")
  q "$DB" "INSERT INTO sys_role_menu (role_id, menu_id, created_at)
           SELECT '$role'::uuid, m.id, now() FROM sys_menu m
            WHERE m.auth_code IN (${list%,}) AND m.deleted_at IS NULL
              AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id='$role'::uuid AND rm.menu_id=m.id)" >/dev/null
  q "$DB" "SELECT count(DISTINCT m.auth_code) FROM sys_role_menu rm
             JOIN sys_menu m ON m.id = rm.menu_id
            WHERE rm.role_id='$role'::uuid AND m.auth_code IN (${list%,})"
}

R_MAKER=$(mk_role P308_MAKER "P3-08 报销人")
R_BOSS=$(mk_role P308_BOSS "P3-08 审预算的人")
is_uuid "$R_MAKER" && is_uuid "$R_BOSS" && ok "2 个角色就绪" || { bad "建角色失败：$(body)"; summary; }

U_MAKER=$(mk_user p308_maker "P3-08 报销人甲" "$R_MAKER") || { bad "建用户 maker 失败"; summary; }
U_BOSS=$(mk_user p308_boss "P3-08 审批人乙" "$R_BOSS") || { bad "建用户 boss 失败"; summary; }
ok "2 个用户就绪（maker=$U_MAKER boss=$U_BOSS）"

eq "报销人权限码 2 个" "$(grant "$R_MAKER" business:flow:instance:create business:flow:instance:read)" "2"
eq "审批人权限码 4 个" \
  "$(grant "$R_BOSS" business:flow:approve business:flow:instance:read business:flow:admin business:flow:instance:create)" "4"

TOK_MAKER=$(login p308_maker "$U_PW" "$U_PW") || { bad "maker 登录失败"; summary; }
TOK_BOSS=$(login p308_boss "$U_PW" "$U_PW") || { bad "boss 登录失败"; summary; }
ok "2 个业务令牌就绪"

# 种子流程（po_approve / pr_approve）的审批节点是**角色口径**：把 boss 放进这些角色，
# 让「回归：既有流程完整走通」有唯一、确定的执行人（不改种子流程一个字）。
for rc in DEPT_LEADER FINANCE_MANAGER PURCHASE_MANAGER GENERAL_MANAGER WAREHOUSE_MANAGER FINANCE_DIRECTOR; do
  q "$DB" "insert into sys_user_role (user_id, role_id)
           select '$U_BOSS'::uuid, id from sys_role where role_code='$rc' and deleted_at is null
           on conflict do nothing" >/dev/null
done
ok "boss 已进入种子审批角色（DEPT_LEADER / FINANCE_MANAGER / PURCHASE_MANAGER / GENERAL_MANAGER / WAREHOUSE_MANAGER / FINANCE_DIRECTOR）"

# ---------- 3) 通用：建 def → 存图 → 发布 ----------
mk_def() { # mk_def CODE NAME BIZ -> "def_id version_id"
  local code=$1 name=$2 biz=$3 rc def vid
  rc=$(http POST "$BASE/api/business/flow/def" "$A" \
    "{\"flow_code\":\"$code\",\"flow_name\":\"$name\",\"biz_type\":\"$biz\"}")
  if [ "$rc" = "200" ]; then
    printf '%s %s' "$(body | jget data.id)" "$(body | jget data.version_id)"
    return 0
  fi
  # 已存在（脚本可重复执行）：取定义 + 最近一个 DRAFT 版本
  def=$(q "$DB" "select id from biz_flow_def where flow_code='$code' and deleted_at is null limit 1")
  if [ -n "$def" ]; then
    vid=$(q "$DB" "select id from biz_flow_def_version where def_id='$def'::uuid
                    order by version_no desc limit 1")
    printf '%s %s' "$def" "$vid"
    return 0
  fi
  say "  [curl] def $code http=$rc $(body)" >&2
  return 1
}
save_graph() { # save_graph VERSION_ID FILE -> rc
  http PUT "$BASE/api/business/flow/version/$1/graph" "$A" "$(cat "$2")"
}
publish_def() { # publish_def DEF_ID -> rc（body 留在 last_body.json）
  http POST "$BASE/api/business/flow/def/$1/publish" "$A" ""
}
err_code() { body | jget code; }

# ---------- 4) 子流程 def：预算调整（START → 审批(ROLE boss) → END） ----------
read -r F_CHILD V_CHILD <<<"$(mk_def p308_budget_adjust "P3-08 预算调整" BUDGET_ADJUST)"
is_uuid "$F_CHILD" || { bad "建子流程 def 失败：$(body)"; summary; }

cat >"$RAW/graph_child.json" <<JSON
{"change_note":"P3-08 子流程 def：单审批节点",
 "nodes":[{"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
          {"key":"n_budget","name":"预算审批","type":"APPROVAL","x":220,"y":0,"sort_order":2,
           "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$R_BOSS","sort_order":0}],
           "approval_mode":"ANY","self_approval":"SKIP","empty_assignee":"ERROR",
           "join_mode":"AND","fork_mode":"AND","reject_veto":true},
          {"key":"e","name":"结束","type":"END","x":440,"y":0,"sort_order":3}],
 "edges":[{"key":"e_s","from":"s","to":"n_budget"},{"key":"e_e","from":"n_budget","to":"e"}]}
JSON
RC=$(save_graph "$V_CHILD" "$RAW/graph_child.json")
eq "子流程 def 存图 → 200" "$RC" "200"
RC=$(publish_def "$F_CHILD")
eq "子流程 def「预算调整」发布 → 200" "$RC" "200"

# ---------- 5) 父流程 def：报销审批（START → SUBPROCESS → 审批 → END） ----------
read -r F_PARENT V_PARENT <<<"$(mk_def p308_expense_sub "P3-08 报销审批（含子流程）" EXPENSE_SUB)"
is_uuid "$F_PARENT" || { bad "建父流程 def 失败：$(body)"; summary; }

cat >"$RAW/graph_parent.json" <<JSON
{"change_note":"P3-08 父流程：中间插子流程节点（fail_action=ABORT + spawn_task）",
 "nodes":[{"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
          {"key":"n_sub","name":"预算调整（子流程）","type":"SUBPROCESS","x":220,"y":0,"sort_order":2,
           "subprocess_def_id":"$F_CHILD",
           "config":{"sub":{"version_policy":"PIN_CURRENT","fail_action":"ABORT",
                            "param_mapping":{"amount":"amount","dept":"dept_id"},
                            "result_mapping":{"budget_approved":"approved"},
                            "spawn_task":true}}},
          {"key":"n_dept","name":"部门负责人审批","type":"APPROVAL","x":440,"y":0,"sort_order":3,
           "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$R_BOSS","sort_order":0}],
           "approval_mode":"ANY","self_approval":"SKIP","empty_assignee":"ERROR",
           "join_mode":"AND","fork_mode":"AND","reject_veto":true},
          {"key":"e","name":"结束","type":"END","x":660,"y":0,"sort_order":4}],
 "edges":[{"key":"e_s","from":"s","to":"n_sub"},
          {"key":"e_sub","from":"n_sub","to":"n_dept"},
          {"key":"e_end","from":"n_dept","to":"e"}]}
JSON
RC=$(save_graph "$V_PARENT" "$RAW/graph_parent.json")
eq "存图（SUBPROCESS 节点 + config.sub）→ 200" "$RC" "200"
RC=$(publish_def "$F_PARENT")
if [ "$RC" != "200" ]; then bad "父流程发布失败 http=$RC $(body)"; else ok "父流程「报销审批（含子流程）」发布 → 200"; fi
eq "节点行 subprocess_def_id 落库" \
  "$(q "$DB" "select subprocess_def_id from biz_flow_node where version_id='$V_PARENT'::uuid and node_key='n_sub'")" "$F_CHILD"
eq "config.sub 落库（分区 CHECK 未拦）" \
  "$(q "$DB" "select config->'sub'->>'fail_action' from biz_flow_node where version_id='$V_PARENT'::uuid and node_key='n_sub'")" "ABORT"

# ---------- 6) S1：父实例发起 → 子实例在同事务内建出来 ----------
RC=$(http POST "$BASE/api/business/flow/instance" "$TOK_MAKER" \
  "{\"def_id\":\"$F_PARENT\",\"title\":\"P3-08 报销-正常路径\",\"form_data\":{\"amount\":5000,\"dept_id\":\"D-P308\"}}")
eq "S1 发起父实例 → 200" "$RC" "200"
P1=$(body | jget data.instance_id)
eq "S1 父实例 PENDING" "$(body | jget data.instance_status)" "PENDING"
is_uuid "$P1" && ok "S1 父实例 id = $P1" || { bad "S1 发起失败：$(body)"; summary; }

C1=$(q "$DB" "select id from biz_flow_instance where parent_instance_id='$P1'::uuid limit 1")
is_uuid "$C1" && ok "S1 子实例已建（$C1）" || bad "S1 没有子实例"
eq "S1 子实例 parent_node_key" \
  "$(q "$DB" "select parent_node_key from biz_flow_instance where id='$C1'::uuid")" "n_sub"
eq "S1 子实例 biz_id 为空（不撞 uk_biz_flow_instance_biz_active）" \
  "$(q "$DB" "select coalesce(biz_id::text,'NULL') from biz_flow_instance where id='$C1'::uuid")" "NULL"
eq "S1 子实例 biz_no 为空" \
  "$(q "$DB" "select coalesce(biz_no,'NULL') from biz_flow_instance where id='$C1'::uuid")" "NULL"
eq "S1 父子两行并存（同一单据同一时刻）" \
  "$(q "$DB" "select count(*) from biz_flow_instance where id in ('$P1'::uuid,'$C1'::uuid)")" "2"
eq "S1 子实例 form_data 映射到 amount" \
  "$(q "$DB" "select form_data->>'amount' from biz_flow_instance where id='$C1'::uuid")" "5000"
eq "S1 子实例 form_data 映射到 dept" \
  "$(q "$DB" "select form_data->>'dept' from biz_flow_instance where id='$C1'::uuid")" "D-P308"
eq "S1 父节点 n_sub = ACTIVE" \
  "$(q "$DB" "select status from biz_flow_instance_node where instance_id='$P1'::uuid and node_key='n_sub'")" "ACTIVE"
eq "S1 子节点 n_budget = ACTIVE（子流程在等人批）" \
  "$(q "$DB" "select status from biz_flow_instance_node where instance_id='$C1'::uuid and node_key='n_budget'")" "ACTIVE"
eq "S9 spawn_task 生成父侧可见性任务（TODO）" \
  "$(q "$DB" "select status from biz_task where flow_instance_id='$P1'::uuid and flow_node_key='n_sub'")" "TODO"

# ---------- 7) S1 继续：子流程通过 → 父节点自动 APPROVED → 继续到 END ----------
T_CHILD=$(q "$DB" "select id from biz_flow_task where instance_id='$C1'::uuid and status='PENDING' limit 1")
is_uuid "$T_CHILD" && ok "S1 子流程待办就绪（$T_CHILD）" || bad "S1 子流程没有待办"
RC=$(http POST "$BASE/api/business/flow/task/$T_CHILD/approve" "$TOK_BOSS" '{"comment":"预算没问题"}')
eq "S1 子流程审批 → 200" "$RC" "200"
eq "S1 子实例 APPROVED" "$(q "$DB" "select status from biz_flow_instance where id='$C1'::uuid")" "APPROVED"
eq "S1 父节点 n_sub 自动 APPROVED" \
  "$(q "$DB" "select status from biz_flow_instance_node where instance_id='$P1'::uuid and node_key='n_sub'")" "APPROVED"
eq "S1 父流程继续到 n_dept（ACTIVE）" \
  "$(q "$DB" "select status from biz_flow_instance_node where instance_id='$P1'::uuid and node_key='n_dept'")" "ACTIVE"
eq "S9 父侧可见性任务转 DONE" \
  "$(q "$DB" "select status from biz_task where flow_instance_id='$P1'::uuid and flow_node_key='n_sub'")" "DONE"
eq "S1 result_mapping 指向子表单不存在的键 → 父表单不写（NULL）" \
  "$(q "$DB" "select coalesce(form_data->>'budget_approved','NULL') from biz_flow_instance where id='$P1'::uuid")" "NULL"

T_PARENT=$(q "$DB" "select id from biz_flow_task where instance_id='$P1'::uuid and status='PENDING' limit 1")
RC=$(http POST "$BASE/api/business/flow/task/$T_PARENT/approve" "$TOK_BOSS" '{"comment":"同意报销"}')
eq "S1 父流程后续审批 → 200" "$RC" "200"
eq "S1 父实例 APPROVED（走到 END）" "$(q "$DB" "select status from biz_flow_instance where id='$P1'::uuid")" "APPROVED"
eq "S1 父 END 节点 APPROVED" \
  "$(q "$DB" "select status from biz_flow_instance_node where instance_id='$P1'::uuid and node_key='e'")" "APPROVED"

# ---------- 8) S7：幂等（重复回调不重复落状态 / 不刷行） ----------
NODE_ROWS_BEFORE=$(q "$DB" "select count(*) from biz_flow_instance_node where instance_id='$P1'::uuid")
RC=$(http POST "$BASE/api/business/flow/task/$T_CHILD/approve" "$TOK_BOSS" '{"comment":"再点一次"}')
eq "S7 重复点同一条子流程待办 → 200（幂等）" "$RC" "200"
eq "S7 父实例状态不变（APPROVED）" "$(q "$DB" "select status from biz_flow_instance where id='$P1'::uuid")" "APPROVED"
eq "S7 父节点行数不变（不重复落状态）" \
  "$(q "$DB" "select count(*) from biz_flow_instance_node where instance_id='$P1'::uuid")" "$NODE_ROWS_BEFORE"
eq "S7 n_sub 只有一行" \
  "$(q "$DB" "select count(*) from biz_flow_instance_node where instance_id='$P1'::uuid and node_key='n_sub'")" "1"

# ---------- 9) S2：fail_action=ABORT，子流程被驳回 → 父实例 REJECTED ----------
RC=$(http POST "$BASE/api/business/flow/instance" "$TOK_MAKER" \
  "{\"def_id\":\"$F_PARENT\",\"title\":\"P3-08 报销-子流程驳回\",\"form_data\":{\"amount\":90000}}")
eq "S2 发起父实例 → 200" "$RC" "200"
P2=$(body | jget data.instance_id)
C2=$(q "$DB" "select id from biz_flow_instance where parent_instance_id='$P2'::uuid limit 1")
is_uuid "$C2" && ok "S2 子实例已建（$C2）" || bad "S2 没有子实例"
T_C2=$(q "$DB" "select id from biz_flow_task where instance_id='$C2'::uuid and status='PENDING' limit 1")
RC=$(http POST "$BASE/api/business/flow/task/$T_C2/reject" "$TOK_BOSS" \
  '{"comment":"预算不足，驳回","return_to":"FINAL"}')
eq "S2 子流程驳回并终止 → 200" "$RC" "200"
eq "S2 子实例 REJECTED" "$(q "$DB" "select status from biz_flow_instance where id='$C2'::uuid")" "REJECTED"
eq "S2 父节点 n_sub = REJECTED" \
  "$(q "$DB" "select status from biz_flow_instance_node where instance_id='$P2'::uuid and node_key='n_sub'")" "REJECTED"
eq "S2 父实例 REJECTED（ABORT 生效）" "$(q "$DB" "select status from biz_flow_instance where id='$P2'::uuid")" "REJECTED"
eq "S2 父流程没有再继续到 n_dept" \
  "$(q "$DB" "select count(*) from biz_flow_instance_node where instance_id='$P2'::uuid and node_key='n_dept'")" "0"
eq "S9 父侧可见性任务转 CANCELLED" \
  "$(q "$DB" "select status from biz_task where flow_instance_id='$P2'::uuid and flow_node_key='n_sub'")" "CANCELLED"
eq "S2 父实例 result_comment 为空（子流程 ABORT 不写审批意见）" \
  "$(q "$DB" "select coalesce(result_comment,'NULL') from biz_flow_instance where id='$P2'::uuid")" "NULL"

# ---------- 10) S3：fail_action=CONTINUE，子流程驳回也继续 ----------
read -r F_CONT V_CONT <<<"$(mk_def p308_expense_cont "P3-08 报销审批（子流程 CONTINUE）" EXPENSE_SUB_CONT)"
cat >"$RAW/graph_continue.json" <<JSON
{"change_note":"P3-08 父流程（CONTINUE 对照）",
 "nodes":[{"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
          {"key":"n_sub","name":"预算调整（子流程）","type":"SUBPROCESS","x":220,"y":0,"sort_order":2,
           "subprocess_def_id":"$F_CHILD",
           "config":{"sub":{"fail_action":"CONTINUE"}}},
          {"key":"n_dept","name":"部门负责人审批","type":"APPROVAL","x":440,"y":0,"sort_order":3,
           "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$R_BOSS","sort_order":0}],
           "approval_mode":"ANY","self_approval":"SKIP","empty_assignee":"ERROR",
           "join_mode":"AND","fork_mode":"AND","reject_veto":true},
          {"key":"e","name":"结束","type":"END","x":660,"y":0,"sort_order":4}],
 "edges":[{"key":"e_s","from":"s","to":"n_sub"},
          {"key":"e_sub","from":"n_sub","to":"n_dept"},
          {"key":"e_end","from":"n_dept","to":"e"}]}
JSON
RC=$(save_graph "$V_CONT" "$RAW/graph_continue.json")
eq "S3 存图（CONTINUE）→ 200" "$RC" "200"
RC=$(publish_def "$F_CONT")
eq "S3 发布 → 200" "$RC" "200"
RC=$(http POST "$BASE/api/business/flow/instance" "$TOK_MAKER" \
  "{\"def_id\":\"$F_CONT\",\"title\":\"P3-08 报销-CONTINUE\",\"form_data\":{\"amount\":1}}")
eq "S3 发起 → 200" "$RC" "200"
P3=$(body | jget data.instance_id)
C3=$(q "$DB" "select id from biz_flow_instance where parent_instance_id='$P3'::uuid limit 1")
T_C3=$(q "$DB" "select id from biz_flow_task where instance_id='$C3'::uuid and status='PENDING' limit 1")
RC=$(http POST "$BASE/api/business/flow/task/$T_C3/reject" "$TOK_BOSS" \
  '{"comment":"预算不通过但按 CONTINUE 继续","return_to":"FINAL"}')
eq "S3 子流程驳回 → 200" "$RC" "200"
eq "S3 子实例 REJECTED" "$(q "$DB" "select status from biz_flow_instance where id='$C3'::uuid")" "REJECTED"
eq "S3 父节点 n_sub = APPROVED（CONTINUE）" \
  "$(q "$DB" "select status from biz_flow_instance_node where instance_id='$P3'::uuid and node_key='n_sub'")" "APPROVED"
eq "S3 父实例仍 PENDING（继续走 n_dept）" "$(q "$DB" "select status from biz_flow_instance where id='$P3'::uuid")" "PENDING"
eq "S3 父流程继续到 n_dept" \
  "$(q "$DB" "select status from biz_flow_instance_node where instance_id='$P3'::uuid and node_key='n_dept'")" "ACTIVE"

# ---------- 11) S4：自递归 def 发布被 65046 拒 ----------
read -r F_SELF V_SELF <<<"$(mk_def p308_self_recur "P3-08 自递归" SELF_RECUR)"
cat >"$RAW/graph_self.json" <<JSON
{"nodes":[{"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
          {"key":"n_sub","name":"自己调自己","type":"SUBPROCESS","x":220,"y":0,"sort_order":2,
           "subprocess_def_id":"$F_SELF","config":{"sub":{}}},
          {"key":"e","name":"结束","type":"END","x":440,"y":0,"sort_order":3}],
 "edges":[{"key":"e_s","from":"s","to":"n_sub"},{"key":"e_e","from":"n_sub","to":"e"}]}
JSON
RC=$(save_graph "$V_SELF" "$RAW/graph_self.json")
RC=$(publish_def "$F_SELF")
eq "S4 自递归发布被拒 → 400" "$RC" "400"
eq "S4 错误码 65046" "$(err_code)" "65046"
contains "S4 文案点名自递归" "$(body | jget msg)" "自己"

# ---------- 12) S5：深度 4 层被拒（D3 允许，D4 拒绝） ----------
mk_chain_graph() { # mk_chain_graph FILE CHILD_DEF_ID（空 = 叶子节点图）
  local file=$1 child=$2
  if [ -n "$child" ]; then
    cat >"$file" <<JSON
{"nodes":[{"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
          {"key":"n_sub","name":"下一层子流程","type":"SUBPROCESS","x":220,"y":0,"sort_order":2,
           "subprocess_def_id":"$child","config":{"sub":{}}},
          {"key":"e","name":"结束","type":"END","x":440,"y":0,"sort_order":3}],
 "edges":[{"key":"e_s","from":"s","to":"n_sub"},{"key":"e_e","from":"n_sub","to":"e"}]}
JSON
  else
    cat >"$file" <<JSON
{"nodes":[{"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
          {"key":"n_ok","name":"叶子审批","type":"APPROVAL","x":220,"y":0,"sort_order":2,
           "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$R_BOSS","sort_order":0}],
           "approval_mode":"ANY","self_approval":"SKIP","empty_assignee":"ERROR",
           "join_mode":"AND","fork_mode":"AND","reject_veto":true},
          {"key":"e","name":"结束","type":"END","x":440,"y":0,"sort_order":3}],
 "edges":[{"key":"e_s","from":"s","to":"n_ok"},{"key":"e_e","from":"n_ok","to":"e"}]}
JSON
  fi
}
mk_chain_def() { # mk_chain_def CODE NAME BIZ FILE -> def_id
  local code=$1 name=$2 biz=$3 file=$4 def vid rc
  read -r def vid <<<"$(mk_def "$code" "$name" "$biz")"
  is_uuid "$def" || return 1
  rc=$(save_graph "$vid" "$file")
  [ "$rc" = "200" ] || { say "  [curl] graph $code http=$rc $(body)" >&2; return 1; }
  printf '%s' "$def"
}

mk_chain_graph "$RAW/graph_d0.json" ""
D0=$(mk_chain_def p308_depth_d0 "P3-08 深度链 D0（叶子）" SELF_RECUR "$RAW/graph_d0.json")
RC=$(publish_def "$D0"); eq "S5 D0 发布 → 200" "$RC" "200"
mk_chain_graph "$RAW/graph_d1.json" "$D0"
D1=$(mk_chain_def p308_depth_d1 "P3-08 深度链 D1" SELF_RECUR "$RAW/graph_d1.json")
RC=$(publish_def "$D1"); eq "S5 D1 发布（深度 1）→ 200" "$RC" "200"
mk_chain_graph "$RAW/graph_d2.json" "$D1"
D2=$(mk_chain_def p308_depth_d2 "P3-08 深度链 D2" SELF_RECUR "$RAW/graph_d2.json")
RC=$(publish_def "$D2"); eq "S5 D2 发布（深度 2）→ 200" "$RC" "200"
mk_chain_graph "$RAW/graph_d3.json" "$D2"
D3=$(mk_chain_def p308_depth_d3 "P3-08 深度链 D3" SELF_RECUR "$RAW/graph_d3.json")
RC=$(publish_def "$D3"); eq "S5 D3 发布（深度 3，上限内）→ 200" "$RC" "200"
mk_chain_graph "$RAW/graph_d4.json" "$D3"
D4=$(mk_chain_def p308_depth_d4 "P3-08 深度链 D4" SELF_RECUR "$RAW/graph_d4.json")
RC=$(publish_def "$D4")
eq "S5 D4 发布（深度 4）被拒 → 400" "$RC" "400"
eq "S5 错误码 65046" "$(err_code)" "65046"
contains "S5 文案点名深度" "$(body | jget msg)" "嵌套深度"

# ---------- 13) S6：指向未发布 def 被 65045 拒 ----------
read -r F_DRAFT V_DRAFT <<<"$(mk_def p308_draft_only "P3-08 未发布草稿" DRAFT_ONLY)"
is_uuid "$F_DRAFT" && ok "草稿 def 就绪（$F_DRAFT，不发布）" || bad "建草稿 def 失败"
read -r F_BADREF V_BADREF <<<"$(mk_def p308_bad_ref "P3-08 指向草稿" BAD_REF)"
cat >"$RAW/graph_badref.json" <<JSON
{"nodes":[{"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
          {"key":"n_sub","name":"指向草稿","type":"SUBPROCESS","x":220,"y":0,"sort_order":2,
           "subprocess_def_id":"$F_DRAFT","config":{"sub":{}}},
          {"key":"e","name":"结束","type":"END","x":440,"y":0,"sort_order":3}],
 "edges":[{"key":"e_s","from":"s","to":"n_sub"},{"key":"e_e","from":"n_sub","to":"e"}]}
JSON
RC=$(save_graph "$V_BADREF" "$RAW/graph_badref.json")
RC=$(publish_def "$F_BADREF")
eq "S6 指向未发布 def 发布被拒 → 400" "$RC" "400"
eq "S6 错误码 65045" "$(err_code)" "65045"

# ---------- 14) S8：回归（不含 SUBPROCESS 的既有流程完整走通） ----------
walk_seeded() { # walk_seeded FLOW_CODE TITLE -> "status 批次数"
  local code=$1 title=$2 rc inst i tid n=0
  rc=$(http POST "$BASE/api/business/flow/instance" "$TOK_MAKER" \
    "{\"flow_code\":\"$code\",\"title\":\"$title\",\"form_data\":{\"amount\":5000,\"over_budget_amount\":0}}")
  [ "$rc" = "200" ] || { say "  [curl] start $code http=$rc $(body)" >&2; return 1; }
  inst=$(body | jget data.instance_id)
  for i in $(seq 1 8); do
    tid=$(q "$DB" "select id from biz_flow_task where instance_id='$inst'::uuid and status='PENDING' order by created_at limit 1")
    [ -n "$tid" ] || break
    rc=$(http POST "$BASE/api/business/flow/task/$tid/approve" "$TOK_BOSS" '{"comment":"回归审批通过"}')
    [ "$rc" = "200" ] || { say "  [curl] approve $code http=$rc $(body)" >&2; return 1; }
    n=$((n + 1))
  done
  printf '%s %s' "$(q "$DB" "select status from biz_flow_instance where id='$inst'::uuid")" "$n"
}
read -r ST_PO N_PO <<<"$(walk_seeded po_approve "P3-08 回归-po_approve")"
eq "S8 po_approve 完整走通（实例 APPROVED）" "$ST_PO" "APPROVED"
read -r ST_PR N_PR <<<"$(walk_seeded pr_approve "P3-08 回归-pr_approve")"
eq "S8 pr_approve 完整走通（实例 APPROVED）" "$ST_PR" "APPROVED"
say "  （po_approve 批了 $N_PO 次，pr_approve 批了 $N_PR 次）"

# ---------- 15) 父子实例 SQL 快照（证据用） ----------
say ""
say "--- 父子实例快照（P1 = 正常路径父单）---"
q "$DB" "select i.instance_no, coalesce(i.parent_instance_id::text,'-') as parent,
                coalesce(i.parent_node_key,'-') as node_key, i.flow_code, i.status,
                coalesce(i.biz_id::text,'NULL') as biz_id
           from biz_flow_instance i
          where i.id = '$P1'::uuid or i.parent_instance_id = '$P1'::uuid
          order by i.parent_instance_id nulls first"
say ""
say "--- 状态流转时间线（父 P1 + 子 C1 的节点行）---"
q "$DB" "select (instance_id = '$P1'::uuid) as is_parent, node_key, node_type, round, status,
                to_char(activated_at,'HH24:MI:SS') as activated, to_char(ended_at,'HH24:MI:SS') as ended
           from biz_flow_instance_node
          where instance_id in ('$P1'::uuid,'$C1'::uuid)
          order by activated_at nulls last, instance_id"

summary
