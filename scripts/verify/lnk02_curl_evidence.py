#!/usr/bin/env python3
# ============================================================================
# LNK-02 真机取证：PURCHASE_ORDER 部分收货 → 联动通知真的发出来
#
# 走的是生产同款 HTTP 链路（不是直接调函数）：
#   配规则（配置页 API）→ 建采购单 → 提交 → 建带来源的收货单（收 4/10）
#   → 过账（POSTED）→ 采购单被 sync_purchase_status 推进到 PARTIAL_RECEIVED
#   → sys_notice 落一条 LNK:PURCHASE_ORDER:PARTIAL_RECEIVED:<id>
#
# 用法：python3 scripts/verify/lnk02_curl_evidence.py [base_url]
# 前置：bash scripts/verify/lnk02_start.sh（实例在 18047）+ 本机能 psql 连 cloudpivot_so44
# ============================================================================
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18047"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk02-purchase-order/curl_evidence.txt"
DB = "cloudpivot_so44"

# 种子主数据（V0137 / V0141 种入）
SUPPLIER = "d1370005-0000-4000-8000-000000000001"  # SUP-0001
WAREHOUSE = "d1370001-0000-4000-8000-000000000002"  # WH02（叶子仓）
MATERIAL = "d1370003-0000-4000-8000-000000000001"  # M-W1A-001（非一物一码）
# po_approve（V0182 种子）的「部门主管审批」走 ROLE 分派，而 DEPT_LEADER 只有超管兜底：
# 用超管自己建单会撞「制单人不可审批自己」的内控（65018）。所以取证用**采购经理**账号建单提交，
# 超管只做「建收货单 + 过账」——两条内控都不碰。
BUYER = {"username": "lnk02_buyer", "password": "Lnk02@123456", "real_name": "LNK-02 采购员"}
ROLE_PURCHASE_MANAGER = "20000000-0000-4000-8000-0000000f0002"

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
        with urllib.request.urlopen(req, timeout=30) as resp:
            payload = json.loads(resp.read().decode())
    except urllib.error.HTTPError as error:  # 4xx/5xx 也要看到报文
        payload = json.loads(error.read().decode())
    code = payload.get("code")
    if expect is not None and code != expect:
        say(f"!! {method} {path} 期望 code={expect}，实际 {json.dumps(payload, ensure_ascii=False)[:600]}")
        raise SystemExit(1)
    return payload


