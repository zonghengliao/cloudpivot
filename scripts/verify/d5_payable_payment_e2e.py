#!/usr/bin/env python3
"""D5 真机验收：应付单 → 发起付款（payment-info + 复用 P17 审批付款）。

# 验收的不是「接口能返 200」，而是卡面 §四 的四步真机链路

```
① 找一张 OPEN 应付 → 详情 → 「发起付款」→ 金额预填**未付**（不是应付总额）
② 付一部分 → 状态 PARTIAL_PAID，未付减少
③ 再发起付款 → 预填剩余
④ 付完 → SETTLED，按钮消失（can_pay = false）
```

# 为什么付款要走真审批（而不是把流程停掉直付）

硬红线 2：**复用 P7/P17 的付款逻辑，走审批不绕过**。所以本脚本用两个账号：

* `d5_maker`（**非超管**，只持 `business:erp:payable:{pay,list,detail}`）= 制单人，发付款；
* 超管（admin / `d5_approver`）= 审批人，逐节点投票。

制单人不能批自己（内控 403），所以必须两套账号；审批人按待办上的真实 assignee 逐个登录，
节点有几级 / 会不会签都由流程定义决定，脚本不假设（P17 的做法）。

# 用法

```bash
D5_BASE_URL=http://127.0.0.1:18095 D5_DB_NAME=cloudpivot_d5 \
  python3 scripts/verify/d5_payable_payment_e2e.py           # 跑完自清理
D5_KEEP=1 … python3 scripts/verify/d5_payable_payment_e2e.py # 留夹具排查
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

BASE = os.environ.get("D5_BASE_URL", "http://127.0.0.1:18095")
DB_NAME = os.environ.get("D5_DB_NAME", "cloudpivot_d5")
DB_HOST = os.environ.get("D5_DB_HOST", "127.0.0.1")
DB_PORT = os.environ.get("D5_DB_PORT", "5432")
DB_USER = os.environ.get("D5_DB_USER", "alun")
DB_PASS = os.environ.get("D5_DB_PASSWORD", "alun")
KEEP = os.environ.get("D5_KEEP") == "1"

ADMIN_USER = os.environ.get("D5_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("D5_ADMIN_PASSWORD", "Admin@123456")
# 首登强制改密后的新口令（同一库重跑时直接用它）
NEW_PASS = os.environ.get("D5_ADMIN_NEW_PASSWORD", "D5Admin#2026x")

MAKER_USER = "d5_maker"
APPROVER_USER = "d5_approver"
RO_USER = "d5_readonly"
MAKER_ROLE = "D5_MAKER"
RO_ROLE = "D5_READONLY"
SUPPLIER_ID = "d5d50001-0000-4000-8000-000000000001"
PAYABLE_PREFIX = "AP-D5E2E-"

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
    """在隔离库上跑一条 SQL（psql 走 PGPASSWORD，不打印口令）。"""
    args = [
        "psql",
        "-h",
        DB_HOST,
        "-p",
        DB_PORT,
        "-U",
        DB_USER,
        "-d",
        DB_NAME,
        "-X",
        "-q",
        "-v",
        "ON_ERROR_STOP=1",
        "-tAc",
        sql,
    ]
    env = dict(os.environ, PGPASSWORD=DB_PASS)
    out = subprocess.run(args, capture_output=True, text=True, env=env, check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}\nSQL: {sql}")
    return out.stdout.strip()


def scalar(sql: str) -> str:
    """取单值：psql 偶尔也会打出命令标签，这里只取第一行。"""
    lines = psql(sql).splitlines()
    return lines[0].strip() if lines else ""


def rows(sql: str) -> list[list[str]]:
    """取多行（每行按 `|` 切）。"""
    out = psql(sql)
    return [line.split("|") for line in out.splitlines() if line]


# ---------------------------------------------------------------- HTTP


def http(
    method: str,
    path: str,
    token: str | None = None,
    body: dict | None = None,
) -> tuple[int, dict]:
    url = f"{BASE}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read().decode()
            payload: dict = json.loads(raw) if raw else {}
            return resp.status, payload
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            payload = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            payload = {"raw": raw}
        return err.code, payload


def login(user: str, password: str) -> tuple[int, dict, str | None]:
    code, body = http("POST", "/api/system/auth/login", body={"username": user, "password": password})
    if code != 200:
        return code, body, None
    data = body.get("data") or {}
    return code, body, data.get("access_token")


def message_of(payload: dict) -> str:
    return str(payload.get("msg") or payload.get("message") or payload.get("code") or payload)


_TOKEN_CACHE: dict[str, str | None] = {}


def token_for(username: str, password: str) -> str | None:
    if username not in _TOKEN_CACHE:
        _, _, token = login(username, password)
        _TOKEN_CACHE[username] = token
    return _TOKEN_CACHE[username]


# ---------------------------------------------------------------- 夹具


def new_payable(amount: str, suffix: str) -> tuple[str, str]:
    """造一张 OPEN 应付单（应付由收货过账生成，那一段是 P2-C1 的事，本卡只验收付款链路）。"""
    no = f"{PAYABLE_PREFIX}{suffix}"
    source_no = f"IN-D5E2E-{suffix}"
    pid = scalar(
        f"""
        insert into biz_payable (payable_no, supplier_id, source_type, source_id, source_no,
                                 currency_code, amount, paid_amount, unpaid_amount, credit_days,
                                 due_date, period_code, status)
        values ('{no}', '{SUPPLIER_ID}'::uuid, 'STOCK_IN', gen_random_uuid(), '{source_no}',
                'CNY', {amount}, 0, {amount}, 30,
                (now() at time zone 'Asia/Shanghai')::date + 30, '2026-10', 'OPEN')
        returning id
        """
    )
    return pid, no


def cleanup() -> None:
    """只删本脚本造的行（按单号前缀 + 外键链），绝不动业务数据。"""
    where_payable = f"select id from biz_payable where payable_no like '{PAYABLE_PREFIX}%'"
    where_payment = f"select id from biz_payment where payable_id in ({where_payable})"
    statements = [
        f"delete from sys_notice where biz_type = 'flow' and biz_id in "
        f"(select i.id::text from biz_flow_instance i where i.biz_type = 'PAYABLE_PAY' "
        f" and i.biz_id in ({where_payment}))",
        f"delete from biz_flow_instance where biz_type = 'PAYABLE_PAY' and biz_id in ({where_payment})",
        f"delete from biz_voucher_entry where voucher_id in "
        f"(select id from biz_voucher where source_id in ({where_payment}))",
        f"delete from biz_voucher where source_id in ({where_payment})",
        f"delete from biz_status_log where entity_id in ({where_payable})",
        f"delete from biz_payment where payable_id in ({where_payable})",
        f"delete from biz_payable where payable_no like '{PAYABLE_PREFIX}%'",
    ]
    for sql in statements:
        psql(sql)


def ensure_user(username: str, real_name: str, role_code: str, auth_codes: list[str]) -> None:
    """建角色（挂权限码）+ 用户（口令哈希复制 admin，仅隔离库验签用）+ 授角色。"""
    psql(
        f"""
        insert into sys_role (role_code, role_name, description, status, data_scope)
        values ('{role_code}', '{real_name}', 'D5 验收夹具', 1, 1) on conflict do nothing
        """
    )
    if auth_codes:
        codes = ", ".join(f"'{code}'" for code in auth_codes)
        psql(
            f"""
            insert into sys_role_menu (role_id, menu_id)
            select r.id, m.id from sys_role r, sys_menu m
             where r.role_code = '{role_code}' and m.auth_code in ({codes})
            on conflict do nothing
            """
        )
    # ⚠️ `sys_user.username` 是**部分唯一索引**（WHERE deleted_at IS NULL），
    # 不能写 `on conflict (username)`（PG 报「没有匹配的唯一约束」）→ 无目标 conflict + UPDATE。
    psql(
        f"""
        insert into sys_user (username, password, real_name, must_change_password, status)
        values ('{username}', (select password from sys_user where username = '{ADMIN_USER}'),
                '{real_name}', false, 1)
        on conflict do nothing
        """
    )
    psql(
        f"""
        update sys_user
           set password = (select password from sys_user where username = '{ADMIN_USER}'),
               must_change_password = false, status = 1,
               is_locked = false, lock_time = null, login_fail_count = 0
         where username = '{username}'
        """
    )
    psql(
        f"""
        insert into sys_user_role (user_id, role_id)
        select u.id, r.id from sys_user u, sys_role r
         where u.username = '{username}' and r.role_code = '{role_code}'
        on conflict do nothing
        """
    )


def drop_user(username: str, role_code: str) -> None:
    psql(f"delete from sys_user_role where user_id = (select id from sys_user where username = '{username}')")
    psql(f"delete from sys_user where username = '{username}'")
    psql(f"delete from sys_role_menu where role_id = (select id from sys_role where role_code = '{role_code}')")
    psql(f"delete from sys_role where role_code = '{role_code}'")


def instance_status(instance_id: str) -> str:
    if not instance_id:
        return "(空)"
    return scalar(f"select status from biz_flow_instance where id = '{instance_id}'::uuid")


def pending_tasks(instance_id: str) -> list[tuple[str, str]]:
    """该实例上还没结束的待办：[(task_id, assignee_username)]。"""
    if not instance_id:
        return []
    data = rows(
        f"""
        select t.id, coalesce(u.username, '')
          from biz_flow_task t left join sys_user u on u.id = t.assignee_id
         where t.instance_id = '{instance_id}'::uuid and t.status = 'PENDING'
         order by t.created_at
        """
    )
    return [(row[0], row[1]) for row in data]


def drive_approval(instance_id: str, limit: int = 8) -> str:
    """按待办上的真实 assignee 逐个投票，直到实例不再是 PENDING（或没人可批）。"""
    for _ in range(limit):
        status = instance_status(instance_id)
        if status != "PENDING":
            return status
        progressed = False
        for task_id, username in pending_tasks(instance_id):
            if not username or username == MAKER_USER:
                continue  # 制单人那一票会被内控 403，跳过
            token = token_for(username, NEW_PASS) or token_for(username, ADMIN_PASS)
            if not token:
                continue
            code, _ = http(
                "POST", f"/api/business/flow/task/{task_id}/approve", token, {"comment": "D5：审批通过"}
            )
            if code == 200:
                progressed = True
        if not progressed:
            break
    return instance_status(instance_id)


# ---------------------------------------------------------------- 主流程


def main() -> int:
    print(f"== D5 真机验收：{BASE} / 库 {DB_NAME} ==")

    code, _ = http("GET", "/health")
    if code != 200:
        print(f"SKIP: /health = {code}（实例没起？）")
        return 2

    # ---- admin 登录（首个实例可能要强制改密）----
    code, body, admin_token = login(ADMIN_USER, ADMIN_PASS)
    if admin_token is None:
        code, body, admin_token = login(ADMIN_USER, NEW_PASS)
    if admin_token is None:
        print(f"SKIP: admin 登录失败：{code} {message_of(body)}")
        return 2
    if (body.get("data") or {}).get("must_change_password"):
        code, body = http(
            "PUT", "/api/system/auth/password", admin_token,
            {"old_password": ADMIN_PASS, "new_password": NEW_PASS},
        )
        if code != 200:
            print(f"SKIP: 首登改密失败：{code} {message_of(body)}")
            return 2
        code, body, admin_token = login(ADMIN_USER, NEW_PASS)
        if admin_token is None:
            print(f"SKIP: 改密后重登失败：{code} {message_of(body)}")
            return 2
        print("  （admin 首登已改密，本次用新口令）")
    _TOKEN_CACHE[ADMIN_USER] = admin_token

    cleanup()
    psql(
        f"""
        insert into biz_supplier (id, supplier_code, supplier_name, status, credit_days)
        values ('{SUPPLIER_ID}'::uuid, 'D5E2E-SUP', 'D5 验收供应商', 1, 30) on conflict do nothing
        """
    )
    psql(
        """
        insert into biz_accounting_period (period_year, period_month, period_code,
                                           start_date, end_date, status)
        values (2026, 10, '2026-10', date '2026-10-01', date '2026-10-31', 'OPEN')
        on conflict (period_code) do update set status = 'OPEN'
        """
    )
    # 制单人（非超管，只有应付的 pay/list/detail）与审批人（超管）+ 只读账号（无 pay）
    ensure_user(
        MAKER_USER,
        "D5 制单员",
        MAKER_ROLE,
        ["business:erp:payable:pay", "business:erp:payable:list", "business:erp:payable:detail"],
    )
    ensure_user(APPROVER_USER, "D5 审批人", "D5_APPROVER", [])
    ensure_user(
        RO_USER, "D5 只读", RO_ROLE, ["business:erp:payable:list", "business:erp:payable:detail"]
    )
    psql(
        f"""
        insert into sys_user_role (user_id, role_id)
        select u.id, r.id from sys_user u, sys_role r
         where u.username = '{APPROVER_USER}' and r.role_code = 'SUPER_ADMIN'
        on conflict do nothing
        """
    )

    maker_token = token_for(MAKER_USER, NEW_PASS)
    if maker_token is None:
        print("SKIP: 制单员登录失败（口令应同 admin）")
        return 2

    pid_a, no_a = new_payable("226.0000", "A")
    print(f"\n① OPEN 应付 {no_a} → payment-info")

    # ---- ① OPEN：can_pay=true，预填 = 未付 = 应付 ----
    code, body = http("GET", f"/api/business/erp/payable/{pid_a}/payment-info", maker_token)
    info = (body.get("data") or {}).get("payable") or {}
    check(code == 200, "① payment-info 200", f"HTTP {code} {message_of(body) if code != 200 else ''}")
    check(info.get("can_pay") is True, "① can_pay = true")
    check(info.get("status") == "OPEN", "① status = OPEN", str(info.get("status")))
    check(str(info.get("amount")).startswith("226"), "① amount = 226.00", str(info.get("amount")))
    check(str(info.get("unpaid_amount")).startswith("226"), "① unpaid = 226.00", str(info.get("unpaid_amount")))
    check(
        str(info.get("default_amount")).startswith("226"),
        "① 预填金额 = 未付 226（F2/O1）",
        str(info.get("default_amount")),
    )
    check(info.get("supplier_name") == "D5 验收供应商", "① 供应商名称带出", str(info.get("supplier_name")))
    check(str(info.get("source_no")).startswith("IN-D5E2E-"), "① 来源收货单号带出", str(info.get("source_no")))
    check(((body.get("data") or {}).get("payments") or []) == [], "① 还没有付款记录")

    # ---- ② 付一部分（走真审批）→ PARTIAL_PAID ----
    print("\n② 付 100（提交审批 → 审批人逐节点通过）")
    code, body = http(
        "POST",
        f"/api/business/erp/payable/{pid_a}/pay",
        maker_token,
        {"amount": "100.0000", "pay_date": "2026-10-29", "pay_method": "银行转账"},
    )
    data = body.get("data") or {}
    check(code == 202, "② 付款走审批（HTTP 202 + PENDING_APPROVAL）", f"HTTP {code} outcome={data.get('outcome')}")
    check(data.get("outcome") == "PENDING_APPROVAL", "② outcome = PENDING_APPROVAL", str(data.get("outcome")))
    instance_id = data.get("approval_instance_id") or ""
    payment_no = data.get("payment_no")

    code, body = http("GET", f"/api/business/erp/payable/{pid_a}/payment-info", maker_token)
    pending_info = (body.get("data") or {}).get("payable") or {}
    check(pending_info.get("can_pay") is False, "② 审批中 can_pay = false（按钮消失）")
    check(
        "审批中" in str(pending_info.get("blocked_reason")),
        "② 原因点名「审批中」",
        str(pending_info.get("blocked_reason")),
    )
    check(
        pending_info.get("pending_payment_no") == payment_no,
        "② pending_payment_no 带出",
        f"{pending_info.get('pending_payment_no')} vs {payment_no}",
    )
    check(
        str(pending_info.get("paid_amount")).startswith("0"),
        "② 审批中账一分没动（已付仍是 0）",
        str(pending_info.get("paid_amount")),
    )

    status = drive_approval(instance_id)
    check(status == "APPROVED", "② 审批人通过付款审批（实例 APPROVED）", status)

    code, body = http("GET", f"/api/business/erp/payable/{pid_a}/payment-info", maker_token)
    info2 = (body.get("data") or {}).get("payable") or {}
    payments2 = (body.get("data") or {}).get("payments") or []
    check(info2.get("status") == "PARTIAL_PAID", "② 状态 = PARTIAL_PAID", str(info2.get("status")))
    check(str(info2.get("paid_amount")).startswith("100"), "② 已付 = 100", str(info2.get("paid_amount")))
    check(str(info2.get("unpaid_amount")).startswith("126"), "② 未付 = 126", str(info2.get("unpaid_amount")))
    check(info2.get("can_pay") is True, "② 还能继续付（can_pay = true）")
    check(
        len(payments2) == 1 and payments2[0].get("approval_status") == "APPROVED",
        "② 付款记录 1 条且审批已通过",
        str([p.get("approval_status") for p in payments2]),
    )
    check(
        bool(payments2 and payments2[0].get("voucher_no")),
        "② 付款记录带出付款凭证号（可点回凭证）",
        str(payments2[0].get("voucher_no") if payments2 else None),
    )

    # ---- ③ 再发起付款 → 预填剩余 ----
    print("\n③ 再发起付款 → 预填剩余")
    check(
        str(info2.get("default_amount")).startswith("126"),
        "③ 预填 = 剩余未付 126（不是应付总额 226）",
        str(info2.get("default_amount")),
    )

    # ---- ④ 付完 → SETTLED，按钮消失 ----
    print("\n④ 付完 126 → SETTLED")
    code, body = http(
        "POST",
        f"/api/business/erp/payable/{pid_a}/pay",
        maker_token,
        {"amount": "126.0000", "pay_date": "2026-10-29", "pay_method": "银行转账"},
    )
    data = body.get("data") or {}
    check(code == 202, "④ 第二笔也走审批（202）", f"HTTP {code} {message_of(body)}")
    status = drive_approval(data.get("approval_instance_id") or "")
    check(status == "APPROVED", "④ 第二笔审批通过", status)

    code, body = http("GET", f"/api/business/erp/payable/{pid_a}/payment-info", maker_token)
    info3 = (body.get("data") or {}).get("payable") or {}
    payments3 = (body.get("data") or {}).get("payments") or []
    check(info3.get("status") == "SETTLED", "④ 状态 = SETTLED", str(info3.get("status")))
    check(str(info3.get("unpaid_amount")).startswith("0"), "④ 未付 = 0", str(info3.get("unpaid_amount")))
    check(info3.get("can_pay") is False, "④ 结清后 can_pay = false（按钮消失）")
    check("已结清" in str(info3.get("blocked_reason")), "④ 原因点名「已结清」", str(info3.get("blocked_reason")))
    check(len(payments3) == 2, "④ 两笔付款记录都在（可追溯）", str(len(payments3)))
    check(
        [str(p.get("amount"))[:3] for p in payments3] == ["100", "126"],
        "④ 两笔金额分别是 100 / 126",
        str([p.get("amount") for p in payments3]),
    )

    # ---- ⑤ 超付 → 400 + 精确文案 ----
    print("\n⑤ 超付 → 400 精确文案")
    pid_b, _ = new_payable("226.0000", "B")
    code, body = http(
        "POST",
        f"/api/business/erp/payable/{pid_b}/pay",
        maker_token,
        {"amount": "226.0100", "pay_date": "2026-10-29"},
    )
    msg = message_of(body)
    check(code == 400, "⑤ 超付 400", f"HTTP {code} {msg}")
    check("本次付款 226.01" in msg and "超过未付余额 226" in msg, "⑤ 文案点名「本次 X > 未付 Y」", msg)

    # ---- ⑥ 作废 → 撤未完成付款审批（反向） ----
    print("\n⑥ 审批中作废应付单 → 撤审批")
    pid_c, _ = new_payable("300.0000", "C")
    code, body = http(
        "POST",
        f"/api/business/erp/payable/{pid_c}/pay",
        maker_token,
        {"amount": "300.0000", "pay_date": "2026-10-29"},
    )
    instance_c = (body.get("data") or {}).get("approval_instance_id") or ""
    code, body = http(
        "POST", f"/api/business/erp/payable/{pid_c}/cancel", writer_token(), {"reason": "D5 验收：作废并撤审批"}
    )
    check(code == 200, "⑥ 作废成功", f"HTTP {code} {message_of(body)}")
    check(instance_status(instance_c) == "CANCELLED", "⑥ 审批实例已终止", instance_status(instance_c))
    code, body = http("GET", f"/api/business/erp/payable/{pid_c}/payment-info", maker_token)
    info_c = (body.get("data") or {}).get("payable") or {}
    check(info_c.get("can_pay") is False, "⑥ 作废后 can_pay = false")
    check("已作废" in str(info_c.get("blocked_reason")), "⑥ 原因点名「已作废」", str(info_c.get("blocked_reason")))
    pay_status = scalar(
        f"select approval_status from biz_payment where payable_id = '{pid_c}'::uuid order by created_at desc limit 1"
    )
    check(pay_status == "CANCELLED", "⑥ 付款记录标 CANCELLED", pay_status)

    # ---- ⑦ 权限：只读账号能看不能付 ----
    print("\n⑦ 只读账号：能看 payment-info，不能付款（403）")
    ro_token = token_for(RO_USER, NEW_PASS)
    if ro_token is None:
        check(False, "⑦ 只读账号登录失败（口令应与 admin 相同）")
    else:
        code, _ = http("GET", f"/api/business/erp/payable/{pid_b}/payment-info", ro_token)
        check(code == 200, "⑦ 只读账号能看 payment-info（detail 码）", f"HTTP {code}")
        code, body = http("POST", f"/api/business/erp/payable/{pid_b}/pay", ro_token, {"amount": "1.0000"})
        check(code == 403, "⑦ 只读账号不能付款（403）", f"HTTP {code} {message_of(body)}")

    # ---- 收尾 ----
    if not KEEP:
        cleanup()
        drop_user(RO_USER, RO_ROLE)
        drop_user(MAKER_USER, MAKER_ROLE)
        drop_user(APPROVER_USER, "D5_APPROVER")
        residue = scalar(f"select count(*) from biz_payable where payable_no like '{PAYABLE_PREFIX}%'")
        check(residue == "0", "清理：夹具应付单残留 = 0", residue)
        accounts = scalar("select count(*) from biz_account where account_code in ('2202','1002')")
        check(accounts == "2", "清理：付款科目仍在（没误删主数据）", accounts)
    else:
        print("（D5_KEEP=1：保留夹具）")

    print(f"\n== 汇总：{len(PASSED)} PASS / {len(FAILED)} FAIL ==")
    for name in FAILED:
        print(f"  FAIL: {name}")
    return 0 if not FAILED else 1


def writer_token() -> str | None:
    """作废应付单需要 `business:erp:payable:cancel`：制单角色没给，用 admin。"""
    return _TOKEN_CACHE.get(ADMIN_USER)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as err:
        print(f"SKIP: {err}")
        sys.exit(2)
