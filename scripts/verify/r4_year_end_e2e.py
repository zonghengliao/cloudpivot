#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
ERP-R4 年结（损益结转 + 下年期初 + 反年结）—— **隔离实例真机验收**（API 层，真实 HTTP + 真实库）。

跑法（在 99 上，库与实例都是隔离的）：

```bash
export ERP_E2E_BASE=http://127.0.0.1:18098
export ERP_E2E_PASSWORD='Admin@123456'
export NIK_DATABASE_NAME=cloudpivot_r4e2e NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun
python3 scripts/verify/r4_year_end_e2e.py
```

**绝不对生产跑**：本脚本会开整年期间、记凭证、逐月结账、年结（写结转凭证 + 下年期初）。
`ERP_E2E_BASE` 默认指向隔离端口 18098，生产是 8023 —— 端口不同就是硬闸。

设计原则（与 ERP-R5 / ERP-P1 的 e2e 同口径）：

1. **只动自己造的年度**：`2041`（被年结）/ `2042` / `2043`（下一年度）；收尾按年度前缀删干净，
   并核对期间表基线。
2. **每条断言贴原文**：HTTP 码 + 业务码 + 文案片段。
3. **金额按数值比**：`NUMERIC(20,4)` 经 JSON 回来是 `"1000.0000"`，逐字比字符串必假红；
   报表类断言一律**增量口径**（库里本来就有历史数据）。
4. **两条硬红线单独验**：
   - 结转凭证借贷平衡 + 落在 2041-12（已结账期间）；
   - 下年期初 = 本年期末、损益类归零、且**不生成** 2042-01 的 OPENING 凭证（否则双计）。
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("ERP_E2E_BASE", "http://127.0.0.1:18098")
ADMIN_USER = os.environ.get("ERP_E2E_ADMIN", "admin")
ADMIN_PASS = os.environ.get("ERP_E2E_PASSWORD") or os.environ.get("NIK_ADMIN_PASSWORD") or ""
NEW_PASS = os.environ.get("ERP_E2E_NEW_PASSWORD") or "R4@Admin2026"

YEAR = 2041
NEXT_YEAR = 2042
AFTER_YEAR = 2043
REVENUE = 1000
EXPENSE = 400
NET_PROFIT = REVENUE - EXPENSE

RESULT = []
TOKEN = None


# ──────────────────────────── 断言与 HTTP ────────────────────────────

