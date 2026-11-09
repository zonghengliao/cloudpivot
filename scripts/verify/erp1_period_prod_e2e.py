#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
ERP-P1 会计期间开账 / 结账 / 反结账 —— **生产（144）真机验收**。

跑法（在 144 上）：

```bash
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
python3 scripts/verify/erp1_period_prod_e2e.py
```

设计原则（与前几张 ERP 卡的 e2e 同口径）：

1. **只动自己开的期间**：本脚本用 `2027-01`（单月）与 `2027-02 ~ 2027-03`（批量），
   生产原来只有 `2026-10`，收尾时把这些期间连同凭证 / 状态日志一起删干净，
   并**核对基线**（`biz_accounting_period` 只剩 `2026-10` 且是 OPEN）。
2. **草稿凭证用 psql 造**：手工凭证接口一落库就是 `POSTED`（会计口径如此），
   要验「有草稿凭证不能结账」只能直接插一行 `status='DRAFT'` 的凭证当夹具，
   验完立刻按 id 删掉。
3. **每条断言都贴原文**：HTTP 码 + 业务码 + 文案片段，失败时人能直接看出是哪条。
4. **403 用「无角色用户」验**：新建一个没有任何角色的账号，
   它的权限快照为空 → 开账接口必须 403（证明权限码真的在守门，不是只有注解）。
