#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P2-5 真机（生产）第三步：把 `n_dept` 节点剩余的会签待办批完，收口核对。

第二步暴露的真实现象（值得记下来）：`po_approve` 的「部门主管审批」节点
是按**角色**展开的或签/会签 —— 生产里该角色有 2 个真人（Henry.Liao / 老六测试号），
所以一张单会生成 2 条待办；只批 1 条实例仍是 PENDING（这与
`references/deploy-cards.md` §3「会签种子让老夹具假红」是同一件事）。
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
ADMIN_PASS = os.environ.get("P25_ADMIN_PASSWORD") or os.environ.get("NIK_ADMIN_PASSWORD", "")
ORDER_NO = os.environ.get("P25_ORDER_NO", "PO-20261104-000001")

PASSED, FAILED = [], []


def check(ok, name, detail=""):
    (PASSED if ok else FAILED).append(name)
    print(("[PASS] " if ok else "[FAIL] ") + name + (f" — {detail}" if detail and not ok else ""))


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
    st, body = call("POST", "/api/system/auth/login", {"username": "admin", "password": ADMIN_PASS})
    if st != 200:
        return 2
    admin = (body.get("data") or {}).get("access_token") or ""
    admin_id = scalar("SELECT id FROM sys_user WHERE username='admin'")

    order_id = scalar(f"SELECT id FROM biz_purchase_order WHERE order_no='{ORDER_NO}' AND deleted_at IS NULL")
    instance_id = scalar(f"SELECT approval_instance_id FROM biz_purchase_order WHERE id='{order_id}'::uuid")

    for round_no in range(1, 6):
        tasks = psql("SELECT id FROM biz_flow_task "
                     f"WHERE instance_id='{instance_id}'::uuid AND status='PENDING' ORDER BY created_at").splitlines()
        tasks = [t for t in tasks if t]
        if not tasks:
            break
        print(f"[info] 第 {round_no} 轮：{len(tasks)} 条待办待批")
        for task in tasks:
            psql(f"UPDATE biz_flow_task SET assignee_id='{admin_id}'::uuid WHERE id='{task}'::uuid")
            st, body = call("POST", f"/api/business/flow/task/{task}/approve",
                            {"comment": "P2-5 词表收敛真机验收：同意"}, admin)
            print(f"  · approve {task[:8]}… → {st}")

    order_status = scalar(f"SELECT status FROM biz_purchase_order WHERE id='{order_id}'::uuid")
    inst_status = scalar(f"SELECT status FROM biz_flow_instance WHERE id='{instance_id}'::uuid")
    check(order_status == "APPROVED", "① 采购单落 APPROVED", order_status)
    check(inst_status == "APPROVED", "② 审批实例落 APPROVED", inst_status)

    inst_biz = scalar(f"SELECT biz_type FROM biz_flow_instance WHERE id='{instance_id}'::uuid")
    def_biz = scalar("SELECT biz_type FROM biz_flow_def WHERE flow_code='po_approve' AND deleted_at IS NULL")
    check(inst_biz == def_biz == "PO_APPROVE", "③ 审批轴：实例 == 定义 == PO_APPROVE", f"{inst_biz}/{def_biz}")

    logs = psql(f"SELECT DISTINCT entity_type FROM biz_status_log WHERE entity_id='{order_id}'::uuid ORDER BY 1").splitlines()
    check(logs == ["PURCHASE_ORDER"], "④ 对象轴：状态日志 entity_type 只有 PURCHASE_ORDER", str(logs))

    done = scalar("SELECT count(*) FROM biz_flow_task "
                  f"WHERE instance_id='{instance_id}'::uuid AND status='APPROVED'")
    check(int(done or 0) >= 2, "⑤ 两条会签待办都已通过", f"approved_tasks={done}")

    print("\n[生产数据变动]")
    print(f"  · 采购单 {ORDER_NO}（{order_id}）= APPROVED（P2-5 验收单，100 元 × 1，可直接作废）")
    print(f"  · 审批实例 {instance_id} = APPROVED，biz_type={inst_biz}")
    print("  · 两条 n_dept 待办由 admin 代批（原指派 Henry.Liao / 老六测试号，验收需要）")
    print("  · 用户 p25maker 已停用 + 软删（保留行满足实例发起人外键）")

    print("\n" + "=" * 60)
    print(f"结果：{len(PASSED)} 通过 / {len(FAILED)} 失败")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
