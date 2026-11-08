#!/usr/bin/env bash
# ============================================================================
# P3-03 验收 ②③：真 HTTP 场景 C（兜底人接管）+ 兜底角色 + 自审兜底拒绝 + 65042
#                 + 场景 A / B / F 回归
#
# 全部走**真 HTTP**（隔离实例 + 独立空库），结论另用 psql 断言落库事实。
#
# 夹具（全部由本脚本用 API 建，不用种子数据）：
#   角色  P303_MAKER(1 人 = 制单人)  **没有上级**          ← 场景 C 的前置（升级链走完仍无人）
#         P303_FB(3 人)                                      ← 兜底角色 / 兜底人来源
#         P303_THREE(3 人)                                   ← 场景 A 回归
#         P303_BOSS(2 人)，P303_MAKER --escalation--> BOSS    ← 场景 B 回归
#         P303_SUBMIT(只提交不审批)
#   流程  p303_fb_user   ROLE=MAKER + fallback_user=fb1        ← 场景 C 主验收
#         p303_fb_role   ROLE=MAKER + fallback_role=FB         ← 单测 ③ 的真库版
#         p303_fb_self   ROLE=MAKER + fallback_user=制单人      ← 单测 ② 的真库版
#         p303_fb_bad    ROLE=MAKER + fallback_user=不存在的 id ← 65042
#         p303_nofb      ROLE=MAKER + 无兜底                    ← 单测 ④ 的真库版
#         p303_esc       ROLE=MAKER（有上级）                    ← 场景 B 回归
#         p303_three     ROLE=THREE                             ← 场景 A 回归
#         p303_forbid    ROLE=MAKER + self=FORBID                ← 场景 F 回归
#   权限  测试角色按 auth_code 直接授 3 个码（夹具构造，等价于权限页勾选）。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/21_lib_p303.sh"

DB=${DB:-cloudpivot_p304_regression}
PORT=${PORT:-18305}
BASE="http://127.0.0.1:$PORT"
ENVF=/root/p304/e2e.env        # 环境文件含密钥，**不能**落在仓库里
LOG=/root/p304/21_regression_instance.log
PIDF="$RAW/regression.pid"
OUT="$RAW/21_regression_p303.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-04 回归：P3-03 场景 A/B/C/F（真 HTTP）（$BASE / $DB）================"

stop_instance "$PIDF"
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun $DB" || { bad "建空库失败"; summary; }
make_env "$DB" "$PORT" "$ENVF" || { bad "抄环境失败"; summary; }

say "--- 0) 空库首启：全部迁移一次装起（含 V0334）"
start_instance "$ENVF" "$LOG" "$PIDF" "$PORT" || { bad "实例没起来"; tail -20 "$LOG"; summary; }
wait_http "$PORT" 240 || { bad "实例 240s 内没起来"; tail -30 "$LOG"; summary; }
MIG=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
say "首启迁移日志：${MIG:-<未找到>}"
eq "schema_migrations 里 V0334 已登记" "$(q "$DB" "select count(*) from schema_migrations where version='334' and kind='versioned'")" "1"

ADMINPW=$(awk -F= '/^NIK_ADMIN_PASSWORD/{print $2}' "$ENVF")
ADMINPW=${ADMINPW:-Admin@123456}
ADMINNEW="P303Admin@2026"

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
ok "admin 令牌就绪（含首登强制改密流程）"

