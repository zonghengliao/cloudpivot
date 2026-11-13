#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Kanban 卡编号唯一性检查 + 下一个可用编号。

用法：
  # 检查（有重复则退出码 1）
  python3 scripts/verify/task_code_unique.py

  # 只看某个前缀的下一个可用编号
  python3 scripts/verify/task_code_unique.py --next HR

  # 建卡前预检（编号已存在则拒绝）
  python3 scripts/verify/task_code_unique.py --claim HR-54

规则：
  · 严格的编号 = 标题开头的 `前缀-数字`，如 `HR-34`、`COST-B2-15`、`VTRACE-7`
  · 带明示后缀的（`HR-06-FU`、`HR-34-B`）视为「后续卡」，不与其主卡冲突
  · `-FU` / `-B` / `-补` 这类后缀不参与唯一性判定，但会单独列出
"""
from __future__ import annotations

import argparse
import re
import sqlite3
import sys
from collections import defaultdict
from pathlib import Path

DB = Path("/root/.hermes/kanban/boards/dev/kanban.db")

# 编号：前缀（大写字母，可带 -B2 这种段）+ 数字
CODE_RE = re.compile(r"^([A-Z][A-Z0-9]*(?:-[A-Z0-9]+)*-\d+)")
# 后缀：编号后面紧跟的 -XXX
SUFFIX_RE = re.compile(r"^([A-Z][A-Z0-9]*(?:-[A-Z0-9]+)*-\d+)(-[A-Z][A-Z0-9]*)")


def load_tasks():
    if not DB.exists():
        sys.exit(f"kanban.db 不存在: {DB}")
    c = sqlite3.connect(DB)
    rows = c.execute("SELECT id, title, status FROM tasks").fetchall()
    c.close()
    return rows


def analyze(rows):
    """返回 (主编号 -> [(id,title,status)], 后缀卡 [(code, id, title)], 无编号)"""
    by_code = defaultdict(list)
    suffixed = []
    no_code = []

    for tid, title, status in rows:
        t = (title or "").strip()
        m_suf = SUFFIX_RE.match(t)
        if m_suf:
            suffixed.append((m_suf.group(1), m_suf.group(2), tid, t, status))
            # 后缀卡归到主编号（不算重复）
            by_code[m_suf.group(1)].append((tid, t, status))
            continue
        m = CODE_RE.match(t)
        if m:
            by_code[m.group(1)].append((tid, t, status))
        else:
            no_code.append((tid, t, status))

    return by_code, suffixed, no_code


def parse_prefix(code: str) -> tuple[str, int] | None:
    """'HR-34' → ('HR', 34)"""
    m = re.match(r"^(.+?)-(\d+)$", code)
    if not m:
        return None
    return m.group(1), int(m.group(2))


def cmd_check() -> int:
    rows = load_tasks()
    by_code, suffixed, no_code = analyze(rows)

    dups = {}
    for code, items in by_code.items():
        # 同一主编号下有多张“非后缀卡”才算重复
        plain = [x for x in items if not SUFFIX_RE.match(x[1].strip())]
        if len(plain) > 1:
            dups[code] = plain

    print("=" * 64)
    print("Kanban 卡编号唯一性检查")
    print("=" * 64)
    print(f"卡总数        {len(rows)}")
    print(f"主编号数      {len(by_code)}")
    print(f"后缀卡（-FU/-B 等） {len(suffixed)}")
    print(f"无编号卡      {len(no_code)}")
    print()

    if dups:
        print(f"❌ 重复编号 {len(dups)} 个：")
        for code, items in sorted(dups.items()):
            print(f"\n  {code}:")
            for tid, title, status in items:
                print(f"    {tid}  [{status}]  {title[:60]}")
        return 1

    print("✅ 无重复编号")

    if suffixed:
        print("\n后缀卡（不参与唯一性判定）：")
        for code, suffix, tid, title, status in suffixed:
            print(f"  {code}{suffix}  {tid}  [{status}]  {title[:50]}")

    if no_code:
        print(f"\n无编号卡 {len(no_code)} 张（不影响唯一性）：")
        for tid, title, status in no_code[:5]:
            print(f"  {tid}  [{status}]  {title[:50]}")
        if len(no_code) > 5:
            print(f"  ... 还有 {len(no_code) - 5} 张")

    return 0


def cmd_next(prefix: str) -> int:
    rows = load_tasks()
    by_code, _, _ = analyze(rows)
    nums = []
    for code in by_code:
        p = parse_prefix(code)
        if p and p[0].upper() == prefix.upper():
            nums.append(p[1])
    nxt = max(nums) + 1 if nums else 1
    print(f"{prefix.upper()}-{nxt}")
    return 0


def cmd_claim(code: str) -> int:
    rows = load_tasks()
    by_code, _, _ = analyze(rows)
    target = code.strip().upper()
    if target in by_code:
        print(f"❌ {target} 已被占用：")
        for tid, title, status in by_code[target]:
            print(f"  {tid}  [{status}]  {title[:60]}")
        return 1
    print(f"✅ {target} 可用")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="Kanban 卡编号唯一性检查")
    ap.add_argument("--next", metavar="PREFIX", help="打印该前缀的下一个可用编号")
    ap.add_argument("--claim", metavar="CODE", help="检查某编号是否已被占用")
    args = ap.parse_args()

    if args.next:
        return cmd_next(args.next)
    if args.claim:
        return cmd_claim(args.claim)
    return cmd_check()


if __name__ == "__main__":
    sys.exit(main())
