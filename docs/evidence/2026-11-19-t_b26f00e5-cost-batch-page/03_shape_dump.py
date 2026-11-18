#!/usr/bin/env python3
"""打印归集表 / 余额表一行的**原始 JSON**（对齐前端 TS 类型用）。"""
import json
import urllib.request

BASE = "http://127.0.0.1:18243"


def call(method, path, body=None, token=""):
    req = urllib.request.Request(
        BASE + path, data=None if body is None else json.dumps(body).encode(), method=method
    )
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    with urllib.request.urlopen(req, timeout=20) as resp:
        return json.loads(resp.read().decode() or "{}")


token = call("POST", "/api/system/auth/login", {"username": "admin", "password": "Admin@123456"})["data"]["access_token"]
sheet = call("GET", "/api/business/erp/cost-collect-sheet?period_code=2026-11", token=token)["data"]
print("=== SheetRes 顶层键 ===")
print(sorted(sheet))
print("=== totals ===")
print(json.dumps(sheet["totals"], ensure_ascii=False, indent=2))
print("=== 第一行 ===")
print(json.dumps(sheet["list"][0], ensure_ascii=False, indent=2))

wip = call("GET", "/api/business/erp/cost-collect-sheet/wip-balance?period_code=2026-11", token=token)["data"]
print("=== WipBalance 顶层键 ===")
print(sorted(wip))
print("=== wip totals ===")
print(json.dumps(wip["totals"], ensure_ascii=False, indent=2))
print("=== wip 第一行 ===")
print(json.dumps(wip["list"][0], ensure_ascii=False, indent=2))
