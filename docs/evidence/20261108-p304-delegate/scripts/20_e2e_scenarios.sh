#!/usr/bin/env bash
# ============================================================================
# P3-04 验收 ②：ARCH-8 **场景 E**（出差 / 休假时审批委托）+ 边界全走真 HTTP
#
# 主链路（卡面验收原文）：
#   审批人 A 设「今天 → +3 天委托给 B」→ 新发起一张单 → B 收到待办且显示「代 A 审批」
#   → B 审批后流程推进 → 委托期内 A 自己的待办 0 条 → 委托期结束后新单回到 A。
#
# 边界：
#   · 区间重叠 → 422 / 65043（换个人也不行，同一时间只能委托给一个人）
#   · 自委托 → 422 / 65043；区间填反 → 422 / 65043
#   · `scope` 限定 biz_type：命中的单据生效、别的单据不生效
#   · 受托人 = 发起人 → 内控剔掉（受托人 0 条待办，委托人也不接）→ 422 / 65018
#   · 行级权限：非管理员只能看 / 改自己的委托（看全部 → 403）
#
# 夹具全部由本脚本用真 HTTP 建（不用种子数据）：
#   角色  P304_APPR（成员只有 A）  ← 审批人；A 同时是委托人
#         P304_SUB（成员只有 P）   ← 发起人
#   用户  A(p304_a) / B(p304_b) / P(p304_p) / D(p304_d，第二受托人用于重叠负例)
#   流程  p304_e    ROLE=P304_APPR，biz_type=P304_E     ← 主链路
#         p304_scope ROLE=P304_APPR，biz_type=P304_SCOPE ← scope 对照
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

DB=${DB:-cloudpivot_p304_e2e}
PORT=${PORT:-18304}
BASE="http://127.0.0.1:$PORT"
ENVF=/root/p304/e2e.env         # 含密钥，**不落仓库**
LOG=/root/p304/20_e2e_instance.log
PIDF="$RAW/e2e.pid"
OUT="$RAW/20_e2e_scenarios.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-04 ② 真 HTTP 验收 · 场景 E（$BASE / $DB）================"

stop_instance "$PIDF"
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun $DB" || { bad "建空库失败"; summary; }
make_env "$DB" "$PORT" "$ENVF" || { bad "抄环境失败"; summary; }

say ""
say "--- 0) 空库首启（含 V0336）"
start_instance "$ENVF" "$LOG" "$PIDF" "$PORT" || { bad "实例没起来"; tail -20 "$LOG"; summary; }
wait_http "$PORT" 300 || { bad "实例 300s 内没起来"; tail -30 "$LOG"; summary; }
eq "schema_migrations 里 V0336 已登记" \
  "$(q "$DB" "select count(*) from schema_migrations where version='336'")" "1"
eq "委托表可用" \
  "$(q "$DB" "select count(*) from information_schema.tables where table_name='biz_flow_delegate'")" "1"

ADMINPW=$(awk -F= '/^NIK_ADMIN_PASSWORD/{print $2}' "$ENVF")
ADMINPW=${ADMINPW:-Admin@123456}
ADMINNEW="P304Admin@2026"

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
ok "admin 令牌就绪"

