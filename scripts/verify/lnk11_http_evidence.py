#!/usr/bin/env python3
# LNK-11 真机取证：年结（ACCOUNTING_YEAR）close / reopen → 联动引擎（真 HTTP 链路）
#
# 链路（触发与查询全走 HTTP；只有「封 12 个月 / 读通知条数 / 清夹具」走 psql）：
#   ① GET  /api/business/flow/linkage —— 配置页必须显示 ACCOUNTING_YEAR wired=true
#   ② PUT  ACCOUNTING_YEAR/CLOSED、ACCOUNTING_YEAR/NOT_CLOSED 各一条 SEND_NOTICE（TAKEOVER，收件人 = 操作人）
#   ③ POST /api/business/erp/period/batch-open —— 开账 2043-01 ~ 2044-12（24 期）
#   ④ POST /api/business/erp/voucher × 3 + audit + post —— 造一个**含多张凭证**的年度
#   ⑤ psql 把 2043 的 12 个月置 CLOSED（年结前置；与 `year_end_test_support::close_all_months` 同口径）
#   ⑥ POST /api/business/erp/year-end/close —— 真年结
#        → 结转凭证 1 张 + 通知 **1 条**（触发单据是年度，不是每张凭证一条）
#   ⑦ 再次 close（ALREADY_CLOSED 409）→ 通知不增、结转凭证不增
#   ⑧ POST /api/business/erp/year-end/reopen —— 反年结 → 通知 1 条（带原因）
#   ⑨ 失败场景（年度 2045）：配一条**关键动作** UPDATE_RECEIVED_QTY（超收）+ SEND_NOTICE，
#      POST close → 500：年度仍未结转 / 无结转凭证 / **通知 0 条**（回滚后不发）
#   ⑩ 撤掉失败规则再 close → 成功 + 通知 1 条（证明 ⑨ 的 0 不是「规则没命中」的虚绿）
#
# 用法：python3 scripts/verify/lnk11_http_evidence.py [base_url]
# 前置：bash scripts/verify/lnk11_start.sh（实例在 18062）
# 夹具：期间 / 凭证 / 规则 / 通知 / 状态日志 / 采购单由本脚本自建自清（按年度 + 号段删）。
# ============================================================================
import atexit
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18062"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk11-year-end/curl_evidence.txt"
DB = "cloudpivot_lnk11"
OK_YEAR, OK_NEXT = 2043, 2044
BAD_YEAR, BAD_NEXT = 2045, 2046
YEARS = (OK_YEAR, OK_NEXT, BAD_YEAR, BAD_NEXT)
PO_NO = "YE-LNK11-PO-1"

_STATE = {"rules": [], "items": []}
lines = []


def say(text=""):
    print(text)
    lines.append(text)


def call(method, path, body=None, token=None, expect=0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=180) as resp:
            raw = resp.read().decode()
            payload = json.loads(raw) if raw.strip() else {}
            http = resp.status
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        payload = json.loads(raw) if raw.startswith("{") else {"raw": raw}
        http = error.code
    code = payload.get("code")
    if expect is not None and code != expect:
        say(f"!! {method} {path} 期望 code={expect}，实际 http={http} "
            f"{json.dumps(payload, ensure_ascii=False)[:800]}")
        raise SystemExit(1)
    return payload, http


def psql(sql):
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-Atqc", sql],
        capture_output=True, text=True,
        env={"PGPASSWORD": "alun", "PATH": "/usr/bin:/bin"},
    )
    if out.returncode != 0:
        say(f"!! psql 失败：{out.stderr.strip()}\nSQL: {sql}")
        raise SystemExit(1)
    return out.stdout.strip()


def cleanup():
    say()
    say("## 清场（夹具全部删掉，凭年度 + 号段）")
    psql("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK11-%'")
    psql("DELETE FROM biz_flow_linkage_rule WHERE trigger_entity = 'ACCOUNTING_YEAR'")
    psql("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:ACCOUNTING_YEAR:%'")
    for year in YEARS:
        psql(f"DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_YEAR' AND entity_no = '{year}'")
        psql("DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' "
             f"AND entity_no LIKE '{year}-%'")
        psql("DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN "
             f"(SELECT id FROM biz_voucher WHERE period LIKE '{year}-%')")
        psql(f"DELETE FROM biz_voucher WHERE period LIKE '{year}-%'")
        psql(f"DELETE FROM biz_opening_balance WHERE period_code LIKE '{year}-%'")
        psql(f"DELETE FROM biz_accounting_period WHERE period_code LIKE '{year}-%'")
    psql(f"DELETE FROM biz_purchase_order WHERE order_no = '{PO_NO}'")
    say("# 夹具已清（期间 / 凭证 / 期初 / 状态日志 / 规则 / 通知 / 采购单）")


