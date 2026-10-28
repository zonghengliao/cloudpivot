#!/usr/bin/env python3
"""对比两棵树的「色值字面量集合」—— 用于重放产物 vs 线上产线的一致性检查。

用法: compare_colors.py <A 根> <B 根> [--files ...]
比较范围与 03-brand/replace-colors.py 一致（frontend/src, frontend/index.html）。
输出：每个文件色值多重集合的差异；末尾给全局差异。
"""
import os
import re
import sys
from collections import Counter

HEX_RE = re.compile(r"#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{3,4})\b")
RGB_RE = re.compile(
    r"rgba?\(\s*(\d{1,3})\s*[,\s]\s*(\d{1,3})\s*[,\s]\s*(\d{1,3})\s*(?:[,/]\s*([0-9.]+%?)\s*)?\)",
    re.I,
)
PATHS = ["frontend/src", "frontend/index.html"]
EXCLUDE = {"node_modules", "dist", ".git", "target", ".brand-backup", ".rename-backup", ".worktrees"}
EXT = {".vue", ".ts", ".js", ".less", ".css", ".html", ".scss", ".svg"}


def collect(root, rel):
    p = os.path.join(root, rel)
    out = Counter()
    files = []
    if os.path.isfile(p):
        files = [(rel, p)]
    else:
        for dp, dn, fn in os.walk(p):
            dn[:] = [d for d in dn if d not in EXCLUDE]
            for f in fn:
                if os.path.splitext(f)[1].lower() in EXT:
                    full = os.path.join(dp, f)
                    files.append((os.path.relpath(full, root), full))
    for r, full in files:
        raw = open(full, "rb").read()
        if b"\x00" in raw:
            continue
        t = raw.decode("utf-8", "replace")
        c = Counter()
        for m in HEX_RE.finditer(t):
            v = m.group(0)[1:].lower()
            c["#" + v] += 1
        for m in RGB_RE.finditer(t):
            r_, g_, b_ = (int(m.group(i)) for i in (1, 2, 3))
            if max(r_, g_, b_) <= 255:
                c["rgb(%02x%02x%02x/%s)" % (r_, g_, b_, m.group(4) or "")] += 1
        if c:
            out[r] = c
    return out


def main():
    a, b = sys.argv[1], sys.argv[2]
    ca, cb = Counter(), Counter()
    for rel in PATHS:
        for k, v in collect(a, rel).items():
            ca[k] = sum(v.values())
        for k, v in collect(b, rel).items():
            cb[k] = sum(v.values())
    only_a = {k: ca[k] - cb.get(k, 0) for k in ca if ca[k] > cb.get(k, 0)}
    only_b = {k: cb[k] - ca.get(k, 0) for k in cb if cb[k] > ca.get(k, 0)}
    print("A=%s  B=%s" % (a, b))
    print("A 色值出现次数: %d  B: %d" % (sum(ca.values()), sum(cb.values())))
    if not only_a and not only_b:
        print("\n✓ 两棵树的色值字面量多重集合完全一致")
        return 0
    if only_a:
        print("\n只在 A 有（A 多出来的）:")
        for k, v in sorted(only_a.items(), key=lambda x: -x[1])[:40]:
            print("  %-22s ×%d" % (k, v))
    if only_b:
        print("\n只在 B 有（B 多出来的）:")
        for k, v in sorted(only_b.items(), key=lambda x: -x[1])[:40]:
            print("  %-22s ×%d" % (k, v))
    return 1


if __name__ == "__main__":
    sys.exit(main())
