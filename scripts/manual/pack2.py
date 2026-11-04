"""打包《使用手册（二）》交付物：13 篇手册 + p2b 截图 + 样例附件 + 两轮采集日志。

用法：python3 scripts/manual/pack2.py [输出路径]
默认输出：~/.hermes/profiles/document-writer/cache/scratch/云枢使用手册-财务8平台5.zip
"""
import os
import sys
import zipfile

SRC = '/opt/cloudpivot/docs/manual'
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser(
    '~/.hermes/profiles/document-writer/cache/scratch/云枢使用手册-财务8平台5.zip'
)

manuals = [f for f in sorted(os.listdir(SRC)) if f[:2].isdigit() and f.startswith(('1', '2')) and f.endswith('.md')]
entries = [(os.path.join(SRC, m), m) for m in manuals]
entries.append((os.path.join(SRC, 'README.md'), 'README.md'))
for root, _dirs, files in os.walk(os.path.join(SRC, 'images', 'p2b')):
    for f in sorted(files):
        p = os.path.join(root, f)
        entries.append((p, os.path.relpath(p, SRC)))
for f in sorted(os.listdir(os.path.join(SRC, 'assets'))):
    p = os.path.join(SRC, 'assets', f)
    entries.append((p, os.path.relpath(p, SRC)))
for f in ('p2b-capture-run1.log.txt', 'p2b-capture-run2.log.txt'):
    p = os.path.join(SRC, 'evidence', f)
    entries.append((p, os.path.relpath(p, SRC)))

os.makedirs(os.path.dirname(OUT), exist_ok=True)
with zipfile.ZipFile(OUT, 'w', zipfile.ZIP_DEFLATED) as z:
    for p, arc in entries:
        z.write(p, arc)

print('manuals:', len(manuals))
print('entries:', len(entries))
print('size:', os.path.getsize(OUT), 'bytes ->', OUT)