atexit.register(cleanup)


def login(username, password="Admin@123456", quiet=False):
    payload, _ = call("POST", "/api/system/auth/login",
                      {"username": username, "password": password}, expect=None)
    if payload.get("code") == 0:
        return payload["data"]["access_token"]
    if not quiet:
        say(f"!! {username} 登录失败：{json.dumps(payload, ensure_ascii=False)[:200]}")
    raise SystemExit(1)


# 初始口令 / 首登改密后的口令（CODE-06：`must_change_password = true` 时除 me / menus /
# password / logout 之外的接口一律 403，所以取证前必须先把初始口令换掉）。
INITIAL_PASSWORD = "Admin@123456"
VERIFY_PASSWORD = "Admin@123456!"


def login_ready():
    """登录并把「首登强制改密」处理掉（幂等：口令已改过就直接用改后的口令登录）。"""
    try:
        return login("admin", VERIFY_PASSWORD, quiet=True)
    except SystemExit:
        pass
    token = login("admin", INITIAL_PASSWORD)
    payload, http = call("PUT", "/api/system/auth/password",
                         {"old_password": INITIAL_PASSWORD,
                          "new_password": VERIFY_PASSWORD}, token=token, expect=None)
    if payload.get("code") != 0:
        say(f"!! 首登改密失败：{json.dumps(payload, ensure_ascii=False)[:200]}")
        raise SystemExit(1)
    say(f"# 首登强制改密（CODE-06）：初始口令已改为固定取证口令，http={http}")
    return login("admin", VERIFY_PASSWORD)


def rule_upsert(token, status, action, params, name):
    payload, http = call(
        "PUT", "/api/business/flow/linkage",
        {"name": name, "trigger_entity": "ACCOUNTING_YEAR", "trigger_status": status,
         "action_code": action, "action_params": params, "enabled": True, "seq": 50,
         "confirm": True},
        token, expect=0)
    rule_id = (payload.get("data") or {}).get("id")
    if rule_id:
        _STATE["rules"].append({"id": rule_id, "name": name})
    return payload, http


def notice_count(status, year):
    return psql("SELECT count(*) FROM sys_notice WHERE idempotency_key LIKE "
                f"'LNK:ACCOUNTING_YEAR:{status}:{year}:%'")


def notice_content(status, year):
    return psql("SELECT content FROM sys_notice WHERE idempotency_key LIKE "
                f"'LNK:ACCOUNTING_YEAR:{status}:{year}:%' ORDER BY created_at DESC LIMIT 1")


def carry_vouchers(year):
    return psql("SELECT count(*) FROM biz_voucher WHERE voucher_type = 'YEAR_END' "
                f"AND period = '{year}-12' AND deleted_at IS NULL")


def year_status(year):
    out = psql("SELECT to_status FROM biz_status_log WHERE entity_type = 'ACCOUNTING_YEAR' "
               f"AND entity_no = '{year}' ORDER BY created_at DESC LIMIT 1")
    return out or "(无状态日志)"


def account_id(code):
    return psql(f"SELECT id FROM biz_account WHERE account_code = '{code}'")


def open_years(token, year, next_year):
    say(f"## POST /api/business/erp/period/batch-open —— 开账 {year}-01 ~ {next_year}-12（24 期）")
    payload, http = call("POST", "/api/business/erp/period/batch-open",
                         {"from": f"{year}-01", "to": f"{next_year}-12",
                          "remark": "LNK-11 真机取证"}, token=token)
    data = payload.get("data") or {}
    say(f"# http={http} created={data.get('created_count')} skipped={data.get('skipped_count')}")


