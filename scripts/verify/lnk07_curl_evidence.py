#!/usr/bin/env python3
# LNK-07 真机取证：合同（CONTRACT）状态变更 → 联动引擎（真 HTTP 链路，不是直接调函数）
#
# 链路：
#   配置页 wired 检查（CONTRACT 必须 true）
#   → PUT CONTRACT/SIGNED 的 GENERATE_VOUCHER（应 400：合同不产生凭证）
#   → PUT 三条 SEND_NOTICE（PENDING_APPROVAL / SIGNED / ACTIVE）
#   → POST /api/business/erp/contract（制单人 = so44_sales_a，草稿）
#   → PATCH /status {PENDING_APPROVAL}（提交审批；handler ① 号调用点）
#        → sys_notice 落 LNK:CONTRACT:PENDING_APPROVAL:<id>
#   → POST /api/business/flow/task/{id}/approve（admin 审批；流程回调 settle ② 号调用点）
#        → 合同 SIGNED + LNK:CONTRACT:SIGNED:<id> 恰好 1 条；合同名下 0 张凭证
#   → 再次 approve 同一待办（应 409：审批单已结束）→ SIGNED 通知仍 1 条（重复触发被抑制）
#   → PATCH /status {ACTIVE}（③ 号调用点）→ ACTIVE 通知 1 条
#   → 重复 PATCH {ACTIVE}（应 409/400）→ ACTIVE 通知仍 1 条
#
# 用法：python3 scripts/verify/lnk07_curl_evidence.py [base_url]
# 前置：bash scripts/verify/lnk07_start.sh（实例在 18054）+ 本机能 psql 连 cloudpivot_so44
# 夹具：合同 / 规则 / 通知 / 审批实例由本脚本自建自清；采购经理角色成员临时借给 admin（结束原样挂回）。
# ============================================================================
import atexit
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18054"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk07-contract/curl_evidence.txt"
DB = "cloudpivot_so44"
MAKER_USER = "so44_sales_a"
SUPPLIER = "d1370005-0000-4000-8000-000000000001"
ROLE_PM = "20000000-0000-4000-8000-0000000f0002"

_STATE = {"admin": None, "maker": None, "contract_id": None, "rules": [],
          "borrowed": [], "instance": None}
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
        with urllib.request.urlopen(req, timeout=60) as resp:
            payload = json.loads(resp.read().decode())
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
    cid = _STATE["contract_id"]
    for rule_id in _STATE["rules"]:
        try:
            call("DELETE", f"/api/business/flow/linkage?id={rule_id}&confirm=true",
                 token=_STATE["admin"])
        except SystemExit:
            psql(f"DELETE FROM biz_flow_linkage_rule WHERE id = '{rule_id}'::uuid")
    psql("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK07%'")
    psql("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:CONTRACT:%'")
    # 要清的合同 = 本次记下来的 id ∪ `LNK07-` 命名空间里的残留（中途断言失败时 cid 可能还没记下）
    targets = []
    if cid:
        targets.append(cid)
    leftovers = psql("SELECT id FROM biz_contract WHERE contract_name LIKE 'LNK07%'")
    targets.extend(x for x in leftovers.splitlines() if x.strip() and x.strip() not in targets)
    for lid in targets:
        inst = (f"(SELECT id FROM biz_flow_instance WHERE biz_type = 'CONTRACT_APPROVE' "
                f"AND biz_id = '{lid}'::uuid)")
        txt = f"(SELECT id::text FROM biz_flow_instance WHERE biz_type = 'CONTRACT_APPROVE' " \
              f"AND biz_id = '{lid}'::uuid)"
        psql(f"DELETE FROM sys_notice_receiver WHERE notice_id IN "
             f"(SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN {txt})")
        psql(f"DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN {txt}")
        psql(f"UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE prev_instance_id IN {inst}")
        psql(f"DELETE FROM biz_flow_instance WHERE id IN {inst}")
        psql(f"DELETE FROM biz_voucher_entry WHERE voucher_id IN "
             f"(SELECT id FROM biz_voucher WHERE source_id = '{lid}'::uuid)")
        psql(f"DELETE FROM biz_voucher WHERE source_id = '{lid}'::uuid")
        psql(f"DELETE FROM biz_status_log WHERE entity_type = 'CONTRACT' "
             f"AND entity_id = '{lid}'::uuid")
        psql(f"DELETE FROM biz_contract WHERE id = '{lid}'::uuid")
        say(f"# 合同 {lid} 及其审批实例 / 状态日志 / 凭证已清")
    # 角色成员原样挂回（只还我们自己摘掉的那几条）
    for user_id in _STATE["borrowed"]:
        psql(f"INSERT INTO sys_user_role (user_id, role_id) VALUES "
             f"('{user_id}'::uuid, '{ROLE_PM}'::uuid) ON CONFLICT DO NOTHING")
    if _STATE["borrowed"]:
        say(f"# 借用的采购经理成员已挂回：{len(_STATE['borrowed'])} 条")
    if _STATE.get("admin_borrowed_pm"):
        psql("DELETE FROM sys_user_role WHERE role_id = "
             f"'{ROLE_PM}'::uuid AND user_id = "
             "(SELECT id FROM sys_user WHERE username = 'admin')")
        say("# admin 的临时 PURCHASE_MANAGER 绑定已删除（原本不在该角色）")
    if _STATE.get("granted_super"):
        psql("DELETE FROM sys_user_role WHERE user_id = "
             f"(SELECT id FROM sys_user WHERE username = '{MAKER_USER}') "
             "AND role_id = (SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN')")
        say(f"# {MAKER_USER} 的临时 SUPER_ADMIN 授权已删除")
    say("# 夹具已清（合同 / 规则 / 通知 / 审批实例 / 角色借用）")


