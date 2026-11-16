"""M2-15 补货建议真接口验收（在 99 上跑，打本机隔离实例 127.0.0.1:18215）。

只做 HTTP：登录 → 改密（空库首启必须）→ 补货建议接口全量 / include_covered / 关键字 /
越权参数 / 未授权。断言写死，逐条 PASS/FAIL，末尾汇总并非 0 退出。

前置：库 `cp_m215_drill` 已由服务首启应用迁移 + 种子，并跑过
`scripts/verify/m215_replenish_fixture.sql`（5 物料 / 5 库存行 / 4 水位 / 3 采购单 / 1 在途）。
"""

import json
import os
import sys
import urllib.error
import urllib.request
from urllib.parse import quote

BASE = os.environ.get("M215_API_BASE", "http://127.0.0.1:18215/api")
OLD_PASSWORD = os.environ.get("M215_ADMIN_OLD_PASSWORD", "Admin@123456")
NEW_PASSWORD = os.environ.get("M215_ADMIN_PASSWORD", "M215@Replenish1")
ERP_FILTER_INVALID = 66006

result = {"pass": 0, "fail": 0}


def check(name, ok, detail=""):
    if ok:
        result["pass"] += 1
        print(f"  PASS {name}")
    else:
        result["fail"] += 1
        print(f"  FAIL {name} {detail}")


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw)
        except json.JSONDecodeError:
            return err.code, raw


def login(username, password):
    status, body = call("POST", "/system/auth/login", {"username": username, "password": password})
    assert status == 200, f"登录失败 {status} {body}"
    return body["data"]


def page(params="", token=None):
    # 中文关键字要 percent-encode（urllib 不会自动编码查询串里的非 ASCII）
    safe_params = quote(params, safe="=&?")
    status, body = call("GET", f"/business/erp/replenish/page{safe_params}", token=token)
    assert status == 200, f"补货建议接口失败 {status} {body}"
    return body["data"]


print("== 登录（空库首启 admin 必须先改密）==")
first_boot = True
try:
    sess = login("admin", OLD_PASSWORD)
except AssertionError:
    first_boot = False
    sess = login("admin", NEW_PASSWORD)
token = sess.get("access_token") or sess.get("token")
check("登录 admin", bool(token))
if first_boot:
    check("空库首启 must_change_password=true", bool(sess.get("must_change_password")))
    status, body = call(
        "PUT", "/system/auth/password",
        {"old_password": OLD_PASSWORD, "new_password": NEW_PASSWORD}, token,
    )
    check("改密 200", status == 200, f"{status} {body}")
    sess = login("admin", NEW_PASSWORD)
    token = sess.get("access_token") or sess.get("token")
    check("重新登录后 must_change_password=false", sess.get("must_change_password") is False)
else:
    print("  SKIP 改密（admin 已是新口令，脚本可重跑）")

print("== 默认列表：只返回要下单的行 ==")
data = page("", token)
rows = data["rows"]
check("total=2（F1 建议 65 + F3 物料级回退 20）", data["total"] == 2, f"total={data['total']}")
check("本页行数=2", len(rows) == 2, f"rows={len(rows)}")
check("scanned_cells=5（5 个库存行 → 4 格参与，F4 在途仓被排除）", data["scanned_cells"] == 4,
      f"scanned={data['scanned_cells']}")
check("scan_truncated=false", data["scan_truncated"] is False)
check("totals.row_count == total", data["totals"]["row_count"] == data["total"])
check("totals.purchase_row_count=2", data["totals"]["purchase_row_count"] == 2)
check("totals.covered_row_count=0（默认不含覆盖行）", data["totals"]["covered_row_count"] == 0)
check("totals.material_count=2", data["totals"]["material_count"] == 2)
check("totals.suggested_qty_total=85（65 + 20）",
      abs(float(data["totals"]["suggested_qty_total"]) - 85.0) < 1e-9,
      f"合计={data['totals']['suggested_qty_total']}")
check("hints 说明建议量的算法",
      any("建议采购量 = 目标水位 − 预计可用" in h for h in data["hints"]), str(data["hints"])[:400])

by_code = {row["material_code"]: row for row in rows}
check("按建议量降序（最急的在前）", rows[0]["suggested_qty"] >= rows[-1]["suggested_qty"])
check("M215-F5（预计可用 == 最低水位）不出现", "M215-F5" not in by_code)
check("M215-F4（在途仓）不出现", "M215-F4" not in by_code)
check("M215-F2（在途覆盖）默认不出现", "M215-F2" not in by_code)

print("== 单行算术：结存 − 在途 + 未清订单 → 建议量 ==")
f1 = by_code.get("M215-F1")
if f1 is None:
    check("M215-F1 在结果里", False, str(list(by_code)))