say ""
say "--- 1) 夹具：2 个角色 + 4 个用户 + 2 个流程"
mk_role() { # mk_role CODE NAME
  local code=$1 name=$2 rc
  rc=$(http POST "$BASE/api/system/role" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"description\":\"P3-04 夹具\",\"status\":1,\"sort_order\":930,\"escalation_role_id\":\"\"}")
  [ "$rc" = "200" ] || { say "  [curl] create role $code → http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}
R_APPR=$(mk_role P304_APPR "P3-04 审批人（只有 A）")
R_SUB=$(mk_role P304_SUB "P3-04 发起人（只有 P）")
R_OTHER=$(mk_role P304_OTHER "P3-04 受托人组（不属于审批节点角色）")
[ -n "$R_APPR" ] && is_uuid "$R_APPR" && is_uuid "$R_SUB" && is_uuid "$R_OTHER" && ok "3 个角色建成" || { bad "角色创建失败"; summary; }

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
APPR_CODES=(business:flow:instance:create business:flow:instance:read business:flow:approve \
            business:flow:delegate:list business:flow:delegate:save business:flow:delegate:delete)
eq "P304_APPR 持有 6 个码（含 3 个委托码）" "$(grant "$R_APPR" "${APPR_CODES[@]}")" "6"
eq "P304_OTHER 持有 5 个码（受托人要能审批 + 能碰委托接口，但不是节点角色）" \
  "$(grant "$R_OTHER" business:flow:instance:read business:flow:approve business:flow:delegate:list business:flow:delegate:save business:flow:delegate:delete)" "5"
eq "P304_SUB 持有 2 个码" \
  "$(grant "$R_SUB" business:flow:instance:create business:flow:instance:read)" "2"

mk_user() { # mk_user USERNAME REAL_NAME ROLE_ID
  local uname=$1 real=$2 role=$3 rc
  rc=$(http POST "$BASE/api/system/user" "$A" \
    "{\"username\":\"$uname\",\"password\":\"P304@2026\",\"real_name\":\"$real\",\"status\":1,\"role_ids\":[\"$role\"]}")
  [ "$rc" = "200" ] || { say "  [curl] create user $uname → http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}
U_A=$(mk_user p304_a "P3-04 审批人甲" "$R_APPR")
U_B=$(mk_user p304_b "P3-04 受托人乙" "$R_OTHER")
U_D=$(mk_user p304_d "P3-04 第二个受托人" "$R_OTHER")
U_P=$(mk_user p304_p "P3-04 发起人" "$R_SUB")
[ -n "$U_A" ] && is_uuid "$U_A" && is_uuid "$U_P" && is_uuid "$U_B" && is_uuid "$U_D" && ok "4 个用户建成" || { bad "建用户失败"; summary; }
eq "审批节点角色的成员数 = 1（只有 A：委托才有意义）" \
  "$(q "$DB" "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id='$R_APPR'::uuid and u.deleted_at is null")" "1"

mk_flow() { # mk_flow CODE NAME BIZ ROLE_ID
  local code=$1 name=$2 biz=$3 role=$4 rc def vid graph
  rc=$(http POST "$BASE/api/business/flow/def" "$A" \
    "{\"flow_code\":\"$code\",\"flow_name\":\"$name\",\"biz_type\":\"$biz\",\"is_default\":true}")
  [ "$rc" = "200" ] || { say "  [curl] create def $code → http=$rc $(body)" >&2; return 1; }
  def=$(body | jget data.id); vid=$(body | jget data.version_id)
  graph=$(cat <<JSON
{
  "change_note": "P3-04 夹具：单审批节点（ROLE=$role）",
  "nodes": [
    {"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
    {"key":"n_approve","name":"审批（P3-04）","type":"APPROVAL","x":220,"y":0,"sort_order":2,
     "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$role","sort_order":0}],
     "approval_mode":"ANY","self_approval":"SKIP","empty_assignee":"ERROR",
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
F_E=$(mk_flow p304_e "P3-04 场景E 委托" P304_E "$R_APPR")
F_SCOPE=$(mk_flow p304_scope "P3-04 scope 对照" P304_SCOPE "$R_APPR")
is_uuid "$F_E" && is_uuid "$F_SCOPE" && ok "2 张流程发布成功" || { bad "流程夹具失败"; summary; }

say ""
say "--- 2) 业务令牌"
TOK_A=$(login p304_a P304@2026 P304newA@2026) || { bad "A 登录失败"; summary; }
TOK_B=$(login p304_b P304@2026 P304newB@2026) || { bad "B 登录失败"; summary; }
TOK_P=$(login p304_p P304@2026 P304newP@2026) || { bad "P 登录失败"; summary; }
ok "3 个业务令牌就绪（A 审批人 / B 受托人 / P 发起人）"

start_instance_of() { # start_instance_of TOKEN FLOW_CODE TITLE → 打印 instance_id
  local tok=$1 code=$2 title=$3 rc
  rc=$(http POST "$BASE/api/business/flow/instance" "$tok" \
    "{\"flow_code\":\"$code\",\"title\":\"$title\",\"form_data\":{}}")
  if [ "$rc" != "200" ]; then say "  [curl] start $code → http=$rc $(body)" >&2; return 1; fi
  body | jget data.instance_id
}

now_plus() { date -u -d "+$1 hour" +%Y-%m-%dT%H:%M:%S+00:00; }

say ""
say "=== 场景 E 主链路：A 委托给 B（今天 → +3 天）→ 新单派给 B ==="
START_AT=$(now_plus -1)
END_AT=$(now_plus 72)
RC=$(http POST "$BASE/api/business/flow/delegate" "$TOK_A" \
  "{\"delegate_id\":\"$U_B\",\"start_at\":\"$START_AT\",\"end_at\":\"$END_AT\",\"reason\":\"出差北京\"}")
eq "A 自己配委托（默认委托人 = 自己）→ 200" "$RC" "200"
DEL_ID=$(body | jget data.id)
is_uuid "$DEL_ID" || { bad "没拿到委托 id：$(body)"; summary; }
eq "委托落到 biz_flow_delegate（启用）" \
  "$(q "$DB" "select delegator_id::text||'|'||delegate_id::text||'|'||enabled from biz_flow_delegate where id='$DEL_ID'::uuid")" \
  "$U_A|$U_B|true"
eq "列表回执 is_active_now = 生效中" \
  "$(printf '%s' "$(body | jget data.is_active_now)" | tr 'A-Z' 'a-z')" "true"

say "  [委托行] $(q "$DB" "select delegator_id::text || ' → ' || delegate_id::text || ' 至 ' || to_char(end_at at time zone 'Asia/Shanghai','YYYY-MM-DD HH24:MI') from biz_flow_delegate where id='$DEL_ID'::uuid")"

I_E1=$(start_instance_of "$TOK_P" p304_e "[P3-04] 委托期内第一单") || bad "发起失败"
is_uuid "$I_E1" || { bad "没拿到实例 id：$I_E1"; summary; }
say "instance=$I_E1（发起人 = P3-04 发起人）"
eq "待办行数 = 1（B 一行，A 没有）" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_E1'::uuid and status='PENDING'")" "1"
eq "待办审批人 = 受托人 B" \
  "$(q "$DB" "select assignee_id from biz_flow_task where instance_id='$I_E1'::uuid and status='PENDING'")" "$U_B"
eq "assigned_by_rule = DELEGATED" \
  "$(q "$DB" "select distinct assigned_by_rule from biz_flow_task where instance_id='$I_E1'::uuid and status='PENDING'")" "DELEGATED"
eq "assignee_source 保留原来源 ROLE（审计口径）" \
  "$(q "$DB" "select distinct assignee_source from biz_flow_task where instance_id='$I_E1'::uuid and status='PENDING'")" "ROLE"
contains "comment 标「代 A 审批」" \
  "$(q "$DB" "select comment from biz_flow_task where instance_id='$I_E1'::uuid and status='PENDING'")" \
  "代 P3-04 审批人甲 审批（委托至"
eq "委托期内 A 自己的待办 = 0" \
  "$(q "$DB" "select count(*) from biz_flow_task where assignee_id='$U_A'::uuid and status='PENDING'")" "0"

say ""
say "--- B 的待办列表（前端读它显示「代 A 审批」）"
RC=$(http GET "$BASE/api/business/flow/task/todo?page=1&page_size=20" "$TOK_B" '')
eq "B 拉待办 → 200" "$RC" "200"
eq "B 的待办里有这条单" \
  "$(body | python3 -c 'import sys,json;d=json.load(sys.stdin)["data"]["list"];print(sum(1 for t in d if t["instance_id"]=="'"$I_E1"'"))')" "1"
say "  [B 的卡片字段] $(body | python3 -c 'import sys,json;d=json.load(sys.stdin)["data"]["list"];print([(t["title"],t.get("assigned_by_rule"),t.get("comment")) for t in d if t["instance_id"]=="'"$I_E1"'"])')"
eq "卡片带 assigned_by_rule=DELEGATED" \
  "$(body | python3 -c 'import sys,json;d=json.load(sys.stdin)["data"]["list"];print([t.get("assigned_by_rule") for t in d if t["instance_id"]=="'"$I_E1"'"][0])')" \
  "DELEGATED"
contains "卡片 comment 就是「代 A 审批（委托至 …）」" \
  "$(body | python3 -c 'import sys,json;d=json.load(sys.stdin)["data"]["list"];print([t.get("comment") for t in d if t["instance_id"]=="'"$I_E1"'"][0])')" \
  "代 P3-04 审批人甲 审批"
RC=$(http GET "$BASE/api/business/flow/task/todo?page=1&page_size=20" "$TOK_A" '')
eq "同期 A 的待办列表为空（0 条）" \
  "$(body | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["total"])')" "0"

say ""
say "--- B 审批 → 流程推进"
T_E1=$(q "$DB" "select id from biz_flow_task where instance_id='$I_E1'::uuid and status='PENDING'")
RC=$(http POST "$BASE/api/business/flow/task/$T_E1/approve" "$TOK_B" '{"comment":"P3-04 受托人代 A 审批通过"}')
eq "受托人审批 → 200" "$RC" "200"
eq "实例生效（APPROVED）" "$(q "$DB" "select status from biz_flow_instance where id='$I_E1'::uuid")" "APPROVED"
eq "审批留痕记在受托人名下" \
  "$(q "$DB" "select assignee_name||'|'||status||'|'||coalesce(assigned_by_rule,'-') from biz_flow_task where id='$T_E1'::uuid")" \
  "P3-04 受托人乙|APPROVED|DELEGATED"

say ""
say "--- 委托期结束后新单回到 A（改窗口到过去 = 委托已过期）"
RC=$(http PUT "$BASE/api/business/flow/delegate/$DEL_ID" "$TOK_A" \
  "{\"delegate_id\":\"$U_B\",\"start_at\":\"$(now_plus -30)\",\"end_at\":\"$(now_plus -3)\",\"reason\":\"出差北京\",\"enabled\":true}")
eq "把委托窗口改到过去 → 200" "$RC" "200"
I_E2=$(start_instance_of "$TOK_P" p304_e "[P3-04] 委托过期后的一单") || bad "发起失败"
eq "过期后待办回到 A（委托人）" \
  "$(q "$DB" "select assignee_id from biz_flow_task where instance_id='$I_E2'::uuid and status='PENDING'")" "$U_A"
eq "过期后 assigned_by_rule = ROLE（不是 DELEGATED）" \
  "$(q "$DB" "select distinct assigned_by_rule from biz_flow_task where instance_id='$I_E2'::uuid and status='PENDING'")" "ROLE"
eq "过期后不带「代 … 审批」comment" \
  "$(q "$DB" "select count(*) from biz_flow_task where instance_id='$I_E2'::uuid and status='PENDING' and comment like '代 %审批%'")" "0"

say ""
say "--- 停用开关：窗口未到期但 enabled=false → 不生效"
RC=$(http PUT "$BASE/api/business/flow/delegate/$DEL_ID" "$TOK_A" \
  "{\"delegate_id\":\"$U_B\",\"start_at\":\"$START_AT\",\"end_at\":\"$END_AT\",\"enabled\":false}")
eq "停用委托 → 200" "$RC" "200"
I_E3=$(start_instance_of "$TOK_P" p304_e "[P3-04] 委托停用后的一单") || bad "发起失败"
eq "停用后待办回到 A" \
  "$(q "$DB" "select assignee_id from biz_flow_task where instance_id='$I_E3'::uuid and status='PENDING'")" "$U_A"

say ""
say "--- 恢复启用（后面继续用这一档做 scope / 内控用例）"
RC=$(http PUT "$BASE/api/business/flow/delegate/$DEL_ID" "$TOK_A" \
  "{\"delegate_id\":\"$U_B\",\"start_at\":\"$START_AT\",\"end_at\":\"$END_AT\",\"enabled\":true}")
eq "重新启用 → 200" "$RC" "200"

say ""
say "=== 保存期边界 ①：区间重叠 → 422 / 65043 ==="
RC=$(http POST "$BASE/api/business/flow/delegate" "$TOK_A" \
  "{\"delegate_id\":\"$U_D\",\"start_at\":\"$(now_plus -1)\",\"end_at\":\"$(now_plus 24)\",\"reason\":\"同一时间委托两个人\"}")
eq "同委托人重叠区间 → 422" "$RC" "422"
eq "→ 错误码 65043" "$(body | jget code)" "65043"
contains "报文说清「重叠」并点名冲突区间" "$(body | jget msg)" "重叠"
say "  报文：$(body | jget msg)"
eq "重叠的那条没落库（A 名下仍 1 档）" \
  "$(q "$DB" "select count(*) from biz_flow_delegate where delegator_id='$U_A'::uuid and deleted_at is null")" "1"

say ""
say "=== 保存期边界 ②：自委托 / 区间填反 → 422 / 65043 ==="
RC=$(http POST "$BASE/api/business/flow/delegate" "$TOK_A" \
  "{\"delegate_id\":\"$(printf '%s' "$U_A")\",\"start_at\":\"$(now_plus -1)\",\"end_at\":\"$(now_plus 24)\"}")
eq "自委托 → 422" "$RC" "422"
eq "→ 错误码 65043" "$(body | jget code)" "65043"
contains "报文说清不能委托给自己" "$(body | jget msg)" "不能是委托人自己"
RC=$(http POST "$BASE/api/business/flow/delegate" "$TOK_A" \
  "{\"delegate_id\":\"$U_D\",\"start_at\":\"$(now_plus 24)\",\"end_at\":\"$(now_plus 1)\"}")
eq "区间填反 → 422" "$RC" "422"
eq "→ 错误码 65043" "$(body | jget code)" "65043"

say ""
say "=== scope：限定 biz_type 时别的单据不生效 ==="
RC=$(http DELETE "$BASE/api/business/flow/delegate/$DEL_ID" "$TOK_A" "")
eq "先删掉上面那档（腾出区间）→ 200" "$RC" "200"
RC=$(http POST "$BASE/api/business/flow/delegate" "$TOK_A" \
  "{\"delegate_id\":\"$U_B\",\"start_at\":\"$(now_plus -1)\",\"end_at\":\"$END_AT\",\"scope\":\"P304_SCOPE\",\"reason\":\"只接 scope 业务\"}")
eq "建 scope=P304_SCOPE 的委托 → 200" "$RC" "200"
DEL_SCOPE=$(body | jget data.id)
eq "scope 落库为大写" "$(q "$DB" "select scope from biz_flow_delegate where id='$DEL_SCOPE'::uuid")" "P304_SCOPE"
I_S1=$(start_instance_of "$TOK_P" p304_e "[P3-04] scope 不匹配的一单") || bad "发起失败"
eq "P304_E 单据 → scope 不含它，仍派给 A" \
  "$(q "$DB" "select assignee_id from biz_flow_task where instance_id='$I_S1'::uuid and status='PENDING'")" "$U_A"
I_S2=$(start_instance_of "$TOK_P" p304_scope "[P3-04] scope 匹配的一单") || bad "发起失败"
eq "P304_SCOPE 单据 → 派给 B" \
  "$(q "$DB" "select assignee_id from biz_flow_task where instance_id='$I_S2'::uuid and status='PENDING'")" "$U_B"

say ""
say "=== 内控：受托人 = 发起人 → 不许代签，委托人也不接 → 422 / 65018 ==="
RC=$(http DELETE "$BASE/api/business/flow/delegate/$DEL_SCOPE" "$TOK_A" "")
eq "删掉 scope 那档 → 200" "$RC" "200"
RC=$(http POST "$BASE/api/business/flow/delegate" "$TOK_A" \
  "{\"delegate_id\":\"$U_P\",\"start_at\":\"$(now_plus -1)\",\"end_at\":\"$END_AT\",\"reason\":\"委托给发起人（应当被拦）\"}")
eq "委托给发起人本人在保存期是允许的（归属校验只看人是否在职）" "$RC" "200"
DEL_SELF=$(body | jget data.id)
RC=$(http POST "$BASE/api/business/flow/instance" "$TOK_P" \
  '{"flow_code":"p304_e","title":"[P3-04] 受托人即发起人","form_data":{}}')
eq "发起 → 422（受托人被内控剔掉、委托人也不再接，且没配兜底）" "$RC" "422"
eq "→ 错误码 65018" "$(body | jget code)" "65018"
SELF_MSG=$(body | jget msg)
contains "报文点名「没有可用审批人」" "$SELF_MSG" "没有"
say "  报文：$SELF_MSG"
eq "这次没留下实例（整单回滚）" \
  "$(q "$DB" "select count(*) from biz_flow_instance where title='[P3-04] 受托人即发起人'")" "0"
eq "发起人 P 名下没有待办（内控不许自审）" \
  "$(q "$DB" "select count(*) from biz_flow_task where assignee_id='$U_P'::uuid and status='PENDING'")" "0"

say ""
say "=== 行级权限：非管理员只能看 / 改自己的委托 ==="
RC=$(http GET "$BASE/api/business/flow/delegate/page?view=all" "$TOK_A" '')
eq "A 看全部委托 → 403（需要 business:flow:admin）" "$RC" "403"
eq "→ 错误码 65004" "$(body | jget code)" "65004"
RC=$(http GET "$BASE/api/business/flow/delegate/page" "$TOK_A" '')
eq "A 看自己的委托 → 200" "$RC" "200"
eq "A 自己名下的委托条数 = 1（委托给 P 的那档）" \
  "$(body | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["total"])')" "1"
RC=$(http DELETE "$BASE/api/business/flow/delegate/$DEL_SELF" "$TOK_B" "")
eq "B 删 A 的委托 → 403" "$RC" "403"
eq "→ 错误码 65004" "$(body | jget code)" "65004"
RC=$(http GET "$BASE/api/business/flow/delegate/page?delegator_id=$U_A" "$TOK_B" '')
eq "B 看 A 的委托 → 403" "$RC" "403"
RC=$(http PUT "$BASE/api/business/flow/delegate/$DEL_SELF" "$TOK_A" \
  "{\"delegate_id\":\"$U_B\",\"start_at\":\"$(now_plus -1)\",\"end_at\":\"$END_AT\",\"enabled\":false}")
eq "A 停用自己的那档 → 200" "$RC" "200"
eq "停用后 enabled = false（行还在）" \
  "$(q "$DB" "select enabled from biz_flow_delegate where id='$DEL_SELF'::uuid")" "f"

say ""
say "=== 删除后查不到（软删留痕）==="
RC=$(http DELETE "$BASE/api/business/flow/delegate/$DEL_SELF" "$TOK_A" "")
eq "A 删自己的委托 → 200" "$RC" "200"
eq "软删：deleted_at 非空" \
  "$(q "$DB" "select (deleted_at is not null) from biz_flow_delegate where id='$DEL_SELF'::uuid")" "t"
eq "列表里不再出现" \
  "$(http GET "$BASE/api/business/flow/delegate/page" "$TOK_A" '' >/dev/null; body | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["total"])')" "0"
RC=$(http DELETE "$BASE/api/business/flow/delegate/$DEL_SELF" "$TOK_A" "")
eq "重复删除 → 404" "$RC" "404"

say ""
say "--- 留痕汇总（DB）：本卡造出来的所有委托行"
say "  $(q "$DB" "select delegator_id::text||'→'||delegate_id::text||' scope='||coalesce(scope,'ALL')||' enabled='||enabled||' deleted='||(deleted_at is not null) from biz_flow_delegate order by created_at" | tr '\n' '|')"
say "--- 待办留痕（DB）：委托派下来的行"
say "  $(q "$DB" "select instance_id||' '||assignee_name||' '||status||' rule='||coalesce(assigned_by_rule,'-')||' comment='||coalesce(comment,'-') from biz_flow_task where assigned_by_rule='DELEGATED'" | tr '\n' '|')"

stop_instance "$PIDF"
say ""
say "（隔离实例已停；库 $DB 保留给 21 回归脚本的对照）"
summary
