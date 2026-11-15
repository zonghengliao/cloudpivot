#!/usr/bin/env python3
"""爆炸半径：全 src 里出现 panel-empty / tool-row__label 的文件，标出是否属于 B5 批。"""
import json, subprocess, sys

d = json.load(open('/opt/cloudpivot/docs/evidence/2026-11-16-shell-b5/report-before.json'))
b5 = {b['file'] for b in d['batches']['B5']['detail']}
other = set()
for k, v in d['batches'].items():
    if k == 'B5':
        continue
    for b in v['detail']:
        other.add(b['file'])
root = '/opt/cloudpivot/frontend/src/'
files = subprocess.run(
    ['grep', '-rl', '-E', 'panel-empty|tool-row__label', '--include=*.vue', '--include=*.less', '--include=*.ts', '.'],
    cwd=root, capture_output=True, text=True).stdout.split()
print(f'共 {len(files)} 文件命中')
for f in sorted(files):
    rel = f[2:] if f.startswith('./') else f
    mark = 'B5 ' if rel in b5 else ('B6 ' if rel in other else '!! ')
    print(mark, rel)
