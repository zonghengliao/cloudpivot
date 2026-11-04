#!/usr/bin/env python3
"""DC-09 计量单位止血：真机（HTTP）验收。

卡面验收项「非基本单位建单 → 400 + 指定文案」在**接口层**再过一遍，三张单据各一组：

```text
① 采购订单：物料挂在折算单位（1 箱 = 12 个）→ 400 + 66030 + 「…请按基本单位录入（1 箱 = 12 个 → 录 24）」
② 收货入库：同上 → 400 + 66030
③ 库存调拨：同上 → 400 + 66030
④ 对照组：基本单位物料 → 同一套请求体全部 200（证明闸门只拦折算单位）
```

外加两条反向证据（防止「错误码对了、脏单却进了库」）：

* 三次被拒之后，库里本夹具的单据行数仍为 0；
* 文案里的行号 `items[0]` 与物料编码都对得上（前端要按行高亮）。

# 用法

```bash
E2E_DB=cp_units_e2e E2E_PORT=18098 bash scripts/verify/dc09_e2e_up.sh
D9U_BASE_URL=http://127.0.0.1:18098 D9U_DB_NAME=cp_units_e2e \
D9U_DB_USER=alun D9U_DB_PASSWORD=alun \
  python3 scripts/verify/dc09_units_e2e.py
```

退出码：0 = 全绿；1 = 有断言不通过；2 = 环境不满足（登录 / 主数据）。
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

BASE = os.environ.get("D9U_BASE_URL", "http://127.0.0.1:18098")
DB_NAME = os.environ.get("D9U_DB_NAME", "cp_units_e2e")
DB_USER = os.environ.get("D9U_DB_USER", "alun")
DB_PASS = os.environ.get("D9U_DB_PASSWORD", "alun")
DB_HOST = os.environ.get("D9U_DB_HOST", "127.0.0.1")
ADMIN_USER = os.environ.get("D9U_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("D9U_ADMIN_PASSWORD", "Admin@123456")
ADMIN_NEW_PASS = os.environ.get("D9U_ADMIN_NEW_PASSWORD", "D9uAdmin#2026x")

# 夹具 UUID（与其它脚本的号段互不冲突）
SUPPLIER = "d9c00001-0000-4000-8000-000000000001"
WH_FROM = "d9c00001-0000-4000-8000-000000000002"
WH_TO = "d9c00001-0000-4000-8000-000000000003"
UNIT_BASE = "d9c00002-0000-4000-8000-000000000001"
UNIT_BOX = "d9c00002-0000-4000-8000-000000000002"
M_BASE = "d9c00003-0000-4000-8000-000000000001"
M_BOX = "d9c00003-0000-4000-8000-000000000002"

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
    global TOKEN, ADMIN_PASS
    for attempt_pass in (ADMIN_PASS, ADMIN_NEW_PASS):
        status, body = call("POST", "/api/system/auth/login",
                            {"username": ADMIN_USER, "password": attempt_pass})
        if status == 200 and body.get("data", {}).get("access_token"):
            TOKEN = body["data"]["access_token"]
            if body["data"].get("must_change_password"):
                status, _ = call("PUT", "/api/system/auth/password",
                                 {"old_password": attempt_pass, "new_password": ADMIN_NEW_PASS})
                if status != 200:
                    print(f"首登改密失败：{status}")
                    return False
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
VALUES ('{UNIT_BASE}', 'D9U-PCS', '个', 1, 1, 1, 1) ON CONFLICT DO NOTHING;
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
VALUES ('{UNIT_BOX}', 'D9U-BOX', '箱', 0, '{UNIT_BASE}', 12, 1, 2) ON CONFLICT DO NOTHING;
INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
VALUES ('{SUPPLIER}', 'D9U-SUP', 'D9U 供应商', 1) ON CONFLICT DO NOTHING;
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
VALUES ('{WH_FROM}', 'D9U-WH1', 'D9U 调出仓', '/{WH_FROM}/', 1, 1, 1) ON CONFLICT DO NOTHING;
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
VALUES ('{WH_TO}', 'D9U-WH2', 'D9U 调入仓', '/{WH_TO}/', 1, 1, 1) ON CONFLICT DO NOTHING;
INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
VALUES ('{M_BASE}', 'D9U-M-BASE', 'D9U 基本单位物料', '{UNIT_BASE}', 1, false) ON CONFLICT DO NOTHING;
INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
VALUES ('{M_BOX}', 'D9U-M-BOX', 'D9U 折算单位物料', '{UNIT_BOX}', 1, false) ON CONFLICT DO NOTHING;
""")


def cleanup() -> None:
    """删掉本夹具的单据 + 审批痕迹，主数据留着（下一轮 ON CONFLICT 复用）。

    顺序写死：审批实例（弱引用，先断）→ 调拨明细（`biz_stock_transfer_item` 对单据头是
    硬外键、**没有** `ON DELETE CASCADE`）→ 调拨单头 → 收货单（明细 CASCADE）→ 采购单。
    """
    psql(f"""
DELETE FROM biz_flow_instance WHERE biz_id IN (
  SELECT id FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid
  UNION ALL SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid
  UNION ALL SELECT id FROM biz_stock_transfer
             WHERE from_warehouse_id = '{WH_FROM}'::uuid OR to_warehouse_id = '{WH_FROM}'::uuid);
DELETE FROM biz_stock_transfer_item WHERE transfer_id IN (
  SELECT id FROM biz_stock_transfer
   WHERE from_warehouse_id = '{WH_FROM}'::uuid OR to_warehouse_id = '{WH_FROM}'::uuid);
DELETE FROM biz_stock_transfer
 WHERE from_warehouse_id = '{WH_FROM}'::uuid OR to_warehouse_id = '{WH_FROM}'::uuid;
DELETE FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid;
DELETE FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid;
""")


