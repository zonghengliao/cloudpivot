#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""NIGHT-REGRESSION · 三条不变量的「正例 + 负例」实测探针
===========================================================

要证明的不是「巡检脚本存在」，而是 **它真的跑、且真的能抓到脏数据**：

| 不变量 | 巡检 | 正例（干净库） | 负例（塞脏行） |
|---|---|---|---|
| ① 收付实现：出库单已过账 → 必有成本结转凭证（P2-9 / DC-13） | `erp_invariants.py --group 账账` 的 B5 | 全绿 | 插一张 POSTED 且 total_amount>0、voucher_id 为空的出库单 → B5 必须 BLOCKER |
| ② 词表一致：业务类型必须在枚举范围内（P2-5） | `biz_type_domain.py` | 全绿 | 插一条 `biz_voucher_rule.biz_type='NR_BOGUS_TYPE'` → 必须退出码 1 且点名该列该值 |
| ③ 单据元数据：有 doc_id 必须能查到单据（P2-9 / DC-08） | `snapshot_audit.py --group DI-18` | 全绿 | 插一条 doc_type='STOCK_IN'、doc_id 指向不存在单据的流水 → DI-18 必须 BLOCKER |

每个负例**做完即删**，删完再跑一次确认回到全绿（证明脏行是唯一变量、不是环境噪声）。

只动隔离库（`db_guard` 拒生产库）。用法：

    NIK_DATABASE_NAME=cloudpivot_nightreg_inv NIK_DATABASE_USER=alun \\
    NIK_DATABASE_PASSWORD=alun python3 scripts/verify/nightreg_dirty_probe.py
