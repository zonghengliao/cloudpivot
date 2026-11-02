#!/usr/bin/env python3
"""字段自洽审计：把巡检 SQL 里出现的每个「别名.列」逐个对到真实列上。

用途：汇总卡的核心职责之一是「DDL 与巡检 SQL 引用的字段全部存在，不得引用未定义列」。
本脚本把 docs/architecture/snapshot-drift/10_checks.sql 里所有 alias.column 抽出来，
逐个在探针库 information_schema 里核实，输出 命中表 / 缺失。

用法：
  su postgres -c "psql -d arch2_synth_poc -tA -c \"...\"" 的结果由调用方喂进来，
  或直接本脚本自带 psql 调用（默认库 arch2_synth_poc，可用环境变量 DI_DB 覆盖）。
"""
import os
import re
import subprocess
import sys

CHECKS = "docs/architecture/snapshot-drift/10_checks.sql"
DB = os.environ.get("DI_DB", "arch2_synth_poc")

# 别名 → 候选对象（业务表 / 巡检视图）。巡检 SQL 只允许出现这些对象上的列。
ALIAS = {
    "o": ["biz_purchase_order", "v_po"],
    "c": ["biz_contract", "v_contract"],
    "s": ["biz_stock_in"],
    "p": ["biz_payable", "po"],            # p 在 DI-01 里是 CTE（po）别名
    "sp": ["biz_supplier"],
    "pc": ["biz_contract_party_change", "v_party_change"],
}
# CTE / 派生列（不是真实表列，但由真实列算出，列在下面登记来源）
DERIVED = {
    "po_id": "biz_purchase_order.id",
    "changed_at": "biz_contract_party_change.changed_at / di.v_party_change.changed_at",
    "order_no": "biz_purchase_order.order_no",
    "contract_no": "biz_contract.contract_no",
    "party_b_id": "biz_contract.party_b_id",
}
# pg_catalog / 自检项用到的系统列（DI-14 读 pg_constraint / pg_indexes），不属业务列
SYSTEM = {
    "conname", "conrelid", "indexdef", "indexname", "schemaname", "tablename",
    "kind", "obj", "rel",
}


def q(sql):
    out = subprocess.run(
        ["psql", "-tA", "-F", "\t", "-d", DB, "-c", sql],
        capture_output=True, text=True, check=True,
    )
    return [l.split("\t") for l in out.stdout.strip().splitlines() if l.strip()]


def main():
    sql = open(CHECKS, encoding="utf-8").read()
    # 去掉行注释，避免把注释里的示例列名当成引用
    sql = re.sub(r"--[^\n]*", "", sql)

    pairs = sorted({f"{a}.{b}" for a, b in re.findall(r"\b([a-z]{1,2})\.([a-z_]+)\b", sql)})
    cols = q("select table_name, column_name from information_schema.columns "
             "where table_schema in ('public','di')")
    have = {}
    for t, c in cols:
        have.setdefault(t, set()).add(c)
    # 临时 CTE「po」的列：由 DDL 定义在 DI-01 里（po_id/order_no/supplier_id/po_status/contract_id/created_at/contract_no/party_b_id/changed_at）
    have["po"] = {"po_id", "order_no", "supplier_id", "po_status", "contract_id",
                  "created_at", "contract_no", "party_b_id", "changed_at"}

    missing = []
    rows = []
    for pair in pairs:
        a, col = pair.split(".", 1)
        if a == "di" and col in ("v_po", "v_contract", "v_party_change", "di_capability",
                                 "c_di01", "c_di02", "c_di03", "c_di04", "c_di05", "c_di06",
                                 "c_di07", "c_di08", "c_di09", "c_di10", "c_di11", "c_di12",
                                 "c_di13", "c_di14", "c_di15"):
            rows.append((pair, "SCHEMA_QUALIFIED_OBJECT", "ok"))
            continue
        if a in ("c", "i", "e", "m") and col in SYSTEM:
            rows.append((pair, "SYSTEM(pg_catalog)", "ok"))
            continue
        if a not in ALIAS:
            rows.append((pair, "-", "UNKNOWN_ALIAS"))
            missing.append(pair)
            continue
        hits = [t for t in ALIAS[a] if col in have.get(t, set())]
        if hits:
            rows.append((pair, ",".join(hits), "ok"))
        elif col in DERIVED:
            rows.append((pair, "DERIVED:" + DERIVED[col], "ok"))
        else:
            rows.append((pair, "-", "MISSING"))
            missing.append(pair)

    w = max(len(r[0]) for r in rows)
    print(f"{'alias.column'.ljust(w)}  {'命中对象'.ljust(34)} 结果")
    print("-" * (w + 46))
    for pair, where, status in rows:
        print(f"{pair.ljust(w)}  {where.ljust(34)} {status}")
    print()
    print(f"引用总数={len(rows)}  缺失={len(missing)}")
    if missing:
        print("缺失清单：" + ", ".join(missing))
        return 1
    print("结论：巡检 SQL 引用的每个字段都能在收敛后的 schema（V0193..V0197）里找到，无未定义列。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
