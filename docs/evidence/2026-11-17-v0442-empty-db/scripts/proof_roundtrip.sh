#!/usr/bin/env bash
# V0442 幂等 / down / 再 up 的可重放证据（在「441 状态、无种子」的库上手工重放）
#   ① up（空库首启路径：系统管理目录缺席 → NOTICE 跳过，不失败）
#   ② 再 up（幂等：授权 0 行、UPDATE 0 行）
#   ③ down（按确定性 id 精确删除 + 父节点归位回滚）
#   ④ 再 up（往返可重放）
set -uo pipefail
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
H=172.18.10.145
U=cloudpivot
SRC=cloudpivot_test_v0442a     # 441 状态（版本化跑完、种子未跑）
DB=cloudpivot_test_v0442f
UP=/opt/cloudpivot/backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.up.sql
DOWN=/opt/cloudpivot/backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.down.sql

dropdb -h "$H" -U $U --if-exists --force $DB >/dev/null 2>&1
createdb -h "$H" -U $U -O $U -T $SRC $DB && echo "== createdb -T $SRC → $DB OK"

counts() {
  psql -h "$H" -U $U -d $DB -Atc "
  select 'v0442_grant_rows='||count(*) from sys_role_menu rm
   where rm.id in (md5('v0442.'||rm.role_id||':'||rm.menu_id)::uuid, md5('v0442c.'||rm.role_id||':'||rm.menu_id)::uuid)"
  psql -h "$H" -U $U -d $DB -Atc "
  select 'notice_mine_btn_parent='||coalesce(parent_id::text,'(null)') from sys_menu
   where id='20000000-0000-0000-0000-000000000414'"
}

echo
echo "===== ① 第一次 up（空库首启路径）"
psql -h "$H" -U $U -d $DB -v ON_ERROR_STOP=1 -P pager=off < "$UP" 2>&1 | grep -E 'NOTICE|ERROR' | sed 's/^/    /'
counts

echo
echo "===== ② 第二次 up（幂等：应为 0 行变化）"
psql -h "$H" -U $U -d $DB -v ON_ERROR_STOP=1 -P pager=off < "$UP" 2>&1 | grep -E 'NOTICE|ERROR' | sed 's/^/    /'
counts

echo
echo "===== ③ down"
psql -h "$H" -U $U -d $DB -v ON_ERROR_STOP=1 -P pager=off < "$DOWN" 2>&1 | grep -E 'NOTICE|ERROR|UPDATE|DELETE' | sed 's/^/    /'
counts

echo
echo "===== ④ 再 up（往返可重放）"
psql -h "$H" -U $U -d $DB -v ON_ERROR_STOP=1 -P pager=off < "$UP" 2>&1 | grep -E 'NOTICE|ERROR' | sed 's/^/    /'
counts
