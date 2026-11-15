#!/usr/bin/env python3
"""核对：别人在我工作期间落的提交（a0a00530）有没有把我的 B5 改动一起带走。

输出：被 sweep 的文件清单 + 每个文件里属于 B5 的那几行。
"""
import json
import subprocess

d = json.load(open('/opt/cloudpivot/docs/evidence/2026-11-16-shell-b5/report-before.json'))
mine = sorted({b['file'] for b in d['batches']['B5']['detail']})


def sh(*args):
    return subprocess.run(args, cwd='/opt/cloudpivot', capture_output=True, text=True).stdout


files = sh('git', 'show', '--name-only', '--format=', 'a0a00530').split()
hit = [f for f in files if f.startswith('frontend/src/views/business/')]
print('该提交里的业务页面文件：', hit)
for f in hit:
    rel = f.replace('frontend/src/', '')
    if rel not in mine:
        print(f'  {rel} 不在 B5 清单里')
        continue
    diff = sh('git', 'show', 'a0a00530', '--', f)
    lines = [l for l in diff.split('\n') if ('panel-empty' in l or 'tool-row__label' in l) and l[:1] in '+-']
    print(f'  {rel} 在 B5 清单里；该提交中与 B5 相关的行 {len(lines)} 条')
    for l in lines[:12]:
        print('     ' + l[:110])
