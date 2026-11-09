#!/usr/bin/env python3
"""清掉用户数据范围缓存（演练夹具用：角色/范围变更后首次解析可能命中旧值）。"""
import subprocess

keys = subprocess.run(["redis-cli", "--scan", "--pattern", "cloudpivot:perm:user_scope:[0-9]*:*"],
                      capture_output=True, text=True).stdout.split()
for key in keys:
    subprocess.run(["redis-cli", "del", key], capture_output=True)
print(f"flushed {len(keys)} scope keys")
