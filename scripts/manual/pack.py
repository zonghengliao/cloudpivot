import os, zipfile

src = '/opt/cloudpivot/docs/manual'
out = os.path.expanduser('~/.hermes/profiles/document-writer/cache/scratch/云枢使用手册-核心业务9功能.zip')
n = 0
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as z:
    for root, dirs, files in os.walk(src):
        if 'evidence' in root:
            continue
        for f in files:
            p = os.path.join(root, f)
            z.write(p, os.path.relpath(p, os.path.dirname(src)))
            n += 1
print('packed', n, 'files ->', out, os.path.getsize(out), 'bytes')
