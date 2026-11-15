#!/usr/bin/env python3
"""找出「panel-empty__* 出现在 .panel-empty 容器之外」的情况（删本地裸类规则后会掉样式）。

判据：把 <template> 段按行扫，记录最近一层 `<div class="...panel-empty...">` 的出现行；
若某个 `panel-empty__` 命中行之前最近的 panel-empty 容器已闭合（用简单的 div 深度计数），则告警。
简化实现：对每处 `panel-empty__` 命中，向上回溯 40 行找 `<div` / `<t-...` 开标签，判断其 class 是否含 panel-empty。
"""
import json, re, sys

path = sys.argv[1] if len(sys.argv) > 1 else 'report-before.json'
d = json.load(open(path))
files = sorted({b['file'] for b in d['batches']['B5']['detail']})
root = '/opt/cloudpivot/frontend/src/'
warn = 0
for f in files:
    lines = open(root + f, encoding='utf-8').read().split('\n')
    in_style = False
    tpl = []
    for i, ln in enumerate(lines, 1):
        if '<style' in ln:
            in_style = True
        if '</style>' in ln:
            in_style = False
            continue
        if not in_style:
            tpl.append((i, ln))
    for idx, (lineno, ln) in enumerate(tpl):
        for m in re.finditer(r'panel-empty__(icon|title|desc)', ln):
            sub = m.group(1)
            # 若这一行自己就是 <div class="panel-empty ..."> 则跳过
            slot = ln[max(0, m.start() - 120):m.start()]
            if re.search(r'class="[^"]*panel-empty["\s]', slot) or 'panel-empty--' in slot:
                continue
            # 向上找最近的「开标签行」判断其 class
            found = None
            for k in range(idx - 1, max(-1, idx - 40), -1):
                cand = tpl[k][1]
                if re.search(r'<(div|section|template|t-card|t-drawer)\b', cand):
                    found = (tpl[k][0], cand.strip())
                    break
            ok = found and 'panel-empty' in found[1]
            if not ok:
                warn += 1
                print(f'⚠ {f}:{lineno}  panel-empty__{sub} 最近容器 = {found}')
print(f'\n=== {warn} 处 panel-empty__* 不在 .panel-empty 容器内 ===')