else:
    check("M215-F1 结存 30", float(f1["on_hand_qty"]) == 30.0, f1["on_hand_qty"])
    check("M215-F1 发货在途 5", float(f1["outbound_transit_qty"]) == 5.0, f1["outbound_transit_qty"])
    check("M215-F1 未清订单 10（草稿 100 不计）", float(f1["open_po_qty"]) == 10.0, f1["open_po_qty"])
    check("M215-F1 可用 25", float(f1["available_qty"]) == 25.0, f1["available_qty"])
    check("M215-F1 预计可用 35", float(f1["expected_qty"]) == 35.0, f1["expected_qty"])
    check("M215-F1 最低水位 50（来源 POLICY）",
          float(f1["min_qty"]) == 50.0 and f1["min_source"] == "POLICY", f"{f1['min_qty']}/{f1['min_source']}")
    check("M215-F1 最高水位 100", float(f1["max_qty"]) == 100.0, f1["max_qty"])
    check("M215-F1 目标水位 100（补到最高水位）",
          float(f1["target_qty"]) == 100.0 and f1["target_source"] == "MAX", f"{f1['target_qty']}/{f1['target_source']}")
    check("M215-F1 建议采购量 65", float(f1["suggested_qty"]) == 65.0, f1["suggested_qty"])
    check("M215-F1 结论 PURCHASE / 标签「建议采购」",
          f1["verdict"] == "PURCHASE" and f1["verdict_label"] == "建议采购", f1["verdict"])
    check("M215-F1 名字实时 join（编码 / 名称 / 仓库名）",
          f1["material_name"] == "补货建议物料一" and f1["warehouse_name"] == "补货建议仓", f1)
    check("M215-F1 一格一个成本桶", f1["inventory_row_count"] == 1, f1["inventory_row_count"])
    check("M215-F1 triggered=true 且 below_min=true", f1["triggered"] is True and f1["below_min"] is True)

f3 = by_code.get("M215-F3")
if f3 is None:
    check("M215-F3 在结果里（物料级回退）", False, str(list(by_code)))
else:
    check("M215-F3 最低水位 40（来源 MATERIAL）",
          float(f3["min_qty"]) == 40.0 and f3["min_source"] == "MATERIAL", f"{f3['min_qty']}/{f3['min_source']}")
    check("M215-F3 未设最高水位 → 目标 = 最低水位",
          f3["max_qty"] is None and f3["target_source"] == "MIN", f3["target_source"])
    check("M215-F3 建议 20（40 − 20）", float(f3["suggested_qty"]) == 20.0, f3["suggested_qty"])

print("== include_covered=true：带上「在途覆盖」行 ==")
data = page("?include_covered=true", token)
rows = data["rows"]
covered = [row for row in rows if row["verdict"] == "COVERED"]
check("total=3（多出 F2）", data["total"] == 3, data["total"])
check("covered_row_count=1", data["totals"]["covered_row_count"] == 1, data["totals"])
check("purchase_row_count 仍为 2", data["totals"]["purchase_row_count"] == 2, data["totals"])
check("F2 建议量为 0", covered and float(covered[0]["suggested_qty"]) == 0.0, covered)
check("F2 结论 COVERED / 标签「在途覆盖」",
      covered and covered[0]["verdict_label"] == "在途覆盖", covered)
check("F2 below_min=true 但 triggered=false",
      covered and covered[0]["below_min"] is True and covered[0]["triggered"] is False, covered)
check("hints 解释这些行为什么不用下单",
      any("在途覆盖" in h for h in data["hints"]), str(data["hints"])[:400])

print("== 筛选参数 ==")
data = page("?keyword=M215-F1", token)
check("keyword 命中 1 行", data["total"] == 1, data["total"])
check("keyword 命中 F1", data["rows"] and data["rows"][0]["material_code"] == "M215-F1")
data = page("?keyword=补货建议物料三", token)
check("keyword 按名称命中 F3", data["total"] == 1 and data["rows"][0]["material_code"] == "M215-F3",
      data["total"])
data = page("?page_size=999", token)
check("page_size 上限收敛到 200", data["page_size"] == 200, data["page_size"])
data = page("?page=9&page_size=1", token)
check("越界页码返回空页（不报错）", data["rows"] == [] and data["total"] == 2, data["total"])
status, body = call("GET", "/business/erp/replenish/page?include_covered=treu", token=token)
check("非法开关 → 400 + 66006", status == 400 and body.get("code") == ERP_FILTER_INVALID, f"{status} {body}")
msg = (body.get("msg") or body.get("message") or "") if isinstance(body, dict) else ""
check("非法开关的文案点名 include_covered", "include_covered" in msg, body)
status, body = call("GET", "/business/erp/replenish/page?material_id=not-a-uuid", token=token)
check("非法 UUID → 400 + 66006", status == 400 and body.get("code") == ERP_FILTER_INVALID, f"{status} {body}")
msg = (body.get("msg") or body.get("message") or "") if isinstance(body, dict) else ""
check("非法 UUID 的文案点名 material_id", "material_id" in msg, body)

print("== 未授权 ==")
status, body = call("GET", "/business/erp/replenish/page")
check("无 token → 401", status == 401, f"{status} {body}")

print("== 汇总 ==")
print(f"  pass={result['pass']} fail={result['fail']}")
if result["fail"]:
    sys.exit(1)
print("  ALL PASS")
