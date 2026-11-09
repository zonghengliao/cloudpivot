#!/usr/bin/env bash
# HOME-5 验收夹具（测试环境）：建部门（主管=admin）+ 给 admin 挂审批角色 + 给 EMPLOYEE 开「发起审批」权限。
set -eu
DB=cloudpivot_home5_v4
Q() { sudo -n -u postgres psql -d "$DB" -t -A -c "$1"; }

ADMIN_ID=$(Q "select id from sys_user where username='admin' and deleted_at is null")
DEPT_ID='e5000000-0000-4000-8000-000000000001'
echo "admin=$ADMIN_ID"

# 1) 部门（主管 = admin）—— EXPENSE 的 INITIATOR_SUPERIOR 需要「直属上级」
sudo -n -u postgres psql -d "$DB" -q -c "
INSERT INTO sys_dept (id, dept_code, dept_name, leader_id, parent_id, dept_path, status, sort_order)
VALUES ('$DEPT_ID', 'H5-DEMO', 'HOME-5 演示部门', '$ADMIN_ID', NULL, '/$DEPT_ID', 1, 90)
ON CONFLICT (id) DO UPDATE SET leader_id = EXCLUDED.leader_id;
"

# 2) admin 挂审批岗（角色解析 → 落到 admin；发起人是另一个人，所以不会撞「自审」）
for ROLE in DEPT_LEADER PURCHASE_MANAGER FINANCE_MANAGER WAREHOUSE_MANAGER GENERAL_MANAGER FINANCE_DIRECTOR SALES_SUPERVISOR; do
  RID=$(Q "select id from sys_role where role_code='$ROLE' and deleted_at is null limit 1")
  [ -n "$RID" ] || continue
  sudo -n -u postgres psql -d "$DB" -q -c "
INSERT INTO sys_user_role (id, user_id, role_id, created_at)
SELECT gen_random_uuid(), '$ADMIN_ID', '$RID', now()
WHERE NOT EXISTS (SELECT 1 FROM sys_user_role WHERE user_id='$ADMIN_ID' AND role_id='$RID');
"
  echo "admin + $ROLE"
done

# 3) EMPLOYEE 角色补「发起审批」菜单 + 按钮（只在本验收库；生产口径不动）
EID=$(Q "select id from sys_role where role_code='EMPLOYEE' and deleted_at is null")
for MENU in 20000000-0000-0000-0000-000000000861 20000000-0000-0000-0000-000000000800; do
  sudo -n -u postgres psql -d "$DB" -q -c "
INSERT INTO sys_role_menu (id, role_id, menu_id, created_at)
SELECT gen_random_uuid(), '$EID', '$MENU', now()
WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu WHERE role_id='$EID' AND menu_id='$MENU');
"
done
echo "EMPLOYEE + flow create menus"

# 4) 发起人账号（EMP，隶属演示部门）—— 走真接口建，避免手写口令哈希
TOKEN=$(cat /var/tmp/home5/api/admin.token)
curl -s -m 15 -X POST http://127.0.0.1:18097/api/system/user \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"username\":\"h5clerk\",\"password\":\"Clerk@2026Pass\",\"real_name\":\"张三（演示发起人）\",\"status\":1,\"dept_id\":\"$DEPT_ID\",\"role_ids\":[\"$EID\"]}" \
  | head -c 200; echo

# 5) clerk 登录 → token
CLOGIN=$(curl -s -m 10 -X POST http://127.0.0.1:18097/api/system/auth/login -H 'Content-Type: application/json' \
  -d '{"username":"h5clerk","password":"Clerk@2026Pass"}')
CTOKEN=$(printf '%s' "$CLOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
[ -n "$CTOKEN" ] || { echo '!! clerk 登录失败'; exit 1; }
echo "$CTOKEN" > /var/tmp/home5/api/clerk.token
curl -s -m 10 -H "Authorization: Bearer $CTOKEN" http://127.0.0.1:18097/api/system/auth/me > /var/tmp/home5/api/clerk-me.json
python3 - /var/tmp/home5/api/clerk-me.json <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))['data']
print('clerk me:', d['username'], d['real_name'], 'dept=', d.get('dept_name'), 'roles=', d['roles'], 'perms=', len(d['permissions']))
PY
