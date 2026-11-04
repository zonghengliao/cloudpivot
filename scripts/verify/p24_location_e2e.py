#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
【P2-4】库位做完整 —— 真机端到端验收（隔离库 + 真 HTTP，不碰生产）
==================================================================

逐项跑卡面 §四 的「真机」验收：

    A. 建一张收货单**指定库位 A** → 过账 → 库位账写在 A 上，「未分配」桶一分没有
    B. 仓库设了默认库位 → 收货**不指定** → 自动落到默认库位（表单回读也能看到）
    C. 仓库**没有默认库位** → 收货不指定 → 进「未分配」桶，且详情给出提示
    D. 跨仓库的库位 → 建单被拒（400，点名两个仓库）
    E. 出库**不指定库位** → FIFO 从最早建账的库位扣；**指定库位** → 只扣那一桶

最后跑 `scripts/verify/erp_invariants.py`（BLOCKER 0 / WARN 0，32 条全绿）。

用法（隔离库，示例）：
    # 1) 建库 + 起实例（迁移自动跑到 V0218）
    D1_DB=cloudpivot_p24 bash scripts/verify/d1_dbsetup.sh
    D1_DB=cloudpivot_p24 bash scripts/verify/d1_migrate.sh 18096

    # 2) 起一个干净的隔离实例（起完脚本自己收）
    bash scripts/verify/p24_location_e2e_up.sh
    python3 scripts/verify/p24_location_e2e.py --db cloudpivot_p24 --port 18096

