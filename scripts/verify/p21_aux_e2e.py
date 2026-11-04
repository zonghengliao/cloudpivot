#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
P2-1 凭证辅助核算端到端验收（隔离实例 + 真 HTTP，不碰生产）
==========================================================

要证明的事（每条都给 `[PASS]/[FAIL]` 与原始数字）：

  ① 迁移 V0214 把 `business:erp:ledger:aux-balance` 权限码种进 `sys_menu`；
  ② 单物料收货过账 → 凭证**存货行**有 `aux_material_id` + `aux_warehouse_id`，
     应付行有 `aux_supplier_id`，且**所有行 `aux_dept_id` 为空**（收货单没有部门语义）；
  ③ 多物料收货过账 → 存货借方**按去重物料拆行**（P2-1b）：甲 160 / 乙 150 各一行、
     各带物料、仓库照写、合计 = 单头；并验 `Σ 1405 分录 = 单头金额` 与
     「按物料辅助账 = 该物料成本」；
  ③b 多物料出库过账 → 成本结转凭证 `2M` 行（借 6401×2 / 贷 1405×2），逐行带物料；
  ④ 领用出库过账 → 成本结转凭证的存货行带物料 + 仓库；
  ⑤ 付款过账 → 借方 2202 带**申请人所属部门** `aux_dept_id`，银行存款行不挂部门；
  ⑥ `GET /api/business/erp/ledger/aux-balance` 按物料 / 部门 / 供应商都能查出借贷发生额；
     回参里的 `unassigned` 自证"本期没挂该维度的发生额"，不让人误以为查到的就是全部；
  ⑦ 存量不被回填：造一张"历史凭证"（四列全空），跑完全流程后它仍然全空。

跑法（隔离实例已在跑，见 docs/evidence/20261104-p21-aux-accounting/README.md）：

    python3 scripts/verify/p21_aux_e2e.py --port 18099 --db cloudpivot_p16md

退出码：全 PASS → 0；有任何 FAIL → 1。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from decimal import Decimal

BASE = "http://127.0.0.1:18099"
DB = "cloudpivot_p16md"
DB_USER = "alun"
DB_PASS = "alun"
TOKEN = ""
RESULTS = []

# 夹具（固定 UUID，脚本可重复跑：每次先按前缀清场）
TAG = "P21E2E"
UNIT_ID = "e21a0101-0000-4000-8000-000000000001"
SUPPLIER_ID = "e21a0101-0000-4000-8000-000000000002"
WAREHOUSE_ID = "e21a0101-0000-4000-8000-000000000003"
MAT1_ID = "e21a0101-0000-4000-8000-000000000011"
MAT2_ID = "e21a0101-0000-4000-8000-000000000012"
DEPT_ID = "e21a0101-0000-4000-8000-000000000021"
HIST_VOUCHER_ID = "e21a0101-0000-4000-8000-0000000000f1"
# 副本库里已发布的付款审批流程（本卡只验凭证辅助核算，跑前下线、跑完恢复）
PAY_FLOW_ID = "20000000-0000-4000-8000-000000000717"


# ---------------------------------------------------------------- 基础设施


def psql(sql, db=None):
    env = dict(os.environ)
    env["PGPASSWORD"] = DB_PASS
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-p", "5432", "-U", DB_USER, "-d", db or DB,
         "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(f"psql 失败：{r.stderr.strip()[:400]}\nSQL: {sql[:200]}")
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line != ""]


def scalar(sql, db=None):
    rows = psql(sql, db)
    return rows[0][0] if rows and rows[0] else None


def num(value):
    return Decimal(str(value if value is not None else "0").strip() or "0")


def ok(name, cond, detail=""):
    RESULTS.append((bool(cond), name, detail))
    print(f"  {'[PASS]' if cond else '[FAIL]'} {name}" + (f"  — {detail}" if detail and not cond else ""))


