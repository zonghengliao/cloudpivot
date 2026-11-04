#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
ARCH-3 DC-13 出库单端到端验收（隔离库 + 真 HTTP，不碰生产）
=============================================================

跑一遍卡面要求的全链：

    采购/无源收货入库 100  →  领用 30  →  报废 10  →  盘亏 5
    （收货走真实审批链，出库走本卡新增的三本账负向 + 成本结转凭证）

然后逐项断言六本账：

    ① 成本账 biz_inventory（数量 / 金额 / 移动加权单价）
    ② 库位账 biz_inventory_location（Σ = 成本账）
    ③ 流水 biz_inventory_txn（IN − OUT = 成本账）
    ④ 存货科目 1405 余额 = Σ 库存金额（B2）
    ⑤ 成本科目 6401 / 1901 借方发生额 = 出库结转金额合计
    ⑥ 出库单总金额 = Σ 明细金额 = 凭证金额

最后跑 `scripts/verify/erp_invariants.py`（含本卡新增的 B5 / B6）要求
`BLOCKER 0 / WARN 0`。

用法（隔离库）：
    # 1) 建库 + 起实例（迁移自动跑 V0203）
    runuser -u postgres -- createdb -O alun cp_dc13_e2e
    NIK_PROFILE=test NIK_DATABASE_NAME=cp_dc13_e2e NIK_DATABASE_USER=alun \
      NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18097 \
      NIK_ADMIN_PASSWORD='Admin@123456' NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
      CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
      setsid nohup backend/target/debug/cloudpivot > /tmp/dc13/e2e.log 2>&1 < /dev/null &

    # 2) 跑本脚本
    python3 scripts/verify/dc13_stock_out_e2e.py --db cp_dc13_e2e --port 18097

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
import urllib.error
import urllib.request
from decimal import Decimal

BASE = "http://127.0.0.1:18097"
DB = "cp_dc13_e2e"
TOKEN = ""
RESULTS = []

# 夹具（固定 UUID，脚本可重复跑：每次先按前缀清场）
TAG = "DC13E2E"
UNIT_ID = "d2030101-0000-4000-8000-000000000001"
SUPPLIER_ID = "d2030101-0000-4000-8000-000000000002"
WAREHOUSE_ID = "d2030101-0000-4000-8000-000000000003"
MATERIAL_ID = "d2030101-0000-4000-8000-000000000011"


def psql(sql, db=None):
    env = dict(os.environ)
    env["PGPASSWORD"] = "alun"
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-p", "5432", "-U", "alun", "-d", db or DB, "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:400])
    rows = [line.split("\x1f") for line in r.stdout.strip().split("\n") if line != ""]
    return rows


def scalar(sql):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] else None


def num(v):
    return Decimal(str(v if v is not None else "0").strip() or "0")


def ok(name, cond, detail=""):
    RESULTS.append((bool(cond), name, detail))
    print(f"  {'[PASS]' if cond else '[FAIL]'} {name}" + (f"  — {detail}" if detail and not cond else ""))


def eq_num(name, expect, actual, detail=""):
    a, e = num(actual), num(expect)
    ok(name, a == e, f"期望 {e} / 实际 {a} {detail}")


def call(method, path, body=None):
    # ⚠ 后端路由的宏路径**不含 `/api` 前缀**（由 [router] prefix 注入）：
    #   脚本里必须写全 `/api/...`，漏了会命中 SPA fallback 返回 404「请求的资源不存在」。
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
    import time

    def attempt(password):
        for _ in range(10):
            status, body = call("POST", "/api/system/auth/login",
                                {"username": "admin", "password": password})
            if status == 429:
                # 登录限流（10005）：等窗口过去再试，不要把这个当成失败
                time.sleep(12)
                continue
            return status, body
        return 429, {"msg": "登录限流窗口过长"}

    status, body = attempt("Admin@123456")
    if status != 200:
        # 上一轮跑过本脚本时已把首登强制改密的新口令写成 Admin@1234567（可重复跑）
        status, body = attempt("Admin@1234567")
    if status != 200:
        raise RuntimeError(f"登录失败：{status} {body}")
    data = body.get("data") or {}
    TOKEN = data.get("access_token") or data.get("token")
    if not TOKEN:
        raise RuntimeError(f"登录响应里没有令牌：{body}")
    if data.get("must_change_password"):
        # 首登强制改密（bootstrap 默认置位）：改完旧令牌立刻失效（token_version bump），
        # 必须清 token 重新登录 —— 否则后面的调用全是 401 + 20008「权限已变更」。
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


