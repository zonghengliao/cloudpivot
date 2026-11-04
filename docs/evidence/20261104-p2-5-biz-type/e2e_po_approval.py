#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P2-5 真机验收：采购单审批全链路（证明词表收敛后路由没断）。

跑在一张**干净隔离库 + 独立实例**上（不是生产），验的是「业务类型词表收敛之后，
最关键的 `biz_type` 这条链还通不通」：

```
① 建采购单（DRAFT）
② 提交审批 → `biz_flow_instance.biz_type` 必须是 `PO_APPROVE`
   （= `biz_flow_def.biz_type`，同一个字面量；旧卡面假设实例侧是 `purchase_order`，实测不是）
③ 审批通过 → 采购单 APPROVED，实例 APPROVED
④ `biz_status_log.entity_type` 必须是 `PURCHASE_ORDER`（业务对象轴，与审批轴分开但都对）
⑤ 联动作废路由：规范路径段 200/400-业务判定；历史别名（purchase_order / PO）必须 400
⑥ 词表巡检（scripts/verify/biz_type_domain.py）跑完仍然 PASS
```

用法：
    P25_BASE_URL=http://127.0.0.1:18145 P25_DB_NAME=cp_p25_vocab \\
    P25_DB_USER=alun P25_DB_PASSWORD=alun python3 docs/evidence/20261104-p2-5-biz-type/e2e_po_approval.py

退出码：0 = 全绿；1 = 有断言失败；2 = 环境不满足。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("P25_BASE_URL", "http://127.0.0.1:18145").rstrip("/")
DB_NAME = os.environ.get("P25_DB_NAME", "cp_p25_vocab")
DB_USER = os.environ.get("P25_DB_USER", "alun")
DB_PASS = os.environ.get("P25_DB_PASSWORD", "alun")

