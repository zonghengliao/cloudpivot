#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""role_code_gate.py —— 迁移授权段角色码常驻闸（**不连库**，纯静态扫描）

## 为什么要有这道闸（不是猜，是生产实测）

`sys_role_menu` 的授权是「按角色 → 按菜单行」写的，而 role_code 是纯字符串字面量：
写错一个字母 / 从一个老迁移里抄一份「历史角色码清单」，SQL **不会报任何错** ——
`WHERE r.role_code IN ('ADMIN','ACCOUNTANT',…)` 在 sys_role 里一行都匹配不到，
于是 `INSERT … SELECT` 选中 0 行，迁移照样成功、指纹照样记账，**授权等于从没发生过**。

2026-11-06（卡 t_227464dd）扫描 27 条「授权段带 `n_roles = 0` 空跑保护」的迁移时实测：

| 角色码                                        | 生产是否存在 |
|-----------------------------------------------|--------------|
| `SUPER_ADMIN`                                  | ✅           |
| `ADMIN` / `ACCOUNTANT` / `FINANCE`             | ❌           |
| `TEST_UADMIN`                                  | ❌           |
| `BUYER` / `PURCHASER` / `WAREHOUSE_KEEPER`     | ❌           |
| `WAREHOUSE`                                    | ❌           |

生产的真实角色是 `*_MANAGER` / `*_DIRECTOR` 体系。后果：这些迁移的授权段即便在非空库上
执行，也只是「什么都没授」，而写迁移的人以为授了（超管的 `*:*:*` 通配掩盖了这个缺口）。
V0258 已把业务角色矩阵整体补齐，但这套「抄老清单」的写法不设闸就会被下一个人再踩一次。

## 判据

1. 扫 `backend/migrations/**/*.up.sql`（**不含** `seed/S__*.sql` 里的授权段：种子是 repeatable、
   重复执行，且不参与版本指纹）；但 `S__*.sql` 里 `INSERT INTO sys_role` **定义**的角色会并入白名单
   —— 否则「新迁移授权给种子造的角色」会被误判未知码；
2. 定位每条 `INSERT INTO sys_role_menu …` 语句（到该语句的 `;` 为止，字符串 / 美元引号里的
   `;` 不截断），语句内取 `role_code` 的**字面量比较**：
     `role_code = 'X'` / `role_code <> 'X'` / `role_code != 'X'` / `role_code [NOT] IN ('X','Y')`
   —— `<>` / `NOT IN` 也查：写了一个不存在的码，语义会退化成「授给所有人」（过度授权，
   比空跑更危险）；
3. 字面量 ∉ 白名单 ⇒ **命中**。白名单 = 生产活角色（写死 + 注释来源）∪ 迁移 `INSERT INTO sys_role`
   种出来的角色（自动解析：新迁移可以「先建角色再授」，同一次提交即可自洽）；
4. 命中只有两条合法出路：
   * 历史基线（`role_code_gate_baseline.json`）：**按 sha256 锁文件**，内容一字不改 ⇒ 豁免；
     内容变了（改动历史迁移）⇒ 全部命中重报，因为已应用迁移是不可改的；
   * 语句（或紧邻其上的 5 行）里写 `-- ROLE-EXEMPT: <理由>`（理由必填，空理由视为没写）。

## 策略检查（只 WARN，不影响退出码）

新增/改动文件里出现 `INSERT INTO sys_role_menu` ⇒ 打印 WARN 并点名 `文件:行`，提示
「授权优先走 V0258 业务角色矩阵（改矩阵），别在新迁移里再手写一份」—— 因为装配库上
`sys_role` 还没有业务角色，手写清单只会静默 0 行。历史基线内的 25 个文件不重复告警
（27 条存量全 WARN 会把真正的新增淹掉）。`--json` 里对应 `policy_warnings`。

## 退出码

    0  通过（含「命中全部被基线/豁免注释覆盖」）
    1  有**新增/改动**的未知角色码 ⇒ 阻塞
    2  环境错误（迁移目录不存在 / 扫不到任何 .up.sql / 基线文件损坏）
       —— 不判绿，避免「路径写错 ⇒ 静默 0 命中 ⇒ 假绿」

## 用法

    python3 scripts/verify/role_code_gate.py                 # 常规检查（CI / check.sh）
    python3 scripts/verify/role_code_gate.py --json          # 给 CI / 监控
    python3 scripts/verify/role_code_gate.py --self-test     # 判据自检，不读仓库
    python3 scripts/verify/role_code_gate.py --write-baseline # 重新冻结历史基线（慎用）
