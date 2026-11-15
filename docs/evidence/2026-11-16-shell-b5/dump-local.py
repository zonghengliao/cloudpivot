#!/usr/bin/env python3
"""B5 逐文件逐类本地声明 dump（读 shell-variant-report.cjs --json 的 B5 明细）。"""
import json, sys, collections

path = sys.argv[1] if len(sys.argv) > 1 else 'report-before.json'
d = json.load(open(path))
b5 = d['batches']['B5']['detail']
by = collections.defaultdict(list)
for b in b5:
    by[b['file']].append(b)
print(f'{len(b5)} 块 / {len(by)} 文件')
for f in sorted(by):
    print('###', f)
    for b in by[f]:
        print('   ', b['cls'], '| sel:', b['selector'], '| direct:', b['direct'], '|', json.dumps(b['props'], ensure_ascii=False))
