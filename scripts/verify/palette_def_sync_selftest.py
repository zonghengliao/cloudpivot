"""门禁负向自检：把 V0397 里的最后一个 id 删掉，palette_def_sync.py 必须报 FAIL。"""
import io
import pathlib
import shutil
import subprocess
import sys

NEG = pathlib.Path('/tmp/neg-check')
if NEG.exists():
    shutil.rmtree(NEG)
(NEG / 'frontend/src/style').mkdir(parents=True)
(NEG / 'backend/migrations/system').mkdir(parents=True)
repo = pathlib.Path('/opt/cloudpivot')
shutil.copy(repo / 'frontend/src/style/palettes.ts', NEG / 'frontend/src/style/palettes.ts')
for name in ('V0091__seed_preference_def.up.sql', 'V0397__preference_palette_ids_gallery77.up.sql'):
    shutil.copy(repo / 'backend/migrations/system' / name, NEG / 'backend/migrations/system' / name)

gate = repo / 'scripts/lint/palette_def_sync.py'


def run(tag):
    proc = subprocess.run(
        [sys.executable, str(gate), '--root', str(NEG)], capture_output=True, text=True
    )
    print(f'--- {tag} exit={proc.returncode}')
    print((proc.stdout + proc.stderr).strip()[:600])
    return proc.returncode


ok_exit = run('正向（99 套一致）')

target = NEG / 'backend/migrations/system/V0397__preference_palette_ids_gallery77.up.sql'
text = io.open(target, encoding='utf-8').read()
io.open(target, 'w', encoding='utf-8').write(text.replace(',guci-gongming', '', 1))
bad_exit = run('负向（字典少一个 id）')

print('\n结论：', '负向自检有效（正向 0 / 负向 1）' if ok_exit == 0 and bad_exit == 1 else '自检异常')
sys.exit(0 if ok_exit == 0 and bad_exit == 1 else 1)
