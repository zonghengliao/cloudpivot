#!/usr/bin/env bash
# E2E-09 DB 证据 + 勾稽 + 菜单不变量 + 历史问题回归（V0335 / V0320）
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e09_common.sh"

say "================ E2E-09 DB 证据 / 勾稽 / 不变量 / 历史回归 ================"
say ""
say "--- 1) 落库证据（本卡 E2E09 前缀）---"
SQ "select 'user='||(select count(*) from sys_user where username like 'e2e09%' and deleted_at is null)
     ||' role='||(select count(*) from sys_role where role_code like 'E2E09%' and deleted_at is null)
     ||' dept='||(select count(*) from sys_dept where dept_code like 'E2E09-%' and deleted_at is null)
     ||' menu='||(select count(*) from sys_menu where deleted_at is null and (path like '/e2e09%' or name like 'e2e09_%'))
     ||' user_role='||(select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where u.username like 'e2e09%')
     ||' role_menu='||(select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id where r.role_code like 'E2E09%')
     ||' field_mask='||(select count(*) from sys_field_mask where reason like 'E2E09%' and deleted_at is null)
     ||' role_mutex='||(select count(*) from sys_role_mutex where reason like 'E2E09%' and deleted_at is null)
     ||' task='||(select count(*) from biz_task where title like 'E2E09-%' or title like 'E2E09-T%')
     ||' flow_def='||(select count(*) from biz_flow_def where flow_code like 'e2e09_%' and deleted_at is null)
     ||' flow_inst='||(select count(*) from biz_flow_instance where title like 'E2E09-%')"
check "落库：本卡用户 12 个" "$(SQ "select count(*) from sys_user where username like 'e2e09%' and deleted_at is null")" 12

say ""
say "--- 2) 勾稽（跨表一致性）---"
say "  ① 部门树：子部门 parent / dept_path"
SQ "select d.dept_code||' parent='||coalesce(p.dept_code,'-')||' path='||d.dept_path from sys_dept d left join sys_dept p on p.id=d.parent_id where d.dept_code like 'E2E09-%' order by d.dept_code"
check "① 子部门 parent 指向 演练一部" "$(SQ "select p.dept_code from sys_dept d join sys_dept p on p.id=d.parent_id where d.dept_code='E2E09-D1A'")" "E2E09-D1"
check "① 子部门 dept_path 含父 id" "$(SQ "select case when position((select id::text from sys_dept where dept_code='E2E09-D1') in d.dept_path)>0 then 1 else 0 end from sys_dept d where d.dept_code='E2E09-D1A'")" 1

say "  ② 角色-菜单授权：权限码 vs 菜单节点（VIEWER 4 个码）"
SQ "select r.role_code||' → '||m.auth_code from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code='E2E09_VIEWER' order by m.auth_code"
check "② VIEWER 授权码数 = 4" "$(SQ "select count(distinct m.auth_code) from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code='E2E09_VIEWER'")" 4

say "  ③ 用户-角色：API 回显数 vs 库内行数"
check "③ auditor 库内 1 行角色" "$(SQ "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id where u.username='e2e09_auditor'")" 1
check "③ taskview 库内 1 行角色（VIEWER）" "$(SQ "select r.role_code from sys_user_role ur join sys_user u on u.id=ur.user_id join sys_role r on r.id=ur.role_id where u.username='e2e09_taskview'")" "E2E09_VIEWER"

say "  ④ 任务归属：biz_task.dept_id 应等于负责人所在部门（0 处不一致）"
check "④ 3 条任务 dept_id 与负责人部门全一致" "$(SQ "select count(*) from biz_task t join sys_user u on u.id=t.owner_id where t.title like 'E2E09-T%' and t.dept_id is distinct from u.dept_id")" 0

say "  ⑤ 数据权限：sys_role.data_scope 与自定义部门存量"
SQ "select role_code||' data_scope='||data_scope from sys_role where role_code like 'E2E09%' order by role_code"
check "⑤ VIEWER data_scope 回滚为 2" "$(SQ "select data_scope from sys_role where role_code='E2E09_VIEWER'")" 2
say "     （自定义集合按三态语义保留：切到 2 不清空配置，便于切回 5）"
SQ "select 'sys_role_dept rows='||count(*) from sys_role_dept rd join sys_role r on r.id=rd.role_id where r.role_code like 'E2E09%'"

say "  ⑥ 脱敏：库内明文 vs 出参打码"
check "⑥ sys_user.mobile 仍为明文（脱敏只在出参）" "$(SQ "select mobile from sys_user where username='e2e09_auditor'")" "13800009001"
check "⑥ 本卡角色规则 1 条（status=1）" "$(SQ "select count(*) from sys_field_mask where reason like 'E2E09%' and deleted_at is null and status=1")" 1
check "⑥ 规则参数 2,2 落库" "$(SQ "select mask_param from sys_field_mask where reason like 'E2E09%' and deleted_at is null")" "2,2"

