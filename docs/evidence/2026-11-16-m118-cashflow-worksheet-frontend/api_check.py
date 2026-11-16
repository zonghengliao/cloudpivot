"""M1-18 真接口验收：现金流底稿录入 / 勾稽 / 出表 / 出表闸（打本机 8023，库 cp_m118_front）。

不写死任何金额以外的假设：每一步都断言后端真实返回，失败立刻非零退出。
"""
import json
import sys
import urllib.error
import urllib.request

from money_util import money_equal

BASE = "http://127.0.0.1:8023/api"
PERIOD = "2023-02"          # OPEN；上期 2023-01 也是 OPEN（验证「上期列有列无数」）
EMPTY_PERIOD = "2023-03"    # OPEN 且不写任何底稿（验证「空底稿」形状；本脚本对它只读）
CLOSED_PERIOD = "2021-01"   # CLOSED（写入必须 66085）

failures = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f"  <<< {detail}"))
    if not ok:
        failures.append(name)


def call(method, path, token=None, body=None, raw=False):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            payload = resp.read()
            if raw:
                return resp.status, dict(resp.headers), payload
            return resp.status, json.loads(payload)
    except urllib.error.HTTPError as error:
        payload = error.read()
        if raw:
            return error.code, dict(error.headers), payload
        return error.code, json.loads(payload)


def login():
    status, body = call("POST", "/system/auth/login", body={"username": "admin", "password": "Admin@123456"})
    assert status == 200 and body["code"] == 0, body
    return body["data"]["access_token"]


token = login()
print("== 登录成功")

# ---------- ① 读空底稿：12 个配置项全部 unentered，期初期末 null ----------
status, body = call("GET", f"/business/erp/report/cash-flow/worksheet?period={EMPTY_PERIOD}", token)
sheet = body.get("data") or {}
check("① GET 底稿 200", status == 200 and body.get("code") == 0, body)
check("① 期间 OPEN 可编辑", sheet.get("editable") is True and sheet.get("period_status") == "OPEN", sheet.get("period_status"))
check("① 明细行 = 12 个配置项", len(sheet.get("rows") or []) == 12, len(sheet.get("rows") or []))
check("① 全部未录入", all(not row["entered"] for row in sheet.get("rows") or []), None)
check("① 期初/期末未录 → null", sheet.get("opening") is None and sheet.get("closing") is None, None)
check(
    "① 勾稽不闭合且点名缺期初/缺期末",
    sheet["check"]["balanced"] is False
    and {p["code"] for p in sheet["check"]["problems"]} >= {"OPENING_MISSING", "CLOSING_MISSING"},
    [p["code"] for p in sheet["check"]["problems"]],
)

# ---------- ② 保存：期初 5000 + 流入 12000 − 流出 7000 = 期末 10000 ----------
save_body = {
    "period": PERIOD,
    "opening": {"amount": "5000", "remark": "上月结转"},
    "closing": {"amount": "10000", "remark": "与银行对账单一致"},
    "items": [
        {"item_code": "OPER_SALES_RECEIPTS", "amount": "12000", "remark": "销售回款"},
        {"item_code": "OPER_PURCHASE_PAYMENTS", "amount": "7000", "remark": None},
    ],
}
status, body = call("PUT", "/business/erp/report/cash-flow/worksheet", token, save_body)
saved = body.get("data") or {}
check("② PUT 保存 200", status == 200 and body.get("code") == 0, body)
check("② 保存后勾稽闭合", saved.get("check", {}).get("balanced") is True, saved.get("check", {}).get("message"))
check("② 流入合计 12000", saved.get("totals", {}).get("inflow") == "12000.0000", saved.get("totals"))
check("② 流出合计 7000", saved.get("totals", {}).get("outflow") == "7000.0000", saved.get("totals"))
check("② 净增加额 5000", saved.get("totals", {}).get("net") == "5000.0000", saved.get("totals"))
check("② 期初余额行落库", saved.get("opening", {}).get("amount") == "5000.0000", saved.get("opening"))
check("② 期末余额行落库", saved.get("closing", {}).get("amount") == "10000.0000", saved.get("closing"))
operating = next(block for block in saved.get("activities") or [] if block["code"] == "OPERATING")
check(
    "② 经营活动小计 = 流入 12000 / 流出 7000 / 净额 5000",
    (operating["inflow"], operating["outflow"], operating["net"]) == ("12000.0000", "7000.0000", "5000.0000"),
    operating,
)

