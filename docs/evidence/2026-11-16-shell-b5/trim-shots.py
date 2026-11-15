#!/usr/bin/env python3
"""入库前收尾：把「有差异的页」的 before/after 成对拷到 shots/changed-1600/，
然后删掉可复跑的原始大件（before/after/control 三档 viewport 图），只留 diff 图 + 变化页。

依据：B4 的先例（只入库 diff 图 + changed 页），原始图可用 README §9 的命令重跑。
"""
import os
import re
import shutil

EV = '/opt/cloudpivot/docs/evidence/2026-11-16-shell-b5'
CHANGED = [
    'business_erp_contract', 'business_erp_lot', 'business_erp_stock-out', 'business_erp_stock-take',
    'business_hr_attendance', 'business_hr_leave', 'business_hr_employee', 'business_erp_material',
    'business_erp_warehouse', 'business_erp_cost-labor', 'business_erp_price-inquiry',
    'business_hr_salary_bbbbbbbb-0000-0000-0000-000000000001',
]
out = os.path.join(EV, 'shots', 'changed-1600')
os.makedirs(out, exist_ok=True)
n = 0
for base in CHANGED:
    for suffix in ('.png', '.empty.png'):
        for side in ('before', 'after'):
            src = os.path.join(EV, 'shots', f'{side}-1600', base + suffix)
            if os.path.exists(src):
                shutil.copy2(src, os.path.join(out, f'{side}__{base}{suffix}'))
                n += 1
print(f'changed-1600：拷了 {n} 张（12 页 × before/after × viewport/空态裁切）')

# 原始大件：viewport 全套（可复跑）→ 删；diff 图与 changed-1600 保留
for d in ('before-1600', 'after-1600', 'before-1280', 'after-1280', 'control-1600'):
    p = os.path.join(EV, 'shots', d)
    if os.path.isdir(p):
        shutil.rmtree(p)
        print('删除可复跑原件：shots/' + d)
print('剩余：', sorted(os.listdir(os.path.join(EV, 'shots'))))
