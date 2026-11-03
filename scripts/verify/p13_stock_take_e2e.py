#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-3 真机验收：盘点单（ARCH-3 DC-14）—— 建单 → 填实盘 → 提交审批 → 过账 → 作废。

跑在隔离实例上（默认 127.0.0.1:18099 / 库 cp_p13_e2e），**全部走真实 HTTP**，
每一步都把三本账（成本账 / 库位账 / 流水）+ 凭证读出来打印，作为「账实调平有据可查」的证据：

```
步骤                       成本账            库位账        流水净额   凭证
① 基线（期初 100 @ 8）     100 / 800.00      100           100        借1405 800 / 贷1901 800
② 建盘点单（全盘）         100 / 800.00      100           100        -
③ 填实盘 92                100 / 800.00      100           100        -
④ 提交审批（待过账）       100 / 800.00      100           100        -
   审批通过（仓管主管）
⑤ 过账                     92 / 736.00       92            92         新增 借1901 64 / 贷1405 64
⑥ 作废                    100 / 800.00      100           100         红字 借1405 64 / 贷1901 64
```

用法：python3 scripts/verify/p13_stock_take_e2e.py
退出码：0 全过 / 1 有失败。
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE = os.environ.get("P13_BASE", "http://127.0.0.1:18099")
DB = dict(
    host=os.environ.get("P13_DB_HOST", "127.0.0.1"),
    port=os.environ.get("P13_DB_PORT", "5432"),
    user=os.environ.get("P13_DB_USER", "alun"),
    name=os.environ.get("P13_DB_NAME", "cp_p13_e2e"),
    password=os.environ.get("P13_DB_PASSWORD", "alun"),
)
ADMIN_USER = os.environ.get("P13_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("P13_ADMIN_PASS", "Admin@123456")
NEW_PASS = os.environ.get("P13_NEW_PASS", "P13@Passw0rd!")
KEEPER_USER = "p13_keeper"
ROLE_WM = "20000000-0000-4000-8000-0000000f0004"  # WAREHOUSE_MANAGER（V0182 种子）

UNIT = "d2130100-0000-4000-8000-000000000001"
WAREHOUSE = "d2130100-0000-4000-8000-000000000002"
MATERIAL = "d2130100-0000-4000-8000-000000000011"
SEED_VOUCHER = "d2130100-0000-4000-8000-0000000000f1"
FLOW_MENUS = ("business:flow:instance:read", "business:flow:approve")

PASSED = []
FAILED = []
ADMIN_CURRENT_PASS = ADMIN_PASS


def check(ok, name, detail=""):
    (PASSED if ok else FAILED).append(name)
    print(("[PASS] " if ok else "[FAIL] ") + name + ("" if ok else f" — {detail}"))
    return ok


def num(text):
    try:
        return Decimal(str(text).strip() or "0")
    except Exception:
        return Decimal("0")


def psql(sql):
    args = [
        "psql", "-h", DB["host"], "-p", DB["port"], "-U", DB["user"], "-d", DB["name"],
        "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tAF", "\x1f", "-c", sql,
    ]
    out = subprocess.run(args, capture_output=True, text=True,
                         env=dict(os.environ, PGPASSWORD=DB["password"]), check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()[:300]}\nSQL: {sql}")
    rows = []
    for line in out.stdout.strip().split("\n"):
        if line == "":
            continue
        rows.append(line.split("\x1f"))
    return rows


def one(sql, default="0"):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] else default


