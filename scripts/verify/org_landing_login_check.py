#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P0-FIX-2B-A 生产落位 · 阶段 5：真机登录与数据范围核对（生产 127.0.0.1:8023）

能做什么 / 不能做什么：
  * admin：用 /etc/cloudpivot/cloudpivot.env 里的种子口令**试一次**登录
    （口令已轮换则 401，不重试、不猜 —— 见 docs/P1-FIX-前端-form_data收口-证据.md 的既定规矩）；
    登进来后核对 /system/auth/me 的部门 = HQ，并取两张受控表的 total 与影响面核算表对账。
  * laoliu_test / cl_43870：status=0 停用，登录在**密码校验之前**就被 403 AUTH_ACCOUNT_DISABLED 拒，
    所以用一次性随机串尝试即可证明「停用号无论口令都不能登录」，全程不涉及猜测真实口令。
  * laoliu：status=1 但生产口令已轮换且无凭据 → **不尝试**（按规矩不猜生产口令），
    其数据范围由 org_landing_impact.py 按 principal.rs 同口径核算覆盖。
"""
import json
import os
import secrets
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("PROD_BASE", "http://127.0.0.1:8023")


def call(method, path, body=None, token=None, timeout=20):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            text = resp.read().decode(errors="replace")
            return resp.status, json.loads(text) if text else {}
    except urllib.error.HTTPError as exc:
        text = exc.read().decode(errors="replace")
        try:
            return exc.code, json.loads(text) if text else {}
        except json.JSONDecodeError:
            return exc.code, {"raw": text[:200]}


def show(tag, st, body):
    code = body.get("code") if isinstance(body, dict) else None
    msg = body.get("message") or body.get("msg") if isinstance(body, dict) else None
    print(f"  {tag}: http={st} code={code} msg={msg}")
    return code


print("== 生产真机（%s）==" % BASE)

admin_pw = os.environ.get("NIK_ADMIN_PASSWORD", "")
print("\n[1] admin 登录（env 种子口令，单次尝试）")
st, body = call("POST", "/api/system/auth/login", {"username": "admin", "password": admin_pw})
code = show("login", st, body)
token = (body.get("data") or {}).get("access_token") if st == 200 else None
if not token:
    print("  ✗ admin 种子口令不匹配（生产口令已轮换）→ 不做第二次尝试，按规矩不猜生产口令")
else:
    print("  ✓ 拿到 token（长度 %d）" % len(token))
    st, me = call("GET", "/api/system/auth/me", token=token)
    d = (me.get("data") or {}) if st == 200 else {}
    print("  /system/auth/me: http=%s username=%s dept=%s dept_id=%s"
          % (st, d.get("username"), d.get("dept_name") or d.get("dept_code"), d.get("dept_id")))
    print("  me 返回字段：%s" % sorted(d.keys()))
    for path, label in (("/api/system/user/page?page=1&page_size=200", "sys_user"),
                        ("/api/business/task/page?page=1&page_size=200", "biz_task")):
        st, page = call("GET", path, token=token)
        dd = page.get("data") or {}
        total = dd.get("total") if isinstance(dd, dict) else None
        rows = len(dd.get("records") or dd.get("list") or dd.get("rows") or []) if isinstance(dd, dict) else None
        print("  %s: http=%s total=%s rows=%s（影响面核算表：%s）"
              % (label, st, total, rows, {"sys_user": 9, "biz_task": 71}[label]))

print("\n[2] 停用号登录必须被拒（status 检查在密码校验之前 → 不需要真实口令）")
for user in ("laoliu_test", "cl_43870"):
    dummy = secrets.token_urlsafe(18)
    st, body = call("POST", "/api/system/auth/login", {"username": user, "password": dummy})
    show("login %s" % user, st, body)

print("\n[3] laoliu：status=1 但生产口令无凭据 → 不尝试（数据范围以 impact 核算为准）")
sys.exit(0)
