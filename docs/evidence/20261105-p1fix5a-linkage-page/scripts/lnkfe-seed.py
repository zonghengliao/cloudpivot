#!/usr/bin/env python3
"""给截图准备演示数据（幂等）：3 条跨触发点的真实规则。"""
import json
import urllib.request

BASE = "http://127.0.0.1:18121"


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode() or "{}")


login = call("POST", "/api/system/auth/login", {"username": "admin", "password": "LinkFe@654321"})
token = login["data"]["access_token"]

RULES = [
    {
        "name": "收货过账后通知仓管",
        "trigger_entity": "STOCK_IN",
        "trigger_status": "POSTED",
        "action_code": "SEND_NOTICE",
        "action_params": {"mode": "TAKEOVER", "title": "收货入库单已过账", "recipient": "OWNER"},
        "enabled": True,
        "seq": 10,
    },
    {
        "name": "收货过账生成凭证（影子对比）",
        "trigger_entity": "STOCK_IN",
        "trigger_status": "POSTED",
        "action_code": "GENERATE_VOUCHER",
        "action_params": {"biz_type": "STOCK_IN", "mode": "SHADOW"},
        "enabled": True,
        "seq": 20,
    },
    {
        "name": "任务完成后通知负责人",
        "trigger_entity": "TASK",
        "trigger_status": "DONE",
        "action_code": "SEND_NOTICE",
        "action_params": {"mode": "SHADOW", "recipient": "ASSIGNER"},
        "enabled": False,
        "seq": 0,
    },
]
for rule in RULES:
    saved = call("PUT", "/api/business/flow/linkage", {**rule, "confirm": True}, token)
    print(rule["trigger_entity"], rule["trigger_status"], rule["action_code"], "->", (saved.get("data") or {}).get("id"))

listed = call("GET", "/api/business/flow/linkage", token=token)["data"]
print("total", listed["total"])
for item in listed["rules"]:
    print(" ", item["trigger_entity"], item["trigger_status"], item["action_code"], "seq", item["seq"], "enabled", item["enabled"])
