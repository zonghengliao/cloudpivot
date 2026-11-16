#!/usr/bin/env python3
"""M3-06 前端卡的 API 契约烟测（真后端 + 真库，不是模拟）。

跑法（后端须先起在 :18023，库 = cp_m306_drill_1）：
    python3 m306_api_smoke.py

验的是**前端 api/business/erp/quotation.ts 依赖的每条契约**：
 1. 用户菜单里能拿到报价单页（path/component 已由 V0436 补齐）；
 2. 分页 GET /quotation/page 200 且 PageData 形状对；
 3. 建单 POST /quotation：金额由后端算、返回 items 全量、allowed_targets = [SENT, CANCELLED]；
 4. 详情 GET /quotation/{id}：approval 段存在（null）、editable/deletable 正确；
 5. 直接 PATCH status=SENT → 409 + 66088（前端「发出」必须走 submit-approval，这条是依据）；
 6. 草稿作废 PATCH status=CANCELLED 无原因 → 200；
 7. 转单：ACCEPTED 才可转；已转过再点 → already_converted=true 且 so_no 不变（幂等）；
 8. 软删 DELETE → 列表里查不到（404）。
"""
from __future__ import annotations

import json
import subprocess
import urllib.error
import urllib.request
from typing import Any

BASE = "http://127.0.0.1:18023"
FAILS: list[str] = []
CHECKS = 0


def call(method: str, path: str, token: str | None = None, body: dict[str, Any] | None = None) -> tuple[int, dict[str, Any]]:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:  # 4xx / 5xx 也要能读到 body
        raw = exc.read().decode()
        try:
            return exc.code, json.loads(raw)
        except json.JSONDecodeError:
            return exc.code, {"raw": raw}


def check(name: str, ok: bool, detail: str = "") -> None:
    global CHECKS
    CHECKS += 1
    if ok:
        print(f"  PASS  {name}")
    else:
        FAILS.append(f"{name}{(' — ' + detail) if detail else ''}")
        print(f"  FAIL  {name}  {detail}")


