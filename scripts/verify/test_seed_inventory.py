#!/usr/bin/env python3
"""测试代码主数据 INSERT 盘点（P2-62）—— `test_seed_gate.sh` 预算数字的来源。

为什么要有它：门禁 ③ 里 12 张表各有一个「主数据 INSERT 上限」，那些数字不是拍脑袋写的，
是**这张脚本量出来的**。下一批用例迁到 `crate::test_seed::ensure_*` 之后：

    1. 跑本脚本，看新的数字；
    2. 把 `scripts/verify/test_seed_gate.sh` 里 `BUDGET` 的对应值**下调**；
    3. 提交（下调 = 又收编了一批；上调 = 回潮，评审直接打回）。

用法：
    python3 scripts/verify/test_seed_inventory.py            # 统计当前树
    python3 scripts/verify/test_seed_inventory.py --root <src>  # 统计别的树（A/B 对比用）

口径（与门禁一致）：只统计**测试文件**（`*_test.rs` / `*_tests.rs` / `*_support.rs` /
`tests/*.rs`）里的 `INSERT INTO <表>`，生产代码不计。
"""
from __future__ import annotations

import argparse
import re
from pathlib import Path

DEFAULT_ROOT = Path(__file__).resolve().parents[2] / "backend/app/src"
TEST_NAME = re.compile(r"(_tests?\.rs|_support\.rs|tests/.*\.rs)$")
TABLES = [
    "sys_dept", "sys_user", "sys_user_role", "sys_role",
    "biz_unit", "biz_material", "biz_warehouse", "biz_account",
    "biz_supplier", "biz_customer", "biz_accounting_period", "biz_location",
]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    args = ap.parse_args()
    if not args.root.is_dir():
        print(f"源码目录不存在：{args.root}")
        return 2

    counts = dict.fromkeys(TABLES, 0)
    insert_total = 0
    uniq_defs = 0
    for path in sorted(args.root.rglob("*.rs")):
        rel = path.relative_to(args.root).as_posix()
        text = path.read_text(encoding="utf-8", errors="ignore")
        uniq_defs += len(re.findall(r"\bfn uniq\(", text))
        if not TEST_NAME.search(rel):
            continue
        insert_total += len(re.findall(r"INSERT INTO [a-z_]+", text))
        for table in TABLES:
            counts[table] += len(re.findall(rf"INSERT INTO {table}\b", text))

    print(f"根目录               : {args.root}")
    print(f"测试文件 INSERT 总数 : {insert_total}")
    print(f"fn uniq( 定义数      : {uniq_defs}")
    for table in TABLES:
        print(f"  {table:26s} {counts[table]:3d}")
    print(f"  {'主数据合计':24s} {sum(counts.values()):3d}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
