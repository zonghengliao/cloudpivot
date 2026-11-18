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

--fix 的两条硬约束（2026-11-18 补，血案见 docs/evidence/2026-11-18-chainfix-probe/）
------------------------------------------------------------------------------
  ① **链头只认 running**。有 running 卡时一律从它往后走；「未完成卡里唯一
     没有父卡的那张」只是没有 running 时的兜底。反例：同一张 running 卡下新立
     2 张游离 ready 清债卡，修掉 1 张后剩下的那张成了"唯一无父卡"→ 变成链头 →
     walk() 只看得到它自己 → 插入点全空 → 每轮删边却建不出边，转满 200 轮。
  ② **先算后改，且每轮必须真的变好**。算出合法的 (prev, card, next) 之前不许
     DELETE；一轮改完若"缺陷重量"没有严格下降，就回滚该轮并打印可粘贴的
     INSERT 语句让人工落边。反例：order 为空时把插入下标静默钳成 0 → prev/next
     皆 None → 删了边却一条不建 → 状态不变 → 循环 200 次，每轮还 commit。

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
        """链头：有 running 卡时**一律**取 running（调度器只从 running 往后走）。

        没有 running 才用「未完成卡里唯一没有父卡的那张」兜底，再没有取标题最小的未完成卡。
        注意顺序：running 必须优先。反例见模块 docstring「--fix 的两条硬约束」①——
        「唯一无父卡」优先会让一张游离卡把整条主链挡在外面。
        """
        r = self.running()
        if r:
            return r["id"]
        no_parent = [t["id"] for t in self.open_tasks() if not self.parents.get(t["id"])]
        if len(no_parent) == 1:
            return no_parent[0]
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


def choose_insert(chain: Chain, card_id: str, order: list[str]) -> tuple[int, str] | None:
    """返回 (插入下标, 理由)；**算不出插入点就返回 None**，由调用方报"无法自动修复"。

    下标含义：插在 order[idx-1] 和 order[idx] 之间。
    `order` 为空（链里没有别的卡可挂）时返回 None —— 绝不许静默钳到 0：
    钳到 0 会让 prev/next 同时为 None，调用方就会"删了边却建不出边"。
    """
    if not order:
        return None

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
# 缺陷重量（--fix 每轮"是否真的变好"的判据）
# ---------------------------------------------------------------------------


def defect_weight(chain: Chain) -> int:
    """结构缺陷的总重量：越小越好，0 = 结构完整。

    C1/C2（分叉）按"多出来的子卡数"计 —— 3 个儿子记 2，搬走 1 个就降 1；
    其余每处记 1。用重量而不是"处数"，是因为「3 儿子搬成 2 儿子」处数不变但确实变好了。
    """
    open_ids = {t["id"] for t in chain.open_tasks()}
    w = 0
    for code, _anchor, affected, _msg in chain.violations():
        if code in ("C1", "C2"):
            n = len([k for k in affected if k in open_ids])
            w += max(1, n - 1)
        else:
            w += 1
    return w


def report_manual(chain: Chain, card: str, order: list[str], reason: str) -> None:
    """修复无法继续时，打印可粘贴的人工落边语句（不许把单据留在中间态）。"""
    t = chain.tasks.get(card, {})
    print("", file=sys.stderr)
    print(f"  ⚠ 无法自动修复：{card} 「{t.get('title', '')[:40]}」", file=sys.stderr)
    print(f"    原因：{reason}", file=sys.stderr)
    prev_node = order[-1] if order else None
    kids = [k for k in chain.children.get(prev_node, [])] if prev_node else []
    next_node = kids[0] if kids else None
    if not prev_node:
        print("    主链里没有可挂的邻位，请先 --verbose 看清主链，再人工落边。", file=sys.stderr)
        return
    print("    人工落边（可直接粘贴到 sqlite3）：", file=sys.stderr)
    print(
        f"      DELETE FROM task_links WHERE child_id='{card}';",
        file=sys.stderr,
    )
    if next_node:
        print(
            f"      DELETE FROM task_links WHERE parent_id='{prev_node}' AND child_id='{next_node}';",
            file=sys.stderr,
        )
    print(
        f"      INSERT INTO task_links(parent_id, child_id) VALUES ('{prev_node}', '{card}');",
        file=sys.stderr,
    )
    if next_node:
        print(
            f"      INSERT INTO task_links(parent_id, child_id) VALUES ('{card}', '{next_node}');",
            file=sys.stderr,
        )


