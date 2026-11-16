"""M3-03 报价单 → 销售订单转换（带行）E2E —— 真接口断言（打本机隔离实例）。

卡：t_de446ead（M3-03【M3·销售】报价单：→ 销售订单转换（带行））

链路（全部走真 HTTP + 真库 + 真权限，admin 是 SUPER_ADMIN 短路）：

```text
夹具：客户（ACTIVE）+ 物料
   ↓
① 建报价单（2 行：3×17.655@13% + 2×100@0%）→ 发出 SENT → 客户接受 ACCEPTED
   ↓
② 转单   POST /quotation/{id}/convert（**不带 body**）
        → 200：新订单 SO-…，DRAFT，金额与报价单同一分钱，明细 2 行逐行等值
        → 报价单回读 converted_so_id + converted_at（来源保留）
        → 订单详情（GET /sales-order/{id}）跨模块回读一致
   ↓
③ 幂等   再 POST 一次 → 同一张订单，meta.already_converted=true，订单分页 total 不变
   ↓
④ 守卫   DRAFT→409 / SENT→409（提示先标「客户已接受」）/ 不存在→404 / 非法 uuid→400
   ↓
⑤ 锁死   转单后的报价单：作废 409（会让订单失去来源）/ 删除 409（状态不允许删）
   ↓
⑥ 带参   order_date + delivery_date + remark → 订单备注 = 用户备注｜由报价单 QT-… 转换；
        交期早于下单日 400；不存在的销售员 400
```

口径说明（不是 bug）：
  · 只有 `ACCEPTED` 能转单（`SENT` 先标接受，那条边自带「未过期」守卫）；
  · 转单**不改报价单状态**（仍是 ACCEPTED），也不改订单状态（新建即 DRAFT）；
  · 重复转单是**幂等**的：回同一张订单 + `already_converted=true`，绝不建第二张；
  · 报价单没有币种列 ⇒ 转单按账套本位币记（`exchange_rate = 1`）。

用法：
    python3 scripts/verify/m303_quotation_convert_e2e.py
环境变量：M303_API_BASE（默认 http://127.0.0.1:18231/api）、
         M303_ADMIN_OLD_PASSWORD（默认 Admin@123456）、M303_ADMIN_PASSWORD（改后口令）
"""

import datetime
import json
import os
import urllib.error
import urllib.request

BASE = os.environ.get("M303_API_BASE", "http://127.0.0.1:18231/api")
ADMIN_OLD = os.environ.get("M303_ADMIN_OLD_PASSWORD", "Admin@123456")
ADMIN_NEW = os.environ.get("M303_ADMIN_PASSWORD", "M303@Convert1")
TOKEN = os.environ.get("M303_TOKEN")  # 可选：跳过登录（调试用）

PASS = 0
FAIL = 0


def check(name, ok, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  PASS {name}")
    else:
        FAIL += 1
        print(f"  FAIL {name} {detail}")


def eq(name, expected, actual):
    check(name, expected == actual, f"（期望 [{expected}]，实际 [{actual}]）")


def num(name, expected, actual, places=4):
    """NUMERIC 一律按数值比（rust_decimal 保留 DB 标度，字面量比会假红）。"""
    try:
        ok = abs(float(actual) - float(expected)) < 10 ** (-places) / 2
    except (TypeError, ValueError):
        ok = False
    check(name, ok, f"（期望 {expected}，实际 [{actual}]）")


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw)
        except json.JSONDecodeError:
            return err.code, raw


def ok_data(method, path, body=None, token=None):
    """成功调用：HTTP 200 + code == 0，返回 data；否则抛断言。"""
    status, resp = call(method, path, body, token)
    assert status == 200 and isinstance(resp, dict) and resp.get("code") == 0, \
        f"{method} {path} 失败：{status} {resp}"
    return resp["data"]


def err_of(method, path, body=None, token=None):
    """失败调用：返回 (http_status, code, msg)。"""
    status, resp = call(method, path, body, token)
    if not isinstance(resp, dict):
        return status, None, str(resp)
    return status, resp.get("code"), resp.get("msg") or resp.get("message") or ""


SKIP_REASON = "E2E：本脚本验状态机 / 转单，不验审批（quotation_approve 已下线）"


def send_quote(token, quotation_id):
    """发出报价单（M3-04 起 = 「没配流程 + 显式跳过审批」这条真实出口）。

    与人工 PATCH 走**同一条** `change_status_with` 路径（三道守卫照跑），
    只是审批闸换成 `IssueGate::Skipped`（理由由 `approval_gate` 产出并写进状态日志）。
    """
    return ok_data("POST", f"/business/erp/quotation/{quotation_id}/submit-approval",
                   {"skip_approval": True, "skip_reason": SKIP_REASON}, token)


