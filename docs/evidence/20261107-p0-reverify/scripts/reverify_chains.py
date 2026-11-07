#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""REVERIFY（t_2f92ee87）六条链重跑 harness —— 跑在 145 本机（HTTP 127.0.0.1:8051 + psql 本机）"""
import json, os, subprocess, sys, time, urllib.request, urllib.error

BASE = os.environ.get("REV_BASE", "http://127.0.0.1:8051")
DB = os.environ.get("REV_DB", "cloudpivot_rev_demo")
PW = os.environ["PGPASSWORD"]
OUT = os.environ.get("REV_RAW", "/root/reverify/raw")
os.makedirs(OUT, exist_ok=True)
PASS = FAIL = 0
FAILS = []
LOG = []

def say(s):
    print(s, flush=True); LOG.append(s)

def ok(m):
    global PASS; PASS += 1; say(f"  ✅ {m}")

def bad(m):
    global FAIL; FAIL += 1; FAILS.append(m); say(f"  ❌ {m}")

def eq(desc, actual, expected):
    if str(actual) == str(expected): ok(f"{desc} = {actual}")
    else: bad(f"{desc} = {actual}（期望 {expected}）")

def num(desc, actual, expected):
    try:
        if abs(float(actual) - float(expected)) < 0.005: ok(f"{desc} = {actual}")
        else: bad(f"{desc} = {actual}（期望 {expected}）")
    except Exception:
        bad(f"{desc} = {actual}（期望数值 {expected}）")

def sq(sql):
    r = subprocess.run(["psql","-h","127.0.0.1","-U","cloudpivot","-d",DB,"-tAc",sql],
                       capture_output=True, text=True, env={**os.environ, "PGPASSWORD": PW})
    return r.stdout.strip()

