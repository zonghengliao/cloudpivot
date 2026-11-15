#!/usr/bin/env python3
"""把 B5 的改动导出成 patch（只含我的文件），用于在干净 worktree 里跑门禁。"""
import json
import subprocess

d = json.load(open('/opt/cloudpivot/docs/evidence/2026-11-16-shell-b5/report-before.json'))
vue = ['frontend/src/' + f for f in sorted({b['file'] for b in d['batches']['B5']['detail']})]
# erp/voucher 的那部分已被别人的提交 a0a00530 带走 → 不再在 patch 里（否则 apply 冲突）
extra = [
    'frontend/src/style/shell.less',
    'frontend/scripts/shell-variant-report.cjs',
    'frontend/scripts/shell-variant-allowlist.json',
    'docs/architecture/shell-样式变体收敛-决策.md',
]
files = [f for f in vue + extra]
r = subprocess.run(['git', 'diff', 'HEAD', '--'] + files, cwd='/opt/cloudpivot', capture_output=True, text=True)
open('/tmp/b5.patch', 'w').write(r.stdout)
print(f'patch {len(r.stdout.splitlines())} 行 / {len(files)} 个文件路径（含未变化的不会出现在 patch 里）')
files_in_patch = [l.split(' b/')[-1] for l in r.stdout.splitlines() if l.startswith('diff --git')]
print('实际变更文件 %d：' % len(files_in_patch))
for f in files_in_patch:
    print('  ' + f)
