#!/usr/bin/env bash
# ============================================================================
# P3-06 验收 ②③：签名模式的**真 HTTP** 实测（隔离实例 + 独立空库）
#
# 卡面验收原文逐条对照：
#   · 隔离实例实测：角色 3 人、制单人第 4 人 → 新发起 → **任 1 人审批即通过**，
#     另 2 条待办变 `CANCELLED`（DB 断言）；把该节点改回显式 ALL → 必须 3 人都签。
#   · 回归 ARCH-8 场景 A（制单人在角色里 → SKIP 后剩 2 人签）：2 人都要签还是 1 人签？
#     **按迁移后该节点的 approval_mode 决定** —— 这里用「迁移会冻结成 ALL」的节点
#     （ROLE 3 人 ≥ 2）实测：SKIP 掉制单人后，剩下 2 人**都要签**才过。
#
# 场景：
#   E1  APPROVAL + approval_mode = NULL（不填）→ 或签：B 一人同意 → 实例通过、
#       C/D 两条待办 CANCELLED；且发布期 R9 不拦（NULL 合法）。
#   E2  同一角色、节点显式 ALL → B/C 各签一次仍 PENDING，D 签完才 APPROVED（3 人都要签）。
#   E3  ARCH-8 A：角色 3 人含制单人，节点 ALL → 制单人那票 SKIPPED，剩 2 人**都签**才过。
#   E4  D2 冻结口径落到真库：ROLE 3 人 → ALL / ROLE 1 人 → ANY / FORM_VAR 算不出 → ANY；
#       重放 up.sql → UPDATE 0；down.sql → 全回 NULL。
#
# 环境：库 cloudpivot_p306_ut / 端口 18307（与迁移证据的 18306 分开）。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

BASE=${BASE:-http://127.0.0.1:18307}
DB=${DB:-cloudpivot_p306_ut}
OUT="$RAW/20_e2e_sign_mode.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

ADMIN_NEW=${ADMIN_NEW:-P306new@2026}

# ---------- 实例生命周期（本脚本自带：独立空库 + 独立端口，可整体复跑） ----------
ENVF=${ENVF:-/root/p306/instance.env}
BIN=${BIN:-/root/p306/bin/cloudpivot}
RUN_DIR=/opt/cloudpivot/backend
ILOG=/root/p306/e2e_instance.log
IPID=/root/p306/e2e.pid

# 初始超管口令只从环境文件来（仓库不落口令）；改密后的口令由脚本自己生成
ADMIN_PW=${ADMIN_PW:-$(grep '^NIK_ADMIN_PASSWORD=' "$ENVF" | cut -d= -f2-)}

stop_instance() {
  local pid
  pid=$(cat "$IPID" 2>/dev/null) || return 0
  kill "$pid" 2>/dev/null || return 0
  for _ in $(seq 1 30); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  kill -9 "$pid" 2>/dev/null || true
}

start_instance() {
  local i pid
  ( cd "$RUN_DIR" && setsid env $(grep -v '^#' "$ENVF" | grep -v '^$' | tr '\n' ' ') \
      NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:18307" \
      "$BIN" >"$ILOG" 2>&1 </dev/null & )
  for i in $(seq 1 120); do
    pid=$(ss -ltnp 2>/dev/null | sed -n "s/.*127\.0\.0\.1:18307 .*pid=\([0-9]*\).*/\1/p" | head -1)
    if [ -n "$pid" ]; then echo "$pid" >"$IPID"; return 0; fi
    sleep 1
  done
  return 1
}

wait_http() {
  local i=0
  while [ "$i" -lt 180 ]; do
    if curl -s -o /dev/null "$BASE/health"; then return 0; fi
    sleep 1
    i=$((i + 1))
  done
  return 1
}

say "================ P3-06 ②③ 签名模式真 HTTP 实测（$BASE / $DB）================"
stop_instance
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun $DB" || { bad "建空库失败"; summary; }
start_instance || { bad "实例没起来"; tail -20 "$ILOG"; summary; }
wait_http || { bad "实例 180s 内没起来"; tail -40 "$ILOG"; summary; }
say "空库首启：$(grep -oE '应用 [0-9]+ 个迁移' "$ILOG" | tail -1)"
ok "隔离实例就绪（:18307 / $DB / 迁移器已装到 V0339）"

# ---------- 0) 管理员登录（首登强制改密；可复跑） ----------
RC=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"admin\",\"password\":\"$ADMIN_PW\"}")
if [ "$RC" = "200" ]; then
  A=$(body | jget data.access_token)
  MUST=$(body | jget data.must_change_password | tr 'A-Z' 'a-z')
else
  RC=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"admin\",\"password\":\"$ADMIN_NEW\"}")
  A=$(body | jget data.access_token)
  MUST=false