def eq_num(name, expect, actual, detail=""):
    a, e = num(actual), num(expect)
    ok(name, a == e, f"期望 {e} / 实际 {a} {detail}")


def call(method, path, body=None):
    # ⚠ 后端路由宏路径不含 `/api` 前缀（由 [router] prefix 注入），脚本里必须写全
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
        for _ in range(10):
            status, body = call("POST", "/api/system/auth/login",
                                {"username": "admin", "password": password})
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
    if not TOKEN:
        raise RuntimeError(f"登录响应里没有令牌：{body}")
    if data.get("must_change_password"):
        status, body = call("PUT", "/api/system/auth/password",
                            {"old_password": "Admin@123456", "new_password": "Admin@1234567"})
        if status != 200:
            raise RuntimeError(f"首登改密失败：{status} {body}")
        TOKEN = ""
        status, body = call("POST", "/api/system/auth/login",
                            {"username": "admin", "password": "Admin@1234567"})
        if status != 200:
            raise RuntimeError(f"改密后登录失败：{status} {body}")
        TOKEN = (body.get("data") or {}).get("access_token")
    if not TOKEN:
        raise RuntimeError("登录成功但响应里没有 access_token")
    print(f"  登录成功（令牌 {TOKEN[:12]}…）")


# ---------------------------------------------------------------- 夹具


def cleanup():
    """按前缀清场（可重复跑）。"""
    psql(f"""
    delete from biz_status_log where entity_type = 'STOCK_IN' and entity_id in
        (select id from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_status_log where entity_type = 'STOCK_OUT' and entity_id in
        (select id from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_status_log where entity_type = 'PAYABLE' and entity_id in
        (select id from biz_payable where supplier_id = '{SUPPLIER_ID}'::uuid);
    delete from biz_status_log where entity_type = 'VOUCHER' and entity_id not in
        (select id from biz_voucher);
    delete from biz_voucher where source_id in
        (select id from biz_payment where payable_id in
            (select id from biz_payable where supplier_id = '{SUPPLIER_ID}'::uuid));
    delete from biz_voucher where source_type = 'STOCK_OUT' and source_id in
        (select id from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_voucher where source_type = 'STOCK_IN' and source_id in
        (select id from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_voucher where id = '{HIST_VOUCHER_ID}'::uuid;
    delete from biz_payment where payable_id in
        (select id from biz_payable where supplier_id = '{SUPPLIER_ID}'::uuid);
    delete from biz_payable where supplier_id = '{SUPPLIER_ID}'::uuid;
    delete from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_inventory_txn where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_inventory_location where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_inventory where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_material_serial where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_material where id in ('{MAT1_ID}'::uuid, '{MAT2_ID}'::uuid);
    delete from biz_warehouse where id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_supplier where id = '{SUPPLIER_ID}'::uuid;
    delete from biz_unit where id = '{UNIT_ID}'::uuid;
    delete from sys_dept where id = '{DEPT_ID}'::uuid;
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
    values ('{WAREHOUSE_ID}', '{TAG}-WH', '{TAG} 收货仓', '/{WAREHOUSE_ID}/', 1, 1, 1, 0)
    on conflict (id) do nothing;
    insert into biz_material (id, material_code, material_name, unit_id, status, serial_required)
    values ('{MAT1_ID}', '{TAG}-M1', '{TAG} 物料甲', '{UNIT_ID}', 1, false),
           ('{MAT2_ID}', '{TAG}-M2', '{TAG} 物料乙', '{UNIT_ID}', 1, false)
    on conflict (id) do nothing;
    insert into sys_dept (id, dept_code, dept_name, status)
    values ('{DEPT_ID}', '{TAG}D', '{TAG} 采购部', 1)
    on conflict (id) do nothing;
    -- 申请人部门：付款凭证的 aux_dept_id 来自登录人（admin）的 sys_user.dept_id
    update sys_user set dept_id = '{DEPT_ID}'::uuid where username = 'admin';
    """)
    # 本期期间必须 OPEN：单据日期取今天，否则凭证落在"无期间记录"的月份
    today = scalar("select to_char(current_date, 'YYYY-MM-DD')")
    year, month = today.split("-")[0], today.split("-")[1]
    start = f"{year}-{month}-01"
    psql(f"""
    insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
    values ({year}, {int(month)}, '{year}-{month}', date '{start}',
            (date '{start}' + interval '1 month' - interval '1 day')::date, 'OPEN')
    on conflict (period_code) do update set status = 'OPEN';
    """)


