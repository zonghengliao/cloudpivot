#!/usr/bin/env python3
"""精确 stage B5 的文件（只加自己的：45 个 .vue + shell.less + 报告工具 + allowlist + 决策文档 + 证据目录）。

其它卡在同一工作区里的 WIP 文件（voucher-logic.ts / contract-logic.ts / error-messages.ts …）一律不碰。
"""
import json
import subprocess

REPO = '/opt/cloudpivot'
d = json.load(open(f'{REPO}/docs/evidence/2026-11-16-shell-b5/report-before.json'))
vue = ['frontend/src/' + f for f in sorted({b['file'] for b in d['batches']['B5']['detail']})]
# 已被别人的提交 a0a00530 带走 → 不在本次提交里
swept = {'frontend/src/views/business/erp/voucher/index.vue'}
files = [f for f in vue if f not in swept] + [
    'frontend/src/style/shell.less',
    'frontend/scripts/shell-variant-report.cjs',
    'frontend/scripts/shell-variant-allowlist.json',
    'docs/architecture/shell-样式变体收敛-决策.md',
    'docs/evidence/2026-11-16-shell-b5',
]
r = subprocess.run(['git', 'add', '-A', '--'] + files, cwd=REPO, capture_output=True, text=True)
print(r.stdout, r.stderr)
st = subprocess.run(['git', 'diff', '--cached', '--name-only'], cwd=REPO, capture_output=True, text=True).stdout.split('\n')
st = [s for s in st if s]
print(f'已 stage {len(st)} 个文件')
for s in st:
    print('  ' + s)
