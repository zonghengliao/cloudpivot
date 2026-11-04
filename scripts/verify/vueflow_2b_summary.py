#!/usr/bin/env python3
"""从 before/after 证据里摘出「句柄命中半径」与「落点容差」两行结论。"""
import json

for tag in ("before", "after"):
    path = f"docs/evidence/20261104-vueflow-2b/drop-tolerance-{tag}.json"
    data = json.load(open(path))
    sweep = [r for r in data if r["section"] == "hit-sweep"][0]["sweep"]
    hit = [int(k) for k, v in sweep.items() if v and "handle" in v]
    drags = [r for r in data if r["section"] in ("drag", "drag-native")]
    print(f"=== {tag} ===")
    print(f"句柄命中区（从中心向右侧能点到 handle 的最远屏幕 px）：{max(hit)}")
    for d in drags:
        print(f"  {d['label']}: 成边={d['created']} 边 {d['edgesBefore']}→{d['edgesAfter']}")
