#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""m210_api_check.py —— 呆滞料标记（M2-10）真接口断言（跑在隔离实例上）。

前置：
  ① 隔离实例已在 127.0.0.1:18087 起来（profile=test，库 = cp_m210_api_test，loader 补迁移到 V0426），
  ② 夹具已灌（scripts/verify/m210_inventory_slow_moving_fixture.sql），
  ③ 自签超管 token 落在 <scratch>/m210inst/token.txt、无权限 token 落在 token_noperm.txt。

断言点（每条打印 PASS/FAIL，末尾汇总；FAIL>0 退 1）：
  ① 200 + JSON（不是 SPA fallback 的 HTML）+ code=0
  ② 表头：report_code / report_name / as_of / as_of_source=QUERY / 阈值 90 / 来源 CONFIG|DEFAULT / 参数键
  ③ 响应无分页字段（呆滞清单不分页），row_limit=2000、rows_truncated=false
  ④ 物料乙 × 仓乙（N=90）：结存 120 / 呆滞 70 / 1 层 / 总 3 层 / 金额 140 / 占比 58.33% / 最大库龄 138
  ⑤ 物料甲 × 仓甲（N=90）：**不进行**（最老剩货 57 天）
  ⑥ 阈值降到 50：甲进榜（呆滞 180 / 结存 230 / 占比 78.26%），来源 QUERY
  ⑦ 边界（物料丙 × 仓甲）：N=90 → 呆滞 20（90 天整不算）；N=89 → 30；N=91 → 该格不进行但被扫过
  ⑧ 汇总口径：单物料筛选时 scanned/slow 计数与数量金额逐项对得上
  ⑨ 不变量（全量响应）：呆滞 ≤ 结存、呆滞层数 ≤ 层数、0<占比≤1、Σ行 = summary
  ⑩ 名字批量补齐（约定 13）：物料编码/名称 + 仓库编码/名称 + 单位；口径 hints 在
  ⑪ 与库龄报表口径互证：N=90 时 qty_slow == 报表 OVER_90 档数量（同一格）
  ⑫ 非法参数 4 组 → 400 + 66006，文案点名字段
  ⑬ 没有该权限码的令牌 → 403（权限闸真的挂着）；无令牌 → 401
  ⑭ as_of 留空 → as_of_source=TODAY、as_of=CST 今天
