#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""SO-43 客户对账单：真实 token + 真实 HTTP 的验收脚本（隔离实例 127.0.0.1:18043）。

断言口径（期初 + 发生 = 期末）：
    period 2026-10-01 ~ 2026-10-31 / CNY
    期初 = 12000（AR1 10000 九月份挂账 + AR3 2000 九月份挂账）
    明细 = 挂账 +5000 / 退货 -1000 / 坏账 -2000 / 收款 -3000 / 退款 +500
    期末 = 11500
"""
import base64
import io
import json
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import zipfile

BASE = "http://127.0.0.1:18043"
CUSTOMER = "aa430000-0000-4000-8000-000000000001"
ADMIN = "admin"
PASSWORD = "Admin@123456"
PASSWORD_CHANGED = "Admin@123456so43"

PASS, FAIL = [], []


def check(name, ok, detail=""):
    if ok:
        PASS.append(name)
        print(f"[PASS] {name}" + (f" | {detail}" if detail else ""))
    else:
        FAIL.append(name)
        print(f"[FAIL] {name} | {detail}")


def req(method, path, token=None, body=None, raw=False):
    url = BASE + path
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as resp:
            payload = resp.read()
            if raw:
                return resp.status, payload, dict(resp.headers)
            return resp.status, json.loads(payload.decode()), dict(resp.headers)
    except urllib.error.HTTPError as error:
        payload = error.read()
        if raw:
            return error.code, payload, dict(error.headers)
        try:
            return error.code, json.loads(payload.decode()), dict(error.headers)
        except Exception:
            return error.code, {"raw": payload.decode("utf-8", "replace")}, dict(error.headers)


def login():
    """登录；若被首登强制改密拦住，先按真实流程改密再重新登录。"""
    passwords = [PASSWORD, PASSWORD_CHANGED]
    status, body = 0, {}
    token = None
    for candidate in passwords:
        status, body, _ = req("POST", "/api/system/auth/login",
                              body={"username": ADMIN, "password": candidate})
        if status == 200:
            token = (body.get("data") or {}).get("access_token")
            if token:
                current = candidate
                break
    else:
        print("登录失败：", status, body)
        sys.exit(2)

    data = body.get("data") or {}
    if data.get("must_change_password"):
        status, changed, _ = req("PUT", "/api/system/auth/password", token,
                                 {"old_password": current, "new_password": PASSWORD_CHANGED})
        print("首登改密：status=%s msg=%s" % (status, str(changed)[:160]))
        status, body, _ = req("POST", "/api/system/auth/login",
                              body={"username": ADMIN, "password": PASSWORD_CHANGED})
        data = body.get("data") or {}
        token = data.get("access_token")
    return token


def q(params):
    return "?" + urllib.parse.urlencode(params)


# ---------- XLSX 读取（纯标准库：sharedStrings + sheet1） ----------
def xlsx_texts(payload):
    with zipfile.ZipFile(io.BytesIO(payload)) as book:
        names = book.namelist()
        shared = []
        if "xl/sharedStrings.xml" in names:
            xml = book.read("xl/sharedStrings.xml").decode("utf-8")
            for si in re.findall(r"<si>(.*?)</si>", xml, re.S):
                shared.append("".join(re.findall(r"<t[^>]*>(.*?)</t>", si, re.S)))
        sheet_name = [n for n in names if n.startswith("xl/worksheets/sheet")]
        sheet = book.read(sorted(sheet_name)[0]).decode("utf-8")
        cells = re.findall(r'<c r="([A-Z]+\d+)"([^>]*)>(.*?)</c>', sheet, re.S)
        out = {}
        for ref, attrs, inner in cells:
            if 't="s"' in attrs:
                index = re.search(r"<v>(\d+)</v>", inner)
                if index:
                    out[ref] = shared[int(index.group(1))]
            else:
                value = re.search(r"<v>(.*?)</v>", inner)
                if value:
                    out[ref] = value.group(1)
        return out


def main():
    token = login()
    print("token 已获取（长度 %d）" % len(token))

    # ---------- ① 对账单查询 ----------
    path = "/api/business/erp/receivable/statement" + q({
        "customer_id": CUSTOMER, "period_from": "2026-10-01", "period_to": "2026-10-31"})
    status, body, _ = req("GET", path, token)
    check("GET /statement 200", status == 200, f"status={status} body={str(body)[:300]}")
    data = body.get("data", {}) or {}
    opening = data.get("opening_balance")
    closing = data.get("closing_balance")
    items = data.get("items", [])
    detail = data.get("detail", {}) or {}
    identity = detail.get("identity", {}) or {}
    check("期初 = 12000", str(opening) in ("12000.00", "12000"), f"opening={opening}")
    check("期末 = 11500", str(closing) in ("11500.00", "11500"), f"closing={closing}")
    check("明细 5 笔", len(items) == 5, f"items={[ (i['kind'], str(i['amount'])) for i in items ]}")
    kinds = [item["kind"] for item in items]
    check("事件顺序（日期/类型）", kinds == ["INVOICE", "RETURN", "WRITE_OFF", "RECEIPT", "REFUND"],
          f"kinds={kinds}")
    check("逐笔余额", [str(i["balance"]) for i in items] ==
          ["17000.00", "16000.00", "14000.00", "11000.00", "11500.00"],
          f"balances={[str(i['balance']) for i in items]}")
    check("自校验 passed", identity.get("passed") is True, f"identity={identity}")
    check("自校验 diff = 0", str(identity.get("diff")) in ("0", "0.00"), f"diff={identity.get('diff')}")
    check("期末两条路径一致", str(identity.get("closing_balance")) == str(identity.get("closing_by_ledger")),
          f"{identity.get('closing_balance')} vs {identity.get('closing_by_ledger')}")
    check("发生额合计 5000/3000/1000", (str(detail.get("total_invoiced")) == "5000.00"
          and str(detail.get("total_received")) == "3000.00"
          and str(detail.get("total_returned")) == "1000.00"), f"detail={detail}")
    check("币种 CNY", data.get("currency") == "CNY", f"currency={data.get('currency')}")
    check("客户带纳税人识别号", (data.get("customer") or {}).get("tax_no") == "91110000SO4300001",
          f"customer={data.get('customer')}")
    check("期间天数 31", (data.get("period") or {}).get("days") == 31, f"period={data.get('period')}")

    # 作废应收整笔剔除：AR-SO43-0004 不许出现在明细里
    docs = [item["doc_no"] for item in items]
    check("作废应收被剔除", "AR-SO43-0004" not in docs, f"docs={docs}")

    # ---------- ② 期初/期末口径：换一个只含九月的期间 ----------
    status, body, _ = req("GET", "/api/business/erp/receivable/statement" + q({
        "customer_id": CUSTOMER, "period_from": "2026-09-01", "period_to": "2026-09-30"}), token)
    sep = body.get("data", {}) or {}
    check("九月：期初 0 / 期末 12000",
          str(sep.get("opening_balance")) == "0.00" and str(sep.get("closing_balance")) == "12000.00",
          f"opening={sep.get('opening_balance')} closing={sep.get('closing_balance')}")

    # ---------- ③ 空期间（没有往来不是错误） ----------
    status, body, _ = req("GET", "/api/business/erp/receivable/statement" + q({
        "customer_id": CUSTOMER, "period_from": "2025-01-01", "period_to": "2025-01-31"}), token)
    empty = body.get("data", {}) or {}
    check("空期间：0 期初 0 明细 0 期末",
          status == 200 and str(empty.get("opening_balance")) == "0.00"
          and empty.get("items") == [] and str(empty.get("closing_balance")) == "0.00",
          f"status={status} opening={empty.get('opening_balance')} items={empty.get('items')}")

    # ---------- ④ Excel 导出 ----------
    status, payload, headers = req(
        "GET", "/api/business/erp/receivable/statement/export" + q({
            "customer_id": CUSTOMER, "period_from": "2026-10-01", "period_to": "2026-10-31"}),
        token, raw=True)
    ctype = {k.lower(): v for k, v in headers.items()}.get("content-type", "")
    check("导出 200 + xlsx 魔数", status == 200 and payload[:4] == b"PK\x03\x04",
          f"status={status} magic={payload[:4]!r} ctype={ctype}")
    check("导出 Content-Type 是 spreadsheet",
          "spreadsheetml" in ctype or "octet-stream" in ctype, f"ctype={ctype}")
    texts = {}
    try:
        texts = xlsx_texts(payload)
    except Exception as error:  # noqa: BLE001
        check("xlsx 可解包", False, f"{error}")
    values = list(texts.values())
    joined = " | ".join(values)
    check("xlsx 含客户编码", "CUS-SO43" in joined, f"head={joined[:200]}")
    check("xlsx 含期初 12000.00", "12000.00" in joined, f"values={values[:20]}")
    check("xlsx 含期末 11500.00", "11500.00" in joined, f"values={values[-10:]}")
    check("xlsx 含 5 笔明细的余额列",
          all(v in joined for v in ["17000.00", "16000.00", "14000.00", "11000.00"]),
          "余额列齐全")
    check("xlsx 行数 = 信息行 + 期初 + 5 明细 + 期末 = 8",
          len({re.sub(r"\d+", "", ref) for ref in texts}) == 6, f"列数={len({re.sub(r'\d+','',r) for r in texts})}")

    # format 参数：xlsx 认、其他必须 400（不能静默给 xlsx）
    status, payload, _ = req(
        "GET", "/api/business/erp/receivable/statement/export" + q({
            "customer_id": CUSTOMER, "period_from": "2026-10-01", "period_to": "2026-10-31",
            "format": "xlsx"}), token, raw=True)
    check("format=xlsx → 200 xlsx", status == 200 and payload[:4] == b"PK\x03\x04",
          f"status={status} magic={payload[:4]!r}")
    status, body, _ = req(
        "GET", "/api/business/erp/receivable/statement/export" + q({
            "customer_id": CUSTOMER, "period_from": "2026-10-01", "period_to": "2026-10-31",
            "format": "csv"}), token)
    check("format=csv → 400", status == 400, f"status={status} body={str(body)[:160]}")

    # ---------- ⑤ 签发 / 重发 / 确认 / 异议 ----------
    status, body, _ = req("POST", "/api/business/erp/receivable/statement/confirm", token, {
        "customer_id": CUSTOMER, "period_from": "2026-10-01", "period_to": "2026-10-31",
        "remark": "SO-43 联调签发"})
    sent = body.get("data", {}) or {}
    order_id = sent.get("id")
    check("签发 200 + SENT", status == 200 and sent.get("status") == "SENT",
          f"status={status} body={str(body)[:300]}")
    check("签发金额 = 期末 11500", str(sent.get("total_amount")) == "11500.00", f"total={sent.get('total_amount')}")
    check("签发快照带期初/发生额",
          str(sent.get("opening_balance")) == "12000.00"
          and str(sent.get("total_invoiced")) == "5000.00"
          and sent.get("item_count") == 5, f"snapshot={ {k: sent.get(k) for k in ('opening_balance','total_invoiced','item_count')} }")

    status, body, _ = req("POST", "/api/business/erp/receivable/statement/confirm", token, {
        "customer_id": CUSTOMER, "period_from": "2026-10-01", "period_to": "2026-10-31"})
    again = body.get("data", {}) or {}
    check("SENT 可重发且同一行", status == 200 and again.get("id") == order_id,
          f"status={status} id={again.get('id')} vs {order_id}")

    status, body, _ = req("PUT", f"/api/business/erp/receivable/statement/confirm/{order_id}/confirm",
                          token, {"confirmed_by": "  "})
    check("确认缺确认人 → 400", status == 400, f"status={status} body={str(body)[:200]}")

    status, body, _ = req("PUT", f"/api/business/erp/receivable/statement/confirm/{order_id}/confirm",
                          token, {"confirmed_by": "张三（客户财务）"})
    confirmed = body.get("data", {}) or {}
    check("客户确认 → CONFIRMED", status == 200 and confirmed.get("status") == "CONFIRMED",
          f"status={status} body={str(body)[:300]}")
    check("确认落 confirmed_by / confirmed_at",
          confirmed.get("confirmed_by") == "张三（客户财务）" and confirmed.get("confirmed_at"),
          f"by={confirmed.get('confirmed_by')} at={confirmed.get('confirmed_at')}")

    status, body, _ = req("PUT", f"/api/business/erp/receivable/statement/confirm/{order_id}/dispute",
                          token, {"dispute_reason": "我们对不上这笔"})
    check("已确认后再提异议 → 409", status == 409, f"status={status} body={str(body)[:200]}")

    status, body, _ = req("POST", "/api/business/erp/receivable/statement/confirm", token, {
        "customer_id": CUSTOMER, "period_from": "2026-10-01", "period_to": "2026-10-31"})
    check("已确认后再签发 → 409", status == 409, f"status={status} body={str(body)[:200]}")

    # 九月单独签发 → 异议
    status, body, _ = req("POST", "/api/business/erp/receivable/statement/confirm", token, {
        "customer_id": CUSTOMER, "period_from": "2026-09-01", "period_to": "2026-09-30"})
    sep_id = (body.get("data") or {}).get("id")
    check("九月签发 200", status == 200 and sep_id, f"status={status}")
    status, body, _ = req("PUT", f"/api/business/erp/receivable/statement/confirm/{sep_id}/dispute",
                          token, {"dispute_reason": "差"})
    check("异议原因过短 → 400", status == 400, f"status={status} body={str(body)[:200]}")
    status, body, _ = req("PUT", f"/api/business/erp/receivable/statement/confirm/{sep_id}/dispute",
                          token, {"dispute_reason": "AR-SO43-0003 那笔坏账我们不认"})
    disputed = body.get("data", {}) or {}
    check("异议 → DISPUTED", status == 200 and disputed.get("status") == "DISPUTED",
          f"status={status} body={str(body)[:200]}")
    check("异议原因落库", disputed.get("dispute_reason") == "AR-SO43-0003 那笔坏账我们不认",
          f"reason={disputed.get('dispute_reason')}")

    # ---------- ⑥ 台账分页 ----------
    status, body, _ = req("GET", "/api/business/erp/receivable/statement/confirm/page"
                          + q({"customer_id": CUSTOMER}), token)
    page = body.get("data", {}) or {}
    check("台账 2 行", status == 200 and page.get("total") == 2, f"status={status} total={page.get('total')}")
    status, body, _ = req("GET", "/api/business/erp/receivable/statement/confirm/page"
                          + q({"customer_id": CUSTOMER, "status": "disputed"}), token)
    page = body.get("data", {}) or {}
    check("按状态筛（小写也认）", status == 200 and page.get("total") == 1,
          f"status={status} total={page.get('total')}")
    status, body, _ = req("GET", "/api/business/erp/receivable/statement/confirm/page"
                          + q({"customer_id": CUSTOMER, "status": "ARCHIVED"}), token)
    check("非法状态 → 400", status == 400, f"status={status} body={str(body)[:160]}")

    # ---------- ⑦ 参数校验 ----------
    cases = [
        ("缺 customer_id", {"period_from": "2026-10-01"}, 400),
        ("非法 customer_id", {"customer_id": "not-a-uuid", "period_from": "2026-10-01"}, 400),
        ("缺 period_from", {"customer_id": CUSTOMER}, 400),
        ("期间倒置", {"customer_id": CUSTOMER, "period_from": "2026-10-31", "period_to": "2026-10-01"}, 400),
        ("非法日期", {"customer_id": CUSTOMER, "period_from": "2026-13-01"}, 400),
        ("客户不存在", {"customer_id": "aa430000-0000-4000-8000-0000000000ff",
                        "period_from": "2026-10-01"}, 404),
    ]
    for name, params, expect in cases:
        status, body, _ = req("GET", "/api/business/erp/receivable/statement" + q(params), token)
        check(f"{name} → {expect}", status == expect, f"status={status} body={str(body)[:160]}")

    # ---------- ⑧ 无 token → 401 ----------
    status, _, _ = req("GET", "/api/business/erp/receivable/statement" + q({
        "customer_id": CUSTOMER, "period_from": "2026-10-01"}), None)
    check("无 token → 401", status == 401, f"status={status}")

    # 汇总
    print("\n===== 汇总：PASS %d / FAIL %d =====" % (len(PASS), len(FAIL)))
    if FAIL:
        print("失败项：" + "; ".join(FAIL))
        sys.exit(1)


if __name__ == "__main__":
    main()
