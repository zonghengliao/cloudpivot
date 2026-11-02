#!/usr/bin/env python3
"""TD-1 拆分器：把单个 Rust 模块文件按「职责」切成多文件 + facade 重导出。

用法：
    python3 scripts/refactor/split_module.py --plan plans/stockin_model.json --dry-run
    python3 scripts/refactor/split_module.py --plan plans/stockin_model.json

设计约束（与 docs/refactor-methodology.md 对齐）：
1. **只搬不移**：不重写任何函数体、不改任何 SQL、不改任何签名（唯一允许的文本改动是
   私有项的可见性 `fn` → `pub(super) fn`，否则跨文件调不动）。
2. 公开 API 不变：原文件变成 facade，`pub use super::<part>::*;` 重导出全部部件，
   调用方 `use crate::business::erp::<orig>::xxx` 一行不改。
3. 部件文件只有两种头部：文件头注释（职责说明）+ 原文件的 import 块 + `use super::<base>::*;`。

plan JSON 结构：
{
  "src": "src/business/erp/stockin_model.rs",
  "base": "stockin_base",                # 常量/契约/状态机/纯函数的落点（未在 targets 里点名的项）
  "targets": {                            # 部件名 -> 归属项名列表（函数/结构体/常量/impl 都算项）
    "stockin_query": ["find_stock_in", "items_of"],
    "stockin_tests": ["pure_tests"]       # 测试模块的名字（`mod pure_tests`）
  },
  "tests_mod": "stockin_tests"            # 测试部件：文件内不再包 #[cfg(test)] 顶层包裹
}
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

# 顶层声明（必须有名字，用于归属判断）
DECL_RE = re.compile(
    r"^(?P<vis>pub(?:\([^)]*\))?\s+)?"
    r"(?:(?:async|const|unsafe|extern)\s+)*"
    r"(?P<kw>fn|struct|enum|union|trait|type|const|static|mod|impl|macro_rules!)\b"
)
NAME_RE = re.compile(
    r"\b(?:fn|struct|enum|union|trait|type|const|static|mod|macro_rules!)\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)"
)
# impl 块没有名字，用 "impl <Type>" 标识；`impl Trait for Type` 取 `Type`
IMPL_RE = re.compile(r"^impl(?:<[^>]*>)?\s+(.+?)\s*\{")
IMPL_FOR_RE = re.compile(r"\bfor\s+([A-Za-z_][A-Za-z0-9_]*)")


@dataclass
class Item:
    name: str
    lines: list[str] = field(default_factory=list)
    start: int = 0  # 1-based，含 doc/attr 前缀

    @property
    def text(self) -> str:
        return "".join(self.lines)


def split_file(path: Path):
    """返回 (module_docs, imports, items, trailer)。"""
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    n = len(lines)

    # 1) 文件头：连续的 //! 行
    i = 0
    module_docs: list[str] = []
    while i < n and (lines[i].startswith("//!") or lines[i].strip() == ""):
        if lines[i].startswith("//!"):
            module_docs.append(lines[i])
        i += 1
    if module_docs and module_docs[-1].strip().endswith("//!") is False:
        module_docs.append("\n")

    # 2) import 块：首个 use 起到最后一个 use（含中间空行、多行 use 的续行）结束
    imports: list[str] = []
    while i < n:
        line = lines[i]
        if line.startswith("use "):
            brace = line.count("{") - line.count("}")
            imports.append(line)
            i += 1
            while brace > 0 and i < n:  # 多行 use：吃到花括号闭合为止
                imports.append(lines[i])
                brace += lines[i].count("{") - lines[i].count("}")
                i += 1
            continue
        if imports and line.strip() == "":
            imports.append(line)
            i += 1
            continue
        break
    # 去掉尾部空行，归入 body
    while imports and imports[-1].strip() == "":
        imports.pop()

    # 3) body 切项
    items: list[Item] = []
    pending: list[str] = []  # 待挂到下一项的 doc/attr 行
    cur: Item | None = None
    depth = 0
    while i < n:
        line = lines[i]
        stripped = line.strip()
        at_top = (not line.startswith((" ", "\t"))) and depth == 0
        is_attr_or_doc = at_top and (
            line.startswith("#[") or line.startswith("///") or line.startswith("//!")
        )
        m = DECL_RE.match(line) if at_top else None
        if m and not line.startswith("//"):
            # 新项开始：先把 pending 并入
            name = ""
            nm = NAME_RE.search(line)
            im = IMPL_RE.match(line)
            if nm:
                name = nm.group(1)
            elif im:
                head = im.group(1)
                fm = IMPL_FOR_RE.search(head)
                name = f"impl:{fm.group(1) if fm else head.split('<')[0].strip()}"
            else:
                name = f"decl:{line.strip()[:40]}"
            cur = Item(name=name, start=i + 1 - len(pending))
            cur.lines.extend(pending)
            pending = []
            items.append(cur)
        if is_attr_or_doc:
            pending.append(line)
            i += 1
            continue
        if cur is None:
            # 极少数顶部注释块：挂到伪项
            cur = Item(name="__head__", start=i + 1)
            items.append(cur)
        cur.lines.append(line)
        depth += line.count("{") - line.count("}")
        if depth < 0:
            depth = 0
        i += 1

    # 尾部空行从最后一项剥离，作为 trailer
    trailer: list[str] = []
    if items:
        while items[-1].lines and items[-1].lines[-1].strip() == "":
            trailer.insert(0, items[-1].lines.pop())
    return module_docs, imports, items, trailer


def unwrap_test_mod(text: str) -> str:
    """把 `#[cfg(test)] mod tests { ... }` 剥成模块体（文件本身就是 mod tests 的内容）。"""
    lines = text.splitlines(keepends=True)
    i = 0
    while i < len(lines) and (lines[i].startswith("#[") or lines[i].strip() == ""):
        i += 1
    # 跳过 `mod tests {`
    if i < len(lines) and lines[i].lstrip().startswith("mod "):
        i += 1
    body = lines[i:]
    while body and body[-1].strip() == "":
        body.pop()
    if body and body[-1].strip() == "}":
        body.pop()
    out = []
    for line in body:
        out.append(line[4:] if line.startswith("    ") else line)
    return "".join(out)


def vis_fix(text: str, visibility: str) -> str:
    """把顶层私有 `fn` / `struct` / `const` 提升到 `pub(super)`（仅跨文件调用需要）。"""
    out = []
    for line in text.splitlines(keepends=True):
        if DECL_RE.match(line) and not line.startswith(
            ("pub", "impl", "mod", "use", "#", "//", "macro_rules!")
        ):
            line = visibility + " " + line
        out.append(line)
    return "".join(out)


def header_comment(part: str, src_name: str, note: str) -> str:
    bar = "// " + "=" * 76
    return (
        f"{bar}\n"
        f"// {part}.rs —— {note}\n"
        f"//\n"
        f"// 由 `{src_name}` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：\n"
        f"// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade\n"
        f"// `{src_name}` 用 `pub use` 重导出，调用方一行不用改。\n"
        f"{bar}\n"
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--plan", required=True)
    ap.add_argument("--root", default=".")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    plan = json.loads(Path(args.plan).read_text(encoding="utf-8"))
    root = Path(args.root).resolve()
    src = root / plan["src"]
    src_name = src.name
    base = plan["base"]
    targets: dict[str, list[str]] = plan["targets"]
    notes: dict[str, str] = plan.get("notes", {})
    tests_mod = plan.get("tests_mod")

    module_docs, imports, items, trailer = split_file(src)

    owner: dict[str, str] = {}
    for part, names in targets.items():
        for nm in names:
            owner[nm] = part

    def part_of(item: Item) -> str:
        if item.name in owner:
            return owner[item.name]
        if item.name.startswith("impl:"):
            t = item.name.split(":", 1)[1]
            if t in owner:
                return owner[t]
        return base

    buckets: dict[str, list[Item]] = {base: []}
    for part in targets:
        buckets.setdefault(part, [])
    for it in items:
        buckets[part_of(it)].append(it)

    missing = [nm for nm in owner if nm not in {i.name for i in items}]
    if missing:
        print(f"!! plan 点名的项在原文件里找不到：{missing}", file=sys.stderr)
        return 2

    import_block = "".join(imports).rstrip("\n") + "\n"
    written = []

    for part, its in buckets.items():
        if part == base:
            continue
        if not its:
            print(f"!! 部件 {part} 没有任何项（plan 写错了？）", file=sys.stderr)
            return 2
        raw = "".join(it.text if it.text.endswith("\n") else it.text + "\n" for it in its)
        head = header_comment(part, src_name, notes.get(part, "按职责拆出的部件"))
        if part == tests_mod:
            body = unwrap_test_mod(raw)
            text = f"{head}\n{body}"
        else:
            body = vis_fix(raw, "pub(super)")
            text = f"{head}\n{import_block}\nuse super::{base}::*;\n\n{body}"
        written.append((src.parent / f"{part}.rs", text))

    base_body = "".join(it.text if it.text.endswith("\n") else it.text + "\n" for it in buckets[base])
    base_body = vis_fix(base_body, "pub(super)")
    base_head = header_comment(base, src_name, notes.get(base, "底座：常量 / 契约结构 / 状态机 / 纯函数"))
    written.append((src.parent / f"{base}.rs", f"{base_head}\n{import_block}\n{base_body}"))

    doc = "".join(module_docs).rstrip("\n")
    facade = [doc, ""]
    facade.append("// ==================== 部件（TD-1 拆分，facade 重导出） ====================")
    facade.append("//")
    facade.append("// 本文件只做 facade：常量 / 状态机 / 查询 / 写入 / 校验 分居各部件文件，")
    facade.append("// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——")
    facade.append("// 所有 `use crate::business::erp::<本文件>::xxx` 的调用方一行不用改。")
    facade.append("")
    order = [base] + [p for p in targets if p != tests_mod]
    for part in order:
        facade.append(f"pub use super::{part}::*;")
    if tests_mod:
        facade.append("")
        facade.append("#[cfg(test)]")
        facade.append(f'#[path = "{tests_mod}.rs"]')
        facade.append("mod tests;")
    facade_text = "\n".join(facade) + "\n" + "".join(trailer)

    if args.dry_run:
        for out, text in written:
            print(f"{out.name}: {len(text.splitlines())} 行")
        print(f"{src.name}: {len(facade_text.splitlines())} 行 (facade)")
        return 0

    for out, text in written:
        out.write_text(text, encoding="utf-8")
        print(f"写出 {out.relative_to(root)}: {len(text.splitlines())} 行")
    src.write_text(facade_text, encoding="utf-8")
    print(f"写出 {src.relative_to(root)}: {len(facade_text.splitlines())} 行 (facade)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
