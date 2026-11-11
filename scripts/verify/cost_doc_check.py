#!/usr/bin/env python3
"""COST-B4-6 · 成本核算文档可达性自检（`docs/后端二开指南.md` §十六）。

用途：文档里写的每个文件名 / 函数名 / 迁移号，都必须能在仓库里落地 ——
否则「新人照文档 30 分钟找到改哪里」就是一句空话（改了文件没改文档，
下一个人按图索骥会扑空）。

判据（全部读真实文件系统，不做「看起来对」的推断）：

  A. 章节里出现的 `*.rs|*.py|*.ts|*.sql|*.sh|*.md` 反引号路径必须解析到真实文件
     （按 repo 根 / backend / backend/src / business/erp / frontend/src / migrations
     等候选前缀逐个试，含 `{a,b,c}` 花括号展开与 `*` 通配）；
  B. 出现的 `fn_name(` 形式的后端函数名必须能在 `backend/src` grep 到 `fn fn_name`
     （`async` 亦可）；SQL / shell / 第三方库方法在 SKIP 里显式列名；
  C. 迁移号引用（`V0xxx__...`）必须有对应 `*.up.sql`；
  D. 号段大于当前最大迁移的示例文件名视为「待新建」，打印 `[PLANNED]` 不算死链。

退出码：0 = 全部可达（打印 `DOC_OK`）；1 = 有死链（逐条打印 `[FILE]` / `[FN]` / `[MIG]`）。

用法：

```bash
python3 scripts/verify/cost_doc_check.py            # 检查默认文档
python3 scripts/verify/cost_doc_check.py <doc.md>   # 检查指定文档（负例自测用）
```
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DOC = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / "docs/后端二开指南.md"
ERP = ROOT / "backend/src/business/erp"

CHAPTER_START = "## 十六、成本核算"
# 号段大于它 ⇒ 文档里的迁移引用属于「示例，待新建」
MAX_MIGRATION = "V0385"

# 相对引用的候选前缀（章节里为了可读性大量写短名，如 `cost_calc_alloc.rs`）
PREFIXES = [
    ROOT,
    ROOT / "backend",
    ERP,
    ERP.parent,
    ROOT / "backend/src",
    ROOT / "frontend/src",
    ROOT / "backend/migrations/business",
    ROOT / "scripts/verify",
    ROOT / "scripts",
    ROOT / "docs/architecture",
]

FILE_RE = re.compile(r"`([A-Za-z0-9_./{}\-]+\*?\.(?:rs|py|ts|sql|sh|md))`")
MIG_RE = re.compile(r"`(V0\d{3}__[a-z0-9_]+)")
BARE_MIG_RE = re.compile(r"(?<![\w])V0\d{3}(?![\d_])")
FN_RE = re.compile(r"`([a-z_][a-z0-9_]{3,})\(")

# 文档里「不是本仓函数」的标识符：SQL 关键字 / shell 命令 / 第三方库方法
SKIP_FNS = {
    "check", "round", "coalesce", "insert", "values", "select", "update",
    "execute", "print", "mkdir", "git", "cargo", "bash", "python3", "curl",
    "round_dp",  # rust_decimal 的方法
}


def expand_braces(raw: str) -> list[str]:
    """`cost_calc_{material,labor}.rs` → 两条真实文件名。"""
    if "{" not in raw:
        return [raw]
    pre, rest = raw.split("{", 1)
    body, post = rest.split("}", 1)
    return [pre + part + post for part in body.split(",")]


def resolve(raw: str) -> list[Path]:
    """把一个引用解析成真实存在的路径（空列表 = 无）。"""
    name = raw.split("/")[-1]
    hits: list[Path] = []
    if "*" in raw:  # 通配：`cost_*.rs` / `cost_calc_*.rs`
        hits += [p for p in ROOT.glob(raw.lstrip("./")) if p.is_file()]
        hits += [p for p in ERP.glob(name) if p.is_file()]
        return hits
    for pre in PREFIXES:
        candidate = (pre / raw).resolve()
        if candidate.exists():
            hits.append(candidate)
    if hits:
        return hits
    # 退路：按文件名全树找（文档常只写文件名）
    for pattern in (f"backend/src/**/{name}", f"frontend/src/**/{name}",
                    f"backend/migrations/**/{name}", f"scripts/**/{name}"):
        hits += [p for p in ROOT.glob(pattern) if p.is_file()]
    return hits


def main() -> int:
    text = DOC.read_text(encoding="utf-8")
    if CHAPTER_START not in text:
        print(f"[FILE] 章节缺失：{DOC} 里没有 `{CHAPTER_START}`")
        return 1
    chapter = text[text.index(CHAPTER_START):]

    fail: list[str] = []
    planned: list[str] = []

    # ---------- A. 文件路径 ----------
    raw_paths: set[str] = set()
    for match in FILE_RE.finditer(chapter):
        raw_paths.update(expand_braces(match.group(1)))
    ok_files = 0
    for raw in sorted(raw_paths):
        if resolve(raw):
            ok_files += 1
        elif raw.startswith("V0") and raw.split("__")[0] > MAX_MIGRATION:
            planned.append(raw)  # 示例：号段大于当前最大迁移
        else:
            fail.append(f"[FILE] 解析不到：{raw}")

    # ---------- B. 后端函数名 ----------
    ok_fns = 0
    for fn in sorted(set(FN_RE.findall(chapter))):
        if fn in SKIP_FNS:
            continue
        got = subprocess.run(
            ["grep", "-rqE", rf"(async )?fn {fn}\b", str(ROOT / "backend/src")],
            capture_output=True,
        )
        if got.returncode == 0:
            ok_fns += 1
        else:
            fail.append(f"[FN] backend/src 里找不到 fn {fn}")

    # ---------- C. 迁移号 ----------
    ok_migs = 0
    for full in sorted(set(MIG_RE.findall(chapter))):
        # 全名匹配（只按号段匹配会把「同号不同名」的其它迁移当成存在）
        if list((ROOT / "backend/migrations").glob(f"*/{full}.up.sql")):
            ok_migs += 1
        elif full[:5] > MAX_MIGRATION:
            planned.append(full)
        else:
            fail.append(f"[MIG] 迁移不存在：{full}")
    # 只写号段的形式（表格里的 `V0375`）
    for ver in sorted(set(BARE_MIG_RE.findall(chapter))):
        if list((ROOT / "backend/migrations").glob(f"*/{ver}__*.up.sql")):
            ok_migs += 1
        elif ver > MAX_MIGRATION:
            planned.append(ver)
        else:
            fail.append(f"[MIG] 号段没有任何迁移：{ver}")
    planned = sorted(set(planned))

    total_files = ok_files + sum(1 for line in fail if line.startswith("[FILE]"))
    total_fns = ok_fns + sum(1 for line in fail if line.startswith("[FN]"))
    total_migs = ok_migs + sum(1 for line in fail if line.startswith("[MIG]"))

    print(f"章节：{CHAPTER_START}（{chapter.count(chr(10)) + 1} 行）")
    for raw in planned:
        print(f"[PLANNED] 示例迁移（尚未创建）：{raw}")
    print(f"[A] 文件引用可达   {ok_files}/{total_files}")
    print(f"[B] 函数名有定义   {ok_fns}/{total_fns}")
    print(f"[C] 迁移号存在     {ok_migs}/{total_migs}")

    if fail:
        print("\n死链明细：")
        for line in fail:
            print("  " + line)
        return 1
    print("\nDOC_OK 全部引用可达")
    return 0


if __name__ == "__main__":
    sys.exit(main())
