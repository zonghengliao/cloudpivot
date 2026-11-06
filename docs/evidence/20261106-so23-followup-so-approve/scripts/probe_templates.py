#!/usr/bin/env python3
"""验收 ③：默认口径的模板列表（GET /business/flow/def/templates）里必须能查到 so_approve。

用 curl 发请求（urllib 被网关当可疑 UA 挡过 403）。
"""
import json
import os
import subprocess

BASE = os.environ.get("BASE_URL", "http://127.0.0.1:18224")
USER = os.environ.get("TEST_USER", "admin")
PASS = os.environ.get("TEST_PASS", "Admin@123456")


def curl(path, token=None, body=None):
    cmd = ["curl", "-s", "-m", "30", BASE + path, "-H", "Content-Type: application/json"]
    if token:
        cmd += ["-H", "Authorization: Bearer " + token]
    if body is not None:
        cmd += ["-X", "POST", "-d", json.dumps(body, ensure_ascii=False)]
    out = subprocess.run(cmd, capture_output=True, text=True).stdout
    return json.loads(out)


login = curl("/api/system/auth/login", body={"username": USER, "password": PASS})
token = (login.get("data") or {}).get("access_token") or ""
assert token, f"登录失败：{login}"

for scope, label in ((None, "默认口径（Standard：AND d.is_system）"),
                     ("all", "scope=all（标准 + 客户副本）")):
    path = "/api/business/flow/def/templates" + (f"?scope={scope}" if scope else "")
    data = curl(path, token).get("data") or {}
    items = [it for g in data.get("groups") or [] for it in g.get("items") or []]
    codes = sorted(it.get("flow_code") for it in items)
    hit = [it for it in items if it.get("flow_code") == "so_approve"]
    print(f"== {label} ==")
    print(f"   total={data.get('total')} items={len(items)} groups={len(data.get('groups') or [])}")
    print(f"   flow_codes={codes}")
    if hit:
        it = hit[0]
        print(f"   OK so_approve 在列表里：flow_name={it.get('flow_name')!r} "
              f"is_system={it.get('is_system')} status={it.get('status')} "
              f"nodes={it.get('node_count')} edges={it.get('edge_count')}")
    else:
        print("   FAIL so_approve 不在列表里")
    print()
