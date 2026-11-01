#!/usr/bin/env python3
"""D7 真机验收：批量操作（勾选 → 逐条事务 → 逐条报告 → 批量导出）。

# 验收的不是「接口返 200」，而是卡面 §五 的真机链路

```
① 收货单列表勾 3 张 DRAFT → 批量过账 → 3 成功
② 勾 2 张（1 DRAFT + 1 POSTED）→ 批量过账 → 1 成功 1 失败（后者状态不对）
③ 勾 5 张 → 批量导出 → 导入导出页能看到任务并能下载 xlsx
```

外加卡面 §六 的三条硬红线在真 HTTP 上的表现：

* **逐条独立事务**：第 5 条失败时，前 4 条已经落库（② 里直接查库确认 POSTED）；
* **失败逐条报原因**：`failed[].error` 是单条接口原文（带单号与当前状态）；
* **幂等**：同一批再点一次 = 0 成功 / N 失败（不是 500，也不是「又过一遍账」）。

# 用法

```bash
D7_BASE_URL=http://127.0.0.1:18091 D7_DB_NAME=cloudpivot_d7 D7_DB_USER=d7role D7_DB_PASSWORD=d7pass \
  python3 scripts/verify/d7_batch_e2e.py
D7_KEEP=1 …   # 留夹具排查
```

退出码：0 = 全绿；1 = 有断言不通过；2 = 环境不满足。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("D7_BASE_URL", "http://127.0.0.1:18091")
DB_NAME = os.environ.get("D7_DB_NAME", "cloudpivot_d7")
DB_USER = os.environ.get("D7_DB_USER", "d7role")
DB_PASS = os.environ.get("D7_DB_PASSWORD", "d7pass")
DB_HOST = os.environ.get("D7_DB_HOST", "127.0.0.1")
DB_PORT = os.environ.get("D7_DB_PORT", "5432")
KEEP = os.environ.get("D7_KEEP") == "1"

ADMIN_USER = os.environ.get("D7_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("D7_ADMIN_PASSWORD", "Admin@123456")

UNIT_ID = "d7e2e001-0000-4000-8000-000000000001"
SUPPLIER_ID = "d7e2e002-0000-4000-8000-000000000001"
WAREHOUSE_ID = "d7e2e003-0000-4000-8000-000000000001"
MATERIAL_ID = "d7e2e004-0000-4000-8000-000000000001"

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
    args = [
        "psql", "-h", DB_HOST, "-p", DB_PORT, "-U", DB_USER, "-d", DB_NAME,
        "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tAc", sql,
    ]
    env = dict(os.environ, PGPASSWORD=DB_PASS)
    out = subprocess.run(args, capture_output=True, text=True, env=env, check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}\nSQL: {sql}")
    return out.stdout.strip()


def call(method: str, path: str, body: dict | None = None, token: str | None = None) -> tuple[int, dict]:
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


def login() -> str:
    status, body = call("POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": ADMIN_PASS})
    if status != 200:
        raise SystemExit(f"登录失败：{status} {body}")
    token = body["data"]["access_token"]
    # 干净库首登：管理员被要求先改初始口令，否则所有写接口都是 403
    if body["data"].get("must_change_password"):
        new_pass = os.environ.get("D7_ADMIN_NEW_PASSWORD", "D7Admin#2026x")
        st, resp = call(
            "PUT",
            "/api/system/auth/password",
            {"old_password": ADMIN_PASS, "new_password": new_pass},
            token,
        )
        if st != 200:
            raise SystemExit(f"改初始口令失败：{st} {resp}")
        status, body = call("POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": new_pass})
        if status != 200:
            raise SystemExit(f"改完口令后登录失败：{status} {body}")
        token = body["data"]["access_token"]
    return token


def setup_fixtures() -> None:
    psql(
        """
        INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('%(unit)s', 'D7E2E-PCS', '个', 1, NULL, 1.000000, 1, 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('%(sup)s', 'D7E2E-SUP', 'D7 真机供应商', 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('%(wh)s', 'D7E2E-WH', 'D7 真机仓', '/%(wh)s/', 1, 1, 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('%(mat)s', 'D7E2E-M1', 'D7 真机螺栓', '%(unit)s', 1, false)
        ON CONFLICT DO NOTHING;
        """
        % {"unit": UNIT_ID, "sup": SUPPLIER_ID, "wh": WAREHOUSE_ID, "mat": MATERIAL_ID}
    )


def cleanup() -> None:
    stmts = [
        f"DELETE FROM biz_material_serial WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_inventory_txn WHERE doc_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_inventory_location WHERE material_id = '{MATERIAL_ID}'::uuid AND warehouse_id = '{WAREHOUSE_ID}'::uuid",
        f"DELETE FROM biz_inventory WHERE material_id = '{MATERIAL_ID}'::uuid AND warehouse_id = '{WAREHOUSE_ID}'::uuid",
        f"DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid))",
        f"DELETE FROM biz_voucher WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_status_log WHERE entity_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid",
        f"DELETE FROM sys_export_task WHERE query_params::text LIKE '%{MATERIAL_ID}%' OR biz_type IN ('erp.stock_in')",
        f"DELETE FROM biz_material WHERE id = '{MATERIAL_ID}'::uuid",
        f"DELETE FROM biz_warehouse WHERE id = '{WAREHOUSE_ID}'::uuid",
        f"DELETE FROM biz_supplier WHERE id = '{SUPPLIER_ID}'::uuid",
        f"DELETE FROM biz_unit WHERE id = '{UNIT_ID}'::uuid",
    ]
    for sql in stmts:
        try:
            psql(sql)
        except RuntimeError as error:
            print(f"[warn] 清理失败（{error}）")


def create_stock_in(token: str, qty: int) -> str:
    status, body = call(
        "POST",
        "/api/business/erp/stock-in",
        {
            "supplier_id": SUPPLIER_ID,
            "warehouse_id": WAREHOUSE_ID,
            "in_date": "2026-11-02",
            "remark": "D7 真机批量",
            "items": [
                {
                    "material_id": MATERIAL_ID,
                    "qty": qty,
                    "price_excl": 10,
                    "tax_rate": 13,
                }
            ],
        },
        token,
    )
    if status != 200:
        raise SystemExit(f"建草稿失败：{status} {body}")
    return body["data"]["header"]["id"]


def main() -> int:
    print(f"== D7 批量操作真机验收 @ {BASE} / {DB_NAME}")
    cleanup()
    setup_fixtures()
    token = login()

    # ---------- ① 3 张 DRAFT 批量过账 → 3 成功 ----------
    three = [create_stock_in(token, 1) for _ in range(3)]
    status, body = call(
        "POST",
        "/api/business/erp/stock-in/batch-status",
        {"ids": three, "status": "POSTED"},
        token,
    )
    data = body.get("data") or {}
    check(status == 200, "① 批量过账 HTTP 200", f"{status} {body}")
    check(data.get("success_count") == 3, "① 3 张全成功 success_count=3", json.dumps(data, ensure_ascii=False))
    check(data.get("fail_count") == 0, "① 没有失败条目")
    posted = psql(
        f"SELECT COUNT(*) FROM biz_stock_in WHERE id = ANY('{{{','.join(three)}}}'::uuid[]) AND status = 'POSTED'"
    )
    check(posted == "3", "① 库里 3 张都是 POSTED", f"count={posted}")

    # ---------- ② 1 DRAFT + 1 POSTED → 1 成功 1 失败 ----------
    draft = create_stock_in(token, 2)
    posted_id = three[0]
    status, body = call(
        "POST",
        "/api/business/erp/stock-in/batch-status",
        {"ids": [draft, posted_id], "status": "POSTED"},
        token,
    )
    data = body.get("data") or {}
    check(status == 200, "② 混合状态批量过账 HTTP 200", f"{status} {body}")
    check(data.get("success_count") == 1, "② 只成功 1 条", json.dumps(data, ensure_ascii=False))
    check(data.get("fail_count") == 1, "② 失败 1 条")
    failed = (data.get("failed") or [{}])[0]
    check(failed.get("code") == 10003, "② 已过账那条返回冲突码 10003", json.dumps(failed, ensure_ascii=False))
    check("已过账" in (failed.get("error") or ""), "② 失败原因逐条可读（含「已过账」）", failed.get("error", ""))
    check(bool(failed.get("no")), "② 失败条目带单号（IN-…）", str(failed.get("no")))
    # 逐条独立事务：成功那条已落库
    st = psql(f"SELECT status FROM biz_stock_in WHERE id = '{draft}'::uuid")
    check(st == "POSTED", "② 成功的那条已提交（逐条事务，不是整批回滚）", st)

    # ---------- ③ 幂等：同一批再点一次 = 0 成功 ----------
    status, body = call(
        "POST",
        "/api/business/erp/stock-in/batch-status",
        {"ids": three, "status": "POSTED"},
        token,
    )
    data = body.get("data") or {}
    check(status == 200 and data.get("success_count") == 0, "③ 重复点批量过账 = 0 成功", json.dumps(data, ensure_ascii=False))
    check(data.get("fail_count") == 3, "③ 3 条都报「已过账」而不是报错", str(data.get("fail_count")))
    inv = psql(
        f"SELECT COALESCE(SUM(qty), 0) FROM biz_inventory WHERE material_id = '{MATERIAL_ID}'::uuid AND warehouse_id = '{WAREHOUSE_ID}'::uuid"
    )
    check(inv == "5.0000", "③ 库存只加一次（1+1+1+2=5，没有被重复过账翻倍）", f"qty={inv}")

    # ---------- ④ 空 ids / 不存在的 id ----------
    status, body = call("POST", "/api/business/erp/stock-in/batch-status", {"ids": [], "status": "POSTED"}, token)
    check(status == 400, "④ 空 ids 直接 400", f"{status} {body}")
    ghost = "d7e2e0ff-0000-4000-8000-00000000ffff"
    status, body = call(
        "POST",
        "/api/business/erp/stock-in/batch-status",
        {"ids": [ghost], "status": "CANCELLED", "reason": "D7 真机：不存在的单"},
        token,
    )
    data = body.get("data") or {}
    check(status == 200 and data.get("fail_count") == 1, "④ 不存在的 id 算该条失败（不整批 500）", json.dumps(data, ensure_ascii=False))
    check(
        (data.get("failed") or [{}])[0].get("code") == 10002,
        "④ 不存在返回 10002（RESOURCE_NOT_FOUND）",
        json.dumps(data.get("failed"), ensure_ascii=False),
    )

    # ---------- ⑤ 批量导出 → 任务可查可下载 ----------
    five = [create_stock_in(token, 1) for _ in range(5)]
    status, body = call("POST", "/api/business/erp/stock-in/batch-export", {"ids": five}, token)
    data = body.get("data") or {}
    task_no = data.get("task_no")
    check(status == 200 and bool(task_no), "⑤ 批量导出返回异步任务号", f"{status} {body}")
    check(data.get("total") == 5, "⑤ 任务行数 = 勾选的 5 行", str(data.get("total")))

    export_ok = False
    for _ in range(40):
        time.sleep(1)
        st, page = call("GET", f"/api/business/impexp/export/page?biz_type=erp.stock_in&page=1&page_size=10", None, token)
        items = ((page.get("data") or {}).get("list")) or []
        hit = next((item for item in items if item.get("task_no") == task_no), None)
        if hit and hit.get("status") in ("SUCCESS", "SUCCEEDED", "DONE", "COMPLETED"):
            export_ok = True
            break
        if hit and hit.get("status") in ("FAILED", "CANCELLED"):
            print(f"[warn] 导出任务状态 {hit.get('status')}：{hit.get('error_message')}")
            break
    check(export_ok, "⑤ 导出任务在导入导出页跑完（SUCCESS）", f"task_no={task_no}")

    if export_ok:
        req = urllib.request.Request(f"{BASE}/api/business/impexp/export/{data.get('id')}/download")
        req.add_header("Authorization", f"Bearer {token}")
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                blob = resp.read()
            check(blob[:2] == b"PK", "⑤ 产物是真正的 xlsx（zip 头 PK）", blob[:8].hex())
        except urllib.error.HTTPError as error:
            check(False, "⑤ 产物可下载", f"{error.code}")

    # ---------- ⑥ 批量删除：只删草稿，已过账的拒绝 ----------
    del_draft = create_stock_in(token, 1)
    status, body = call(
        "POST",
        "/api/business/erp/stock-in/batch-delete",
        {"ids": [del_draft, posted_id]},
        token,
    )
    data = body.get("data") or {}
    check(status == 200 and data.get("success_count") == 1, "⑥ 批量删除只删草稿", json.dumps(data, ensure_ascii=False))
    check(data.get("fail_count") == 1, "⑥ 已过账的那条被拒（1 失败）")
    deleted = psql(f"SELECT COUNT(*) FROM biz_stock_in WHERE id = '{del_draft}'::uuid AND deleted_at IS NOT NULL")
    check(deleted == "1", "⑥ 草稿已软删", f"count={deleted}")

    # ---------- ⑦ 另外三张卡片的批量入口已注册（路由 + 逐条报告） ----------
    for path, payload, name in [
        ("/api/business/erp/transfer/batch-status", {"ids": [ghost], "status": "CANCELLED", "reason": "D7"}, "调拨单"),
        ("/api/business/erp/serial/batch-status", {"ids": [ghost], "status": "SCRAPPED", "reason": "D7"}, "序列号"),
        ("/api/business/erp/purchase-order/batch-status", {"ids": [ghost], "status": "CANCELLED", "reason": "D7 真机"}, "采购单"),
        ("/api/business/flow/task/batch-approve", {"ids": [ghost], "decision": "APPROVE"}, "审批待办"),
        ("/api/business/task/batch-status", {"ids": [ghost], "status": "DONE"}, "任务"),
    ]:
        status, body = call("POST", path, payload, token)
        data = body.get("data") or {}
        check(
            status == 200 and data.get("fail_count") == 1,
            f"⑦ {name}批量接口已注册并逐条报告（不存在的 id → 1 失败）",
            f"{status} {body}",
        )

    # ---------- ⑧ 未登录一律 401（路由已在鉴权后面） ----------
    status, _ = call("POST", "/api/business/erp/stock-in/batch-status", {"ids": [ghost], "status": "POSTED"})
    check(status == 401, "⑧ 未登录调批量接口 → 401（不是 404 路由缺失）", str(status))

    if not KEEP:
        cleanup()
        print("== 夹具已清理")
    print(f"\n== 结果：{len(PASSED)} PASS / {len(FAILED)} FAIL")
    for item in FAILED:
        print(f"   ✗ {item}")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
