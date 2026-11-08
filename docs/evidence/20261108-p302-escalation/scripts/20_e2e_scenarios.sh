#!/usr/bin/env bash
# ============================================================================
# P3-02 验收 ②③：ARCH-8 场景 B（自审跳过 → 升级上级角色）+ 场景 A / F 回归
#                 + 升级链二级上溯 + 到顶仍无人（422 报文含链路）
#
# 全部走**真 HTTP**（隔离实例 + 独立空库），结论另用 psql 断言落库事实。
#
# 夹具（全部由本脚本用 API 建，不用种子数据）：
#   角色  P302_MAKER(1 人) --escalation--> P302_BOSS(2 人)
#         P302_THREE(3 人，场景 A)
#         P302_DEAD(0 人) --escalation--> P302_EMPTY(0 人) --escalation--> P302_BOSS(2 人)
#         P302_DEAD2(0 人) --escalation--> P302_EMPTY2(0 人)   ← 到顶仍无人
#   流程  每个场景一张 p302_* 流程（START → APPROVAL → END，审批人 ROLE，approval_mode=ANY）
# 权限  测试角色按 auth_code 直接授 3 个码（instance:create / instance:read / approve），
#       不走「点权限页」—— 夹具构造，注释里写明。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

DB=${DB:-cloudpivot_p302_e2e}
PORT=${PORT:-18203}
BASE="http://127.0.0.1:$PORT"
ENVF="$RAW/e2e.env"
LOG="$RAW/20_e2e_instance.log"
PIDF="$RAW/e2e.pid"
OUT="$RAW/20_e2e_scenarios.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-02 ②③ 真 HTTP 验收（$BASE / $DB）================"

stop_instance "$PIDF"
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun $DB" || { bad "建空库失败"; summary; }
make_env "$DB" "$PORT" "$ENVF"

say "--- 0) 空库首启：全部迁移一次装起（含 V0333）"
start_instance "$ENVF" "$LOG" "$PIDF" "$PORT"
wait_http "$PORT" 180 || { bad "实例 180s 内没起来"; tail -30 "$LOG"; summary; }
MIG=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
say "首启迁移日志：${MIG:-<未找到>}"
eq "schema_migrations 里 V0333 已登记" "$(q "$DB" "select count(*) from schema_migrations where version='333' and kind='versioned'")" "1"

ADMINPW=$(awk -F= '/^NIK_ADMIN_PASSWORD/{print $2}' "$ENVF")
ADMINPW=${ADMINPW:-Admin@123456}
ADMINNEW="P302Admin@2026"

http_err() { say "$*" >&2; }

login() { # login USER PW NEWPW → 打印 token（首登强制改密会自动改）
  local user=$1 pw=$2 newpw=$3 code tok must
  code=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"$user\",\"password\":\"$pw\"}")
  if [ "$code" != "200" ]; then
    say "  [curl] login $user → http=$code $(body)" >&2
    return 1
  fi
  tok=$(body | jget data.access_token)
  must=$(body | jget data.must_change_password)
  if [ "$must" = "True" ] || [ "$must" = "true" ]; then
    say "  [curl] $user 首登强制改密 → PUT password" >&2
    code=$(http PUT "$BASE/api/system/auth/password" "$tok" \
      "{\"old_password\":\"$pw\",\"new_password\":\"$newpw\"}")
    [ "$code" = "200" ] || { say "  [curl] 改密失败 http=$code $(body)" >&2; return 1; }
    code=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"$user\",\"password\":\"$newpw\"}")
    [ "$code" = "200" ] || { say "  [curl] 改密后重登失败 http=$code $(body)" >&2; return 1; }
    tok=$(body | jget data.access_token)
  fi
  [ -n "$tok" ] || { say "  [curl] login $user → 没有 access_token：$(body)" >&2; return 1; }
  printf '%s' "$tok"
}

is_uuid() { printf '%s' "$1" | grep -qiE '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'; }

A=$(login admin "$ADMINPW" "$ADMINNEW") || { bad "admin 登录失败"; summary; }
is_uuid "$A" || true
ok "admin 令牌就绪（含首登强制改密流程）"


