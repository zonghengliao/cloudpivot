#!/usr/bin/env python3
"""打印 H6 的判定明细（含「已由 V0301 补偿」这一行），不连库也能跑。"""
import sys

sys.path.insert(0, "/opt/cloudpivot/scripts/verify")
import erp_invariants as e  # noqa: E402

migrations = e._load_up_migrations()
print("migrations scanned:", len(migrations))
for ver, path, text in migrations:
    if ver < e._IS_SYSTEM_COLUMN_FROM:
        continue
    code_text = e._blank_sql_comments(text)
    for line, cols, stmt in e._flow_def_inserts(code_text):
        code = e._flow_code_of(stmt)
        by = e._compensating_migration(code, ver, migrations) if "is_system" not in cols else None
        print(f"V{ver:04d} {path.name}:{line} flow_code={code} cols_has_is_system={'is_system' in cols} "
              f"compensated_by={by}")
e._check_flow_seed_is_system()
