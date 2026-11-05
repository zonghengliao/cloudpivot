#!/usr/bin/env python3
"""P1-FIX-10 验收公共客户端：真 HTTP，无 mock。"""
import json
import urllib.request
import urllib.error

BASE = "http://127.0.0.1:18210/api"


class Api:
    def __init__(self, base=BASE):
        self.base = base
        self.token = None
        self.who = None

    def login(self, username, password):
        body = self.call("POST", "/system/auth/login",
                         {"username": username, "password": password},
                         auth=False, raw=True)
        self.token = body["data"]["access_token"]
        self.who = username
        return body["data"]

    def call(self, method, path, payload=None, auth=True, raw=False):
        url = self.base + path
        data = None
        headers = {"Content-Type": "application/json"}
        if payload is not None:
            data = json.dumps(payload).encode()
        if auth and self.token:
            headers["Authorization"] = "Bearer " + self.token
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                text = resp.read().decode()
                status = resp.status
        except urllib.error.HTTPError as err:
            text = err.read().decode()
            status = err.code
        try:
            body = json.loads(text) if text else None
        except json.JSONDecodeError:
            body = {"_raw": text}
        if status >= 400 or (isinstance(body, dict) and body.get("code") not in (None, 0, 200)):
            return {"_http": status, "_error": body, "_ok": False}
        if raw:
            return body
        return {"_http": status, "_ok": True, "data": (body or {}).get("data", body)}

    def ok(self, method, path, payload=None):
        res = self.call(method, path, payload)
        if not res["_ok"]:
            raise RuntimeError(f"{method} {path} 失败: {json.dumps(res, ensure_ascii=False)[:800]}")
        return res["data"]

    def try_(self, method, path, payload=None):
        return self.call(method, path, payload)
