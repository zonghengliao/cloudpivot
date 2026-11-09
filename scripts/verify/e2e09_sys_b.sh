#!/usr/bin/env bash
# E2E-09 B：数据权限（⑤）/ 字段脱敏（⑥）/ 角色互斥（⑦）/ 变更日志（⑧）
# 依赖 e2e09_sys_a.sh 的夹具（部门 D1/D1A/D2、角色 VIEWER/BOSS、用户 auditor/other）
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e09_common.sh"

say "================ E2E-09 B：数据权限 / 字段脱敏 / 角色互斥 / 变更日志 ================"
for v in D1 D1A D2 VIEWER BOSS U_AUD U_OTH; do
  eval "cur=\${$v:-}"
  [ -n "$cur" ] || { say "FATAL: 缺夹具 $v（先跑 e2e09_sys_a.sh）"; exit 1; }
done
T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD") || { bad "admin 登录失败"; exit 1; }
save T_ADMIN "$T_ADMIN"
ok "admin 令牌就绪（沿用 A 卡夹具 D1=$D1 D1A=$D1A）"

# ============ 夹具：补充角色与用户 ============
say ""
say "--- 0) 补夹具：数据权限操作员 / 收权对象 / 互斥角色与本卡用户 ---"
mk_role() { # mk_role CODE NAME SCOPE → id
  api "role_$1" POST /system/role "$T_ADMIN" "{\"role_code\":\"$1\",\"role_name\":\"$2\",\"status\":1,\"sort_order\":950,\"data_scope\":$3,\"escalation_role_id\":\"\"}"
  data '.data.id'
}
SCOPEOP=$(mk_role E2E09_SCOPEOP "演练权限操作员" 2)
VICTIM=$(mk_role E2E09_VICTIM "演练收权对象" 1)
MA=$(mk_role E2E09_MUTEX_A "演练互斥A" 1)
MB=$(mk_role E2E09_MUTEX_B "演练互斥B" 1)
MC=$(mk_role E2E09_MUTEX_C "演练互斥C" 1)
MD=$(mk_role E2E09_MUTEX_D "演练互斥D" 1)
for n in SCOPEOP VICTIM MA MB MC MD; do eval "id=\$$n"; save "R_$n" "$id"; done
check "6 个补充角色建成（uuid 均合法）" "$(printf '%s %s %s %s %s %s' "$SCOPEOP" "$VICTIM" "$MA" "$MB" "$MC" "$MD" | grep -oE '[0-9a-f]{8}-[0-9a-f]{4}' | wc -l | tr -d ' ')" 6

mk_user() { # mk_user USERNAME REAL DEPT ROLE_JSON → id
  api "user_$1" POST /system/user "$T_ADMIN" "{\"username\":\"$1\",\"password\":\"E2e09@Pass1\",\"real_name\":\"$2\",\"dept_id\":\"$3\",\"status\":1,\"role_ids\":$4}"
  data '.data.id'
}
U_TV=$(mk_user e2e09_taskview "演练任务可见性" "$D1A" "[\"$VIEWER\"]")
U_PEER=$(mk_user e2e09_peer "演练同组同僚" "$D1A" "[\"$VIEWER\"]")
U_OP=$(mk_user e2e09_scopeop "演练权限操作员账号" "$D1A" "[\"$SCOPEOP\"]")
U_MU=$(mk_user e2e09_mutex_u1 "演练互斥用户" "$D1" "[\"$MA\"]")
U_MU2=$(mk_user e2e09_mutex_u2 "演练互斥存量用户" "$D1" "[\"$MC\",\"$MD\"]")
for n in TV PEER OP MU MU2; do eval "id=\$U_$n"; save "U_$n" "$id"; done
check "5 个补充用户建成" "$(printf '%s %s %s %s %s' "$U_TV" "$U_PEER" "$U_OP" "$U_MU" "$U_MU2" | grep -oE '[0-9a-f]{8}-' | wc -l | tr -d ' ')" 5
check "u2 已同时持有 C/D（存量双持，供 61010 用）" "$(SQ "select count(*) from sys_user_role where user_id='$U_MU2'::uuid")" 2

