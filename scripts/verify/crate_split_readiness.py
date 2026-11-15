#!/usr/bin/env python3
"""抽 crate 就绪度体检（P2-C 排期工具）。

## 为什么需要它

`Cargo` 的包依赖图**必须无环** —— 两个包互相 `path` 依赖时 `cargo check` 直接报
`error: cyclic package dependency`。所以「把某个模块抽成独立 crate」这件事，能不能成立
只取决于一件事：**抽走之后，这个模块的出边是不是都指向更下层（已抽出的包 / 内核）**。

两个方向不对称，必须分开看（P2-C 重切后新增，2026-11-25）：

- **入边（app / 别的域 → 本域）**：合法方向。装配层本来就要依赖域 crate（否则它的
  `inventory` 路由项进不了链接单元 ⇒ 静默 404）。它只决定「改 import 的面」有多大。
- **出边（本域 → 还留在单体的代码）**：环的唯一来源，**必须切**（port / 注册表 /
  词表上提 / 事件订阅）。

本脚本量化的三件事：

1. **回边（in-edges）** —— 有多少行（多少来源）引用目标域 ⇒ 改 import 的面。
2. **出边（out-edges）** —— 目标域引用了谁 ⇒ 抽出前必须切掉的账。
3. **强连通分量（SCC）** —— 互相引用成环的节点集合。SCC 里的节点**不可能**拆成两个
   无环的包（A→B 与 B→A 同时存在），必须先解环或合并成一个包。

## 用法

```bash
python3 scripts/verify/crate_split_readiness.py                  # 全量：顶层域 + erp 子域
python3 scripts/verify/crate_split_readiness.py --domain erp     # 单域明细（入边 + 出边 + 层次判据）
python3 scripts/verify/crate_split_readiness.py --domain cost --scope erp-sub
python3 scripts/verify/crate_split_readiness.py --json           # 机器可读

# 层次口径（推荐）：出边全落在 L1 内核 ⇒ 退 0；否则退 1 并列出本步要切的出边
python3 scripts/verify/crate_split_readiness.py --domain masking --layer-check

# 卡序校验：读 docs 里 ```p2c-order 代码块，按序模拟抽包，每一步都对得上 ⇒ 退 0
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md

# 旧口径（只看入边，方向反了：dashboard 入边 0 但出边 6，抽出来照样成环）
python3 scripts/verify/crate_split_readiness.py --domain cost --scope erp-sub --check
```

## 已知口径

- 只数 `crate::…` **绝对路径**（`super::…` 相对路径按同包处理），跨域引用绝大多数是绝对路径。
- 节点命名：`business/<名>` 与 `src/<名>` 同名时（`shared` / `task`）分别成节点；
  早期版本这里有个覆盖 bug，导致 `business/shared`(14 384 行) 与 `business/task`(27 688 行)
  从未进图（已修，2026-11-25）。

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
# 内核 crate（P2-59 · C1）：`shared` / `utils` / `business/shared` 已整体搬到这里。
# 它们仍是 L1 基准节点，只是源码根换了 —— 不认这个根的话 `--plan-order` 会报「未知节点」，
# `--domain shared` 也会因为「节点没文件」而看不见出边（假绿）。
KERNEL_SRC = os.path.join(os.path.dirname(os.path.dirname(SRC)), "crates", "cloudpivot-kernel", "src")

TOP_KERNEL = ("shared", "utils", "system", "support", "middleware", "task")

# L1 内核的目标内容（P2-C1）：层次切法的「更下层」基准。
# - `shared` / `utils` 是 src/ 下的横切模块；
# - `business/shared` 是 business 下的横切模块（state_machine / scope / principal /
#   separation / cost_switch / voucher_port / test_helpers …）。
# 判据：一个节点能抽成 crate ⇔ 它的**出边**全部落在 {ASSUMED_KERNEL ∪ 已抽出的包} 里。
ASSUMED_KERNEL = ("shared", "utils", "business/shared")


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
            # ⚠ `business/shared` 与 `business/task` 必须有自己的节点名：它们与
            # `src/shared` / `src/task` 同名，早先版本先写 nodes["shared"]（business/shared）
            # 再被 TOP_KERNEL 的 src/shared **覆盖** ⇒ business/shared（14 384 行）与
            # business/task（27 688 行）整个目录没有 owner、根本没被扫描（2026-11-25 修）。
            node_name = "business/" + name if name in TOP_KERNEL else name
            if os.path.isdir(path):
                nodes[node_name] = _rs_files(path)
            elif name.endswith(".rs"):
                nodes.setdefault("business#root", []).extend(_rs_files(path))
        for name in TOP_KERNEL:
            path = os.path.join(SRC, name)
            if os.path.isdir(path):
                nodes[name] = _rs_files(path)
        # P2-59（C1）：内核三节点（`shared` / `utils` / `business/shared`）的源码在内核 crate 里。
        for rel in ASSUMED_KERNEL:
            path = os.path.join(KERNEL_SRC, *rel.split("/"))
            if os.path.isdir(path):
                nodes[rel] = _rs_files(path)
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

    # 内核已抽出（P2-59 · C1）：应用侧引用写成 `cloudpivot_kernel::shared|utils` 与
    # `cloudpivot_kernel::business::shared`。这两个 pattern 与 scope 无关，先建好；
    # `erp-sub` 口径下用「永不匹配」占位（那边只关心 erp 内部与外域）。
    kernel_biz = [n.split("/")[-1] for n in ASSUMED_KERNEL if "/" in n]
    kernel_top = [n for n in ASSUMED_KERNEL if "/" not in n]
    pattern3 = re.compile(r"cloudpivot_kernel::business::(" + "|".join(sorted(kernel_biz)) + r")\b")
    pattern4 = re.compile(r"cloudpivot_kernel::(" + "|".join(sorted(kernel_top)) + r")\b")
    if scope == "top":
        # 候选名 = 模块路径的最后一段（`business/shared` 的模块名是 `shared`）
        candidates = [n.split("/")[-1] for n in nodes if n not in TOP_KERNEL and n != "app#root"]
        pattern = re.compile(r"crate::business::(" + "|".join(sorted(map(re.escape, candidates))) + r")\b")
        pattern2 = re.compile(r"crate::(" + "|".join(TOP_KERNEL) + r")\b")
    else:
        sub = [n for n in nodes if not n.startswith("ext:")]
        names = [n.split(":", 1)[1] if ":" in n else n for n in sub]
        pattern = re.compile(r"(?:crate::business::erp::|super::super::)(" + "|".join(sorted(map(re.escape, names))) + r")\b")
        pattern2 = re.compile(r"crate::business::(flow|hr|impexp|changelog|masking|task)\b")
        pattern3 = pattern4 = re.compile(r"(?!)")  # 占位：erp-sub 口径不判内核边

    edges: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))   # 全部（含注释）
    code: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))    # 只算代码
    where: dict[tuple[str, str], list[str]] = defaultdict(list)
    loc: dict[str, int] = defaultdict(int)
    nfile: dict[str, int] = defaultdict(int)

    def resolve(node: str, raw: str, under_business: bool = False) -> str | None:
        # `crate::business::shared::…` 指的是 `business/shared`；`crate::shared::…` 指的是
        # `src/shared`（内核）。两者名字相同，必须靠「匹配来自哪个 pattern」区分。
        if under_business and ("business/" + raw) in nodes:
            return "business/" + raw
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
            for pat in (pattern, pattern2, pattern3, pattern4):
                for m in pat.finditer(line):
                    raw = m.group(1)
                    if pat is pattern3:
                        target = ("business/" + raw) if ("business/" + raw) in nodes else None
                    elif pat is pattern4:
                        target = raw if raw in nodes else None
                    else:
                        target = ("ext:" + raw if scope == "erp-sub" and pat is pattern2
                                  else resolve(node, raw, under_business=(pat is pattern)))
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


def report(scope: str, domain: str | None, as_json: bool, layer_check: bool = False) -> int:
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
        # 层次判据（P2-C 重切后新增）：抽 crate 的真实可行性看**出边**。
        # 入边（装配层 → 本域）是合法方向（bin 必须依赖域 crate 才能链接路由），
        # 出边（本域 → 还留在单体的代码）才会让包图成环。两者都要报。
        outside = [(t, c) for t, c, _a in out_rows if t not in ASSUMED_KERNEL]
        verdict = "可抽" if code_in == 0 else f"不可抽（{code_in} 处代码级回边，{len(in_rows)} 个来源）"
        if as_json:
            print(json.dumps({
                "scope": scope, "domain": domain, "loc": loc[domain], "files": nfile[domain],
                "scc_size": scc_size.get(domain), "scc": comps[comp_of[domain]],
                "inbound": [{"from": s, "code": c, "all": a, "where": w} for s, c, a, w in in_rows],
                "outbound": [{"to": t, "code": c, "all": a} for t, c, a in out_rows],
                "layer_cut": [{"to": t, "code": c} for t, c in outside],
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
            if not outside:
                print(f"  层次判据：可抽（出边全部落在更下层内核 {', '.join(ASSUMED_KERNEL)}）")
            else:
                print(f"  层次判据：本步要切 {len(outside)} 条 / "
                      f"{sum(c for _t, c in outside)} 行 —— 出边指向更下层以外："
                      f"{', '.join(f'{t}({c})' for t, c in outside)}")
                print(f"            （假设 L1 内核 {', '.join(ASSUMED_KERNEL)} 已抽出、"
                      f"装配层 bin 已独立成包；完整卡序见 --plan-order）")
            print(f"  结论：{verdict}")
        if layer_check:
            return 0 if not outside else 1
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


def parse_plan(path: str) -> list[dict]:
    """从 markdown 的 ```p2c-order 代码块读卡序。

    每行（`|` 分隔，`#` 开头是注释）：

        卡号 | crate 名 | 节点 | 承诺解环账(行) | 类型(lib|bin，可省)

    节点列的写法区分两件事：
    - `,` 分隔 ⇒ 同一张卡里的**多个独立包**（卡内按列出的先后抽，单向引用没问题）；
    - `+` 连接 ⇒ 合进**同一个包**（包内互相引用不算环）。
    类型 = `bin` ⇒ 装配层：它可以依赖任何包（bin 必须链接域 crate，否则路由
    `inventory` 项进不了链接单元 ⇒ 静默 404），所以**不产生解环账**。

    「承诺解环账」= 这张卡开工前必须切断的跨域出边行数（改成 port / 注册表 /
    词表上提），也就是这张卡的**前置欠账**。工具会按真实边重算，对不上就红。
    """
    try:
        text = open(path, encoding="utf-8").read()
    except OSError as exc:
        raise SystemExit(f"读不到卡序文件：{path}（{exc}）")
    m = re.search(r"```p2c-order\n(.*?)```", text, re.S)
    if not m:
        raise SystemExit(f"{path} 里没有 ```p2c-order 代码块")
    steps = []
    for lineno, line in enumerate(m.group(1).splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        cells = [c.strip() for c in line.strip("|").split("|")]
        if len(cells) not in (4, 5):
            raise SystemExit(f"卡序第 {lineno} 行应为 4~5 列（卡号|crate|节点|承诺行数[|lib/bin]）：{line!r}")
        card, crate, node_list, declared = cells[:4]
        kind = (cells[4] if len(cells) == 5 else "lib").lower()
        if kind not in ("lib", "bin"):
            raise SystemExit(f"卡序第 {lineno} 行类型只能是 lib/bin：{kind!r}")
        groups = [[n.strip() for n in g.split("+") if n.strip()]
                  for g in node_list.split(",") if g.strip()]
        steps.append({"card": card, "crate": crate,
                      "group_names": [" + ".join(g) for g in groups],
                      "nodes": [n for g in groups for n in g],
                      "declared": int(declared), "kind": kind})
    return steps


def plan_order(path: str) -> int:
    """按卡序模拟抽包，验证每一步都成立（P2-C 排期的一致性检查）。

    规则（Cargo 包图必须无环的推论）：第 i 步把节点集合 S 抽成包，要求 S 的出边都
    指向「更下层」——即 {ASSUMED_KERNEL ∪ 前面各步已抽出的节点 ∪ S 自身（同包）}。
    指向后面步骤（或根本没进卡序）的出边，必须在这一步之前切断，计入「本步要切」。
    装配层（bin）节点例外：它朝任何方向都可以。
    """
    nodes, edges, code, where, loc, nfile = build_graph("top")
    steps = parse_plan(path)

    print(f"卡序文件：{path} · {len(steps)} 步 · 图：{len(nodes)} 节点")
    print(f"更下层基准（L1 内核）：{', '.join(ASSUMED_KERNEL)}")
    print(f"\n{'卡号':6s} {'crate':38s} {'本步要切':>8s} {'承诺':>5s} {'判定':>4s}  说明")

    extracted = set(ASSUMED_KERNEL) & set(nodes)
    seen: list[str] = []
    failures: list[str] = []
    total_cut = 0
    for step in steps:
        unknown = [n for n in step["nodes"] if n not in nodes]
        if unknown:
            failures.append(f"{step['card']}：节点名在图里不存在 {unknown}（可用：{', '.join(sorted(nodes))}）")
            print(f"{step['card']:6s} {step['crate']:38s} {'?':>8s} {step['declared']:5d} {'红':>4s}  "
                  f"未知节点 {unknown}")
            continue
        dst = set(step["nodes"])
        cut: dict[str, int] = {}
        if step["kind"] == "lib":
            for n in step["nodes"]:
                for t, c in code[n].items():
                    if t in dst or t in extracted or t == n:
                        continue
                    cut[t] = cut.get(t, 0) + c
        # 卡内跨包（不同 group）单向引用没问题（抽第二个时第一个已是包）；
        # 互相引用就必须拆成两步、或并成同一个包。
        group_of = {n: i for i, g in enumerate(step["group_names"]) for n in g.split(" + ")}
        intra = defaultdict(int)
        intra_cycle = []
        for n in step["nodes"]:
            for t in step["nodes"]:
                if n == t or t not in code[n]:
                    continue
                if group_of[n] == group_of[t]:
                    continue
                intra[(n, t)] += code[n][t]
        intra_cycle = sorted({f"{n}↔{t}" for (n, t) in intra if (t, n) in intra})
        actual = sum(cut.values())
        total_cut += actual
        status, note = "OK", ""
        if intra_cycle:
            status = "红"
            note = f"同卡内跨包互相引用（{', '.join(intra_cycle)}）⇒ 拆成两步，或用 `+` 并进同一个包"
            failures.append(f"{step['card']}：{note}")
        if actual > step["declared"]:
            status = "红"
            note = (f"实际要切 {actual} 行 > 承诺 {step['declared']}："
                    f"{', '.join(f'{t}({c})' for t, c in sorted(cut.items(), key=lambda kv: -kv[1]))}")
            failures.append(f"{step['card']}：{note}")
        elif actual < step["declared"] and status == "OK":
            note = f"实际只 {actual} 行 < 承诺 {step['declared']}（可下调承诺值）"
        if step["kind"] == "bin":
            note = (note + " " if note else "") + "装配层：出边不算账"
        print(f"{step['card']:6s} {step['crate']:38s} {actual:8d} {step['declared']:5d} {status:>4s}  {note}")
        if cut:
            print(f"{'':6s} {'':38s} 切向："
                  f"{', '.join(f'{t}({c})' for t, c in sorted(cut.items(), key=lambda kv: -kv[1]))}")
        if intra:
            print(f"{'':6s} {'':38s} 卡内跨包先后："
                  f"{', '.join(f'{n}→{t}({c})' for (n, t), c in sorted(intra.items()))}")
        extracted |= dst
        seen.extend(step["nodes"])

    planned = set(seen) | (set(ASSUMED_KERNEL) & set(nodes))
    leftover = sorted(n for n in nodes if n not in planned)
    print(f"\n全序累计要切：{total_cut} 行")
    print(f"卡序覆盖 {len(planned)} 个节点；未进卡序（留在单体）{len(leftover)} 个："
          f"{', '.join(leftover) if leftover else '（无）'}")
    if leftover:
        print("  ↑ 这些节点留在单体不会让卡序失效，但**任何已抽出的包都不许回指它们**——"
              "上面的「本步要切」已经把它们算进去了。")

    if failures:
        print(f"\n判定：红（{len(failures)} 处）")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("\n判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="抽 crate 就绪度体检（只读）")
    ap.add_argument("--domain", help="看单个域：顶层域（erp/flow/hr/…）或 erp 子域（cost/purchase/…）")
    ap.add_argument("--scope", choices=("top", "erp-sub"),
                    default=None, help="top=顶层域（默认）；erp-sub=business/erp 下的一级子域")
    ap.add_argument("--json", action="store_true", help="机器可读输出")
    ap.add_argument("--check", action="store_true",
                    help="【旧口径】域可抽→退 0；有回边→退 1（只看入边，方向反了，见 --layer-check）")
    ap.add_argument("--layer-check", action="store_true",
                    help="【层次口径】出边全落在 L1 内核→退 0；否则退 1，并列出本步要切的出边")
    ap.add_argument("--plan-order", metavar="MD",
                    help="按 docs 里的 ```p2c-order 卡序模拟抽包，每一步都对得上退出码 0")
    args = ap.parse_args()
    if args.plan_order:
        return plan_order(args.plan_order)
    scope = args.scope or ("erp-sub" if args.domain in _erp_subdirs() else "top")
    if args.layer_check and not args.domain:
        print("--layer-check 需要配 --domain <域>", file=sys.stderr)
        return 2
    rc = report(scope, args.domain, args.json, layer_check=args.layer_check)
    return rc if (args.check or args.layer_check) else 0


def _erp_subdirs() -> set[str]:
    if not os.path.isdir(ERP):
        return set()
    return {n for n in os.listdir(ERP) if os.path.isdir(os.path.join(ERP, n))}


if __name__ == "__main__":
    sys.exit(main())
