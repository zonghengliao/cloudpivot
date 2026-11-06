#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""SO-34 价格维护接口 + 订单自动带价 真实 HTTP 验收 —— 真实 token + 真实库。

前置：
    bash scripts/verify/so34_instance.sh up        # 库 cloudpivot_so34（cloudpivot_so33 的模板克隆）起在 :18241
    # 夹具复用 SO-33 的客户 / 物料（C2 = 无协议价客户，M4 = 无标准价物料）

跑法：
    python3 scripts/verify/so34_price_admin_e2e.py

退出码：0 = 全部断言通过；1 = 有 FAIL（逐条打印 expected / actual）。

断言分组：
  A. 协议价 CRUD（新建 / 分页筛选 / 唯一键 409 / 价格 0 → 400 / 非法 uuid → 400 /
     物料不存在 404 / 编辑 / 软删 / 软删后同键可重录 / 再删 404）
  B. 档位 CRUD（新建通用档 / 唯一键 409 / 上界 <= 下界 → 400 / 编辑清空上限 -1 /
     分页筛选 / 软删）
  C. 订单自动带价（SO-34 核心验收）：配协议价 → `/price/resolve` 命中 AGREED →
     用该价开会话订单 → 订单行 price 落库为该价；起订量不满足时降级
"""

import json
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18241"
AGREED = BASE + "/api/business/erp/price/agreed"
TIER = BASE + "/api/business/erp/price/tier"
RESOLVE = BASE + "/api/business/erp/price/resolve"
SALES_ORDER = BASE + "/api/business/erp/sales-order"

# 夹具固定 uuid（SO-33 夹具，见 scripts/verify/so33_price_fixture.sql）
C1 = "f0330000-0000-4000-8000-000000000001"  # 已有协议价（不用于本卡新建，避免撞键）
C2 = "f0330000-0000-4000-8000-000000000002"  # 无协议价 → 本卡在它身上配
M1 = "f0330000-0000-4000-8000-000000000011"  # 有协议价 / 有标准价
M4 = "f0330000-0000-4000-8000-000000000014"  # 一无所有（无协议价、无档位、无标准价）

failures = []
checks = 0


def _payload(raw: str) -> dict:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError:
        return {"raw": raw}
    return value if isinstance(value, dict) else {"raw": value}


def http(method, url, body=None, token=None):
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status, _payload(response.read().decode())
    except urllib.error.HTTPError as error:
        return error.code, _payload(error.read().decode())


def check(name, ok, detail=""):
    global checks
    checks += 1
    if ok:
        print("  PASS  %s" % name)
    else:
        failures.append("%s %s" % (name, detail))
        print("  FAIL  %s   %s" % (name, detail))


def data_of(payload):
    return payload.get("data") if isinstance(payload, dict) else None


def same_num(actual, expected) -> bool:
    """金额 / 数量口径：响应是 `"9.999900"`（单据 6 位）或 `"9.9999"`（价格 4 位），
    数学值相同即算过 —— 断言的是"带出的价对不对"，不是小数位宽。"""
    try:
        return abs(float(actual) - float(expected)) < 1e-9
    except (TypeError, ValueError):
        return False


def login():
    status, payload = http(
        "POST", BASE + "/api/system/auth/login", {"username": "admin", "password": "Admin@123456"}
    )
    token = (data_of(payload) or {}).get("access_token") if isinstance(payload, dict) else None
    if status != 200 or not token:
        raise SystemExit("登录失败：HTTP %s %s" % (status, payload))
    print("登录成功（admin / SUPER_ADMIN 短路权限），token 长度 %d" % len(token))
    return token


def cleanup(token):
    """软删本卡夹具在 (C2, M4) 上的协议价与档位 —— 让脚本可重复跑。

    只删 `material_id = M4` 且（客户为空或 C2）的行，不碰 SO-33 夹具与其它物料。
    """
    removed = 0
    for base in (AGREED, TIER):
        status, payload = http(
            "GET", "%s/page?material_id=%s&page=1&page_size=200" % (base, M4), token=token
        )
        rows = (data_of(payload) or {}).get("list") or []
        for row in rows:
            if row.get("customer_id") not in (None, C2):
                continue
            code, _ = http("DELETE", "%s/%s" % (base, row["id"]), token=token)
            if code == 200:
                removed += 1
    print("清场：软删 %d 条本卡价格夹具（material = %s）" % (removed, M4))


def main():
    token = login()

    # 清场：本卡在 (C2, M4) 上的一切价格配置都先软删（幂等，可重复跑）
    cleanup(token)

    # ==================== A. 协议价 CRUD ====================
    print("\n[A] 客户协议价 CRUD")
    row_id = None

    # A1 新建（C2 × M4，起订量 50，生效日 2026-01-01）
    status, payload = http(
        "POST",
        AGREED,
        {
            "customer_id": C2,
            "material_id": M4,
            "price": "7.7777",
            "min_qty": "50",
            "effective_from": "2026-01-01",
            "effective_to": "2026-12-31",
            "remark": "SO-34 e2e 协议价",
        },
        token,
    )
    created = data_of(payload) or {}
    row_id = created.get("id")
    check("A1 新建协议价 200/code0", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))
    check("A1 回显单价 7.7777", same_num(created.get("price"), "7.7777"), str(created.get("price")))
    check("A1 回显客户名（LEFT JOIN 补齐）", bool(created.get("customer_name")), str(created))
    check("A1 回显物料编码", bool(created.get("material_code")), str(created))
    check("A1 状态缺省 ACTIVE", created.get("status") == "ACTIVE", str(created.get("status")))

    # A2 分页筛选（按客户 + 物料，必须能查到刚建的那条）
    status, payload = http(
        "GET", "%s/page?customer_id=%s&material_id=%s&page=1&page_size=20" % (AGREED, C2, M4), token=token
    )
    page = data_of(payload) or {}
    ids = [item.get("id") for item in (page.get("list") or [])]
    check("A2 分页 200", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))
    check("A2 列表命中新建行", row_id in ids, "%s not in %s" % (row_id, ids))
    check("A2 total >= 1", (page.get("total") or 0) >= 1, str(page.get("total")))

    # A3 唯一键冲突 → 409（同客户 + 物料 + 起订量 + 生效日）
    status, payload = http(
        "POST",
        AGREED,
        {"customer_id": C2, "material_id": M4, "price": "1.0000", "min_qty": "50", "effective_from": "2026-01-01"},
        token,
    )
    check("A3 重复键 409", status == 409, "%s %s" % (status, payload))
    check("A3 冲突文案点名起订量/生效日", "起订量" in str(payload.get("msg")), str(payload.get("msg")))

    # A4 价格 0 → 400（价格表里 0 会让取价静默命中 0 元）
    status, payload = http(
        "POST",
        AGREED,
        {"customer_id": C2, "material_id": M4, "price": "0", "effective_from": "2026-02-01"},
        token,
    )
    check("A4 price=0 → 400", status == 400, "%s %s" % (status, payload))

    # A5 非法 uuid → 400
    status, payload = http(
        "POST", AGREED, {"customer_id": C2, "material_id": "not-a-uuid", "price": "1.5"}, token
    )
    check("A5 非法 material_id → 400", status == 400, "%s %s" % (status, payload))

    # A6 物料不存在 → 404（不能变成 500 外键错）
    status, payload = http(
        "POST",
        AGREED,
        {"customer_id": C2, "material_id": "f0340000-0000-4000-8000-00000000dead", "price": "1.5"},
        token,
    )
    check("A6 物料不存在 → 404", status == 404, "%s %s" % (status, payload))

    # A7 编辑（改价 + 清空失效日）
    status, payload = http(
        "PUT",
        "%s/%s" % (AGREED, row_id),
        {"price": "8.8888", "effective_to": ""},
        token,
    )
    updated = data_of(payload) or {}
    check("A7 编辑 200", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))
    check("A7 单价改成功", same_num(updated.get("price"), "8.8888"), str(updated.get("price")))
    check("A7 失效日被清空（长期有效）", updated.get("effective_to") is None, str(updated.get("effective_to")))
    check("A7 未传的起订量沿用", same_num(updated.get("min_qty"), "50"), str(updated.get("min_qty")))

    # A8 取价命中刚配的协议价（qty 满足起订量 50）
    status, payload = http(
        "GET", "%s?customer_id=%s&material_id=%s&qty=50&date=2026-06-01" % (RESOLVE, C2, M4), token=token
    )
    resolved = data_of(payload) or {}
    check("A8 取价命中 AGREED", resolved.get("source") == "AGREED", str(resolved))
    check("A8 取价单价 = 8.8888", same_num(resolved.get("price"), "8.8888"), str(resolved.get("price")))
    check("A8 source_id = 协议价行 id", resolved.get("source_id") == row_id, str(resolved.get("source_id")))

    # A9 起订量不满足（qty=10 < 50）→ 降级 NONE（M4 无档位无标准价）
    status, payload = http(
        "GET", "%s?customer_id=%s&material_id=%s&qty=10&date=2026-06-01" % (RESOLVE, C2, M4), token=token
    )
    resolved_low = data_of(payload) or {}
    check("A9 起订量不满足 → NONE(200)", status == 200 and resolved_low.get("source") == "NONE", str(resolved_low))
    check("A9 NONE 时 price=null", resolved_low.get("price") is None, str(resolved_low.get("price")))

    # A10 软删
    status, payload = http("DELETE", "%s/%s" % (AGREED, row_id), token=token)
    check("A10 软删 200", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))

    status, payload = http(
        "GET", "%s/page?customer_id=%s&material_id=%s&page=1&page_size=20" % (AGREED, C2, M4), token=token
    )
    ids = [item.get("id") for item in ((data_of(payload) or {}).get("list") or [])]
    check("A10 软删后列表不再出现", row_id not in ids, "%s still in %s" % (row_id, ids))

    # A11 再删 → 404
    status, payload = http("DELETE", "%s/%s" % (AGREED, row_id), token=token)
    check("A11 重复删除 → 404", status == 404, "%s %s" % (status, payload))

    # A12 软删后同键可重录（部分唯一索引只活行）
    status, payload = http(
        "POST",
        AGREED,
        {"customer_id": C2, "material_id": M4, "price": "9.9999", "min_qty": "50", "effective_from": "2026-01-01"},
        token,
    )
    recreated = data_of(payload) or {}
    row_id = recreated.get("id")
    check("A12 软删后同键可重录", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))

    # ==================== B. 档位 CRUD ====================
    print("\n[B] 阶梯价 / 促销价 CRUD")
    tier_id = None

    # B1 新建通用档（customer_id 缺省 = 所有客户）
    status, payload = http(
        "POST",
        TIER,
        {
            "material_id": M4,
            "tier_type": "TIER",
            "min_qty": "0",
            "max_qty": "100",
            "price": "11.0000",
            "effective_from": "2026-01-01",
        },
        token,
    )
    tier = data_of(payload) or {}
    tier_id = tier.get("id")
    check("B1 新建档位 200", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))
    check("B1 通用档 customer_id = null", tier.get("customer_id") is None, str(tier.get("customer_id")))
    check("B1 档位类型缺省 TIER", tier.get("tier_type") == "TIER", str(tier.get("tier_type")))

    # B2 同键重复 → 409
    status, payload = http(
        "POST",
        TIER,
        {"material_id": M4, "tier_type": "TIER", "min_qty": "0", "max_qty": "50", "price": "10", "effective_from": "2026-01-01"},
        token,
    )
    check("B2 重复档位键 → 409", status == 409, "%s %s" % (status, payload))

    # B3 上界 <= 下界 → 400
    status, payload = http(
        "POST",
        TIER,
        {"material_id": M4, "tier_type": "TIER", "min_qty": "100", "max_qty": "100", "price": "10", "effective_from": "2026-01-01"},
        token,
    )
    check("B3 max_qty == min_qty → 400", status == 400, "%s %s" % (status, payload))

    # B4 同一物料多档：第二个档位（[100, +∞)）
    status, payload = http(
        "POST",
        TIER,
        {"material_id": M4, "tier_type": "TIER", "min_qty": "100", "price": "9.0000", "effective_from": "2026-01-01"},
        token,
    )
    tier2 = data_of(payload) or {}
    check("B4 同物料第二档 200", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))

    # B5 分页：同物料两档都在，且按 min_qty 升序（列表第一行 = 低档）
    status, payload = http("GET", "%s/page?material_id=%s&page=1&page_size=20" % (TIER, M4), token=token)
    page = data_of(payload) or {}
    tiers = page.get("list") or []
    mins = [str(item.get("min_qty")) for item in tiers]
    check("B5 同物料两档都在", len(tiers) >= 2, str(mins))
    check("B5 档位按起订量升序（列表可读成一条阶梯）", mins == sorted(mins, key=float), str(mins))

    # B6 编辑：max_qty = -1 → 清空成无上限
    status, payload = http("PUT", "%s/%s" % (TIER, tier_id), {"max_qty": -1}, token=token)
    edited = data_of(payload) or {}
    check("B6 编辑档位 200", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))
    check("B6 max_qty=-1 → null（无上限）", edited.get("max_qty") is None, str(edited.get("max_qty")))

    # B7 档位参与取价（客户为空、qty=200 命中第二档 9.0000）
    status, payload = http("GET", "%s?material_id=%s&qty=200&date=2026-06-01" % (RESOLVE, M4), token=token)
    resolved = data_of(payload) or {}
    check("B7 通用阶梯命中 TIER_COMMON", resolved.get("source") == "TIER_COMMON", str(resolved))
    check("B7 高档位单价 9.0000", same_num(resolved.get("price"), "9.0000"), str(resolved.get("price")))

    # B8 软删两个档位
    status, payload = http("DELETE", "%s/%s" % (TIER, tier_id), token=token)
    check("B8 删除档位 200", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))
    status, payload = http("DELETE", "%s/%s" % (TIER, tier2.get("id")), token=token)
    check("B8 删除第二档 200", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))

    # ==================== C. 订单自动带价（SO-34 核心验收） ====================
    print("\n[C] 销售订单自动带价")
    # C1 协议价 9.9999（A12 重录的那条，起订量 50）在 qty=60 时被取到
    status, payload = http(
        "GET", "%s?customer_id=%s&material_id=%s&qty=60&date=2026-06-01" % (RESOLVE, C2, M4), token=token
    )
    resolved = data_of(payload) or {}
    picked = str(resolved.get("price"))
    check("C1 取价 AGREED 9.9999", resolved.get("source") == "AGREED" and same_num(picked, "9.9999"), str(resolved))

    # C2 用取到的价开单（前端 `refetchLinePrice` 的行为：把 price 填进行里再提交）
    status, payload = http(
        "POST",
        SALES_ORDER,
        {
            "customer_id": C2,
            "order_date": "2026-06-01",
            "remark": "SO-34 e2e 自动带价",
            "items": [{"material_id": M4, "qty": "60", "price": picked, "tax_rate": "13"}],
        },
        token,
    )
    order = data_of(payload) or {}
    order_id = order.get("id")
    items = order.get("items") or []
    check("C2 开单 200/code0", status == 200 and payload.get("code") == 0, "%s %s" % (status, payload))
    check("C2 订单行单价 = 取价结果 9.9999", bool(items) and same_num(items[0].get("price"), "9.9999"), str(items))

    # C3 订单详情回读：价格真的落库了（不是只有响应里对）
    status, payload = http("GET", "%s/%s" % (SALES_ORDER, order_id), token=token)
    detail = data_of(payload) or {}
    detail_items = detail.get("items") or []
    check("C3 详情回读单价 9.9999", bool(detail_items) and same_num(detail_items[0].get("price"), "9.9999"), str(detail_items))
    check(
        "C3 金额 = 60 × 9.9999（服务端算）",
        bool(detail_items) and same_num(detail_items[0].get("amount"), "599.994"),
        str(detail_items[0].get("amount") if detail_items else None),
    )

    # C4 换客户（C1 有协议价但那是 M1 的）→ C1 × M4 无协议价/无档位 → NONE（前端提示手填）
    status, payload = http(
        "GET", "%s?customer_id=%s&material_id=%s&qty=60&date=2026-06-01" % (RESOLVE, C1, M4), token=token
    )
    resolved_other = data_of(payload) or {}
    check("C4 换客户后无价 → NONE", resolved_other.get("source") == "NONE", str(resolved_other))

    # ==================== 汇总 ====================
    print("\n=========================")
    if failures:
        print("FAIL %d / %d" % (len(failures), checks))
        for item in failures:
            print("  - %s" % item)
        return 1
    print("ALL_PASS %d / %d" % (checks, checks))
    return 0


if __name__ == "__main__":
    sys.exit(main())