say "  ⑦ 互斥：规则规范化（role_a_id < role_b_id）与违规持有 0"
check "⑦ 互斥对按 UUID 序规范化" "$(SQ "select case when m.role_a_id < m.role_b_id then 1 else 0 end from sys_role_mutex m join sys_role r on r.id=m.role_b_id where r.role_code='E2E09_MUTEX_B' and m.deleted_at is null")" 1
check "⑦ 无用户同时持有互斥对（A/B 双持 0）" "$(SQ "select count(*) from (select ur.user_id from sys_user_role ur join sys_role r on r.id=ur.role_id where r.role_code in ('E2E09_MUTEX_A','E2E09_MUTEX_B') group by 1 having count(*)>1) t")" 0

say "  ⑧ 变更日志：本卡角色的 CREATE/UPDATE 都在（含上级角色变更）"
SQ "select l.action||'|'||coalesce(l.changed_fields::text,'-') from sys_change_log l join sys_role r on r.id::text=l.resource_id where r.role_code='E2E09_VIEWER' order by l.created_at"
check "⑧ 本卡角色变更日志 ≥2 条（CREATE + UPDATE）" "$(SQ "select count(*)>=2 from sys_change_log l join sys_role r on r.id::text=l.resource_id where r.role_code='E2E09_VIEWER'")" t
check "⑧ 上级角色变更已进日志（changed_fields 含 escalation_role_id）" "$(SQ "select count(*)>0 from sys_change_log l join sys_role r on r.id::text=l.resource_id where r.role_code='E2E09_VIEWER' and l.changed_fields @> array['escalation_role_id']")" t

say "  ⑨ 降级链留痕：escalated_from_role / escalate_reason / assigned_by_rule"
SQ "select i.title||' | '||t.status||' | from='||coalesce(t.escalated_from_role,'-')||' | reason='||coalesce(t.escalate_reason,'-')||' | by='||coalesce(t.assigned_by_rule,'-') from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.title like 'E2E09-%' order by i.title, t.status"
check "⑨ SKIP 升级任务 2 条带 escalated_from_role=MAKER" "$(SQ "select count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.title='E2E09-自审跳过升级' and t.escalated_from_role='E2E09_ESC_MAKER'")" 2
check "⑨ 兜底人任务 1 条 assigned_by_rule=FALLBACK" "$(SQ "select count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.title='E2E09-兜底人接管' and t.assigned_by_rule='FALLBACK'")" 1

say ""
say "--- 3) 菜单结构不变量（scripts/verify/menu_invariants.sh，8 条）---"
set +e
MENU_OUT=$(bash /opt/cloudpivot/scripts/verify/menu_invariants.sh "$NIK_DATABASE_NAME" 2>&1)
MENU_RC=$?
printf '%s\n' "$MENU_OUT" | tail -25
check "菜单不变量脚本 rc=0（8 条全绿）" "$MENU_RC" 0

say ""
say "--- 4) 历史问题回归 ---"
say "  MENU-FIX-1（V0335）：menu_type 1/2 的 title 全部 ≤12 字"
check "title>12 的目录/页面 = 0 行" "$(SQ "select count(*) from sys_menu where deleted_at is null and menu_type in (1,2) and length(title)>12")" 0
check "同父 sort 重复（menu_type=2）= 0 组" "$(SQ "select count(*) from (select parent_id,sort_order from sys_menu where deleted_at is null and menu_type=2 group by 1,2 having count(*)>1) t")" 0
say "  V0320：单号序列回归基础数据（parent=/business/erp/master-data，sort=30）"
SQ "select m.path||' parent='||coalesce(p.path,'-')||' sort='||m.sort_order from sys_menu m left join sys_menu p on p.id=m.parent_id where m.path='/business/erp/seq' and m.deleted_at is null"
check "单号序列父节点 = 基础数据" "$(SQ "select p.path from sys_menu m join sys_menu p on p.id=m.parent_id where m.path='/business/erp/seq' and m.deleted_at is null")" "/business/erp/master-data"
check "单号序列 sort_order = 30" "$(SQ "select sort_order from sys_menu where path='/business/erp/seq' and deleted_at is null")" 30
check "迁移 V0320 / V0335 均已登记" "$(SQ "select count(*) from schema_migrations where version in ('320','335')")" 2

say ""
say "--- 5) 演练数据边界（本卡只造 E2E09 前缀）---"
SQ "select 'others_role_mutex='||count(*) from sys_role_mutex where reason not like 'E2E09%' and deleted_at is null
 union all select 'others_field_mask='||count(*) from sys_field_mask where reason not like 'E2E09%' and deleted_at is null
 union all select 'e2e09_users='||count(*) from sys_user where username like 'e2e09%' and deleted_at is null"

say ""
summary
