#!/usr/bin/env python3
"""独立复核：迁移版本号跨目录按【数值】全局唯一（不复用仓库内的 lint 工具）。"""
import glob
import os
import re
import collections
import sys

ROOT = sys.argv[1] if len(sys.argv) > 1 else "/opt/cloudpivot"
pat = re.compile(r"^V(\d+)__(.+)\.(up|down)\.sql$")

vers = collections.defaultdict(list)
up_names = collections.defaultdict(list)
bad_names = []
files = sorted(glob.glob(os.path.join(ROOT, "backend/migrations/*/V*.sql")))
for f in files:
    b = os.path.basename(f)
    rel = os.path.relpath(f, ROOT)
    m = pat.match(b)
    if not m:
        bad_names.append(rel)
        continue
    v, n, k = m.groups()
    vers[int(v)].append((k, rel, n))
    if k == "up":
        up_names[n].append(int(v))

up_dup = {v: [r for k, r, _ in fs if k == "up"] for v, fs in vers.items()
          if len([1 for k, _, _ in fs if k == "up"]) > 1}
multi = {v: [(k, r) for k, r, _ in fs] for v, fs in vers.items() if len(fs) > 2}
name_reuse = {n: vs for n, vs in up_names.items() if len(vs) > 1}
missing_pair = []
for v, fs in vers.items():
    kinds = {k for k, _, _ in fs}
    if kinds != {"up", "down"}:
        missing_pair.append((v, sorted(kinds)))

print(f"files={len(files)} versions={len(vers)} max=V{max(vers):04d}")
print(f"duplicate-version-with-two-up (FATAL形态) = {dict(sorted(up_dup.items()))}")
print(f"versions with extra files (>2)          = {multi}")
print(f"up-name reused across versions          = {name_reuse}")
print(f"unpaired up/down                        = {missing_pair}")
print(f"bad file names                          = {bad_names}")

# 关键断言：V0397 只剩 business 一条 up；V0399 属于 system 配色
for v in (397, 398, 399):
    print(f"V{v:04d} -> {[r for _, r, _ in vers.get(v, [])]}")

ok = not (up_dup or multi or name_reuse or missing_pair or bad_names)
print("RESULT:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
