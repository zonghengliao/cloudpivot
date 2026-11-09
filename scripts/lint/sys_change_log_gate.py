#!/usr/bin/env python3
"""C7 G1（v2 · 只判生产面）：`sys_change_log` 只能由 `business/changelog/` 访问。

## 为什么有这道闸

变更日志是**审计证据**：谁、什么时候、把哪张单的哪个字段从 A 改成 B。它必须只有一条
写入路径（changelog 模块），否则「同一次修改写了 2 行」「某条路径忘写」这类问题会在
审计里静默存在 —— 报表按 resource_id 数行数时才发现，那时已经追不回来了。

## 为什么口径从 v1（纯 grep）改成 v2（只判生产面）

v1 的判据是 `grep -rn sys_change_log | grep -v "business/changelog/"`，只排「整行注释」。
2026-11-10（卡 t_2746ca53）实测它在 HEAD 上是红的，两条命中**全部是测试代码**：

    backend/src/business/erp/contract_handler.rs:1998  #[cfg(test)] 里断言「改一次只写一行」
    backend/src/business/erp/contract_handler.rs:2059  #[cfg(test)] 里 count(*) 断言「值没变不写」

测试**读**一眼审计表来断言行为，不是生产写路径。判红等于「写这个测试要跟门禁打架」，
于是要么有人关闸，要么有人不写测试 —— 两种结果都比这道闸想防的问题更糟。

v2 与同仓的 `scripts/lint/scope_gate.py`（门禁口径 v2）和
`scripts/verify/inventory_write_gate.py` 完全同口径：**只判定会进生产编译的代码**
（测试文件按命名整体跳过 + 文件内 `#[cfg(test)]` 项按花括号配对跳过），
并且自带 `--self-test` 红/绿语料守住判据本身（判据被改坏时自检先红，不会静默变绿）。

判据**没有放宽**：生产代码里出现 `sys_change_log` 仍然是 0 容忍 —— v1 能抓到的生产越界，
v2 一条不落地照样抓（自检语料里专门有这条）。

用法：
    python3 scripts/lint/sys_change_log_gate.py             # 判红/判绿
    python3 scripts/lint/sys_change_log_gate.py --self-test # 判据自检（不扫仓库）
    python3 scripts/lint/sys_change_log_gate.py --json      # 机读输出

退出码：0 = 生产面 0 命中；1 = 有越界；2 = 环境错误（源码目录不存在）
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# 复用「抹注释 + 识别 #[cfg(test)] 项 + 测试文件命名」同一套实现，
# 避免同一语义在两处脚本里各写一份、慢慢漂移。
_VERIFY_DIR = Path(__file__).resolve().parent.parent / "verify"
if str(_VERIFY_DIR) not in sys.path:
    sys.path.insert(0, str(_VERIFY_DIR))

from inventory_write_gate import _cfg_test_spans, is_test_file, strip_comments  # noqa: E402

TABLE = "sys_change_log"
ALLOWED_PREFIX = "business/changelog/"  # 唯一允许访问该表的模块


def scan_source(name: str, text: str) -> list[tuple[int, str]]:
    """返回 [(行号, 代码片段)]，只报**生产面**的命中（测试文件 / #[cfg(test)] 项不算）。

    `name` 用相对 src 的 posix 路径（如 `business/erp/contract_handler.rs`）。
    """
    if is_test_file(Path(name)):
        return []
    if name.startswith(ALLOWED_PREFIX):
        return []
    clean = strip_comments(text)
    spans = _cfg_test_spans(clean)
    line_starts = [0] + [i + 1 for i, ch in enumerate(clean) if ch == "\n"]

    hits: list[tuple[int, str]] = []
    start = 0
    while True:
        pos = clean.find(TABLE, start)
        if pos < 0:
            break
        start = pos + len(TABLE)
        if any(a <= pos < b for a, b in spans):
            continue
        # 行号 = 该偏移之前有多少个换行
        lo, hi = 0, len(line_starts)
        while lo < hi:
            mid = (lo + hi) // 2
            if line_starts[mid] <= pos:
                lo = mid + 1
            else:
                hi = mid
        lineno = lo
        line_end = clean.find("\n", pos)
        line_end = len(clean) if line_end < 0 else line_end
        snippet = " ".join(clean[max(line_starts[lineno - 1], pos - 40):line_end].split())
        hits.append((lineno, snippet.strip()))
    return hits


# ══════════════════════════════════════════════════════════════════════════
# 判据自检：红/绿语料（每条都是「这道闸必须能分辨」的真实形态）
# ══════════════════════════════════════════════════════════════════════════
SELF_TEST_CASES: list[tuple[str, str, str, bool]] = [
    (
        "生产代码 SELECT 审计表",
        "business/erp/contract_handler.rs",
        'async fn f() { db().find("SELECT id FROM sys_change_log WHERE resource = ?").await; }',
        True,   # 期望红
    ),
    (
        "生产代码 INSERT 审计表",
        "business/erp/sales_order.rs",
        'db().execute("INSERT INTO sys_change_log (id) VALUES (?)").await;',
        True,
    ),
    (
        "changelog 模块自己写审计表",
        "business/changelog/model.rs",
        'db().execute("INSERT INTO sys_change_log (id) VALUES (?)").await;',
        False,  # 期望绿（唯一合法入口）
    ),
    (
        "测试文件整体豁免",
        "business/erp/contract_handler_tests.rs",
        'db().find("SELECT 1 FROM sys_change_log").await;',
        False,
    ),
    (
        "tests 目录豁免",
        "business/erp/payable_payment/tests/linkage.rs",
        'db().find("SELECT 1 FROM sys_change_log").await;',
        False,
    ),
    (
        "文件内 #[cfg(test)] 项豁免（本次实测命中的形态）",
        "business/erp/contract_handler.rs",
        'pub async fn prod() { 1 }\n#[cfg(test)]\nmod tests {\n    async fn t() {\n        db().find("SELECT 1 FROM sys_change_log").await;\n    }\n}\n',
        False,
    ),
    (
        "注释里提到表名不算",
        "business/erp/contract_handler.rs",
        "// 不要直接写 sys_change_log，走 changelog::record\npub async fn prod() { 1 }\n",
        False,
    ),
    (
        "块注释里提到表名不算",
        "business/erp/contract_handler.rs",
        "/* SELECT * FROM sys_change_log */\npub async fn prod() { 1 }\n",
        False,
    ),
    (
        "#[cfg(test)] 之后的**生产代码**仍然要判红（别把整个文件豁免掉）",
        "business/erp/contract_handler.rs",
        '#[cfg(test)]\nmod tests { fn t() {} }\npub async fn prod() { db().find("SELECT 1 FROM sys_change_log").await; }\n',
        True,
    ),
]


def self_test() -> bool:
    bad = 0
    print("=" * 92)
    print("C7 G1 判据自检（红/绿语料）")
    print("=" * 92)
    for label, name, code, expect_red in SELF_TEST_CASES:
        hits = scan_source(name, code)
        got_red = bool(hits)
        ok = got_red == expect_red
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 期望{'红' if expect_red else '绿'} 实得{'红' if got_red else '绿'}  {label}")
    print("-" * 92)
    print(f"{len(SELF_TEST_CASES) - bad}/{len(SELF_TEST_CASES)} 通过" + ("" if not bad else f"；✗ {bad} 条不符"))
    return bad == 0


def main() -> None:
    ap = argparse.ArgumentParser(description="C7 G1：sys_change_log 只能由 business/changelog/ 访问（只判生产面）")
    ap.add_argument("--root", default=str(Path(__file__).resolve().parents[2]), help="仓库根")
    ap.add_argument("--src", default=None, help="源码目录（默认 <root>/backend/src）")
    ap.add_argument("--self-test", action="store_true", dest="self_test")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        sys.exit(0 if self_test() else 1)

    root = Path(args.root).resolve()
    src = Path(args.src).resolve() if args.src else root / "backend" / "src"
    if not src.is_dir():
        print(f"✗ 源码目录不存在：{src}", file=sys.stderr)
        sys.exit(2)

    violations: list[dict] = []
    scanned = 0
    skipped = 0
    for path in sorted(src.rglob("*.rs")):
        rel = path.relative_to(src).as_posix()
        if rel.startswith(ALLOWED_PREFIX):
            continue
        if is_test_file(path):
            skipped += 1
            continue
        scanned += 1
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:  # pragma: no cover
            continue
        for lineno, snippet in scan_source(rel, text):
            violations.append(dict(file=f"backend/src/{rel}", line=lineno, code=snippet))

    ok = not violations
    if args.json:
        print(json.dumps(dict(ok=ok, scanned=scanned, skipped_test_files=skipped,
                              violations=violations), ensure_ascii=False, indent=2))
        sys.exit(0 if ok else 1)

    print("=" * 92)
    print("C7 G1：sys_change_log 只能由 business/changelog/ 访问（只判生产面）")
    print(f"扫描 {scanned} 个非测试 .rs（整体豁免 {skipped} 个测试文件；#[cfg(test)] 项按花括号配对豁免）")
    print("=" * 92)
    if violations:
        print(f"\n✗ 有 {len(violations)} 处生产代码在访问 sys_change_log（必须走 changelog 模块）：")
        for v in violations:
            print(f"  · {v['file']}:{v['line']}  {v['code'][:110]}")
    else:
        print("\n★ 生产面 0 命中：sys_change_log 只有 business/changelog/ 在碰。")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
