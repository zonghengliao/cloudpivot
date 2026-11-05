#!/usr/bin/env python3
"""联动规则配置面真机 HTTP 验收（卡 t_d0daa4f0 / P1-FIX-5A）。

隔离实例 127.0.0.1:18121（库 cloudpivot_lnkfe）。全部走真实 HTTP + 真实令牌：
登录 → 列表 → 预览 → 新增两条 → 启停 → 草稿预览 → 非法用例（自由动作 / SQL 参数 / 无二次确认）→ 删除。

输出：每条 PASS/FAIL 与后端原文，末尾汇总；退出码 = FAIL 条数。
"""
import json
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18121"
OUT = []


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def check(name, ok, detail):
    OUT.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + " :: " + str(detail)[:600])
    return ok


def login(password):
    return call("POST", "/api/system/auth/login", {"username": "admin", "password": password})


def main():
    initial = "LinkFe@123456"
    changed = "LinkFe@654321"
    status, data = login(initial)
    token = (data.get("data") or {}).get("access_token")
    if status != 200:
        # 上一轮已经改过密码（脚本可重入）
        status, data = login(changed)
        token = (data.get("data") or {}).get("access_token")
    if not check("login", status == 200 and bool(token), f"status={status} token={'yes' if token else 'no'}"):
        return 1

    # 首登强制改密：本隔离库的 admin 是 bootstrap 种的初始口令
    status, data = call(
        "PUT",
        "/api/system/auth/password",
        {"old_password": initial, "new_password": changed},
        token,
    )
    if status == 200:
        print("PASS 首登改密 :: 初始口令已改为验收口令")
        status, data = login(changed)
        token = (data.get("data") or {}).get("access_token")
        if not check("重新登录", status == 200 and bool(token), f"status={status}"):
            return 1
    else:
        print(f"INFO 首登改密跳过 :: status={status} msg={data.get('msg')}")

    # ⓪ 复位：清掉上一轮遗留的规则（脚本可重入；两轮验收的起点必须一致）
    status, data = call("GET", "/api/business/flow/linkage", token=token)
    existing = (data.get("data") or {}).get("rules") or []
    for item in existing:
        call("DELETE", f"/api/business/flow/linkage?id={item['id']}&confirm=true", token=token)
    if existing:
        print(f"INFO reset :: 清掉上一轮遗留规则 {len(existing)} 条")

    # ① 列表（0 行 + 字典）
    status, data = call("GET", "/api/business/flow/linkage", token=token)
    body = data.get("data") or {}
    check("list 初始 0 行", status == 200 and body.get("total") == 0, f"status={status} total={body.get('total')}")
    check(
        "字典下发 triggers(15)/actions(11)/forbidden_keys(12)",
        len(body.get("triggers") or []) == 15
        and len(body.get("actions") or []) == 11
        and len(body.get("forbidden_keys") or []) == 12,
        f"triggers={len(body.get('triggers') or [])} actions={len(body.get('actions') or [])} forbidden={len(body.get('forbidden_keys') or [])}",
    )
    implemented = [item["code"] for item in body.get("actions") or [] if item.get("implemented")]
    check("已接管动作 = SEND_NOTICE/UPDATE_RECEIVED_QTY/GENERATE_VOUCHER", sorted(implemented) == ["GENERATE_VOUCHER", "SEND_NOTICE", "UPDATE_RECEIVED_QTY"], implemented)
    send_notice = next((item for item in body.get("actions") or [] if item["code"] == "SEND_NOTICE"), None)
    check(
        "SEND_NOTICE 参数声明（mode/title/content/force/recipient）",
        send_notice is not None and [p["key"] for p in send_notice["params"]] == ["mode", "title", "content", "force", "recipient"],
        [p["key"] for p in send_notice["params"]] if send_notice else None,
    )

    # ② 空触发点预览
    status, data = call("POST", "/api/business/flow/linkage/preview", {"trigger_entity": "STOCK_IN", "trigger_status": "POSTED"}, token)
    body = data.get("data") or {}
    check(
        "空触发点预览：0 步 + 有提醒",
        status == 200 and body.get("enabled_count") == 0 and bool(body.get("warnings")),
        f"status={status} enabled={body.get('enabled_count')} warnings={body.get('warnings')}",
    )

    # ③ 新增（SEND_NOTICE）
    status, data = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": "收货过账后通知仓管",
            "trigger_entity": "STOCK_IN",
            "trigger_status": "POSTED",
            "action_code": "SEND_NOTICE",
            "action_params": {"mode": "TAKEOVER", "title": "收货入库单已过账"},
            "enabled": True,
            "seq": 10,
            "confirm": True,
        },
        token,
    )
    rule_a = data.get("data") or {}
    check(
        "新增 SEND_NOTICE 规则",
        status == 200 and rule_a.get("action_code") == "SEND_NOTICE" and rule_a.get("seq") == 10,
        f"status={status} id={rule_a.get('id')} msg={data.get('msg')}",
    )

    status, data = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": "收货过账生成凭证",
            "trigger_entity": "STOCK_IN",
            "trigger_status": "POSTED",
            "action_code": "GENERATE_VOUCHER",
            "action_params": {"biz_type": "STOCK_IN", "mode": "SHADOW"},
            "enabled": True,
            "seq": 20,
            "confirm": True,
        },
        token,
    )
    rule_b = data.get("data") or {}
    check("新增 GENERATE_VOUCHER 规则", status == 200 and rule_b.get("action_code") == "GENERATE_VOUCHER", f"status={status} msg={data.get('msg')}")

    # ④ 列表 2 行 + 排序（seq 升序）
    status, data = call("GET", "/api/business/flow/linkage", token=token)
    body = data.get("data") or {}
    codes = [item["action_code"] for item in body.get("rules") or []]
    check("列表 2 行且按 seq 排序", status == 200 and codes == ["SEND_NOTICE", "GENERATE_VOUCHER"], codes)

    # ⑤ 触发点预览：执行链 + 回滚风险
    status, data = call("POST", "/api/business/flow/linkage/preview", {"trigger_entity": "STOCK_IN", "trigger_status": "POSTED"}, token)
    body = data.get("data") or {}
    check(
        "预览执行链 2 步 + rollback_risk=true + after_commit=1",
        status == 200
        and len(body.get("steps") or []) == 2
        and body.get("rollback_risk") is True
        and body.get("after_commit_count") == 1,
        f"steps={len(body.get('steps') or [])} risk={body.get('rollback_risk')} after={body.get('after_commit_count')}",
    )

    # ⑥ 草稿校验：合法 / 未接管 / 参数字典外的键
    status, data = call(
        "POST",
        "/api/business/flow/linkage/preview",
        {
            "trigger_entity": "STOCK_IN",
            "trigger_status": "POSTED",
            "draft": {"action_code": "SEND_NOTICE", "action_params": {"mode": "TAKEOVER"}},
        },
        token,
    )
    draft = (data.get("data") or {}).get("draft") or {}
    check("草稿校验通过 + overrides_existing=true", status == 200 and draft.get("ok") is True and draft.get("overrides_existing") is True, f"status={status} draft={draft}")

    status, data = call(
        "POST",
        "/api/business/flow/linkage/preview",
        {"trigger_entity": "STOCK_IN", "trigger_status": "POSTED", "draft": {"action_code": "LOCK_DOCUMENT"}},
        token,
    )
    draft = (data.get("data") or {}).get("draft") or {}
    check("草稿校验拦未接管动作", status == 200 and draft.get("ok") is False and "尚未接管" in (draft.get("message") or ""), f"ok={draft.get('ok')} msg={draft.get('message')}")

    # ⑦ 非法用例：动作码不在白名单（等价「自由输入动作」）
    status, data = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": "自由动作",
            "trigger_entity": "STOCK_IN",
            "trigger_status": "POSTED",
            "action_code": "DROP TABLE",
            "action_params": {},
            "confirm": True,
        },
        token,
    )
    check("非白名单动作 400", status in (400, 422) and "不在白名单" in (data.get("msg") or ""), f"status={status} msg={data.get('msg')}")

    # ⑧ 非法用例：参数里带 sql 键
    status, data = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": "SQL 参数",
            "trigger_entity": "STOCK_IN",
            "trigger_status": "POSTED",
            "action_code": "SEND_NOTICE",
            "action_params": {"mode": "TAKEOVER", "sql": "delete from biz_voucher"},
            "confirm": True,
        },
        token,
    )
    check("参数带 sql 键 400", status in (400, 422), f"status={status} msg={data.get('msg')}")

    # ⑨ 非法用例：缺二次确认
    status, data = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": "无确认",
            "trigger_entity": "STOCK_IN",
            "trigger_status": "POSTED",
            "action_code": "SEND_NOTICE",
            "action_params": {"mode": "TAKEOVER"},
            "confirm": False,
        },
        token,
    )
    check("缺二次确认 400", status in (400, 422) and "二次确认" in (data.get("msg") or ""), f"status={status} msg={data.get('msg')}")

    # ⑩ 未知触发点 / 未知状态
    status, data = call("GET", "/api/business/flow/linkage?trigger_entity=NOT_A_DOC", token=token)
    check("未知触发点 400", status == 400, f"status={status} msg={data.get('msg')}")
    status, data = call("POST", "/api/business/flow/linkage/preview", {"trigger_entity": "STOCK_IN", "trigger_status": "NOPE"}, token)
    check("未知状态 400", status == 400, f"status={status} msg={data.get('msg')}")

    # ⑪ 启停
    rule_id = rule_a.get("id")
    status, data = call("PUT", "/api/business/flow/linkage/toggle", {"id": rule_id, "enabled": False, "confirm": True}, token)
    check("停用规则", status == 200 and (data.get("data") or {}).get("enabled") is False, f"status={status} enabled={(data.get('data') or {}).get('enabled')}")
    status, data = call("POST", "/api/business/flow/linkage/preview", {"trigger_entity": "STOCK_IN", "trigger_status": "POSTED"}, token)
    body = data.get("data") or {}
    check("停用后执行链只剩 1 步", body.get("enabled_count") == 1, f"enabled={body.get('enabled_count')}")
    status, data = call("PUT", "/api/business/flow/linkage/toggle", {"id": rule_id, "enabled": True, "confirm": True}, token)
    check("重新启用", status == 200 and (data.get("data") or {}).get("enabled") is True, f"status={status}")

    # ⑫ 删除 + 幂等 404
    status, data = call("DELETE", f"/api/business/flow/linkage?id={rule_b.get('id')}&confirm=true", token=token)
    check("删除 GENERATE_VOUCHER", status == 200, f"status={status} msg={data.get('msg')}")
    status, data = call("DELETE", f"/api/business/flow/linkage?id={rule_b.get('id')}&confirm=true", token=token)
    check("重复删除 404", status == 404, f"status={status} msg={data.get('msg')}")

    # ⑬ 审计留痕
    print("-- 审计行（biz_status_log entity_type=FLOW_LINKAGE_CFG）需在 SQL 侧核对 --")

    failed = [name for name, ok, _ in OUT if not ok]
    print(f"\nSUMMARY total={len(OUT)} pass={len(OUT) - len(failed)} fail={len(failed)}")
    if failed:
        print("FAILED: " + "; ".join(failed))
    return len(failed)


if __name__ == "__main__":
    sys.exit(main())
