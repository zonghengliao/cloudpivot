#!/usr/bin/env python3
"""P1-FIX-10 步骤 1b：把隔离库的权限矩阵补成与生产一致（FINANCE_MANAGER 持有凭证/期间码），
并建一个 FINANCE_MANAGER 成员，让「处理人」能解析出来（生产同款：财务经理 2 个成员）。

注意：这是**测试夹具**（隔离库），不是产品代码；生产库这些授予已经存在（实测见证据）。
"""
import json
import os
import subprocess
import sys

sys.path.insert(0, '/root/r10-run')
from apilib import Api

OUT = []


def log(title, payload):
    line = f"### {title}\n{json.dumps(payload, ensure_ascii=False, indent=2) if not isinstance(payload, str) else payload}"
    print(line)
    OUT.append(line)


def psql(sql):
    res = subprocess.run(['psql', 'postgres://alun:alun@127.0.0.1:5432/cloudpivot_r10', '-Atc', sql],
                         capture_output=True, text=True)
    return (res.stdout.strip() or res.stderr.strip())


grant_sql = """
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  JOIN sys_menu m ON m.deleted_at IS NULL
 WHERE r.role_code = 'FINANCE_MANAGER' AND r.deleted_at IS NULL
   AND m.auth_code IN ('business:erp:voucher:create', 'business:erp:voucher:post',
                       'business:erp:period:close', 'business:erp:period:remind',
                       'business:erp:period:open')
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id);
"""
log('1) 补权限矩阵（隔离库夹具）', {'psql': psql(grant_sql) or 'OK'})
log('2) 复核授予', psql(
    "select m.auth_code||' -> '||r.role_code from sys_menu m join sys_role_menu rm on rm.menu_id=m.id "
    "join sys_role r on r.id=rm.role_id where r.role_code='FINANCE_MANAGER' and m.auth_code like "
    "'business:erp:period:%' or (r.role_code='FINANCE_MANAGER' and m.auth_code in "
    "('business:erp:voucher:create','business:erp:voucher:post')) order by 1"))

a = Api()
a.login('admin', os.environ['R10_ADMIN_PASSWORD'])

roles = a.ok('GET', '/system/role/list')
rows = roles if isinstance(roles, list) else (roles.get('list') or roles.get('rows') or [])
fm = [x for x in rows if x.get('role_code') == 'FINANCE_MANAGER']
log('3) FINANCE_MANAGER 角色', fm)

created = a.call('POST', '/system/user', {
    'username': 'fin1',
    'password': 'Fin1-Pass.2026',
    'real_name': '财务经理一号',
    'status': 1,
    'role_ids': [fm[0]['id']] if fm else [],
})
log('4) 建 FINANCE_MANAGER 成员 fin1', created)

log('5) 成员数', psql(
    "select r.role_code||'='||count(ur.id) from sys_role r left join sys_user_role ur on ur.role_id=r.id "
    "where r.role_code='FINANCE_MANAGER' group by 1"))

with open('/root/r10-run/step1b.txt', 'w') as fh:
    fh.write('\n'.join(OUT) + '\n')
