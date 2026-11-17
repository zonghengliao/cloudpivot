#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""L0 门禁：**版本化迁移不得把「种子行」当作前置条件**。

## 为什么需要这条门禁（一次真实的 P0）

t_08e1816a 实测：V0442 的前置护栏断言了「系统管理」目录行
（`1000…0001`）存在，而这一行**只**由种子迁移 `migrations/seed/S__menu_data.sql` 建出；
内核的执行次序固定为

    baseline(0) → versioned(1) → repeatable(2) → seed(3)
    （vendor/nik-db/src/migrate/kind.rs::execution_rank）

⇒ **任何空库首启跑到这条迁移时，那一行一定还不存在**，于是启动直接失败：

    ERROR nik_web::app: 数据库迁移失败: query failed: … V0442: 祖先目录「系统管理」不存在
    FATAL: 服务启动失败: 配置错误: 数据库迁移失败

代价不是一条用例红，而是所有「新库」通不了：`make_full_suite_db.sh`（全量 lib 套件的
标准库）造不出来、L3 干净检出闸、新客户部署全被挡；而且 **V0443 救不了**（442 先炸，
内核按版本升序执行，443 永远轮不到）。

## 判据（只管「不会失败的引用」之外的硬断言）

扫 `backend/app/migrations/{system,support,business}/**/*.up.sql`，对每个
`DO $$ … $$` 块：

1. 找 `SELECT count(*) INTO <v> … WHERE … id = '<uuid>'` 这类**行存在性检查**
   （把 `<uuid>` 关联到变量 `<v>`：取该 uuid 之前最近的 `INTO`）；
2. 该 uuid 之后紧跟 `IF <v> <> 1 THEN … RAISE EXCEPTION`（或 `IF <v> = 0 THEN … RAISE`）
   ⇒ 这是**会失败**的存在性断言；
3. 若这个 `<uuid>` **从未被任何版本化迁移以 `id` 列 INSERT 过** ⇒ 判红。

第 3 步的「产出集合」按文件解析：`INSERT INTO <表> (id, …)` 的每个元组首个值
（支持 `VALUES (…),(…)`、`INSERT … SELECT '<id>'::uuid, …` 与
`INSERT … SELECT v.id … FROM (VALUES (…))` 三种写法，本仓都在用）。
**seed 目录不参与产出集合** —— 这正是不许依赖的那一侧。

三类引用**不判红**（它们不会让迁移失败）：
`IF <v> = 0 THEN RAISE NOTICE`（空库首启的正常分支）、`UPDATE/DELETE … WHERE id=`、
纯 `JOIN … ON dir.id = p.parent_id` 这类动态查找。

## 豁免

确有理由的（例如「断言某个**版本化**迁移建出的哨兵行」被误判）写进
`scripts/lint/seed_dependency_allow.txt`，每行
`<相对路径>|<uuid>|<理由>`（`#` 开头为注释）。**没有理由的豁免一律不收。**

## 用法

    python3 scripts/lint/seed_dependency_gate.py            # 扫描（默认仓根 = 本脚本上两级）
    python3 scripts/lint/seed_dependency_gate.py --root .
    python3 scripts/lint/seed_dependency_gate.py --selftest # 判据自检（红/绿语料）
    python3 scripts/lint/seed_dependency_gate.py --verbose

