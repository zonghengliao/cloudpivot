#!/usr/bin/env python3
"""路径引用门禁：脚本 / 钩子 / CI / 会读源码的测试里写死的源码路径必须真实存在。

## 为什么有这道闸

目录搬迁（例：ERP 拆 24 个子目录，P2-01~26，2026-11-15）之后，脚本里的硬编码路径
**不会报错**，而是静默失效：

- `grep -rn 'FROM t' <旧路径>` → 0 行命中，闸②判绿；
- `cargo test --lib business::erp::<旧模块>` → `test result: ok. 0 passed`，退出码 0；
- `[ -f "$f" ]` 会红，但绝大多数脚本没那么写。

也就是说：**搬一次目录，几十条验证脚本集体变成空跑，而门禁仍然全绿**。
本闸把「引用的路径必须存在」变成静态断言 —— 秒级、不编译、不连库，放在 L0。

## 判据

1. 范围：**会执行的**脚本与**会读源码的测试** —— `scripts/**`、`.githooks/**`、`.github/**`、
   `frontend/tests/**`、`backend/app/tests/**`，扩展名 `.sh/.bash/.py/.mjs/.js/.json/.ts/.rs`
   （`*.txt`/`*.md` 是证据快照与说明，不判）。
   `frontend/tests` 与 `backend/app/tests` 是 2026-11-15（P2-27）补进来的：那里的用例用
   `readFileSync` / `include_str!` 直接读后端源码做「逐字对齐」，路径写错同样只是 ENOENT 或
   空断言 —— ERP 拆目录后就有 11 个后端路径在 9 个前端用例里失效（22 处），
   而 `pnpm test:unit` 不在这道闸里，没人看得见。判据不变，只是把「会读源码的地方」补齐。
2. 归一化：`$SRC` / `${SRC}` → `backend/app/src`，`$REPO/` → ``（脚本里拼路径的常见写法）。
   归一化后含变量的片段（如 `$f`）跳过，不做臆测。
3. 匹配：`backend/app/src/**/*.rs`、`frontend/src/**/*.{ts,vue,js,mjs}`。
4. 断言：路径必须存在于仓库磁盘上；缺失即判红，打印 `文件:行 + 路径 + 原文`。
5. `ALLOW_LIST`：一次性重构工具 / 合成语料（**不是**真实路径引用）。每条的「理由」必写，
   且**条目文件本身必须存在** —— 白名单失效时判红，防止无限期膨胀。

## 用法

    python3 scripts/lint/path_refs_gate.py              # 判红/判绿（退出码 0/1）
    python3 scripts/lint/path_refs_gate.py --self-test  # 判据自检（临时语料，不碰真仓库）
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import tempfile
from pathlib import Path

SCAN_ROOTS = ("scripts", ".githooks", ".github", "frontend/tests", "backend/app/tests")
SCAN_EXTS = (".sh", ".bash", ".py", ".mjs", ".js", ".json", ".ts", ".rs")

# 引用形态 ①：带前缀的绝对（仓库相对）路径
# P2-40 起后端源码在 `backend/app/src/`（workspace 化：`backend/` 是 workspace 根，
# `backend/app/` 是主 binary crate），前端源码仍在 `frontend/src/`。
# P2-59（2026-11-15）起 `backend/crates/<crate>/src/` 也要认：域 crate 逐个抽出后，
# 脚本 / 用例里引用的是新路径；不认的话这些引用会**静默不被检查**（红变绿的漏洞），
# 而且形态 ② 会把 `…/cloudpivot-kernel/src/business/shared/x.rs` 的尾巴误判成
# `backend/app/src/business/shared/x.rs`（假红）。
REF_RE = re.compile(
    r"((?:backend/app|backend/crates/[A-Za-z0-9_\-]+|frontend)/src/[A-Za-z0-9_./\-]+\.(?:rs|ts|vue|js|mjs))"
)
# 引用形态 ②：相对 backend/app/src 的裸路径（脚本里很常见：`("business/erp/x_dto.rs", "Res", …)`）
# 只认「已知后端模块根 + 至少两级」且必须以 .rs 结尾 —— API URL（/api/business/erp/…）
# 没有 .rs 后缀，不会被误判。
REF_RE_SRC_REL = re.compile(
    r"\b((?:business|system|support|shared|middleware|utils|task)/"
    r"[A-Za-z0-9_]+/[A-Za-z0-9_./\-]*\.rs)"
)
# 脚本里拼路径的写法：$SRC / ${SRC} / $REPO/
NORM_RULES = (
    ("${SRC}", "backend/app/src"),
    ("$SRC", "backend/app/src"),
    ("${REPO}/", ""),
    ("$REPO/", ""),
)

# 白名单：{相对路径: 理由}。文件搬走后这里要跟着改或删，见 check_allow_list。
ALLOW_LIST = {
    "scripts/refactor/verify_move.py": "一次性重构工具：形参是「搬迁前路径 → 搬迁后路径」，旧路径按设计保留（它就是被验的对象）",
    "scripts/refactor/verify_pub_api.py": "一次性重构工具：同上，核对搬迁前公开 API 的对照表",
    "scripts/refactor/split_module.py": "一次性重构工具：DEFAULT plan 里的 `src` 是「拆分前」源文件，按设计保留旧路径",
    "scripts/refactor/plans/stockin_handler.json": "一次性重构计划（TD-2 已完成）：记录的是拆分前的源文件与落点对照",
    "scripts/refactor/plans/stockin_model.json": "一次性重构计划（TD-2 已完成）：同上",
    "scripts/refactor/plans/transfer_handler.json": "一次性重构计划（TD-2 已完成）：同上",
    "scripts/refactor/plans/transfer_model.json": "一次性重构计划（TD-2 已完成）：同上",
    "scripts/lint/changelog_in_tx.py": "自检语料：case_*.rs 是内存里的合成文件名，磁盘上本就没有这些文件",
    "scripts/lint/scope_gate.py": "自检语料：zz_*/exempt.rs 等是临时目录里的合成文件名，磁盘上本就没有这些文件",
    "scripts/lint/path_refs_gate.py": "本文件自身：判据自检的红/绿语料就写在源码里（`--self-test` 用临时目录跑，不碰真仓库）",
}


def normalize(line: str) -> str:
    """把脚本里的 `$SRC` / `$REPO/` 还原成真实相对路径前缀。"""
    for old, new in NORM_RULES:
        line = line.replace(old, new)
    return line


def iter_scan_files(root: Path):
    for rel in SCAN_ROOTS:
        base = root / rel
        if not base.is_dir():
            continue
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = sorted(
                d for d in dirnames if d not in {".git", "node_modules", "__pycache__", "target"}
            )
            for fn in sorted(filenames):
                if fn.endswith(SCAN_EXTS):
                    yield (Path(dirpath) / fn).relative_to(root)


def scan(root: Path, allow: dict[str, str]) -> list[tuple[str, int, str, str]]:
    """返回 [(相对文件, 行号, 缺失路径, 原文)]。"""
    failures: list[tuple[str, int, str, str]] = []
    for rel in iter_scan_files(root):
        rel_s = rel.as_posix()
        if rel_s in allow:
            continue
        try:
            text = (root / rel).read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        for lineno, raw in enumerate(text.splitlines(), 1):
            line = normalize(raw)
            spans = []
            refs: list[str] = []
            for m in REF_RE.finditer(line):
                refs.append(m.group(1))
                spans.append(m.span(1))
            for m in REF_RE_SRC_REL.finditer(line):
                if any(s <= m.start(1) < e for s, e in spans):
                    continue  # 已被 ① 形态覆盖
                refs.append("backend/app/src/" + m.group(1))
            for ref in refs:
                if "$" in ref:
                    continue
                if not (root / ref).exists():
                    failures.append((rel_s, lineno, ref, raw.strip()[:160]))
    return failures


def check_allow_list(root: Path, allow: dict[str, str]) -> list[str]:
    """白名单条目必须指向真实存在的文件、且必须写了理由。"""
    errs: list[str] = []
    for rel, reason in sorted(allow.items()):
        if not (root / rel).is_file():
            errs.append(f"白名单条目已失效（文件不存在）：{rel} —— 搬走/删除了就把这条一起删掉")
        if len(reason.strip()) < 8:
            errs.append(f"白名单条目缺理由：{rel}")
    return errs


def run(root: Path) -> int:
    allow_errors = check_allow_list(root, ALLOW_LIST)
    failures = scan(root, ALLOW_LIST)

    print("=" * 92)
    print("路径引用门禁：脚本 / 钩子 / CI / 测试里写死的源码路径必须真实存在")
    print("=" * 92)

    if allow_errors:
        for e in allow_errors:
            print(f"  [FAIL] {e}")

    for rel, lineno, ref, snippet in failures:
        print(f"  [FAIL] {rel}:{lineno}  {ref}\n         {snippet}")

    if failures or allow_errors:
        print()
        print(f"结论：FAIL（{len(failures)} 处失效路径，{len(allow_errors)} 处白名单问题）")
        print("修法：文件搬了 → 把脚本里的路径改成新路径（或改 glob）；文件真删了 → 删掉这条引用。")
        print("      别把这行加进 ALLOW_LIST —— 白名单只给「合成语料 / 一次性重构对照表」。")
        return 1

    print(f"结论：PASS（扫描 {len(list(iter_scan_files(root)))} 个脚本，"
          f"白名单 {len(ALLOW_LIST)} 条，0 处失效路径）")
    return 0


# ══════════════════════════════════════════════════════════════════════════
# 判据自检：红/绿语料（判据被改坏时，自检先红）
# ══════════════════════════════════════════════════════════════════════════
def self_test() -> int:
    # 每条：(标签, 语料文件, 真实存在的目标路径, 该条的白名单, 期望红?, 为什么这条语料重要)
    cases: list[tuple[str, dict[str, str], dict[str, str], dict[str, str], bool, str]] = [
        (
            "引用存在的路径 → 绿",
            {"scripts/a.sh": "grep -rn x backend/app/src/ok.rs\n"},
            {"backend/app/src/ok.rs": ""},
            {},
            False,
            "存在的路径不该判红",
        ),
        (
            "引用已搬走的旧路径 → 红",
            {"scripts/b.sh": "grep -rn x backend/app/src/business/erp/gone.rs\n"},
            {},
            {},
            True,
            "旧路径必须判红（这条就是 ERP 拆目录现场）",
        ),
        (
            "$SRC 归一化后能判出旧路径 → 红",
            {"scripts/c.sh": "grep -rn x $SRC/business/erp/gone.rs\n"},
            {},
            {},
            True,
            "变量拼路径也是最常见的写法，必须归一化后再判",
        ),
        (
            "$SRC 归一化后路径存在 → 绿",
            {"scripts/d.sh": "grep -rn x ${SRC}/business/erp/kept.rs\n"},
            {"backend/app/src/business/erp/kept.rs": ""},
            {},
            False,
            "归一化不许把正常引用判红",
        ),
        (
            "前端源码引用同样判",
            {"scripts/e.mjs": "const p='frontend/src/views/gone.vue'\n"},
            {},
            {},
            True,
            "前端路径搬了也一样静默失效",
        ),
        (
            "src 相对裸路径（business/erp/x.rs）同样判",
            {"scripts/f.py": 'REQUIRED = [("business/erp/gone_dto.rs", "Res")]\n'},
            {},
            {},
            True,
            "aud02 这类清单写的是相对 src 的路径，漏判就等于漏掉一整类",
        ),
        (
            "src 相对裸路径存在 → 绿",
            {"scripts/g.py": 'REQUIRED = [("business/erp/kept_dto.rs", "Res")]\n'},
            {"backend/app/src/business/erp/kept_dto.rs": ""},
            {},
            False,
            "别把正常清单判红",
        ),
        (
            "白名单文件不判 → 绿",
            {"scripts/lint/changelog_in_tx.py": "f='backend/app/src/case_x.rs'\n"},
            {},
            {"scripts/lint/changelog_in_tx.py": "自检语料：合成文件名，磁盘上本就没有这些文件"},
            False,
            "ALLOW_LIST 的文件整体跳过",
        ),
        (
            "白名单条目文件不存在 → 红",
            {"scripts/noop.sh": "echo hi\n"},
            {},
            {"scripts/lint/missing_allow.py": "自检语料：故意指向不存在的文件，期望判红"},
            True,
            "白名单失效必须判红，否则白名单会无限膨胀",
        ),
        (
            "白名单理由为空 → 红",
            {"scripts/noop2.sh": "echo hi\n"},
            {},
            {"scripts/noop2.sh": "  "},
            True,
            "没有理由的白名单等于永久豁免",
        ),
        (
            "前端用例里读后端源码的旧路径 → 红",
            {"frontend/tests/unit/x.test.ts": "readFileSync(new URL('../../../backend/app/src/business/erp/gone_model.rs'))\n"},
            {},
            {},
            True,
            "frontend/tests 用 readFileSync 读后端源码做逐字对齐，路径搬了只会 ENOENT/空断言（P2-27 现场）",
        ),
        (
            "集成测试里 `src/` 相对裸路径（含 .rs 扩展名）同样判",
            {"backend/app/tests/x.rs": "let p = \"business/erp/gone_v2.rs\";\n"},
            {},
            {},
            True,
            "backend/app/tests 是 .rs，扫不到就等于漏掉一整类（同一套裸路径判据必须覆盖）",
        ),
        (
            "前端用例引用存在的后端源码 → 绿",
            {"frontend/tests/unit/y.test.ts": "readFileSync(new URL('../../../backend/app/src/business/erp/kept_model.rs'))\n"},
            {"backend/app/src/business/erp/kept_model.rs": ""},
            {},
            False,
            "别把正常的逐字对齐用例判红",
        ),
    ]

    bad = 0
    print("=" * 92)
    print("路径引用门禁 · 判据自检（红/绿语料）")
    print("=" * 92)
    for label, files, existing, allow, expect_red, why in cases:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            for rel, content in files.items():
                p = root / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(content, encoding="utf-8")
            for rel in existing:
                p = root / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text("// x\n", encoding="utf-8")

            allow_errs = check_allow_list(root, allow)
            fails = scan(root, allow)
            red = bool(fails or allow_errs)
            ok = red == expect_red
            status = "PASS" if ok else "FAIL"
            if not ok:
                bad += 1
            print(f"  [{status}] {label}  （期望{'红' if expect_red else '绿'}，实得{'红' if red else '绿'}）")
            if not ok:
                print(f"         为什么这条语料重要：{why}")
                print(f"         failures={fails} allow_errors={allow_errs}")
    print()
    print(f"自检结论：{'全绿（判据可用）' if bad == 0 else f'{bad} 条语料不符 —— 判据被改坏了'}")
    return 0 if bad == 0 else 1


def main() -> int:
    ap = argparse.ArgumentParser(description="脚本/钩子/CI/测试里的硬编码源码路径必须真实存在")
    ap.add_argument("--root", default=".", help="仓库根目录（默认当前目录）")
    ap.add_argument("--self-test", "--selftest", dest="self_test",
                    action="store_true", help="跑判据自检（红/绿语料），不检查真仓库")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    return run(Path(args.root).resolve())


if __name__ == "__main__":
    sys.exit(main())
