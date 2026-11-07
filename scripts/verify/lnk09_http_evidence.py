#!/usr/bin/env python3
# LNK-09 真机取证：固定资产（FIXED_ASSET）处置 / 作废处置 → 联动引擎（真 HTTP 链路）
#
# 链路（全部走 HTTP，不直接调 Rust 函数）：
#   ① GET  /api/business/flow/linkage —— 配置页必须显示 FIXED_ASSET wired=true
#   ② PUT  FIXED_ASSET/DISPOSED 与 /IN_USE 各一条 SEND_NOTICE（TAKEOVER，收件人 = 制单人）
#   ③ POST /api/business/erp/asset —— 建卡（IN_USE，同时出购入凭证）
#   ④ POST /api/business/erp/asset/{id}/dispose —— 提交处置审批
#   ⑤ POST /api/business/flow/task/{id}/approve —— 审批通过
#        → asset_bridge::execute 第 ⑤ 步触发联动：卡片 DISPOSED + LNK:FIXED_ASSET:DISPOSED:<id> 1 条
#   ⑥ 再次 approve 同一待办（幂等重放）→ 通知仍 1 条（重复触发被抑制）
#   ⑦ POST /api/business/erp/asset/{id}/dispose/cancel —— 作废处置（红冲 + 恢复在役）
#        → 卡片 IN_USE + LNK:FIXED_ASSET:IN_USE:<id> 1 条
#
# 用法：python3 scripts/verify/lnk09_http_evidence.py [base_url]
# 前置：bash scripts/verify/lnk09_start.sh（实例在 18059）+ 本机能 psql 连 cloudpivot_lnk08
# 夹具：资产卡片 / 审批实例 / 规则 / 通知 / 凭证由本脚本自建自清；
#       审批人角色（GENERAL_MANAGER）临时借给 admin，结束时原样摘掉。
# ============================================================================
import atexit
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18059"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk09-asset/curl_evidence.txt"
DB = "cloudpivot_lnk09n"
ROLE_GM = "20000000-0000-4000-8000-0000000f0003"  # GENERAL_MANAGER（asset_disposal 的总经理节点）
DISPOSE_DATE = "2099-03-15"
PURCHASE_DATE = "2099-01-15"

_STATE = {"admin": None, "asset_id": None, "rules": [], "borrowed_role": False,
          "instance": None}
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


def cleanup():
    say()
    say("## 清场（夹具全部删掉，凭 id 集合 / 号段，不按业务类型全库删）")
    psql("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK09-%'")
    psql("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:FIXED_ASSET:%'")
    asset = _STATE.get("asset_id")
    if asset:
        psql("DELETE FROM biz_voucher WHERE source_type IN "
             f"('FIXED_ASSET_DISPOSAL','FIXED_ASSET_ACQ','FIXED_ASSET_DISPOSAL_RED') "
             f"AND source_id = '{asset}'::uuid")
        psql(f"DELETE FROM biz_fixed_asset_change WHERE asset_id = '{asset}'::uuid")
        psql("DELETE FROM biz_status_log WHERE entity_type = 'FIXED_ASSET' "
             f"AND entity_id = '{asset}'::uuid")
        psql("DELETE FROM biz_flow_instance WHERE biz_type = 'ASSET_DISPOSAL' "
             f"AND biz_id = '{asset}'::uuid")
        psql(f"DELETE FROM biz_fixed_asset WHERE id = '{asset}'::uuid")
    for rule in _STATE.get("rules") or []:
        psql(f"DELETE FROM biz_flow_linkage_rule WHERE id = '{rule}'::uuid")
    dept = _STATE.get("dept_id")
    if dept:
        psql(f"DELETE FROM sys_dept WHERE id = '{dept}'::uuid")
        say("# 已删夹具部门")
    maker = _STATE.get("maker_id")
    if maker:
        psql(f"DELETE FROM sys_user_role WHERE user_id = '{maker}'::uuid")
        psql(f"DELETE FROM sys_user WHERE id = '{maker}'::uuid")
        say("# 已删夹具制单人 lnk09_maker")
    if _STATE.get("borrowed_role"):
        admin_id = psql("SELECT id FROM sys_user WHERE username = 'admin'")
        psql(f"DELETE FROM sys_user_role WHERE user_id = '{admin_id}'::uuid "
             f"AND role_id = '{ROLE_GM}'::uuid")
        _STATE["borrowed_role"] = False
        say("# 已把 admin 从 GENERAL_MANAGER 角色摘掉（借用还原）")
    say("# 夹具已清（资产卡片 / 处置审批实例 / 规则 / 通知 / 凭证 / 角色借用）")


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
        {"name": name, "trigger_entity": "FIXED_ASSET", "trigger_status": status,
         "action_code": action, "action_params": params, "enabled": True, "seq": 50,
         "confirm": True},
        token, expect=0)
    rule_id = (payload.get("data") or {}).get("id")
    if rule_id:
        _STATE["rules"].append(rule_id)
    return payload, http


