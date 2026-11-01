#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
ERP-R5 期初余额录入 —— **隔离实例真机验收**（API 层，走真实 HTTP + 真实库）。

跑法（在 99 上，库与实例都是隔离的）：

```bash
export ERP_E2E_BASE=http://127.0.0.1:18098
export ERP_E2E_PASSWORD='Admin@123456'
export NIK_DATABASE_NAME=cloudpivot_r5 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun
python3 scripts/verify/r5_opening_e2e.py
```

**绝不对生产跑**：本脚本会开期间、造期初、动库存三本账。`ERP_E2E_BASE` 默认指向
隔离端口 18098，生产是 8023 —— 端口不同就是硬闸。

设计原则（与 ERP-P1 的 e2e 同口径）：

1. **只动自己造的数据**：期间 `2027-01`、物料 `R5-E2E-*`、仓库 `R5-E2E-WH`；
   收尾按前缀 / 期间编码删干净，并核对基线（期间表回到空）。
2. **每条断言贴原文**：HTTP 码 + 业务码 + 文案片段。
3. **金额按数值比**：`NUMERIC(20,4)` 经 JSON 回来是 `"50000.0000"`，逐字比字符串必假红。
4. **两条硬红线单独验**：期初只有一张凭证（库层唯一索引）+ 期初不进本期发生额（利润表）。
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
# 首登强制改密后用的新口令（隔离库里 admin 的初始口令一次性，改完就用它）
NEW_PASS = os.environ.get("ERP_E2E_NEW_PASSWORD") or "R5@Admin2026"

PERIOD = "2027-01"
MATERIAL_CODE = "R5-E2E-001"
WAREHOUSE_CODE = "R5-E2E-WH"

# SQL 里要用的期间字面量（在 f-string 表达式里再套引号会踩「同引号嵌套」的限制）
PERIOD_SQL = f"'{PERIOD}'"
MATERIAL_SQL = f"'{MATERIAL_CODE}'"

RESULT = []
TOKEN = None


# ──────────────────────────── 断言与 HTTP ────────────────────────────

