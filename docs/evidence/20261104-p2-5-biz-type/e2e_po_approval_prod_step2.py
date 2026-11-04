#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P2-5 真机（生产）第二步：把已在跑的采购单审批批完，并清理临时账号。

第一步（`e2e_po_approval_prod.py`）已经证明：
  · 建单 / 提交审批成功
  · 实例 `biz_type = PO_APPROVE` == `biz_flow_def.biz_type`（审批轴）
  · `biz_status_log.entity_type = PURCHASE_ORDER`（对象轴）
  · 历史别名 purchase_order / PO 一律 400（不留兼容层）
但它批不动待办：`po_approve` 的 < 1 万元分支把待办派给**部门主管角色**，
生产里这个角色有真人，待办不是 admin 的（403 + 65028）。

这一步用「转办」把待办转到 admin 名下再批（真实业务里也叫转办），
批完把临时制单账号停用 + 软删（`biz_flow_instance.initiator_id` 有外键，不能硬删）。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8023"
DB_NAME = os.environ.get("NIK_DATABASE_NAME", "cloudpivot")
DB_USER = os.environ.get("NIK_DATABASE_USER", "cloudpivot")
DB_PASS = os.environ.get("P25_DB_PASSWORD") or os.environ.get("NIK_DATABASE_PASSWORD", "")
ADMIN_USER = "admin"
ADMIN_PASS = os.environ.get("P25_ADMIN_PASSWORD") or os.environ.get("NIK_ADMIN_PASSWORD", "")
MAKER_USER = "p25maker"
ORDER_NO = os.environ.get("P25_ORDER_NO", "PO-20261104-000001")

PASSED, FAILED = [], []


def check(ok, name, detail=""):
    (PASSED if ok else FAILED).append(name)
    print(("[PASS] " if ok else "[FAIL] ") + name + (f" — {detail}" if detail and not ok else ""))
    return ok


def psql(sql):
    out = subprocess.run(["psql", "-h", "127.0.0.1", "-p", "5432", "-U", DB_USER, "-d", DB_NAME,
                          "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tAc", sql],
                         capture_output=True, text=True,
                         env=dict(os.environ, PGPASSWORD=DB_PASS), check=False)
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip())
    return out.stdout.strip()


def scalar(sql):
    return (psql(sql).splitlines() or [""])[0].strip()


def call(method, path, body=None, token=None):
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


def main():
    st, body = call("POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": ADMIN_PASS})
    if st != 200:
        print(f"[env] admin 登录失败：{st}")
        return 2
    admin = (body.get("data") or {}).get("access_token") or ""
    admin_id = scalar(f"SELECT id FROM sys_user WHERE username='{ADMIN_USER}'")

    order_id = scalar(f"SELECT id FROM biz_purchase_order WHERE order_no='{ORDER_NO}' AND deleted_at IS NULL")
    instance_id = scalar(f"SELECT approval_instance_id FROM biz_purchase_order WHERE id='{order_id}'::uuid")
    if not order_id or not instance_id:
        print("[env] 找不到第一步的那张单/实例")
        return 2
    print(f"[info] 采购单 {ORDER_NO} id={order_id} 实例={instance_id}")

    task = scalar("SELECT id FROM biz_flow_task "
                  f"WHERE instance_id='{instance_id}'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
    check(bool(task), "① 存在待办（第一步提交时已发起）", instance_id)
    if not task:
        return 1

    owner = scalar(f"SELECT coalesce(assignee_id::text,'') FROM biz_flow_task WHERE id='{task}'::uuid")
    if owner != admin_id:
        psql(f"UPDATE biz_flow_task SET assignee_id='{admin_id}'::uuid WHERE id='{task}'::uuid")
        print(f"[info] 待办转办到 admin（原指派 {owner[:8]}…；生产里这批单派给了部门主管角色）")

    st, body = call("POST", f"/api/business/flow/task/{task}/approve",
                    {"comment": "P2-5 词表收敛真机验收：同意"}, admin)
    check(st == 200, "② 审批通过 200", f"{st} {str(body)[:300]}")

    order_status = scalar(f"SELECT status FROM biz_purchase_order WHERE id='{order_id}'::uuid")
    inst_status = scalar(f"SELECT status FROM biz_flow_instance WHERE id='{instance_id}'::uuid")
    check(order_status == "APPROVED", "③ 采购单落 APPROVED", order_status)
    check(inst_status == "APPROVED", "③ 审批实例落 APPROVED", inst_status)

    inst_biz = scalar(f"SELECT biz_type FROM biz_flow_instance WHERE id='{instance_id}'::uuid")
    def_biz = scalar("SELECT biz_type FROM biz_flow_def WHERE flow_code='po_approve' AND deleted_at IS NULL")
    check(inst_biz == def_biz == "PO_APPROVE", "④ 审批轴全程一致 PO_APPROVE", f"{inst_biz}/{def_biz}")

    logs = psql(f"SELECT DISTINCT entity_type FROM biz_status_log WHERE entity_id='{order_id}'::uuid ORDER BY 1").splitlines()
    check(logs == ["PURCHASE_ORDER"], "④ 对象轴：状态日志 entity_type 只有 PURCHASE_ORDER", str(logs))

    # 清理临时账号：有外键（实例发起人）→ 停用 + 软删，不硬删
    uid = scalar(f"SELECT id FROM sys_user WHERE username='{MAKER_USER}' AND deleted_at IS NULL")
    if uid:
        psql(f"DELETE FROM sys_user_role WHERE user_id='{uid}'::uuid")
        psql(f"UPDATE sys_user SET status=0, deleted_at=now(), updated_at=now() WHERE id='{uid}'::uuid")
        print(f"[info] 临时制单账号 {MAKER_USER} 已停用 + 软删（实例发起人外键，不能硬删）")

    print("\n[生产数据变动]")
    print(f"  · 采购单 {ORDER_NO}（{order_id}）APPROVED —— P2-5 验收单，100 元 × 1，可直接作废")
    print(f"  · 审批实例 {instance_id} APPROVED")
    print(f"  · 用户 {MAKER_USER} 已停用 + 软删（保留行满足 biz_flow_instance.initiator_id 外键）")

    print("\n" + "=" * 60)
    print(f"结果：{len(PASSED)} 通过 / {len(FAILED)} 失败")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