def post_voucher(token, date, summary, debit_code, credit_code, amount):
    """建一张手工凭证并走完 DRAFT → AUDITED → POSTED（全真实接口）。"""
    debit = account_id(debit_code)
    credit = account_id(credit_code)
    payload, http = call("POST", "/api/business/erp/voucher", {
        "voucher_date": date,
        "summary": summary,
        "entries": [
            {"account_id": debit, "debit": amount, "summary": summary},
            {"account_id": credit, "credit": amount, "summary": summary},
        ],
        "remark": "LNK-11 真机取证",
    }, token=token)
    voucher_id = payload["data"]["id"]
    call("POST", f"/api/business/erp/voucher/{voucher_id}/audit",
         {"remark": "LNK-11 取证审核"}, token=token)
    payload, http = call("POST", f"/api/business/erp/voucher/{voucher_id}/post",
                         {}, token=token)
    say(f"# 凭证 {payload['data']['voucher_no']} http={http} status={payload['data']['status']}")
    return voucher_id


def main():
    say("## LNK-11 真机取证：年结（ACCOUNTING_YEAR）close / reopen → 联动引擎")
    say(f"# 实例 {BASE} / 库 {DB} / 成功年度 {OK_YEAR}（下年 {OK_NEXT}）/ 失败年度 {BAD_YEAR}（下年 {BAD_NEXT}）")
    say()
    admin = login_ready()
    say(f"# 超管 admin 登录成功，token 长度 = {len(admin)}")

    # ---------- ① 配置页：ACCOUNTING_YEAR 必须 wired=true ----------
    say()
    say("## ① GET /api/business/flow/linkage —— 配置页显示 ACCOUNTING_YEAR wired=true")
    data, _ = call("GET", "/api/business/flow/linkage?trigger_entity=ACCOUNTING_YEAR", token=admin)
    trigger = next(item for item in data["data"]["triggers"] if item["entity"] == "ACCOUNTING_YEAR")
    say(json.dumps({"entity": trigger["entity"], "wired": trigger["wired"],
                    "wiring": trigger["wiring"][:420] + "…"}, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示 ACCOUNTING_YEAR 未接线")
        raise SystemExit(1)

    # ---------- ② 两条通知规则 ----------
    say()
    say("## ② PUT /api/business/flow/linkage —— CLOSED / NOT_CLOSED 各配一条 SEND_NOTICE（TAKEOVER→CREATOR）")
    params = {"mode": "TAKEOVER", "recipient": "CREATOR"}
    rule_upsert(admin, "CLOSED", "SEND_NOTICE", params, "LNK11-年结完成 → 通知操作人")
    rule_upsert(admin, "NOT_CLOSED", "SEND_NOTICE", params, "LNK11-反年结 → 通知操作人")
    say(json.dumps({"rules": [item["name"] for item in _STATE["rules"]]}, ensure_ascii=False))

    # ---------- ③ 开账 ----------
    say()
    open_years(admin, OK_YEAR, OK_NEXT)

    # ---------- ④ 造一个含多张凭证的年度 ----------
    say()
    say(f"## ④ POST /api/business/erp/voucher × 3 —— 造一个含多张凭证的 {OK_YEAR} 年度")
    # 内控开关：制单与审核分离（approve=true 时本人不能审自己的凭证）。取证只有一个 admin 账号，
    # 这里按「小团队」口径关掉它（就是系统参数里那个开关），不伪造第二个账号。
    psql("UPDATE sys_config SET config_value = 'false' "
         "WHERE config_key = 'erp.separation_of_duties.approve'")
    say("# 夹具：erp.separation_of_duties.approve = false（单账号取证，关掉制单/审核分离）")
    for month, amount in ((3, "1000"), (7, "2500"), (11, "500")):
        post_voucher(admin, f"{OK_YEAR}-{month:02}-20",
                     f"LNK11 取证收入 {month} 月", "1002", "5001", amount)
    say(f"# {OK_YEAR} 年度已过账凭证数 = "
        + psql(f"SELECT count(*) FROM biz_voucher WHERE period LIKE '{OK_YEAR}-%' AND deleted_at IS NULL"))

    # ---------- ⑤ 封 12 个月（前置）----------
    say()
    say(f"## ⑤ psql —— 把 {OK_YEAR} 的 12 个月置 CLOSED（年结前置；与测试夹具同口径）")
    psql(f"UPDATE biz_accounting_period SET status = 'CLOSED', closed_at = now() "
         f"WHERE period_code LIKE '{OK_YEAR}-%'")
    say(f"# {OK_YEAR} 已结账月份 = " + psql(
        f"SELECT count(*) FROM biz_accounting_period WHERE period_code LIKE '{OK_YEAR}-%' "
        "AND status = 'CLOSED'"))

    # ---------- ⑥ 真年结 ----------
    say()
    say(f"## ⑥ POST /api/business/erp/year-end/close —— 真年结 {OK_YEAR}")
    payload, http = call("POST", "/api/business/erp/year-end/close",
                         {"year": OK_YEAR, "reason": "LNK-11 真机取证：年度结转"}, token=admin)
    state = payload["data"]["state"]
    say(json.dumps({"http": http, "year": payload["data"]["year"],
                    "voucher_no": payload["data"]["carry_forward_voucher_no"],
                    "entry_count": payload["data"]["entry_count"],
                    "net_profit": payload["data"]["net_profit"],
                    "closed": state["closed"], "status": state["status"],
                    "opening_rows": state["opening_rows"]}, ensure_ascii=False))
    say(f"# 结转凭证张数（YEAR_END / {OK_YEAR}-12）= {carry_vouchers(OK_YEAR)}（必须 1）")
    say(f"# 年度状态日志 = {year_status(OK_YEAR)}（必须 CLOSED）")
    say(f"# 通知条数（LNK:ACCOUNTING_YEAR:CLOSED:{OK_YEAR}:*）= {notice_count('CLOSED', OK_YEAR)}（必须 1）")
    say(f"# 通知正文 = {notice_content('CLOSED', OK_YEAR)}")
    if carry_vouchers(OK_YEAR) != "1" or notice_count("CLOSED", OK_YEAR) != "1":
        say("!! 年结成功但凭证 / 通知条数不对")
        raise SystemExit(1)

    # ---------- ⑦ 重复年结：不重复触发 ----------
    say()
    say(f"## ⑦ 再次 close {OK_YEAR} —— ALREADY_CLOSED（409），通知不增、凭证不增")
    payload, http = call("POST", "/api/business/erp/year-end/close",
                         {"year": OK_YEAR, "reason": "LNK-11 重复年结"}, token=admin, expect=None)
    say(json.dumps({"http": http, "code": payload.get("code"),
                    "msg": payload.get("msg")}, ensure_ascii=False))
    say(f"# 通知条数 = {notice_count('CLOSED', OK_YEAR)}（必须仍是 1）")
    say(f"# 结转凭证张数 = {carry_vouchers(OK_YEAR)}（必须仍是 1）")
    if payload.get("code") == 0 or notice_count("CLOSED", OK_YEAR) != "1":
        say("!! 重复年结没被挡住 / 通知翻倍")
        raise SystemExit(1)

    # ---------- ⑧ 反年结 → 触发一次 ----------
    say()
    say(f"## ⑧ POST /api/business/erp/year-end/reopen —— 反年结 {OK_YEAR}")
    payload, http = call("POST", "/api/business/erp/year-end/reopen",
                         {"year": OK_YEAR, "reason": "LNK-11 真机取证：反年结以重算结转基数"},
                         token=admin)
    say(json.dumps({"http": http, "reopened": payload["data"]["reopened"],
                    "deleted_voucher_no": payload["data"]["deleted_voucher_no"],
                    "december_reopened": payload["data"]["december_reopened"]},
                   ensure_ascii=False))
    say(f"# 结转凭证张数 = {carry_vouchers(OK_YEAR)}（必须 0：整张删掉）")
    say(f"# 年度状态日志 = {year_status(OK_YEAR)}（必须 NOT_CLOSED）")
    say(f"# 通知条数（LNK:ACCOUNTING_YEAR:NOT_CLOSED:{OK_YEAR}:*）= "
        f"{notice_count('NOT_CLOSED', OK_YEAR)}（必须 1）")
    say(f"# 通知正文 = {notice_content('NOT_CLOSED', OK_YEAR)}")
    if carry_vouchers(OK_YEAR) != "0" or notice_count("NOT_CLOSED", OK_YEAR) != "1":
        say("!! 反年结失败 / 通知条数不对")
        raise SystemExit(1)

    # ---------- ⑨ 失败场景：关键动作失败 → 整笔回滚、通知不发 ----------
    say()
    say(f"## ⑨ 失败场景（{BAD_YEAR}）：给失败年度配一条关键动作 UPDATE_RECEIVED_QTY（超收）+ SEND_NOTICE")
    open_years(admin, BAD_YEAR, BAD_NEXT)
    post_voucher(admin, f"{BAD_YEAR}-06-20", "LNK11 取证失败场景收入", "1002", "5001", "800")
    psql(f"UPDATE biz_accounting_period SET status = 'CLOSED', closed_at = now() "
         f"WHERE period_code LIKE '{BAD_YEAR}-%'")
    # 一张已关闭的采购单 + 一条订购 10 件的明细（CLOSED 不算未完成单据，不挡年结）
    po_id = psql("SELECT gen_random_uuid()")
    psql("INSERT INTO biz_purchase_order "
         "(id, order_no, supplier_id, order_date, warehouse_id, status, create_by) VALUES "
         f"('{po_id}', '{PO_NO}', gen_random_uuid(), '{BAD_YEAR}-05-15', gen_random_uuid(), 'CLOSED', "
         "gen_random_uuid())")
    item_id = psql("SELECT gen_random_uuid()")
    psql("INSERT INTO biz_purchase_order_item "
         "(id, order_id, line_no, material_id, unit_id, qty, price, amount, tax_amount, with_tax_amount) "
         f"VALUES ('{item_id}', '{po_id}', 1, gen_random_uuid(), gen_random_uuid(), 10, 0, 0, 0, 0)")
    rule_upsert(admin, "CLOSED", "UPDATE_RECEIVED_QTY",
                {"source_item_id": item_id, "qty": "50"},
                "LNK11-失败场景：订购10/本次收50（超收）")
    payload, http = call("POST", "/api/business/erp/year-end/close",
                         {"year": BAD_YEAR, "reason": "LNK-11 失败场景"}, token=admin, expect=None)
    say(json.dumps({"http": http, "code": payload.get("code"),
                    "msg": (payload.get("msg") or "")[:300]}, ensure_ascii=False))
    say(f"# 年度状态日志 = {year_status(BAD_YEAR)}（必须是「(无状态日志)」）")
    say(f"# 结转凭证张数 = {carry_vouchers(BAD_YEAR)}（必须 0）")
    say(f"# 通知条数（LNK:ACCOUNTING_YEAR:CLOSED:{BAD_YEAR}:*）= {notice_count('CLOSED', BAD_YEAR)}"
        f"（**必须 0**：通知排在 commit 之后，回滚就发不出去）")
    if http != 500 or carry_vouchers(BAD_YEAR) != "0" or notice_count("CLOSED", BAD_YEAR) != "0":
        say("!! 失败场景：回滚 / 通知不发 的断言没通过")
        raise SystemExit(1)
    if year_status(BAD_YEAR) != "(无状态日志)":
        say("!! 失败场景：年度居然留下了状态日志")
        raise SystemExit(1)

    # ---------- ⑩ 正向对照：撤掉失败规则再 close ----------
    say()
    say(f"## ⑩ 撤掉失败规则，再次 close {BAD_YEAR} —— 必须成功 + 通知 1 条（对照）")
    psql("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK11-失败场景%'")
    payload, http = call("POST", "/api/business/erp/year-end/close",
                         {"year": BAD_YEAR, "reason": "LNK-11 对照：撤掉失败规则"}, token=admin)
    say(json.dumps({"http": http, "voucher_no": payload["data"]["carry_forward_voucher_no"],
                    "entry_count": payload["data"]["entry_count"],
                    "closed": payload["data"]["state"]["closed"]}, ensure_ascii=False))
    say(f"# 结转凭证张数 = {carry_vouchers(BAD_YEAR)}（必须 1）")
    say(f"# 通知条数 = {notice_count('CLOSED', BAD_YEAR)}（必须 1）")
    if carry_vouchers(BAD_YEAR) != "1" or notice_count("CLOSED", BAD_YEAR) != "1":
        say("!! 对照没通过：上面的 0 可能是「规则没命中」的虚绿")
        raise SystemExit(1)

    say()
    say("## ALL PASS：年结 1 条通知 / 重复年结被挡不翻倍 / 反年结 1 条 / "
        "关键动作失败整笔回滚且通知 0 条 / 撤掉规则后成功 1 条")


try:
    main()
finally:
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")
    print(f"\n# 证据已写入 {OUT}")