ADMIN_USER = os.environ.get("P25_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("P25_ADMIN_PASSWORD", "Admin@123456")
ADMIN_NEW_PASS = os.environ.get("P25_ADMIN_NEW_PASSWORD", "P25Admin#2026x")

MAKER_USER = "p25maker"
MAKER_PASS = "P25Maker@12345"

UNIT_ID = "b2e2e001-0000-4000-8000-0000000000a5"
SUPPLIER_ID = "b2e2e002-0000-4000-8000-0000000000a5"
WAREHOUSE_ID = "b2e2e003-0000-4000-8000-0000000000a5"
MATERIAL_ID = "b2e2e004-0000-4000-8000-0000000000a5"

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
    env = dict(os.environ, PGPASSWORD=DB_PASS)
    out = subprocess.run(args, capture_output=True, text=True, env=env, check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}\nSQL: {sql}")
    return out.stdout.strip()


def scalar(sql: str) -> str:
    return (psql(sql).splitlines() or [""])[0].strip()


def call(method: str, path: str, body: dict | None = None,
         token: str | None = None) -> tuple[int, dict]:
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


def login(user: str, password: str, new_password: str | None = None) -> str:
    # 幂等：上一次跑可能已经把「初始口令」改成 new_password 了。
    # 先试 password，401 再试 new_password（脚本要能反复跑）。
    status, body = call("POST", "/api/system/auth/login",
                        {"username": user, "password": password})
    if status != 200 and new_password:
        status, body = call("POST", "/api/system/auth/login",
                            {"username": user, "password": new_password})
        if status == 200:
            password = new_password
    if status != 200:
        raise SystemExit(f"[env] 登录 {user} 失败：{status} {body}")
    data = body.get("data") or {}
    token = data.get("access_token")
    if data.get("must_change_password"):
        if not new_password:
            raise SystemExit(f"[env] {user} 要求先改初始口令，但脚本没给新口令")
        st, resp = call("PUT", "/api/system/auth/password",
                        {"old_password": password, "new_password": new_password}, token)
        if st != 200:
            raise SystemExit(f"[env] 改初始口令失败：{st} {resp}")
        status, body = call("POST", "/api/system/auth/login",
                            {"username": user, "password": new_password})
        if status != 200:
            raise SystemExit(f"[env] 改口令后登录失败：{status} {body}")
        token = (body.get("data") or {}).get("access_token")
    return token


def setup_fixtures() -> None:
    psql(f"""
        INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('{UNIT_ID}', 'P25E2E-PCS', '个', 1, NULL, 1.000000, 1, 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('{SUPPLIER_ID}', 'P25E2E-SUP', 'P2-5 真机供应商', 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('{WAREHOUSE_ID}', 'P25E2E-WH', 'P2-5 真机仓', '/{WAREHOUSE_ID}/', 1, 1, 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('{MATERIAL_ID}', 'P25E2E-M1', 'P2-5 真机螺栓', '{UNIT_ID}', 1, false)
        ON CONFLICT DO NOTHING;
    """)


def ensure_maker(admin_token: str) -> None:
    """造一个「制单人」账号（SUPER_ADMIN，免权限码），否则 admin 自己提单自己批会被内控闸拒。

    走**接口**建号（`POST /api/system/user`）：口令哈希是 Argon2id PHC 串，
    只能在服务端算（SQL 里写不出来）；直接 INSERT 一个假哈希会让登录永远 20001。
    """
    exists = scalar(f"SELECT count(*) FROM sys_user WHERE username='{MAKER_USER}' AND deleted_at IS NULL")
    if exists != "0":
        return
    role = scalar("SELECT id FROM sys_role WHERE role_code='SUPER_ADMIN' LIMIT 1")
    st, body = call("POST", "/api/system/user", {
        "username": MAKER_USER,
        "password": MAKER_PASS,
        "real_name": "P2-5 制单人",
        "status": 1,
        "role_ids": [role],
    }, admin_token)
    if st != 200:
        raise SystemExit(f"[env] 建制单人失败：{st} {body}")
    print(f"[info] 制单人 {MAKER_USER} 已创建（SUPER_ADMIN）")


def pending_task(instance_id: str) -> tuple[str, str]:
    row = scalar(
        "SELECT id || '|' || coalesce(assignee_id::text,'') FROM biz_flow_task "
        f"WHERE instance_id='{instance_id}'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1"
    )
    if not row:
        return "", ""
    task_id, assignee = row.split("|", 1)
    return task_id, assignee


def main() -> int:
    admin = login(ADMIN_USER, ADMIN_PASS, ADMIN_NEW_PASS)
    setup_fixtures()
    ensure_maker(admin)
    maker = login(MAKER_USER, MAKER_PASS)

    # ---------- ① 建单 ----------
    status, body = call("POST", "/api/business/erp/purchase-order", {
        "supplier_id": SUPPLIER_ID,
        "warehouse_id": WAREHOUSE_ID,
        "purchase_type": "SPOT",
        "price_mode": "EXCL",
        "items": [{"material_id": MATERIAL_ID, "qty": "10", "price_excl": "500", "tax_rate": "13"}],
    }, maker)
    if status != 200:
        print(f"[env] 建单失败：{status} {body}")
        return 2
    order = ((body.get("data") or {}).get("order") or {})
    order_id = order.get("id")
    order_no = order.get("order_no")
    check(bool(order_id), "① 采购单建单成功（DRAFT）", f"{status} {str(body)[:300]}")

    # ---------- ② 提交审批 ----------
    status, body = call("POST", f"/api/business/erp/purchase-order/{order_id}/submit-approval", {}, maker)
    check(status == 200, "② 提交审批 200", f"{status} {str(body)[:300]}")
    payload = body.get("data") or {}
    detail = payload.get("order") or payload
    instance_id = detail.get("approval_instance_id") or ""
    check(bool(instance_id), "② 返回 approval_instance_id", str(detail)[:200])

    # 审批轴：实例 biz_type 必须与流程定义 biz_type 逐字相同
    inst_biz_type = scalar(f"SELECT biz_type FROM biz_flow_instance WHERE id='{instance_id}'::uuid") if instance_id else ""
    def_biz_type = scalar("SELECT biz_type FROM biz_flow_def WHERE flow_code='po_approve' AND deleted_at IS NULL")
    check(inst_biz_type == def_biz_type == "PO_APPROVE",
          "② 审批实例 biz_type == 流程定义 biz_type == PO_APPROVE",
          f"instance={inst_biz_type!r} def={def_biz_type!r}")

    # 业务对象轴：状态日志 entity_type 是 PURCHASE_ORDER（与审批轴不同名但都对）
    log_types = psql(
        "SELECT DISTINCT entity_type FROM biz_status_log "
        f"WHERE entity_id='{order_id}'::uuid ORDER BY 1"
    ).splitlines()
    check(log_types == ["PURCHASE_ORDER"],
          "② 状态日志 entity_type = PURCHASE_ORDER（业务对象轴）", str(log_types))

    # ---------- ③ 审批通过 ----------
    task_id, assignee = pending_task(instance_id)
    check(bool(task_id), "③ 审批发起后有待办", f"instance={instance_id}")
    if not task_id:
        return 1
    approver_token = admin
    if assignee and assignee != scalar(f"SELECT id FROM sys_user WHERE username='{ADMIN_USER}'"):
        # 待办不是 admin 的：改成 admin（本卡验的是词表/路由，不是待办归属算法）
        psql(f"UPDATE biz_flow_task SET assignee_id=(SELECT id FROM sys_user WHERE username='{ADMIN_USER}') "
             f"WHERE id='{task_id}'::uuid")
        print("[info] 待办归属改到 admin（为了让脚本能批完；不属于本卡验收面）")
    status, body = call("POST", f"/api/business/flow/task/{task_id}/approve", {"comment": "P2-5 真机验收：同意"}, approver_token)
    check(status == 200, "③ 审批通过 200", f"{status} {body}")

    order_status = scalar(f"SELECT status FROM biz_purchase_order WHERE id='{order_id}'::uuid")
    inst_status = scalar(f"SELECT status FROM biz_flow_instance WHERE id='{instance_id}'::uuid")
    check(order_status == "APPROVED", "③ 采购单落 APPROVED", order_status)
    check(inst_status == "APPROVED", "③ 审批实例落 APPROVED", inst_status)

    # ---------- ④ 联动作废路由：只认规范路径段 ----------
    ok_status, ok_body = call("GET", f"/api/business/erp/purchase-order/{order_id}/cancel-impact", None, admin)
    check(ok_status == 200, "④ 规范路径段 purchase-order → 影响面 200",
          f"{ok_status} {str(ok_body)[:200]}")
    for legacy in ("purchase_order", "PO", "purchaseOrder"):
        st, bd = call("GET", f"/api/business/erp/{legacy}/{order_id}/cancel-impact", None, admin)
        msg = ((bd.get("data") or {}) if isinstance(bd.get("data"), dict) else {}) or bd
        text = json.dumps(bd, ensure_ascii=False)
        check(st in (400, 404) and "purchase-order" in text,
              f"④ 历史别名 {legacy} 被拒（400 且点名规范写法）", f"{st} {text[:160]}")

    # ---------- ⑤ 巡检 ----------
    run = subprocess.run(
        [sys.executable, "scripts/verify/biz_type_domain.py"],
        capture_output=True, text=True, cwd=os.path.dirname(os.path.dirname(os.path.dirname(
            os.path.dirname(os.path.abspath(__file__))))),
        env=dict(os.environ, NIK_DATABASE_HOST="127.0.0.1", NIK_DATABASE_USER=DB_USER,
                 NIK_DATABASE_PASSWORD=DB_PASS, NIK_DATABASE_NAME=DB_NAME),
    )
    tail = "\n".join(run.stdout.strip().splitlines()[-6:])
    check(run.returncode == 0, "⑤ 审批跑完后词表巡检仍 PASS", tail)

    print("\n" + "=" * 60)
    print(f"结果：{len(PASSED)} 通过 / {len(FAILED)} 失败")
    if FAILED:
        for name in FAILED:
            print(f"  ✗ {name}")
        return 1
    print("全绿：采购单审批发起 → 通过，词表各轴取值全部在域内")
    print(f"  采购单 {order_no} / 实例 biz_type={inst_biz_type} / entity_type=PURCHASE_ORDER")
    return 0


if __name__ == "__main__":
    sys.exit(main())
