#!/usr/bin/env python3
"""TD-2 拆分器：把一个 Rust 模块文件按「职责」切成多文件 + facade 重导出。

与 TD-1 的 scripts/refactor/split_module.py 语义一致（只搬不移），在其上加了：
  1. 任意 src 路径（不限 erp/）；
  2. 每个部件可声明 extra_uses（跨部件互调时显式 use）；
  3. 测试模块的搬离与「测试模块内部分组」：
     - tests: [{"mod": "tests", "file": "x_tests.rs"}]  → 整个 mod 搬到文件，facade 用 #[path] 挂载（模块名不变）；
     - "inner_of": 把某个测试 mod 的**内部条目**按组拆到多个文件（超千行的测试 mod 必须这么做）。

用法：
    python3 td2_split.py --plan plans/task_model.json --root /opt/cloudpivot/backend [--dry-run]
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import split_module as sm  # 复用 TD-1 的解析器（parse 逻辑一字未改）


def header_comment(part: str, src_name: str, note: str) -> str:
    bar = "// " + "=" * 76
    return (
        f"{bar}\n"
        f"// {part}.rs —— {note}\n"
        f"//\n"
        f"// 由 `{src_name}` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：\n"
        f"// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade\n"
        f"// `{src_name}` 用 `pub use` 重导出，调用方一行不用改。\n"
        f"{bar}\n"
    )


def dedent(lines: list[str], n: int = 4) -> list[str]:
    pad = " " * n
    return [l[n:] if l.startswith(pad) else l for l in lines]


def split_test_mod(mod_text: str, modname: str):
    """把 `#[cfg(test)] mod X { ... }` 拆成 (前置 docs/attr 行, 模块体内条目)。"""
    lines = mod_text.splitlines(keepends=True)
    i = 0
    prefix: list[str] = []
    while i < len(lines) and not re.match(r"\s*(pub\s+)?mod\s+%s\b" % re.escape(modname), lines[i]):
        prefix.append(lines[i])
        i += 1
    assert i < len(lines), "找不到 mod %s 的声明行" % modname
    body = lines[i + 1:]
    end = max(j for j, l in enumerate(body) if l.strip() == "}")
    # 结尾的 `}` 与可能的尾随空行丢弃
    body = body[:end]
    body = dedent(body)
    items = _parse_top(body)
    return prefix, items


def _parse_top(body: list[str]):
    items = []
    cur = None
    pending = []
    depth = 0
    for raw in body:
        s = raw.strip()
        at_top = (not raw.startswith((" ", "\t"))) and depth == 0
        is_doc = at_top and (s.startswith("#[") or s.startswith("///") or s.startswith("//"))
        m = sm.DECL_RE.match(raw) if at_top else None
        if m and not s.startswith("//"):
            nm = sm.NAME_RE.search(raw)
            im = sm.IMPL_RE.match(raw)
            name = nm.group(1) if nm else (im.group(1) if im else s[:40])
            cur = {"name": name, "lines": list(pending)}
            pending = []
            items.append(cur)
        if is_doc:
            pending.append(raw)
            continue
        if cur is None:
            cur = {"name": "__head__", "lines": []}
            items.append(cur)
        cur["lines"].append(raw)
        depth += raw.count("{") - raw.count("}")
        depth = max(depth, 0)
    return items


def item_text(it) -> str:
    t = "".join(it["lines"] if isinstance(it, dict) else it.lines)
    return t if t.endswith("\n") else t + "\n"


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

    module_docs, imports, items, trailer = sm.split_file(src)
    import_block = "".join(imports).rstrip("\n") + "\n"
    by_name = {i.name: i for i in items}
    owner: dict[str, str] = {}
    parts = plan["parts"]
    for p in parts:
        for nm in p["items"]:
            assert nm in by_name, "%s: plan 点名的项不存在：%s" % (src_name, nm)
            assert nm not in owner, "%s: 项被点名两次：%s" % (src_name, nm)
            owner[nm] = p["name"]
    base = parts[0]["name"]
    test_mods = set()
    for spec in plan.get("tests", []):
        test_mods.add(spec.get("mod") or spec["inner_of"])
    for it in items:
        if it.name in test_mods:
            continue
        if it.name not in owner:
            owner[it.name] = base
            parts[0]["items"].append(it.name)

    # 条目编码为 {part: [Item]}
    buckets: dict[str, list] = {p["name"]: [] for p in parts}
    for it in items:
        if it.name in test_mods:
            continue
        buckets[owner[it.name]].append(it)

    written: list[tuple[Path, str]] = []
    for p in parts:
        its = buckets[p["name"]]
        assert its, "部件 %s 没有任何项" % p["name"]
        body = sm.vis_fix("".join(item_text(i) for i in its), "pub(super)")
        head = header_comment(p["name"], src_name, p.get("note", "按职责拆出的部件"))
        extra = "".join(p.get("extra_uses", []))
        if p["name"] != base:
            extra = f"use super::{base}::*;\n" + extra
        if extra and not extra.endswith("\n"):
            extra += "\n"
        text = f"{head}\n{import_block}{extra}\n{body}"
        written.append((src.parent / f"{p['name']}.rs", text))

    # ---------- 测试模块 ----------
    facade_test_lines: list[str] = []
    for spec in plan.get("tests", []):
        if "inner_of" not in spec:
            modname = spec["mod"]
            it = by_name[modname]
            prefix, inner = split_test_mod(item_text(it), modname)
            body = "".join(item_text(i) for i in inner)
            head = header_comment(spec["file"].replace(".rs", ""), src_name, spec.get("note", "测试部件"))
            text = f"{head}\n{import_block}\n{body}"
            written.append((src.parent / spec["file"], text))
            # facade 挂载：保留原来的 doc/attr 行（含 #[cfg(test)]）
            facade_test_lines.extend(l.rstrip("\n") for l in prefix)
            facade_test_lines.append(f'#[path = "{spec["file"]}"]')
            facade_test_lines.append(f"mod {modname};")
            facade_test_lines.append("")
            continue

        # 内部分组
        src_mod = spec["inner_of"]
        it = by_name[src_mod]
        prefix, inner = split_test_mod(item_text(it), src_mod)
        inner_by_name = {i["name"]: i for i in inner}
        assigned: set[str] = set()
        groups = spec["into"]
        doc_prefix = [l.rstrip("\n") for l in prefix]
        for g in groups:
            names = g.get("items")
            if names == "ALL":
                names = [i["name"] for i in inner]
            names = [n for n in names if n not in g.get("exclude", [])]
            picked = []
            for n in names:
                assert n in inner_by_name, "%s: mod %s 里没有条目 %s" % (src_name, src_mod, n)
                assert n not in assigned, "%s: 条目被分到两组：%s" % (src_name, n)
                assigned.add(n)
                picked.append(inner_by_name[n])
            body_parts = []
            for i in picked:
                txt = item_text(i)
                if g.get("unwrap_mod"):
                    lines = txt.splitlines(keepends=True)
                    j = next(k for k, l in enumerate(lines)
                             if re.match(r"\s*(pub\s+)?mod\s+\w+", l))
                    sub = lines[j + 1:]
                    end = max(k for k, l in enumerate(sub) if l.strip() == "}")
                    body_parts.append("".join(dedent(sub[:end])))
                else:
                    body_parts.append(txt)
            body = "".join(body_parts)
            publish_all = g.get("publish_all", False)
            if publish_all:
                body = sm.vis_fix(body, "pub(super)")
            head = header_comment(g["file"].replace(".rs", ""), src_name, g.get("note", "测试部件"))
            extra = "".join(g.get("extra_uses", []))
            if extra and not extra.endswith("\n"):
                extra += "\n"
            text = f"{head}\n{import_block}{extra}\n{body}"
            written.append((src.parent / g["file"], text))
            if g.get("mount", True):
                if not doc_prefix:
                    facade_test_lines.append("#[cfg(test)]")
                elif g["name"] == src_mod:
                    # 继承原 mod 的 doc/attr 行（含 #[cfg(test)]）
                    facade_test_lines.extend(doc_prefix)
                else:
                    facade_test_lines.append("#[cfg(test)]")
                facade_test_lines.append(f'#[path = "{g["file"]}"]')
                facade_test_lines.append(f"mod {g['name']};")
                facade_test_lines.append("")
        missing = [i["name"] for i in inner
                   if i["name"] not in assigned and i["name"] not in set(spec.get("drop", []))]
        assert not missing, "%s: mod %s 有条目未分组：%s" % (src_name, src_mod, missing)

    # ---------- facade ----------
    doc = "".join(module_docs).rstrip("\n")
    facade = [doc, ""]
    facade.append("// ==================== 部件（TD-2 拆分，facade 重导出） ====================")
    facade.append("//")
    facade.append("// 本文件只做 facade：底座 / 行结构 / 查询 / 写入 / 校验 分居各部件文件，")
    facade.append("// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——")
    facade.append("// 所有 `use crate::business::<模块>::<本文件>::xxx` 的调用方一行不用改。")
    facade.append("")
    for p in parts:
        facade.append(f"pub use super::{p['name']}::*;")
    if facade_test_lines:
        facade.append("")
        facade.append("// ==================== 测试（#[path] 挂到各测试部件） ====================")
        facade.extend(facade_test_lines)
    facade_text = "\n".join(facade).rstrip("\n") + "\n" + "".join(trailer)

    if args.dry_run:
        for out, text in written:
            n = len(text.splitlines())
            print(("  !! %s: %d 行" if n >= 1000 else "     %s: %d 行") % (out.name, n))
        print("facade %s: %d 行" % (src.name, len(facade_text.splitlines())))
        bad = [o.name for o, t in written if len(t.splitlines()) >= 1000]
        print("超限文件：", bad or "无")
        return 0

    for out, text in written:
        out.write_text(text, encoding="utf-8")
        print("写出", out.relative_to(root), len(text.splitlines()), "行")
    src.write_text(facade_text, encoding="utf-8")
    print("写出", src.relative_to(root), len(facade_text.splitlines()), "行 (facade)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
