"""M3-02 报价单 CRUD + 状态机 E2E —— 真接口断言（打本机隔离实例）。

卡：t_0750ad31（M3-02【M3·销售】报价单：CRUD + 状态机 草稿→已发→已接受/已拒绝）

链路（全部走真 HTTP + 真库 + 真权限，admin 是 SUPER_ADMIN 短路）：

```text
建客户 / 建物料（夹具）
   ↓
① 建单     POST /quotation（2 行明细：3×17.655@13% + 2×100@0%）→ DRAFT / QT-yyyyMMdd-000001
   ↓        金额 252.9650 + 6.8855 = 259.8505（逐行四舍五入后汇总）
② 分页     GET  /quotation/page（status / customer_id / expired / 非法状态 400）
   ↓
③ 改单     PUT  /quotation/{id}（明细整单替换 → 金额重算）
   ↓
④ 发出     PATCH /quotation/{id}/status {SENT} → 已发出（明细与单价冻结）
   ↓        改单 409 / 删除 409（已发出的不许原地改、不许删）
⑤ 接受     PATCH {ACCEPTED} → 已接受（终态之一，只能再作废）
   ↓        已接受 → 已拒绝 = 非法迁移 409；已接受 → 作废（未转单）200
⑥ 拒绝     新单 SENT → REJECTED：无 reason 400、有 reason 200；拒绝是终态（再流转 409）
⑦ 作废/删  草稿 → CANCELLED（不要原因）→ 软删 200 → 详情 404；REJECTED 单删除 409
⑧ 过期护栏 建一张过期的（quote_date −10 天 / valid_until −1 天）→ expired=true；
           发出 409「有效期已过」→ 改期后可发出
```

口径说明（不是 bug）：
  · 「已过期」是**时间事实**（`today > valid_until`，CST），不是状态 —— 库里 status 仍是 DRAFT；
  · `REJECTED` / `CANCELLED` 是终态（`allowed_targets` 为空数组）；
  · 需要原因的边：客户拒绝、已发出 / 已接受后作废（草稿作废不要原因）。

用法：
    python3 scripts/verify/m302_quotation_state_e2e.py
环境变量：M302_API_BASE（默认 http://127.0.0.1:18230/api）、
         M302_ADMIN_OLD_PASSWORD（默认 Admin@123456）、M302_ADMIN_PASSWORD（改后口令）
"""

import datetime
import json
import os
import sys
import urllib.error
import urllib.request
from urllib.parse import quote

BASE = os.environ.get("M302_API_BASE", "http://127.0.0.1:18230/api")
ADMIN_OLD = os.environ.get("M302_ADMIN_OLD_PASSWORD", "Admin@123456")
ADMIN_NEW = os.environ.get("M302_ADMIN_PASSWORD", "M302@Quote1")
TOKEN = os.environ.get("M302_TOKEN")  # 可选：跳过登录（调试用）

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
                       {"name": "M302 报价演练客户", "remark": "M3-02 E2E 夹具"}, token)
    check("夹具客户建出（默认 ACTIVE）", customer.get("status") == "ACTIVE", str(customer.get("status")))
    units = ok_data("GET", "/business/erp/unit/page?page=1&page_size=1", token=token)
    assert units["list"], "空库应已种计量单位（V0137 主数据基线）"
    material = ok_data("POST", "/business/erp/material", {
        "material_name": "M302 报价演练物料",
        "unit_id": units["list"][0]["id"],
        "remark": "M3-02 E2E 夹具",
    }, token)
    check("夹具物料建出", bool(material.get("id")), str(material)[:120])
    return customer["id"], material["id"]


def today():
    return datetime.date.today()


def create_quote(token, customer_id, material_id, *, qty="3", price="17.655", rate="13",
                 quote_date=None, valid_until=None, extra_items=None):
    items = [{"material_id": material_id, "qty": qty, "price": price, "tax_rate": rate}]
    if extra_items:
        items.extend(extra_items)
    body = {
        "customer_id": customer_id,
        "valid_until": (valid_until or (today() + datetime.timedelta(days=30))).isoformat(),
        "remark": "M3-02 E2E",
        "items": items,
    }
    if quote_date is not None:
        body["quote_date"] = quote_date.isoformat()
    return body


