import re, os, glob

missing = []
total_refs = 0
for f in sorted(glob.glob('docs/manual/*.md')):
    for m in re.findall(r'!\[[^\]]*\]\(([^)]+)\)', open(f, encoding='utf-8').read()):
        total_refs += 1
        p = os.path.join('docs/manual', m)
        if not os.path.exists(p):
            missing.append((f, m))
print('图片引用总数:', total_refs)
print('缺失图片:', missing if missing else '无')
print('图片文件总数:', len(glob.glob('docs/manual/images/**/*.png', recursive=True)))
print('未被引用的图片:')
refs = set()
for f in glob.glob('docs/manual/*.md'):
    refs |= set(os.path.normpath(m) for m in re.findall(r'!\[[^\]]*\]\(([^)]+)\)', open(f, encoding='utf-8').read()))
for p in sorted(glob.glob('docs/manual/images/**/*.png', recursive=True)):
    rel = os.path.normpath(os.path.relpath(p, 'docs/manual'))
    if rel not in refs:
        print(' ', rel)
