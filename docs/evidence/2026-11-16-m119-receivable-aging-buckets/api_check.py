"""M1-19 真接口验收：应收账龄 30/60/90/180/365 七档（打本机 8023，库 cp_m119_api）。

断言全部来自后端真实响应；失败立刻非零退出。金额用 Decimal 比较（后端出字符串）。
"""
import json
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE = "http://127.0.0.1:8023/api"
AS_OF = "2026-11-05"

failures = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f"  <<< {detail}"))
    if not ok:
        failures.append(name)


def money(value):
    return Decimal(str(value)).quantize(Decimal("0.0001"))


def call(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read())


def login():
    status, body = call("POST", "/system/auth/login", body={"username": "admin", "password": "Admin@123456"})
    assert status == 200 and body["code"] == 0, body
    return body["data"]["access_token"]


token = login()
print("== 登录成功")

# ---------- ① 基准日 as_of：七档表头 + 逐档金额 ----------
status, body = call("GET", f"/business/erp/receivable/aging?as_of={AS_OF}", token)
report = body.get("data") or {}
check("① GET 应收账龄 200", status == 200 and body.get("code") == 0, body)
check("① 报表编码/名称", report.get("report_code") == "RECEIVABLE_AGING", report.get("report_code"))
check("① 基准日回显", report.get("as_of") == AS_OF, report.get("as_of"))
check("① 基准日来源 QUERY", report.get("as_of_source") == "QUERY", report.get("as_of_source"))

keys = [b["key"] for b in report.get("buckets") or []]
labels = [b["label"] for b in report.get("buckets") or []]
check(
    "① 表头 = 未到期 + 30/60/90/180/365 六段",
    keys == ["NOT_DUE", "D0_30", "D31_60", "D61_90", "D91_180", "D181_365", "OVER_365"],
    keys,
)
check(
    "① 表头中文名",
    labels == ["未到期", "1-30 天", "31-60 天", "61-90 天", "91-180 天", "181-365 天", "365 天以上"],
    labels,
)
check("① 档位上下界（180/365 两刀）", [(b["min_days"], b["max_days"]) for b in report["buckets"]][4:]
      == [(91, 180), (181, 365), (366, None)], report["buckets"][4:])

summary = {cell["key"]: money(cell["unpaid"]) for cell in report.get("summary") or []}
expect = {
    "NOT_DUE": "1200",    # 到期当天 100 + 未到期 1100
    "D0_30": "500",       # 1 天 200 + 30 天 300（部分已收，取未收余额）
    "D31_60": "900",      # 31 天 400 + 60 天 500
    "D61_90": "1300",     # 61 天 600 + 90 天 700
    "D91_180": "2100",    # 91 天 800 + 180 天 1300（180 是上界）
    "D181_365": "2900",   # 181 天 1400 + 365 天 1500（365 是上界）
    "OVER_365": "1600",   # 366 天 1600（365 天以上的下界）
}
for key, want in expect.items():
    check(f"① {key} = {want}", summary.get(key) == money(want), summary.get(key))

check("① 未收合计 10500", money(report.get("total_unsettled")) == money("10500"), report.get("total_unsettled"))
check("① 各档之和 = 未收合计", sum(summary.values()) == money(report.get("total_unsettled")), summary)
check("① 逾期合计 9300（未到期那档不算逾期）", money(report.get("overdue_unsettled")) == money("9300"),
      report.get("overdue_unsettled"))
check("① 逾期占比 88.57", money(report.get("overdue_ratio")) == money("88.57"), report.get("overdue_ratio"))
check("① 单据数 13（SETTLED / 坏账两张不进表）", report.get("doc_count") == 13, report.get("doc_count"))
check("① 客户数 2", report.get("customer_count") == 2, report.get("customer_count"))

customers = report.get("customers") or []
check("① 客户按未收倒序：乙 5300 在前、甲 5200 在后",
      [(c["customer_name"], money(c["total_unsettled"])) for c in customers]
      == [("乙客户", money("5300")), ("甲客户", money("5200"))],
      [(c["customer_name"], c["total_unsettled"]) for c in customers])
check("① 客户行列数 = 表头 7 列", all(len(c["buckets"]) == 7 for c in customers), None)
check("① 客户行各档之和 = 该客户未收",
      all(sum(money(cell["unpaid"]) for cell in c["buckets"]) == money(c["total_unsettled"]) for c in customers),
      None)
check("① 乙最长逾期 366 天", customers[0]["max_overdue_days"] == 366, customers[0]["max_overdue_days"])

details = report.get("details") or []
check("① 明细 13 条且按到期日升序",
      len(details) == 13 and [d["due_date"] for d in details] == sorted(d["due_date"] for d in details),
      len(details))
check("① 明细首行 = 最久逾期（366 天 / 365 天以上 / 乙客户）",
      (details[0]["overdue_days"], details[0]["bucket"], details[0]["bucket_label"], details[0]["customer_name"])
      == (366, "OVER_365", "365 天以上", "乙客户"),
      details[0])
detail_buckets = {}
for row in details:
    detail_buckets.setdefault(row["bucket"], Decimal("0"))
    detail_buckets[row["bucket"]] += money(row["unsettled_amount"])
check("① 明细逐档合计 = 汇总（SQL 聚合与 Rust 现算同口径）",
      {k: detail_buckets.get(k, Decimal("0")) for k in expect} == {k: money(v) for k, v in expect.items()},
      detail_buckets)
check("① 口径提示写明 30/60/90/180/365",
      any("30/60/90/180/365" in hint for hint in report.get("hints") or []), report.get("hints"))

# ---------- ② period：基准日取期间末日，且期间早于基准月的单被截断 ----------
status, body = call("GET", "/business/erp/receivable/aging?period=2026-10", token)
by_period = body.get("data") or {}
check("② GET ?period=2026-10 200", status == 200 and body.get("code") == 0, body)
check("② 基准日 = 期间末日 2026-10-31", by_period.get("as_of") == "2026-10-31", by_period.get("as_of"))
check("② 基准日来源 PERIOD", by_period.get("as_of_source") == "PERIOD", by_period.get("as_of_source"))
check("② 期间名 2026年10月", by_period.get("period_name") == "2026年10月", by_period.get("period_name"))
check("② 期间晚于基准月的 3 张被截断 ⇒ 10 条", by_period.get("doc_count") == 10, by_period.get("doc_count"))
check("② 表头仍是 7 档（空档补 0 而不是缺列）",
      [b["key"] for b in by_period.get("buckets") or []] == keys, [b["key"] for b in by_period.get("buckets") or []])

# ---------- ③ 参数闸：非法基准日 400 + 10001（不静默当今天） ----------
status, body = call("GET", "/business/erp/receivable/aging?as_of=2026-13-01", token)
check("③ 非法 as_of → 400 + 10001", status == 400 and body.get("code") == 10001, (status, body.get("code")))

# ---------- ④ 应付账龄不受影响（本卡不动应付契约：仍是 5 档 + OVER_90） ----------
status, body = call("GET", "/business/erp/report/payable-aging", token)
ap = body.get("data") or {}
check("④ GET 应付账龄 200", status == 200 and body.get("code") == 0, body)
check("④ 应付仍是 5 档（M1-21 才补分档）",
      [b["key"] for b in ap.get("buckets") or []] == ["NOT_DUE", "D0_30", "D31_60", "D61_90", "OVER_90"],
      [b["key"] for b in ap.get("buckets") or []])

print()
if failures:
    print(f"FAILED {len(failures)} 条：" + ", ".join(failures))
    sys.exit(1)
print("全部 PASS")
