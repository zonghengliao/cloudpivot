#!/usr/bin/env python3
"""DC-03 收货过账复核来源采购单：真机（HTTP）验收。

卡面要求的三条验收在**接口层**再过一遍（单测已在 `stockin_handler::tests::dc03_*` 里钉过一遍）：

```text
A 建单（采购单 SUBMITTED/APPROVED）→ 采购单改 CANCELLED → 过账 → 409 + 66031
   报文点名采购单号 + 当前状态；三本账 / 凭证 / 应付 **0 变化**，单据仍是 DRAFT
B 两张收货单双开（采购 100、各收 60）→ 过账 A 成功 → 过账 B → 409 + 66032
   报文给出「只剩 40 件待收」；B 仍是 DRAFT，采购已收只算 A 的 60
C 正常路径（采购 100 / 收 100）→ 200 + 采购单 RECEIVED（不受影响）
```

# 为什么用 HTTP 而不是只跑单测

单测走的是 handler 函数；本脚本走的是 `PATCH /business/erp/stock-in/{id}/status` 这条**真路由**
（含认证、JSON 编解码、错误码落 HTTP 状态），能证明前端拿到的就是 `409 + 66031/66032`。

# 环境（隔离实例，绝不碰生产）

```bash
bash scripts/verify/dc03_e2e_up.sh up          # 建库 cp_dc03_e2e + 起 :18096 + 迁移/种子
DC03_BASE=http://127.0.0.1:18096 DC03_DB_NAME=cp_dc03_e2e \
DC03_DB_USER=alun DC03_DB_PASSWORD=alun \
  python3 scripts/verify/dc03_posting_guard_e2e.py
bash scripts/verify/dc03_e2e_up.sh down        # 停实例 + dropdb
```

> ⚠️ 夹具会把 `stock_in_post` 流程临时下线（`status='DRAFT'`），让 `PATCH /status` 走
> 「未配流程 → 免审直过」这条路 —— 否则过账会先进 `PENDING_POST` 等审批，
> 测不到过账闸本身。收尾（`cleanup()`）无条件恢复 `PUBLISHED`。

退出码：0 = 全绿；1 = 有断言不通过；2 = 环境不满足（登录 / 主数据）。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("DC03_BASE", "http://127.0.0.1:18096")
DB_NAME = os.environ.get("DC03_DB_NAME", "cp_dc03_e2e")
DB_USER = os.environ.get("DC03_DB_USER", "alun")
DB_PASS = os.environ.get("DC03_DB_PASSWORD", "alun")
DB_HOST = os.environ.get("DC03_DB_HOST", "127.0.0.1")
ADMIN_USER = os.environ.get("DC03_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("DC03_ADMIN_PASSWORD", "Admin@123456")
ADMIN_NEW_PASS = os.environ.get("DC03_ADMIN_NEW_PASSWORD", "Dc03Admin#2026x")

# 夹具号段（与 d9u / d9c / b2b1 等既有脚本互不冲突）
SUPPLIER = "d3c00001-0000-4000-8000-000000000001"
WAREHOUSE = "d3c00001-0000-4000-8000-000000000002"
UNIT = "d3c00002-0000-4000-8000-000000000001"
MATERIAL = "d3c00003-0000-4000-8000-000000000001"

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


def call(method: str, path: str, body: dict | None = None) -> tuple[int, dict]:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{BASE}{path}", data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if TOKEN:
        req.add_header("Authorization", f"Bearer {TOKEN}")
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


def login() -> bool:
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
                    print(f"首登改密失败：{status}")
                    return False
                # 改密会 bump token_version ⇒ 旧令牌立刻失效；带着它再登录会被
                # session_guard 当成「权限已变更」直接 401，所以先清掉。
                TOKEN = ""
                status, body = call("POST", "/api/system/auth/login",
                                    {"username": ADMIN_USER, "password": ADMIN_NEW_PASS})
                if status != 200:
                    print(f"改密后登录失败：{status} {body}")
                    return False
                TOKEN = body["data"]["access_token"]
            return True
    print("登录失败：口令不对（隔离库首启会强制改密，见脚本头）")
    return False


def fixtures() -> None:
    psql(f"""
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order)
VALUES ('{UNIT}', 'DC03-PCS', '个', 1, 1, 1, 1) ON CONFLICT DO NOTHING;
INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
VALUES ('{SUPPLIER}', 'DC03-SUP', 'DC03 验收供应商', 1) ON CONFLICT DO NOTHING;
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
VALUES ('{WAREHOUSE}', 'DC03-WH', 'DC03 收货仓', '/{WAREHOUSE}/', 1, 1, 1)
ON CONFLICT DO NOTHING;
INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
VALUES ('{MATERIAL}', 'DC03-M1', 'DC03 物料', '{UNIT}', 1, false) ON CONFLICT DO NOTHING;
""")


def cleanup() -> None:
    """删本夹具的单据 + 恢复流程；主数据留着（下一轮直接复用）。"""
    psql(f"""
UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'stock_in_post';
DELETE FROM biz_flow_instance
 WHERE biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL')
   AND biz_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid);
DELETE FROM biz_voucher_entry WHERE voucher_id IN (
  SELECT id FROM biz_voucher WHERE source_id IN (
    SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid));
DELETE FROM biz_voucher WHERE source_id IN (
  SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid);
DELETE FROM biz_payable WHERE source_id IN (
  SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid);
DELETE FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND entity_id IN (
  SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid);
DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN (
  SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid);
DELETE FROM biz_material_serial WHERE source_id IN (
  SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid);
DELETE FROM biz_inventory_location WHERE material_id = '{MATERIAL}'::uuid
   AND warehouse_id = '{WAREHOUSE}'::uuid;
DELETE FROM biz_inventory WHERE material_id = '{MATERIAL}'::uuid
   AND warehouse_id = '{WAREHOUSE}'::uuid;
DELETE FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid;
DELETE FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid;
""")


def unpublish_post_flow() -> None:
    """过账流程临时下线 → `PATCH /status` 走「免审直过」分支（否则先进 PENDING_POST）。"""
    psql("UPDATE biz_flow_def SET status = 'DRAFT' WHERE flow_code = 'stock_in_post'")


def po_body(qty: str) -> dict:
    return {
        "supplier_id": SUPPLIER,
        "warehouse_id": WAREHOUSE,
        "purchase_type": "SPOT",
        "remark": "DC-03 过账复核来源采购单验收",
        "items": [{"material_id": MATERIAL, "qty": qty, "price_excl": "2", "tax_rate": "13"}],
    }


def new_order(qty: str) -> tuple[str, str, str]:
    """建采购单 + 提交审批 → 返回 `(order_id, order_no, 状态)`。"""
    status, body = call("POST", "/api/business/erp/purchase-order", po_body(qty))
    if status != 200:
        raise RuntimeError(f"建采购单失败：{status} {body}")
    order = body["data"]["order"]
    order_id, order_no = order["id"], order["order_no"]
    status, body = call("PATCH", f"/api/business/erp/purchase-order/{order_id}/status",
                        {"status": "SUBMITTED"})
    if status != 200:
        raise RuntimeError(f"提交采购单失败：{status} {body}")
    cur = psql(f"SELECT status FROM biz_purchase_order WHERE id = '{order_id}'::uuid")
    return order_id, order_no, cur


def setting_order_cancelled(order_id: str) -> str:
    status, body = call("PATCH", f"/api/business/erp/purchase-order/{order_id}/status",
                        {"status": "CANCELLED", "reason": "DC-03 验收：故意取消"})
    if status != 200:
        raise RuntimeError(f"取消采购单失败：{status} {body}")
    return psql(f"SELECT status FROM biz_purchase_order WHERE id = '{order_id}'::uuid")


def pending_item_id(order_id: str) -> str:
    status, body = call("GET", f"/api/business/erp/purchase-order/{order_id}/pending-items")
    if status != 200:
        raise RuntimeError(f"查待收失败：{status} {body}")
    return body["data"]["items"][0]["item_id"]


def new_stock_in(order_id: str, qty: str | None, item_id: str | None) -> str:
    body: dict = {"source_order_id": order_id, "warehouse_id": WAREHOUSE}
    if qty is None:
        body["items"] = []          # 空明细 → 由采购单未收数量自动带出
    else:
        body["items"] = [{
            "purchase_order_item_id": item_id,
            "material_id": MATERIAL,
            "qty": qty,
            "price_excl": "2",
            "tax_rate": "13",
        }]
    status, resp = call("POST", "/api/business/erp/stock-in", body)
    if status != 200:
        raise RuntimeError(f"建收货单失败：{status} {resp}")
    return resp["data"]["header"]["id"]


def post(stock_in_id: str) -> tuple[int, dict]:
    return call("PATCH", f"/api/business/erp/stock-in/{stock_in_id}/status", {"status": "POSTED"})


def footprint(stock_in_id: str) -> tuple[int, int, int, int]:
    """(库存行数, 流水, 凭证, 应付) —— 被拒的单必须四项全 0。"""
    inv = int(psql(f"""SELECT count(*) FROM biz_inventory
      WHERE material_id = '{MATERIAL}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid AND qty <> 0"""))
    txn = int(psql(f"""SELECT count(*) FROM biz_inventory_txn
      WHERE doc_type = 'STOCK_IN' AND doc_id = '{stock_in_id}'::uuid"""))
    vou = int(psql(f"""SELECT count(*) FROM biz_voucher WHERE source_id = '{stock_in_id}'::uuid"""))
    pay = int(psql(f"""SELECT count(*) FROM biz_payable WHERE source_id = '{stock_in_id}'::uuid"""))
    return inv, txn, vou, pay