"""
from __future__ import annotations

import argparse
import bisect
import hashlib
import json
import os
import re
import sys
import time

# ============================================================================
# 白名单来源 ①：生产库实际存在的活角色
#
# 取样命令（2026-11-06，root@172.18.10.144 /cloudpivot）：
#   select role_code from sys_role where deleted_at is null and status = 1 order by role_code;
# 输出（12 行，逐字抄录）：DEPT_LEADER / FINANCE_DIRECTOR / FINANCE_MANAGER /
#   FLOW_CONFIG_APPROVER / GENERAL_MANAGER / MUTEX_A / MUTEX_B / PURCHASE_MANAGER /
#   QA_LIMITED / QA_VIEWER / SUPER_ADMIN / WAREHOUSE_MANAGER
#
# 注：QA_LIMITED / QA_VIEWER / MUTEX_A / MUTEX_B 是验收夹具角色（互斥/只读限制用例），
#     生产里是**活角色**、迁移里也确实授过 —— 按「授权能否命中」的口径必须留在白名单。
# 新增业务角色时：**先**在迁移里 `INSERT INTO sys_role` 建出来（会被自动解析进白名单），
#     再在授权段引用；不要指望这里手改（手改 = 白名单与生产脱钩）。
# ============================================================================
PROD_ACTIVE_ROLES = {
    "DEPT_LEADER",
    "FINANCE_DIRECTOR",
    "FINANCE_MANAGER",
    "FLOW_CONFIG_APPROVER",
    "GENERAL_MANAGER",
    "MUTEX_A",
    "MUTEX_B",
    "PURCHASE_MANAGER",
    "QA_LIMITED",
    "QA_VIEWER",
    "SUPER_ADMIN",
    "WAREHOUSE_MANAGER",
}

BASELINE_NAME = "role_code_gate_baseline.json"
MIGRATIONS_REL = os.path.join("backend", "migrations")
EXEMPT_MARK = "ROLE-EXEMPT:"
EXEMPT_LOOKBACK_LINES = 5  # 豁免注释写在 INSERT 上方几行内都算

# `INSERT INTO [ONLY] ["schema".]"sys_role_menu"` —— 这几种写法都是合法的 PostgreSQL：
# 漏掉它们等于给「换个写法就绕过闸」留门（实测 `public.sys_role_menu` / `INSERT INTO ONLY` 旧版漏抓）。
_TARGET = r"(?:ONLY\s+)?(?:\"?[A-Za-z_][\w$]*\"?\s*\.\s*)?\"?%s\"?"
INSERT_ROLE_MENU_RE = re.compile(r"INSERT\s+INTO\s+" + (_TARGET % "sys_role_menu"), re.I)
INSERT_SYS_ROLE_RE = re.compile(r"INSERT\s+INTO\s+" + (_TARGET % "sys_role") + r"(?![\w$])", re.I)
# 角色码还常从「矩阵 / 临时表 / CTE」里灌进来（V0258 就是这套模板）：
#   (role_code, auth_code) VALUES ('DEPT_LEADER','a:b:c'), …
# 形如 `(…role_code… ) VALUES` 或 `t(…role_code…) AS (VALUES`
VALUES_ROLE_COLS_RE = re.compile(r"\(([^()]*\brole_code\b[^()]*)\)\s*(?:AS\s*\(\s*)?VALUES", re.I)
INSERT_INTO_NAME_RE = re.compile(r"INSERT\s+INTO\s+\"?([A-Za-z_][\w]*)\"?", re.I)
ROLE_CODE_EQ_RE = re.compile(r"\"?role_code\"?\s*(?:=|<>|!=)\s*'([^']*)'", re.I)
# 也认 `(role_code) IN (…)` 这种给列加括号的行表达式写法（合法 PG，不加就漏）。
ROLE_CODE_IN_RE = re.compile(r"(?:\(\s*)?\"?role_code\"?\s*\)?\s+(?:NOT\s+)?IN\s*\(", re.I)
# `role_code = ANY (ARRAY['A','B'])`：PostgreSQL 惯用写法，不认它 = 换个写法就能静默绕过闸。
ROLE_CODE_ANY_RE = re.compile(r"\"?role_code\"?\s*=\s*ANY\s*\(\s*(?:ARRAY\s*\[)?", re.I)
# 静态判不出来的角色匹配：LIKE / ILIKE / SIMILAR TO / 正则 / regexp_like(role_code,…)。
# 这些只能出策略 WARN（脚本不假装看得懂正则），绝不能当「没有字面量 ⇒ 没问题」。
ROLE_CODE_LIKE_RE = re.compile(
    r"\"?role_code\"?\s*\)?\s*(?:NOT\s+)?(?:I?LIKE|SIMILAR\s+TO|~[~*]?)", re.I)
ROLE_CODE_REGEXP_FN_RE = re.compile(r"regexp_(?:like|match)\s*\([^)]*role_code", re.I)
IN_SUBQUERY_RE = re.compile(r"^\s*\(\s*(?:SELECT|WITH)\b", re.I)
SQL_STRING_RE = re.compile(r"'([^']*)'")

RC_OK, RC_FAIL, RC_ENV = 0, 1, 2


# ============================================================================
# SQL 词法：剥注释（保留字节位置与换行）/ 找语句结束 / 取字面量
# ============================================================================

def mask_sql(text: str) -> str:
    """把 `--` 行注释与 `/* */` 块注释抹成空格（换行保留），字符串/美元引号原样保留。

    位置一一对应 ⇒ 抹完之后正则匹配到的 offset 仍能换算回原始行号。
    """
    out = list(text)
    i, n = 0, len(text)
    while i < n:
        ch = text[i]
        if ch == "'":  # 单引号字符串（'' 转义）
            i += 1
            while i < n:
                if text[i] == "'":
                    if i + 1 < n and text[i + 1] == "'":
                        i += 2
                        continue
                    i += 1
                    break
                i += 1
            continue
        if ch == '"':  # 双引号标识符
            i += 1
            while i < n and text[i] != '"':
                i += 1
            i += 1
            continue
        if ch == "$":  # 美元引号 $tag$ … $tag$
            m = re.match(r"\$[A-Za-z_0-9]*\$", text[i:])
            if m:
                tag = m.group(0)
                end = text.find(tag, i + len(tag))
                i = (end + len(tag)) if end != -1 else n
                continue
            i += 1
            continue
        if ch == "-" and i + 1 < n and text[i + 1] == "-":
            while i < n and text[i] != "\n":
                out[i] = " "
                i += 1
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "*":
            while i < n and not (text[i] == "*" and i + 1 < n and text[i + 1] == "/"):
                if text[i] != "\n":
                    out[i] = " "
                i += 1
            if i < n:
                out[i] = " "
                if i + 1 < n:
                    out[i + 1] = " "
                i += 2
            continue
        i += 1
    return "".join(out)


def statement_end(masked: str, start: int) -> int:
    """从 start 起找该语句的 `;`（跳过字符串/美元引号/双引号内的 `;`）。返回 `;` 之后的位置。"""
    i, n = start, len(masked)
    while i < n:
        ch = masked[i]
        if ch == "'":
            i += 1
            while i < n:
                if masked[i] == "'":
                    if i + 1 < n and masked[i + 1] == "'":
                        i += 2
                        continue
                    i += 1
                    break
                i += 1
            continue
        if ch == '"':
            i += 1
            while i < n and masked[i] != '"':
                i += 1
            i += 1
            continue
        if ch == "$":
            m = re.match(r"\$[A-Za-z_0-9]*\$", masked[i:])
            if m:
                tag = m.group(0)
                end = masked.find(tag, i + len(tag))
                i = (end + len(tag)) if end != -1 else n
                continue
            i += 1
            continue
        if ch == ";":
            return i + 1
        i += 1
    return n


class LineIndex:
    def __init__(self, text: str):
        self.starts = [0]
        for m in re.finditer("\n", text):
            self.starts.append(m.end())

    def line_of(self, offset: int) -> int:
        return bisect.bisect_right(self.starts, offset)


def role_literals_in_statement(stmt_masked: str, base_offset: int, lines: LineIndex):
    """语句内 role_code 的字面量比较 → ([(code, 行号)], dynamic)。

    dynamic=True 表示语句里还存在**静态无法判定**的角色匹配（`LIKE '%x%'` / `IN (SELECT …)`）：
    这类语句只出策略 WARN，不当「未知码」判红 —— 但也不能当作「没问题」闷掉，
    否则 `WHERE lower(role_code) LIKE '%admin%'` 就是一条免检通道。
    """
    hits = []
    dynamic = False

    def add(code: str, pos: int):
        hits.append((code, lines.line_of(base_offset + pos)))

    for m in ROLE_CODE_EQ_RE.finditer(stmt_masked):
        add(m.group(1), m.start(1))
    for m in ROLE_CODE_IN_RE.finditer(stmt_masked):
        depth, i, n = 0, m.end() - 1, len(stmt_masked)
        while i < n:
            ch = stmt_masked[i]
            if ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        group_start = m.end() - 1
        if IN_SUBQUERY_RE.match(stmt_masked[group_start:i + 1]):
            # IN (SELECT …) / IN (WITH …) —— 码从子查询算出来，静态看不见
            dynamic = True
            continue
        for s in SQL_STRING_RE.finditer(stmt_masked[m.end():i]):
            add(s.group(1), m.end() + s.start(1))
    for m in ROLE_CODE_ANY_RE.finditer(stmt_masked):
        body_start = m.end()
        tail = stmt_masked[body_start:]
        close = tail.find("]")
        if close != -1:
            body = tail[:close]
        else:  # ANY (VALUES ('A'),('B'))
            depth, i = 1, 0
            while i < len(tail):
                if tail[i] == "(":
                    depth += 1
                elif tail[i] == ")":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
            body = tail[:i]
        found = list(SQL_STRING_RE.finditer(body))
        for s in found:
            add(s.group(1), body_start + s.start(1))
        if not found:
            dynamic = True
    if ROLE_CODE_LIKE_RE.search(stmt_masked) or ROLE_CODE_REGEXP_FN_RE.search(stmt_masked):
        dynamic = True
    # 兜底：语句里提到 role_code，却既没解析出字面量、也没命中已知动态写法
    # ⇒ 说明是脚本不认识的形状（`(role_code) IN (…)`、函数包一层、…）。不能当绿：
    # 至少标成 dynamic 让策略 WARN 点名，别让「换个写法」变成免检通道。
    if not hits and not dynamic and re.search(r"\brole_code\b", stmt_masked, re.I):
        dynamic = True
    return hits, dynamic


def split_tuples(body: str):
    """从 `(a,b),(c,d)` 这样的 VALUES 体里切出每个元组的字段文本（按括号深度 + 顶层逗号）。"""
    groups, buf, depth = [], [], 0
    for ch in body:
        if ch == "(":
            depth += 1
            if depth == 1:
                buf = []
                continue
        elif ch == ")":
            depth -= 1
            if depth == 0:
                groups.append("".join(buf))
                continue
        if depth >= 1:
            buf.append(ch)
    out = []
    for g in groups:
        parts, piece, depth2 = [], [], 0
        for ch in g:
            if ch == "(":
                depth2 += 1
            elif ch == ")":
                depth2 -= 1
            if ch == "," and depth2 == 0:
                parts.append("".join(piece))
                piece = []
                continue
            piece.append(ch)
        parts.append("".join(piece))
        out.append(parts)
    return out


def _col_names(collist: str):
    return [c.strip().strip('"').strip() for c in collist.split(",")]


def _split_top_level(text: str) -> list:
    """按顶层逗号切分 SQL 片段（认单引号字符串与括号深度）。"""
    parts, piece, depth, in_quote = [], [], 0, False
    for ch in text:
        if in_quote:
            piece.append(ch)
            if ch == "'":
                in_quote = False
            continue
        if ch == "'":
            in_quote = True
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        elif ch == "," and depth == 0:
            parts.append("".join(piece))
            piece = []
            continue
        piece.append(ch)
    parts.append("".join(piece))
    return parts


def _role_code_from_select(stmt: str, cols: list) -> set:
    """`INSERT INTO sys_role (…) SELECT …` 形态的角色码（按 `role_code` 的列位置取字面量）。

    本仓的幂等角色种子一律写成

        INSERT INTO sys_role (id, role_code, …)
        SELECT '…'::uuid, 'HR_MANAGER', … WHERE NOT EXISTS (SELECT 1 FROM sys_role …);

    （`sys_role.role_code` 没有唯一约束，`VALUES` + `ON CONFLICT` 用不了）。
    旧解析器只认 `VALUES` 形式 ⇒ 这类角色**进不了白名单** ⇒ 新迁移一旦给它们授权就会被
    判成「未知角色码」（V0403 就是这么撞上的）。这里补上 SELECT 形态。
    """
    idx = cols.index("role_code")
    m = re.search(r"\bSELECT\b", stmt, re.I)
    if not m:
        return set()
    body = stmt[m.end():]
    cut = len(body)
    for kw in (r"\bFROM\b", r"\bWHERE\b", r"\bON\s+CONFLICT\b", r"\bGROUP\s+BY\b", r"\bORDER\s+BY\b"):
        mm = re.search(kw, body, re.I)
        if mm:
            cut = min(cut, mm.start())
    parts = _split_top_level(body[:cut])
    if idx < len(parts):
        mm = re.search(r"'([^']*)'", parts[idx])
        if mm:
            return {mm.group(1)}
    return set()


def parse_seeded_roles(masked: str) -> set:
    """解析 `INSERT INTO sys_role (…role_code…)` 种出来的角色码（`VALUES` 与 `SELECT` 两种形态）。"""
    roles = set()
    for m in INSERT_SYS_ROLE_RE.finditer(masked):
        end = statement_end(masked, m.start())
        stmt = masked[m.start():end]
        head = stmt.split("VALUES", 1)
        if len(head) != 2:
            # 幂等种子的 `INSERT … SELECT … WHERE NOT EXISTS`（V0387 的 HR_MANAGER 就是这种）
            cols = _col_names(stmt.split("(", 1)[-1].split(")", 1)[0])
            if "role_code" in cols:
                roles |= _role_code_from_select(stmt, cols)
            continue
        cols = _col_names(head[0].split("(", 1)[-1].split(")", 1)[0])
        if "role_code" not in cols:
            continue
        idx = cols.index("role_code")
        for parts in split_tuples(head[1]):
            if idx < len(parts):
                mm = re.search(r"'([^']*)'", parts[idx])
                if mm:
                    roles.add(mm.group(1))
    return roles


def values_role_codes(masked: str, lines: "LineIndex"):
    """角色码矩阵块（`INSERT INTO <tmp> (…role_code…) VALUES …` / `t(role_code) AS (VALUES …)`）。

    V0258 的模板就是把 (角色,码) 先灌进 temp 表再 `INSERT INTO sys_role_menu … FROM <tmp>`。
    抄这个模板时角色码同样可以是错的（矩阵照样是「安静的 0 行」），因此一并扫。
    跳过 `INSERT INTO sys_role (…role_code…) VALUES` —— 那是角色**定义**，由
    parse_seeded_roles 负责（它种出来的角色本来就在白名单里）。

    → [{"line": 块起始行, "start": 起始 offset, "end": 语句结束 offset, "codes": [(code, line)]}]
    """
    blocks = []
    for m in VALUES_ROLE_COLS_RE.finditer(masked):
        head_start = masked.rfind("INSERT", 0, m.start())
        if head_start != -1 and ";" not in masked[head_start:m.start()]:
            hm = INSERT_INTO_NAME_RE.match(masked, head_start)
            if hm and hm.group(1).lower() == "sys_role":
                continue
        cols = [c.lower() for c in _col_names(m.group(1))]
        if "role_code" not in cols:
            continue
        col = cols.index("role_code")
        body_start = m.end()
        end = statement_end(masked, body_start)
        codes = []
        for parts in split_tuples(masked[body_start:end]):
            if col < len(parts):
                mm = re.search(r"'([^']*)'", parts[col])
                if mm:
                    codes.append((mm.group(1), lines.line_of(m.start())))
        if codes:
            blocks.append({"line": lines.line_of(m.start()), "start": m.start(),
                           "end": end, "codes": codes})
    return blocks


def exempt_reason(raw: str, lines: "LineIndex", start: int, end: int):
    """语句（或紧邻其上的 N 行）里的 `-- ROLE-EXEMPT: <理由>` → 理由字符串；没写 → None。"""
    line_no = lines.line_of(start)
    lo = lines.starts[max(0, line_no - 1 - EXEMPT_LOOKBACK_LINES)]
    hi = lines.starts[min(len(lines.starts) - 1, lines.line_of(end))]
    for mm in re.finditer(re.escape(EXEMPT_MARK) + r"(.*)", raw[lo:hi]):
        return mm.group(1).strip()
    return None


def collect(root: str):
    """扫仓库：返回 (files, statements, seeded_roles, env_error)。

    statements: [{"file": 相对路径, "line": 行号, "kind": 语句种类,
                  "codes": [(code, line)], "exempt_reason": str|None}]
    """
    mig_dir = os.path.join(root, MIGRATIONS_REL)
    if not os.path.isdir(mig_dir):
        return [], [], set(), f"迁移目录不存在：{mig_dir}"

    paths = []
    seed_paths = []
    for dirpath, _dirnames, filenames in os.walk(mig_dir):
        for fn in filenames:
            if fn.endswith(".up.sql"):
                paths.append(os.path.join(dirpath, fn))
            elif fn.startswith("S__") and fn.endswith(".sql"):
                # 种子（repeatable）不参与授权段扫描，但它**定义**的角色是真角色：
                # 不解析会漏进白名单 ⇒ 迁移里授权给「种子造的角色」会被误判未知码。
                seed_paths.append(os.path.join(dirpath, fn))
    paths.sort()
    seed_paths.sort()
    if not paths:
        return [], [], set(), f"迁移目录里扫不到任何 *.up.sql：{mig_dir}"

    files, statements, seeded = [], [], set()
    for sp in seed_paths:
        seeded |= parse_seeded_roles(mask_sql(open(sp, encoding="utf-8", errors="replace").read()))
    for path in paths:
        rel = os.path.relpath(path, mig_dir)
        raw = open(path, encoding="utf-8", errors="replace").read()
        masked = mask_sql(raw)
        lines = LineIndex(raw)
        files.append({
            "path": rel,
            "sha256": hashlib.sha256(raw.encode("utf-8")).hexdigest(),
        })
        seeded |= parse_seeded_roles(masked)
        for m in INSERT_ROLE_MENU_RE.finditer(masked):
            end = statement_end(masked, m.start())
            stmt = masked[m.start():end]
            codes, dynamic = role_literals_in_statement(stmt, m.start(), lines)
            statements.append({
                "file": rel,
                "line": lines.line_of(m.start()),
                "kind": "INSERT INTO sys_role_menu",
                "codes": codes,
                "dynamic": dynamic,
                "exempt_reason": exempt_reason(raw, lines, m.start(), end),
            })
        for blk in values_role_codes(masked, lines):
            statements.append({
                "file": rel,
                "line": blk["line"],
                "kind": "角色码矩阵块（temp 表 / CTE）",
                "codes": blk["codes"],
                "dynamic": False,
                "exempt_reason": exempt_reason(raw, lines, blk["start"], blk["end"]),
            })
    return files, statements, seeded, None



# ============================================================================
# 基线与裁决
# ============================================================================

def load_baseline(path: str):
    """→ (dict path→{sha256, codes}, error|None)。文件不存在时返回空基线（不报错：首次上线）。"""
    if not os.path.exists(path):
        return {"files": {}}, None
    try:
        data = json.load(open(path, encoding="utf-8"))
    except Exception as e:  # noqa: BLE001
        return None, f"基线文件损坏（{path}）：{e}"
    if not isinstance(data, dict) or not isinstance(data.get("files"), dict):
        return None, f"基线文件结构不对（{path}）：缺少 files 字典"
    return data, None


def evaluate(files, statements, seeded, baseline):
    """→ report dict。纯函数，便于自检。"""
    whitelist = set(PROD_ACTIVE_ROLES) | set(seeded)
    base = baseline.get("files", {})
    by_path = {f["path"]: f for f in files}

    baseline_files = set(base)
    stale_baseline = sorted(p for p in baseline_files if p not in by_path)
    tampered = sorted(p for p in baseline_files
                      if p in by_path and base[p].get("sha256") != by_path[p]["sha256"])

    known, unknown, exempted, bad_exempt = [], [], [], []
    for st in statements:
        if not st["codes"]:
            continue
        codes = [(c, ln) for c, ln in st["codes"] if c not in whitelist]
        if not codes:
            continue
        reason = st["exempt_reason"]
        if reason is not None:
            if len(reason) < 8:
                bad_exempt.append(dict(st, codes=codes, why="ROLE-EXEMPT 理由太短（<8 字符）"))
            else:
                exempted.append(dict(st, codes=codes, why=reason))
            continue
        f = by_path.get(st["file"])
        in_base = st["file"] in base
        if in_base and f and base[st["file"]].get("sha256") == f["sha256"]:
            known.append(dict(st, codes=codes))
        else:
            unknown.append(dict(st, codes=codes,
                                why="改动历史迁移（sha256 与基线不符）" if in_base else "新增文件"))

    used = {"statements": len(statements),
            "with_codes": len([s for s in statements if s["codes"]]),
            "dynamic": len([s for s in statements if s.get("dynamic")]),
            "whitelist": whitelist,
            "seeded": sorted(seeded),
            "baseline_files": len(baseline_files),
            "stale_baseline": stale_baseline,
            "tampered": tampered,
            "known": known, "unknown": unknown,
            "exempted": exempted, "bad_exempt": bad_exempt,
            "policy": policy_warnings(statements, baseline),
            "policy_baselined": len([s for s in statements if s["file"] in base]),
            "files": len(files)}
    if bad_exempt or unknown or tampered or stale_baseline:
        used["rc"] = RC_FAIL
    else:
        used["rc"] = RC_OK
    return used


POLICY_HINT = (
    "授权段优先走 V0258 业务角色矩阵（改矩阵）而不是在新迁移里再写一份："
    "空库上 sys_role 还没种出业务角色 ⇒ 写死的 role_code 只会静默 0 行"
)

VERSION_RE = re.compile(r"/V(\d{4})__", re.I)


def file_version(rel_path: str):
    """`business/V0259__xxx.up.sql` → 259；命名不合规 → None。"""
    m = VERSION_RE.search("/" + rel_path)
    return int(m.group(1)) if m else None


def policy_warnings(statements, baseline):
    """策略检查（只 WARN，**不参与退出码**）：新增迁移里出现 `INSERT INTO sys_role_menu`。

    为什么是一条独立提示而不是判红：授权段本身可以是**对的**（角色码真实、主闸放行），
    但「在新迁移里手写一份 WHERE role_code IN (…)」这件事在装配路径上依然脆弱 ——
    V0258 已经把生产在生效的业务矩阵冻结成一条版本化迁移，正确做法是改矩阵（或用矩阵里的
    真实角色码）。空库上压根还没有业务角色，手写清单只是「看起来授了」。

    判据：基线里的 `policy.frozen_max_version`（冻结那天的最大迁移号）之后的**新文件**才出声：
      · 冻结前就存在的迁移（存量 43 条只授 SUPER_ADMIN 的授权段）不重复告警 ——
        每跑一次检查刷 43 行 WARN 会把真正的新增淹掉，门禁就此变成噪音；
      · 基线 `files` 里登记的文件（带未知角色码的 25 个）永远不在此列（主闸管它们）；
      · 没有 meta（老基线 / 手工构造）⇒ 退回「凡不在基线 files 里就告警」，宁噪不漏。

    → [{"file","line","kind","codes"}]
    """
    base = baseline.get("files", {}) if baseline else {}
    cutoff_raw = ((baseline or {}).get("policy") or {}).get("frozen_max_version")
    cutoff = None
    if cutoff_raw:
        try:
            cutoff = int(str(cutoff_raw).lstrip("Vv"))
        except ValueError:
            cutoff = None
    out = []
    for st in statements:
        if st["file"] in base:
            continue
        if cutoff is not None:
            v = file_version(st["file"])
            if v is not None and v <= cutoff:
                continue
        out.append({"file": st["file"], "line": st["line"],
                    "kind": st["kind"], "codes": [c for c, _ln in st["codes"]],
                    "dynamic": bool(st.get("dynamic"))})
    return out



def write_baseline(root: str, files, statements, force: bool = False) -> int:
    """按当前树冻结历史基线。**不是「跑红了就重跑一次」的修法** —— 带防呆。

    防呆：已有基线时，若本次会**新增**被豁免的文件（= 把刚写的、带未知角色码的新迁移
    一起吞进基线），必须显式 `--force`（人工评审后），否则拒绝写入并列出文件名。
    这也顺带挡住「为了过闸去改历史迁移再重冻基线」这条错误路径（那些文件 sha 变了，
    重冻会把基线偷偷改宽）。
    """
    whitelist = set(PROD_ACTIVE_ROLES)
    _f, _s, seeded, err = collect(root)
    whitelist |= seeded
    per_file = {}
    for st in statements:
        bad = [c for c, _ln in st["codes"] if c not in whitelist]
        if bad and st["exempt_reason"] is None:
            per_file.setdefault(st["file"], set()).update(bad)

    out = os.path.join(root, "scripts", "verify", BASELINE_NAME)
    old = {}
    if os.path.exists(out):
        prev, perr = load_baseline(out)
        if perr or prev is None:
            print(f"✗ 旧基线读不出来，拒绝覆盖：{perr}", file=sys.stderr)
            return RC_ENV
        old = prev.get("files", {})
        added = sorted(set(per_file) - set(old))
        widened = sorted(p for p in set(per_file) & set(old)
                         if not set(per_file[p]) <= set(old[p].get("codes") or []))
        if (added or widened) and not force:
            print("✗ 拒绝写入：本次冻结会**放宽**基线（把新的未知角色码吞进去）", file=sys.stderr)
            for p in added:
                print(f"  · 新增文件 backend/migrations/{p} → {sorted(per_file[p])}", file=sys.stderr)
            for p in widened:
                print(f"  · 码范围变宽 backend/migrations/{p} → {sorted(per_file[p])}", file=sys.stderr)
            print("  正确修法：把新迁移里的 code 换成真实角色码（或先 INSERT INTO sys_role 建角色）。",
                  file=sys.stderr)
            print("  确属历史补录（评审过），加 --force 重新冻结。", file=sys.stderr)
            return RC_FAIL

    versions = [file_version(f["path"]) for f in files]
    versions = [v for v in versions if v is not None]
    data = {
        "_doc": [
            "迁移授权段角色码闸的历史基线：只豁免以下文件，且必须 sha256 逐字一致。",
            "文件一旦被改动（hash 不符）⇒ 该文件的未知角色码全部重报（已应用迁移不可改）。",
            "生成/更新：python3 scripts/verify/role_code_gate.py --write-baseline [--force]",
            "新增文件**不能**进基线 —— 新迁移必须用真实角色码（见 docs 与脚本头注释）。",
            "policy.frozen_max_version：策略 WARN 的静音边界，该号之后的迁移写 sys_role_menu 才告警。",
        ],
        "policy": {
            "frozen_max_version": f"V{max(versions):04d}" if versions else None,
            "frozen_at": time.strftime("%Y-%m-%d"),
            "files_at_freeze": len(files),
        },
        "files": {p: {"sha256": next(f["sha256"] for f in files if f["path"] == p),
                      "codes": sorted(codes)} for p, codes in sorted(per_file.items())},
    }
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(data, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    print(f"已写入基线：{out}（{len(data['files'])} 个文件；"
          f"policy.frozen_max_version={data['policy']['frozen_max_version']}）")
    return RC_OK


# ============================================================================
# 自检（不读仓库、不连库）
# ============================================================================

FIXTURES = [
    # (名字, SQL, 期望命中的 (code, line) 集合, 期望豁免理由)
    ("基本 IN 列表（多行）",
     "INSERT INTO sys_role_menu (role_id, menu_id)\n"
     "SELECT r.id, m.id FROM sys_role r CROSS JOIN sys_menu m\n"
     " WHERE r.role_code IN ('SUPER_ADMIN',\n"
     "       'ADMIN') AND m.deleted_at IS NULL;\n",
     [("SUPER_ADMIN", 3), ("ADMIN", 4)], None),
    ("单值 = 比较",
     "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, 'x'::uuid FROM sys_role r"
     " WHERE r.role_code = 'ACCOUNTANT';\n",
     [("ACCOUNTANT", 1)], None),
    ("<> 反选也要查（写错码会退化成授给所有人）",
     "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, 'x'::uuid FROM sys_role r"
     " WHERE r.role_code <> 'TEST_UADMIN';\n",
     [("TEST_UADMIN", 1)], None),
    ("注释掉的码不算",
     "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
     " WHERE 1 = 1 -- AND r.role_code = 'ADMIN'\n   AND r.role_code = 'DEPT_LEADER';\n",
     [("DEPT_LEADER", 2)], None),
    ("字符串里的分号不截断语句",
     "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, 'a;b'::uuid FROM sys_role r"
     " WHERE r.role_code = 'WAREHOUSE';\n",
     [("WAREHOUSE", 1)], None),
    ("豁免注释（写在 INSERT 上方 2 行）",
     "-- ROLE-EXEMPT: 历史授权段，生产在此码上线前已手工授过\n"
     "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, 'x'::uuid FROM sys_role r"
     " WHERE r.role_code = 'FINANCE';\n",
     [("FINANCE", 2)], "历史授权段，生产在此码上线前已手工授过"),
    ("NOT IN 列表",
     "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, 'x'::uuid FROM sys_role r"
     " WHERE r.role_code NOT IN ('SUPER_ADMIN','BUYER');\n",
     [("SUPER_ADMIN", 1), ("BUYER", 1)], None),
]

SELF_CASES = [
    # (未知命中数, bad_exempt 数, tampered 数, stale 数, 期望退出码, 说明)
    (0, 0, 0, 0, RC_OK, "全部覆盖 → 绿"),
    (1, 0, 0, 0, RC_FAIL, "1 处新增未知码 → 红"),
    (0, 1, 0, 0, RC_FAIL, "ROLE-EXEMPT 没写理由 → 红"),
    (0, 0, 1, 0, RC_FAIL, "历史基线文件被改动 → 红"),
    (0, 0, 0, 1, RC_FAIL, "基线里的文件已不存在 → 红（基线漂移需显式收敛）"),
]


def self_test() -> bool:
    bad = 0
    total = 0
    print("=" * 92)
    print("判据自检：SQL 解析 + 豁免 + verdict 红/绿语料（不读仓库、不连库）")
    print("=" * 92)

    total += 1
    ok = {"SUPER_ADMIN", "DEPT_LEADER"} <= PROD_ACTIVE_ROLES
    bad += 0 if ok else 1
    print(f"{'✔' if ok else '✘'} 白名单含生产活角色（SUPER_ADMIN / DEPT_LEADER）")

    total += 1
    ok = not ({"ADMIN", "ACCOUNTANT", "FINANCE", "TEST_UADMIN"} & PROD_ACTIVE_ROLES)
    bad += 0 if ok else 1
    print(f"{'✔' if ok else '✘'} 白名单**不含**历史孤儿码（ADMIN / ACCOUNTANT / FINANCE / TEST_UADMIN）")

    for name, sql, want, want_reason in FIXTURES:
        masked = mask_sql(sql)
        lines = LineIndex(sql)
        got, reason, dyn = [], None, False
        for m in INSERT_ROLE_MENU_RE.finditer(masked):
            end = statement_end(masked, m.start())
            hits, d = role_literals_in_statement(masked[m.start():end], m.start(), lines)
            got += hits
            dyn = dyn or d
            if reason is None:
                reason = exempt_reason(sql, lines, m.start(), end)
        total += 1
        ok = got == want and reason == want_reason
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 解析：{name} → {got}"
              + (f"  exempt={reason!r}" if reason else ""))

    # 写法形状：换一种**合法**写法不能绕过闸（否则闸只是摆设）
    # (名字, SQL, 期望字面量, 期望 dynamic, 期望被识别为 sys_role_menu 语句)
    SHAPES = [
        ("role_code = ANY (ARRAY[...])",
         "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
         " WHERE r.role_code = ANY (ARRAY['ADMIN','FINANCE']);",
         [("ADMIN", 1), ("FINANCE", 1)], False, True),
        ("schema 限定 public.sys_role_menu",
         "INSERT INTO public.sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
         " WHERE r.role_code = 'ADMIN';",
         [("ADMIN", 1)], False, True),
        ("INSERT INTO ONLY sys_role_menu",
         "INSERT INTO ONLY sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
         " WHERE r.role_code = 'ADMIN';",
         [("ADMIN", 1)], False, True),
        ("IN (SELECT …)：子查询里的 '%财务%' 不能当角色码判红",
         "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
         " WHERE r.role_code IN (SELECT role_code FROM sys_role WHERE role_name LIKE '%财务%');",
         [], True, True),
        ("lower(role_code) LIKE：静态判不了 ⇒ dynamic（既不判绿也不判红）",
         "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
         " WHERE lower(r.role_code) LIKE '%admin%';",
         [], True, True),
        ("注释里的 sys_role_menu 不触发（否则误报）",
         "-- INSERT INTO sys_role_menu 已废弃\nSELECT 1;",
         [], False, False),
        ("(role_code) IN (('ADMIN'),…)：给列加括号也拦得住",
         "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
         " WHERE (r.role_code) IN (('ADMIN'),('FINANCE'));",
         [("ADMIN", 1), ("FINANCE", 1)], False, True),
        ("脚本不认识的写法 ⇒ 兜底 dynamic（有 WARN，不静默绿）",
         "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
         " WHERE r.id IN (SELECT id FROM sys_role WHERE role_code = s.code);",
         [], True, True),
    ]
    for name, sql, want, want_dyn, want_stmt in SHAPES:
        masked = mask_sql(sql)
        lines = LineIndex(sql)
        ms = list(INSERT_ROLE_MENU_RE.finditer(masked))
        got, dyn = [], False
        for m in ms:
            end = statement_end(masked, m.start())
            hits, d = role_literals_in_statement(masked[m.start():end], m.start(), lines)
            got += hits
            dyn = dyn or d
        total += 1
        ok = got == want and dyn == want_dyn and bool(ms) == want_stmt
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 形状：{name} → hits={got} dynamic={dyn} 语句识别={bool(ms)}")

    # 矩阵 / CTE 里的角色码（V0258 模板：先灌 temp 表再 INSERT INTO sys_role_menu）
    MATRIX_FIXTURES = [
        ("temp 表矩阵块",
         "CREATE TEMP TABLE m (role_code text, auth_code text);\n"
         "INSERT INTO m (role_code, auth_code) VALUES\n"
         "    ('DEPT_LEADER','a:b:c'), ('ADMIN','x:y:z');\n"
         "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, s.menu_id FROM m s"
         " JOIN sys_role r ON r.role_code = s.role_code;\n",
         [("DEPT_LEADER", 3), ("ADMIN", 3)]),
        ("CTE 矩阵块 `t(role_code) AS (VALUES …)`",
         "WITH mm (role_code, auth_code) AS (VALUES ('SUPER_ADMIN','a:b:c'), ('WAREHOUSE','d:e:f'))\n"
         "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM mm"
         " JOIN sys_role r ON r.role_code = mm.role_code;\n",
         [("SUPER_ADMIN", 1), ("WAREHOUSE", 1)]),
        ("角色定义块不进矩阵扫描（INSERT INTO sys_role 由种子解析负责）",
         "INSERT INTO sys_role (id, role_code, role_name, description, status) VALUES"
         " ('u1'::uuid, 'BRANCH_MANAGER', 'x', 'y', 1);\n",
         []),
    ]
    for name, sql, want in MATRIX_FIXTURES:
        masked = mask_sql(sql)
        got = [(c, ln) for blk in values_role_codes(masked, LineIndex(sql))
               for c, ln in blk["codes"]]
        total += 1
        ok = [c for c, _ in got] == [c for c, _ in want]
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 解析：{name} → {[c for c, _ in got]}")

    # 种子角色解析
    seed_sql = ("INSERT INTO sys_role (id, role_code, role_name, description, status)\nVALUES\n"
                "    ('u1'::uuid, 'NEW_MANAGER', '新经理', 'x', 1),\n"
                "    ('u2'::uuid, 'OTHER_DIRECTOR', '新总监', 'y', 1);\n")
    total += 1
    got = parse_seeded_roles(mask_sql(seed_sql))
    ok = got == {"NEW_MANAGER", "OTHER_DIRECTOR"}
    bad += 0 if ok else 1
    print(f"{'✔' if ok else '✘'} 解析：INSERT INTO sys_role 种子角色 → {sorted(got)}")

    for n_unknown, n_bad_exempt, n_tampered, n_stale, expect, label in SELF_CASES:
        rep = {"known": [], "unknown": [{}] * n_unknown,
               "bad_exempt": [{}] * n_bad_exempt, "exempted": [],
               "tampered": ["f"] * n_tampered, "stale_baseline": ["g"] * n_stale}
        rc = (RC_FAIL if (rep["unknown"] or rep["bad_exempt"] or rep["tampered"]
                          or rep["stale_baseline"]) else RC_OK)
        total += 1
        ok = rc == expect
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} verdict → {rc}（期望 {expect}）：{label}")

    # 端到端：在临时目录里跑 collect + evaluate（真实文件 IO，覆盖基线 sha256 逻辑）
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        mig = os.path.join(td, MIGRATIONS_REL, "business")
        os.makedirs(mig)
        good = os.path.join(mig, "V0001__ok.up.sql")
        old = os.path.join(mig, "V0002__legacy.up.sql")
        open(good, "w", encoding="utf-8").write(
            "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
            " WHERE r.role_code IN ('SUPER_ADMIN','DEPT_LEADER');\n")
        open(old, "w", encoding="utf-8").write(
            "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
            " WHERE r.role_code IN ('SUPER_ADMIN','ADMIN');\n")
        files, stmts, seeded, err = collect(td)
        total += 1
        ok = err is None and len(files) == 2 and len(stmts) == 2
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 端到端：collect 扫到 {len(files)} 个文件 / {len(stmts)} 条授权语句")

        base = {"files": {os.path.join("business", "V0002__legacy.up.sql"):
                          {"sha256": next(f["sha256"] for f in files
                                          if f["path"].endswith("V0002__legacy.up.sql")),
                           "codes": ["ADMIN"]}}}
        rep = evaluate(files, stmts, seeded, base)
        total += 1
        ok = rep["rc"] == RC_OK and len(rep["known"]) == 1 and not rep["unknown"]
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 端到端：历史文件被基线豁免 → rc={rep['rc']}（期望 0）")

        # 改动历史文件 ⇒ sha256 不符 ⇒ 必须判红
        open(old, "w", encoding="utf-8").write(
            "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
            " WHERE r.role_code IN ('SUPER_ADMIN','ADMIN','FINANCE');\n")
        files2, stmts2, seeded2, _ = collect(td)
        rep2 = evaluate(files2, stmts2, seeded2, base)
        total += 1
        ok = rep2["rc"] == RC_FAIL and len(rep2["tampered"]) == 1 and len(rep2["unknown"]) == 1
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 端到端：改动历史迁移 ⇒ 判红（tampered={len(rep2['tampered'])}"
              f" / unknown={len(rep2['unknown'])}）→ rc={rep2['rc']}（期望 1）")

        # 新增文件带合法种子角色 ⇒ 绿
        open(os.path.join(mig, "V0003__new.up.sql"), "w", encoding="utf-8").write(
            "INSERT INTO sys_role (id, role_code, role_name, description, status) VALUES"
            " ('u9'::uuid, 'BRANCH_MANAGER', '分公司经理', 'x', 1);\n"
            "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
            " WHERE r.role_code = 'BRANCH_MANAGER';\n")
        files3, stmts3, seeded3, _ = collect(td)
        rep3 = evaluate(files3, stmts3, seeded3, base)
        total += 1
        ok = rep3["rc"] == RC_FAIL  # 因为 V0002 还是改过的
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 端到端：新文件 + 自建角色不因白名单判红"
              f"（unknown={len(rep3['unknown'])}，其中 NEW 文件 0 处）")
        total += 1
        ok = all(u["file"].endswith("V0002__legacy.up.sql") for u in rep3["unknown"])
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 端到端：判红只落在改动的历史文件上")

        # 策略检查（WARN）：基线内文件不出声，新增文件出声；且 WARN 不影响退出码
        total += 1
        pol_files = sorted({w["file"] for w in rep3["policy"]})
        ok = (all(not f.endswith("V0002__legacy.up.sql") for f in pol_files)
              and len(rep3["policy"]) == 2)
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 策略检查：基线内文件不再 WARN，新增文件 {len(rep3['policy'])} 条被点名"
              f" → {pol_files}")

    # 全绿 + 策略 WARN 同时存在时必须仍是 rc=0（WARN 绝不改退出码）
    with tempfile.TemporaryDirectory() as td:
        mig = os.path.join(td, MIGRATIONS_REL, "business")
        os.makedirs(mig)
        open(os.path.join(mig, "V0001__legal.up.sql"), "w", encoding="utf-8").write(
            "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
            " WHERE r.role_code = 'FINANCE_MANAGER';\n")
        files4, stmts4, seeded4, _ = collect(td)
        rep4 = evaluate(files4, stmts4, seeded4, {"files": {}})
        total += 1
        ok = rep4["rc"] == RC_OK and len(rep4["policy"]) == 1
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 策略 WARN 不改退出码：rc={rep4['rc']}（期望 0）"
              f"，policy={len(rep4['policy'])} 条（期望 1）")

    # 策略 WARN 的静音边界：冻结号之前的老迁移不告警（否则存量 43 条会淹掉新增）
    rep5 = evaluate(files4, stmts4, seeded4,
                    {"files": {}, "policy": {"frozen_max_version": "V0001"}})
    total += 1
    ok = len(rep5["policy"]) == 0
    bad += 0 if ok else 1
    print(f"{'✔' if ok else '✘'} 策略静音边界：frozen_max_version=V0001 时 V0001 不再 WARN"
          f"（policy={len(rep5['policy'])}，期望 0）")

    # 种子定义的角色也算真实角色（否则「新迁移授权给 S__*.sql 造的角色」会被误判）
    with tempfile.TemporaryDirectory() as td3:
        seed_dir = os.path.join(td3, MIGRATIONS_REL, "seed")
        bus_dir = os.path.join(td3, MIGRATIONS_REL, "business")
        os.makedirs(seed_dir)
        os.makedirs(bus_dir)
        open(os.path.join(seed_dir, "S__role_data.sql"), "w", encoding="utf-8").write(
            "INSERT INTO sys_role (id, role_code, role_name, description, status) VALUES\n"
            " ('u1'::uuid, 'SEED_ONLY_ROLE', '种子角色', 'x', 1);\n")
        open(os.path.join(bus_dir, "V0001__grant.up.sql"), "w", encoding="utf-8").write(
            "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
            " WHERE r.role_code = 'SEED_ONLY_ROLE';\n")
        f7, s7, se7, _e7 = collect(td3)
        r7 = evaluate(f7, s7, se7, {"files": {}})
        total += 1
        ok = r7["rc"] == RC_OK and "SEED_ONLY_ROLE" in se7
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 种子角色并入白名单：seeded={sorted(se7)} → rc={r7['rc']}（期望 0）")

    # `INSERT INTO sys_role (…) SELECT … WHERE NOT EXISTS`（本仓幂等种子的真实写法）也算种子角色
    with tempfile.TemporaryDirectory() as td3b:
        bus = os.path.join(td3b, MIGRATIONS_REL, "business")
        os.makedirs(bus)
        open(os.path.join(bus, "V0001__role.up.sql"), "w", encoding="utf-8").write(
            "INSERT INTO sys_role (id, role_code, role_name, description, status, data_scope, sort_order)\n"
            "SELECT '20000000-0000-0000-0000-0000000f0016'::uuid, 'SELECT_FORM_ROLE', '选择形态角色',\n"
            "       'x', 1, 3, 25\n"
            " WHERE NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'SELECT_FORM_ROLE' AND deleted_at IS NULL);\n")
        open(os.path.join(bus, "V0002__grant.up.sql"), "w", encoding="utf-8").write(
            "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r\n"
            " WHERE r.role_code = 'SELECT_FORM_ROLE';\n")
        f7b, s7b, se7b, _e7b = collect(td3b)
        r7b = evaluate(f7b, s7b, se7b, {"files": {}})
        total += 1
        ok = r7b["rc"] == RC_OK and "SELECT_FORM_ROLE" in se7b
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} `INSERT … SELECT … WHERE NOT EXISTS` 种的角色并入白名单："
              f"seeded={sorted(se7b)} → rc={r7b['rc']}（期望 0）")

    # 基线防呆：已有基线时，新增带未知码的文件 → 拒绝重冻（除非 --force）
    with tempfile.TemporaryDirectory() as td2:
        mig = os.path.join(td2, MIGRATIONS_REL, "business")
        os.makedirs(mig)
        os.makedirs(os.path.join(td2, "scripts", "verify"))
        legacy = os.path.join(mig, "V0002__legacy.up.sql")
        open(legacy, "w", encoding="utf-8").write(
            "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
            " WHERE r.role_code = 'ADMIN';\n")
        f5, s5, _se5, _ = collect(td2)
        total += 1
        ok = write_baseline(td2, f5, s5) == RC_OK
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 基线：首次冻结写入成功")

        open(os.path.join(mig, "V0260__bad_new.up.sql"), "w", encoding="utf-8").write(
            "INSERT INTO sys_role_menu (role_id, menu_id) SELECT r.id, m.id FROM sys_role r"
            " WHERE r.role_code = 'ADMIN';\n")
        f6, s6, _se6, _ = collect(td2)
        bl = os.path.join(td2, "scripts", "verify", BASELINE_NAME)
        total += 1
        ok = write_baseline(td2, f6, s6) == RC_FAIL
        bad += 0 if ok else 1
        after = json.load(open(bl, encoding="utf-8"))
        ok = ok and len(after["files"]) == 1
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 基线防呆：新迁移带 'ADMIN' 时拒绝重冻（文件数仍 ={len(after['files'])}，期望 1）")

        total += 1
        ok = write_baseline(td2, f6, s6, force=True) == RC_OK
        bad += 0 if ok else 1
        after2 = json.load(open(bl, encoding="utf-8"))
        ok = ok and len(after2["files"]) == 2 and after2["policy"]["frozen_max_version"] == "V0260"
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 基线防呆：--force 才写入（文件数 ={len(after2['files'])}，期望 2；"
              f"冻结号 {after2['policy']['frozen_max_version']}，期望 V0260）")

    print("-" * 92)
    print(f"{total - bad}/{total} 通过" + ("" if not bad else f"；✗ {bad} 条不符"))
    return bad == 0

# ============================================================================
# main
# ============================================================================

def main() -> int:
    ap = argparse.ArgumentParser(description="迁移授权段角色码常驻闸（静态扫描，不连库）")
    ap.add_argument("--root", default=None, help="仓库根（默认按脚本位置推两级）")
    ap.add_argument("--json", action="store_true", help="JSON 输出（给 CI / 监控）")
    ap.add_argument("--self-test", action="store_true", dest="selftest", help="判据自检，不读仓库")
    ap.add_argument("--write-baseline", action="store_true", dest="write_baseline",
                    help="按当前树重新冻结历史基线（只应在首次上线/新增历史文件时用）")
    ap.add_argument("--force", action="store_true",
                    help="配合 --write-baseline：允许把本次新增的豁免文件写进基线（需评审）")
    args = ap.parse_args()

    if args.selftest:
        return RC_OK if self_test() else RC_FAIL

    root = args.root or os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    files, statements, seeded, err = collect(root)
    if err:
        print(f"✗ {err}", file=sys.stderr)
        print("  （按环境错误退出 2，不判绿：路径/目录不对时静默 0 命中就是假绿）", file=sys.stderr)
        return RC_ENV

    if args.write_baseline:
        return write_baseline(root, files, statements, force=args.force)

    baseline_path = os.path.join(root, "scripts", "verify", BASELINE_NAME)
    baseline, berr = load_baseline(baseline_path)
    if berr:
        print(f"✗ {berr}", file=sys.stderr)
        return RC_ENV

    rep = evaluate(files, statements, seeded, baseline)

    if args.json:
        print(json.dumps({
            "ok": rep["rc"] == RC_OK,
            "exit": rep["rc"],
            "files_scanned": rep["files"],
            "role_menu_statements": rep["statements"],
            "statements_with_role_literal": rep["with_codes"],
            "whitelist": sorted(rep["whitelist"]),
            "seeded_roles": rep["seeded"],
            "baseline_files": rep["baseline_files"],
            "tampered_baseline_files": rep["tampered"],
            "stale_baseline_files": rep["stale_baseline"],
            "unknown": [{"file": f"backend/migrations/{u['file']}", "line": u["line"],
                         "codes": [c for c, _ in u["codes"]], "why": u["why"]}
                        for u in rep["unknown"]],
            "baseline_exempted_statements": len(rep["known"]),
            "exempt_comment_statements": len(rep["exempted"]),
            "policy_warnings": [{"file": f"backend/migrations/{u['file']}", "line": u["line"],
                                 "kind": u["kind"], "codes": u["codes"],
                                 "dynamic": u.get("dynamic", False)}
                                for u in rep["policy"]],
            "policy_baselined_statements": rep["policy_baselined"],
            "bad_exempt": [{"file": f"backend/migrations/{u['file']}", "line": u["line"],
                            "why": u["why"]} for u in rep["bad_exempt"]],
        }, ensure_ascii=False, indent=2))
        return rep["rc"]

    print("=" * 92)
    print("迁移授权段角色码闸 · role_code_gate.py（INSERT INTO sys_role_menu 的 role_code 必须真实存在）")
    print("=" * 92)
    print(f"扫描面：{rep['files']} 个 *.up.sql / {rep['statements']} 条 INSERT INTO sys_role_menu 语句"
          f" / {rep['with_codes']} 条带 role_code 字面量"
          f" / {rep['dynamic']} 条动态匹配（LIKE / 子查询，只能 WARN）")
    print(f"白名单：{len(rep['whitelist'])} 个角色"
          f"（生产活角色 {len(PROD_ACTIVE_ROLES)} + 迁移种子 {len(rep['seeded'])}："
          f"{', '.join(rep['seeded']) or '无'}）")
    print(f"历史基线：{rep['baseline_files']} 个文件（sha256 锁定，改动即失效）")
    print("-" * 92)

    if rep["bad_exempt"]:
        print(f"✗ ROLE-EXEMPT 写法不合格（理由必填，≥8 字符）{len(rep['bad_exempt'])} 处：")
        for u in rep["bad_exempt"]:
            print(f"  · backend/migrations/{u['file']}:{u['line']}  {u['why']}")
    if rep["tampered"]:
        print(f"✗ 历史基线文件被改动（已应用迁移不可改 ⇒ 原豁免失效）{len(rep['tampered'])} 个：")
        for p in rep["tampered"]:
            print(f"  · backend/migrations/{p}")
        print("  若确属「路径/换行符」类无害变更，核对后跑 --write-baseline 重新冻结（需评审）。")
    if rep["stale_baseline"]:
        print(f"✗ 基线里登记的迁移文件已不存在 {len(rep['stale_baseline'])} 个（基线漂移）：")
        for p in rep["stale_baseline"]:
            print(f"  · backend/migrations/{p}")
        print("  历史迁移只能新增不能删；确属误登记就跑 --write-baseline 收敛。")
    if rep["unknown"]:
        print(f"✗ 未知角色码 {sum(len(u['codes']) for u in rep['unknown'])} 处"
              f"（{len(rep['unknown'])} 条语句）—— 这些码在 sys_role 里不存在，授权会**静默 0 行**：")
        for u in rep["unknown"]:
            tag = "，".join(f"'{c}'@L{ln}" for c, ln in u["codes"])
            print(f"  · backend/migrations/{u['file']}:{u['line']} → {tag}   [{u['why']}]")
        print("  修法：① 换成生产真实角色码（`select role_code from sys_role where deleted_at is null；"
              "见 docs/后端二开指南.md`「迁移授权段怎么拿角色码」）；")
        print("        ② 新业务角色 ⇒ 先在迁移里 `INSERT INTO sys_role` 建出来，再在同一/后续迁移里授；")
        print("        ③ 确属历史个例 ⇒ 在上方写 `-- ROLE-EXEMPT: <为什么这个码可以不存在>`（≥8 字符理由）。")
    if rep["exempted"]:
        print(f"ℹ 显式豁免（ROLE-EXEMPT 注释）{len(rep['exempted'])} 条语句：")
        for u in rep["exempted"][:10]:
            print(f"  · backend/migrations/{u['file']}:{u['line']}  {u['why']}")
    if rep["known"]:
        print(f"ℹ 历史基线豁免 {len(rep['known'])} 条语句（存量授权段，文件指纹已锁）。")
    if rep["policy"]:
        print(f"⚠ 策略提示（WARN，不判红）：新增迁移里有 {len(rep['policy'])} 条 "
              f"`INSERT INTO sys_role_menu` 授权段：")
        for u in rep["policy"][:10]:
            codes = "，".join(f"'{c}'" for c in u["codes"]) or "（无字面量，按子查询/矩阵）"
            tail = "  ← 动态表达式（LIKE / 子查询），闸无法静态验证角色码，请人工确认" if u.get("dynamic") else ""
            print(f"  · backend/migrations/{u['file']}:{u['line']}  {u['kind']}  {codes}{tail}")
        if len(rep["policy"]) > 10:
            print(f"  … 另有 {len(rep['policy']) - 10} 条，加 --json 看全量")
        print(f"  {POLICY_HINT}；见 docs/architecture/P2-新装配库权限矩阵-决策.md")
    elif rep["policy_baselined"]:
        print(f"ℹ 策略提示：{rep['policy_baselined']} 条历史授权段已在基线内，不重复告警"
              f"（新增迁移再写授权段会在这里 WARN）。")

    if rep["rc"] == RC_OK:
        print("✓ 无**新增/改动**的未知角色码（历史基线全部按指纹豁免）")
    return rep["rc"]


if __name__ == "__main__":
    sys.exit(main())
