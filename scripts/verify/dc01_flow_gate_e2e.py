#!/usr/bin/env python3
"""DC-01（t_4130a770）真 HTTP 验收：审批流未配置 → 409 + 显式跳过 + 自检端点。

卡面的四条验收在**接口层**再过一遍（单测已在 `erp::approval_gate` / `purchase_handler` /
`stockin_handler` / `payable_bridge` / `pflow3b_tests` 里钉过一遍）：

```text
A 自检端点      GET  /api/business/erp/approval-flow/health → 200，8 个 biz_type 全 published + graph_ok
B 默认拒绝      下线 po_approve → POST /purchase-order/{id}/submit-approval
                → 409 + 66033、报文含 `po_approve`、单据仍 DRAFT、无审批单
C 无权限码      普通角色（只有 submit-approval）请求 skip_approval → 403 + 报文点名 skip-approval
D 有权限没理由  持有 skip-approval 但理由 < 10 字 → 400 + 报文给出实际字数
E 显式跳过成立  持 skip-approval + 理由 ≥10 字 → 200，单据 SUBMITTED，
                `biz_status_log.reason` 含「跳过审批（未配置 po_approve 流程）」+ 用户理由
F 有流程不许跳  流程恢复 PUBLISHED 后仍请求 skip_approval → 400（不许绕过已配置的内控）
```

# 环境（隔离实例，绝不碰生产）

```bash
bash scripts/verify/dc01_e2e_up.sh up
DC01_BASE=http://127.0.0.1:18097 DC01_DB_NAME=cp_dc01_e2e \
DC01_DB_USER=alun DC01_DB_PASSWORD=alun \
  python3 scripts/verify/dc01_flow_gate_e2e.py
bash scripts/verify/dc01_e2e_up.sh down
```

退出码：0 = 全绿；1 = 有断言不通过；2 = 环境不满足（登录 / 夹具）。
"""

from __future__ import annotations


# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("DC01_BASE", "http://127.0.0.1:18097")
DB_NAME = os.environ.get("DC01_DB_NAME", "cp_dc01_e2e")
DB_USER = os.environ.get("DC01_DB_USER", "alun")
DB_PASS = os.environ.get("DC01_DB_PASSWORD", "alun")
DB_HOST = os.environ.get("DC01_DB_HOST", "127.0.0.1")
ADMIN_USER = os.environ.get("DC01_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("DC01_ADMIN_PASSWORD", "Admin@123456")
ADMIN_NEW_PASS = os.environ.get("DC01_ADMIN_NEW_PASSWORD", "Dc01Admin#2026x")

# 夹具号段（与 dc03 等既有脚本互不冲突）
SUPPLIER = "d4c10001-0000-4000-8000-000000000001"
WAREHOUSE = "d4c10001-0000-4000-8000-000000000002"
UNIT = "d4c10002-0000-4000-8000-000000000001"
MATERIAL = "d4c10003-0000-4000-8000-000000000001"
ROLE_NOSKIP = "DC01_NOSKIP"
ROLE_SKIP = "DC01_SKIP"
USER_NOSKIP = "dc01_noskip"
USER_SKIP = "dc01_skip"
USER_PASS = "Dc01User#2026x"
SKIP_PERM = "business:erp:skip-approval"

TOKEN = ""
PASSED: list[str] = []
FAILED: list[str] = []


def ok(name: str) -> None:
    PASSED.append(name)
    print(f"[PASS] {name}")


def bad(name: str) -> None:
    FAILED.append(name)
    print(f"[FAIL] {name}")


def check(cond: bool, name: str, detail: str = "") -> None:
    ok(name) if cond else bad(f"{name}{('：' + detail) if detail else ''}")


def psql(sql: str) -> str:
    env = dict(os.environ, PGPASSWORD=DB_PASS)
    out = subprocess.run(
        ["psql", "-h", DB_HOST, "-U", DB_USER, "-d", DB_NAME, "-Atc", sql],
        capture_output=True, text=True, env=env,
    )
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}")
    return out.stdout.strip()


