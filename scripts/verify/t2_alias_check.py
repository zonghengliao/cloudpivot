#!/usr/bin/env python3
"""验证 PATCH /status 接受前端草稿字段名 {status, ...}（卡面字段名是 {to, ...}）。"""
import json
import os
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18091/api"
PW = os.environ["ALUN_ADMIN_PASSWORD"]


def req(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(BASE + path, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    if token:
        request.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status, json.loads(response.read().decode())
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(name):
    status, payload = req("POST", "/system/auth/login", None, {"username": name, "password": PW})
    assert status == 200, (status, payload)
    return payload["data"]["access_token"]


token = login("t2_maker")
status, payload = req("POST", "/business/task", token, {"title": "T2走查-别名status"})
task_id = payload["data"]["id"]
print("task =", task_id)

# 1) 卡面字段名 {to}
status, payload = req("PATCH", f"/business/task/{task_id}/status", token, {"to": "DOING"})
print("卡面字段名 {to:'DOING'}      →", status, payload.get("data", {}).get("status"))
assert status == 200 and payload["data"]["status"] == "DOING"

# 2) 前端草稿字段名 {status}
status, payload = req("PATCH", f"/business/task/{task_id}/status", token, {"status": "REVIEW"})
print("前端草稿 {status:'REVIEW'}   →", status, payload.get("data", {}).get("status"))
assert status == 200 and payload["data"]["status"] == "REVIEW"

# 3) 同时传两个 → 422（歧义请求：serde duplicate field，被框架 JSON 提取器拒绝）
status, payload = req(
    "PATCH", f"/business/task/{task_id}/status", token, {"to": "DOING", "status": "DOING"}
)
print("同时传 {to,status}          →", status, "（body:", repr(payload.get("raw"))[:48], "）")
assert status == 422

print("ALIAS_OK")
