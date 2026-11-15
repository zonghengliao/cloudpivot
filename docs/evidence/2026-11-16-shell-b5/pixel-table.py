#!/usr/bin/env python3
"""像素比对结果 → README 用的逐页表（含归因列）。

原因码：
  C1 §3.3-1/2  doc 族 padding 36/32/28→40、icon 28/32→40
  C2 §3.3-3     rich 族 desc max-width 420→460
  C3 §3.3-4     rich 族 desc line-height 20px→1.6（hr/employee）
  C4 §3.3-9     label 12→13px（cost-labor）
  C5 §3.3-10/12 doc 族 title 加粗 600 / 色 placeholder→primary
  C6 §3.3-11/13 doc 族 desc 归一到 12px/placeholder、lot 的 UA 边距归零
  C7 §3.3-14    price-inquiry 首次吃到基线（无样式→标准空态）
  C8 §3.3-15    hr/salary/detail 逐元素归族（inline 3 + doc 1）
  C9 派生       空态块变高 → 其下方内容整体下移（含分页器/卡片间距）
  N  噪声       与对照组同量级（≤ 4px 计数 / maxΔ ≤ 22 / 全在 y≤17）
"""
import re
import sys
import collections

CAUSE = {
    'business_erp_contract': 'C1 C5 C6 C9',
    'business_erp_lot': 'C1 C5 C6 C9',
    'business_erp_stock-out': 'C1 C5 C9',
    'business_erp_stock-take': 'C5',
    'business_hr_attendance': 'C1 C5 C6 C9',
    'business_hr_leave': 'C1 C6 C9',
    'business_hr_employee': 'C3 C9',
    'business_erp_material': 'C2',
    'business_erp_warehouse': 'C2',
    'business_erp_cost-labor': 'C4',
    'business_erp_price-inquiry': 'C7',
    'business_hr_salary_bbbbbbbb-0000-0000-0000-000000000001': 'C8 C9',
}


def parse(path):
    rows = {}
    for line in open(path, encoding='utf-8'):
        m = re.match(r'^(\S+\.png)\s+(.*)$', line.strip())
        if not m:
            continue
        name, rest = m.group(1), m.group(2)
        if '尺寸不同' in rest:
            rows[name] = {'size': rest.replace('✗ 尺寸不同 ', '')}
            continue
        m2 = re.match(r'差异像素 (\d+)（mask 内）\s+有意义 (\d+)(?:\s+maxΔ=(\d+)\s+bbox=(\S+)\s+y 区间=(\S+))?', rest)
        if m2:
            rows[name] = {'n': int(m2.group(1)), 'sig': int(m2.group(2)), 'maxd': int(m2.group(3) or 0), 'bbox': m2.group(4), 'bands': m2.group(5)}
    return rows


for w in ('1600', '1280'):
    rows = parse(f'logs/pixels-{w}.log')
    print(f'\n### {w}px（{len(rows)} 张图）\n')
    print('| 图 | 差异像素 | 有意义 | maxΔ | 归因 |')
    print('|---|---|---|---|---|')
    for name in sorted(rows):
        r = rows[name]
        base = name.replace('.png', '').replace('.empty', '')
        if 'size' in r:
            print(f'| `{name}` | 尺寸不同 {r["size"]} | — | — | 空态块高度变化（裁切高度随之变） |')
            continue
        if r['n'] == 0:
            continue
        cause = CAUSE.get(base, 'N')
        print(f"| `{name}` | {r['n']} | {r['sig']} | {r['maxd']} | {cause} |")
    zero = [n for n, r in rows.items() if r.get('n') == 0]
    print(f'\n零差异图 {len(zero)} 张：' + '、'.join('`' + z + '`' for z in sorted(zero)))
