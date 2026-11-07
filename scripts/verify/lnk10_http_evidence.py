#!/usr/bin/env python3
# LNK-10 真机取证：折旧批次（DEPRECIATION）过账 / 红冲 → 联动引擎（真 HTTP 链路）
#
# 链路（触发与查询全走 HTTP；只有「读通知条数 / 清夹具」走 psql）：
#   ① GET  /api/business/flow/linkage —— 配置页必须显示 DEPRECIATION wired=true
#   ② PUT  DEPRECIATION/POSTED、DEPRECIATION/REVERSED 各一条 SEND_NOTICE（TAKEOVER，收件人 = 制单人）
#   ③ POST /api/business/erp/period —— 开账 2047-08
#   ④ POST /api/business/erp/asset × N —— 建 3 张卡（起提日 2047-01-01，本期应提）
#   ⑤ POST /api/business/erp/depreciation/run —— 手动计提
#        → run_batch 第 ⑤ 步触发联动：批次 POSTED + 通知 **1 条**（聚合：3 台资产也只发一条）
#   ⑥ 再次 POST run（幂等闸 Exists）→ 通知仍 1 条、凭证仍 1 张（重复计提不重复触发）
#   ⑦ POST /api/business/erp/depreciation/{id}/reverse —— 红冲
#        → 批次 REVERSED + REVERSED 通知 1 条
#   ⑧ 再次 reverse（状态机 409）→ 通知不增
#
# 用法：python3 scripts/verify/lnk10_http_evidence.py [base_url]
# 前置：bash scripts/verify/lnk10_start.sh（实例在 18061）
# 夹具：期间 / 资产卡 / 规则 / 通知 / 凭证 / 批次由本脚本自建自清（按 id + 号段删）。
# ============================================================================
import atexit
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18061"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk10-depreciation/curl_evidence.txt"
DB = "cloudpivot_lnk09n"
YEAR, MONTH = 2047, 8
PERIOD = f"{YEAR}-{MONTH:02}"
PURCHASE_DATE = "2046-12-10"

_STATE = {"rules": [], "assets": [], "batch_id": None, "dept_id": None}
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
    say("## 清场（夹具全部删掉，凭 id 集合 / 号段）")
    psql("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK10-%'")
    psql("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:DEPRECIATION:%'")
    batch = _STATE.get("batch_id")
    if batch:
        psql("DELETE FROM biz_voucher WHERE source_type IN "
             f"('DEPRECIATION','DEPRECIATION_REVERSAL') AND source_id = '{batch}'::uuid")
        psql(f"DELETE FROM biz_depreciation WHERE batch_id = '{batch}'::uuid")
        psql(f"DELETE FROM biz_depreciation_batch WHERE id = '{batch}'::uuid")
        psql("DELETE FROM biz_status_log WHERE entity_type = 'DEPRECIATION' "
             f"AND entity_id = '{batch}'::uuid")
    for asset in _STATE.get("assets") or []:
        psql("DELETE FROM biz_voucher WHERE source_type = 'FIXED_ASSET_ACQ' "
             f"AND source_id = '{asset}'::uuid")
        psql(f"DELETE FROM biz_fixed_asset_change WHERE asset_id = '{asset}'::uuid")
        psql("DELETE FROM biz_status_log WHERE entity_type = 'FIXED_ASSET' "
             f"AND entity_id = '{asset}'::uuid")
        psql(f"DELETE FROM biz_fixed_asset WHERE id = '{asset}'::uuid")
    periods = psql(f"SELECT id FROM biz_accounting_period WHERE period_code = '{PERIOD}'")
    if periods:
        psql("DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' "
             f"AND entity_id IN ({','.join(chr(39) + p + chr(39) + '::uuid' for p in periods.split())})")
        psql(f"DELETE FROM biz_accounting_period WHERE period_code = '{PERIOD}'")
    dept = _STATE.get("dept_id")
    if dept:
        psql(f"DELETE FROM sys_dept WHERE id = '{dept}'::uuid")
    say("# 夹具已清（期间 / 资产卡 / 批次 + 明细 / 凭证 / 规则 / 通知 / 部门）")


atexit.register(cleanup)