def check(name, ok, detail=""):
    RESULT.append((name, bool(ok), detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return bool(ok)


def num(value):
    """金额/数量：字符串或数字都转 float 比（NULL 当 0）。"""
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
    # psql 只认 PGPASSWORD（不认 NIK_DATABASE_PASSWORD）；生产 env 里两者都有值时也能跑
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", env.get("PGPASSWORD", ""))
    command = [
        "psql",
        "-h", os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
        "-p", os.environ.get("NIK_DATABASE_PORT", "5432"),
        "-U", os.environ.get("NIK_DATABASE_USER", "alun"),
        "-d", os.environ.get("NIK_DATABASE_NAME", "cloudpivot_r5"),
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

def fixtures():
    """造一个物料 + 一个仓库（前缀 R5-E2E-，收尾删干净）。"""
    existing = psql(f"SELECT id FROM biz_material WHERE material_code = '{MATERIAL_CODE}'")
    if existing:
        material_id = existing[0]
    else:
        unit_id = one("SELECT id FROM biz_unit LIMIT 1")
        material_id = one(
            "INSERT INTO biz_material (material_code, material_name, unit_id, material_type, status) "
            f"VALUES ('{MATERIAL_CODE}', 'R5 e2e 物料', '{unit_id}'::uuid, 'RAW', 1) RETURNING id"
        )
    existing_wh = psql(f"SELECT id FROM biz_warehouse WHERE warehouse_code = '{WAREHOUSE_CODE}'")
    if existing_wh:
        warehouse_id = existing_wh[0]
    else:
        # warehouse_path 是 NOT NULL 且形如 /uuid/：先插占位串，再按真实 id 回填
        warehouse_id = one(
            "INSERT INTO biz_warehouse (warehouse_code, warehouse_name, warehouse_type, status, "
            "warehouse_path) VALUES "
            f"('{WAREHOUSE_CODE}', 'R5 e2e 仓', 'NORMAL', 1, '/pending/') RETURNING id"
        )
        psql(f"UPDATE biz_warehouse SET warehouse_path = '/{warehouse_id}/' WHERE id = '{warehouse_id}'::uuid")
    return material_id, warehouse_id


def cleanup():
    """删掉本脚本造的一切（顺序：分录随凭证 CASCADE → 流水 → 库位账 → 成本账 → 台账 → 主数据）。"""
    psql(f"DELETE FROM biz_voucher WHERE period = '{PERIOD}'")
    psql(f"DELETE FROM biz_opening_balance WHERE period_code = '{PERIOD}'")
    psql(f"DELETE FROM biz_opening_inventory WHERE period_code = '{PERIOD}'")
    psql(
        "DELETE FROM biz_inventory_txn WHERE material_id IN "
        f"(SELECT id FROM biz_material WHERE material_code = '{MATERIAL_CODE}')"
    )
    psql(
        "DELETE FROM biz_inventory_location WHERE material_id IN "
        f"(SELECT id FROM biz_material WHERE material_code = '{MATERIAL_CODE}')"
    )
    psql(
        "DELETE FROM biz_inventory WHERE material_id IN "
        f"(SELECT id FROM biz_material WHERE material_code = '{MATERIAL_CODE}')"
    )
    psql(f"DELETE FROM biz_material WHERE material_code = '{MATERIAL_CODE}'")
    psql(f"DELETE FROM biz_warehouse WHERE warehouse_code = '{WAREHOUSE_CODE}'")
    psql(
        "DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' "
        f"AND entity_no = '{PERIOD}'"
    )
    psql(f"DELETE FROM biz_accounting_period WHERE period_code = '{PERIOD}'")


def baseline():
    return psql("SELECT period_code || ':' || status FROM biz_accounting_period ORDER BY period_code")


# ──────────────────────────── 主流程 ────────────────────────────

def bs_amounts(period):
    """资产负债表的 {item_code: 金额}（接口不可用时返回空字典）。"""
    status, payload = api("GET", f"/api/business/erp/report/balance-sheet?period={period}", token=TOKEN)
    body = data_of(payload) or {}
    amounts = {}
    for line in (body.get("assets") or []) + (body.get("liabilities") or []) + (body.get("equity") or []):
        amounts[line.get("item_code")] = line.get("amount")
    return status, amounts


def login():
    """登录（自动处理「首次登录必须改密」：改密 → 用新口令重登）。

    后端对环境口令种下的 admin 会打 `must_change_password`，此时除改密接口外一律 403
    「首次登录必须先修改初始密码」。隔离库里第一次跑必然遇到，所以脚本自己把它走完，
    并把新口令记在 `ADMIN_PASS` 里（重跑时先试新口令，再退回环境口令）。
    """
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
        # 首登强制改密：带旧 token 改密 → 旧 token 因 token_version++ 失效 → 重登
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


def main():
    global TOKEN

    print("=" * 88)
    print(f"ERP-R5 期初余额 —— 隔离实例真机验收（{BASE}）")
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
    material_id, warehouse_id = fixtures()
    before = baseline()

    # ---------- ① 开账 ----------
    status, payload = api(
        "POST", "/api/business/erp/period",
        {"period_year": 2027, "period_month": 1, "remark": "R5 e2e 夹具"}, TOKEN,
    )
    check("① 开 2027-01 期间", status == 200, f"HTTP {status} {msg_of(payload)}")

    # 报表基线（**增量口径**：生产库里本来就有历史数据，绝对值不可断言）
    bs_status, bs_base = bs_amounts(PERIOD)
    check("①b 资产负债表基线可取（增量断言的基准）", bs_status == 200, f"HTTP {bs_status}")

    # ---------- ② 未录状态 ----------
    status, payload = api("GET", f"/api/business/erp/opening-balance?period={PERIOD}", token=TOKEN)
    body = data_of(payload) or {}
    check(
        "② GET 未录状态（state=NONE / 无凭证）",
        status == 200 and body.get("state") == "NONE" and body.get("voucher") is None,
        f"HTTP {status} state={body.get('state')} voucher={body.get('voucher')}",
    )

    # ---------- ③ 借贷不平 → 400 + 差额 ----------
    status, payload = api(
        "POST", "/api/business/erp/opening-balance",
        {"period": PERIOD, "entries": [
            {"account_code": "1002", "debit": 50000, "credit": 0},
            {"account_code": "3001", "debit": 0, "credit": 40000},
        ]}, TOKEN,
    )
    msg = msg_of(payload)
    check(
        "③ 借贷不平 → 400 + 66012 + 差额 10000 + 点名科目",
        status == 400 and code_of(payload) == 66012 and "10000" in msg and "1002" in msg and "3001" in msg,
        f"HTTP {status} code={code_of(payload)} msg={msg[:120]}",
    )
    check(
        "③b 被拒的期初一行都没落库",
        one(f"SELECT COUNT(*) FROM biz_opening_balance WHERE period_code = '{PERIOD}'") == "0",
    )

    # ---------- ④ 平衡 → 保存成功 + 期初凭证 ----------
    status, payload = api(
        "POST", "/api/business/erp/opening-balance",
        {"period": PERIOD, "entries": [
            {"account_code": "1002", "debit": 50000, "credit": 0},
            {"account_code": "3001", "debit": 0, "credit": 50000},
        ]}, TOKEN,
    )
    body = data_of(payload) or {}
    voucher = body.get("voucher") or {}
    check(
        "④ 借贷平衡 → 200 + state=ENTERED + 生成期初凭证",
        status == 200 and body.get("state") == "ENTERED" and bool(voucher.get("voucher_no")),
        f"HTTP {status} state={body.get('state')} voucher={voucher.get('voucher_no')}",
    )

    # 期初的报表效应按**增量**验（生产库里本来就有数据，绝对值不可断言）
    status, bs_before = bs_amounts(PERIOD)
    check(
        "④c 录期初后资产负债表：货币资金 +50000 / 资产合计 +50000（增量口径）",
        status == 200
        and eq_num(num(bs_before.get("MONETARY_FUNDS")) - num(bs_base.get("MONETARY_FUNDS")), 50000)
        and eq_num(num(bs_before.get("TOTAL_ASSETS")) - num(bs_base.get("TOTAL_ASSETS")), 50000),
        f"货币资金 {bs_base.get('MONETARY_FUNDS')} → {bs_before.get('MONETARY_FUNDS')}；"
        f"资产合计 {bs_base.get('TOTAL_ASSETS')} → {bs_before.get('TOTAL_ASSETS')}",
    )
    v_type = one(f"SELECT voucher_type FROM biz_voucher WHERE period = {PERIOD_SQL}")
    v_date = one(f"SELECT voucher_date::text FROM biz_voucher WHERE period = {PERIOD_SQL}")
    v_debit = one(f"SELECT total_debit FROM biz_voucher WHERE period = {PERIOD_SQL}")
    v_credit = one(f"SELECT total_credit FROM biz_voucher WHERE period = {PERIOD_SQL}")
    check(
        "④b 期初凭证类型 = OPENING、日期 = 期间第一天、借=贷=50000",
        v_type == "OPENING" and v_date == "2027-01-01" and eq_num(v_debit, 50000) and eq_num(v_credit, 50000),
        f"type={v_type} date={v_date} 借={v_debit} 贷={v_credit}",
    )
    first_no = voucher.get("voucher_no")

    # ---------- ⑤ 重复录入 → 覆盖（不新增第二张） ----------
    status, payload = api(
        "POST", "/api/business/erp/opening-balance",
        {"period": PERIOD, "entries": [
            {"account_code": "1002", "debit": 60000, "credit": 0},
            {"account_code": "3001", "debit": 0, "credit": 60000},
        ]}, TOKEN,
    )
    body = data_of(payload) or {}
    again = (body.get("voucher") or {}).get("voucher_no")
    voucher_cnt = one(
        f"SELECT COUNT(*) FROM biz_voucher WHERE period = {PERIOD_SQL} AND voucher_type = 'OPENING'"
    )
    balance_cnt = one(f"SELECT COUNT(*) FROM biz_opening_balance WHERE period_code = {PERIOD_SQL}")
    check(
        "⑤ 重复录入 = 覆盖（凭证号不变、库里仍只有 1 张）",
        status == 200 and again == first_no and voucher_cnt == "1" and balance_cnt == "2",
        f"HTTP {status} no={again}（首次 {first_no}）凭证数={voucher_cnt} 科目行={balance_cnt}",
    )

    # 回到 50000（后面的报表断言以它为准）
    api(
        "POST", "/api/business/erp/opening-balance",
        {"period": PERIOD, "entries": [
            {"account_code": "1002", "debit": 50000, "credit": 0},
            {"account_code": "3001", "debit": 0, "credit": 50000},
        ]}, TOKEN,
    )

    # ---------- ⑥ 库存期初 100 件 × 10 元 ----------
    status, payload = api(
        "POST", "/api/business/erp/opening-balance/inventory",
        {"period": PERIOD, "items": [
            {"material_id": material_id, "warehouse_id": warehouse_id, "qty": 100, "unit_cost": 10},
        ]}, TOKEN,
    )
    body = data_of(payload) or {}
    check(
        "⑥ 库存期初 100×10 → 200 + inventory_amount=1000 + 合并口径 51000",
        status == 200
        and eq_num(body.get("inventory_amount"), 1000)
        and eq_num(body.get("combined_debit_total"), 51000)
        and eq_num(body.get("combined_credit_total"), 51000)
        and body.get("balanced") is True,
        f"HTTP {status} inv={body.get('inventory_amount')} "
        f"合计={body.get('combined_debit_total')}/{body.get('combined_credit_total')} {msg_of(payload)[:80]}",
    )

    # ---------- ⑦ 库存三本账 ----------
    inv_row = one(
        "SELECT qty||'|'||amount||'|'||avg_cost FROM biz_inventory "
        f"WHERE material_id = '{material_id}'::uuid AND warehouse_id = '{warehouse_id}'::uuid",
        default="0|0|0",
    )
    qty, amount, avg = (inv_row.split("|") + ["0", "0", "0"])[:3]
    check(
        "⑦a biz_inventory qty=100 amount=1000 avg_cost=10",
        eq_num(qty, 100) and eq_num(amount, 1000) and eq_num(avg, 10),
        f"qty={qty} amount={amount} avg={avg}",
    )
    loc = one("SELECT COALESCE(SUM(qty),0)::numeric(18,4) FROM biz_inventory_location "
              f"WHERE material_id = '{material_id}'::uuid AND location_id IS NULL")
    check("⑦b biz_inventory_location 同 qty（未分配库位桶）", eq_num(loc, 100), f"qty={loc}")
    txn = psql("SELECT txn_type||'|'||biz_type||'|'||doc_no||'|'||qty FROM biz_inventory_txn "
               f"WHERE material_id = '{material_id}'::uuid")
    check(
        "⑦c biz_inventory_txn 一条 IN / biz_type=OPENING / doc_no=OPENING-2027-01",
        len(txn) == 1 and txn[0].startswith("IN|OPENING|OPENING-2027-01|"),
        f"{txn}",
    )

    # ---------- ⑧ 1405 借方累加 ----------
    debit_1405 = one(
        "SELECT COALESCE(SUM(e.debit_amount),0)::numeric(20,4) FROM biz_voucher_entry e "
        "JOIN biz_voucher v ON v.id = e.voucher_id "
        f"WHERE v.period = '{PERIOD}' AND e.account_code = '1405'"
    )
    credit_3103 = one(
        "SELECT COALESCE(SUM(e.credit_amount),0)::numeric(20,4) FROM biz_voucher_entry e "
        "JOIN biz_voucher v ON v.id = e.voucher_id "
        f"WHERE v.period = '{PERIOD}' AND e.account_code = '3103'"
    )
    entry_count = one(f"SELECT entry_count FROM biz_voucher WHERE period = '{PERIOD}'")
    check(
        "⑧ 期初凭证 借1405=1000 / 贷3103=1000 / 4 条分录",
        eq_num(debit_1405, 1000) and eq_num(credit_3103, 1000) and entry_count == "4",
        f"1405借={debit_1405} 3103贷={credit_3103} 分录={entry_count}",
    )

    # ---------- ⑨ 资产负债表 ----------
    status, amounts = bs_amounts(PERIOD)
    check(
        "⑨ 资产负债表（增量口径）：货币资金 +50000 / 存货 +1000 / 资产合计 +51000",
        status == 200
        and eq_num(num(amounts.get("MONETARY_FUNDS")) - num(bs_base.get("MONETARY_FUNDS")), 50000)
        and eq_num(num(amounts.get("INVENTORY")) - num(bs_base.get("INVENTORY")), 1000)
        and eq_num(num(amounts.get("TOTAL_ASSETS")) - num(bs_base.get("TOTAL_ASSETS")), 51000),
        f"HTTP {status} 货币资金 {bs_base.get('MONETARY_FUNDS')}→{amounts.get('MONETARY_FUNDS')} "
        f"存货 {bs_base.get('INVENTORY')}→{amounts.get('INVENTORY')} "
        f"资产合计 {bs_base.get('TOTAL_ASSETS')}→{amounts.get('TOTAL_ASSETS')}",
    )
    check(
        "⑨b 勾稽：资产 ≡ 负债+权益（期初不破坏平衡）",
        eq_num(amounts.get("TOTAL_ASSETS"), num(amounts.get("TOTAL_LIABILITIES_AND_EQUITY"))),
        f"{amounts.get('TOTAL_ASSETS')} vs {amounts.get('TOTAL_LIABILITIES_AND_EQUITY')}",
    )

    # ---------- ⑩ 利润表看不到期初 ----------
    status, payload = api("GET", f"/api/business/erp/report/income-statement?period={PERIOD}", token=TOKEN)
    body = data_of(payload) or {}
    revenue = next((row for row in (body.get("items") or []) if row.get("item_code") == "REVENUE"), {})
    check(
        "⑩ 利润表营业收入 本期=0 / 本年累计=0（期初不进本期发生额）",
        status == 200 and eq_num(revenue.get("current"), 0) and eq_num(revenue.get("ytd"), 0),
        f"HTTP {status} current={revenue.get('current')} ytd={revenue.get('ytd')}",
    )

    # ---------- ⑪ 结账后只读 ----------
    period_id = one(f"SELECT id FROM biz_accounting_period WHERE period_code = '{PERIOD}'")
    status, payload = api("POST", f"/api/business/erp/period/{period_id}/close", {"reason": "R5 e2e 结账"}, TOKEN)
    check("⑪ 结账 2027-01", status == 200, f"HTTP {status} {msg_of(payload)[:160]}")

    status, payload = api("GET", f"/api/business/erp/opening-balance?period={PERIOD}", token=TOKEN)
    body = data_of(payload) or {}
    check(
        "⑪b GET 变只读（locked=true / state=LOCKED）",
        status == 200 and body.get("locked") is True and body.get("state") == "LOCKED",
        f"HTTP {status} locked={body.get('locked')} state={body.get('state')}",
    )
    status, payload = api(
        "POST", "/api/business/erp/opening-balance",
        {"period": PERIOD, "entries": [
            {"account_code": "1002", "debit": 1, "credit": 0},
            {"account_code": "3001", "debit": 0, "credit": 1},
        ]}, TOKEN,
    )
    check(
        "⑪c 结账后录入科目期初 → 409",
        status == 409,
        f"HTTP {status} code={code_of(payload)} {msg_of(payload)[:100]}",
    )
    status, payload = api(
        "POST", "/api/business/erp/opening-balance/inventory",
        {"period": PERIOD, "items": [
            {"material_id": material_id, "warehouse_id": warehouse_id, "qty": 1, "unit_cost": 1},
        ]}, TOKEN,
    )
    check(
        "⑪d 结账后录入库存期初 → 409",
        status == 409,
        f"HTTP {status} code={code_of(payload)} {msg_of(payload)[:100]}",
    )

    # ---------- ⑫ 全账不变量（B2：库存金额 = 1405 余额） ----------
    inv_amount = one("SELECT COALESCE(SUM(amount),0)::numeric(20,4) FROM biz_inventory")
    gl_1405 = one(
        "SELECT COALESCE(SUM(e.debit_amount - e.credit_amount),0)::numeric(20,4) "
        "FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL "
        "JOIN biz_account a ON a.id = e.account_id WHERE a.account_code LIKE '1405%'"
    )
    check("⑫ 账账相符：库存金额 = 总账 1405 余额", eq_num(inv_amount, num(gl_1405)),
          f"库存 {inv_amount} / 1405 {gl_1405}")

    # ---------- 收尾 ----------
    cleanup()
    after = baseline()
    check("⑬ 收尾基线核对（期间表回到运行前）", before == after, f"before={before} after={after}")
    residue = one(
        "SELECT COUNT(*) FROM biz_opening_balance WHERE period_code LIKE '2027-%'"
    )
    check("⑭ 无残留期初行", residue == "0", f"残留 {residue} 行")

    passed = sum(1 for _, ok, _ in RESULT if ok)
    total = len(RESULT)
    print("\n" + "=" * 88)
    print(f"汇总：{passed}/{total} 通过")
    print("=" * 88)
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