# ---------- ③ 试算（不落库）：把期末改成 9900 → diff = 100 ----------
check_body = {
    "period": PERIOD,
    "closing": {"amount": "9900", "remark": None},
    "items": [],
    "require_balanced": False,
}
status, body = call("POST", "/business/erp/report/cash-flow/worksheet/check", token, check_body)
draft = body.get("data") or {}
check("③ POST check 200（不落库）", status == 200 and body.get("code") == 0, body)
check("③ 草稿差 100 判不闭合", draft.get("balanced") is False and money_equal(draft.get("diff"), "100"), draft.get("diff"))
check("③ 草稿问题点名 IMBALANCED", any(p["code"] == "IMBALANCED" for p in draft.get("problems") or []), draft.get("problems"))

# 校验确实没写库
status, body = call("GET", f"/business/erp/report/cash-flow/worksheet?period={PERIOD}", token)
check("③ 试算未污染已存底稿（期末仍 10000）", body["data"]["closing"]["amount"] == "10000.0000", body["data"]["closing"])

# ---------- ④ 出表：三栏报表 + 上期列 ----------
status, body = call("GET", f"/business/erp/report/cash-flow/statement?period={PERIOD}", token)
statement = body.get("data") or {}
check("④ GET statement 200", status == 200 and body.get("code") == 0, body)
check("④ 报表编码 / 名称", statement.get("report_code") == "CASH_FLOW_WORKSHEET" and statement.get("report_name") == "现金流量表", statement.get("report_name"))
check("④ 出表闭合", statement.get("check", {}).get("balanced") is True, statement.get("check", {}).get("message"))
check("④ 报表加减关系成立", statement.get("check", {}).get("arithmetic_ok") is True, statement.get("check"))
check("④ 上期期间 = 2023-01", statement.get("previous_period") == "2023-01", statement.get("previous_period"))
current_label, previous_label = statement.get("current_label"), statement.get("previous_label")
check("④ 两栏列名", (current_label, previous_label) == ("本期金额", "上期金额"), (current_label, previous_label))
sales_line = None
for section in statement.get("sections") or []:
    for line in section.get("inflow") or []:
        if line["item_code"] == "OPER_SALES_RECEIPTS":
            sales_line = line
check("④ 明细行本期 12000", sales_line and sales_line["current"] == "12000.0000", sales_line)
check("④ 明细行上期 0 且 previous_entered=false（上期没编）", bool(sales_line) and money_equal(sales_line["previous"], "0") and sales_line["previous_entered"] is False, sales_line)
check("④ 净增加额本期 5000", statement.get("net_increase", {}).get("current") == "5000.0000", statement.get("net_increase"))
check("④ 期初/期末本期", (statement.get("opening", {}).get("current"), statement.get("closing", {}).get("current")) == ("5000.0000", "10000.0000"), (statement.get("opening"), statement.get("closing")))

# ---------- ⑤ 导出 xlsx ----------
status, headers, payload = call("GET", f"/business/erp/report/cash-flow/statement/export?period={PERIOD}", token, raw=True)
check("⑤ 导出 200 且是真 xlsx（PK 魔数）", status == 200 and payload[:2] == b"PK" and len(payload) > 2000, (status, len(payload), payload[:8]))
print(f"      xlsx 大小 = {len(payload)} 字节，content-type = {headers.get('Content-Type') or headers.get('content-type')}")

