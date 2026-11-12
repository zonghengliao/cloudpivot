#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""门禁：迁移版本号必须**跨目录全局唯一**、up/down 成对、名字不复用。

为什么需要它（一次真实事故）
--------------------------
`backend/migrations/` 分了 `system` / `support` / `business` 三个目录，但 nik 的迁移
加载器（`backend/vendor/nik-db/src/migrate/loader.rs`）把**所有来源目录合成一个槽位表**，
再按版本号排序执行。同一个版本号出现在两个目录里，加载阶段直接失败：

    migration error: duplicate up migration for version 397:
      'migrations/system/V0397__…up.sql' conflicts with
      'migrations/business/V0397__…up.sql'; identities must be globally unique across all sources
    FATAL: 服务启动失败: 配置错误: 数据库迁移失败

这条失败与「库是空库还是已迁满」无关 —— 它发生在读迁移记录表之前，所以**任何**实例都起不来
（2026-11-13 实测：一个已迁满 329 条记录的库，重启同样 rc=1 FATAL）。事故经过：
主题配色卡 `2609c7f6` 与 HR-40 卡 `83a4cfab` 各自「按自己目录看来」取了下一个号，都取了
V0397，main 上无人发现 —— 因为各卡的真库回放脚本是自己 `ls | sort -n | psql` 逐条回放，
**不经过加载器**，319 条回放全绿也证明不了版本号唯一。

所以这道检查只能放在「静态、秒级、每次 push 都跑」的 L0 层。

判据（任一不成立即 FAIL）
------------------------
A. 同一前缀（`V` / `B`）的版本号**数值**全局唯一（跨 `system` / `support` / `business` 全部目录）。
   比数值而不是比字符串：`V97` 与 `V0097` 在 nik 眼里是同一个版本，字符串比较会漏。
B. 版本号 4 位补零（`V0399` 而不是 `V399`）。零填充是排序稳定与人工核对的前提；
   仓库里 `scripts/verify/**` 的真库回放脚本按 `sort -n` 排文件名，混位数会排错序。
C. 同一个「版本化名字」（`__` 之后那段）不得跨不同版本复用 —— 迁移记录表主键是
   `(kind, name)`，nik 对「同名两个版本」同样启动失败（`name is used by two versions`）。
D. 成对：每个 `V####__x.up.sql` 都要有 `.down.sql`，反之亦然（AGENTS.md 约定 6）。
E. 文件名主干只允许 `[A-Za-z0-9_]`（空格 / 中文 / 连字符会让加载器解析出意料之外的名字）。

判定规则本身由 `--selftest` 的红/绿语料守住（每次跑门禁都会先跑自检）：语料被改坏、
或有人把判据放宽，自检立刻红。

用法
----
    python3 scripts/lint/migration_version_unique.py --root .
    python3 scripts/lint/migration_version_unique.py --selftest     # 只跑判据自检

