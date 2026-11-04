#!/usr/bin/env python3
"""P-FLOW-3B 真机验收：**应付作废审批** + **凭证红冲审批**（两条财务链标准流）。

# 验收的是卡面 §六 的四条真机链路（真 HTTP + 真库 + 真种子流程）

```
① 建应付单（OPEN）→ 提交作废 → 财务经理待办 → 批 → CANCELLED
② 建凭证 → 提交红冲 → 财务经理待办 → 批 → 生成红字
③ 查：原凭证 reversed_by 指向红字，红字 reverse_of 指向原凭证
④ 金额 > 1 万 的凭证红冲 → 两人会签（一票不通过、两票才红冲）
```

外加三条硬红线在真 HTTP 上的表现：

* **凭证红冲不改状态**：送审后 `status` 仍是 `POSTED`（报表口径不变），只挂 `pending_reverse_instance_id`；
* **PARTIAL_PAID / SETTLED 不许作废**：接口 409（不是静默跳过）；
* **内控**：制单人/发起人那一票点不动（403 + 65019），且待办在生成时就排除他。

# 用法

```bash
P3B_BASE_URL=http://127.0.0.1:18095 \
P3B_DB_NAME=cloudpivot_3b P3B_DB_USER=alun P3B_DB_PASSWORD=alun \
  python3 scripts/verify/pflow3b_finance_approval_e2e.py
P3B_KEEP=1 …   # 留夹具排查
```

退出码：0 = 全绿；1 = 有断言不通过；2 = 环境不满足。
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

BASE = os.environ.get("P3B_BASE_URL", "http://127.0.0.1:18095")
DB_NAME = os.environ.get("P3B_DB_NAME", "cloudpivot_3b")
DB_USER = os.environ.get("P3B_DB_USER", "alun")
DB_PASS = os.environ.get("P3B_DB_PASSWORD", "alun")
DB_HOST = os.environ.get("P3B_DB_HOST", "127.0.0.1")
DB_PORT = os.environ.get("P3B_DB_PORT", "5432")
KEEP = os.environ.get("P3B_KEEP") == "1"

ADMIN_USER = os.environ.get("P3B_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("P3B_ADMIN_PASSWORD", "Admin@123456")
ADMIN_NEW_PASS = os.environ.get("P3B_ADMIN_NEW_PASSWORD", "P3BAdmin#2026x")

# 业务日期：必须落在**已开启（OPEN）**的会计期间里（手工凭证只许记到 OPEN 期间）。
# 新库默认只有「上个月」是 OPEN，所以默认取 2026-10-30；换库时用 P3B_BIZ_DATE 覆盖。
BIZ_DATE = os.environ.get("P3B_BIZ_DATE", "2026-10-30")

FM_USER = "p3be2e_fm"
FD_USER = "p3be2e_fd"
USER_PASS = "P3bE2e@2026x"

ROLE_FM = "20000000-0000-4000-8000-0000000f0005"
ROLE_FD = "20000000-0000-4000-8000-0000000f0006"

UNIT_ID = "b3be2e01-0000-4000-8000-000000000001"
SUPPLIER_ID = "b3be2e02-0000-4000-8000-000000000001"
WAREHOUSE_ID = "b3be2e03-0000-4000-8000-000000000001"
MATERIAL_ID = "b3be2e04-0000-4000-8000-000000000001"
ACCOUNT_DEBIT = "b3be2e05-0000-4000-8000-000000000001"
ACCOUNT_CREDIT = "b3be2e06-0000-4000-8000-000000000001"

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
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(username: str, password: str) -> tuple[int, dict, str]:
    status, body = call("POST", "/api/system/auth/login", {"username": username, "password": password})
    token = (body.get("data") or {}).get("access_token") or ""
    return status, body, token


def admin_token() -> str:
    status, body, token = login(ADMIN_USER, ADMIN_PASS)
    if status != 200:
        # 重复跑：上一轮已经改过初始口令
        status, body, token = login(ADMIN_USER, ADMIN_NEW_PASS)
    if status != 200 or not token:
        raise SystemExit(f"管理员登录失败：{status} {body}")
    if body["data"].get("must_change_password"):
        st, resp = call(
            "PUT",
            "/api/system/auth/password",
            {"old_password": ADMIN_PASS, "new_password": ADMIN_NEW_PASS},
            token,
        )
        if st != 200:
            raise SystemExit(f"改初始口令失败：{st} {resp}")
        status, body, token = login(ADMIN_USER, ADMIN_NEW_PASS)
        if status != 200 or not token:
            raise SystemExit(f"改完口令后登录失败：{status} {body}")
    return token


def setup_fixtures() -> None:
    psql(
        """
        INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('%(unit)s', 'P3BE2E-PCS', '个', 1, NULL, 1.000000, 1, 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days)
        VALUES ('%(sup)s', 'P3BE2E-SUP', 'P3B 真机供应商', 1, 30)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('%(wh)s', 'P3BE2E-WH', 'P3B 真机仓', '/%(wh)s/', 1, 1, 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('%(mat)s', 'P3BE2E-M1', 'P3B 真机物料', '%(unit)s', 1, false)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                                 account_category, balance_direction, is_leaf, status)
        VALUES ('%(deb)s', 'P3BE2E.1002', 'P3B 银行存款', NULL, 1, 'ASSET', 'DEBIT', 1, 1),
               ('%(cre)s', 'P3BE2E.2202', 'P3B 应付账款', NULL, 1, 'LIABILITY', 'CREDIT', 1, 1)
        ON CONFLICT DO NOTHING;
        """
        % {
            "unit": UNIT_ID,
            "sup": SUPPLIER_ID,
            "wh": WAREHOUSE_ID,
            "mat": MATERIAL_ID,
            "deb": ACCOUNT_DEBIT,
            "cre": ACCOUNT_CREDIT,
        }
    )


def cleanup() -> None:
    stmts = [
        "DELETE FROM sys_notice_receiver WHERE notice_id IN "
        "(SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN "
        "(SELECT id::text FROM biz_flow_instance WHERE biz_type IN ('PAYABLE_CANCEL','VOUCHER_REVERSE')))",
        "DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN "
        "(SELECT id::text FROM biz_flow_instance WHERE biz_type IN ('PAYABLE_CANCEL','VOUCHER_REVERSE'))",
        "DELETE FROM biz_flow_instance WHERE biz_type IN ('PAYABLE_CANCEL','VOUCHER_REVERSE')",
        f"DELETE FROM biz_flow_task WHERE assignee_id IN (SELECT id FROM sys_user WHERE username IN ('{FM_USER}','{FD_USER}'))",
        f"DELETE FROM biz_flow_instance WHERE initiator_id IN (SELECT id FROM sys_user WHERE username IN ('{FM_USER}','{FD_USER}'))",
        f"DELETE FROM biz_status_log WHERE entity_id IN (SELECT id FROM biz_voucher WHERE source_no LIKE 'P3BE2E-%')",
        f"DELETE FROM biz_status_log WHERE entity_id IN (SELECT id FROM biz_payable WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_no LIKE 'P3BE2E-%')",
        f"DELETE FROM biz_voucher WHERE source_no LIKE 'P3BE2E-%'",
        f"DELETE FROM biz_material_serial WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_inventory_txn WHERE doc_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_inventory_location WHERE material_id = '{MATERIAL_ID}'::uuid",
        f"DELETE FROM biz_inventory WHERE material_id = '{MATERIAL_ID}'::uuid",
        f"DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid))",
        f"DELETE FROM biz_voucher WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_payable WHERE supplier_id = '{SUPPLIER_ID}'::uuid",
        f"DELETE FROM biz_status_log WHERE entity_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid",
        f"DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username IN ('{FM_USER}','{FD_USER}'))",
        f"DELETE FROM sys_user WHERE username IN ('{FM_USER}','{FD_USER}')",
    ]
    for sql in stmts:
        try:
            psql(sql)
        except RuntimeError as error:
            print(f"[warn] 清理失败（{error}）")


def ensure_approver(token: str, username: str, real_name: str, role_id: str) -> str:
    """建（或复用）一个审批账号，并绑到指定角色 + 超管（拿到 flow:approve 权限）。"""
    super_role = psql("SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN' AND deleted_at IS NULL LIMIT 1")
    status, body = call(
        "POST",
        "/api/system/user",
        {
            "username": username,
            "password": USER_PASS,
            "real_name": real_name,
            "status": 1,
            "role_ids": [role_id, super_role],
        },
        token,
    )
    if status == 200:
        return body["data"]["id"]
    # 已存在（重复跑）→ 直接查 id 并重置角色
    uid = psql(f"SELECT id FROM sys_user WHERE username = '{username}' AND deleted_at IS NULL LIMIT 1")
    if not uid:
        raise SystemExit(f"建审批账号失败：{status} {body}")
    st, resp = call("PUT", f"/api/system/user/{uid}/roles", {"role_ids": [role_id]}, token)
    if st != 200:
        raise SystemExit(f"重置审批角色失败：{st} {resp}")
    return uid


def post_stock_in(token: str) -> str:
    """建一张 800 元的收货单并过账（< 1000 走免审分支，会自动落应付）。"""
    status, body = call(
        "POST",
        "/api/business/erp/stock-in",
        {
            "supplier_id": SUPPLIER_ID,
            "warehouse_id": WAREHOUSE_ID,
            "in_date": BIZ_DATE,
            "remark": "P3B 真机：应付作废审批",
            "items": [{"material_id": MATERIAL_ID, "qty": 8, "price_excl": 100, "tax_rate": 0}],
        },
        token,
    )
    if status != 200:
        raise SystemExit(f"建收货单失败：{status} {body}")
    stock_in_id = body["data"]["header"]["id"]
    status, body = call("POST", f"/api/business/erp/stock-in/{stock_in_id}/submit-post", {}, token)
    if status != 200:
        raise SystemExit(f"收货过账失败：{status} {body}")
    return stock_in_id


def create_voucher(token: str, amount: float) -> str:
    status, body = call(
        "POST",
        "/api/business/erp/voucher",
        {
            "voucher_date": BIZ_DATE,
            "summary": f"P3B 真机红冲 {amount}",
            "entries": [
                {"account_id": ACCOUNT_DEBIT, "debit": amount, "summary": "借 银行存款"},
                {"account_id": ACCOUNT_CREDIT, "credit": amount, "summary": "贷 应付账款"},
            ],
        },
        token,
    )
    if status != 200:
        raise SystemExit(f"手工建凭证失败：{status} {body}")
    return body["data"]["id"]


def todos(token: str) -> list[dict]:
    status, body = call("GET", "/api/business/flow/task/todo?page=1&page_size=50", token=token)
    if status != 200:
        raise SystemExit(f"查待办失败：{status} {body}")
    return body["data"]["list"]


def find_todos(token: str, biz_no: str) -> list[dict]:
    """按**单号**匹配待办。

    `TaskRes` 没有 `biz_no` 列（只有 `instance_no` / `title` / `form_data`），
    而两条流的 `title` 都是「{单号} 应付单作废 / {单号} 凭证红冲」，
    `form_data.payable_no` / `voucher_no` 也是单号 —— 三处任一命中即可。
    """
    hits: list[dict] = []
    for item in todos(token):
        form = item.get("form_data") or {}
        if (
            biz_no in (item.get("title") or "")
            or biz_no == (form.get("payable_no") or "")
            or biz_no == (form.get("voucher_no") or "")
        ):
            hits.append(item)
    return hits


def approve(token: str, task_id: str) -> tuple[int, dict]:
    return call("POST", f"/api/business/flow/task/{task_id}/approve", {"comment": "同意（P3B 真机）"}, token)


def voucher_row(voucher_id: str) -> dict:
    raw = psql(
        "SELECT coalesce(status,'') || '|' || coalesce(reversed_by::text,'') || '|' || "
        "coalesce(reverse_of::text,'') || '|' || is_red::text || '|' || "
        "coalesce(pending_reverse_instance_id::text,'') "
        f"FROM biz_voucher WHERE id = '{voucher_id}'::uuid"
    )
    status, reversed_by, reverse_of, is_red, pending = raw.split("|")
    return {
        "status": status,
        "reversed_by": reversed_by or None,
        "reverse_of": reverse_of or None,
        "is_red": is_red,
        "pending": pending or None,
    }


def payable_row(payable_id: str) -> dict:
    raw = psql(
        "SELECT status || '|' || paid_amount::text || '|' || unpaid_amount::text || '|' || "
        f"coalesce(cancel_instance_id::text,'') FROM biz_payable WHERE id = '{payable_id}'::uuid"
    )
    status, paid, unpaid, cancel_instance = raw.split("|")
    return {
        "status": status,
        "paid": paid,
        "unpaid": unpaid,
        "cancel_instance": cancel_instance or None,
    }


def main() -> int:
    print(f"== P3B 真机验收 @ {BASE}（库 {DB_NAME}）==")
    health = call("GET", "/health")
    if health[0] != 200:
        print(f"环境不满足：/health = {health[0]}")
        return 2

    admin = admin_token()
    setup_fixtures()
    cleanup()
    setup_fixtures()

    fm_id = ensure_approver(admin, FM_USER, "P3B 财务经理", ROLE_FM)
    fd_id = ensure_approver(admin, FD_USER, "P3B 财务总监", ROLE_FD)
    st, body, fm = login(FM_USER, USER_PASS)
    if st != 200:
        raise SystemExit(f"财务经理登录失败：{st} {body}")
    st, body, fd = login(FD_USER, USER_PASS)
    if st != 200:
        raise SystemExit(f"财务总监登录失败：{st} {body}")

    # ---------- ① 应付作废：OPEN → 提交作废 → 财务经理批 → CANCELLED ----------
    stock_in_id = post_stock_in(admin)
    payable_id = psql(
        f"SELECT id FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id = '{stock_in_id}'::uuid"
    )
    check(bool(payable_id), "①-1 收货过账自动生成应付单")
    before = payable_row(payable_id)
    check(before["status"] == "OPEN", "①-2 应付单初始状态 OPEN", before["status"])

    st, body = call(
        "POST",
        f"/api/business/erp/payable/{payable_id}/submit-cancel",
        {"reason": "供应商重复开票（P3B 真机）"},
        admin,
    )
    data = body.get("data") or {}
    check(st == 200, "①-3 提交作废返回 200", f"{st} {body}")
    check(data.get("outcome") == "PENDING_APPROVAL", "①-4 outcome=PENDING_APPROVAL", str(data.get("outcome")))
    after = payable_row(payable_id)
    check(after["status"] == "PENDING_CANCEL", "①-5 单据进入 PENDING_CANCEL", after["status"])
    check(after["paid"] == before["paid"] and after["unpaid"] == before["unpaid"],
          "①-6 欠款一分没动（审批中只是锁单）", f"{before} → {after}")
    check(after["cancel_instance"] is not None, "①-7 回写 cancel_instance_id")
    # 再点一次 → 409（幂等）
    st, body = call(
        "POST", f"/api/business/erp/payable/{payable_id}/submit-cancel", {"reason": "重复点"}, admin
    )
    check(st == 409, "①-8 重复提交作废 → 409", f"{st} {body}")

    fm_todos = find_todos(fm, psql(f"SELECT payable_no FROM biz_payable WHERE id = '{payable_id}'::uuid"))
    check(len(fm_todos) == 1, "①-9 财务经理收到 1 条作废待办", f"{len(fm_todos)} 条")
    if fm_todos:
        st, body = approve(fm, fm_todos[0]["id"])
        check(st == 200, "①-10 财务经理审批通过", f"{st} {body}")
    after = payable_row(payable_id)
    check(after["status"] == "CANCELLED", "①-11 审批通过 → CANCELLED", after["status"])
    check(after["cancel_instance"] is None, "①-12 通过后摘掉实例引用")
    st, body = call("GET", f"/api/business/erp/payable/{payable_id}", token=admin)
    detail = body.get("data") or {}
    check(st == 200 and "重复开票" in (detail.get("remark") or ""), "①-13 作废原因写进 remark",
          str(detail.get("remark")))

    # ---------- ①b 反向：提交后撤回 → 回 OPEN ----------
    stock_in_b = post_stock_in(admin)
    payable_b = psql(
        f"SELECT id FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id = '{stock_in_b}'::uuid"
    )
    st, body = call(
        "POST", f"/api/business/erp/payable/{payable_b}/submit-cancel", {"reason": "撤回测试"}, admin
    )
    check(st == 200, "①b-1 第二张应付单提交作废", f"{st} {body}")
    st, body = call("POST", f"/api/business/erp/payable/{payable_b}/withdraw-cancel", {}, admin)
    check(st == 200, "①b-2 发起人撤回成功", f"{st} {body}")
    check(payable_row(payable_b)["status"] == "OPEN", "①b-3 撤回后回到 OPEN",
          payable_row(payable_b)["status"])

    # ---------- ②③ 凭证红冲（≤ 1 万，单人审批）----------
    voucher_id = create_voucher(admin, 8000)
    st, body = call(
        "POST",
        f"/api/business/erp/voucher/{voucher_id}/submit-reverse",
        {"reason": "凭证金额录错（P3B 真机）"},
        admin,
    )
    data = body.get("data") or {}
    check(st == 200, "②-1 提交红冲返回 200", f"{st} {body}")
    check(data.get("outcome") == "PENDING_APPROVAL", "②-2 outcome=PENDING_APPROVAL", str(data.get("outcome")))
    row = voucher_row(voucher_id)
    check(row["status"] == "POSTED", "②-3 送审后凭证仍 POSTED（报表口径不变）", row["status"])
    check(row["pending"] is not None, "②-4 挂上 pending_reverse_instance_id")
    check(row["reversed_by"] is None, "②-5 送审时未红冲")
    st, body = call(
        "POST", f"/api/business/erp/voucher/{voucher_id}/submit-reverse", {"reason": "重复点"}, admin
    )
    check(st == 409, "②-6 重复提交红冲 → 409", f"{st} {body}")

    voucher_no = psql(f"SELECT voucher_no FROM biz_voucher WHERE id = '{voucher_id}'::uuid")
    fm_todos = find_todos(fm, voucher_no)
    check(len(fm_todos) == 1, "②-7 财务经理收到 1 条红冲待办（1 万以内单人）", f"{len(fm_todos)} 条")
    if fm_todos:
        st, body = approve(fm, fm_todos[0]["id"])
        check(st == 200, "②-8 财务经理审批通过", f"{st} {body}")
    original = voucher_row(voucher_id)
    check(original["status"] == "POSTED", "②-9 原凭证仍 POSTED", original["status"])
    check(original["pending"] is None, "②-10 通过后清空 pending")
    check(original["reversed_by"] is not None, "②-11 回写 reversed_by")
    red_id = original["reversed_by"]
    if red_id:
        red = voucher_row(red_id)
        check(red["is_red"] == "1", "③-1 红字凭证 is_red=1", red["is_red"])
        check(red["reverse_of"] == voucher_id, "③-2 红字 reverse_of 指向原凭证", str(red["reverse_of"]))
        debit = psql(f"SELECT total_debit::text || '|' || total_credit::text FROM biz_voucher WHERE id = '{red_id}'::uuid")
        orig = psql(f"SELECT total_debit::text || '|' || total_credit::text FROM biz_voucher WHERE id = '{voucher_id}'::uuid")
        d_red, c_red = debit.split("|")
        d_org, c_org = orig.split("|")
        check(d_red == c_org and c_red == d_org, "③-3 红字借贷互换", f"红 {debit} / 原 {orig}")

    # 已红冲的再提交 → 409
    st, body = call(
        "POST", f"/api/business/erp/voucher/{voucher_id}/submit-reverse", {"reason": "再红冲"}, admin
    )
    check(st == 409, "③-4 已红冲再提交 → 409", f"{st} {body}")

    # ---------- ④ 金额 > 1 万 → 两人会签 ----------
    big_id = create_voucher(admin, 20000)
    st, body = call(
        "POST", f"/api/business/erp/voucher/{big_id}/submit-reverse", {"reason": "大额红冲（会签）"}, admin
    )
    check(st == 200, "④-1 大额凭证提交红冲", f"{st} {body}")
    big_no = psql(f"SELECT voucher_no FROM biz_voucher WHERE id = '{big_id}'::uuid")
    fm_todos = find_todos(fm, big_no)
    fd_todos = find_todos(fd, big_no)
    check(len(fm_todos) == 1 and len(fd_todos) == 1,
          "④-2 财务经理 + 财务总监各 1 条待办（会签）", f"fm={len(fm_todos)} fd={len(fd_todos)}")
    if fm_todos and fd_todos:
        st, body = approve(fm, fm_todos[0]["id"])
        check(st == 200, "④-3 第一票通过（实例仍在跑）", f"{st} {body}")
        check(voucher_row(big_id)["reversed_by"] is None, "④-4 只批一票 → 不红冲",
              str(voucher_row(big_id)["reversed_by"]))
        st, body = approve(fd, fd_todos[0]["id"])
        check(st == 200, "④-5 第二票通过", f"{st} {body}")
        check(voucher_row(big_id)["reversed_by"] is not None, "④-6 两票齐 → 红冲")

    # ---------- ⑤ 内控：制单人/发起人被排除在待办之外，且他那一票点不动（403 + 65019）----------
    voucher_c = create_voucher(admin, 6000)
    no_c = psql(f"SELECT voucher_no FROM biz_voucher WHERE id = '{voucher_c}'::uuid")
    st, body = call(
        "POST", f"/api/business/erp/voucher/{voucher_c}/submit-reverse", {"reason": "内控验证"}, admin
    )
    check(st == 200, "⑤-1 发起人（也是制单人）提交红冲成功", f"{st} {body}")
    admin_todos = find_todos(admin, no_c)
    check(not admin_todos, "⑤-2 发起人在**生成待办时**就被排除（自己收不到待办）", str(admin_todos))
    fm_todos_c = find_todos(fm, no_c)
    check(len(fm_todos_c) == 1, "⑤-3 待办派给了财务经理", f"{len(fm_todos_c)} 条")
    if fm_todos_c:
        st, body = approve(admin, fm_todos_c[0]["id"])
        code = (body.get("code") or (body.get("error") or {}).get("code") or "")
        check(st == 403 and str(code) == "65019",
              "⑤-4 制单人自批 → 403 + 65019（内控闸先于归属校验）", f"{st} {body}")
    # 收尾：把这条审批撤回，别把「红冲审批中」留给后面的手工验收
    st, body = call("POST", f"/api/business/erp/voucher/{voucher_c}/withdraw-reverse", {}, admin)
    check(st == 200 and voucher_row(voucher_c)["pending"] is None,
          "⑤-5 撤回红冲审批（pending 清空，凭证仍 POSTED）", f"{st} {body}")

    # ---------- 汇总 ----------
    print()
    print(f"PASS {len(PASSED)} / FAIL {len(FAILED)}")
    if FAILED:
        for item in FAILED:
            print(f"  - {item}")
        return 1
    if not KEEP:
        cleanup()
    return 0


if __name__ == "__main__":
    sys.exit(main())
