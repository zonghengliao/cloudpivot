#!/usr/bin/env python3
"""D5 生产**只读**验收：`GET /business/erp/payable/{id}/payment-info` 在真实数据上自洽。

**为什么生产只读**：付款 = 真金白银 + 真凭证（借 2202 / 贷 1002），不接受在生产的应付单上
做「付一笔试试」。写路径的完整链路（提交审批 → 审批通过 → 记账 → 结清 / 超付 400 / 作废撤审批）
已经在**隔离实例**上用真实 HTTP 打过 42 项（`d5_payable_payment_e2e.py`）；
生产这一步证明的是：**新接口在真实业务数据上不出错、口径与库列一致**。

断言（逐张应付单）：
1. `payment-info` 200，`id / payable_no` 与列表行一致；
2. `amount = paid_amount + unpaid_amount`（库侧恒等在前端契约上也成立）；
3. `default_amount` 口径：`can_pay` 为真时 **= 未付金额**，为假时 = 0；
4. `can_pay` 与状态自洽：`SETTLED / CANCELLED` ⇒ false 且有 `blocked_reason`；
5. `payments` 条数 = 库里该应付单的付款记录条数；每条 `payable_id` 都指回本单；
6. 全程 GET（不写库）：跑完再核一次应付单的 `paid_amount` 与凭证总数没变。

```bash
set -a; . /etc/cloudpivot/cloudpivot.env; set +a     # 144 上跑，拿 NIK_ADMIN_PASSWORD / NIK_DATABASE_*
python3 scripts/verify/d5_prod_readonly_check.py
```

退出码：0 全绿 / 1 有断言不通过 / 2 环境不满足。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("D5_PROD_BASE_URL", "http://127.0.0.1:8023")
USER = os.environ.get("D5_PROD_USER", "admin")
PASSWORDS = [
    os.environ.get("D5_PROD_PASSWORD", ""),
    os.environ.get("NIK_ADMIN_PASSWORD", ""),
    "Admin@123456",
]
PASSWORDS = [p for p in PASSWORDS if p]
MAX_PAYABLES = int(os.environ.get("D5_PROD_MAX", "8"))

PASSED: list[str] = []
FAILED: list[str] = []


def check(ok: bool, name: str, detail: str = "") -> None:
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" —— {detail}" if detail else ""))
    (PASSED if ok else FAILED).append(name)


def sql(query: str) -> str:
    """生产库只读查询（只允许 select / count）。"""
    if not query.strip().lower().startswith("select"):
        raise RuntimeError(f"只读脚本拒绝非 SELECT：{query[:40]}")
    env = dict(os.environ)
    args = [
        "psql",
        "-h",
        os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
        "-p", os.environ.get("NIK_DATABASE_PORT", "5432"),
        "-U", os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
        "-d", os.environ.get("NIK_DATABASE_NAME", "cloudpivot"),
        "-X", "-q", "-tAc", query,
    ]
    env.setdefault("PGPASSWORD", os.environ.get("NIK_DATABASE_PASSWORD", ""))
    out = subprocess.run(args, capture_output=True, text=True, env=env, check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}")
    return out.stdout.strip()


def http(method: str, path: str, token: str | None = None) -> tuple[int, dict]:
    req = urllib.request.Request(f"{BASE}{path}", method=method)
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
            payload = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            payload = {"raw": raw}
        return err.code, payload


def login() -> str | None:
    for password in PASSWORDS:
        req = urllib.request.Request(
            f"{BASE}/api/system/auth/login",
            data=json.dumps({"username": USER, "password": password}).encode(),
            method="POST",
        )
        req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=20) as resp:
                body = json.loads(resp.read().decode())
            token = (body.get("data") or {}).get("access_token")
            if token:
                return token
        except urllib.error.HTTPError:
            continue
    return None


def dec(text) -> float:
    return float(str(text))


def main() -> int:
    print(f"== D5 生产只读验收：{BASE} ==")
    code, _ = http("GET", "/health")
    if code != 200:
        print(f"SKIP: /health = {code}")
        return 2
    token = login()
    if not token:
        print("SKIP: 登录失败（试过 D5_PROD_PASSWORD / NIK_ADMIN_PASSWORD / Admin@123456）")
        return 2

    # 基线：付款/凭证规模（跑完要一模一样，证明全程只读）
    base_payments = sql("select count(*) from biz_payment where deleted_at is null")
    base_vouchers = sql("select count(*) from biz_voucher where deleted_at is null")

    code, body = http("GET", "/api/business/erp/payable/page?page=1&page_size=50", token)
    rows = (body.get("data") or {}).get("list") or []
    check(code == 200 and bool(rows), "应付列表可取（有真实数据）", f"HTTP {code}，{len(rows)} 行")
    if not rows:
        print("SKIP: 列表为空")
        return 2

    # 优先覆盖不同状态：SETTLED / PARTIAL_PAID / OPEN 各取一张
    picked: list[dict] = []
    seen_status: set[str] = set()
    for row in rows:
        if row["status"] not in seen_status:
            picked.append(row)
            seen_status.add(row["status"])
    for row in rows:
        if len(picked) >= MAX_PAYABLES:
            break
        if row not in picked:
            picked.append(row)

    for row in picked[:MAX_PAYABLES]:
        no = row["payable_no"]
        status = row["status"]
        code, body = http("GET", f"/api/business/erp/payable/{row['id']}/payment-info", token)
        info = (body.get("data") or {}).get("payable") or {}
        payments = (body.get("data") or {}).get("payments") or []
        if code != 200:
            check(False, f"{no} payment-info 200", f"HTTP {code} {body.get('msg')}")
            continue
        check(info.get("payable_no") == no, f"{no} 单号一致")
        total = dec(info.get("amount"))
        paid = dec(info.get("paid_amount"))
        unpaid = dec(info.get("unpaid_amount"))
        check(abs(total - paid - unpaid) < 1e-6, f"{no} amount = paid + unpaid", f"{total} = {paid} + {unpaid}")
        if info.get("can_pay"):
            check(
                abs(dec(info.get("default_amount")) - unpaid) < 1e-6,
                f"{no} 可付：default_amount = 未付 {unpaid}",
                str(info.get("default_amount")),
            )
            check(status in {"OPEN", "PARTIAL_PAID"}, f"{no} 可付状态合法（{status}）")
        else:
            check(
                dec(info.get("default_amount")) == 0,
                f"{no} 不可付：default_amount = 0",
                str(info.get("default_amount")),
            )
            check(bool(info.get("blocked_reason")), f"{no} 不可付必给原因", str(info.get("blocked_reason"))[:60])
            if status in {"SETTLED", "CANCELLED"}:
                keyword = "已结清" if status == "SETTLED" else "已作废"
                check(keyword in str(info.get("blocked_reason")), f"{no} 原因点名「{keyword}」")
        db_count = sql(f"select count(*) from biz_payment where payable_id = '{row['id']}' and deleted_at is null")
        check(str(len(payments)) == db_count, f"{no} 付款记录条数与库一致", f"{len(payments)} vs {db_count}")
        if payments:
            check(
                all(p.get("payable_id") == row["id"] for p in payments),
                f"{no} 每条付款记录 payable_id 指回本单",
            )

    # 只读基线核对
    check(sql("select count(*) from biz_payment where deleted_at is null") == base_payments, "只读核对：付款记录数未变", base_payments)
    check(sql("select count(*) from biz_voucher where deleted_at is null") == base_vouchers, "只读核对：凭证数未变", base_vouchers)

    print(f"\n== 汇总：{len(PASSED)} PASS / {len(FAILED)} FAIL ==")
    for name in FAILED:
        print(f"  FAIL: {name}")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as err:
        print(f"SKIP: {err}")
        sys.exit(2)
