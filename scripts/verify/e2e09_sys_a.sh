#!/usr/bin/env bash
# E2E-09 A：用户管理 / 角色管理 / 菜单管理 / 部门管理 + 权限生效（①②③④ + 关键点「权限生效」）
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e09_common.sh"

say "================ E2E-09 A：用户 / 角色 / 菜单 / 部门 + 权限闸 ================"
T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD") || { bad "admin 登录失败"; summary; exit 1; }
save T_ADMIN "$T_ADMIN"
ok "admin 令牌就绪 (len=${#T_ADMIN})"
BASE_MENU=$(SQ "select count(*) from sys_menu where deleted_at is null")
BASE_DEPT=$(SQ "select count(*) from sys_dept where deleted_at is null")
say "  基线：菜单 ${BASE_MENU} 条 / 部门 ${BASE_DEPT} 个"
save BASE_MENU "$BASE_MENU"; save BASE_DEPT "$BASE_DEPT"

# ============================ ④ 部门管理 ============================
say ""
say "--- ④ 部门管理：树 / 负责人 / 迁移删除 ---"
api dept_d1 POST /system/dept "$T_ADMIN" '{"dept_code":"E2E09-D1","dept_name":"演练一部","sort_order":910,"status":1,"phone":"010-88000001","email":"d1@e2e09.local"}'
check "建根部门 演练一部 → HTTP" "$HTTP" 200
D1=$(data '.data.id'); save D1 "$D1"
matches "根部门返回 uuid" "$D1" '^[0-9a-f]{8}-'

api dept_d1a POST /system/dept "$T_ADMIN" "{\"dept_code\":\"E2E09-D1A\",\"dept_name\":\"演练一部A组\",\"parent_id\":\"$D1\",\"sort_order\":10,\"status\":1}"
check "建子部门 A组 → HTTP" "$HTTP" 200
D1A=$(data '.data.id'); save D1A "$D1A"

api dept_d2 POST /system/dept "$T_ADMIN" '{"dept_code":"E2E09-D2","dept_name":"演练二部","sort_order":920,"status":1}'
check "建根部门 演练二部 → HTTP" "$HTTP" 200
D2=$(data '.data.id'); save D2 "$D2"

api dept_d3 POST /system/dept "$T_ADMIN" '{"dept_code":"E2E09-D3","dept_name":"演练三部","sort_order":930,"status":1}'
check "建根部门 演练三部（迁移删除用）→ HTTP" "$HTTP" 200
D3=$(data '.data.id'); save D3 "$D3"

api dept_dup POST /system/dept "$T_ADMIN" '{"dept_code":"E2E09-D1","dept_name":"演练一部副本","sort_order":911,"status":1}'
check "部门编码重复 → HTTP" "$HTTP" 409
check "部门编码重复 → 错误码 30008" "$CODE" 30008
api dept_dupname POST /system/dept "$T_ADMIN" "{\"dept_code\":\"E2E09-D1B\",\"dept_name\":\"演练一部A组\",\"parent_id\":\"$D1\",\"sort_order\":11,\"status\":1}"
check "同父下部门名重复 → HTTP" "$HTTP" 409
check "同父下部门名重复 → 错误码 30011" "$CODE" 30011

api dept_tree GET /system/dept/tree "$T_ADMIN"
check "部门树 → HTTP" "$HTTP" 200
check "新建部门数 = 基线 + 4" "$(printf '%s' "$BODY" | jq '[.. | objects | select(.dept_code? and (.dept_code|startswith("E2E09-")))] | length')" 4
check "A组挂在演练一部下" "$(printf '%s' "$BODY" | jq -r --arg id "$D1A" '[.. | objects | select(.id==$id)][0].parent_id')" "$D1"
check "根节点的 children 含 A组（勾稽）" "$(printf '%s' "$BODY" | jq -r --arg p "$D1" '[.. | objects | select(.id==$p)][0].children | map(.dept_code) | join(",")')" "E2E09-D1A"