def call(method: str, path: str, body: dict | None = None, token: str | None = None) -> tuple[int, dict]:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{BASE}{path}", data=data, method=method)
    # ⚠️ 只有真有 body 时才声明 Content-Type：空 body + `Content-Type: application/json`
    # 会让 axum 的 `Option<Json<T>>` 走「解析空串」分支 → 400 EOF（本卡实测）。
    if data is not None:
        req.add_header("Content-Type", "application/json")
    use = TOKEN if token is None else token
    if use:
        req.add_header("Authorization", f"Bearer {use}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw)
        except json.JSONDecodeError:
            return err.code, {"raw": raw}


def login_as(username: str, password: str) -> str:
    status, body = call("POST", "/api/system/auth/login",
                        {"username": username, "password": password})
    if status != 200:
        raise RuntimeError(f"登录 {username} 失败：{status} {body}")
    return body["data"]["access_token"]


def login_admin() -> None:
    global TOKEN
    for attempt in (ADMIN_PASS, ADMIN_NEW_PASS):
        status, body = call("POST", "/api/system/auth/login",
                            {"username": ADMIN_USER, "password": attempt})
        if status == 200 and body.get("data", {}).get("access_token"):
            TOKEN = body["data"]["access_token"]
            if body["data"].get("must_change_password"):
                status, _ = call("PUT", "/api/system/auth/password",
                                 {"old_password": attempt, "new_password": ADMIN_NEW_PASS})
                if status != 200:
                    raise RuntimeError("首登改密失败")
                TOKEN = ""
                TOKEN = login_as(ADMIN_USER, ADMIN_NEW_PASS)
            return
    raise RuntimeError("admin 登录失败（口令不对；隔离库首启会强制改密，见脚本头）")


# ==================== 夹具 ====================

def fixtures() -> None:
    psql(f"""
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order)
VALUES ('{UNIT}', 'DC01-PCS', '个', 1, 1, 1, 1) ON CONFLICT DO NOTHING;
INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
VALUES ('{SUPPLIER}', 'DC01-SUP', 'DC01 验收供应商', 1) ON CONFLICT DO NOTHING;
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
VALUES ('{WAREHOUSE}', 'DC01-WH', 'DC01 收货仓', '/{WAREHOUSE}/', 1, 1, 1)
ON CONFLICT DO NOTHING;
INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
VALUES ('{MATERIAL}', 'DC01-M1', 'DC01 物料', '{UNIT}', 1, false) ON CONFLICT DO NOTHING;
""")


def menu_id(auth_code: str) -> str:
    got = psql(f"SELECT id FROM sys_menu WHERE auth_code = '{auth_code}' AND deleted_at IS NULL LIMIT 1")
    if not got:
        raise RuntimeError(f"sys_menu 里没有权限码 {auth_code}")
    return got


def make_role(code: str, name: str, perms: list[str]) -> str:
    status, body = call("POST", "/api/system/role",
                        {"role_code": code, "role_name": name, "status": 1, "data_scope": 1})
    if status == 200:
        role_id = body["data"]["id"]
    else:
        role_id = psql(f"SELECT id FROM sys_role WHERE role_code = '{code}' AND deleted_at IS NULL")
    ids = ",".join(f"'{menu_id(p)}'::uuid" for p in perms)
    call("PUT", f"/api/system/role/{role_id}/menus", {"menu_ids": [menu_id(p) for p in perms]})
    _ = ids
    return role_id


def make_user(username: str, role_id: str) -> None:
    status, body = call("POST", "/api/system/user", {
        "username": username, "password": USER_PASS, "real_name": username,
        "status": 1, "role_ids": [role_id],
    })
    if status != 200:
        # 幂等：已存在就复用（重跑脚本时）
        exists = psql(f"SELECT count(*) FROM sys_user WHERE username = '{username}'")
        if exists == "0":
            raise RuntimeError(f"建用户 {username} 失败：{status} {body}")


def po_body(qty: str) -> dict:
    return {
        "supplier_id": SUPPLIER,
        "warehouse_id": WAREHOUSE,
        "purchase_type": "SPOT",
        "remark": "DC-01 审批流未配置验收",
        "items": [{"material_id": MATERIAL, "qty": qty, "price_excl": "2", "tax_rate": "13"}],
    }


def new_draft_po(qty: str = "5") -> tuple[str, str]:
    status, body = call("POST", "/api/business/erp/purchase-order", po_body(qty))
    if status != 200:
        raise RuntimeError(f"建采购单失败：{status} {body}")
    order = body["data"]["order"]
    return order["id"], order["order_no"]


def order_status(order_id: str) -> str:
    return psql(f"SELECT status FROM biz_purchase_order WHERE id = '{order_id}'::uuid")


def log_reasons(order_id: str) -> list[str]:
    out = psql(
        "SELECT coalesce(reason,'') FROM biz_status_log "
        f"WHERE entity_type = 'PURCHASE_ORDER' AND entity_id = '{order_id}'::uuid ORDER BY created_at"
    )
    return [line for line in out.splitlines() if line]


def set_flow(status: str) -> None:
    psql(f"UPDATE biz_flow_def SET status = '{status}' WHERE flow_code = 'po_approve'")


def cleanup() -> None:
    psql(f"""
UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'po_approve';
DELETE FROM biz_flow_instance WHERE biz_type = 'PO_APPROVE'
   AND biz_id IN (SELECT id FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid);
DELETE FROM biz_flow_task WHERE instance_id NOT IN (SELECT id FROM biz_flow_instance);
DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER'
   AND entity_id IN (SELECT id FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid);
DELETE FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid;
DELETE FROM sys_user_role WHERE user_id IN
   (SELECT id FROM sys_user WHERE username IN ('{USER_NOSKIP}','{USER_SKIP}'));
DELETE FROM sys_user WHERE username IN ('{USER_NOSKIP}','{USER_SKIP}');
DELETE FROM sys_role_menu WHERE role_id IN
   (SELECT id FROM sys_role WHERE role_code IN ('{ROLE_NOSKIP}','{ROLE_SKIP}'));
DELETE FROM sys_role WHERE role_code IN ('{ROLE_NOSKIP}','{ROLE_SKIP}');
""")


# ==================== 断言 ====================

def assert_health() -> None:
    status, body = call("GET", "/api/business/erp/approval-flow/health")
    check(status == 200, f"A1 自检端点 200（实际 {status}）")
    data = body.get("data") or {}
    check(data.get("total") == 8, f"A2 total=8（实际 {data.get('total')}）")
    check(data.get("healthy") == 8, f"A3 healthy=8（实际 {data.get('healthy')}）")
    check(data.get("unhealthy") == 0, f"A4 unhealthy=0（实际 {data.get('unhealthy')}）")
    items = data.get("items") or []
    check(len(items) == 8, f"A5 items 8 条（实际 {len(items)}）")
    for item in items:
        biz = item.get("biz_type")
        check(bool(item.get("published")), f"A6-{biz} 已发布")
        check(item.get("version_no") is not None, f"A7-{biz} 有当前版本号")
        check(bool(item.get("has_snapshot")), f"A8-{biz} 有发布快照")
        check(bool(item.get("graph_ok")), f"A9-{biz} 图形校验通过（issues={item.get('graph_issues')}）")
    print("   [自检明细] " + json.dumps(
        [{k: item.get(k) for k in ("biz_type", "flow_code", "published", "version_no", "graph_ok")}
         for item in items], ensure_ascii=False))


def main() -> int:
    try:
        login_admin()
    except RuntimeError as err:
        print(f"[ENV] {err}")
        return 2
    ok("0 管理员登录成功")

    assert_health()

    try:
        fixtures()
    except RuntimeError as err:
        print(f"[ENV] {err}")
        return 2
    ok("0b 主数据夹具就绪")

    # ---------- B 默认拒绝 ----------
    set_flow("DRAFT")  # 下线 = 「客户还没配审批」
    order_id, order_no = new_draft_po()
    status, body = call("POST", f"/api/business/erp/purchase-order/{order_id}/submit-approval")
    msg = body.get("msg") or body.get("message") or ""
    check(status == 409, f"B1 未配流程提交审批 → 409（实际 {status}：{msg}）")
    check(body.get("code") == 66033, f"B2 业务码 66033（实际 {body.get('code')}）")
    check("po_approve" in msg, f"B3 报文点名 flow_code（{msg[:60]}…）")
    check(SKIP_PERM in msg, "B4 报文给出「显式跳过」这条出路")
    check(order_status(order_id) == "DRAFT", f"B5 被拒后单据仍 DRAFT（实际 {order_status(order_id)}）")
    instance_cnt = psql(
        f"SELECT count(*) FROM biz_flow_instance WHERE biz_type='PO_APPROVE' AND biz_id='{order_id}'::uuid"
    )
    check(instance_cnt == "0", f"B6 被拒后无审批实例（实际 {instance_cnt}）")

    # ---------- C/D/E 跳过审批的三态 ----------
    perms = ["business:erp:purchase-order:submit-approval", "business:erp:purchase-order:list"]
    role_noskip = make_role(ROLE_NOSKIP, "DC01 无跳过权限", perms)
    role_skip = make_role(ROLE_SKIP, "DC01 有跳过权限", perms + [SKIP_PERM])
    make_user(USER_NOSKIP, role_noskip)
    make_user(USER_SKIP, role_skip)
    tok_noskip = login_as(USER_NOSKIP, USER_PASS)
    tok_skip = login_as(USER_SKIP, USER_PASS)
    ok("0c 两个受限账号就绪（一个无 skip-approval、一个有）")

    oid2, _ = new_draft_po()
    status, body = call("POST", f"/api/business/erp/purchase-order/{oid2}/submit-approval",
                        {"skip_approval": True, "skip_reason": "月底关账，先提交不等审批"}, tok_noskip)
    msg = body.get("msg") or ""
    check(status == 403, f"C1 无 skip-approval 请求跳过 → 403（实际 {status}：{msg}）")
    check(SKIP_PERM in msg, "C2 403 报文点名缺哪个权限码")
    check(order_status(oid2) == "DRAFT", "C3 被拒后单据仍 DRAFT")

    status, body = call("POST", f"/api/business/erp/purchase-order/{oid2}/submit-approval",
                        {"skip_approval": True, "skip_reason": "先放行"}, tok_skip)
    msg = body.get("msg") or ""
    check(status == 400, f"D1 有权限但理由不足 10 字 → 400（实际 {status}：{msg}）")
    check("当前 3 字" in msg or "10" in msg, f"D2 报文给出字数要求（{msg[:60]}…）")
    check(order_status(oid2) == "DRAFT", "D3 被拒后单据仍 DRAFT")

    reason_text = "供应商现场验收完成，月底关账前先提交待补流程"
    status, body = call("POST", f"/api/business/erp/purchase-order/{oid2}/submit-approval",
                        {"skip_approval": True, "skip_reason": reason_text}, tok_skip)
    msg = body.get("msg") or ""
    check(status == 200, f"E1 有权限 + 理由齐全 → 200（实际 {status}：{msg}）")
    check(order_status(oid2) == "SUBMITTED", f"E2 单据已提交（实际 {order_status(oid2)}）")
    reasons = log_reasons(oid2)
    joined = " | ".join(reasons)
    check(any("跳过审批" in r and "po_approve" in r for r in reasons),
          f"E3 跳过审批理由写进 biz_status_log（实际：{joined}）")
    check(any(reason_text in r for r in reasons), f"E4 用户填的理由原样留痕（实际：{joined}）")

    # ---------- F 流程已发布时不许跳过 ----------
    set_flow("PUBLISHED")
    oid3, _ = new_draft_po()
    status, body = call("POST", f"/api/business/erp/purchase-order/{oid3}/submit-approval",
                        {"skip_approval": True, "skip_reason": reason_text})
    msg = body.get("msg") or ""
    check(status == 400, f"F1 有已发布流程时请求跳过 → 400（实际 {status}：{msg}）")
    check(order_status(oid3) == "DRAFT", "F2 被拒后单据仍 DRAFT")
    # 正常路径不受影响：不传 skip 就走审批
    status, body = call("POST", f"/api/business/erp/purchase-order/{oid3}/submit-approval")
    check(status == 200, f"F3 正常提交（走审批）→ 200（实际 {status}：{body.get('msg')}）")
    check(order_status(oid3) == "SUBMITTED", f"F4 正常提交后 SUBMITTED（实际 {order_status(oid3)}）")

    cleanup()
    print(f"\n==> 汇总：PASS={len(PASSED)} FAIL={len(FAILED)}")
    if FAILED:
        for name in FAILED:
            print(f"    ✗ {name}")
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as err:
        print(f"[ENV] {err}")
        sys.exit(2)
