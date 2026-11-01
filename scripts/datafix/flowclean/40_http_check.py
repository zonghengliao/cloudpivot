#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生产接口复核：登录 admin → GET /api/business/flow/def/page → 打印存活流程清单。
只读，不改任何数据。口令从 /etc/cloudpivot/cloudpivot.env 的 NIK_ADMIN_PASSWORD 取，不落日志。"""
import json, os, sys, urllib.request, urllib.error

BASE = os.environ.get("BASE", "http://127.0.0.1:8023")


def env_val(key):
    with open("/etc/cloudpivot/cloudpivot.env", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line.startswith(key + "="):
                return line.split("=", 1)[1].strip()
    return None


def call(path, token=None, method="GET", body=None):
    req = urllib.request.Request(BASE + path, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    data = json.dumps(body).encode() if body is not None else None
    try:
        with urllib.request.urlopen(req, data=data, timeout=20) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "{}")


pwd = env_val("NIK_ADMIN_PASSWORD")
if not pwd:
    print("FAIL: /etc/cloudpivot/cloudpivot.env 里没有 NIK_ADMIN_PASSWORD")
    sys.exit(1)

st, res = call("/api/system/auth/login", method="POST",
               body={"username": "admin", "password": pwd})
if st != 200 or res.get("code") not in (0, 200):
    print("FAIL 登录:", st, res.get("code"), res.get("msg"))
    sys.exit(1)
tok = res["data"]["access_token"]
print("登录 =", st, "code =", res.get("code"))

st, res = call("/api/business/flow/def/page?page=1&page_size=50", token=tok)
print("GET /api/business/flow/def/page =", st, "code =", res.get("code"))
items = (res.get("data") or {}).get("list") or []
print("total =", (res.get("data") or {}).get("total"), " items =", len(items))
for it in items:
    print("  %-22s %-10s default=%s v%s  %s" % (
        it.get("flow_code"), it.get("status"), it.get("is_default"),
        it.get("current_version_no"), it.get("flow_name")))

codes = sorted(it.get("flow_code") for it in items)
want = sorted(["task_accept", "stock_transfer", "stock_in_cancel",
               "payable_pay", "po_approve", "stock_in_post"])
print("PASS" if codes == want else "FAIL", "存活流程 == 6 个业务流:", codes)
