"""M1-21 真接口验收：应付账龄 7 档（与应收同口径）+ xlsx 导出列跟着变宽。

打本机 :8023（库 cp_m121_api，空库首启）。断言全部来自后端真实响应 / 真实 xlsx 字节，
失败立刻非零退出。xlsx 不装第三方库：zipfile + xml 直读 OOXML。
金额用 Decimal 比较（后端出字符串，表里是千分位文本）。
"""
import json
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

KEYS = ["NOT_DUE", "D0_30", "D31_60", "D61_90", "D91_180", "D181_365", "OVER_365"]
LABELS = ["未到期", "1-30 天", "31-60 天", "61-90 天", "91-180 天", "181-365 天", "365 天以上"]

# 夹具：单号 → (逾期天数, 档位键)
BOUNDARY = {
    "AP-M121-0001": (-15, "NOT_DUE"),
    "AP-M121-0002": (0, "NOT_DUE"),
    "AP-M121-0003": (1, "D0_30"),
    "AP-M121-0004": (30, "D0_30"),
    "AP-M121-0005": (31, "D31_60"),
    "AP-M121-0006": (60, "D31_60"),
    "AP-M121-0007": (61, "D61_90"),
    "AP-M121-0008": (90, "D61_90"),
    "AP-M121-0009": (91, "D91_180"),
    "AP-M121-000a": (180, "D91_180"),
    "AP-M121-000b": (181, "D181_365"),
    "AP-M121-000c": (365, "D181_365"),
    "AP-M121-000d": (366, "OVER_365"),
}

SUMMARY = {
    "NOT_DUE": Decimal("1700"),    # −15 天 1200 + 到期当天 500
    "D0_30": Decimal("1100"),      # 1 天 300 + 30 天 800
    "D31_60": Decimal("1500"),     # 31 天 (900−500) 400 + 60 天 1100
    "D61_90": Decimal("2800"),     # 61 天 600 + 90 天 2200
    "D91_180": Decimal("2100"),    # 91 天 800 + 180 天 1300
    "D181_365": Decimal("2900"),   # 181 天 1400 + 365 天 1500
    "OVER_365": Decimal("1600"),   # 366 天 1600
}

# 供应商：名称 → (未付合计, 各档金额, 最长逾期天数)
SUPPLIERS = {
    "丙服务（M121 夹具）": (Decimal("5600"), {"D61_90": "2800", "D91_180": "1300", "D181_365": "1500"}, 365),
    "甲材料（M121 夹具）": (Decimal("4400"), {"NOT_DUE": "1700", "D0_30": "300", "D91_180": "800", "OVER_365": "1600"}, 366),
    "乙设备（M121 夹具）": (Decimal("3700"), {"D0_30": "800", "D31_60": "1500", "D181_365": "1400"}, 181),
}

# 夹具（金额, 逾期天数）：用来在 Python 侧**独立**实现一遍分档，跟后端逐档对账，
# 而不是把人算的合计写死在断言里（人算容易错，两份实现互证才是硬证据）。
ROWS = [
    (1200, -15),
    (500, 0),
    (300, 1),
    (800, 30),
    (400, 31),
    (1100, 60),
    (600, 61),
    (2200, 90),
    (800, 91),
    (1300, 180),
    (1400, 181),
    (1500, 365),
    (1600, 366),
]

THRESHOLDS = [
    (0, "NOT_DUE"),
    (30, "D0_30"),
    (60, "D31_60"),
    (90, "D61_90"),
    (180, "D91_180"),
    (365, "D181_365"),
]


def bucket_of(days):
    for max_days, key in THRESHOLDS:
        if days <= max_days:
            return key
    return "OVER_365"


def expected_summary(shift=0):
    out = {key: Decimal("0") for key in KEYS}
    for amount, days in ROWS:
        out[bucket_of(days + shift)] += Decimal(amount)
    return out


failures = []


def check(name, ok, detail: object = ""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f"  <<< {detail}"))
    if not ok:
        failures.append(name)


def money(value):
    return Decimal(str(value).replace(",", "")).quantize(Decimal("0.01"))


def call(method, path, token=None, body=None):
    data = None if body is None else (body if isinstance(body, bytes) else str(body).encode())
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, dict(resp.headers), resp.read()
    except urllib.error.HTTPError as error:
        return error.code, dict(error.headers), error.read()


NEW_PASS = "Aging@2026M121"


def attempt_login(password):
    """登录一次；撞上限流（10005）就等 20 秒重试，最多 4 次。"""
    import time

    for _ in range(4):
        status, _, payload = call(
            "POST", "/system/auth/login", body=json.dumps({"username": "admin", "password": password})
        )
        body = json.loads(payload.decode())
        if body.get("code") != 10005:
            return status, body
        print("登录被限流（10005），等 20 秒重试")
        time.sleep(20)
    return status, body


