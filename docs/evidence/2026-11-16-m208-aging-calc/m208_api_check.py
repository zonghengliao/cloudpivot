#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""m208_api_check.py —— 库龄（M2-08）真接口断言（跑在隔离实例上）。

前置：实例已在 127.0.0.1:18086 起来（profile=test，库=演练库），夹具已灌
（scripts/verify/m208_inventory_aging_fixture.sql），同目录有自签超管 token（token.txt）。

断言点（每条都会打印 PASS/FAIL，末尾汇总；FAIL>0 退 1）：
  ① 200 + 分页形状（list/total/page/page_size）
  ② FIFO 分层：默认 2 行（A 层吃满不返回），B 剩 120 / C 剩 50
  ③ 库龄：B = 32 天（11-16 − 10-15）、C = 6 天（11-16 − 11-10）；as_of 回显
  ④ 名字批量补齐：物料编码/名称 + 仓库编码/名称 + 单位（约定 13）
  ⑤ 批号来自入库单明细（M208H-LOT-B / M208H-LOT-C）
  ⑥ 数量口径自洽：qty_in = qty_consumed + qty_remaining；amount_remaining = 剩余 × 单价
  ⑦ include_consumed=true → 3 行，A 层剩 0 / 已消耗 100
  ⑧ as_of=2026-10-31 → C 层不出现（业务日期晚于基准日），B 仍剩 120（补录出库算进来）
  ⑨ as_of=2026-10-18 → A 100 + B 200（出库还没发生）
  ⑩ 库龄区间筛选：min_age_days=10 → 只剩 B；max_age_days=10 → 只剩 C
  ⑪ 批号筛选：lot_no=M208H-LOT-C → 1 行
  ⑫ 分页：page_size=1 第一页 B、第二页 C，total 恒为 2
  ⑬ 非法参数 → 400 + 66006（material_id 非 UUID / as_of 格式 / min>max / include_consumed 不认识）
  ⑭ 超管 200（无 401/403）+ 响应体是 JSON（不是 SPA fallback 的 HTML）
