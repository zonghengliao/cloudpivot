#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""门禁：kanban 卡的 assignee 必须是**真实存在的 Hermes profile**。

为什么需要它（一次真实事故）
--------------------------
2026-11-13 建 P1 批次卡时，5 张运维卡的 `assignee` 写成了 `ops-developer`。
舰队里**没有**这个 profile（真实的是 backend-developer / frontend-developer /
test-enginner / manong / architect ...）。dispatcher 拿到卡 → 找不到对应
worker → **静默跳过**（不报错、不写日志、卡状态保持 ready）。

于是 P1-09 从 11-13 17:08 一直躺在 ready，直到 11-14 15:20 人工发现
才被拉起 —— **21 小时空转**，而且是"看起来一切正常"的空转。

判据
----
对 kanban DB 里 `todo` / `ready` / `running` 的每张卡：

  * assignee 为空或 NULL          → 跳过（继承 `kanban.default_assignee`）
  * assignee 非空                 → 必须存在目录
                                     `$HERMES_HOME/profiles/<assignee>`
                                     或 `$HOME/.hermes/profiles/<assignee>`

只校验**未完成**的卡。已完成/归档的卡是历史，不追溯。

DB 位置（按顺序找，都没有则**跳过**）
----------------------------------
  1. `$HERMES_KANBAN_DB`（显式指定，测试用）
  2. `$HOME/.hermes/kanban/boards/<board>/kanban.db`
     （`--board` 指定，默认 `dev`）

**为什么缺失就跳过**：145（测试机）/144（生产机）/CI 上都没有 kanban 库，
强行要求会把这些环境的 `check.sh` 全打红。这个门禁的适用范围就是
"有 kanban 的开发机"。

用法
----
    python3 scripts/lint/assignee_valid.py            # 扫默认 board=dev
    python3 scripts/lint/assignee_valid.py --board dev
    python3 scripts/lint/assignee_valid.py --list-profiles   # 打印舰队 profile
    python3 scripts/lint/assignee_valid.py --root .          # 兼容 check.sh 传参
"""

import argparse
import os
import sqlite3
import sys
from pathlib import Path

# 未完成状态（只校验这些）
OPEN_STATUSES = ("todo", "ready", "running", "blocked")


def hermes_roots() -> list[Path]:
    """按可能性排序的「$HERMES_HOME 候选」。

    注意：在某个 profile 的会话里，`HERMES_HOME` 可能指向
    `/root/.hermes/profiles/laoliu`（profile 目录本身），而不是
    `/root/.hermes`。所以要先判断 `profiles/` 子目录在不在。
    """
    out = []
    env = os.environ.get("HERMES_HOME")
    if env:
        p = Path(env)
        out.append(p)
        # 若 env 本身是 profiles/<name>，往上两级才是根
        if p.parent.name == "profiles":
            out.append(p.parent.parent)
    out.append(Path.home() / ".hermes")
    out.append(Path("/root/.hermes"))
    # 去重保序
    seen = set()
    uniq = []
    for p in out:
        if p not in seen:
            seen.add(p)
            uniq.append(p)
    return uniq


def hermes_home() -> Path:
    """返回**含 profiles/ 子目录**的那个 HERMES_HOME。"""
    for p in hermes_roots():
        if (p / "profiles").is_dir():
            return p
    return hermes_roots()[0]


def candidates_for_db(board: str) -> list[Path]:
    """按优先级返回候选 DB 路径。

    `$HERMES_KANBAN_DB` 是**显式指定**（测试/特殊环境用）：
    设了它就只认它，不存在也不回退 —— 否则负向测试会静默落到真实库上，
    得到一个"假绿"（这正是本门禁要防的那类静默跳过）。
    """
    if os.environ.get("HERMES_KANBAN_DB"):
        return [Path(os.environ["HERMES_KANBAN_DB"])]
    return [hermes_home() / "kanban" / "boards" / board / "kanban.db"]


def find_db(board: str) -> Path | None:
    for p in candidates_for_db(board):
        if p.is_file():
            return p
    return None


def real_profiles() -> set[str]:
    """舰队里真实存在的 profile 名。"""
    root = hermes_home() / "profiles"
    if not root.is_dir():
        return set()
    return {p.name for p in root.iterdir() if p.is_dir() and not p.name.startswith("_")}


def main() -> int:
    ap = argparse.ArgumentParser(description="kanban assignee 必须是真实 profile")
    ap.add_argument("--board", default="dev")
    ap.add_argument("--root", default=None, help="兼容 check.sh 的 --root，仅用于定位，不改变逻辑")
    ap.add_argument("--list-profiles", action="store_true")
    args = ap.parse_args()

    profiles = real_profiles()
    if args.list_profiles:
        print("舰队列真实 profile（%d 个）：" % len(profiles))
        for p in sorted(profiles):
            print("  " + p)
        return 0

    if not profiles:
        print("  ⚠ 找不到 %s/profiles，跳过" % hermes_home())
        return 0

    db = find_db(args.board)
    if db is None:
        # 145/144/CI 没有 kanban，正常跳过
        print("  · 无 kanban DB（board=%s），跳过 assignee 校验" % args.board)
        for c in candidates_for_db(args.board):
            print("    尝试过: %s" % c)
        return 0

    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        placeholders = ",".join("?" * len(OPEN_STATUSES))
        rows = conn.execute(
            f"""SELECT id, status, COALESCE(assignee,''), COALESCE(title,'')
                FROM tasks
                WHERE status IN ({placeholders})
                ORDER BY status, id""",
            OPEN_STATUSES,
        ).fetchall()
    except sqlite3.Error as e:
        print("  ⚠ 读 %s 失败：%s（跳过）" % (db, e))
        return 0
    finally:
        conn.close()

    bad = []
    checked = 0
    for tid, status, assignee, title in rows:
        if not assignee.strip():
            continue  # 继承 default_assignee
        checked += 1
        if assignee not in profiles:
            bad.append((tid, status, assignee, title))

    print("  kanban: %s" % db)
    print("  舰队 profile: %d 个 | 待校验卡: %d 张 | 未完成总数: %d" % (len(profiles), checked, len(rows)))

    if not bad:
        print("  ✓ 全部 assignee 有效")
        return 0

    print("", file=sys.stderr)
    print("  ✗ 有 %d 张卡的 assignee 不是真实 profile：" % len(bad), file=sys.stderr)
    for tid, status, assignee, title in bad:
        print("      %-12s %-8s assignee=%-22s %s" % (tid, status, assignee, title[:44]), file=sys.stderr)
    print("", file=sys.stderr)
    print("    修法：改成舰队里真实存在的 profile（可用 --list-profiles 查看），", file=sys.stderr)
    print("    或用 `hermes kanban --board %s reassign <id> <profile>`。" % args.board, file=sys.stderr)
    print("    注意：dispatcher 对不存在的 assignee 是**静默跳过**，卡会永远躺在 ready。", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