grant() { # grant ROLE_ID CODE... → 授权后覆盖的**权限码**个数
  local role=$1; shift
  local ids="" c
  for c in "$@"; do
    local one; one=$(menu_ids_by_code "$T_ADMIN" "$c")
    [ -n "$one" ] || { say "  ⚠ 菜单树无 $c"; continue; }
    ids="$ids $one"
  done
  local json; json=$(printf '%s\n' $ids | jq -R . | jq -s -c 'unique')
  api "grant_$role" PUT "/system/role/$role/menus" "$T_ADMIN" "{\"menu_ids\":$json}"
  [ "$HTTP" = 200 ] || say "  ⚠ 授权失败 $role http=$HTTP $(show)"
  SQ "select count(distinct m.auth_code) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where rm.role_id='$role'::uuid"
}
check "VIEWER 授权 4 个码（user:list / task:read / scope:read / self-check）" \
  "$(grant "$VIEWER" system:user:list business:task:read business:permission:scope:read business:permission:scope:self-check)" 4
check "SCOPEOP 授权 3 个码（scope:write / scope:read / self-check）" \
  "$(grant "$SCOPEOP" business:permission:scope:write business:permission:scope:read business:permission:scope:self-check)" 3

# ============ ⑤ 数据权限 ============
say ""
say "--- ⑤ 数据权限：范围规则 / 真实数据过滤（本部门 / 仅本人 / 自定义 / 全部）---"
# 任务夹具（admin 建，负责人决定 biz_task.dept_id 快照）
mk_task() { # mk_task TITLE OWNER → id
  api "task_$1" POST /business/task "$T_ADMIN" "{\"title\":\"$1\",\"owner_id\":\"$2\",\"priority\":\"P2\"}"
  data '.data.id'
}
T1=$(mk_task "E2E09-T1 A组本人" "$U_TV")
T2=$(mk_task "E2E09-T2 一部他人" "$U_OTH")
T3=$(mk_task "E2E09-T3 A组同僚" "$U_PEER")
save T1 "$T1"; save T2 "$T2"; save T3 "$T3"
check "T1 落库 dept_id = D1A（负责人部门快照）" "$(SQ "select dept_id from biz_task where id='$T1'::uuid")" "$D1A"
check "T2 落库 dept_id = D1" "$(SQ "select dept_id from biz_task where id='$T2'::uuid")" "$D1"
check "T3 落库 dept_id = D1A" "$(SQ "select dept_id from biz_task where id='$T3'::uuid")" "$D1A"

T_TV=$(login e2e09_taskview 'E2e09@Pass1') || bad "taskview 登录失败"
save T_TV "$T_TV"
titles() { api "tv_$1" GET "/business/task/page?page=1&page_size=50&keyword=E2E09-T" "$T_TV"; \
  printf '%s' "$BODY" | jq -r '[.data.list[]? | select(.title|startswith("E2E09-T")) | .title] | sort | join(",")'; }

api scope_get GET "/business/permission/scope/role/$VIEWER" "$T_ADMIN"
check "角色数据范围详情 HTTP" "$HTTP" 200
check "VIEWER 初始 data_scope = 2（本部门）" "$(data '.data.data_scope')" 2
check "data_scope_code = DEPT" "$(data '.data.data_scope_code')" "DEPT"
check "holder_count = 3（auditor/taskview/peer）" "$(data '.data.holder_count')" 3

api scope_bad6 PUT "/business/permission/scope/role/$VIEWER" "$T_ADMIN" '{"data_scope":6}'
check "data_scope=6 → HTTP" "$HTTP" 400
check "data_scope=6 → 61001" "$CODE" 61001
api scope_empty5 PUT "/business/permission/scope/role/$VIEWER" "$T_ADMIN" '{"data_scope":5,"custom_dept_ids":[]}'
check "自定义范围但集合为空 → HTTP" "$HTTP" 400
check "自定义范围但集合为空 → 61001" "$CODE" 61001
api scope_baddept PUT "/business/permission/scope/role/$VIEWER" "$T_ADMIN" '{"data_scope":5,"custom_dept_ids":["00000000-0000-4000-8000-00000000dead"]}'
check "自定义集合含不存在部门 → HTTP" "$HTTP" 404
check "自定义集合含不存在部门 → 61004" "$CODE" 61004
api scope_nonempty2 PUT "/business/permission/scope/role/$VIEWER" "$T_ADMIN" "{\"data_scope\":2,\"custom_dept_ids\":[\"$D1\"]}"
check "范围≠5 却提交非空集合 → HTTP" "$HTTP" 400
check "范围≠5 却提交非空集合 → 61001" "$CODE" 61001