"""
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18086"
TOKEN = open("/root/.hermes/profiles/backend-developer/cache/scratch/m208inst/token.txt").read().strip()
MATERIAL = "20800000-0000-4000-8000-000000000003"
WAREHOUSE = "20800000-0000-4000-8000-000000000002"

passed = 0
failed = 0


def ok(msg):
    global passed
    passed += 1
    print(f"  PASS: {msg}")


def bad(msg):
    global failed
    failed += 1
    print(f"  FAIL: {msg}")


def get(path, expect_status=200):
    req = urllib.request.Request(BASE + path, headers={"Authorization": f"Bearer {TOKEN}"})
    try:
        with urllib.request.urlopen(req) as resp:
            body = resp.read().decode()
            return resp.status, body
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()


def call(query, expect_status=200):
    status, body = get(f"/api/business/erp/inventory-aging/page?{query}", expect_status)
    try:
        payload = json.loads(body)
    except json.JSONDecodeError:
        payload = None
    return status, payload, body


def base_query(as_of="2026-11-16", extra="", page=1, page_size=50):
    return (
        f"material_id={MATERIAL}&warehouse_id={WAREHOUSE}&as_of={as_of}"
        f"&page={page}&page_size={page_size}{extra}"
    )


def dec(text):
    from decimal import Decimal

    return Decimal(str(text))


print("== ① 分页形状 + 超管可取（200 / JSON）")
status, payload, body = call(base_query())
if status == 200 and payload and payload.get("code") == 0:
    ok(f"HTTP 200 + code=0（body 是 JSON，长度 {len(body)}）")
else:
    bad(f"HTTP {status}，body 前缀：{body[:200]}")
    sys.exit(1)
data = payload["data"]
for field in ("list", "total", "page", "page_size"):
    if field in data:
        ok(f"分页字段 {field} 在位（{field}={data[field] if field != 'list' else len(data['list'])}）")
    else:
        bad(f"分页字段缺 {field}")
rows = data["list"]

print("== ② FIFO 分层（默认只看有结存：A 吃满不返回）")
if len(rows) == 2:
    ok("默认 2 行（A 层已吃满，不返回）")
else:
    bad(f"应为 2 行，实得 {len(rows)}")
if [r["doc_no"] for r in rows] == ["M208H-IN-B", "M208H-IN-C"]:
    ok("顺序最老在前：B（10-15）→ C（11-10）")
else:
    bad(f"顺序不对：{[r['doc_no'] for r in rows]}")
if dec(rows[0]["qty_in"]) == 200 and dec(rows[0]["qty_consumed"]) == 80 and dec(rows[0]["qty_remaining"]) == 120:
    ok("B 层：入库 200 / 已消耗 80 / 剩余 120")
else:
    bad(f"B 层数量不对：{rows[0]}")
if dec(rows[1]["qty_remaining"]) == 50 and dec(rows[1]["qty_consumed"]) == 0:
    ok("C 层：入库 50 / 已消耗 0 / 剩余 50")
else:
    bad(f"C 层数量不对：{rows[1]}")

print("== ③ 库龄（as_of − 入库日期，业务日期不是过账时间）")
if rows[0]["age_days"] == 32:
    ok("B 层库龄 32 天（2026-11-16 − 2026-10-15）")
else:
    bad(f"B 层库龄 = {rows[0]['age_days']}（应 32）")
if rows[1]["age_days"] == 6:
    ok("C 层库龄 6 天（2026-11-16 − 2026-11-10，且过账 11-14 不影响）")
else:
    bad(f"C 层库龄 = {rows[1]['age_days']}（应 6）")
if all(r["as_of"] == "2026-11-16" for r in rows):
    ok("as_of 回显在每一行")
else:
    bad("as_of 没回显")

print("== ④ 名字批量补齐（AGENTS 约定 13）")
row = rows[0]
if row["material_code"] == "M208H-M" and row["material_name"] == "库龄验收物料":
    ok("物料编码/名称补齐")
else:
    bad(f"物料名字不对：{row['material_code']} / {row['material_name']}")
if row["warehouse_code"] == "M208H-WH" and row["warehouse_name"] == "库龄验收仓":
    ok("仓库编码/名称补齐")
else:
    bad(f"仓库名字不对：{row['warehouse_code']} / {row['warehouse_name']}")
if row.get("unit_name") == "件":
    ok("计量单位补齐（件）")
else:
    bad(f"单位名称 = {row.get('unit_name')}")

print("== ⑤ 批号来自入库单明细")
if [r["lot_no"] for r in rows] == ["M208H-LOT-B", "M208H-LOT-C"]:
    ok("批号：M208H-LOT-B / M208H-LOT-C")
else:
    bad(f"批号不对：{[r['lot_no'] for r in rows]}")

print("== ⑥ 数量口径自洽")
for item in rows:
    if dec(item["qty_in"]) == dec(item["qty_consumed"]) + dec(item["qty_remaining"]):
        ok(f"{item['doc_no']}：入库 = 已消耗 + 剩余")
    else:
        bad(f"{item['doc_no']}：数量不自洽 {item}")
    if dec(item["amount_remaining"]) == dec(item["qty_remaining"]) * dec(item["unit_cost"]):
        ok(f"{item['doc_no']}：参考金额 = 剩余 × 层单价")
    else:
        bad(f"{item['doc_no']}：金额不对 {item['amount_remaining']}")

print("== ⑦ include_consumed=true 才翻出吃满的层")
status, payload, _ = call(base_query(extra="&include_consumed=true"))
rows3 = payload["data"]["list"]
if len(rows3) == 3 and rows3[0]["doc_no"] == "M208H-IN-A":
    ok("3 行，A 层排最前")
else:
    bad(f"应有 3 行且 A 最前，实得 {[r['doc_no'] for r in rows3]}")
if dec(rows3[0]["qty_remaining"]) == 0 and dec(rows3[0]["qty_consumed"]) == 100:
    ok("A 层：剩余 0 / 已消耗 100")
else:
    bad(f"A 层数量不对：{rows3[0]}")
if rows3[0]["age_days"] == 76:
    ok("A 层库龄 76 天")
else:
    bad(f"A 层库龄 = {rows3[0]['age_days']}")

print("== ⑧ as_of=2026-10-31（期末快照按业务日期切）")
status, payload, _ = call(base_query(as_of="2026-10-31", extra="&include_consumed=true"))
rows_1031 = payload["data"]["list"]
if [r["doc_no"] for r in rows_1031] == ["M208H-IN-A", "M208H-IN-B"]:
    ok("C 层（业务日期 11-10）不出现")
else:
    bad(f"10-31 行集不对：{[r['doc_no'] for r in rows_1031]}")
if dec(rows_1031[1]["qty_remaining"]) == 120:
    ok("B 层剩 120（出库业务日期 10-20 已算进来，尽管过账是 11-12）")
else:
    bad(f"10-31 B 层剩余 = {rows_1031[1]['qty_remaining']}")

print("== ⑨ as_of=2026-10-18（出库还没发生）")
status, payload, _ = call(base_query(as_of="2026-10-18"))
rows_1018 = payload["data"]["list"]
if [dec(r["qty_remaining"]) for r in rows_1018] == [100, 200]:
    ok("A 100 + B 200（两层都满）")
else:
    bad(f"10-18 剩余不对：{[r['qty_remaining'] for r in rows_1018]}")

print("== ⑩ 库龄区间筛选")
status, payload, _ = call(base_query(extra="&min_age_days=10"))
if [r["doc_no"] for r in payload["data"]["list"]] == ["M208H-IN-B"]:
    ok("min_age_days=10 → 只剩 B（32 天）")
else:
    bad(f"min_age_days=10 结果：{[r['doc_no'] for r in payload['data']['list']]}")
status, payload, _ = call(base_query(extra="&max_age_days=10"))
if [r["doc_no"] for r in payload["data"]["list"]] == ["M208H-IN-C"]:
    ok("max_age_days=10 → 只剩 C（6 天）")
else:
    bad(f"max_age_days=10 结果：{[r['doc_no'] for r in payload['data']['list']]}")

print("== ⑪ 批号筛选")
status, payload, _ = call(base_query(extra="&lot_no=M208H-LOT-C"))
if [r["doc_no"] for r in payload["data"]["list"]] == ["M208H-IN-C"]:
    ok("lot_no=M208H-LOT-C → 只剩 C")
else:
    bad(f"批号筛选结果：{[r['doc_no'] for r in payload['data']['list']]}")

print("== ⑫ 分页（page_size=1）")
status, p1, _ = call(base_query(page=1, page_size=1))
status, p2, _ = call(base_query(page=2, page_size=1))
if p1["data"]["total"] == 2 and len(p1["data"]["list"]) == 1 and p1["data"]["list"][0]["doc_no"] == "M208H-IN-B":
    ok("第 1 页：total=2、1 行、B 层")
else:
    bad(f"第 1 页不对：{p1['data']}")
if p2["data"]["list"][0]["doc_no"] == "M208H-IN-C":
    ok("第 2 页接上 C 层（不漏不重）")
else:
    bad(f"第 2 页不对：{p2['data']}")

print("== ⑬ 非法参数 → 400 + 66006（不静默忽略）")
for query, label in [
    ("material_id=not-a-uuid", "material_id 非 UUID"),
    ("as_of=2026/11/16", "as_of 格式错"),
    ("min_age_days=60&max_age_days=30", "库龄区间反了"),
    ("include_consumed=treu", "布尔开关不认识"),
    ("min_age_days=-1", "负库龄界"),
]:
    # 只带这一个坏参数（重复同名参数会让 serde 报另一种错，验不到我们的 66006）
    status, payload, _ = call("page=1&page_size=10&" + query)
    if status == 400 and payload and payload.get("code") == 66006:
        ok(f"{label} → 400 + 66006")
    else:
        bad(f"{label} → HTTP {status} code={payload and payload.get('code')}")

print("== ⑭ 无令牌 → 401（权限闸真的挂着）")
try:
    req = urllib.request.Request(BASE + f"/api/business/erp/inventory-aging/page?{base_query()}")
    with urllib.request.urlopen(req) as resp:
        code = resp.status
except urllib.error.HTTPError as error:
    code = error.code
if code == 401:
    ok("无令牌 → 401（不是 200）")
else:
    bad(f"无令牌返回 {code}")

print()
print(f"================ M2-08 真接口断言：PASS={passed} FAIL={failed} ================")
print("ALL PASS" if failed == 0 else "HAS FAILURES")
sys.exit(1 if failed else 0)
