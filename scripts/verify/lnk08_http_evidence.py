#!/usr/bin/env python3
# LNK-08 真机取证：会计期间（ACCOUNTING_PERIOD）结账 / 反结账 → 联动引擎（真 HTTP 链路）
#
# 链路：
#   ① GET  /api/business/flow/linkage —— 配置页必须显示 ACCOUNTING_PERIOD wired=true
#   ② PUT  ACCOUNTING_PERIOD/CLOSED 与 /OPEN 的 SEND_NOTICE（TAKEOVER，收件人 = 操作人）
#   ③ POST /api/business/erp/period —— 开 2033-05（OPEN）
#   ④ POST /api/business/erp/period/{id}/close —— 结账
#        → status CLOSED + sys_notice 落 LNK:PERIOD:CLOSED:<id> 恰好 1 条
#   ⑤ 重复 close（应 409）→ CLOSED 通知仍 1 条（重复触发被抑制）
#   ⑥ POST /api/business/erp/period/{id}/reopen —— 反结账（原因 ≥ 10 字）
#        → status OPEN + LNK:PERIOD:OPEN:<id> 恰好 1 条
#   ⑦ 回滚场景（卡面硬要求：事务回滚时通知不发）：
#        开 2033-06 → 在库里注入「commit 期失败」的延迟约束触发器
#        → close 返回 500 / 期间仍 OPEN / 通知 0 条
#        → 拆掉注入后再 close 成功 → 通知 1 条（证明上面的 0 不是规则没配上）
#
# 用法：python3 scripts/verify/lnk08_http_evidence.py [base_url]
# 前置：bash scripts/verify/lnk08_start.sh（实例在 18055）+ 本机能 psql 连 cloudpivot_lnk08
# 夹具：期间 / 规则 / 通知 / 注入触发器由本脚本自建自清。
# ============================================================================
import atexit
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18055"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk08-period/curl_evidence.txt"
DB = "cloudpivot_lnk08"
PERIOD_YEAR = 2033
MONTH_CLOSE = 5
MONTH_FAIL = 6

_STATE = {"admin": None, "rules": [], "periods": {}}
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
        with urllib.request.urlopen(req, timeout=120) as resp:
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


def login(username):
    for password in ("Admin@123456", "Admin@123456so44"):
        payload, _ = call("POST", "/api/system/auth/login",
                          {"username": username, "password": password}, expect=None)
        token = (payload.get("data") or {}).get("access_token")
        if token:
            return token
    say(f"!! {username} 登录失败")
    raise SystemExit(1)


def cleanup():
    psql("DROP TRIGGER IF EXISTS lnk08_fail_close_trg ON biz_accounting_period")
    psql("DROP FUNCTION IF EXISTS lnk08_fail_close()")
    psql("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK08%'")
    psql("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:PERIOD:%'")
    # 期间行只删本卡造的 2033-05 / 2033-06（先删状态日志，它没有外键）
    psql("DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' "
         f"AND entity_no IN ('{PERIOD_YEAR}-{MONTH_CLOSE:02}', '{PERIOD_YEAR}-{MONTH_FAIL:02}')")
    psql("DELETE FROM biz_voucher WHERE period IN "
         f"('{PERIOD_YEAR}-{MONTH_CLOSE:02}', '{PERIOD_YEAR}-{MONTH_FAIL:02}')")
    psql("DELETE FROM biz_accounting_period WHERE period_code IN "
         f"('{PERIOD_YEAR}-{MONTH_CLOSE:02}', '{PERIOD_YEAR}-{MONTH_FAIL:02}')")
    say("# 夹具已清（期间 2033-05/06 / 规则 / 通知 / 注入触发器）")


atexit.register(cleanup)


def rule_upsert(token, status, action, params, name):
    payload, http = call(
        "PUT", "/api/business/flow/linkage",
        {"name": name, "trigger_entity": "ACCOUNTING_PERIOD", "trigger_status": status,
         "action_code": action, "action_params": params, "enabled": True, "seq": 50,
         "confirm": True},
        token, expect=0)
    rule_id = (payload.get("data") or {}).get("id")
    if rule_id:
        _STATE["rules"].append(rule_id)
    return payload, http


