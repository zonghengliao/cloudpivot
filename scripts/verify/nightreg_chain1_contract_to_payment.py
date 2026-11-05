#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""NIGHT-REGRESSION 链路① 真机：合同 → 采购单 → 收货 → 过账 → 应付 → 付款

为什么单独写一条（而不是复用别的卡脚本）：
  · D4 覆盖「采购单→收货→过账」，D5 覆盖「应付→付款」，P1-1 覆盖「合同审批」，
    但**没有任何一条脚本把「合同被采购单引用」这一步走通**（A3 起 purchase_type=CONTRACT
    必须给 contract_id，供应商从合同乙方派生）。本脚本补齐这一跳，并按真机链路一路走到付款。

每一步都打 `[PASS]/[FAIL]`，退出码 0 = 全过。

用法（隔离库 + 隔离实例，脚本自带前缀清场）：
    NIK_DATABASE_NAME=cloudpivot_nightreg_chain NIK_DATABASE_USER=alun \\
    NIK_DATABASE_PASSWORD=alun NIK_ADMIN_PASSWORD='Admin@123456' \\
      python3 scripts/verify/nightreg_chain1_contract_to_payment.py --port 8023
"""
from __future__ import annotations

import json
import time
import os
import subprocess
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402

_require_non_prod(script=os.path.basename(__file__))

BASE = os.environ.get("NR_BASE", "http://127.0.0.1:8023")
DB = {
    "host": os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    "port": os.environ.get("NIK_DATABASE_PORT", "5432"),
    "user": os.environ.get("NIK_DATABASE_USER", "alun"),
    "name": os.environ.get("NIK_DATABASE_NAME", "cloudpivot_nightreg_chain"),
    "password": os.environ.get("NIK_DATABASE_PASSWORD", "alun"),
}
ADMIN_USER = os.environ.get("NR_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("NIK_ADMIN_PASSWORD", "Admin@123456")
PREFIX = "NR-C1"

RESULTS: list[tuple[bool, str, str]] = []


# ------------------------------------------------------------------ 基础工具
def psql(sql: str) -> list[list[str]]:
    env = dict(os.environ, PGPASSWORD=DB["password"])
    proc = subprocess.run(
        ["psql", "-h", DB["host"], "-p", DB["port"], "-U", DB["user"], "-d", DB["name"],
         "-tAF", "\x1f", "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True, text=True, env=env, timeout=120,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip()[:400])
    return [line.split("\x1f") for line in proc.stdout.strip().split("\n") if line]


def one(sql: str, default: str = "") -> str:
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] else default


def http(method: str, path: str, body=None, token: str | None = None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode()
        try:
            return exc.code, json.loads(raw or "{}")
        except Exception:
            return exc.code, {"raw": raw}


def check(cond: bool, name: str, detail: str = "") -> None:
    RESULTS.append((bool(cond), name, str(detail)[:400]))
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}" + (f"  — {detail}" if detail else ""))


def login(username: str, password: str):
    st, body = http("POST", "/api/system/auth/login", {"username": username, "password": password})
    tok = ((body.get("data") or {}) if isinstance(body, dict) else {}).get("access_token")
    return st, body, tok


_TOKENS: dict[str, str] = {}


def token_for(username: str) -> str | None:
    """按需登录取 token。夹具账号的口令哈希统一成 admin 的（同一口令可登录）。

    登录有频控（10005「登录尝试过于频繁」）：批量跑夹具时会连续登录多个账号，
    命中后必须退避重试，否则 drive_approval 会「没人可批」而静默停在 PENDING。
    """
    if username in _TOKENS:
        return _TOKENS[username]

    def try_login():
        for attempt in range(6):
            st, body, tok = login(username, ADMIN_PASS)
            if tok:
                return tok
            msg = str((body or {}).get("msg", ""))
            if "频繁" in msg or st == 429:
                time.sleep(10)
                continue
            return None
        return None

    tok = try_login()
    if not tok:
        # 口令不是同一个 —— 对齐成 admin 的哈希再登（只在隔离库发生）
        psql(
            "update sys_user set password = (select password from sys_user where username = "
            f"'{ADMIN_USER}'), must_change_password = false where username = '{username}'"
        )
        tok = try_login()
    if tok:
        _TOKENS[username] = tok
    else:
        print(f"  ! 拿不到 {username} 的 token（跳过该审批人）")
    return tok


def instance_status(instance_id: str) -> str:
    if not instance_id:
        return "(空)"
    return one(f"select status from biz_flow_instance where id = '{instance_id}'::uuid")


def pending_tasks(instance_id: str) -> list[tuple[str, str]]:
    if not instance_id:
        return []
    rows = psql(
        "select t.id, coalesce(u.username, '') from biz_flow_task t "
        " left join sys_user u on u.id = t.assignee_id "
        f" where t.instance_id = '{instance_id}'::uuid and t.status = 'PENDING' "
        " order by t.created_at"
    )
    return [(row[0], row[1]) for row in rows]


def drive_approval(instance_id: str, skip_users: set[str] | None = None, limit: int = 12) -> str:
    """按待办上的真实 assignee 逐个投票，直到实例不再是 PENDING（或没人可批）。"""
    skip = skip_users or set()
    for _ in range(limit):
        status = instance_status(instance_id)
        if status != "PENDING":
            return status
        progressed = False
        for task_id, username in pending_tasks(instance_id):
            if not username or username in skip:
                continue
            tok = token_for(username)
            if not tok:
                continue
            code, _ = http("POST", f"/api/business/flow/task/{task_id}/approve",
                           {"comment": "同意（NIGHT-REGRESSION 链路①）"}, tok)
            if code == 200:
                progressed = True
        if not progressed:
            break
    return instance_status(instance_id)


def instance_of(biz_type: str, biz_id: str) -> str:
    return one(
        "select id from biz_flow_instance where deleted_at is null "
        f" and biz_type = '{biz_type}' and biz_id = '{biz_id}'::uuid "
        " order by created_at desc limit 1"
    )


def cleanup() -> None:
    """清掉上一轮 NR-C1 夹具（按前缀；先撤审批实例，再删单据）。"""
    stmts = [
        "update biz_flow_instance set status = 'CANCELLED', ended_at = now() "
        f" where status = 'PENDING' and biz_id in (select id from biz_purchase_order where remark like '{PREFIX}%')",
        "delete from biz_payment where payable_id in (select id from biz_payable where source_id in "
        f" (select id from biz_stock_in where remark like '{PREFIX}%'))",
        "delete from biz_payable where source_id in (select id from biz_stock_in where remark like "
        f" '{PREFIX}%')",
        "delete from biz_inventory_txn where doc_id in (select id from biz_stock_in where remark like "
        f" '{PREFIX}%')",
        "delete from biz_stock_in_item where stock_in_id in (select id from biz_stock_in where remark like "
        f" '{PREFIX}%')",
        f"delete from biz_stock_in where remark like '{PREFIX}%'",
        "delete from biz_purchase_order_item where order_id in (select id from biz_purchase_order where "
        f" remark like '{PREFIX}%')",
        f"delete from biz_purchase_order where remark like '{PREFIX}%'",
        f"delete from biz_contract where name like '{PREFIX}%'",
    ]
    for sql in stmts:
        try:
            psql(sql)
        except RuntimeError as exc:
            print(f"  ! 清场语句失败（继续）：{exc}")


def ensure_supplier() -> str:
    """合同乙方必须是一个真实供应商。已有就复用（按编码），没有就造一个（只动隔离库）。"""
    rows = psql(
        f"select id from biz_supplier where supplier_code = '{PREFIX}-SUP' and deleted_at is null"
    )
    if rows:
        psql(f"update biz_supplier set credit_limit = 1000000, credit_days = 30, status = 1 "
             f" where supplier_code = '{PREFIX}-SUP'")
        return rows[0][0]
    psql(
        "insert into biz_supplier (id, supplier_code, supplier_name, status, credit_days, credit_limit) "
        f"values (gen_random_uuid(), '{PREFIX}-SUP', '夜间回归链路①供应商', 1, 30, 1000000)"
    )
    return one(f"select id from biz_supplier where supplier_code = '{PREFIX}-SUP'")


# ------------------------------------------------------------------ 主流程
def main() -> int:
    print(f"== NIGHT-REGRESSION 链路① 合同 → 采购单 → 收货 → 过账 → 应付 → 付款 ==")
    print(f"   {BASE} / 库 {DB['name']}")
    st, body = http("GET", "/health")
    if st != 200:
        print("环境不满足：/health 不是 200")
        return 2

    st, body, admin_tok = login(ADMIN_USER, ADMIN_PASS)
    if not admin_tok:
        print(f"admin 登录失败：{st} {body}")
        return 2
    _TOKENS[ADMIN_USER] = admin_tok
    print("   admin 登录 OK")

    cleanup()

    mat = one("select id from biz_material where material_code = 'M-W1A-001' and deleted_at is null")
    wh = one("select id from biz_warehouse where warehouse_code = 'WH01'")
    sup = ensure_supplier()
    check(bool(mat) and bool(wh), "夹具：种子物料 M-W1A-001 / 仓库 WH01 存在",
          f"mat={mat[:8]} wh={wh[:8]}")
    check(bool(sup), "夹具：供应商（合同乙方）存在", PREFIX + "-SUP")

    # ---------------- ① 合同：建 → 提交审批 → 批完 → 签订 → 生效 ----------------
    print("\n== ① 合同 ==")
    st, body = http("POST", "/api/business/erp/contract", {
        "name": f"{PREFIX}-CONTRACT", "contract_type": "PURCHASE", "party_b_id": sup,
        "amount": 50000, "sign_date": "2026-01-01", "start_date": "2026-01-02",
        "end_date": "2026-12-31", "remark": f"{PREFIX} 夜间回归链路①",
    }, admin_tok)
    check(st == 200 and (body.get("data") or {}).get("id"), "① 建合同（草稿）", f"{st} {body.get('msg')}")
    cid = (body.get("data") or {}).get("id", "")
    cno = (body.get("data") or {}).get("contract_no", "")

    st, body = http("PATCH", f"/api/business/erp/contract/{cid}/status",
                    {"target_status": "PENDING_APPROVAL"}, admin_tok)
    check(st == 200, "① 提交合同审批", f"{st} {body.get('msg')}")
    c_inst = one(f"select coalesce(approval_instance_id::text,'') from biz_contract where id = '{cid}'::uuid")
    c_status = drive_approval(c_inst)
    check(c_status == "APPROVED", "① 合同审批实例终态 APPROVED", c_status)
    st, body = http("GET", f"/api/business/erp/contract/{cid}", None, admin_tok)
    detail = body.get("data") or {}
    check(detail.get("status") == "SIGNED", "① 审批通过 → 合同已签订", detail.get("status"))
    check(detail.get("usable_for_order") is True, "① 已签订 → 可被采购单引用",
          str(detail.get("usable_for_order")))

    # ---------------- ② 采购单：合同采购（供应商从合同派生） ----------------
    print("\n== ② 采购单（purchase_type=CONTRACT） ==")
    st, body = http("POST", "/api/business/erp/purchase-order", {
        "purchase_type": "CONTRACT", "contract_id": cid, "warehouse_id": wh,
        "price_mode": "EXCL", "remark": f"{PREFIX} 链路①",
        "items": [{"material_id": mat, "qty": "5", "price_excl": "1000", "tax_rate": "13"}],
    }, admin_tok)
    check(st == 200, "② 建采购单（只给 contract_id，供应商从合同派生）", f"{st} {body.get('msg')}")
    order = (body.get("data") or {}).get("order") or {}
    oid = order.get("id", "")
    row = psql(
        "select contract_id::text, coalesce(contract_no_snapshot,''), supplier_id::text, "
        " purchase_type, status from biz_purchase_order "
        f" where id = '{oid}'::uuid"
    )[0]
    check(row[0] == cid, "② 采购单落库 contract_id = 合同 id", row[0])
    check(row[1] == cno, "② 采购单落库 contract_no_snapshot = 合同号", f"{row[1]} vs {cno}")
    check(row[2] == sup, "② 供应商由合同乙方带出", row[2])
    check(row[3] == "CONTRACT", "② purchase_type = CONTRACT", row[3])

    st, body = http("POST", f"/api/business/erp/purchase-order/{oid}/submit-approval", {}, admin_tok)
    check(st == 200, "② 提交采购单审批", f"{st} {body.get('msg')}")
    o_inst = instance_of("PO_APPROVE", oid)
    o_status = drive_approval(o_inst)
    check(o_status == "APPROVED", "② 采购单审批实例终态 APPROVED", o_status)
    check(one(f"select status from biz_purchase_order where id = '{oid}'::uuid") == "APPROVED",
          "② 采购单状态 APPROVED")

    # ---------------- ③ 收货：从采购单生成收货单 ----------------
    print("\n== ③ 收货 ==")
    st, body = http("GET", f"/api/business/erp/purchase-order/{oid}/pending-items", None, admin_tok)
    items = (body.get("data") or {}).get("items") or []
    check(st == 200 and len(items) == 1, "③ 待收明细 1 行", f"{len(items)} 行")
    line = items[0]
    check(str(line.get("pending_qty")) in ("5", "5.0000"), "③ 待收 5 件", str(line.get("pending_qty")))
    st, body = http("POST", "/api/business/erp/stock-in", {
        "purchase_order_id": oid, "supplier_id": sup, "warehouse_id": wh,
        "invoice_received": True, "remark": f"{PREFIX} 收货",
        "items": [{
            "purchase_order_item_id": line.get("item_id"), "material_id": mat,
            "qty": line.get("pending_qty"), "price_excl": "1000", "tax_rate": "13",
        }],
    }, admin_tok)
    check(st == 200, "③ 由采购单生成收货单（带 purchase_order_id）", f"{st} {body.get('msg')}")
    header = (body.get("data") or {}).get("header") or {}
    in_id, in_no = header.get("id", ""), header.get("in_no", "")
    check(header.get("status") == "DRAFT", "③ 落 DRAFT（不自动过账）", header.get("status"))
    check(str((body.get("data") or {}).get("purchase_order_id")) == oid,
          "③ 收货单回填来源采购单", str((body.get("data") or {}).get("purchase_order_id")))

    # ---------------- ④ 过账：走审批（仓管主管） ----------------
    print("\n== ④ 过账 ==")
    st, body = http("PATCH", f"/api/business/erp/stock-in/{in_id}/status", {"status": "POSTED"}, admin_tok)
    check(st == 200, "④ 提交过账", f"{st} {body.get('msg')}")
    after_submit = one(f"select status from biz_stock_in where id = '{in_id}'::uuid")
    in_inst = one(f"select coalesce(approval_instance_id::text,'') from biz_stock_in where id = '{in_id}'::uuid")
    if after_submit == "PENDING_POST":
        check(True, "④ 提交后停在 PENDING_POST（等仓管主管复核）", after_submit)
        check(drive_approval(in_inst) == "APPROVED", "④ 过账审批实例 APPROVED")
    status = one(f"select status from biz_stock_in where id = '{in_id}'::uuid")
    check(status == "POSTED", "④ 收货单 POSTED", status)
    qty = one(f"select qty::text from biz_inventory where material_id = '{mat}'::uuid "
              f" and warehouse_id = '{wh}'::uuid and location_id is null")
    check(float(qty or 0) >= 5, "④ 库存 +5（成本账）", qty)
    check(one(f"select status from biz_purchase_order where id = '{oid}'::uuid") in
          ("RECEIVED", "PARTIAL_RECEIVED"), "④ 采购单前进到 RECEIVED/PARTIAL_RECEIVED",
          one(f"select status from biz_purchase_order where id = '{oid}'::uuid"))

    # ---------------- ⑤ 应付：过账自动生成应付单 ----------------
    print("\n== ⑤ 应付 ==")
    prow = psql(
        "select id::text, payable_no, amount::text, unpaid_amount::text, status, "
        " coalesce(source_no,''), coalesce(source_type,'') from biz_payable "
        f" where source_id = '{in_id}'::uuid and deleted_at is null"
    )
    check(bool(prow), "⑤ 过账生成应付单（source_id = 收货单）", f"{len(prow)} 行")
    pid = prow[0][0] if prow else ""
    if prow:
        amount = float(prow[0][2])
        check(amount == 5650.0, "⑤ 应付金额 = 5×1000×1.13 = 5650", prow[0][2])
        check(prow[0][3] == prow[0][2], "⑤ 未付 = 应付（还没付款）", f"{prow[0][3]}/{prow[0][2]}")
        check(prow[0][4] == "OPEN", "⑤ 应付状态 OPEN", prow[0][4])
        check(prow[0][5] == in_no, "⑤ 应付 source_no = 收货单号", f"{prow[0][5]} vs {in_no}")

    # ---------------- ⑥ 付款：走审批，直到 SETTLED ----------------
    print("\n== ⑥ 付款 ==")
    st, body = http("GET", f"/api/business/erp/payable/{pid}/payment-info", None, admin_tok)
    info = (body.get("data") or {}).get("payable") or {}
    unpaid = info.get("unpaid_amount")
    check(st == 200 and unpaid is not None and float(unpaid) > 0, "⑥ 付款预填未付金额",
          f"can_pay={info.get('can_pay')} unpaid={unpaid} default={info.get('default_amount')}")
    pay_amount = str(info.get("default_amount") or unpaid or "5650.0000")
    st, body = http("POST", f"/api/business/erp/payable/{pid}/pay", {"amount": pay_amount}, admin_tok)
    # 202 = 「已提交审批，批准后才记账」（付款必须走审批的硬红线），不是失败
    check(st in (200, 202), "⑥ 发起付款（200 直接 / 202 进审批）",
          f"{st} {body.get('msg')}")
    p_inst = one(f"select coalesce(payment_approval_instance_id::text,'') from biz_payable "
                 f" where id = '{pid}'::uuid")
    if p_inst:
        check(drive_approval(p_inst) == "APPROVED", "⑥ 付款审批实例 APPROVED")
    else:
        check(False, "⑥ 付款挂了审批实例（硬红线：付款必须走审批）", "无实例")
    final = psql(f"select status, paid_amount::text, unpaid_amount::text from biz_payable "
                 f" where id = '{pid}'::uuid")[0]
    check(final[0] == "SETTLED", "⑥ 应付状态 SETTLED", final[0])
    check(float(final[1]) == 5650.0 and float(final[2]) == 0.0,
          "⑥ 已付 5650 / 未付 0", f"{final[1]}/{final[2]}")
    pays = psql(f"select count(*) from biz_payment where payable_id = '{pid}'::uuid "
                f" and deleted_at is null")
    check(pays and int(pays[0][0]) >= 1, "⑥ biz_payment 留痕", pays[0][0] if pays else "0")

    failed = [r for r in RESULTS if not r[0]]
    print(f"\n== 汇总：{len(RESULTS) - len(failed)}/{len(RESULTS)} 条断言通过 ==")
    if failed:
        for _, name, detail in failed:
            print(f"  [FAIL] {name} — {detail}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