def http(method, path, token=None, body=None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except Exception:
            return err.code, {"raw": raw}


def payload_of(payload):
    return (payload or {}).get("data") if isinstance(payload, dict) else None


def login(username, password):
    """登录（自动处理「首登强制改密」与「口令已改过」两种状态）。"""
    global ADMIN_CURRENT_PASS
    status, body, token, used = 0, {}, "", password
    for candidate in (password, ADMIN_PASS, NEW_PASS):
        status, body = http("POST", "/api/system/auth/login",
                            body={"username": username, "password": candidate})
        token = ((body or {}).get("data") or {}).get("access_token") or ""
        if token:
            used = candidate
            break
    if not token:
        raise RuntimeError(f"登录失败 {username}: {status} {body}")
    if ((body or {}).get("data") or {}).get("must_change_password") and used != NEW_PASS:
        st, resp = http("PUT", "/api/system/auth/password", token,
                        {"old_password": used, "new_password": NEW_PASS})
        if st != 200:
            raise RuntimeError(f"{username} 改初始口令失败：{st} {resp}")
        status, body = http("POST", "/api/system/auth/login",
                            body={"username": username, "password": NEW_PASS})
        token = ((body or {}).get("data") or {}).get("access_token") or ""
        used = NEW_PASS
    if username == ADMIN_USER:
        ADMIN_CURRENT_PASS = used
    return token


# ==================== 三本账读数 ====================

def cost_ledger():
    return psql(
        "SELECT coalesce(sum(qty),0)::text, coalesce(sum(amount),0)::text "
        f"FROM biz_inventory WHERE material_id = '{MATERIAL}'::uuid"
    )[0]


def location_ledger():
    return one(
        "SELECT coalesce(sum(qty),0)::text FROM biz_inventory_location "
        f"WHERE material_id = '{MATERIAL}'::uuid"
    )


def txn_net():
    return one(
        "SELECT coalesce(sum(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END),0)::text "
        f"FROM biz_inventory_txn WHERE material_id = '{MATERIAL}'::uuid"
    )


def take_txn_net(take_id):
    return num(one(
        "SELECT coalesce(sum(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END),0)::text "
        f"FROM biz_inventory_txn WHERE doc_type = 'STOCK_TAKE' AND doc_id = '{take_id}'::uuid"
    ))


def vouchers_of_take(take_id):
    return psql(
        "SELECT voucher_no, status, is_red, coalesce(source_type,'') "
        "FROM biz_voucher WHERE source_type = 'STOCK_TAKE' "
        f"AND source_id = '{take_id}'::uuid ORDER BY is_red, voucher_no"
    )


def entries_of_take(take_id):
    return psql(
        "SELECT a.account_code, e.debit_amount::text, e.credit_amount::text, v.is_red "
        "FROM biz_voucher_entry e "
        "JOIN biz_voucher v ON v.id = e.voucher_id AND v.source_type = 'STOCK_TAKE' "
        f"AND v.source_id = '{take_id}'::uuid "
        "JOIN biz_account a ON a.id = e.account_id ORDER BY v.is_red, e.line_no"
    )


def snapshot(label):
    qty, amount = cost_ledger()
    print(f"    · 三本账 @ {label}：成本账 {qty} / {amount}｜库位账 {location_ledger()}｜流水净额 {txn_net()}")
    return qty, amount


# ==================== 夹具：主数据 + 期初三本账 ====================

def seed():
    psql("DELETE FROM biz_voucher WHERE source_type = 'STOCK_TAKE'")
    psql("DELETE FROM biz_stock_take WHERE take_no LIKE 'ST-%'")
    psql(f"DELETE FROM biz_inventory WHERE material_id = '{MATERIAL}'::uuid")
    psql(f"DELETE FROM biz_inventory_location WHERE material_id = '{MATERIAL}'::uuid")
    psql(f"DELETE FROM biz_inventory_txn WHERE material_id = '{MATERIAL}'::uuid")
    psql("DELETE FROM biz_voucher WHERE source_no IN ('P13-SEED', 'P13-SEED-IN') "
         f"OR id = '{SEED_VOUCHER}'::uuid")
    psql("DELETE FROM biz_stock_in WHERE in_no = 'P13-SEED-IN'")
    psql("DELETE FROM biz_accounting_period WHERE period_code = '2026-11'")
    psql(f"DELETE FROM biz_warehouse WHERE id = '{WAREHOUSE}'::uuid")
    psql(f"DELETE FROM biz_material WHERE id = '{MATERIAL}'::uuid")
    psql(f"DELETE FROM biz_unit WHERE id = '{UNIT}'::uuid")

    psql(
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order) "
        f"VALUES ('{UNIT}'::uuid, 'P13-PCS', '个', 1, 1, 1, 1)"
    )
    psql(
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, "
        "level, is_leaf, status, allow_negative) "
        f"VALUES ('{WAREHOUSE}'::uuid, 'P13-WH', 'P13 盘点仓', '/{WAREHOUSE}/', 1, 1, 1, 0)"
    )
    psql(
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) "
        f"VALUES ('{MATERIAL}'::uuid, 'P13-M1', 'P13 螺丝', '{UNIT}'::uuid, 1, false)"
    )
    # 期初：100 件 @ 8 → 成本账 800 / 库位账 100 / 流水 IN 100
    psql(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, "
        "avg_cost, amount) "
        f"VALUES (gen_random_uuid(), '{MATERIAL}'::uuid, '{WAREHOUSE}'::uuid, NULL, 100, 0, 8, 800)"
    )
    psql(
        "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty) "
        f"VALUES (gen_random_uuid(), '{MATERIAL}'::uuid, '{WAREHOUSE}'::uuid, NULL, 100, 0)"
    )
    psql(
        "INSERT INTO biz_inventory_txn (id, material_id, warehouse_id, txn_type, biz_type, doc_type, "
        "doc_id, doc_no, line_no, qty, unit_cost, amount, before_qty, before_amount, before_cost, "
        "after_qty, after_amount, after_cost, period_code, created_at) "
        f"VALUES (gen_random_uuid(), '{MATERIAL}'::uuid, '{WAREHOUSE}'::uuid, 'IN', 'SERIAL_BATCH', "
        f"'SERIAL_BATCH', gen_random_uuid(), 'P13-SEED', 1, 100, 8, 800, 0, 0, 0, 100, 800, 8, "
        "'2026-11', now())"
    )
    # 期间行（凭证的期间必须存在 → 巡检 F2；状态 OPEN 才不会拦过账）
    psql(
        "INSERT INTO biz_accounting_period (id, period_year, period_month, period_code, "
        "start_date, end_date, status, remark) VALUES "
        "(gen_random_uuid(), 2026, 11, '2026-11', '2026-11-01', '2026-11-30', 'OPEN', "
        "'P13 真机验收期间') ON CONFLICT DO NOTHING"
    )
    # 期初的**合法来源单据**：一张已过账的无源收货单（巡检 E4 要求「库存 ≤ 有效入库」）
    stock_in_id = "d2130100-0000-4000-8000-0000000000e1"
    psql(
        "INSERT INTO biz_stock_in (id, in_no, in_type, supplier_id, is_sourceless, warehouse_id, "
        "in_date, status, total_qty, total_amount, total_tax, total_with_tax, voucher_id, remark) "
        f"VALUES ('{stock_in_id}'::uuid, 'P13-SEED-IN', 'OTHER', NULL, true, "
        f"'{WAREHOUSE}'::uuid, '2026-11-01', 'POSTED', 100, 800, 0, 800, "
        f"'{SEED_VOUCHER}'::uuid, 'P13 真机验收期初')"
    )
    psql(
        "INSERT INTO biz_stock_in_item (id, stock_in_id, line_no, material_id, unit_id, qty, "
        "price, price_incl, tax_rate, amount, tax_amount, with_tax_amount, cost_price) "
        f"VALUES (gen_random_uuid(), '{stock_in_id}'::uuid, 1, '{MATERIAL}'::uuid, "
        f"'{UNIT}'::uuid, 100, 8, 8, 0, 800, 0, 800, 8)"
    )
    inv_id = one("SELECT id::text FROM biz_account WHERE account_code = '1405' AND deleted_at IS NULL")
    pnd_id = one("SELECT id::text FROM biz_account WHERE account_code = '1901' AND deleted_at IS NULL")
    psql(
        "INSERT INTO biz_voucher (id, voucher_no, period, voucher_date, total_debit, total_credit, "
        "entry_count, status, summary, voucher_type, source_type, source_no) "
        f"VALUES ('{SEED_VOUCHER}'::uuid, 'PZ-P13-SEED', '2026-11', '2026-11-01', 800, 800, 2, "
        "'POSTED', 'P13 期初入库', 'AUTO', 'SERIAL_BATCH', 'P13-SEED-IN')"
    )
    psql(
        "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, "
        "account_name, debit_amount, credit_amount) VALUES "
        f"('{SEED_VOUCHER}'::uuid, 1, 'P13 期初入库', '{inv_id}'::uuid, '1405', '库存商品', 800, 0), "
        f"('{SEED_VOUCHER}'::uuid, 2, 'P13 期初入库', '{pnd_id}'::uuid, '1901', '待处理财产损溢', 0, 800)"
    )