def login():
    """登录；空库首启的 admin 带 `must_change_password`，先按强制改密把它换掉再重登。

    脚本可重复跑：第一次跑会把初始口令改成 NEW_PASS，后续直接用它（先试 NEW_PASS，
    少一次失败尝试，免得撞上「登录尝试过于频繁」的限流）。
    """
    for password in (NEW_PASS, "Admin@123456"):
        status, body = attempt_login(password)
        if status != 200 or body["code"] != 0:
            continue
        data = body["data"]
        if not data.get("must_change_password"):
            return data["access_token"]
        status, _, payload = call(
            "PUT",
            "/system/auth/password",
            token=data["access_token"],
            body=json.dumps({"old_password": password, "new_password": NEW_PASS}),
        )
        assert status == 200, (status, payload[:400])
        status, body = attempt_login(NEW_PASS)
        assert status == 200 and body["code"] == 0, body
        print(f"首次登录：已按强制改密把初始口令改成 {NEW_PASS}")
        return body["data"]["access_token"]
    raise SystemExit("admin 登录失败（两个口令都不认）")


# ---------- 最小 xlsx 读表（OOXML 直读） ----------

def shared_strings(book):
    try:
        xml = book.read("xl/sharedStrings.xml")
    except KeyError:
        return []
    root = ET.fromstring(xml)
    return [
        "".join(node.text or "" for node in si.iter(f"{NS}t"))
        for si in root.findall(f"{NS}si")
    ]


def read_sheet(payload):
    book = zipfile.ZipFile(BytesIO(payload))
    sheet = next(
        (name for name in book.namelist() if name.startswith("xl/worksheets/sheet")), None
    )
    assert sheet is not None, f"xlsx 里没有 sheet：{book.namelist()}"
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
                cells.append(value_node.text or "")
            else:
                cells.append("")
        grid.append(cells)
    return grid


def disposition_name(headers):
    raw = headers.get("Content-Disposition") or headers.get("content-disposition") or ""
    for part in raw.split(";"):
        part = part.strip()
        if part.startswith("filename*=UTF-8''"):
            return urllib.parse.unquote(part[len("filename*=UTF-8''") :])
    return raw


def report_of(token, **params):
    query = "&".join(f"{k}={urllib.parse.quote(str(v))}" for k, v in params.items())
    status, _, payload = call("GET", f"/business/erp/report/payable-aging?{query}", token=token)
    body = json.loads(payload.decode())
    assert status == 200 and body["code"] == 0, (status, body)
    return body["data"]