api scope_set2 PUT "/business/permission/scope/role/$VIEWER" "$T_ADMIN" '{"data_scope":2}'
check "设为本部门(2) → HTTP" "$HTTP" 200
check "本部门范围：只看到 A 组两条（T1/T3），看不到 T2" "$(titles set2)" "E2E09-T1 A组本人,E2E09-T3 A组同僚"
api scope_set4 PUT "/business/permission/scope/role/$VIEWER" "$T_ADMIN" '{"data_scope":4}'
check "设为仅本人(4) → HTTP" "$HTTP" 200
check "仅本人范围：只剩自己负责的 T1" "$(titles set4)" "E2E09-T1 A组本人"
api scope_set5 PUT "/business/permission/scope/role/$VIEWER" "$T_ADMIN" "{\"data_scope\":5,\"custom_dept_ids\":[\"$D1\"]}"
check "设为自定义部门集合(5)={演练一部} → HTTP" "$HTTP" 200
check "回读自定义部门明细 = 演练一部" "$(data '.data.custom_depts[0].dept_name')" "演练一部"
check "自定义范围：看到一部下的 T2 + 自己参与的 T1（参与关系 OR，不含 T3）" "$(titles set5)" "E2E09-T1 A组本人,E2E09-T2 一部他人"
api scope_set1 PUT "/business/permission/scope/role/$VIEWER" "$T_ADMIN" '{"data_scope":1}'
check "设为全部(1) → HTTP" "$HTTP" 200
check "全部范围：三条都可见" "$(titles set1)" "E2E09-T1 A组本人,E2E09-T2 一部他人,E2E09-T3 A组同僚"
api scope_back2 PUT "/business/permission/scope/role/$VIEWER" "$T_ADMIN" '{"data_scope":2}'
check "回滚为本部门(2) → HTTP" "$HTTP" 200
check "回滚勾稽：又只剩 A 组两条" "$(titles back2)" "E2E09-T1 A组本人,E2E09-T3 A组同僚"
check "DB sys_role.data_scope = 2" "$(SQ "select data_scope from sys_role where id='$VIEWER'::uuid")" 2

api scope_depttree GET '/business/permission/scope/dept-tree' "$T_TV"
check "数据权限部门树 HTTP" "$HTTP" 200
check "按操作者范围裁剪 scope_limited = true" "$(data '.data.scope_limited')" true
check "可见部门只有 A 组 1 个" "$(data '.data.total')" 1
check "visible_dept_ids = [D1A]" "$(data '.data.visible_dept_ids[0]')" "$D1A"

api scope_selfcheck GET '/business/permission/scope/self-check' "$T_TV"
check "数据权限自检 HTTP" "$HTTP" 200
check "自检 scope.code = DEPT" "$(data '.data.scope.code')" "DEPT"
check "自检 roles 含 E2E09_VIEWER" "$(printf '%s' "$BODY" | jq -r '[.data.roles[].role_code] | index("E2E09_VIEWER") != null')" true
check "自检注入片段非空（sys_user 表）" "$(printf '%s' "$BODY" | jq -r '[.data.tables[] | select(.table=="sys_user")][0].is_noop')" false
matches "sys_user 注入片段含 dept_id" "$(printf '%s' "$BODY" | jq -r '[.data.tables[] | select(.table=="sys_user")][0].where_sql')" 'dept_id'