退出码：0 全绿 / 1 有违规或自检失败 / 2 用法或路径错误
"""

from __future__ import annotations

import argparse
import os
import re
import sys

# 文件名主干：`V0012__create_sys_job` / `S__menu_data` / `R__some_view`
VERSIONED_RE = re.compile(r"^(?P<prefix>[VB])(?P<digits>\d+)__(?P<name>[A-Za-z0-9_]+)$")
NAMED_RE = re.compile(r"^(?P<prefix>[RS])__(?P<name>[A-Za-z0-9_]+)$")
PAD = 4


class Violation:
    def __init__(self, code: str, detail: str):
        self.code = code
        self.detail = detail

    def __str__(self) -> str:
        return f"[{self.code}] {self.detail}"


def iter_sql(migroot: str):
    """递归列出全部迁移 .sql 文件的相对路径（排序，输出稳定）。"""
    out = []
    for dirpath, _dirnames, filenames in os.walk(migroot):
        for filename in filenames:
            if filename.endswith(".sql"):
                full = os.path.join(dirpath, filename)
                out.append(os.path.relpath(full, migroot).replace(os.sep, "/"))
    out.sort()
    return out


def split_direction(relpath: str):
    """`a/b/V0001__x.up.sql` → (`a/b/V0001__x`, 'up')；非 up/down 后缀返回 'plain'。"""
    stem = relpath.rsplit("/", 1)[-1]
    for suffix, direction in ((".up.sql", "up"), (".down.sql", "down")):
        if stem.endswith(suffix):
            return relpath[: -len(suffix)], direction
    return relpath[: -len(".sql")], "plain"


def check(relpaths):
    """对一组相对路径做 A–E 五项判据，返回 (violations, stats)。"""
    violations = []
    stats = {"files": len(relpaths), "versions": 0, "pairs": 0, "max_version": 0}

    # 版本值 → [(relpath, direction)]；名字 → {版本值}
    by_version = {}
    names = {}
    up_names, down_names = set(), set()

    for relpath in relpaths:
        stem, direction = split_direction(relpath)
        basename = stem.rsplit("/", 1)[-1]
        match = VERSIONED_RE.match(basename)
        if match:
            prefix, digits, name = match.group("prefix"), match.group("digits"), match.group("name")
            version = int(digits)
            if prefix == "V":
                stats["versions"] = max(stats["versions"], version) if direction == "up" else stats["versions"]
                stats["max_version"] = max(stats["max_version"], version)
            if len(digits) != PAD:
                violations.append(
                    Violation(
                        "B",
                        f"{relpath}：版本号 `{prefix}{digits}` 没有 4 位补零"
                        f"（应为 `{prefix}{version:0{PAD}d}`）；零填充是排序稳定的前提",
                    )
                )
            by_version.setdefault((prefix, version), []).append((relpath, direction))
            names.setdefault((prefix, name), set()).add(version)
            (up_names if direction == "up" else down_names).add((prefix, version, name))
            continue

        named = NAMED_RE.match(basename)
        if named:
            names.setdefault((named.group("prefix"), named.group("name")), set()).add(None)
            continue

        violations.append(
            Violation(
                "E",
                f"{relpath}：文件名主干 `{basename}` 不合规 —— 版本化迁移须为 "
                f"`V####__<name>.{{up,down}}.sql`（name 只允许字母数字下划线），"
                f"种子/可重复迁移须为 `S__<name>.sql` / `R__<name>.sql`",
            )
        )

    # A：版本号数值跨目录唯一（同一前缀）
    for (prefix, version), entries in sorted(by_version.items()):
        same_direction = {}
        for relpath, direction in entries:
            same_direction.setdefault(direction, []).append(relpath)
        for direction, paths in sorted(same_direction.items()):
            if len(paths) < 2:
                continue
            paths = sorted(paths)
            violations.append(
                Violation(
                    "A",
                    f"版本号 {version} 重复（{len(paths)} 个 {direction} 迁移，nik 要求跨来源目录全局唯一）：\n"
                    + "\n".join(f"        - {path}" for path in paths)
                    + f"\n      修法：给其中「未上生产」的那条改号 —— 取全局最大 {prefix}"
                    f"{stats['max_version']:0{PAD}d} + 1；已应用过的迁移文件不许改名/改内容（指纹校验会红）",
                )
            )

    # C：同名跨版本
    for (prefix, name), versions in sorted(names.items()):
        real_versions = sorted(version for version in versions if version is not None)
        if len(real_versions) > 1:
            violations.append(
                Violation(
                    "C",
                    f"名字 `{name}` 被多个版本复用（{prefix}{real_versions[0]:0{PAD}d}、"
                    f"{prefix}{real_versions[1]:0{PAD}d}）；迁移记录表主键是 (kind, name)，"
                    f"nik 会报 `name is used by two versions` —— 名字必须唯一",
                )
            )

    # D：up / down 成对
    for prefix, version, name in sorted(up_names - down_names):
        violations.append(
            Violation(
                "D",
                f"{prefix}{version:0{PAD}d}__{name}：有 up 缺 down（回滚脚本不能缺；"
                f"确无可回滚的写 `DO $$ BEGIN RAISE NOTICE '无需回滚'; END $$;`）",
            )
        )
    for prefix, version, name in sorted(down_names - up_names):
        violations.append(Violation("D", f"{prefix}{version:0{PAD}d}__{name}：有 down 缺 up（孤儿回滚脚本）"))

    stats["pairs"] = len(up_names & down_names)
    return violations, stats


def find_migroot(root: str) -> str:
    migroot = os.path.join(root, "backend", "migrations")
    if not os.path.isdir(migroot):
        raise SystemExit(f"找不到迁移目录：{migroot}（--root 要指向仓库根）")
    return migroot


def run_tree(root: str) -> int:
    migroot = find_migroot(root)
    relpaths = iter_sql(migroot)
    if not relpaths:
        print(f"✗ {migroot} 下一个 .sql 都没有 —— 路径不对？")
        return 2
    violations, stats = check(relpaths)
    if violations:
        print(f"✗ 迁移文件名判据未通过（{len(violations)} 条）：")
        for violation in violations:
            print(f"    {violation}")
        return 1
    print(
        f"  ✓ 迁移版本号跨目录唯一：{stats['files']} 个文件 / {stats['pairs']} 对 up-down "
        f"/ {stats['versions']} 个版本，最大 V{stats['max_version']:0{PAD}d}"
    )
    print(f"    下一个可用版本号：V{stats['max_version'] + 1:0{PAD}d}（新卡请以此为准，别再各目录各算）")
    return 0


# ---------------------------------------------------------------------------
# 判据自检：红/绿语料。语料是「假的」相对路径，不落盘，因此自检在干净检出树上也能跑。
# ---------------------------------------------------------------------------
SELFTEST_CASES = [
    (
        "正向：跨目录连续号 + 成对 + 种子",
        [
            "system/V0001__a.up.sql",
            "system/V0001__a.down.sql",
            "support/V0002__b.up.sql",
            "support/V0002__b.down.sql",
            "business/V0003__c.up.sql",
            "business/V0003__c.down.sql",
            "seed/S__menu.sql",
        ],
        [],
    ),
    (
        "负向 A：跨目录同号（就是本次事故）",
        [
            "system/V0397__preference_palette_ids_gallery77.up.sql",
            "system/V0397__preference_palette_ids_gallery77.down.sql",
            "business/V0397__hr_salary_period_after_reverse.up.sql",
            "business/V0397__hr_salary_period_after_reverse.down.sql",
        ],
        ["A"],
    ),
    (
        "负向 A：零填充不同但数值相同也必须抓",
        [
            "system/V0097__a.up.sql",
            "system/V0097__a.down.sql",
            "business/V97__b.up.sql",
            "business/V97__b.down.sql",
        ],
        ["A", "B"],
    ),
    ("负向 B：未 4 位补零", ["business/V399__x.up.sql", "business/V399__x.down.sql"], ["B"]),
    (
        "负向 C：同名跨版本",
        [
            "business/V0010__same_name.up.sql",
            "business/V0010__same_name.down.sql",
            "business/V0011__same_name.up.sql",
            "business/V0011__same_name.down.sql",
        ],
        ["C"],
    ),
    ("负向 D：有 up 缺 down", ["system/V0001__a.up.sql"], ["D"]),
    ("负向 D：孤儿 down", ["system/V0001__a.down.sql"], ["D"]),
    ("负向 E：文件名带空格/连字符", ["business/V0001__bad-name.up.sql"], ["E"]),
]


def run_selftest() -> int:
    failures = 0
    for tag, relpaths, expect_codes in SELFTEST_CASES:
        violations, _stats = check(relpaths)
        got = sorted({violation.code for violation in violations})
        want = sorted(set(expect_codes))
        ok = got == want
        print(f"  {'✓' if ok else '✗'} {tag}：期望 {want or '全绿'} / 实际 {got or '全绿'}")
        if not ok:
            failures += 1
            for violation in violations:
                print(f"      {violation}")
    if failures:
        print(f"✗ 判据自检失败 {failures} 条 —— 门禁自身失效，别急着相信它的「通过」")
        return 1
    print(f"  ✓ 判据自检全绿（{len(SELFTEST_CASES)} 条语料）")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="迁移文件名/版本号门禁")
    parser.add_argument("--root", default=".", help="仓库根（缺省当前目录）")
    parser.add_argument("--selftest", action="store_true", help="只跑判据自检（不扫树）")
    args = parser.parse_args()

    if args.selftest:
        return run_selftest()
    return run_tree(os.path.abspath(args.root))


if __name__ == "__main__":
    sys.exit(main())