# ============================ ② 角色管理 ============================
say ""
say "--- ② 角色管理：建角色 / 数据范围 / 上级角色（升级链配置）/ 非法配置 ---"
api role_viewer POST /system/role "$T_ADMIN" '{"role_code":"E2E09_VIEWER","role_name":"演练查看员","description":"E2E09 只读","status":1,"sort_order":930,"data_scope":2,"escalation_role_id":""}'
check "建角色 E2E09_VIEWER → HTTP" "$HTTP" 200
VIEWER=$(data '.data.id'); save VIEWER "$VIEWER"
check "新建角色回显 data_scope = 2（本部门）" "$(data '.data.data_scope')" 2
check "新建角色 escalation_role_id 空" "$(data '.data.escalation_role_id')" ""

api role_boss POST /system/role "$T_ADMIN" '{"role_code":"E2E09_BOSS","role_name":"演练主管","status":1,"sort_order":931,"data_scope":1,"escalation_role_id":""}'
check "建角色 E2E09_BOSS → HTTP" "$HTTP" 200
BOSS=$(data '.data.id'); save BOSS "$BOSS"

api role_dup POST /system/role "$T_ADMIN" '{"role_code":"E2E09_VIEWER","role_name":"重复编码","status":1}'
check "角色编码重复 → HTTP" "$HTTP" 409
check "角色编码重复 → 错误码 30002" "$CODE" 30002

api role_scope6 POST /system/role "$T_ADMIN" '{"role_code":"E2E09_BAD6","role_name":"非法范围","status":1,"data_scope":6}'
check "data_scope=6 非法 → HTTP" "$HTTP" 400

api role_set_esc PUT "/system/role/$VIEWER" "$T_ADMIN" "{\"role_code\":\"E2E09_VIEWER\",\"role_name\":\"演练查看员\",\"description\":\"E2E09 只读\",\"status\":1,\"sort_order\":930,\"data_scope\":2,\"escalation_role_id\":\"$BOSS\"}"
check "VIEWER 上级角色设为 BOSS → HTTP" "$HTTP" 200
api role_get_viewer GET "/system/role/$VIEWER" "$T_ADMIN"
check "回读 escalation_role_id = BOSS（升级链配置落库）" "$(data '.data.escalation_role_id')" "$BOSS"
check "DB 里 escalation_role_id 一致" "$(SQ "select escalation_role_id from sys_role where id='$VIEWER'::uuid")" "$BOSS"

api role_esc_self PUT "/system/role/$VIEWER" "$T_ADMIN" "{\"role_code\":\"E2E09_VIEWER\",\"role_name\":\"演练查看员\",\"status\":1,\"sort_order\":930,\"data_scope\":2,\"escalation_role_id\":\"$VIEWER\"}"
check "上级角色指向自己 → HTTP" "$HTTP" 400
check "上级角色指向自己 → 30013" "$CODE" 30013
contains "自引用报文" "$(data '.msg')" "不能是本角色自己"

api role_esc_cycle PUT "/system/role/$BOSS" "$T_ADMIN" "{\"role_code\":\"E2E09_BOSS\",\"role_name\":\"演练主管\",\"status\":1,\"sort_order\":931,\"data_scope\":1,\"escalation_role_id\":\"$VIEWER\"}"
check "上级角色成环（BOSS→VIEWER→BOSS）→ HTTP" "$HTTP" 400
check "成环 → 30013" "$CODE" 30013
contains "成环报文" "$(data '.msg')" "环"

api role_list GET /system/role/list "$T_ADMIN"
check "角色下拉含两个新角色" "$(printf '%s' "$BODY" | jq -r '[.data[]? | select(.role_code|startswith("E2E09_"))] | length')" 2
api role_page GET "/system/role/page?page=1&page_size=10&role_code=E2E09" "$T_ADMIN"
check "角色分页 keyword 命中 2 条" "$(data '.data.total')" 2

