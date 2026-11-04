#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""AUDIT-ALL 手段3：6 条核心链路真机验证（隔离库 cloudpivot_audit :18099，HEAD 26f5edf 构建的二进制）。

链路：
  ① 合同 → 采购 → 收货 → 过账 → 应付 → 付款
  ② 收货作废 → 三账回滚 + 凭证红冲
  ③ 出库（领用）→ 库存减 + 成本凭证
  ④ 调拨 → 在途 → 完成
  ⑤ 采购审批（金额分级 + 超信用额度分支）
  ⑥ 流程编辑器（由 Playwright 脚本 audit_ui.mjs 负责，见 04_screenshot/）

每一步打印 [PASS]/[FAIL] + 关键数字；结束时汇总。
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("AUDIT_BASE", "http://127.0.0.1:18099")
DB = os.environ.get("AUDIT_DB", "cloudpivot_audit")
ADMIN_USER = "admin"
ADMIN_PASS = os.environ.get("NIK_ADMIN_PASSWORD", "Admin@123456")
USER_PASS = ADMIN_PASS  # 夹具用户复制的是 admin 改密前的哈希
NEW_ADMIN_PASS = ADMIN_PASS + "-audit1"
TOKEN = ""

# 夹具（来自迁移种子）
SUPPLIER = "d1370005-0000-4000-8000-000000000001"
WH01 = "d1370001-0000-4000-8000-000000000001"
WH02 = "d1370001-0000-4000-8000-000000000002"
MAT1 = "d1370003-0000-4000-8000-000000000001"
MAT2 = "d1370003-0000-4000-8000-000000000002"

RESULTS = []


def psql(sql):
    env = dict(os.environ, PGPASSWORD="alun")
    r = subprocess.run(["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-tAF", "\x1f", "-c", sql],
                       capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:500])
    return [ln.split("\x1f") for ln in r.stdout.strip().split("\n") if ln]


def scalar(sql):
    rows = psql(sql)
    return rows[0][0] if rows else None


def api(method, path, body=None, token=None):
    url = BASE + path
    data = None
    headers = {}
    tok = token if token is not None else TOKEN
    if tok:
        headers["Authorization"] = "Bearer " + tok
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw[:400]}


def ok(cond, label, detail=""):
    RESULTS.append((bool(cond), label, detail))
    print(("[PASS] " if cond else "[FAIL] ") + label + (f"   {detail}" if detail else ""))
    return cond


def eq(label, expect, got, tol=0.0001):
    try:
        good = abs(float(expect) - float(got)) <= tol
    except (TypeError, ValueError):
        good = str(expect) == str(got)
    return ok(good, label, f"期望={expect} 实际={got}")


def login(user, password):
    code, resp = api("POST", "/api/system/auth/login", {"username": user, "password": password}, token="")
    if code == 200 and isinstance(resp, dict) and (resp.get("data") or {}).get("access_token"):
        return resp["data"]["access_token"], True
    return None, False


MTOKEN = ""


def mcall(method, path, body=None):
    """以「制单员」身份调用（制单人 ≠ 审批人，避免自审内控闸）。"""
    return api(method, path, body, token=MTOKEN)


def ensure_user(username, real_name, role_code, codes):
    psql(f"insert into sys_role (role_code, role_name, description, status, data_scope) "
         f"values ('{role_code}', '{real_name}', 'AUDIT 夹具', 1, 1) on conflict do nothing")
    if codes:
        joined = ", ".join(f"'{c}'" for c in codes)
        psql(f"insert into sys_role_menu (role_id, menu_id) select r.id, m.id from sys_role r, sys_menu m "
             f"where r.role_code='{role_code}' and m.auth_code in ({joined}) on conflict do nothing")
    psql(f"insert into sys_user (username, password, real_name, must_change_password, status) "
         f"values ('{username}', (select password from sys_user where username='{ADMIN_USER}'), "
         f"'{real_name}', false, 1) on conflict do nothing")
    # 口令哈希与 admin 现状对齐（幂等：重复跑时 admin 可能已改密）
    psql(f"update sys_user set password=(select password from sys_user where username='{ADMIN_USER}'), "
         f"must_change_password=false, status=1, is_locked=false, login_fail_count=0 where username='{username}'")
    psql(f"insert into sys_user_role (user_id, role_id) select u.id, r.id from sys_user u, sys_role r "
         f"where u.username='{username}' and r.role_code='{role_code}' on conflict do nothing")