fi
[ -n "$A" ] || { bad "管理员登录失败：$(body)"; summary; }
ok "管理员登录成功"
if [ "$MUST" = "true" ]; then
  RC=$(http PUT "$BASE/api/system/auth/password" "$A" \
    "{\"old_password\":\"$ADMIN_PW\",\"new_password\":\"$ADMIN_NEW\"}")
  eq "首登改密 -> 200" "$RC" "200"
  RC=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"admin\",\"password\":\"$ADMIN_NEW\"}")
  A=$(body | jget data.access_token)
  [ -n "$A" ] || { bad "改密后重登失败"; summary; }
  ok "改密后新令牌就绪"
fi

# ---------- 1) 夹具清理（前缀 P306_ / p306_，隔离库专用，可复跑） ----------
reset_fixtures() {
  local BIZLIST="'P306_NULL','P306_ALL','P306_MIX','P306_FRZ'"
  q "$DB" "delete from operation_logs where target_id in (
             select id::text from biz_flow_instance where biz_type in ($BIZLIST))" >/dev/null
  q "$DB" "delete from biz_flow_task where instance_id in (
             select id from biz_flow_instance where biz_type in ($BIZLIST))" >/dev/null
  q "$DB" "delete from biz_flow_instance_node where instance_id in (
             select id from biz_flow_instance where biz_type in ($BIZLIST))" >/dev/null
  q "$DB" "delete from biz_flow_instance where biz_type in ($BIZLIST)" >/dev/null
  q "$DB" "delete from biz_flow_def_version where def_id in (
             select id from biz_flow_def where flow_code like 'p306\_%')" >/dev/null
  q "$DB" "delete from biz_flow_def where flow_code like 'p306\_%'" >/dev/null
  q "$DB" "delete from sys_role_menu where role_id in (
             select id from sys_role where role_code like 'P306\_%')" >/dev/null
  q "$DB" "delete from sys_user_role where role_id in (
             select id from sys_role where role_code like 'P306\_%')" >/dev/null
  q "$DB" "delete from sys_user_role where user_id in (
             select id from sys_user where username like 'p306\_%')" >/dev/null
  q "$DB" "delete from sys_user where username like 'p306\_%'" >/dev/null
  q "$DB" "delete from sys_role where role_code like 'P306\_%'" >/dev/null
  say "  [reset] 上一轮夹具已清空（可复跑）"
}
reset_fixtures