def main():
    token = login()
    print(f"登录成功 base={BASE}")

    # ---------------- ① 档位表 7 档（与应收同口径） ----------------
    report = report_of(token, period="2026-11", as_of=AS_OF)
    check("report_code = PAYABLE_AGING", report["report_code"] == "PAYABLE_AGING", report["report_code"])
    check("as_of = 2026-11-05（QUERY）", report["as_of"] == AS_OF and report["as_of_source"] == "QUERY",
          f'{report["as_of"]}/{report["as_of_source"]}')
    keys = [b["key"] for b in report["buckets"]]
    check("档位表 = 7 档且顺序固定（M1-21 对齐应收）", keys == KEYS, keys)
    check("档位中文名逐字", [b["label"] for b in report["buckets"]] == LABELS,
          [b["label"] for b in report["buckets"]])
    check("档位上下界（未到期 ≤0 / 1-30 / 31-60 / 61-90 / 91-180 / 181-365 / 365+）",
          [(b["min_days"], b["max_days"]) for b in report["buckets"]]
          == [(-9223372036854775808, 0), (1, 30), (31, 60), (61, 90), (91, 180), (181, 365), (366, None)],
          [(b["min_days"], b["max_days"]) for b in report["buckets"]])
    check("不再出现旧 5 档的 OVER_90", "OVER_90" not in keys, keys)

    # ---------------- ② 汇总逐档金额（后端 SQL CASE vs Python 独立实现，两份互证） ----------------
    summary = {cell["key"]: money(cell["unpaid"]) for cell in report["summary"]}
    check("summary 也是 7 格", [cell["key"] for cell in report["summary"]] == KEYS,
          [cell["key"] for cell in report["summary"]])
    model = expected_summary()
    check("断言常量与 Python 独立分档一致（自检）",
          all(model[key] == SUMMARY[key] for key in KEYS),
          {key: str(model[key]) for key in KEYS})
    for key in KEYS:
        check(f"档位 {key} = {model[key]}", summary.get(key) == model[key], f"实际 {summary.get(key)}")
    check("未付合计 = 13700", money(report["total_unpaid"]) == Decimal("13700"), report["total_unpaid"])
    check("逾期合计 = 12000（除未到期）", money(report["overdue_unpaid"]) == Decimal("12000"),
          report["overdue_unpaid"])
    check("逾期占比 = 87.59", money(report["overdue_ratio"]) == Decimal("87.59"), report["overdue_ratio"])
    check("单据数 = 13（3 张排除样本不算）", report["doc_count"] == 13, report["doc_count"])
    check("供应商数 = 3", report["supplier_count"] == 3, report["supplier_count"])
    check("口径提示里写明 7 档分档",
          any("91-180" in hint and "365 天以上" in hint for hint in report["hints"]), report["hints"])

    # ---------------- ③ 明细逐行边界（Rust 现算的 bucket 必须与档位表同源） ----------------
    details = {row["payable_no"]: row for row in report["details"]}
    for no, (days, bucket) in BOUNDARY.items():
        row = details.get(no)
        if not check(f"明细存在 {no}", row is not None):
            continue
        check(f"{no} 逾期天数 = {days}", row["overdue_days"] == days, row["overdue_days"])
        check(f"{no} 档位 = {bucket}", row["bucket"] == bucket, row["bucket"])
    check("SETTLED 不进表", "AP-M121-000e" not in details)
    check("CANCELLED 不进表", "AP-M121-000f" not in details)
    check("基准日之后的期间（2026-12）不进表", "AP-M121-000g" not in details)
    check("明细未截断（13 条）", report["detail_truncated"] is False and report["detail_count"] == 13,
          f'{report["detail_truncated"]}/{report["detail_count"]}')

    # ---------------- ④ 供应商透视：各档之和 = 该行未付合计 ----------------
    suppliers = {row["supplier_name"]: row for row in report["suppliers"]}
    check("供应商行数 = 3", len(report["suppliers"]) == 3, len(report["suppliers"]))
    check("按未付合计倒序（丙 5600 > 甲 4400 > 乙 3700）",
          [row["supplier_name"] for row in report["suppliers"]] == list(SUPPLIERS),
          [row["supplier_name"] for row in report["suppliers"]])
    for name, (total, buckets, max_days) in SUPPLIERS.items():
        row = suppliers.get(name)
        if not check(f"供应商存在 {name}", row is not None):
            continue
        check(f"{name} 未付合计 = {total}", money(row["total_unpaid"]) == total, row["total_unpaid"])
        cell_sum = sum((money(cell["unpaid"]) for cell in row["buckets"]), Decimal("0"))
        check(f"{name} 各档之和 == 未付合计", cell_sum == total, f"{cell_sum} vs {total}")
        check(f"{name} 档位格子 7 个", [cell["key"] for cell in row["buckets"]] == KEYS,
              [cell["key"] for cell in row["buckets"]])
        for key in KEYS:
            want = Decimal(buckets.get(key, "0"))
            got = money(next(cell["unpaid"] for cell in row["buckets"] if cell["key"] == key))
            check(f"{name} {key} = {want}", got == want, got)
        check(f"{name} 最长逾期 = {max_days}", row["max_overdue_days"] == max_days, row["max_overdue_days"])

    # ---------------- ⑤ 汇总 = 明细按档求和（SQL 聚合 vs Rust 逐行分档，两条独立路径） ----------------
    by_detail = {}
    for row in report["details"]:
        by_detail[row["bucket"]] = by_detail.get(row["bucket"], Decimal("0")) + money(row["unpaid_amount"])
    for key, value in SUMMARY.items():
        check(f"汇总 {key} == 明细按档求和", summary[key] == by_detail.get(key, Decimal("0")),
              f'汇总 {summary[key]} vs 明细 {by_detail.get(key)}')

    # ---------------- ⑥ 基准日 +1：到期当天那张推进到 1-30，31 天的上界那张推进到 31-60 ----------------
    next_day = report_of(token, period="2026-11", as_of="2026-11-06")
    moved = {row["payable_no"]: row for row in next_day["details"]}["AP-M121-0002"]
    check("+1 天后 AP-M121-0002 逾期 1 天", moved["overdue_days"] == 1, moved["overdue_days"])
    check("+1 天后 AP-M121-0002 落 1-30", moved["bucket"] == "D0_30", moved["bucket"])
    next_summary = {cell["key"]: money(cell["unpaid"]) for cell in next_day["summary"]}
    model_next = expected_summary(shift=1)
    for key in KEYS:
        check(f"+1 天后 {key} = {model_next[key]}（Python 独立实现对照）",
              next_summary[key] == model_next[key], next_summary[key])
    check("+1 天后 未到期 = 1200（只走掉到期当天那张）", next_summary["NOT_DUE"] == Decimal("1200"),
          next_summary["NOT_DUE"])
    check("+1 天后 61-90 = 1700（600 + 跨过 60 天边界进来的 1100）",
          next_summary["D61_90"] == Decimal("1700"), next_summary["D61_90"])
    check("+1 天后 91-180 = 3000（800 + 跨过 90 天边界进来的 2200；180 天那张走到 181-365）",
          next_summary["D91_180"] == Decimal("3000"), next_summary["D91_180"])

    # ---------------- ⑦ 极端基准日：全部落进最后一档（列不错位） ----------------
    # 2099-12-31 的期间键是 2099-12 → 夹具里那张 period_code=2026-12 的单据此时**已纳入**
    # （基准日之后的期间才排除），所以总额是 13700 + 700 = 14400。
    far = report_of(token, as_of="2099-12-31")
    far_summary = {cell["key"]: money(cell["unpaid"]) for cell in far["summary"]}
    check("2099-12-31 全部落 365 天以上 = 14400", far_summary["OVER_365"] == Decimal("14400"),
          far_summary["OVER_365"])
    check("2099-12-31 前六档全 0",
          all(far_summary[key] == 0 for key in KEYS[:6]), far_summary)
    check("2099-12-31 单据数 = 14（未来期间那张也纳入）", far["doc_count"] == 14, far["doc_count"])

    # ---------------- ⑧ 导出 xlsx：11 列（7 档 + 前后各两列），逐格 == 报表 ----------------
    status, headers, payload = call(
        "GET", f"/business/erp/report/payable-aging/export?period=2026-11&as_of={AS_OF}", token=token
    )
    check("导出 HTTP 200", status == 200, status)
    check("导出 Content-Type 是 xlsx",
          "spreadsheetml.sheet" in (headers.get("Content-Type") or headers.get("content-type") or ""),
          headers.get("Content-Type"))
    check("导出文件名含期间 + 基准日",
          disposition_name(headers) == f"应付账款账龄分析表_2026-11_截止{AS_OF}.xlsx",
          disposition_name(headers))
    check("导出体是 zip（PK 开头，不是 JSON 错误）", payload[:2] == b"PK", payload[:40])

    grid = read_sheet(payload)
    want_headers = ["供应商", "单据数"] + LABELS + ["未付合计", "最长逾期天数"]
    check("表头 11 列逐字（7 档 + 供应商/单据数 + 未付合计/最长逾期）", grid[0] == want_headers, grid[0])
    check("行数 = 表头 + 3 供应商 + 合计", len(grid) == 5, len(grid))
    name_col = 0
    col = {name: index for index, name in enumerate(want_headers)}
    rows_by_name = {row[name_col]: row for row in grid[1:]}
    for name, (total, buckets, max_days) in SUPPLIERS.items():
        row = rows_by_name.get(name)
        if not check(f"导出行存在 {name}", row is not None):
            continue
        check(f"导出 {name} 单据数 = 导入条数",
              int(row[col["单据数"]]) == len([1 for no, (_d, _b) in BOUNDARY.items() if details[no]["supplier_name"] == name]),
              row[col["单据数"]])
        for key, label in zip(KEYS, LABELS):
            want = Decimal(buckets.get(key, "0"))
            check(f"导出 {name} {label} = {want}", money(row[col[label]]) == want, row[col[label]])
        check(f"导出 {name} 未付合计 = {total}", money(row[col["未付合计"]]) == total, row[col["未付合计"]])
        check(f"导出 {name} 最长逾期 = {max_days}", int(row[col["最长逾期天数"]]) == max_days,
              row[col["最长逾期天数"]])
    total_row = rows_by_name.get("合计")
    if check("导出行存在 合计", total_row is not None):
        check("合计行 单据数 = 13", int(total_row[col["单据数"]]) == 13, total_row[col["单据数"]])
        for key, label in zip(KEYS, LABELS):
            check(f"合计行 {label} == report.summary", money(total_row[col[label]]) == SUMMARY[key],
                  f'{total_row[col[label]]} vs {SUMMARY[key]}')
        check("合计行 未付合计 = 13700", money(total_row[col["未付合计"]]) == Decimal("13700"),
              total_row[col["未付合计"]])
        check("合计行 最长逾期 = 366", int(total_row[col["最长逾期天数"]]) == 366,
              total_row[col["最长逾期天数"]])

    # ---------------- ⑨ 出错路径 ----------------
    status, _, payload = call("GET", "/business/erp/report/payable-aging?as_of=2026-13-01", token=token)
    body = json.loads(payload.decode())
    check("非法 as_of → 400 + 10001", status == 400 and body.get("code") == 10001,
          f"{status}/{body.get('code')}")
    status, _, _ = call("GET", f"/business/erp/report/payable-aging/export?as_of={AS_OF}")
    check("导出无 token → 401", status == 401, status)

    print()
    if failures:
        print(f"FAILED {len(failures)} 条：")
        for name in failures:
            print("  -", name)
        return 1
    print("全部 PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
