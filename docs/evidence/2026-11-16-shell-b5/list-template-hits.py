#!/usr/bin/env python3
"""列出 B5 46 个文件里 <template> 段中所有 panel-empty* / tool-row__label 的出现（含行号）。

用法： python3 list-template-hits.py [report.json]
"""
import json, re, sys, collections

path = sys.argv[1] if len(sys.argv) > 1 else 'report-before.json'
d = json.load(open(path))
files = sorted({b['file'] for b in d['batches']['B5']['detail']})
root = '/opt/cloudpivot/frontend/src/'
total = 0
for f in files:
    text = open(root + f, encoding='utf-8').read()
    # 去掉 <style> 段，只留模板
    lines = text.split('\n')
    in_style = False
    hits = []
    for i, ln in enumerate(lines, 1):
        if '<style' in ln:
            in_style = True
        if '</style>' in ln:
            in_style = False
            continue
        if in_style:
            continue
        if re.search(r'panel-empty|tool-row__label', ln):
            hits.append((i, ln.strip()))
    total += len(hits)
    print(f'### {f}  ({len(hits)} 处)')
    for i, ln in hits:
        print(f'  {i}: {ln[:160]}')
print(f'\n=== 合计 {len(files)} 文件 / {total} 处模板命中 ===')