退出码：全 PASS → 0；任何 FAIL → 1。
"""
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from decimal import Decimal

BASE = "http://127.0.0.1:18096"
DB = "cloudpivot_p24"
TOKEN = ""
RESULTS = []

# ---------- 夹具（固定 UUID，可重复跑：每次先按前缀清场） ----------
TAG = "P24E2E"
UNIT_ID = "e2040000-0000-4000-8000-000000000001"
SUPPLIER_ID = "e2040000-0000-4000-8000-000000000002"
WAREHOUSE_ID = "e2040000-0000-4000-8000-000000000003"
WAREHOUSE_B_ID = "e2040000-0000-4000-8000-000000000004"
MATERIAL_ID = "e2040000-0000-4000-8000-000000000011"
LOC_A = "e2040000-0000-4000-8000-000000000021"        # 指定库位用例
LOC_B = "e2040000-0000-4000-8000-000000000022"        # 第二个库位（FIFO / 指定出库）
LOC_DEFAULT = "e2040000-0000-4000-8000-000000000023"  # 默认库位用例
LOC_WH_B = "e2040000-0000-4000-8000-000000000024"     # 另一个仓库的库位（跨仓被拒）


def psql(sql, db=None):
    env = dict(os.environ)
    env["PGPASSWORD"] = "alun"
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-p", "5432", "-U", "alun", "-d", db or DB, "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:400])
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line != ""]


def scalar(sql):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] else None


def num(value):
    return Decimal(str(value if value is not None else "0").strip() or "0")


def ok(name, cond, detail=""):
    RESULTS.append((name, bool(cond), detail))
    print(f"  {'PASS' if cond else 'FAIL'}  {name}{('  —— ' + detail) if detail and not cond else ''}")
    return cond


def eq_num(name, expect, actual, detail=""):
    return ok(name, num(expect) == num(actual), f"期望 {expect} 实际 {actual} {detail}".strip())


def call(method, path, body=None):
    if not path.startswith("/api/"):
        path = "/api" + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if TOKEN:
        req.add_header("Authorization", f"Bearer {TOKEN}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
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
        for _ in range(10):
            status, body = call("POST", "/api/system/auth/login", {"username": "admin", "password": password})
            if status == 429:
                time.sleep(12)
                continue
            return status, body
        return 429, {"msg": "登录限流窗口过长"}

    status, body = attempt("Admin@123456")
    if status != 200:
        status, body = attempt("Admin@1234567")
    if status != 200:
        raise RuntimeError(f"登录失败：{status} {body}")
    data = body.get("data") or {}
    TOKEN = data.get("access_token") or data.get("token")
    if data.get("must_change_password"):
        call("PUT", "/api/system/auth/password",
             {"old_password": "Admin@123456", "new_password": "Admin@1234567"})
        TOKEN = ""
        status, body = call("POST", "/api/system/auth/login",
                            {"username": "admin", "password": "Admin@1234567"})
        TOKEN = (body.get("data") or {}).get("access_token")
    if not TOKEN:
        raise RuntimeError("登录成功但响应里没有 access_token")
    print(f"  登录成功（令牌 {TOKEN[:12]}…）")


def cleanup():
    psql(f"""
    delete from biz_status_log where entity_type = 'STOCK_OUT' and entity_id in
        (select id from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_status_log where entity_type = 'STOCK_IN' and entity_id in
        (select id from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_voucher where source_id in
        (select id from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_voucher where source_id in
        (select id from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_status_log where entity_type = 'VOUCHER' and entity_id not in (select id from biz_voucher);
    delete from biz_payable where supplier_id = '{SUPPLIER_ID}'::uuid;
    delete from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    -- 库存 / 库位账 / 流水（只清本夹具物料）
    -- ⚠ 顺序：流水 → 成本账 → 库位账（`biz_inventory.location_id` 有 FK 指向库位账行）
    delete from biz_inventory_txn where material_id = '{MATERIAL_ID}'::uuid;
    delete from biz_inventory where material_id = '{MATERIAL_ID}'::uuid;
    delete from biz_inventory_location where material_id = '{MATERIAL_ID}'::uuid;
    delete from biz_location where warehouse_id in ('{WAREHOUSE_ID}'::uuid, '{WAREHOUSE_B_ID}'::uuid);
    delete from biz_material where id = '{MATERIAL_ID}'::uuid;
    """)


def fixtures():
    psql(f"""
    insert into biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
    values ('{UNIT_ID}', '{TAG}-PCS', '个', 1, null, 1.000000, 1, 1)
    on conflict (id) do nothing;
    insert into biz_supplier (id, supplier_code, supplier_name, status, credit_days)
    values ('{SUPPLIER_ID}', '{TAG}-SUP', '{TAG} 供应商', 1, 30)
    on conflict (id) do nothing;
    insert into biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf,
                               status, allow_negative)
    values ('{WAREHOUSE_ID}', '{TAG}-WH', '{TAG} 收货仓', '/{WAREHOUSE_ID}/', 1, 1, 1, 0),
           ('{WAREHOUSE_B_ID}', '{TAG}-WH2', '{TAG} 二号仓', '/{WAREHOUSE_B_ID}/', 1, 1, 1, 0)
    on conflict (id) do nothing;
    insert into biz_location (id, location_code, location_name, warehouse_id, location_type,
                              is_default, capacity, status, sort_order)
    values ('{LOC_A}',       'P24-A-01', 'A 区一号',   '{WAREHOUSE_ID}'::uuid, 'STORAGE', 0, null, 1, 1),
           ('{LOC_B}',       'P24-B-01', 'B 区一号',   '{WAREHOUSE_ID}'::uuid, 'STORAGE', 0, null, 1, 2),
           ('{LOC_DEFAULT}', 'P24-D-01', '默认收货位', '{WAREHOUSE_ID}'::uuid, 'STORAGE', 0, null, 1, 3),
           ('{LOC_WH_B}',    'P24-W2-01', '二号仓一号', '{WAREHOUSE_B_ID}'::uuid, 'STORAGE', 0, null, 1, 1)
    on conflict (id) do nothing;
    insert into biz_material (id, material_code, material_name, unit_id, status, serial_required)
    values ('{MATERIAL_ID}', '{TAG}-M1', '{TAG} 螺丝', '{UNIT_ID}', 1, false)
    on conflict (id) do nothing;
    -- 本期会计期间必须 OPEN，否则巡检 F2（凭证期间必有期间记录）会 WARN
    insert into biz_accounting_period (id, period_year, period_month, period_code,
                                       start_date, end_date, status)
    select gen_random_uuid(),
           extract(year from current_date)::int,
           extract(month from current_date)::int,
           to_char(current_date, 'YYYY-MM'),
           date_trunc('month', current_date)::date,
           (date_trunc('month', current_date) + interval '1 month - 1 day')::date,
           'OPEN'
    where not exists (select 1 from biz_accounting_period
                       where period_code = to_char(current_date, 'YYYY-MM'));
    """)


# ---------- 账务读数 ----------

def loc_qty(location_id):
    return num(scalar(
        f"select coalesce(sum(qty),0) from biz_inventory_location "
        f"where material_id = '{MATERIAL_ID}'::uuid and warehouse_id = '{WAREHOUSE_ID}'::uuid "
        f"and location_id = '{location_id}'::uuid"))


def unassigned_qty():
    return num(scalar(
        f"select coalesce(sum(qty),0) from biz_inventory_location "
        f"where material_id = '{MATERIAL_ID}'::uuid and warehouse_id = '{WAREHOUSE_ID}'::uuid "
        f"and location_id is null"))


def unassigned_rows():
    return int(scalar(
        f"select count(*) from biz_inventory_location "
        f"where material_id = '{MATERIAL_ID}'::uuid and warehouse_id = '{WAREHOUSE_ID}'::uuid "
        f"and location_id is null") or 0)


def ledger_row(location_id):
    return scalar(
        f"select id from biz_inventory_location "
        f"where material_id = '{MATERIAL_ID}'::uuid and warehouse_id = '{WAREHOUSE_ID}'::uuid "
        f"and location_id = '{location_id}'::uuid")


# ---------- 业务动作 ----------

def create_stock_in(items, warehouse_id=None):
    body = {
        "supplier_id": SUPPLIER_ID,
        "is_sourceless": True,
        "warehouse_id": warehouse_id or WAREHOUSE_ID,
        "invoice_received": True,
        "remark": f"{TAG} 无源收货（P2-4 库位验收）",
        "items": items,
    }
    return call("POST", "/business/erp/stock-in", body)


def line(qty, location_dict_id=None, price="2"):
    item = {"material_id": MATERIAL_ID, "qty": qty, "price_excl": price, "tax_rate": 0}
    if location_dict_id:
        item["location_dict_id"] = location_dict_id
    return item


def post_stock_in(stock_in_id):
    return call("POST", f"/business/erp/stock-in/{stock_in_id}/submit-post", {})


def create_stock_out(qty, location_row=None):
    item = {"material_id": MATERIAL_ID, "qty": qty}
    if location_row:
        item["location_id"] = location_row
    body = {"out_type": "ISSUE", "warehouse_id": WAREHOUSE_ID,
            "reason": f"{TAG} 领用（P2-4 出库库位验收）", "items": [item]}
    return call("POST", "/business/erp/stock-out", body)


def post_stock_out(out_id):
    return call("PATCH", f"/business/erp/stock-out/{out_id}/status", {"status": "POSTED"})


def main():
    global BASE, DB, TOKEN
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default="cloudpivot_p24")
    ap.add_argument("--port", type=int, default=18096)
    ap.add_argument("--skip-clean", action="store_true")
    args = ap.parse_args()
    BASE = f"http://127.0.0.1:{args.port}"
    DB = args.db
    TOKEN = ""

    print(f"P2-4 库位做完整 · 真机端到端   库={DB}   接口={BASE}")
    login()
    if not args.skip_clean:
        cleanup()
    fixtures()
    psql(f"update biz_location set is_default = 0 where warehouse_id = '{WAREHOUSE_ID}'::uuid;")

    # ==================== A. 指定库位 A → 过账 → A 有量、未分配没变 ====================
    print("\n[A] 收货单指定库位 A")
    status, body = create_stock_in([line("200", LOC_A)])
    ok("A1 建单成功（指定库位）", status == 200, f"{status} {body}")
    doc = (body.get("data") or {}) if status == 200 else {}
    doc_id = (doc.get("header") or {}).get("id")
    item0 = (doc.get("items") or [{}])[0]
    ok("A2 明细记住 location_dict_id = A", item0.get("location_dict_id") == LOC_A, str(item0))
    ok("A3 详情译出库位编码 P24-A-01", item0.get("location_code") == "P24-A-01", str(item0.get("location_code")))
    ok("A4 草稿态不建库位账行（0 数量空行不落库）", ledger_row(LOC_A) is None,
       f"ledger_row={ledger_row(LOC_A)}")

    status, body = post_stock_in(doc_id) if doc_id else (500, {})
    ok("A5 过账成功", status == 200, f"{status} {body}")
    eq_num("A6 库位 A 的库位账 = 200", "200", loc_qty(LOC_A))
    eq_num("A7 「未分配」桶数量 = 0（没被创建/没被写）", "0", unassigned_qty())
    ok("A8 「未分配」桶行数 = 0", unassigned_rows() == 0, f"rows={unassigned_rows()}")
    row_a = ledger_row(LOC_A)
    ok("A9 明细回写了库位账行 id", row_a is not None, f"row_a={row_a}")
    status, body = call("GET", f"/business/erp/stock-in/{doc_id}")
    posted_item = ((body.get("data") or {}).get("items") or [{}])[0] if status == 200 else {}
    ok("A10 过账后详情仍显示库位编码", posted_item.get("location_code") == "P24-A-01", str(posted_item))

    # ==================== B. 不指定 → 自动用默认库位 ====================
    print("\n[B] 仓库设了默认库位 → 不指定库位")
    psql(f"update biz_location set is_default = 1 where id = '{LOC_DEFAULT}'::uuid;")
    status, body = create_stock_in([line("100")])
    ok("B1 建单成功（不指定库位）", status == 200, f"{status} {body}")
    doc = (body.get("data") or {}) if status == 200 else {}
    doc_id = (doc.get("header") or {}).get("id")
    item0 = (doc.get("items") or [{}])[0]
    ok("B2 建单回读：自动带出默认库位", item0.get("location_dict_id") == LOC_DEFAULT, str(item0))
    status, body = post_stock_in(doc_id) if doc_id else (500, {})
    ok("B3 过账成功", status == 200, f"{status} {body}")
    eq_num("B4 默认库位收到 100", "100", loc_qty(LOC_DEFAULT))
    eq_num("B5 「未分配」桶仍然 0", "0", unassigned_qty())

    # ==================== C. 没有默认库位 → 未分配桶 + 提示 ====================
    print("\n[C] 仓库没有默认库位 → 不指定库位")
    psql(f"update biz_location set is_default = 0 where id = '{LOC_DEFAULT}'::uuid;")
    status, body = create_stock_in([line("50")])
    ok("C1 建单成功（无默认库位也放行）", status == 200, f"{status} {body}")
    doc = (body.get("data") or {}) if status == 200 else {}
    doc_id = (doc.get("header") or {}).get("id")
    warnings = doc.get("location_warnings") or []
    ok("C2 详情给出「未分配」提示", any("未分配" in w for w in warnings), str(warnings))
    status, body = post_stock_in(doc_id) if doc_id else (500, {})
    ok("C3 过账成功", status == 200, f"{status} {body}")
    eq_num("C4 数量进「未分配」桶 = 50", "50", unassigned_qty())
    ok("C5 「未分配」桶只有一行", unassigned_rows() == 1, f"rows={unassigned_rows()}")

    # ==================== D. 跨仓库库位被拒 ====================
    print("\n[D] 跨仓库库位")
    status, body = create_stock_in([line("10", LOC_WH_B)])
    msg = (body.get("msg") or "") if isinstance(body, dict) else str(body)
    ok("D1 跨仓库位建单被拒（400）", status == 400, f"{status} {msg}")
    ok("D2 文案点名仓库不一致", "不一致" in msg, msg)

    # ==================== E. 出库：FIFO / 指定 ====================
    print("\n[E] 出库库位扣减")
    # 先把 B 库位也入一批（比 A 晚建账）
    status, body = create_stock_in([line("100", LOC_B)])
    doc = (body.get("data") or {}) if status == 200 else {}
    if (doc.get("header") or {}).get("id"):
        post_stock_in(doc["header"]["id"])
    psql(f"update biz_inventory_location set created_at = current_timestamp - interval '2 hour' "
         f"where id = '{row_a}'::uuid")
    eq_num("E1 前置：A=200 / B=100", "300", loc_qty(LOC_A) + loc_qty(LOC_B))

    status, body = create_stock_out("50")
    ok("E2 出库单建单成功（不指定库位）", status == 200, f"{status} {body}")
    out = (body.get("data") or {}) if status == 200 else {}
    # 出库单详情是**扁平**结构（`data.id`），收货单才是 `data.header.id`
    out_id = out.get("id") or (out.get("header") or {}).get("id")
    status, body = post_stock_out(out_id) if out_id else (500, {})
    ok("E3 出库过账成功（FIFO 选位）", status == 200, f"{status} {body}")
    eq_num("E4 FIFO：最早建账的 A 被扣到 150", "150", loc_qty(LOC_A))
    eq_num("E5 FIFO：后排建账的 B 不动 = 100", "100", loc_qty(LOC_B))
    item_loc = scalar(f"select location_id from biz_stock_out_item where stock_out_id = '{out_id}'::uuid") if out_id else None
    ok("E6 明细回写 FIFO 选中的库位账行", item_loc == row_a, f"item.loc={item_loc} row_a={row_a}")

    status, body = create_stock_out("40", ledger_row(LOC_B))
    out = (body.get("data") or {}) if status == 200 else {}
    out_id = out.get("id") or (out.get("header") or {}).get("id")
    status, body = post_stock_out(out_id) if out_id else (500, {})
    ok("E7 出库过账成功（指定库位 B）", status == 200, f"{status} {body}")
    eq_num("E8 指定库位：B 扣到 60", "60", loc_qty(LOC_B))
    eq_num("E9 指定库位：A 仍是 150", "150", loc_qty(LOC_A))

    # ==================== 汇总 + 不变量 ====================
    passed = sum(1 for _, cond, _ in RESULTS if cond)
    total = len(RESULTS)
    print(f"\n==== P2-4 真机验收：{passed}/{total} PASS ====")

    print("\n---- erp_invariants.py（BLOCKER 0 / WARN 0 才算过）----")
    env = dict(os.environ)
    env.update({
        "NIK_DATABASE_HOST": "127.0.0.1", "NIK_DATABASE_PORT": "5432",
        "NIK_DATABASE_USER": "alun", "NIK_DATABASE_PASSWORD": "alun", "NIK_DATABASE_NAME": DB,
    })
    inv = subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), "erp_invariants.py")],
                         capture_output=True, text=True, env=env, timeout=600)
    tail = "\n".join(inv.stdout.strip().split("\n")[-14:])
    print(tail)
    ok("F1 不变量巡检 BLOCKER = 0", "BLOCKER: 0" in inv.stdout or "BLOCKER 0" in inv.stdout,
       f"exit={inv.returncode}")
    ok("F2 不变量巡检 WARN = 0", "WARN: 0" in inv.stdout or "WARN 0" in inv.stdout,
       f"exit={inv.returncode}")

    passed = sum(1 for _, cond, _ in RESULTS if cond)
    total = len(RESULTS)
    print(f"\n==== 最终：{passed}/{total} PASS ====")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
