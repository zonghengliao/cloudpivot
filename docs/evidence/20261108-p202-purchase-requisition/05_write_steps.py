#!/usr/bin/env python3
"""把 00_api_responses.json 的 steps 落成 01_api_steps.txt（证据可读版）。"""
import json
import os

OUT = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(OUT, "00_api_responses.json"), encoding="utf-8") as handle:
    data = json.load(handle)
lines = ["P2-02 采购申请闭环 · API 全链实测（隔离实例 18105 / 库 cloudpivot_prq03）", ""]
lines.extend(data["steps"])
lines.append("")
lines.append(f"失败项：{len(data['fails'])}")
for item in data["fails"]:
    lines.append(f"  - {item}")
with open(os.path.join(OUT, "01_api_steps.txt"), "w", encoding="utf-8") as handle:
    handle.write("\n".join(lines) + "\n")
print("\n".join(lines[:12]))
print("...")
print(f"steps 行数 {len(lines)}，失败 {len(data['fails'])}")