def hist_voucher():
    """造一张"历史凭证"（四列辅助核算全空），用来证明存量不会被回填。"""
    acc_dr = scalar("select id from biz_account where account_code = '1405' and deleted_at is null limit 1")
    acc_cr = scalar("select id from biz_account where account_code = '2202' and deleted_at is null limit 1")
    period = scalar("select to_char(current_date, 'YYYY-MM')")
    today = scalar("select to_char(current_date, 'YYYY-MM-DD')")
    psql(f"""
    insert into biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                             source_type, source_no, summary, total_debit, total_credit,
                             entry_count, status, is_red)
    values ('{HIST_VOUCHER_ID}'::uuid, '记', '{TAG}-HIST-0001', '{period}', date '{today}',
            'AUTO', 'MANUAL', '{TAG}-HIST', '{TAG} 历史凭证（辅助核算全空）', 9, 9, 2, 'POSTED', 0)
    on conflict (id) do nothing;
    insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                                   account_name, debit_amount, credit_amount)
    values ('{HIST_VOUCHER_ID}'::uuid, 1, '{TAG} 历史借方', '{acc_dr}'::uuid, '1405', '库存商品', 9, 0),
           ('{HIST_VOUCHER_ID}'::uuid, 2, '{TAG} 历史贷方', '{acc_cr}'::uuid, '2202', '应付账款', 0, 9)
    on conflict do nothing;
    """)


# ---------------------------------------------------------------- 业务动作


def entries_of_voucher_by_source(source_id):
    # ⚠ psql `-A` 会把**最后一行的尾部空字段**省掉（同一个查询里非末行是全的），
    #    所以最后一个 select 项放一个恒非空的守卫列（`e.line_no`），前 8 列才按位可靠。
    rows = psql(f"""
    select e.line_no, e.account_code, e.debit_amount, e.credit_amount,
           coalesce(e.aux_material_id::text, ''), coalesce(e.aux_warehouse_id::text, ''),
           coalesce(e.aux_dept_id::text, ''), coalesce(e.aux_supplier_id::text, ''),
           e.line_no::text
      from biz_voucher_entry e
      join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
     where v.source_id = '{source_id}'::uuid and v.is_red = 0
     order by e.line_no
    """)
    out = []
    for r in rows:
        if len(r) < 9:
            raise RuntimeError(f"分录查询列数不足（{len(r)} < 9），原始行：{r}")
        out.append({
            "line_no": int(r[0]),
            "account_code": r[1],
            "debit": num(r[2]),
            "credit": num(r[3]),
            "aux_material_id": r[4],
            "aux_warehouse_id": r[5],
            "aux_dept_id": r[6],
            "aux_supplier_id": r[7],
        })
    return out


def create_and_post_stock_in(items, remark):
    body = {
        "warehouse_id": WAREHOUSE_ID,
        "is_sourceless": True,
        "supplier_id": SUPPLIER_ID,
        "remark": remark,
        "items": items,
    }
    status, resp = call("POST", "/business/erp/stock-in", body)
    if status != 200:
        return None, f"建单 {status} {resp}"
    in_id = (resp.get("data") or {}).get("id") or (((resp.get("data") or {}).get("header") or {}).get("id"))
    if not in_id:
        return None, f"建单没回 id：{str(resp)[:200]}"
    status, resp = call("PATCH", f"/business/erp/stock-in/{in_id}/status", {"status": "POSTED"})
    if status != 200:
        return None, f"过账 {status} {resp}"
    return in_id, ""


