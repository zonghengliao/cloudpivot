#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
P2-1b 切换点证据（隔离实例 + 真 HTTP + 真 SQL，不碰生产）
=========================================================

要留下的是什么：**切换日本身**的行为证据 —— 切换日**之后**新建的多物料单
按「每个去重物料一行」出分录，切换日之前的存量单不动（§6 不回填）。

本脚本逐条打印 SQL 原始输出（不是"我记得是 5 行"，而是查询结果）：

  ① 3 物料收货单（甲 10@10 + 乙 20@5 + 丙 30@2，13%）→ 过账
       · 凭证 `entry_count = M + 2 = 5`
       · 3 条 1405 存货行，逐行带 `aux_material_id`（各 = 该物料）+ `aux_warehouse_id`
       · `Σ 1405 = 单头 total_amount`
       · 该单的 1405 分录里 `aux_material_id IS NULL` 的行数 = 0 ⇒ **不再进 unassigned**
  ② 2 物料出库单（甲 5 + 乙 5）→ 过账
       · 凭证 4 行（借 6401×2 + 贷 1405×2 = 2M）
       · 借方逐行带物料、不挂仓库；贷方逐行带物料 + 仓库
  ③ 按物料辅助账：丙 的 1405 本期借方 = 该物料成本（30×2 = 60）

跑法（隔离实例已在跑）：

    bash scripts/verify/p21_instance.sh start
    python3 scripts/verify/p21b_switch_point.py --port 18099 --db cloudpivot_p16md

