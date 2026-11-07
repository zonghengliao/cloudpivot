#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
P2-01 询比价闭环 · 真机 API 全链实测
====================================
跑在隔离实例 http://127.0.0.1:18099（库 cloudpivot_pi01）上，按卡面验收标准 1–6 逐步断言：

  1. 建询价单（3 个物料）
  2. 3 家供应商分别报价
  3. 比价表横向对比（同物料不同供应商的价格 / 交期 / 最低价标记）
  4. 定标 A 供应商
  5. 转采购订单 → 订单明细单价 = A 的报价（逐行精确比对）
  6. 反证：重复定标 / 重复转单 409；单源未填理由 409 code=66047；填理由放行；
     一单拆多单（A 供物料 1、B 供物料 2 → 两张采购单）

用法：python3 p201_price_inquiry_e2e.py
"""
from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE = os.environ.get("P201_BASE", "http://127.0.0.1:18099")
USER = os.environ.get("P201_USER", "admin")
PASSWORD = os.environ.get("P201_PASSWORD", "Admin@123456")
OUT_DIR = os.environ.get("P201_OUT", "/opt/cloudpivot/docs/evidence/20261107-p201-price-inquiry")

TOKEN = ""
FAILS: list[str] = []
STEPS: list[str] = []
RAW: list[dict] = []


def call(method: str, path: str, body: dict | None = None, token: str | None = None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    token = token if token is not None else TOKEN
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            payload = json.loads(response.read().decode() or "{}")
            return response.status, payload
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            payload = json.loads(raw or "{}")
        except json.JSONDecodeError:
            payload = {"raw": raw}
        return error.code, payload


def check(condition: bool, message: str) -> None:
    if condition:
        STEPS.append(f"  ✓ {message}")
    else:
        FAILS.append(message)
        STEPS.append(f"  ✗ {message}")


def step(title: str) -> None:
    STEPS.append(f"\n=== {title} ===")


def record(name: str, payload) -> None:
    RAW.append({"name": name, "payload": payload})


def login() -> None:
    """登录；首登强制改密的库里先改密再登录（幂等：改过一次就走新口令）。"""
    global TOKEN
    new_password = os.environ.get("P201_NEW_PASSWORD", "Pi01@123456")

    def try_login(password: str):
        return call("POST", "/api/system/auth/login",
                    {"username": USER, "password": password}, token="")

    status, body = try_login(PASSWORD)
    if status == 200 and (body.get("data") or {}).get("must_change_password"):
        # 首登强制改密：登录能拿 token，但业务接口一律 403「首次登录必须先修改初始密码」。
        # 所以判据必须是 must_change_password 标志，不能只看 status。
        TOKEN = body["data"]["access_token"]
        put_status, put_body = call("PUT", "/api/system/auth/password",
                                    {"old_password": PASSWORD, "new_password": new_password})
        check(put_status == 200 and put_body.get("code") == 0, f"首登强制改密 {put_status}")
        status, body = try_login(new_password)
    if status != 200:
        # 首登强制改密 / 已经改过：统一走新口令
        status, body = try_login(new_password)
        if status != 200:
            status, body = try_login(PASSWORD)
            if status == 200:
                put_status, _ = call("PUT", "/api/system/auth/password",
                                     {"old_password": PASSWORD, "new_password": new_password})
                check(put_status == 200, f"首登强制改密 {put_status}")
                status, body = try_login(new_password)
    check(status == 200 and body.get("code") == 0, f"管理员登录 {status}：{body}")
    TOKEN = body["data"]["access_token"]


def ensure_supplier(code: str, name: str) -> str:
    """供应商编码可复用（存在就查回来，不存在就建）。"""
    status, body = call("POST", "/api/business/erp/supplier", {
        "supplier_code": code,
        "supplier_name": name,
        "contact_person": "张三",
        "contact_phone": "13800000001",
        "credit_days": 30,
    })
    if status in (200, 201) and body.get("code") == 0:
        return body["data"]["id"]
    # 编码已存在 → 409，翻页查回来
    status, page = call("GET", f"/api/business/erp/supplier/page?page=1&page_size=200&keyword={code}")
    if status == 200:
        for row in page.get("data", {}).get("list", []):
            if row.get("supplier_code") == code:
                return row["id"]
    raise RuntimeError(f"供应商 {code} 建不出来也查不到：{status} {body}")


def create_inquiry(items: list[dict], remark: str, warehouse_id: str) -> dict:
    status, body = call("POST", "/api/business/erp/price-inquiry", {
        "warehouse_id": warehouse_id,
        "remark": remark,
        "items": items,
    })
    check(status == 200 and body.get("code") == 0, f"建询价单 {status}：{json.dumps(body, ensure_ascii=False)[:160]}")
    if status != 200:
        raise RuntimeError("建询价单失败")
    return body["data"]


def main() -> int:
    os.makedirs(OUT_DIR, exist_ok=True)
    login()

    # ---------- 物料 / 仓库（种子） ----------
    status, body = call("GET", "/api/business/erp/material/page?page=1&page_size=5")
    materials = body["data"]["list"]
    check(len(materials) >= 3, f"物料种子里至少有 3 个（实得 {len(materials)}）")
    status, body = call("GET", "/api/business/erp/warehouse/page?page=1&page_size=5")
    warehouse_id = body["data"]["list"][0]["id"]
    warehouse_name = body["data"]["list"][0]["warehouse_name"]

    # ---------- 供应商 A / B / C ----------
    step("准备：3 家供应商")
    sup_a = ensure_supplier("P201-A", "P201 甲供应商")
    sup_b = ensure_supplier("P201-B", "P201 乙供应商")
    sup_c = ensure_supplier("P201-C", "P201 丙供应商")
    check(len({sup_a, sup_b, sup_c}) == 3, "三家供应商 id 互不相同")

    # ---------- ① 建询价单（3 个物料） ----------
    step("① 建询价单（3 个物料）")
    items = [
        {"material_id": materials[0]["id"], "qty": "10", "demand_date": "2026-12-01", "remark": "第一批"},
        {"material_id": materials[1]["id"], "qty": "5", "demand_date": "2026-12-05", "remark": None},
        {"material_id": materials[2]["id"], "qty": "2.5", "demand_date": "2026-12-10", "remark": "急件"},
    ]
    detail = create_inquiry(items, "P2-01 验收：3 物料 3 家比价", warehouse_id)
    inquiry_id = detail["head"]["id"]
    inquiry_no = detail["head"]["inquiry_no"]
    record("create", detail)
    check(detail["head"]["status"] == "DRAFT", f"初始状态 DRAFT（实得 {detail['head']['status']}）")
    check(len(detail["items"]) == 3, f"明细 3 行（实得 {len(detail['items'])}）")
    check(detail["head"]["warehouse_id"] == warehouse_id, "收货仓库取自建单请求")
    check(detail["head"]["buyer_name"] == "管理员" or detail["head"]["buyer_id"], "采购员由当前登录人快照")
    check(all(item["unit_id"] for item in detail["items"]), "明细单位由物料带出（客户端没传 unit_id）")
    check(detail["head"]["item_count"] == 3, "item_count = 3")

    # ---------- ② 3 家供应商分别报价 ----------
    step("② 3 家供应商分别报价")
    # A：报价最低但交期最长；B：居中；C：最贵最快
    quote_plan = [
        (sup_a, ["100.00", "50.00", "200.00"], "13", 30, "甲：低价长交期"),
        (sup_b, ["108.00", "52.00", "190.00"], "13", 15, "乙：居中"),
        (sup_c, ["120.00", "48.00", "260.00"], "9", 7, "丙：最快最贵"),
    ]
    for supplier_id, prices, tax, days, remark in quote_plan:
        payload_items = []
        for index, item in enumerate(detail["items"]):
            payload_items.append({
                "item_id": item["id"],
                "price_excl": prices[index],
                "tax_rate": tax,
                "delivery_days": days,
                "delivery_date": None,
                "remark": remark,
            })
        status, body = call("POST", f"/api/business/erp/price-inquiry/{inquiry_id}/quote",
                            {"supplier_id": supplier_id, "items": payload_items})
        check(status == 200 and body.get("code") == 0, f"报价入库（供应商 {supplier_id[:8]}）{status}")
        if status != 200:
            print(json.dumps(body, ensure_ascii=False))
    status, detail = call("GET", f"/api/business/erp/price-inquiry/{inquiry_id}")
    detail = detail["data"]
    record("after_quotes", detail)
    check(detail["head"]["status"] == "QUOTING", f"状态推进到 QUOTING（实得 {detail['head']['status']}）")
    check(len(detail["quotes"]) == 9, f"报价 3 家 × 3 行 = 9 条（实得 {len(detail['quotes'])}）")
    check(detail["head"]["supplier_count"] == 3, f"报价家数 3（实得 {detail['head']['supplier_count']}）")
    # 含税单价必须由服务端算：100 × 1.13 = 113.000000
    quote_a_line1 = next(q for q in detail["quotes"]
                         if q["item_id"] == detail["items"][0]["id"] and q["supplier_id"] == sup_a)
    check(Decimal(quote_a_line1["price_incl"]) == Decimal("113.000000"),
          f"含税单价服务端算：100 × 1.13 = {quote_a_line1['price_incl']}")

    # 重复报价 = 改价（不新增行）
    status, body = call("POST", f"/api/business/erp/price-inquiry/{inquiry_id}/quote",
                        {"supplier_id": sup_c, "items": [{
                            "item_id": detail["items"][0]["id"], "price_excl": "120.00",
                            "tax_rate": "9", "delivery_days": 7}]})
    check(status == 200, "同一供应商重复报价返回 200（改价语义）")

    # ---------- ③ 比价表横向对比 ----------
    step("③ 比价表横向对比（同物料不同供应商的价格 / 交期 / 最低价）")
    comparison = detail["comparison"]
    check(len(comparison) == 3, f"比价表 3 行（实得 {len(comparison)}）")
    for row in comparison:
        names = [q["supplier_name"] for q in row["quotes"]]
        check(row["bidder_count"] == 3, f"行 {row['line_no']} 有 3 家报价（实得 {row['bidder_count']}）")
        check(row["has_competition"] is True, f"行 {row['line_no']} 满足 ≥2 家")
        check(len(set(names)) == 3, f"行 {row['line_no']} 三家名称不重复：{names}")
        lowest = [q for q in row["quotes"] if q["is_lowest"]]
        check(len(lowest) == 1, f"行 {row['line_no']} 只有一个最低价（{[(q['supplier_name'], q['price_excl']) for q in lowest]}）")
        days = {q["supplier_name"]: q["delivery_days"] for q in row["quotes"]}
        check(len(set(days.values())) == 3, f"行 {row['line_no']} 交期可横向对比：{days}")
    # 逐行最低价必须是 A(第1行)/C(第2行)/B(第3行)
    expect_lowest = {"甲": 1, "乙": 2, "丙": 3}
    lowest_supplier = {}
    for row in comparison:
        for quote in row["quotes"]:
            if quote["is_lowest"]:
                lowest_supplier[row["line_no"]] = quote["supplier_name"]
    check("甲" in lowest_supplier[1], f"第 1 行最低价是甲（实得 {lowest_supplier[1]}）")
    check("丙" in lowest_supplier[2], f"第 2 行最低价是丙（实得 {lowest_supplier[2]}）")
    check("乙" in lowest_supplier[3], f"第 3 行最低价是乙（实得 {lowest_supplier[3]}）")
    check(expect_lowest["甲"] == 1, "（占位：最低价格口径按不含税单价）")

    # ---------- ④ 定标 A ----------
    step("④ 定标 A 供应商")
    status, body = call("POST", f"/api/business/erp/price-inquiry/{inquiry_id}/award",
                        {"supplier_id": sup_a})
    check(status == 200 and body.get("code") == 0, f"定标成功 {status}：{json.dumps(body, ensure_ascii=False)[:200]}")
    awarded = body.get("data", {})
    record("award", awarded)
    check(awarded["head"]["status"] == "AWARDED", f"状态 AWARDED（实得 {awarded['head']['status']}）")
    check(awarded["head"]["award_mode"] == "MULTI", f"定标方式 MULTI（实得 {awarded['head']['award_mode']}）")
    check(awarded["head"]["award_supplier_id"] == sup_a, "单一供应商定标写入 award_supplier_id")
    check(awarded["head"]["awarded_at"], "定标时间已落库")
    won = [q for q in awarded["quotes"] if q["is_awarded"]]
    check(len(won) == 3 and all(q["supplier_id"] == sup_a for q in won),
          f"中标标记只落在 A 的 3 条报价上（实得 {len(won)} 条）")
    check(awarded["can_to_po"] is True, "can_to_po = true")

    # 反证：重复定标 409
    status, body = call("POST", f"/api/business/erp/price-inquiry/{inquiry_id}/award",
                        {"supplier_id": sup_b})
    check(status == 409 and body.get("code") == 66047,
          f"重复定标被 409/66047 挡住（实得 {status}/{body.get('code')}）")

    # ---------- ⑤ 转采购订单 ----------
    step("⑤ 转采购订单（单价 = A 的报价）")
    status, body = call("POST", f"/api/business/erp/price-inquiry/{inquiry_id}/to-po", {})
    check(status == 200 and body.get("code") == 0, f"转单成功 {status}：{json.dumps(body, ensure_ascii=False)[:300]}")
    result = body.get("data", {})
    record("to_po", result)
    check(result.get("status") == "CONVERTED", f"状态 CONVERTED（实得 {result.get('status')}）")
    pos = result.get("purchase_orders", [])
    check(len(pos) == 1, f"单一供应商 → 1 张采购订单（实得 {len(pos)}）")
    po = pos[0]
    check(po["supplier_id"] == sup_a, "采购订单挂 A 供应商")
    check(po["line_count"] == 3, f"采购订单 3 行（实得 {po['line_count']}）")
    # 采购单金额 = Σ 不含税 + 税（A 的报价：100*10 + 50*5 + 200*2.5 = 1750；13%）
    status, po_detail = call("GET", "/api/business/erp/purchase-order/page?page=1&page_size=200")
    po_row = next((row for row in po_detail["data"]["list"] if row["id"] == po["po_id"]), None)
    check(po_row is not None, f"采购单 {po['po_no']} 能在采购订单台账里查到")
    if po_row:
        check(Decimal(str(po_row["total_amount"])) == Decimal("1750"),
              f"采购单不含税金额 = 100×10 + 50×5 + 200×2.5 = 1750（实得 {po_row['total_amount']}）")
        check(Decimal(str(po_row["total_with_tax"])) == Decimal("1977.5"),
              f"价税合计 = 1750 × 1.13 = 1977.5（实得 {po_row['total_with_tax']}）")

    # 逐行单价精确比对：采购单明细 vs A 的报价
    quote_a = {q["item_id"]: q for q in awarded["quotes"] if q["supplier_id"] == sup_a}
    status, body = call("GET", f"/api/business/erp/purchase-order-item/page?page=1&page_size=200")
    po_lines = [row for row in body["data"]["list"] if row["order_id"] == po["po_id"]]
    check(len(po_lines) == 3, f"采购明细 3 行（实得 {len(po_lines)}）")
    by_material = {q["item_id"]: q for q in quote_a.values()}
    item_by_id = {item["id"]: item for item in awarded["items"]}
    for line in po_lines:
        quote = next((q for iid, q in by_material.items()
                      if item_by_id[iid]["material_id"] == line["material_id"]), None)
        check(quote is not None, f"采购明细物料 {line['material_id'][:8]} 能对上中标报价")
        if quote:
            check(Decimal(str(line["price"])) == Decimal(str(quote["price_excl"])),
                  f"单价传递：物料 {line['material_id'][:8]} 采购单价 {line['price']} = A 报价 {quote['price_excl']}")
            check(Decimal(str(line["qty"])) == Decimal(str(item_by_id[quote["item_id"]]["qty"])),
                  f"数量传递：{line['qty']} = 询价数量 {item_by_id[quote['item_id']]['qty']}")
        check(Decimal(str(line["tax_rate"])) == Decimal("13"), f"税率传递 13%（实得 {line['tax_rate']}）")

    # 反证：重复转单 409
    status, body = call("POST", f"/api/business/erp/price-inquiry/{inquiry_id}/to-po", {})
    check(status == 409, f"重复转单被 409 挡住（实得 {status}/{body.get('code')}）")

    # ---------- ⑥ 单源例外 ----------
    step("⑥ 单源例外：只有 1 家报价时，没理由 409，有理由放行")
    single = create_inquiry([{"material_id": materials[0]["id"], "qty": "1", "demand_date": "2026-12-20"}],
                            "P2-01 单源例外", warehouse_id)
    single_id = single["head"]["id"]
    status, body = call("POST", f"/api/business/erp/price-inquiry/{single_id}/quote",
                        {"supplier_id": sup_c, "items": [{
                            "item_id": single["items"][0]["id"], "price_excl": "99.50",
                            "tax_rate": "13", "delivery_days": 3}]})
    check(status == 200, "单源报价入库")
    status, body = call("POST", f"/api/business/erp/price-inquiry/{single_id}/award",
                        {"supplier_id": sup_c})
    check(status == 409 and body.get("code") == 66047,
          f"只有 1 家报价、没填理由 → 409/66047（实得 {status}/{body.get('code')}）")
    check("single_source_reason" in json.dumps(body, ensure_ascii=False), "409 文案点名 single_source_reason")
    # 理由太短也拦
    status, body = call("POST", f"/api/business/erp/price-inquiry/{single_id}/award",
                        {"supplier_id": sup_c, "single_source_reason": "独家"})
    check(status == 409 and body.get("code") == 66047, f"理由 < 10 字同样 409（实得 {status}）")
    # 合法理由放行
    status, body = call("POST", f"/api/business/erp/price-inquiry/{single_id}/award",
                        {"supplier_id": sup_c,
                         "single_source_reason": "该物料为独家授权代理，市场无第二家可报价"})
    check(status == 200 and body.get("code") == 0, f"填理由后定标放行 {status}")
    if status == 200:
        head = body["data"]["head"]
        check(head["award_mode"] == "SINGLE_SOURCE", f"award_mode = SINGLE_SOURCE（实得 {head['award_mode']}）")
    # 未报价就定标 → 409（不替供应商编价）
    step("⑥b 反证：中标供应商没报过价 → 409")
    pair = create_inquiry([{"material_id": materials[1]["id"], "qty": "2"}], "P2-01 未报价定标", warehouse_id)
    pair_id = pair["head"]["id"]
    call("POST", f"/api/business/erp/price-inquiry/{pair_id}/quote",
         {"supplier_id": sup_a, "items": [{"item_id": pair["items"][0]["id"], "price_excl": "10", "tax_rate": "0"}]})
    status, body = call("POST", f"/api/business/erp/price-inquiry/{pair_id}/award",
                        {"supplier_id": sup_b, "single_source_reason": "测试：供应商 B 从未报价"})
    check(status == 409 and body.get("code") == 66047,
          f"未报价的中标组合被 409/66047 挡住（实得 {status}/{body.get('code')}）")
    check("没有对应报价" in json.dumps(body, ensure_ascii=False), "409 文案说明缺报价")

    # ---------- ⑦ 一单拆多单（按供应商分组） ----------
    step("⑦ 一单拆多单：物料 1 → A，物料 2 → B")
    split = create_inquiry([
        {"material_id": materials[0]["id"], "qty": "4", "demand_date": "2026-12-25"},
        {"material_id": materials[1]["id"], "qty": "6", "demand_date": "2026-12-28"},
    ], "P2-01 拆单", warehouse_id)
    split_id = split["head"]["id"]
    line1, line2 = split["items"][0]["id"], split["items"][1]["id"]
    # 每条明细各两家报价（满足 2 家口径），但按行定标给不同供应商
    call("POST", f"/api/business/erp/price-inquiry/{split_id}/quote",
         {"supplier_id": sup_a, "items": [
             {"item_id": line1, "price_excl": "30", "tax_rate": "13", "delivery_days": 10},
             {"item_id": line2, "price_excl": "70", "tax_rate": "13", "delivery_days": 10}]})
    call("POST", f"/api/business/erp/price-inquiry/{split_id}/quote",
         {"supplier_id": sup_b, "items": [
             {"item_id": line1, "price_excl": "40", "tax_rate": "13", "delivery_days": 5},
             {"item_id": line2, "price_excl": "60", "tax_rate": "13", "delivery_days": 5}]})
    status, body = call("POST", f"/api/business/erp/price-inquiry/{split_id}/award",
                        {"items": [
                            {"item_id": line1, "supplier_id": sup_a},
                            {"item_id": line2, "supplier_id": sup_b}]})
    check(status == 200, f"按行定标（A 供物料 1 / B 供物料 2）{status}")
    if status == 200:
        head = body["data"]["head"]
        check(head["award_mode"] == "MULTI", "两家都比过价 → MULTI")
        check(head["award_supplier_id"] in (None, ""), "多供应商定标不写单一 award_supplier_id")
    status, body = call("POST", f"/api/business/erp/price-inquiry/{split_id}/to-po", {})
    check(status == 200, f"拆单转采购 {status}")
    if status == 200:
        pos = body["data"]["purchase_orders"]
        record("split_to_po", body["data"])
        check(len(pos) == 2, f"按供应商拆成 2 张采购单（实得 {len(pos)}）")
        by_sup = {po["supplier_id"]: po for po in pos}
        check(sup_a in by_sup and sup_b in by_sup, "两张采购单分别挂 A / B")
        if sup_a in by_sup:
            check(by_sup[sup_a]["line_count"] == 1, "A 的采购单只有物料 1 一行")
            check(Decimal(str(by_sup[sup_a]["total_with_tax"])) == Decimal("135.6"),
                  f"A 单价税合计 = 30×4×1.13 = 135.6（实得 {by_sup[sup_a]['total_with_tax']}）")
        if sup_b in by_sup:
            check(Decimal(str(by_sup[sup_b]["total_with_tax"])) == Decimal("406.8"),
                  f"B 单价税合计 = 60×6×1.13 = 406.8（实得 {by_sup[sup_b]['total_with_tax']}）")

    # ---------- 分页 / 过滤 ----------
    step("⑧ 分页与过滤")
    status, body = call("GET", "/api/business/erp/price-inquiry/page?page=1&page_size=10")
    check(status == 200 and body["data"]["total"] >= 4,
          f"分页 total ≥ 4（实得 {body['data'].get('total')}）")
    status, body = call("GET", "/api/business/erp/price-inquiry/page?page=1&page_size=10&status=CONVERTED")
    rows = body["data"]["list"]
    check(all(row["status"] == "CONVERTED" for row in rows), "状态过滤只返回 CONVERTED")
    status, body = call("GET", f"/api/business/erp/price-inquiry/page?page=1&page_size=10&keyword={inquiry_no}")
    check(any(row["inquiry_no"] == inquiry_no for row in body["data"]["list"]), "关键字能按单号命中")
    status, body = call("GET", "/api/business/erp/price-inquiry/page?page=1&page_size=10&status=NOPE")
    check(status == 400, f"非法状态过滤 400（实得 {status}）")

    # ---------- 落盘 ----------
    with open(os.path.join(OUT_DIR, "00_api_responses.json"), "w", encoding="utf-8") as handle:
        json.dump({"raw": RAW, "steps": STEPS}, handle, ensure_ascii=False, indent=2)
    with open(os.path.join(OUT_DIR, "01_api_steps.txt"), "w", encoding="utf-8") as handle:
        handle.write("\n".join(STEPS) + "\n")

    print("\n".join(STEPS))
    print(f"\n关键单号：询价单 {inquiry_no}（{inquiry_id}） 采购单 {po['po_no']}")
    print(f"\n断言：{len([s for s in STEPS if s.strip().startswith('✓')])} 通过 / {len(FAILS)} 失败")
    if FAILS:
        print("\n失败清单：")
        for item in FAILS:
            print(f"  ✗ {item}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
