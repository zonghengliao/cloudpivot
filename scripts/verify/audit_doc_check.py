"""Markdown 交付前校验：围栏配对 / 表格列数一致 / 章节引用有效 / 行数。"""
import re, sys, os

path = sys.argv[1] if len(sys.argv) > 1 else '/opt/cloudpivot/docs/architecture/ARCH-3-模块勾稽关系审计.md'
lines = open(path, encoding='utf-8').read().splitlines()
print("总行数:", len(lines))

# 1) 围栏
fences = []
for i, l in enumerate(lines, 1):
    if l.lstrip().startswith("```"):
        fences.append(i)
print("围栏行数:", len(fences), "配对:", "OK" if len(fences) % 2 == 0 else "!! 奇数，未闭合")

# 2) 表格列数
groups, cur = [], []
def flush():
    if len(cur) >= 2:
        groups.append(list(cur))
    cur.clear()
for i, l in enumerate(lines, 1):
    s = l.strip()
    if s.startswith("|") and s.endswith("|"):
        cur.append((i, s.count("|")))
    else:
        flush()
flush()
bad = 0
for g in groups:
    counts = {c for _, c in g}
    if len(counts) > 1:
        bad += 1
        print(f"!! 表格列数不一致：起始行 {g[0][0]}，各行列数 {[c for _, c in g]}")
print(f"表格组数: {len(groups)}，列数不一致的表: {bad}")

# 3) 章节引用
heads = set()
for l in lines:
    m = re.match(r"^(#{2,4})\s+(\d+(?:\.\d+)*)", l)
    if m:
        heads.add(m.group(2))
refs = set(re.findall(r"§(\d+(?:\.\d+)*)", "\n".join(lines)))
missing = sorted(r for r in refs if r not in heads and not re.match(r"^\d+$", r) or (r not in heads))
missing = sorted(r for r in refs if r not in heads)
print("引用章节数:", len(refs), "缺失:", missing if missing else "无")

# 4) DC 编号连续性与统计
dcs = re.findall(r"DC-(\d\d)", "\n".join(lines))
uniq = sorted({int(d) for d in dcs})
print("DC 编号:", f"{uniq[0]}..{uniq[-1]}", "共", len(uniq), "个", "连续" if uniq == list(range(uniq[0], uniq[-1] + 1)) else "不连续!")
