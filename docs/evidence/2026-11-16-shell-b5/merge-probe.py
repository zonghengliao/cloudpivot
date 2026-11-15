#!/usr/bin/env python3
"""把「单路由补采」的探针 JSON 合并进主 JSON（删掉旧 key、写入新 key）。

用法： python3 merge-probe.py main.json patch.json [old-key new-key ...]
"""
import json
import sys

main_path, patch_path = sys.argv[1], sys.argv[2]
main = json.load(open(main_path))
patch = json.load(open(patch_path))
# 补采时用的是修好的路由；主 JSON 里可能留着 404 的旧 key（如 /business/flow/def）
for old in ['/business/flow/def?view=mine', '/business/flow/def']:
    main['routes'].pop(old, None)
for k, v in patch['routes'].items():
    main['routes'][k] = v
json.dump(main, open(main_path, 'w'), ensure_ascii=False, indent=1)
print(f'{main_path}: 现有 {len(main["routes"])} 条路由；合并入 {list(patch["routes"])}')