"""
import json
import sys
import urllib.error
import urllib.request
from datetime import date, datetime, timedelta, timezone

BASE = "http://127.0.0.1:18087"
SCRATCH = "/root/.hermes/profiles/backend-developer/cache/scratch/m210inst"
TOKEN = open(f"{SCRATCH}/token.txt").read().strip()
TOKEN_NOPERM = open(f"{SCRATCH}/token_noperm.txt").read().strip()
SEP = "/api/business/erp/inventory-aging/slow-moving"
REPORT_SEP = "/api/business/erp/inventory-aging/report"

MAT_A = "21000000-0000-4000-8000-000000000003"
WH_A = "21000000-0000-4000-8000-000000000002"
MAT_B = "21000000-0000-4000-8000-000000000013"
WH_B = "21000000-0000-4000-8000-000000000012"
MAT_C = "21000000-0000-4000-8000-000000000023"

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


def check(cond, msg):
    if cond:
        ok(msg)
    else:
        bad(msg)
    return bool(cond)


def get(path, token=TOKEN):
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    req = urllib.request.Request(BASE + path, headers=headers)
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, resp.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()


def fetch(path):
    status, body = get(path)
    if status != 200:
        bad(f"HTTP {status}：{path} → {body[:300]}")
        raise SystemExit(1)
    return json.loads(body)


def num(value):
    return float(value)


def find_row(payload, material, warehouse):
    for row in payload["data"]["rows"]:
        if row["material_id"] == material and row["warehouse_id"] == warehouse:
            return row
    return None


print("== ① 200 + JSON + code=0")
raw_status, raw_body = get(f"{SEP}?as_of=2026-11-16")
check(raw_status == 200, f"HTTP 200（实得 {raw_status}）")
try:
    payload = json.loads(raw_body)
    is_json = True
except json.JSONDecodeError:
    payload, is_json = {}, False
check(is_json, "响应体是 JSON（不是 SPA fallback 的 HTML）")
check(payload.get("code") == 0, f"code=0（实得 {payload.get('code')}）")

print("== ② 表头 / 阈值回显")
data = payload["data"]
check(data["report_code"] == "INVENTORY_SLOW_MOVING", f"report_code（实得 {data['report_code']}）")
check(data["report_name"] == "呆滞料清单", f"report_name（实得 {data['report_name']}）")
check(data["as_of"] == "2026-11-16", f"as_of 回显（实得 {data['as_of']}）")
check(data["as_of_source"] == "QUERY", f"as_of_source=QUERY（实得 {data['as_of_source']}）")
check(data["threshold_days"] == 90, f"缺省阈值 90 天（实得 {data['threshold_days']}）")
check(
    data["threshold_source"] in ("CONFIG", "DEFAULT"),
    f"阈值来源 CONFIG（V0426 种了 90）/ DEFAULT（实得 {data['threshold_source']}）",
)
check(
    data["threshold_config_key"] == "erp.inventory.slow-moving-days",
    f"阈值参数键（实得 {data['threshold_config_key']}）",
)

print("== ③ 不分页")
check("page" not in data and "page_size" not in data, "响应里没有 page / page_size（清单不分页）")
check(data["row_limit"] == 2000, f"row_limit=2000（实得 {data['row_limit']}）")
check(data["rows_truncated"] is False, "rows_truncated=false（3 格远未到上限）")

print("== ④ 物料乙 × 仓乙（N=90）")
row_b = find_row(payload, MAT_B, WH_B)
if check(row_b is not None, "物料乙 × 仓乙 出现在呆滞清单里"):
    check(num(row_b["qty_on_hand"]) == 120.0, f"结存 120（实得 {row_b['qty_on_hand']}）")
    check(num(row_b["qty_slow"]) == 70.0, f"呆滞 70（实得 {row_b['qty_slow']}）")
    check(row_b["slow_layer_count"] == 1, f"呆滞层数 1（实得 {row_b['slow_layer_count']}）")
    check(row_b["layer_count"] == 3, f"总层数 3（实得 {row_b['layer_count']}）")
    check(num(row_b["amount_slow"]) == 140.0, f"呆滞参考金额 140（实得 {row_b['amount_slow']}）")
    check(abs(num(row_b["slow_ratio"]) - 0.5833) < 1e-9, f"占比 0.5833（实得 {row_b['slow_ratio']}）")
    check(row_b["max_age_days"] == 138, f"最大库龄 138（实得 {row_b['max_age_days']}）")
    check(row_b["oldest_inbound_date"] == "2026-07-01", f"最老入库日（实得 {row_b['oldest_inbound_date']}）")
    check(row_b["material_code"] == "M210H-M2", f"物料编码（实得 {row_b['material_code']}）")
    check(row_b["material_name"] == "呆滞料验收物料乙", f"物料名称（实得 {row_b['material_name']}）")
    check(row_b["warehouse_code"] == "M210H-WHB", f"仓库编码（实得 {row_b['warehouse_code']}）")
    check(row_b["warehouse_name"] == "呆滞料验收仓乙", f"仓库名称（实得 {row_b['warehouse_name']}）")
    check(row_b["unit_name"] == "件", f"计量单位（实得 {row_b['unit_name']}）")

print("== ⑤ 物料甲 × 仓甲（N=90）不呆滞")
check(find_row(payload, MAT_A, WH_A) is None, "物料甲（最老剩货 57 天）不进行")

print("== ⑥ 阈值降到 50：甲进榜")
p50 = fetch(f"{SEP}?as_of=2026-11-16&days=50")["data"]
check(p50["threshold_days"] == 50, f"阈值 50（实得 {p50['threshold_days']}）")
check(p50["threshold_source"] == "QUERY", f"来源 QUERY（实得 {p50['threshold_source']}）")
row_a50 = find_row({"data": p50}, MAT_A, WH_A)
if check(row_a50 is not None, "物料甲在 N=50 时进榜"):
    check(num(row_a50["qty_slow"]) == 180.0, f"呆滞 180（实得 {row_a50['qty_slow']}）")
    check(num(row_a50["qty_on_hand"]) == 230.0, f"结存 230（实得 {row_a50['qty_on_hand']}）")
    check(abs(num(row_a50["slow_ratio"]) - 0.7826) < 1e-9, f"占比 0.7826（实得 {row_a50['slow_ratio']}）")

print("== ⑦ 边界（物料丙 × 仓甲：90 天整 / 91 天）")
row_c90 = find_row(payload, MAT_C, WH_A)
if check(row_c90 is not None, "物料丙在 N=90 时进榜（91 天那层）"):
    check(num(row_c90["qty_slow"]) == 20.0, f"呆滞 20（90 天整不算，实得 {row_c90['qty_slow']}）")
    check(num(row_c90["qty_on_hand"]) == 30.0, f"结存 30（实得 {row_c90['qty_on_hand']}）")
    check(row_c90["slow_layer_count"] == 1, f"呆滞层数 1（实得 {row_c90['slow_layer_count']}）")
    check(row_c90["max_age_days"] == 91, f"最大库龄 91（实得 {row_c90['max_age_days']}）")
p89 = fetch(f"{SEP}?as_of=2026-11-16&days=89&material_id={MAT_C}")["data"]
check(num(p89["rows"][0]["qty_slow"]) == 30.0 if p89["rows"] else False, "N=89 → 呆滞 30（90 天那层也算）")
p91 = fetch(f"{SEP}?as_of=2026-11-16&days=91&material_id={MAT_C}")["data"]
check(p91["row_count"] == 0, f"N=91 → 该格 0 呆滞行（实得 {p91['row_count']}）")
check(p91["summary"]["scanned_row_count"] == 1, f"但该格仍被扫过（实得 {p91['summary']['scanned_row_count']}）")

print("== ⑧ 汇总口径（单物料筛选）")
ps = fetch(f"{SEP}?as_of=2026-11-16&material_id={MAT_B}")["data"]
summary = ps["summary"]
check(ps["row_count"] == 1, f"1 行（实得 {ps['row_count']}）")
check(summary["scanned_row_count"] == 1, f"扫过 1 格（实得 {summary['scanned_row_count']}）")
check(summary["slow_row_count"] == 1, f"呆滞 1 格（实得 {summary['slow_row_count']}）")
check(summary["layer_count"] == 3, f"层数 3（实得 {summary['layer_count']}）")
check(summary["slow_layer_count"] == 1, f"呆滞层数 1（实得 {summary['slow_layer_count']}）")
check(num(summary["qty_on_hand"]) == 120.0, f"Σ结存 120（实得 {summary['qty_on_hand']}）")
check(num(summary["qty_slow"]) == 70.0, f"Σ呆滞 70（实得 {summary['qty_slow']}）")
check(num(summary["amount_slow"]) == 140.0, f"Σ呆滞金额 140（实得 {summary['amount_slow']}）")
check(abs(num(summary["slow_ratio"]) - 0.5833) < 1e-9, f"Σ占比 0.5833（实得 {summary['slow_ratio']}）")

print("== ⑨ 不变量（全量响应）")
inv_ok = True
row_slow_qty = 0.0
row_slow_amount = 0.0
for row in data["rows"]:
    if not (0 < num(row["qty_slow"]) <= num(row["qty_on_hand"])):
        inv_ok = False
        bad(f"呆滞数越界：{row['qty_slow']} / {row['qty_on_hand']}")
    if row["slow_layer_count"] > row["layer_count"]:
        inv_ok = False
        bad("呆滞层数 > 层数")
    if not (0 < num(row["slow_ratio"]) <= 1):
        inv_ok = False
        bad(f"占比越界：{row['slow_ratio']}")
    row_slow_qty += num(row["qty_slow"])
    row_slow_amount += num(row["amount_slow"])
check(inv_ok, "逐行不变量：0 < 呆滞 ≤ 结存、呆滞层数 ≤ 层数、0 < 占比 ≤ 1")
check(abs(row_slow_qty - num(data["summary"]["qty_slow"])) < 1e-6, f"Σ行呆滞 = summary（{row_slow_qty}）")
check(abs(row_slow_amount - num(data["summary"]["amount_slow"])) < 1e-6, "Σ行呆滞金额 = summary")
check(data["summary"]["slow_row_count"] == data["row_count"] == len(data["rows"]), "slow_row_count == row_count == len(rows)")
check(data["summary"]["scanned_row_count"] >= len(data["rows"]), "扫过格数 ≥ 呆滞行数")

print("== ⑩ hints（口径明写）")
joined = " | ".join(data["hints"])
check("批次库龄" in joined, "hints 说明「按批次库龄」判定（不是整格最后出库日）")
check("erp.inventory.slow-moving-days" in joined, "hints 给出阈值参数键（去参数页可改）")
check("参考金额" in joined, "hints 说明金额是参考金额")

print("== ⑪ 与库龄报表口径互证（N=90 ↔ OVER_90）")
aging = fetch(f"{REPORT_SEP}?as_of=2026-11-16&material_id={MAT_B}&warehouse_id={WH_B}")["data"]
over90 = 0.0
for cell in aging["rows"][0]["buckets"]:
    if cell["key"] == "OVER_90":
        over90 = num(cell["qty"])
check(over90 == 70.0, f"报表 OVER_90 = 70（实得 {over90}）")
check(over90 == num(row_b["qty_slow"]), "呆滞 qty_slow == 报表 OVER_90（同一库函数、同一基准日）")

print("== ⑫ 非法参数 → 400 + 66006")
for label, query, field in [
    ("days=abc", "days=abc", "days"),
    ("days=0", "days=0", "days"),
    ("days=99999", "days=99999", "days"),
    ("as_of=2026-13-01", "as_of=2026-13-01", "as_of"),
    ("material_id=not-a-uuid", "material_id=not-a-uuid", "material_id"),
]:
    status, body = get(f"{SEP}?{query}")
    try:
        err = json.loads(body)
    except json.JSONDecodeError:
        err = {}
    check(status == 400, f"{label} → 400（实得 {status}）")
    check(err.get("code") == 66006, f"{label} → 66006（实得 {err.get('code')}）")
    check(field in (err.get("msg") or ""), f"{label} 文案点名字段 {field}（实得 {err.get('msg')}）")

print("== ⑬ 权限闸")
status, body = get(f"{SEP}?as_of=2026-11-16", token=TOKEN_NOPERM)
check(status == 403, f"缺 business:erp:inventory-aging:slow-moving 码 → 403（实得 {status}）")
status, body = get(f"{SEP}?as_of=2026-11-16", token="")
check(status == 401, f"无令牌 → 401（实得 {status}）")

print("== ⑭ as_of 留空 → TODAY")
today = fetch(f"{SEP}")["data"]
cst_today = (datetime.now(timezone.utc) + timedelta(hours=8)).date().isoformat()
check(today["as_of_source"] == "TODAY", f"as_of_source=TODAY（实得 {today['as_of_source']}）")
check(today["as_of"] == cst_today, f"as_of = CST 今天 {cst_today}（实得 {today['as_of']}）")

print()
print(f"================ M2-10 真接口断言：PASS={passed} FAIL={failed} ================")
print("ALL PASS" if failed == 0 else "HAS FAILURES")
sys.exit(1 if failed else 0)