"""

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

BASE = os.environ.get("ERP_E2E_BASE", "http://127.0.0.1:8023")
ADMIN_USER = os.environ.get("ERP_E2E_ADMIN", "admin")
# 生产口令只从环境取（`set -a; . /etc/cloudpivot/cloudpivot.env; set +a`），不写进仓库
ADMIN_PASS = (
    os.environ.get("NIK_ADMIN_PASSWORD")
    or os.environ.get("ALUN_ADMIN_PASSWORD")
    or os.environ.get("ERP_E2E_PASSWORD")
    or ""
)

# 本脚本独占的期间（生产原本没有这些期间）
PERIOD_MAIN = "2027-01"
BATCH_FROM = "2027-02"
BATCH_TO = "2027-03"
ALL_PERIODS = ["2027-01", "2027-02", "2027-03"]
TEST_USER = "p1e2e_noperm"

RESULT = []
TOKEN = None


# ──────────────────────────── 断言与 HTTP ────────────────────────────


def check(name, ok, detail=""):
    RESULT.append((name, bool(ok), detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return bool(ok)


def api(method, path, body=None, token=None, expect=None):
    """调接口，返回 (status, payload)。4xx/5xx 不抛异常，交给断言判。"""
    url = f"{BASE}{path}"
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode("utf-8")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read().decode("utf-8")
            return response.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as error:
        raw = error.read().decode("utf-8")
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw}
    except Exception as error:  # noqa: BLE001 — 网络层失败也要让断言看得见
        return 0, {"error": str(error)}


def msg_of(payload):
    """后端出参：`{code, msg, data}`。"""
    if not isinstance(payload, dict):
        return ""
    return str(payload.get("msg") or payload.get("message") or payload)


def data_of(payload):
    if isinstance(payload, dict):
        return payload.get("data")
    return None


def psql(sql):
    """在 144 上用应用账号跑一条 SQL（只给夹具与基线核对用）。"""
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", "")
    command = [
        "psql",
        "-h",
        os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
        "-p",
        os.environ.get("NIK_DATABASE_PORT", "5432"),
        "-U",
        os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
        "-d",
        os.environ.get("NIK_DATABASE_NAME", "cloudpivot"),
        "-tAF|",
        "-c",
        sql,
    ]
    result = subprocess.run(command, capture_output=True, text=True, env=env, timeout=60)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip()[:400])
    return [line for line in result.stdout.strip().split("\n") if line != ""]


# ──────────────────────────── 夹具 ────────────────────────────


def cleanup():
    """删掉本脚本造的一切（期间 / 凭证 / 状态日志 / 无权限测试用户）。"""
    periods = ", ".join(f"'{code}'" for code in ALL_PERIODS)
    psql(f"DELETE FROM biz_voucher WHERE period IN ({periods})")
    psql(
        "DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' "
        f"AND entity_no IN ({periods})"
    )
    psql(f"DELETE FROM biz_accounting_period WHERE period_code IN ({periods})")
    psql(
        "DELETE FROM sys_user_role WHERE user_id IN "
        f"(SELECT id FROM sys_user WHERE username = '{TEST_USER}')"
    )
    psql(f"DELETE FROM sys_user WHERE username = '{TEST_USER}'")


def baseline():
    """生产基线：期间行清单（收尾要对回原样）。"""
    return psql("SELECT period_code || ':' || status FROM biz_accounting_period ORDER BY period_code")


# ──────────────────────────── 主流程 ────────────────────────────


def login(username, password):
    status, payload = api("POST", "/api/system/auth/login", {"username": username, "password": password})
    body = data_of(payload) or {}
    return status, body.get("access_token"), body


def main():
    global TOKEN

    print("=" * 88)
    print("ERP-P1 会计期间 开账 / 结账 / 反结账 —— 生产真机验收")
    print("=" * 88)

    if not ADMIN_PASS:
        print("缺管理员口令：先 `set -a; . /etc/cloudpivot/cloudpivot.env; set +a`（脚本只认 NIK_ADMIN_PASSWORD）")
        sys.exit(2)

    before = baseline()
    print(f"基线期间行：{before}")
    cleanup()  # 上一轮残留先清（幂等）

    # ── ⓪ 登录 ──────────────────────────────────────────────
    status, token, body = login(ADMIN_USER, ADMIN_PASS)
    if status != 200 or not token:
        check("⓪ admin 登录", False, f"HTTP {status} / {msg_of(body)}")
        summary()
        return
    TOKEN = token
    check("⓪ admin 登录", True, "拿到 access_token")

    # 找两个末级科目（手工凭证要真科目 id）
    status, payload = api("GET", "/api/business/erp/account/page?page_size=200&is_leaf=true", token=TOKEN)
    accounts = data_of(payload) or {}
    rows = accounts.get("list") or []
    debit_account = next((row for row in rows if str(row.get("account_code", "")).startswith("1001")), None)
    credit_account = next((row for row in rows if str(row.get("account_code", "")).startswith("3001")), None)
    if not debit_account or not credit_account:
        check("⓪ 取两个末级科目", False, f"科目数 {len(rows)}，找不到 1001/3001")
        summary()
        return

    def post_voucher(summary_text="P1E2E 期间测试凭证"):
        return api(
            "POST",
            "/api/business/erp/voucher",
            {
                "voucher_date": f"{PERIOD_MAIN}-10",
                "summary": summary_text,
                "entries": [
                    {"account_id": debit_account["id"], "debit": "10.00", "summary": "P1E2E 借"},
                    {"account_id": credit_account["id"], "credit": "10.00", "summary": "P1E2E 贷"},
                ],
            },
            token=TOKEN,
        )

    # ── ① 开账 2027-01 ─────────────────────────────────────
    status, payload = api(
        "POST",
        "/api/business/erp/period",
        {"period_year": 2027, "period_month": 1, "remark": "ERP-P1 真机验收"},
        token=TOKEN,
    )
    created = data_of(payload) or {}
    check(
        "① 开账 2027-01 → 200 / 起止日正确 / 状态 OPEN",
        status == 200
        and created.get("period_code") == PERIOD_MAIN
        and created.get("start_date") == "2027-01-01"
        and created.get("end_date") == "2027-01-31"
        and created.get("status") == "OPEN",
        f"HTTP {status} / {msg_of(payload) if status != 200 else json.dumps(created, ensure_ascii=False)}",
    )
    period_id = created.get("id")

    # 期间列表里能看到它
    status, payload = api("GET", "/api/business/erp/period/page?period_year=2027&page_size=50", token=TOKEN)
    codes = [row.get("period_code") for row in (data_of(payload) or {}).get("list", [])]
    check("① 期间列表出现 2027-01", status == 200 and PERIOD_MAIN in codes, f"HTTP {status} / 列表 {codes}")

    # 开账必须留一条 NULL→OPEN 的状态日志（审计：谁在什么时候开的账）
    created_trail = psql(
        "SELECT coalesce(from_status, 'NULL') || '>' || to_status FROM biz_status_log "
        f"WHERE entity_type = 'ACCOUNTING_PERIOD' AND entity_no = '{PERIOD_MAIN}' ORDER BY created_at, id"
    )
    check("① 开账写一条 NULL>OPEN 状态日志", created_trail == ["NULL>OPEN"], f"轨迹 {created_trail}")

    # ── ② 在 11 月（这里是 2027-01）建一张凭证 → 能建 ────────
    status, payload = post_voucher()
    voucher = data_of(payload) or {}
    check(
        "② 期间内建手工凭证 → 200 / 期间 2027-01 / POSTED",
        status == 200 and voucher.get("period") == PERIOD_MAIN and voucher.get("status") == "POSTED",
        f"HTTP {status} / {msg_of(payload) if status != 200 else voucher.get('voucher_no')}",
    )

    # ── ③ 有草稿凭证 → 结账拦住 + 说清几张 ─────────────────
    draft_id = "01a1f150-0000-7000-8000-00000000e2e1"
    psql(
        "INSERT INTO biz_voucher (id, voucher_no, period, voucher_date, total_debit, total_credit, "
        "entry_count, status, summary, voucher_type, source_type) VALUES "
        f"('{draft_id}'::uuid, 'P1E2E-DRAFT', '{PERIOD_MAIN}', '{PERIOD_MAIN}-12', 5, 5, 2, 'DRAFT', "
        "'P1E2E 草稿夹具', 'MANUAL', 'MANUAL') ON CONFLICT (id) DO NOTHING"
    )
    psql(
        "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, "
        "account_name, debit_amount, credit_amount) VALUES "
        f"('{draft_id}'::uuid, 1, 'P1E2E 草稿', gen_random_uuid(), '1001', '库存现金', 5, 0) "
        "ON CONFLICT (voucher_id, line_no) DO NOTHING"
    )
    psql(
        "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, "
        "account_name, debit_amount, credit_amount) VALUES "
        f"('{draft_id}'::uuid, 2, 'P1E2E 草稿', gen_random_uuid(), '3001', '实收资本', 0, 5) "
        "ON CONFLICT (voucher_id, line_no) DO NOTHING"
    )

    status, payload = api("GET", f"/api/business/erp/period/{period_id}/close-check", token=TOKEN)
    check_data = data_of(payload) or {}
    blockers = check_data.get("blockers") or []
    draft_blocker = next((item for item in blockers if item.get("type") == "DRAFT_VOUCHER"), None)
    check(
        "③ 预检发现 1 张草稿凭证（close-check）",
        status == 200
        and check_data.get("can_close") is False
        and draft_blocker is not None
        and draft_blocker.get("count") == 1
        and "还有 1 张草稿凭证未过账" in (draft_blocker.get("message") or ""),
        f"HTTP {status} / blockers={json.dumps(blockers, ensure_ascii=False)[:220]}",
    )

    status, payload = api("POST", f"/api/business/erp/period/{period_id}/close", {"reason": "P1E2E 结账"}, token=TOKEN)
    check(
        "③ 有草稿凭证 → 结账 409 + 文案点名张数",
        status == 409 and "还有 1 张草稿凭证未过账" in msg_of(payload),
        f"HTTP {status} / {msg_of(payload)[:200]}",
    )
    still_open = (psql(f"SELECT status FROM biz_accounting_period WHERE period_code = '{PERIOD_MAIN}'") or [""])[0]
    check("③ 结账失败后期间仍是 OPEN", still_open == "OPEN", f"status={still_open}")

    # ── ④ 清掉草稿 → 再结账 → 成功 ───────────────────────────
    psql(f"DELETE FROM biz_voucher WHERE id = '{draft_id}'::uuid")
    status, payload = api("GET", f"/api/business/erp/period/{period_id}/close-check", token=TOKEN)
    check_data = data_of(payload) or {}
    check(
        "④ 草稿清掉后预检全绿（can_close=true）",
        status == 200 and check_data.get("can_close") is True and not (check_data.get("blockers") or []),
        f"HTTP {status} / blockers={json.dumps(check_data.get('blockers'), ensure_ascii=False)[:160]}",
    )

    status, payload = api("POST", f"/api/business/erp/period/{period_id}/close", {"reason": "2027 年 1 月结账"}, token=TOKEN)
    closed = data_of(payload) or {}
    check(
        "④ 结账 → 200 / CLOSED / closed_at 落库",
        status == 200 and closed.get("status") == "CLOSED" and bool(closed.get("closed_at")),
        f"HTTP {status} / {msg_of(payload) if status != 200 else closed.get('closed_at')}",
    )

    # ── ⑤ 结账后建凭证 → 409 + 10003「期间已结账」 ───────────────────
    # E2E04-FIX-3 ①：手工凭证入口曾返 400/10001（参数错误），与期初录入 / 红冲的
    # 409/10003 不一致；「期间已封账」是状态冲突，前端该引导去反结账而不是高亮字段。
    status, payload = post_voucher("P1E2E 结账后凭证")
    check(
        "⑤ 结账后建凭证 → 409 + `10003` + 「已结账」",
        status == 409 and payload.get("code") == 10003 and "已结账" in msg_of(payload),
        f"HTTP {status} / code={payload.get('code')} / {msg_of(payload)[:200]}",
    )

    # ── ⑥ 反结账：先拦「原因不足」，再成功 ───────────────────
    status, payload = api("POST", f"/api/business/erp/period/{period_id}/reopen", {"reason": "太短"}, token=TOKEN)
    check(
        "⑥ 反结账原因太短 → 400",
        status == 400 and "至少 10 个字" in msg_of(payload),
        f"HTTP {status} / {msg_of(payload)[:160]}",
    )
    status, payload = api("POST", f"/api/business/erp/period/{period_id}/reopen", {}, token=TOKEN)
    check(
        "⑥ 反结账不给原因 → 400",
        status == 400 and "必须填写原因" in msg_of(payload),
        f"HTTP {status} / {msg_of(payload)[:160]}",
    )

    reason = "2027 年 1 月漏记一笔运费，需要补一张凭证"
    status, payload = api("POST", f"/api/business/erp/period/{period_id}/reopen", {"reason": reason}, token=TOKEN)
    reopened = data_of(payload) or {}
    check(
        "⑥ 反结账 → 200 / OPEN / closed_at 清空",
        status == 200 and reopened.get("status") == "OPEN" and reopened.get("closed_at") is None,
        f"HTTP {status} / {msg_of(payload) if status != 200 else json.dumps(reopened, ensure_ascii=False)[:160]}",
    )
    trail = psql(
        "SELECT coalesce(from_status, 'NULL') || '>' || to_status FROM biz_status_log "
        f"WHERE entity_type = 'ACCOUNTING_PERIOD' AND entity_no = '{PERIOD_MAIN}' ORDER BY created_at, id"
    )
    check(
        "⑥ 状态日志三条（NULL>OPEN / OPEN>CLOSED / CLOSED>OPEN）",
        len(trail) == 3 and trail[1] == "OPEN>CLOSED" and trail[2] == "CLOSED>OPEN",
        f"轨迹 {trail}",
    )
    reason_logged = psql(
        "SELECT coalesce(reason, '') FROM biz_status_log "
        f"WHERE entity_type = 'ACCOUNTING_PERIOD' AND entity_no = '{PERIOD_MAIN}' "
        "AND to_status = 'OPEN' ORDER BY created_at DESC LIMIT 1"
    )
    check("⑥ 反结账原因进了状态日志", bool(reason_logged) and reason in reason_logged[0], f"reason={reason_logged}")

    # ── ⑦ 反结账后能继续建凭证 ──────────────────────────────
    status, payload = post_voucher("P1E2E 反结账后凭证")
    voucher2 = data_of(payload) or {}
    check(
        "⑦ 反结账后建凭证 → 200 / 期间 2027-01",
        status == 200 and voucher2.get("period") == PERIOD_MAIN,
        f"HTTP {status} / {msg_of(payload) if status != 200 else voucher2.get('voucher_no')}",
    )

    # ── ⑧ 重复开账 → 409 ────────────────────────────────────
    status, payload = api(
        "POST", "/api/business/erp/period", {"period_year": 2027, "period_month": 1}, token=TOKEN
    )
    check("⑧ 重复开账同一期间 → 409", status == 409 and "已经开账" in msg_of(payload), f"HTTP {status} / {msg_of(payload)[:120]}")

    # ── ⑨ 批量开账（幂等）──────────────────────────────────
    status, payload = api(
        "POST",
        "/api/business/erp/period/batch-open",
        {"from": BATCH_FROM, "to": BATCH_TO},
        token=TOKEN,
    )
    batch = data_of(payload) or {}
    check(
        "⑨ 批量开账 2027-02 ~ 2027-03 → 新建 2 个",
        status == 200 and batch.get("created_count") == 2,
        f"HTTP {status} / created={batch.get('created_count')} skipped={batch.get('skipped_count')}",
    )
    status, payload = api(
        "POST",
        "/api/business/erp/period/batch-open",
        {"from": BATCH_FROM, "to": BATCH_TO},
        token=TOKEN,
    )
    again = data_of(payload) or {}
    check(
        "⑨ 批量开账重跑幂等 → 新建 0 / 跳过 2",
        status == 200 and again.get("created_count") == 0 and again.get("skipped_count") == 2,
        f"HTTP {status} / created={again.get('created_count')} skipped={again.get('skipped_count')}",
    )

    # ── ⑩ 无权限用户 → 403 ─────────────────────────────────
    psql(
        "DELETE FROM sys_user_role WHERE user_id IN "
        f"(SELECT id FROM sys_user WHERE username = '{TEST_USER}')"
    )
    psql(f"DELETE FROM sys_user WHERE username = '{TEST_USER}'")
    status, payload = api(
        "POST",
        "/api/system/user",
        {"username": TEST_USER, "password": "P1e2e@123456", "real_name": "P1 无权限验收", "role_ids": []},
        token=TOKEN,
    )
    created_user = status in (200, 201)
    status_login, token2, body2 = login(TEST_USER, "P1e2e@123456")
    if not token2:
        check("⑩ 无权限用户登录", False, f"HTTP {status_login} / {msg_of(body2)}")
    else:
        status, payload = api(
            "POST",
            "/api/business/erp/period",
            {"period_year": 2027, "period_month": 4},
            token=token2,
        )
        check(
            "⑩ 无开账权限 → 403（权限码真在守门）",
            status == 403,
            f"HTTP {status} / {msg_of(payload)[:160]}",
        )
    check("⑩ 无权限用户已建（无角色）", created_user, f"HTTP {status}")

    # ── ⑪ 清理 + 基线核对 ──────────────────────────────────
    cleanup()
    after = baseline()
    check("⑪ 收尾：期间行回到基线（只剩 2026-10:OPEN）", after == before, f"after={after} / before={before}")
    leftover = psql(f"SELECT count(*) FROM biz_voucher WHERE period IN ('2027-01','2027-02','2027-03')")
    check("⑪ 收尾：没有残留凭证", leftover and leftover[0] == "0", f"残留 {leftover}")
    leftover_user = psql(f"SELECT count(*) FROM sys_user WHERE username = '{TEST_USER}'")
    check("⑪ 收尾：没有残留测试用户", leftover_user and leftover_user[0] == "0", f"残留 {leftover_user}")

    summary()


def summary():
    print("\n" + "=" * 88)
    total = len(RESULT)
    passed = sum(1 for _, ok, _ in RESULT if ok)
    failed = [name for name, ok, _ in RESULT if not ok]
    print(f"合计 {total} 项：PASS {passed} / FAIL {len(failed)}")
    if failed:
        print("失败项：")
        for name in failed:
            print(f"  · {name}")
    print("=" * 88)
    sys.exit(0 if not failed else 1)


if __name__ == "__main__":
    main()