# ---------------------------------------------------------------------------
# 修复
# ---------------------------------------------------------------------------


def backup_db(db: Path) -> Path:
    dst = db.with_suffix(db.suffix + f".bak-chaingate-{datetime.now():%m%d%H%M}")
    shutil.copy2(db, dst)
    return dst


def apply_fix(con: sqlite3.Connection, chain: Chain, verbose=False, commit=True) -> int:
    """一次修一张违规卡，返回修的条数。

    规矩（见模块 docstring「--fix 的两条硬约束」②）：
      · **先算后改**：`(prev, card, next)` 三元组算出来之前，一行 DELETE 都不许执行；
      · **每轮必须真的变好**：改完断言缺陷重量严格下降，否则回滚该轮并打印人工落边语句；
      · 每轮只在"已验证下降"之后放行（SAVEPOINT），最终由本函数提交一次，别边改边提交。

    commit=False 用于 --dry-run：所有改动留在事务里，由调用方 rollback。
    """
    # 显式开事务：SQLite 里「RELEASE 最外层 SAVEPOINT」会**顺带提交**整笔事务，
    # 所以不先 BEGIN 的话，--dry-run 的 RELEASE 就把改动落库了（2026-11-18 实测踩到：
    # 干跑后副本库的 task_links 从 1285 变成 1287，等于干跑改了库）。
    con.execute("BEGIN IMMEDIATE")
    fixed = 0
    guard = 0
    max_rounds = max(200, 4 * len(chain.open_tasks()))
    while guard < max_rounds:
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

        # 一轮只搬一张：每轮都能独立断言"重量真的降了"，不会被多卡互相抵消
        card = movers[0]
        order = chain.walk()
        order_wo = [t for t in order if t != card]

        plan = choose_insert(chain, card, order_wo)
        if plan is None:
            report_manual(chain, card, order_wo, "链里没有可挂的邻位（order 为空），插入点算不出来")
            break
        idx, why = plan
        if not 0 <= idx <= len(order_wo):
            report_manual(chain, card, order_wo, f"插入下标越界（idx={idx}，链长 {len(order_wo)}）")
            break
        prev_node = order_wo[idx - 1] if idx > 0 else None
        next_node = order_wo[idx] if idx < len(order_wo) else None
        if prev_node is None and next_node is None:
            report_manual(chain, card, order_wo, "算出的 prev / next 同时为空，插进去等于不挂任何边")
            break

        w_before = defect_weight(chain)
        con.execute("SAVEPOINT chainfix_round")

        # 1. 拆掉这张卡的所有父链接（切断分叉）
        con.execute("DELETE FROM task_links WHERE child_id = ?", (card,))
        # 2. 也拆掉自环（防环）
        con.execute("DELETE FROM task_links WHERE parent_id = ? AND child_id = ?", (card, card))

        # 3. 串进插入点：prev → card → next
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
            con.execute(
                "INSERT OR IGNORE INTO task_links(parent_id, child_id) VALUES (?, ?)",
                (card, next_node),
            )

        # 4. 状态：如果它现在是 ready 但父卡没完成，压回 todo
        if prev_node:
            p_st = chain.tasks[prev_node]["status"]
            if p_st not in CLOSED_STATUS:
                con.execute("UPDATE tasks SET status='todo' WHERE id=? AND status='ready'", (card,))

        new_chain = Chain(con)
        w_after = defect_weight(new_chain)
        if w_after >= w_before:
            con.execute("ROLLBACK TO chainfix_round")
            con.execute("RELEASE chainfix_round")
            report_manual(
                chain,
                card,
                order_wo,
                f"改完缺陷重量没下降（{w_before} → {w_after}），已回滚该轮，不再硬转",
            )
            break
        con.execute("RELEASE chainfix_round")

        if verbose:
            where = f"插在 {chain.tasks[prev_node]['title'][:16]} 之后" if prev_node else "插在链头"
            print(f"    · {card} 「{chain.tasks[card]['title'][:30]}」→ {where}")
            print(f"      理由：{why}（缺陷 {w_before} → {w_after}）")
        chain = new_chain
        fixed += 1

    if commit:
        con.execute("COMMIT")
    return fixed