def send_quote_err(token, quotation_id):
    """发出报价单（预期失败）：返回 (status, code, msg)。"""
    return err_of("POST", f"/business/erp/quotation/{quotation_id}/submit-approval",
                  {"skip_approval": True, "skip_reason": SKIP_REASON}, token)


def login_admin():
    status, body = call("POST", "/system/auth/login", {"username": "admin", "password": ADMIN_OLD})
    if status == 200 and isinstance(body, dict) and body.get("code") == 0:
        token = body["data"]["access_token"]
        check("空库首启：admin 首登 must_change_password=true", bool(body["data"].get("must_change_password")))
        status, changed = call(
            "PUT", "/system/auth/password",
            {"old_password": ADMIN_OLD, "new_password": ADMIN_NEW}, token)
        eq("首次登录改密", 0, (changed or {}).get("code"))
        status, body = call("POST", "/system/auth/login", {"username": "admin", "password": ADMIN_NEW})
        assert status == 200 and body.get("code") == 0, f"改密后登录失败：{status} {body}"
        return body["data"]["access_token"]
    status, body = call("POST", "/system/auth/login", {"username": "admin", "password": ADMIN_NEW})
    assert status == 200 and isinstance(body, dict) and body.get("code") == 0, \
        f"admin 登录失败：{status} {body}"
    return body["data"]["access_token"]


def fixture(token):
    """夹具：客户（ACTIVE）+ 物料（挂第一个计量单位）。"""
    customer = ok_data("POST", "/business/erp/customer",
                       {"name": "M303 转单演练客户", "remark": "M3-03 E2E 夹具"}, token)
    check("夹具客户建出（默认 ACTIVE）", customer.get("status") == "ACTIVE", str(customer.get("status")))
    units = ok_data("GET", "/business/erp/unit/page?page=1&page_size=1", token=token)
    assert units["list"], "空库应已种计量单位（V0137 主数据基线）"
    material = ok_data("POST", "/business/erp/material", {
        "material_name": "M303 转单演练物料",
        "unit_id": units["list"][0]["id"],
        "remark": "M3-03 E2E 夹具",
    }, token)
    check("夹具物料建出", bool(material.get("id")), str(material)[:120])
    return customer["id"], material["id"]


def today():
    return datetime.date.today()


def accepted_quotation(token, customer_id, material_id, item2_material=None):
    """建报价单（2 行）→ 发出 → 客户接受，返回 ACCEPTED 的报价单详情。"""
    items = [
        {"material_id": material_id, "qty": "3", "price": "17.655", "tax_rate": "13"},
        {"material_id": item2_material or material_id, "qty": "2", "price": "100", "tax_rate": "0"},
    ]
    body = {
        "customer_id": customer_id,
        "valid_until": (today() + datetime.timedelta(days=30)).isoformat(),
        "remark": "M3-03 E2E",
        "items": items,
    }
    quote = ok_data("POST", "/business/erp/quotation", body, token)
    quote = send_quote(token, quote['id'])
    eq("夹具报价单已发出", "SENT", quote["status"])
    quote = ok_data("PATCH", f"/business/erp/quotation/{quote['id']}/status", {"target_status": "ACCEPTED"}, token)
    eq("夹具报价单已接受（可转单）", "ACCEPTED", quote["status"])
    return quote