# 防自我扩权（61003 / 61002）
T_OP=$(login e2e09_scopeop 'E2e09@Pass1') || bad "scopeop 登录失败"
save T_OP "$T_OP"
api scope_self PUT "/business/permission/scope/role/$SCOPEOP" "$T_OP" '{"data_scope":1}'
check "改自己持有角色的数据范围 → HTTP" "$HTTP" 403
check "改自己持有角色的数据范围 → 61003" "$CODE" 61003
api scope_exceed PUT "/business/permission/scope/role/$VICTIM" "$T_OP" '{"data_scope":1}'
check "新范围超出操作者自身范围(2→1) → HTTP" "$HTTP" 403
check "超出自身范围 → 61002" "$CODE" 61002
contains "61002 报文说明越界" "$(data '.msg')" "超出你自身的可见范围"
api scope_inrange PUT "/business/permission/scope/role/$VICTIM" "$T_OP" '{"data_scope":2}'
check "同范围内改（2→2）→ HTTP" "$HTTP" 200

# ============ ⑥ 字段脱敏 ============
say ""
say "--- ⑥ 字段脱敏：规则 CRUD / 试运行 / 生效清单 / 真实接口出参打码 ---"
BASELINE_MOBILE=$(SQ "select count(*) from sys_field_mask where deleted_at is null and table_name='sys_user' and column_name='mobile' and role_id is null")
check "基线已有全局 mobile 规则（seed）" "$(printf '%s' "$BASELINE_MOBILE" | awk '{print ($1>=1)?"1":"0"}')" 1

# 角色规则用「更严的参数」覆盖全局规则（pick_effective：同列角色规则优先），
# 这样「规则是否生效」有可观测的差异：全局 3,4 → 138****9001；角色 2,2 → 13****01
api mask_create POST /system/field-mask "$T_ADMIN" "{\"table_name\":\"sys_user\",\"column_name\":\"mobile\",\"mask_type\":1,\"mask_param\":\"2,2\",\"role_id\":\"$VIEWER\",\"status\":1,\"reason\":\"E2E09 演练：手机号按角色脱敏\"}"
check "建脱敏规则（sys_user.mobile / 手机号 2,2 / 角色 VIEWER）→ HTTP" "$HTTP" 200
MASK=$(data '.data.id'); save MASK "$MASK"
check "规则 scope = ROLE" "$(data '.data.scope')" "ROLE"
check "规则 role_name = 演练查看员（实时取名）" "$(data '.data.role_name')" "演练查看员"
check "参数回显 2,2" "$(data '.data.mask_param')" "2,2"
check "mask_type_code = mobile" "$(data '.data.mask_type_code')" "mobile"

api mask_dup POST /system/field-mask "$T_ADMIN" "{\"table_name\":\"sys_user\",\"column_name\":\"mobile\",\"mask_type\":1,\"mask_param\":\"2,2\",\"role_id\":\"$VIEWER\",\"status\":1,\"reason\":\"E2E09 重复规则\"}"
check "同(表,列,角色)重复建 → HTTP" "$HTTP" 409
api mask_badtarget POST /system/field-mask "$T_ADMIN" '{"table_name":"sys_user","column_name":"no_such_col","mask_type":1,"status":1,"reason":"E2E09 目标不存在"}'
check "目标列不存在 → HTTP" "$HTTP" 404
check "目标列不存在 → 61006" "$CODE" 61006
api mask_badtype POST /system/field-mask "$T_ADMIN" '{"table_name":"sys_user","column_name":"email","mask_type":9,"status":1,"reason":"E2E09 类型非法"}'
check "mask_type=9 → HTTP" "$HTTP" 400
check "mask_type=9 → 61005" "$CODE" 61005

api mask_prev_mobile GET '/system/field-mask/preview?mask_type=1&value=13800009001' "$T_ADMIN"
check "手机号试运行 HTTP" "$HTTP" 200
check "手机号脱敏结果（默认 3,4）" "$(data '.data.output')" "138****9001"
check "手机号 changed = true" "$(data '.data.changed')" true
api mask_prev_idcard GET '/system/field-mask/preview?mask_type=2&value=110101199001011234' "$T_ADMIN"
check "身份证试运行输出" "$(data '.data.output')" "1101****1234"
api mask_prev_email GET '/system/field-mask/preview?mask_type=3&value=auditor@e2e09.local' "$T_ADMIN"
check "邮箱试运行输出" "$(data '.data.output')" "a***@e2e09.local"
api mask_prev_param GET '/system/field-mask/preview?mask_type=1&mask_param=2,2&value=13800009001' "$T_ADMIN"
check "自定义参数 2,2 试运行输出" "$(data '.data.output')" "13****01"

