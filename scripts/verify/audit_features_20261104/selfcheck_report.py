#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""报告自检：围栏配对 / 表格列数一致 / 关键数字一致性 / 引用路径存在性。"""
import re, sys, os, collections

p = sys.argv[1] if len(sys.argv) > 1 else "/opt/cloudpivot/docs/评审报告-现有功能评分-20261104.md"
root = "/opt/cloudpivot"
lines = open(p, encoding="utf-8").read().split("\n")
print(f"行数={len(lines)}")

# 1) 围栏
fence = 0
bad_fence = []
for i, ln in enumerate(lines, 1):
    if ln.strip().startswith("```"):
        fence += 1
print("围栏行数:", fence, "（应为偶数）", "OK" if fence % 2 == 0 else "!! 不配对")

# 2) 表格列数一致性
groups, cur = [], []
for i, ln in enumerate(lines, 1):
    if ln.strip().startswith("|"):
        cur.append((i, ln))
    else:
        if cur:
            groups.append(cur); cur = []
if cur: groups.append(cur)
bad_tables = 0
for g in groups:
    counts = collections.Counter(ln.count("|") for _, ln in g)
    if len(counts) > 1:
        bad_tables += 1
        print(f"!! 表格列数不一致：起始行 {g[0][0]}，列数分布 {dict(counts)}")
        for i, ln in g[:3]:
            print("    ", i, ln[:110])
print(f"表格数={len(groups)}，列数不一致的表={bad_tables}")

# 3) 关键数字
for kw in ["82 个 ≤3 分", "56/56", "40/40", "208 个分数", "29.77", "≥800"]:
    n = sum(1 for ln in lines if kw in ln)
    print(f"关键词 {kw!r} 出现 {n} 次")

# 4) 引用路径
refs = set(re.findall(r"`(scripts/verify/[^`]+|docs/[^`]+|backend/[^`]+|frontend/[^`]+)`", "\n".join(lines)))
missing = []
for r in sorted(refs):
    if any(c in r for c in "*{}<>") or r.endswith("/"):
        continue
    base = r.split(":")[0]  # 去掉 `:255-263` 行号后缀
    if not os.path.exists(os.path.join(root, base)):
        missing.append(r)
print(f"引用路径 {len(refs)} 条，缺失 {len(missing)}：")
for m in missing[:12]:
    print("  缺:", m)