mk_role() { # mk_role CODE NAME -> role_id
  local code=$1 name=$2 rc
  rc=$(http POST "$BASE/api/system/role" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"status\":1,\"sort_order\":970}")
  [ "$rc" = "200" ] || { say "  [curl] create role $code http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}

mk_user() { # mk_user USERNAME REAL ROLE_ID -> user_id
  local uname=$1 real=$2 role=$3 rc
  rc=$(http POST "$BASE/api/system/user" "$A" \
    "{\"username\":\"$uname\",\"password\":\"P306@2026\",\"real_name\":\"$real\",\"status\":1,\"role_ids\":[\"$role\"]}")
  [ "$rc" = "200" ] || { say "  [curl] create user $uname http=$rc $(body)" >&2; return 1; }
  body | jget data.id
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

R_APPR=$(mk_role P306_APPR "P3-06 三人审批角色")
R_INIT=$(mk_role P306_INIT "P3-06 发起人角色")
R_MIX=$(mk_role P306_MIX "P3-06 含发起人的审批角色")
is_uuid "$R_APPR" && is_uuid "$R_INIT" && is_uuid "$R_MIX" && ok "3 个角色就绪" \
  || { bad "建角色失败：$(body)"; summary; }

U_A=$(mk_user p306_a "P3-06 制单人" "$R_INIT") || { bad "建用户 A 失败"; summary; }
U_B=$(mk_user p306_b "P3-06 乙" "$R_APPR") || { bad "建用户 B 失败"; summary; }
U_C=$(mk_user p306_c "P3-06 丙" "$R_APPR") || { bad "建用户 C 失败"; summary; }
U_D=$(mk_user p306_d "P3-06 丁" "$R_APPR") || { bad "建用户 D 失败"; summary; }
ok "4 个用户就绪（制单人 A + 审批人 B/C/D）"

FLOW_CODES=(business:flow:instance:create business:flow:instance:read business:flow:approve)
eq "R_APPR 持有 3 个实例侧码" "$(grant "$R_APPR" "${FLOW_CODES[@]}")" "3"
eq "R_INIT 持有 3 个实例侧码" "$(grant "$R_INIT" "${FLOW_CODES[@]}")" "3"
eq "R_MIX 持有 3 个实例侧码" "$(grant "$R_MIX" "${FLOW_CODES[@]}")" "3"

# A 同时进 R_MIX（ARCH-8 场景 A：制单人在审批角色里）；B/C 也进 R_MIX（该角色 3 个候选）
for u in "$U_A" "$U_B" "$U_C"; do
  q "$DB" "insert into sys_user_role (user_id, role_id) values ('$u'::uuid,'$R_MIX'::uuid) on conflict do nothing" >/dev/null
done
eq "R_APPR 成员数 = 3（B/C/D）" \
  "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id
              where ur.role_id='$R_APPR'::uuid and u.deleted_at is null and u.status=1")" "3"
eq "R_MIX 成员数 = 3（A/B/C，含制单人）" \
  "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id
              where ur.role_id='$R_MIX'::uuid and u.deleted_at is null and u.status=1")" "3"

# ---------- 2) 流程夹具 ----------
# MODE 传 "-" 表示**不写** approval_mode（= 库里的 NULL，E1 要用这条证明默认或签）
mk_flow() { # mk_flow CODE NAME BIZ ROLE MODE -> def_id
  local code=$1 name=$2 biz=$3 role=$4 mode=$5 rc def vid graph mode_json
  if [ "$mode" = "-" ]; then mode_json=""; else mode_json=",\"approval_mode\":\"$mode\""; fi
  rc=$(http POST "$BASE/api/business/flow/def" "$A" \
    "{\"flow_code\":\"$code\",\"flow_name\":\"$name\",\"biz_type\":\"$biz\",\"is_default\":true}")
  [ "$rc" = "200" ] || { say "  [curl] create def $code http=$rc $(body)" >&2; return 1; }
  def=$(body | jget data.id); vid=$(body | jget data.version_id)
  graph=$(cat <<JSON
{
  "change_note": "P3-06 夹具：单审批节点（ROLE=$role / mode=${mode}）",
  "nodes": [
    {"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
    {"key":"n_approve","name":"部门经理审批","type":"APPROVAL","x":220,"y":0,"sort_order":2,
     "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$role","sort_order":0}]$mode_json,
     "self_approval":"SKIP","empty_assignee":"ERROR",
     "join_mode":"AND","fork_mode":"AND","reject_veto":true},
    {"key":"e","name":"结束","type":"END","x":440,"y":0,"sort_order":3}
  ],
  "edges": [
    {"key":"e_start","from":"s","to":"n_approve"},
    {"key":"e_end","from":"n_approve","to":"e"}
  ]
}
JSON
)
  rc=$(http PUT "$BASE/api/business/flow/version/$vid/graph" "$A" "$graph")
  [ "$rc" = "200" ] || { say "  [curl] save graph $code http=$rc $(body)" >&2; return 1; }
  rc=$(http POST "$BASE/api/business/flow/def/$def/publish" "$A" "")
  [ "$rc" = "200" ] || { say "  [curl] publish $code http=$rc $(body)" >&2; return 1; }
  printf '%s' "$def"
}

F_NULL=$(mk_flow p306_null "P3-06 不填模式（默认或签）" P306_NULL "$R_APPR" -)
is_uuid "$F_NULL" && ok "E1 夹具：APPROVAL 不填 approval_mode 发布成功（R9 已放宽）" \
  || { bad "E1 流程发布失败：$(body)"; summary; }
F_ALL=$(mk_flow p306_all "P3-06 显式全员签" P306_ALL "$R_APPR" ALL)
is_uuid "$F_ALL" && ok "E2 夹具：APPROVAL 显式 ALL 发布成功" || { bad "E2 流程发布失败"; summary; }
F_MIX=$(mk_flow p306_mix "P3-06 制单人在角色里（全员签）" P306_MIX "$R_MIX" ALL)
is_uuid "$F_MIX" && ok "E3 夹具：ARCH-8 A（角色含制单人）发布成功" || { bad "E3 流程发布失败"; summary; }

# ---------- 3) 业务令牌 ----------
TOK_A=$(login p306_a P306@2026 P306newA@2026) || { bad "A 登录失败：$(body)"; summary; }
TOK_B=$(login p306_b P306@2026 P306newB@2026) || { bad "B 登录失败：$(body)"; summary; }
TOK_C=$(login p306_c P306@2026 P306newC@2026) || { bad "C 登录失败：$(body)"; summary; }
TOK_D=$(login p306_d P306@2026 P306newD@2026) || { bad "D 登录失败：$(body)"; summary; }
ok "4 个业务令牌就绪"

start_instance() { # start_instance TOKEN CODE TITLE -> instance_id
  local tok=$1 code=$2 title=$3 rc
  rc=$(http POST "$BASE/api/business/flow/instance" "$tok" \
    "{\"flow_code\":\"$code\",\"title\":\"$title\",\"form_data\":{\"amount\":100,\"approver_id\":\"$U_B\"}}")
  LAST_RC="$rc"
  [ "$rc" = "200" ] || return 1
  body | jget data.instance_id
}

approve() { # approve TOKEN TASK_ID COMMENT -> http code
  http POST "$BASE/api/business/flow/task/$2/approve" "$1" "{\"comment\":\"$3\"}"
}

task_status() { q "$DB" "select status from biz_flow_task where instance_id='$1'::uuid and assignee_id='$2'::uuid"; }
task_of() { q "$DB" "select id from biz_flow_task where instance_id='$1'::uuid and assignee_id='$2'::uuid"; }
inst_status() { q "$DB" "select status from biz_flow_instance where id='$1'::uuid"; }
node_status() { q "$DB" "select status from biz_flow_instance_node where instance_id='$1'::uuid and node_key='n_approve'"; }
pending_count() { q "$DB" "select count(*) from biz_flow_task where instance_id='$1'::uuid and status='PENDING'"; }

# ============================================================================
say ""
say "=== E1 APPROVAL + approval_mode = NULL -> 或签：任一 1 人同意即通过 ==="
# ============================================================================
I1=$(start_instance "$TOK_A" p306_null "[P3-06] 不填模式单")
is_uuid "$I1" || { bad "E1 发起失败（http=$LAST_RC / $(body)）"; summary; }
eq "E1 节点行落库的 approval_mode 仍是 NULL（不填就不编一个值出来）" \
  "$(q "$DB" "select coalesce(approval_mode,'<NULL>') from biz_flow_instance_node where instance_id='$I1'::uuid and node_key='n_approve'")" \
  "<NULL>"
eq "E1 应到 3 人（B/C/D）" "$(pending_count "$I1")" "3"
T1B=$(task_of "$I1" "$U_B"); T1C=$(task_of "$I1" "$U_C"); T1D=$(task_of "$I1" "$U_D")
is_uuid "$T1B" && is_uuid "$T1C" && is_uuid "$T1D" && ok "E1 三条待办分别落在 B/C/D" \
  || { bad "E1 待办没派全"; summary; }

RC=$(approve "$TOK_B" "$T1B" "或签，我一个人签就行")
eq "E1 B 一人同意 -> 200" "$RC" "200"
eq "E1 实例已通过（不等另外两人）" "$(inst_status "$I1")" "APPROVED"
eq "E1 节点状态 APPROVED" "$(node_status "$I1")" "APPROVED"
eq "E1 B 的票 = APPROVED" "$(task_status "$I1" "$U_B")" "APPROVED"
eq "E1 C 的待办 = CANCELLED" "$(task_status "$I1" "$U_C")" "CANCELLED"
eq "E1 D 的待办 = CANCELLED" "$(task_status "$I1" "$U_D")" "CANCELLED"
contains "E1 作废原因写明「无需再投」" \
  "$(q "$DB" "select comment from biz_flow_task where id='$T1C'::uuid")" "无需再投"
eq "E1 只剩 0 条 PENDING 待办" "$(pending_count "$I1")" "0"

# ============================================================================
say ""
say "=== E2 同角色 + 显式 ALL -> 必须 3 人都签（回归：新默认不能带跑显式全员签）==="
# ============================================================================
I2=$(start_instance "$TOK_A" p306_all "[P3-06] 全员签单")
is_uuid "$I2" || { bad "E2 发起失败（http=$LAST_RC / $(body)）"; summary; }
eq "E2 节点行 approval_mode = ALL" \
  "$(q "$DB" "select approval_mode from biz_flow_instance_node where instance_id='$I2'::uuid and node_key='n_approve'")" "ALL"
eq "E2 应到 3 人" "$(pending_count "$I2")" "3"
T2B=$(task_of "$I2" "$U_B"); T2C=$(task_of "$I2" "$U_C"); T2D=$(task_of "$I2" "$U_D")

RC=$(approve "$TOK_B" "$T2B" "第一票")
eq "E2 B 同意 -> 200" "$RC" "200"
eq "E2 实例仍 PENDING（1/3 不够）" "$(inst_status "$I2")" "PENDING"
eq "E2 节点仍 PENDING（进行中 = ACTIVE，不是 APPROVED）" "$(node_status "$I2")" "ACTIVE"
eq "E2 剩 2 条待办" "$(pending_count "$I2")" "2"
eq "E2 C 的待办没被误作废（ALL 不提前通过）" "$(task_status "$I2" "$U_C")" "PENDING"

RC=$(approve "$TOK_C" "$T2C" "第二票")
eq "E2 C 同意 -> 200" "$RC" "200"
eq "E2 实例仍 PENDING（2/3 不够）" "$(inst_status "$I2")" "PENDING"
eq "E2 还剩 1 条待办（D）" "$(task_status "$I2" "$U_D")" "PENDING"

RC=$(approve "$TOK_D" "$T2D" "第三票")
eq "E2 D 同意 -> 200" "$RC" "200"
eq "E2 3 人都签完 -> 实例通过" "$(inst_status "$I2")" "APPROVED"

# ============================================================================
say ""
say "=== E3 回归 ARCH-8 场景 A：制单人在审批角色里（角色 3 人）-> SKIP 后剩 2 人 ==="
say "    该节点 approval_mode = ALL（迁移会冻结成 ALL：候选人数 3 >= 2）-> 剩下 2 人**都要签**"
# ============================================================================
I3=$(start_instance "$TOK_A" p306_mix "[P3-06] 制单人在角色里")
is_uuid "$I3" || { bad "E3 发起失败（http=$LAST_RC / $(body)）"; summary; }
eq "E3 节点 approval_mode = ALL（迁移口径：ROLE 3 人 -> 全员签）" \
  "$(q "$DB" "select approval_mode from biz_flow_instance_node where instance_id='$I3'::uuid and node_key='n_approve'")" "ALL"
eq "E3 制单人那一票 SKIPPED（self_approval=SKIP）" "$(task_status "$I3" "$U_A")" "SKIPPED"
eq "E3 剩 2 条 PENDING（B/C）" "$(pending_count "$I3")" "2"
T3B=$(task_of "$I3" "$U_B"); T3C=$(task_of "$I3" "$U_C")

RC=$(approve "$TOK_B" "$T3B" "第一票")
eq "E3 B 同意 -> 200" "$RC" "200"
eq "E3 实例仍 PENDING（ALL：2 人里第 2 人还没签）" "$(inst_status "$I3")" "PENDING"
eq "E3 C 仍 PENDING" "$(task_status "$I3" "$U_C")" "PENDING"

RC=$(approve "$TOK_C" "$T3C" "第二票")
eq "E3 C 同意 -> 200" "$RC" "200"
eq "E3 2 人都签完 -> 实例通过" "$(inst_status "$I3")" "APPROVED"
say "  结论：ARCH-8 场景 A 在「迁移冻结成 ALL」的节点上是「2 人都要签」；"
say "        若该节点在库里是 NULL/ANY（或签），SKIP 后剩下任 1 人签即通过。"

# ============================================================================
say ""
say "=== E4 D2 冻结口径在真库跑一遍（ROLE 3 人->ALL / ROLE 1 人->ANY / FORM_VAR->ANY）+ 幂等 ==="
# ============================================================================
R_ONE=$(mk_role P306_ONE "P3-06 单人角色")
is_uuid "$R_ONE" || { bad "建单角色失败"; summary; }
mk_multi_flow() { # -> def_id
  local rc def vid graph
  rc=$(http POST "$BASE/api/business/flow/def" "$A" \
    "{\"flow_code\":\"p306_frz\",\"flow_name\":\"P3-06 冻结口径\",\"biz_type\":\"P306_FRZ\",\"is_default\":true}")
  [ "$rc" = "200" ] || { say "  [curl] create def http=$rc $(body)" >&2; return 1; }
  def=$(body | jget data.id); vid=$(body | jget data.version_id)
  graph=$(cat <<JSON
{
  "change_note": "P3-06 E4：三个 APPROVAL 节点都不写 approval_mode",
  "nodes": [
    {"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
    {"key":"n_three","name":"三人角色","type":"APPROVAL","x":200,"y":0,"sort_order":2,
     "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$R_APPR","sort_order":0}],
     "self_approval":"SKIP","empty_assignee":"ERROR","join_mode":"AND","fork_mode":"AND","reject_veto":true},
    {"key":"n_one","name":"单人角色","type":"APPROVAL","x":400,"y":0,"sort_order":3,
     "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$R_ONE","sort_order":0}],
     "self_approval":"SKIP","empty_assignee":"ERROR","join_mode":"AND","fork_mode":"AND","reject_veto":true},
    {"key":"n_var","name":"表单变量","type":"APPROVAL","x":600,"y":0,"sort_order":4,
     "assignee_type":"FORM_VAR","form_var_key":"approver_id",
     "self_approval":"SKIP","empty_assignee":"ERROR","join_mode":"AND","fork_mode":"AND","reject_veto":true},
    {"key":"e","name":"结束","type":"END","x":800,"y":0,"sort_order":5}
  ],
  "edges": [
    {"key":"e_s","from":"s","to":"n_three"},
    {"key":"e_1","from":"n_three","to":"n_one"},
    {"key":"e_2","from":"n_one","to":"n_var"},
    {"key":"e_3","from":"n_var","to":"e"}
  ]
}
JSON
)
  rc=$(http PUT "$BASE/api/business/flow/version/$vid/graph" "$A" "$graph")
  [ "$rc" = "200" ] || { say "  [curl] save graph http=$rc $(body)" >&2; return 1; }
  # 故意**不发布**：V0339 的 SQL 只处理草稿档（已发布档归启动期冻结器，需要同改快照+hash）
  printf '%s' "$def"
}
F_FRZ=$(mk_multi_flow) || { bad "E4 流程保存失败：$(body)"; summary; }
is_uuid "$F_FRZ" && ok "E4 三节点草稿图保存成功（三条都不写模式）" || { bad "E4 流程保存失败"; summary; }