def ensure_keeper():
    """建/复用一个仓管主管账号（审批人 + 过账人；制单人 admin 自己不能批自己的单）。

    幂等：已存在就只把口令哈希与 admin 对齐并补授权 —— **不删用户**
    （历史待办用 `assignee_id` 硬外键引用它，删了第二遍就跑不了）。
    """
    if one(f"SELECT count(*) FROM sys_user WHERE username = '{KEEPER_USER}'") == "0":
        psql(
            "INSERT INTO sys_user (id, username, password, real_name, status, must_change_password) "
            f"SELECT gen_random_uuid(), '{KEEPER_USER}', u.password, 'P13 仓管主管', 1, false "
            "FROM sys_user u WHERE u.username = 'admin'"
        )
    else:
        psql(
            "UPDATE sys_user SET password = (SELECT password FROM sys_user WHERE username = 'admin'), "
            f"status = 1, must_change_password = false WHERE username = '{KEEPER_USER}'"
        )
    psql(
        "INSERT INTO sys_user_role (id, user_id, role_id) "
        f"SELECT gen_random_uuid(), (SELECT id FROM sys_user WHERE username = '{KEEPER_USER}'), "
        f"'{ROLE_WM}'::uuid WHERE NOT EXISTS (SELECT 1 FROM sys_user_role x "
        f"  WHERE x.user_id = (SELECT id FROM sys_user WHERE username = '{KEEPER_USER}') "
        f"    AND x.role_id = '{ROLE_WM}'::uuid)"
    )
    for code in FLOW_MENUS:
        psql(
            "INSERT INTO sys_role_menu (id, role_id, menu_id) "
            f"SELECT gen_random_uuid(), '{ROLE_WM}'::uuid, m.id FROM sys_menu m "
            f"WHERE m.auth_code = '{code}' AND m.deleted_at IS NULL "
            "AND NOT EXISTS (SELECT 1 FROM sys_role_menu x "
            f"  WHERE x.role_id = '{ROLE_WM}'::uuid AND x.menu_id = m.id)"
        )
    return login(KEEPER_USER, ADMIN_CURRENT_PASS)