api mask_eff_tv GET "/system/field-mask/effective?user_id=$U_TV" "$T_ADMIN"
check "生效清单（taskview）HTTP" "$HTTP" 200
check "taskview 未豁免" "$(data '.data.exempt')" false
check "taskview 生效规则含本卡规则" "$(printf '%s' "$BODY" | jq -r --arg id "$MASK" '[.data.rules[].rule_id] | index($id) != null')" true
api mask_eff_admin GET "/system/field-mask/effective?user_id=$U_AUD" "$T_ADMIN"
check "生效清单（admin）HTTP" "$HTTP" 200

api mask_page GET '/system/field-mask/page?page=1&page_size=20&table_name=sys_user&scope=role' "$T_ADMIN"
check "规则列表按表+作用域过滤命中" "$(printf '%s' "$BODY" | jq -r --arg id "$MASK" '[.data.list[].id] | index($id) != null')" true

# 真实接口出参：taskview（持 VIEWER，命中角色规则 2,2）→ 13****01；admin（超管豁免）→ 明文
api tv_userlist GET '/system/user/page?page=1&page_size=50&username=e2e09_auditor' "$T_TV"
check "taskview 查用户列表 HTTP" "$HTTP" 200
check "taskview 命中角色规则（2,2 → 13****01）" "$(data '.data.list[0].mobile')" "13****01"
api adm_userlist GET '/system/user/page?page=1&page_size=50&username=e2e09_auditor' "$T_ADMIN"
check "admin（超管豁免）看到的手机号是明文" "$(data '.data.list[0].mobile')" "13800009001"
api tv_userdetail GET "/system/user/$U_AUD" "$T_TV"
check "详情与列表同口径（详情也打码）" "$(data '.data.mobile')" "13****01"
check "DB 里仍是明文（脱敏只发生在出参）" "$(SQ "select mobile from sys_user where id='$U_AUD'::uuid")" "13800009001"

api mask_off PUT "/system/field-mask/$MASK" "$T_ADMIN" '{"mask_type":1,"mask_param":"2,2","status":0,"reason":"E2E09 演练：临时停用"}'
check "停用规则 → HTTP" "$HTTP" 200
check "停用状态落库" "$(SQ "select status from sys_field_mask where id='$MASK'::uuid")" 0
api mask_eff_off GET "/system/field-mask/effective?user_id=$U_TV" "$T_ADMIN"
check "停用后规则不在生效清单里" "$(printf '%s' "$BODY" | jq -r --arg id "$MASK" '[.data.rules[].rule_id] | index($id) == null')" true
check "停用后回落到全局规则（3,4 → 138****9001，缓存已失效）" "$(api tv_userlist2 GET '/system/user/page?page=1&page_size=50&username=e2e09_auditor' "$T_TV"; data '.data.list[0].mobile')" "138****9001"
api mask_on PUT "/system/field-mask/$MASK" "$T_ADMIN" '{"mask_type":1,"mask_param":"2,2","status":1,"reason":"E2E09 演练：恢复启用"}'
check "恢复启用 → HTTP" "$HTTP" 200
check "恢复后又命中角色规则（13****01）" "$(api tv_userlist3 GET '/system/user/page?page=1&page_size=50&username=e2e09_auditor' "$T_TV"; data '.data.list[0].mobile')" "13****01"