def login(username, password="Admin@123456"):
    payload, _ = call("POST", "/api/system/auth/login",
                      {"username": username, "password": password}, expect=None)
    if payload.get("code") == 0:
        return payload["data"]["access_token"]
    say(f"!! {username} 登录失败：{json.dumps(payload, ensure_ascii=False)[:200]}")
    raise SystemExit(1)


def rule_upsert(token, status, action, params, name):
    payload, http = call(
        "PUT", "/api/business/flow/linkage",
        {"name": name, "trigger_entity": "DEPRECIATION", "trigger_status": status,
         "action_code": action, "action_params": params, "enabled": True, "seq": 50,
         "confirm": True},
        token, expect=0)
    rule_id = (payload.get("data") or {}).get("id")
    if rule_id:
        _STATE["rules"].append(rule_id)
    return payload, http


def notice_count(status):
    return psql("SELECT count(*) FROM sys_notice WHERE idempotency_key LIKE "
                f"'LNK:DEPRECIATION:{status}:{_STATE['batch_id']}:%'")


def notice_content(status):
    return psql("SELECT content FROM sys_notice WHERE idempotency_key LIKE "
                f"'LNK:DEPRECIATION:{status}:{_STATE['batch_id']}:%' "
                "ORDER BY created_at DESC LIMIT 1")


def posted_vouchers():
    return psql("SELECT count(*) FROM biz_voucher WHERE source_type = 'DEPRECIATION' "
                f"AND source_id = '{_STATE['batch_id']}'::uuid AND deleted_at IS NULL")