def asset_status(asset_id):
    return psql(f"SELECT status FROM biz_fixed_asset WHERE id = '{asset_id}'::uuid")


def notice_count(status, asset_id):
    return psql("SELECT count(*) FROM sys_notice WHERE idempotency_key = "
                f"'LNK:FIXED_ASSET:{status}:{asset_id}'")


def disposal_vouchers(asset_id):
    return psql("SELECT count(*) FROM biz_voucher WHERE source_type = 'FIXED_ASSET_DISPOSAL' "
                f"AND source_id = '{asset_id}'::uuid AND deleted_at IS NULL")


def main():
    say("## LNK-09 真机取证：固定资产（FIXED_ASSET）处置 / 作废处置 → 联动引擎")
    say()
    admin = login("admin")
    _STATE["admin"] = admin
    say(f"# 超管 admin 登录成功，token 长度 = {len(admin)}")

    # ---------- ① 配置页：FIXED_ASSET 必须 wired=true ----------
    say()
    say("## ① GET /api/business/flow/linkage —— 配置页显示 FIXED_ASSET wired=true")
    data, _ = call("GET", "/api/business/flow/linkage?trigger_entity=FIXED_ASSET", token=admin)
    trigger = next(item for item in data["data"]["triggers"] if item["entity"] == "FIXED_ASSET")
    say(json.dumps({"entity": trigger["entity"], "wired": trigger["wired"],
                    "wiring": trigger["wiring"][:320] + "…"}, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示 FIXED_ASSET 未接线")
        raise SystemExit(1)

    # ---------- ② 配两条通知规则 ----------
    say()
    say("## ② PUT /api/business/flow/linkage —— DISPOSED / IN_USE 各配一条 SEND_NOTICE（TAKEOVER→CREATOR）")
    params = {"mode": "TAKEOVER", "recipient": "CREATOR"}
    rule_upsert(admin, "DISPOSED", "SEND_NOTICE", params,
                "LNK09 资产处置完成 → 通知制单人")
    rule_upsert(admin, "IN_USE", "SEND_NOTICE", params,
                "LNK09 作废处置 → 资产恢复在役通知制单人")
    say(json.dumps({"rules": _STATE["rules"], "trigger_status": ["DISPOSED", "IN_USE"]},
                   ensure_ascii=False))

    # ---------- ③ 造制单人 + 建卡 ----------
    say()
    say("## ③ POST /api/system/dept + /api/system/user + /api/business/erp/asset —— 造夹具与建卡")
    admin_id = psql("SELECT id FROM sys_user WHERE username = 'admin'")
    cats, _ = call("GET", "/api/business/erp/asset-category/list", token=admin)
    category = cats["data"][0]
    dept = psql("SELECT id FROM sys_dept WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1")
    if not dept:
        # 空库（全新首启的库）没有部门：用真实接口建一个，清理时按 id 删
        payload, _ = call("POST", "/api/system/dept", {
            "dept_code": "LNK09",
            "dept_name": "LNK09 取证部门",
            "sort_order": 99,
            "status": 1,
        }, token=admin)
        dept = payload["data"]["id"]
        _STATE["dept_id"] = dept
        say(f"# 库里没有部门，已用 /api/system/dept 建夹具部门 {dept}")

    # 制单人必须是**别人**：内控禁止「制单人 / 发起人审批自己的处置」（65018 职责分离）
    maker = None
    maker_id = psql("SELECT id FROM sys_user WHERE username LIKE 'lnk09_maker%' LIMIT 1")
    if not maker_id:
        super_role = psql("SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN'")
        payload, _ = call("POST", "/api/system/user", {
            "username": "lnk09_maker",
            "password": "Maker@123456",
            "real_name": "LNK09 制单人",
            "dept_id": dept,
            "status": 1,
            "role_ids": [super_role],
        }, token=admin)
        maker_id = payload["data"]["id"] if isinstance(payload.get("data"), dict) \
            else payload["data"]
        _STATE["maker_id"] = maker_id
        psql(f"UPDATE sys_user SET must_change_password = false WHERE id = '{maker_id}'::uuid")
        say(f"# 已建制单人 lnk09_maker（{maker_id}，SUPER_ADMIN 角色）")
    maker = login("lnk09_maker", "Maker@123456")
    say(f"# 制单人登录成功（token 长度 {len(maker)}）；分类 = "
        f"{category['category_code']}({category['id']})；部门 = {dept}")

    # 审批人：admin 临时挂到总经理角色（asset_disposal 的总经理节点要它；结束摘掉）
    borrowed = psql(f"SELECT count(*) FROM sys_user_role WHERE user_id = '{admin_id}'::uuid "
                    f"AND role_id = '{ROLE_GM}'::uuid")
    if borrowed == "0":
        psql(f"INSERT INTO sys_user_role (id, user_id, role_id) VALUES "
             f"(gen_random_uuid(), '{admin_id}'::uuid, '{ROLE_GM}'::uuid)")
        _STATE["borrowed_role"] = True
        say("# admin 临时挂到 GENERAL_MANAGER（审批人要非制单人；结束时摘掉）")

    payload, _ = call("POST", "/api/business/erp/asset", {
        "asset_name": "LNK09 真机取证笔记本",
        "category_id": category["id"],
        "use_dept_id": dept,
        "original_value": 100000,
        "tax_amount": 0,
        "purchase_date": PURCHASE_DATE,
        "remark": "LNK-09 真机取证（脚本自建自清）",
    }, token=maker)
    card = payload["data"]
    asset_id = card["id"]
    _STATE["asset_id"] = asset_id
    say(f"# 卡片已建：id={asset_id} no={card['no']} status={asset_status(asset_id)}")

    # ---------- ④ 提交处置审批 ----------
    say()
    say("## ④ POST /api/business/erp/asset/{id}/dispose —— 提交处置审批（IN_USE → 待审批）")
    payload, _ = call("POST", f"/api/business/erp/asset/{asset_id}/dispose", {
        "disposal_type": "SALE",
        "disposal_date": DISPOSE_DATE,
        "reason": "LNK-09 真机取证：闲置设备出售",
        "sale_price": 50000,
        "output_tax": 6500,
    }, token=maker)
    say(f"# 提交后卡片状态 = {asset_status(asset_id)}（审批未通过前必须仍在役）")
    say(f"# 处置通知（此刻）= {notice_count('DISPOSED', asset_id)}（提交不是触发点）")

    # ---------- ⑤ 审批通过：asset_bridge::execute 调用点 ----------
    say()
    say("## ⑤ POST /api/business/flow/task/{id}/approve —— 审批通过（asset_bridge::execute ⑤ 号调用点）")
    task = psql("SELECT t.id FROM biz_flow_task t JOIN biz_flow_instance i ON i.id = t.instance_id "
                f"WHERE i.biz_id = '{asset_id}'::uuid AND t.status = 'PENDING' LIMIT 1")
    if not task:
        say("!! 提交后没有待办（流程配置异常）")
        raise SystemExit(1)
    _STATE["instance"] = psql(f"SELECT instance_id FROM biz_flow_task WHERE id = '{task}'::uuid")
    payload, _ = call("POST", f"/api/business/flow/task/{task}/approve",
                      {"comment": "同意（LNK-09 真机取证）"}, token=admin)
    say(f"# 审批结果：{json.dumps(payload.get('data'), ensure_ascii=False)[:300]}")
    status = asset_status(asset_id)
    n = notice_count("DISPOSED", asset_id)
    vouchers = disposal_vouchers(asset_id)
    say(f"# 卡片状态 = {status}；sys_notice[LNK:FIXED_ASSET:DISPOSED:{asset_id}] = {n}；"
        f"处置凭证 = {vouchers} 张")
    if status != "DISPOSED" or n != "1" or vouchers == "0":
        say("!! 审批通过后的状态 / 通知 / 凭证不符合预期")
        raise SystemExit(1)

    # ---------- ⑥ 重复 approve：幂等重放，不许再触发业务联动 ----------
    say()
    say("## ⑥ 再次 POST 同一条待办 /approve —— 幂等重放（重复触发抑制）")
    payload, http = call("POST", f"/api/business/flow/task/{task}/approve",
                         {"comment": "重复点一次（LNK-09 取证）"}, token=admin, expect=None)
    data = payload.get("data") or {}
    idem = data.get("idempotent")
    say(f"# http={http} code={payload.get('code')} idempotent={idem} "
        f"{json.dumps(data, ensure_ascii=False)[:200]}")
    still = asset_status(asset_id)
    n_again = notice_count("DISPOSED", asset_id)
    v_again = disposal_vouchers(asset_id)
    say(f"# 重复 approve 之后：卡片状态 = {still}，DISPOSED 通知 = {n_again}（必须仍是 1），"
        f"处置凭证 = {v_again}（不许翻倍）")
    if still != "DISPOSED" or n_again != "1" or v_again != vouchers:
        say("!! 重复审批把处置又跑了一遍（通知 / 凭证翻倍）")
        raise SystemExit(1)

    # ---------- ⑦ 作废处置：asset_dispose_cancel 调用点 ----------
    say()
    say("## ⑦ POST /api/business/erp/asset/{id}/dispose/cancel —— 作废处置（红冲 + 恢复在役）")
    payload, _ = call("POST", f"/api/business/erp/asset/{asset_id}/dispose/cancel",
                      {"reason": "LNK-09 真机取证：处置依据不成立，作废处置"},
                      token=admin)
    status = asset_status(asset_id)
    n_restore = notice_count("IN_USE", asset_id)
    reds = psql("SELECT count(*) FROM biz_voucher WHERE source_type = 'FIXED_ASSET_DISPOSAL' "
                f"AND source_id = '{asset_id}'::uuid AND deleted_at IS NULL")
    say(f"# 卡片状态 = {status}；sys_notice[LNK:FIXED_ASSET:IN_USE:{asset_id}] = {n_restore}；"
        f"处置凭证 = {reds}（3 蓝 + 3 红；红冲是新增红字凭证，不删蓝字）")
    if status != "IN_USE" or n_restore != "1":
        say("!! 作废处置的状态 / 通知不符合预期")
        raise SystemExit(1)

    say()
    say("## ALL PASS：FIXED_ASSET 处置 / 作废处置两处状态推进都真的触发联动，"
        "重复审批不重复触发，凭证没有翻倍")


try:
    main()
finally:
    with open(OUT, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")
