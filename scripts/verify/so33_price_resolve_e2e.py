#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""SO-33 取价解析接口（5 级）真实 HTTP 验收 —— 真实 token + 真实库。

前置：
    bash scripts/verify/so33_instance.sh up          # 库 cloudpivot_so33 起在 :18239
    PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so33 \
        -v ON_ERROR_STOP=1 -f scripts/verify/so33_price_fixture.sql

跑法：
    python3 scripts/verify/so33_price_resolve_e2e.py

退出码：0 = 全部断言通过；1 = 有 FAIL（逐条打印 expected / actual）。

断言分两组：
  A. 5 级各级命中（①协议价 / ②客户专属阶梯 / ③通用阶梯 / ④标准价 / ⑤无价）
  B. 边界与确定性（有效期两端 / 档位左闭右开 / 多条取 min_qty 最大 / 同量取最近生效 /
     停用行不参与 / 起订量边界 / qty 缺省 400 / 物料不存在 404 / 重复调用同结果）
"""

import json
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE = "http://127.0.0.1:18239"
RESOLVE = BASE + "/api/business/erp/price/resolve"

# 夹具固定 uuid（见 scripts/verify/so33_price_fixture.sql）
C1 = "f0330000-0000-4000-8000-000000000001"  # 有协议价 / 有专属档位
C2 = "f0330000-0000-4000-8000-000000000002"  # 无协议价 / 无专属档位
M1 = "f0330000-0000-4000-8000-000000000011"  # 协议价 6 条（含停用 / 限期 / 同量两条）
M2 = "f0330000-0000-4000-8000-000000000012"  # 专属 TIER+PROMO、通用 TIER
M3 = "f0330000-0000-4000-8000-000000000013"  # 只有标准价 12.5000
M4 = "f0330000-0000-4000-8000-000000000014"  # 一无所有
A_MQ100 = "f0330000-0000-4000-8000-000000000102"
A_NEWEST = "f0330000-0000-4000-8000-000000000104"
A_LIMITED = "f0330000-0000-4000-8000-000000000105"
T_PROMO = "f0330000-0000-4000-8000-000000000202"
T_COMMON = "f0330000-0000-4000-8000-000000000203"

failures = []
checks = 0


def _payload(raw: str) -> dict:
    """响应体 → dict（永不为 str：非对象 / 非 JSON 都塞进 {"raw": …}）。"""
    try:
        value = json.loads(raw)
    except json.JSONDecodeError:
        return {"raw": raw}
    if isinstance(value, dict):
        return value
    return {"raw": value}


def http(
    method: str, url: str, body: "dict | None" = None, token: "str | None" = None
) -> "tuple[int, dict]":
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return response.status, _payload(response.read().decode())
    except urllib.error.HTTPError as error:
        return error.code, _payload(error.read().decode())


def login():
    status, payload = http(
        "POST",
        BASE + "/api/system/auth/login",
        {"username": "admin", "password": "Admin@123456"},
    )
    token = (payload.get("data") or {}).get("access_token") if isinstance(payload, dict) else None
    if status != 200 or not token:
        raise SystemExit("登录失败：HTTP %s %s" % (status, payload))
    print("登录成功（admin / SUPER_ADMIN 短路权限），token 长度 %d" % len(token))
    return token


def call(token, **params):
    """调取价接口：params 里值为 None 的项直接不传（验证「缺省」语义）。"""
    query = "&".join(
        "%s=%s" % (key, value) for key, value in params.items() if value is not None
    )
    return http("GET", RESOLVE + ("?" + query if query else ""), token=token)


def check(label, ok, detail):
    global checks
    checks += 1
    mark = "PASS" if ok else "FAIL"
    print("[%s] %s —— %s" % (mark, label, detail))
    if not ok:
        failures.append(label)


def expect_hit(label, payload, price, source, source_id):
    data = (payload.get("data") or {}) if isinstance(payload, dict) else {}
    ok = (
        payload.get("code") == 0
        and data.get("source") == source
        and data.get("price") is not None
        and Decimal(str(data["price"])) == Decimal(price)
        and data.get("source_id") == source_id
    )
    check(
        label,
        ok,
        "期望 price=%s source=%s id=%s，实际 %s"
        % (price, source, source_id, json.dumps(data, ensure_ascii=False)),
    )
    return data


def main():
    token = login()

    print("\n=== A. 5 级各级命中 ===")
    # ① 协议价：qty=150 → 三条满足（0 / 50 / 100），取 min_qty 最大的 100
    _, body = call(token, customer_id=C1, material_id=M1, qty="150", date="2026-01-01")
    expect_hit("① AGREED（多条取 min_qty 最大）", body, "9.0000", "AGREED", A_MQ100)

    # ② 客户专属阶梯：同段 TIER 7.0 与 PROMO 6.5 并存 → 取 PROMO
    _, body = call(token, customer_id=C1, material_id=M2, qty="50", date="2026-01-01")
    expect_hit("② TIER_CUSTOMER（PROMO 压过同段 TIER）", body, "6.5000", "TIER_CUSTOMER", T_PROMO)

    # ③ 通用阶梯：C2 无专属档位 → 通用 TIER 8.0
    _, body = call(token, customer_id=C2, material_id=M2, qty="50", date="2026-01-01")
    expect_hit("③ TIER_COMMON", body, "8.0000", "TIER_COMMON", T_COMMON)

    # ④ 标准价：M3 只有 standard_price=12.5000，source_id = 物料 id
    _, body = call(token, customer_id=C2, material_id=M3, qty="10", date="2026-01-01")
    data = expect_hit("④ STANDARD（source_id = 物料 id）", body, "12.5000", "STANDARD", M3)
    check(
        "④ STANDARD 的 price 是 JSON 字符串（金额口径）",
        isinstance(data.get("price"), str),
        "实际类型 %s 值 %r" % (type(data.get("price")).__name__, data.get("price")),
    )

    # ⑤ 全未命中：M4 无协议价 / 无档位 / 标准价 NULL
    _, body = call(token, customer_id=C2, material_id=M4, qty="10", date="2026-01-01")
    data = (body.get("data") or {}) if isinstance(body, dict) else {}
    check(
        "⑤ NONE（price=null / source_id=null / HTTP 200）",
        body.get("code") == 0
        and data.get("source") == "NONE"
        and data.get("price") is None
        and data.get("source_id") is None,
        "实际 %s" % json.dumps(data, ensure_ascii=False),
    )

    print("\n=== B. 边界与确定性 ===")
    # 有效期：失效日当天（2026-03-31）仍命中 200 起订的 8.5000
    _, body = call(token, customer_id=C1, material_id=M1, qty="250", date="2026-03-31")
    expect_hit("有效期边界：effective_to 当天仍生效", body, "8.5000", "AGREED", A_LIMITED)

    # 失效日次日 → 限期档位失效，降级到 min_qty=100 的 9.0000
    _, body = call(token, customer_id=C1, material_id=M1, qty="250", date="2026-04-01")
    expect_hit("有效期边界：effective_to 次日失效并降级", body, "9.0000", "AGREED", A_MQ100)

    # 生效日前一天 → 2026-06-01 才生效的新价还不能用，取旧的 9.8000
    _, body = call(token, customer_id=C1, material_id=M1, qty="60", date="2026-05-31")
    expect_hit(
        "有效期边界：未到 effective_from 的价不参与",
        body,
        "9.8000",
        "AGREED",
        "f0330000-0000-4000-8000-000000000103",
    )

    # 同 min_qty=50 两条 → 取最近生效（2026-06-01 的 9.6000）
    _, body = call(token, customer_id=C1, material_id=M1, qty="60", date="2026-07-01")
    expect_hit("多条协议价：同起订量取最近生效", body, "9.6000", "AGREED", A_NEWEST)

    # 档位左闭右开：qty == max_qty(100) 不命中档位 → 落通用阶梯 8.0000
    _, body = call(token, customer_id=C1, material_id=M2, qty="100", date="2026-01-01")
    expect_hit("档位区间左闭右开：qty == max_qty 不命中", body, "8.0000", "TIER_COMMON", T_COMMON)

    # 档位左闭：qty == min_qty 命中
    _, body = call(token, customer_id=C1, material_id=M2, qty="0.0000", date="2026-01-01")
    expect_hit("档位区间左闭：qty == min_qty 命中", body, "6.5000", "TIER_CUSTOMER", T_PROMO)

    # 停用行（INACTIVE，1.0000 / 300 起订）不得被命中：qty=300 落到 200 起的限期价 8.5000
    _, body = call(token, customer_id=C1, material_id=M1, qty="300", date="2026-01-01")
    expect_hit("停用行不参与取价（INACTIVE 的 1.0000 未命中）", body, "8.5000", "AGREED", A_LIMITED)

    # 起订量：qty 不达 50 → 只有 0 起订那档
    _, body = call(token, customer_id=C1, material_id=M1, qty="49.9999", date="2026-01-01")
    expect_hit("起订量边界：未达 min_qty 不命中", body, "10.0000", "AGREED",
               "f0330000-0000-4000-8000-000000000101")

    # 不传客户：跳过 ①②，M2 仍可取通用阶梯
    _, body = call(token, material_id=M2, qty="50", date="2026-01-01")
    expect_hit("不传 customer_id：跳过协议价与专属阶梯", body, "8.0000", "TIER_COMMON", T_COMMON)

    # 确定性：同参数连打 10 次，body 必须完全一致
    bodies = set()
    for _ in range(10):
        _, body = call(token, customer_id=C1, material_id=M1, qty="150", date="2026-01-01")
        bodies.add(json.dumps(body.get("data"), sort_keys=True, ensure_ascii=False))
    check("确定性：同条件 10 次调用结果完全一致", len(bodies) == 1, "不同结果数 %d" % len(bodies))

    print("\n=== C. 失败路径 ===")
    status, body = call(token, customer_id=C1, material_id=M1, date="2026-01-01")
    check(
        "缺 qty → 400 + 10001（不静默按 0 取价）",
        status == 400 and body.get("code") == 10001 and "qty" in (body.get("msg") or ""),
        "HTTP %s %s" % (status, json.dumps(body, ensure_ascii=False)),
    )

    status, body = call(token, customer_id=C1, material_id=M1, qty="-5", date="2026-01-01")
    check(
        "qty 为负 → 400 + 10001",
        status == 400 and body.get("code") == 10001,
        "HTTP %s %s" % (status, json.dumps(body, ensure_ascii=False)),
    )

    status, body = call(token, customer_id=C1, material_id=M1, qty="1", date="2026-13-01")
    check(
        "date 非法 → 400 + 10001",
        status == 400 and body.get("code") == 10001,
        "HTTP %s %s" % (status, json.dumps(body, ensure_ascii=False)),
    )

    status, body = call(
        token, customer_id=C1, material_id="00000000-0000-4000-8000-0000000000ff", qty="1",
        date="2026-01-01",
    )
    check(
        "物料不存在 → 404 + 10002（不静默给 NONE）",
        status == 404 and body.get("code") == 10002,
        "HTTP %s %s" % (status, json.dumps(body, ensure_ascii=False)),
    )

    status, body = call(
        token, customer_id="00000000-0000-4000-8000-0000000000fe", material_id=M1, qty="1",
        date="2026-01-01",
    )
    check(
        "客户不存在 → 404 + 10002",
        status == 404 and body.get("code") == 10002,
        "HTTP %s %s" % (status, json.dumps(body, ensure_ascii=False)),
    )

    # 无 token：必须 401（证明这不是豁免接口）
    status, body = http("GET", RESOLVE + "?material_id=" + M1 + "&qty=1")
    check(
        "无 token → 401（接口受鉴权保护）",
        status == 401,
        "HTTP %s %s" % (status, json.dumps(body, ensure_ascii=False) if isinstance(body, dict) else body),
    )

    print("\n=== 结果：%d 条断言，FAIL %d ===" % (checks, len(failures)))
    if failures:
        for name in failures:
            print("  FAIL: %s" % name)
        return 1
    print("ALL_PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