# ============ ⑦ 角色互斥 ============
say ""
say "--- ⑦ 角色互斥：建规则 / 冲突授予 409 / 存量双持拦建 409 / 停用与恢复 / 体检与扫描 ---"
api mutex_create POST /business/mutex "$T_ADMIN" "{\"role_a_id\":\"$MA\",\"role_b_id\":\"$MB\",\"reason\":\"E2E09 演练：A 与 B 不得兼任\"}"
check "建互斥规则 A×B → HTTP" "$HTTP" 200
MUTEX=$(data '.data.id'); save MUTEX "$MUTEX"
check "规则回显 role_a_code/role_b_code 已规范化" "$(printf '%s' "$BODY" | jq -r '[.data.role_a_code,.data.role_b_code] | sort | join(",")')" "E2E09_MUTEX_A,E2E09_MUTEX_B"
check "create_by_name 实时取名 = 超级管理员（AUD-02）" "$(data '.data.create_by_name')" "超级管理员"
api mutex_self POST /business/mutex "$T_ADMIN" "{\"role_a_id\":\"$MA\",\"role_b_id\":\"$MA\",\"reason\":\"E2E09 自反\"}"
check "角色与自身互斥 → HTTP" "$HTTP" 400
check "角色与自身互斥 → 61012" "$CODE" 61012
api mutex_dup POST /business/mutex "$T_ADMIN" "{\"role_a_id\":\"$MB\",\"role_b_id\":\"$MA\",\"reason\":\"E2E09 重复对（顺序颠倒）\"}"
check "重复互斥对（反向提交）→ HTTP" "$HTTP" 409

api mutex_assign PUT "/system/user/$U_MU/roles" "$T_ADMIN" "{\"role_ids\":[\"$MA\",\"$MB\"]}"
check "给用户同时授予 A+B → HTTP" "$HTTP" 409
check "互斥冲突 → 61009" "$CODE" 61009
contains "冲突报文点名互斥角色（名称）" "$(data '.msg')" "演练互斥"
check "DB 里未落 B（失败不写半截）" "$(SQ "select count(*) from sys_user_role where user_id='$U_MU'::uuid")" 1

api mutex_holders POST /business/mutex "$T_ADMIN" "{\"role_a_id\":\"$MC\",\"role_b_id\":\"$MD\",\"reason\":\"E2E09 演练：C 与 D 存量双持\"}"
check "存量已有双持时建规则 → HTTP" "$HTTP" 409
check "存量双持拦建 → 61010" "$CODE" 61010
contains "61010 报文点名持有者" "$(data '.msg')" "e2e09_mutex_u2"

api mutex_conflict GET "/business/mutex/conflict?user_id=$U_MU2" "$T_ADMIN"
check "用户互斥体检 HTTP" "$HTTP" 200
check "C/D 尚无规则 → conflicts 为空" "$(data '.data.conflicts | length')" 0
api mutex_page GET '/business/mutex/page?page=1&page_size=20&keyword=E2E09' "$T_ADMIN"
check "规则列表按关键词命中 1 条" "$(data '.data.total')" 1
api mutex_scan POST /business/mutex/scan "$T_ADMIN"
check "全量扫描 HTTP" "$HTTP" 200
check "扫描生效规则对数 ≥1" "$(printf '%s' "$BODY" | jq -r '.data.scanned_pairs >= 1')" true
check "扫描违规持有者 = 0" "$(data '.data.violated_holders')" 0

# 停用规则 → 允许双持；恢复启用 → 存量双持拦住
api mutex_disable PUT "/business/mutex/$MUTEX" "$T_ADMIN" '{"enabled":false,"reason":"E2E09 演练：临时停用"}'
check "停用互斥规则 → HTTP" "$HTTP" 200
api mutex_assign_ok PUT "/system/user/$U_MU/roles" "$T_ADMIN" "{\"role_ids\":[\"$MA\",\"$MB\"]}"
check "停用后授予 A+B → HTTP" "$HTTP" 200
check "DB 双持落库 2 行" "$(SQ "select count(*) from sys_user_role where user_id='$U_MU'::uuid")" 2
api mutex_enable PUT "/business/mutex/$MUTEX" "$T_ADMIN" '{"enabled":true}'
check "存量双持时恢复启用 → HTTP" "$HTTP" 409
check "恢复启用被存量拦住 → 61010" "$CODE" 61010
api mutex_back PUT "/system/user/$U_MU/roles" "$T_ADMIN" "{\"role_ids\":[\"$MA\"]}"
check "解除 B（只剩 A）→ HTTP" "$HTTP" 200
api mutex_enable2 PUT "/business/mutex/$MUTEX" "$T_ADMIN" '{"enabled":true}'
check "无存量双持后恢复启用 → HTTP" "$HTTP" 200
api mutex_assign2 PUT "/system/user/$U_MU/roles" "$T_ADMIN" "{\"role_ids\":[\"$MA\",\"$MB\"]}"
check "恢复后再次授予 A+B → 仍 409" "$HTTP" 409
check "恢复后再次授予 → 61009" "$CODE" 61009