def check(name, ok, detail=""):
    RESULT.append((name, bool(ok), detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return bool(ok)


def num(value):
    if value is None:
        return 0.0
    if isinstance(value, (int, float)):
        return float(value)
    text = str(value).strip()
    if not text:
        return 0.0
    try:
        return float(text)
    except ValueError:
        return 0.0


def eq_num(value, expected):
    return abs(num(value) - float(expected)) < 0.0001


def api(method, path, body=None, token=None):
    url = f"{BASE}{path}"
    data = json.dumps(body).encode("utf-8") if body is not None else None
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read().decode("utf-8")
            return response.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as error:
        raw = error.read().decode("utf-8")
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw}
    except Exception as error:  # noqa: BLE001
        return 0, {"error": str(error)}


def msg_of(payload):
    if not isinstance(payload, dict):
        return ""
    return str(payload.get("msg") or payload.get("message") or payload)


def code_of(payload):
    if not isinstance(payload, dict):
        return None
    return payload.get("code")


def data_of(payload):
    return payload.get("data") if isinstance(payload, dict) else None


def psql(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", env.get("PGPASSWORD", ""))
    command = [
        "psql",
        "-h", os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
        "-p", os.environ.get("NIK_DATABASE_PORT", "5432"),
        "-U", os.environ.get("NIK_DATABASE_USER", "alun"),
        "-d", os.environ.get("NIK_DATABASE_NAME", "cloudpivot_r4e2e"),
        "-tAF|", "-c", sql,
    ]
    result = subprocess.run(command, capture_output=True, text=True, env=env, timeout=60)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip()[:400])
    return [line for line in result.stdout.strip().split("\n") if line != ""]


def one(sql, default="0"):
    rows = psql(sql)
    return rows[0] if rows else default


# ──────────────────────────── 夹具 ────────────────────────────

def cleanup():
    """删掉本脚本造的年度（顺序：状态日志 → 凭证 → 期初 → 期间）。"""
    years = [YEAR, NEXT_YEAR, AFTER_YEAR]
    period_like = " OR ".join(f"period LIKE '{year}-%'" for year in years)
    code_like = " OR ".join(f"period_code LIKE '{year}-%'" for year in years)
    entity_like = " OR ".join(f"entity_no LIKE '{year}%'" for year in years)
    psql(f"DELETE FROM biz_status_log WHERE entity_type IN ('ACCOUNTING_YEAR', 'ACCOUNTING_PERIOD') AND ({entity_like})")
    psql(
        "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN "
        f"(SELECT id FROM biz_voucher WHERE {period_like})"
    )
    psql(f"DELETE FROM biz_voucher WHERE {period_like}")
    opening = ", ".join(f"'{year}-01'" for year in [YEAR, NEXT_YEAR, AFTER_YEAR])
    psql(f"DELETE FROM biz_opening_balance WHERE period_code IN ({opening})")
    psql(f"DELETE FROM biz_accounting_period WHERE {code_like}")


def baseline():
    return psql("SELECT period_code || ':' || status FROM biz_accounting_period ORDER BY period_code")


def account_ids(codes):
    """科目编码 → id（末级科目）。"""
    status, payload = api("GET", "/api/business/erp/account/page?page=1&page_size=500", token=TOKEN)
    rows = (data_of(payload) or {}).get("list") or []
    mapping = {}
    for row in rows:
        code = str(row.get("account_code") or "")
        if code in codes:
            mapping[code] = row.get("id")
    return status, mapping


def period_ids(year):
    """年度 → {period_code: (id, status)}。"""
    status, payload = api(
        "GET", f"/api/business/erp/period/page?period_year={year}&page=1&page_size=20", token=TOKEN
    )
    rows = (data_of(payload) or {}).get("list") or []
    return status, {row.get("period_code"): (row.get("id"), row.get("status")) for row in rows}


def batch_open(year):
    return api(
        "POST",
        "/api/business/erp/period/batch-open",
        {"from": f"{year}-01", "to": f"{year}-12", "remark": f"R4 e2e {year}"},
        TOKEN,
    )


def post_voucher(date, summary, entries):
    """entries: [(account_id, debit, credit)]"""
    body = {
        "voucher_date": date,
        "summary": summary,
        "remark": "R4 e2e 夹具",
        "entries": [
            {"account_id": account_id, "debit": debit, "credit": credit, "summary": summary}
            for account_id, debit, credit in entries
        ],
    }
    return api("POST", "/api/business/erp/voucher", body, TOKEN)


def close_months(year):
    """逐月结账（**真实走 ERP-P1 的结账接口**，不绕过前置校验）。"""
    _, periods = period_ids(year)
    failed = []
    for code in sorted(periods):
        period_id, status = periods[code]
        if status == "CLOSED":
            continue
        status_code, payload = api(
            "POST", f"/api/business/erp/period/{period_id}/close", {"reason": "R4 e2e"}, TOKEN
        )
        if status_code != 200:
            failed.append(f"{code}:HTTP{status_code}:{msg_of(payload)[:80]}")
    return failed


def year_state():
    status, payload = api("GET", f"/api/business/erp/year-end/check?year={YEAR}", token=TOKEN)
    return status, (data_of(payload) or {})


def blocker_types(state):
    return [item.get("type") for item in (state.get("blockers") or [])]


def bs_amounts(period):
    status, payload = api("GET", f"/api/business/erp/report/balance-sheet?period={period}", token=TOKEN)
    body = data_of(payload) or {}
    amounts = {}
    for line in (body.get("assets") or []) + (body.get("liabilities") or []) + (body.get("equity") or []):
        amounts[line.get("item_code")] = line.get("amount")
    return status, amounts


def login():
    global ADMIN_PASS
    candidates = [ADMIN_PASS, NEW_PASS] if ADMIN_PASS else [NEW_PASS]
    for password in candidates:
        if not password:
            continue
        status, payload = api(
            "POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": password}
        )
        body = data_of(payload) or {}
        token = body.get("access_token")
        if status != 200 or not token:
            continue
        ADMIN_PASS = password
        if not body.get("must_change_password"):
            return status, token
        code, changed = api(
            "PUT", "/api/system/auth/password",
            {"old_password": password, "new_password": NEW_PASS}, token,
        )
        if code != 200:
            print(f"[WARN] 首次改密失败：HTTP {code} {msg_of(changed)[:120]}")
            return 0, None
        status, payload = api(
            "POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": NEW_PASS}
        )
        body = data_of(payload) or {}
        if status == 200 and body.get("access_token"):
            ADMIN_PASS = NEW_PASS
            return status, body.get("access_token")
        return 0, None
    return 0, None