say ""
say "--- 1) 建角色（走 /api/system/role，顺带验证 escalation_role_id 的写入与校验）"
mk_role() { # mk_role CODE NAME → 打印 id
  local code=$1 name=$2 rc
  rc=$(http POST "$BASE/api/system/role" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"description\":\"P3-02 夹具\",\"status\":1,\"sort_order\":900,\"escalation_role_id\":\"\"}")
  [ "$rc" = "200" ] || { say "  [curl] create role $code → http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}
MAKER=$(mk_role P302_MAKER "P3-02 销售员")
BOSS=$(mk_role P302_BOSS "P3-02 销售主管")
THREE=$(mk_role P302_THREE "P3-02 三人组")
DEAD=$(mk_role P302_DEAD "P3-02 空角色（链中继）")
EMPTY=$(mk_role P302_EMPTY "P3-02 空角色（链末）")
DEAD2=$(mk_role P302_DEAD2 "P3-02 空角色（死链起点）")
EMPTY2=$(mk_role P302_EMPTY2 "P3-02 空角色（死链终点）")
SUBMIT=$(mk_role P302_SUBMIT "P3-02 提交人（只提交不审批）")
[ -n "$MAKER" ] && is_uuid "$MAKER" && is_uuid "$BOSS" && is_uuid "$EMPTY2" && is_uuid "$SUBMIT" && ok "8 个角色建成" \
  || { bad "角色创建失败（MAKER=$MAKER / BOSS=$BOSS / EMPTY2=$EMPTY2 / SUBMIT=$SUBMIT）"; summary; }

set_esc() { # set_esc ROLE_ID NAME ROLE_CODE PARENT_ID → 打印状态码
  local id=$1 name=$2 code=$3 parent=$4
  http PUT "$BASE/api/system/role/$id" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"description\":\"P3-02 夹具\",\"status\":1,\"sort_order\":900,\"escalation_role_id\":\"$parent\"}"
}
eq "P302_MAKER 的上级角色设为 P302_BOSS" "$(set_esc "$MAKER" "P3-02 销售员" P302_MAKER "$BOSS")" "200"
eq "GET 角色能回读 escalation_role_id" "$(http GET "$BASE/api/system/role/$MAKER" "$A" '' >/dev/null; body | jget data.escalation_role_id)" "$BOSS"
eq "P302_DEAD → P302_EMPTY" "$(set_esc "$DEAD" "P3-02 空角色（链中继）" P302_DEAD "$EMPTY")" "200"
eq "P302_EMPTY → P302_BOSS" "$(set_esc "$EMPTY" "P3-02 空角色（链末）" P302_EMPTY "$BOSS")" "200"
eq "P302_DEAD2 → P302_EMPTY2" "$(set_esc "$DEAD2" "P3-02 空角色（死链起点）" P302_DEAD2 "$EMPTY2")" "200"

say ""
say "--- 1b) 负例：上级角色指向自己 / 成环"
SELF_CODE=$(set_esc "$MAKER" "P3-02 销售员" P302_MAKER "$MAKER")
eq "指向自己 → 400" "$SELF_CODE" "400"
eq "指向自己 → 错误码 30013(ROLE_INVALID)" "$(body | jget code)" "30013"
contains "指向自己 → 报文" "$(body | jget msg)" "不能是本角色自己"
CYCLE_CODE=$(set_esc "$BOSS" "P3-02 销售主管" P302_BOSS "$MAKER")
eq "BOSS→MAKER 会成环（MAKER→BOSS）→ 400" "$CYCLE_CODE" "400"
contains "成环 → 报文" "$(body | jget msg)" "环"

say ""
say "--- 2) 授权（夹具：按 auth_code 直接授，等价于权限页勾选；同一码可能挂多个菜单节点，按「码」计数）"
grant() { # grant ROLE_ID CODES...
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
SUB_ALL=(business:flow:instance:create business:flow:instance:read business:flow:approve)
eq "P302_MAKER 持有 3 个码" "$(grant "$MAKER" "${SUB_ALL[@]}")" "3"
eq "P302_BOSS 持有 2 个码" "$(grant "$BOSS" business:flow:approve business:flow:instance:read)" "2"
eq "P302_THREE 持有 3 个码" "$(grant "$THREE" "${SUB_ALL[@]}")" "3"
eq "P302_SUBMIT 持有 2 个码" "$(grant "$SUBMIT" business:flow:instance:create business:flow:instance:read)" "2"

say ""
say "--- 3) 建用户（角色内成员数就是升级链的判据；P302_DEAD / P302_DEAD2 **故意 0 人**）"
mk_user() { # mk_user USERNAME REAL_NAME ROLE_ID → 打印 id
  local uname=$1 real=$2 role=$3 rc
  rc=$(http POST "$BASE/api/system/user" "$A" \
    "{\"username\":\"$uname\",\"password\":\"P302@2026\",\"real_name\":\"$real\",\"status\":1,\"role_ids\":[\"$role\"]}")
  [ "$rc" = "200" ] || { say "  [curl] create user $uname → http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}
U_MAKER=$(mk_user p302_maker "P3-02 制单人" "$MAKER")
U_BOSS1=$(mk_user p302_boss1 "P3-02 主管甲" "$BOSS")
U_BOSS2=$(mk_user p302_boss2 "P3-02 主管乙" "$BOSS")
U_T1=$(mk_user p302_t1 "P3-02 组员甲" "$THREE")
U_T2=$(mk_user p302_t2 "P3-02 组员乙" "$THREE")
U_T3=$(mk_user p302_t3 "P3-02 组员丙" "$THREE")
U_D1=$(mk_user p302_d1 "P3-02 链提交人" "$SUBMIT")
U_D2=$(mk_user p302_d2 "P3-02 死链提交人" "$SUBMIT")
[ -n "$U_MAKER" ] && is_uuid "$U_MAKER" && is_uuid "$U_D2" && ok "8 个用户建成" \
  || { bad "建用户失败（maker=$U_MAKER / d2=$U_D2）"; summary; }
eq "P302_MAKER 角色成员数（= 1，场景 B 的前提）" "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$MAKER'::uuid and u.deleted_at is null")" "1"
eq "P302_BOSS 角色成员数" "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$BOSS'::uuid and u.deleted_at is null")" "2"
eq "P302_DEAD 角色成员数（= 0，二级上溯的起点）" "$(q "$DB" "select count(*) from sys_user_role where role_id='$DEAD'::uuid")" "0"
eq "P302_DEAD2 角色成员数（= 0，死链起点）" "$(q "$DB" "select count(*) from sys_user_role where role_id='$DEAD2'::uuid")" "0"

say ""
say "--- 4) 建 5 张流程（START → APPROVAL(ROLE) → END，APPROVAL 用 ANY 或签，方便单次审批到终态）"
mk_flow() { # mk_flow CODE NAME BIZ ROLE_ID SELF_POLICY EMPTY_POLICY → 打印 def_id
  local code=$1 name=$2 biz=$3 role=$4 self=$5 empty=$6 rc def vid
  rc=$(http POST "$BASE/api/business/flow/def" "$A" \
    "{\"flow_code\":\"$code\",\"flow_name\":\"$name\",\"biz_type\":\"$biz\",\"is_default\":true}")
  [ "$rc" = "200" ] || { say "  [curl] create def $code → http=$rc $(body)" >&2; return 1; }
  def=$(body | jget data.id); vid=$(body | jget data.version_id)
  rc=$(http PUT "$BASE/api/business/flow/version/$vid/graph" "$A" "{
    \"change_note\":\"P3-02 夹具：单审批节点（$self / $empty）\",
    \"nodes\":[
      {\"key\":\"s\",\"name\":\"开始\",\"type\":\"START\",\"x\":0,\"y\":0,\"sort_order\":1},
      {\"key\":\"n_approve\",\"name\":\"审批（P3-02）\",\"type\":\"APPROVAL\",\"x\":220,\"y\":0,\"sort_order\":2,
       \"assignee_type\":\"ROLE\",\"assignees\":[{\"type\":\"ROLE\",\"ref_id\":\"$role\",\"sort_order\":0}],
       \"approval_mode\":\"ANY\",\"self_approval\":\"$self\",\"empty_assignee\":\"$empty\",
       \"join_mode\":\"AND\",\"fork_mode\":\"AND\",\"reject_veto\":true},
      {\"key\":\"e\",\"name\":\"结束\",\"type\":\"END\",\"x\":440,\"y\":0,\"sort_order\":3}
    ],
    \"edges\":[
      {\"key\":\"e_start\",\"from\":\"s\",\"to\":\"n_approve\"},
      {\"key\":\"e_end\",\"from\":\"n_approve\",\"to\":\"e\"}
    ]}")
  [ "$rc" = "200" ] || { say "  [curl] save graph $code → http=$rc $(body)" >&2; return 1; }
  rc=$(http POST "$BASE/api/business/flow/def/$def/publish" "$A" "")
  [ "$rc" = "200" ] || { say "  [curl] publish $code → http=$rc $(body)" >&2; return 1; }
  printf '%s' "$def"
}
F_ESC=$(mk_flow p302_esc "P3-02 升级链（自审跳过）" P302_ESC "$MAKER" SKIP ERROR)
F_CHAIN=$(mk_flow p302_chain "P3-02 升级链（二级上溯）" P302_CHAIN "$DEAD" SKIP ERROR)
F_DEADEND=$(mk_flow p302_deadend "P3-02 升级链（到顶无人）" P302_DEADEND "$DEAD2" SKIP ERROR)
F_THREE=$(mk_flow p302_three "P3-02 场景A 三人角色" P302_THREE "$THREE" SKIP ERROR)
F_FORBID=$(mk_flow p302_forbid "P3-02 场景F 禁止自审" P302_FORBID "$MAKER" FORBID ERROR)
[ -n "$F_ESC" ] && is_uuid "$F_ESC" && is_uuid "$F_FORBID" && ok "5 张流程发布成功" \
  || { bad "流程夹具失败（esc=$F_ESC / forbid=$F_FORBID）"; summary; }

start_instance_of() { # start_instance_of TOKEN FLOW_CODE TITLE → 打印 instance id
  local tok=$1 code=$2 title=$3 rc id
  rc=$(http POST "$BASE/api/business/flow/instance" "$tok" \
    "{\"flow_code\":\"$code\",\"title\":\"$title\",\"form_data\":{}}")
  if [ "$rc" != "200" ]; then say "  [curl] start $code → http=$rc $(body)" >&2; return 1; fi
  id=$(body | jget data.instance_id)
  printf '%s' "$id"
}

MAKERTOK=$(login p302_maker P302@2026 P302new@2026) || { bad "制单人登录失败"; summary; }
BOSS1TOK=$(login p302_boss1 P302@2026 P302new@2026) || { bad "主管甲登录失败"; summary; }
T1TOK=$(login p302_t1 P302@2026 P302new@2026) || { bad "组员甲登录失败"; summary; }
D1TOK=$(login p302_d1 P302@2026 P302new@2026) || { bad "死链提交人登录失败"; summary; }
D2TOK=$(login p302_d2 P302@2026 P302new@2026) || { bad "死链提交人2 登录失败"; summary; }
ok "5 个业务令牌就绪"

say ""
say "=== 场景 B（P3-02 主验收）：角色只有制单人 1 人 → 提交 → 自动升级到上级角色 → 审批 → 生效 ==="
I_B=$(start_instance_of "$MAKERTOK" p302_esc "P3-02 场景B 自审跳过升级") || { bad "场景 B 发起失败"; }
is_uuid "$I_B" || { bad "场景 B 没拿到实例 id：$I_B"; summary; }
say "instance=$I_B"
eq "场景 B 实例状态" "$(q "$DB" "select status from biz_flow_instance where id='$I_B'::uuid")" "PENDING"
eq "场景 B 待办行数（= 升级后上级角色的 2 人）" "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_B'::uuid and status='PENDING'")" "2"
eq "场景 B PENDING 待办的 escalated_from_role" "$(q "$DB" "select distinct escalated_from_role from biz_flow_task where instance_id='$I_B'::uuid and status='PENDING'")" "P302_MAKER"
eq "场景 B PENDING 待办的 escalate_reason" "$(q "$DB" "select distinct escalate_reason from biz_flow_task where instance_id='$I_B'::uuid and status='PENDING'")" "SKIP_SELF_APPROVAL"
eq "场景 B 待办审批人 = P302_BOSS 的两个人" "$(q "$DB" "select count(*) from biz_flow_task t join sys_user_role ur on ur.user_id=t.assignee_id where t.instance_id='$I_B'::uuid and t.status='PENDING' and ur.role_id='$BOSS'::uuid")" "2"
eq "场景 B 原 SKIPPED 留痕行仍在（制单人）" "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_B'::uuid and status='SKIPPED' and assignee_id='$U_MAKER'::uuid")" "1"
SKIPPED_COMMENT=$(q "$DB" "select comment from biz_flow_task where instance_id='$I_B'::uuid and status='SKIPPED' limit 1")
contains "SKIPPED 行原因" "$SKIPPED_COMMENT" "self_approval=SKIP"
BOSS_TASK=$(q "$DB" "select id from biz_flow_task where instance_id='$I_B'::uuid and status='PENDING' order by assignee_name limit 1")
RC=$(http POST "$BASE/api/business/flow/task/$BOSS_TASK/approve" "$BOSS1TOK" '{"comment":"P3-02 上级角色审批通过"}')
eq "上级角色审批 → 200" "$RC" "200"
eq "场景 B 单据（实例）生效" "$(q "$DB" "select status from biz_flow_instance where id='$I_B'::uuid")" "APPROVED"
eq "场景 B 时间线上仍能看到升级留痕行" "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_B'::uuid and escalated_from_role='P302_MAKER'")" "2"
say "  [留痕] $(q "$DB" "select assignee_name || ' ' || status || ' escalate=' || coalesce(escalate_reason,'-') from biz_flow_task where instance_id='$I_B'::uuid order by status" | tr '\n' '|')"
say "  [API] 时间线 escalate_reason / escalated_to_role："
http GET "$BASE/api/business/flow/instance/$I_B" "$BOSS1TOK" '' >/dev/null
say "    $(body | python3 -c 'import sys,json;d=json.load(sys.stdin)["data"]["timeline"];print([ (t["assignee_name"],t["status"],t.get("escalated_from_role"),t.get("escalate_reason"),t.get("escalated_to_role")) for t in d])')"

say ""
say "=== 升级链二级上溯（卡面单测 ② 的真库版）：第一级 0 人 → 第二级 0 人 → 第三级命中 ==="
I_C=$(start_instance_of "$D1TOK" p302_chain "P3-02 二级上溯") || { bad "二级上溯发起失败"; }
eq "二级上溯：待办 2 行（P302_BOSS）" "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_C'::uuid and status='PENDING'")" "2"
eq "二级上溯：escalate_reason = NO_MEMBER（角色 0 人触发）" "$(q "$DB" "select distinct escalate_reason from biz_flow_task where instance_id='$I_C'::uuid and status='PENDING'")" "NO_MEMBER"
eq "二级上溯：escalated_from_role = P302_DEAD" "$(q "$DB" "select distinct escalated_from_role from biz_flow_task where instance_id='$I_C'::uuid and status='PENDING'")" "P302_DEAD"
say "  [trace] 见实例日志：$(grep -oE "升级链第 [0-9]+ 级：[^,]*" "$LOG" | tail -3 | tr '\n' '|')"

say ""
say "=== 到顶仍无人（卡面单测 ④）：422 + 报文含链路 + 结构化诊断带每一级 ==="
RC=$(http POST "$BASE/api/business/flow/instance" "$D2TOK" \
  '{"flow_code":"p302_deadend","title":"P3-02 死链","form_data":{}}')
eq "死链发起 → 422" "$RC" "422"
eq "死链 → 错误码 65018" "$(body | jget code)" "65018"
MSG=$(body | jget msg)
contains "报文写明升级链已走完" "$MSG" "升级链已走完"
contains "报文带第一级角色名" "$MSG" "P3-02 空角色（死链终点）"
say "  报文：$MSG"
say "  结构化诊断 escalation_chain：$(body | python3 -c 'import sys,json;d=json.load(sys.stdin).get("data") or {};print([(c["role_code"],c["member_count"],c["usable_count"]) for c in d.get("escalation_chain") or []])')"
eq "诊断里链路有 1 级（P302_EMPTY2）" "$(body | python3 -c 'import sys,json;d=json.load(sys.stdin).get("data") or {};print(len(d.get("escalation_chain") or []))')" "1"
eq "死链没留下半截实例" "$(q "$DB" "select count(*) from biz_flow_instance where title='P3-02 死链'")" "0"

say ""
say "=== 场景 A 回归：角色 3 人，制单人 1 人 → 其余 2 人审批（不走升级链） ==="
I_A=$(start_instance_of "$T1TOK" p302_three "P3-02 场景A 三人角色") || { bad "场景 A 发起失败"; }
eq "场景 A 待办 2 行（另两人）" "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_A'::uuid and status='PENDING'")" "2"
eq "场景 A 无人被升级（escalated_from_role 全空）" "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_A'::uuid and escalated_from_role is not null")" "0"
eq "场景 A 制单人落 SKIPPED 留痕" "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_A'::uuid and status='SKIPPED'")" "1"

