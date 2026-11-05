#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-FIX-8A 应付账龄分析：隔离实例真机验收（HTTP 层）。

针对 `scripts/verify/fix8a_aging_db.sh` 造出来的库 `cloudpivot_aging8`
（生产快照副本 + 12 张边界夹具），实例 `scripts/verify/fix8a_aging_instance.sh`。

用法：
    BASE_URL=http://127.0.0.1:18144 python3 scripts/verify/fix8a_aging_e2e.py

校验的东西（每条都是硬断言，红就是红）：
 1. 基准日 2026-11-05 的分档金额逐格对（含 0/1/30/31/60/61/90/91 全部边界）
 2. 明细行的 `bucket` 与 `overdue_days` 在**每个边界**上自洽
 3. 汇总 = 明细按档求和（两条独立路径：SQL 聚合 vs Rust 逐行分档）
 4. 「基准日 +1 天」把 0 天那张从「未到期」推到「1-30」
 5. 排除样本真的排除（SETTLED / CANCELLED / 基准日之后的期间）
 6. 连跑两轮：除 `generated_at` 外逐字节一致
 7. 非法参数 400 + 10001；导出 xlsx 是 zip 且非空
"""

import json
import os
import sys
import urllib.error
import urllib.request
from collections import defaultdict
from decimal import Decimal

BASE_URL = os.environ.get("BASE_URL", "http://127.0.0.1:18144").rstrip("/")
USER = os.environ.get("TEST_USER", "admin")
PASSWORD = os.environ.get("TEST_PASS", "Admin@123456")
# 首登强制改密后的口令（隔离库专用；只在本机隔离实例上用）
NEW_PASSWORD = os.environ.get("NEW_PASS", "Aging@2026Fix8")
API = f"{BASE_URL}/api"

FAILS = []
CHECKS = 0


def check(name, cond, detail: object = ""):
    global CHECKS
    CHECKS += 1
    if cond:
        print(f"[PASS] {name}")
        return True
    print(f"[FAIL] {name}  {detail}")
    FAILS.append(f"{name} {detail}")
    return False


def http(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{API}{path}", data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
        req.add_header("x_access_token", token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as err:
        return err.code, err.read()


def _login(password):
    status, raw = http("POST", "/system/auth/login", {"username": USER, "password": password})
    if status != 200:
        return None
    payload = json.loads(raw)
    return (payload.get("data") or {}).get("access_token")


def login():
    """登录；顺带处理「首次登录必须先修改初始密码」（隔离库 bootstrap 建的 admin 带此标记）。

    脚本可重复跑：第二轮起初始口令已被改掉，会退回 NEW_PASSWORD。
    """
    token = _login(PASSWORD)
    if not token:
        token = _login(NEW_PASSWORD)
        if not token:
            print(f"登录失败：{USER} 的两个口令都不通")
            sys.exit(2)
        return token
    # 探测一次业务接口：403 + 「首次登录」→ 改密后重登
    status, raw = http("GET", "/business/erp/report/payable-aging", token=token)
    if status == 403 and "首次登录" in raw.decode("utf-8", "ignore"):
        status, body = http(
            "PUT",
            "/system/auth/password",
            {"old_password": PASSWORD, "new_password": NEW_PASSWORD},
            token=token,
        )
        if status != 200:
            print(f"修改初始密码失败 HTTP {status}: {body[:300]!r}")
            sys.exit(2)
        token = _login(NEW_PASSWORD)
        if not token:
            print("改密后用新口令仍登录失败")
            sys.exit(2)
        print("首次登录：已按强制改密把初始密码改成 NIK_ADMIN_PASSWORD→NEW_PASS")
    return token


def get_report(token, **params):
    query = "&".join(f"{k}={v}" for k, v in params.items() if v not in (None, ""))
    status, raw = http("GET", f"/business/erp/report/payable-aging?{query}", token=token)
    if status != 200:
        raise SystemExit(f"报表接口 HTTP {status}: {raw[:600]!r}")
    return json.loads(raw)["data"]


def dec(value):
    return Decimal(str(value or "0"))


def main():
    token = login()
    print(f"登录成功 user={USER} base={BASE_URL}")

    # ---------------- ① 基准日 2026-11-05 的分档金额 ----------------
    report = get_report(token, period="2026-11", as_of="2026-11-05")
    check("report_code", report["report_code"] == "PAYABLE_AGING", report["report_code"])
    check("as_of 回显", report["as_of"] == "2026-11-05", report["as_of"])
    check("as_of_source=QUERY", report["as_of_source"] == "QUERY", report["as_of_source"])
    check("期间回显", report["period"] == "2026-11" and report["period_name"] == "2026年11月",
          f'{report["period"]}/{report["period_name"]}')
    check("档位表 = 5 档且顺序固定",
          [b["key"] for b in report["buckets"]] == ["NOT_DUE", "D0_30", "D31_60", "D61_90", "OVER_90"],
          [b["key"] for b in report["buckets"]])

    summary = {cell["key"]: dec(cell["unpaid"]) for cell in report["summary"]}
    expect = {
        "NOT_DUE": Decimal("17000"),   # 未到期 12000 + 到期当天 5000
        "D0_30": Decimal("11000"),     # 逾期 1 天 3000 + 逾期 30 天 8000
        "D31_60": Decimal("17000"),    # 逾期 31 天 (9000-3000)=6000 + 逾期 60 天 11000
        "D61_90": Decimal("37000"),    # 逾期 61 天 15000 + 逾期 90 天 22000
        "OVER_90": Decimal("30000"),   # 逾期 91 天 (40000-10000)
    }
    for key, value in expect.items():
        check(f"档位 {key} 金额 = {value}", summary.get(key) == value, f'实际 {summary.get(key)}')
    check("未付合计 = 112000", dec(report["total_unpaid"]) == Decimal("112000"), report["total_unpaid"])
    check("逾期合计 = 95000", dec(report["overdue_unpaid"]) == Decimal("95000"), report["overdue_unpaid"])
    check("逾期占比 = 84.82", dec(report["overdue_ratio"]) == Decimal("84.82"), report["overdue_ratio"])
    check("单据数 = 9（3 张排除样本不算）", report["doc_count"] == 9, report["doc_count"])
    check("供应商数 = 3", report["supplier_count"] == 3, report["supplier_count"])

    # ---------------- ② 明细逐行边界 ----------------
    boundary = {
        "AP-FIX8A-000002": (0, "NOT_DUE"),      # 到期当天
        "AP-FIX8A-000003": (1, "D0_30"),        # 1 天
        "AP-FIX8A-000004": (30, "D0_30"),       # 30 天
        "AP-FIX8A-000005": (31, "D31_60"),      # 31 天
        "AP-FIX8A-000006": (60, "D31_60"),      # 60 天
        "AP-FIX8A-000007": (61, "D61_90"),      # 61 天
        "AP-FIX8A-000008": (90, "D61_90"),      # 90 天
        "AP-FIX8A-000009": (91, "OVER_90"),     # 91 天
        "AP-FIX8A-000001": (-15, "NOT_DUE"),    # 未到期
    }
    details = {row["payable_no"]: row for row in report["details"]}
    for no, (days, bucket) in boundary.items():
        row = details.get(no)
        if not check(f"明细存在 {no}", row is not None):
            continue
        check(f"{no} 逾期天数 = {days}", row["overdue_days"] == days, row["overdue_days"])
        check(f"{no} 档位 = {bucket}", row["bucket"] == bucket, row["bucket"])

    # 排除样本
    check("SETTLED 不进表", "AP-FIX8A-000010" not in details)
    check("CANCELLED 不进表", "AP-FIX8A-000011" not in details)
    check("基准日之后的期间（2026-12）不进表", "AP-FIX8A-000012" not in details)
    check("明细未截断", report["detail_truncated"] is False and report["detail_count"] == 9,
          f'{report["detail_truncated"]}/{report["detail_count"]}')

    # ---------------- ③ 汇总 = 明细按档求和（两条独立实现互证） ----------------
    by_detail = defaultdict(Decimal)
    for row in report["details"]:
        by_detail[row["bucket"]] += dec(row["unpaid_amount"])
    for key in expect:
        check(f"汇总 {key} == 明细按档求和（SQL 聚合 vs Rust 逐行分档）",
              summary[key] == by_detail[key], f'汇总 {summary[key]} vs 明细 {by_detail[key]}')
    total_from_details = sum((dec(row["unpaid_amount"]) for row in report["details"]), Decimal("0"))
    check("未付合计 == 明细未付之和", dec(report["total_unpaid"]) == total_from_details,
          f'{report["total_unpaid"]} vs {total_from_details}')

    # 供应商汇总与档位一致
    supplier_total = sum((dec(s["total_unpaid"]) for s in report["suppliers"]), Decimal("0"))
    check("供应商合计之和 == 未付合计", supplier_total == dec(report["total_unpaid"]),
          f"{supplier_total} vs {report['total_unpaid']}")
    for supplier in report["suppliers"]:
        cell_sum = sum((dec(cell["unpaid"]) for cell in supplier["buckets"]), Decimal("0"))
        check(f'供应商「{supplier["supplier_name"]}」各档之和 == 其未付合计',
              cell_sum == dec(supplier["total_unpaid"]), f"{cell_sum} vs {supplier['total_unpaid']}")

    # ---------------- ④ 基准日 +1 天：0 天那张跳到 1-30 ----------------
    next_day = get_report(token, period="2026-11", as_of="2026-11-06")
    moved = {row["payable_no"]: row for row in next_day["details"]}["AP-FIX8A-000002"]
    check("基准日 +1 天后 AP-FIX8A-000002 逾期 1 天", moved["overdue_days"] == 1, moved["overdue_days"])
    check("基准日 +1 天后 AP-FIX8A-000002 落 1-30 档", moved["bucket"] == "D0_30", moved["bucket"])
    next_summary = {cell["key"]: dec(cell["unpaid"]) for cell in next_day["summary"]}
    check("基准日 +1 天后 未到期 少 5000", next_summary["NOT_DUE"] == Decimal("12000"), next_summary["NOT_DUE"])
    # 000002 进 1-30（1 天）；同时 000004 从 30 天变 31 天 → 从 1-30 挪到 31-60（8000 走掉）
    check("基准日 +1 天后 1-30 = 3000+5000（8000 因跨 30→31 挪到 31-60）",
          next_summary["D0_30"] == Decimal("8000"), next_summary["D0_30"])
    check("基准日 +1 天后 31-60 = 14000（6000 + 迁入的 8000）",
          next_summary["D31_60"] == Decimal("14000"), next_summary["D31_60"])

    # ---------------- ⑤ 连跑两轮：除编制时间外一致 ----------------
    again = get_report(token, period="2026-11", as_of="2026-11-05")
    first = dict(report)
    second = dict(again)
    first.pop("generated_at")
    second.pop("generated_at")
    check("连跑两轮除 generated_at 外逐字一致", first == second)

    # ---------------- ⑥ 只传期间：基准日 = 期间末日 ----------------
    by_period = get_report(token, period="2026-10")
    check("只传 period=2026-10 时基准日 = 2026-10-31", by_period["as_of"] == "2026-10-31", by_period["as_of"])
    check("只传 period 时 as_of_source=PERIOD", by_period["as_of_source"] == "PERIOD", by_period["as_of_source"])

    # ---------------- ⑦ 非法参数 ----------------
    status, raw = http("GET", "/business/erp/report/payable-aging?as_of=2026-1-1", token=token)
    payload = json.loads(raw) if raw else {}
    check("非法 as_of → 400 + 10001", status == 400 and payload.get("code") == 10001,
          f"{status} {payload.get('code')} {payload.get('msg')}")
    status, raw = http("GET", "/business/erp/report/payable-aging?period=2026-13", token=token)
    payload = json.loads(raw) if raw else {}
    check("非法 period → 400 + 10001", status == 400 and payload.get("code") == 10001,
          f"{status} {payload.get('code')}")

    # ---------------- ⑧ 导出 xlsx ----------------
    status, blob = http("GET", "/business/erp/report/payable-aging/export?period=2026-11&as_of=2026-11-05",
                        token=token)
    check("导出 HTTP 200", status == 200, status)
    check("导出是 xlsx（PK 头）", blob[:2] == b"PK", blob[:8])
    check("导出非空（> 4KB）", len(blob) > 4096, len(blob))
    with open("/tmp/fix8a_payable_aging_export.xlsx", "wb") as fh:
        fh.write(blob)

    print(f"\n===== 断言 {CHECKS} 条，FAIL {len(FAILS)} =====")
    for item in FAILS:
        print(f"  - {item}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