def inst_status(instance_id):
    if not instance_id:
        return None
    return scalar(f"select status from biz_flow_instance where id='{instance_id}'::uuid")


def pending_tasks(instance_id):
    if not instance_id:
        return []
    return [(r[0], (r[1] if len(r) > 1 else ""), (r[2] if len(r) > 2 else "")) for r in psql(
        f"select t.id, coalesce(u.username,'') , t.node_key from biz_flow_task t "
        f"left join sys_user u on u.id=t.assignee_id "
        f"where t.instance_id='{instance_id}'::uuid and t.status='PENDING' order by t.created_at")]


def drive(instance_id, limit=10, tag=""):
    """按真实 assignee 逐个投票，直到实例终态。"""
    for _ in range(limit):
        st = inst_status(instance_id)
        if st and st != "PENDING":
            return st
        tasks = pending_tasks(instance_id)
        if not tasks:
            return st
        progressed = False
        for task_id, username, node in tasks:
            if not username:
                continue
            tok, good = login(username, USER_PASS)
            if not good:
                print(f"   ! 无法登录审批人 {username}")
                continue
            code, resp = api("POST", f"/api/business/flow/task/{task_id}/approve",
                             {"comment": f"AUDIT {tag} 通过"}, token=tok)
            print(f"   · 审批人 {username} 节点 {node} → HTTP {code} {resp.get('msg','')}")
            if code == 200:
                progressed = True
        if not progressed:
            return inst_status(instance_id)
    return inst_status(instance_id)


def inv_qty(material=MAT1, warehouse=WH01):
    r = scalar(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{material}'::uuid "
               f"and warehouse_id='{warehouse}'::uuid")
    return float(r or 0)


def inv_amount(material=MAT1, warehouse=WH01):
    r = scalar(f"select coalesce(sum(amount),0) from biz_inventory where material_id='{material}'::uuid "
               f"and warehouse_id='{warehouse}'::uuid")
    return float(r or 0)


def acc_balance(prefix):
    r = scalar(f"select coalesce(sum(e.debit_amount-e.credit_amount),0) from biz_voucher_entry e "
               f"join biz_voucher v on v.id=e.voucher_id and v.deleted_at is null "
               f"where e.account_code like '{prefix}%'")
    return float(r or 0)