def aux_balance(aux_type, aux_id=None):
    path = f"/business/erp/ledger/aux-balance?aux_type={aux_type}"
    if aux_id:
        path += f"&aux_id={aux_id}"
    return call("GET", path)


# ---------------------------------------------------------------- 主流程


def main():
    global BASE, DB
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18099)
    ap.add_argument("--db", default=DB)
    args = ap.parse_args()
    BASE = f"http://127.0.0.1:{args.port}"
    DB = args.db

    print("=" * 88)
    print(f"P2-1 凭证辅助核算端到端验收   库={DB}   接口={BASE}")
    print("=" * 88)

    print("\n① 准备：清场 + 夹具 + 迁移自检 + 历史凭证")
    cleanup()
    fixtures()
    login()
    ok("迁移 V0214：辅助账权限码已种",
       scalar("select count(*) from sys_menu where auth_code = 'business:erp:ledger:aux-balance'"
              " and deleted_at is null") == "1",
       "sys_menu 里应恰好 1 条 business:erp:ledger:aux-balance")
    hist_voucher()
    ok("历史凭证（辅助核算全空）已就位",
       scalar(f"select count(*) from biz_voucher_entry where voucher_id = '{HIST_VOUCHER_ID}'::uuid"
              " and aux_material_id is null and aux_warehouse_id is null"
              " and aux_dept_id is null and aux_supplier_id is null") == "2")

    print("\n② 单物料收货 100 @ 8（含税 13%）→ 过账 → 查凭证分录")
    in_id, err = create_and_post_stock_in(
        [{"material_id": MAT1_ID, "qty": "100", "price_excl": "8", "tax_rate": "13"}],
        f"{TAG} 单物料收货")
    ok("单物料收货过账成功", bool(in_id), err)
    if in_id:
        rows = entries_of_voucher_by_source(in_id)
        ok("凭证分录行数 ≥ 3", len(rows) >= 3, f"实际 {len(rows)}")
        inv = [r for r in rows if r["account_code"] == "1405"]
        ok("存货行存在", bool(inv), str(rows))
        if inv:
            ok("存货行 aux_material_id = 物料甲", inv[0]["aux_material_id"] == MAT1_ID,
               f"实际 {inv[0]['aux_material_id']}")
            ok("存货行 aux_warehouse_id = 收货仓", inv[0]["aux_warehouse_id"] == WAREHOUSE_ID,
               f"实际 {inv[0]['aux_warehouse_id']}")
            eq_num("存货行借方 = 800（不含税）", "800", inv[0]["debit"])
        pay = [r for r in rows if r["account_code"] == "2202"]
        ok("应付行 aux_supplier_id = 供应商", bool(pay) and pay[0]["aux_supplier_id"] == SUPPLIER_ID,
           str(pay))
        ok("**所有行** aux_dept_id 为空（收货单没有部门语义）",
           all(r["aux_dept_id"] == "" for r in rows),
           str([(r["line_no"], r["aux_dept_id"]) for r in rows]))

    print("\n③ 多物料收货（甲 20 @ 8 + 乙 30 @ 5，含税 13%）→ 存货借方按物料拆 2 条（P2-1b）")
    multi_id, err = create_and_post_stock_in(
        [{"material_id": MAT1_ID, "qty": "20", "price_excl": "8", "tax_rate": "13"},
         {"material_id": MAT2_ID, "qty": "30", "price_excl": "5", "tax_rate": "13"}],
        f"{TAG} 多物料收货")
    ok("多物料收货过账成功", bool(multi_id), err)
    if multi_id:
        rows = entries_of_voucher_by_source(multi_id)
        inv = [r for r in rows if r["account_code"] == "1405"]
        header_amount = num(scalar(
            f"select total_amount from biz_stock_in where id = '{multi_id}'::uuid"))
        ok("多物料：凭证 4 行（1405×2 + 进项税×1 + 应付×1 = M + 2）", len(rows) == 4,
           f"实际 {len(rows)} 行：{[(r['line_no'], r['account_code'], str(r['debit']), str(r['credit'])) for r in rows]}")
        ok("多物料：存货行 2 条（每个去重物料一行）", len(inv) == 2, f"实际 {len(inv)} 条")
        if len(inv) == 2:
            ok("存货行 1 = 物料甲（按 material_id 升序）", inv[0]["aux_material_id"] == MAT1_ID,
               f"实际 {inv[0]['aux_material_id']}")
            eq_num("存货行 1 借方 = 160（甲 20 × 8）", "160", inv[0]["debit"])
            ok("存货行 2 = 物料乙", inv[1]["aux_material_id"] == MAT2_ID,
               f"实际 {inv[1]['aux_material_id']}")
            eq_num("存货行 2 借方 = 150（乙 30 × 5）", "150", inv[1]["debit"])
            ok("两条存货行都挂收货仓（仓库照写）",
               all(r["aux_warehouse_id"] == WAREHOUSE_ID for r in inv),
               str([r["aux_warehouse_id"] for r in inv]))
            eq_num("Σ 1405 分录 = 单头 total_amount（310）", str(header_amount),
                   sum((r["debit"] for r in inv), Decimal("0")))
        ok("多物料：进项税 / 应付仍各一条（按 DOC，不按物料拆）",
           len([r for r in rows if r["account_code"] == "2221.01"]) == 1
           and len([r for r in rows if r["account_code"] == "2202"]) == 1,
           str([r["account_code"] for r in rows]))

        # 按物料辅助账 = 该物料成本（P2-1b 的目的：MATERIAL 维度能按物料对账）
        status, resp = aux_balance("MATERIAL", MAT2_ID)
        bal_rows = ((resp.get("data") or {}).get("rows") or []) if status == 200 else []
        b_inv = [r for r in bal_rows if r["account_code"] == "1405"]
        ok("按物料辅助账：乙 1405 本期借 150 / 期末 150（= 该物料成本）",
           bool(b_inv) and num(b_inv[0]["period_debit"]) == Decimal("150")
           and num(b_inv[0]["closing_debit"]) == Decimal("150"),
           f"{status} {str(b_inv)[:300]}")

    print("\n④ 领用出库（甲 10）→ 过账 → 成本结转凭证带物料 + 仓库")
    status, resp = call("POST", "/business/erp/stock-out", {
        "out_type": "ISSUE", "warehouse_id": WAREHOUSE_ID, "reason": f"{TAG} 车间领用",
        "items": [{"material_id": MAT1_ID, "qty": "10"}],
    })
    ok("建领用单 200", status == 200, f"{status} {resp}")
    out_id = (resp.get("data") or {}).get("id") if status == 200 else None
    if out_id:
        status, resp = call("PATCH", f"/business/erp/stock-out/{out_id}/status", {"status": "POSTED"})
        ok("领用过账 200", status == 200, f"{status} {resp}")
        rows = entries_of_voucher_by_source(out_id)
        inv = [r for r in rows if r["account_code"] == "1405"]
        ok("出库凭证存货行带物料", bool(inv) and inv[0]["aux_material_id"] == MAT1_ID, str(inv))
        ok("出库凭证存货行带仓库", bool(inv) and inv[0]["aux_warehouse_id"] == WAREHOUSE_ID, str(inv))

    print("\n③b 多物料出库（甲 10 + 乙 10）→ 成本结转凭证 2M = 4 行（借 6401×2 / 贷 1405×2）")
    status, resp = call("POST", "/business/erp/stock-out", {
        "out_type": "ISSUE", "warehouse_id": WAREHOUSE_ID, "reason": f"{TAG} 多物料领用",
        "items": [{"material_id": MAT1_ID, "qty": "10"}, {"material_id": MAT2_ID, "qty": "10"}],
    })
    ok("建多物料领用单 200", status == 200, f"{status} {resp}")
    multi_out_id = (resp.get("data") or {}).get("id") if status == 200 else None
    if multi_out_id:
        status, resp = call("PATCH", f"/business/erp/stock-out/{multi_out_id}/status",
                            {"status": "POSTED"})
        ok("多物料领用过账 200", status == 200, f"{status} {resp}")
        rows = entries_of_voucher_by_source(multi_out_id)
        dr = [r for r in rows if r["account_code"] == "6401"]
        cr = [r for r in rows if r["account_code"] == "1405"]
        ok("多物料出库：凭证 4 行（借 6401×2 + 贷 1405×2 = 2M）", len(rows) == 4,
           f"实际 {len(rows)} 行：{[(r['line_no'], r['account_code'], str(r['debit']), str(r['credit'])) for r in rows]}")
        ok("借方两条各带物料（甲 / 乙，升序）",
           [r["aux_material_id"] for r in dr] == [MAT1_ID, MAT2_ID],
           str([r["aux_material_id"] for r in dr]))
        ok("借方（成本科目）不挂仓库", all(r["aux_warehouse_id"] == "" for r in dr),
           str([r["aux_warehouse_id"] for r in dr]))
        ok("贷方两条各带物料 + 仓库",
           [r["aux_material_id"] for r in cr] == [MAT1_ID, MAT2_ID]
           and all(r["aux_warehouse_id"] == WAREHOUSE_ID for r in cr),
           str([(r["aux_material_id"], r["aux_warehouse_id"]) for r in cr]))
        eq_num("借方合计 = 贷方合计 = 130（甲 10×8 + 乙 10×5）", "130",
               sum((r["debit"] for r in dr), Decimal("0")))

    print("\n⑤ 付款（应付单 → 付款）→ 借方 2202 带申请人部门，银行行不挂")
    # 付款审批：这个副本库里 `payable_pay` 是发布状态 ⇒ 引擎要求走审批（DC-01 的护栏）。
    # 本卡验的是**付款凭证的辅助核算**，不是审批链（审批链已有 pflow3b 用例覆盖），
    # 所以按引擎报错的指引「让管理员下线该流程」→ 显式 skip → 跑完再原样发布回去。
    status, resp = call("POST", f"/business/flow/def/{PAY_FLOW_ID}/offline")
    ok("临时下线 payable_pay 流程（跑完恢复）", status == 200, f"{status} {resp}")
    payable_id = scalar(f"select id from biz_payable where supplier_id = '{SUPPLIER_ID}'::uuid"
                        " and deleted_at is null order by created_at limit 1")
    ok("收货生成的应付单存在", bool(payable_id), "收货过账应生成应付单")
    if payable_id:
        status, resp = call("POST", f"/business/erp/payable/{payable_id}/pay", {
            "amount": "100", "pay_method": "银行转账", "remark": f"{TAG} 付款",
            "skip_approval": True, "skip_reason": f"{TAG} 验收：未配置付款审批流",
        })
        ok("付款 200", status == 200, f"{status} {resp}")
        if status == 200:
            payment_id = scalar(f"select id from biz_payment where payable_id = '{payable_id}'::uuid"
                                " order by created_at desc limit 1")
            rows = entries_of_voucher_by_source(payment_id)
            dr = [r for r in rows if r["account_code"] == "2202"]
            cr = [r for r in rows if r["account_code"] == "1002"]
            ok("付款凭证有借方 2202 / 贷方 1002", bool(dr) and bool(cr), str(rows))
            ok("借方 2202 的 aux_dept_id = 申请人部门",
               bool(dr) and dr[0]["aux_dept_id"] == DEPT_ID, str(dr))
            ok("贷方 1002 不挂部门", bool(cr) and cr[0]["aux_dept_id"] == "", str(cr))
            eq_num("付款金额 100", "100", dr[0]["debit"] if dr else "0")

    status, resp = call("POST", f"/business/flow/def/{PAY_FLOW_ID}/enable")
    ok("恢复 payable_pay 流程为发布状态", status == 200, f"{status} {resp}")

    print("\n⑥ 辅助账接口 GET /business/erp/ledger/aux-balance")
    status, resp = call("GET", f"/business/erp/ledger/aux-balance?aux_type=MATERIAL&aux_id={MAT1_ID}")
    ok("按物料查 200", status == 200, f"{status} {resp}")
    if status == 200:
        data = resp.get("data") or {}
        rows = data.get("rows") or []
        ok("物料辅助账有行", bool(rows), str(data)[:300])
        if rows:
            inv_row = [r for r in rows if r["account_code"] == "1405"]
            ok("物料甲的 1405 行存在", bool(inv_row), str(rows)[:300])
            if inv_row:
                # 借方 = 单物料收货 800 + 多物料收货里的甲 160 = 960
                # 贷方 = 领用 10×8 = 80 + 多物料领用里的甲 10×8 = 80 → 160
                eq_num("物料甲 1405 本期借方 = 960", "960", inv_row[0]["period_debit"])
                eq_num("物料甲 1405 本期贷方 = 160", "160", inv_row[0]["period_credit"])
                eq_num("物料甲 1405 期末余额 = 800", "800", inv_row[0]["closing_debit"])
        ok("回参带 aux_name（物料名称）", (data.get("aux_name") or "") == f"{TAG} 物料甲",
           str(data.get("aux_name")))
        unassigned = data.get("unassigned") or {}
        ok("回参带 unassigned（本期没挂物料的借方 > 0）",
           num(unassigned.get("period_debit")) > 0, str(unassigned))

    status, resp = call("GET", f"/business/erp/ledger/aux-balance?aux_type=DEPT&aux_id={DEPT_ID}")
    ok("按部门查 200", status == 200, f"{status} {resp}")
    if status == 200:
        data = resp.get("data") or {}
        rows = data.get("rows") or []
        ok("部门辅助账有行（付款借 2202）", bool(rows), str(data)[:300])
        if rows:
            eq_num("采购部本期借方 = 100（付款）", "100", rows[0]["period_debit"])
        ok("部门名称回显", (data.get("aux_name") or "") == f"{TAG} 采购部", str(data.get("aux_name")))

    status, resp = call("GET", f"/business/erp/ledger/aux-balance?aux_type=SUPPLIER&aux_id={SUPPLIER_ID}")
    ok("按供应商查 200", status == 200, f"{status} {resp}")
    if status == 200:
        data = resp.get("data") or {}
        rows = data.get("rows") or []
        credits = sum(num(r["period_credit"]) for r in rows)
        ok("供应商贷方发生额 > 0（两张收货的应付）", credits > 0, f"实际 {credits}")

    status, resp = call("GET", "/business/erp/ledger/aux-balance?aux_type=PROJECT")
    ok("非法 aux_type → 400", status == 400, f"{status} {resp}")

    print("\n⑦ 存量不回填：那张历史凭证的四个辅助列仍然全空")
    ok("历史凭证未被改动",
       scalar(f"select count(*) from biz_voucher_entry where voucher_id = '{HIST_VOUCHER_ID}'::uuid"
              " and aux_material_id is null and aux_warehouse_id is null"
              " and aux_dept_id is null and aux_supplier_id is null") == "2")

    print("\n" + "=" * 88)
    passed = sum(1 for okv, _, _ in RESULTS if okv)
    failed = len(RESULTS) - passed
    print(f"合计：PASS {passed} / FAIL {failed}")
    print("=" * 88)
    if failed:
        for okv, name, detail in RESULTS:
            if not okv:
                print(f"  [FAIL] {name} — {detail}")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
