#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M1-24 财务缺口收口 E2E：多币种 → 现金流底稿 → 账龄，一条链跑通。

卡：`t_cf5ab203`（父卡 `t_55590fbd` M1-23）。上游：M1-01..M1-23。

# 这张卡干什么

M1 的 23 张卡各自钉住了自己那一段，但没有一条用例把「多币种 → 底稿 → 账龄」
放在**同一个库、同一个实例、同一条链路**上跑一遍。单卡全绿仍可能出现：

* 调汇凭证把外币原币口径污染（余额表 / 明细账 / 报表三处各自算各自的尺子）；
* 底稿期初与上期资产负债表的货币资金对不上（跨期口径各写一份）；
* 账龄表的档位与应付账龄不是同一张表（M1-21 之前就是 5 档 vs 7 档）。

所以本脚本断言的是**链上的恒等式**，不是各接口的形状：

| # | 恒等式 | 断言 |
|:--|:--|:--|
| ① | 外币凭证锁定汇率 = 凭证日的 SPOT 牌价（区间语义） | 1-10 建单锁 7.0 / 1-20 查询给 7.5 / 二期区间末日 = 前一期生效日 − 1 |
| ② | 未标「外币核算」的科目不许进外币凭证 | 400 + 66081，且报文点名科目 |
| ③ | 调汇差额 = 折算 − 账面；落账后差额归零、**原币不变** | fx-balance 前后对比 + 调汇凭证 0 条外币分录 |
| ④ | 余额表 / 明细账 / 资产负债表三处用**同一把尺子** | 1001 closing_fc == 明细账 closing_fc == 报表 amount；折算列逐位相等 |
| ⑤ | 调汇凭证不进外币原币口径（也不是「第二遍累计」） | 报表 amount 仍 1000（不是 1500），明细账行数不增 |
| ⑥ | 底稿三处对拍：期初 = 上期货币资金、期末 = 本月总账货币资金、净增加额 = 本月现金账面变动 | BS(1998-01)=7450 → opening；BS(1998-02)=7650 = closing；SQL 现金变动 200 = net_increase |
| ⑦ | 底稿不闭合时页面能开、导出被闸（400 + 66087），修平即恢复 | 反例 + 自愈 |
| ⑧ | 账龄入表金额 = **未收余额**，余额整笔落自身档位 | 收 300 的 1000 单只出 700，且整笔在 D31_60 |
| ⑨ | 跨期口径：金额 = **当前**未收余额快照（基准日之后的收款会减掉），档位 / 逾期天数按基准日重算 | as_of 1998-02-28 两轮 2900 → 2800（收 100 后），1998-03-31 档位前移 |
| ⑩ | 应收 / 应付账龄用**同一张档位表**（7 档） | 两份出参 buckets key 逐字相同 |
| ⑪ | 账龄合计 = 库侧独立聚合（SQL 自己算一遍） | summary vs `SUM(amount-received)` by bucket |

# 跑法（不要把库名写死成生产库）

```bash
bash scripts/verify/m124_finance_chain_e2e.sh            # 99 上跑，连 145:5432
bash scripts/verify/m124_finance_chain_e2e.sh 127.0.0.1  # 145 上跑，走 loopback
```

脚本只被 driver 调用（driver 负责建库 / 起后端 / 迁移断言 / 收场），
也可以对已在跑隔离实例手工执行：

```bash
M124_BASE=http://127.0.0.1:18099/api \
M124_PGHOST=172.18.10.145 M124_PGDATABASE=cp_m124_e2e \
M124_PGUSER=cloudpivot M124_PGPASSWORD=*** python3 scripts/verify/m124_finance_chain_e2e.py
```
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE = os.environ.get("M124_BASE", "http://127.0.0.1:18099/api").rstrip("/")
PGHOST = os.environ.get("M124_PGHOST", "172.18.10.145")
PGDATABASE = os.environ.get("M124_PGDATABASE", "cp_m124_e2e")
PGUSER = os.environ.get("M124_PGUSER", "cloudpivot")
PGPASSWORD = os.environ.get("M124_PGPASSWORD", "fbb1263691ff75c31ec99eaf258138b4")
ADMIN = os.environ.get("M124_ADMIN", "admin")
PASSWORD = os.environ.get("M124_PASSWORD", "M124@Drill#2026")

PASS = 0
FAILS = []


# ---------------------------------------------------------------------------
# 断言 / IO helpers
# ---------------------------------------------------------------------------
def check(name, cond, detail=""):
    global PASS
    if cond:
        PASS += 1
        print(f"[PASS] {name}")
        return True
    print(f"[FAIL] {name}  {detail}")
    FAILS.append(f"{name}  {detail}")
    return False


def section(title):
    print(f"\n== {title} ==")


def dec(value):
    if value is None or value == "":
        return None
    return Decimal(str(value))


def num_eq(label, actual, expected):
    got = dec(actual)
    want = Decimal(str(expected))
    return check(label, got is not None and got == want, f"actual={actual!r} expected={want}")


def str_eq(label, actual, expected):
    return check(label, actual == expected, f"actual={actual!r} expected={expected!r}")


def date_minus_one(text):
    """`yyyy-MM-dd` 减一天（区间末日 = 下一期生效日 − 1）。"""
    if not text:
        return None
    y, m, d = (int(x) for x in text.split("-"))
    from datetime import date, timedelta

    return (date(y, m, d) - timedelta(days=1)).isoformat()