# ============================ ① 用户管理 ============================
say ""
say "--- ① 用户管理：建用户（角色+部门）/ 停用 / 改密 / 调岗 ---"
api user_auditor POST /system/user "$T_ADMIN" "{\"username\":\"e2e09_auditor\",\"password\":\"E2e09@Pass1\",\"real_name\":\"演练审计员\",\"email\":\"auditor@e2e09.local\",\"mobile\":\"13800009001\",\"dept_id\":\"$D1A\",\"status\":1,\"role_ids\":[\"$VIEWER\"]}"
check "建用户 e2e09_auditor（带部门+角色）→ HTTP" "$HTTP" 200
U_AUD=$(data '.data.id'); save U_AUD "$U_AUD"
check "回显部门名 = 演练一部A组" "$(data '.data.dept_name')" "演练一部A组"
check "回显角色 1 个 = E2E09_VIEWER" "$(data '[.data.roles[].role_code] | join(",")')" "E2E09_VIEWER"
check "DB 落库 dept_id 一致" "$(SQ "select dept_id from sys_user where id='$U_AUD'::uuid")" "$D1A"
check "DB sys_user_role 1 行" "$(SQ "select count(*) from sys_user_role where user_id='$U_AUD'::uuid")" 1

api user_other POST /system/user "$T_ADMIN" "{\"username\":\"e2e09_other\",\"password\":\"E2e09@Pass1\",\"real_name\":\"演练他人\",\"dept_id\":\"$D1\",\"status\":1}"
check "建用户 e2e09_other（无角色）→ HTTP" "$HTTP" 200
U_OTH=$(data '.data.id'); save U_OTH "$U_OTH"

api user_dup POST /system/user "$T_ADMIN" '{"username":"e2e09_auditor","password":"E2e09@Pass1"}'
check "用户名重复 → HTTP" "$HTTP" 409
check "用户名重复 → 30010" "$CODE" 30010

api user_pg GET "/system/user/page?page=1&page_size=50&username=e2e09_auditor" "$T_ADMIN"
check "用户分页命中 1 条" "$(data '.data.total')" 1
check "列表 dept_name 已 join" "$(data '.data.list[0].dept_name')" "演练一部A组"
check "列表 roles 已聚合" "$(data '[.data.list[0].roles[].role_code] | join(",")')" "E2E09_VIEWER"
api user_detail GET "/system/user/$U_AUD" "$T_ADMIN"
check "用户详情 HTTP 200 且 dept_name 一致" "$(data '.data.dept_name')" "演练一部A组"

# 角色分配（全量替换）
api user_roles PUT "/system/user/$U_AUD/roles" "$T_ADMIN" "{\"role_ids\":[\"$VIEWER\",\"$BOSS\"]}"
check "改分配角色 [VIEWER,BOSS] → HTTP" "$HTTP" 200
check "DB 角色行数 2" "$(SQ "select count(*) from sys_user_role where user_id='$U_AUD'::uuid")" 2
api user_roles_get GET "/system/user/$U_AUD" "$T_ADMIN"
check "回读角色 2 个" "$(printf '%s' "$BODY" | jq -r '[.data.roles[].role_code] | sort | join(",")')" "E2E09_BOSS,E2E09_VIEWER"
api user_roles_restore PUT "/system/user/$U_AUD/roles" "$T_ADMIN" "{\"role_ids\":[\"$VIEWER\"]}"
check "角色回退为 [VIEWER] → HTTP" "$HTTP" 200
check "DB 角色行数回 1" "$(SQ "select count(*) from sys_user_role where user_id='$U_AUD'::uuid")" 1

# 停用 / 启用
api user_off PATCH "/system/user/$U_AUD/status" "$T_ADMIN" '{"status":0}'
check "停用用户 → HTTP" "$HTTP" 200
check "DB status = 0" "$(SQ "select status from sys_user where id='$U_AUD'::uuid")" 0
api login_disabled POST /system/auth/login "" '{"username":"e2e09_auditor","password":"E2e09@Pass1"}'
check "停用账号登录 → HTTP" "$HTTP" 403
check "停用账号登录 → 20002" "$CODE" 20002
api user_on PATCH "/system/user/$U_AUD/status" "$T_ADMIN" '{"status":1}'
check "启用用户 → HTTP" "$HTTP" 200
check "DB status = 1" "$(SQ "select status from sys_user where id='$U_AUD'::uuid")" 1