def main() -> int:
    status, body = call("POST", "/api/system/auth/login", body={"username": "admin", "password": "Admin@123456"})
    check("登录 200", status == 200, f"status={status} body={body}")
    token = body.get("data", {}).get("access_token", "")

    # ---------- 1. 菜单 ----------
    status, body = call("GET", "/api/system/auth/menus", token)
    text = json.dumps(body, ensure_ascii=False)
    check("菜单接口 200", status == 200, f"status={status}")
    check("用户菜单里有报价单页", "business:erp:quotation:list" in text)
    check("菜单 path 是 /business/erp/quotation", "/business/erp/quotation" in text)

    # ---------- 2. 分页 ----------
    status, body = call("GET", "/api/business/erp/quotation/page?page=1&page_size=20", token)
    check("分页 200", status == 200, f"status={status} body={body}")
    page = body.get("data", {})
    check(
        "分页形状 list/total/page/page_size",
        all(k in page for k in ("list", "total", "page", "page_size")),
        f"keys={list(page)}",
    )

    # ---------- 3. 建单 ----------
    customer = "d0260000-0000-4000-8000-000000000001"
    mat_a = "01a1bdda-332c-7093-bdc0-ad514a7fc979"
    mat_b = "d2a20001-0000-4000-8000-000000000011"
    payload = {
        "customer_id": customer,
        "quote_date": "2026-11-17",
        "valid_until": "2026-12-01",
        "remark": "M3-06 前端烟测",
        "items": [
            {"material_id": mat_a, "qty": 3, "price": "12.3456", "tax_rate": 13, "remark": "行一"},
            {"material_id": mat_b, "qty": 2, "price": "100", "tax_rate": 13},
        ],
    }
    status, body = call("POST", "/api/business/erp/quotation", token, payload)
    check("建单 200", status == 200, f"status={status} body={body}")
    created = body.get("data", {})
    qid = created.get("id", "")
    quote_no = created.get("quote_no", "")
    check("单号形如 QT-yyyyMMdd-NNNNNN", quote_no.startswith("QT-20261117-"), quote_no)
    check("返回明细全量", len(created.get("items", [])) == 2, f"items={len(created.get('items', []))}")
    # 后端算法：金额 = round4(单价 × 数量)，税 = round4(金额 × 税率/100)
    line1 = created.get("items", [{}])[0]
    check("第 1 行金额 37.0368", str(line1.get("amount")) == "37.0368", str(line1.get("amount")))
    check("第 1 行税额 4.8148", str(line1.get("tax_amount")) == "4.8148", str(line1.get("tax_amount")))
    check("第 1 行含税 41.8516", str(line1.get("with_tax")) == "41.8516", str(line1.get("with_tax")))
    check(
        "单头价税合计 = 237.0368 + 30.8148",
        str(created.get("total_with_tax")) == "267.8516",
        str(created.get("total_with_tax")),
    )
    check("客户名已补齐", created.get("customer_name") == "北京云枢科技有限公司", str(created.get("customer_name")))
    check("allowed_targets = [SENT, CANCELLED]", created.get("allowed_targets") == ["SENT", "CANCELLED"], str(created.get("allowed_targets")))
    check("editable / deletable 都是 true", created.get("editable") is True and created.get("deletable") is True)
    check("列表态 expired = false", created.get("expired") is False)

    # ---------- 4. 详情 ----------
    status, body = call("GET", f"/api/business/erp/quotation/{qid}", token)
    detail = body.get("data", {})
    check("详情 200", status == 200, f"status={status}")
    check("详情 approval 段存在且为 null", "approval" in detail and detail["approval"] is None)
    check("详情明细 2 行且带物料编码", len(detail.get("items", [])) == 2 and bool(detail["items"][0].get("material_code")))

    # ---------- 5. 直接发出必须 409 + 66088（前端「发出」走 submit-approval 的依据） ----------
    status, body = call("PATCH", f"/api/business/erp/quotation/{qid}/status", token, {"target_status": "SENT"})
    check("直接 PATCH status=SENT → 409", status == 409, f"status={status} body={body}")
    check("错误码 66088（缺审批实例）", body.get("code") == 66088, str(body.get("code")))

    # ---------- 6. 草稿作废（不需要原因） ----------
    status, body = call("PATCH", f"/api/business/erp/quotation/{qid}/status", token, {"target_status": "CANCELLED"})
    check("草稿作废 200", status == 200, f"status={status} body={body}")
    check("作废后状态 CANCELLED", body.get("data", {}).get("status") == "CANCELLED")
    check("作废后 allowed_targets 为空", body.get("data", {}).get("allowed_targets") == [])

    # ---------- 7. 转单：先造一张 ACCEPTED 的单（客户接受这一步由审批/人工流程写） ----------
    status, body = call("POST", "/api/business/erp/quotation", token, payload)
    accepted_id = body.get("data", {}).get("id", "")
    accepted_no = body.get("data", {}).get("quote_no", "")
    check("第二张单已建（待转单）", status == 200 and bool(accepted_id), f"status={status}")
    # 直接改库把状态推到 ACCEPTED：本脚本验的是「前端点转单」这条链路，
    # SENT/ACCEPTED 的审批链路已由 M3-04/M3-05 的真库用例覆盖
    env = {"PGPASSWORD": "fbb1263691ff75c31ec99eaf258138b4"}
    subprocess.run(
        [
            "psql", "-h", "172.18.10.145", "-U", "cloudpivot", "-d", "cp_m306_drill_1", "-tAc",
            f"UPDATE biz_quotation SET status='ACCEPTED' WHERE id='{accepted_id}';",
        ],
        env={**env, "PATH": "/usr/bin:/bin"},
        check=True,
        capture_output=True,
    )
    status, body = call("GET", f"/api/business/erp/quotation/{accepted_id}", token)
    check("改库后详情是 ACCEPTED", body.get("data", {}).get("status") == "ACCEPTED", str(body.get("data", {}).get("status")))

    convert_payload = {"order_date": "2026-11-17", "delivery_date": "2026-12-05", "remark": "前端烟测转单"}
    status, body = call("POST", f"/api/business/erp/quotation/{accepted_id}/convert", token, convert_payload)
    check("转单 200", status == 200, f"status={status} body={body}")
    result = body.get("data", {})
    so_no = result.get("header", {}).get("so_no", "")
    check("返回新订单号 SO-…", so_no.startswith("SO-"), so_no)
    check("三段式 header/items/meta 齐", all(k in result for k in ("header", "items", "meta")), str(list(result)))
    check("搬了 2 行明细", len(result.get("items", [])) == 2, str(len(result.get("items", []))))
    check("meta.line_count = 2", result.get("meta", {}).get("line_count") == 2, str(result.get("meta")))
    check("meta.already_converted = false", result.get("meta", {}).get("already_converted") is False)
    check("订单金额原样搬（267.8516）", str(result.get("header", {}).get("total_with_tax")) == "267.8516")
    check("订单备注带来源", "由报价单" in (result.get("header", {}).get("remark") or ""), str(result.get("header", {}).get("remark")))

    # 幂等：再点一次
    status, body = call("POST", f"/api/business/erp/quotation/{accepted_id}/convert", token, convert_payload)
    again = body.get("data", {})
    check("重复转单 200（幂等）", status == 200, f"status={status}")
    check("already_converted = true", again.get("meta", {}).get("already_converted") is True)
    check("回的是同一张订单", again.get("header", {}).get("so_no") == so_no, str(again.get("header", {}).get("so_no")))

    # 已转单的单不能再作废（M3-03 守卫）
    status, body = call("PATCH", f"/api/business/erp/quotation/{accepted_id}/status", token, {"target_status": "CANCELLED", "reason": "想反悔"})
    check("已转单不许作废 → 409", status == 409, f"status={status} body={body}")

    # ---------- 8. 软删 ----------
    status, body = call("DELETE", f"/api/business/erp/quotation/{qid}", token)
    check("删除 200", status == 200, f"status={status} body={body}")
    status, body = call("GET", f"/api/business/erp/quotation/{qid}", token)
    check("软删后详情 404", status == 404, f"status={status}")

    status, body = call("GET", f"/api/business/erp/quotation/page?quote_no={accepted_no}", token)
    rows = body.get("data", {}).get("list", [])
    check("按单号筛选能查到转单那张", len(rows) == 1 and rows[0]["quote_no"] == accepted_no, str(rows))

    # ---------- 9. 有效期三态筛选（前端 buildQuery 把 ALL/ACTIVE/EXPIRED 映射成「不传 / false / true」） ----------
    # 先造一张「已过期」的单（valid_until 在昨天之前），否则 expired=true 恒 0 行，断言会假绿
    expired_payload = dict(payload)
    expired_payload["quote_date"] = "2026-01-01"
    expired_payload["valid_until"] = "2026-01-31"
    expired_payload["remark"] = "M3-06 前端烟测：已过期"
    expired_payload["items"] = [{"material_id": mat_a, "qty": 1, "price": "10", "tax_rate": 0}]
    status, body = call("POST", "/api/business/erp/quotation", token, expired_payload)
    expired_id = body.get("data", {}).get("id", "")
    check("建一张已过期的单", status == 200 and bool(expired_id), f"status={status} body={body}")
    check("该单 expired = true（现算，不是状态）", body.get("data", {}).get("expired") is True)
    check("该单状态仍是 DRAFT（过期不改状态）", body.get("data", {}).get("status") == "DRAFT")

    status, body = call("GET", "/api/business/erp/quotation/page?page=1&page_size=200", token)
    all_rows = body.get("data", {}).get("list", [])
    expired_ids = {row["id"] for row in all_rows if row.get("expired")}
    check("全部里至少有 1 张已过期", bool(expired_ids), f"expired_ids={expired_ids}")
    status, body = call("GET", "/api/business/erp/quotation/page?expired=true&page=1&page_size=200", token)
    only_expired = body.get("data", {}).get("list", [])
    check(
        "expired=true 只回已过期的单",
        {row["id"] for row in only_expired} == expired_ids,
        f"期望 {len(expired_ids)} 行，实际 {len(only_expired)} 行",
    )
    status, body = call("GET", "/api/business/erp/quotation/page?expired=false&page=1&page_size=200", token)
    only_active = body.get("data", {}).get("list", [])
    check(
        "expired=false 只回未过期的单",
        all(row.get("expired") is False for row in only_active) and len(only_active) >= 1,
        f"实际 {len(only_active)} 行",
    )
    check(
        "两态相加 = 全部（不传 expired）",
        len(only_expired) + len(only_active) == len(all_rows),
        f"{len(only_expired)} + {len(only_active)} != {len(all_rows)}",
    )

    print()
    print(f"共 {CHECKS} 条断言，失败 {len(FAILS)} 条")
    if FAILS:
        for item in FAILS:
            print("  ✗ " + item)
        return 1
    print("全部通过 ✓")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
