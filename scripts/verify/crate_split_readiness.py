#!/usr/bin/env python3
"""抽 crate 就绪度体检（P2-C 排期工具）。

## 为什么需要它

`Cargo` 的包依赖图**必须无环** —— 两个包互相 `path` 依赖时 `cargo check` 直接报
`error: cyclic package dependency`。所以「把某个模块抽成独立 crate」这件事，能不能成立
只取决于一件事：**抽走之后，还留在应用 crate 里的代码有没有回边指向它**。

本脚本把这件事量化成两个数：

1. **回边（in-edges）** —— 留在应用 crate 的代码里，有多少行引用目标域。有代码级回边
   ⇒ 直接抽就成环（要把这些引用倒转成接缝（port / 注册表）才能抽）。
2. **强连通分量（SCC）** —— 目标域与谁互相引用。SCC 里的节点**不可能**拆成两个无环的
   包（A→B 与 B→A 同时存在），必须先解环或合并成一个包。

## 用法

```bash
python3 scripts/verify/crate_split_readiness.py                  # 全量：顶层域 + erp 子域
python3 scripts/verify/crate_split_readiness.py --domain erp     # 单域明细（顶层域）
python3 scripts/verify/crate_split_readiness.py --domain cost --scope erp-sub
python3 scripts/verify/crate_split_readiness.py --json           # 机器可读
python3 scripts/verify/crate_split_readiness.py --domain cost --scope erp-sub --check
#   ↑ --check：域可抽时退出码 0，不可抽时退出码 1（给卡内验收用）
```

**只读**：不写文件、不连库、不改代码；不进门禁（这是排期工具，不是质量闸）。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections import defaultdict

SRC = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))), "backend", "app", "src")
BIZ = os.path.join(SRC, "business")
ERP = os.path.join(BIZ, "erp")

TOP_KERNEL = ("shared", "utils", "system", "support", "middleware", "task")


def _rs_files(path: str) -> list[str]:
    if os.path.isfile(path):
        return [path]
    out = []
    for dirpath, _dirnames, filenames in os.walk(path):
        for name in filenames:
            if name.endswith(".rs"):
                out.append(os.path.join(dirpath, name))
    return sorted(out)


def collect_nodes(scope: str) -> dict[str, list[str]]:
    """返回 {节点名: 源码文件列表}。"""
    nodes: dict[str, list[str]] = {}
    if scope == "top":
        for name in sorted(os.listdir(BIZ)):
            path = os.path.join(BIZ, name)
            if os.path.isdir(path):
                nodes[name] = _rs_files(path)
            elif name.endswith(".rs"):
                nodes.setdefault("business#root", []).extend(_rs_files(path))
        for name in TOP_KERNEL:
            path = os.path.join(SRC, name)
            if os.path.isdir(path):
                nodes[name] = _rs_files(path)
        for name in ("lib.rs", "main.rs", "bootstrap.rs"):
            path = os.path.join(SRC, name)
            if os.path.isfile(path):
                nodes.setdefault("app#root", []).extend(_rs_files(path))
    elif scope == "erp-sub":
        for name in sorted(os.listdir(ERP)):
            path = os.path.join(ERP, name)
            if os.path.isdir(path):
                nodes[name] = _rs_files(path)
            elif name.endswith(".rs"):
                nodes.setdefault("erp#root:" + name[:-3], []).extend(_rs_files(path))
        # 外部域（跨出 erp 的边仍然要报，只是不进同一张 SCC）
        for name in ("flow", "hr", "impexp", "changelog", "masking", "task", "shared"):
            path = os.path.join(BIZ, name) if name != "shared" else os.path.join(SRC, "shared")
            if os.path.isdir(path):
                nodes["ext:" + name] = _rs_files(path)
    else:
        raise SystemExit(f"未知 scope: {scope}")
    return nodes


def build_graph(scope: str):
    nodes = collect_nodes(scope)
    owner: dict[str, str] = {}
    for node, files in nodes.items():
        for f in files:
            owner[f] = node

    if scope == "top":
        candidates = [n for n in nodes if n not in TOP_KERNEL and n != "app#root"]
        pattern = re.compile(r"crate::business::(" + "|".join(sorted(map(re.escape, candidates))) + r")\b")
        pattern2 = re.compile(r"crate::(" + "|".join(TOP_KERNEL) + r")\b")
    else:
        sub = [n for n in nodes if not n.startswith("ext:")]
        names = [n.split(":", 1)[1] if ":" in n else n for n in sub]
        pattern = re.compile(r"(?:crate::business::erp::|super::super::)(" + "|".join(sorted(map(re.escape, names))) + r")\b")
        pattern2 = re.compile(r"crate::business::(flow|hr|impexp|changelog|masking|task)\b")

    edges: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))   # 全部（含注释）
    code: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))    # 只算代码
    where: dict[tuple[str, str], list[str]] = defaultdict(list)
    loc: dict[str, int] = defaultdict(int)
    nfile: dict[str, int] = defaultdict(int)

    def resolve(node: str, raw: str) -> str | None:
        if raw in nodes:
            return raw
        cand = ("erp#root:" if scope == "erp-sub" else "business#root:") + raw
        if cand in nodes:
            return cand
        return None

    for path, node in owner.items():
        try:
            text = open(path, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        loc[node] += text.count("\n")
        nfile[node] += 1
        for lineno, line in enumerate(text.splitlines(), 1):
            is_comment = line.lstrip().startswith("//")
            for pat in (pattern, pattern2):
                for m in pat.finditer(line):
                    raw = m.group(1)
                    target = "ext:" + raw if scope == "erp-sub" and pat is pattern2 else resolve(node, raw)
                    if target is None or target == node:
                        continue
                    edges[node][target] += 1
                    rel = os.path.relpath(path, SRC)
                    if not is_comment:
                        code[node][target] += 1
                        where[(node, target)].append(f"{rel}:{lineno}")
    return nodes, edges, code, where, loc, nfile


def tarjan(graph: dict[str, list[str]]) -> list[list[str]]:
    index: dict[str, int] = {}
    low: dict[str, int] = {}
    on: dict[str, bool] = {}
    stack: list[str] = []
    comps: list[list[str]] = []
    counter = [0]

    def visit(v: str) -> None:
        index[v] = low[v] = counter[0]
        counter[0] += 1
        stack.append(v)
        on[v] = True
        for w in graph.get(v, ()):  # type: ignore[union-attr]
            if w not in index:
                visit(w)
                low[v] = min(low[v], low[w])
            elif on.get(w):
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            comp = []
            while True:
                w = stack.pop()
                on[w] = False
                comp.append(w)
                if w == v:
                    break
            comps.append(sorted(comp))

    for v in list(graph):
        if v not in index:
            visit(v)
    return sorted(comps, key=len, reverse=True)


def report(scope: str, domain: str | None, as_json: bool) -> int:
    nodes, edges, code, where, loc, nfile = build_graph(scope)
    graph = {n: list(edges[n]) for n in nodes}
    comps = tarjan(graph)
    comp_of: dict[str, int] = {}
    for i, comp in enumerate(comps):
        for n in comp:
            comp_of[n] = i
    scc_size = {n: len(comps[comp_of[n]]) for n in comp_of}

    def inbound(target: str):
        rows = []
        for src in sorted(code):
            if src != target and target in code[src]:
                rows.append((src, code[src][target], edges[src][target], where[(src, target)]))
        return rows

    if domain:
        if domain not in nodes:
            print(f"未知域：{domain}（scope={scope}）。可用：{', '.join(sorted(nodes))}", file=sys.stderr)
            return 2
        in_rows = inbound(domain)
        code_in = sum(r[1] for r in in_rows)
        out_rows = sorted(((t, n, edges[domain][t]) for t, n in code[domain].items()), key=lambda r: -r[1])
        verdict = "可抽" if code_in == 0 else f"不可抽（{code_in} 处代码级回边，{len(in_rows)} 个来源）"
        if as_json:
            print(json.dumps({
                "scope": scope, "domain": domain, "loc": loc[domain], "files": nfile[domain],
                "scc_size": scc_size.get(domain), "scc": comps[comp_of[domain]],
                "inbound": [{"from": s, "code": c, "all": a, "where": w} for s, c, a, w in in_rows],
                "outbound": [{"to": t, "code": c, "all": a} for t, c, a in out_rows],
                "verdict": verdict,
            }, ensure_ascii=False, indent=2))
        else:
            print(f"域：{domain}（scope={scope}）")
            print(f"  规模：{nfile[domain]} 个文件 / {loc[domain]} 行")
            print(f"  SCC：{scc_size.get(domain)} 个节点同属一个强连通分量"
                  f"{'' if scc_size.get(domain) == 1 else '（>1 ⇒ 与它们不可能拆成两个无环包）'}")
            if scc_size.get(domain, 1) > 1:
                others = [n for n in comps[comp_of[domain]] if n != domain]
                print(f"  同 SCC 的其它节点（{len(others)}）：{', '.join(others)}")
            print(f"  回边（留在应用 crate 的代码 → 本域）：{code_in} 处代码级")
            for src, c, a, w in in_rows:
                print(f"    {src:22s} code={c:3d} all={a:3d}  {', '.join(w[:6])}")
            if not in_rows:
                print("    （无）")
            print(f"  出边（本域 → 其它）：{len(out_rows)} 个目标")
            for t, c, a in out_rows:
                print(f"    {t:22s} code={c:3d} all={a:3d}")
            print(f"  结论：{verdict}")
        if code_in:
            return 1
        return 0

    # 全量表格
    rows = []
    for node in sorted(nodes):
        if node in TOP_KERNEL or node.startswith("ext:") or node.endswith("#root"):
            continue
        in_rows = inbound(node)
        rows.append({
            "domain": node, "loc": loc[node], "files": nfile[node],
            "scc": scc_size.get(node, 1), "inbound_code": sum(r[1] for r in in_rows),
            "inbound_srcs": len(in_rows),
            "out_targets": len(code[node]),
        })
    if as_json:
        print(json.dumps({"scope": scope, "domains": rows}, ensure_ascii=False, indent=2))
        return 0
    print(f"scope={scope} · SRC={SRC}")
    print(f"{'域':24s} {'文件':>5s} {'行数':>8s} {'SCC':>4s} {'回边源':>6s} {'回边行':>6s} {'出边':>5s}  结论")
    for r in rows:
        verdict = "可抽" if r["inbound_code"] == 0 else "先解环"
        if r["scc"] > 1:
            verdict = f"SCC={r['scc']}·{verdict}"
        print(f"{r['domain']:24s} {r['files']:5d} {r['loc']:8d} {r['scc']:4d} "
              f"{r['inbound_srcs']:6d} {r['inbound_code']:6d} {r['out_targets']:5d}  {verdict}")
    big = [c for c in comps if len(c) > 1]
    print(f"\n强连通分量（size>1）：{len(big)} 个 —— 每个分量内部**不可能**拆成两个无环 crate")
    for comp in big:
        print(f"  size={len(comp)}：{', '.join(comp)}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="抽 crate 就绪度体检（只读）")
    ap.add_argument("--domain", help="看单个域：顶层域（erp/flow/hr/…）或 erp 子域（cost/purchase/…）")
    ap.add_argument("--scope", choices=("top", "erp-sub"),
                    default=None, help="top=顶层域（默认）；erp-sub=business/erp 下的一级子域")
    ap.add_argument("--json", action="store_true", help="机器可读输出")
    ap.add_argument("--check", action="store_true", help="域可抽→退出码 0；有回边→1（配合 --domain）")
    args = ap.parse_args()
    scope = args.scope or ("erp-sub" if args.domain in _erp_subdirs() else "top")
    rc = report(scope, args.domain, args.json)
    return rc if args.check else 0


def _erp_subdirs() -> set[str]:
    if not os.path.isdir(ERP):
        return set()
    return {n for n in os.listdir(ERP) if os.path.isdir(os.path.join(ERP, n))}


if __name__ == "__main__":
    sys.exit(main())