def si_status(stock_in_id: str) -> str:
    return psql(f"SELECT status FROM biz_stock_in WHERE id = '{stock_in_id}'::uuid")


def ledger_received(item_id: str) -> str:
    """已收数量 —— P2-3 起是派生值（只算已过账收货单），库表里没有这一列。"""
    return psql(
        f"SELECT COALESCE(SUM(si.qty),0)::numeric(18,4)::text "
        f"FROM biz_purchase_order_item i "
        f"LEFT JOIN biz_stock_in_item si ON si.source_item_id = i.id "
        f"LEFT JOIN biz_stock_in s ON s.id = si.stock_in_id "
        f"WHERE i.id = '{item_id}'::uuid AND s.status = 'POSTED' AND s.deleted_at IS NULL"
    )


def scenario_a() -> None:
    print("\n== A 建单后可收 → 采购单被取消 → 过账必须 409（三本账/凭证/应付 0 变化） ==")
    order_id, order_no, po_status = new_order("100")
    check(po_status in ("SUBMITTED", "APPROVED"), "A0 前提：采购单已提交/已审批", po_status)
    si = new_stock_in(order_id, None, None)
    check(si_status(si) == "DRAFT", "A1 建单成功且为 DRAFT")
    check(footprint(si) == (0, 0, 0, 0), "A2 前提：建单不动任何账", str(footprint(si)))

    cancelled = setting_order_cancelled(order_id)
    check(cancelled == "CANCELLED", "A3 采购单已通过接口取消", cancelled)

    status, body = post(si)
    msg = body.get("msg", "") or str(body)
    check(status == 409, "A4 过账被拒：HTTP 409", f"{status} {msg[:120]}")
    check(body.get("code") == 66031, "A5 业务码 66031（ERP_SI_PO_NOT_RECEIVABLE）", str(body.get("code")))
    check(order_no in msg, "A6 报文点名采购单号", msg[:160])
    check("已取消" in msg and "CANCELLED" in msg, "A7 报文带当前状态（已取消 / CANCELLED）", msg[:160])
    check("本单未过账" in msg, "A8 报文说清「本单未过账」", msg[:160])
    check(footprint(si) == (0, 0, 0, 0), "A9 被拒后三本账 / 凭证 / 应付全 0", str(footprint(si)))
    check(si_status(si) == "DRAFT", "A10 单据仍是 DRAFT", si_status(si))
    item_id = psql(f"SELECT id FROM biz_purchase_order_item WHERE order_id = '{order_id}'::uuid")
    check(ledger_received(item_id) in ("0.0000", "0"), "A11 采购明细已收数量未动",
          ledger_received(item_id))