# ---------- ⑥ 出表闸：把期末改成 9900 → 导出 400+66087，但 GET 仍 200 ----------
status, body = call(
    "PUT",
    "/business/erp/report/cash-flow/worksheet",
    token,
    {"period": PERIOD, "closing": {"amount": "9900", "remark": None}, "items": []},
)
check("⑥ 不平也能保存（工作稿）200", status == 200 and body.get("code") == 0, body)
check("⑥ 保存返回 balanced=false + 差 100", body["data"]["check"]["balanced"] is False and money_equal(body["data"]["check"]["diff"], "100"), body["data"]["check"])

status, body = call("GET", f"/business/erp/report/cash-flow/statement?period={PERIOD}", token)
check("⑥ 未闭合时 GET statement 仍 200（页面能开去补）", status == 200 and body.get("code") == 0, status)
check("⑥ 未闭合时报表 check.balanced=false", body["data"]["check"]["balanced"] is False, body["data"]["check"]["message"])

status, headers, payload = call("GET", f"/business/erp/report/cash-flow/statement/export?period={PERIOD}", token, raw=True)
check("⑥ 未闭合时导出被拦", status == 400, status)
try:
    error_body = json.loads(payload)
except Exception:  # noqa: BLE001
    error_body = {}
check("⑥ 拦截码 66087", error_body.get("code") == 66087, error_body.get("code"))
check("⑥ 拦截文案给了差额", "差 100" in (error_body.get("msg") or ""), error_body.get("msg"))

# 恢复闭合（页面「改回即自愈」）
status, body = call(
    "PUT",
    "/business/erp/report/cash-flow/worksheet",
    token,
    {"period": PERIOD, "closing": {"amount": "10000", "remark": "与银行对账单一致"}, "items": []},
)
check("⑥ 改回期末 → 重新闭合", body["data"]["check"]["balanced"] is True, body["data"]["check"])
status, headers, payload = call("GET", f"/business/erp/report/cash-flow/statement/export?period={PERIOD}", token, raw=True)
check("⑥ 改回后导出恢复 200", status == 200 and payload[:2] == b"PK", status)

# ---------- ⑦ 期间闸：已结账期间写入 66085 ----------
status, body = call(
    "PUT",
    "/business/erp/report/cash-flow/worksheet",
    token,
    {"period": CLOSED_PERIOD, "opening": {"amount": "1", "remark": None}, "items": []},
)
check("⑦ 已结账期间写入被拒", status == 400 and body.get("code") == 66085, (status, body.get("code"), body.get("msg")))
status, body = call("GET", f"/business/erp/report/cash-flow/worksheet?period={CLOSED_PERIOD}", token)
check("⑦ 已结账期间仍可读 + editable=false", status == 200 and body["data"]["editable"] is False, (status, body.get("data", {}).get("editable")))

# ---------- ⑧ 项目闸：不存在的项目编码 66086 ----------
status, body = call(
    "PUT",
    "/business/erp/report/cash-flow/worksheet",
    token,
    {"period": PERIOD, "items": [{"item_code": "NO_SUCH_ITEM", "amount": "1", "remark": None}]},
)
check("⑧ 未知项目编码被拒 66086", status == 400 and body.get("code") == 66086, (status, body.get("code")))

# ---------- ⑨ 金额闸：5 位小数 10001 ----------
status, body = call(
    "PUT",
    "/business/erp/report/cash-flow/worksheet",
    token,
    {"period": PERIOD, "items": [{"item_code": "OPER_SALES_RECEIPTS", "amount": "1.23456", "remark": None}]},
)
check("⑨ 5 位小数被拒（10001）", status == 400 and body.get("code") == 10001, (status, body.get("code"), body.get("msg")))

print()
if failures:
    print(f"FAIL {len(failures)} 项：{failures}")
    sys.exit(1)
print("全部通过")