# ---------------------------------------------------------------------------
# 判据自检（内存库，不碰真库）
# ---------------------------------------------------------------------------

_SELFTEST_DDL = """
CREATE TABLE tasks(id TEXT PRIMARY KEY, title TEXT NOT NULL, status TEXT NOT NULL,
                   priority INTEGER DEFAULT 0, assignee TEXT);
CREATE TABLE task_links(parent_id TEXT NOT NULL, child_id TEXT NOT NULL,
                        PRIMARY KEY(parent_id, child_id));
"""


class _OldHeadChain(Chain):
    """反证用：把 head() 退回 2026-11-18 之前的优先序（唯一无父卡先于 running）。"""

    def head(self):
        no_parent = [t["id"] for t in self.open_tasks() if not self.parents.get(t["id"])]
        if len(no_parent) == 1:
            return no_parent[0]
        r = self.running()
        if r:
            return r["id"]
        if no_parent:
            return sorted(no_parent, key=lambda x: self.tasks[x]["title"])[0]
        return None


def _selftest_db():
    """血案形态：running 卡在链中段，另有 2 张游离 ready 清债卡。"""
    con = sqlite3.connect(":memory:")
    con.isolation_level = None
    con.executescript(_SELFTEST_DDL)
    rows = [
        ("t_root", "G-1 根卡", "done"),
        ("t_run", "【门禁】跑着的卡", "running"),
        ("t_next", "【测试卫生】链上后续卡", "todo"),
        ("t_done2", "M9-01 已完成的后续", "done"),
        ("t_free1", "【测试卫生】游离清债卡1", "ready"),
        ("t_free2", "【测试卫生】游离清债卡2", "ready"),
    ]
    for tid, title, st in rows:
        con.execute("INSERT INTO tasks(id,title,status,priority) VALUES(?,?,?,0)", (tid, title, st))
    for pid, cid in [("t_root", "t_run"), ("t_run", "t_next"), ("t_next", "t_done2")]:
        con.execute("INSERT INTO task_links(parent_id, child_id) VALUES(?,?)", (pid, cid))
    return con


