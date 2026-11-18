#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""任务链完整性门禁 + 自动修复。

为什么需要它
------------
调度器按优先级从 ready 队列里挑一张卡跑。**一个父卡挂 2 个子卡时，两个子卡
同时变 ready**，优先级高的先跑，跑完它的后继又变 ready —— 另一个儿子永远排在
队尾，不是卡住了，是**再也不会被选中**。

这个坑在 2026-11-18 一天里出现了两次：

  · 第一次：M3-26 挂了 M3-27 和 M4-01 → M4 整批跑完 15 张，M3-27 一直躺 ready
  · 第二次：t_85e005d9 挂了 2 个新子卡（菜单/ERP 测试卫生），t_7938e24a 挂了
    1 个（报表）→ 3 张卡都躺在 ready，而且它们**自身没有子卡**，跑完也不解锁
    任何东西

根因不是"链脚本写错了"，是 **worker 建卡时把新卡挂在了"自己刚跑完的那张卡"
下面**。那张卡已经 done，子卡立刻 ready，于是每建一张就多一个分叉。

所以光靠人发现"链好像有点乱"不够 —— 要有门禁，而且门禁要能**自己算出
新卡该插在哪**，不用人再手工调。

五类问题
--------
  C1  未完成父卡 → 子卡 > 1            并行分叉（两个子卡抢同一个 ready 位置）
  C2  已完成父卡 → 未完成子卡 > 1      跑完之后又新挂的（本次的真实场景）
  C3  未完成卡无父（链头除外）          游离，会被重算就绪直接提升
  C4  未完成卡无子（链尾除外）          死路，跑完不解锁任何东西
  C5  环路                              互相等待，永远不动

插入点判断（--fix 用）
----------------------
不是所有新卡都该丢到链尾。判据按优先级：

  ① **有批次编号**（`M5-13`、`P2-07`、`O-12`）→ 插到同批次内**按编号排序**
     的位置（M5-12 之后、M5-14 之前）。因为编号本身就是顺序契约。

  ② **类型是"清债"类**（`【测试卫生】`、`【修复】`、`FIX-`、`【闸】`、
     `【性能】`、`【安全】`）→ 插到**当前 running 卡之后**。这类卡修的是
     存量红，越早清越好 —— 拖在链尾会让中间每一张卡的全量测试都红。

  ③ **其余**（无编号、无类型）→ 插到**链尾**（最后一个未完成卡之前），
     不打断任何已有批次。

为什么不是"无脑插链尾"：测试卫生卡拖到链尾，等于让 M5/M6/M7/O 几十张卡
都在红的测试基线上跑，发现问题时已经晚了。
为什么不是"无脑插 running 之后"：有编号的卡会打乱批次内顺序，后续卡依赖
前面的表结构。

用法
----
    python3 scripts/lint/task_chain_valid.py              # 只检查（门禁模式）
    python3 scripts/lint/task_chain_valid.py --verbose    # 打印主链和建议位置
    python3 scripts/lint/task_chain_valid.py --fix        # 自动修复（会先备份 DB）
    python3 scripts/lint/task_chain_valid.py --fix --dry-run   # 只看会怎么改

退出码：0 = 无问题（或已修好）；1 = 有问题（未修复时）；2 = 无法执行

环境变量：
    HERMES_KANBAN_DB   显式指定 DB（不给则按默认位置找）
    KANBAN_BOARD       板名（默认 dev）