def main():
    global TOKEN
    token = TOKEN or login_admin()
    print("\n⓪ 夹具：客户 + 物料")
    customer_id, material_id = fixture(token)

    # ---------------------------------------------------------------- ① 建单
    print("\n① 建单（DRAFT / 取号 / 服务端算钱）")
    body = create_quote(token, customer_id, material_id,
                        extra_items=[{"material_id": material_id, "qty": "2", "price": "100", "tax_rate": "0"}])
    quote = ok_data("POST", "/business/erp/quotation", body, token)
    quote_id = quote["id"]
    eq("状态 = DRAFT", "DRAFT", quote["status"])
    eq("状态中文名 = 草稿", "草稿", quote["status_label"])
    check("单号形态 QT-yyyyMMdd-000001",
          len(quote["quote_no"]) == len("QT-yyyyMMdd-000001") and quote["quote_no"].startswith("QT-"),
          f"{quote['quote_no']}（长度 {len(quote['quote_no'])}，应为 {len('QT-yyyyMMdd-000001')}）")
    check("单号日期段 = 今天（CST）", today().strftime("%Y%m%d") in quote["quote_no"], quote["quote_no"])
    num("不含税合计 = 3×17.655 + 2×100 = 252.9650", 252.9650, quote["total_amount"])
    num("税额合计 = 52.9650×13% 四舍五入 = 6.8855", 6.8855, quote["total_tax"])
    num("价税合计 = 259.8505（精确恒等）", 259.8505, quote["total_with_tax"])
    eq("明细 2 行", 2, len(quote["items"]))
    eq("行号从 1 起", [1, 2], [line["line_no"] for line in quote["items"]])
    eq("物料编码已回填（约定 13：*_id 必须配 *_name）", True, bool(quote["items"][0]["material_code"]))
    eq("客户名称已回填", "M302 报价演练客户", quote["customer_name"])
    eq("未过期", False, quote["expired"])
    eq("可编辑（DRAFT）", True, quote["editable"])
    eq("可删除（DRAFT）", True, quote["deletable"])
    eq("可流转目标 = SENT / CANCELLED", ["SENT", "CANCELLED"], quote["allowed_targets"])

    detail = ok_data("GET", f"/business/erp/quotation/{quote_id}", token=token)
    eq("详情回读单号一致", quote["quote_no"], detail["quote_no"])
    eq("详情带全量明细", 2, len(detail["items"]))
    num("详情明细含税 = 52.9650 + 6.8855", 59.8505, detail["items"][0]["with_tax"])

    # ---------------------------------------------------------------- ② 分页
    print("\n② 分页 / 筛选 / 参数校验")
    page = ok_data("GET", f"/business/erp/quotation/page?status=DRAFT&customer_id={customer_id}", token=token)
    eq("按状态 + 客户筛到 1 张", 1, page["total"])
    page = ok_data("GET", f"/business/erp/quotation/page?quote_no={quote['quote_no']}", token=token)
    eq("按单号模糊筛到 1 张", 1, page["total"])
    page = ok_data("GET", f"/business/erp/quotation/page?expired=false&customer_id={customer_id}", token=token)
    eq("未过期筛到 1 张", 1, page["total"])
    page = ok_data("GET", f"/business/erp/quotation/page?expired=true&customer_id={customer_id}", token=token)
    eq("已过期筛到 0 张", 0, page["total"])
    status, code, msg = err_of("GET", "/business/erp/quotation/page?status=WIN", token=token)
    eq("非法状态筛选 → 400", 400, status)
    check("非法状态文案列出域内值", "SENT" in msg and "REJECTED" in msg, msg)

    # ---------------------------------------------------------------- ③ 改单
    print("\n③ 改单（只有 DRAFT；明细整表替换 → 金额重算）")
    updated = ok_data("PUT", f"/business/erp/quotation/{quote_id}", {
        "items": [{"material_id": material_id, "qty": "4", "price": "25", "tax_rate": "6"}],
        "remark": "M3-02 E2E 改单",
    }, token)
    num("改成 4×25 = 100", 100, updated["total_amount"])
    num("税额 = 100×6% = 6", 6, updated["total_tax"])
    num("价税合计 = 106", 106, updated["total_with_tax"])
    eq("明细被整表替换（1 行）", 1, len(updated["items"]))
    eq("备注被更新", "M3-02 E2E 改单", updated["remark"])

    # ---------------------------------------------------------------- ④ 发出
    print("\n④ DRAFT → SENT（发出后冻结）")
    sent = ok_data("PATCH", f"/business/erp/quotation/{quote_id}/status", {"target_status": "SENT"}, token)
    eq("状态 = SENT", "SENT", sent["status"])
    eq("已发出不可编辑", False, sent["editable"])
    eq("已发出不可删除", False, sent["deletable"])
    eq("可流转目标 = ACCEPTED / REJECTED / CANCELLED", ["ACCEPTED", "REJECTED", "CANCELLED"], sent["allowed_targets"])
    status, code, msg = err_of("PUT", f"/business/erp/quotation/{quote_id}",
                               {"remark": "发出后还想改"}, token)
    eq("已发出改单 → 409", 409, status)
    check("改单 409 文案指向「作废后新开」", "新开" in msg or "作废" in msg, msg)
    status, code, msg = err_of("DELETE", f"/business/erp/quotation/{quote_id}", token=token)
    eq("已发出删除 → 409", 409, status)
    check("删除 409 文案说明要作废", "作废" in msg, msg)

    # ---------------------------------------------------------------- ⑤ 接受
    print("\n⑤ SENT → ACCEPTED（终态之一，只能再作废）")
    accepted = ok_data("PATCH", f"/business/erp/quotation/{quote_id}/status", {"target_status": "ACCEPTED"}, token)
    eq("状态 = ACCEPTED", "ACCEPTED", accepted["status"])
    eq("已接受只能作废", ["CANCELLED"], accepted["allowed_targets"])
    eq("已接受仍可删除？", False, accepted["deletable"])
    status, code, msg = err_of("PATCH", f"/business/erp/quotation/{quote_id}/status",
                               {"target_status": "REJECTED", "reason": "客户改主意"}, token)
    eq("已接受 → 已拒绝 = 非法迁移 409", 409, status)
    check("非法迁移文案给出可选去向", "CANCELLED" in msg, msg)

    # ---------------------------------------------------------------- ⑥ 拒绝
    print("\n⑥ 新单 SENT → REJECTED（拒绝必须给原因；拒绝是终态）")
    rejected_id = ok_data("POST", "/business/erp/quotation",
                          create_quote(token, customer_id, material_id), token)["id"]
    ok_data("PATCH", f"/business/erp/quotation/{rejected_id}/status", {"target_status": "SENT"}, token)
    status, code, msg = err_of("PATCH", f"/business/erp/quotation/{rejected_id}/status",
                               {"target_status": "REJECTED"}, token)
    eq("客户拒绝缺原因 → 400", 400, status)
    check("400 文案要求 reason", "原因" in msg, msg)
    rejected = ok_data("PATCH", f"/business/erp/quotation/{rejected_id}/status",
                       {"target_status": "REJECTED", "reason": "客户嫌贵"}, token)
    eq("状态 = REJECTED", "REJECTED", rejected["status"])
    eq("拒绝是终态（无可流转目标）", [], rejected["allowed_targets"])
    status, code, msg = err_of("PATCH", f"/business/erp/quotation/{rejected_id}/status",
                               {"target_status": "SENT"}, token)
    eq("拒绝了再发出 → 409", 409, status)
    check("终态文案说「已是终态」", "终态" in msg, msg)

    # ---------------------------------------------------------------- ⑦ 作废 / 删
    print("\n⑦ 草稿作废（不要原因）/ 软删 / 终态不许删")
    draft_id = ok_data("POST", "/business/erp/quotation",
                       create_quote(token, customer_id, material_id), token)["id"]
    cancelled = ok_data("PATCH", f"/business/erp/quotation/{draft_id}/status",
                        {"target_status": "CANCELLED"}, token)
    eq("草稿 → 作废 = 200（草稿作废不要原因）", "CANCELLED", cancelled["status"])
    ok_data("DELETE", f"/business/erp/quotation/{draft_id}", token=token)
    status, code, msg = err_of("GET", f"/business/erp/quotation/{draft_id}", token=token)
    eq("软删后详情 → 404", 404, status)
    status, code, msg = err_of("DELETE", f"/business/erp/quotation/{rejected_id}", token=token)
    eq("已拒绝单不许删 → 409", 409, status)
    check("409 文案列出可删状态", "DRAFT" in msg and "CANCELLED" in msg, msg)
    # 已接受（未转单）可以作废，但必须给原因
    status, code, msg = err_of("PATCH", f"/business/erp/quotation/{quote_id}/status",
                               {"target_status": "CANCELLED"}, token)
    eq("已接受作废缺原因 → 400", 400, status)
    final = ok_data("PATCH", f"/business/erp/quotation/{quote_id}/status",
                    {"target_status": "CANCELLED", "reason": "客户反悔，另开新单"}, token)
    eq("已接受 → 作废 = 200（需原因）", "CANCELLED", final["status"])

    # ---------------------------------------------------------------- ⑧ 过期护栏
    print("\n⑧ 过期护栏（过期是时间事实，不是状态）")
    expired_body = create_quote(token, customer_id, material_id,
                                quote_date=today() - datetime.timedelta(days=10),
                                valid_until=today() - datetime.timedelta(days=1))
    expired = ok_data("POST", "/business/erp/quotation", expired_body, token)
    eq("过期报价单能建（valid_until >= quote_date 即可）", True, expired["expired"])
    eq("状态仍是 DRAFT（没有 EXPIRED 状态）", "DRAFT", expired["status"])
    status, code, msg = err_of("PATCH", f"/business/erp/quotation/{expired['id']}/status",
                               {"target_status": "SENT"}, token)
    eq("过期单发出 → 409", 409, status)
    check("409 文案说「有效期已过」", "有效期" in msg, msg)
    # 改期后可发出
    ok_data("PUT", f"/business/erp/quotation/{expired['id']}", {
        "quote_date": today().isoformat(),
        "valid_until": (today() + datetime.timedelta(days=5)).isoformat(),
    }, token)
    refreshed = ok_data("GET", f"/business/erp/quotation/{expired['id']}", token=token)
    eq("改期后未过期", False, refreshed["expired"])
    fixed = ok_data("PATCH", f"/business/erp/quotation/{expired['id']}/status", {"target_status": "SENT"}, token)
    eq("改期后可以发出", "SENT", fixed["status"])

    # ---------------------------------------------------------------- ⑨ 入参校验
    print("\n⑨ 入参校验（400，不是 500）")
    bad = create_quote(token, customer_id, material_id)
    bad["items"] = []
    status, code, msg = err_of("POST", "/business/erp/quotation", bad, token)
    eq("空明细 → 400", 400, status)
    check("空明细文案「至少要 1 行」", "至少要 1 行" in msg, msg)

    status, code, msg = err_of("POST", "/business/erp/quotation",
                               create_quote(token, customer_id, material_id, qty="0"), token)
    eq("数量 0 → 400", 400, status)
    check("数量文案指名字段", "数量必须大于 0" in msg, msg)

    status, code, msg = err_of("POST", "/business/erp/quotation",
                               create_quote(token, customer_id, material_id, rate="101"), token)
    eq("税率 101 → 400", 400, status)

    past = create_quote(token, customer_id, material_id,
                        quote_date=today(), valid_until=today() - datetime.timedelta(days=1))
    status, code, msg = err_of("POST", "/business/erp/quotation", past, token)
    eq("有效期早于报价日 → 400", 400, status)
    check("文案说「不能早于」", "不能早于" in msg, msg)

    ghost = create_quote(token, "00000000-0000-4000-8000-000000000000", material_id)
    status, code, msg = err_of("POST", "/business/erp/quotation", ghost, token)
    eq("客户不存在 → 400", 400, status)
    check("文案说客户不存在", "客户不存在" in msg or "不存在" in msg, msg)

    status, code, msg = err_of("GET", "/business/erp/quotation/not-a-uuid", token=token)
    eq("非法 id → 400", 400, status)

    print(f"\n==== M3-02 E2E 断言：PASS={PASS} FAIL={FAIL} ====")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
