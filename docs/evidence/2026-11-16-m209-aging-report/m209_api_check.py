#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""m209_api_check.py —— 库龄分段报表（M2-09）真接口断言（跑在隔离实例上）。

前置：
  ① 隔离实例已在 127.0.0.1:18086 起来（profile=test，库 = 演练库），
  ② 夹具已灌（scripts/verify/m209_inventory_aging_report_fixture.sql），
  ③ 自签超管 token 落在 <scratch>/m209inst/token.txt。

断言点（每条打印 PASS/FAIL，末尾汇总；FAIL>0 退 1）：
  ① 200 + JSON（不是 SPA fallback 的 HTML）+ code=0
  ② 报表头：report_code / report_name / as_of / as_of_source=QUERY / 四档表头键与中文名
  ③ 物料甲 × 仓甲：数量 230 / 2 层 / 最老 2026-09-20 / 最大库龄 57 / 加权平均 47
  ④ 四档数量：0-30=50、31-60=180、61-90=0、90+=0（被 FIFO 吃满的老层不进报表）
  ⑤ 参考金额：0-30=500、31-60=1800、行合计 2300（= 剩余 × 层单价）
  ⑥ 名字批量补齐（约定 13）：物料编码/名称 + 仓库编码/名称 + 单位
  ⑦ 物料乙 × 仓乙：数量 120 / 3 层 / 四档 20 / 0 / 30 / 70 / 加权平均 104
  ⑧ 基准日快照：甲 as_of=2026-10-01 → 300（层 C 与出库都不在）/ 90+=100 / 0-30=200
  ⑨ 空结果：甲 as_of=2025-01-01 → 0 行 + 0 合计 + 提示，不报错
  ⑩ as_of 留空 → as_of_source=TODAY、as_of=CST 今天
  ⑪ 合计恒等式：Σ档位 == Σ行 == qty_total（任意一次响应）
  ⑫ 仓库筛选：只出该仓的行（行的 warehouse_id 全部一致）
  ⑬ 非法参数 2 组 → 400 + 66006，文案点名字段
  ⑭ 无令牌 → 401（权限闸真的挂着）
  ⑮ 分页字段不存在（报表不做分页）而 rows_truncated / row_limit 在