# ============ ⑧ 变更日志 ============
say ""
say "--- ⑧ 变更日志：列表 / 时间线 / 统计 / 表清单 / 详情 / 导出（L1+L3 脱敏）---"
api cl_page GET '/business/changelog/page?page=1&page_size=50&resource=sys_role' "$T_ADMIN"
check "变更日志列表（resource=sys_role）HTTP" "$HTTP" 200
check "有本卡角色的变更记录" "$(printf '%s' "$BODY" | jq -r --arg id "$VIEWER" '[.data.list[].resource_id] | index($id) != null')" true
check "操作人名字落库并可读" "$(printf '%s' "$BODY" | jq -r '[.data.list[] | select(.operator_name=="admin")] | length > 0')" true
api cl_timeline GET "/business/changelog/timeline?table=sys_role&target_id=$VIEWER" "$T_ADMIN"
check "对象时间线 HTTP" "$HTTP" 200
check "时间线 ≥2 条（建 + 改数据范围/上级）" "$(printf '%s' "$BODY" | jq -r '.data.items | length >= 2')" true
api cl_stats GET '/business/changelog/stats?module=system' "$T_ADMIN"
check "统计 HTTP" "$HTTP" 200
check "system 模块变更数 > 0" "$(printf '%s' "$BODY" | jq -r '.data.total > 0')" true
check "统计 by_module 首项 key = system" "$(data '.data.by_module[0].key')" "system"
api cl_registry GET '/business/changelog/table-registry' "$T_ADMIN"
check "表清单含 sys_user / sys_role" "$(printf '%s' "$BODY" | jq -r '[.data.tables[].table] as $t | (($t|index("sys_user"))!=null) and (($t|index("sys_role"))!=null)')" true
api cl_page2 GET '/business/changelog/page?page=1&page_size=1&resource=sys_role' "$T_ADMIN"
CLID=$(data '.data.list[0].id'); CLTS=$(data '.data.list[0].created_at')
ENC=$(printf '%s' "$CLTS" | sed 's/+/%2B/g')
api cl_detail GET "/business/changelog/$CLID?created_at=$ENC" "$T_ADMIN"
check "变更详情 HTTP" "$HTTP" 200
check "详情带 changed_fields" "$(printf '%s' "$BODY" | jq -r '.data.changed_fields | length >= 1')" true

# L1：写库即脱敏 —— sys_user 的 mobile 明文不得出现在 change_log.diff 里
check "change_log 里无本卡用户手机号明文（L1 写库脱敏）" "$(SQ "select count(*) from sys_change_log where resource='sys_user' and diff::text like '%13800009001%'")" 0
check "change_log 里存的是打码值（L1）" "$(SQ "select count(*)>0 from sys_change_log where resource='sys_user' and diff::text like '%138****9001%'")" t

# 导出（xlsx，含 L3 再脱敏头；响应头是 HTTP 小写，按不区分大小写匹配）
api cl_export GET '/business/changelog/export?module=system&limit=200' "$T_ADMIN"
check "导出 HTTP" "$HTTP" 200
HDR=$(curl -s -D - -o /dev/null -m 30 --oauth2-bearer "$T_ADMIN" "$BASE/business/changelog/export?module=system&limit=50" | tr -d '\r')
matches_i "导出响应头 x-mask-applied: true" "$HDR" 'x-mask-applied: true'
matches_i "导出响应头 x-mask-policy: L1+L3" "$HDR" 'x-mask-policy: L1\+L3'

say ""
summary