def main():
    print("=" * 92)
    print(f"P1-3 盘点单真机验收：{BASE} / 库 {DB['name']}")
    print("=" * 92)

    seed()
    admin = login(ADMIN_USER, ADMIN_PASS)
    keeper = ensure_keeper()

    # ① 基线
    print("\n① 基线（期初 100 @ 8）")
    qty, amount = snapshot("基线")
    check(num(qty) == num("100") and num(amount) == num("800"), "基线：成本账 100 / 800", f"{qty} / {amount}")
    check(num(location_ledger()) == num("100"), "基线：库位账 100", location_ledger())
    check(num(txn_net()) == num("100"), "基线：流水净额 100", txn_net())

    # ② 建盘点单（全盘）
    print("\n② 建盘点单（全盘，账面数服务端快照）")
    status, body = http("POST", "/api/business/erp/stock-take", admin, {
        "warehouse_id": WAREHOUSE,
        "take_type": "FULL",
        "remark": "P13 真机验收：年底全盘",
    })
    take = payload_of(body)
    if not check(status == 200 and take is not None, "建盘点单 200", f"{status} {body}"):
        sys.exit(1)
    take_id = take["id"]
    items = take.get("items") or []
    check(len(items) == 1, "全盘明细 = 该仓 1 个库存桶", str(len(items)))
    check(num(items[0]["book_qty"]) == num("100"), "明细账面数快照 100", items[0]["book_qty"])
    check(num(items[0]["actual_qty"]) == num("100"), "实盘数预填账面数", items[0]["actual_qty"])
    check(take["status"] == "DRAFT", "建单后是草稿", take["status"])
    check(take["take_no"].startswith("ST-"), "单号取号器产出（ST-yyyyMMdd-）", take["take_no"])
    snapshot("建单后（账未动）")

    # ③ 填实盘 92
    print("\n③ 填实盘数 92（差异 -8）")
    item_id = items[0]["id"]
    status, body = http("PUT", f"/api/business/erp/stock-take/{take_id}", admin, {
        "lines": [{"item_id": item_id, "actual_qty": "92"}],
    })
    detail = payload_of(body)
    check(status == 200, "填实盘 200", f"{status} {body}")
    check(num(detail["items"][0]["diff_qty"]) == num("-8"), "差异 = -8（盘亏）", detail["items"][0]["diff_qty"])
    check(num(detail["total_actual_qty"]) == num("92"), "实盘合计 92", detail["total_actual_qty"])
    snapshot("填实盘后（账未动）")

    # ④ 提交审批
    print("\n④ 提交审批（DRAFT → PENDING_POST）")
    status, body = http("PATCH", f"/api/business/erp/stock-take/{take_id}/status", admin,
                        {"status": "PENDING_POST"})
    submitted = payload_of(body)
    check(status == 200, "提交审批 200", f"{status} {body}")
    check(submitted and submitted["status"] == "PENDING_POST", "状态 = 待过账",
          str(submitted and submitted.get("status")))
    instance_id = (submitted or {}).get("approval_instance_id")
    check(bool(instance_id), "已挂审批实例", str(instance_id))

    # 未审批就过账 → 409
    status, body = http("PATCH", f"/api/business/erp/stock-take/{take_id}/status", keeper,
                        {"status": "POSTED"})
    check(status == 409, "未审批就过账被拒（409）", f"{status} {body}")

    # 审批人：仓管主管（制单人 admin 被内控排除）
    status, body = http("GET", "/api/business/flow/task/todo?page=1&page_size=50", keeper)
    todo = payload_of(body) or {}
    tasks = [t for t in (todo.get("list") or []) if t.get("instance_id") == instance_id]
    if not check(status == 200 and len(tasks) >= 1, "仓管主管收到盘点审批待办", f"{status} {body}"):
        sys.exit(1)
    for task in tasks:
        tid = task["id"]
        status, body = http("POST", f"/api/business/flow/task/{tid}/approve", keeper,
                            {"comment": "P13 真机：账实相符，同意过账"})
        check(status == 200, f"审批通过 200（待办 {tid[:8]}）", f"{status} {body}")

    # ⑤ 过账
    print("\n⑤ 过账（PENDING_POST → POSTED）")
    status, body = http("PATCH", f"/api/business/erp/stock-take/{take_id}/status", keeper,
                        {"status": "POSTED"})
    posted = payload_of(body)
    if not check(status == 200 and posted is not None, "过账 200", f"{status} {body}"):
        sys.exit(1)
    check(posted["status"] == "POSTED", "状态 = 已过账", posted["status"])
    qty, amount = snapshot("过账后")
    check(num(qty) == num("92") and num(amount) == num("736"), "成本账 92 / 736（盘亏 8×8=64）",
          f"{qty} / {amount}")
    check(num(location_ledger()) == num("92"), "库位账 92", location_ledger())
    check(take_txn_net(take_id) == num("-8"), "盘点流水净额 -8（OUT）", str(take_txn_net(take_id)))
    entries = entries_of_take(take_id)
    print(f"    · 盘点差异凭证分录：{entries}")
    check(any(row[0] == "1901" and num(row[1]) == num("64") for row in entries),
          "盘亏凭证：借 1901 64", str(entries))
    check(any(row[0] == "1405" and num(row[2]) == num("64") for row in entries),
          "盘亏凭证：贷 1405 64", str(entries))
    check(num(posted.get("total_loss_amount")) == num("64"), "单头盘亏金额 64",
          str(posted.get("total_loss_amount")))
    check(num(posted.get("total_loss_qty")) == num("8"), "单头盘亏数量 8",
          str(posted.get("total_loss_qty")))

    # ⑥ 作废（回滚 + 红冲）
    print("\n⑥ 作废（POSTED → CANCELLED：三本账回滚 + 凭证红冲）")
    status, body = http("PATCH", f"/api/business/erp/stock-take/{take_id}/status", keeper,
                        {"status": "CANCELLED", "reason": "P13 真机：复核实盘数填错，作废重盘"})
    cancelled = payload_of(body)
    check(status == 200, "作废 200", f"{status} {body}")
    check(cancelled and cancelled["status"] == "CANCELLED", "状态 = 已作废",
          str(cancelled and cancelled.get("status")))
    qty, amount = snapshot("作废后")
    check(num(qty) == num("100") and num(amount) == num("800"), "成本账回到 100 / 800", f"{qty} / {amount}")
    check(num(location_ledger()) == num("100"), "库位账回到 100", location_ledger())
    check(take_txn_net(take_id) == num("0"), "盘点流水净额回到 0（正向 + 冲销）", str(take_txn_net(take_id)))
    vouchers = vouchers_of_take(take_id)
    print(f"    · 盘点凭证：{vouchers}")
    check(len(vouchers) == 2, "两张凭证（蓝字 + 红字）", str(vouchers))
    check(any(row[2] == "1" for row in vouchers), "存在红字凭证（is_red=1）", str(vouchers))
    entries = entries_of_take(take_id)
    check(any(row[0] == "1405" and num(row[1]) == num("64") and row[3] == "1" for row in entries),
          "红字凭证：借 1405 64（冲原贷方）", str(entries))
    check(any(row[0] == "1901" and num(row[2]) == num("64") and row[3] == "1" for row in entries),
          "红字凭证：贷 1901 64（冲原借方）", str(entries))

    # ⑦ 巡检
    print("\n⑦ 不变量巡检（全量）")
    env = dict(os.environ, NIK_DATABASE_USER=DB["user"], NIK_DATABASE_PASSWORD=DB["password"],
               NIK_DATABASE_NAME=DB["name"], NIK_DATABASE_HOST=DB["host"],
               NIK_DATABASE_PORT=DB["port"])
    out = subprocess.run([sys.executable, "/opt/cloudpivot/scripts/verify/erp_invariants.py"],
                         capture_output=True, text=True, env=env, check=False)
    tail = [line for line in out.stdout.strip().split("\n") if line.startswith("结果")]
    print("    " + (tail[-1] if tail else "(巡检没输出结果行)"))
    check(out.returncode == 0, "不变量全绿（BLOCKER 0）", out.stdout[-600:])

    print("\n" + "=" * 92)
    print(f"结果：{len(PASSED)} 通过 / {len(FAILED)} 失败")
    for name in FAILED:
        print(f"  ✗ {name}")
    print("=" * 92)
    sys.exit(1 if FAILED else 0)


if __name__ == "__main__":
    main()