mode_of() { # mode_of NODE_KEY -> mode 或 <NULL>
  q "$DB" "select coalesce(n.approval_mode,'<NULL>') from biz_flow_node n
             join biz_flow_def_version v on v.id = n.version_id
             join biz_flow_def d on d.id = v.def_id
            where d.flow_code='p306_frz' and n.node_key='$1'"
}
eq "E4 冻结前：n_three 是 NULL" "$(mode_of n_three)" "<NULL>"
eq "E4 冻结前：n_one 是 NULL" "$(mode_of n_one)" "<NULL>"
eq "E4 冻结前：n_var 是 NULL" "$(mode_of n_var)" "<NULL>"

FREPLAY=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0339__approval_sign_mode_freeze.up.sql 2>&1)
printf '%s\n' "$FREPLAY" | grep -E 'UPDATE|NOTICE' || true
eq "E4 冻结后：ROLE 3 人 -> ALL" "$(mode_of n_three)" "ALL"
eq "E4 冻结后：ROLE 1 人 -> ANY（ALL/ANY 在 1 人时判定等价）" "$(mode_of n_one)" "ANY"
eq "E4 冻结后：FORM_VAR 算不出来 -> ANY" "$(mode_of n_var)" "ANY"

FREPLAY2=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0339__approval_sign_mode_freeze.up.sql 2>&1)
eq "E4 重放 up.sql：第一条 UPDATE 改 0 行（幂等）" \
  "$(printf '%s\n' "$FREPLAY2" | grep -c '^UPDATE 0$')" "1"

FDOWN=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0339__approval_sign_mode_freeze.down.sql 2>&1)
printf '%s\n' "$FDOWN" | grep -E 'NOTICE' || true
eq "E4 down 后：n_three 回到 NULL" "$(mode_of n_three)" "<NULL>"
eq "E4 down 后：n_one 回到 NULL" "$(mode_of n_one)" "<NULL>"
eq "E4 down 后：n_var 回到 NULL" "$(mode_of n_var)" "<NULL>"

# 复原（把 E4 库留在「已冻结」状态，便于事后复查）
psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0339__approval_sign_mode_freeze.up.sql >/dev/null 2>&1
eq "E4 复原后：n_three 又是 ALL" "$(mode_of n_three)" "ALL"

stop_instance && ok "实例已停（库与日志留档备查）"
summary
