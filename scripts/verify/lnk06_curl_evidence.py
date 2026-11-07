#!/usr/bin/env python3
# LNK-06 真机取证：凭证（VOUCHER）手工过账 / 审核 / 记账 → 联动通知，且「凭证生凭证」被拒
#
# 走生产同款 HTTP 链路（不是直接调函数）：
#
#   配置页 wired 检查 → PUT 规则（VOUCHER/POSTED 的 GENERATE_VOUCHER，应 400 死循环）
#   → PUT 三条 SEND_NOTICE（DRAFT / AUDITED / POSTED）
#   → POST /api/business/erp/voucher（手工凭证落 DRAFT，admin 制单）
#     → manual_in_tx ② run_excluding → commit → run_deferred → sys_notice 落 LNK:VOUCHER:DRAFT:<id>
#   → POST /{id}/audit（另一个账号复核：制单 ≠ 审核）
#     → transition_voucher_in_tx ⑥ run_excluding → commit → run_deferred → AUDITED 通知
#   → POST /{id}/post → POSTED 通知
#   → 再 POST /{id}/post → 409（重复记账不许重复触发）
#
# 用法：python3 scripts/verify/lnk06_curl_evidence.py [base_url]
# 前置：bash scripts/verify/lnk06_start.sh（实例在 18053）+ 本机能 psql 连 cloudpivot_so44
# 夹具全部自建自清（凭证 / 规则 / 通知 / 临时超管授权）。
# ============================================================================
import atexit
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18053"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk06-voucher/curl_evidence.txt"
DB = "cloudpivot_so44"
AUDITOR_USER = "so44_sales_a"  # 复核人（制单 ≠ 审核；临时授 SUPER_ADMIN 只是为了过权限）

_STATE = {"admin": None, "auditor": None, "rules": [], "voucher_id": None, "granted": False}
lines = []


def say(text=""):
    print(text)
    lines.append(text)


def call(method, path, body=None, token=None, expect: "int | None" = 0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            payload = json.loads(resp.read().decode())
            http = resp.status
    except urllib.error.HTTPError as error:
        payload = json.loads(error.read().decode())
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
        capture_output=True,
        text=True,
        env={"PGPASSWORD": "alun", "PATH": "/usr/bin:/bin"},
    )
    if out.returncode != 0:
        say(f"!! psql 失败：{out.stderr.strip()}")
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
    for rule_id in _STATE["rules"]:
        try:
            call("DELETE", f"/api/business/flow/linkage?id={rule_id}&confirm=true",
                 token=_STATE["admin"])
            say(f"# 规则 {rule_id} 已删除（API）")
        except SystemExit:
            psql(f"DELETE FROM biz_flow_linkage_rule WHERE id = '{rule_id}'::uuid")
            say(f"# 规则 {rule_id} 已删除（SQL 兜底）")
    subprocess.run(["bash", "/opt/cloudpivot/scripts/verify/lnk06_cleanup.sh", DB],
                   capture_output=True, text=True)
    say(f"# 夹具已清（凭证 {_STATE['voucher_id']} / 规则 / 通知）")
    if _STATE["granted"]:
        psql("DELETE FROM sys_user_role WHERE user_id = "
             "(SELECT id FROM sys_user WHERE username = 'so44_sales_a') "
             "AND role_id = (SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN')")
        say("# so44_sales_a 的临时 SUPER_ADMIN 授权已删除")


atexit.register(cleanup)


def rule_upsert(token, status, action, params, name, expect: "int | None" = 0):
    payload, http = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": name,
            "trigger_entity": "VOUCHER",
            "trigger_status": status,
            "action_code": action,
            "action_params": params,
            "enabled": True,
            "seq": 50,
            "confirm": True,
        },
        token,
        expect=expect,
    )
    return payload, http


def notice_count(status, voucher_id):
    return psql("SELECT count(*) FROM sys_notice WHERE idempotency_key = "
                f"'LNK:VOUCHER:{status}:{voucher_id}'")


def notice_title(status, voucher_id):
    return psql("SELECT coalesce(title,'') FROM sys_notice WHERE idempotency_key = "
                f"'LNK:VOUCHER:{status}:{voucher_id}' LIMIT 1")