say ""
say "--- 1) 建角色"
mk_role() { # mk_role CODE NAME
  local code=$1 name=$2 rc
  rc=$(http POST "$BASE/api/system/role" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"description\":\"P3-03 夹具\",\"status\":1,\"sort_order\":920,\"escalation_role_id\":\"\"}")
  [ "$rc" = "200" ] || { say "  [curl] create role $code → http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}
MAKER=$(mk_role P303_MAKER "P3-03 销售员（只有制单人）")
FB=$(mk_role P303_FB "P3-03 兜底组")
THREE=$(mk_role P303_THREE "P3-03 三人组")
BOSS=$(mk_role P303_BOSS "P3-03 销售主管")
CHAIN=$(mk_role P303_CHAIN "P3-03 升级链起点（只有制单人）")
SUBMIT=$(mk_role P303_SUBMIT "P3-03 提交人（只提交不审批）")
[ -n "$MAKER" ] && is_uuid "$MAKER" && is_uuid "$FB" && is_uuid "$BOSS" && is_uuid "$SUBMIT" \
  && ok "6 个角色建成" || { bad "角色创建失败（MAKER=$MAKER / FB=$FB）"; summary; }

set_esc() { # set_esc ROLE_ID NAME ROLE_CODE PARENT_ID → 打印状态码
  local id=$1 name=$2 code=$3 parent=$4
  http PUT "$BASE/api/system/role/$id" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"description\":\"P3-03 夹具\",\"status\":1,\"sort_order\":920,\"escalation_role_id\":\"$parent\"}"
}
# 场景 B 回归专用：P303_CHAIN（只有制单人）→ 上级 P303_BOSS
# **不能**把 P303_MAKER 也挂上上级：场景 C 的前提就是「升级链走完仍无人」（MAKER 无上级）
eq "P303_CHAIN 的上级角色设为 P303_BOSS（场景 B 用）" \
  "$(set_esc "$CHAIN" "P3-03 升级链起点（只有制单人）" P303_CHAIN "$BOSS")" "200"

say ""
say "--- 2) 授权（夹具：按 auth_code 直接授，等价于权限页勾选）"
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
eq "P303_MAKER 持有 3 个码" "$(grant "$MAKER" "${SUB_ALL[@]}")" "3"
eq "P303_FB 持有 3 个码" "$(grant "$FB" "${SUB_ALL[@]}")" "3"
eq "P303_THREE 持有 3 个码" "$(grant "$THREE" "${SUB_ALL[@]}")" "3"
eq "P303_BOSS 持有 2 个码" "$(grant "$BOSS" business:flow:approve business:flow:instance:read)" "2"
eq "P303_CHAIN 持有 3 个码" "$(grant "$CHAIN" "${SUB_ALL[@]}")" "3"
eq "P303_SUBMIT 持有 2 个码" "$(grant "$SUBMIT" business:flow:instance:create business:flow:instance:read)" "2"

say ""
say "--- 3) 建用户（角色成员数就是判据；P303_MAKER 只有制单人 1 个）"
mk_user() { # mk_user USERNAME REAL_NAME ROLE_ID
  local uname=$1 real=$2 role=$3 rc
  rc=$(http POST "$BASE/api/system/user" "$A" \
    "{\"username\":\"$uname\",\"password\":\"P303@2026\",\"real_name\":\"$real\",\"status\":1,\"role_ids\":[\"$role\"]}")
  [ "$rc" = "200" ] || { say "  [curl] create user $uname → http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}
U_MAKER=$(mk_user p303_maker "P3-03 制单人" "$MAKER")
U_FB1=$(mk_user p303_fb1 "P3-03 兜底甲" "$FB")
U_FB2=$(mk_user p303_fb2 "P3-03 兜底乙" "$FB")
U_FB3=$(mk_user p303_fb3 "P3-03 兜底丙" "$FB")
U_T1=$(mk_user p303_t1 "P3-03 组员甲" "$THREE")
U_T2=$(mk_user p303_t2 "P3-03 组员乙" "$THREE")
U_T3=$(mk_user p303_t3 "P3-03 组员丙" "$THREE")
U_B1=$(mk_user p303_b1 "P3-03 主管甲" "$BOSS")
U_B2=$(mk_user p303_b2 "P3-03 主管乙" "$BOSS")
U_D1=$(mk_user p303_d1 "P3-03 提交人" "$SUBMIT")
[ -n "$U_MAKER" ] && is_uuid "$U_MAKER" && is_uuid "$U_FB3" && is_uuid "$U_D1" \
  && ok "10 个用户建成" || { bad "建用户失败（maker=$U_MAKER / fb3=$U_FB3）"; summary; }
# 制单人同时也是「升级链起点角色」的唯一成员（场景 B 要的是「角色里只有他 1 人 + 有上级」）
q "$DB" "INSERT INTO sys_user_role (user_id, role_id) SELECT '$U_MAKER'::uuid, '$CHAIN'::uuid
          WHERE NOT EXISTS (SELECT 1 FROM sys_user_role WHERE user_id='$U_MAKER'::uuid AND role_id='$CHAIN'::uuid)" >/dev/null
eq "P303_CHAIN 成员数（= 1，场景 B 的前提：角色里只有制单人）" \
  "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$CHAIN'::uuid and u.deleted_at is null")" "1"
eq "P303_MAKER 成员数（= 1，场景 C 的前提）" \
  "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$MAKER'::uuid and u.deleted_at is null")" "1"
eq "P303_FB 成员数（= 3）" \
  "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$FB'::uuid and u.deleted_at is null")" "3"
eq "P303_MAKER 没有上级角色（升级链到此为止 → 场景 C 成立）" \
  "$(q "$DB" "select coalesce(escalation_role_id::text,'NULL') from sys_role where id='$MAKER'::uuid")" "NULL"

say ""
say "--- 4) 建流程（START → APPROVAL(ROLE) → END；APPROVAL 用 ANY 或签）"
# mk_flow CODE NAME BIZ ROLE_ID SELF EMPTY [EXTRA_JSON]
mk_flow() {
  local code=$1 name=$2 biz=$3 role=$4 self=$5 empty=$6 extra=${7:-} rc def vid graph
  rc=$(http POST "$BASE/api/business/flow/def" "$A" \
    "{\"flow_code\":\"$code\",\"flow_name\":\"$name\",\"biz_type\":\"$biz\",\"is_default\":true}")
  [ "$rc" = "200" ] || { say "  [curl] create def $code → http=$rc $(body)" >&2; return 1; }
  def=$(body | jget data.id); vid=$(body | jget data.version_id)
  # `extra` 只能拼在**节点对象**里（拼进 change_note 的字符串字面量会把 JSON 撕开）
  graph=$(cat <<JSON
{
  "change_note": "P3-03 夹具：单审批节点（$self / $empty）",
  "nodes": [
    {"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
    {"key":"n_approve","name":"审批（P3-03）","type":"APPROVAL","x":220,"y":0,"sort_order":2,
     "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$role","sort_order":0}],
     "approval_mode":"ANY","self_approval":"$self","empty_assignee":"$empty",
     "join_mode":"AND","fork_mode":"AND","reject_veto":true${extra:+,$extra}},
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

F_FBUSER=$(mk_flow p303_fb_user "P3-03 场景C 兜底人" P303_FBUSER "$MAKER" SKIP ERROR "\"fallback_user_id\":\"$U_FB1\"")
F_FBROLE=$(mk_flow p303_fb_role "P3-03 兜底角色" P303_FBROLE "$MAKER" SKIP ERROR "\"fallback_role_id\":\"$FB\"")
F_FBSELF=$(mk_flow p303_fb_self "P3-03 兜底=发起人" P303_FBSELF "$MAKER" SKIP ERROR "\"fallback_user_id\":\"$U_MAKER\"")
F_FBBAD=$(mk_flow p303_fb_bad "P3-03 兜底人配错" P303_FBBAD "$MAKER" SKIP ERROR "\"fallback_user_id\":\"11111111-2222-3333-4444-555555555555\"")
F_NOFB=$(mk_flow p303_nofb "P3-03 未配兜底" P303_NOFB "$MAKER" SKIP ERROR)
F_ESC=$(mk_flow p303_esc "P3-03 场景B 升级链" P303_ESC "$CHAIN" SKIP ERROR)
F_THREE=$(mk_flow p303_three "P3-03 场景A 三人角色" P303_THREE "$THREE" SKIP ERROR)
F_FORBID=$(mk_flow p303_forbid "P3-03 场景F FORBID" P303_FORBID "$MAKER" FORBID ERROR)
for f in "$F_FBUSER" "$F_FBROLE" "$F_FBSELF" "$F_FBBAD" "$F_NOFB" "$F_ESC" "$F_THREE" "$F_FORBID"; do
  is_uuid "$f" || { bad "流程夹具失败（$f）"; summary; }
done
ok "8 张流程发布成功"

say ""
say "--- 4b) 发布期 R22 与保存期二选一（两个都配 → 400，不是库 CHECK 的英文报错）"
RC=$(http POST "$BASE/api/business/flow/def" "$A" '{"flow_code":"p303_dual","flow_name":"P3-03 双配","biz_type":"P303_DUAL"}')
VID=$(body | jget data.version_id)
DUAL_GRAPH=$(cat <<JSON
{"nodes":[
   {"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
   {"key":"n_approve","name":"审批","type":"APPROVAL","x":220,"y":0,"sort_order":2,
    "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$MAKER","sort_order":0}],
    "approval_mode":"ANY","self_approval":"SKIP","empty_assignee":"ERROR",
    "fallback_user_id":"$U_FB1","fallback_role_id":"$FB"},
   {"key":"e","name":"结束","type":"END","x":440,"y":0,"sort_order":3}],
 "edges":[{"key":"e1","from":"s","to":"n_approve"},{"key":"e2","from":"n_approve","to":"e"}]}
JSON
)
RC=$(http PUT "$BASE/api/business/flow/version/$VID/graph" "$A" "$DUAL_GRAPH")
eq "两个兜底目标同时配 → 400" "$RC" "400"
contains "保存期报文说清只能二选一" "$(body | jget msg)" "只能二选一"

say ""
say "--- 4c) 发布期 R22：兜底人 == 指定审批人（USER 来源）→ 发布被拒（65042）"
RC=$(http POST "$BASE/api/business/flow/def" "$A" '{"flow_code":"p303_r22","flow_name":"P3-03 R22","biz_type":"P303_R22"}')
R22DEF=$(body | jget data.id); R22VID=$(body | jget data.version_id)
R22_GRAPH=$(cat <<JSON
{"nodes":[
   {"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
   {"key":"n_approve","name":"审批","type":"APPROVAL","x":220,"y":0,"sort_order":2,
    "assignee_type":"USER","assignees":[{"type":"USER","ref_id":"$U_FB1","sort_order":0}],
    "approval_mode":"ANY","self_approval":"SKIP","empty_assignee":"ERROR",
    "fallback_user_id":"$U_FB1"},
   {"key":"e","name":"结束","type":"END","x":440,"y":0,"sort_order":3}],
 "edges":[{"key":"e1","from":"s","to":"n_approve"},{"key":"e2","from":"n_approve","to":"e"}]}
JSON
)
RC=$(http PUT "$BASE/api/business/flow/version/$R22VID/graph" "$A" "$R22_GRAPH")
eq "R22 反例图能存成草稿（编辑期不拦完整性）" "$RC" "200"
RC=$(http POST "$BASE/api/business/flow/def/$R22DEF/publish" "$A" "")
eq "发布期 R22 → 400（发布校验统一 HTTP 400，业务码区分）" "$RC" "400"
eq "R22 → 错误码 65042" "$(body | jget code)" "65042"
contains "R22 → 报文说清同一个人" "$(body | jget msg)" "同一个"

say ""
say "--- 5) 业务令牌"
MAKERTOK=$(login p303_maker P303@2026 P303new@2026) || { bad "制单人登录失败"; summary; }
FB1TOK=$(login p303_fb1 P303@2026 P303new@2026) || { bad "兜底甲登录失败"; summary; }
FB2TOK=$(login p303_fb2 P303@2026 P303new@2026) || { bad "兜底乙登录失败"; summary; }
T1TOK=$(login p303_t1 P303@2026 P303new@2026) || { bad "组员甲登录失败"; summary; }
D1TOK=$(login p303_d1 P303@2026 P303new@2026) || { bad "提交人登录失败"; summary; }
ok "5 个业务令牌就绪"

start_instance_of() { # start_instance_of TOKEN FLOW_CODE TITLE
  local tok=$1 code=$2 title=$3 rc id
  rc=$(http POST "$BASE/api/business/flow/instance" "$tok" \
    "{\"flow_code\":\"$code\",\"title\":\"$title\",\"form_data\":{}}")
  if [ "$rc" != "200" ]; then say "  [curl] start $code → http=$rc $(body)" >&2; return 1; fi
  id=$(body | jget data.instance_id)
  printf '%s' "$id"
}

say ""
say "=== 场景 C（本卡主验收）：角色 1 人（=制单人）+ 无上级 + 节点配兜底人 ==="
say "    提交 → 兜底人收到待办（assigned_by_rule=FALLBACK）→ 审批通过"
I_C=$(start_instance_of "$MAKERTOK" p303_fb_user "P3-03 场景C 兜底人接管") || bad "场景 C 发起失败"
is_uuid "$I_C" || { bad "场景 C 没拿到实例 id：$I_C"; summary; }
say "instance=$I_C"
eq "场景 C 实例状态" "$(q "$DB" "select status from biz_flow_instance where id='$I_C'::uuid")" "PENDING"
eq "场景 C PENDING 待办行数（兜底人 1 人）" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_C'::uuid and status='PENDING'")" "1"
eq "场景 C 待办审批人 = 兜底甲" \
  "$(q "$DB" "select assignee_id from biz_flow_task where instance_id='$I_C'::uuid and status='PENDING'")" "$U_FB1"
eq "场景 C assigned_by_rule = FALLBACK" \
  "$(q "$DB" "select distinct assigned_by_rule from biz_flow_task where instance_id='$I_C'::uuid and status='PENDING'")" "FALLBACK"
eq "场景 C 兜底行不带升级留痕（escalated_from_role 应为空）" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_C'::uuid and escalated_from_role is not null")" "0"
eq "场景 C 原 SKIPPED 留痕行仍在（制单人）" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_C'::uuid and status='SKIPPED' and assignee_id='$U_MAKER'::uuid")" "1"
C_TASK=$(q "$DB" "select id from biz_flow_task where instance_id='$I_C'::uuid and status='PENDING'")
RC=$(http POST "$BASE/api/business/flow/task/$C_TASK/approve" "$FB1TOK" '{"comment":"P3-03 兜底人审批通过"}')
eq "兜底人审批 → 200" "$RC" "200"
eq "场景 C 单据（实例）生效" "$(q "$DB" "select status from biz_flow_instance where id='$I_C'::uuid")" "APPROVED"
say "  [留痕] $(q "$DB" "select assignee_name || ' ' || status || ' rule=' || coalesce(assigned_by_rule,'-') from biz_flow_task where instance_id='$I_C'::uuid order by status" | tr '\n' '|')"
say "  [API] 时间线（含 assigned_by_rule）："
http GET "$BASE/api/business/flow/instance/$I_C" "$FB1TOK" '' >/dev/null
say "    $(body | python3 -c 'import sys,json;d=json.load(sys.stdin)["data"]["timeline"];print([(t["assignee_name"],t["status"],t.get("assigned_by_rule")) for t in d])')"

say ""
say "=== 单测 ③ 真库版：兜底角色 3 人 → 3 行待办（ALL/ANY 由 P3-06 决定，本卡按现行为全落行）==="
I_R=$(start_instance_of "$MAKERTOK" p303_fb_role "P3-03 兜底角色三人") || bad "兜底角色发起失败"
eq "兜底角色 → PENDING 3 行" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_R'::uuid and status='PENDING'")" "3"
eq "兜底角色 → 3 行全是 FALLBACK" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_R'::uuid and status='PENDING' and assigned_by_rule='FALLBACK'")" "3"
eq "兜底角色 → 3 行审批人都属于 P303_FB" \
  "$(q "$DB" "select count(*) from biz_flow_task t join sys_user_role ur on ur.user_id=t.assignee_id where t.instance_id='$I_R'::uuid and t.status='PENDING' and ur.role_id='$FB'::uuid")" "3"
say "  [留痕] $(q "$DB" "select assignee_name || ' ' || status || ' rule=' || coalesce(assigned_by_rule,'-') from biz_flow_task where instance_id='$I_R'::uuid and status='PENDING' order by assignee_name" | tr '\n' '|')"
R_TASK=$(q "$DB" "select id from biz_flow_task where instance_id='$I_R'::uuid and status='PENDING' order by assignee_name limit 1")
http POST "$BASE/api/business/flow/task/$R_TASK/approve" "$FB1TOK" '{"comment":"P3-03 兜底角色第一人通过"}' >/dev/null
eq "兜底角色批 1 人后仍 PENDING（APPROVAL 节点按现行为 = 全员签，ALL/ANY 留给 P3-06）" \
  "$(q "$DB" "select status from biz_flow_instance where id='$I_R'::uuid")" "PENDING"
# 把剩下的都批掉 → 到终态（证明 3 行待办都是有效待办，不是死行）
for tid in $(q "$DB" "select id from biz_flow_task where instance_id='$I_R'::uuid and status='PENDING'"); do
  http POST "$BASE/api/business/flow/task/$tid/approve" "$FB1TOK" '{"comment":"P3-03 兜底角色全员通过"}' >/dev/null
done
eq "兜底角色全部批完 → 实例生效" \
  "$(q "$DB" "select status from biz_flow_instance where id='$I_R'::uuid")" "APPROVED"

say ""
say "=== 单测 ② 真库版：兜底人 = 发起人 → 422，报文「兜底人即发起人，内控不允许自审」==="
RC=$(http POST "$BASE/api/business/flow/instance" "$MAKERTOK" \
  '{"flow_code":"p303_fb_self","title":"P3-03 兜底=发起人","form_data":{}}')
eq "兜底人即发起人 → 422" "$RC" "422"
eq "→ 错误码 65018" "$(body | jget code)" "65018"
SELF_MSG=$(body | jget msg)
contains "报文点名兜底人即发起人" "$SELF_MSG" "兜底人即发起人"
contains "报文点名内控不允许自审" "$SELF_MSG" "内控不允许自审"
say "  报文：$SELF_MSG"
eq "自审兜底没留下半截实例" "$(q "$DB" "select count(*) from biz_flow_instance where title='P3-03 兜底=发起人'")" "0"

say ""
say "=== 单测 ④ 真库版：未配兜底 → 仍 422/65018，报文含「升级链与兜底人均未配置」==="
RC=$(http POST "$BASE/api/business/flow/instance" "$MAKERTOK" \
  '{"flow_code":"p303_nofb","title":"P3-03 未配兜底","form_data":{}}')
eq "未配兜底 → 422" "$RC" "422"
eq "→ 错误码 65018" "$(body | jget code)" "65018"
NOFB_MSG=$(body | jget msg)
contains "报文含「升级链与兜底人均未配置」" "$NOFB_MSG" "升级链与兜底人均未配置"
say "  报文：$NOFB_MSG"
eq "结构化诊断仍带 data（前端据此弹指派引导）" \
  "$(body | python3 -c 'import sys,json;d=json.load(sys.stdin).get("data");print("yes" if d else "no")')" "yes"

say ""
say "=== 65042 真库版：兜底人配置无效（用户不存在）→ 422/65042 ==="
RC=$(http POST "$BASE/api/business/flow/instance" "$MAKERTOK" \
  '{"flow_code":"p303_fb_bad","title":"P3-03 兜底人配错","form_data":{}}')
eq "兜底人不存在 → 422" "$RC" "422"
eq "→ 错误码 65042" "$(body | jget code)" "65042"
contains "报文说是配置无效" "$(body | jget msg)" "不存在"
say "  报文：$(body | jget msg)"

say ""
say "=== 回归 场景 A：角色 3 人，制单人 1 人 → 其余 2 人审批（不走升级链、不走兜底）==="
I_A=$(start_instance_of "$T1TOK" p303_three "P3-03 场景A 三人角色") || bad "场景 A 发起失败"
eq "场景 A 待办 2 行（另两人）" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_A'::uuid and status='PENDING'")" "2"
eq "场景 A assigned_by_rule 全是 ROLE（普通角色展开）" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_A'::uuid and status='PENDING' and assigned_by_rule='ROLE'")" "2"
eq "场景 A 无人被升级" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_A'::uuid and escalated_from_role is not null")" "0"
eq "场景 A 制单人落 SKIPPED 留痕" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_A'::uuid and status='SKIPPED'")" "1"

say ""
say "=== 回归 场景 B：升级链（制单人 1 人 → 上级 P303_BOSS 2 人）—— 兜底不抢升级链的活 ==="
I_B=$(start_instance_of "$MAKERTOK" p303_esc "P3-03 场景B 升级链") || bad "场景 B 发起失败"
eq "场景 B 待办 2 行（上级角色）" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_B'::uuid and status='PENDING'")" "2"
eq "场景 B assigned_by_rule = ESCALATED（升级优先于兜底）" \
  "$(q "$DB" "select distinct assigned_by_rule from biz_flow_task where instance_id='$I_B'::uuid and status='PENDING'")" "ESCALATED"
eq "场景 B escalated_from_role = P303_CHAIN（原角色编码）" \
  "$(q "$DB" "select distinct escalated_from_role from biz_flow_task where instance_id='$I_B'::uuid and status='PENDING'")" "P303_CHAIN"
eq "场景 B escalate_reason = SKIP_SELF_APPROVAL" \
  "$(q "$DB" "select distinct escalate_reason from biz_flow_task where instance_id='$I_B'::uuid and status='PENDING'")" "SKIP_SELF_APPROVAL"
eq "场景 B 待办审批人 = P303_BOSS 的两个人" \
  "$(q "$DB" "select count(*) from biz_flow_task t join sys_user_role ur on ur.user_id=t.assignee_id where t.instance_id='$I_B'::uuid and t.status='PENDING' and ur.role_id='$BOSS'::uuid")" "2"

say ""
say "=== 回归 场景 F：FORBID → 65019（不降级、不换人、不走兜底）==="
RC=$(http POST "$BASE/api/business/flow/instance" "$MAKERTOK" \
  '{"flow_code":"p303_forbid","title":"P3-03 场景F FORBID","form_data":{}}')
eq "场景 F 发起 → 422" "$RC" "422"
eq "场景 F → 错误码 65019" "$(body | jget code)" "65019"
contains "场景 F 报文" "$(body | jget msg)" "self_approval=FORBID"
eq "场景 F 没留下半截实例" "$(q "$DB" "select count(*) from biz_flow_instance where title='P3-03 场景F FORBID'")" "0"

stop_instance "$PIDF"

# 实例日志：只留「与兜底 / 升级链 / 迁移 / R22 有关」的 INFO/WARN 行（原始日志大且含 SQL 参数，不入库）
grep -E "INFO|WARN" "$LOG" \
  | grep -E "应用 [0-9]+ 个迁移|V0334|V0333|R22|兜底|升级链|审批人解析完成" \
  >"$RAW/21_regression_instance_trace.txt"
say "实例日志摘录 → raw/21_regression_instance_trace.txt（$(wc -l <"$RAW/21_regression_instance_trace.txt") 行）"

summary
