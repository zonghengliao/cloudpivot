#!/usr/bin/env bash
# ============================================================================
# P3-05 验收 ②：ALLOW 自审的配套约束（审计日志 + 金额阈值 + 月度报表）真 HTTP 实测
#
# 卡面验收原文（逐条对照）：
#   · 一人公司场景（唯一审批人 = 制单人，节点 ALLOW）→ 自审通过 → operation_logs 有
#     1 行可读记录 → /stats/self-approval?month= 返回该单（件数 1、金额正确、占比 100%）
#   · 超阈值场景：金额 5000 > 阈值 1000 → 自审被拦，报文 / 留痕写明阈值
#   · 回归：self_approval=SKIP 与 FORBID 行为不变（SKIP 仍走 P3-02 升级链，FORBID 仍 65019）
#
# 全部走**真 HTTP**（隔离实例 + 独立空库），夹具用接口现建（不依赖种子流程）。
#
# 环境：实例与库由 10_upgrade_migration.sh 备好；本脚本只消费它们。
#   端口 18305 / 库 cloudpivot_p305_ut（证据里写清，便于复查）
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

BASE=${BASE:-http://127.0.0.1:18305}
DB=${DB:-cloudpivot_p305_ut}
OUT="$RAW/20_e2e_scenarios.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

ADMIN_PW=${ADMIN_PW:-P305@2026}
ADMIN_NEW=${ADMIN_NEW:-P305new@2026}

say "================ P3-05 ② ALLOW 自审配套（$BASE / $DB）================"

# ---------- 0) 管理员登录（首登强制改密） ----------
RC=$(http POST "$BASE/api/system/auth/login" "" \
  "{\"username\":\"admin\",\"password\":\"$ADMIN_PW\"}")
if [ "$RC" = "200" ]; then
  A=$(body | jget data.access_token)
  # ⚠️ `jget` 走 python 打印，布尔是 `True`/`False`（不是 JSON 的 true/false）→ 统一小写再比
  MUST=$(body | jget data.must_change_password | tr 'A-Z' 'a-z')
else
  # 已经改过密（脚本可重复执行）：直接用新口令
  RC=$(http POST "$BASE/api/system/auth/login" "" \
    "{\"username\":\"admin\",\"password\":\"$ADMIN_NEW\"}")
  A=$(body | jget data.access_token)
  MUST=false
fi
[ -n "$A" ] || { bad "管理员登录失败：$(body)"; summary; }
ok "管理员登录成功"

if [ "$MUST" = "true" ]; then
  RC=$(http PUT "$BASE/api/system/auth/password" "$A" \
    "{\"old_password\":\"$ADMIN_PW\",\"new_password\":\"$ADMIN_NEW\"}")
  eq "首登改密 → 200" "$RC" "200"
  A=$(login_admin "$BASE" "$ADMIN_NEW") || { bad "改密后重新登录失败"; summary; }
  ok "改密后新令牌就绪"
fi

# ---------- 1) 夹具：两个角色 + 两个用户 ----------
# 可复跑：先把本卡遗留的夹具按 FK 顺序清掉（隔离库专用，前缀 P305_ / p305_）。
reset_fixtures() {
  local BIZLIST="'P305_SOLO','P305_DEG','P305_SKIP','P305_FORBID'"
  q "$DB" "delete from operation_logs where target_id in (
             select id::text from biz_flow_instance where biz_type in ($BIZLIST))" >/dev/null
  q "$DB" "delete from biz_flow_task where instance_id in (
             select id from biz_flow_instance where biz_type in ($BIZLIST))" >/dev/null
  q "$DB" "delete from biz_flow_instance_node where instance_id in (
             select id from biz_flow_instance where biz_type in ($BIZLIST))" >/dev/null
  q "$DB" "delete from biz_flow_instance where biz_type in ($BIZLIST)" >/dev/null
  q "$DB" "delete from biz_flow_def_version where def_id in (
             select id from biz_flow_def where flow_code like 'p305\_%')" >/dev/null
  q "$DB" "delete from biz_flow_def where flow_code like 'p305\_%'" >/dev/null
  q "$DB" "delete from sys_role_menu where role_id in (
             select id from sys_role where role_code like 'P305\_%')" >/dev/null
  q "$DB" "delete from sys_user_role where role_id in (
             select id from sys_role where role_code like 'P305\_%')" >/dev/null
  q "$DB" "delete from sys_user_role where user_id in (
             select id from sys_user where username like 'p305\_%')" >/dev/null
  q "$DB" "delete from sys_user where username like 'p305\_%'" >/dev/null
  q "$DB" "delete from sys_role where role_code like 'P305\_%'" >/dev/null
  say "  [reset] 上一轮夹具已清空（可复跑）"
}
reset_fixtures

