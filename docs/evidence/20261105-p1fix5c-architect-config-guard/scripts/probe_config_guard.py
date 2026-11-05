#!/usr/bin/env python3
"""P1-FIX-5C（architect / t_21dc81c3）独立验收探针：配置面「防误配」硬门。

与 t_befea97a 的 `p1fix5c_e2e.py`（接线侧端到端）**互不重叠**：本脚本只回答一个问题——
「未接线的触发点还能不能在被静默保存？」以及「下发给配置页的 wired 到底是不是 3/15」。

用法（隔离实例必须已起在 LNK_PORT）：
    LNK_PORT=18132 python3 probe_config_guard.py
"""
import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("LNK_BASE", f"http://127.0.0.1:{os.environ.get('LNK_PORT', '18132')}")
PASSWORD = os.environ.get("LNK_ADMIN_PASSWORD", "Admin@123456")

PASS = 0
FAIL = 0


def http(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return err.code, {"raw": raw}


def check(ok, name, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"[PASS] {name}  —— {detail}")
    else:
        FAIL += 1
        print(f"[FAIL] {name}  —— {detail}")


def login():
    for _ in range(2):
        code, body = http("POST", "/api/system/auth/login", {"username": "admin", "password": PASSWORD})
        data = body.get("data") or {}
        token = data.get("token") or data.get("access_token")
        if code == 200 and token:
            return token
        if code == 200 and data.get("must_change_password"):
            os.system(
                "PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_lnk5b -Atc "
                "\"UPDATE sys_user SET must_change_password = false WHERE username = 'admin'\" >/dev/null"
            )
            continue
        print(f"登录失败 HTTP {code} {body}")
        return None
    return None


def main():
    token = login()
    if not token:
        sys.exit("无法登录，放弃")
    print(f"登录成功（{BASE}）\n")

    # ---------- A. 配置页字典：wired 标记与接线说明 ----------
    code, body = http("GET", "/api/business/flow/linkage", token=token)
    data = body.get("data") or {}
    triggers = data.get("triggers") or []
    actions = data.get("actions") or []
    wired = [t["entity"] for t in triggers if t.get("wired")]
    unwired = [t["entity"] for t in triggers if not t.get("wired")]

    check(code == 200 and len(triggers) == 15, "A1 触发点字典 15 条全部下发", f"HTTP {code} count={len(triggers)}")
    check(
        wired == ["STOCK_IN", "STOCK_OUT", "TASK"],
        "A2 已接线恰好 3 个且与 spec::TRIGGER_SPECS 一致",
        f"wired={wired}",
    )
    check(len(unwired) == 12, "A3 未接线 12 个（配了也不会触发）", f"unwired={unwired}")
    check(
        all((t.get("wiring") or "").strip() for t in triggers),
        "A4 每个触发点都下发「接线位置 / 未接线原因」（前端不自己编文案）",
        f"示例：{triggers[0]['entity']} → {(triggers[0]['wiring'] or '')[:60]}…",
    )
    stockout = next((t for t in triggers if t["entity"] == "STOCK_OUT"), {})
    check(
        stockout.get("wired") is True and "post_out_tx" in (stockout.get("wiring") or ""),
        "A5 STOCK_OUT 标注已接线并点明调用点",
        (stockout.get("wiring") or "")[:80],
    )
    for entity in unwired[:2]:
        spec = next(t for t in triggers if t["entity"] == entity)
        print(f"       · 未接线示例 {entity}: {(spec.get('wiring') or '')[:96]}…")

    implemented = [a["code"] for a in actions if a.get("implemented")]
    check(
        sorted(implemented) == sorted(["SEND_NOTICE", "CREATE_TASK", "GENERATE_VOUCHER", "UPDATE_RECEIVED_QTY"]),
        "A6 已接管动作 4 个（CREATE_TASK 已从占位转真实现，下拉可选）",
        f"implemented={implemented}",
    )

    # ---------- B. 写入口硬门：未接线触发点不许静默保存 ----------
    target = "STOCK_TRANSFER" if "STOCK_TRANSFER" in unwired else unwired[0]
    status = next(t for t in triggers if t["entity"] == target)["statuses"][0]
    payload = {
        "name": "ARCH5C-未接线静默保存探针",
        "trigger_entity": target,
        "trigger_status": status,
        "action_code": "SEND_NOTICE",
        "action_params": {},
        "enabled": True,
        "seq": 90,
        "confirm": True,
    }
    code, body = http("PUT", "/api/business/flow/linkage", payload, token=token)
    check(
        code == 400 and "尚未接线" in json.dumps(body, ensure_ascii=False),
        f"B1 在未接线触发点（{target}/{status}）上保存被 400 拒绝（不是静默成功）",
        f"HTTP {code} msg={(body.get('msg') or '')[:120]}",
    )

    payload_ack = dict(payload, allow_unwired=True)
    code, body = http("PUT", "/api/business/flow/linkage", payload_ack, token=token)
    rule = (body.get("data") or {})
    check(
        code == 200 and rule.get("id"),
        "B2 显式 allow_unwired=true（页面上第二道勾选）才允许「预置配置」",
        f"HTTP {code} id={rule.get('id')}",
    )

    if rule.get("id"):
        rule_id = rule["id"]
        # 停用再启用：启用一条挂未接线触发点的规则也必须显式确认
        http("PUT", "/api/business/flow/linkage/toggle", {"id": rule_id, "enabled": False, "confirm": True}, token)
        code, body = http(
            "PUT", "/api/business/flow/linkage/toggle", {"id": rule_id, "enabled": True, "confirm": True}, token
        )
        check(
            code == 400,
            "B3 未带 allow_unwired 的「启用」同样被拒（停用不属于变更，放行）",
            f"HTTP {code} msg={(body.get('msg') or '')[:100]}",
        )
        code, body = http("DELETE", f"/api/business/flow/linkage?id={rule_id}&confirm=true", token=token)
        check(code == 200, "B4 探针规则已清理（可重复跑）", f"HTTP {code}")

    # ---------- C. 影响预览：未接线必须 warning，不能绿条盖过 ----------
    code, body = http(
        "POST",
        "/api/business/flow/linkage/preview",
        {"trigger_entity": target, "trigger_status": status},
        token=token,
    )
    preview = body.get("data") or {}
    check(
        code == 200 and preview.get("trigger_wired") is False,
        f"C1 未接线触发点（{target}）的影响预览带 trigger_wired=false",
        f"HTTP {code} trigger_wired={preview.get('trigger_wired')}",
    )
    warnings = json.dumps(preview.get("warnings") or [], ensure_ascii=False)
    check("未接线" in warnings, "C2 预览 warnings 里点名「未接线」", warnings[:120])

    code, body = http(
        "POST",
        "/api/business/flow/linkage/preview",
        {"trigger_entity": "STOCK_IN", "trigger_status": "POSTED"},
        token=token,
    )
    preview = body.get("data") or {}
    check(
        code == 200 and preview.get("trigger_wired") is True,
        "C3 已接线触发点（STOCK_IN/POSTED）的预览 trigger_wired=true（对照组）",
        f"HTTP {code} enabled_rules={preview.get('enabled_count')}",
    )

    print(f"\nPASS={PASS} FAIL={FAIL}")
    sys.exit(0 if FAIL == 0 else 1)


if __name__ == "__main__":
    main()