# 改密（管理员重置 → 首登强制改密）
api user_pwd PATCH "/system/user/$U_AUD/password" "$T_ADMIN" '{"password":"E2e09@Reset1"}'
check "管理员重置密码 → HTTP" "$HTTP" 200
api login_reset POST /system/auth/login "" '{"username":"e2e09_auditor","password":"E2e09@Reset1"}'
check "新密码登录 → HTTP" "$HTTP" 200
check "被重置后 must_change_password = true" "$(data '.data.must_change_password')" true
TRESET=$(data '.data.access_token')
api chpwd_reset PUT /system/auth/password "$TRESET" '{"old_password":"E2e09@Reset1","new_password":"E2e09@Pass1"}'
check "首登强制改密回原口令 → HTTP" "$HTTP" 200
check "DB must_change_password 回落 false" "$(SQ "select must_change_password from sys_user where id='$U_AUD'::uuid")" f

# 批量调岗
api user_batch PUT /system/user/batch-dept "$T_ADMIN" "{\"user_ids\":[\"$U_OTH\"],\"dept_id\":\"$D2\"}"
check "批量调整部门（other → 演练二部）→ HTTP" "$HTTP" 200
check "DB other.dept_id = D2" "$(SQ "select dept_id from sys_user where id='$U_OTH'::uuid")" "$D2"
api user_batch_back PUT /system/user/batch-dept "$T_ADMIN" "{\"user_ids\":[\"$U_OTH\"],\"dept_id\":\"$D1\"}"
check "调回演练一部 → HTTP" "$HTTP" 200

# 部门负责人（AUD-02 姓名 join）
api dept_leader PUT "/system/dept/$D1A" "$T_ADMIN" "{\"dept_code\":\"E2E09-D1A\",\"dept_name\":\"演练一部A组\",\"parent_id\":\"$D1\",\"leader_id\":\"$U_AUD\",\"sort_order\":10,\"status\":1}"
check "设置部门负责人 → HTTP" "$HTTP" 200
api dept_detail GET "/system/dept/$D1A" "$T_ADMIN"
check "部门详情 leader_id 一致" "$(data '.data.leader_id')" "$U_AUD"
check "部门详情 leader_name 实时取名 = 演练审计员" "$(data '.data.leader_name')" "演练审计员"
api dept_tree2 GET /system/dept/tree "$T_ADMIN"
check "部门树 leader_name 已批量填充" "$(printf '%s' "$BODY" | jq -r --arg id "$D1A" '[.. | objects | select(.id==$id)][0].leader_name')" "演练审计员"

# ============================ ③ 菜单管理 ============================
say ""
say "--- ③ 菜单管理：树 / 排序 / 可见性 / 按钮权限点 ---"
api menu_root POST /system/menu "$T_ADMIN" '{"name":"e2e09_root","menu_type":1,"auth_code":"","path":"/e2e09","title":"演练九","icon":"folder","sort_order":940,"status":1,"is_visible":true}'
check "建菜单目录 → HTTP" "$HTTP" 200
M_ROOT=$(data '.data.id'); save M_ROOT "$M_ROOT"
api menu_page POST /system/menu "$T_ADMIN" "{\"name\":\"e2e09_page\",\"menu_type\":2,\"auth_code\":\"e2e09:demo:list\",\"path\":\"/e2e09/page\",\"parent_id\":\"$M_ROOT\",\"component\":\"e2e09/index\",\"title\":\"演练页面\",\"sort_order\":10,\"status\":1,\"is_visible\":true,\"is_cache\":true}"
check "建页面菜单 → HTTP" "$HTTP" 200
M_PAGE=$(data '.data.id'); save M_PAGE "$M_PAGE"
api menu_btn POST /system/menu "$T_ADMIN" "{\"name\":\"e2e09_btn\",\"menu_type\":3,\"auth_code\":\"e2e09:demo:create\",\"parent_id\":\"$M_PAGE\",\"title\":\"新增\",\"sort_order\":10,\"status\":1}"
check "建按钮权限点 → HTTP" "$HTTP" 200
M_BTN=$(data '.data.id'); save M_BTN "$M_BTN"
check "DB 菜单落库 3 条（path 前缀）" "$(SQ "select count(*) from sys_menu where deleted_at is null and (path like '/e2e09%' or name like 'e2e09_%')")" 3