mk_role() { # mk_role CODE NAME → 打印 role_id
  local code=$1 name=$2 rc
  rc=$(http POST "$BASE/api/system/role" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"status\":1,\"sort_order\":960}")
  [ "$rc" = "200" ] || rc=$(http POST "$BASE/api/system/role" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"status\":1,\"sort_order\":960}")
  body | jget data.id
}

mk_user() { # mk_user USERNAME REAL ROLE_ID → 打印 user_id
  local uname=$1 real=$2 role=$3 rc
  rc=$(http POST "$BASE/api/system/user" "$A" \
    "{\"username\":\"$uname\",\"password\":\"P305@2026\",\"real_name\":\"$real\",\"status\":1,\"role_ids\":[\"$role\"]}")
  [ "$rc" = "200" ] || { say "  [curl] create user $uname → http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}

# 角色 A：一人公司（成员只有 A = 既是发起人又是唯一审批人）
R_SOLO=$(mk_role P305_SOLO "P3-05 一人公司")
# 角色 D：两人（A + B），用于「超阈值 → 降级给别人」
R_DEG=$(mk_role P305_DEG "P3-05 双人审批")
# 角色 S：只收发起人（SKIP / FORBID 回归）
R_REGR=$(mk_role P305_REGR "P3-05 回归角色")
is_uuid "$R_SOLO" && is_uuid "$R_DEG" && is_uuid "$R_REGR" && ok "3 个角色就绪" \
  || { bad "建角色失败：$(body)"; summary; }

U_A=$(mk_user p305_a "P3-05 甲乙丙" "$R_SOLO") || { bad "建用户 A 失败"; summary; }
U_B=$(mk_user p305_b "P3-05 第二审批人" "$R_DEG") || { bad "建用户 B 失败"; summary; }
is_uuid "$U_A" && is_uuid "$U_B" && ok "2 个用户就绪（A=$U_A / B=$U_B）" || { bad "建用户失败"; summary; }

# 授权：发起 / 审批 / 读实例（菜单承载行 → sys_role_menu 即权限授予的唯一道）
grant() { # grant ROLE_ID CODES... → 打印实际持有的码数
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
FLOW_CODES=(business:flow:instance:create business:flow:instance:read business:flow:approve)
eq "R_SOLO 持有 3 个实例侧码" "$(grant "$R_SOLO" "${FLOW_CODES[@]}")" "3"
eq "R_DEG 持有 3 个实例侧码" "$(grant "$R_DEG" "${FLOW_CODES[@]}")" "3"
eq "R_REGR 持有 3 个实例侧码" "$(grant "$R_REGR" "${FLOW_CODES[@]}")" "3"

# A 同时进 R_DEG（双人节点上 A 也是候选）与 R_REGR（回归节点上 A 是候选）
q "$DB" "insert into sys_user_role (user_id, role_id) values ('$U_A'::uuid,'$R_DEG'::uuid) on conflict do nothing" >/dev/null
q "$DB" "insert into sys_user_role (user_id, role_id) values ('$U_A'::uuid,'$R_REGR'::uuid) on conflict do nothing" >/dev/null
eq "R_SOLO 成员数 = 1（一人公司）" \
  "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$R_SOLO'::uuid and u.deleted_at is null")" "1"
eq "R_DEG 成员数 = 2（A + B）" \
  "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$R_DEG'::uuid and u.deleted_at is null")" "2"

# ---------- 2) 夹具：四条流程（ALLOW 单人 / ALLOW 双人 / SKIP / FORBID） ----------
mk_flow() { # mk_flow CODE NAME BIZ ROLE POLICY MODE → 打印 def_id
  local code=$1 name=$2 biz=$3 role=$4 policy=$5 mode=$6 rc def vid graph
  rc=$(http POST "$BASE/api/business/flow/def" "$A" \
    "{\"flow_code\":\"$code\",\"flow_name\":\"$name\",\"biz_type\":\"$biz\",\"is_default\":true}")
  [ "$rc" = "200" ] || { say "  [curl] create def $code → http=$rc $(body)" >&2; return 1; }
  def=$(body | jget data.id); vid=$(body | jget data.version_id)
  graph=$(cat <<JSON
{
  "change_note": "P3-05 夹具：单审批节点（ROLE=$role / self_approval=$policy / mode=$mode）",
  "nodes": [
    {"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
    {"key":"n_approve","name":"部门经理审批","type":"APPROVAL","x":220,"y":0,"sort_order":2,
     "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$role","sort_order":0}],
     "approval_mode":"$mode","self_approval":"$policy","empty_assignee":"ERROR",
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
  [ "$rc" = "200" ] || { say "  [curl] save graph $code → http=$rc $(body)" >&2; return 1; }
  rc=$(http POST "$BASE/api/business/flow/def/$def/publish" "$A" "")
  [ "$rc" = "200" ] || { say "  [curl] publish $code → http=$rc $(body)" >&2; return 1; }
  printf '%s' "$def"
}

F_SOLO=$(mk_flow p305_solo "P3-05 一人公司（ALLOW）" P305_SOLO "$R_SOLO" ALLOW ANY)
F_DEG=$(mk_flow p305_deg "P3-05 超阈值降级（ALLOW 双人）" P305_DEG "$R_DEG" ALLOW ANY)
F_SKIP=$(mk_flow p305_skip "P3-05 回归（SKIP）" P305_SKIP "$R_REGR" SKIP ANY)
F_FORBID=$(mk_flow p305_forbid "P3-05 回归（FORBID）" P305_FORBID "$R_REGR" FORBID ANY)
for f in "$F_SOLO" "$F_DEG" "$F_SKIP" "$F_FORBID"; do is_uuid "$f" || { bad "流程发布失败"; summary; }; done
ok "4 张流程发布成功（ALLOW 单人 / ALLOW 双人 / SKIP / FORBID）"

# ---------- 3) 业务令牌 ----------
TOK_A=$(login p305_a P305@2026 P305newA@2026) || { bad "A 登录失败：$(body)"; summary; }
TOK_B=$(login p305_b P305@2026 P305newB@2026) || { bad "B 登录失败：$(body)"; summary; }
ok "两个业务令牌就绪（A 独自一人 / B 第二审批人）"

start_instance() { # start_instance TOKEN CODE TITLE AMOUNT → 打印 instance_id
  local tok=$1 code=$2 title=$3 amount=$4 rc
  rc=$(http POST "$BASE/api/business/flow/instance" "$tok" \
    "{\"flow_code\":\"$code\",\"title\":\"$title\",\"form_data\":{\"amount\":$amount}}")
  LAST_RC="$rc"
  [ "$rc" = "200" ] || return 1
  body | jget data.instance_id
}

# 阈值归零（默认口径）—— 保证 E1 从「不限额」起步
CFG_ID=$(q "$DB" "select id from sys_config where config_key='erp.separation_of_duties.self_approval_max_amount' and deleted_at is null")
is_uuid "$CFG_ID" || { bad "找不到自审金额上限参数（V0338 没生效？）"; summary; }
set_threshold() { # set_threshold VALUE
  local rc
  rc=$(http PUT "$BASE/api/support/config/$CFG_ID" "$A" \
    "{\"config_key\":\"erp.separation_of_duties.self_approval_max_amount\",\"config_value\":\"$1\",\"config_type\":\"NUMBER\"}")
  [ "$rc" = "200" ] || { say "  [curl] set threshold=$1 → http=$rc $(body)" >&2; return 1; }
  sleep 1  # 参数写后广播重载（快照 TTL 60s，主动 reload 是即时的；这条 sleep 只为稳妥）
}

set_threshold 0 || { bad "设定阈值失败"; summary; }
eq "参数落库：阈值 = 0（不限额）" \
  "$(q "$DB" "select config_value from sys_config where id='$CFG_ID'::uuid")" "0"

# ============================================================================
say ""
say "=== E1 一人公司：唯一审批人 = 发起人，节点 ALLOW，阈值 0（不限额）==="
# ============================================================================
I1=$(start_instance "$TOK_A" p305_solo "[P3-05] 一人公司自审单" 5000)
is_uuid "$I1" || { bad "E1 发起失败：$(body)"; summary; }
T1=$(q "$DB" "select id from biz_flow_task where instance_id='$I1'::uuid and status='PENDING'")
is_uuid "$T1" || { bad "E1 没派到待办"; summary; }
eq "E1 待办落在发起人本人（A）" \
  "$(q "$DB" "select assignee_id from biz_flow_task where id='$T1'::uuid")" "$U_A"
eq "E1 待办带自审标 is_self_approval = true" \
  "$(q "$DB" "select case when is_self_approval then 'true' else 'false' end from biz_flow_task where id='$T1'::uuid")" "true"

ALLOW_LOGS=$(q "$DB" "select count(*) from operation_logs where operation='FLOW_SELF_APPROVAL_ALLOWED' and target_id='$I1'")
eq "E1 落 PENDING 时已写「允许自审」审计 1 行" "$ALLOW_LOGS" "1"

RC=$(http POST "$BASE/api/business/flow/task/$T1/approve" "$TOK_A" \
  "{\"comment\":\"一人公司，我自己批\"}")
eq "E1 自审同意 → 200" "$RC" "200"
eq "E1 实例已通过" \
  "$(q "$DB" "select status from biz_flow_instance where id='$I1'::uuid")" "APPROVED"

say ""
say "  --- E1 审计留痕（operation_logs 原文）---"
q "$DB" "select operation || ' | ' || username || ' | ' || detail->>'node_name' || ' | 金额 ' || (detail->>'amount') || ' | ' || (detail->>'reason') from operation_logs where target_id='$I1' order by created_at"
eq "E1 两张审计行（ALLOWED + VOTED）" \
  "$(q "$DB" "select count(*) from operation_logs where target_id='$I1' and operation like 'FLOW_SELF_APPROVAL_%'")" "2"
eq "E1 「真自审了」那行带投票结论 APPROVED" \
  "$(q "$DB" "select detail->>'vote' from operation_logs where target_id='$I1' and operation='FLOW_SELF_APPROVAL_VOTED'")" "APPROVED"
eq "E1 审计行账号列可读（= p305_a）" \
  "$(q "$DB" "select username from operation_logs where target_id='$I1' and operation='FLOW_SELF_APPROVAL_VOTED'")" "p305_a"
eq "E1 审计行节点名可读" \
  "$(q "$DB" "select detail->>'node_name' from operation_logs where target_id='$I1' and operation='FLOW_SELF_APPROVAL_VOTED'")" "部门经理审批"

say ""
say "  --- E1 月度报表（GET /business/flow/stats/self-approval?biz_type=P305_SOLO）---"
MONTH=$(date -u -d '+8 hours' +%Y-%m)
RC=$(http GET "$BASE/api/business/flow/stats/self-approval?month=$MONTH&biz_type=P305_SOLO" "$A" "")
eq "E1 报表接口 → 200" "$RC" "200"
say "  [报表 JSON] $(body)"
eq "E1 报表：自审单数 = 1" "$(body | jget data.self_approval_instances)" "1"
eq "E1 报表：金额 = 5000" "$(body | jget data.self_approval_amount)" "5000"
eq "E1 报表：分母（同期已结算）= 1" "$(body | jget data.settled_instances)" "1"
eq "E1 报表：占比 = 1.0（100%，JSON 里是浮点）" "$(body | jget data.ratio)" "1.0"
eq "E1 报表：按节点节点名" "$(body | jget data.by_node.0.node_name)" "部门经理审批"
eq "E1 报表：按人姓名" "$(body | jget data.by_person.0.assignee_name)" "P3-05 甲乙丙"
eq "E1 报表：明细里就是这张单" "$(body | jget data.list.0.instance_id)" "$I1"

# ============================================================================
say ""
say "=== E2 阈值 1000 + 金额 5000 + 双人节点 → 发起人被降级，铁票给 B ==="
# ============================================================================
set_threshold 1000 || { bad "设定阈值失败"; summary; }
eq "参数落库：阈值 = 1000" \
  "$(q "$DB" "select config_value from sys_config where id='$CFG_ID'::uuid")" "1000"

I2=$(start_instance "$TOK_A" p305_deg "[P3-05] 超阈值降级单" 5000)
is_uuid "$I2" || { bad "E2 发起失败（http=$LAST_RC / $(body)）"; summary; }
eq "E2 发起人 A 的票被降级为 SKIPPED" \
  "$(q "$DB" "select status from biz_flow_task where instance_id='$I2'::uuid and assignee_id='$U_A'")" "SKIPPED"
contains "E2 降级留痕写明上限" \
  "$(q "$DB" "select comment from biz_flow_task where instance_id='$I2'::uuid and assignee_id='$U_A'")" \
  "超出自审金额上限"
contains "E2 降级留痕写明金额" \
  "$(q "$DB" "select comment from biz_flow_task where instance_id='$I2'::uuid and assignee_id='$U_A'")" "5000.00"
eq "E2 第二审批人 B 拿到 PENDING" \
  "$(q "$DB" "select status from biz_flow_task where instance_id='$I2'::uuid and assignee_id='$U_B'")" "PENDING"
eq "E2 被拦日志 FLOW_SELF_APPROVAL_BLOCKED 写了 1 行" \
  "$(q "$DB" "select count(*) from operation_logs where operation='FLOW_SELF_APPROVAL_BLOCKED' and target_id='$I2'")" "1"
say "  [被拦日志] $(q "$DB" "select detail->>'reason' from operation_logs where operation='FLOW_SELF_APPROVAL_BLOCKED' and target_id='$I2'")"

T2=$(q "$DB" "select id from biz_flow_task where instance_id='$I2'::uuid and assignee_id='$U_B' and status='PENDING'")
RC=$(http POST "$BASE/api/business/flow/task/$T2/approve" "$TOK_B" "{\"comment\":\"我替公司批\"}")
eq "E2 B 审批 → 200" "$RC" "200"
eq "E2 实例通过" "$(q "$DB" "select status from biz_flow_instance where id='$I2'::uuid")" "APPROVED"

RC=$(http GET "$BASE/api/business/flow/stats/self-approval?month=$MONTH&biz_type=P305_DEG" "$A" "")
eq "E2 报表接口 → 200" "$RC" "200"
eq "E2 报表：自审单数 = 0（这单没人自审）" "$(body | jget data.self_approval_instances)" "0"
eq "E2 报表：超阈值被降级 = 1" "$(body | jget data.blocked_count)" "1"

# ============================================================================
say ""
say "=== E3 阈值 1000 + 金额 5000 + 只有 A 一个人 → 422 / 65044（报文写明阈值）==="
# ============================================================================
RC=$(http POST "$BASE/api/business/flow/instance" "$TOK_A" \
  "{\"flow_code\":\"p305_solo\",\"title\":\"[P3-05] 硬拦单\",\"form_data\":{\"amount\":5000}}")
eq "E3 发起 → 422" "$RC" "422"
eq "E3 错误码 = 65044" "$(body | jget code)" "65044"
contains "E3 报文写明「超出自审金额上限」" "$(body | jget msg)" "超出自审金额上限"
contains "E3 报文写明金额 5000.00" "$(body | jget msg)" "5000.00"
contains "E3 报文写明上限 1000.00" "$(body | jget msg)" "1000.00"
contains "E3 报文给出配置键" "$(body | jget msg)" "erp.separation_of_duties.self_approval_max_amount"
eq "E3 库里没有留下半张单（整单回滚）" \
  "$(q "$DB" "select count(*) from biz_flow_instance where title='[P3-05] 硬拦单'")" "0"
eq "E3 也没有自动通过（auto_passed_nodes 无处可查 = 单不存在）" \
  "$(q "$DB" "select count(*) from biz_flow_instance where biz_type='P305_SOLO' and status='APPROVED' and title like '%硬拦%'")" "0"

# ============================================================================
say ""
say "=== E4 阈值 1000 + 金额 500（不超）→ 照旧自审，不拦 ==="
# ============================================================================
I4=$(start_instance "$TOK_A" p305_solo "[P3-05] 小额自审单" 500)
is_uuid "$I4" || { bad "E4 发起失败：$(body)"; summary; }
eq "E4 待办仍在 A 手上（PENDING）" \
  "$(q "$DB" "select status from biz_flow_task where instance_id='$I4'::uuid and assignee_id='$U_A'")" "PENDING"
eq "E4 没有产生被拦日志" \
  "$(q "$DB" "select count(*) from operation_logs where operation='FLOW_SELF_APPROVAL_BLOCKED' and target_id='$I4'")" "0"
T4=$(q "$DB" "select id from biz_flow_task where instance_id='$I4'::uuid and status='PENDING'")
RC=$(http POST "$BASE/api/business/flow/task/$T4/approve" "$TOK_A" "{\"comment\":\"小额\"}")
eq "E4 自审同意 → 200" "$RC" "200"
eq "E4 实例通过" "$(q "$DB" "select status from biz_flow_instance where id='$I4'::uuid")" "APPROVED"

# ============================================================================
say ""
say "=== E5 回归：self_approval=SKIP（A 是唯一候选）→ 仍走 P3-02 升级链 → 422 / 65018 ==="
# ============================================================================
RC=$(http POST "$BASE/api/business/flow/instance" "$TOK_A" \
  "{\"flow_code\":\"p305_skip\",\"title\":\"[P3-05] SKIP 回归\",\"form_data\":{\"amount\":100}}")
eq "E5 发起 → 422" "$RC" "422"
eq "E5 错误码仍是 65018（不是 65044）" "$(body | jget code)" "65018"
contains "E5 报文说明升级链已走完" "$(body | jget msg)" "升级链"
eq "E5 没有写任何自审审计行" \
  "$(q "$DB" "select count(*) from operation_logs where operation like 'FLOW_SELF_APPROVAL_%' and detail->>'biz_type'='P305_SKIP'")" "0"

# ============================================================================
say ""
say "=== E6 回归：self_approval=FORBID → 仍 422 / 65019 ==="
# ============================================================================
RC=$(http POST "$BASE/api/business/flow/instance" "$TOK_A" \
  "{\"flow_code\":\"p305_forbid\",\"title\":\"[P3-05] FORBID 回归\",\"form_data\":{\"amount\":100}}")
eq "E6 发起 → 422" "$RC" "422"
eq "E6 错误码仍是 65019" "$(body | jget code)" "65019"
contains "E6 报文说明不允许自审" "$(body | jget msg)" "self_approval=FORBID"

# ============================================================================
say ""
say "=== E7 报表口径：分母是「同期已结算」，不是「同期所有单」 ==="
# ============================================================================
# 再造一张**在跑未结算**的单（P305_SOLO 业务）：分母不许把它算进去
I7=$(start_instance "$TOK_A" p305_solo "[P3-05] 在跑未结算" 300)
is_uuid "$I7" || { bad "E7 发起失败：$(body)"; summary; }
RC=$(http GET "$BASE/api/business/flow/stats/self-approval?month=$MONTH&biz_type=P305_SOLO" "$A" "")
eq "E7 报表接口 → 200" "$RC" "200"
eq "E7 分母仍是 2（E1 的 5000 + E4 的 500，在跑那张不算）" "$(body | jget data.settled_instances)" "2"
eq "E7 分子 = 2（两张都是自审通过）" "$(body | jget data.self_approval_instances)" "2"
eq "E7 金额合计 = 5500" "$(body | jget data.self_approval_amount)" "5500"
eq "E7 占比 = 1.0（分子 2 / 分母 2）" "$(body | jget data.ratio)" "1.0"

# 非法月份 → 400 / 65005
RC=$(http GET "$BASE/api/business/flow/stats/self-approval?month=2026-13" "$A" "")
eq "E7 非法 month → 400" "$RC" "400"
eq "E7 非法 month 错误码 65005" "$(body | jget code)" "65005"

say ""
say "=== 收尾：把阈值改回 0（保持隔离库可复跑） ==="
set_threshold 0 && ok "阈值已复原为 0"

summary
