"""门禁负向自检：把「最新那条写 ui.palette 的迁移」里的最后一个 id 删掉，palette_def_sync.py 必须报 FAIL。

⚠ 不写死迁移文件名：V0397 那条（gallery 77 扩白名单）已改号为 V0399（与 HR-40 的
business/V0397 撞号，见卡 t_83977c43），以后扩白名单还会再换号。所以这里按「system/ 下
版本号最大的那条提到 ui.palette 的迁移」定位 —— 与 palette_def_sync.py 自己的取数口径一致。
"""
import io
import pathlib
import re
import shutil
import subprocess
import sys

NEG = pathlib.Path('/tmp/neg-check')
if NEG.exists():
    shutil.rmtree(NEG)
(NEG / 'frontend/src/style').mkdir(parents=True)
(NEG / 'backend/migrations/system').mkdir(parents=True)
repo = pathlib.Path(__file__).resolve().parents[2]   # scripts/verify/x.py → 仓库根（不再写死 /opt/cloudpivot，worktree 里也能跑）
shutil.copy(repo / 'frontend/src/style/palettes.ts', NEG / 'frontend/src/style/palettes.ts')

VERSION_RE = re.compile(r'V(\d+)__[^/]*\.up\.sql$')
candidates = []
for path in (repo / 'backend/migrations/system').glob('V*.up.sql'):
    match = VERSION_RE.search(str(path))
    if not match:
        continue
    text = io.open(path, encoding='utf-8').read()
    if 'ui.palette' in text and 'sys_preference_def' in text:
        candidates.append((int(match.group(1)), path))
if not candidates:
    print('找不到写 ui.palette 的迁移，自检无法进行')
    sys.exit(1)
candidates.sort()
PALETTE_UP = candidates[-1][1]

for name in ('V0091__seed_preference_def.up.sql', PALETTE_UP.name):
    shutil.copy(repo / 'backend/migrations/system' / name, NEG / 'backend/migrations/system' / name)

gate = repo / 'scripts/lint/palette_def_sync.py'


def run(tag):
    proc = subprocess.run(
        [sys.executable, str(gate), '--root', str(NEG)], capture_output=True, text=True
    )
    print(f'--- {tag} exit={proc.returncode}')
    print((proc.stdout + proc.stderr).strip()[:600])
    return proc.returncode


print(f'被测迁移：{PALETTE_UP.name}')
ok_exit = run('正向（99 套一致）')

target = NEG / 'backend/migrations/system' / PALETTE_UP.name
text = io.open(target, encoding='utf-8').read()
io.open(target, 'w', encoding='utf-8').write(text.replace(',guci-gongming', '', 1))
bad_exit = run('负向（字典少一个 id）')

print('\n结论：', '负向自检有效（正向 0 / 负向 1）' if ok_exit == 0 and bad_exit == 1 else '自检异常')
sys.exit(0 if ok_exit == 0 and bad_exit == 1 else 1)