def scenario_b() -> None:
    print("\n== B 两张收货单双开（采购 100 / 各收 60）→ 第二张过账 409 + 可用待收量 ==")
    order_id, order_no, po_status = new_order("100")
    check(po_status in ("SUBMITTED", "APPROVED"), "B0 前提：采购单已提交/已审批", po_status)
    item_id = pending_item_id(order_id)
    a = new_stock_in(order_id, "60", item_id)
    b = new_stock_in(order_id, "60", item_id)
    check(a != b, "B1 两张草稿都建出来了（双开的前提）")

    status, body = post(a)
    check(status == 200, "B2 第一张过账成功", f"{status} {str(body)[:120]}")
    check(si_status(a) == "POSTED", "B3 A 已 POSTED", si_status(a))
    check(ledger_received(item_id) == "60.0000", "B4 采购已收 = 60", ledger_received(item_id))
    before = footprint(b)

    status, body = post(b)
    msg = body.get("msg", "") or str(body)
    check(status == 409, "B5 第二张过账被拒：HTTP 409", f"{status} {msg[:120]}")
    check(body.get("code") == 66032, "B6 业务码 66032（ERP_SI_OVER_RECEIVE）", str(body.get("code")))
    check(order_no in msg, "B7 报文点名采购单号", msg[:160])
    check("只剩 40 件待收" in msg, "B8 报文给出可用待收量（只剩 40 件待收）", msg[:200])
    check("已过账收货 60" in msg and "订单 100" in msg, "B9 报文给出订单量与已收量", msg[:200])
    check(footprint(b) == before, "B10 被拒的 B 不留任何账", f"{before} -> {footprint(b)}")
    check(si_status(b) == "DRAFT", "B11 B 仍是 DRAFT", si_status(b))
    check(ledger_received(item_id) == "60.0000", "B12 采购已收仍只算 A 的 60", ledger_received(item_id))


def scenario_c() -> None:
    print("\n== C 正常路径（采购 100 / 收 100）不受影响 ==")
    inv_sql = f"""SELECT COALESCE(sum(qty),0)::text FROM biz_inventory
      WHERE material_id = '{MATERIAL}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"""
    before = float(psql(inv_sql))
    order_id, order_no, _ = new_order("100")
    si = new_stock_in(order_id, None, None)
    status, body = post(si)
    check(status == 200, "C1 收满 100/100 过账成功", f"{status} {str(body)[:120]}")
    check(si_status(si) == "POSTED", "C2 单据 POSTED", si_status(si))
    po = psql(f"SELECT status FROM biz_purchase_order WHERE id = '{order_id}'::uuid")
    check(po == "RECEIVED", "C3 采购单自动推进为 RECEIVED", po)
    after = float(psql(inv_sql))
    check(abs(after - before - 100) < 0.0001, "C4 库存精确 +100（本单）", f"{before} -> {after}")
    check(footprint(si)[2] == 1 and footprint(si)[3] == 1, "C5 正常单有凭证 + 应付", str(footprint(si)))


def main() -> int:
    if not login():
        return 2
    fixtures()
    unpublish_post_flow()
    try:
        scenario_a()
        scenario_b()
        scenario_c()
    finally:
        cleanup()
    total = len(PASSED) + len(FAILED)
    print(f"\n结果：{len(PASSED)}/{total} 通过" + (f"   失败 {len(FAILED)}" if FAILED else ""))
    for name in FAILED:
        print(f"  · {name}")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
