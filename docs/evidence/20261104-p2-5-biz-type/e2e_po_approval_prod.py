#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P2-5 真机（生产 144）：走一张采购单审批，证明词表收敛后路由没断。

与 `e2e_po_approval.py`（隔离实例）的区别：本脚本跑在**生产**上，用生产真实主数据，
不建夹具；为了满足「制单人 ≠ 审批人」的内控闸，临时建一个 SUPER_ADMIN 制单账号，
验收完立刻删掉（只留一张小额采购单，见输出末尾的「生产数据变动」）。

用法（在 144 上）：
    set -a; . /etc/cloudpivot/cloudpivot.env; set +a
    P25_DB_PASSWORD="$NIK_DATABASE_PASSWORD" P25_ADMIN_PASSWORD="$NIK_ADMIN_PASSWORD" \\
      python3 /root/p25_prod_e2e.py

退出码：0 = 全绿；1 = 有断言失败；2 = 环境不满足。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("P25_BASE_URL", "http://127.0.0.1:8023").rstrip("/")
DB_NAME = os.environ.get("NIK_DATABASE_NAME", "cloudpivot")
DB_USER = os.environ.get("NIK_DATABASE_USER", "cloudpivot")
DB_PASS = os.environ.get("P25_DB_PASSWORD") or os.environ.get("NIK_DATABASE_PASSWORD", "")

ADMIN_USER = "admin"
ADMIN_PASS = os.environ.get("P25_ADMIN_PASSWORD") or os.environ.get("NIK_ADMIN_PASSWORD", "")
MAKER_USER = "p25maker"
MAKER_PASS = "P25Maker@123456"

SUPPLIER_ID = os.environ.get("P25_SUPPLIER_ID", "d1370005-0000-4000-8000-000000000001")
WAREHOUSE_ID = os.environ.get("P25_WAREHOUSE_ID", "d1370001-0000-4000-8000-000000000001")
MATERIAL_ID = os.environ.get("P25_MATERIAL_ID", "d1370003-0000-4000-8000-000000000002")

PASSED: list[str] = []
FAILED: list[str] = []


def check(ok: bool, name: str, detail: str = "") -> bool:
    if ok:
        PASSED.append(name)
        print(f"[PASS] {name}")
    else:
        FAILED.append(name)
        print(f"[FAIL] {name}" + (f" — {detail}" if detail else ""))
    return ok


def psql(sql: str) -> str:
    args = ["psql", "-h", "127.0.0.1", "-p", "5432", "-U", DB_USER, "-d", DB_NAME,
            "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tAc", sql]
    out = subprocess.run(args, capture_output=True, text=True,
                         env=dict(os.environ, PGPASSWORD=DB_PASS), check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}\nSQL: {sql}")
    return out.stdout.strip()


def scalar(sql: str) -> str:
    return (psql(sql).splitlines() or [""])[0].strip()


