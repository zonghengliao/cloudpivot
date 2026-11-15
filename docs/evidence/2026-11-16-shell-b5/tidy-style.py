#!/usr/bin/env python3
"""收尾清理：`}</style>` → `}\\n</style>`；删掉因迁走规则而变成孤儿的注释行。

只对 B5 的 46 个文件动，且只做这两类机械替换（改完用 git diff 复核）。
"""
import json
import re
import sys

d = json.load(open('/opt/cloudpivot/docs/evidence/2026-11-16-shell-b5/report-before.json'))
files = sorted({b['file'] for b in d['batches']['B5']['detail']})
fix_style = 0
fix_comment = 0
for f in files:
    p = '/opt/cloudpivot/frontend/src/' + f
    text = open(p, encoding='utf-8').read()
    new = text
    if re.search(r'\S</style>', new):
        new = re.sub(r'([^\n])</style>', r'\1\n</style>', new)
        fix_style += 1
    # 孤儿注释：注释行紧贴 </style>，且注释里提到「空态」「shell 下沉」这类只服务于已迁规则的说明
    m = re.search(r'\n(/\*[^\n]*?(空态|shell)[^\n]*?\*/)\n?</style>', new)
    if m:
        new = new[:m.start()] + '\n</style>' + new[m.end():]
        fix_comment += 1
    if new != text:
        open(p, 'w', encoding='utf-8').write(new)
        print(f'fixed {f}')
print(f'\n收尾：</style> 换行 {fix_style} 处 / 孤儿注释 {fix_comment} 处')