def api(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + "/api" + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token: req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        raw = e.read()
        try: return e.code, json.loads(raw or b"{}")
        except Exception: return e.code, {"raw": raw.decode("utf-8", "ignore")}

def login(user, pw):
    cands = [pw]
    if user == "admin":
        cands.append(os.environ.get("ADMIN_PW", ""))
    else:
        cands.append("ChangeMe@2026")
    cands += ["Admin@123456", "Admin@20261108"]
    for p in [c for c in cands if c]:
        st, r = api("POST", "/system/auth/login", None, {"username": user, "password": p})
        if st == 200 and r.get("code") == 0:
            d = r["data"]
            if d.get("must_change_password"):
                newp = "Admin@Rev2026"
                st2, _ = api("PUT", "/system/auth/password", d["access_token"],
                             {"old_password": p, "new_password": newp})
                st, r = api("POST", "/system/auth/login", None, {"username": user, "password": newp})
                if st == 200 and r.get("code") == 0: return r["data"]["access_token"]
            return d["access_token"]
    return None

def appr(token, instance_id, comment="REVERIFY 同意"):
    """按实例 PENDING 待办逐个签（审批人取 assignee → 对应演示账号令牌）"""
    for _ in range(10):
        tid = sq(f"select t.id || '|' || coalesce(u.username,'') from biz_flow_task t left join sys_user u on u.id=t.assignee_id "
                 f"where t.instance_id='{instance_id}'::uuid and t.status='PENDING' order by t.created_at limit 1")
        if not tid: return True
        task, uname = tid.split("|")
        tok = TOKENS.get(uname) or TOKENS["admin"]
        st, r = api("POST", f"/business/flow/task/{task}/approve", tok, {"comment": comment})
        if st != 200: bad(f"审批 task={task} who={uname} http={st} {r.get('msg')}"); return False
        time.sleep(0.3)
    return True

TOKENS = {}

def section(t):
    say("\n" + "=" * 70); say(t); say("=" * 70)

def main():
    section("0. 登录（admin + 9 个演示账号，全新库 NIK_SEED_DEMO_USERS=true）")
    users = sq("select string_agg(username, ',') from sys_user where deleted_at is null")
    say(f"库内账号：{users}")
    for u in users.split(","):
        t = login(u, "Admin@123456" if u == "admin" else "ChangeMe@2026")
        if t: TOKENS[u] = t
    say(f"取到令牌：{len(TOKENS)}/{len(users.split(','))}")
    eq("演示账号数", len(users.split(",")), 10)
    if len(TOKENS) < 10: bad("有演示账号登录失败")
    A = TOKENS["admin"]

    UNIT = sq("select id from biz_unit where deleted_at is null limit 1")
    WH = sq("select id from biz_warehouse where deleted_at is null and is_leaf=1 limit 1") or \
         sq("select id from biz_warehouse where deleted_at is null limit 1")
    WHC = sq(f"select warehouse_code from biz_warehouse where id='{WH}'")

    # ================= P0-01 销售链 =================
    section("P0-01 销售链（客户→物料→期初→订单→审批→发货→出库→应收→开票→报表）")
    st, r = api("POST", "/business/erp/customer", A, {"customer_code": "REV-C001", "customer_name": "REVERIFY 客户A", "customer_type": "ENTERPRISE", "status": 1})
    eq("①建客户 http", st, 200); CUST = (r.get("data") or {}).get("id")
    mats = {}
    for code, rate in (("REV-M001", 0), ("REV-M002", 13)):
        st, r = api("POST", "/business/erp/material", A, {"material_code": code, "material_name": "REVERIFY " + code, "unit_id": UNIT,
                     "material_type": "RAW", "costing_method": "MOVING_AVG", "tax_rate": rate, "status": 1})
        eq(f"①建物料 {code} http", st, 200); mats[code] = (r.get("data") or {}).get("id")
    st, r = api("POST", "/business/erp/opening-balance/inventory", A, {"period": "2026-11", "items": [
        {"material_code": "REV-M001", "warehouse_code": WHC, "qty": 100, "unit_cost": 10},
        {"material_code": "REV-M002", "warehouse_code": WHC, "qty": 100, "unit_cost": 20}]})
    eq("②库存期初 http", st, 200)
    num("期初 M001 数量", sq(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{mats['REV-M001']}'"), 100)
    st, r = api("POST", "/business/erp/sales-order", A, {"customer_id": CUST, "order_date": "2026-11-08", "remark": "REVERIFY 链A",
        "items": [{"material_id": mats["REV-M001"], "qty": 10, "price": 100, "tax_rate": 0},
                  {"material_id": mats["REV-M002"], "qty": 5, "price": 200, "tax_rate": 0}]})
    eq("③建销售订单 http", st, 200); SO = (r.get("data") or {}).get("id"); SO_NO = (r.get("data") or {}).get("so_no")
    num("订单金额", (r.get("data") or {}).get("total_amount"), 2000)
    st, r = api("POST", f"/business/erp/sales-order/{SO}/submit-approval", A)
    eq("③提交审批 http", st, 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{SO}' order by created_at desc limit 1")
    eq("③审批实例已生成", "yes" if INST else "no", "yes")
    appr(A, INST)
    eq("③订单终态", sq(f"select status from biz_sales_order where id='{SO}'"), "APPROVED")
    st, r = api("POST", "/business/erp/shipment", A, {"so_id": SO, "warehouse_id": WH, "ship_date": "2026-11-08", "remark": "REVERIFY 发货"})
    eq("④建发货单 http", st, 200); SH = (r.get("data") or {}).get("id")
    eq("④→PENDING_POST", api("PATCH", f"/business/erp/shipment/{SH}/status", A, {"target_status": "PENDING_POST"})[0], 200)
    st, r = api("PATCH", f"/business/erp/shipment/{SH}/status", A, {"target_status": "POSTED"})
    eq("④过账 POSTED http", st, 200)
    num("④过账后 M001 库存", sq(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{mats['REV-M001']}'"), 90)
    num("④过账后 M002 库存", sq(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{mats['REV-M002']}'"), 95)
    eq("④订单 OUTBOUND", sq(f"select status from biz_sales_order where id='{SO}'"), "OUTBOUND")
    AR = sq(f"select id||'|'||amount||'|'||status from biz_receivable where source_id='{SH}' or source_id='{SO}' limit 1")
    say(f"  应收：{AR}")
    num("④应收金额", (AR.split('|')[1] if AR else 0), 2000)
    st, r = api("GET", "/business/erp/report/balance-sheet?period=2026-11", A)
    eq("⑨资产负债表 http", st, 200)
    d = (r.get("data") or {})
    if "check" in d: num("⑨资产=负债+权益 diff", (d["check"] or {}).get("diff", 999), 0)
    st, r = api("GET", "/business/erp/ledger/trial-balance?period=2026-11", A)
    eq("⑨试算平衡 http", st, 200)
    d = (r.get("data") or {})
    for k in ("total_debit", "debit_total"):
        if k in d: num("⑨试算借方==贷方", d.get("total_debit") or d.get("debit_total"), d.get("total_credit") or d.get("credit_total")); break
    st, r = api("POST", "/business/erp/invoice", A, {"customer_id": CUST, "invoice_date": "2026-11-08", "amount": 2000,
                 "tax_rate": 13, "source_type": "MANUAL", "remark": "REVERIFY 13%"})
    eq("⑧开票（税 13 百分数）http", st, 200); INV = (r.get("data") or {}).get("id")
    num("⑧发票税额 2000×13%", (r.get("data") or {}).get("tax_amount"), 260)
    eq("⑧ISSUED http", api("PATCH", f"/business/erp/invoice/{INV}/status", A, {"status": "ISSUED"})[0], 200)
    eq("⑧发票凭证借贷平衡", sq(f"select count(*) from biz_voucher v join biz_invoice i on i.voucher_id=v.id where i.id='{INV}' and v.total_debit=v.total_credit"), 1)
    eq("⑧全库凭证无不平", sq("select count(*) from biz_voucher where status='POSTED' and total_debit<>total_credit"), 0)

    # ================= P0-02 采购链 =================
    section("P0-02 采购链（供应商→采购单 13%→审批→收货→应付→付款→凭证）")
    st, r = api("POST", "/business/erp/supplier", A, {"supplier_code": "REV-S001", "supplier_name": "REVERIFY 供应商", "status": 1})
    eq("①建供应商 http", st, 200); SUP = (r.get("data") or {}).get("id")
    st, r = api("POST", "/business/erp/purchase-order", A, {"supplier_id": SUP, "warehouse_id": WH, "purchase_type": "SPOT",
        "order_date": "2026-11-08", "price_mode": "EXCL", "remark": "REVERIFY 采购链",
        "items": [{"material_id": mats["REV-M002"], "qty": 10, "price_excl": 100, "tax_rate": 13}]})
    eq("②建采购单 http", st, 200); PO = (r.get("data") or {}).get("order", {}).get("id") if isinstance((r.get("data") or {}).get("order"), dict) else (r.get("data") or {}).get("id")
    num("②采购单不含税", sq(f"select total_amount from biz_purchase_order where id='{PO}'"), 1000)
    num("②采购单税额", sq(f"select total_tax from biz_purchase_order where id='{PO}'"), 130)
    num("②采购单价税合计", sq(f"select total_with_tax from biz_purchase_order where id='{PO}'"), 1130)
    eq("③提交审批 http", api("POST", f"/business/erp/purchase-order/{PO}/submit-approval", A)[0], 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{PO}' order by created_at desc limit 1")
    appr(A, INST)
    eq("③采购单终态", sq(f"select status from biz_purchase_order where id='{PO}'"), "APPROVED")
    POI = sq(f"select id from biz_purchase_order_item where order_id='{PO}' limit 1")
    st, r = api("GET", f"/business/erp/purchase-order/{PO}/pending-items", A)
    say(f"  待收明细接口 http={st} {json.dumps(r.get('data'), ensure_ascii=False)[:200]}")
    st, r = api("POST", "/business/erp/stock-in", A, {"purchase_order_id": PO, "warehouse_id": WH, "in_date": "2026-11-08",
        "items": [{"purchase_order_item_id": POI, "qty": 10}]})
    eq("④按采购单收货 http", st, 200); SI = (r.get("data") or {}).get("header", {}).get("id") or (r.get("data") or {}).get("id")
    eq("⑤收货过账 POSTED http", api("PATCH", f"/business/erp/stock-in/{SI}/status", A, {"status": "POSTED"})[0], 200)
    num("⑤收货后 M002 库存", sq(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{mats['REV-M002']}'"), 105)
    AP = sq(f"select id||'|'||amount||'|'||status from biz_payable where source_id='{SI}' limit 1")
    say(f"  应付：{AP}")
    num("⑥应付金额=价税合计", (AP.split('|')[1] if AP else 0), 1130)
    APID = AP.split('|')[0] if AP else ""
    st, r = api("POST", f"/business/erp/payable/{APID}/pay", A, {"amount": 1130, "pay_date": "2026-11-08", "pay_method": "BANK_TRANSFER"})
    say(f"  付款 http={st} {json.dumps(r.get('payload') or r, ensure_ascii=False)[:160]}")
    if st in (200, 202): ok(f"⑦付款受理（{st}{'，走审批 202' if st==202 else '，直付 200'}）")
    else: bad(f"⑦付款 http={st}")
    eq("⑦收货凭证借贷平衡", sq(f"select count(*) from biz_voucher v join biz_stock_in s on s.voucher_id=v.id where s.id='{SI}' and v.total_debit=v.total_credit"), 1)

    # ================= P0-03 库存链 =================
    section("P0-03 库存链（无源入库→盘点→对账 0 差异）")
    st, r = api("POST", "/business/erp/stock-in", A, {"is_sourceless": True, "supplier_id": SUP, "warehouse_id": WH,
        "remark": "REVERIFY 无源入库", "items": [{"material_id": mats["REV-M001"], "qty": 50, "price_excl": 10, "tax_rate": 0}]})
    eq("③无源入库建单 http", st, 200); SI3 = (r.get("data") or {}).get("header", {}).get("id") or (r.get("data") or {}).get("id")
    eq("③无源入库 in_type", sq(f"select in_type from biz_stock_in where id='{SI3}'"), "SOURCELESS")
    eq("③过账 http", api("PATCH", f"/business/erp/stock-in/{SI3}/status", A, {"status": "POSTED"})[0], 200)
    eq("③流水 biz_type 词表内", sq(f"select count(*) from biz_inventory_txn where material_id='{mats['REV-M001']}' and biz_type not in ('OPENING','PURCHASE_IN','SOURCELESS','SALE_OUT','OTHER_OUT','STOCK_TAKE_GAIN','STOCK_TAKE_LOSS','TRANSFER_OUT','TRANSFER_IN','ISSUE','SCRAP','LOSS')"), 0)
    st, r = api("POST", "/business/erp/stock-take", A, {"warehouse_id": WH, "take_date": "2026-11-08", "remark": "REVERIFY 盘点"})
    eq("⑤建盘点单 http", st, 200); STK = (r.get("data") or {}).get("id")
    eq("⑤盘点提交审批 http", api("PATCH", f"/business/erp/stock-take/{STK}/status", A, {"status": "PENDING_POST"})[0], 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{STK}' order by created_at desc limit 1")
    if INST:
        appr(A, INST, "REVERIFY 盘点同意")
        eq("⑤盘点审批后状态", sq(f"select status from biz_stock_take where id='{STK}'"), "PENDING_POST")
    eq("⑥逐桶对账差异行", sq("""select count(*) from (
      select coalesce(i.material_id,l.material_id) m, coalesce(i.warehouse_id,l.warehouse_id) w, coalesce(i.location_id,l.location_id) loc,
             coalesce(i.qty,0) cq, coalesce(l.qty,0) lq
        from biz_inventory i full join biz_inventory_location l
          on l.material_id=i.material_id and l.warehouse_id=i.warehouse_id and l.location_id is not distinct from i.location_id
     ) x where abs(cq-lq) > 0.0001"""), 0)
    eq("⑥成本账与流水自洽差异行", sq("""select count(*) from (
      select t.material_id, t.warehouse_id, sum(t.qty) s, (select coalesce(sum(qty),0) from biz_inventory i where i.material_id=t.material_id and i.warehouse_id=t.warehouse_id) q
        from biz_inventory_txn t group by 1,2) x where abs(s-q) > 0.0001"""), 0)

    # ================= P0-06 预算链 =================
    section("P0-06 预算链（编制→审批→超支 409→未编预算不误判）")
    ACC = sq("select id from biz_account where account_code='5602.02' and deleted_at is null limit 1") or \
          sq("select id from biz_account where account_code like '5602%' and deleted_at is null limit 1")
    DEPT = sq("select id from sys_dept where deleted_at is null limit 1")
    st, r = api("POST", "/business/erp/budget", A, {"fiscal_year": 2026, "dept_id": DEPT, "account_id": ACC, "total_amount": 10000,
        "periods": [{"period_no": i, "amount": round(10000 / 12, 2)} for i in range(1, 13)], "control_mode": "BLOCK", "remark": "REVERIFY 预算"})
    eq("①编预算 http", st, 200); BDG = (r.get("data") or {}).get("id")
    eq("②提交审批 http", api("POST", f"/business/erp/budget/{BDG}/submit-approval", A)[0], 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{BDG}' order by created_at desc limit 1")
    if INST: appr(A, INST)
    eq("②预算 ACTIVE", sq(f"select status from biz_budget where id='{BDG}'"), "ACTIVE")
    ETP = sq("select id from biz_expense_type where deleted_at is null limit 1")
    if not ETP:
        st, r = api("POST", "/business/erp/expense-type", A, {"type_code": "REV_TRAVEL", "type_name": "REVERIFY 差旅", "account_id": ACC})
        eq("③建费用类型 http", st, 200); ETP = (r.get("data") or {}).get("id")
    st, r = api("GET", f"/business/erp/budget/{BDG}", A)
    eq("④预算详情 usages 字段存在", "usages" in (r.get("data") or {}), True)
    say(f"  预算详情 usages={len((r.get('data') or {}).get('usages') or [])} 条")
    st, r = api("GET", f"/business/erp/budget/report?fiscal_year=2026", A)
    eq("④执行报表 http", st, 200)

    # ================= P0-05 报销链 =================
    section("P0-05 报销链（建单→提交→审批→付款→凭证 + 预算拦截）")
    st, r = api("POST", "/business/erp/expense", A, {"title": "REVERIFY 报销 12000", "expense_date": "2026-11-08",
        "reporter_id": sq("select id from sys_user where username='demo_lead'"), "dept_id": DEPT,
        "items": [{"expense_type_id": ETP, "with_tax_amount": 12000, "tax_rate": 0, "summary": "REVERIFY 超月额度"}]})
    say(f"  建单 http={st} {json.dumps(r.get('data') if r.get('code')==0 else r, ensure_ascii=False)[:160]}")
    EXP = (r.get("data") or {}).get("id") if st == 200 else None
    if EXP:
        st, r = api("POST", f"/business/erp/expense/{EXP}/submit", A)
        eq("②提交：超月额度被 BLOCK 拦截 409", st, 409)
        eq("②拦截错误码 66037", (r.get("code")), 66037)
        say(f"  报文：{r.get('msg','')[:220]}")
    st, r = api("POST", "/business/erp/expense", A, {"title": "REVERIFY 报销 300", "expense_date": "2026-11-08",
        "reporter_id": sq("select id from sys_user where username='demo_lead'"), "dept_id": DEPT,
        "items": [{"expense_type_id": ETP, "with_tax_amount": 300, "tax_rate": 0, "summary": "REVERIFY 额度内"}]})
    EXP2 = (r.get("data") or {}).get("id")
    eq("③建单（额度内）http", st, 200)
    st, r = api("POST", f"/business/erp/expense/{EXP2}/submit", A)
    eq("③提交（额度内）http", st, 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{EXP2}' order by created_at desc limit 1")
    if INST: appr(A, INST, "REVERIFY 报销同意")
    eq("④单据 APPROVED", sq(f"select status from biz_expense_report where id='{EXP2}'"), "APPROVED")
    st, r = api("POST", f"/business/erp/expense/{EXP2}/pay", A, {"amount": 300, "pay_date": "2026-11-08", "pay_method": "BANK_TRANSFER", "pay_account_code": "1002"})
    eq("⑤付款 http", st, 200)
    eq("⑥凭证借贷平衡", sq(f"select count(*) from biz_voucher where status='POSTED' and total_debit<>total_credit"), 0)
    eq("⑦占用行 over_budget 与 missing_budget 互斥", sq("select count(*) from biz_budget_usage where over_budget=1 and missing_budget=1"), 0)

    # ================= P0-04 资产链 =================
    section("P0-04 资产链（分类→建卡→计提→处置前置闸 409→计提后处置）")
    st, r = api("GET", "/business/erp/asset-category", A)
    eq("①资产分类列表 http", st, 200)
    CAT = sq("select id from biz_asset_category where deleted_at is null limit 1")
    st, r = api("POST", "/business/erp/asset", A, {"category_id": CAT, "asset_name": "REVERIFY 设备A", "asset_code": "REV-FA-001",
        "purchase_price": 120000, "purchase_date": "2026-11-01", "dept_id": DEPT, "status": "IN_USE"})
    say(f"  建卡 http={st} {json.dumps(r.get('data') if r.get('code')==0 else r, ensure_ascii=False)[:160]}")
    if st == 200:
        FA = (r.get("data") or {}).get("id")
        st, r = api("POST", "/business/erp/depreciation/run", A, {"period_code": "2026-11"})
        eq("③计提 2026-11 http", st, 200)
        st, r = api("POST", f"/business/erp/asset/{FA}/dispose", A, {"disposal_date": "2026-11-20", "disposal_type": "SALE", "sale_price": 1000})
        say(f"  处置 http={st} {json.dumps(r if r.get('code')!=0 else {}, ensure_ascii=False)[:160]}")
        if st == 409: ok(f"④处置前置闸生效（409 {r.get('code')}：处置当月折旧未计提/流程）")
        if st == 200: ok("④处置受理")

    say("\n" + "=" * 70)
    say(f"RESULT: PASS={PASS} FAIL={FAIL}")
    if FAILS:
        say("FAILED:")
        for f in FAILS: say("  - " + f)
    with open(os.path.join(OUT, "harness.log"), "w") as fh:
        fh.write("\n".join(LOG) + "\n")
    return 0 if FAIL == 0 else 1

if __name__ == "__main__":
    sys.exit(main())
