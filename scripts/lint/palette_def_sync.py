#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""门禁：偏好字典 `ui.palette` 的合法取值必须与前端色板**逐字一致**。

为什么需要它
------------
后端的 `sys_preference_def.validator` 是 `oneof:<22 个色板 id>`，它是「能不能保存」的
唯一权威；前端的 `style/palettes.ts` 是「能不能选」的权威。两者一旦漂移：

* 前端加了新色板、后端没加 → 用户选了新配色后保存被 400 拒（界面能选、库里存不下）；
* 后端多留了已删色板 → 库里存着前端不认识的值（加载时被 `isPaletteId` 打回默认，
  表现为「一刷新配色就变回去」）。

两种都是静默故障，所以放进 `scripts/check.sh` 当门禁。

用法：python3 scripts/lint/palette_def_sync.py --root /opt/cloudpivot
退出码：0 一致 / 1 不一致或文件读不到
"""

import argparse
import io
import re
import sys

PALETTE_RE = re.compile(r"^\s{4}id:\s*'([a-z][a-z0-9-]*)',\s*$", re.MULTILINE)
ONEOF_RE = re.compile(r"'oneof:([^']+)'")


def read(path):
    with io.open(path, encoding="utf-8") as handle:
        return handle.read()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=".")
    args = parser.parse_args()

    palettes_ts = f"{args.root}/frontend/src/style/palettes.ts"
    seed_sql = f"{args.root}/backend/migrations/system/V0091__seed_preference_def.up.sql"

    try:
        ts_text = read(palettes_ts)
        sql_text = read(seed_sql)
    except FileNotFoundError as error:
        print(f"  FAIL：找不到文件 {error.filename}")
        return 1

    palette_ids = PALETTE_RE.findall(ts_text)
    if not palette_ids:
        print(f"  FAIL：{palettes_ts} 里没有解析到任何色板 id（正则失效？）")
        return 1

    match = ONEOF_RE.search(sql_text)
    if not match:
        print(f"  FAIL：{seed_sql} 里没找到 'oneof:...' 校验规则")
        return 1
    allowed = [item.strip() for item in match.group(1).split(",") if item.strip()]

    missing = [item for item in palette_ids if item not in allowed]
    extra = [item for item in allowed if item not in palette_ids]
    duplicated = len(allowed) != len(set(allowed))

    if missing or extra or duplicated:
        print(f"  FAIL：色板 {len(palette_ids)} 套 / 字典 oneof {len(allowed)} 项")
        if missing:
            print(f"    字典缺少（界面能选、后端会拒绝保存）：{', '.join(missing)}")
        if extra:
            print(f"    字典多余（库里可能存着前端不认识的值）：{', '.join(extra)}")
        if duplicated:
            print("    字典 oneof 里有重复项")
        return 1

    print(f"  PASS：{len(palette_ids)} 套色板与 V0091 的 ui.palette oneof 逐字一致")
    return 0


if __name__ == "__main__":
    sys.exit(main())