atexit.register(cleanup)


def rule_upsert(token, status, action, params, name, expect=0):
    payload, http = call(
        "PUT", "/api/business/flow/linkage",
        {"name": name, "trigger_entity": "CONTRACT", "trigger_status": status,
         "action_code": action, "action_params": params, "enabled": True, "seq": 50,
         "confirm": True},
        token, expect=expect)
    return payload, http


def notice_count(status, contract_id):
    return psql("SELECT count(*) FROM sys_notice WHERE idempotency_key = "
                f"'LNK:CONTRACT:{status}:{contract_id}'")


def main():
    say("## LNK-07 真机取证：合同（CONTRACT）状态变更 → 联动引擎 + 重复触发抑制")
    say()
    admin = login("admin")
    _STATE["admin"] = admin
    say(f"# 超管 admin 登录成功，token 长度 = {len(admin)}")

    # ---------- ⓪ 夹具：采购经理角色临时只留 admin（会签语义：角色上每个人都会摊一条待办） ----------
    say()
    say("## ⓪ 夹具：PURCHASE_MANAGER 的成员临时借给 admin（结束原样挂回）")
    members = psql(f"SELECT user_id FROM sys_user_role WHERE role_id = '{ROLE_PM}'::uuid "
                   "AND user_id <> (SELECT id FROM sys_user WHERE username = 'admin')")
    _STATE["borrowed"] = [m for m in members.splitlines() if m.strip()]
    psql(f"DELETE FROM sys_user_role WHERE role_id = '{ROLE_PM}'::uuid "
         "AND user_id <> (SELECT id FROM sys_user WHERE username = 'admin')")
    admin_was_pm = psql("SELECT count(*) FROM sys_user_role WHERE role_id = "
                        f"'{ROLE_PM}'::uuid AND user_id = "
                        "(SELECT id FROM sys_user WHERE username = 'admin')") != "0"
    _STATE["admin_borrowed_pm"] = not admin_was_pm
    psql(f"INSERT INTO sys_user_role (user_id, role_id) SELECT id, '{ROLE_PM}'::uuid "
         "FROM sys_user WHERE username = 'admin' ON CONFLICT DO NOTHING")
    say(f"# 摘掉的非 admin 成员 = {len(_STATE['borrowed'])} 条；"
        f"admin 原本在 PURCHASE_MANAGER = {admin_was_pm}")

    # 制单人需要 business:erp:contract:create / :status 权限：临时授 SUPER_ADMIN（结束删除授权）。
    # 必须在**登录之前**授：令牌里的角色是登录那一刻的快照。
    role_super = psql("SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN'")
    already = psql("SELECT count(*) FROM sys_user_role WHERE user_id = "
                   f"(SELECT id FROM sys_user WHERE username = '{MAKER_USER}') "
                   f"AND role_id = '{role_super}'::uuid")
    if already == "0":
        psql("INSERT INTO sys_user_role (user_id, role_id) SELECT id, "
             f"'{role_super}'::uuid FROM sys_user WHERE username = '{MAKER_USER}' "
             "ON CONFLICT DO NOTHING")
        _STATE["granted_super"] = True
        say(f"# {MAKER_USER} 临时授 SUPER_ADMIN（取证完毕删除）")
    else:
        _STATE["granted_super"] = False
        say(f"# {MAKER_USER} 原本已有 SUPER_ADMIN（不重复授予）")

    maker = login(MAKER_USER)
    _STATE["maker"] = maker
    say(f"# 制单人 {MAKER_USER} 登录成功，token 长度 = {len(maker)}")

    # ---------- ① 配置页：CONTRACT 必须 wired=true ----------
    say()
    say("## ① GET /api/business/flow/linkage?trigger_entity=CONTRACT —— 配置页显示 wired=true")
    data, _ = call("GET", "/api/business/flow/linkage?trigger_entity=CONTRACT", token=admin)
    trigger = next(item for item in data["data"]["triggers"] if item["entity"] == "CONTRACT")
    say(json.dumps({"entity": trigger["entity"], "wired": trigger["wired"],
                    "wiring": trigger["wiring"][:220] + "…"}, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示 CONTRACT 未接线")
        raise SystemExit(1)

    # ---------- ② 配置期闸：合同上配 GENERATE_VOUCHER 必须 400 ----------
    say()
    say("## ② PUT /api/business/flow/linkage —— CONTRACT/SIGNED 上配 GENERATE_VOUCHER（应 400）")
    payload, http = rule_upsert(
        admin, "SIGNED", "GENERATE_VOUCHER", {"biz_type": "STOCK_IN"},
        "LNK07 合同签订 → 生成凭证（必须被拒：合同不产生凭证）", expect=None)
    say(f"# HTTP {http} code={payload.get('code')} msg={str(payload.get('msg'))[:240]}")
    if http != 400 or payload.get("code") != 10001 or "合同不产生任何凭证" not in str(payload.get("msg")):
        say("!! 配 GENERATE_VOUCHER 没有被 400 拒绝（配置期闸失效）")
        raise SystemExit(1)
    if psql("SELECT count(*) FROM biz_flow_linkage_rule WHERE name LIKE 'LNK07%'") != "0":
        say("!! 被拒绝的规则竟然落库了")
        raise SystemExit(1)
    say("# 库里没有落这条规则（拒绝发生在写入之前）")

    # ---------- ③ 三条正常规则 ----------
    say()
    say("## ③ PUT /api/business/flow/linkage —— PENDING_APPROVAL / SIGNED / ACTIVE 各一条 SEND_NOTICE")
    rules = []
    for status, label in (("PENDING_APPROVAL", "审批中"), ("SIGNED", "已签订"), ("ACTIVE", "已生效")):
        payload, _ = rule_upsert(admin, status, "SEND_NOTICE", {"mode": "TAKEOVER"},
                                 f"LNK07 合同{label} → 通知制单人")
        rule = payload["data"]
        # 新建规则可能返回 data.id 或 data.rule.id，两种都兜住
        rule_id = rule.get("id") or (rule.get("rule") or {}).get("id")
        rules.append(rule_id)
        say(f"# {status} → SEND_NOTICE 已保存 id={rule_id}（wired 已接线，无需 allow_unwired）")
    _STATE["rules"] = [r for r in rules if r]

    # ---------- ④ 建合同（草稿） ----------
    say()
    say("## ④ POST /api/business/erp/contract —— 建一张草稿合同（三个日期齐全）")
    payload, _ = call("POST", "/api/business/erp/contract", {
        "name": "LNK07-真机取证合同",
        "contract_type": "PURCHASE",
        "party_b_id": SUPPLIER,
        "amount": 50000,
        "sign_date": "2026-01-01",
        "start_date": "2026-01-02",
        "end_date": "2026-12-31",
        "remark": "LNK-07 真机取证（脚本自建自清）",
    }, token=maker)
    # `ContractDetailRes` 的 `base` 是 `#[serde(flatten)]`：单头字段直接在 data 顶层
    contract = payload["data"]
    cid = contract["id"]
    _STATE["contract_id"] = cid
    say(f"# 合同已建：id={cid} no={contract['contract_no']} status={contract['status']}")

    # ---------- ⑤ 提交审批：handler ① 号调用点 ----------
    say()
    say("## ⑤ PATCH /status {PENDING_APPROVAL} —— 提交审批（handler 联动调用点 ①）")
    payload, _ = call("PATCH", f"/api/business/erp/contract/{cid}/status",
                      {"target_status": "PENDING_APPROVAL"}, token=maker)
    say(f"# status={payload['data']['status']}")
    n = notice_count("PENDING_APPROVAL", cid)
    say(f"# sys_notice[LNK:CONTRACT:PENDING_APPROVAL:{cid}] = {n}")
    if n != "1":
        say("!! 提交审批没有触发联动通知")
        raise SystemExit(1)

    # ---------- ⑥ 审批通过：流程回调 settle ② 号调用点 ----------
    say()
    say("## ⑥ POST /api/business/flow/task/{id}/approve —— admin 审批通过（流程回调调用点 ②）")
    task = psql("SELECT t.id FROM biz_flow_task t JOIN biz_flow_instance i ON i.id = t.instance_id "
                f"WHERE i.biz_id = '{cid}'::uuid AND t.status = 'PENDING' LIMIT 1")
    if not task:
        say("!! 提交后没有待办（流程配置异常）")
        raise SystemExit(1)
    _STATE["instance"] = psql("SELECT instance_id FROM biz_flow_task "
                              f"WHERE id = '{task}'::uuid")
    payload, _ = call("POST", f"/api/business/flow/task/{task}/approve",
                      {"comment": "同意（LNK-07 真机取证）"}, token=admin)
    say(f"# 审批结果：{json.dumps(payload.get('data'), ensure_ascii=False)[:300]}")
    signed = notice_count("SIGNED", cid)
    status = psql(f"SELECT status FROM biz_contract WHERE id = '{cid}'::uuid")
    vouchers = psql(f"SELECT count(*) FROM biz_voucher WHERE source_id = '{cid}'::uuid "
                    "AND deleted_at IS NULL")
    say(f"# 合同状态 = {status}；sys_notice[LNK:CONTRACT:SIGNED:{cid}] = {signed}；"
        f"合同名下凭证 = {vouchers}")
    if status != "SIGNED" or signed != "1" or vouchers != "0":
        say("!! 审批通过后的状态 / 通知 / 凭证不符合预期")
        raise SystemExit(1)

    # ---------- ⑦ 重复 approve：幂等重放，不许再触发业务联动 ----------
    # 审批引擎对「同一待办重复 approve」是**幂等**的（`idempotent = true`：不重复推进节点、
    # 不重复写时间线）。本卡要证明的是「合同侧的联动不会跟着跑第二遍」：
    # 无论引擎返回 200+idempotent 还是 409，`LNK:CONTRACT:SIGNED` 都必须仍是 1 条。
    say()
    say("## ⑦ 再次 POST 同一条待办 /approve —— 幂等重放（重复触发抑制）")
    payload, http = call("POST", f"/api/business/flow/task/{task}/approve",
                         {"comment": "重复点击（幂等重放）"}, token=admin, expect=None)
    say(f"# HTTP {http} code={payload.get('code')} "
        f"data={json.dumps(payload.get('data'), ensure_ascii=False)[:240]}")
    if http == 200:
        idem = (payload.get("data") or {}).get("idempotent")
        if idem is not True:
            say(f"!! 重复 approve 返回 200 但不是幂等重放（idempotent={idem}）")
            raise SystemExit(1)
        say("# 引擎按幂等重放处理（idempotent = true），不重复推进")
    signed_again = notice_count("SIGNED", cid)
    still = psql(f"SELECT status FROM biz_contract WHERE id = '{cid}'::uuid")
    say(f"# 重复 approve 之后：合同状态 = {still}，sys_notice[SIGNED] = {signed_again}（必须仍是 1）")
    if signed_again != "1" or still != "SIGNED":
        say("!! 重复审批触发了第二次联动通知 / 把状态改坏")
        raise SystemExit(1)

    # ---------- ⑧ 生效：handler ③ 号调用点 + 重复点被挡 ----------
    say()
    say("## ⑧ PATCH /status {ACTIVE} —— 生效（handler 通用分支），再重复一次")
    payload, _ = call("PATCH", f"/api/business/erp/contract/{cid}/status",
                      {"target_status": "ACTIVE"}, token=maker)
    say(f"# status={payload['data']['status']}")
    say(f"# sys_notice[LNK:CONTRACT:ACTIVE:{cid}] = {notice_count('ACTIVE', cid)}")
    if notice_count("ACTIVE", cid) != "1":
        say("!! 生效没有触发联动通知")
        raise SystemExit(1)
    payload, http = call("PATCH", f"/api/business/erp/contract/{cid}/status",
                         {"target_status": "ACTIVE"}, token=maker, expect=None)
    say(f"# 重复生效：HTTP {http} code={payload.get('code')} msg={str(payload.get('msg'))[:160]}")
    if http == 200:
        say("!! 重复生效竟然成功了")
        raise SystemExit(1)
    say(f"# 重复生效之后 sys_notice[ACTIVE] = {notice_count('ACTIVE', cid)}（必须仍是 1）")
    if notice_count("ACTIVE", cid) != "1":
        say("!! 重复生效触发了第二次联动通知")
        raise SystemExit(1)

    say()
    say("## ALL PASS")


try:
    main()
finally:
    out = "\n".join(lines) + "\n"
    import os
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as fh:
        fh.write(out)