api menu_tree GET '/system/menu/tree' "$T_ADMIN"
check "菜单树 → HTTP" "$HTTP" 200
check "菜单树含本卡 3 节点" "$(printf '%s' "$BODY" | jq '[.. | objects | select(.name? and (.name|startswith("e2e09_")))] | length')" 3
check "三层嵌套：目录.children 含页面" "$(printf '%s' "$BODY" | jq -r '[.. | objects | select(.name?=="e2e09_root")][0].children | map(.name) | join(",")')" "e2e09_page"
check "三层嵌套：页面.children 含按钮" "$(printf '%s' "$BODY" | jq -r '[.. | objects | select(.name?=="e2e09_page")][0].children | map(.name) | join(",")')" "e2e09_btn"
check "页面 title = 演练页面" "$(printf '%s' "$BODY" | jq -r '[.. | objects | select(.name?=="e2e09_page")][0].title')" "演练页面"

api menu_vis PUT "/system/menu/$M_PAGE" "$T_ADMIN" "{\"name\":\"e2e09_page\",\"menu_type\":2,\"auth_code\":\"e2e09:demo:list\",\"path\":\"/e2e09/page\",\"parent_id\":\"$M_ROOT\",\"component\":\"e2e09/index\",\"title\":\"演练页面\",\"sort_order\":20,\"status\":1,\"is_visible\":false,\"is_cache\":true}"
check "改可见性 false + 排序 20 → HTTP" "$HTTP" 200
api menu_tree2 GET '/system/menu/tree' "$T_ADMIN"
check "树里 is_visible 已变 false" "$(printf '%s' "$BODY" | jq -r '[.. | objects | select(.name=="e2e09_page")][0].is_visible')" false
check "树里 sort_order 已变 20" "$(printf '%s' "$BODY" | jq -r '[.. | objects | select(.name=="e2e09_page")][0].sort_order')" 20
api menu_detail GET "/system/menu/$M_BTN" "$T_ADMIN"
check "菜单详情 auth_code 一致" "$(data '.data.auth_code')" "e2e09:demo:create"

# 结构不变量（只查本卡新建节点）
check "本卡菜单 title 均 ≤12 字（MENU-FIX-1 口径）" "$(SQ "select count(*) from sys_menu where deleted_at is null and (path like '/e2e09%' or name like 'e2e09_%') and menu_type in (1,2) and length(title)>12")" 0
check "本卡同父 sort 无重复（menu_type=2）" "$(SQ "select count(*) from (select parent_id,sort_order from sys_menu where deleted_at is null and menu_type=2 and (path like '/e2e09%') group by 1,2 having count(*)>1) t")" 0

# ============================ 权限生效（核心） ============================
say ""
say "--- 权限闸：分配菜单后可访问 / 未分配 403 / 授权变更旧令牌失效 ---"
T_AUD=$(login e2e09_auditor 'E2e09@Pass1') || bad "e2e09_auditor 登录失败"
save T_AUD "$T_AUD"
ok "e2e09_auditor 令牌就绪"
api aud_noperm GET '/system/user/page?page=1&page_size=5' "$T_AUD"
check "未分配菜单时访问用户列表 → HTTP" "$HTTP" 403
check "未分配菜单时 → 响应 code 403（权限闸）" "$CODE" 403
contains "403 报文点名缺失的权限码" "$(data '.msg')" "system:user:list"

UID_IDS=$(menu_ids_by_code "$T_ADMIN" "system:user:list")
say "  system:user:list 的菜单节点 id：$UID_IDS"
[ -n "$UID_IDS" ] || bad "菜单树里找不到 system:user:list 节点"
JSON_IDS=$(printf '%s' "$UID_IDS" | tr ' ' '\n' | jq -R . | jq -s -c .)
api role_grant PUT "/system/role/$VIEWER/menus" "$T_ADMIN" "{\"menu_ids\":$JSON_IDS}"
check "给 VIEWER 授 system:user:list → HTTP" "$HTTP" 200
check "DB sys_role_menu 行数 = 节点数" "$(SQ "select count(*) from sys_role_menu where role_id='$VIEWER'::uuid")" "$(printf '%s' "$UID_IDS" | wc -w | tr -d ' ')"
api role_grant_get GET "/system/role/$VIEWER/menus" "$T_ADMIN"
check "GET 角色菜单回读行数一致" "$(printf '%s' "$BODY" | jq '.data | length')" "$(printf '%s' "$UID_IDS" | wc -w | tr -d ' ')"
api aud_stale GET '/system/user/page?page=1&page_size=5' "$T_AUD"
check "授权后旧令牌 → HTTP 403（旧快照不含新权限，无越权）" "$HTTP" 403
check "授权后旧令牌 → code 403" "$CODE" 403
T_AUD=$(login e2e09_auditor 'E2e09@Pass1') || bad "改权后重登失败"
save T_AUD "$T_AUD"
api aud_perm GET '/system/user/page?page=1&page_size=5' "$T_AUD"
check "重登后访问用户列表 → HTTP" "$HTTP" 200
api aud_role GET '/system/role/page?page=1&page_size=5' "$T_AUD"
check "未授权的角色管理 → HTTP" "$HTTP" 403
check "未授权的角色管理 → code 403" "$CODE" 403
api aud_dept GET '/system/dept/tree' "$T_AUD"
check "未授权的部门树 → HTTP" "$HTTP" 403

