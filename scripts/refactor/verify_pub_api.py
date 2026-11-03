#!/usr/bin/env python3
"""核对「原有 pub 接口一个没变」：拆分前每个 `pub` 顶层项，拆分后必须仍是 `pub`（不是 pub(super)）。"""
import re
import subprocess
import sys
from pathlib import Path

REPO = Path("/opt/cloudpivot")
REF = sys.argv[1] if len(sys.argv) > 1 else "52f1ccf"

PUB_DECL = re.compile(
    r"^pub\s+(?:(?:async|const|unsafe|extern)\s+)*(fn|struct|enum|union|trait|type|const|static)\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)"
)


def pub_names(text: str):
    out = set()
    for line in text.splitlines():
        m = PUB_DECL.match(line)
        if m:
            out.add(m.group(2))
    return out


targets = [
    ("backend/src/business/task/model.rs",
     [f"backend/src/business/task/task_model_{n}.rs" for n in [
         "base", "rows", "query", "write", "hierarchy", "dep", "ai", "stats", "flow", "schedule"]]),
    ("backend/src/business/erp/stockin_handler.rs",
     [f"backend/src/business/erp/stockin_handler_{n}.rs" for n in [
         "common", "purchase", "write", "post", "cancel", "status", "query"]]),
]

ok = True
for src, parts in targets:
    before = subprocess.run(["git", "show", f"{REF}:{src}"], cwd=REPO,
                            capture_output=True, text=True, check=True).stdout
    after = "\n".join((REPO / p).read_text(encoding="utf-8") for p in parts)
    want = pub_names(before)
    got = pub_names(after)
    missing = sorted(want - got)
    added = sorted(got - want)
    print("=" * 70)
    print(f"{src}   拆分前 pub 项 {len(want)} 个 → 拆分后 pub 项 {len(got)} 个")
    print("  丢失（降级成 pub(super) 或消失）:", missing or "none")
    print("  新增:", added or "none")
    if missing:
        ok = False
print("=" * 70)
print("结论:", "PASS（原有 pub 接口一个没变）" if ok else "FAIL")
raise SystemExit(0 if ok else 1)
