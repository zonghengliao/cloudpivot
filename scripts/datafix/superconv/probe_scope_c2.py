#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""数据范围可见行数探针（【P0-FIX-2B-C】t_db2506d2 · 验收 C2）。

只做一件事：以某个账号的身份，问应用「你现在能看到多少行任务 / 项目 / 用户」。
走的是真实接口与真实数据权限注入（`shared/scope.rs::paginate` 唯一入口），
不是拿 SQL 复算 —— 所以它能证明「摘权之后他到底还能看什么」。

用法：
    SUPERCONV_BASE=http://127.0.0.1:18097 python3 probe_scope_c2.py \
        --username laoliu_test --password '***'
    SUPERCONV_BASE=http://127.0.0.1:8023 python3 probe_scope_c2.py --token '***'

输出：`/api/system/auth/me` 的 roles + 三张受控表的 total（JSON 一行，便于对照/落档）。
退出码：0 = 全部接口 200；1 = 有接口非 200。
"""
import argparse
import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("SUPERCONV_BASE", "http://127.0.0.1:8023").rstrip("/")
TIMEOUT = 30


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
            text = resp.read().decode(errors="replace")
            return resp.status, (json.loads(text) if text.strip() else {})
    except urllib.error.HTTPError as exc:
        text = exc.read().decode(errors="replace")
        try:
            return exc.code, (json.loads(text) if text.strip() else {})
        except json.JSONDecodeError:
            return exc.code, {"raw": text[:300]}


def total_of(body):
    data = body.get("data") or {}
    if isinstance(data, dict):
        if "total" in data:
            return data["total"]
        if isinstance(data.get("list"), list):
            return len(data["list"])
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--username")
    ap.add_argument("--password")
    ap.add_argument("--token")
    ap.add_argument("--label", default="", help="写进输出，便于 before/after 对照")
    args = ap.parse_args()

    token = args.token
    if not token:
        st, body = call("POST", "/api/system/auth/login",
                        {"username": args.username, "password": args.password})
        token = (body.get("data") or {}).get("access_token") if st == 200 else None
        if not token:
            print(json.dumps({"label": args.label, "login_http": st, "login": body}, ensure_ascii=False))
            return 1

    out = {"label": args.label, "base": BASE}
    st, me = call("GET", "/api/system/auth/me", token=token)
    out["http"] = {"me": st}
    out["roles"] = (me.get("data") or {}).get("roles")
    out["perms"] = len(((me.get("data") or {}).get("permissions") or []))

    ok = st == 200
    for name, path in [
        ("biz_task", "/api/business/task/page?page=1&page_size=1"),
        ("biz_project", "/api/business/project/page?page=1&page_size=1"),
        ("sys_user", "/api/system/user/page?page=1&page_size=1"),
    ]:
        st, body = call("GET", path, token=token)
        out["http"][name] = st
        out[name] = total_of(body)
        ok = ok and st == 200

    print(json.dumps(out, ensure_ascii=False))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
