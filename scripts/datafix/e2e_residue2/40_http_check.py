#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""E2ERES2 · 生产接口复核（只读，不改任何数据）

卡 t_142317a2 的验收第 4 条：清完之后生产接口必须还正常。

断言：
  A  GET /api/business/erp/approval-flow/health  → 200 / total=8 / healthy=8 / unhealthy=0
  B  GET /api/business/flow/def/page             → 存活定义正好 11 条，且没有一条 flow_code 命中
                                                    e2e_/diag_/vrf_/d5_ 前缀（删干净了）
  C  GET /api/business/flow/task/todo            → 能正常返回（清场没把待办接口打坏）

口令从 /etc/cloudpivot/cloudpivot.env 的 NIK_ADMIN_PASSWORD 取，不落日志。
退出码：0 = 全绿；1 = 有断言不过。
"""
import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("BASE", "http://127.0.0.1:8023").rstrip("/")
ENV_FILE = os.environ.get("ENV_FILE", "/etc/cloudpivot/cloudpivot.env")

FAILED = []


def check(ok, msg):
    print(("PASS  " if ok else "FAIL  ") + msg)
    if not ok:
        FAILED.append(msg)


def env_val(key):
    with open(ENV_FILE, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line.startswith(key + "="):
                return line.split("=", 1)[1].strip().strip('"').strip("'")
    return None


def call(path, token=None, method="GET", body=None):
    req = urllib.request.Request(BASE + path, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    data = json.dumps(body).encode() if body is not None else None
    try:
        with urllib.request.urlopen(req, data=data, timeout=20) as r:
            return r.status, json.loads(r.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "{}")


pwd = env_val("NIK_ADMIN_PASSWORD")
if not pwd:
    print("FAIL: %s 里没有 NIK_ADMIN_PASSWORD" % ENV_FILE)
    sys.exit(1)

st, res = call("/api/system/auth/login", method="POST",
               body={"username": "admin", "password": pwd})
if st != 200 or res.get("code") not in (0, 200):
    print("FAIL 登录:", st, res.get("code"), res.get("msg"))
    sys.exit(1)
tok = res["data"]["access_token"]
print("登录 = %s code = %s  base = %s" % (st, res.get("code"), BASE))

# ---------------------------------------------------------------- A 审批流自检
st, res = call("/api/business/erp/approval-flow/health", token=tok)
data = res.get("data") or {}
check(st == 200 and res.get("code") in (0, 200), "A0 approval-flow/health HTTP 200（实际 %s）" % st)
check(data.get("total") == 8, "A1 total=8（实际 %s）" % data.get("total"))
check(data.get("healthy") == 8, "A2 healthy=8（实际 %s）" % data.get("healthy"))
check(data.get("unhealthy") == 0, "A3 unhealthy=0（实际 %s）" % data.get("unhealthy"))
for it in data.get("items") or []:
    print("        %-18s published=%s graph_ok=%s %s" % (
        it.get("biz_type") or it.get("flow_code"), it.get("published"),
        it.get("graph_ok"), it.get("message") or ""))

# ---------------------------------------------------------------- B 流程定义清单
st, res = call("/api/business/flow/def/page?page=1&page_size=100", token=tok)
page = res.get("data") or {}
items = page.get("list") or []
check(st == 200 and res.get("code") in (0, 200), "B0 flow/def/page HTTP 200（实际 %s）" % st)
check(page.get("total") == 11, "B1 存活流程定义 = 11 条（实际 %s）" % page.get("total"))
bad = [it.get("flow_code") for it in items
       if (it.get("flow_code") or "").startswith(("e2e_", "diag_", "diag2_", "vrf_", "d5_", "acc_", "tn_"))]
check(not bad, "B2 存活定义里没有 e2e/diag/vrf/d5 前缀（实际 %s）" % (bad or "无"))
print("        存活定义清单：")
for it in items:
    print("          %-22s %-10s v%s  %s" % (
        it.get("flow_code"), it.get("status"), it.get("current_version_no"), it.get("flow_name")))

# ---------------------------------------------------------------- C 待办接口
st, res = call("/api/business/flow/task/todo?page=1&page_size=10", token=tok)
check(st == 200 and res.get("code") in (0, 200), "C0 flow/task/todo HTTP 200（实际 %s）" % st)

print()
if FAILED:
    print("结果：%d 条断言不通过" % len(FAILED))
    for m in FAILED:
        print("  - " + m)
    sys.exit(1)
print("结果：全部 PASS（A 8/8 · B 11 条 · C 接口正常）")
sys.exit(0)
