#!/usr/bin/env python3
"""SO-03 验收：取真 token 写到文件（口令与令牌都不进对话）。"""
import json
import os
import urllib.request

BASE = os.environ.get("BASE", "http://127.0.0.1:18097")
OUT = os.environ.get("OUTDIR", os.path.expanduser("~/.cache/so03"))
os.makedirs(OUT, exist_ok=True)
TOKEN_FILE = os.path.join(OUT, "token.txt")
REFRESH_FILE = os.path.join(OUT, "refresh.txt")

INIT_PASS = "Admin@123456"
NEW_PASS = f"So03_{INIT_PASS}_new"


def call(method: str, path: str, body=None, token: str | None = None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(f"{BASE}{path}", data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        return exc.code, json.loads(exc.read().decode() or "{}")


def login(password: str):
    return call("POST", "/api/system/auth/login", {"username": "admin", "password": password})


status, body = login(NEW_PASS)
if not (body or {}).get("data", {}).get("access_token"):
    status, body = login(INIT_PASS)
token = (body or {}).get("data", {}).get("access_token") or ""
must = (body or {}).get("data", {}).get("must_change_password") is True

if must and token:
    st, res = call("PUT", "/api/system/auth/password",
                   {"old_password": INIT_PASS, "new_password": NEW_PASS}, token)
    print("改密:", st, res.get("code"), res.get("msg"))
    status, body = login(NEW_PASS)
    token = (body or {}).get("data", {}).get("access_token") or ""

if not token:
    print("FAIL 登录失败:", status, json.dumps(body, ensure_ascii=False)[:300])
    raise SystemExit(1)

with open(TOKEN_FILE, "w") as fh:
    fh.write(token)
with open(REFRESH_FILE, "w") as fh:
    fh.write((body or {}).get("data", {}).get("refresh_token") or "")

st, me = call("GET", "/api/system/auth/me", token=token)
print(f"OK token_len={len(token)} me_status={st} user={(me.get('data') or {}).get('username')}")
