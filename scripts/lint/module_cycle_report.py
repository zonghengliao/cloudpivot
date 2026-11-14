#!/usr/bin/env python3
"""业务域级依赖环检测（只读报告 · P2-31）。

## 为什么需要这个脚本

`cargo tree` 只能表达 **crate 级** 依赖。当前 `backend` 是**单 crate**（workspace 是 P2-C 的活），
`business::shared` 与 `business::erp` 之间是**模块级**依赖 —— `cargo tree` 根本画不出这条边，
更谈不上证明它消失。本脚本从源码抽 `crate::...` 引用、按「业务域」聚合，用 Tarjan 求 SCC，
把模块级环变成可复核的输出。

## 用法

    python3 scripts/lint/module_cycle_report.py                    # 业务域环报告（恒退出 0）
    python3 scripts/lint/module_cycle_report.py --fail-on-cycle    # 有环即退出 1
    python3 scripts/lint/module_cycle_report.py --from business::shared         # 某域的出边
    python3 scripts/lint/module_cycle_report.py --edge business::shared business::erp
    python3 scripts/lint/module_cycle_report.py --mutual           # 所有双向边（2 环）

## 口径

- 节点：`business::<域>`；`--all-containers` 时把 `system`/`support`/`utils`/`task`/`middleware`
  等容器也当节点（默认只看业务域，容器之间的合法分层边会把整张图粘成一个大 SCC，失去诊断价值）。
- 引用：文件全文的 `crate::a::b::c`（**含注释** —— intra-doc link 是 rustdoc 依赖）；
  注释命中单独标 `[comment]`。
- 自引用（同域内）不计。
- 扫描面硬断言：< 500 个 `.rs` 直接报错退出 —— 防「路径基准变了 → 扫 0 个文件 → 静默无环」。
"""
from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from pathlib import Path

REF = re.compile(r"crate::([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)")
MIN_FILES = 500


def resolve_src(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).resolve()
    here = Path.cwd().resolve()
    for base in (here, *here.parents):
        cand = base / "backend" / "src"
        if cand.is_dir():
            return cand
    raise SystemExit(f"找不到 backend/src（cwd={here}）；请用 --src 显式指定")


def module_of(path: Path, src: Path) -> str:
    return "::".join(path.relative_to(src).with_suffix("").parts).removesuffix("::mod")


def domain_of(module: str) -> str:
    segs = module.split("::")
    if segs[0] == "business" and len(segs) > 1:
        return "business::" + segs[1]
    return segs[0]


def collect(src: Path):
    edges: dict[tuple[str, str], dict[str, set[str]]] = defaultdict(lambda: defaultdict(set))
    domains: set[str] = set()
    files = sorted(src.rglob("*.rs"))
    if len(files) < MIN_FILES:
        raise SystemExit(f"扫描面异常：只找到 {len(files)} 个 .rs（期望 ≥{MIN_FILES}）—— 路径基准错了？")
    for path in files:
        src_domain = domain_of(module_of(path, src))
        domains.add(src_domain)
        for lineno, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
            for ref in REF.findall(line):
                dst_domain = domain_of(ref)
                if dst_domain == src_domain:
                    continue
                kind = "comment" if line.lstrip().startswith("//") else "code"
                edges[(src_domain, dst_domain)][kind].add(f"{path.relative_to(src)}:{lineno}")
    return domains, edges, len(files)


def tarjan(nodes, succ):
    index: dict[str, int] = {}
    low: dict[str, int] = {}
    stack: list[str] = []
    on_stack: set[str] = set()
    sccs: list[list[str]] = []
    counter = 0

    def strongconnect(v: str) -> None:
        nonlocal counter
        index[v] = low[v] = counter
        counter += 1
        stack.append(v)
        on_stack.add(v)
        for w in succ.get(v, ()):
            if w not in index:
                strongconnect(w)
                low[v] = min(low[v], low[w])
            elif w in on_stack:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            comp: list[str] = []
            while True:
                w = stack.pop()
                on_stack.discard(w)
                comp.append(w)
                if w == v:
                    break
            sccs.append(comp)

    for v in sorted(nodes):
        if v not in index:
            strongconnect(v)
    return [sorted(c) for c in sccs]


def counts(edges, a, b):
    hit = edges[(a, b)]
    return len(hit.get("code", ())), len(hit.get("comment", ()))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default=None, help="源码根（默认从 cwd 向上找 backend/src）")
    ap.add_argument("--fail-on-cycle", action="store_true")
    ap.add_argument("--all-containers", action="store_true", help="把 system/support/utils 等容器也当节点")
    ap.add_argument("--from", dest="from_domain", default=None, help="列出某域的出边")
    ap.add_argument("--edge", nargs=2, action="append", default=[])
    ap.add_argument("--mutual", action="store_true", help="列出所有双向边（2 环）")
    args = ap.parse_args()

    src = resolve_src(args.src)
    domains, edges, nfiles = collect(src)
    if not args.all_containers:
        domains = {d for d in domains if d.startswith("business::")}
        edges = {k: v for k, v in edges.items() if k[0] in domains and k[1] in domains}

    succ: dict[str, set[str]] = defaultdict(set)
    for (a, b) in edges:
        succ[a].add(b)

    print(f"源码根：{src}")
    print(f"扫描面：{nfiles} 个 .rs / 节点 {len(domains)} 个 / 跨域有向边 {len(edges)} 条 "
          f"（口径：{'含容器' if args.all_containers else '仅 business::*'}）")
    print()

    if args.from_domain:
        outs = sorted(b for (a, b) in edges if a == args.from_domain)
        print(f"{args.from_domain} 的出边：{len(outs)} 条")
        for b in outs:
            code, comment = counts(edges, args.from_domain, b)
            print(f"  → {b}  (代码 {code} / 注释 {comment})")
        print()

    for (a, b) in args.edge:
        if (a, b) in edges:
            code, comment = counts(edges, a, b)
            print(f"边 {a} → {b}：存在（代码 {code} / 注释 {comment}）")
            for kind in ("code", "comment"):
                for loc in sorted(edges[(a, b)].get(kind, ()))[:6]:
                    print(f"  [{kind}] {loc}")
        else:
            print(f"边 {a} → {b}：**不存在**（该域内无任何 crate:: 引用指向 {b}）")
        print()

    if args.mutual:
        pairs = sorted({tuple(sorted((a, b))) for (a, b) in edges if (b, a) in edges})
        print(f"双向边（互为依赖，2 环）：{len(pairs)} 对")
        for a, b in pairs:
            ca, ma = counts(edges, a, b)
            cb, mb = counts(edges, b, a)
            print(f"  ● {a} → {b} (代码 {ca}/注释 {ma})  ⇄  {b} → {a} (代码 {cb}/注释 {mb})")
        print()

    cycles = [c for c in tarjan(domains, succ) if len(c) > 1]
    cycles.sort(key=lambda c: (-len(c), c))
    print(f"SCC 环（长度 >1）：{len(cycles)} 个")
    for comp in cycles:
        print(f"  ● {len(comp)} 个域互为可达：{' ↔ '.join(comp)}")
        for a in comp:
            for b in sorted(succ.get(a, ())):
                if b in comp and b != a:
                    code, comment = counts(edges, a, b)
                    print(f"      {a} → {b}  (代码 {code} / 注释 {comment})")

    if args.fail_on_cycle and cycles:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