def notice_count(status, period_id):
    return psql("SELECT count(*) FROM sys_notice WHERE idempotency_key = "
                f"'LNK:PERIOD:{status}:{period_id}'")


def period_status(period_id):
    return psql(f"SELECT status FROM biz_accounting_period WHERE id = '{period_id}'::uuid")


def open_period(token, month):
    payload, _ = call("POST", "/api/business/erp/period",
                      {"period_year": PERIOD_YEAR, "period_month": month,
                       "remark": "LNK-08 真机取证"}, token, expect=0)
    data = payload["data"]
    _STATE["periods"][month] = data["id"]
    return data["id"]


def main():
    say("## LNK-08 真机取证：会计期间（ACCOUNTING_PERIOD）结账 / 反结账 → 联动引擎")
    say()
    admin = login("admin")
    _STATE["admin"] = admin
    say(f"# 超管 admin 登录成功，token 长度 = {len(admin)}")

    # ---------- ① 配置页：ACCOUNTING_PERIOD 必须 wired=true ----------
    say()
    say("## ① GET /api/business/flow/linkage —— 配置页显示 ACCOUNTING_PERIOD wired=true")
    data, _ = call("GET", "/api/business/flow/linkage?trigger_entity=ACCOUNTING_PERIOD",
                   token=admin)
    trigger = next(item for item in data["data"]["triggers"]
                   if item["entity"] == "ACCOUNTING_PERIOD")
    say(json.dumps({"entity": trigger["entity"], "wired": trigger["wired"],
                    "wiring": trigger["wiring"][:260] + "…"}, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示 ACCOUNTING_PERIOD 未接线")
        raise SystemExit(1)

    # ---------- ② 配两条规则（结账 / 反结账各一条通知） ----------
    say()
    say("## ② PUT /api/business/flow/linkage —— 结账 / 反结账各配一条 SEND_NOTICE（TAKEOVER→OPERATOR）")
    params = {"mode": "TAKEOVER", "recipient": "OPERATOR"}
    rule_upsert(admin, "CLOSED", "SEND_NOTICE", params,
                "LNK08 月结完成 → 通知操作人（告警财务组用）")
    rule_upsert(admin, "OPEN", "SEND_NOTICE", params,
                "LNK08 反结账 → 上报财务负责人")
    say(json.dumps({"rules": _STATE["rules"], "trigger_status": ["CLOSED", "OPEN"]},
                   ensure_ascii=False))

    # ---------- ③ 开账 2033-05 ----------
    say()
    say(f"## ③ POST /api/business/erp/period —— 开账 {PERIOD_YEAR}-{MONTH_CLOSE:02}")
    close_id = open_period(admin, MONTH_CLOSE)
    say(f"# 期间 id = {close_id}，status = {period_status(close_id)}")

    # ---------- ④ 结账 ----------
    say()
    say(f"## ④ POST /api/business/erp/period/{close_id}/close —— 结账（事务内跑联动、commit 后发通知）")
    payload, http = call("POST", f"/api/business/erp/period/{close_id}/close",
                         {"reason": "LNK-08 真机取证结账"}, admin, expect=0)
    say(f"# http = {http}，返回 status = {(payload.get('data') or {}).get('status')}")
    assert period_status(close_id) == "CLOSED", "结账后期间必须是 CLOSED"
    closed_notices = notice_count("CLOSED", close_id)
    say(f"# sys_notice LNK:PERIOD:CLOSED:{close_id} = {closed_notices}（期望 1）")
    if closed_notices != "1":
        say("!! 结账没有触发通知")
        raise SystemExit(1)

    # ---------- ⑤ 重复结账（应 409，通知不增） ----------
    say()
    say("## ⑤ 重复 close —— 期望 409，且通知不增（重复过账不重复触发）")
    _, http = call("POST", f"/api/business/erp/period/{close_id}/close",
                   {"reason": "重复结账"}, admin, expect=None)
    again = notice_count("CLOSED", close_id)
    say(f"# http = {http}（期望 409/400），通知仍 = {again}")
    if again != "1":
        say("!! 重复结账又发了一条通知")
        raise SystemExit(1)

    # ---------- ⑥ 反结账 ----------
    say()
    say(f"## ⑥ POST /api/business/erp/period/{close_id}/reopen —— 反结账")
    payload, http = call("POST", f"/api/business/erp/period/{close_id}/reopen",
                         {"reason": "LNK-08 真机取证：本月有一笔跨期调账要补"},
                         admin, expect=0)
    say(f"# http = {http}，返回 status = {(payload.get('data') or {}).get('status')}")
    assert period_status(close_id) == "OPEN", "反结账后期间必须是 OPEN"
    opened = notice_count("OPEN", close_id)
    say(f"# sys_notice LNK:PERIOD:OPEN:{close_id} = {opened}（期望 1）")
    if opened != "1":
        say("!! 反结账没有触发通知")
        raise SystemExit(1)

    # ---------- ⑦ 回滚场景：commit 期失败 → 通知不发 ----------
    say()
    say(f"## ⑦ 回滚场景：开 {PERIOD_YEAR}-{MONTH_FAIL:02} → 注入 commit 期失败 → 结账 500 且通知 0 条")
    fail_id = open_period(admin, MONTH_FAIL)
    psql(
        "CREATE OR REPLACE FUNCTION lnk08_fail_close() RETURNS trigger AS $$ "
        "BEGIN "
        f"IF NEW.period_code = '{PERIOD_YEAR}-{MONTH_FAIL:02}' AND NEW.status = 'CLOSED' THEN "
        "RAISE EXCEPTION 'LNK08 注入的提交期失败'; "
        "END IF; "
        "RETURN NEW; "
        "END $$ LANGUAGE plpgsql")
    psql("CREATE CONSTRAINT TRIGGER lnk08_fail_close_trg AFTER UPDATE ON biz_accounting_period "
         "DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION lnk08_fail_close()")
    _, http = call("POST", f"/api/business/erp/period/{fail_id}/close",
                   {"reason": "LNK-08 注入失败用例"}, admin, expect=None)
    failed_status = period_status(fail_id)
    failed_notices = notice_count("CLOSED", fail_id)
    say(f"# close http = {http}（期望 500），期间 status = {failed_status}（期望 OPEN），"
        f"通知 = {failed_notices}（期望 0）")
    if http != 500 or failed_status != "OPEN" or failed_notices != "0":
        say("!! 回滚场景不符合预期（通知在事务内发出来了？）")
        raise SystemExit(1)

    # 拆掉注入 → 同一期间结账应当成功且通知照发（证明 0 条是回滚造成的，不是规则没配上）
    psql("DROP TRIGGER lnk08_fail_close_trg ON biz_accounting_period")
    psql("DROP FUNCTION lnk08_fail_close()")
    call("POST", f"/api/business/erp/period/{fail_id}/close",
         {"reason": "LNK-08 注入拆除后重试"}, admin, expect=0)
    recovered = notice_count("CLOSED", fail_id)
    say(f"# 拆掉注入后 close 成功，期间 status = {period_status(fail_id)}，"
        f"通知 = {recovered}（期望 1）")
    if recovered != "1":
        say("!! 拆掉注入后通知没发（上面的 0 条是虚绿）")
        raise SystemExit(1)

    # ---------- 收尾 ----------
    say()
    say("## 收尾：删除规则（走 API，留审计）+ 清夹具")
    for rule_id in _STATE["rules"]:
        call("DELETE", f"/api/business/flow/linkage?id={rule_id}&confirm=true",
             token=admin, expect=0)
    say("# 规则已按 API 删除；期间 / 通知 / 注入触发器由 cleanup() 清")
    say()
    say("ALL PASS")


if __name__ == "__main__":
    try:
        main()
    finally:
        out_dir = OUT.rsplit("/", 1)[0]
        subprocess.run(["mkdir", "-p", out_dir], check=False)
        with open(OUT, "w", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
        print(f"# 证据已写入 {OUT}")
