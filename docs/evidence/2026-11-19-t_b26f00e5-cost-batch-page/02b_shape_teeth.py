#!/usr/bin/env python3
"""反证：如果按 DTO 的**字面嵌套**形状写前端类型（`totals.amounts.mat`），真跑会怎样。

`SheetTotalsRes` / `SheetRowRes` 对 `amounts` 用了 `#[serde(flatten)]`，出 JSON 是扁的。
只读 Rust 结构体、不看真响应，很容易写成 `{ amounts: {...} }` —— 编译期不报错，
运行时页脚合计恒为 0（`undefined` 被 `formatMoney` 当 0）。

本脚本故意按**错的**契约断言：它必须 FAIL。用来证明 02_http_smoke.py 的
「扁平四金额」断言不是走过场 —— 换个形状就红。
退出码：0 = 反证成立（确实抓到了错形状）；1 = 反证失败（说明冒烟断言没牙）。
"""
import json
import sys
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
totals = sheet["totals"]
row = (sheet["list"] or [{}])[0]

wrong_hits = []
if "amounts" in totals:
    wrong_hits.append("totals.amounts 竟然存在（说明后端改成嵌套了，前端类型要跟着改）")
else:
    print("[teeth] totals 里没有 amounts —— 扁平形状确认：", sorted(totals))
if "amounts" in row:
    wrong_hits.append("row.amounts 竟然存在")
else:
    print("[teeth] 行里也没有 amounts —— 扁平形状确认：", [k for k in ("mat", "labor", "oh", "total") if k in row])

print("[teeth] 按错契约取值 totals.amounts.mat =", totals.get("amounts", {}).get("mat"))
print("[teeth] 按对契约取值 totals.mat       =", totals.get("mat"))

if wrong_hits:
    print("[teeth] 反证失败（后端形状变了，前端类型必须同步）：")
    for hit in wrong_hits:
        print("   -", hit)
    sys.exit(1)

print("[teeth] 反证成立：按 DTO 字面写嵌套会拿到 None ⇒ 页脚合计恒 0，编译期无感；")
print("[teeth]          只有真跑 HTTP 才抓得到，故 cost-batch-kinds::SheetTotals 按扁平声明。")