def selftest() -> int:
    """判据自检：把 2026-11-18 的血案形态在内存库里重放一遍（不碰真库）。

    钉住四条：
      ① 链头只认 running（2 张游离卡时 head 仍是 running）；
      ② choose_insert 在链为空时返回 None（不许静默钳到 0）；
      ③ 干跑不写库、真跑收敛、每条卡只出现一次、主链张数不减少；
      ④ 反证：head() 去掉 running 优先 → 主链塌成 1 张（证明判据在承重）。
    """
    import io
    from contextlib import redirect_stderr, redirect_stdout

    checks = []

    def ok(cond, msg):
        checks.append(bool(cond))
        print(f"  {'✓' if cond else '✗'} {msg}")
        return bool(cond)

    # ① 链头
    con = _selftest_db()
    ch = Chain(con)
    ok(ch.head() == "t_run", "① 有 running 时链头就是 running（不被 2 张游离卡的『唯一无父卡』劫走）")
    ok(len(ch.walk()) == 3, f"① 主链从 running 往后走得到 3 张（实得 {len(ch.walk())}）")
    ok(len(ch.violations()) == 4, f"① 初始 4 处问题（2 张游离卡各 C3+C4，实得 {len(ch.violations())}）")

    # ② 空链不许静默钳位
    ok(
        choose_insert(ch, "t_free1", []) is None,
        "② choose_insert(order=[]) 返回 None（旧代码钳到 0 → 删边不建边）",
    )

    # ③ 干跑：不写库 + 收敛 + 不重复打印
    links0 = sorted(con.execute("SELECT parent_id, child_id FROM task_links").fetchall())
    buf_out, buf_err = io.StringIO(), io.StringIO()
    with redirect_stdout(buf_out), redirect_stderr(buf_err):
        n_dry = apply_fix(con, Chain(con), verbose=True, commit=False)
        left = len(Chain(con).violations())
    con.rollback()
    lines = [ln for ln in buf_out.getvalue().splitlines() if ln.strip().startswith("·")]
    ok(n_dry == 2, f"③ 干跑收敛在 2 处调整（旧代码 200 轮不收敛，实得 {n_dry}）")
    ok(left == 0, f"③ 干跑终态剩余问题 0 处（实得 {left}）")
    ok(
        sorted(con.execute("SELECT parent_id, child_id FROM task_links").fetchall()) == links0,
        "③ 干跑不写库（rollback 后 task_links 与开跑前逐条一致）",
    )
    ok(
        sum(1 for ln in lines if "t_free1" in ln) == 1
        and sum(1 for ln in lines if "t_free2" in ln) == 1,
        f"③ 每条卡只打印一次（旧代码同一张卡打印 198 次，实得 {len(lines)} 行）",
    )

    # ③ 真跑：收敛 + 主链不减少 + 两张卡串在 running 之后
    ch = Chain(con)
    n_fix = apply_fix(con, ch, verbose=False, commit=True)
    ch2 = Chain(con)
    order = ch2.walk()
    ok(n_fix == 2, f"③ 真跑修 2 处（实得 {n_fix}）")
    ok(ch2.violations() == [], "③ 真跑后结构完整（0 处问题）")
    ok(len(order) == 5, f"③ 主链张数不减少：3 → 5（实得 {len(order)}）")
    ok(
        order[0] == "t_run" and set(order[1:3]) == {"t_free1", "t_free2"},
        f"③ 两张清债卡都串在 running 之后（实得前 3 张 {order[:3]}）",
    )

    # ④ 反证：同一中间态下，旧优先序会把链挡成 1 张
    con4 = sqlite3.connect(":memory:")
    con4.isolation_level = None
    con4.executescript(_SELFTEST_DDL)
    con4.execute("INSERT INTO tasks(id,title,status,priority) VALUES('t_root','G-1 根卡','done',0)")
    con4.execute("INSERT INTO tasks(id,title,status,priority) VALUES('t_run','【门禁】跑着的卡','running',0)")
    con4.execute("INSERT INTO tasks(id,title,status,priority) VALUES('t_next','【测试卫生】链上后续卡','todo',0)")
    con4.execute("INSERT INTO tasks(id,title,status,priority) VALUES('t_done2','M9-01 已完成的后续','done',0)")
    con4.execute("INSERT INTO tasks(id,title,status,priority) VALUES('t_free1','【测试卫生】游离清债卡1','todo',0)")
    con4.execute("INSERT INTO tasks(id,title,status,priority) VALUES('t_free2','【测试卫生】游离清债卡2','ready',0)")
    for pid, cid in [
        ("t_root", "t_run"),
        ("t_run", "t_free1"),
        ("t_free1", "t_next"),
        ("t_next", "t_done2"),
    ]:
        con4.execute("INSERT INTO task_links(parent_id, child_id) VALUES(?,?)", (pid, cid))
    ok(
        _OldHeadChain(con4).head() == "t_free2" and len(_OldHeadChain(con4).walk()) == 1,
        "④ 反证：head() 去掉 running 优先后，链头变成游离卡、主链塌成 1 张",
    )
    ok(
        Chain(con4).head() == "t_run" and len(Chain(con4).walk()) == 4,
        "④ 对照：现行 head() 仍认 running，主链 4 张",
    )

    # ⑤ 算不出更好落点时宁可不动：链头是游离卡（C3 无处可挂）、链尾是死路卡（C4 无子可挂）。
    #    旧代码在这里每轮"删一次边再建一次边"，重量不变 → 转满 200 轮且每轮 commit。
    con5 = sqlite3.connect(":memory:")
    con5.isolation_level = None
    con5.executescript(_SELFTEST_DDL)
    con5.execute("INSERT INTO tasks(id,title,status,priority) VALUES('t_free','【测试卫生】游离卡头','todo',0)")
    con5.execute("INSERT INTO tasks(id,title,status,priority) VALUES('t_tail','【测试卫生】链尾死路卡','todo',0)")
    con5.execute("INSERT INTO task_links(parent_id, child_id) VALUES('t_free','t_tail')")
    links5 = sorted(con5.execute("SELECT parent_id, child_id FROM task_links").fetchall())
    buf5 = io.StringIO()
    with redirect_stderr(buf5):
        n5 = apply_fix(con5, Chain(con5), verbose=False, commit=False)
    con5.rollback()
    ok(n5 == 0, f"⑤ 修不动就不动（旧代码转满 200 轮，实得 {n5} 处调整）")
    ok(
        sorted(con5.execute("SELECT parent_id, child_id FROM task_links").fetchall()) == links5,
        "⑤ 该轮已回滚：task_links 与开跑前逐条一致",
    )
    ok("无法自动修复" in buf5.getvalue(), "⑤ 并且打印了「无法自动修复」+ 人工落边提示")

    print(f"  {'✓' if all(checks) else '✗'} 自检{'通过' if all(checks) else '失败'}（{sum(checks)}/{len(checks)} 条）")
    return 0 if all(checks) else 1


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description="任务链完整性门禁（可自动修复）")
    ap.add_argument("--root", default=None, help="兼容 check.sh 的 --root，仅作定位，不改变逻辑")
    ap.add_argument("--verbose", action="store_true")
    ap.add_argument("--fix", action="store_true", help="自动修复（会先备份 DB）")
    ap.add_argument("--dry-run", action="store_true", help="配合 --fix：只打印计划，不改 DB")
    ap.add_argument(
        "--selftest",
        action="store_true",
        help="判据自检（内存库重放 2026-11-18 的血案形态，不碰真库）",
    )
    args = ap.parse_args()

    if args.selftest:
        return selftest()

    db = find_db()
    if not db:
        print("  · 无 kanban DB，跳过任务链门禁")
        return 0

    con = sqlite3.connect(str(db))
    # 事务由脚本自己管（apply_fix 里显式 BEGIN/COMMIT/ROLLBACK）。
    # 用默认的隐式事务，RELEASE 最外层 SAVEPOINT 会顺带提交 → --dry-run 会写库。
    con.isolation_level = None
    con.execute("PRAGMA busy_timeout = 5000")
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
                plan = choose_insert(chain, k, [x for x in order if x != k])
                why = plan[1] if plan else "算不出插入点（链里没有可挂的邻位），需人工落边"
                print(f"         → {k} 「{kt.get('title','')[:30]}」建议：{why}", file=sys.stderr)
        elif code == "C4":
            plan = choose_insert(chain, anchor, order)
            why = plan[1] if plan else "算不出插入点（链里没有可挂的邻位），需人工落边"
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
        sys.stdout.flush()
        print("", file=sys.stderr)
        print("  （--dry-run）将执行以下调整：", file=sys.stderr)
        try:
            n = apply_fix(con, chain, verbose=True, commit=False)
            after = Chain(con)
            left = len(after.violations())
            sys.stdout.flush()
            print(
                f"  （dry-run）共 {n} 处调整，未写库；修完主链 {len(after.walk())} 张、剩余问题 {left} 处",
                file=sys.stderr,
            )
        finally:
            con.rollback()
        return 1

    bak = backup_db(db)
    sys.stdout.flush()
    print(f"  已备份：{bak}", file=sys.stderr)
    n = apply_fix(con, chain, verbose=True)
    # 必须重新加载：apply_fix 内部重建的是局部 Chain，外面这个还是旧的
    after_chain = Chain(con)
    after = after_chain.violations()
    sys.stdout.flush()
    if after:
        print(
            f"  ⚠ 修了 {n} 处，仍有 {len(after)} 处未解决（主链 {len(after_chain.walk())} 张），"
            f"见上面的「无法自动修复」+ 人工落边语句",
            file=sys.stderr,
        )
        return 1
    print(f"  ✓ 已修 {n} 处，任务链现在结构完整（主链 {len(after_chain.walk())} 张）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