def main():
    say("## LNK-10 真机取证：折旧批次（DEPRECIATION）过账 / 红冲 → 联动引擎")
    say(f"# 实例 {BASE} / 库 {DB} / 期间 {PERIOD}")
    say()
    admin = login("admin")
    say(f"# 超管 admin 登录成功，token 长度 = {len(admin)}")

    # ---------- ① 配置页：DEPRECIATION 必须 wired=true ----------
    say()
    say("## ① GET /api/business/flow/linkage —— 配置页显示 DEPRECIATION wired=true")
    data, _ = call("GET", "/api/business/flow/linkage?trigger_entity=DEPRECIATION", token=admin)
    trigger = next(item for item in data["data"]["triggers"] if item["entity"] == "DEPRECIATION")
    say(json.dumps({"entity": trigger["entity"], "wired": trigger["wired"],
                    "wiring": trigger["wiring"][:360] + "…"}, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示 DEPRECIATION 未接线")
        raise SystemExit(1)

    # ---------- ② 两条通知规则 ----------
    say()
    say("## ② PUT /api/business/flow/linkage —— POSTED / REVERSED 各配一条 SEND_NOTICE（TAKEOVER→CREATOR）")
    params = {"mode": "TAKEOVER", "recipient": "CREATOR"}
    rule_upsert(admin, "POSTED", "SEND_NOTICE", params, "LNK10 折旧过账 → 通知操作人")
    rule_upsert(admin, "REVERSED", "SEND_NOTICE", params, "LNK10 折旧红冲 → 通知操作人")
    say(json.dumps({"rules": _STATE["rules"]}, ensure_ascii=False))

    # ---------- ③ 开账 ----------
    say()
    say(f"## ③ POST /api/business/erp/period —— 开账 {PERIOD}")
    payload, http = call("POST", "/api/business/erp/period",
                         {"period_year": YEAR, "period_month": MONTH,
                          "remark": "LNK-10 真机取证"}, token=admin)
    say(f"# http={http} period={PERIOD}")

    # ---------- ④ 建 3 张卡 ----------
    say()
    say("## ④ POST /api/business/erp/asset × 3 —— 建卡（起提日 2047-01-01，本期应提）")
    cats, _ = call("GET", "/api/business/erp/asset-category/list", token=admin)
    category = cats["data"][0]
    dept = psql("SELECT id FROM sys_dept WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1")
    if not dept:
        payload, _ = call("POST", "/api/system/dept", {
            "dept_code": "LNK10X",
            "dept_name": "LNK10 取证部门",
            "sort_order": 99,
            "status": 1,
        }, token=admin)
        dept = payload["data"]["id"]
        _STATE["dept_id"] = dept
        say(f"# 库里没有部门，已用 /api/system/dept 建夹具部门 {dept}")
    for index in range(3):
        payload, http = call("POST", "/api/business/erp/asset", {
            "asset_name": f"LNK10 取证笔记本 {index}",
            "category_id": category["id"],
            "use_dept_id": dept,
            "original_value": "100000",
            "tax_amount": "0",
            "purchase_date": PURCHASE_DATE,
            "remark": "LNK-10 真机取证",
        }, token=admin)
        asset_id = payload["data"]["id"]
        _STATE["assets"].append(asset_id)
        say(f"# 建卡 http={http} asset={asset_id}")

    # ---------- ⑤ 过账 → 触发一次（聚合）----------
    say()
    say("## ⑤ POST /api/business/erp/depreciation/run —— 手动计提（过账）")
    payload, http = call("POST", "/api/business/erp/depreciation/run",
                         {"period_code": PERIOD, "remark": "LNK-10 真机取证"}, token=admin)
    batch = payload["data"]["batch"]
    _STATE["batch_id"] = batch["id"]
    say(json.dumps({"http": http, "outcome": payload["data"]["outcome"],
                    "batch": {"id": batch["id"], "status": batch["status"],
                              "asset_count": batch["asset_count"],
                              "total_amount": batch["total_amount"]}}, ensure_ascii=False))
    posted_notices = notice_count("POSTED")
    say(f"# 通知条数（LNK:DEPRECIATION:POSTED:<批次>:*）= {posted_notices}")
    say(f"# 通知正文 = {notice_content('POSTED')}")
    say(f"# 计提凭证张数（source=DEPRECIATION）= {posted_vouchers()}")
    if posted_notices != "1":
        say("!! 过账通知不是 1 条：大批量聚合口径失败")
        raise SystemExit(1)

    # ---------- ⑥ 重复计提：不重复触发 ----------
    say()
    say("## ⑥ 再次 POST depreciation/run —— 幂等闸（Exists），通知不增、凭证不增")
    payload, http = call("POST", "/api/business/erp/depreciation/run",
                         {"period_code": PERIOD, "remark": "LNK-10 重复计提"}, token=admin)
    say(json.dumps({"http": http, "outcome": payload["data"]["outcome"],
                    "message": payload["data"]["message"]}, ensure_ascii=False))
    say(f"# 通知条数 = {notice_count('POSTED')}（必须仍是 1）")
    say(f"# 凭证张数 = {posted_vouchers()}（必须仍是 1）")
    if notice_count("POSTED") != "1" or posted_vouchers() != "1":
        say("!! 重复计提翻倍了")
        raise SystemExit(1)

    # ---------- ⑦ 红冲 → 触发一次 ----------
    say()
    say("## ⑦ POST /api/business/erp/depreciation/{id}/reverse —— 红冲")
    payload, http = call("POST", f"/api/business/erp/depreciation/{batch['id']}/reverse",
                         {"reason": "LNK-10 真机取证：基数修正"}, token=admin)
    say(json.dumps({"http": http, "status": payload["data"]["batch"]["status"]},
                   ensure_ascii=False))
    reversed_notices = notice_count("REVERSED")
    say(f"# REVERSED 通知条数 = {reversed_notices}")
    say(f"# REVERSED 通知正文 = {notice_content('REVERSED')}")
    if reversed_notices != "1":
        say("!! 红冲没有触发通知（或发了多条）")
        raise SystemExit(1)

    # ---------- ⑧ 重复红冲：不重复触发 ----------
    say()
    say("## ⑧ 再次 reverse —— 状态机挡住（非 0 code），通知不增")
    payload, http = call("POST", f"/api/business/erp/depreciation/{batch['id']}/reverse",
                         {"reason": "LNK-10 真机取证：重复红冲"}, token=admin, expect=None)
    say(json.dumps({"http": http, "code": payload.get("code"),
                    "msg": payload.get("msg")}, ensure_ascii=False))
    say(f"# REVERSED 通知条数 = {notice_count('REVERSED')}（必须仍是 1）")
    if payload.get("code") == 0 or notice_count("REVERSED") != "1":
        say("!! 重复红冲没被挡住 / 通知翻倍")
        raise SystemExit(1)

    say()
    say("## ALL PASS：过账 1 条聚合通知 / 重复计提不翻倍 / 红冲 1 条 / 重复红冲被挡")


try:
    main()
finally:
    with open(OUT, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")