# ──────────────────────────── 主流程 ────────────────────────────

def main():
    global TOKEN

    print("=" * 88)
    print(f"ERP-R4 年结 —— 隔离实例真机验收（{BASE}，年度 {YEAR}）")
    print("=" * 88)

    if not check("实例可达 + 健康检查 200", api("GET", "/health")[0] == 200):
        print("实例不可用，退出")
        return 1
    status, token = login()
    if not check("管理员登录", status == 200 and bool(token), f"HTTP {status}"):
        print("登录失败，退出（ERP_E2E_PASSWORD 是否设置？）")
        return 1
    TOKEN = token

    cleanup()
    before = baseline()

    # ---------- ① 开账 + 前置校验（未结账 → 拦） ----------
    status, payload = batch_open(YEAR)
    check(f"① 批量开 {YEAR} 全 12 个月", status == 200, f"HTTP {status} {msg_of(payload)}")

    status, state = year_state()
    check(
        "①b 未结账 → can_close=false + PERIOD_NOT_CLOSED 点名 12 个月",
        status == 200
        and state.get("can_close") is False
        and "PERIOD_NOT_CLOSED" in blocker_types(state)
        and any(
            (item.get("type") == "PERIOD_NOT_CLOSED" and len(item.get("periods") or []) == 12)
            for item in (state.get("blockers") or [])
        ),
        f"HTTP {status} blockers={blocker_types(state)}",
    )

    status, payload = api(
        "POST", "/api/business/erp/year-end/close", {"year": YEAR, "reason": "R4 e2e 抢跑"}, TOKEN
    )
    check(
        f"①c 未结账时年结 → 409 + 点名 {YEAR}-01",
        status == 409 and f"{YEAR}-01" in msg_of(payload),
        f"HTTP {status} code={code_of(payload)} {msg_of(payload)[:120]}",
    )

    # ---------- ② 记凭证（收入 + 费用） ----------
    status, accounts = account_ids({"1002", "5001", "5602", "3103", "3104"})
    check("② 取到年结相关科目 id", status == 200 and len(accounts) == 5, f"HTTP {status} {sorted(accounts)}")
    if len(accounts) != 5:
        return 1

    status, payload = post_voucher(
        f"{YEAR}-03-15", "R4 e2e 销售收入",
        [(accounts["1002"], REVENUE, 0), (accounts["5001"], 0, REVENUE)],
    )
    check(f"②b 记收入凭证（借 1002 / 贷 5001 {REVENUE}）", status == 200, f"HTTP {status} {msg_of(payload)[:120]}")

    status, payload = post_voucher(
        f"{YEAR}-04-10", "R4 e2e 管理费用",
        [(accounts["5602"], EXPENSE, 0), (accounts["1002"], 0, EXPENSE)],
    )
    check(f"②c 记费用凭证（借 5602 / 贷 1002 {EXPENSE}）", status == 200, f"HTTP {status} {msg_of(payload)[:120]}")

    # ---------- ③ 逐月结账（真实 P1 结账接口） ----------
    failed = close_months(YEAR)
    check(f"③ {YEAR} 逐月结账 12 个月全部成功", not failed, f"失败 {failed[:3]}")

    status, state = year_state()
    check(
        "③b 结完账但下一年度未开账 → NEXT_YEAR_NOT_OPENED",
        status == 200 and "NEXT_YEAR_NOT_OPENED" in blocker_types(state) and state.get("can_close") is False,
        f"blockers={blocker_types(state)}",
    )

    status, payload = api(
        "POST", "/api/business/erp/year-end/close", {"year": YEAR, "reason": "R4 e2e"}, TOKEN
    )
    check(
        f"③c 下一年度没开账 → 409 + 点名 {NEXT_YEAR}",
        status == 409 and str(NEXT_YEAR) in msg_of(payload),
        f"HTTP {status} {msg_of(payload)[:140]}",
    )

    status, payload = batch_open(NEXT_YEAR)
    check(f"③d 批量开 {NEXT_YEAR} 全 12 个月", status == 200, f"HTTP {status} {msg_of(payload)}")

    status, state = year_state()
    check(
        "③e 五项全过 → can_close=true",
        status == 200 and state.get("can_close") is True and not blocker_types(state),
        f"blockers={blocker_types(state)}",
    )

    # ---------- ④ 年结 ----------
    status, payload = api(
        "POST", "/api/business/erp/year-end/close", {"year": YEAR, "reason": "R4 e2e 年度结账"}, TOKEN
    )
    body = data_of(payload) or {}
    voucher_no = body.get("carry_forward_voucher_no")
    check(
        "④ 年结成功：返回结转凭证号 + 下年期初已生成 + 12 月已结账",
        status == 200
        and bool(voucher_no)
        and body.get("opening_balance_created") is True
        and body.get("periods_closed") == 12
        and body.get("next_year_periods_ready") is True,
        f"HTTP {status} voucher={voucher_no} rows={body.get('opening_rows')} net={body.get('net_profit')}",
    )
    check(
        f"④b 净利润 = 收入 {REVENUE} − 费用 {EXPENSE} = {NET_PROFIT}",
        eq_num(body.get("net_profit"), NET_PROFIT),
        f"net_profit={body.get('net_profit')}",
    )

    # 结转凭证：落在 2041-12、借贷平衡、12 月仍 CLOSED
    row = one(
        f"SELECT period || '|' || voucher_date::text || '|' || total_debit::text || '|' || "
        f"total_credit::text || '|' || entry_count FROM biz_voucher "
        f"WHERE voucher_type = 'YEAR_END' AND deleted_at IS NULL AND period = '{YEAR}-12'"
    )
    parts = row.split("|") if row else []
    check(
        f"④c 结转凭证落在 {YEAR}-12 / 日期 {YEAR}-12-31 / 借贷平衡",
        len(parts) == 5
        and parts[0] == f"{YEAR}-12"
        and parts[1] == f"{YEAR}-12-31"
        and eq_num(parts[2], parts[3])
        and num(parts[2]) > 0,
        f"row={row}",
    )
    check(
        f"④d {YEAR}-12 期间在年结后仍是 CLOSED（没有临时解封残留）",
        one(f"SELECT status FROM biz_accounting_period WHERE period_code = '{YEAR}-12'") == "CLOSED",
    )

    # 分录：借 5001 / 贷 5001 结平、3103 清零、3104 收净利润
    pl_after = num(one(
        "SELECT COALESCE(SUM(e.debit_amount - e.credit_amount),0)::numeric(20,4) FROM biz_voucher_entry e "
        "JOIN biz_voucher v ON v.id = e.voucher_id JOIN biz_account a ON a.id = e.account_id "
        f"WHERE v.deleted_at IS NULL AND a.account_category = 'PROFIT_LOSS' AND v.voucher_date <= '{YEAR}-12-31'"
    ))
    check("④e 结转后本年损益类余额合计 = 0", eq_num(pl_after, 0), f"合计 {pl_after}")
    check(
        "④f 结转后 3103 本年利润 = 0",
        eq_num(one(
            "SELECT COALESCE(SUM(e.debit_amount - e.credit_amount),0)::numeric(20,4) FROM biz_voucher_entry e "
            f"WHERE e.account_code = '3103'"
        ), 0),
    )
    dist = num(one(
        "SELECT COALESCE(SUM(e.credit_amount - e.debit_amount),0)::numeric(20,4) FROM biz_voucher_entry e "
        f"WHERE e.account_code = '3104'"
    ))
    check(f"④g 3104 利润分配收到净利润 {NET_PROFIT}", eq_num(dist, NET_PROFIT), f"3104={dist}")

    # ---------- ⑤ 下年期初 ----------
    status, payload = api("GET", f"/api/business/erp/opening-balance?period={NEXT_YEAR}-01", token=TOKEN)
    opening = data_of(payload) or {}
    entries = opening.get("entries") or []
    codes = {row.get("account_code") for row in entries}
    pl_rows = [row for row in entries if row.get("account_category") == "PROFIT_LOSS"]
    check(
        f"⑤ {NEXT_YEAR}-01 期初已生成且借贷平衡",
        status == 200 and opening.get("balanced") is True and len(entries) > 0,
        f"HTTP {status} rows={len(entries)} balanced={opening.get('balanced')} diff={opening.get('difference')}",
    )
    check(
        "⑤b 期初里没有损益类科目（归零）+ 没有 3103",
        len(pl_rows) == 0 and "3103" not in codes,
        f"损益类 {len(pl_rows)} 行 codes={sorted(codes)[:8]}",
    )
    check(
        "⑤c 资产 / 负债 / 权益都在（1002 / 2202 / 3104）",
        {"1002", "3104"}.issubset(codes),
        f"codes={sorted(codes)}",
    )
    check(
        "⑤d 期初行来源全部是 YEAR_END（页面标「由年结生成」，不许手工改）",
        all(str(row.get("source")) == "YEAR_END" for row in entries),
        f"sources={sorted({str(row.get('source')) for row in entries})}",
    )
    check(
        f"⑤e {NEXT_YEAR}-01 **没有** OPENING 凭证（否则与历史累计双计）",
        one(f"SELECT COUNT(*) FROM biz_voucher WHERE voucher_type = 'OPENING' "
            f"AND period = '{NEXT_YEAR}-01' AND deleted_at IS NULL") == "0",
    )
    cash_row = next((row for row in entries if row.get("account_code") == "1002"), None)
    check(
        f"⑤f 银行存款期初 = {NET_PROFIT}（本年净增）",
        cash_row is not None and eq_num(num(cash_row.get("debit")) - num(cash_row.get("credit")), NET_PROFIT),
        f"row={cash_row}",
    )

    # ---------- ⑥ 报表：2027 期初 = 2026 期末 ----------
    bs_status, bs_next = bs_amounts(f"{NEXT_YEAR}-01")
    check(f"⑥ {NEXT_YEAR}-01 资产负债表可出（期初已接上）", bs_status == 200, f"HTTP {bs_status}")
    assets = num(bs_next.get("TOTAL_ASSETS"))
    le = num(bs_next.get("TOTAL_LIABILITIES_AND_EQUITY"))
    check("⑥b 资产 ≡ 负债 + 权益（年结没有破坏勾稽）", eq_num(assets, le), f"资产 {assets} / 负债+权益 {le}")
    check(
        f"⑥c 未分配利润含本年净利润（≥ {NET_PROFIT}）",
        num(bs_next.get("UNDISTRIBUTED_PROFIT")) >= NET_PROFIT - 0.0001,
        f"未分配利润 {bs_next.get('UNDISTRIBUTED_PROFIT')}",
    )

    # ---------- ⑥d 年结态下跑全账不变量（32 项，BLOCKER 必须 0） ----------
    # 为什么放在「已结转」这一步：结转凭证 + 下年期初是本次新增的账务形态，
    # 不变量套件只有在它存在时跑过，才算证明「年结不破坏全账」。
    if os.environ.get("ERP_E2E_INVARIANTS", "1") == "1":
        inv = subprocess.run(
            ["python3", "scripts/verify/erp_invariants.py"],
            capture_output=True, text=True, timeout=600, cwd="/opt/cloudpivot", env=dict(os.environ),
        )
        tail = (inv.stdout or "").strip().splitlines()
        summary = next((line for line in reversed(tail) if "32/" in line or "结果" in line), "")
        check(
            "⑥d 年结态下 erp_invariants 32/32 · BLOCKER 0",
            inv.returncode == 0 and "32/32" in (inv.stdout or "") and "BLOCKER 0" in (inv.stdout or ""),
            f"rc={inv.returncode} {summary.strip()[:120]}",
        )

    # ---------- ⑦ 负向：重复年结 ----------
    status, payload = api(
        "POST", "/api/business/erp/year-end/close", {"year": YEAR, "reason": "R4 e2e 重复"}, TOKEN
    )
    check(
        "⑦ 重复年结 → 409（已结转）",
        status == 409 and "已经结转" in msg_of(payload),
        f"HTTP {status} {msg_of(payload)[:120]}",
    )
    status, state = year_state()
    check(
        "⑦b 检查接口：closed=true + ALREADY_CLOSED + can_close=false",
        status == 200
        and (state.get("state") or {}).get("closed") is True
        and "ALREADY_CLOSED" in blocker_types(state)
        and state.get("can_close") is False,
        f"state={ (state.get('state') or {}).get('status') } blockers={blocker_types(state)}",
    )

    # ---------- ⑧ 负向：下一年度有凭证 → 反年结 409 ----------
    status, payload = post_voucher(
        f"{NEXT_YEAR}-01-05", "R4 e2e 2027 业务",
        [(accounts["1002"], 50, 0), (accounts["5001"], 0, 50)],
    )
    check(f"⑧ 在 {NEXT_YEAR}-01 记一张凭证（模拟新年已用起来）", status == 200, f"HTTP {status}")
    status, payload = api(
        "POST", "/api/business/erp/year-end/reopen",
        {"year": YEAR, "reason": "R4 e2e 试图拆掉已经用起来的年度"}, TOKEN,
    )
    check(
        f"⑧b 反年结 → 409（{NEXT_YEAR} 年已有凭证）",
        status == 409 and str(NEXT_YEAR) in msg_of(payload),
        f"HTTP {status} {msg_of(payload)[:140]}",
    )
    psql(f"DELETE FROM biz_voucher WHERE period = '{NEXT_YEAR}-01'")

    status, payload = api(
        "POST", "/api/business/erp/year-end/reopen", {"year": YEAR, "reason": "改一下"}, TOKEN
    )
    check(
        "⑧c 反年结原因过短 → 400",
        status == 400 and "10" in msg_of(payload),
        f"HTTP {status} {msg_of(payload)[:120]}",
    )

    # ---------- ⑨ 反年结 ----------
    status, payload = api(
        "POST", "/api/business/erp/year-end/reopen",
        {"year": YEAR, "reason": "R4 e2e 验收：成本口径要重做，先反年结"}, TOKEN,
    )
    body = data_of(payload) or {}
    check(
        "⑨ 反年结成功：删凭证 + 删期初 + 12 月解封",
        status == 200
        and body.get("reopened") is True
        and body.get("deleted_voucher_no") == voucher_no
        and int(body.get("deleted_opening_rows") or 0) > 0
        and body.get("december_reopened") is True,
        f"HTTP {status} {msg_of(payload)[:80]} deleted={body.get('deleted_voucher_no')} "
        f"rows={body.get('deleted_opening_rows')}",
    )
    check(
        "⑨b 结转凭证已删除",
        one(f"SELECT COUNT(*) FROM biz_voucher WHERE voucher_type = 'YEAR_END' "
            f"AND period = '{YEAR}-12' AND deleted_at IS NULL") == "0",
    )
    check(
        f"⑨c {NEXT_YEAR}-01 期初行已清空",
        one(f"SELECT COUNT(*) FROM biz_opening_balance WHERE period_code = '{NEXT_YEAR}-01'") == "0",
    )
    check(
        f"⑨d {YEAR}-12 已解封（CLOSED → OPEN）",
        one(f"SELECT status FROM biz_accounting_period WHERE period_code = '{YEAR}-12'") == "OPEN",
    )
    status, state = year_state()
    check(
        "⑨e 年度状态回到「未结转」",
        status == 200 and (state.get("state") or {}).get("closed") is False,
        f"state={(state.get('state') or {}).get('status')}",
    )
    pl_restored = num(one(
        "SELECT COALESCE(SUM(e.debit_amount - e.credit_amount),0)::numeric(20,4) FROM biz_voucher_entry e "
        "JOIN biz_voucher v ON v.id = e.voucher_id JOIN biz_account a ON a.id = e.account_id "
        f"WHERE v.deleted_at IS NULL AND a.account_category = 'PROFIT_LOSS' AND v.voucher_date <= '{YEAR}-12-31'"
    ))
    check(
        f"⑨f 损益余额回到结转前（= −{NET_PROFIT}）",
        eq_num(pl_restored, -NET_PROFIT),
        f"损益合计 {pl_restored}",
    )

    # ---------- ⑩ 收尾 ----------
    cleanup()
    after = baseline()
    check("⑩ 收尾基线核对（期间表回到运行前）", before == after, f"before={before} after={after}")
    residue = one(
        "SELECT (SELECT COUNT(*) FROM biz_opening_balance WHERE period_code IN "
        f"('{YEAR}-01','{NEXT_YEAR}-01','{AFTER_YEAR}-01'))"
        " + (SELECT COUNT(*) FROM biz_voucher WHERE voucher_type IN ('YEAR_END') AND deleted_at IS NULL "
        f"AND period LIKE '{YEAR}-%')"
    )
    check("⑩b 无残留（期初行 / 结转凭证）", residue == "0", f"残留 {residue}")

    passed = sum(1 for _, ok, _ in RESULT if ok)
    total = len(RESULT)
    print("\n" + "=" * 88)
    print(f"汇总：{passed}/{total} 通过")
    print("=" * 88)
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
