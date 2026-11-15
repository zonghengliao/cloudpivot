#!/usr/bin/env python3
"""探针 JSON 小工具：打印某路由的元素清单 / 页面错误。"""
import json
import sys

path = sys.argv[1]
route = sys.argv[2] if len(sys.argv) > 2 else None
d = json.load(open(path))
for r, v in d['routes'].items():
    if route and route not in r:
        continue
    print(f'### {r}  元素 {v["count"]}  错误 {len(v["errors"])}')
    for e in v['errors'][:4]:
        print('   err:', e)
    for e in v['elements']:
        print(f'   {e["tag"]}.{e["cls"][:46]:46s} rect={e["rect"]} font={e["style"]["font-size"]}/{e["style"]["font-weight"]}/{e["style"]["color"]} pad={e["style"]["padding-top"]},{e["style"]["padding-bottom"]} disp={e["style"]["display"]} lines={e.get("lines")} own={e["hasOwnText"]} txt={e["text"][:24]!r}')
