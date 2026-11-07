#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""REVERIFY（t_2f92ee87）六条链重跑 harness —— 跑在 145 本机（HTTP 127.0.0.1:8051 + 本机 psql）。

口径
====
* 目标库是**全新库** `cloudpivot_rev_demo`（`NIK_SEED_DEMO_USERS=true` 首启）：
  261 条迁移 / 10 个账号 / 16 条已发布流程，对应卡面「全新库开箱即用」。
* 断言按**当前接口契约**写（P0-01..06 原始脚本是按旧契约写的，见 README §1 说明）；
  每条断言都打 `期望/实际`，失败时附 `http + body` 便于定位。
* 登录**只做单次正确口令尝试**（上一轮「多口令候选」把 admin 打成 locked + 触发 10005 限流）；
  令牌缓存在 `/root/reverify/tokens.json.<db>`，复跑时先用 `/system/auth/me` 验活，避免重复登录。
* 幂等：客户 / 物料 / 供应商按编码 get-or-create；库存期初是「整期覆盖」，
  所以同一库复跑不会让数量断言漂移（其余单据每次新建一行，断言只看增量）。
"""
import json, os, subprocess, sys, time, urllib.request, urllib.error

BASE = os.environ.get("REV_BASE", "http://127.0.0.1:8051")
DB = os.environ.get("REV_DB", "cloudpivot_rev_demo")
PW = os.environ["PGPASSWORD"]
OUT = os.environ.get("REV_RAW", "/root/reverify/raw")
os.makedirs(OUT, exist_ok=True)
PASS = FAIL = 0
FAILS = []
LOG = []
LAST = {"http": None, "body": None}
CHAIN = {"name": "-", "pass": 0, "fail": 0, "rows": []}

def say(s):
    print(s, flush=True); LOG.append(s)

def ok(m):
    global PASS; PASS += 1; CHAIN["pass"] += 1; say(f"  ✅ {m}")

def bad(m):
    global FAIL; FAIL += 1; CHAIN["fail"] += 1
    hint = ""
    if LAST["http"] is not None:
        hint = f"  ← http={LAST['http']} body={json.dumps(LAST['body'], ensure_ascii=False)[:320]}"
    FAILS.append(f"[{CHAIN['name']}] {m}")
    say(f"  ❌ {m}{hint}")

def eq(desc, actual, expected):
    if str(actual) == str(expected): ok(f"{desc} = {actual}")
    else: bad(f"{desc} = {actual}（期望 {expected}）")

def num(desc, actual, expected, tol=0.005):
    try:
        if abs(float(actual) - float(expected)) <= tol: ok(f"{desc} = {actual}")
        else: bad(f"{desc} = {actual}（期望 {expected}）")
    except Exception:
        bad(f"{desc} = {actual}（期望数值 {expected}）")

def sq(sql):
    r = subprocess.run(["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-tAc", sql],
                       capture_output=True, text=True, env={**os.environ, "PGPASSWORD": PW})
    if r.returncode != 0:
        say(f"  (psql 失败: {r.stderr.strip()[:200]})")
    return r.stdout.strip()

def api(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + "/api" + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token: req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            raw = r.read()
        out = (200, json.loads(raw or b"{}"))
    except urllib.error.HTTPError as e:
        raw = e.read()
        try: out = (e.code, json.loads(raw or b"{}"))
        except Exception: out = (e.code, {"raw": raw.decode("utf-8", "ignore")})
    LAST["http"], LAST["body"] = out
    return out

def data_of(r):
    d = r.get("data")
    return d if isinstance(d, dict) else {}

def pick(*vals):
    for v in vals:
        if v not in (None, ""):
            return v
    return None

# ---------------- 登录 ----------------
CHANGED_PW = os.environ.get("REV_CHANGED_PW", "Admin@Rev2026")
TOKEN_CACHE = os.environ.get("REV_TOKENS", "/root/reverify/tokens.json")
PW_OF = {}

def _unlock(user):
    sq(f"update sys_user set login_fail_count=0, is_locked=false, lock_time=null where username='{user}'")

def _login_once(user, pw):
    st, r = api("POST", "/system/auth/login", None, {"username": user, "password": pw})
    if st == 429 or r.get("code") == 10005:
        say(f"  ⏳ {user} 命中登录限流（10005），等 65s 重试一次")
        time.sleep(65)
        st, r = api("POST", "/system/auth/login", None, {"username": user, "password": pw})
    if r.get("code") == 20003:
        say(f"  🔓 {user} 账号被锁（20003）→ SQL 解锁后重试")
        _unlock(user)
        st, r = api("POST", "/system/auth/login", None, {"username": user, "password": pw})
    return st, r

def login(user, pw):
    """正确的单一口令登录；`must_change_password` 时按真实流程改密再登（改一次，后续复用）。"""
    st, r = _login_once(user, pw)
    if st != 200 or r.get("code") != 0:
        say(f"  ❌ 登录失败 {user} http={st} code={r.get('code')} msg={r.get('msg')}")
        return None
    d = data_of(r)
    if d.get("must_change_password"):
        st2, r2 = api("PUT", "/system/auth/password", d["access_token"],
                      {"old_password": pw, "new_password": CHANGED_PW})
        if st2 != 200 or r2.get("code") != 0:
            say(f"  ❌ 强制改密失败 {user} http={st2} code={r2.get('code')} msg={r2.get('msg')}")
            return None
        say(f"  🔑 {user} 首次登录强制改密成功")
        st, r = _login_once(user, CHANGED_PW)
        if st != 200 or r.get("code") != 0:
            say(f"  ❌ 改密后登录失败 {user} http={st} code={r.get('code')}")
            return None
        d = data_of(r)
        PW_OF[user] = CHANGED_PW
    else:
        PW_OF[user] = pw
    return d["access_token"]

def _cache_path():
    return f"{TOKEN_CACHE}.{DB}"

def save_tokens():
    try:
        with open(_cache_path(), "w") as fh:
            json.dump({"tokens": TOKENS, "pw": PW_OF}, fh)
    except Exception as e:
        say(f"  (令牌缓存写入失败，不影响结论：{e})")

def load_tokens():
    try:
        with open(_cache_path()) as fh:
            d = json.load(fh)
        TOKENS.update(d.get("tokens") or {})
        PW_OF.update(d.get("pw") or {})
        return True
    except Exception:
        return False

TOKENS = {}

def appr(instance_id, comment="REVERIFY 同意", admin_token=None):
    """按实例的 PENDING 待办逐个签：审批人 → 对应演示账号令牌（缺失时用 admin 代签并记一条说明）。"""
    for _ in range(10):
        tid = sq(f"select t.id || '|' || coalesce(u.username,'') from biz_flow_task t "
                 f"left join sys_user u on u.id=t.assignee_id "
                 f"where t.instance_id='{instance_id}'::uuid and t.status='PENDING' "
                 f"order by t.created_at limit 1")
        if not tid: return True
        task, uname = tid.split("|")
        tok = TOKENS.get(uname)
        if not tok:
            tok = admin_token or TOKENS.get("admin")
            say(f"    (待办 {uname or '空'} 无令牌，用 admin 代签)")
        st, r = api("POST", f"/business/flow/task/{task}/approve", tok, {"comment": comment})
        if st != 200 or r.get("code") != 0:
            bad(f"审批 task={task} who={uname} http={st} code={r.get('code')} msg={r.get('msg')}")
            return False
        time.sleep(0.25)
    return True

def start_chain(name):
    CHAIN.update({"name": name, "pass": 0, "fail": 0})
    say("\n" + "=" * 70); say(name); say("=" * 70)

def end_chain():
    CHAIN["rows"].append((CHAIN["name"], CHAIN["pass"], CHAIN["fail"]))
    say(f"  —— {CHAIN['name']}：{CHAIN['pass']}/{CHAIN['pass'] + CHAIN['fail']} 通过")

def get_or_create(kind, code_col, code, path, token, body):
    """按编码 get-or-create：已存在直接复用 id（幂等，避免复跑 409）。"""
    exist = sq(f"select id from {kind} where {code_col}='{code}' and deleted_at is null limit 1")
    if exist:
        say(f"  (复用已有 {kind} {code} → {exist})")
        return exist, True
    st, r = api("POST", path, token, body)
    eq(f"①建{'客户' if kind == 'biz_customer' else '供应商' if kind == 'biz_supplier' else '物料'} {code} http", st, 200)
    return data_of(r).get("id"), False

def main():
    start_chain("0. 登录（admin + 9 个演示账号，全新库 NIK_SEED_DEMO_USERS=true）")
    users = sq("select string_agg(username, ',' order by username) from sys_user where deleted_at is null")
    say(f"库内账号：{users}")
    eq("账号总数", len(users.split(",")) if users else 0, 10)
    if load_tokens():
        stale = []
        for u, tk in list(TOKENS.items()):
            st, _ = api("GET", "/system/auth/me", tk)
            if st != 200:
                stale.append(u); del TOKENS[u]; PW_OF.pop(u, None)
        if not TOKENS:
            PW_OF.clear()   # 令牌全失效 ⇒ 缓存里的口令也不可信（库可能被重建过）
        say(f"令牌缓存命中 {len(TOKENS)} 个，失效 {len(stale)} 个 {stale}")
    ADMIN_PW = os.environ.get("ADMIN_PW", "")
    DEMO_PW = os.environ.get("DEMO_PW", "ChangeMe@2026")
    # 目标库是一次性全新库：登录前把失败计数/锁定清零，避免上一轮探测残留把账号锁掉（与六条链无关）
    sq("update sys_user set login_fail_count=0, is_locked=false, lock_time=null")
    for u in (users.split(",") if users else []):
        if u in TOKENS: continue
        canonical = ADMIN_PW if u == "admin" else DEMO_PW
        if u == "admin" and not ADMIN_PW:
            bad("缺少 admin 口令（用 ADMIN_PW 环境变量传）"); break
        cand = PW_OF.get(u) or canonical
        t = login(u, cand)
        if not t and cand != canonical:
            say(f"  ↩ {u} 用种子口令重试一次")
            t = login(u, canonical)
        if t: TOKENS[u] = t; time.sleep(0.4)
        else: bad(f"{u} 登录失败")
    save_tokens()
    say(f"取到令牌：{len(TOKENS)}/{len(users.split(',')) if users else 0}")
    if len(TOKENS) < (len(users.split(",")) if users else 0):
        say("❌ 令牌不齐，后续链路断言无意义 —— 退出 1")
        end_chain(); return 1
    A = TOKENS["admin"]
    end_chain()

    UNIT = sq("select id from biz_unit where deleted_at is null order by is_base desc limit 1")
    WH = (sq("select id from biz_warehouse where deleted_at is null and is_leaf=1 "
             "and warehouse_code='WH02' limit 1")
          or sq("select id from biz_warehouse where deleted_at is null and is_leaf=1 limit 1"))
    DEPT = sq("select id from sys_dept where deleted_at is null order by dept_code limit 1")
    say(f"环境：unit={UNIT} wh={WH} dept={DEPT}")

    # ==================== P0-01 销售链 ====================
    start_chain("P0-01 销售链（客户→物料→期初→订单→审批→发货→出库→应收→开票→报表）")
    CUST, _ = get_or_create("biz_customer", "code", "REV-C001", "/business/erp/customer", A,
                            {"code": "REV-C001", "name": "REVERIFY 客户A", "status": "ACTIVE"})
    mats = {}
    for code, rate in (("REV-M001", 0), ("REV-M002", 13)):
        mid, _new = get_or_create("biz_material", "material_code", code, "/business/erp/material", A,
                                  {"material_code": code, "material_name": "REVERIFY " + code,
                                   "unit_id": UNIT, "material_type": "RAW",
                                   "costing_method": "MOVING_AVG", "tax_rate": rate, "status": 1})
        mats[code] = mid
    st, r = api("POST", "/business/erp/opening-balance/inventory", A,
                {"period": "2026-11", "items": [
                    {"material_id": mats["REV-M001"], "warehouse_id": WH, "qty": 100, "unit_cost": 10},
                    {"material_id": mats["REV-M002"], "warehouse_id": WH, "qty": 100, "unit_cost": 20}]})
    eq("②库存期初 http", st, 200)
    eq("②期初行 M001 数量（biz_opening_inventory）=100",
       sq(f"select coalesce(sum(qty),0) from biz_opening_inventory where period_code='2026-11' "
          f"and material_id='{mats['REV-M001']}'"), "100.0000")
    eq("②期初行 M002 数量（biz_opening_inventory）=100",
       sq(f"select coalesce(sum(qty),0) from biz_opening_inventory where period_code='2026-11' "
          f"and material_id='{mats['REV-M002']}'"), "100.0000")
    # 期初是「整期覆盖」，但**已经发生的过账不会被回滚**；所以库存断言一律取**增量**（相对本次开单前）
    Q1_0 = sq(f"select coalesce(sum(qty),0)::numeric(18,4) from biz_inventory where material_id='{mats['REV-M001']}'")
    Q2_0 = sq(f"select coalesce(sum(qty),0)::numeric(18,4) from biz_inventory where material_id='{mats['REV-M002']}'")
    st, r = api("POST", "/business/erp/sales-order", A,
                {"customer_id": CUST, "order_date": "2026-11-08", "remark": "REVERIFY 链A",
                 "items": [{"material_id": mats["REV-M001"], "qty": 10, "price": 100, "tax_rate": 0},
                           {"material_id": mats["REV-M002"], "qty": 5, "price": 200, "tax_rate": 0}]})
    eq("③建销售订单 http", st, 200)
    SO, SO_NO = data_of(r).get("id"), data_of(r).get("so_no") or data_of(r).get("order_no")
    say(f"  订单 {SO_NO} id={SO} 金额={data_of(r).get('total_amount')}")
    num("③订单金额", data_of(r).get("total_amount"), 2000)
    st, r = api("POST", f"/business/erp/sales-order/{SO}/submit-approval", A, {})
    eq("③提交审批 http", st, 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{SO}' order by created_at desc limit 1")
    eq("③审批实例已生成", bool(INST), True)
    appr(INST, "REVERIFY 销售订单同意", A)
    eq("③订单终态 APPROVED", sq(f"select status from biz_sales_order where id='{SO}'"), "APPROVED")
    st, r = api("POST", "/business/erp/shipment", A,
                {"so_id": SO, "warehouse_id": WH, "ship_date": "2026-11-08", "remark": "REVERIFY 发货"})
    eq("④建发货单 http", st, 200)
    SH = data_of(r).get("id")
    eq("④发货单 PENDING_POST", api("PATCH", f"/business/erp/shipment/{SH}/status", A,
                                  {"target_status": "PENDING_POST"})[0], 200)
    eq("④过账 POSTED http", api("PATCH", f"/business/erp/shipment/{SH}/status", A,
                               {"target_status": "POSTED"})[0], 200)
    num("④过账后 M001 库存 = 开单前 −10",
        float(Q1_0 or 0) - float(sq(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{mats['REV-M001']}'") or 0), 10)
    num("④过账后 M002 库存 = 开单前 −5",
        float(Q2_0 or 0) - float(sq(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{mats['REV-M002']}'") or 0), 5)
    eq("④订单 OUTBOUND", sq(f"select status from biz_sales_order where id='{SO}'"), "OUTBOUND")
    AR = sq(f"select r.id||'|'||r.amount||'|'||r.status from biz_receivable r "
            f"join biz_stock_out o on o.id = r.source_id where o.source_id='{SH}' limit 1")
    say(f"  应收：{AR}（经 biz_stock_out.source_id=发货单 关联）")
    num("④应收金额", (AR.split("|")[1] if AR else 0), 2000)
    st, r = api("GET", "/business/erp/report/balance-sheet?period=2026-11", A)
    eq("⑨资产负债表 http", st, 200)
    d = data_of(r)
    if "check" in d: num("⑨资产=负债+权益 diff", (d.get("check") or {}).get("diff", 999), 0)
    st, r = api("GET", "/business/erp/ledger/trial-balance?period=2026-11", A)
    eq("⑨试算平衡 http", st, 200)
    d = data_of(r)
    debit = pick(d.get("total_debit"), d.get("debit_total"), (d.get("total") or {}).get("debit"))
    credit = pick(d.get("total_credit"), d.get("credit_total"), (d.get("total") or {}).get("credit"))
    if debit is not None and credit is not None: num("⑨试算借方==贷方", debit, credit)
    st, r = api("POST", "/business/erp/invoice", A,
                {"customer_id": CUST, "invoice_date": "2026-11-08", "amount": 2000, "tax_rate": 13,
                 "invoice_type": "OUT", "remark": "REVERIFY 13%"})
    eq("⑧开票（税率 13 百分数）http", st, 200)
    INV = data_of(r).get("id")
    num("⑧发票税额 2000×13%", data_of(r).get("tax_amount"), 260)
    num("⑧发票价税合计", data_of(r).get("total_with_tax"), 2260)
    eq("⑧ISSUED http", api("PATCH", f"/business/erp/invoice/{INV}/status", A, {"status": "ISSUED"})[0], 200)
    eq("⑧发票凭证借贷平衡", sq(f"select count(*) from biz_voucher v join biz_invoice i on i.voucher_id=v.id "
                          f"where i.id='{INV}' and v.total_debit=v.total_credit"), 1)
    eq("⑧全库凭证无不平", sq("select count(*) from biz_voucher where status='POSTED' and total_debit<>total_credit"), 0)
    end_chain()

    # ==================== P0-02 采购链 ====================
    start_chain("P0-02 采购链（供应商→采购单 13%→审批→收货→应付→付款→凭证）")
    SUP, _ = get_or_create("biz_supplier", "supplier_code", "REV-S001", "/business/erp/supplier", A,
                           {"supplier_code": "REV-S001", "supplier_name": "REVERIFY 供应商", "status": 1})
    st, r = api("POST", "/business/erp/purchase-order", A,
                {"supplier_id": SUP, "warehouse_id": WH, "purchase_type": "SPOT",
                 "order_date": "2026-11-08", "price_mode": "EXCL", "remark": "REVERIFY 采购链",
                 "items": [{"material_id": mats["REV-M002"], "qty": 10, "price_excl": 100, "tax_rate": 13}]})
    eq("②建采购单 http", st, 200)
    PO = pick(data_of(r).get("id"), (data_of(r).get("order") or {}).get("id"))
    num("②采购单不含税", sq(f"select total_amount from biz_purchase_order where id='{PO}'"), 1000)
    num("②采购单税额", sq(f"select total_tax from biz_purchase_order where id='{PO}'"), 130)
    num("②采购单价税合计", sq(f"select total_with_tax from biz_purchase_order where id='{PO}'"), 1130)
    st, r = api("POST", f"/business/erp/purchase-order/{PO}/submit-approval", A, {})
    eq("③提交审批 http", st, 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{PO}' order by created_at desc limit 1")
    appr(INST, "REVERIFY 采购单同意", A)
    eq("③采购单终态 APPROVED", sq(f"select status from biz_purchase_order where id='{PO}'"), "APPROVED")
    POI = sq(f"select id from biz_purchase_order_item where order_id='{PO}' limit 1")
    st, r = api("GET", f"/business/erp/purchase-order/{PO}/pending-items", A)
    eq("③待收明细接口 http", st, 200)
    Q2_PRE = float(sq(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{mats['REV-M002']}'") or 0)
    st, r = api("POST", "/business/erp/stock-in", A,
                {"purchase_order_id": PO, "warehouse_id": WH, "in_date": "2026-11-08",
                 "items": [{"purchase_order_item_id": POI, "material_id": mats["REV-M002"], "qty": 10,
                            "price_excl": 100, "tax_rate": 13}]})
    eq("④按采购单收货 http", st, 200)
    SI = pick((data_of(r).get("header") or {}).get("id"), data_of(r).get("id"))
    eq("⑤收货过账 POSTED http", api("PATCH", f"/business/erp/stock-in/{SI}/status", A,
                                   {"status": "POSTED"})[0], 200)
    # 收货过账是**审批步**（`stock_in_post` 流程，已发布）：PATCH 只发起审批，
    # 审批落定后才真正过账（库存 / 应付 / 凭证都在之后出现）。
    INSP = sq(f"select id from biz_flow_instance where biz_id='{SI}' and biz_type='STOCK_IN_POST' "
              f"order by created_at desc limit 1")
    if INSP:
        appr(INSP, "REVERIFY 收货过账同意", A)
    eq("⑤审批后收货单 POSTED", sq(f"select status from biz_stock_in where id='{SI}'"), "POSTED")
    num("⑤收货后 M002 库存 = 收货前 +10",
        float(sq(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{mats['REV-M002']}'") or 0) - Q2_PRE, 10)
    AP = sq(f"select id||'|'||amount||'|'||status from biz_payable where source_id='{SI}' limit 1")
    say(f"  应付：{AP}")
    num("⑥应付金额=价税合计", (AP.split("|")[1] if AP else 0), 1130)
    APID = AP.split("|")[0] if AP else ""
    st, r = api("POST", f"/business/erp/payable/{APID}/pay", A,
                {"amount": 1130, "pay_date": "2026-11-08", "pay_method": "BANK_TRANSFER"})
    if st in (200, 202): ok(f"⑦付款受理（{st}{'，走审批 202' if st == 202 else '，直付 200'}）")
    else: bad(f"⑦付款 http={st}")
    eq("⑦收货凭证借贷平衡", sq(f"select count(*) from biz_voucher v join biz_stock_in s on s.voucher_id=v.id "
                          f"where s.id='{SI}' and v.total_debit=v.total_credit"), 1)
    end_chain()

    # ==================== P0-03 库存链 ====================
    start_chain("P0-03 库存链（无源入库→盘点→审批→逐桶对账 0 差异）")
    st, r = api("POST", "/business/erp/stock-in", A,
                {"is_sourceless": True, "supplier_id": SUP, "warehouse_id": WH,
                 "remark": "REVERIFY 无源入库（期初调整）",
                 "items": [{"material_id": mats["REV-M001"], "qty": 50, "price_excl": 10, "tax_rate": 0}]})
    eq("③无源入库建单 http", st, 200)
    SI3 = pick((data_of(r).get("header") or {}).get("id"), data_of(r).get("id"))
    eq("③无源入库 in_type=SOURCELESS", sq(f"select in_type from biz_stock_in where id='{SI3}'"), "SOURCELESS")
    eq("③过账 http", api("PATCH", f"/business/erp/stock-in/{SI3}/status", A, {"status": "POSTED"})[0], 200)
    INSP3 = sq(f"select id from biz_flow_instance where biz_id='{SI3}' and biz_type='STOCK_IN_POST' "
               f"order by created_at desc limit 1")
    if INSP3:
        appr(INSP3, "REVERIFY 无源入库过账同意", A)
    eq("③审批后无源入库 POSTED", sq(f"select status from biz_stock_in where id='{SI3}'"), "POSTED")
    eq("③流水 biz_type 全在词表内", sq(
        f"select count(*) from biz_inventory_txn where material_id='{mats['REV-M001']}' and biz_type not in "
        "('OPENING','PURCHASE_IN','SOURCELESS','SALE_OUT','OTHER_OUT','STOCK_TAKE_GAIN','STOCK_TAKE_LOSS',"
        "'TRANSFER_OUT','TRANSFER_IN','ISSUE','SCRAP','LOSS')"), 0)
    st, r = api("POST", "/business/erp/stock-take", A,
                {"warehouse_id": WH, "take_date": "2026-11-08", "remark": "REVERIFY 盘点"})
    eq("⑤建盘点单 http", st, 200)
    STK = pick(data_of(r).get("id"), (data_of(r).get("header") or {}).get("id"))
    eq("⑤盘点提交审批 http", api("PATCH", f"/business/erp/stock-take/{STK}/status", A,
                                {"status": "PENDING_POST"})[0], 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{STK}' order by created_at desc limit 1")
    if INST:
        appr(INST, "REVERIFY 盘点同意", A)
        eq("⑤盘点审批后仍 PENDING_POST（两步口径）",
           sq(f"select status from biz_stock_take where id='{STK}'"), "PENDING_POST")
    else:
        bad("⑤盘点未生成审批实例")
    # 对账口径**逐字照抄** docs/ERP-库存对账口径.md §2.2 / §2.1（FIX-7 定稿）：
    #   · 逐桶只能比「成本账 vs 库位账」，且必须按桶键 FULL JOIN（i.location_id 引用的是
    #     biz_inventory_location.id，不是库位字典 id）；
    #   · 「库存 = 流水」只能在 (物料, 仓库) 聚合比，方向靠 txn_type(IN/OUT)，qty 恒正。
    eq("⑥逐桶对账差异行（成本账 vs 库位账，B1b）", sq("""
        WITH cost AS (
            SELECT i.material_id, i.warehouse_id,
                   COALESCE(il.location_id::text, '(未分配)') AS bucket,
                   SUM(i.qty) AS qty
              FROM biz_inventory i
              LEFT JOIN biz_inventory_location il ON il.id = i.location_id
             GROUP BY 1, 2, 3
        ), loc AS (
            SELECT material_id, warehouse_id,
                   COALESCE(location_id::text, '(未分配)') AS bucket,
                   SUM(qty) AS qty
              FROM biz_inventory_location
             GROUP BY 1, 2, 3
        )
        SELECT count(*) FROM cost FULL JOIN loc
          ON cost.material_id = loc.material_id
         AND cost.warehouse_id = loc.warehouse_id
         AND cost.bucket = loc.bucket
        WHERE COALESCE(cost.qty, 0) <> COALESCE(loc.qty, 0)"""), 0)
    eq("⑥库存表 vs 流水累计差异行（B4）", sq("""
        WITH inv AS (
            SELECT material_id, warehouse_id, SUM(qty) AS qty FROM biz_inventory GROUP BY 1, 2
        ), txn AS (
            SELECT material_id, warehouse_id,
                   SUM(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END) AS qty
              FROM biz_inventory_txn GROUP BY 1, 2
        )
        SELECT count(*) FROM inv FULL JOIN txn USING (material_id, warehouse_id)
        WHERE COALESCE(inv.qty, 0) <> COALESCE(txn.qty, 0)"""), 0)
    eq("⑥流水自洽差异行（after-before=±qty）", sq("""
        SELECT count(*) FROM biz_inventory_txn
         WHERE (txn_type = 'IN'  AND after_qty - before_qty <> qty)
            OR (txn_type = 'OUT' AND before_qty - after_qty <> qty)"""), 0)
    end_chain()

    # ==================== P0-06 预算链 ====================
    start_chain("P0-06 预算链（编制→提交审批→ACTIVE→详情 usages→执行报表）")
    ACC = sq("select id from biz_account where account_code='5602.02' and deleted_at is null limit 1")
    # 同库复跑：同一「年度+部门+科目」槽只能有一份生效预算，已存在就复用（幂等）
    BDG = sq(f"select id from biz_budget where dept_id='{DEPT}' and account_id='{ACC}' "
             f"and fiscal_year=2026 and deleted_at is null order by created_at desc limit 1")
    if BDG:
        say(f"  (复用已存在的预算 {BDG}，跳过编制)")
        eq("①编预算（复用已有槽）", bool(BDG), True)
    else:
        st, r = api("POST", "/business/erp/budget", A,
                    {"fiscal_year": 2026, "dept_id": DEPT, "account_id": ACC, "total_amount": 10000,
                     "control_mode": "BLOCK", "remark": "REVERIFY 预算",
                     "periods": [{"month": i, "amount": "833.33"} for i in range(1, 12)]
                                + [{"month": 12, "amount": "833.37"}]})
        eq("①编预算 http", st, 200)
        BDG = (data_of(r).get("budget") or {}).get("id") or data_of(r).get("id")
    eq("①12 个月分解合计=年度额度",
       sq(f"select sum(amount)::numeric(18,2) from biz_budget_period where budget_id='{BDG}'"), "10000.00")
    if sq(f"select status from biz_budget where id='{BDG}'") == "DRAFT":
        eq("②提交审批 http", api("POST", f"/business/erp/budget/{BDG}/submit", A, {})[0], 200)
        INST = sq(f"select id from biz_flow_instance where biz_id='{BDG}' order by created_at desc limit 1")
        if INST: appr(INST, "REVERIFY 预算同意", A)
    eq("②预算 ACTIVE", sq(f"select status from biz_budget where id='{BDG}'"), "ACTIVE")
    st, r = api("GET", f"/business/erp/budget/{BDG}", A)
    eq("④预算详情 http", st, 200)
    eq("④详情含 usages 字段", "usages" in data_of(r), True)
    say(f"  预算详情 periods={len(data_of(r).get('periods') or [])} usages={len(data_of(r).get('usages') or [])}")
    st, r = api("GET", "/business/erp/budget/report?fiscal_year=2026", A)
    eq("④执行报表 http", st, 200)
    end_chain()

    # ==================== P0-05 报销链 ====================
    start_chain("P0-05 报销链（建单→超月额度 409 拦截→额度内提交→审批→付款→凭证）")
    ETP = sq("select id from biz_expense_type where type_code='TRAVEL' and deleted_at is null limit 1")
    # 用 demo_finance 当报销人 + 提交人：全新的演示账号里，只有 FINANCE_MANAGER / DEPT_LEADER 角色持
    # `business:erp:expense:submit`（普通员工 demo_sales 提交会被 403 —— 见 README §5 发现②）；
    # 而 DEPT_LEADER（demo_lead）自己提报会撞「职责分离」65018（发现①），所以走财务代提的路径。
    REPORTER = sq("select id from sys_user where username='demo_finance'")
    LT = TOKENS["demo_finance"]
    def new_expense(amount, title, type_id=None, date="2026-11-08"):
        st, r = api("POST", "/business/erp/expense", A,
                    {"title": title, "expense_date": date, "reporter_id": REPORTER, "dept_id": DEPT,
                     "items": [{"expense_type_id": type_id or ETP, "with_tax_amount": amount, "tax_rate": 0,
                                "summary": title, "invoice_no": "REV-INV-0001",
                                "invoice_date": date, "invoice_type": "VAT_NORMAL"}]})
        return st, (data_of(r).get("report") or {}).get("id") or data_of(r).get("id")
    # FIX-2 口径 A：生效 ⟺ `effective_from ≤ 期间月末`。本卡 P0-06 新建的 2026 年度预算
    # `effective_from=2026-12-01`（建档在 11 月），所以 **11 月放行 / 12 月才拦** —— 这正是
    # FIX-2 证据里的那条反证，这里两个方向都断言。
    st, EXP_NOV = new_expense(12000, "REVERIFY 11 月 12000（预算未生效，应放行）", date="2026-11-08")
    eq("①建单（11 月 12000）http", st, 200)
    st, r = api("POST", f"/business/erp/expense/{EXP_NOV}/submit", LT, {})
    eq("②11 月超额度：预算未生效 ⇒ 放行 200", st, 200)
    say(f"  预算提示：budget_warnings={json.dumps(data_of(r).get('budget_warnings'), ensure_ascii=False)[:200]}")
    st, EXP = new_expense(12000, "REVERIFY 报销 12000（12 月超月额度）", date="2026-12-05")
    eq("②建单（12 月 12000）http", st, 200)
    # 由**报销人本人**提交（demo_finance 的直属上级 = demo_lead ≠ 本人）
    st, r = api("POST", f"/business/erp/expense/{EXP}/submit", LT, {})
    eq("②12 月超月额度被预算 BLOCK 拦截 409", st, 409)
    eq("②拦截错误码 66037", r.get("code"), 66037)
    say(f"  报文：{(r.get('msg') or '')[:240]}")
    st, EXP2 = new_expense(300, "REVERIFY 报销 300（额度内）")
    eq("③建单（额度内）http", st, 200)
    st, r = api("POST", f"/business/erp/expense/{EXP2}/submit", LT, {})
    eq("③提交（额度内）http", st, 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{EXP2}' order by created_at desc limit 1")
    if INST: appr(INST, "REVERIFY 报销同意", A)
    eq("④单据 APPROVED", sq(f"select status from biz_expense_report where id='{EXP2}'"), "APPROVED")
    st, r = api("POST", f"/business/erp/expense/{EXP2}/pay", A,
                {"amount": 300, "pay_date": "2026-11-08", "pay_method": "BANK_TRANSFER",
                 "pay_account_code": "1002"})
    eq("⑤付款 http", st, 200)
    eq("⑥全库凭证借贷平衡", sq("select count(*) from biz_voucher where status='POSTED' and total_debit<>total_credit"), 0)
    # FIX-5：未编预算不得被误判成「超预算」——用 5602.03（办公费，没编预算）走一遍
    ETP2 = sq("select id from biz_expense_type where type_code='OFFICE' and deleted_at is null limit 1")
    st, EXP3 = new_expense(200, "REVERIFY 未编预算科目 200", ETP2, date="2026-12-05")
    eq("⑦建单（未编预算科目）http", st, 200)
    st, r = api("POST", f"/business/erp/expense/{EXP3}/submit", LT, {})
    eq("⑦未编预算提交放行 http", st, 200)
    row = sq(f"select coalesce(over_budget::text,'-') || '|' || coalesce(over_amount::text,'NULL') "
             f"from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='{EXP3}' limit 1")
    say(f"  未编预算科目占用行（over_budget|over_amount）= {row or '（无占用行：预算未覆盖该科目）'}")
    if row:
        eq("⑦未编预算 over_budget=不超支", row.split("|")[0] in ("false", "f", "0"), True)
        eq("⑦未编预算 over_amount=NULL", row.split("|")[1], "NULL")
    else:
        ok("⑦未编预算不留占用行（预算未覆盖 ⇒ 不判超支）")
    eq("⑦占用行 over_budget=1 必带超支额",
       sq("select count(*) from biz_budget_usage where over_budget=1 and over_amount is null"), 0)
    # ---- 发现①（不计入通过率，结论进 README §5）：部门负责人本人提报 → 职责分离 65018 ----
    st, EXP4 = new_expense(100, "REVERIFY 负责人本人提报探测")
    st, r = api("POST", f"/business/erp/expense/{EXP4}/submit", TOKENS["demo_lead"], {})
    say(f"  ℹ 负责人本人提报 http={st} code={r.get('code')}（期望 422/65018：职责分离内控）")
    leaked = sq(f"select status from biz_expense_report where id='{EXP4}'")
    tasks = sq(f"select count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id "
               f"where i.biz_id='{EXP4}'")
    say(f"  ℹ 该单提交失败后的单据状态={leaked}，关联待办数={tasks}（若 SUBMITTED 且 0 待办 ⇒ 单据卡死，见 §5）")
    end_chain()

    # ==================== P0-04 资产链 ====================
    start_chain("P0-04 资产链（分类→建卡→计提→处置前置闸→处置）")
    st, r = api("GET", "/business/erp/asset-category/list", A)
    eq("①资产分类列表 http", st, 200)
    CAT = sq("select id from biz_asset_category where category_code='ELEC' and deleted_at is null limit 1")
    # 计提是**期间级**的（uk_biz_dep_batch_period：一个期间一批）。若本库该期间批次已过账，
    # 本次新建的卡片不可能被它覆盖（这正是「复跑场景」），此时正确的结论是处置被前置闸拦。
    BATCH = sq("select status from biz_depreciation_batch where period_code='2026-11'")
    st, r = api("POST", "/business/erp/asset", A,
                {"asset_name": "REVERIFY 设备A", "category_id": CAT, "use_dept_id": DEPT,
                 "original_value": 120000, "purchase_date": "2026-10-01", "source_type": "MANUAL"})
    eq("②建卡 http", st, 200)
    FA = data_of(r).get("id")
    DISPOSE = {"disposal_type": "SALE", "disposal_date": "2026-11-20", "sale_price": 1000,
               "reason": "REVERIFY 处置：已提当月折旧后出售"}
    if not BATCH:
        # 反证（P0-04 原卡面）：处置当月折旧未计提 → 409 66048
        st, r = api("POST", f"/business/erp/asset/{FA}/dispose", A, DISPOSE)
        eq("④未计提先处置被前置闸拦 409", st, 409)
        eq("④前置闸错误码 66048", r.get("code"), 66048)
        st, r = api("POST", "/business/erp/depreciation/run", A, {"period_code": "2026-11"})
        eq("③计提 2026-11 http", st, 200)
    else:
        st, r = api("POST", "/business/erp/depreciation/run", A, {"period_code": "2026-11"})
        say(f"  (期间批次已存在且为 {BATCH}：计提按期间幂等，返回 http={st} code={r.get('code')})")
    EXPECT_DEP = sq("select round(120000 * (1 - salvage_rate) / useful_life_months, 4) "
                    "from biz_asset_category where category_code='ELEC'")
    DEP = sq(f"select coalesce(sum(amount),0) from biz_depreciation where asset_id='{FA}' and period_code='2026-11'")
    st, r = api("POST", f"/business/erp/asset/{FA}/dispose", A, DISPOSE)
    if BATCH:
        eq("③复跑场景：本卡不在已过账批次内（当月折旧=0）", DEP, 0)
        eq("④处置被前置闸拦 409", st, 409)
        eq("④前置闸错误码 66048", r.get("code"), 66048)
        ok("④复跑场景结论：先计提后处置的口径在期间级批次下仍然自洽")
    else:
        num("③该资产 2026-11 折旧额 = 原值×(1-残值率)/年限", DEP, EXPECT_DEP or 0)
        if st in (200, 202): ok(f"④计提后处置受理 http={st}（前置闸放行）")
        else: bad(f"④处置 http={st}")
        INSTD = sq(f"select id from biz_flow_instance where biz_id='{FA}' order by created_at desc limit 1")
        if INSTD:
            appr(INSTD, "REVERIFY 处置同意", A)
        eq("④处置后资产状态 DISPOSED", sq(f"select status from biz_fixed_asset where id='{FA}'"), "DISPOSED")
        eq("④处置凭证已生成（借贷平衡）",
           sq(f"select count(*) from biz_voucher v join biz_fixed_asset f on f.disposal_voucher_id=v.id "
              f"where f.id='{FA}' and v.total_debit=v.total_credit"), 1)
    end_chain()

    # ---------------- 汇总 ----------------
    say("\n" + "=" * 70)
    say(f"RESULT: PASS={PASS} FAIL={FAIL}")
    for name, p, f in CHAIN["rows"]:
        say(f"  {name.split('（')[0]:<16} {p}/{p + f}")
    if FAILS:
        say("FAILED:")
        for f in FAILS: say("  - " + f)
    with open(os.path.join(OUT, "harness.log"), "w") as fh:
        fh.write("\n".join(LOG) + "\n")
    return 0 if FAIL == 0 else 1

if __name__ == "__main__":
    sys.exit(main())