say ""
say "=== 场景 F 回归：FORBID → 65019（不降级、不换人） ==="
RC=$(http POST "$BASE/api/business/flow/instance" "$MAKERTOK" \
  '{"flow_code":"p302_forbid","title":"P3-02 场景F FORBID","form_data":{}}')
eq "场景 F 发起 → 422" "$RC" "422"
eq "场景 F → 错误码 65019" "$(body | jget code)" "65019"
contains "场景 F 报文" "$(body | jget msg)" "self_approval=FORBID"
eq "场景 F 没留下半截实例" "$(q "$DB" "select count(*) from biz_flow_instance where title='P3-02 场景F FORBID'")" "0"

say ""
say "=== 升级链配置非法的运行时表现（65040 / 65041 的代码路径）==="
say "  65040（环）：API 写入口会拦，但历史脏数据 / 直接改库仍要在运行期报错"
say "  造环：P302_DEAD2 → P302_EMPTY2 → P302_DEAD2（两级都是 0 人，才会走到环检测那一跳）"
q "$DB" "UPDATE sys_role SET escalation_role_id='$DEAD2'::uuid WHERE id='$EMPTY2'::uuid" >/dev/null
RC=$(http POST "$BASE/api/business/flow/instance" "$D2TOK" \
  '{"flow_code":"p302_deadend","title":"P3-02 环检测","form_data":{}}')