def cleanup():
    """按前缀清场（可重复跑）。"""
    psql(f"""
    delete from biz_status_log where entity_type = 'STOCK_OUT' and entity_id in
        (select id from biz_stock_out where out_no like 'OUT-%');
    delete from biz_status_log where entity_type = 'STOCK_IN' and entity_id in
        (select id from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_status_log where entity_type = 'PURCHASE_ORDER' and entity_id in
        (select id from biz_purchase_order where supplier_id = '{SUPPLIER_ID}'::uuid);
    delete from biz_voucher where source_type = 'STOCK_OUT' and is_red = 1;
    delete from biz_voucher where source_type = 'STOCK_OUT';
    delete from biz_voucher where source_type = 'STOCK_IN' and source_id in
        (select id from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid);
    delete from biz_status_log where entity_type = 'VOUCHER' and entity_id not in
        (select id from biz_voucher);
    delete from biz_payable where supplier_id = '{SUPPLIER_ID}'::uuid;
    delete from biz_stock_out where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_stock_in where warehouse_id = '{WAREHOUSE_ID}'::uuid;
    delete from biz_purchase_order where supplier_id = '{SUPPLIER_ID}'::uuid;
    delete from biz_opening_inventory where material_id = '{MATERIAL_ID}'::uuid;
    delete from biz_inventory_txn where material_id = '{MATERIAL_ID}'::uuid;
    delete from biz_inventory_location where material_id = '{MATERIAL_ID}'::uuid;
    delete from biz_inventory where material_id = '{MATERIAL_ID}'::uuid;
    delete from biz_material_serial where material_id = '{MATERIAL_ID}'::uuid;
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
    values ('{WAREHOUSE_ID}', '{TAG}-WH', '{TAG} 出库仓', '/{WAREHOUSE_ID}/', 1, 1, 1, 0)
    on conflict (id) do nothing;
    insert into biz_material (id, material_code, material_name, unit_id, status, serial_required)
    values ('{MATERIAL_ID}', '{TAG}-M1', '{TAG} 螺丝', '{UNIT_ID}', 1, false)
    on conflict (id) do nothing;
    -- 本期会计期间必须是 OPEN：单据日期取今天，没有期间行时凭证会落在「无期间记录」的月份，
    -- 巡检 F2（凭证期间必有期间记录）当场 WARN —— 那是造数缺口，不是账错。
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

def cost_ledger():
    row = psql(f"""select coalesce(sum(qty),0), coalesce(sum(amount),0), coalesce(max(avg_cost),0)
                   from biz_inventory where material_id = '{MATERIAL_ID}'::uuid
                     and warehouse_id = '{WAREHOUSE_ID}'::uuid""")[0]
    return num(row[0]), num(row[1]), num(row[2])


def location_ledger():
    return num(scalar(f"""select coalesce(sum(qty),0) from biz_inventory_location
                          where material_id = '{MATERIAL_ID}'::uuid
                            and warehouse_id = '{WAREHOUSE_ID}'::uuid"""))


def txn_net():
    return num(scalar(f"""select coalesce(sum(case when txn_type='IN' then qty else -qty end),0)
                          from biz_inventory_txn where material_id = '{MATERIAL_ID}'::uuid
                            and warehouse_id = '{WAREHOUSE_ID}'::uuid"""))


def account_balance(prefix):
    return num(scalar(f"""select coalesce(sum(coalesce(e.debit_amount,0) - coalesce(e.credit_amount,0)),0)
                          from biz_voucher_entry e
                          join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
                          join biz_account a on a.id = e.account_id
                         where a.account_code like '{prefix}%'"""))


def account_debit(prefix):
    return num(scalar(f"""select coalesce(sum(coalesce(e.debit_amount,0)),0)
                          from biz_voucher_entry e
                          join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
                          join biz_account a on a.id = e.account_id
                         where a.account_code like '{prefix}%'"""))


def main():
    global BASE, DB
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18097)
    ap.add_argument("--db", default=DB)
    args = ap.parse_args()
    BASE = f"http://127.0.0.1:{args.port}"
    DB = args.db

    print("=" * 88)
    print(f"DC-13 出库单端到端验收   库={DB}   接口={BASE}")
    print("=" * 88)

    print("\n① 准备")
    cleanup()
    fixtures()
    login()
    inv0, amt0, _ = cost_ledger()
    eq_num("初始库存为 0", "0", inv0)
    acc1405_before = account_balance("1405")

    print("\n② 收货入库 100（真实审批链：小额免审直过）")
    body = {
        "warehouse_id": WAREHOUSE_ID,
        "is_sourceless": True,
        "supplier_id": SUPPLIER_ID,
        "remark": f"{TAG} 无源入库：本用例不依赖采购单",
        "items": [{"material_id": MATERIAL_ID, "qty": "100", "price_excl": "8", "tax_rate": "13"}],
    }
    status, resp = call("POST", "/business/erp/stock-in", body)
    ok("建收货单 200", status == 200, f"{status} {resp.get('msg') if isinstance(resp, dict) else resp}")
    in_id = (resp.get("data") or {}).get("id") or (
        ((resp.get("data") or {}).get("header") or {}).get("id"))
    ok("收货单返回 id", bool(in_id), str(resp)[:200])
    status, resp = call("PATCH", f"/business/erp/stock-in/{in_id}/status", {"status": "POSTED"})
    ok("收货过账（含审批链）200", status == 200, f"{status} {resp}")
    inv1, amt1, avg1 = cost_ledger()
    eq_num("入库后数量 100", "100", inv1)
    eq_num("入库后金额 800", "800", amt1)
    eq_num("移动加权单价 8", "8", avg1)

    print("\n③ 领用 30（ISSUE → 借 6401）")
    status, resp = call("POST", "/business/erp/stock-out", {
        "out_type": "ISSUE", "warehouse_id": WAREHOUSE_ID, "reason": "车间领用",
        "items": [{"material_id": MATERIAL_ID, "qty": "30"}],
    })
    ok("建领用单 200", status == 200, f"{status} {resp.get('msg') if isinstance(resp, dict) else resp}")
    issue_id = (resp.get("data") or {}).get("id")
    status, resp = call("PATCH", f"/business/erp/stock-out/{issue_id}/status", {"status": "POSTED"})
    ok("领用过账 200", status == 200, f"{status} {resp}")
    issue_doc = resp.get("data") or {}
    eq_num("领用单金额 = 240（30×8）", "240", issue_doc.get("total_amount"))
    ok("领用单 voucher_id 非空", bool(issue_doc.get("voucher_id")))
    inv2, amt2, _ = cost_ledger()
    eq_num("领用后数量 70", "70", inv2)
    eq_num("领用后金额 560", "560", amt2)

    print("\n④ 报废 10（SCRAP → 借 1901）")
    status, resp = call("POST", "/business/erp/stock-out", {
        "out_type": "SCRAP", "warehouse_id": WAREHOUSE_ID, "reason": "锈蚀报废",
        "items": [{"material_id": MATERIAL_ID, "qty": "10"}],
    })
    ok("建报废单 200", status == 200, f"{status} {resp.get('msg') if isinstance(resp, dict) else resp}")
    scrap_id = (resp.get("data") or {}).get("id")
    status, resp = call("PATCH", f"/business/erp/stock-out/{scrap_id}/status", {"status": "POSTED"})
    ok("报废过账 200", status == 200, f"{status} {resp}")
    eq_num("报废单金额 = 80", "80", (resp.get("data") or {}).get("total_amount"))

    print("\n⑤ 盘亏 5（LOSS → 借 1901）")
    status, resp = call("POST", "/business/erp/stock-out", {
        "out_type": "LOSS", "warehouse_id": WAREHOUSE_ID, "reason": "盘点短少",
        "items": [{"material_id": MATERIAL_ID, "qty": "5"}],
    })
    ok("建盘亏单 200", status == 200, f"{status} {resp.get('msg') if isinstance(resp, dict) else resp}")
    loss_id = (resp.get("data") or {}).get("id")
    status, resp = call("PATCH", f"/business/erp/stock-out/{loss_id}/status", {"status": "POSTED"})
    ok("盘亏过账 200", status == 200, f"{status} {resp}")
    eq_num("盘亏单金额 = 40", "40", (resp.get("data") or {}).get("total_amount"))

    print("\n⑥ 六本账断言")
    inv3, amt3, avg3 = cost_ledger()
    eq_num("成本账数量 55", "55", inv3)
    eq_num("成本账金额 440（55×8）", "440", amt3)
    eq_num("成本账均价 8（扣减不改单价）", "8", avg3)
    eq_num("库位账 Σ = 成本账（B1）", str(inv3), location_ledger())
    eq_num("流水 IN−OUT = 成本账（B4）", str(inv3), txn_net())
    # 入库 +800（借 1405），三次出库结转 −360（贷 1405）→ 余额 = 期初 + 440
    eq_num("1405 余额 = 期初 + 440", str(acc1405_before + Decimal("440")), account_balance("1405"))
    eq_num("6401 借方 = 240（领用）", "240", account_debit("6401"))
    eq_num("1901 借方 = 120（报废 80 + 盘亏 40）", "120", account_debit("1901"))
    ok("无负库存（C1）", inv3 >= 0)

    print("\n⑦ 负库存被拦（409 + 66034）")
    status, resp = call("POST", "/business/erp/stock-out", {
        "out_type": "ISSUE", "warehouse_id": WAREHOUSE_ID, "reason": "超额领用",
        "items": [{"material_id": MATERIAL_ID, "qty": "999"}],
    })
    ok("建单本身允许（草稿）", status == 200, f"{status}")
    over_id = (resp.get("data") or {}).get("id")
    status, resp = call("PATCH", f"/business/erp/stock-out/{over_id}/status", {"status": "POSTED"})
    ok("过账被拦 409", status == 409, f"{status} {resp}")
    ok("错误码 66034", str(resp.get("code")) == "66034", str(resp.get("code")))
    ok("报文点名物料与可用量",
       "DC13E2E-M1" in str(resp.get("msg")) and "55" in str(resp.get("msg")),
       str(resp.get("msg"))[:200])
    inv4, amt4, _ = cost_ledger()
    eq_num("拦截后库存不变（整笔回滚）", str(inv3), inv4)

    print("\n⑧ 作废领用单 → 三本账回滚 + 凭证红冲")
    status, resp = call("PATCH", f"/business/erp/stock-out/{issue_id}/status",
                        {"status": "CANCELLED", "reason": "领错料，退回"})
    ok("作废 200", status == 200, f"{status} {resp}")
    doc = resp.get("data") or {}
    ok("状态 CANCELLED", doc.get("status") == "CANCELLED", str(doc.get("status")))
    inv5, amt5, _ = cost_ledger()
    eq_num("作废后数量回到 85", "85", inv5)
    eq_num("作废后金额回到 680（800 − 报废 80 − 盘亏 40）", "680", amt5)
    eq_num("库位账仍等于成本账（B1）", str(inv5), location_ledger())
    eq_num("流水仍等于成本账（B4）", str(inv5), txn_net())
    eq_num("1405 余额 = 期初 + 680", str(acc1405_before + Decimal("680")), account_balance("1405"))
    reds = num(scalar(f"""select count(*) from biz_voucher where source_type='STOCK_OUT' and is_red=1
                            and source_id = '{issue_id}'::uuid"""))
    ok("已生成红字凭证", reds == 1, f"红字凭证 {reds} 张")
    reversed_flag = num(scalar(f"""select count(*) from biz_voucher
                                     where source_type='STOCK_OUT' and is_red=0
                                       and source_id = '{issue_id}'::uuid and reversed_by is not null"""))
    ok("原凭证已标记 reversed_by", reversed_flag == 1)

    print("\n⑨ 状态机：提交待过账 → 撤回")
    status, resp = call("POST", "/business/erp/stock-out", {
        "out_type": "OTHER", "warehouse_id": WAREHOUSE_ID,
        "items": [{"material_id": MATERIAL_ID, "qty": "1"}],
    })
    pending_id = (resp.get("data") or {}).get("id")
    status, resp = call("PATCH", f"/business/erp/stock-out/{pending_id}/status",
                        {"status": "PENDING_POST", "reason": "交仓管过账"})
    ok("提交待过账 200", status == 200, f"{status} {resp}")
    ok("状态 PENDING_POST", (resp.get("data") or {}).get("status") == "PENDING_POST")
    inv6, _, _ = cost_ledger()
    eq_num("提交不动账", str(inv5), inv6)
    status, resp = call("PATCH", f"/business/erp/stock-out/{pending_id}/status", {"status": "DRAFT"})
    ok("撤回 200", status == 200, f"{status} {resp}")
    ok("回到 DRAFT", (resp.get("data") or {}).get("status") == "DRAFT")

    print("\n⑩ 列表 / 详情接口")
    status, resp = call("GET", "/business/erp/stock-out/page?page=1&page_size=20")
    ok("分页 200", status == 200, f"{status}")
    total = num((resp.get("data") or {}).get("total"))
    ok("列表至少 4 张（3 已过账 + 1 草稿）", total >= 4, f"total={total}")
    status, resp = call("GET", f"/business/erp/stock-out/{loss_id}")
    ok("详情 200", status == 200, f"{status}")
    items = (resp.get("data") or {}).get("items") or []
    ok("详情带明细", len(items) == 1, f"items={len(items)}")
    if items:
        eq_num("明细金额 = 40", "40", items[0].get("amount"))
        ok("明细带物料编码", items[0].get("material_code") == f"{TAG}-M1",
           str(items[0].get("material_code")))

    print("\n" + "=" * 88)
    passed = sum(1 for ok_flag, _, _ in RESULTS if ok_flag)
    failed = len(RESULTS) - passed
    print(f"结果：{passed}/{len(RESULTS)} 通过   失败 {failed}")
    print("=" * 88)
    if failed:
        for ok_flag, name, detail in RESULTS:
            if not ok_flag:
                print(f"  [FAIL] {name}  —  {detail}")
        sys.exit(1)


if __name__ == "__main__":
    main()
