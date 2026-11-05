#!/usr/bin/env python3
"""探针：登录 + 拉定义 + 下载模板（存到 out dir）+ 打印各 sheet 内容。"""
import json
import sys
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18140"
USER = sys.argv[2] if len(sys.argv) > 2 else "admin"
PWD = sys.argv[3] if len(sys.argv) > 3 else "Nereus792379"


def post(path, payload):
    req = urllib.request.Request(
        BASE + path,
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read())


def get_json(path, token):
    req = urllib.request.Request(BASE + path, headers={"Authorization": "Bearer " + token})
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read())


def get_bytes(path, token):
    req = urllib.request.Request(BASE + path, headers={"Authorization": "Bearer " + token})
    with urllib.request.urlopen(req, timeout=120) as resp:
        return resp.read(), dict(resp.headers)


login = post("/api/system/auth/login", {"username": USER, "password": PWD})
print("LOGIN code =", login.get("code"), login.get("msg"))
if login.get("code") != 0:
    print(json.dumps(login, ensure_ascii=False)[:500])
    sys.exit(1)
token = login["data"]["access_token"]
open("/tmp/impexp-token.txt", "w").write(token)
print("TOKEN len =", len(token))

defs = get_json("/api/business/impexp/def/list", token)
print("DEF code =", defs.get("code"))
rows = defs.get("data") or []
print("DEF COUNT =", len(rows))
for r in rows:
    print(" ", r.get("biz_type"), "|", r.get("biz_name"), "| import=", r.get("supports_import"), "export=", r.get("supports_export"), "v", r.get("template_version"), "max", r.get("max_rows"))