api role_revoke PUT "/system/role/$VIEWER/menus" "$T_ADMIN" '{"menu_ids":[]}'
check "收回 VIEWER 全部菜单 → HTTP" "$HTTP" 200
check "DB sys_role_menu 清空" "$(SQ "select count(*) from sys_role_menu where role_id='$VIEWER'::uuid")" 0
api aud_revoked_stale GET '/system/user/page?page=1&page_size=5' "$T_AUD"
check "收回后旧令牌 → HTTP 401（会话版本校验拦截残留越权）" "$HTTP" 401
check "收回后旧令牌 → 20008 AUTH_PERMISSION_CHANGED" "$CODE" 20008
T_AUD=$(login e2e09_auditor 'E2e09@Pass1') || bad "收回后重登失败"
save T_AUD "$T_AUD"
api aud_revoked GET '/system/user/page?page=1&page_size=5' "$T_AUD"
check "收回后重登再访问 → HTTP" "$HTTP" 403
# 复原为已授权状态（供 B 卡/后续使用）
api role_regrant PUT "/system/role/$VIEWER/menus" "$T_ADMIN" "{\"menu_ids\":$JSON_IDS}"
check "重新授权（复原）→ HTTP" "$HTTP" 200

# ============================ 菜单删除（收尾） ============================
api menu_del_btn DELETE "/system/menu/$M_BTN" "$T_ADMIN"
check "删按钮菜单 → HTTP" "$HTTP" 200
api menu_tree3 GET '/system/menu/tree' "$T_ADMIN"
check "树里按钮已消失（剩 2 节点）" "$(printf '%s' "$BODY" | jq '[.. | objects | select(.name? and (.name|startswith("e2e09_")))] | length')" 2
check "DB 按钮已软删" "$(SQ "select count(*) from sys_menu where id='$M_BTN'::uuid and deleted_at is not null")" 1

# ============================ 部门迁移删除 ============================
say ""
say "--- 部门迁移删除（用户先迁移，再软删部门；事务一致性）---"
api user_d3 POST /system/user "$T_ADMIN" "{\"username\":\"e2e09_d3u\",\"password\":\"E2e09@Pass1\",\"real_name\":\"演练三部人\",\"dept_id\":\"$D3\",\"status\":1}"
check "建 D3 部门用户 → HTTP" "$HTTP" 200
U_D3=$(data '.data.id'); save U_D3 "$U_D3"
api dept_migrate POST "/system/dept/$D3/migrate-delete" "$T_ADMIN" "{\"migrate_to_dept_id\":\"$D2\"}"
check "迁移删除 D3（用户迁到 D2）→ HTTP" "$HTTP" 200
check "D3 已软删" "$(SQ "select count(*) from sys_dept where id='$D3'::uuid and deleted_at is not null")" 1
check "该用户 dept_id 已迁到 D2" "$(SQ "select dept_id from sys_user where id='$U_D3'::uuid")" "$D2"
api dept_haschild POST "/system/dept/$D1/migrate-delete" "$T_ADMIN" '{"migrate_to_dept_id":null}'
check "删有子部门的 D1 → HTTP" "$HTTP" 409
check "删有子部门 → 30007" "$CODE" 30007

say ""
summary