"""
import json
import sys
import urllib.error
import urllib.request
from datetime import date, datetime, timedelta, timezone

BASE = "http://127.0.0.1:18086"
TOKEN = open("/root/.hermes/profiles/backend-developer/cache/scratch/m209inst/token.txt").read().strip()
SEP = "/api/business/erp/inventory-aging/report"

MAT_A = "20900000-0000-4000-8000-000000000003"
WH_A = "20900000-0000-4000-8000-000000000002"
MAT_B = "20900000-0000-4000-8000-000000000013"
WH_B = "20900000-0000-4000-8000-000000000012"

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
    return cond


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


def buckets_of(row):
    return {cell["key"]: cell for cell in row["buckets"]}


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

print("== ② 报表头")
data = payload["data"]
check(data["report_code"] == "INVENTORY_AGING", "report_code=INVENTORY_AGING")
check(data["report_name"] == "库存库龄分析表", "report_name=库存库龄分析表")
check(data["as_of"] == "2026-11-16", f"as_of 回显（实得 {data['as_of']}）")
check(data["as_of_source"] == "QUERY", "as_of_source=QUERY（传了基准日）")
heads = [(cell["key"], cell["label"]) for cell in data["buckets"]]
check(
    heads == [("D0_30", "0-30 天"), ("D31_60", "31-60 天"), ("D61_90", "61-90 天"), ("OVER_90", "90 天以上")],
    f"四档表头（实得 {heads}）",
)

print("== ③④⑤⑥ 物料甲 × 仓甲")
row_a = fetch(f"{SEP}?as_of=2026-11-16&material_id={MAT_A}&warehouse_id={WH_A}")
check(row_a["data"]["row_count"] == 1, f"1 行（实得 {row_a['data']['row_count']}）")
row = row_a["data"]["rows"][0]
check(num(row["qty_total"]) == 230.0, f"数量 230（实得 {row['qty_total']}）")
check(row["layer_count"] == 2, f"层数 2（吃满的老层不算，实得 {row['layer_count']}）")
check(row["oldest_inbound_date"] == "2026-09-20", f"最老入库 2026-09-20（实得 {row['oldest_inbound_date']}）")
check(row["max_age_days"] == 57, f"最大库龄 57（实得 {row['max_age_days']}）")
check(num(row["avg_age_days"]) == 47.0, f"加权平均库龄 47（实得 {row['avg_age_days']}）")
cells = buckets_of(row)
check(num(cells["D0_30"]["qty"]) == 50.0, f"0-30 = 50（实得 {cells['D0_30']['qty']}）")
check(num(cells["D31_60"]["qty"]) == 180.0, f"31-60 = 180（实得 {cells['D31_60']['qty']}）")
check(num(cells["D61_90"]["qty"]) == 0.0, "61-90 = 0")
check(num(cells["OVER_90"]["qty"]) == 0.0, "90 天以上 = 0（老层已被 FIFO 吃满）")
check(num(cells["D0_30"]["amount"]) == 500.0, f"0-30 参考金额 500（实得 {cells['D0_30']['amount']}）")
check(num(cells["D31_60"]["amount"]) == 1800.0, f"31-60 参考金额 1800（实得 {cells['D31_60']['amount']}）")
check(num(row["amount_total"]) == 2300.0, f"行参考金额 2300（实得 {row['amount_total']}）")
check(row["material_code"] == "M209H-M1", f"物料编码（实得 {row['material_code']}）")
check(row["material_name"] == "库龄报表验收物料甲", f"物料名称（实得 {row['material_name']}）")
check(row["warehouse_code"] == "M209H-WHA", f"仓库编码（实得 {row['warehouse_code']}）")
check(row["warehouse_name"] == "库龄报表验收仓甲", f"仓库名称（实得 {row['warehouse_name']}）")
check(row["unit_name"] == "件", f"单位名（实得 {row['unit_name']}）")

print("== ⑦ 物料乙 × 仓乙（四档各占一层）")
row_b = fetch(f"{SEP}?as_of=2026-11-16&material_id={MAT_B}&warehouse_id={WH_B}")
row = row_b["data"]["rows"][0]
cells = buckets_of(row)
check(num(row["qty_total"]) == 120.0, f"数量 120（实得 {row['qty_total']}）")
check(row["layer_count"] == 3, f"层数 3（实得 {row['layer_count']}）")
check(num(cells["D0_30"]["qty"]) == 20.0, "0-30 = 20")
check(num(cells["D61_90"]["qty"]) == 30.0, "61-90 = 30")
check(num(cells["OVER_90"]["qty"]) == 70.0, "90 天以上 = 70")
check(num(row["avg_age_days"]) == 104.0, f"加权平均 104（实得 {row['avg_age_days']}）")
check(row["max_age_days"] == 138, f"最大库龄 138（实得 {row['max_age_days']}）")
check(num(row["amount_total"]) == 240.0, f"参考金额 240（实得 {row['amount_total']}）")

print("== ⑧ 基准日快照（业务日期切）")
early = fetch(f"{SEP}?as_of=2026-10-01&material_id={MAT_A}&warehouse_id={WH_A}")["data"]
row = early["rows"][0]
cells = buckets_of(row)
check(num(row["qty_total"]) == 300.0, f"2026-10-01：300（层 C 与出库都不在，实得 {row['qty_total']}）")
check(row["layer_count"] == 2, f"2 层（实得 {row['layer_count']}）")
check(num(cells["OVER_90"]["qty"]) == 100.0, "90 天以上 = 100（06-01 那层，122 天）")
check(num(cells["D0_30"]["qty"]) == 200.0, "0-30 = 200（09-20 那层，11 天）")

print("== ⑨ 空结果不是出错")
empty = fetch(f"{SEP}?as_of=2025-01-01&material_id={MAT_A}&warehouse_id={WH_A}")["data"]
check(empty["row_count"] == 0 and empty["rows"] == [], "0 行")
check(num(empty["qty_total"]) == 0.0, "合计 0")
check(any("没有还剩结存的库龄层" in hint for hint in empty["hints"]), "有「没有数据」的口径提示")

print("== ⑩ as_of 留空 → TODAY")
today_report = fetch(SEP)["data"]
cst_today = (datetime.now(timezone.utc) + timedelta(hours=8)).date().isoformat()
check(today_report["as_of_source"] == "TODAY", f"as_of_source=TODAY（实得 {today_report['as_of_source']}）")
check(today_report["as_of"] == cst_today, f"as_of=CST 今天 {cst_today}（实得 {today_report['as_of']}）")

print("== ⑪ 合计恒等式（全库任一响应）")
for label, blob in (("全量", data), ("甲", row_a["data"]), ("乙", row_b["data"])):
    rows_sum = sum(num(item["qty_total"]) for item in blob["rows"])
    summary_sum = sum(num(cell["qty"]) for cell in blob["summary"])
    inner_ok = all(
        abs(sum(num(cell["qty"]) for cell in item["buckets"]) - num(item["qty_total"])) < 1e-6
        for item in blob["rows"]
    )
    check(
        abs(rows_sum - summary_sum) < 1e-6 and abs(summary_sum - num(blob["qty_total"])) < 1e-6 and inner_ok,
        f"{label}：Σ档位 {summary_sum} == Σ行 {rows_sum} == qty_total {blob['qty_total']}",
    )

print("== ⑫ 仓库筛选只出该仓")
wh_only = fetch(f"{SEP}?as_of=2026-11-16&warehouse_id={WH_A}")["data"]
check(
    all(item["warehouse_id"] == WH_A for item in wh_only["rows"]),
    f"行全属仓甲（{len(wh_only['rows'])} 行）",
)
check(wh_only["warehouse_count"] == 1, f"warehouse_count=1（实得 {wh_only['warehouse_count']}）")

print("== ⑬ 非法参数 → 400 + 66006")
for path, field in (
    (f"{SEP}?as_of=2026-11-16&material_id=not-a-uuid", "material_id"),
    (f"{SEP}?as_of=20261116", "as_of"),
):
    status, body = get(path)
    try:
        blob = json.loads(body)
    except json.JSONDecodeError:
        blob = {}
    check(
        status == 400 and blob.get("code") == 66006 and field in blob.get("msg", ""),
        f"{field} 非法 → 400 + 66006 且文案点名字段（实得 {status} / {blob.get('code')} / {blob.get('msg')}）",
    )

print("== ⑭ 无令牌 → 401")
status, _ = get(f"{SEP}?as_of=2026-11-16", token=None)
check(status == 401, f"无 token → 401（实得 {status}）")

print("== ⑮ 报表不分页，但有截断标记")
check("page" not in data and "page_size" not in data, "响应里没有 page / page_size（报表不分页）")
check(data["row_limit"] == 2000, f"row_limit=2000（实得 {data['row_limit']}）")
check(data["rows_truncated"] is False, "rows_truncated=false（数据量远未到上限）")

print()
print(f"================ M2-09 真接口断言：PASS={passed} FAIL={failed} ================")
print("ALL PASS" if failed == 0 else "HAS FAILURES")
sys.exit(0 if failed == 0 else 1)
