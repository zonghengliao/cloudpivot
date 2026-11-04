#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""库存三本账「唯一写入口」闸（P2-5A）。

判据：`backend/src` 下**非测试**代码里，除了
`backend/src/business/erp/inventory_writer.rs` 之外，任何文件都不许出现对
`biz_inventory` / `biz_inventory_location` / `biz_inventory_txn` 的写语句
（`INSERT INTO …` / `UPDATE …` / `DELETE FROM …`）。

为什么要有这道闸：收口前 `biz_inventory` 有 13 个文件直写、`biz_inventory_location`
13 个、`biz_inventory_txn` 8 个 —— 同一件事在 N 个文件里各写一遍 SQL，
任何一处的 `ROUND` / 夹 0 / 归零口径漂移都不会立刻报错，只在巡检里变成「账不对」。
唯一写入口 + 静态闸，才能保证「口径只有一份」。

豁免（都是**故意的**，不是漏网）：
  * 文件名含 `test` 的文件（`*_tests.rs` / `tests.rs` / `*_test_support.rs`）：
    夹具必须能**直写脏数据**来造「台账被人手工改过」这类场景（例如
    `UPDATE biz_inventory SET qty = 999` 去验巡检能抓到），走写入口就造不出来了；
  * 文件内部 `#[cfg(test)]` 之后的代码：同上，测试夹具；
  * 以 `//` / `///` / `//!` 开头的注释行：文档里可以提到 SQL（会被误判）。

用法：
    python3 scripts/verify/inventory_write_gate.py          # 明细 + 退出码
    python3 scripts/verify/inventory_write_gate.py --json   # 给 CI 解析
退出码：0 = 干净；1 = 有越界写入。
"""
import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "backend" / "src"
WRITER = SRC / "business" / "erp" / "inventory_writer.rs"

# 三张表（`biz_inventory` 必须放最后，正则用 \b 收尾即可覆盖 `biz_inventory AS i`）
WRITE_RE = re.compile(
    r"(?i)\b(?:INSERT\s+INTO|UPDATE|DELETE\s+FROM)\s+(biz_inventory(?:_location|_txn)?)\b"
)
CFG_TEST_RE = re.compile(r"^\s*#\[cfg\(test\)\]")


def is_comment(line: str) -> bool:
    stripped = line.lstrip()
    return stripped.startswith("//")


def scan_file(path: Path):
    """返回 [(行号, 表名, 代码行)]，只报**非测试代码**里的写语句。"""
    hits = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if CFG_TEST_RE.match(line):
            # 同文件内 `#[cfg(test)]` 之后全是测试夹具 —— 整个文件余下部分豁免
            break
        if is_comment(line):
            continue
        for match in WRITE_RE.finditer(line):
            hits.append((lineno, match.group(1), line.strip()))
    return hits


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    violations = []
    scanned = 0
    exempt_files = []
    for path in sorted(SRC.rglob("*.rs")):
        if path == WRITER:
            continue
        if "test" in path.name:
            exempt_files.append(str(path.relative_to(ROOT)))
            continue
        scanned += 1
        for lineno, table, code in scan_file(path):
            violations.append(
                dict(file=str(path.relative_to(ROOT)), line=lineno, table=table, code=code)
            )

    writer_hits = scan_file(WRITER) if WRITER.exists() else []
    ok = not violations and WRITER.exists()

    if args.json:
        print(json.dumps(
            dict(ok=ok, scanned=scanned, exempt_files=len(exempt_files),
                 writer_statements=len(writer_hits), violations=violations),
            ensure_ascii=False, indent=2))
    else:
        print("=" * 92)
        print("库存三本账唯一写入口闸（P2-5A）")
        print(f"扫描 {scanned} 个非测试 .rs（豁免 {len(exempt_files)} 个测试文件）")
        print(f"唯一写入口 {WRITER.relative_to(ROOT)}：{len(writer_hits)} 条写语句")
        print("=" * 92)
        if not WRITER.exists():
            print(f"✗ 唯一写入口不存在：{WRITER}")
        if violations:
            print(f"\n✗ 有 {len(violations)} 处越界写入（必须改走 inventory_writer）：")
            for v in violations:
                print(f"  · {v['file']}:{v['line']}  [{v['table']}]  {v['code'][:110]}")
        else:
            print("\n★ 零越界：非测试代码里没有任何一处直写库存三本账。")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
