#!/usr/bin/env python3
"""P2-8 真机验收（隔离实例 + 真 HTTP，不是 mock）：采购单挂项目全链路。

跑法（先起隔离实例，见 p28_e2e_up.sh）：
    python3 p28_e2e.py http://127.0.0.1:18097
"""

import json
import sys
import urllib.error
import urllib.parse
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18097"
TOKEN = ""


def call(method, path, body=None, token=None, expect=200):
    url = BASE + path
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode()
    if token or TOKEN:
        headers["Authorization"] = "Bearer " + (token or TOKEN)
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            status = resp.status
            raw = resp.read().decode()
    except urllib.error.HTTPError as err:
        status = err.code
        raw = err.read().decode()
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError:
        payload = {"_raw": raw[:400]}
    print(f"--- {method} {path} -> {status}")
    print(json.dumps(payload, ensure_ascii=False)[:1200])
    assert status == expect, f"{method} {path} 期望 {expect}，实际 {status}"
    return payload


def data_of(payload):
    return payload.get("data", payload)


def main():
    global TOKEN
    print("========== 0. 登录（admin / SUPER_ADMIN 跳过权限校验） ==========")
    login = None
    for candidate in ("Admin@123456", "P28E2e@2026"):
        try:
            login = call(
                "POST",
                "/api/system/auth/login",
                {"username": "admin", "password": candidate},
            )
            print(f"--- 用口令 {candidate} 登录成功")
            break
        except AssertionError:
            print(f"--- 口令 {candidate} 不通，换下一个")
    assert login is not None, "两个候选口令都登录失败"
    TOKEN = data_of(login)["access_token"]

    # 种子账号带 must_change_password=true：除改密/登出外全部 403（CODE-06）。
    # 先改密（清掉这个标记），再用新口令重新登录。
    if data_of(login).get("must_change_password"):
        print("\n--- 种子账号要求首登改密，先改密再继续 ---")
        call(
            "PUT",
            "/api/system/auth/password",
            {"old_password": "Admin@123456", "new_password": "P28E2e@2026"},
        )
        login = call(
            "POST",
            "/api/system/auth/login",
            {"username": "admin", "password": "P28E2e@2026"},
        )
        TOKEN = data_of(login)["access_token"]
        assert data_of(login).get("must_change_password") is False

    print("\n========== 1. 主数据（新建采购单需要 supplier / warehouse / material） ==========")
    suppliers = data_of(
        call("GET", "/api/business/erp/supplier/page?page=1&page_size=5")
    )["list"]
    warehouses = data_of(
        call("GET", "/api/business/erp/warehouse/page?page=1&page_size=5")
    )["list"]
    materials = data_of(
        call("GET", "/api/business/erp/material/page?page=1&page_size=5")
    )["list"]
    supplier_id = suppliers[0]["id"]
    warehouse_id = warehouses[0]["id"]
    material_id = materials[0]["id"]
    print(f"supplier={supplier_id} warehouse={warehouse_id} material={material_id}")

    print("\n========== 2. 建一个项目（P2-8 真机项目） ==========")
    import time

    code = "P28E%d" % (int(time.time()) % 100000)
    project = data_of(
        call(
            "POST",
            "/api/business/project",
            {"name": "P2-8 真机项目", "code": code},
        )
    )
    project_id = project["id"]
    print(f"project_id={project_id} code={code}")

    print("\n========== 3. 建采购单：带 project_id ==========")
    order_body = {
        "supplier_id": supplier_id,
        "warehouse_id": warehouse_id,
        "purchase_type": "SPOT",
        "order_date": "2026-11-05",
        "price_mode": "EXCL",
        "remark": "P2-8 真机验收",
        "project_id": project_id,
        "items": [
            {
                "material_id": material_id,
                "qty": 2,
                "price_excl": 50,
                "tax_rate": 13,
            }
        ],
    }
    created = data_of(call("POST", "/api/business/erp/purchase-order", order_body))
    order_id = created["order"]["id"]
    assert created["order"]["project_id"] == project_id
    assert created["order"]["project_name"] == "P2-8 真机项目"
    print(f"order_id={order_id}（建单响应里已带 project_name）")

    print("\n========== 4. 列表按 project_id 筛：能看到项目名 ==========")
    page = data_of(
        call(
            "GET",
            "/api/business/erp/purchase-order/page?page=1&page_size=10&project_id="
            + urllib.parse.quote(project_id),
        )
    )
    assert page["total"] == 1, f"按项目筛应命中 1 张，实际 {page['total']}"
    row = page["list"][0]
    assert row["project_id"] == project_id
    assert row["project_name"] == "P2-8 真机项目", row["project_name"]
    print(f"列表行：order_no={row['order_no']} project_name={row['project_name']}")

    print("\n========== 5. 详情能看到项目 ==========")
    detail = data_of(call("GET", f"/api/business/erp/purchase-order/{order_id}"))
    assert detail["project_id"] == project_id
    assert detail["project_name"] == "P2-8 真机项目"
    print(f"详情：project_id={detail['project_id']} project_name={detail['project_name']}")

    print("\n========== 6. 项目改名 → 采购单详情/列表跟着变（证明是关联查，不是快照） ==========")
    call("PUT", f"/api/business/project/{project_id}", {"name": "P2-8 真机项目（改名后）"})
    detail2 = data_of(call("GET", f"/api/business/erp/purchase-order/{order_id}"))
    assert detail2["project_name"] == "P2-8 真机项目（改名后）", detail2["project_name"]
    print(f"改名后详情 project_name={detail2['project_name']} ✅")

    print("\n========== 7. 项目不存在 → 400 + 66002（不落单） ==========")
    ghost = dict(order_body)
    ghost["project_id"] = "0199a000-0000-7000-8000-0000000000ff"
    bad = call("POST", "/api/business/erp/purchase-order", ghost, expect=400)
    assert bad["code"] == 66002, bad
    print(f"被拒：code={bad['code']} msg={bad['msg']}")

    print("\n========== 8. 项目已软删 → 详情保留 project_id，但不再给名字 ==========")
    call("DELETE", f"/api/business/project/{project_id}")
    detail3 = data_of(call("GET", f"/api/business/erp/purchase-order/{order_id}"))
    assert detail3["project_id"] == project_id, "归属不因主档软删而消失"
    assert detail3["project_name"] is None, detail3["project_name"]
    print(
        f"软删后：project_id={detail3['project_id']} project_name={detail3['project_name']} ✅"
    )

    print("\n========== 9. 改单清空项目（整单覆盖口径） ==========")
    clear_body = dict(order_body)
    clear_body.pop("project_id")
    # 项目已软删，先建一个新项目再挂上，然后清空
    project2 = data_of(
        call(
            "POST",
            "/api/business/project",
            {"name": "P2-8 清空用项目", "code": code + "B"},
        )
    )
    attach = dict(order_body)
    attach["project_id"] = project2["id"]
    call("PUT", f"/api/business/erp/purchase-order/{order_id}", attach)
    d4 = data_of(call("GET", f"/api/business/erp/purchase-order/{order_id}"))
    assert d4["project_name"] == "P2-8 清空用项目", d4["project_name"]
    call("PUT", f"/api/business/erp/purchase-order/{order_id}", clear_body)
    d5 = data_of(call("GET", f"/api/business/erp/purchase-order/{order_id}"))
    assert d5["project_id"] is None and d5["project_name"] is None, d5
    print("不传 project_id 后归属被清空 ✅")

    print("\nALL E2E ASSERTIONS PASSED")


if __name__ == "__main__":
    main()