退出码：全 PASS → 0；有任何 FAIL → 1。
"""

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE = "http://127.0.0.1:18099"
DB = "cloudpivot_p16md"
DB_USER = "alun"
DB_PASS = "alun"
TOKEN = ""
RESULTS = []

TAG = "P21BSP"
UNIT_ID = "e21b0101-0000-4000-8000-000000000001"
SUPPLIER_ID = "e21b0101-0000-4000-8000-000000000002"
WAREHOUSE_ID = "e21b0101-0000-4000-8000-000000000003"
MAT1_ID = "e21b0101-0000-4000-8000-000000000011"  # 甲 10 @ 10
MAT2_ID = "e21b0101-0000-4000-8000-000000000012"  # 乙 20 @ 5
MAT3_ID = "e21b0101-0000-4000-8000-000000000013"  # 丙 30 @ 2


# ---------------------------------------------------------------- 基础设施


def psql(sql, db=None):
    env = dict(os.environ)
    env["PGPASSWORD"] = DB_PASS
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-p", "5432", "-U", DB_USER, "-d", db or DB,
         "-tAF", "|", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(f"psql 失败：{r.stderr.strip()[:400]}\nSQL: {sql[:200]}")
    return [line for line in r.stdout.strip().split("\n") if line != ""]


def scalar(sql, db=None):
    rows = psql(sql, db)
    return rows[0].split("|")[0] if rows else None


def num(value):
    return Decimal(str(value if value is not None else "0").strip() or "0")


def ok(name, cond, detail=""):
    RESULTS.append((bool(cond), name, detail))
    print(f"  {'[PASS]' if cond else '[FAIL]'} {name}" + (f"  — {detail}" if detail and not cond else ""))


def call(method, path, body=None):
    if not path.startswith("/api/"):
        path = "/api" + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if TOKEN:
        req.add_header("Authorization", f"Bearer {TOKEN}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except Exception:
            return err.code, {"raw": raw}


def login():
    global TOKEN

    def attempt(password):
        status, body = call("POST", "/api/system/auth/login",
                            {"username": "admin", "password": password})
        return status, body

    for password in ("Admin@123456", "Admin@1234567"):
        status, body = attempt(password)
        if status == 200:
            data = body.get("data") or {}
            TOKEN = data.get("access_token") or data.get("token") or ""
            # 干净库首次登录必须先改初始密码（否则后续接口 403）
            if TOKEN and data.get("must_change_password"):
                status, body = call("PUT", "/api/system/auth/password",
                                    {"old_password": password, "new_password": "Admin@1234567"})
                if status != 200:
                    raise RuntimeError(f"首登改密失败：{status} {body}")
                TOKEN = ""
                status, body = attempt("Admin@1234567")
                TOKEN = (body.get("data") or {}).get("access_token") or ""
            if TOKEN:
                print(f"  登录成功（令牌 {TOKEN[:12]}…）")
                return
    raise RuntimeError("登录失败：admin 口令不是既有的两个")


# ---------------------------------------------------------------- 夹具


def cleanup():
    psql(f"""
    delete from biz_status_log where entity_type in ('STOCK_IN', 'STOCK_OUT') and entity_id in
        (select id from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid
         union select id from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_voucher where source_type = 'STOCK_OUT' and source_id in
        (select id from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_voucher where source_type = 'STOCK_IN' and source_id in
        (select id from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_payable where supplier_id = '{SUPPLIER_ID}'::uuid;
    delete from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_inventory_txn where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_inventory_location where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_inventory where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_material where id in
        ('{MAT1_ID}'::uuid, '{MAT2_ID}'::uuid, '{MAT3_ID}'::uuid);
    delete from biz_warehouse where id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_supplier where id = '{SUPPLIER_ID}'::uuid;
    delete from biz_unit where id = '{UNIT_ID}'::uuid;
    """)


def fixtures():
    psql(f"""
    insert into biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
    values ('{UNIT_ID}', '{TAG}-PCS', '个', 1, null, 1.000000, 1, 1);
    insert into biz_supplier (id, supplier_code, supplier_name, status, credit_days)
    values ('{SUPPLIER_ID}', '{TAG}-SUP', '{TAG} 供应商', 1, 30);
    insert into biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf,
                               status, allow_negative)
    values ('{WAREHOUSE_ID}', '{TAG}-WH', '{TAG} 收货仓', '/{WAREHOUSE_ID}/', 1, 1, 1, 0);
    insert into biz_material (id, material_code, material_name, unit_id, status, serial_required)
    values ('{MAT1_ID}', '{TAG}-M1', '{TAG} 物料甲', '{UNIT_ID}', 1, false),
           ('{MAT2_ID}', '{TAG}-M2', '{TAG} 物料乙', '{UNIT_ID}', 1, false),
           ('{MAT3_ID}', '{TAG}-M3', '{TAG} 物料丙', '{UNIT_ID}', 1, false);
    """)
    # 期间必须 OPEN（单据日期 = 今天）
    today = scalar("select to_char(current_date, 'YYYY-MM-DD')")
    year, month = today.split("-")[0], today.split("-")[1]
    start = f"{year}-{month}-01"
    psql(f"""
    insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
    values ({year}, {int(month)}, '{year}-{month}', date '{start}',
            (date '{start}' + interval '1 month' - interval '1 day')::date, 'OPEN')
    on conflict (period_code) do update set status = 'OPEN';
    """)


# ---------------------------------------------------------------- 主流程


def post_stock_in(items, remark):
    status, resp = call("POST", "/business/erp/stock-in", {
        "warehouse_id": WAREHOUSE_ID, "is_sourceless": True, "supplier_id": SUPPLIER_ID,
        "remark": remark, "items": items,
    })
    if status != 200:
        return None, f"建单 {status} {resp}"
    data = resp.get("data") or {}
    doc_id = data.get("id") or (data.get("header") or {}).get("id")
    status, resp = call("PATCH", f"/business/erp/stock-in/{doc_id}/status", {"status": "POSTED"})
    if status != 200:
        return None, f"过账 {status} {resp}"
    return doc_id, ""


def main():
    global BASE, DB
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18099)
    ap.add_argument("--db", default=DB)
    args = ap.parse_args()
    BASE = f"http://127.0.0.1:{args.port}"
    DB = args.db
    print("=" * 92)
    print(f"P2-1b 切换点证据（多物料按物料拆分录）   库={DB}   接口={BASE}")
    print("=" * 92)

    print("\n⓪ 清场 + 夹具 + 登录")
    cleanup()
    fixtures()
    login()
    ok("3 个物料 / 仓库 / 供应商 / 单位就位",
       scalar(f"select count(*) from biz_material where id in "
              f"('{MAT1_ID}'::uuid,'{MAT2_ID}'::uuid,'{MAT3_ID}'::uuid)") == "3")

    print("\n① 3 物料收货（甲 10@10 + 乙 20@5 + 丙 30@2，税 13%）→ 过账")
    doc_id, err = post_stock_in([
        {"material_id": MAT1_ID, "qty": "10", "price_excl": "10", "tax_rate": "13"},
        {"material_id": MAT2_ID, "qty": "20", "price_excl": "5", "tax_rate": "13"},
        {"material_id": MAT3_ID, "qty": "30", "price_excl": "2", "tax_rate": "13"},
    ], f"{TAG} 3 物料收货")
    ok("3 物料收货过账成功", bool(doc_id), err)
    if not doc_id:
        return summarize()

    header = scalar(f"select total_amount from biz_stock_in where id = '{doc_id}'::uuid")
    print(f"    单头 total_amount = {header}")
    print("    —— 该单的凭证分录（SQL 原样输出）——")
    rows = psql(f"""
    select e.line_no, e.account_code, e.debit_amount, e.credit_amount,
           coalesce(m.material_code, '(空)'), coalesce(e.aux_warehouse_id::text, '(空)')
      from biz_voucher_entry e
      join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
      left join biz_material m on m.id = e.aux_material_id
     where v.source_type = 'STOCK_IN' and v.source_id = '{doc_id}'::uuid and v.is_red = 0
     order by e.line_no""")
    for row in rows:
        print(f"    {row}")
    entry_count = scalar(f"select entry_count from biz_voucher where source_type = 'STOCK_IN'"
                         f" and source_id = '{doc_id}'::uuid and is_red = 0")
    ok("凭证 entry_count = M + 2 = 5", entry_count == "5", f"实际 {entry_count}")
    inv = [r for r in rows if r.split("|")[1] == "1405"]
    ok("1405 存货行 3 条（每个去重物料一行）", len(inv) == 3, f"实际 {len(inv)} 条")
    ok("逐行物料：甲 / 乙 / 丙（按 material_id 升序）",
       [r.split("|")[4] for r in inv] == [f"{TAG}-M1", f"{TAG}-M2", f"{TAG}-M3"],
       str([r.split("|")[4] for r in inv]))
    ok("逐行金额 = 该物料明细金额（100 / 100 / 60）",
       [num(r.split("|")[2]) for r in inv] == [Decimal("100"), Decimal("100"), Decimal("60")],
       str([r.split("|")[2] for r in inv]))
    ok("每行都挂收货仓",
       all(r.split("|")[5] == WAREHOUSE_ID for r in inv), str([r.split("|")[5] for r in inv]))
    inv_sum = sum((num(r.split("|")[2]) for r in inv), Decimal("0"))
    ok("Σ 1405 分录 = 单头 total_amount", inv_sum == num(header), f"{inv_sum} vs {header}")
    null_aux = scalar(f"""
    select count(*) from biz_voucher_entry e
      join biz_voucher v on v.id = e.voucher_id
     where v.source_type = 'STOCK_IN' and v.source_id = '{doc_id}'::uuid and v.is_red = 0
       and e.account_code = '1405' and e.aux_material_id is null""")
    ok("该单 1405 分录里 aux_material_id 为空的行数 = 0（不再进 unassigned）",
       null_aux == "0", f"实际 {null_aux}")
    print("    —— 税 / 应付仍各一条 ——")
    for code in ("2221.01", "2202"):
        cnt = scalar(f"""
        select count(*) from biz_voucher_entry e join biz_voucher v on v.id = e.voucher_id
         where v.source_type = 'STOCK_IN' and v.source_id = '{doc_id}'::uuid
           and e.account_code = '{code}'""")
        ok(f"{code} 行数 = 1（按 DOC，不按物料拆）", cnt == "1", f"实际 {cnt}")

    print("\n② 2 物料出库（甲 5 + 乙 5）→ 过账 → 成本结转凭证 4 行")
    status, resp = call("POST", "/business/erp/stock-out", {
        "out_type": "ISSUE", "warehouse_id": WAREHOUSE_ID, "reason": f"{TAG} 2 物料领用",
        "items": [{"material_id": MAT1_ID, "qty": "5"}, {"material_id": MAT2_ID, "qty": "5"}],
    })
    ok("建多物料出库单 200", status == 200, f"{status} {resp}")
    out_id = (resp.get("data") or {}).get("id") if status == 200 else None
    if out_id:
        status, resp = call("PATCH", f"/business/erp/stock-out/{out_id}/status", {"status": "POSTED"})
        ok("出库过账 200", status == 200, f"{status} {resp}")
        print("    —— 该单的成本结转凭证（SQL 原样输出）——")
        out_rows = psql(f"""
        select e.line_no, e.account_code, e.debit_amount, e.credit_amount,
               coalesce(m.material_code, '(空)'), coalesce(e.aux_warehouse_id::text, '(空)')
          from biz_voucher_entry e
          join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
          left join biz_material m on m.id = e.aux_material_id
         where v.source_type = 'STOCK_OUT' and v.source_id = '{out_id}'::uuid and v.is_red = 0
         order by e.line_no""")
        for row in out_rows:
            print(f"    {row}")
        ok("出库凭证 4 行（借 6401×2 + 贷 1405×2 = 2M）", len(out_rows) == 4,
           f"实际 {len(out_rows)} 行")
        dr = [r for r in out_rows if r.split("|")[1] == "6401"]
        cr = [r for r in out_rows if r.split("|")[1] == "1405"]
        ok("借方逐行带物料（甲 / 乙）",
           [r.split("|")[4] for r in dr] == [f"{TAG}-M1", f"{TAG}-M2"],
           str([r.split("|")[4] for r in dr]))
        ok("借方（成本科目）不挂仓库", all(r.split("|")[5] == "(空)" for r in dr),
           str([r.split("|")[5] for r in dr]))
        ok("贷方逐行带物料 + 仓库",
           [r.split("|")[4] for r in cr] == [f"{TAG}-M1", f"{TAG}-M2"]
           and all(r.split("|")[5] == WAREHOUSE_ID for r in cr),
           str([(r.split("|")[4], r.split("|")[5]) for r in cr]))

    print("\n③ 按物料辅助账：丙 的 1405 本期借方 = 该物料成本（30 × 2 = 60）")
    status, resp = call("GET", f"/business/erp/ledger/aux-balance?aux_type=MATERIAL&aux_id={MAT3_ID}")
    ok("按物料查 200", status == 200, f"{status} {resp}")
    if status == 200:
        data = resp.get("data") or {}
        rows = data.get("rows") or []
        print(f"    aux_name = {data.get('aux_name')}   unassigned = {data.get('unassigned')}")
        for row in rows:
            print(f"    {row.get('account_code')} 期初 {row.get('opening_debit')}/"
                  f"{row.get('opening_credit')} 本期 {row.get('period_debit')}/"
                  f"{row.get('period_credit')} 期末 {row.get('closing_debit')}/"
                  f"{row.get('closing_credit')}")
        inv_bal = [r for r in rows if r.get("account_code") == "1405"]
        ok("丙 1405 本期借方 = 60（= 该物料成本）",
           bool(inv_bal) and num(inv_bal[0].get("period_debit")) == Decimal("60"),
           str(inv_bal)[:300])
        ok("丙 1405 期末借方 = 60", bool(inv_bal)
           and num(inv_bal[0].get("closing_debit")) == Decimal("60"), str(inv_bal)[:300])

    return summarize()


def summarize():
    print("\n" + "=" * 92)
    passed = sum(1 for okv, _, _ in RESULTS if okv)
    failed = len(RESULTS) - passed
    print(f"合计：PASS {passed} / FAIL {failed}")
    print("=" * 92)
    if failed:
        for okv, name, detail in RESULTS:
            if not okv:
                print(f"  [FAIL] {name} — {detail}")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
