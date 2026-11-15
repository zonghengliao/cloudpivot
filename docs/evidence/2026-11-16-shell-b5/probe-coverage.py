#!/usr/bin/env python3
"""探针覆盖率核对：哪些路由「一条差异都没有」（= 零观感变化），哪些有差异（逐条已在 probe-attrib 里归因）。"""
import json
import sys

w = sys.argv[1] if len(sys.argv) > 1 else '1600'
b = json.load(open(f'probe-before-{w}.json'))
a = json.load(open(f'probe-after-{w}.json'))
d = json.load(open(f'probe-after-{w}.diff.json'))
touched = {x['route'] for x in d}
routes = list(b['routes'])
zero = [r for r in routes if r not in touched]
els = sum(v['count'] for v in b['routes'].values())
els_a = sum(v['count'] for v in a['routes'].values())
print(f'{w}px：{len(routes)} 路由 / 采样元素 {els}（前）{els_a}（后）；有差异 {len(touched)} 路由，零差异 {len(zero)} 路由')
print('\n零差异（逐元素 40 个计算属性 + 几何全同）：')
for r in zero:
    print('  ' + r)
print('\n有差异：')
for r in sorted(touched):
    kinds = [x for x in d if x['route'] == r]
    geom = [x for x in kinds if x['kind'] in ('GEOM', 'COUNT', 'ELEMENT-MISSING')]
    style = [x for x in kinds if x['kind'] == 'STYLE']
    print(f'  {r}  几何 {len(geom)} / 样式 {len(style)}')
