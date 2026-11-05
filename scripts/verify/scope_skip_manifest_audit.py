#!/usr/bin/env python3
"""闸② 跳过清单（`SKIP_MANIFEST`）的**逐条核对**审计工具。

为什么需要它：`scripts/lint/scope_gate.py` 的跳过清单是硬断言 —— 实测跳过集合与
`SKIP_MANIFEST` 多一条少一条都 FAIL，但报错只说「某文件被判成了测试面、清单里没登记」，
**没有给出「它凭什么是测试面」的证据**。补登记前必须逐条裁定，否则就是把生产文件
藏进测试面的那个洞自己动手挖开（口径 v2 硬化 R1）。

本工具对每个「未登记但被跳过」的文件给出四条可复核证据：

1. 声明位置上下文（必须是 `#[cfg(test)]` 紧跟 `mod` 声明）；
2. `#[test]` / `#[tokio::test]` 的条数（0 条时看第 4 条）；
3. `pub fn` 条数（>0 说明可能有生产 API，要人工看）；
4. 哪些文件引用了它（只应被测试面文件或它自己的父模块引用）。

用法：

    python3 scripts/verify/scope_skip_manifest_audit.py                # 审计当前清单 vs 实测
    python3 scripts/verify/scope_skip_manifest_audit.py --gate <旧版 scope_gate.py>
    python3 scripts/verify/scope_skip_manifest_audit.py --list          # 只列未登记条目

退出码：0 = 无未登记条目（清单与实测一致）；1 = 有未登记 / 已失效条目。
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--root", default=".", help="仓库根（含 backend/src 与 scripts/lint/scope_gate.py）")
    ap.add_argument("--gate", default=None, help="要审计的 scope_gate.py（默认用仓库内那份）")
    ap.add_argument("--list", action="store_true", help="只列文件清单，不打印逐条证据")
    args = ap.parse_args()

    root = pathlib.Path(args.root).resolve()
    src = root / "backend" / "src"
    gate = pathlib.Path(args.gate).resolve() if args.gate else root / "scripts" / "lint" / "scope_gate.py"
    if not gate.is_file():
        print(f"找不到门禁脚本：{gate}", file=sys.stderr)
        return 2

    run = subprocess.run(
        [sys.executable, str(gate), "--root", str(root)],
        capture_output=True,
        text=True,
    )
    out = run.stdout + run.stderr
    pairs = re.findall(
        r"跳过清单断言：backend/src/(\S+) 被判成了测试面，但 SKIP_MANIFEST 里没有登记（声明位置 ([^）]+)）",
        out,
    )
    stale = re.findall(r"跳过清单断言：SKIP_MANIFEST 登记了 backend/src/(\S+)，但本次解析没有跳过它", out)
    text = gate.read_text()
    m = re.search(r'SKIP_MANIFEST = frozenset\(\n    """\n(.*?)\n"""\.split\(\)', text, re.S)
    registered = m.group(1).split() if m else []

    print(f"门禁脚本：{gate}")
    print(f"登记 {len(registered)} 条；未登记但被跳过 {len(pairs)} 条；登记了但未跳过 {len(stale)} 条")
    if args.list:
        for rel, decl in sorted(pairs):
            print(f"  + backend/src/{rel}   ← {decl}")
        for rel in sorted(stale):
            print(f"  - backend/src/{rel}   （登记失效：文件被删/改名，或声明被去掉）")
        return 1 if (pairs or stale) else 0

    print()
    for rel, decl in sorted(pairs):
        p = src / rel
        body = p.read_text(errors="ignore")
        ntest = len(re.findall(r"#\[(?:tokio::)?test\]", body))
        pub_fn = re.findall(r"^\s*pub (?:async )?fn (\w+)", body, re.M)
        decl_path, line = decl.rsplit(":", 1)
        dl = pathlib.Path(decl_path).read_text(errors="ignore").splitlines()
        ln = int(line)
        ctx = " ⏎ ".join(x.strip() for x in dl[max(0, ln - 3): ln + 1])
        cfg_ok = "#[cfg(test)]" in ctx or "#[cfg(all(test" in ctx
        stem = pathlib.Path(rel).stem
        users = []
        for other in src.rglob("*.rs"):
            if other == p:
                continue
            if re.search(rf"\b{re.escape(stem)}\b", other.read_text(errors="ignore")):
                users.append(other.relative_to(src).as_posix())
        print(f"backend/src/{rel}")
        print(f"  声明 {decl_path}:{line}   cfg(test)={cfg_ok}")
        print(f"  tests={ntest}  pub_fn={len(pub_fn)}  被引用={users[:6]}{' …' if len(users) > 6 else ''}")
        print(f"  上下文: {ctx[:160]}")
        print()
    for rel in sorted(stale):
        print(f"登记失效：business/{rel}（本次解析没有被跳过 —— 请从 SKIP_MANIFEST 删掉这一行）")
    return 1 if (pairs or stale) else 0


if __name__ == "__main__":
    sys.exit(main())
