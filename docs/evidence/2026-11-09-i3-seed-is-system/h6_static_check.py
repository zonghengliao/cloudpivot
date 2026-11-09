#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""只跑 H6 静态检查（不连库），打印每条 INSERT 的判定结果。"""
import importlib.util
import sys

sys.path.insert(0, "/opt/cloudpivot/scripts/verify")
spec = importlib.util.spec_from_file_location(
    "ei", "/opt/cloudpivot/scripts/verify/erp_invariants.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

m._check_flow_seed_is_system()
for r in m.RESULT:
    print(r)

print("=" * 70)
print("逐条明细：")
migrations = m._load_up_migrations()
for ver, path, text in migrations:
    if ver < m._IS_SYSTEM_COLUMN_FROM:
        continue
    code_text = m._blank_sql_comments(text)
    for line, cols, stmt in m._flow_def_inserts(code_text):
        code = m._flow_code_of(stmt)
        has = "is_system" in cols
        by = None if has else m._compensating_migration(code, ver, migrations)
        print(f"V{ver:04d} {path.name}:{line} flow_code={code} is_system_in_cols={has} 补偿={by}")
    for mm in m._INSERT_FLOW_DEF_IMPLICIT_RE.finditer(code_text):
        ln = code_text.count("\n", 0, mm.start()) + 1
        print(f"V{ver:04d} {path.name}:{ln} 隐式列清单")