def main():
    global TOKEN
    token = TOKEN or login_admin()
    print("\n⓪ 夹具：客户 + 物料")
    customer_id, material_id = fixture(token)

    # ------------------------------------------------------------ ① 建单并接受
    print("\n① 建报价单 → 发出 → 客户接受")
    quote = accepted_quotation(token, customer_id, material_id)
    quote_id = quote["id"]
    num("报价单价税合计 = 259.8505", 259.8505, quote["total_with_tax"])
    eq("接受后仍未转单", None, quote["converted_so_id"])

    # ------------------------------------------------------------ ② 转单主链
    print("\n② 转单（不带 body）→ 新销售订单 + 带行 + 保留来源")
    conv = ok_data("POST", f"/business/erp/quotation/{quote_id}/convert", None, token)
    header = conv["header"]
    meta = conv["meta"]
    items = conv["items"]
    so_id = header["so_id"]
    so_no = header["so_no"]

    check("订单号形态 SO-yyyyMMdd-NNNNNN",
          so_no.startswith("SO-") and len(so_no) == len("SO-yyyyMMdd-000001"),
          f"{so_no}（长度 {len(so_no)}）")
    eq("订单状态 = DRAFT", "DRAFT", header["status"])
    eq("订单状态中文名 = 草稿", "草稿", header["status_label"])
    eq("客户沿用报价单客户", customer_id, header["customer_id"])
    eq("客户名称已回填（约定 13）", "M303 转单演练客户", header["customer_name"])
    num("订单不含税金额 = 报价单", 252.9650, header["total_amount"])
    num("订单税额 = 报价单", 6.8855, header["total_tax"])
    num("订单价税合计 = 报价单（同一分钱）", 259.8505, header["total_with_tax"])
    num("本位币单汇率 = 1", 1, header["exchange_rate"])
    num("本位币金额 = 原币金额", 259.8505, header["base_total_with_tax"])
    check("订单备注保留来源（由报价单 QT-… 转换）",
          quote["quote_no"] in (header["remark"] or ""), str(header["remark"]))

    eq("明细行数 = 报价行数（带行）", 2, len(items))
    eq("行号与报价行一一对应", [1, 2], [line["line_no"] for line in items])
    first = items[0]
    num("第 1 行金额 = 3×17.655 = 52.9650", 52.9650, first["amount"])
    num("第 1 行税额 = 52.9650×13% = 6.8855", 6.8855, first["tax_amount"])
    num("第 1 行单价原样搬", 17.655, first["price"])
    num("第 1 行税率原样搬", 13, first["tax_rate"])
    check("第 1 行物料 id 原样搬", first["material_id"] == material_id, first["material_id"])
    check("物料编码已回填（约定 13：*_id 必配 *_name）", bool(first["material_code"]), str(first["material_name"]))
    check("物料名称已回填", bool(first["material_name"]), str(first["material_name"]))
    num("第 2 行金额 = 2×100 = 200", 200, items[1]["amount"])
    num("第 2 行税额 = 0", 0, items[1]["tax_amount"])

    eq("meta.quotation_id 回链报价单", quote_id, meta["quotation_id"])
    eq("meta.quote_no", quote["quote_no"], meta["quote_no"])
    eq("meta.quotation_status = ACCEPTED", "ACCEPTED", meta["quotation_status"])
    eq("meta.line_count = 2", 2, meta["line_count"])
    eq("首次转单 already_converted = false", False, meta["already_converted"])
    eq("meta.quotation_expired = false", False, meta["quotation_expired"])

    # 报价单回读：来源真的落库
    detail = ok_data("GET", f"/business/erp/quotation/{quote_id}", token=token)
    eq("报价单 converted_so_id = 新订单", so_id, detail["converted_so_id"])
    check("报价单 converted_at 非空", bool(detail["converted_at"]), str(detail["converted_at"]))
    eq("转单不改报价单状态（仍 ACCEPTED）", "ACCEPTED", detail["status"])

    # 跨模块回读：订单在自己模块里查得到、金额与明细一致
    order = ok_data("GET", f"/business/erp/sales-order/{so_id}", token=token)
    eq("订单详情单号一致", so_no, order["so_no"])
    eq("订单详情状态 DRAFT", "DRAFT", order["status"])
    num("订单详情价税合计", 259.8505, order["total_with_tax"])
    eq("订单详情明细 2 行", 2, len(order["items"]))
    num("订单详情第 1 行金额", 52.9650, order["items"][0]["amount"])
    check("订单详情备注带来源", quote["quote_no"] in (order["remark"] or ""), str(order["remark"]))

    # ------------------------------------------------------------ ③ 幂等
    print("\n③ 幂等：再点一次不建第二张订单")
    again = ok_data("POST", f"/business/erp/quotation/{quote_id}/convert", None, token)
    eq("幂等回同一张订单", so_id, again["header"]["so_id"])
    eq("幂等回同一单号", so_no, again["header"]["so_no"])
    eq("幂等标记 already_converted = true", True, again["meta"]["already_converted"])
    eq("幂等路径也带明细", 2, len(again["items"]))
    page = ok_data("GET", f"/business/erp/sales-order/page?customer_id={customer_id}", token=token)
    eq("客户名下订单仍只有 1 张", 1, page["total"])

    # ------------------------------------------------------------ ④ 守卫
    print("\n④ 守卫：只有 ACCEPTED 能转单；失败路径一张订单都不建")
    draft = ok_data("POST", "/business/erp/quotation", {
        "customer_id": customer_id,
        "valid_until": (today() + datetime.timedelta(days=30)).isoformat(),
        "items": [{"material_id": material_id, "qty": "1", "price": "10", "tax_rate": "13"}],
    }, token)
    status, _code, msg = err_of("POST", f"/business/erp/quotation/{draft['id']}/convert", None, token)
    eq("DRAFT 转单 → 409", 409, status)
    check("DRAFT 文案点名当前状态", "DRAFT" in msg, msg)

    sent = send_quote(token, draft['id'])
    eq("（夹具）草稿已发出", "SENT", sent["status"])
    status, _code, msg = err_of("POST", f"/business/erp/quotation/{draft['id']}/convert", None, token)
    eq("SENT 转单 → 409", 409, status)
    check("SENT 文案提示先标「客户已接受」", "客户已接受" in msg, msg)
    check("SENT 文案给出目标状态码 ACCEPTED", "ACCEPTED" in msg, msg)

    status, _code, msg = err_of(
        "POST", "/business/erp/quotation/00000000-0000-0000-0000-000000000000/convert", None, token)
    eq("不存在的报价单 → 404", 404, status)
    status, _code, _msg = err_of("POST", "/business/erp/quotation/not-a-uuid/convert", None, token)
    eq("非法 uuid → 400", 400, status)
    page = ok_data("GET", f"/business/erp/sales-order/page?customer_id={customer_id}", token=token)
    eq("守卫路径没有多建订单（仍 1 张）", 1, page["total"])

    # ------------------------------------------------------------ ⑤ 转单后的报价单被锁死
    print("\n⑤ 已转单的报价单：不能作废、不能删（订单不能失去来源）")
    status, _code, msg = err_of("PATCH", f"/business/erp/quotation/{quote_id}/status",
                                {"target_status": "CANCELLED", "reason": "客户反悔，另开新单"}, token)
    eq("已转单仍可作废？→ 409", 409, status)
    check("作废文案点名已转成销售订单", "已转成销售订单" in msg, msg)
    status, _code, msg = err_of("DELETE", f"/business/erp/quotation/{quote_id}", None, token)
    eq("已接受（非 DRAFT/CANCELLED）删除 → 409", 409, status)

    # ------------------------------------------------------------ ⑥ 带参转单
    print("\n⑥ 带参转单：下单日 / 交期 / 备注 / 非法参数")
    quote2 = accepted_quotation(token, customer_id, material_id)
    order_date = today().isoformat()
    delivery = (today() + datetime.timedelta(days=7)).isoformat()
    conv2 = ok_data("POST", f"/business/erp/quotation/{quote2['id']}/convert", {
        "order_date": order_date,
        "delivery_date": delivery,
        "remark": "款到发货",
    }, token)
    eq("下单日 = 传入值", order_date, conv2["header"]["order_date"])
    eq("交期 = 传入值", delivery, conv2["header"]["delivery_date"])
    eq("备注 = 用户备注｜由报价单 QT-… 转换",
       f"款到发货｜由报价单 {quote2['quote_no']} 转换", conv2["header"]["remark"])
    check("第二张订单号与第一张不同", conv2["header"]["so_no"] != so_no, conv2["header"]["so_no"])

    quote3 = accepted_quotation(token, customer_id, material_id)
    status, _code, msg = err_of("POST", f"/business/erp/quotation/{quote3['id']}/convert", {
        "order_date": delivery,
        "delivery_date": order_date,
    }, token)
    eq("交期早于下单日 → 400", 400, status)
    check("文案点名字段", "delivery_date" in msg, msg)

    status, _code, msg = err_of("POST", f"/business/erp/quotation/{quote3['id']}/convert", {
        "salesperson_id": "11111111-1111-1111-1111-111111111111",
    }, token)
    eq("不存在的销售员 → 400", 400, status)
    check("文案点名销售员", "销售员" in msg, msg)

    # 参数失败的单没有被转（还能正常转）
    ok3 = ok_data("POST", f"/business/erp/quotation/{quote3['id']}/convert", None, token)
    eq("参数失败后同一张单仍可转", "DRAFT", ok3["header"]["status"])
    eq("第三张订单已转过标记为 false", False, ok3["meta"]["already_converted"])
    page = ok_data("GET", f"/business/erp/sales-order/page?customer_id={customer_id}", token=token)
    eq("最终订单数 = 3（主链 + 带参 + 参数失败重试）", 3, page["total"])

    print("\n==== M3-03 真接口断言结果：PASS=%d FAIL=%d ====" % (PASS, FAIL))
    if FAIL:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