eq "环 → 422" "$RC" "422"
eq "环 → 错误码 65040" "$(body | jget code)" "65040"
contains "环 → 报文" "$(body | jget msg)" "成环"
say "  报文：$(body | jget msg)"
# 复原链条（EMPTY2 恢复为链尾）
q "$DB" "UPDATE sys_role SET escalation_role_id=NULL WHERE id='$EMPTY2'::uuid" >/dev/null

say ""
say "=== 65041：升级链超过 8 级（构造 1+9 级全 0 人的链，第 9 级仍要上溯 → 配置非法）==="
PREV=""
CH_FIRST=""
for i in 1 2 3 4 5 6 7 8 9; do
  id=$(mk_role "P302_CH$i" "P3-02 链$i")
  is_uuid "$id" || { bad "第 $i 级链角色创建失败：$id"; summary; }
  if [ -z "$CH_FIRST" ]; then CH_FIRST=$id; fi
  if [ -n "$PREV" ]; then
    set_esc "$PREV" "P3-02 链$((i - 1))" "P302_CH$((i - 1))" "$id" >/dev/null
  fi
  PREV=$id
done
DEEP=$(mk_role P302_DEEP "P3-02 超深链起点")
set_esc "$DEEP" "P3-02 超深链起点" P302_DEEP "$CH_FIRST" >/dev/null
eq "超深链起点 → 第 1 级" "$(q "$DB" "select escalation_role_id from sys_role where id='$DEEP'::uuid")" "$CH_FIRST"
F_DEEP=$(mk_flow p302_deep "P3-02 超深链" P302_DEEP "$DEEP" SKIP ERROR)
DEEPTOK=$(login p302_d1 P302@2026 P302new@2026) || { bad "超深链提交人登录失败"; summary; }
RC=$(http POST "$BASE/api/business/flow/instance" "$DEEPTOK" \
  '{"flow_code":"p302_deep","title":"P3-02 超深链","form_data":{}}')
eq "超深链 → 422" "$RC" "422"
eq "超深链 → 错误码 65041" "$(body | jget code)" "65041"
contains "超深链 → 报文" "$(body | jget msg)" "超过 8 级"

stop_instance "$PIDF"
summary