def http_abs(url, method="GET", body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as err:
        return err.code, err.read(), dict(err.headers or {})


def http(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{BASE}{path}", data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as err:
        return err.code, err.read(), dict(err.headers or {})


def api(method, path, body=None, token=None):
    """→ (status, payload|None, raw_bytes)"""
    status, raw, _ = http(method, path, body, token)
    try:
        payload = json.loads(raw.decode())
    except Exception:
        payload = None
    return status, payload, raw


def data_of(payload):
    if not isinstance(payload, dict):
        return {}
    return payload.get("data") or {}


def err_code(payload):
    if not isinstance(payload, dict):
        return None
    return payload.get("code")


def err_msg(payload):
    if not isinstance(payload, dict):
        return ""
    return str(payload.get("msg") or "")


def sql(query):
    """单值查询（第一行第一列）。连不上库直接失败退出——不允许静默 SKIP。"""
    proc = subprocess.run(
        ["psql", "-h", PGHOST, "-U", PGUSER, "-d", PGDATABASE, "-tAc", query],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": PGPASSWORD},
    )
    if proc.returncode != 0:
        print(f"FATAL: SQL 失败：{query}\n{proc.stderr[:800]}")
        sys.exit(2)
    return proc.stdout.strip()


def sql_exec(query):
    proc = subprocess.run(
        ["psql", "-h", PGHOST, "-U", PGUSER, "-d", PGDATABASE, "-q", "-v", "ON_ERROR_STOP=1", "-c", query],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": PGPASSWORD},
    )
    if proc.returncode != 0:
        print(f"FATAL: SQL 执行失败：{query}\n{proc.stderr[:800]}")
        sys.exit(2)


def buckets_of(report):
    return {cell["key"]: cell for cell in report.get("summary") or []}


def detail_of(report, ar_no):
    for row in report.get("details") or []:
        if row.get("ar_no") == ar_no or row.get("payable_no") == ar_no:
            return row
    return None


def bs_line(report, item_code):
    """资产负债表行在 assets / liabilities / equity 三个分区里（不在顶层 lines）。"""
    for side in ("assets", "liabilities", "equity"):
        for row in report.get(side) or []:
            if row.get("item_code") == item_code:
                return row
    return {}


# ---------------------------------------------------------------------------
# 登录（admin 制单 / checker 复核：内控开着时制单 ≠ 审核）
# ---------------------------------------------------------------------------
def login(username, password):
    status, payload, raw = api("POST", "/system/auth/login", {"username": username, "password": password})
    token = data_of(payload).get("access_token")
    check(f"{username} 登录 200 + 拿到 access_token", status == 200 and bool(token),
          f"status={status} body={raw[:200]!r}")
    return token


def ensure_checker():
    """复制 admin 的口令哈希造一个复核账号（持 SUPER_ADMIN），内控开关保持开着。"""
    sql_exec(
        "insert into sys_user (username, password, must_change_password) "
        "select 'm124checker', password, false from sys_user "
        " where username='admin' and deleted_at is null "
        "   and not exists (select 1 from sys_user u where u.username='m124checker' and u.deleted_at is null)"
    )
    sql_exec(
        "insert into sys_user_role (user_id, role_id) "
        "select u.id, r.id from sys_user u, sys_role r "
        " where u.username='m124checker' and u.deleted_at is null "
        "   and r.role_code='SUPER_ADMIN' and r.deleted_at is null "
        "   and not exists (select 1 from sys_user_role ur where ur.user_id=u.id and ur.role_id=r.id)"
    )


# ===========================================================================
# A. 多币种（M1-01..M1-12）
# ===========================================================================
FX_PERIOD = "1998-01"
FX_PERIOD_ID = None
ACC = {}
V1 = None
V2 = None


def phase_a(token, checker):
    global FX_PERIOD_ID, V1, V2

    section("A0) 夹具：期间 1997-12..1998-03 + 标准科目（本位币 CNY）")
    sql_exec(
        "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status) values "
        "(1997,12,'1997-12','1997-12-01','1997-12-31','OPEN'),"
        "(1998,1,'1998-01','1998-01-01','1998-01-31','OPEN'),"
        "(1998,2,'1998-02','1998-02-01','1998-02-28','OPEN'),"
        "(1998,3,'1998-03','1998-03-01','1998-03-31','OPEN') "
        "on conflict (period_code) do update set status='OPEN', closed_at=null, closed_by=null"
    )
    for code in ("1001", "1002", "1012", "1122", "2202", "5603"):
        ACC[code] = sql(f"select id from biz_account where account_code='{code}' and deleted_at is null")
    missing = [code for code, pid in ACC.items() if not pid]
    check("标准科目 1001/1002/1012/1122/2202/5603 都在（模板库自带）", not missing, f"缺 {missing}")
    if missing:
        return
    FX_PERIOD_ID = sql(f"select id from biz_accounting_period where period_code='{FX_PERIOD}'")
    check(f"期间 {FX_PERIOD} 就位（调汇要按期间 id 打）", bool(FX_PERIOD_ID), FX_PERIOD_ID)
    str_eq("账套本位币 = CNY", sql("select code from biz_currency where is_base and status='ACTIVE' limit 1"), "CNY")

    section("A1) 汇率区间（M1-02 写入口派生 effective_to）")
    st, p, _ = api("POST", "/business/erp/exchange-rate",
                   {"from_currency": "USD", "rate": 7.0, "rate_type": "SPOT", "effective_date": "1998-01-01"}, token)
    check("POST /exchange-rate（USD 7.0 @1998-01-01）200", st == 200, f"status={st} msg={err_msg(p)}")
    st, p, _ = api("POST", "/business/erp/exchange-rate",
                   {"from_currency": "USD", "rate": 7.5, "rate_type": "SPOT", "effective_date": "1998-01-16"}, token)
    check("POST /exchange-rate（USD 7.5 @1998-01-16）200", st == 200, f"status={st} msg={err_msg(p)}")
    str_eq("写入口把前一期区间封到下一期生效日 −1（1998-01-15）",
           sql("select effective_to from biz_exchange_rate where from_currency='USD' "
               "and effective_date='1998-01-01' and to_currency='CNY'"), "1998-01-15")
    # 模板库本身就有 USD 的现行牌价（2026 年），所以 1998-01-16 这一期不是「开口」，
    # 而是被下一期（模板那条 2026-11-01）封住 —— 断言「区间首尾相接」而写死 NULL 是错的。
    NEXT_USD = sql("select min(effective_date) from biz_exchange_rate where from_currency='USD' "
                   "and to_currency='CNY' and rate_type='SPOT' and effective_date > '1998-01-16'")
    str_eq(f"后一期区间 = 下一期生效日 − 1（next={NEXT_USD}，说明模板已有 2026 牌价）",
           sql("select effective_to from biz_exchange_rate where from_currency='USD' "
               "and effective_date='1998-01-16' and to_currency='CNY'"),
           str(date_minus_one(NEXT_USD)))

    section("A2) 凭证日牌价：同源（fx-options 与建单锁定同一把尺子）")
    st, p, _ = api("GET", "/business/erp/voucher/fx-options?voucher_date=1998-01-10", token=token)
    d = data_of(p)
    usd = next((c for c in d.get("currencies") or [] if c.get("code") == "USD"), {})
    check("fx-options 200", st == 200, f"status={st} msg={err_msg(p)}")
    str_eq("base_currency = CNY", d.get("base_currency"), "CNY")
    num_eq("1998-01-10 的 USD 牌价 = 7.0（区间内）", usd.get("exchange_rate"), 7)
    st, p, _ = api("GET", "/business/erp/voucher/fx-options?voucher_date=1998-01-20", token=token)
    usd = next((c for c in data_of(p).get("currencies") or [] if c.get("code") == "USD"), {})
    num_eq("1998-01-20 的 USD 牌价 = 7.5（跨了区间）", usd.get("exchange_rate"), 7.5)

    section("A3) 外币科目标记（M1-03 接口）+ 反证：未标记科目不许进外币凭证")
    for code in ("1001", "2202"):
        st, p, _ = api("PUT", f"/business/erp/account/{ACC[code]}/foreign-currency",
                       {"is_foreign_currency": True}, token)
        check(f"标记 {code} 为外币核算 200", st == 200, f"status={st} msg={err_msg(p)}")
    check("库里 1001/2202 真的落成 1（不是只改返回体）",
          sql("select count(*) from biz_account where account_code in ('1001','2202') and is_foreign_currency=1") == "2")
    st, p, _ = api("POST", "/business/erp/voucher", {
        "voucher_date": "1998-01-10", "summary": "M124 反证：未标外币科目", "currency_code": "USD",
        "entries": [{"account_id": ACC["1012"], "debit": 1}, {"account_id": ACC["2202"], "credit": 1}],
    }, token)
    check("未标记科目 1012 进外币凭证 → 400 + 66081", st == 400 and err_code(p) == 66081,
          f"status={st} code={err_code(p)} msg={err_msg(p)}")
    check("报文点名科目 1012", "1012" in err_msg(p), err_msg(p))

    section("A4) 外币凭证：1000 USD @7.0（借 1001 / 贷 2202）")
    st, p, body = api("POST", "/business/erp/voucher", {
        "voucher_date": "1998-01-10", "summary": "M124 USD 期初", "currency_code": "USD",
        "entries": [{"account_id": ACC["1001"], "debit": 1000}, {"account_id": ACC["2202"], "credit": 1000}],
    }, token)
    check("USD 凭证建单 200（落 DRAFT）", st == 200, f"status={st} body={body[:300]!r}")
    d = data_of(p)
    V1 = d.get("id")
    str_eq("单头币种 = USD", d.get("currency_code"), "USD")
    num_eq("单头锁定汇率 = 7.0", d.get("exchange_rate"), 7)
    str_eq("建单后状态 = DRAFT（还要审核 + 记账）", d.get("status"), "DRAFT")
    num_eq("分录借合计（本位币）= 7000", d.get("entry_total_debit"), 7000)
    entries = d.get("entries") or []
    fc = [e for e in entries if dec(e.get("fc_debit_amount")) == Decimal("1000")]
    check("分录同时落原币 1000 与本位币 7000",
          bool(fc) and dec(fc[0].get("debit_amount")) == Decimal("7000"),
          f"entries={entries}")

    section("A5) 本位币调整凭证：借 2202 / 贷 1002 50（只改账面、不改原币）")
    st, p, body = api("POST", "/business/erp/voucher", {
        "voucher_date": "1998-01-20", "summary": "M124 本位币调整",
        "entries": [{"account_id": ACC["2202"], "debit": 50}, {"account_id": ACC["1002"], "credit": 50}],
    }, token)
    check("本位币凭证建单 200", st == 200, f"status={st} body={body[:300]!r}")
    V2 = data_of(p).get("id")
    str_eq("单头币种 = CNY（缺省本位币）", data_of(p).get("currency_code"), "CNY")

    section("A6) 审核 + 记账（复核账号，内控不关）")
    for name, vid in (("V1（USD 1000）", V1), ("V2（CNY 50）", V2)):
        st, p, _ = api("POST", f"/business/erp/voucher/{vid}/audit", {}, checker)
        check(f"{name} 审核 200", st == 200, f"status={st} msg={err_msg(p)}")
        st, p, _ = api("POST", f"/business/erp/voucher/{vid}/post", {}, checker)
        check(f"{name} 记账 200", st == 200, f"status={st} msg={err_msg(p)}")
    str_eq("两张凭证都是 POSTED",
           sql("select string_agg(status, ',' order by voucher_no) from biz_voucher where id in "
               f"('{V1}','{V2}')"), "POSTED,POSTED")

    section("A7) 外币余额表（M1-07）：原币 / 账面 / 折算 / 差额")
    st, p, body = api("GET", f"/business/erp/report/fx-balance?period={FX_PERIOD}", token=token)
    check("fx-balance 200", st == 200, f"status={st} body={body[:300]!r}")
    rows = {(r.get("account_code"), r.get("currency_code")): r for r in data_of(p).get("rows") or []}
    r1001 = rows.get(("1001", "USD"), {})
    r2202 = rows.get(("2202", "USD"), {})
    str_eq("report_code = FX_BALANCE", data_of(p).get("report_code"), "FX_BALANCE")
    num_eq("1001 期末原币 = 1000", r1001.get("closing_fc"), 1000)
    num_eq("1001 期末账面 = 7000", r1001.get("closing_book"), 7000)
    num_eq("1001 期末汇率 = 7.5", r1001.get("close_rate"), 7.5)
    num_eq("1001 折算 = 7500（1000 × 7.5）", r1001.get("converted_base"), 7500)
    num_eq("1001 调汇差额 = +500（折算 − 账面）", r1001.get("reval_diff"), 500)
    num_eq("2202 期末原币 = -1000", r2202.get("closing_fc"), -1000)
    num_eq("2202 期末账面 = -6950（含那笔本位币 50）", r2202.get("closing_book"), -6950)
    num_eq("2202 调汇差额 = -550", r2202.get("reval_diff"), -550)
    num_eq("折算合计 = 0（借贷同币种自然抵平）", data_of(p).get("total_converted_base"), 0)
    num_eq("差额合计 = -50（净汇兑损失）", data_of(p).get("total_reval_diff"), -50)

    section("A8) 外币明细账（M1-08）：与余额表逐位对拍")
    st, p, body = api("GET",
                      f"/business/erp/report/fx-ledger?currency=USD&period_from={FX_PERIOD}&period_to={FX_PERIOD}",
                      token=token)
    check("fx-ledger 200", st == 200, f"status={st} body={body[:300]!r}")
    d = data_of(p)
    ledger = {(a.get("account_code"),): a for a in d.get("accounts") or []}
    num_eq("1001 期末原币 = 与余额表一致（1000）", (ledger.get(("1001",)) or {}).get("closing_fc"), 1000)
    num_eq("2202 期末原币 = 与余额表一致（-1000）", (ledger.get(("2202",)) or {}).get("closing_fc"), -1000)
    str_eq("明细行只有 USD 的 2 条（本位币 50 那条不进外币明细账）", str(len(d.get("rows") or [])), "2")
    check("明细账里没有 5603/1002 科目行",
          not any(r.get("account_code") in ("5603", "1002") for r in d.get("rows") or []),
          str([r.get("account_code") for r in d.get("rows") or []]))
    num_eq("期初原币合计 = 0（期初无外币）", d.get("opening_fc"), 0)

    section("A9) 期末调汇（M1-05）：试算不落库 → 落账 → 同期重跑幂等")
    st, p, body = api("POST", f"/business/erp/period/{FX_PERIOD_ID}/fx-reval", {"dry_run": True}, token)
    d = data_of(p)
    check("dry_run 200", st == 200, f"status={st} body={body[:300]!r}")
    num_eq("试算净差额 = -50", d.get("net"), -50)
    str_eq("试算出 2 行科目调整", str(len(d.get("lines") or [])), "2")
    check("试算不生成凭证", not d.get("voucher_id"), str(d.get("voucher_id")))
    str_eq("试算后库里没有 FX_REVAL_ACCOUNT 凭证",
           sql("select count(*) from biz_voucher where source_type='FX_REVAL_ACCOUNT' and deleted_at is null"), "0")

    st, p, body = api("POST", f"/business/erp/period/{FX_PERIOD_ID}/fx-reval", {}, token)
    d = data_of(p)
    check("落账 200", st == 200, f"status={st} body={body[:300]!r}")
    num_eq("落账净差额 = -50", d.get("net"), -50)
    check("落账生成调汇凭证（有单号）", bool(d.get("voucher_no")), str(d.get("voucher_no")))
    str_eq("调汇凭证：本位币 / rate=1 / POSTED / FX_REVAL_ACCOUNT / 期间 1998-01",
           sql("select currency_code||'/'||exchange_rate||'/'||status||'/'||source_type||'/'||period "
               f"from biz_voucher where id='{d.get('voucher_id')}'"), "CNY/1.00000000/POSTED/FX_REVAL_ACCOUNT/1998-01")
    str_eq("调汇凭证 3 条分录（1001 借 500 / 2202 贷 550 / 5603 借 50 兜平）",
           sql("select string_agg(account_code||':'||debit_amount||'/'||credit_amount, ' ' order by line_no) "
               f"from biz_voucher_entry where voucher_id='{d.get('voucher_id')}'"),
           "1001:500.0000/0.0000 2202:0.0000/550.0000 5603:50.0000/0.0000")
    str_eq("调汇凭证没有任何原币金额（不是第二遍外币分录）",
           sql("select count(*) from biz_voucher_entry "
               f"where voucher_id='{d.get('voucher_id')}' and (fc_debit_amount is not null or fc_credit_amount is not null)"),
           "0")

    st, p, body = api("POST", f"/business/erp/period/{FX_PERIOD_ID}/fx-reval", {}, token)
    check("同期重跑仍 200（幂等，不报错）", st == 200, f"status={st} body={body[:300]!r}")
    str_eq("同期重跑不再生成第二张调汇凭证",
           sql("select count(*) from biz_voucher where source_type='FX_REVAL_ACCOUNT' and deleted_at is null"), "1")

    section("A10) 调汇后：差额归零、原币一分没变（口径不被污染）")
    st, p, _ = api("GET", f"/business/erp/report/fx-balance?period={FX_PERIOD}", token=token)
    rows = {(r.get("account_code"), r.get("currency_code")): r for r in data_of(p).get("rows") or []}
    r1001 = rows.get(("1001", "USD"), {})
    r2202 = rows.get(("2202", "USD"), {})
    num_eq("1001 调汇后原币仍是 1000", r1001.get("closing_fc"), 1000)
    num_eq("1001 调汇后账面 = 7500（7000 + 500）", r1001.get("closing_book"), 7500)
    num_eq("1001 调汇后差额归零", r1001.get("reval_diff"), 0)
    num_eq("2202 调汇后原币仍是 -1000", r2202.get("closing_fc"), -1000)
    num_eq("2202 调汇后差额归零", r2202.get("reval_diff"), 0)

    section("A11) 资产负债表按币种（M1-09）：与余额表同一把尺子")
    st, p, body = api("GET", f"/business/erp/report/balance-sheet?period={FX_PERIOD}&currency=USD", token=token)
    d = data_of(p)
    check("USD 资产负债表 200", st == 200, f"status={st} body={body[:300]!r}")
    cur = d.get("currency") or {}
    str_eq("mode = FOREIGN", cur.get("mode"), "FOREIGN")
    num_eq("期间末日汇率 = 7.5（与余额表 close_rate 同源）", cur.get("rate"), 7.5)
    line = bs_line(d, "MONETARY_FUNDS")
    num_eq("货币资金原币 = 1000（= 余额表 1001 closing_fc，调汇凭证不进原币口径）",
           line.get("amount"), 1000)
    num_eq("货币资金折合 = 7500（= 余额表 converted_base）", line.get("converted_amount"), 7500)
    check("勾稽平衡（USD 原币口径也平）", bool((d.get("check") or {}).get("balanced")),
          str((d.get("check") or {}).get("message")))

    st, p, _ = api("GET", f"/business/erp/report/balance-sheet?period={FX_PERIOD}", token=token)
    d_base = data_of(p)
    base_line = bs_line(d_base, "MONETARY_FUNDS")
    num_eq("本位币货币资金 = 7450（1001 7500 + 1002 -50）—— 底稿期初就取这个数",
           base_line.get("amount"), 7450)
    check("本位币口径不出折合列", base_line.get("converted_amount") is None,
          str(base_line.get("converted_amount")))

    section("A12) 2 月的本位币现金流入 200（让底稿与总账能对上）")
    st, p, body = api("POST", "/business/erp/voucher", {
        "voucher_date": "1998-02-15", "summary": "M124 二月现金流入（本位币）",
        "entries": [{"account_id": ACC["1001"], "debit": 200}, {"account_id": ACC["2202"], "credit": 200}],
    }, token)
    check("2 月本位币凭证建单 200", st == 200, f"status={st} body={body[:300]!r}")
    v3 = data_of(p).get("id")
    st, p, _ = api("POST", f"/business/erp/voucher/{v3}/audit", {}, checker)
    check("V3 审核 200", st == 200, f"status={st} msg={err_msg(p)}")
    st, p, _ = api("POST", f"/business/erp/voucher/{v3}/post", {}, checker)
    check("V3 记账 200", st == 200, f"status={st} msg={err_msg(p)}")
    num_eq("1998-02 货币资金账面变动 = +200（SQL 独立聚合，底稿要对的就是这个数）",
           sql("select coalesce(sum(e.debit_amount - e.credit_amount),0) from biz_voucher_entry e "
               "join biz_voucher v on v.id = e.voucher_id "
               "where v.status='POSTED' and v.period='1998-02' and e.account_code in ('1001','1002','1012')"), 200)
    return base_line.get("amount")


# ===========================================================================
# B. 现金流底稿（M1-13..M1-18）
# ===========================================================================
WS_PERIOD = "1998-02"
WS_PREV = "1998-01"
WS_OPENING = "7450.0000"
WS_CLOSING = "7650.0000"


def worksheet_payload(closing):
    return {
        "period": WS_PERIOD,
        "opening": {"amount": WS_OPENING, "remark": "上期结转（= 1998-01 资产负债表货币资金）"},
        "closing": {"amount": closing},
        "items": [
            {"item_code": "OPER_SALES_RECEIPTS", "amount": "200.0000", "remark": "销售回款"},
        ],
    }


def phase_b(token, base_monetary):
    section("B1) 底稿录入（M1-14）：期初 = 上期货币资金，勾稽闭合")
    num_eq("上期（1998-01）资产负债表货币资金 = 7450（底稿期初就取这个数）", base_monetary, 7450)
    check("底稿期初常量与上期货币资金一致（同一把尺子）", dec(WS_OPENING) == dec(base_monetary),
          f"opening={WS_OPENING} bs={base_monetary}")
    st, p, body = api("PUT", "/business/erp/report/cash-flow/worksheet", worksheet_payload(WS_CLOSING), token)
    d = data_of(p)
    check("PUT worksheet 200", st == 200, f"status={st} body={body[:400]!r}")
    check("返回侧勾稽闭合", bool((d.get("check") or {}).get("balanced")),
          str((d.get("check") or {}).get("message")))

    st, p, _ = api("GET", f"/business/erp/report/cash-flow/worksheet?period={WS_PERIOD}", token=token)
    d = data_of(p)
    check("GET worksheet 200", st == 200, f"status={st}")
    num_eq("回读期初 = 7450", (d.get("opening") or {}).get("amount"), 7450)
    num_eq("回读期末 = 7650", (d.get("closing") or {}).get("amount"), 7650)
    rows = {r.get("item_code"): r for r in d.get("rows") or []}
    num_eq("回读：销售收款 = 200", (rows.get("OPER_SALES_RECEIPTS") or {}).get("amount"), 200)
    num_eq("回读：未录入的采购付款 = 0（配置项全列，未录 ≠ 缺行）",
           (rows.get("OPER_PURCHASE_PAYMENTS") or {}).get("amount"), 0)
    check("回读行包含未录入的配置项（明细行数 > 已录行）", len(d.get("rows") or []) > 1,
          str(len(d.get("rows") or [])))
    check("期间未结账 ⇒ editable = true", d.get("editable") is not False, str(d.get("editable")))

    section("B2) 由底稿出表（M1-16）：三栏 + 勾稽闭合 + 期初期末")
    st, p, body = api("GET", f"/business/erp/report/cash-flow/statement?period={WS_PERIOD}", token=token)
    d = data_of(p)
    check("GET statement 200", st == 200, f"status={st} body={body[:400]!r}")
    str_eq("source = WORKSHEET（数据来自底稿）", d.get("source"), "WORKSHEET")
    str_eq("上期期间 = 1998-01", d.get("previous_period"), WS_PREV)
    st_eq = d.get("check") or {}
    check("本期勾稽闭合", bool(st_eq.get("balanced")), str(st_eq.get("message")))
    check("报表加减关系成立（流入 − 流出 = 净增加额 = 期末 − 期初）", bool(st_eq.get("arithmetic_ok")),
          str(st_eq))
    num_eq("现金净增加额 = 200（300 流入 − 100 流出）", d.get("net_increase", {}).get("current"), 200)
    num_eq("期末现金 = 7650", d.get("closing", {}).get("current"), 7650)
    num_eq("期初现金 = 7450（跨链：上期资产负债表货币资金）", d.get("opening", {}).get("current"), 7450)
    check("上期列整列 null（1998-01 没有底稿，不冒充 0）",
          d.get("opening", {}).get("previous") is None and d.get("closing", {}).get("previous") is None,
          f"opening.previous={d.get('opening', {}).get('previous')} "
          f"closing.previous={d.get('closing', {}).get('previous')}")
    str_eq("三栏顺序固定（OPERATING/INVESTING/FINANCING）",
           ",".join(s.get("activity") for s in d.get("sections") or []), "OPERATING,INVESTING,FINANCING")
    check("报表明细行数 > 10（配置项全列 + 已录行）", (d.get("line_count") or 0) > 10, str(d.get("line_count")))

    section("B2b) 跨链对拍：底稿 ↔ 总账 ↔ 外币报表（三处必须同一套数）")
    st, p, _ = api("GET", "/business/erp/report/balance-sheet?period=1998-02", token=token)
    bs_feb = bs_line(data_of(p), "MONETARY_FUNDS")
    num_eq("1998-02 资产负债表货币资金 = 7650（7450 + 本月现金 200）", bs_feb.get("amount"), 7650)
    num_eq("= 现金流量表期末现金（底稿与总账一致）", bs_feb.get("amount"),
           d.get("closing", {}).get("current"))
    st, p, _ = api("GET", "/business/erp/report/balance-sheet?period=1998-02&currency=USD", token=token)
    d_usd = data_of(p)
    usd_feb = bs_line(d_usd, "MONETARY_FUNDS")
    num_eq("1998-02 外币报表货币资金原币 = 1000（调汇凭证不进原币口径，跨月仍成立）",
           usd_feb.get("amount"), 1000)
    st, p, _ = api("GET", "/business/erp/report/fx-balance?period=1998-02", token=token)
    fx_feb = {(r.get("account_code"), r.get("currency_code")): r for r in data_of(p).get("rows") or []}
    num_eq("= 外币余额表 1001 期末原币（同一把尺子，本月没有外币业务）",
           (fx_feb.get(("1001", "USD")) or {}).get("closing_fc"), 1000)
    num_eq("1998-02 外币余额表 1001 折算 = 1000 × 7.5", (fx_feb.get(("1001", "USD")) or {}).get("converted_base"), 7500)

    section("B3) 底稿导出 xlsx（真文件）")
    status, raw, headers = http("GET",
                                f"/business/erp/report/cash-flow/statement/export?period={WS_PERIOD}", token=token)
    check("statement/export 200", status == 200, f"status={status} body={raw[:200]!r}")
    check("xlsx 真文件（PK 魔数 + >5KB）", raw[:2] == b"PK" and len(raw) > 5000, f"size={len(raw)}")
    check("Content-Type 是 spreadsheetml",
          "spreadsheetml" in (headers.get("Content-Type") or headers.get("content-type") or ""),
          str(headers.get("Content-Type")))

    section("B4) 反证：期末改坏 → 页面能开、导出被闸；修平即恢复")
    sql_exec(f"update biz_cashflow_worksheet set amount = 7750.0000 "
             f"where period_code='{WS_PERIOD}' and line_type='CLOSING'")
    st, p, _ = api("GET", f"/business/erp/report/cash-flow/statement?period={WS_PERIOD}", token=token)
    d = data_of(p)
    check("不闭合时 GET 仍 200（页面要能打开去补）", st == 200, f"status={st}")
    check("不闭合时 check.balanced = false", (d.get("check") or {}).get("balanced") is False,
          str((d.get("check") or {}).get("balanced")))
    num_eq("差额精确 = -100（期初 + 净增加 − 期末）", (d.get("check") or {}).get("diff"), -100)
    st, p, body = api("GET", f"/business/erp/report/cash-flow/statement/export?period={WS_PERIOD}", token=token)
    check("不闭合时导出 → 400（出表闸）", st == 400, f"status={st}")
    check("错误码 66087 + 报文点名期间", err_code(p) == 66087 and WS_PERIOD in err_msg(p),
          f"code={err_code(p)} msg={err_msg(p)}")
    st, p, _ = api("PUT", "/business/erp/report/cash-flow/worksheet", worksheet_payload(WS_CLOSING), token)
    check("改回 7650（用户自愈，不改库）→ PUT 200 且闭合",
          st == 200 and bool((data_of(p).get("check") or {}).get("balanced")), f"status={st}")
    status, raw, _ = http("GET",
                          f"/business/erp/report/cash-flow/statement/export?period={WS_PERIOD}", token=token)
    check("改回后导出恢复 200", status == 200 and raw[:2] == b"PK", f"status={status}")

    section("B5) 幂等：同一批再存一遍 → 行数不变、报表逐行相同")
    before_rows = sql(f"select count(*) from biz_cashflow_worksheet where period_code='{WS_PERIOD}'")
    st, p, _ = api("PUT", "/business/erp/report/cash-flow/worksheet", worksheet_payload(WS_CLOSING), token)
    after_rows = sql(f"select count(*) from biz_cashflow_worksheet where period_code='{WS_PERIOD}'")
    str_eq("重存后底稿行数不变（upsert 不重复插）", after_rows, before_rows)
    st, p, _ = api("GET", f"/business/erp/report/cash-flow/statement?period={WS_PERIOD}", token=token)
    d = data_of(p)
    num_eq("重存后净增加额仍 200（合并没重复计数）", d.get("net_increase", {}).get("current"), 200)
    num_eq("重存后期末仍 7650", d.get("closing", {}).get("current"), 7650)


# ===========================================================================
# C. 账龄（M1-19..M1-23）
# ===========================================================================
CUST = {"id": None, "name": "M124 收口客户"}
ARS = {}
AS_OF = "1998-02-28"
AS_OF_LATER = "1998-03-31"


def phase_c(token):
    section("C0) 客户 + 4 张应收（不同到期日，覆盖 未到期 / 1-30 / 31-60）")
    st, p, body = api("POST", "/business/erp/customer", {"name": CUST["name"], "credit_days": 0}, token)
    check("建客户 200", st == 200, f"status={st} body={body[:300]!r}")
    CUST["id"] = data_of(p).get("id")
    check("拿到客户 id", bool(CUST["id"]), str(CUST["id"]))
    specs = [
        ("AR1", "1000", "1997-12-31", "1997-12"),
        ("AR2", "1000", "1998-01-31", "1998-01"),
        ("AR3", "800", "1998-02-20", "1998-02"),
        ("AR4", "600", "1998-03-15", "1998-02"),
    ]
    for tag, amount, due, period in specs:
        st, p, body = api("POST", "/business/erp/receivable", {
            "customer_id": CUST["id"], "amount": amount, "due_date": due, "period_code": period,
            "remark": f"M124 账龄夹具 {tag}",
        }, token)
        check(f"{tag} 挂账 200（{amount} 到期 {due}）", st == 200, f"status={st} body={body[:300]!r}")
        ARS[tag] = data_of(p).get("id")

    section("C1) 部分收款 + 核销（M1-22 的真写路径：create → allocate）")
    st, p, body = api("POST", "/business/erp/receipt", {
        "customer_id": CUST["id"], "receipt_date": "1998-02-10", "amount": "500",
        "method": "BANK_TRANSFER", "remark": "M124 部分收款",
    }, token)
    check("登记收款 500 200", st == 200, f"status={st} body={body[:300]!r}")
    receipt_id = data_of(p).get("id")
    st, p, body = api("POST", f"/business/erp/receipt/{receipt_id}/allocate", {
        "items": [{"receivable_id": ARS["AR1"], "amount": "300"},
                  {"receivable_id": ARS["AR2"], "amount": "200"}],
    }, token)
    check("核销到 AR1 300 / AR2 200 → 200", st == 200, f"status={st} body={body[:300]!r}")
    for tag, exp_received, exp_status in (("AR1", "300", "PARTIAL"), ("AR2", "200", "PARTIAL")):
        str_eq(f"{tag} 已收金额回写 = {exp_received}",
               sql(f"select received_amount from biz_receivable where id='{ARS[tag]}'"), f"{exp_received}.0000")
        str_eq(f"{tag} 状态 = {exp_status}",
               sql(f"select status from biz_receivable where id='{ARS[tag]}'"), exp_status)
        str_eq(f"{tag} 未收 = amount − received",
               sql(f"select unsettled_amount from biz_receivable where id='{ARS[tag]}'"),
               f"{Decimal(1000) - Decimal(exp_received):.4f}")

    section(f"C2) 应收账龄（M1-19/20/23）基准日 {AS_OF}")
    st, p, body = api("GET", f"/business/erp/receivable/aging?as_of={AS_OF}", token=token)
    rep = data_of(p)
    check("账龄接口 200", st == 200, f"status={st} body={body[:300]!r}")
    str_eq("report_code = RECEIVABLE_AGING", rep.get("report_code"), "RECEIVABLE_AGING")
    str_eq("as_of 来源 = QUERY", rep.get("as_of_source"), "QUERY")
    num_eq("未收合计 = 2900（3400 应收 − 500 已收）", rep.get("total_unsettled"), 2900)
    num_eq("应收金额合计 = 3400", rep.get("total_amount"), 3400)
    num_eq("已收合计 = 500", rep.get("total_received"), 500)
    num_eq("逾期合计 = 2300（不含未到期 600）", rep.get("overdue_unsettled"), 2300)
    num_eq("逾期占比 = 79.31%", rep.get("overdue_ratio"), "79.31")
    str_eq("单据数 = 4", str(rep.get("doc_count")), "4")
    str_eq("客户数 = 1（模板库 2026 年的老单据被期间截断挡在外面）", str(rep.get("customer_count")), "1")

    buckets = buckets_of(rep)
    keys = [b.get("key") for b in rep.get("buckets") or []]
    str_eq("7 档（未到期 + 30/60/90/180/365）", ",".join(keys),
           "NOT_DUE,D0_30,D31_60,D61_90,D91_180,D181_365,OVER_365")
    num_eq("未到期档 = 600（AR4）", (buckets.get("NOT_DUE") or {}).get("unpaid"), 600)
    num_eq("1-30 档 = 1600（AR2 800 + AR3 800）", (buckets.get("D0_30") or {}).get("unpaid"), 1600)
    num_eq("31-60 档 = 700（AR1 收 300 后剩 700，整笔落自身档位）",
           (buckets.get("D31_60") or {}).get("unpaid"), 700)
    num_eq("60 天以上三档都是 0", sum((dec((buckets.get(k) or {}).get("unpaid")) or Decimal("0")
                                     for k in ("D61_90", "D91_180", "D181_365", "OVER_365")), Decimal("0")), 0)

    d1 = detail_of(rep, sql(f"select ar_no from biz_receivable where id='{ARS['AR1']}'"))
    check("AR1 明细：入表金额是未收余额 700（不是应收 1000）",
          d1 is not None and dec(d1.get("unsettled_amount")) == Decimal("700")
          and dec(d1.get("amount")) == Decimal("1000"), str(d1))
    check("AR1 明细：逾期 59 天 / 档位 D31_60",
          d1 is not None and d1.get("overdue_days") == 59 and d1.get("bucket") == "D31_60",
          f"{d1}"[:200])
    check("口径提示非空（hints 必须下发）", bool(rep.get("hints")), str(rep.get("hints"))[:120])

    section("C3) 账龄合计 vs 库侧独立聚合（两条独立路径必须相等）")
    for key, sql_expr in (("NOT_DUE", "due_date >= date '1998-02-28'"),
                          ("D0_30", "date '1998-02-28' - due_date between 1 and 30"),
                          ("D31_60", "date '1998-02-28' - due_date between 31 and 60")):
        want = sql("select coalesce(sum(unsettled_amount),0) from biz_receivable "
                   "where deleted_at is null and status in ('OPEN','PARTIAL') and unsettled_amount > 0 "
                   f"and period_code <= '1998-02' and {sql_expr}")
        num_eq(f"{key} 档金额 = SQL 聚合（{want}）", (buckets.get(key) or {}).get("unpaid"), want)

    section("C4) 应收账龄导出 xlsx")
    status, raw, headers = http("GET", f"/business/erp/receivable/aging/export?as_of={AS_OF}", token=token)
    check("aging/export 200", status == 200, f"status={status} body={raw[:200]!r}")
    check("xlsx 真文件（PK 魔数 + >2KB）", raw[:2] == b"PK" and len(raw) > 2000, f"size={len(raw)}")

    section("C5) 跨期口径：金额是「当前余额快照」，档位按基准日重算（不做历史回溯）")
    st, p, body = api("POST", "/business/erp/receipt", {
        "customer_id": CUST["id"], "receipt_date": "1998-03-05", "amount": "100",
        "method": "BANK_TRANSFER", "remark": "M124 基准日之后的收款",
    }, token)
    check("3 月收款 100 200", st == 200, f"status={st} body={body[:300]!r}")
    rid2 = data_of(p).get("id")
    st, p, body = api("POST", f"/business/erp/receipt/{rid2}/allocate",
                      {"items": [{"receivable_id": ARS["AR3"], "amount": "100"}]}, token)
    check("核销到 AR3 100 → 200", st == 200, f"status={st} body={body[:300]!r}")

    st, p, _ = api("GET", f"/business/erp/receivable/aging?as_of={AS_OF}", token=token)
    rep2 = data_of(p)
    # 已决策语义（M1-22 定稿）：金额取**当前**未收余额，基准日只决定「哪些单进表 + 逾期几天」；
    # 所以 3 月收的 100 会让 2 月末的表从 2900 变成 2800 —— 这不是 bug，是「不回滚成历史值」。
    num_eq("as_of 1998-02-28 未收合计 = 2800（当前余额快照，不是历史回溯的 2900）",
           rep2.get("total_unsettled"), 2800)
    check("hints 明示「当前余额快照 / 不回滚」",
          any(("快照" in h or "不回滚" in h) for h in rep2.get("hints") or []), str(rep2.get("hints"))[:200])
    d3_early = detail_of(rep2, sql(f"select ar_no from biz_receivable where id='{ARS['AR3']}'"))
    check("AR3 在 1998-02-28 仍是 8 天逾期（档位不因后发生的收款而变），但金额已减到 700",
          d3_early is not None and d3_early.get("overdue_days") == 8
          and dec(d3_early.get("unsettled_amount")) == Decimal("700"), str(d3_early))

    st, p, _ = api("GET", f"/business/erp/receivable/aging?as_of={AS_OF_LATER}", token=token)
    rep3 = data_of(p)
    num_eq("推到 1998-03-31：未收合计仍 2800", rep3.get("total_unsettled"), 2800)
    b3 = buckets_of(rep3)
    num_eq("1998-03-31：AR1（90 天）走进 61-90 档 = 700", (b3.get("D61_90") or {}).get("unpaid"), 700)
    num_eq("1998-03-31：AR2（59 天）+ AR3（39 天）在 31-60 档 = 1500",
           (b3.get("D31_60") or {}).get("unpaid"), 1500)
    num_eq("1998-03-31：AR4（16 天）在 1-30 档 = 600", (b3.get("D0_30") or {}).get("unpaid"), 600)

    section("C6) 应付账龄（M1-21）：与应收同一张 7 档表")
    supplier = sql("select id from biz_supplier where deleted_at is null order by supplier_code limit 1")
    check("模板库有供应商（应付账龄夹具用）", bool(supplier), supplier)
    sql_exec("delete from biz_payable where payable_no like 'M124-PB-%'")
    sql_exec(
        "insert into biz_payable (payable_no, supplier_id, source_type, source_id, source_no, currency_code, "
        " amount, paid_amount, unpaid_amount, credit_days, due_date, period_code, status) values "
        f"('M124-PB-1','{supplier}','PURCHASE_IN',gen_random_uuid(),'M124-SRC-1','CNY',1200,200,1000,0,date '1997-12-31','1997-12','PARTIAL_PAID'),"
        f"('M124-PB-2','{supplier}','PURCHASE_IN',gen_random_uuid(),'M124-SRC-2','CNY',900,0,900,0,date '1998-02-20','1998-02','OPEN'),"
        f"('M124-PB-3','{supplier}','PURCHASE_IN',gen_random_uuid(),'M124-SRC-3','CNY',400,0,400,0,date '1998-03-31','1998-02','OPEN'),"
        f"('M124-PB-4','{supplier}','PURCHASE_IN',gen_random_uuid(),'M124-SRC-4','CNY',500,500,0,0,date '1997-12-31','1997-12','SETTLED')"
    )
    check("应付夹具 4 行就位（其中 1 行已付清，必须被排除）",
          sql("select count(*) from biz_payable where payable_no like 'M124-PB-%'") == "4")
    st, p, body = api("GET", f"/business/erp/report/payable-aging?as_of={AS_OF}", token=token)
    pay = data_of(p)
    check("应付账龄 200", st == 200, f"status={st} body={body[:300]!r}")
    str_eq("report_code = PAYABLE_AGING", pay.get("report_code"), "PAYABLE_AGING")
    num_eq("未付合计 = 2300（1200−200 + 900 + 400；已付清那笔不进）", pay.get("total_unpaid"), 2300)
    str_eq("单据数 = 3（SETTLED 那行被排除）", str(pay.get("doc_count")), "3")
    pkeys = [b.get("key") for b in pay.get("buckets") or []]
    str_eq("应付也是同一张 7 档表（与应收逐字相同）", ",".join(pkeys), ",".join(keys))
    pbuckets = buckets_of(pay)
    num_eq("应付未到期档 = 400", (pbuckets.get("NOT_DUE") or {}).get("unpaid"), 400)
    num_eq("应付 1-30 档 = 900", (pbuckets.get("D0_30") or {}).get("unpaid"), 900)
    num_eq("应付 31-60 档 = 1000", (pbuckets.get("D31_60") or {}).get("unpaid"), 1000)
    pd1 = detail_of(pay, "M124-PB-1")
    check("PB1 明细：逾期 59 天 / D31_60 / 未付 1000",
          pd1 is not None and pd1.get("overdue_days") == 59 and pd1.get("bucket") == "D31_60"
          and dec(pd1.get("unpaid_amount")) == Decimal("1000"), str(pd1))
    status, raw, _ = http("GET", f"/business/erp/report/payable-aging/export?as_of={AS_OF}", token=token)
    check("应付账龄导出 200 + 真 xlsx", status == 200 and raw[:2] == b"PK", f"status={status} size={len(raw)}")

    section("C7) 基准日 / 期间参数语义（两个账龄表同口径）")
    st, p, _ = api("GET", "/business/erp/receivable/aging?period=1998-02", token=token)
    d = data_of(p)
    check("只传 period ⇒ as_of_source=PERIOD 且 as_of=期末 1998-02-28",
          d.get("as_of_source") == "PERIOD" and d.get("as_of") == "1998-02-28",
          f"{d.get('as_of_source')} / {d.get('as_of')}")
    st, p, _ = api("GET", "/business/erp/receivable/aging?as_of=1998-13-01", token=token)
    check("非法基准日 → 400", st == 400, f"status={st}")
    st, p, _ = api("GET", "/business/erp/report/payable-aging?as_of=1998-13-01", token=token)
    check("付账龄非法基准日 → 400", st == 400, f"status={st}")


# ===========================================================================
# D. 收口：重放无副作用 + 全链数字最后一次对拍
# ===========================================================================
def phase_d(token):
    section("D1) 重放：同一批 GET 再打一遍，数字逐位不变（报表无副作用）")
    st, p, _ = api("GET", f"/business/erp/report/fx-balance?period={FX_PERIOD}", token=token)
    r = {(x.get("account_code"), x.get("currency_code")): x for x in data_of(p).get("rows") or []}
    num_eq("重放：1001 期末账面仍 7500", (r.get(("1001", "USD")) or {}).get("closing_book"), 7500)
    num_eq("重放：1001 原币仍 1000", (r.get(("1001", "USD")) or {}).get("closing_fc"), 1000)
    st, p, _ = api("GET", f"/business/erp/report/cash-flow/statement?period={WS_PERIOD}", token=token)
    num_eq("重放：现金流量表期末仍 7650", data_of(p).get("closing", {}).get("current"), 7650)
    st, p, _ = api("GET", f"/business/erp/receivable/aging?as_of={AS_OF}", token=token)
    num_eq("重放：应收账龄未收仍 2800（含 3 月那笔收款后的当前余额）", data_of(p).get("total_unsettled"), 2800)

    section("D2) 库侧痕迹（收口不是只跑通接口）")
    str_eq("凭证：2 张手工 + 1 张调汇，全部 POSTED",
           sql("select string_agg(distinct status, ',' ) from biz_voucher where source_type in ('M124','MANUAL') "
               "or source_type='FX_REVAL_ACCOUNT'"), "POSTED")
    str_eq("外币分录原币合计 = 2000（借 1000 + 贷 1000，调汇不增）",
           sql("select coalesce(sum(fc_debit_amount+fc_credit_amount),0) from biz_voucher_entry"),
           "2000.0000")
    str_eq("底稿 1998-02 行数 = 3（1 明细 + 期初 + 期末）",
           sql(f"select count(*) from biz_cashflow_worksheet where period_code='{WS_PERIOD}'"), "3")
    str_eq("应收账龄涉及的 4 张单都还在（没有为了绿而删单）",
           sql("select count(*) from biz_receivable where deleted_at is null "
               "and (period_code like '1997-12' or period_code like '1998-%')"), "4")

    section("D3) 灵敏度反证：把三处关键数据改坏，报表必须跟着变（断言不是常量）")
    # ① 外币原币口径。先证明「本位币凭证上塞原币不进口径」（按凭证币种分流是产品自带的防线），
    #    再证明「外币凭证上塞原币立刻进口径」（说明这条断言真在读库，不是常量）。
    reval_vid = sql("select id from biz_voucher where source_type='FX_REVAL_ACCOUNT' "
                    "and deleted_at is null order by voucher_no limit 1")
    check("找到调汇凭证（反证要有对象）", bool(reval_vid), reval_vid)
    sql_exec("insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, "
             "account_name, debit_amount, credit_amount, fc_debit_amount, fc_credit_amount) values "
             f"('{reval_vid}', 9, 'M124 反证：本位币凭证补原币', '{ACC['1001']}', '1001', '库存现金', 100, 0, 100, 0)")
    st, p, _ = api("GET", f"/business/erp/report/fx-balance?period={FX_PERIOD}", token=token)
    rows = {(r.get("account_code"), r.get("currency_code")): r for r in data_of(p).get("rows") or []}
    num_eq("反证①-a：本位币凭证上的原币不进外币口径（取数按凭证币种分流，仍是 1000）",
           (rows.get(("1001", "USD")) or {}).get("closing_fc"), 1000)
    sql_exec(f"delete from biz_voucher_entry where voucher_id='{reval_vid}' and line_no=9")

    sql_exec("insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, "
             "account_name, debit_amount, credit_amount, fc_debit_amount, fc_credit_amount) values "
             f"('{V1}', 9, 'M124 反证：外币凭证补原币', '{ACC['1001']}', '1001', '库存现金', 100, 0, 100, 0)")
    st, p, _ = api("GET", f"/business/erp/report/fx-balance?period={FX_PERIOD}", token=token)
    rows = {(r.get("account_code"), r.get("currency_code")): r for r in data_of(p).get("rows") or []}
    num_eq("反证①-b：外币凭证上补 100 原币，余额表 1001 原币立刻变 1100（断言真在读库）",
           (rows.get(("1001", "USD")) or {}).get("closing_fc"), 1100)
    sql_exec(f"delete from biz_voucher_entry where voucher_id='{V1}' and line_no=9")
    st, p, _ = api("GET", f"/business/erp/report/fx-balance?period={FX_PERIOD}", token=token)
    rows = {(r.get("account_code"), r.get("currency_code")): r for r in data_of(p).get("rows") or []}
    num_eq("反证①恢复：删掉那条分录，原币回到 1000", (rows.get(("1001", "USD")) or {}).get("closing_fc"), 1000)

    # ② 底稿勾稽：期末改成 7700（净增加额没跟上）→ 必须转不平、导出被闸
    sql_exec(f"update biz_cashflow_worksheet set amount = 7700.0000 "
             f"where period_code='{WS_PERIOD}' and line_type='CLOSING'")
    st, p, _ = api("GET", f"/business/erp/report/cash-flow/statement?period={WS_PERIOD}", token=token)
    d = data_of(p)
    check("反证②：期末 7700 时勾稽转不平", (d.get("check") or {}).get("balanced") is False,
          str((d.get("check") or {}).get("balanced")))
    num_eq("反证②：差额 = -50（7450 + 200 − 7700）", (d.get("check") or {}).get("diff"), -50)
    sql_exec(f"update biz_cashflow_worksheet set amount = {WS_CLOSING} "
             f"where period_code='{WS_PERIOD}' and line_type='CLOSING'")
    st, p, _ = api("GET", f"/business/erp/report/cash-flow/statement?period={WS_PERIOD}", token=token)
    check("反证②恢复：改回 7650 立即闭合", bool((data_of(p).get("check") or {}).get("balanced")),
          str((data_of(p).get("check") or {}).get("message")))

    # ③ 账龄余额：把 AR2 的已收回写成 0 → 未收 +200、档位金额跟着变
    sql_exec(f"update biz_receivable set received_amount = 0, unsettled_amount = 1000 "
             f"where id = '{ARS['AR2']}'")
    st, p, _ = api("GET", f"/business/erp/receivable/aging?as_of={AS_OF}", token=token)
    rep = data_of(p)
    num_eq("反证③：AR2 已收回写成 0 后，未收合计变 3000（不是 2800）",
           rep.get("total_unsettled"), 3000)
    num_eq("反证③：1-30 档跟着变 1700（AR2 1000 + AR3 700）",
           (buckets_of(rep).get("D0_30") or {}).get("unpaid"), 1700)
    sql_exec(f"update biz_receivable set received_amount = 200, unsettled_amount = 800 "
             f"where id = '{ARS['AR2']}'")
    st, p, _ = api("GET", f"/business/erp/receivable/aging?as_of={AS_OF}", token=token)
    num_eq("反证③恢复：写回 200 后未收合计回到 2800", data_of(p).get("total_unsettled"), 2800)


def main():
    global PASS
    print(f"== M1-24 财务缺口收口 E2E ==\n   后端 {BASE}\n   库 {PGDATABASE} @ {PGHOST}")
    st, raw, _ = http_abs(BASE.rsplit("/api", 1)[0] + "/health")
    if st != 200:
        print(f"FATAL: /health 不通（status={st}），后端没起来")
        return 2
    check("/health 200（真实例）", True)

    token = login(ADMIN, PASSWORD)
    if not token:
        print("\n== 汇总：登录失败，后续不跑 ==")
        return 2
    ensure_checker()
    checker = login("m124checker", PASSWORD)
    if not checker:
        print("\n== 汇总：复核账号登录失败 ==")
        return 2

    base_monetary = phase_a(token, checker)
    if FAILS:
        print("\n!! A 段已红，B/C/D 段仍继续跑（要让失败面一次暴露完）")
    phase_b(token, base_monetary or 0)
    phase_c(token)
    phase_d(token)

    print(f"\n== 汇总：PASS={PASS} FAIL={len(FAILS)} ==")
    for f in FAILS:
        print(f"  - {f}")
    return 0 if not FAILS else 1


if __name__ == "__main__":
    sys.exit(main())