退出码：0 = 通过（或只有 --verbose 才看的「幽灵 id」提示）；1 = 有未豁免的种子行依赖。
"""

import argparse
import re
import sys
from pathlib import Path

VERSIONED_DIRS = ("system", "support", "business")
SEED_DIR = "seed"
UUID = r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
UUID_ANY = re.compile(UUID, re.I)
INS_HEAD = re.compile(r"insert\s+into\s+([a-z_][a-z_0-9]*)\s*\(", re.I)
DO_BLOCK = re.compile(r"\bDO\s+(\$[A-Za-z_0-9]*\$)(.*?)\1", re.I | re.S)


# ---------------------------------------------------------------------------
# 语句切分（引号 / 美元引用 / 注释都要认，否则 DO 块里的分号会把语句切碎）
# ---------------------------------------------------------------------------
def split_statements(sql: str) -> list[str]:
    out, buf, i, n, dollar = [], [], 0, len(sql), None
    while i < n:
        ch = sql[i]
        if dollar:
            if sql.startswith(dollar, i):
                buf.append(dollar)
                i += len(dollar)
                dollar = None
                continue
            buf.append(ch)
            i += 1
            continue
        if ch == "-" and sql.startswith("--", i):
            j = sql.find("\n", i)
            j = n if j < 0 else j
            buf.append(sql[i:j])
            i = j
            continue
        if ch == "/" and sql.startswith("/*", i):
            j = sql.find("*/", i + 2)
            j = n if j < 0 else j + 2
            buf.append(sql[i:j])
            i = j
            continue
        if ch == "'":
            j = i + 1
            while j < n:
                if sql[j] == "'":
                    if j + 1 < n and sql[j + 1] == "'":
                        j += 2
                        continue
                    break
                j += 1
            buf.append(sql[i : j + 1])
            i = j + 1
            continue
        if ch == "$":
            m = re.match(r"\$[A-Za-z_0-9]*\$", sql[i:])
            if m:
                dollar = m.group(0)
                buf.append(dollar)
                i += len(dollar)
                continue
        if ch == ";":
            out.append("".join(buf))
            buf = []
            i += 1
            continue
        buf.append(ch)
        i += 1
    if "".join(buf).strip():
        out.append("".join(buf))
    return out


def strip_comments(sql: str) -> str:
    sql = re.sub(r"/\*.*?\*/", " ", sql, flags=re.S)
    return re.sub(r"--[^\n]*", " ", sql)


def top_parens(text: str) -> list[str]:
    """所有**顶层** `( … )` 的内容（跳过引号内、支持嵌套）。"""
    out, i, n = [], 0, len(text)
    while i < n:
        ch = text[i]
        if ch == "'":
            i += 1
            while i < n:
                if text[i] == "'":
                    if i + 1 < n and text[i + 1] == "'":
                        i += 2
                        continue
                    break
                i += 1
            i += 1
            continue
        if ch == "(":
            depth, j, start = 1, i + 1, i + 1
            while j < n and depth:
                c = text[j]
                if c == "'":
                    j += 1
                    while j < n:
                        if text[j] == "'":
                            if j + 1 < n and text[j + 1] == "'":
                                j += 2
                                continue
                            break
                        j += 1
                elif c == "(":
                    depth += 1
                elif c == ")":
                    depth -= 1
                    if depth == 0:
                        out.append(text[start:j])
                        break
                j += 1
            i = j + 1
            continue
        i += 1
    return out


def row_id_values(body: str) -> set[str]:
    """INSERT 正文里每行的 id（列序首位）候选。"""
    ids: set[str] = set()
    stripped = body.lstrip()
    if re.match(r"select\b", stripped, re.I):
        groups = [
            p for p in top_parens(stripped) if re.match(r"values\b", p.lstrip(), re.I)
        ]
        if groups:
            for g in groups:
                for row in top_parens(g):
                    u = UUID_ANY.search(row)
                    if u:
                        ids.add(u.group(0).lower())
            return ids
        u = UUID_ANY.search(stripped)
        if u:
            ids.add(u.group(0).lower())
        return ids
    for row in top_parens(body):
        u = UUID_ANY.search(row)
        if u:
            ids.add(u.group(0).lower())
    return ids


def produced_ids(text: str) -> set[str]:
    """本文件以 `id` 列 INSERT 进任意表的 uuid 集合。"""
    ids: set[str] = set()
    for raw in split_statements(strip_comments(text)):
        stmt = raw.lstrip()
        m = INS_HEAD.match(stmt)
        if not m:
            continue
        col_open = m.end() - 1
        depth, j = 0, col_open
        while j < len(stmt):
            if stmt[j] == "(":
                depth += 1
            elif stmt[j] == ")":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        cols = [c.strip().lower() for c in stmt[col_open + 1 : j].split(",")]
        if not cols or cols[0] != "id":
            continue
        ids |= row_id_values(stmt[j + 1 :])
    return ids


def existence_guard_ids(text: str) -> list[tuple[str, str]]:
    """会失败的存在性断言：`(uuid, 变量名)`。

    形态：`SELECT count(*) INTO <v> … WHERE … id = '<uuid>'` 之后
    `IF <v> <> 1 THEN … RAISE EXCEPTION`（或 `IF <v> = 0 THEN … RAISE EXCEPTION`）。
    `IF <v> = 0 THEN RAISE NOTICE` 属空库首启的正常分支，**不判红**。
    """
    hits: list[tuple[str, str]] = []
    for m in DO_BLOCK.finditer(strip_comments(text)):
        block = m.group(2)
        # uuid → 最近的 INTO 变量
        into_positions = [(mm.start(), mm.group(1)) for mm in re.finditer(r"\bINTO\s+([a-z_][a-z_0-9]*)", block, re.I)]
        for um in re.finditer(r"\bid\s*=\s*'(" + UUID + r")'", block, re.I):
            var = None
            for pos, name in into_positions:
                if pos < um.start():
                    var = name
                else:
                    break
            if var is None:
                continue
            # 「这一次」存在性检查的作用窗口：从这个 id 断言到**下一个 INTO** 之间。
            # 不这么限定的话，同一个变量（本仓惯例都叫 `n`）后面那条无关的判断会被误算进来
            # （V0442 修好之后就是这么被误报的：0001 的 NOTICE 分支后面紧跟着 2701 的 RAISE）。
            window_end = next((pos for pos, _ in into_positions if pos > um.start()), len(block))
            window = block[um.end() : window_end]
            pattern = re.compile(
                r"IF\s+" + re.escape(var) + r"\s*(?:<>|!=)\s*1\s*THEN([\s\S]*?)END\s+IF",
                re.I,
            )
            pattern0 = re.compile(
                r"IF\s+" + re.escape(var) + r"\s*=\s*0\s*THEN([\s\S]*?)END\s+IF", re.I
            )
            body = None
            mm = pattern.search(window)
            if mm:
                body = mm.group(1)
            else:
                mm = pattern0.search(window)
                if mm:
                    body = mm.group(1)
            if body is None:
                continue
            if re.search(r"RAISE\s+EXCEPTION", body, re.I):
                hits.append((um.group(1).lower(), var))
    return hits


def load_allow(path: Path) -> dict[tuple[str, str], str]:
    allow: dict[tuple[str, str], str] = {}
    if not path.is_file():
        return allow
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        parts = s.split("|")
        if len(parts) != 3 or not all(p.strip() for p in parts):
            print(f"  !! 豁免文件第 {lineno} 行格式不对（应为 `路径|uuid|理由`）：{line}", file=sys.stderr)
            continue
        allow[(parts[0].strip(), parts[1].strip().lower())] = parts[2].strip()
    return allow


def scan(root: Path) -> tuple[int, list[str]]:
    mig = root / "backend/app/migrations"
    if not mig.is_dir():
        print(f"!! 迁移目录不存在：{mig}", file=sys.stderr)
        return 2, []
    versioned = sorted(p for d in VERSIONED_DIRS for p in (mig / d).rglob("*.up.sql"))
    seeds = sorted((mig / SEED_DIR).rglob("*.sql"))
    if not versioned:
        print(f"!! 没扫到任何版本化迁移：{mig}/{{{','.join(VERSIONED_DIRS)}}}", file=sys.stderr)
        return 2, []

    produced: set[str] = set()
    for p in versioned:
        produced |= produced_ids(p.read_text(encoding="utf-8"))
    seed_produced: set[str] = set()
    for p in seeds:
        seed_produced |= produced_ids(p.read_text(encoding="utf-8"))

    allow = load_allow(root / "scripts/lint/seed_dependency_allow.txt")

    problems: list[str] = []
    for p in versioned:
        rel = str(p.relative_to(mig))
        for uuid, var in existence_guard_ids(p.read_text(encoding="utf-8")):
            if uuid in produced:
                continue
            if (rel, uuid) in allow:
                continue
            where = "seed 行（空库首启必炸）" if uuid in seed_produced else "谁都不产出（幽灵 id / 运行时生成）"
            problems.append(
                f"{rel}: 存在性断言 `{var}` 依赖 {uuid} —— {where}"
            )

    print(f"==> 扫描 {len(versioned)} 个版本化迁移 / {len(seeds)} 个种子迁移")
    print(f"    版本化迁移以 id 列 INSERT 的 uuid：{len(produced)}；seed：{len(seed_produced)}")
    if problems:
        print(f"\n✗ {len(problems)} 处「版本化迁移依赖非版本化产出的行」（会让空库首启失败）：")
        for line in problems:
            print(f"  - {line}")
        print(
            "\n修法：① 改成本迁移自己保证存在（写死 INSERT 或动态解析父节点）；\n"
            "      ② 该行确实只在种子之后才可能有 ⇒ 把护栏降级为 `IF <v> = 0 THEN RAISE NOTICE`；\n"
            "      ③ 确有理由（判据误报）⇒ 写进 scripts/lint/seed_dependency_allow.txt（必须带理由）。"
        )
        return 1, problems
    print("✓ 没有版本化迁移把 seed 行当作硬前置条件")
    return 0, []


# ---------------------------------------------------------------------------
# 判据自检：语料红/绿固定，判据被改坏立刻红
# ---------------------------------------------------------------------------
SELFTEST_CASES = [
    (
        "红：断言 seed 才建的行存在",
        """
        INSERT INTO sys_menu (id, name) VALUES ('00000000-0000-0000-0000-0000000000aa', 'x');
        DO $$
        DECLARE n integer;
        BEGIN
            SELECT count(*) INTO n FROM sys_menu WHERE id = '10000000-0000-0000-0000-000000000001'::uuid;
            IF n <> 1 THEN
                RAISE EXCEPTION 'V0000: 祖先目录不存在';
            END IF;
        END $$;
        """,
        "10000000-0000-0000-0000-000000000001",
        True,
    ),
    (
        "绿：同一断言但走 NOTICE 分支（空库首启正常路径）",
        """
        DO $$
        DECLARE n integer;
        BEGIN
            SELECT count(*) INTO n FROM sys_menu WHERE id = '10000000-0000-0000-0000-000000000001'::uuid;
            IF n = 0 THEN
                RAISE NOTICE '尚不存在，跳过';
            END IF;
        END $$;
        """,
        "10000000-0000-0000-0000-000000000001",
        False,
    ),
    (
        "绿：该 id 正是本文件自己 INSERT 出来的",
        """
        INSERT INTO sys_menu (id, name) VALUES ('20000000-0000-0000-0000-000000000002', 'y');
        DO $$
        DECLARE n integer;
        BEGIN
            SELECT count(*) INTO n FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000000002'::uuid;
            IF n <> 1 THEN
                RAISE EXCEPTION '自己建的行不该缺';
            END IF;
        END $$;
        """,
        "20000000-0000-0000-0000-000000000002",
        False,
    ),
    (
        "绿：动态 JOIN 父节点（不写死 id）不算依赖",
        """
        DO $$
        DECLARE n integer;
        BEGIN
            SELECT count(*) INTO n FROM sys_menu dir JOIN sys_menu p ON dir.id = p.parent_id
             WHERE dir.id = '20000000-0000-0000-0000-000000000003'::uuid;
            IF n = 0 THEN
                RAISE NOTICE '无子节点';
            END IF;
        END $$;
        """,
        "20000000-0000-0000-0000-000000000003",
        False,
    ),
]


def selftest() -> int:
    bad = 0
    for label, text, uuid, expect_hit in SELFTEST_CASES:
        produced = produced_ids(text)
        hits = [u for u, _ in existence_guard_ids(text)]
        got = (uuid in hits) if uuid not in produced else False
        ok = got == expect_hit
        print(f"  {'✓' if ok else '✗'} {label}")
        if not ok:
            bad += 1
            print(f"      期望命中={expect_hit} 实得={got}（hits={hits}, produced={sorted(produced)}）")
    print(("判据自检全绿" if bad == 0 else f"判据自检 {bad} 条红") + f"（{len(SELFTEST_CASES)} 条语料）")
    return 0 if bad == 0 else 1


def main() -> int:
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("--root", default=str(Path(__file__).resolve().parents[2]))
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        return selftest()
    rc, _ = scan(Path(args.root).resolve())
    return rc


if __name__ == "__main__":
    sys.exit(main())