def doc_counts() -> tuple[int, int, int]:
    po = int(psql(f"SELECT count(*) FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid"))
    si = int(psql(f"SELECT count(*) FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid"))
    tr = int(psql(f"SELECT count(*) FROM biz_stock_transfer WHERE from_warehouse_id = '{WH_FROM}'::uuid"))
    return po, si, tr


def po_body(material: str, qty: str) -> dict:
    return {
        "supplier_id": SUPPLIER,
        "warehouse_id": WH_TO,
        "remark": "D9U 单位止血验收",
        "items": [{"material_id": material, "qty": qty, "price_excl": "2", "tax_rate": "13"}],
    }


def si_body(material: str, qty: str) -> dict:
    # A4：无源入库必须显式声明 is_sourceless + 写明来源
    return {
        "supplier_id": SUPPLIER,
        "warehouse_id": WH_TO,
        "is_sourceless": True,
        "remark": "D9U 无源入库（期初）",
        "items": [{
            "material_id": material, "qty": qty, "price_excl": "2", "tax_rate": "13",
        }],
    }


def tr_body(material: str, qty: str) -> dict:
    return {
        "from_warehouse_id": WH_FROM,
        "to_warehouse_id": WH_TO,
        "reason": "D9U 单位止血验收",
        "items": [{"material_id": material, "qty": qty}],
    }


def expect_unit_error(label: str, path: str, body: dict, qty_hint: str) -> None:
    status, payload = call("POST", path, body)
    check(status == 400, f"{label}：非基本单位建单被拒（{status}）")
    check(payload.get("code") == 66030, f"{label}：错误码 66030（{payload.get('code')}）")
    msg = payload.get("msg", "")
    expect = (
        "单位「箱」不是基本单位：本版本不支持多单位换算，"
        f"请按基本单位录入（1 箱 = 12 个 → 录 {qty_hint}）"
    )
    check(expect in msg, f"{label}：文案逐字命中（含折算建议）", msg)
    check("items[0]" in msg, f"{label}：文案点出行号 items[0]", msg)
    check("D9U-M-BOX" in msg, f"{label}：文案点出物料编码", msg)


def main() -> int:
    if not login():
        return 2
    fixtures()
    cleanup()
    psql(f"UPDATE biz_inventory SET qty = 100, amount = 200 WHERE material_id = '{M_BASE}'::uuid")
    # 调拨需要调出仓有库存（只给基本单位物料；折算单位物料本卡故意不给库存）
    psql(f"""
DELETE FROM biz_inventory WHERE material_id IN ('{M_BASE}'::uuid, '{M_BOX}'::uuid);
INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount)
VALUES (gen_random_uuid(), '{M_BASE}', '{WH_FROM}', NULL, 100, 0, 2, 200);
""")

    print("== ① 采购订单")
    expect_unit_error("采购单", "/api/business/erp/purchase-order", po_body(M_BOX, "2"), "24")

    print("== ② 收货入库")
    expect_unit_error("收货单", "/api/business/erp/stock-in", si_body(M_BOX, "2"), "24")

    print("== ③ 库存调拨")
    expect_unit_error("调拨单", "/api/business/erp/transfer", tr_body(M_BOX, "1"), "12")

    print("== ④ 被拒之后：一张单都不许落库")
    po, si, tr = doc_counts()
    check((po, si, tr) == (0, 0, 0), f"单据行数为 0（po={po} si={si} tr={tr}）")

    print("== ⑤ 对照组：基本单位物料走同一套请求体必须 200")
    status, body = call("POST", "/api/business/erp/purchase-order", po_body(M_BASE, "2"))
    check(status == 200, f"采购单（基本单位）建单成功（{status}）", str(body)[:200])
    status, body = call("POST", "/api/business/erp/stock-in", si_body(M_BASE, "2"))
    check(status == 200, f"收货单（基本单位）建单成功（{status}）", str(body)[:200])
    status, body = call("POST", "/api/business/erp/transfer", tr_body(M_BASE, "2"))
    check(status == 200, f"调拨单（基本单位）建单成功（{status}）", str(body)[:200])

    # 对照组落库的明细单位必须都是基本单位（三处口径同源）
    non_base_rows = int(psql(f"""
SELECT (SELECT count(*) FROM biz_purchase_order_item i JOIN biz_unit u ON u.id = i.unit_id
          WHERE u.is_base = 0 AND i.material_id = '{M_BASE}'::uuid)
     + (SELECT count(*) FROM biz_stock_in_item i JOIN biz_unit u ON u.id = i.unit_id
          WHERE u.is_base = 0 AND i.material_id = '{M_BASE}'::uuid)"""))
    check(non_base_rows == 0, f"对照组明细全部落在基本单位上（非基本单位行 = {non_base_rows}）")

    print("== ⑥ 清理")
    cleanup()
    po, si, tr = doc_counts()
    check((po, si, tr) == (0, 0, 0), f"夹具残留 = 0（po={po} si={si} tr={tr}）")

    print(f"==> 汇总：PASS={len(PASSED)} FAIL={len(FAILED)}")
    for name in FAILED:
        print(f"    ✗ {name}")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
