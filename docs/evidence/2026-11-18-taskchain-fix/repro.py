#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""复现 + 验收脚本：在**副本库**上重放「同一张 running 卡下 2 张游离 ready 清债卡」形态。

    python3 repro.py                # 用默认 live 库（HERMES_KANBAN_DB 或 ~/.hermes/kanban/...）
    python3 repro.py --db <path> --work <dir>

真库只读：先 `cp` 快照（连 `-wal` / `-shm` 一起，WAL 模式下只 cp 主文件会得到旧快照），
所有写操作都在副本上。四条断言与卡面验收一一对应，任何一条不符就以 rc=1 退出。
"""
import argparse
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / 'scripts/lint/task_chain_valid.py'


def find_live_db() -> Path | None:
    explicit = os.environ.get('HERMES_KANBAN_DB')
    if explicit and Path(explicit).is_file():
        return Path(explicit)
    board = os.environ.get('KANBAN_BOARD', 'dev')
    p = Path.home() / '.hermes' / 'kanban' / 'boards' / board / 'kanban.db'
    return p if p.is_file() else None


def clone(src: Path, dst: Path) -> None:
    for suffix in ('', '-wal', '-shm'):
        s = Path(str(src) + suffix)
        d = Path(str(dst) + suffix)
        if d.exists():
            d.unlink()
        if s.exists():
            shutil.copy2(s, d)


def add_free_cards(db: Path, ids=('t_pt7d2aa63', 't_pa5eca17f')) -> None:
    """克隆任意一张卡的行，清掉它自己的边，改成 ready —— 就是「新立的游离清债卡」。"""
    con = sqlite3.connect(str(db))
    cols = [r[1] for r in con.execute('PRAGMA table_info(tasks)')]
    tpl = con.execute('SELECT * FROM tasks WHERE status=? LIMIT 1', ('done',)).fetchone()
    row = dict(zip(cols, tpl))
    with con:
        for i, nid in enumerate(ids, 1):
            r = dict(row)
            r['id'] = nid
            r['title'] = f'【测试卫生·门禁】探针游离清债卡{i}（副本库占位）'
            r['status'] = 'ready'
            con.execute(
                f"INSERT INTO tasks({','.join(r)}) VALUES({','.join('?' for _ in r)})",
                list(r.values()),
            )
            con.execute('DELETE FROM task_links WHERE child_id=? OR parent_id=?', (nid, nid))
    con.close()


def links(db: Path) -> int:
    con = sqlite3.connect(str(db))
    n = con.execute('SELECT count(*) FROM task_links').fetchone()[0]
    con.close()
    return n


def run(db: Path, *args) -> tuple[int, str]:
    env = dict(os.environ, HERMES_KANBAN_DB=str(db))
    r = subprocess.run(
        [sys.executable, str(SCRIPT), *args], capture_output=True, text=True, env=env
    )
    return r.returncode, r.stdout + r.stderr


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--db', default=None)
    ap.add_argument('--work', default=None)
    a = ap.parse_args()

    live = Path(a.db) if a.db else find_live_db()
    if not live or not live.is_file():
        print('找不到 live kanban DB，用 --db 指定', file=sys.stderr)
        return 2
    work = Path(a.work) if a.work else Path(tempfile.mkdtemp(prefix='chainfix-'))
    work.mkdir(parents=True, exist_ok=True)
    print(f'live（只读）: {live}\n副本目录  : {work}')

    dry, fix = work / 'r_dry.db', work / 'r_fix.db'
    for d in (dry, fix):
        clone(live, d)
        add_free_cards(d)

    ok = True

    def check(cond, msg):
        nonlocal ok
        ok = ok and bool(cond)
        print(f"  {'✓' if cond else '✗'} {msg}")

    n0 = links(dry)

    def num(pattern: str, text: str, default: int = -1) -> int:
        mm = re.search(pattern, text)
        return int(mm.group(1)) if mm else default

    rc1, out1 = run(dry, '--fix', '--dry-run', '--verbose')
    moved = num(r'共 (\d+) 处调整', out1)
    check(rc1 == 1 and moved == 2, f'① 干跑收敛：共 {moved} 处调整（期望 2，旧代码 200）')
    check(links(dry) == n0, f'① 干跑不写库：task_links 仍 {links(dry)}（开跑前 {n0}）')
    per = [len(re.findall(rf'· {cid} ', out1)) for cid in ('t_pt7d2aa63', 't_pa5eca17f')]
    check(per == [1, 1], f'① 每条卡只出现一次（实测 {per}，旧代码同一张 198 次）')

    rc2, out2 = run(fix, '--fix', '--verbose')
    check(rc2 == 0, f'② 真跑 rc={rc2}（期望 0）')
    before = num(r'主链 (\d+) 张', out2)
    rc3, out3 = run(fix, '--verbose')
    after = num(r'主链 (\d+) 张', out3)
    check(rc3 == 0 and '任务链结构完整' in out3, f'③ 修后检查 rc={rc3}、结构完整')
    check(after >= before + 2, f'③ 主链张数不减少：{before} → {after}（期望 ≥ {before + 2}）')

    rc4, out4 = run(fix, '--selftest')
    check(rc4 == 0, f'④ --selftest rc={rc4}')
    print('  结论：' + ('全部符合卡面验收' if ok else '有不符项，见上面 ✗'))
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())