def main():
    say("## LNK-06 真机取证：凭证（VOUCHER）手工落草稿 / 审核 / 记账 → 联动 + 死循环保护")
    say()
    admin = login("admin")
    _STATE["admin"] = admin
    say(f"# 超管 admin 登录成功，token 长度 = {len(admin)}")

    # ---------- ⓪ 夹具：复核人临时授 SUPER_ADMIN ----------
    say()
    say("## ⓪ 夹具：so44_sales_a 临时授 SUPER_ADMIN（取证完毕删除授权）")
    role_id = psql("SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN'")
    existed = psql("SELECT count(*) FROM sys_user_role WHERE user_id = "
                   "(SELECT id FROM sys_user WHERE username = 'so44_sales_a') "
                   f"AND role_id = '{role_id}'::uuid") != "0"
    psql("INSERT INTO sys_user_role (user_id, role_id) SELECT id, "
         f"'{role_id}'::uuid FROM sys_user WHERE username = 'so44_sales_a' "
         "ON CONFLICT DO NOTHING")
    _STATE["granted"] = not existed
    auditor = login(AUDITOR_USER)
    _STATE["auditor"] = auditor
    say(f"# 复核人 {AUDITOR_USER} 登录成功（原本未授超管 = {not existed}），token 长度 = {len(auditor)}")

    # ---------- ① 配置页：VOUCHER 必须 wired=true ----------
    say()
    say("## ① GET /api/business/flow/linkage?trigger_entity=VOUCHER —— 配置页显示 wired=true")
    data, _ = call("GET", "/api/business/flow/linkage?trigger_entity=VOUCHER", token=admin)
    trigger = next(item for item in data["data"]["triggers"] if item["entity"] == "VOUCHER")
    say(json.dumps({"entity": trigger["entity"], "wired": trigger["wired"],
                    "wiring": trigger["wiring"][:200] + "…"}, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示 VOUCHER 未接线")
        raise SystemExit(1)

    # ---------- ② 死循环保护：配 GENERATE_VOUCHER 必须 400 ----------
    say()
    say("## ② PUT /api/business/flow/linkage —— VOUCHER/POSTED 上配 GENERATE_VOUCHER（应 400）")
    payload, http = rule_upsert(
        admin, "POSTED", "GENERATE_VOUCHER",
        {"biz_type": "VOUCHER"},
        "LNK06 凭证过账 → 生成凭证（必须被拒：凭证生凭证的死循环）",
        expect=None,
    )
    say(f"# HTTP {http} code={payload.get('code')} msg={str(payload.get('msg'))[:220]}")
    if http != 400 or payload.get("code") != 10001:
        say("!! 配 GENERATE_VOUCHER 没有被 400 拒绝（死循环保护失效）")
        raise SystemExit(1)
    if "死循环" not in str(payload.get("msg")):
        say("!! 400 文案没有点明死循环")
        raise SystemExit(1)
    leftover = psql("SELECT count(*) FROM biz_flow_linkage_rule WHERE name LIKE 'LNK06%'")
    if leftover != "0":
        say(f"!! 被拒绝的规则竟然落库了：{leftover} 条")
        raise SystemExit(1)
    say("# 库里没有落这条规则（拒绝发生在写入之前）")

    # ---------- ③ 正常规则：三条 SEND_NOTICE ----------
    say()
    say("## ③ PUT /api/business/flow/linkage —— DRAFT / AUDITED / POSTED 各一条 SEND_NOTICE")
    rules = []
    for status, label in (("DRAFT", "草稿"), ("AUDITED", "已审核"), ("POSTED", "已记账")):
        payload, _ = rule_upsert(admin, status, "SEND_NOTICE", {"mode": "TAKEOVER"},
                                 f"LNK06 凭证{label} → 通知操作人")
        rule = payload["data"]
        rules.append(rule)
        say(json.dumps({"id": rule["id"],
                        "trigger": rule["trigger_entity"] + "/" + rule["trigger_status"],
                        "action": rule["action_code"], "phase": rule["phase"],
                        "problems": rule["problems"]}, ensure_ascii=False))
        if rule["problems"]:
            say("!! 写入口报 problems，规则不可用")
            raise SystemExit(1)
    _STATE["rules"] = [rule["id"] for rule in rules]

    # ---------- ④ 科目 / 期间 ----------
    psql("UPDATE biz_account SET status = 1 WHERE account_code IN ('1405', '2202')")
    debit_account = psql("SELECT id FROM biz_account WHERE account_code = '1405' "
                         "AND deleted_at IS NULL LIMIT 1")
    credit_account = psql("SELECT id FROM biz_account WHERE account_code = '2202' "
                          "AND deleted_at IS NULL LIMIT 1")
    period_row = psql("SELECT period_code || '|' || start_date FROM biz_accounting_period "
                      "WHERE status = 'OPEN' AND start_date <= now()::date "
                      "ORDER BY start_date DESC LIMIT 1")
    if not period_row:
        say("!! so44 没有已开启（OPEN）的会计期间，无法落手工凭证")
        raise SystemExit(1)
    period, biz_date = period_row.split("|")
    say(f"# 记账科目 借 1405（{debit_account}）/ 贷 2202（{credit_account}），期间 {period} 首日 {biz_date}")

    # ---------- ⑤ 建手工凭证（DRAFT） ----------
    say()
    say("## ⑤ POST /api/business/erp/voucher —— 手工凭证落 DRAFT（admin 制单）")
    created, http = call(
        "POST",
        "/api/business/erp/voucher",
        {
            "voucher_date": biz_date,
            "summary": "LNK06 真机取证：手工凭证",
            "remark": "LNK-06 联动接线取证",
            "entries": [
                {"account_id": debit_account, "debit": 100, "summary": "LNK06 借 存货"},
                {"account_id": credit_account, "credit": 100, "summary": "LNK06 贷 应付"},
            ],
        },
        admin,
    )
    voucher = created["data"]  # VoucherDetailRes：单头字段是 #[serde(flatten)]，没有 voucher 子对象
    _STATE["voucher_id"] = psql("SELECT id FROM biz_voucher WHERE voucher_no = "
                                f"'{voucher['voucher_no']}' ORDER BY created_at DESC LIMIT 1")
    vid = _STATE["voucher_id"]
    say(f"# HTTP {http} 凭证号 {voucher['voucher_no']} 状态 {voucher['status']} id {vid}")
    if voucher["status"] != "DRAFT":
        say(f"!! 手工凭证应落 DRAFT，实际 {voucher['status']}")
        raise SystemExit(1)
    count = notice_count("DRAFT", vid)
    say(f"# 通知 LNK:VOUCHER:DRAFT:<id> 条数 {count} 标题「{notice_title('DRAFT', vid)}」")
    if count != "1":
        say("!! 手工凭证落草稿没有触发联动通知（VOUCHER/DRAFT 的接线没生效）")
        raise SystemExit(1)

    # ---------- ⑥ 审核（另一个账号） ----------
    say()
    say("## ⑥ POST /api/business/erp/voucher/{id}/audit（复核人 ≠ 制单人）")
    audited, http = call("POST", f"/api/business/erp/voucher/{vid}/audit",
                         {"remark": "LNK06 真机取证：复核"}, auditor)
    say(f"# HTTP {http} 审核后状态 {audited['data']['status']}")
    if audited["data"]["status"] != "AUDITED":
        say("!! 审核后状态应为 AUDITED")
        raise SystemExit(1)
    count = notice_count("AUDITED", vid)
    say(f"# 通知 LNK:VOUCHER:AUDITED:<id> 条数 {count} 标题「{notice_title('AUDITED', vid)}」")
    if count != "1":
        say("!! 审核没有触发联动通知")
        raise SystemExit(1)

    # ---------- ⑦ 记账 ----------
    say()
    say("## ⑦ POST /api/business/erp/voucher/{id}/post —— 记账（AUDITED → POSTED）")
    posted, http = call("POST", f"/api/business/erp/voucher/{vid}/post", {}, auditor)
    say(f"# HTTP {http} 记账后状态 {posted['data']['status']}"
        f" 记账人 {posted['data'].get('posted_by')}")
    if posted["data"]["status"] != "POSTED":
        say("!! 记账后状态应为 POSTED")
        raise SystemExit(1)
    count = notice_count("POSTED", vid)
    say(f"# 通知 LNK:VOUCHER:POSTED:<id> 条数 {count} 标题「{notice_title('POSTED', vid)}」")
    if count != "1":
        say("!! 记账没有触发联动通知（VOUCHER/POSTED 的接线没生效）")
        raise SystemExit(1)

    # ---------- ⑧ 重复记账：409 且不重复触发 ----------
    say()
    say("## ⑧ 再 POST 一次 /post —— 409，通知仍是 1 条")
    again, http = call("POST", f"/api/business/erp/voucher/{vid}/post", {}, auditor, expect=None)
    say(f"# HTTP {http} code={again.get('code')} msg={str(again.get('msg'))[:160]}")
    if http != 409:
        say(f"!! 重复记账应 409，实际 {http}")
        raise SystemExit(1)
    count = notice_count("POSTED", vid)
    say(f"# 通知 LNK:VOUCHER:POSTED:<id> 条数 {count}（必须仍是 1）")
    if count != "1":
        say("!! 409 挡住的事务里又跑了一遍联动")
        raise SystemExit(1)

    # ---------- ⑨ 死循环保护的后果：没有多出来的凭证 ----------
    say()
    say("## ⑨ 库内物证：凭证条数（配不了 GENERATE_VOUCHER，不存在「凭证生凭证」）")
    total = psql("SELECT count(*) FROM biz_voucher WHERE summary LIKE 'LNK06%' AND deleted_at IS NULL")
    say(f"# summary LIKE 'LNK06%' 的凭证条数 {total}（应为 1：只有手工凭证本身）")
    if total != "1":
        say("!! 凭证条数异常（疑似凭证生凭证）")
        raise SystemExit(1)
    say()
    say("ALL PASS：wired=true → 配 GENERATE_VOUCHER 被 400 → DRAFT/AUDITED/POSTED 各发一条通知"
        " → 重复记账 409 不重复触发 → 只有 1 张凭证")


try:
    main()
finally:
    with open(OUT, "w") as handle:
        handle.write("\n".join(lines) + "\n")
    print(f"\n证据已写入 {OUT}")