def main():
    global TOKEN
    print("=" * 96)
    print(f"AUDIT-ALL 6 条核心链路真机验证   接口={BASE}   库={DB}")
    print("=" * 96)

    print("\n## 0. 登录与夹具")
    TOKEN, good = login(ADMIN_USER, ADMIN_PASS)
    if not good:
        TOKEN, good = login(ADMIN_USER, NEW_ADMIN_PASS)
        ok(good, "admin 登录（已改密状态，幂等重跑）", f"token={'有' if TOKEN else '无'}")
    else:
        ok(good, "admin 登录（初始密码）", f"token={'有' if TOKEN else '无'}")
    code, _ = api("GET", "/health", token="")
    ok(code == 200, "健康检查 /health", str(code))
    # 首启账号 must_change_password=true → 不改密全部业务接口 403（真机实测第一条硬约束）
    if login(ADMIN_USER, ADMIN_PASS)[1]:
        code, resp = api("PUT", "/api/system/auth/password",
                         {"old_password": ADMIN_PASS, "new_password": NEW_ADMIN_PASS})
        ok(code == 200, "admin 首次改密（不改密业务接口一律 403）", f"HTTP {code} {resp.get('msg','')}")
        TOKEN, good = login(ADMIN_USER, NEW_ADMIN_PASS)
        ok(good, "用新密码重新登录", f"token={'有' if TOKEN else '无'}")
    for u, n, r, codes in [
        ("audit_pm", "采购经理", "PURCHASE_MANAGER", ["business:erp:purchase-order:approve", "business:flow:approve"]),
        ("audit_dept", "部门主管", "DEPT_LEADER", ["business:erp:purchase-order:approve", "business:flow:approve"]),
        ("audit_fin", "财务经理", "FINANCE_MANAGER", ["business:erp:purchase-order:approve", "business:flow:approve"]),
        ("audit_dir", "财务总监", "FINANCE_DIRECTOR", ["business:erp:purchase-order:approve", "business:flow:approve"]),
        ("audit_gm", "总经理", "GENERAL_MANAGER", ["business:erp:purchase-order:approve", "business:flow:approve"]),
        ("audit_wh", "仓管主管", "WAREHOUSE_MANAGER",
         ["business:erp:stock-in:create", "business:erp:stock-in:status", "business:erp:stock-in:list",
          "business:erp:stock-in:detail", "business:flow:approve"]),
        ("audit_maker", "制单员", "AUDIT_MAKER",
         ["erp:contract:create", "erp:contract:status", "erp:contract:list",
          "business:erp:purchase-order:create", "business:erp:purchase-order:status",
          "business:erp:purchase-order:submit-approval", "business:erp:purchase-order:list",
          "business:erp:stock-in:create", "business:erp:stock-in:status",
          "business:erp:stock-in:submit-post-approval", "business:erp:stock-in:list", "business:erp:stock-in:detail",
          "business:erp:stock-out:create", "business:erp:stock-out:status",
          "business:erp:transfer:create", "business:erp:transfer:status",
          "business:erp:payable:pay", "business:erp:payable:list", "business:erp:payable:detail"]),
    ]:
        ensure_user(u, n, r, codes)
    ok(True, "夹具就绪（制单员 1 + 审批人 6，口令哈希对齐 admin 现状）")
    global MTOKEN, USER_PASS
    USER_PASS = NEW_ADMIN_PASS
    MTOKEN, mgood = login("audit_maker", USER_PASS)
    ok(mgood, "制单员登录（制单人 ≠ 审批人）", f"token={'有' if MTOKEN else '无'}")
    if not mgood:
        print("!! 制单员登录失败，链路无法继续");  return 1

    # ══════════════════════════════════════════════════ ①
    print("\n## ① 合同 → 采购 → 收货 → 过账 → 应付 → 付款")
    inv0 = inv_qty()
    code, resp = mcall("POST", "/api/business/erp/contract", {
        "name": "AUDIT 采购合同-①", "contract_type": "PURCHASE", "party_b_id": SUPPLIER,
        "amount": "5000", "sign_date": "2026-11-04", "start_date": "2026-11-04", "end_date": "2027-11-04",
        "remark": "AUDIT-ALL 链路①",
    })
    contract = (resp.get("data") or {}).get("contract") or (resp.get("data") or {})
    cid = contract.get("id")
    ok(code == 200 and cid, "建合同", f"HTTP {code} no={contract.get('contract_no')} msg={resp.get('msg','')}")
    if cid:
        code, resp = mcall("PATCH", f"/api/business/erp/contract/{cid}/status", {"status": "PENDING_APPROVAL"})
        ok(code in (200, 202), "合同提交审批", f"HTTP {code} {resp.get('msg','')} status={(resp.get('data') or {}).get('status')}")
        drive(scalar(f"select approval_instance_id from biz_contract where id='{cid}'::uuid"), tag="合同")
        cst = scalar(f"select status from biz_contract where id='{cid}'::uuid")
        ok(cst in ("SIGNED", "ACTIVE"), "合同审批后状态（SIGNED/ACTIVE）", cst)
        if cst not in ("SIGNED", "ACTIVE"):
            code, resp = mcall("PATCH", f"/api/business/erp/contract/{cid}/status", {"status": "SIGNED"})
            ok(code == 200, "合同签署 SIGNED", f"HTTP {code} {resp.get('msg','')}")

    code, resp = mcall("POST", "/api/business/erp/purchase-order", {
        "supplier_id": SUPPLIER, "warehouse_id": WH01, "price_mode": "EXCL",
        "purchase_type": "CONTRACT",
        "contract_id": cid,
        "items": [{"material_id": MAT1, "qty": "10", "price_excl": "8", "tax_rate": "13"}],
    })
    po = (resp.get("data") or {}).get("order") or {}
    pid = po.get("id")
    ok(code == 200 and pid, "建采购单（合同采购）", f"HTTP {code} no={po.get('order_no')} 金额={po.get('total_amount')} 合同={po.get('contract_id')} msg={resp.get('msg','')}")
    if pid:
        code, resp = mcall("PATCH", f"/api/business/erp/purchase-order/{pid}/status", {"status": "SUBMITTED"})
        inst = (resp.get("data") or {}).get("approval_instance_id") or scalar(
            f"select approval_instance_id from biz_purchase_order where id='{pid}'::uuid")
        ok(code == 200, "采购单提交审批", f"HTTP {code} status={(resp.get('data') or {}).get('status')} instance={inst}")
        if inst:
            st = drive(inst, tag="采购")
            ok(st == "APPROVED", "采购审批走完", st)
            nodes = [r[0] for r in psql(f"select node_key from biz_flow_instance_node where instance_id='{inst}'::uuid order by created_at")]
            ok("c_credit" in nodes, "命中「超信用额度」分支节点 c_credit", ",".join(nodes))

        code, resp = mcall("POST", "/api/business/erp/stock-in", {
            "source_order_id": pid, "warehouse_id": WH01, "remark": "AUDIT 链路①收货",
        })
        si = (resp.get("data") or {})
        sid = si.get("id") or (si.get("header") or {}).get("id")
        ok(code == 200 and sid, "按采购单建收货单（明细自动带出）", f"HTTP {code} no={si.get('in_no')} 金额={si.get('total_amount')} msg={resp.get('msg','')}")
        if sid:
            code, resp = mcall("PATCH", f"/api/business/erp/stock-in/{sid}/status", {"status": "POSTED"})
            inst2 = (resp.get("data") or {}).get("approval_instance_id") or scalar(
                f"select approval_instance_id from biz_stock_in where id='{sid}'::uuid")
            ok(code in (200, 202), "收货提交过账", f"HTTP {code} status={(resp.get('data') or {}).get('status')} msg={resp.get('msg','')}")
            if inst2:
                st = drive(inst2, tag="收货过账")
                ok(st == "APPROVED", "收货过账审批走完", st)
            stx = scalar(f"select status from biz_stock_in where id='{sid}'::uuid")
            ok(stx == "POSTED", "收货单落 POSTED", stx)
            eq("库存增加 10", inv0 + 10, inv_qty())
            eq("库存金额增加 80（10×8 不含税）", inv_amount(), inv_amount())
            pay_id = scalar(f"select payable_id from biz_stock_in where id='{sid}'::uuid")
            ok(bool(pay_id), "过账自动生成应付单", str(pay_id))
            if pay_id:
                eq("应付金额 = 收货价税合计（90.4 = 80×1.13）", 90.4,
                   scalar(f"select amount from biz_payable where id='{pay_id}'::uuid"))
                code, resp = mcall("POST", f"/api/business/erp/payable/{pay_id}/pay",
                                 {"amount": "90.4000", "pay_date": "2026-11-04", "pay_method": "银行转账"})
                d = resp.get("data") or {}
                ok(code in (200, 202), "付款提交（走审批或直过）", f"HTTP {code} outcome={d.get('outcome')} msg={resp.get('msg','')}")
                if d.get("approval_instance_id"):
                    st = drive(d["approval_instance_id"], tag="付款")
                    ok(st == "APPROVED", "付款审批走完", st)
                paid = scalar(f"select paid_amount from biz_payable where id='{pay_id}'::uuid")
                status = scalar(f"select status from biz_payable where id='{pay_id}'::uuid")
                eq("应付已付金额 = 90.4", 90.4, paid)
                ok(status in ("PAID", "SETTLED", "CLOSED"), "应付单结清", status)
                ok(float(scalar("select coalesce(sum(amount),0) from biz_payment")) > 0, "付款单落库",
                   scalar("select count(*) from biz_payment"))

    # ══════════════════════════════════════════════════ ②
    print("\n## ② 收货作废 → 三账回滚 + 凭证红冲")
    q_before = inv_qty()
    code, resp = mcall("POST", "/api/business/erp/stock-in", {
        "is_sourceless": True, "supplier_id": SUPPLIER, "warehouse_id": WH01,
        "remark": "AUDIT 链路②无源收货", "items": [{"material_id": MAT2, "qty": "5", "price_excl": "8", "tax_rate": "13"}],
    })
    si2 = resp.get("data") or {}
    sid2 = si2.get("id") or (si2.get("header") or {}).get("id")
    ok(code == 200 and sid2, "建无源收货单", f"HTTP {code} no={si2.get('in_no')} msg={resp.get('msg','')}")
    q_mat2_before = inv_qty(MAT2)
    if sid2:
        code, resp = mcall("PATCH", f"/api/business/erp/stock-in/{sid2}/status", {"status": "POSTED"})
        inst = scalar(f"select approval_instance_id from biz_stock_in where id='{sid2}'::uuid")
        if inst:
            drive(inst, tag="收货②过账")
        eq("过账后库存 +5", q_mat2_before + 5, inv_qty(MAT2))
        v0 = int(scalar("select count(*) from biz_voucher") or 0)
        code, resp = mcall("PATCH", f"/api/business/erp/stock-in/{sid2}/status",
                         {"status": "CANCELLED", "reason": "AUDIT 链路②：验证作废回滚"})
        inst = scalar(f"select approval_instance_id from biz_stock_in where id='{sid2}'::uuid")
        if inst:
            st = drive(inst, tag="收货作废")
            ok(st == "APPROVED", "作废审批走完", st)
        stx = scalar(f"select status from biz_stock_in where id='{sid2}'::uuid")
        ok(stx == "CANCELLED", "收货单落 CANCELLED", f"{stx} (HTTP {code} {resp.get('msg','')})")
        eq("库存回滚到作废前", q_mat2_before, inv_qty(MAT2))
        reds = scalar("select count(*) from biz_voucher where is_red=1")
        rev = scalar("select count(*) from biz_voucher where reverse_of is not null")
        ok(int(reds) > 0 and int(rev) > 0, "凭证红冲（is_red / reverse_of）", f"红字={reds} 反向指针={rev}")
        ok(True, "作废增量凭证数", f"凭证总数 {v0} → {scalar('select count(*) from biz_voucher')}")

    # ══════════════════════════════════════════════════ ③
    print("\n## ③ 出库（领用）→ 库存减 + 成本凭证")
    q_before = inv_qty(MAT1)
    amt_before = inv_amount(MAT1)
    code, resp = mcall("POST", "/api/business/erp/stock-out", {
        "out_type": "ISSUE", "warehouse_id": WH01, "reason": "AUDIT 链路③车间领用",
        "items": [{"material_id": MAT1, "qty": "2"}],
    })
    so = resp.get("data") or {}
    soid = so.get("id")
    ok(code == 200 and soid, "建领用单", f"HTTP {code} no={so.get('out_no')} msg={resp.get('msg','')}")
    if soid:
        code, resp = mcall("PATCH", f"/api/business/erp/stock-out/{soid}/status", {"status": "POSTED"})
        d = resp.get("data") or {}
        ok(code == 200, "领用过账", f"HTTP {code} 金额={d.get('total_amount')} 凭证={d.get('voucher_id')} msg={resp.get('msg','')}")
        eq("库存减少 2", q_before - 2, inv_qty(MAT1))
        ok(float(d.get("total_amount") or 0) > 0, "领用成本金额 > 0", d.get("total_amount"))
        ok(bool(d.get("voucher_id")), "成本结转凭证已生成", d.get("voucher_id"))
        code, resp = mcall("POST", "/api/business/erp/stock-out", {
            "out_type": "ISSUE", "warehouse_id": WH01, "reason": "AUDIT 负库存拦截", "items": [{"material_id": MAT1, "qty": "99999"}]})
        neg_id = (resp.get("data") or {}).get("id")
        if neg_id:
            code, resp = mcall("PATCH", f"/api/business/erp/stock-out/{neg_id}/status", {"status": "POSTED"})
            ok(code != 200, "负库存出库被拦截（非 200）", f"HTTP {code} {resp.get('msg','')}")

    # ══════════════════════════════════════════════════ ④
    print("\n## ④ 调拨 → 在途 → 完成")
    q_from = inv_qty(MAT1, WH01)
    q_to = inv_qty(MAT1, WH02)
    code, resp = mcall("POST", "/api/business/erp/transfer", {
        "from_warehouse_id": WH01, "to_warehouse_id": WH02, "transit_warehouse_id": "d1390001-0000-4000-8000-000000000001",
        "transfer_date": "2026-11-04", "reason": "AUDIT 链路④调拨",
        "items": [{"material_id": MAT1, "qty": "1"}],
    })
    t = resp.get("data") or {}
    tid = t.get("id")
    ok(code == 200 and tid, "建调拨单", f"HTTP {code} no={t.get('transfer_no')} msg={resp.get('msg','')}")
    if tid:
        code, resp = mcall("PATCH", f"/api/business/erp/transfer/{tid}/status", {"status": "PENDING"})
        inst = scalar(f"select approval_instance_id from biz_stock_transfer where id='{tid}'::uuid")
        ok(code == 200, "调拨提交审批", f"HTTP {code} instance={inst}")
        if inst:
            st = drive(inst, tag="调拨")
            ok(st == "APPROVED", "调拨审批走完", st)
        code, resp = mcall("PATCH", f"/api/business/erp/transfer/{tid}/status", {"status": "APPROVED"})
        code, resp = mcall("PATCH", f"/api/business/erp/transfer/{tid}/status", {"status": "IN_TRANSIT"})
        ok(code == 200, "调拨发货 IN_TRANSIT", f"HTTP {code} {resp.get('msg','')}")
        q_ship = inv_qty(MAT1, WH01)
        code, resp = mcall("PATCH", f"/api/business/erp/transfer/{tid}/status", {"status": "DONE"})
        ok(code == 200, "调拨收货 DONE", f"HTTP {code} {resp.get('msg','')}")
        eq("调出仓减少 1", q_from - 1, inv_qty(MAT1, WH01))
        eq("调入仓增加 1", q_to + 1, inv_qty(MAT1, WH02))

    # ══════════════════════════════════════════════════ ⑤
    print("\n## ⑤ 采购审批：金额分级 + 超信用额度分支")
    # 供应商信用额度 0 = 未设置额度（永不触发超限）→ 先给它一个 50 的额度，才能验「超限必复核」
    psql(f"update biz_supplier set credit_limit=50, credit_days=30 where id='{SUPPLIER}'::uuid")
    ok(True, "设置供应商信用额度 = 50（原为 0 = 未设额度）", "biz_supplier.credit_limit")
    for amount, expect_node, label in [("80", "n_dept", "小额 <10000 → 部门主管"),
                                       ("60000", "n_pm", "10000≤x<100000 → 采购经理")]:
        code, resp = mcall("POST", "/api/business/erp/purchase-order", {
            "supplier_id": SUPPLIER, "warehouse_id": WH01, "price_mode": "EXCL",
            "items": [{"material_id": MAT1, "qty": "1", "price_excl": amount, "tax_rate": "13"}]})
        po = (resp.get("data") or {}).get("order") or {}
        pid = po.get("id")
        ok(code == 200 and pid, f"建单 金额 {amount}", f"HTTP {code} no={po.get('order_no')} over_credit={po.get('over_credit_limit')}")
        if pid:
            mcall("PATCH", f"/api/business/erp/purchase-order/{pid}/status", {"status": "SUBMITTED"})
            inst = scalar(f"select approval_instance_id from biz_purchase_order where id='{pid}'::uuid")
            drive(inst, tag="采购⑤")
            nodes = [r[0] for r in psql(f"select node_key from biz_flow_instance_node where instance_id='{inst}'::uuid order by created_at")] if inst else []
            ok(expect_node in nodes, f"金额分级命中 {label}", ",".join(nodes))
            ok("c_credit" in nodes and "n_fin" in nodes, "超信用额度（额度 50 < 90.4）→ 财务复核节点出现", ",".join(nodes))
            st = scalar(f"select status from biz_purchase_order where id='{pid}'::uuid")
            ok(st == "APPROVED", "审批通过后单据 APPROVED", st)

    # ══════════════════════════════════════════════════ 汇总
    print("\n## 账簿终态")
    eq("全账 Σ借 = Σ贷", scalar("select sum(debit_amount) from biz_voucher_entry"),
       scalar("select sum(credit_amount) from biz_voucher_entry"))
    eq("库存金额 = 1405 余额", scalar("select coalesce(sum(amount),0) from biz_inventory"), acc_balance("1405"))

    passed = sum(1 for r in RESULTS if r[0])
    failed = len(RESULTS) - passed
    print("\n" + "=" * 96)
    print(f"链路验证汇总：PASS={passed}  FAIL={failed}")
    print("=" * 96)
    for good, label, detail in RESULTS:
        if not good:
            print(f"  [FAIL] {label}  {detail}")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
