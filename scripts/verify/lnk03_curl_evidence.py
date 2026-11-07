#!/usr/bin/env python3
# ============================================================================
# LNK-03 真机取证：PAYABLE 付款 / 部分付款 → 联动通知真的发出来，且**余额只扣一次**
#
# 走的是生产同款 HTTP 链路（不是直接调函数）：
#   配规则（配置页 API）→ 应付单付款 ——POST /pay→ pay_in_tx → settle_payment_in_tx
#   （凭证 → 回写 → transition_if_changed → ⑤ 联动 run_excluding）→ commit 后 run_deferred
#   → sys_notice 落 LNK:PAYABLE:SETTLED:<id> / LNK:PAYABLE:PARTIAL_PAID:<id>
#
# 用法：python3 scripts/verify/lnk03_curl_evidence.py [base_url]
# 前置：bash scripts/verify/lnk03_start.sh（实例在 18049）+ 本机能 psql 连 cloudpivot_so44
#
# 取证前置：本卡要验证的是**直付路径**（pay_in_tx）。取证库 cloudpivot_so44 里
# `payable_pay` 流程是 PUBLISHED 的（种子 V0175），此时 POST /pay 会走审批（202）而不是直付。
# 脚本先把它置 DRAFT、跑完再恢复 PUBLISHED —— 审批路径与直付路径共用
# `settle_payment_in_tx`（联动只在那一个函数里接），代码级由 tests_wiring 的
# 「两条入口都要 flush」断言 + 真库用例覆盖。
# ============================================================================
import atexit
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18049"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk03-payable/curl_evidence.txt"
DB = "cloudpivot_so44"

SUPPLIER = "d1370005-0000-4000-8000-000000000001"  # SUP-0001（V0137 种子）

# 每次跑的应付单号后缀（唯一索引 uk_biz_payable_no：同号复跑会撞）
RUN_SUFFIX = time.strftime("%H%M%S")

# 收尾状态（异常退出时由 atexit 兜底恢复流程 / 删规则，不把取证库弄脏）
_STATE = {"flow": None, "token": None, "rules": [], "done": False}

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
        say(f"!! {method} {path} 期望 code={expect}，实际 http={http} {json.dumps(payload, ensure_ascii=False)[:600]}")
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


def login():
    for password in ("Admin@123456so44", "Admin@123456"):
        payload, _ = call(
            "POST",
            "/api/system/auth/login",
            {"username": "admin", "password": password},
            expect=None,
        )
        token = (payload.get("data") or {}).get("access_token")
        if token:
            return token
    say("!! 登录失败")
    raise SystemExit(1)


def rule_upsert(token, status, name):
    payload, _ = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": name,
            "trigger_entity": "PAYABLE",
            "trigger_status": status,
            "action_code": "SEND_NOTICE",
            "action_params": {"mode": "TAKEOVER", "recipient": "CREATOR"},
            "enabled": True,
            "seq": 30,
            "confirm": True,
        },
        token,
    )
    return payload["data"]


def seed_payable(no_suffix, amount):
    """直接落一张应付单（取证夹具；生产里它由收货过账创建，本卡不测那条链）。"""
    admin_id = psql("SELECT id FROM sys_user WHERE username = 'admin' LIMIT 1")
    period = psql(
        "SELECT period_code FROM biz_accounting_period WHERE status = 'OPEN' "
        "ORDER BY period_code LIMIT 1"
    )
    due = psql(
        f"SELECT start_date FROM biz_accounting_period WHERE period_code = '{period}'"
    )
    rows = psql(
        "INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no, "
        "currency_code, amount, paid_amount, unpaid_amount, credit_days, due_date, period_code, "
        "status, create_by) VALUES (gen_random_uuid(), "
        f"'AP-LNK03-{no_suffix}-{RUN_SUFFIX}', '{SUPPLIER}'::uuid, 'STOCK_IN', gen_random_uuid(), "
        f"'LNK03-IN-{no_suffix}', 'CNY', {amount}, 0, {amount}, 0, DATE '{due}', '{period}', "
        f"'OPEN', '{admin_id}'::uuid) RETURNING id || '|' || payable_no"
    )
    payable_id, payable_no = rows.split("|")
    return payable_id, payable_no, period, admin_id