"""

import argparse
import os
import re
import shutil
import sqlite3
import sys
from datetime import datetime
from pathlib import Path

# ---------------------------------------------------------------------------
# 常量
# ---------------------------------------------------------------------------

OPEN_STATUS = ("todo", "ready", "running", "blocked")
CLOSED_STATUS = ("done", "archived")

# 链尾卡：没有子卡是正常的
CHAIN_TAIL_PATTERN = re.compile(r"^O-\d+|O-64")

# 批次编号：M5-13 / P2-07 / O-12 / M4-11b
BATCH_RE = re.compile(r"^(?P<batch>[A-Z]+\d*)-(?P<num>\d+)(?P<suffix>[a-z]?)\b")

# "清债"类卡：越早清越好
DEBT_KEYWORDS = (
    "【测试卫生",
    "【修复",
    "【闸",
    "【性能",
    "【安全",
    "FIX-",
    "FIX：",
    "修复",
    "存量红",
)


# ---------------------------------------------------------------------------
# DB 定位
# ---------------------------------------------------------------------------


def find_db() -> Path | None:
    explicit = os.environ.get("HERMES_KANBAN_DB")
    if explicit:
        p = Path(explicit)
        return p if p.is_file() else None

    home = Path(os.environ.get("HERMES_HOME", str(Path.home() / ".hermes")))
    # profile 会话里 HERMES_HOME 指向 profile 目录本身
    bases = [home]
    if home.parent.name == "profiles":
        bases.append(home.parent.parent)
    elif (home / "profiles").is_dir():
        pass
    else:
        bases.append(Path.home() / ".hermes")

    board = os.environ.get("KANBAN_BOARD", "dev")
    for base in bases:
        p = base / "kanban" / "boards" / board / "kanban.db"
        if p.is_file():
            return p
    return None


# ---------------------------------------------------------------------------
# 图模型
# ---------------------------------------------------------------------------


class Chain:
    def __init__(self, con: sqlite3.Connection):
        self.con = con
        self.tasks = {}
        for row in con.execute(
            "SELECT id, title, status, priority, assignee FROM tasks WHERE status <> 'archived'"
        ):
            self.tasks[row[0]] = {
                "id": row[0],
                "title": row[1] or "",
                "status": row[2],
                "priority": row[3] or 0,
                "assignee": row[4],
            }
        self.children = {}
        self.parents = {}
        for pid, cid in con.execute("SELECT parent_id, child_id FROM task_links"):
            if pid in self.tasks and cid in self.tasks:
                self.children.setdefault(pid, []).append(cid)
                self.parents.setdefault(cid, []).append(pid)

    def open_tasks(self):
        return [t for t in self.tasks.values() if t["status"] in OPEN_STATUS]

    def is_open(self, tid: str) -> bool:
        return self.tasks.get(tid, {}).get("status") in OPEN_STATUS

    def running(self):
        for t in self.tasks.values():
            if t["status"] == "running":
                return t
        return None

    # -- 主链 ---------------------------------------------------------------

    def head(self):
        """链头：未完成卡里唯一没有父卡的；没有就取 running；再没有取第一个未完成卡。"""
        no_parent = [t["id"] for t in self.open_tasks() if not self.parents.get(t["id"])]
        if len(no_parent) == 1:
            return no_parent[0]
        r = self.running()
        if r:
            return r["id"]
        if no_parent:
            return sorted(no_parent, key=lambda x: self.tasks[x]["title"])[0]
        return None

    def walk(self, start: str | None = None, limit: int = 2000):
        """从链头往下走，返回有序列表。遇到分叉取优先级最高的分支继续走。"""
        start = start or self.head()
        if not start:
            return []
        out, seen = [], set()
        cur = start
        while cur and cur not in seen and len(out) < limit:
            seen.add(cur)
            out.append(cur)
            kids = [k for k in self.children.get(cur, []) if k not in seen]
            if not kids:
                break
            if len(kids) > 1:
                # 分叉：优先走未完成的、优先级高的
                kids.sort(
                    key=lambda k: (
                        0 if self.is_open(k) else 1,
                        -self.tasks[k]["priority"],
                        self.tasks[k]["title"],
                    )
                )
            cur = kids[0]
        return out

    # -- 违规检查 -----------------------------------------------------------

    def violations(self):
        out = []
        open_ids = {t["id"] for t in self.open_tasks()}

        # C1 / C2：分叉
        for pid, kids in self.children.items():
            p_status = self.tasks[pid]["status"]
            open_kids = [k for k in kids if k in open_ids]
            if p_status in OPEN_STATUS and len(kids) > 1:
                out.append(("C1", pid, open_kids, f"未完成父卡有 {len(kids)} 个子卡"))
            elif p_status in CLOSED_STATUS and len(open_kids) > 1:
                out.append(
                    ("C2", pid, open_kids, f"已完成父卡下有 {len(open_kids)} 个未完成子卡")
                )

        # C3：游离
        for tid in open_ids:
            if not self.parents.get(tid):
                out.append(("C3", tid, [tid], "未完成卡没有父卡"))

        # C4：死路
        for tid in open_ids:
            if not self.children.get(tid):
                t = self.tasks[tid]
                if not CHAIN_TAIL_PATTERN.match(t["title"]):
                    out.append(("C4", tid, [tid], "未完成卡没有子卡（跑完不解锁任何东西）"))

        # C5：环路（只在未完成子图里找）
        color = {}

        def dfs(u, stack):
            color[u] = 1
            for v in self.children.get(u, []):
                if v not in open_ids:
                    continue
                if color.get(v) == 1:
                    cyc = stack[stack.index(v):] + [v] if v in stack else [u, v]
                    out.append(("C5", v, cyc, "环路"))
                elif color.get(v, 0) == 0:
                    dfs(v, stack + [v])
            color[u] = 2

        for tid in open_ids:
            if color.get(tid, 0) == 0:
                dfs(tid, [tid])

        return out


# ---------------------------------------------------------------------------
# 插入点计算
# ---------------------------------------------------------------------------


def batch_key(title: str):
    m = BATCH_RE.match(title.strip())
    if not m:
        return None
    return (m.group("batch"), int(m.group("num")), m.group("suffix") or "")


def is_debt(title: str) -> bool:
    return any(k in title for k in DEBT_KEYWORDS)


def choose_insert(chain: Chain, card_id: str, order: list[str]) -> tuple[int, str]:
    """返回 (插入下标, 理由)。下标含义：插在 order[idx-1] 和 order[idx] 之间。"""
    card = chain.tasks[card_id]
    title = card["title"]

    # ① 有批次编号 → 同批次内按编号插
    bk = batch_key(title)
    if bk:
        same = []
        for i, tid in enumerate(order):
            if tid == card_id:
                continue
            b2 = batch_key(chain.tasks[tid]["title"])
            if b2 and b2[0] == bk[0]:
                same.append((i, b2))
        if same:
            before = [(i, b) for i, b in same if b[1:] < bk[1:]]
            if before:
                idx, _ = max(before, key=lambda x: x[1][1:])
                return idx + 1, f"同批次 {bk[0]} 内按编号插在 {chain.tasks[order[idx]]['title'][:16]} 之后"
            after = [(i, b) for i, b in same if b[1:] > bk[1:]]
            if after:
                idx, _ = min(after, key=lambda x: x[1][1:])
                return idx, f"同批次 {bk[0]} 内按编号插在 {chain.tasks[order[idx]]['title'][:16]} 之前"

    # ② 清债类 → 插到 running 之后
    if is_debt(title):
        r = chain.running()
        if r and r["id"] in order:
            return order.index(r["id"]) + 1, f"清债类卡，插在 running（{r['title'][:16]}）之后"

    # ③ 其余 → 链尾（最后一个未完成卡之前）
    tail_idx = len(order)
    for i in range(len(order) - 1, -1, -1):
        if chain.is_open(order[i]):
            tail_idx = i
            break
    return tail_idx, "无编号/非清债类，插到链尾（最后一个未完成卡之前）"


# ---------------------------------------------------------------------------
# 修复
# ---------------------------------------------------------------------------


def backup_db(db: Path) -> Path:
    dst = db.with_suffix(db.suffix + f".bak-chaingate-{datetime.now():%m%d%H%M}")
    shutil.copy2(db, dst)
    return dst


def apply_fix(con: sqlite3.Connection, chain: Chain, verbose=False, commit=True) -> int:
    """一次修一张违规卡，返回修的条数。

    commit=False 用于 --dry-run：所有改动留在事务里，由调用方 rollback。
    """
    fixed = 0
    guard = 0
    while guard < 200:
        guard += 1
        vs = chain.violations()
        if not vs:
            break

        # 优先修 C2（已完成父卡下的未完成子卡）——这是最常见的分叉
        vs.sort(key=lambda v: {"C2": 0, "C1": 1, "C3": 2, "C4": 3, "C5": 4}[v[0]])
        code, anchor, affected, msg = vs[0]

        # 要搬的卡：C1/C2 里"多出来"的那些（保留优先级最高的一个在原位）
        if code in ("C1", "C2"):
            kids = [k for k in affected if chain.is_open(k)]
            if len(kids) <= 1:
                # 不是可修的状态，跳过这一类
                break
            kids.sort(key=lambda k: (-chain.tasks[k]["priority"], chain.tasks[k]["title"]))
            movers = kids[1:]  # 留第一个在原位
        else:
            movers = [affected[0]] if affected else []

        if not movers:
            break

        order = chain.walk()
        # 搬走的卡先从链里剔除，避免自己插到自己
        order_wo = [t for t in order if t not in movers]

        changed = False
        for card in movers:
            idx, why = choose_insert(chain, card, order_wo)
            idx = max(0, min(idx, len(order_wo)))
            prev_node = order_wo[idx - 1] if idx > 0 else None
            next_node = order_wo[idx] if idx < len(order_wo) else None

            # 1. 拆掉这张卡的所有父链接（切断分叉）
            con.execute("DELETE FROM task_links WHERE child_id = ?", (card,))
            # 2. 也拆掉所有指向它的链接（防环）
            con.execute("DELETE FROM task_links WHERE parent_id = ? AND child_id = ?", (card, card))

            # 3. 串进插入点
            if prev_node:
                con.execute(
                    "INSERT OR IGNORE INTO task_links(parent_id, child_id) VALUES (?, ?)",
                    (prev_node, card),
                )
            if next_node:
                # 断掉 prev → next，改走 prev → card → next
                if prev_node:
                    con.execute(
                        "DELETE FROM task_links WHERE parent_id = ? AND child_id = ?",
                        (prev_node, next_node),
                    )
                else:
                    # 插在链头：把 next 的其它父链接保留，只加 card → next
                    pass
                con.execute(
                    "INSERT OR IGNORE INTO task_links(parent_id, child_id) VALUES (?, ?)",
                    (card, next_node),
                )

            # 4. 状态：如果它现在是 ready 但父卡没完成，压回 todo
            if prev_node:
                p_st = chain.tasks[prev_node]["status"]
                if p_st not in CLOSED_STATUS:
                    con.execute(
                        "UPDATE tasks SET status='todo' WHERE id=? AND status='ready'", (card,)
                    )

            if verbose:
                where = f"插在 {chain.tasks[prev_node]['title'][:16]} 之后" if prev_node else "插在链头"
                print(f"    · {card} 「{chain.tasks[card]['title'][:30]}」→ {where}")
                print(f"      理由：{why}")
            changed = True
            fixed += 1

        if commit:
            con.commit()
        if not changed:
            break
        # 重新加载图（同一事务里能看到未提交的改动）
        chain = Chain(con)

    return fixed


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description="任务链完整性门禁（可自动修复）")
    ap.add_argument("--root", default=None, help="兼容 check.sh 的 --root，仅作定位，不改变逻辑")
    ap.add_argument("--verbose", action="store_true")
    ap.add_argument("--fix", action="store_true", help="自动修复（会先备份 DB）")
    ap.add_argument("--dry-run", action="store_true", help="配合 --fix：只打印计划，不改 DB")
    args = ap.parse_args()

    db = find_db()
    if not db:
        print("  · 无 kanban DB，跳过任务链门禁")
        return 0

    con = sqlite3.connect(str(db))
    chain = Chain(con)

    open_n = len(chain.open_tasks())
    if open_n == 0:
        print("  ✓ 没有未完成任务，链门禁跳过")
        return 0

    vs = chain.violations()

    if args.verbose or vs:
        order = chain.walk()
        print(f"  主链 {len(order)} 张，未完成 {open_n} 张（DB: {db.name}）")
        if args.verbose:
            for i, tid in enumerate(order[:8]):
                t = chain.tasks[tid]
                print(f"    {i:2d} {t['status']:8s} {t['title'][:44]}")
            if len(order) > 8:
                print(f"    … 共 {len(order)} 张")

    if not vs:
        print("  ✓ 任务链结构完整（无分叉 / 游离 / 死路 / 环路）")
        return 0

    # 有违规
    print("", file=sys.stderr)
    print(f"  ✗ 任务链有 {len(vs)} 处问题：", file=sys.stderr)
    order = chain.walk()
    for code, anchor, affected, msg in vs[:20]:
        t = chain.tasks.get(anchor, {})
        print(f"    [{code}] {anchor} 「{t.get('title','')[:34]}」— {msg}", file=sys.stderr)
        if code in ("C1", "C2"):
            for k in affected:
                kt = chain.tasks.get(k, {})
                idx, why = choose_insert(chain, k, [x for x in order if x != k])
                print(f"         → {k} 「{kt.get('title','')[:30]}」建议：{why}", file=sys.stderr)
        elif code == "C4":
            idx, why = choose_insert(chain, anchor, order)
            print(f"         → 建议：{why}", file=sys.stderr)

    if not args.fix:
        print("", file=sys.stderr)
        print("    修法：", file=sys.stderr)
        print("      python3 scripts/lint/task_chain_valid.py --fix --dry-run   # 先看计划", file=sys.stderr)
        print("      python3 scripts/lint/task_chain_valid.py --fix             # 真改（会备份）", file=sys.stderr)
        print("", file=sys.stderr)
        print("    规则：一个未完成的父卡只能有 1 个子卡；新卡要么插到同批次编号位置，", file=sys.stderr)
        print("    要么插到 running 之后（清债类），要么插到链尾。", file=sys.stderr)
        return 1

    # --fix
    if args.dry_run:
        print("", file=sys.stderr)
        print("  （--dry-run）将执行以下调整：", file=sys.stderr)
        try:
            n = apply_fix(con, chain, verbose=True, commit=False)
            print(f"  （dry-run）共 {n} 处调整，未写库", file=sys.stderr)
        finally:
            con.rollback()
        return 1

    bak = backup_db(db)
    print(f"  已备份：{bak}", file=sys.stderr)
    n = apply_fix(con, chain, verbose=True)
    # 必须重新加载：apply_fix 内部重建的是局部 Chain，外面这个还是旧的
    after = Chain(con).violations()
    if after:
        print(f"  ⚠ 修了 {n} 处，仍有 {len(after)} 处未解决，需人工看", file=sys.stderr)
        return 1
    print(f"  ✓ 已修 {n} 处，任务链现在结构完整")
    return 0


if __name__ == "__main__":
    sys.exit(main())
