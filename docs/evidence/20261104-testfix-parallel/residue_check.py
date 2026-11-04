#!/usr/bin/env python3
"""25926f63 库侧残留复查（只读，不经测试代码）。

用法：ERP_DB=<dbname> python3 /root/25926f63_residue.py
输出：每类单据的 PENDING 流程实例数 + 孤儿状态日志数 + 2040 号段期间行数。
"""
import os
import subprocess
import sys

DB = os.environ.get("ERP_DB", "cloudpivot_25926f63")
URL = os.environ.get("ERP_DB_URL", "")

TABLES = [
    ("STOCK_IN", "biz_stock_in"),
    ("PURCHASE_ORDER", "biz_purchase_order"),
    ("STOCK_TRANSFER", "biz_stock_transfer"),
    ("PAYABLE", "biz_payable"),
    ("VOUCHER", "biz_voucher"),
    ("ACCOUNTING_PERIOD", "biz_accounting_period"),
    ("CONTRACT", "biz_contract"),
]


def psql(sql):
    env = dict(os.environ, PGPASSWORD="alun")
    if URL:
        args = ["psql", URL, "-Atc", sql]
    else:
        args = ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-Atc", sql]
    r = subprocess.run(args, capture_output=True, text=True, env=env)
    if r.returncode != 0:
        return f"<ERR {r.stderr.strip()[:160]}>"
    return r.stdout.strip()


def main():
    print(f"[residue] db={DB} @ {__import__('datetime').datetime.now():%Y-%m-%d %H:%M:%S}")
    print("-- PENDING 流程实例（按 biz_type）--")
    print(psql(
        "SELECT biz_type || '=' || count(*) FROM biz_flow_instance "
        "WHERE status='PENDING' GROUP BY biz_type ORDER BY biz_type"
    ) or "(none)")
    print("-- 孤儿状态日志（entity_id 指向已不存在单据）--")
    total = 0
    for et, tbl in TABLES:
        n = psql(f"SELECT count(*) FROM biz_status_log WHERE entity_type='{et}' "
                 f"AND entity_id NOT IN (SELECT id FROM {tbl})")
        try:
            total += int(n)
        except ValueError:
            pass
        print(f"  {et:18s} = {n}")
    print(f"  ORPHAN_TOTAL = {total}")
    print("-- 夹具号段行数（2040 = period_items 独占号段；AP-P17 / P17- = payable_bridge）--")
    print("  biz_accounting_period 2040-% = " + psql(
        "SELECT count(*) FROM biz_accounting_period WHERE period_code LIKE '2040-%'"))
    print("  biz_payable AP-P17-%       = " + psql(
        "SELECT count(*) FROM biz_payable WHERE payable_no LIKE 'AP-P17-%'"))
    print("-- 角色绑定（SEED_ROLE_LOCK 组）--")
    print(psql(
        "SELECT r.role_code || '=' || count(*) FROM sys_user_role ur "
        "JOIN sys_role r ON r.id=ur.role_id WHERE r.role_code IN ('PURCHASE_MANAGER','GENERAL_MANAGER') "
        "GROUP BY r.role_code ORDER BY 1"
    ) or "(none)")


if __name__ == "__main__":
    sys.exit(main())
