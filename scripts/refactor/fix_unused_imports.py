#!/usr/bin/env python3
"""按 `cargo check` 的 unused-imports 警告，自动裁剪部件文件的 import 块。

背景：TD-1 拆分只许「搬」不许「移」，所以每个部件都继承了原文件的整块 import；
裁剪 import 是拆分后唯一必须做的文本收敛（否则 `-D warnings` 不干净）。
做法：把每条 use 语句归一化成 `use 前缀::{名字列表};` 后逐名字删除，最后交给
`cargo fmt` 重新排版 —— 不做行级外科手术，避免把多行 use 拆坏。

用法：
    cargo check --lib > /tmp/check.log 2>&1 || true
    python3 scripts/refactor/fix_unused_imports.py --log /tmp/check.log --root backend
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path

WARN_RE = re.compile(r"^warning: unused imports?: (.+)$")
SPAN_RE = re.compile(r"^\s+--> ([^:]+):(\d+):(\d+)")
NAME_RE = re.compile(r"`([^`]+)`")


def parse_warnings(log_text: str):
    out = []
    pending = None
    for line in log_text.splitlines():
        m = WARN_RE.match(line)
        if m:
            pending = NAME_RE.findall(m.group(1))
            continue
        s = SPAN_RE.match(line)
        if s and pending is not None:
            out.append((s.group(1), int(s.group(2)), pending))
            pending = None
    return out


def header_range(lines: list[str]) -> tuple[int, int]:
    """import 区间 [start, end)：跳过文件头注释，收集开头的 use 语句。"""
    i, n = 0, len(lines)
    while i < n and (lines[i].startswith("//") or lines[i].strip() == ""):
        i += 1
    start = i
    seen = False
    while i < n:
        if lines[i].startswith(("use ", "pub use ")):
            seen = True
            while i < n and not lines[i].rstrip().endswith(";"):
                i += 1
            i += 1
            continue
        if seen and lines[i].strip() == "":
            i += 1
            continue
        break
    return start, i


def split_stmts(lines, start, end):
    """[(stmt_start, stmt_end_exclusive)]"""
    spans, i = [], start
    while i < end:
        if lines[i].startswith(("use ", "pub use ")):
            j = i
            while j < end and not lines[j].rstrip().endswith(";"):
                j += 1
            spans.append((i, j + 1))
            i = j + 1
        else:
            i += 1
    return spans


def normalize_stmt(text: str):
    """`use a::{b, c};` / `use a::b;` → (kind, prefix, names, alias)"""
    body = text.strip()
    assert body.endswith(";")
    body = body[:-1].strip()
    if body.startswith("pub use "):
        body = body[len("pub use "):].strip()
    elif body.startswith("use "):
        body = body[len("use "):].strip()
    if "{" in body:
        prefix = body.split("{", 1)[0]
        inner = body.split("{", 1)[1].rsplit("}", 1)[0]
        names = [x.strip() for x in inner.replace("\n", " ").split(",") if x.strip()]
        return "braced", prefix, names, None
    # 简单路径：可能是 `a::b as c`
    alias = None
    if " as " in body:
        body, alias = [x.strip() for x in body.split(" as ", 1)]
    return "simple", body, [body.split("::")[-1]], alias


def render(kind, prefix, names, alias) -> str | None:
    if not names:
        return None
    if kind == "simple":
        return f"use {names[0]}" + (f" as {alias}" if alias else "") + ";\n"
    one = f"use {prefix}{{{', '.join(names)}}};"
    if len(one) <= 100:
        return one + "\n"
    inner = "".join(f"    {n},\n" for n in names)
    return f"use {prefix}{{\n{inner}}};\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", required=True)
    ap.add_argument("--root", default=".")
    args = ap.parse_args()

    log = Path(args.log)
    warnings = parse_warnings(log.read_text(encoding="utf-8", errors="replace"))
    per_file: dict[str, list[tuple[int, list[str]]]] = {}
    for f, line, names in warnings:
        per_file.setdefault(f, []).append((line, names))

    root = Path(args.root)
    touched = 0
    for f, entries in per_file.items():
        p = root / f
        if not p.exists():
            print(f"跳过（文件不存在）: {f}")
            continue
        lines = p.read_text(encoding="utf-8").splitlines(keepends=True)
        hstart, hend = header_range(lines)
        spans = split_stmts(lines, hstart, hend)

        # 语句 idx -> 待删名字
        drop: dict[int, set[str]] = {}
        for line_no, names in entries:
            idx = line_no - 1
            for k, (s, e) in enumerate(spans):
                if s <= idx < e:
                    if "*" in "".join(lines[s:e]):  # glob 语句不动
                        break
                    drop.setdefault(k, set()).update(names)
                    break

        removed = set()
        out: list[str] = []
        cursor = 0
        for k, (s, e) in enumerate(spans):
            out.extend(lines[cursor:s])
            text = "".join(lines[s:e])
            if k in drop:
                kind, prefix, names, alias = normalize_stmt(text)
                for nm in drop[k]:
                    target = nm.split("::")[-1]
                    if target in names:
                        names = [x for x in names if x != target]
                        removed.add(nm)
                new = render(kind, prefix, names, alias)
                if new:
                    out.append(new)
            else:
                out.append(text)
            cursor = e
        out.extend(lines[cursor:])

        new_text = "".join(out)
        if new_text != p.read_text(encoding="utf-8"):
            p.write_text(new_text, encoding="utf-8")
            touched += 1
            print(f"裁剪 {f}: -{sorted(removed) if removed else '空语句'}")
    print(f"共处理 {touched} 个文件")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
