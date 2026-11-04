#!/usr/bin/env python3
"""按 n08 的解析规则复算：sys_menu(menu_type=2) 的 path 是否都能在 frontend/src/router/index.ts 里找到字面量 path。"""
import subprocess
import sys

ROUTER = '/root/t_539dab1d-tree/frontend/src/router/index.ts'
DB = sys.argv[1] if len(sys.argv) > 1 else 'cloudpivot_0d7c51f8b'

src = open(ROUTER, encoding='utf-8').read()
registered = []
for line in src.splitlines():
    i = line.find("path: '")
    if i < 0:
        continue
    rest = line[i + len("path: '"):]
    j = rest.find("'")
    if j < 0:
        continue
    raw = rest[:j]
    if raw == '' or raw == '/' or raw.startswith(':'):
        continue
    registered.append('/' + raw.lstrip('/'))

q = ("select name||'|'||path from sys_menu where deleted_at is null and menu_type=2 "
     "and path is not null and path<>'' order by path")
out = subprocess.run(['psql', '-h', '127.0.0.1', '-U', 'alun', '-d', DB, '-Atc', q],
                     capture_output=True, text=True, env={'PGPASSWORD': 'alun', 'PATH': '/usr/bin:/bin'})
if out.returncode != 0:
    print('psql failed:', out.stderr)
    sys.exit(1)
rows = [l.split('|', 1) for l in out.stdout.strip().splitlines() if l.strip()]

reg = set(registered)
missing = [(n, p) for n, p in rows if p not in reg]
print(f'router registered paths: {len(registered)}')
print(f'sys_menu menu_type=2 rows: {len(rows)}')
print(f'MISSING: {len(missing)}')
for n, p in missing:
    print(' -', n, p)

print('-- budget 三条 --')
for p in ['/business/erp/budget', '/business/erp/budget/mapping', '/business/erp/budget/report']:
    hit = p in reg
    print(f'{p}: {"OK" if hit else "MISSING"}')
