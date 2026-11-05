#!/usr/bin/env python3
"""根据 plan 与各条目正文，算出每个部件需要 `use super::<别的部件>::*;` 的清单。

用法：python3 gen_part_uses.py <plan.json> <handler.rs> [--apply]
不带 --apply 只打印，带 --apply 直接改写 plan 里的 extra_uses。
"""
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, "/opt/cloudpivot/scripts/refactor")
import split_module as sm

plan_path = Path(sys.argv[1])
src_path = Path(sys.argv[2])
apply = "--apply" in sys.argv

plan = json.loads(plan_path.read_text(encoding="utf-8"))
docs, imports, items, trailer = sm.split_file(src_path)
by_name = {i.name: i for i in items}

part_of = {}
for p in plan["parts"]:
    for nm in p["items"]:
        assert nm in by_name, "plan 里的项不存在：%s" % nm
        part_of[nm] = p["name"]

base = plan["parts"][0]["name"]
ident = re.compile(r"\b[A-Za-z_][A-Za-z0-9_]*\b")

result = {}
for p in plan["parts"]:
    body = "".join(by_name[nm].text for nm in p["items"])
    # 去掉注释行，避免注释里的名字制造假依赖
    code = "\n".join(l for l in body.splitlines() if not l.strip().startswith("//"))
    names = set(ident.findall(code))
    need = set()
    for other, owner in part_of.items():
        if owner == p["name"]:
            continue
        if other in names:
            need.add(owner)
    need.discard(p["name"])
    result[p["name"]] = sorted(need)

for name, need in result.items():
    print(name, "->", need)

if apply:
    for p in plan["parts"]:
        need = [x for x in result[p["name"]] if x != base]
        p["extra_uses"] = ["use super::%s::*;\n" % x for x in need]
    plan_path.write_text(json.dumps(plan, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print("已写回", plan_path)