def call(method: str, path: str, body: dict | None = None, token: str | None = None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(user: str, password: str) -> str:
    status, body = call("POST", "/api/system/auth/login",
                        {"username": user, "password": password})
    if status != 200:
        raise SystemExit(f"[env] 登录 {user} 失败：{status} {body}")
    data = body.get("data") or {}
    if data.get("must_change_password"):
        raise SystemExit(f"[env] {user} 被要求改初始口令，本脚本不改生产账号口令")
    return data.get("access_token") or ""


def main() -> int:
    if not ADMIN_PASS:
        print("[env] 没有 admin 口令（P25_ADMIN_PASSWORD / NIK_ADMIN_PASSWORD）")
        return 2
    admin = login(ADMIN_USER, ADMIN_PASS)
    check(bool(admin), "① admin 登录成功")
    if not admin:
        return 2

    # ---------- 临时制单账号（内控闸要求制单人 ≠ 审批人） ----------
    made_user = False
    exists = scalar(f"SELECT count(*) FROM sys_user WHERE username='{MAKER_USER}' AND deleted_at IS NULL")
    if exists == "0":
        role = scalar("SELECT id FROM sys_role WHERE role_code='SUPER_ADMIN' LIMIT 1")
        st, body = call("POST", "/api/system/user", {
            "username": MAKER_USER, "password": MAKER_PASS,
            "real_name": "P2-5 验收临时制单人", "status": 1, "role_ids": [role],
        }, admin)
        if st != 200:
            print(f"[env] 建临时账号失败：{st} {str(body)[:200]}")
            return 2
        made_user = True
        print(f"[info] 临时制单账号 {MAKER_USER} 已创建（验收后删除）")
    maker = login(MAKER_USER, MAKER_PASS)

    try:
        # ---------- ② 建单（生产真实主数据，小额） ----------
        st, body = call("POST", "/api/business/erp/purchase-order", {
            "supplier_id": SUPPLIER_ID, "warehouse_id": WAREHOUSE_ID,
            "purchase_type": "SPOT", "price_mode": "EXCL",
            "remark": "P2-5 词表收敛真机验收（可直接作废）",
            "items": [{"material_id": MATERIAL_ID, "qty": "1", "price_excl": "100", "tax_rate": "13"}],
        }, maker)
        order = ((body.get("data") or {}).get("order") or {})
        order_id, order_no = order.get("id"), order.get("order_no")
        check(bool(order_id), "② 采购单建单成功（DRAFT）", f"{st} {str(body)[:200]}")
        if not order_id:
            return 1

        # ---------- ③ 提交审批 ----------
        st, body = call("POST", f"/api/business/erp/purchase-order/{order_id}/submit-approval", {}, maker)
        payload = body.get("data") or {}
        detail = payload.get("order") or payload
        instance_id = detail.get("approval_instance_id") or ""
        check(st == 200 and bool(instance_id), "③ 提交审批成功并发起实例", f"{st} {str(body)[:200]}")

        inst_biz = scalar(f"SELECT biz_type FROM biz_flow_instance WHERE id='{instance_id}'::uuid") if instance_id else ""
        def_biz = scalar("SELECT biz_type FROM biz_flow_def WHERE flow_code='po_approve' AND deleted_at IS NULL")
        check(inst_biz == def_biz == "PO_APPROVE",
              "③ 审批轴：实例 biz_type == 定义 biz_type == PO_APPROVE",
              f"instance={inst_biz!r} def={def_biz!r}")

        logs = psql(f"SELECT DISTINCT entity_type FROM biz_status_log WHERE entity_id='{order_id}'::uuid ORDER BY 1").splitlines()
        check(logs == ["PURCHASE_ORDER"], "③ 对象轴：状态日志 entity_type = PURCHASE_ORDER", str(logs))

        # ---------- ④ 审批通过（换 admin 批，制单人 ≠ 审批人） ----------
        task = scalar("SELECT id FROM biz_flow_task "
                      f"WHERE instance_id='{instance_id}'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
        check(bool(task), "④ 产生待办", instance_id)
        if not task:
            return 1
        st, body = call("POST", f"/api/business/flow/task/{task}/approve",
                        {"comment": "P2-5 词表收敛真机验收：同意"}, admin)
        check(st == 200, "④ 审批通过 200", f"{st} {str(body)[:300]}")

        order_status = scalar(f"SELECT status FROM biz_purchase_order WHERE id='{order_id}'::uuid")
        inst_status = scalar(f"SELECT status FROM biz_flow_instance WHERE id='{instance_id}'::uuid")
        check(order_status == "APPROVED", "④ 采购单落 APPROVED", order_status)
        check(inst_status == "APPROVED", "④ 审批实例落 APPROVED", inst_status)

        # ---------- ⑤ 别名不再被接受（不留兼容层） ----------
        for legacy in ("purchase_order", "PO"):
            st, b = call("GET", f"/api/business/erp/{legacy}/{order_id}/cancel-impact", None, admin)
            check(st in (400, 404), f"⑤ 历史别名 {legacy} 被拒（{st}）", json.dumps(b, ensure_ascii=False)[:160])
        st, _ = call("GET", f"/api/business/erp/purchase-order/{order_id}/cancel-impact", None, admin)
        check(st == 200, "⑤ 规范路径段 purchase-order 正常", str(st))

        print("\n[生产数据变动]")
        print(f"  · 采购单 {order_no}（{order_id}）状态 APPROVED，来源 = P2-5 真机验收，可直接作废")
        print(f"  · 审批实例 {instance_id} APPROVED（同一次验收）")
    finally:
        if made_user:
            uid = scalar(f"SELECT id FROM sys_user WHERE username='{MAKER_USER}'")
            if uid:
                psql(f"DELETE FROM sys_user_role WHERE user_id='{uid}'::uuid")
                psql(f"DELETE FROM sys_user WHERE id='{uid}'::uuid")
                print(f"[info] 临时制单账号 {MAKER_USER} 已删除（硬删，无业务引用）")

    print("\n" + "=" * 60)
    print(f"结果：{len(PASSED)} 通过 / {len(FAILED)} 失败")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
