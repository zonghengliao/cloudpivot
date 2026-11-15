#!/usr/bin/env python3
"""探针前后 diff 逐页归因表（B5）：把 *.diff.json 按路由汇总，几何与样式分开列。

用法： python3 probe-attrib.py probe-after-1600.diff.json [--geom-only]
"""
import json
import sys
import collections

path = sys.argv[1]
geom_only = '--geom-only' in sys.argv
diff = json.load(open(path))
by = collections.defaultdict(list)
for d in diff:
    by[d['route']].append(d)
GEOM = {'GEOM', 'COUNT', 'ELEMENT-MISSING', 'CLASS'}
print(f'共 {len(diff)} 条差异 / {len(by)} 条路由\n')
for route in sorted(by):
    items = by[route]
    g = [d for d in items if d['kind'] in GEOM and d['kind'] != 'CLASS']
    s = [d for d in items if d['kind'] == 'STYLE']
    cls_changes = [d for d in items if d['kind'] == 'CLASS']
    other = [d for d in items if d['kind'] in ('COUNT', 'ELEMENT-MISSING')]
    print(f'### {route}   几何 {len(g)} / 样式 {len(s)} / class 属性 {len(cls_changes)} 条')
    for d in other:
        print(f'    !! {d}')
    if not geom_only:
        # 按 元素 class + prop 分组，值打印一次
        agg = collections.defaultdict(list)
        for d in g + s:
            agg[(d.get('cls', ''), d.get('prop', d['kind']))].append((d.get('before'), d.get('after')))
        for (cls, prop), vals in sorted(agg.items()):
            uniq = sorted({(v[0], v[1]) for v in vals})
            tag = 'GEOM' if prop in ('x', 'y', 'w', 'h') else 'STYLE'
            print(f'    {tag:5s} {cls:44s} {prop:14s} ×{len(vals):2d}  ' + '; '.join(f'{a} → {b}' for a, b in uniq))