def psql(sql):
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-Atc", sql],
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
        payload = call(
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


def ensure_buyer(admin_token):
    """建一个采购经理账号（幂等：已存在就跳过）。用它建单，避开「制单人 = 唯一审批人」内控。"""
    payload = call("POST", "/api/system/user", {
        "username": BUYER["username"],
        "password": BUYER["password"],
        "real_name": BUYER["real_name"],
        "status": 1,
        "role_ids": [ROLE_PURCHASE_MANAGER],
    }, admin_token, expect=None)
    say(f"# 取证账号 {BUYER['username']}：code={payload.get('code')} msg={payload.get('msg')}")


def main():
    token = login()
    say(f"# 超管登录成功，token 长度 = {len(token)}")
    ensure_buyer(token)
    buyer = call("POST", "/api/system/auth/login",
                 {"username": BUYER["username"], "password": BUYER["password"]},
                 expect=0)["data"]["access_token"]
    say(f"# 采购员登录成功，token 长度 = {len(buyer)}")

    # ---------- ① 配置页配一条规则（走写入口，不绕过 API） ----------
    say()
    say("## ① PUT /api/business/flow/linkage —— PURCHASE_ORDER / PARTIAL_RECEIVED → SEND_NOTICE")
    rule = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": "LNK02 采购部分收货 → 通知制单人",
            "trigger_entity": "PURCHASE_ORDER",
            "trigger_status": "PARTIAL_RECEIVED",
            "action_code": "SEND_NOTICE",
            "action_params": {"mode": "TAKEOVER", "recipient": "CREATOR"},
            "enabled": True,
            "seq": 30,
            "confirm": True,
        },
        token,
    )["data"]
    rule_id = rule["id"]
    say(json.dumps({
        "id": rule_id,
        "trigger": rule["trigger_entity"] + "/" + rule["trigger_status"],
        "action": rule["action_code"],
        "phase": rule["phase"],
        "implemented": rule["implemented"],
        "problems": rule["problems"],
    }, ensure_ascii=False))

    # ---------- ② 触发点字典里的 wired 必须是 true ----------
    say()
    say("## ② GET /api/business/flow/linkage?trigger_entity=PURCHASE_ORDER —— 配置页显示 wired=true")
    data = call("GET", "/api/business/flow/linkage?trigger_entity=PURCHASE_ORDER", token=token)["data"]
    trigger = next(item for item in data["triggers"] if item["entity"] == "PURCHASE_ORDER")
    say(json.dumps({"entity": trigger["entity"], "wired": trigger["wired"],
                    "wiring": trigger["wiring"][:120] + "…"}, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示未接线")
        raise SystemExit(1)

    # ---------- ③ 建采购单（草稿） ----------
    say()
    say("## ③ POST /api/business/erp/purchase-order —— 建单（M-W1A-001 × 10，单价 2）")
    order = call("POST", "/api/business/erp/purchase-order", {
        "supplier_id": SUPPLIER,
        "warehouse_id": WAREHOUSE,
        "price_mode": "EXCL",
        "remark": "LNK-02 真机取证",
        "items": [{"material_id": MATERIAL, "qty": 10, "price_excl": 2, "tax_rate": 13}],
    }, buyer)["data"]
    order_id = order["order"]["id"]
    order_no = order["order"]["order_no"]
    item_id = order["items"][0]["id"]
    say(json.dumps({"id": order_id, "order_no": order_no, "status": order["order"]["status"],
                    "total_with_tax": str(order["order"]["total_with_tax"]),
                    "item_id": item_id}, ensure_ascii=False))

    # ---------- ④ 提交（DRAFT → SUBMITTED） ----------
    say()
    say("## ④ PATCH /api/business/erp/purchase-order/{id}/status  {\"status\":\"SUBMITTED\"}")
    submitted = call("PATCH", f"/api/business/erp/purchase-order/{order_id}/status",
                     {"status": "SUBMITTED"}, buyer)
    say(json.dumps({"code": submitted["code"], "status": submitted["data"]["order"]["status"]},
                   ensure_ascii=False))
    if submitted["data"]["order"]["status"] != "SUBMITTED":
        say("!! 提交后状态不是 SUBMITTED")
        raise SystemExit(1)

    # ---------- ⑤ 建带来源采购单的收货单（收 4 / 订 10） ----------
    say()
    say("## ⑤ POST /api/business/erp/stock-in —— 带来源采购单收货（4/10）")
    stock_in = call("POST", "/api/business/erp/stock-in", {
        "source_order_id": order_id,
        "supplier_id": SUPPLIER,
        "warehouse_id": WAREHOUSE,
        "remark": "LNK-02 真机取证：部分收货",
        "items": [{
            "purchase_order_item_id": item_id,
            "material_id": MATERIAL,
            "qty": 4,
            "price_excl": 2,
            "tax_rate": 13,
        }],
    }, token)["data"]["header"]
    stock_in_id = stock_in["id"]
    say(json.dumps({"id": stock_in_id, "in_no": stock_in.get("in_no"),
                    "status": stock_in.get("status"), "qty": 4}, ensure_ascii=False))

    # ---------- ⑥ 过账（POSTED）→ 采购单被推进为 PARTIAL_RECEIVED ----------
    say()
    say("## ⑥ PATCH /api/business/erp/stock-in/{id}/status  {\"status\":\"POSTED\"}")
    posted = call("PATCH", f"/api/business/erp/stock-in/{stock_in_id}/status",
                  {"status": "POSTED"}, token, expect=None)
    say(json.dumps({"code": posted.get("code"), "msg": posted.get("msg"),
                    "status": (posted.get("data") or {}).get("status")}, ensure_ascii=False))

    # 过账可能走「过账审批」（种子里的小额免审会自己走完）；最多等 20s 落到 POSTED
    deadline = time.time() + 20
    status = ""
    while time.time() < deadline:
        detail = call("GET", f"/api/business/erp/stock-in/{stock_in_id}", token=token)["data"]
        status = detail["header"]["status"]
        if status in ("POSTED", "PENDING_POST"):
            break
        time.sleep(1)
    say(f"# 收货单当前状态 = {status}")
    if status != "POSTED":
        say("!! 收货单没落到 POSTED（过账审批未走完），本次取证中止")
        raise SystemExit(1)

    # ---------- ⑦ 采购单状态 + 联动通知（落库真值） ----------
    say()
    say("## ⑦ GET /api/business/erp/purchase-order/{id} —— 状态应为 PARTIAL_RECEIVED")
    po = call("GET", f"/api/business/erp/purchase-order/{order_id}", token=token)["data"]
    say(json.dumps({"order_no": po["order_no"], "status": po["status"],
                    "total_with_tax": str(po["total_with_tax"])}, ensure_ascii=False))
    if po["status"] != "PARTIAL_RECEIVED":
        say("!! 采购单没有被推进到部分收货")
        raise SystemExit(1)

    time.sleep(1)
    say()
    say(f"## ⑧ 落库真值：sys_notice（幂等键 LNK:PURCHASE_ORDER:PARTIAL_RECEIVED:{order_id}）")
    say(psql(
        "select notice_no||' | '||title||' | '||target_type||' | '||biz_type||' | '||idempotency_key "
        "from sys_notice where idempotency_key='LNK:PURCHASE_ORDER:PARTIAL_RECEIVED:" + order_id + "'"
    ))
    say()
    say("## ⑨ 落库真值：状态日志（状态机唯一入口，PURCHASE_ORDER）")
    say(psql(
        "select from_status||'→'||to_status||' | '||coalesce(reason,'') from biz_status_log "
        "where entity_type='PURCHASE_ORDER' and entity_id='" + order_id + "'::uuid "
        "order by created_at"
    ))

    # ---------- ⑩ 收尾：删取证规则 + 作废取证单据 ----------
    say()
    say("## ⑩ 收尾：删掉取证规则（写入口 DELETE），单据留作取证（已收货单不可硬删）")
    call("DELETE", f"/api/business/flow/linkage?id={rule_id}&confirm=true", token=token, expect=0)
    say(f"# 规则 {rule_id} 已删除；采购单 {order_no} / 收货单 {stock_in_id} 保留为取证物证")

    with open(OUT, "w") as handle:
        handle.write("\n".join(lines) + "\n")
    say()
    say(f"# 取证写入 {OUT}")


if __name__ == "__main__":
    main()