def restore():
    """收尾（幂等）：恢复 payable_pay 流程状态、删掉本卡的规则。"""
    if _STATE["done"]:
        return
    _STATE["done"] = True
    if _STATE["flow"]:
        psql(f"UPDATE biz_flow_def SET status = '{_STATE['flow']}' WHERE flow_code = 'payable_pay'")
        say(f"# payable_pay 流程恢复 {_STATE['flow']}")
    for rule_id in _STATE["rules"]:
        try:
            call("DELETE", f"/api/business/flow/linkage?id={rule_id}&confirm=true", token=_STATE["token"])
            say(f"# 规则 {rule_id} 已删除")
        except SystemExit:
            psql(f"DELETE FROM biz_flow_linkage_rule WHERE id = '{rule_id}'::uuid")
            say(f"# 规则 {rule_id} 用 SQL 删除（API 失败）")


atexit.register(restore)


def main():
    token = login()
    _STATE["token"] = token
    say(f"# 超管登录成功，token 长度 = {len(token)}")

    # ---------- ① 配置页：PAYABLE 必须显示 wired=true（LNK-03 接线生效） ----------
    say()
    say("## ① GET /api/business/flow/linkage?trigger_entity=PAYABLE —— 配置页显示 wired=true")
    data, _ = call("GET", "/api/business/flow/linkage?trigger_entity=PAYABLE", token=token)
    trigger = next(item for item in data["data"]["triggers"] if item["entity"] == "PAYABLE")
    say(json.dumps({
        "entity": trigger["entity"],
        "wired": trigger["wired"],
        "wiring": trigger["wiring"][:160] + "…",
    }, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示 PAYABLE 未接线")
        raise SystemExit(1)

    # ---------- ② 两条规则（走写入口，未接线触发点会被 400 —— 这里必须 problems=[]） ----------
    say()
    say("## ② PUT /api/business/flow/linkage —— PAYABLE/SETTLED + PAYABLE/PARTIAL_PAID → SEND_NOTICE")
    settled_rule = rule_upsert(token, "SETTLED", "LNK03 应付结清 → 通知制单人")
    partial_rule = rule_upsert(token, "PARTIAL_PAID", "LNK03 应付部分已付 → 通知制单人")
    _STATE["rules"] = [settled_rule["id"], partial_rule["id"]]
    for rule in (settled_rule, partial_rule):
        say(json.dumps({
            "id": rule["id"],
            "trigger": rule["trigger_entity"] + "/" + rule["trigger_status"],
            "action": rule["action_code"],
            "phase": rule["phase"],
            "implemented": rule["implemented"],
            "problems": rule["problems"],
        }, ensure_ascii=False))
        if rule["problems"]:
            say("!! 写入口报 problems，规则不可用")
            raise SystemExit(1)

    # ---------- ③ 取证前置：把 payable_pay 流程临时下线（要验的是直付路径） ----------
    say()
    old_flow = psql("SELECT status FROM biz_flow_def WHERE flow_code = 'payable_pay'")
    _STATE["flow"] = old_flow
    psql("UPDATE biz_flow_def SET status = 'DRAFT' WHERE flow_code = 'payable_pay'")
    say(f"# payable_pay 流程 {old_flow} → DRAFT（取证完毕恢复）；直付 = pay_in_tx 路径")

    # ---------- ④ 全额付款：OPEN → SETTLED ----------
    say()
    say("## ④ POST /api/business/erp/payable/{id}/pay —— 全额付款 100/100（直付）")
    full_id, full_no, period, admin_id = seed_payable("FULL", "100")
    say(f"# 夹具应付单 {full_no} id={full_id} amount=100 period={period} create_by={admin_id}")
    payload, http = call(
        "POST",
        f"/api/business/erp/payable/{full_id}/pay",
        {
            "amount": 100,
            "pay_method": "银行转账",
            "remark": "LNK-03 真机取证（全额）",
            "skip_approval": True,
            "skip_reason": "LNK03 真机取证：取证库未启用 payable_pay 流程，显式跳过审批走直付",
        },
        token,
    )
    say(f"# HTTP {http}")
    say(json.dumps(payload.get("data"), ensure_ascii=False, indent=1)[:800])

    # ---------- ⑤ 部分付款：OPEN → PARTIAL_PAID ----------
    say()
    say("## ⑤ POST /api/business/erp/payable/{id}/pay —— 部分付款 30/100（直付）")
    part_id, part_no, _, _ = seed_payable("PART", "100")
    say(f"# 夹具应付单 {part_no} id={part_id} amount=100")
    payload, http = call(
        "POST",
        f"/api/business/erp/payable/{part_id}/pay",
        {
            "amount": 30,
            "pay_method": "银行转账",
            "remark": "LNK-03 真机取证（部分）",
            "skip_approval": True,
            "skip_reason": "LNK03 真机取证：取证库未启用 payable_pay 流程，显式跳过审批走直付",
        },
        token,
    )
    say(f"# HTTP {http}")
    say(json.dumps(payload.get("data"), ensure_ascii=False, indent=1)[:800])

    # ---------- ⑥ 库内物证 ----------
    say()
    say("## ⑥ 库内物证（余额 / 状态 / 凭证 / 通知 / 状态日志）")
    for label, pid, no, want_status, want_paid, want_unpaid in (
        ("全额", full_id, full_no, "SETTLED", "100.00", "0.00"),
        ("部分", part_id, part_no, "PARTIAL_PAID", "30.00", "70.00"),
    ):
        got = psql(
            "SELECT status || '|' || (paid_amount::numeric(18,2))::text || '|' || "
            f"(unpaid_amount::numeric(18,2))::text FROM biz_payable WHERE id = '{pid}'::uuid"
        ).split("|")
        say(f"- {label} {no}: status={got[0]} paid={got[1]} unpaid={got[2]}"
            f"（期望 {want_status}/{want_paid}/{want_unpaid}）")
        if got != [want_status, want_paid, want_unpaid]:
            say("!! 余额 / 状态与期望不符（联动可能重复扣款）")
            raise SystemExit(1)
        pay_cnt = psql(f"SELECT count(*) FROM biz_payment WHERE payable_id = '{pid}'::uuid")
        v_cnt = psql(
            "SELECT count(*) FROM biz_voucher v JOIN biz_payment p ON v.source_id = p.id "
            f"WHERE v.source_type = 'PAYMENT' AND p.payable_id = '{pid}'::uuid"
        )
        say(f"  付款记录 {pay_cnt} 条；付款凭证 {v_cnt} 张（期望 1 —— 排除 GENERATE_VOUCHER 生效）")
        if pay_cnt != "1" or v_cnt != "1":
            say("!! 付款记录或凭证条数不符")
            raise SystemExit(1)

    for label, pid, status, title in (
        ("全额", full_id, "SETTLED", "应付单已结清"),
        ("部分", part_id, "PARTIAL_PAID", "应付单部分已付"),
    ):
        key = f"LNK:PAYABLE:{status}:{pid}"
        rows = psql(
            "SELECT coalesce(string_agg(title || ' ⇢ ' || target_type || ' ⇢ ' || "
            "coalesce(biz_type,'') || ' ⇢ ' || coalesce(link_url,''), ' ; '), '(无)') "
            f"FROM sys_notice WHERE idempotency_key = '{key}'"
        )
        say(f"- {label} 通知 {key} → {rows}")
        if rows == "(无)":
            say("!! 联动通知没落库（引擎没被调到 / wired 假接线）")
            raise SystemExit(1)

    logs = psql(
        f"SELECT string_agg(coalesce(from_status,'∅') || '→' || to_status, ' ; ' ORDER BY created_at) "
        f"FROM biz_status_log WHERE entity_type = 'PAYABLE' AND entity_id = '{full_id}'::uuid"
    )
    say(f"- {full_no} 状态日志：{logs}")

    # ---------- ⑦ 收尾：恢复流程、删规则（单据保留为物证） ----------
    say()
    say("## ⑦ 收尾")
    restore()
    say(f"# 取证单据保留：{full_no}（SETTLED）/ {part_no}（PARTIAL_PAID）")

    say()
    say("# 结论：PAYABLE 已接线 —— 全额付款 / 部分付款都真的触发联动（通知落 sys_notice），"
        "余额只扣一次（100→0 / 100→70），付款凭证每笔恰好 1 张（排除清单生效）")

    import os
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")
    print(f"\n证据已写入 {OUT}")


if __name__ == "__main__":
    main()
