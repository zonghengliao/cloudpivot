#!/usr/bin/env python3
"""D9 真机验收：单据复制（收货单 / 调拨单 / 任务）。

# 验收的不是「接口能返 200」，而是卡面 §六 的三步真机链路

```
① 找一张已过账收货单 → 复制 → 新草稿，明细全带出，单号是新的
② 改数量 → 提交 → 过账（不影响原单）
③ 任务复制 → 标题带「（副本）」
```

外加卡面 §五 的运行时口径（单测已在库层钉过，这里再用真 HTTP 复核一遍）：

* 采购单关联在复制单上被**清空**（防待收额度二次占用）；
* 无 `create` 权限的账号复制 → **403**，文案点名权限码；
* 原单一个字节都不改（状态 / 单号 / 更新时间 / 明细）；
* 调拨单复制不带序列号。

# 为什么用两个账号

权限类断言必须用**非超管**账号打（超管短路一切校验）。`d9_no_create` 只挂
`list` / `detail` 码，用它打 `POST …/copy` 才能看到真的 403。

# 用法

```bash
D9_BASE_URL=http://127.0.0.1:18096 D9_DB_NAME=cloudpivot_d9 \
  python3 scripts/verify/d9_document_copy_e2e.py           # 跑完自清理
D9_KEEP=1 … python3 scripts/verify/d9_document_copy_e2e.py # 留夹具排查
```

退出码：0 = 全绿；1 = 有断言不通过；2 = 环境不满足。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("D9_BASE_URL", "http://127.0.0.1:18096")
DB_NAME = os.environ.get("D9_DB_NAME", "cloudpivot_d9")
DB_HOST = os.environ.get("D9_DB_HOST", "127.0.0.1")
DB_PORT = os.environ.get("D9_DB_PORT", "5432")
DB_USER = os.environ.get("D9_DB_USER", "alun")
DB_PASS = os.environ.get("D9_DB_PASSWORD", "alun")
KEEP = os.environ.get("D9_KEEP") == "1"

ADMIN_USER = os.environ.get("D9_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("D9_ADMIN_PASSWORD", "Admin@123456")
NEW_PASS = os.environ.get("D9_ADMIN_NEW_PASSWORD", "D9Admin#2026x")

RO_USER = "d9_no_create"
RO_ROLE = "D9_RO"
RO_CODES = [
    "business:erp:stock-in:list",
    "business:erp:stock-in:detail",
    "business:erp:transfer:list",
    "business:erp:transfer:detail",
    "business:task:read",
]

UNIT_ID = "d9d90001-0000-4000-8000-000000000001"
SUPPLIER_ID = "d9d90002-0000-4000-8000-000000000001"
WH_A = "d9d90003-0000-4000-8000-000000000001"
WH_B = "d9d90003-0000-4000-8000-000000000002"
M_PLAIN = "d9d90004-0000-4000-8000-000000000001"
M_SN = "d9d90004-0000-4000-8000-000000000002"
PO_ID = "d9d90005-0000-4000-8000-000000000001"
PO_ITEM_ID = "d9d90005-0000-4000-8000-000000000002"
TITLE_PREFIX = "D9E2E "

PASSED: list[str] = []
FAILED: list[str] = []


def check(ok: bool, name: str, detail: str = "") -> bool:
    if ok:
        PASSED.append(name)
        print(f"  [PASS] {name}" + (f" —— {detail}" if detail else ""))
    else:
        FAILED.append(name)
        print(f"  [FAIL] {name}" + (f" —— {detail}" if detail else ""))
    return ok


# ---------------------------------------------------------------- psql


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


def scalar(sql: str) -> str:
    lines = psql(sql).splitlines()
    return lines[0].strip() if lines else ""


def rows(sql: str) -> list[list[str]]:
    return [line.split("|") for line in psql(sql).splitlines() if line]


# ---------------------------------------------------------------- HTTP


def http(method: str, path: str, token: str | None = None, body: dict | None = None) -> tuple[int, dict]:
    url = f"{BASE}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, (json.loads(raw) if raw else {})
        except json.JSONDecodeError:
            return err.code, {"raw": raw}


def login(user: str, password: str) -> tuple[int, dict, str | None]:
    code, body = http("POST", "/api/system/auth/login", body={"username": user, "password": password})
    data = body.get("data") or {}
    return code, body, data.get("access_token")


def token_for(username: str, password: str) -> str | None:
    _, _, token = login(username, password)
    return token


def msg_of(body: dict) -> str:
    return str(body.get("msg") or body.get("message") or "")


def data_of(body: dict) -> dict:
    value = body.get("data")
    return value if isinstance(value, dict) else {}


# ---------------------------------------------------------------- 夹具


def cleanup() -> None:
    """只删本脚本造的行（按夹具 id / 标题前缀 / 单号前缀收敛），顺序按外键链。"""
    title_like = f"{TITLE_PREFIX}%"
    statements = [
        # 任务链：评论 → 断父指针 → 任务
        f"delete from biz_task_comment where task_id in (select id from biz_task where title like '{title_like}')",
        f"update biz_task set parent_id = null where title like '{title_like}'",
        f"delete from biz_task where title like '{title_like}'",
        # 收货链：序列号 → 流水 → 状态日志 → 凭证/应付 → 明细 → 单头
        f"delete from biz_material_serial where source_type = 'PURCHASE' and source_id in "
        f"(select id from biz_stock_in where supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"delete from biz_inventory_txn where doc_id in "
        f"(select id from biz_stock_in where supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"delete from biz_status_log where entity_id in "
        f"(select id from biz_stock_in where supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"delete from biz_voucher_entry where voucher_id in (select id from biz_voucher where source_id in "
        f"(select id from biz_stock_in where supplier_id = '{SUPPLIER_ID}'::uuid))",
        f"delete from biz_voucher where source_id in "
        f"(select id from biz_stock_in where supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"delete from biz_payable where source_id in "
        f"(select id from biz_stock_in where supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"delete from biz_stock_in_item where stock_in_id in "
        f"(select id from biz_stock_in where supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"delete from biz_stock_in where supplier_id = '{SUPPLIER_ID}'::uuid",
        # 调拨链：审批实例 → 序列号 → 明细 → 单头
        f"delete from biz_flow_instance where biz_type = 'STOCK_TRANSFER' and biz_id in "
        f"(select id from biz_stock_transfer where from_warehouse_id in "
        f"('{WH_A}'::uuid, '{WH_B}'::uuid))",
        f"delete from biz_status_log where entity_id in "
        f"(select id from biz_stock_transfer where from_warehouse_id in ('{WH_A}'::uuid, '{WH_B}'::uuid))",
        f"delete from biz_stock_transfer_item_serial where item_id in "
        f"(select i.id from biz_stock_transfer_item i join biz_stock_transfer t on t.id = i.transfer_id "
        f"where t.from_warehouse_id in ('{WH_A}'::uuid, '{WH_B}'::uuid))",
        f"delete from biz_stock_transfer_item where transfer_id in "
        f"(select id from biz_stock_transfer where from_warehouse_id in ('{WH_A}'::uuid, '{WH_B}'::uuid))",
        f"delete from biz_stock_transfer where from_warehouse_id in ('{WH_A}'::uuid, '{WH_B}'::uuid)",
        # 采购单夹具
        f"delete from biz_purchase_order_item where order_id = '{PO_ID}'::uuid",
        f"delete from biz_purchase_order where id = '{PO_ID}'::uuid",
        # 库存残渣
        f"delete from biz_inventory where material_id in ('{M_PLAIN}'::uuid, '{M_SN}'::uuid) "
        f"and warehouse_id in ('{WH_A}'::uuid, '{WH_B}'::uuid)",
    ]
    for sql in statements:
        psql(sql)
    drop_user(RO_USER, RO_ROLE)


def ensure_user(username: str, real_name: str, role_code: str, auth_codes: list[str]) -> None:
    psql(
        f"insert into sys_role (role_code, role_name, description, status, data_scope) "
        f"values ('{role_code}', '{real_name}', 'D9 验收夹具', 1, 1) on conflict do nothing"
    )
    if auth_codes:
        codes = ", ".join(f"'{code}'" for code in auth_codes)
        psql(
            f"insert into sys_role_menu (role_id, menu_id) "
            f"select r.id, m.id from sys_role r, sys_menu m "
            f"where r.role_code = '{role_code}' and m.auth_code in ({codes}) on conflict do nothing"
        )
    # `sys_user.username` 是部分唯一索引（WHERE deleted_at IS NULL）→ 不能写 on conflict (username)
    psql(
        f"insert into sys_user (username, password, real_name, must_change_password, status) "
        f"values ('{username}', (select password from sys_user where username = '{ADMIN_USER}'), "
        f"'{real_name}', false, 1) on conflict do nothing"
    )
    psql(
        f"update sys_user set password = (select password from sys_user where username = '{ADMIN_USER}'), "
        f"must_change_password = false, status = 1, is_locked = false, lock_time = null, "
        f"login_fail_count = 0 where username = '{username}'"
    )
    psql(
        f"insert into sys_user_role (user_id, role_id) "
        f"select u.id, r.id from sys_user u, sys_role r "
        f"where u.username = '{username}' and r.role_code = '{role_code}' on conflict do nothing"
    )


def drop_user(username: str, role_code: str) -> None:
    psql(f"delete from sys_user_role where user_id = (select id from sys_user where username = '{username}')")
    psql(f"delete from sys_user where username = '{username}'")
    psql(f"delete from sys_role_menu where role_id = (select id from sys_role where role_code = '{role_code}')")
    psql(f"delete from sys_role where role_code = '{role_code}'")


def seed_master_data() -> None:
    psql(
        f"insert into biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) "
        f"values ('{UNIT_ID}'::uuid, 'D9-PCS', '个', 1, null, 1.000000, 1, 1) on conflict do nothing"
    )
    psql(
        f"insert into biz_supplier (id, supplier_code, supplier_name, status) "
        f"values ('{SUPPLIER_ID}'::uuid, 'D9-SUP', 'D9 复制验收供应商', 1) on conflict do nothing"
    )
    psql(
        f"insert into biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status) "
        f"values ('{WH_A}'::uuid, 'D9-WH1', 'D9 收货仓', '/{WH_A}/', 1, 1, 1), "
        f"('{WH_B}'::uuid, 'D9-WH2', 'D9 调入仓', '/{WH_B}/', 1, 1, 1) on conflict do nothing"
    )
    psql(
        f"insert into biz_material (id, material_code, material_name, unit_id, status, serial_required) "
        f"values ('{M_PLAIN}'::uuid, 'D9-M1', 'D9 螺丝', '{UNIT_ID}'::uuid, 1, false), "
        f"('{M_SN}'::uuid, 'D9-M2', 'D9 电机', '{UNIT_ID}'::uuid, 1, true) on conflict do nothing"
    )


def seed_purchase_order(qty: str) -> None:
    psql(
        f"insert into biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, "
        f"total_qty, total_amount, total_tax, total_with_tax) "
        f"values ('{PO_ID}'::uuid, 'PO-D9E2E-000001', '{SUPPLIER_ID}'::uuid, current_date, "
        f"'{WH_A}'::uuid, 'SUBMITTED', {qty}, 0, 0, 0) on conflict do nothing"
    )
    psql(
        f"insert into biz_purchase_order_item (id, order_id, line_no, material_id, unit_id, qty, price, "
        f"price_incl, tax_rate, amount, tax_amount, with_tax_amount) "
        f"values ('{PO_ITEM_ID}'::uuid, '{PO_ID}'::uuid, 1, '{M_PLAIN}'::uuid, '{UNIT_ID}'::uuid, "
        f"{qty}, 2, 0, 0, {qty}, 0, {qty}) on conflict do nothing"
    )


# ---------------------------------------------------------------- 主流程


def main() -> int:
    code, body = http("GET", "/health")
    if code != 200:
        print(f"环境不满足：/health = {code}")
        return 2

    token = token_for(ADMIN_USER, ADMIN_PASS)
    if not token:
        # 首登强制改密：换新口令再来一次
        code, body, token = login(ADMIN_USER, NEW_PASS)
        if not token:
            print(f"环境不满足：admin 登录失败（{code} {msg_of(body)}）")
            return 2
    # 首登强制改密：令牌能拿到但业务接口会 403，必须先改密
    code, body = http("GET", "/api/system/auth/me", token)
    if code == 200 and (data_of(body).get("must_change_password") or body.get("data", {}).get("must_change_password")):
        http(
            "PUT",
            "/api/system/auth/password",
            token,
            {"old_password": ADMIN_PASS, "new_password": NEW_PASS},
        )
        token = token_for(ADMIN_USER, NEW_PASS)
        if not token:
            print("环境不满足：改密后 admin 登录失败")
            return 2

    print("== 夹具 ==")
    cleanup()
    seed_master_data()
    seed_purchase_order("30")
    ensure_user(RO_USER, "D9 只读账号", RO_ROLE, RO_CODES)
    print(f"  supplier={SUPPLIER_ID} wh={WH_A} material={M_PLAIN}")

    # ============ ① 收货单：建 → 过账 → 复制 ============
    print("== ① 收货单复制 ==")
    create_body = {
        "source_order_id": PO_ID,
        "supplier_id": SUPPLIER_ID,
        "warehouse_id": WH_A,
        "items": [
            {
                "purchase_order_item_id": PO_ITEM_ID,
                "material_id": M_PLAIN,
                "qty": "10",
                "price_excl": "2",
                "tax_rate": "13",
            }
        ],
    }
    code, body = http("POST", "/api/business/erp/stock-in", token, create_body)
    if code != 200:
        check(False, "收货单建单", f"{code} {msg_of(body)}")
        return 1
    origin = data_of(body)
    origin_id = (origin.get("header") or {}).get("id")
    origin_no = (origin.get("header") or {}).get("in_no")
    check(bool(origin_id), "收货单建单成功", f"{origin_no}")

    code, body = http(
        "POST", f"/api/business/erp/stock-in/{origin_id}/submit-post", token, {"status": "POSTED"}
    )
    posted = data_of(body).get("header") or {}
    check(
        code == 200 and posted.get("status") == "POSTED",
        "原单过账成功（含税 22.6 < 1000，免审直过）",
        f"{code} status={posted.get('status')}",
    )
    origin_voucher = posted.get("voucher_id")
    origin_payable = posted.get("payable_id")
    check(bool(origin_voucher) and bool(origin_payable), "原单过账生成了凭证 + 应付", f"{origin_voucher} / {origin_payable}")

    origin_snapshot_before = scalar(
        f"select (to_jsonb(t))::text from biz_stock_in t where id = '{origin_id}'::uuid"
    )

    code, body = http("POST", f"/api/business/erp/stock-in/{origin_id}/copy", token, {})
    copied = data_of(body)
    copied_header = copied.get("header") or {}
    copied_id = copied_header.get("id")
    copied_no = copied_header.get("in_no")
    check(code == 200 and bool(copied_id), "复制接口 200 且返回新单", f"{code} {copied_no} {msg_of(body)}")
    check(copied_no != origin_no, "新单号 ≠ 原单号", f"{origin_no} → {copied_no}")
    check(
        str(copied_no or "").startswith("IN-"),
        "新单号走 biz_seq 取号（IN-yyyyMMdd-NNNNNN）",
        str(copied_no),
    )
    check(copied_header.get("status") == "DRAFT", "新单状态 = DRAFT", str(copied_header.get("status")))
    check(
        copied_header.get("source_order_id") in (None, ""),
        "卡面决策 B：采购单关联被清空",
        f"source_order_id={copied_header.get('source_order_id')}",
    )
    check(
        all(item.get("source_item_id") in (None, "") for item in copied.get("items") or []),
        "明细行的 source_item_id 也被清空",
    )
    check(
        copied_header.get("voucher_id") in (None, "") and copied_header.get("payable_id") in (None, ""),
        "凭证 / 应付关联被清空",
    )
    check(copied_header.get("posted_at") in (None, ""), "过账时间被清空")

    origin_items = (origin.get("items") or []) if origin.get("items") else []
    if not origin_items:
        _, body = http("GET", f"/api/business/erp/stock-in/{origin_id}", token)
        origin_items = data_of(body).get("items") or []
    copied_items = copied.get("items") or []
    same_items = len(origin_items) == len(copied_items) and all(
        left.get("material_id") == right.get("material_id")
        and float(left.get("qty", 0)) == float(right.get("qty", 0))
        and float(left.get("price", 0)) == float(right.get("price", 0))
        for left, right in zip(origin_items, copied_items)
    )
    check(same_items, "明细全带出（物料 / 数量 / 单价一致）", f"{len(copied_items)} 行")

    # ============ ② 改数量 → 提交 → 过账（不影响原单） ============
    print("== ② 复制单改数量 → 提交 → 过账 ==")
    update_body = {
        "supplier_id": SUPPLIER_ID,
        "warehouse_id": WH_A,
        "items": [{"material_id": M_PLAIN, "qty": "4", "price_excl": "2", "tax_rate": "13"}],
    }
    code, body = http("PUT", f"/api/business/erp/stock-in/{copied_id}", token, update_body)
    check(code == 200, "复制单改数量成功", f"{code} {msg_of(body)}")
    code, body = http(
        "POST", f"/api/business/erp/stock-in/{copied_id}/submit-post", token, {"status": "POSTED"}
    )
    reposted = data_of(body).get("header") or {}
    check(
        code == 200 and reposted.get("status") == "POSTED",
        "复制单过账成功",
        f"{code} qty={reposted.get('total_qty')}",
    )
    check(
        float(reposted.get("total_qty") or 0) == 4.0,
        "复制单过账后数量 = 改后的 4（不是原单的 10）",
        str(reposted.get("total_qty")),
    )

    origin_snapshot_after = scalar(
        f"select (to_jsonb(t))::text from biz_stock_in t where id = '{origin_id}'::uuid"
    )
    check(
        origin_snapshot_before == origin_snapshot_after,
        "原单整行一个字节都没变（含 updated_at）",
    )
    code, body = http("GET", f"/api/business/erp/stock-in/{origin_id}", token)
    origin_after = data_of(body).get("header") or {}
    check(origin_after.get("status") == "POSTED", "原单仍是 POSTED", str(origin_after.get("status")))
    check(
        float(origin_after.get("total_qty") or 0) == 10.0,
        "原单数量仍是 10",
        str(origin_after.get("total_qty")),
    )
    check(
        scalar(
            f"select count(*) from biz_inventory_txn where doc_type = 'STOCK_IN' "
            f"and doc_id = '{origin_id}'::uuid"
        )
        == "1",
        "原单库存流水仍只有 1 条",
    )

    # ============ ③ 调拨单复制 ============
    print("== ③ 调拨单复制 ==")
    code, body = http(
        "POST",
        "/api/business/erp/transfer",
        token,
        {
            "from_warehouse_id": WH_A,
            "to_warehouse_id": WH_B,
            "reason": "D9 调拨复制验收",
            "items": [{"material_id": M_PLAIN, "qty": "3"}],
        },
    )
    transfer = data_of(body)
    transfer_id = transfer.get("id")
    transfer_no = transfer.get("transfer_no")
    if not check(code == 200 and bool(transfer_id), "调拨单建单成功", f"{code} {transfer_no} {msg_of(body)}"):
        return 1
    code, body = http(
        "PATCH", f"/api/business/erp/transfer/{transfer_id}/status", token, {"status": "PENDING"}
    )
    check(code == 200, "调拨单提交审批成功", f"{code} {msg_of(body)}")
    code, body = http("GET", f"/api/business/erp/transfer/{transfer_id}", token)
    pending = data_of(body)
    check(pending.get("status") == "PENDING", "原调拨单已 PENDING", str(pending.get("status")))

    code, body = http("POST", f"/api/business/erp/transfer/{transfer_id}/copy", token, {})
    tcopy = data_of(body)
    check(code == 200 and bool(tcopy.get("id")), "调拨单复制接口 200", f"{code} {tcopy.get('transfer_no')}")
    check(tcopy.get("transfer_no") != transfer_no, "调拨单新单号 ≠ 原单号", f"{transfer_no} → {tcopy.get('transfer_no')}")
    check(str(tcopy.get("transfer_no") or "").startswith("TK-"), "调拨单新号走 biz_seq", str(tcopy.get("transfer_no")))
    check(tcopy.get("status") == "DRAFT", "复制单状态 = DRAFT", str(tcopy.get("status")))
    check(
        tcopy.get("from_warehouse_id") == WH_A and tcopy.get("to_warehouse_id") == WH_B,
        "两仓带出",
    )
    check(tcopy.get("reason") == "D9 调拨复制验收", "原因带出", str(tcopy.get("reason")))
    t_items = tcopy.get("items") or []
    check(
        len(t_items) == 1 and t_items[0].get("material_id") == M_PLAIN and float(t_items[0].get("qty", 0)) == 3.0,
        "明细带出（物料 / 数量一致）",
        str(t_items[0].get("qty") if t_items else None),
    )
    check(all(not (item.get("serials") or []) for item in t_items), "序列号不复制（复制单不凭空造号码）")
    check(tcopy.get("approval_instance_id") in (None, ""), "审批实例被清空")
    code, body = http("GET", f"/api/business/erp/transfer/{transfer_id}", token)
    check(
        data_of(body).get("status") == "PENDING",
        "原调拨单仍是 PENDING（复制不改原单）",
        str(data_of(body).get("status")),
    )

    # ============ ④ 任务复制 ============
    print("== ④ 任务复制 ==")
    title = f"{TITLE_PREFIX}月度对账"
    code, body = http(
        "POST",
        "/api/business/task",
        token,
        {"title": title, "description": "D9 复制验收用描述", "priority": "P1", "estimate_hours": 3.5},
    )
    task = data_of(body)
    task_id = task.get("id")
    if not check(code == 200 and bool(task_id), "任务建单成功", f"{code} {msg_of(body)}"):
        return 1
    http("POST", f"/api/business/task/{task_id}/comment", token, {"content": "D9 源任务评论"})
    http(
        "POST",
        f"/api/business/task/{task_id}/subtask",
        token,
        {"title": f"{TITLE_PREFIX}子任务"},
    )

    code, body = http("POST", f"/api/business/task/{task_id}/copy", token, {})
    tcopy = data_of(body)
    check(code == 200 and bool(tcopy.get("task_no")), "任务复制接口 200", f"{code} {tcopy.get('task_no')}")
    check(str(tcopy.get("task_no") or "").startswith("TSK-"), "任务副本编号走取号器", str(tcopy.get("task_no")))
    new_task_id = tcopy.get("id")
    code, body = http("GET", f"/api/business/task/{new_task_id}", token)
    detail = data_of(body)
    check(
        str(detail.get("title") or "").endswith("（副本）"),
        "副本标题带「（副本）」",
        str(detail.get("title")),
    )
    check(detail.get("priority") == "P1", "优先级带出", str(detail.get("priority")))
    check(float(detail.get("estimate_hours") or 0) == 3.5, "预估工时带出", str(detail.get("estimate_hours")))
    check(detail.get("status") == "TODO", "副本状态 = TODO", str(detail.get("status")))
    check(detail.get("description") == "D9 复制验收用描述", "描述带出")
    check(
        not (detail.get("children") or []),
        "子任务不复制",
        f"{len(detail.get('children') or [])} 个",
    )
    comments = scalar(
        f"select count(*) from biz_task_comment where task_id = '{new_task_id}'::uuid"
    )
    check(comments == "0", "评论不复制", f"{comments} 条")
    code, body = http("GET", f"/api/business/task/{task_id}", token)
    origin_task = data_of(body)
    check(origin_task.get("title") == title, "原任务标题没变", str(origin_task.get("title")))
    check(
        len(origin_task.get("children") or []) == 1,
        "原任务的子任务还在",
        f"{len(origin_task.get('children') or [])} 个",
    )

    # ============ ⑤ 权限：无 create 码 → 403 ============
    print("== ⑤ 权限（复制 = 建单权限） ==")
    ro_token = token_for(RO_USER, NEW_PASS) or token_for(RO_USER, ADMIN_PASS)
    if not check(bool(ro_token), "只读账号登录成功"):
        return 1
    for label, path in [
        ("收货单", f"/api/business/erp/stock-in/{origin_id}/copy"),
        ("调拨单", f"/api/business/erp/transfer/{transfer_id}/copy"),
        ("任务", f"/api/business/task/{task_id}/copy"),
    ]:
        code, body = http("POST", path, ro_token, {})
        check(
            code == 403 and "create" in msg_of(body),
            f"{label}复制：无 create 权限 → 403 且文案点名权限码",
            f"{code} {msg_of(body)}",
        )

    # ============ ⑥ 不存在 / 已删的原单 ============
    print("== ⑥ 边界 ==")
    code, body = http(
        "POST", "/api/business/erp/stock-in/00000000-0000-0000-0000-000000000000/copy", token, {}
    )
    check(code == 404, "原单不存在 → 404", f"{code} {msg_of(body)}")
    code, body = http("POST", "/api/business/erp/stock-in/not-a-uuid/copy", token, {})
    check(code == 400, "id 非 UUID → 400（不静默当没传）", f"{code} {msg_of(body)}")

    # ============ 汇总 ============
    print()
    print(f"== 结果：{len(PASSED)} 通过 / {len(FAILED)} 失败 ==")
    for name in FAILED:
        print(f"  ✗ {name}")

    if not KEEP:
        print("== 清理夹具 ==")
        cleanup()
        left = scalar(
            f"select count(*) from biz_stock_in where supplier_id = '{SUPPLIER_ID}'::uuid"
        )
        print(f"  残留收货单 {left} 张（应为 0）")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
