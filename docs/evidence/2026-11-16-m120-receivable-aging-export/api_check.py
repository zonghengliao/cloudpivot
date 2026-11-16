"""M1-20 真接口验收：应收账龄报表 + xlsx 导出（打本机 8023，库 cp_m120_api）。

断言全部来自后端真实响应 / 真实 xlsx 字节；失败立刻非零退出。
xlsx 不装第三方库：直接按 OOXML 读 `sharedStrings.xml` + `sheet1.xml`（zipfile + xml）。
金额用 Decimal 比较（后端出字符串，表内是千分位文本）。
"""
import sys
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from decimal import Decimal
from io import BytesIO

BASE = "http://127.0.0.1:8023/api"
AS_OF = "2026-11-05"
NS = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"

failures = []


def check(name, ok, detail: object = ""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f"  <<< {detail}"))
    if not ok:
        failures.append(name)


def money(value):
    text = str(value).replace(",", "")
    return Decimal(text).quantize(Decimal("0.01"))


def call(method, path, token=None, body=None, raw=False):
    data = None if body is None else str(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            payload = resp.read()
            if raw:
                return resp.status, dict(resp.headers), payload
            return resp.status, dict(resp.headers), (payload if isinstance(payload, bytes) else payload)
    except urllib.error.HTTPError as error:
        payload = error.read()
        return error.code, dict(error.headers), payload


def json_of(payload):
    import json

    return json.loads(payload.decode())


def login():
    status, _, payload = call(
        "POST", "/system/auth/login", body='{"username":"admin","password":"Admin@123456"}'
    )
    body = json_of(payload)
    assert status == 200 and body["code"] == 0, body
    return body["data"]["access_token"]


# ---------- 最小 xlsx 读表（OOXML 直读，不引第三方库） ----------


def shared_strings(book):
    try:
        xml = book.read("xl/sharedStrings.xml")
    except KeyError:
        return []
    root = ET.fromstring(xml)
    out = []
    for si in root.findall(f"{NS}si"):
        out.append("".join(node.text or "" for node in si.iter(f"{NS}t")))
    return out


def read_sheet(payload):
    """返回 [[cell_text, ...], ...]，按行列展开（空格给空串）。"""
    book = zipfile.ZipFile(BytesIO(payload))
    names = book.namelist()
    sheet = next((name for name in names if name.startswith("xl/worksheets/sheet")), None)
    if sheet is None:
        raise AssertionError(f"xlsx 里没有 sheet：{names}")
    strings = shared_strings(book)
    root = ET.fromstring(book.read(sheet))
    grid = []
    for row in root.iter(f"{NS}row"):
        cells = []
        for cell in row.findall(f"{NS}c"):
            kind = cell.get("t")
            value_node = cell.find(f"{NS}v")
            inline = cell.find(f"{NS}is")
            if kind == "s" and value_node is not None:
                cells.append(strings[int(value_node.text or "0")])
            elif kind == "inlineStr" and inline is not None:
                cells.append("".join(node.text or "" for node in inline.iter(f"{NS}t")))
            elif value_node is not None:
                text = value_node.text or ""
                if "." in text and text.split(".")[1] in ("0", ""):
                    text = text.split(".")[0]
                cells.append(text)
            else:
                cells.append("")
        grid.append(cells)
    return grid


def content_disposition_name(headers):
    raw = headers.get("Content-Disposition") or headers.get("content-disposition") or ""
    for part in raw.split(";"):
        part = part.strip()
        if part.startswith("filename*=UTF-8''"):
            return urllib.parse.unquote(part[len("filename*=UTF-8''") :])
    return raw


def export(token, query=""):
    return call("GET", "/business/erp/receivable/aging/export" + query, token, raw=True)


token = login()
print("== 登录成功")

# ---------- ① 报表（导出必须与它同源） ----------
status, _, payload = call("GET", f"/business/erp/receivable/aging?as_of={AS_OF}", token)
body = json_of(payload)
check("① GET 应收账龄 200", status == 200 and body.get("code") == 0, body)
report = body.get("data") or {}
check("① 未收合计 10500（夹具口径）", money(report.get("total_unsettled", "0")) == Decimal("10500.00"), report.get("total_unsettled"))
check("① 单据数 13", report.get("doc_count") == 13, report.get("doc_count"))

# ---------- ② 导出：状态 / 响应头 / 文件名 ----------
status, headers, xlsx = export(token, f"?as_of={AS_OF}")
check("② GET 导出 200", status == 200, status)
ctype = headers.get("Content-Type") or headers.get("content-type") or ""
check(
    "② Content-Type = xlsx",
    "spreadsheetml.sheet" in ctype,
    ctype,
)
name = content_disposition_name(headers)
check(
    "② 文件名 = 应收账款账龄分析表_无期间_截止2026-11-05.xlsx",
    name == f"应收账款账龄分析表_无期间_截止{AS_OF}.xlsx",
    name,
)
check("② 响应体是 zip 容器（PK 头）", xlsx[:2] == b"PK", xlsx[:8])
check("② 响应体不是 JSON 错误", b'"code"' not in xlsx[:200], xlsx[:80])

# ---------- ③ 读回表：表头 / 行数 / 逐格等于报表 ----------
grid = read_sheet(xlsx)
expect_header = [
    "客户",
    "单据数",
    "未到期",
    "1-30 天",
    "31-60 天",
    "61-90 天",
    "91-180 天",
    "181-365 天",
    "365 天以上",
    "未收合计",
    "最长逾期天数",
]
check("③ 表头 11 列逐字", grid[0] == expect_header, grid[0])
check("③ 行数 = 表头 + 2 客户 + 合计 = 4", len(grid) == 4, len(grid))
col = {name: index for index, name in enumerate(grid[0])}

customers = report.get("customers") or []
check("③ 客户行数 = 报表客户数", len(customers) == 2, len(customers))

for offset, customer in enumerate(customers):
    row = grid[1 + offset]
    check(f"③ 第 {offset + 1} 行客户名", row[col["客户"]] == customer["customer_name"], row[col["客户"]])
    check(f"③ 第 {offset + 1} 行单据数", row[col["单据数"]] == str(customer["doc_count"]), row[col["单据数"]])
    check(
        f"③ 第 {offset + 1} 行最长逾期天数",
        row[col["最长逾期天数"]] == str(customer["max_overdue_days"]),
        row[col["最长逾期天数"]],
    )
    check(
        f"③ 第 {offset + 1} 行未收合计 = 报表",
        money(row[col["未收合计"]]) == money(customer["total_unsettled"]),
        (row[col["未收合计"]], customer["total_unsettled"]),
    )
    sheet_sum = Decimal("0.00")
    for cell in customer["buckets"]:
        label = next(b["label"] for b in report["buckets"] if b["key"] == cell["key"])
        sheet_value = money(row[col[label]])
        check(
            f"③ 第 {offset + 1} 行「{label}」= 报表同档",
            sheet_value == money(cell["unpaid"]),
            (row[col[label]], cell["unpaid"]),
        )
        sheet_sum += sheet_value
    check(
        f"③ 第 {offset + 1} 行各档之和 = 未收合计",
        sheet_sum == money(customer["total_unsettled"]),
        (sheet_sum, customer["total_unsettled"]),
    )

total = grid[3]
check("③ 合计行标签", total[col["客户"]] == "合计", total[col["客户"]])
check("③ 合计行单据数 = 13", total[col["单据数"]] == "13", total[col["单据数"]])
check(
    "③ 合计行未收合计 = 10500.00",
    money(total[col["未收合计"]]) == Decimal("10500.00"),
    total[col["未收合计"]],
)
for cell in report["summary"]:
    label = next(b["label"] for b in report["buckets"] if b["key"] == cell["key"])
    check(
        f"③ 合计行「{label}」= 报表 summary",
        money(total[col[label]]) == money(cell["unpaid"]),
        (total[col[label]], cell["unpaid"]),
    )
check(
    "③ 合计行最长逾期 = 各客户最大（366）",
    total[col["最长逾期天数"]] == "366",
    total[col["最长逾期天数"]],
)

# ---------- ④ 期间口径：?period=2026-10 导出与报表同样截断 ----------
status, headers, xlsx_period = export(token, "?period=2026-10")
check("④ 期间导出 200", status == 200, status)
grid_period = read_sheet(xlsx_period)
check(
    "④ 期间导出行数 = 表头 + 2 客户 + 合计 = 4（两家都还有未收，只是甲被截断 1400）",
    len(grid_period) == 4,
    len(grid_period),
)
period_col = {name: index for index, name in enumerate(grid_period[0])}
by_name_period = {row[period_col["客户"]]: row for row in grid_period[1:-1]}
check(
    "④ 期间导出：甲客户 3800.00（11 月三张 100+200+1100 被截断）",
    "甲客户" in by_name_period and money(by_name_period["甲客户"][period_col["未收合计"]]) == Decimal("3800.00"),
    by_name_period.get("甲客户"),
)
check(
    "④ 期间导出：乙客户 5300.00 不受影响",
    "乙客户" in by_name_period and money(by_name_period["乙客户"][period_col["未收合计"]]) == Decimal("5300.00"),
    by_name_period.get("乙客户"),
)
status, _, payload = call("GET", "/business/erp/receivable/aging?period=2026-10", token)
period_report = json_of(payload)["data"]
check("④ 期间报表基准日 = 2026-10-31", period_report.get("as_of") == "2026-10-31", period_report.get("as_of"))
check(
    "④ 期间报表未收合计 = 9100（11 月三张被截断）",
    money(period_report.get("total_unsettled", "0")) == Decimal("9100.00"),
    period_report.get("total_unsettled"),
)
check(
    "④ 期间导出合计 == 期间报表合计",
    money(grid_period[-1][col["未收合计"]]) == money(period_report["total_unsettled"]),
    (grid_period[-1][col["未收合计"]], period_report["total_unsettled"]),
)
status, headers, _ = export(token, "?period=2026-10")
check(
    "④ 期间导出文件名带期间",
    content_disposition_name(headers) == "应收账款账龄分析表_2026-10_截止2026-10-31.xlsx",
    content_disposition_name(headers),
)

# ---------- ⑤ 出错路径 ----------
status, headers, payload = export(token, "?as_of=2026-13-01")
body = json_of(payload)
check("⑤ 非法 as_of → 400", status == 400, status)
check("⑤ 非法 as_of → 错误码 10001", body.get("code") == 10001, body.get("code"))

status, headers, payload = call("GET", "/business/erp/receivable/aging/export", None, raw=True)
check("⑤ 无 token → 401（导出不是裸接口）", status == 401, status)

# ---------- ⑤b 权限闸：能看报表 ≠ 能导出（SALES_SUPERVISOR 有 aging、没有 report:export） ----------
status, _, payload = call(
    "POST", "/system/auth/login", body='{"username":"m120sales","password":"Admin@123456"}'
)
sales_body = json_of(payload)
check("⑤b 对照用户登录 200", status == 200 and sales_body.get("code") == 0, sales_body)
if sales_body.get("code") == 0:
    sales_token = sales_body["data"]["access_token"]
    status, _, _ = call("GET", f"/business/erp/receivable/aging?as_of={AS_OF}", sales_token)
    check("⑤b 对照用户看报表 200（有 receivable:aging）", status == 200, status)
    status, _, payload = call(
        "GET", f"/business/erp/receivable/aging/export?as_of={AS_OF}", sales_token, raw=True
    )
    check("⑤b 对照用户导出 403（没有 report:export）", status == 403, status)


# ---------- ⑥ 极端基准日：全部落进最后一档（分档列不许错位） ----------
status, headers, xlsx_future = export(token, "?as_of=2099-12-31")
check("⑥ 未来基准日导出 200", status == 200, status)
grid_future = read_sheet(xlsx_future)
check("⑥ 未来基准日仍是 2 客户 + 合计（4 行）", len(grid_future) == 4, len(grid_future))
check(
    "⑥ 未来基准日：全部金额落进「365 天以上」",
    money(grid_future[-1][col["365 天以上"]]) == Decimal("10500.00"),
    grid_future[-1][col["365 天以上"]],
)
for label in ("未到期", "1-30 天", "31-60 天", "61-90 天", "91-180 天", "181-365 天"):
    check(f"⑥ 未来基准日「{label}」= 0.00", money(grid_future[-1][col[label]]) == Decimal("0.00"), grid_future[-1][col[label]])


print()
if failures:
    print(f"共 {len(failures)} 条失败：{failures}")
    sys.exit(1)
print("全部断言 PASS")