"""
from __future__ import annotations

import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402

_require_non_prod(script=os.path.basename(__file__))

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DB = {
    "host": os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    "port": os.environ.get("NIK_DATABASE_PORT", "5432"),
    "user": os.environ.get("NIK_DATABASE_USER", "alun"),
    "name": os.environ.get("NIK_DATABASE_NAME", "cloudpivot_nightreg_inv"),
    "password": os.environ.get("NIK_DATABASE_PASSWORD", "alun"),
}
ENV = dict(os.environ, PGPASSWORD=DB["password"],
           NIK_DATABASE_HOST=DB["host"], NIK_DATABASE_PORT=DB["port"],
           NIK_DATABASE_USER=DB["user"], NIK_DATABASE_NAME=DB["name"],
           NIK_DATABASE_PASSWORD=DB["password"])
RESULTS: list[tuple[bool, str]] = []


def psql(sql: str) -> str:
    proc = subprocess.run(
        ["psql", "-h", DB["host"], "-p", DB["port"], "-U", DB["user"], "-d", DB["name"],
         "-tA", "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True, text=True, env=ENV, timeout=120,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip()[:400])
    return proc.stdout.strip()


def run(script: str, *args: str) -> tuple[int, str]:
    proc = subprocess.run(
        [sys.executable, os.path.join(ROOT, "scripts", "verify", script), *args],
        capture_output=True, text=True, env=ENV, timeout=600, cwd=ROOT,
    )
    return proc.returncode, proc.stdout + proc.stderr


def check(cond: bool, name: str, detail: str = "") -> None:
    RESULTS.append((bool(cond), name))
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}" + (f"  — {detail}" if detail else ""))


def grep_line(text: str, needle: str) -> str:
    for line in text.splitlines():
        if needle in line:
            return line.strip()[:300]
    return ""


def main() -> int:
    print(f"== 三条不变量 正例/负例实测 @ 库 {DB['name']} ==")
    wh = psql("select id from biz_warehouse where warehouse_code='WH01' limit 1").strip()
    mat = psql("select id from biz_material where material_code='M-W1A-001' and deleted_at is null limit 1").strip()
    if not wh or not mat:
        print("夹具缺失：WH01 / M-W1A-001 不在本库（换个干净库跑）")
        return 2

    # ---------------- 正例：三条巡检在干净库都必须全绿 ----------------
    print("\n== 正例：干净库 ==")
    rc, out = run("erp_invariants.py", "--group", "账账")
    check(rc == 0 and "BLOCKER 0" in out, "① B5：干净库「账账」组 BLOCKER 0",
          grep_line(out, "B5 出库单已过账"))
    rc_v, out_v = run("biz_type_domain.py")
    check(rc_v == 0, "② 词表：干净库全绿（退出码 0）", grep_line(out_v, "域外") or out_v.strip().splitlines()[-1][:160])
    rc_d, out_d = run("snapshot_audit.py", "--group", "DI-18")
    check(rc_d == 0, "③ DI-18：干净库通过", grep_line(out_d, "DI-18"))

    # ---------------- 负例①：已过账出库单没有成本结转凭证 ----------------
    print("\n== 负例①：塞一张「已过账但无凭证」的出库单 ==")
    psql(
        "insert into biz_stock_out (out_no, out_type, warehouse_id, out_date, status, total_qty, total_amount) "
        f"values ('NR-DIRTY-B5', 'ISSUE', '{wh}'::uuid, current_date, 'POSTED', 1, 100)"
    )
    try:
        rc, out = run("erp_invariants.py", "--group", "账账")
        line = grep_line(out, "B5 出库单已过账")
        check(rc != 0 and "BLOCKER 1" in out, "① 负例：B5 抓到了（BLOCKER 1 + 退出码非 0）", line)
        check("NR-DIRTY-B5" in out or "缺凭证" in out, "① 负例：报错点名到具体单据", line)
    finally:
        psql("delete from biz_stock_out where out_no = 'NR-DIRTY-B5'")
    rc, out = run("erp_invariants.py", "--group", "账账")
    check(rc == 0, "① 删除脏行后回到全绿（脏行是唯一变量）", grep_line(out, "结果"))

    # ---------------- 负例②：词表域外值 ----------------
    print("\n== 负例②：塞一条 biz_type 域外值的凭证规则 ==")
    psql(
        "insert into biz_voucher_rule (rule_code, biz_type, is_active) "
        "values ('NR-DIRTY-RULE', 'NR_BOGUS_TYPE', 0)"
    )
    try:
        rc_v, out_v = run("biz_type_domain.py")
        hit = grep_line(out_v, "NR_BOGUS_TYPE")
        check(rc_v == 1, "② 负例：词表巡检退出码 1", hit)
        check("biz_voucher_rule" in out_v and "NR_BOGUS_TYPE" in out_v,
              "② 负例：点名「表.列 = 值」", hit)
    finally:
        psql("delete from biz_voucher_rule where rule_code = 'NR-DIRTY-RULE'")
    rc_v, out_v = run("biz_type_domain.py")
    check(rc_v == 0, "② 删除脏行后回到全绿", grep_line(out_v, "全部") or "")

    # ---------------- 负例③：doc_id 指向不存在的单据 ----------------
    print("\n== 负例③：塞一条 doc_id 悬空的库存流水 ==")
    psql(
        "insert into biz_inventory_txn (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, "
        " doc_no, line_no, qty, period_code) values "
        f" ('{mat}'::uuid, '{wh}'::uuid, 'IN', 'PURCHASE_IN', 'STOCK_IN', gen_random_uuid(), "
        "  'NR-DIRTY-TXN', 1, 1, '2026-11')"
    )
    try:
        rc_d, out_d = run("snapshot_audit.py", "--group", "DI-18")
        hit = grep_line(out_d, "DI-18")
        check(rc_d == 1 and "DI-18" in out_d, "③ 负例：DI-18 抓到了（BLOCKER + 退出码 1）", hit)
        check("NR-DIRTY-TXN" in out_d, "③ 负例：报错点名到具体流水", hit)
    finally:
        psql("delete from biz_inventory_txn where doc_no = 'NR-DIRTY-TXN'")
    rc_d, out_d = run("snapshot_audit.py", "--group", "DI-18")
    check(rc_d == 0, "③ 删除脏行后回到全绿", grep_line(out_d, "汇总"))

    failed = [r for r in RESULTS if not r[0]]
    print(f"\n== 汇总：{len(RESULTS) - len(failed)}/{len(RESULTS)} 条通过 ==")
    for _, name in failed:
        print(f"  [FAIL] {name}")
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
