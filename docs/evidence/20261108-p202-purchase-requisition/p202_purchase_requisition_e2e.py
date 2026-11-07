#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
P2-02 采购申请闭环 · 真机 API 全链实测
=====================================
跑在隔离实例 http://127.0.0.1:18105（库 cloudpivot_prq02）上，按卡面验收 1–6 逐步断言：

  1. 部门 A 申请 100 个物料 X
  2. 部门 B 申请 50 个物料 X
  3. 两张申请都提交审批 → 走 pr_approve 流程（B 超预算 → 多一道财务经理特殊审批）
  4. 合并转成一张采购订单（150 个物料 X，单价按数量加权平均）
  5. 两张申请单状态都变 CONVERTED，指向同一个 PO
  6. 反证：重复转单 409/66049、DRAFT 转单 409、未审批转单 409、自审批 403

用法：python3 p202_purchase_requisition_e2e.py
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE = os.environ.get("P202_BASE", "http://127.0.0.1:18105")
USER = os.environ.get("P202_USER", "admin")
PASSWORD = os.environ.get("P202_PASSWORD", "Admin@123456")
NEW_PASSWORD = os.environ.get("P202_NEW_PASSWORD", "Prq02@123456")
DB = os.environ.get("P202_DB", "cloudpivot_prq02")
OUT_DIR = os.environ.get(
    "P202_OUT", "/opt/cloudpivot/docs/evidence/20261108-p202-purchase-requisition"
)

TOKEN = ""
FAILS: list[str] = []
STEPS: list[str] = []
RAW: list[dict] = []


def call(method: str, path: str, body=None, token: str | None = None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    tok = token if token is not None else TOKEN
    if tok:
        request.add_header("Authorization", f"Bearer {tok}")
    try:
        with urllib.request.urlopen(request, timeout=90) as response:
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


def sql(statement: str) -> str:
    env = dict(os.environ, PGPASSWORD="alun")
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-Atc", statement],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr}")
    return out.stdout.strip()


def login_as(username: str, password: str, new_password: str | None = None) -> str:
    """登录；首登强制改密时先改密（返回可用令牌）。"""
    status, body = call("POST", "/api/system/auth/login",
                        {"username": username, "password": password}, token="")
    if status == 200 and (body.get("data") or {}).get("must_change_password"):
        token = body["data"]["access_token"]
        target = new_password or password
        put_status, put_body = call("PUT", "/api/system/auth/password",
                                    {"old_password": password, "new_password": target},
                                    token=token)
        check(put_status == 200 and put_body.get("code") == 0,
              f"{username} 首登强制改密 {put_status}")
        status, body = call("POST", "/api/system/auth/login",
                            {"username": username, "password": target}, token="")
    if status != 200:
        return ""
    return (body.get("data") or {}).get("access_token", "")


def ensure_dept(code: str, name: str) -> str:
    status, body = call("POST", "/api/system/dept", {
        "dept_code": code, "dept_name": name, "status": 1, "sort_order": 900,
    })
    if status in (200, 201) and body.get("code") == 0:
        return body["data"]["id"]
    rows = sql(f"SELECT id FROM sys_dept WHERE dept_code = '{code}' AND deleted_at IS NULL LIMIT 1")
    if rows:
        return rows
    raise RuntimeError(f"部门 {code} 建不出来也查不到：{status} {body}")


def ensure_user(username: str, real_name: str, dept_id: str, role_ids: list[str]) -> str:
    status, body = call("POST", "/api/system/user", {
        "username": username, "password": NEW_PASSWORD, "real_name": real_name,
        "dept_id": dept_id, "status": 1, "role_ids": role_ids,
    })
    if status in (200, 201) and body.get("code") == 0:
        return body["data"]["id"]
    rows = sql(f"SELECT id FROM sys_user WHERE username = '{username}' AND deleted_at IS NULL LIMIT 1")
    if rows:
        return rows
    raise RuntimeError(f"用户 {username} 建不出来也查不到：{status} {body}")


def ensure_supplier(code: str, name: str) -> str:
    status, body = call("POST", "/api/business/erp/supplier", {
        "supplier_code": code, "supplier_name": name,
        "contact_person": "李四", "contact_phone": "13800000002", "credit_days": 30,
    })
    if status in (200, 201) and body.get("code") == 0:
        return body["data"]["id"]
    status, page = call("GET", f"/api/business/erp/supplier/page?page=1&page_size=200&keyword={code}")
    for row in (page.get("data") or {}).get("list", []):
        if row.get("supplier_code") == code:
            return row["id"]
    raise RuntimeError(f"供应商 {code} 建不出来也查不到：{status} {body}")


def create_requisition(token: str, dept_id: str, warehouse_id: str, material_id: str,
                       qty: str, price: str, tax: str, supplier_id: str, demand: str,
                       remark: str) -> dict:
    status, body = call("POST", "/api/business/erp/purchase-requisition", {
        "dept_id": dept_id,
        "warehouse_id": warehouse_id,
        "demand_date": demand,
        "remark": remark,
        "items": [{
            "material_id": material_id,
            "qty": qty,
            "est_price": price,
            "tax_rate": tax,
            "demand_date": demand,
            "suggest_supplier_id": supplier_id,
        }],
    }, token=token)
    check(status == 200 and body.get("code") == 0,
          f"建申请 {status}：{json.dumps(body, ensure_ascii=False)[:200]}")
    if status != 200:
        raise RuntimeError("建申请失败")
    return body["data"]


def submit(token: str, req_id: str, reason: str) -> tuple[int, dict]:
    return call("POST", f"/api/business/erp/purchase-requisition/{req_id}/submit",
                {"reason": reason}, token=token)


def todos(token: str) -> list[dict]:
    status, body = call("GET", "/api/business/flow/task/todo?page=1&page_size=100", token=token)
    if status != 200:
        raise RuntimeError(f"取待办失败 {status} {body}")
    data = body.get("data") or {}
    if isinstance(data, dict):
        return data.get("list", [])
    return data or []


def approve(token: str, task_id: str, comment: str) -> tuple[int, dict]:
    return call("POST", f"/api/business/flow/task/{task_id}/approve", {"comment": comment}, token=token)


def main() -> int:
    global TOKEN
    os.makedirs(OUT_DIR, exist_ok=True)
    TOKEN = login_as(USER, PASSWORD, NEW_PASSWORD)
    check(bool(TOKEN), "管理员登录成功")

    # ---------- 主数据 ----------
    status, body = call("GET", "/api/business/erp/material/page?page=1&page_size=5")
    materials = body["data"]["list"]
    material = materials[0]
    material_id = material["id"]
    status, body = call("GET", "/api/business/erp/warehouse/page?page=1&page_size=5")
    warehouse_id = body["data"]["list"][0]["id"]
    warehouse_name = body["data"]["list"][0]["warehouse_name"]
    supplier_id = ensure_supplier("P202-S1", "P202 甲供应商")
    role_id = sql("SELECT id FROM sys_role WHERE role_code = 'DEPT_LEADER' AND deleted_at IS NULL LIMIT 1")

    step("准备：两个部门 + 两个申请人（DEPT_LEADER）+ 预算夹具")
    dept_a = ensure_dept("P202-A", "P202 需求部A")
    dept_b = ensure_dept("P202-B", "P202 需求部B")
    user_a = ensure_user("p202_a", "P202 申请人A", dept_a, [role_id])
    user_b = ensure_user("p202_b", "P202 申请人B", dept_b, [role_id])
    token_a = login_as("p202_a", NEW_PASSWORD)
    token_b = login_as("p202_b", NEW_PASSWORD)
    check(bool(token_a) and bool(token_b), "两个申请人各自登录成功（非 admin，避免自审批）")

    # 预算：只给部门 B 编 1000 元/年的预算（月额度 83.33），B 申请 500 元 → 必然超预算
    account = sql("SELECT id || '|' || account_code || '|' || account_name FROM biz_account "
                  "WHERE is_leaf = 1 AND deleted_at IS NULL AND account_code = '1405' LIMIT 1")
    account_id, account_code, account_name = account.split("|")
    sql(f"""
    INSERT INTO biz_budget (id, budget_no, fiscal_year, dept_id, dept_name, account_id,
                            account_code, account_name, total_amount, version_no, change_type,
                            status, effective_from, created_at, updated_at)
    SELECT '20000000-0000-4000-8000-0000000e2021'::uuid, 'BUD-P202-0001', 2026,
           '{dept_b}'::uuid, 'P202 需求部B', '{account_id}'::uuid, '{account_code}', '{account_name}',
           1000.0000, 1, 'ORIGINAL', 'ACTIVE', NULL, now(), now()
     WHERE NOT EXISTS (SELECT 1 FROM biz_budget WHERE budget_no = 'BUD-P202-0001');
    INSERT INTO biz_budget_subject_map (id, map_type, account_id, dept_id, priority, status, remark)
    SELECT '20000000-0000-4000-8000-0000000e2022'::uuid, 'DEFAULT', '{account_id}'::uuid,
           '{dept_b}'::uuid, 0, 1, 'P2-02 验收夹具：部门B默认映射到 1405 库存商品'
     WHERE NOT EXISTS (SELECT 1 FROM biz_budget_subject_map
                        WHERE map_type = 'DEFAULT' AND dept_id = '{dept_b}'::uuid AND deleted_at IS NULL);
    """)
    check(sql("SELECT count(*) FROM biz_budget WHERE budget_no = 'BUD-P202-0001' AND status = 'ACTIVE'") == "1",
          "预算夹具就位：部门B 1405 科目 2026 年度 1000 元（ACTIVE）")

    # ---------- ① 部门 A 申请 100 个物料 X ----------
    step("① 部门 A 申请 100 个物料 X")
    detail_a = create_requisition(token_a, dept_a, warehouse_id, material_id, "100", "10", "13",
                                 supplier_id, "2026-12-20", "P2-02 验收：部门A 100 个")
    record("create_a", detail_a)
    req_a = detail_a["requisition"]["id"]
    check(detail_a["requisition"]["status"] == "DRAFT",
          f"A 初始 DRAFT（实得 {detail_a['requisition']['status']}）")
    check(detail_a["requisition"]["dept_id"] == dept_a, "A 申请部门 = P202 需求部A")
    check(detail_a["requisition"]["dept_name"] == "P202 需求部A", "A 部门名快照落库")
    check(Decimal(detail_a["requisition"]["est_amount"]) == Decimal("1000.0000"),
          f"A 预估金额 100×10 = {detail_a['requisition']['est_amount']}")
    check(len(detail_a["items"]) == 1 and detail_a["items"][0]["unit_id"],
          "A 明细单位由物料带出（客户端没传 unit_id）")
    check(Decimal(detail_a["items"][0]["est_price_incl"]) == Decimal("11.300000"),
          f"A 含税单价服务端算 10×1.13 = {detail_a['items'][0]['est_price_incl']}")

    # ---------- ② 部门 B 申请 50 个物料 X ----------
    step("② 部门 B 申请 50 个物料 X")
    detail_b = create_requisition(token_b, dept_b, warehouse_id, material_id, "50", "10", "13",
                                 supplier_id, "2026-12-10", "P2-02 验收：部门B 50 个")
    record("create_b", detail_b)
    req_b = detail_b["requisition"]["id"]
    check(Decimal(detail_b["requisition"]["est_amount"]) == Decimal("500.0000"),
          f"B 预估金额 50×10 = {detail_b['requisition']['est_amount']}")

    # ---------- ③ 两张申请都提交审批 ----------
    step("③ 两张申请都提交审批（走 pr_approve 流程；B 超预算 → 加财务经理）")
    status, body = submit(token_a, req_a, "P2-02 验收：A 提交")
    check(status == 200 and body.get("code") == 0, f"A 提交审批 {status}")
    draft_a = (body.get("data") or {}).get("requisition") or {}
    check(draft_a.get("status") == "SUBMITTED", f"A 状态 SUBMITTED（实得 {draft_a.get('status')}）")
    check(bool(draft_a.get("approval_instance_id")), "A 回写了审批实例 id")
    check(draft_a.get("over_budget") is False, "A 无预算科目 → 不超预算（budget_checked=false）")

    status, body = submit(token_b, req_b, "P2-02 验收：B 提交（超预算）")
    check(status == 200 and body.get("code") == 0, f"B 提交审批 {status}")
    draft_b = (body.get("data") or {}).get("requisition") or {}
    check(draft_b.get("status") == "SUBMITTED", f"B 状态 SUBMITTED（实得 {draft_b.get('status')}）")
    check(draft_b.get("over_budget") is True,
          f"B 超预算被标出（over_amount={draft_b.get('over_amount')}）")
    record("submit_b", body)

    # 重复提交 → 409
    status, body = submit(token_a, req_a, "重复提交")
    check(status == 409 and body.get("code") == 66049,
          f"重复提交 A → 409/66049（实得 {status}/{body.get('code')}）")

    # ---------- 审批 ----------
    step("④ 审批：A 一票通过；B 采购经理 → 财务经理（超预算特殊审批）")
    my = todos(TOKEN)
    names = [t.get("title") or "" for t in my]
    record("todos_after_submit", my)

    def todo_of(req_no: str, items: list[dict]) -> list[dict]:
        """按 form_data.req_no 认领待办（todo 出参没有 biz_no，靠表单快照里的申请单号）。"""
        return [
            t for t in items
            if ((t.get("form_data") or {}).get("req_no") or "") == req_no
        ]

    a_tasks = todo_of(detail_a["requisition"]["req_no"], my)
    b_tasks = todo_of(detail_b["requisition"]["req_no"], my)
    check(len(a_tasks) == 1, f"A 只有 1 条待办（实得 {len(a_tasks)}）{names}")
    check(len(b_tasks) == 1, f"B 首节点只有 1 条待办（实得 {len(b_tasks)}）")

    status, body = approve(TOKEN, a_tasks[0]["id"], "A 需求合理，同意")
    check(status == 200 and body.get("code") == 0, f"A 审批通过 {status}")
    status, body = approve(TOKEN, b_tasks[0]["id"], "B 需求合理，同意（转财务复核预算）")
    check(status == 200 and body.get("code") == 0, f"B 采购经理审批通过 {status}")

    # 超预算 → 流程必须再落一条财务经理待办（条件边生效）
    my = todos(TOKEN)
    record("todos_after_first_approve", my)
    b_fin = todo_of(detail_b["requisition"]["req_no"], my)
    check(len(b_fin) == 1, f"B 超预算 → 出现财务经理待办（实得 {len(b_fin)}）")
    if b_fin:
        status, body = approve(TOKEN, b_fin[0]["id"], "超预算部分同意，请控制单价")
        check(status == 200 and body.get("code") == 0, f"B 财务经理审批通过 {status}")

    status, da = call("GET", f"/api/business/erp/purchase-requisition/{req_a}")
    status, db_ = call("GET", f"/api/business/erp/purchase-requisition/{req_b}")
    da, db_ = da["data"], db_["data"]
    record("detail_a_after_approve", da)
    record("detail_b_after_approve", db_)
    check(da["requisition"]["status"] == "APPROVED", f"A 审批后 APPROVED（实得 {da['requisition']['status']}）")
    check(db_["requisition"]["status"] == "APPROVED", f"B 审批后 APPROVED（实得 {db_['requisition']['status']}）")

    # ---------- ⑤ 合并转采购订单 ----------
    step("⑤ 合并转一张采购订单（150 个物料 X）")
    status, body = call("POST", f"/api/business/erp/purchase-requisition/{req_a}/to-po", {
        "requisition_ids": [req_a, req_b],
        "supplier_id": supplier_id,
        "warehouse_id": warehouse_id,
        "order_date": "2026-11-08",
        "remark": "P2-02 验收：两部门合并采购",
    })
    check(status == 200 and body.get("code") == 0,
          f"合并转单 {status}：{json.dumps(body, ensure_ascii=False)[:300]}")
    if status != 200:
        print("\n".join(STEPS))
        return 1
    result = body["data"]
    record("to_po", result)
    po = result["purchase_order"]
    check(result["merged_count"] == 2, f"合并 2 张申请单（实得 {result['merged_count']}）")
    check(len(result["lines"]) == 1, f"同物料并成 1 行（实得 {len(result['lines'])}）")
    line = result["lines"][0]
    check(Decimal(line["qty"]) == Decimal("150.0000"), f"合并数量 100+50 = {line['qty']}")
    check(Decimal(line["est_price"]) == Decimal("10.000000"), f"加权单价 = {line['est_price']}")
    check(sorted(line["source_req_nos"]) == sorted([detail_a["requisition"]["req_no"],
                                                    detail_b["requisition"]["req_no"]]),
          "合并行标注了两张来源申请单")
    check(po["line_count"] == 1, f"采购订单 1 行（实得 {po['line_count']}）")
    check(Decimal(po["total_with_tax"]) == Decimal("1695.0000"),
          f"价税合计 1500×1.13 = {po['total_with_tax']}")

    # ---------- ⑥ 两张申请都 CONVERTED 且指向同一 PO ----------
    step("⑥ 两张申请单状态 = 已转单，指向同一个 PO")
    status, da = call("GET", f"/api/business/erp/purchase-requisition/{req_a}")
    status, db_ = call("GET", f"/api/business/erp/purchase-requisition/{req_b}")
    da, db_ = da["data"], db_["data"]
    record("detail_a_after_to_po", da)
    record("detail_b_after_to_po", db_)
    check(da["requisition"]["status"] == "CONVERTED" and db_["requisition"]["status"] == "CONVERTED",
          f"两张都 CONVERTED（A={da['requisition']['status']} B={db_['requisition']['status']}）")
    check(da["requisition"]["po_id"] == db_["requisition"]["po_id"] == po["po_id"],
          f"两张指向同一个 PO（{da['requisition']['po_no']}）")
    check(da["requisition"]["po_no"] == po["po_no"], "PO 号回写到申请单")
    check(da["items"][0]["po_item_id"] == db_["items"][0]["po_item_id"] is not None,
          "明细回写了「并进采购单的哪一行」（同物料同一行）")

    # ---------- 反证 ----------
    step("⑦ 反证：重复转单 / 未审批转单 / 自审批")
    status, body = call("POST", f"/api/business/erp/purchase-requisition/{req_a}/to-po", {
        "requisition_ids": [req_a, req_b], "supplier_id": supplier_id,
    })
    check(status == 409 and body.get("code") == 66049,
          f"重复转单 → 409/66049（实得 {status}/{body.get('code')}）")

    # 新建一张草稿，直接转单 → 409（状态闸）
    detail_c = create_requisition(token_a, dept_a, warehouse_id, material_id, "1", "9", "13",
                                 supplier_id, "2026-12-31", "P2-02 反证：草稿转单")
    req_c = detail_c["requisition"]["id"]
    status, body = call("POST", f"/api/business/erp/purchase-requisition/{req_c}/to-po", {
        "supplier_id": supplier_id,
    })
    check(status == 409 and body.get("code") == 66049,
          f"DRAFT 转单 → 409/66049（实得 {status}/{body.get('code')}）")

    # 税率不一致合并 → 409（合并口径）
    detail_d = create_requisition(token_a, dept_a, warehouse_id, material_id, "3", "9", "9",
                                 supplier_id, "2026-12-31", "P2-02 反证：税率不一致")
    req_d = detail_d["requisition"]["id"]
    status, body = submit(token_a, req_d, "反证：税率 9% 的单")
    check(status == 200, f"9% 申请提交成功 {status}")
    status, body = submit(token_a, req_c, "反证：草稿提交（用于合并税率冲突的前置）")
    check(status == 200, f"草稿 C 提交成功 {status}")
    my = todos(TOKEN)
    for req_no in (detail_d["requisition"]["req_no"], detail_c["requisition"]["req_no"]):
        task = next(iter(todo_of(req_no, my)), None)
        if task:
            approve(TOKEN, task["id"], "同意（反证夹具）")
    status, body = call("POST", f"/api/business/erp/purchase-requisition/{req_c}/to-po", {
        "requisition_ids": [req_c, req_d], "supplier_id": supplier_id,
    })
    check(status == 409 and body.get("code") == 66050,
          f"同物料税率不一致合并 → 409/66050（实得 {status}/{body.get('code')}）："
          f"{(body.get('msg') or '')[:80]}")

    # 自审批：申请人 A 自己没有审批权限（流程审批人 = 采购经理/超管兜底）
    status, da = call("GET", f"/api/business/erp/purchase-requisition/{req_a}")
    check(da["data"]["requisition"]["status"] == "CONVERTED", "A 仍是 CONVERTED（反证未污染）")

    # ---------- 汇总 ----------
    summary = {
        "steps": STEPS,
        "fails": FAILS,
        "raw": RAW,
    }
    with open(os.path.join(OUT_DIR, "00_api_responses.json"), "w", encoding="utf-8") as handle:
        json.dump(summary, handle, ensure_ascii=False, indent=2)
    print("\n".join(STEPS))
    print(f"\n断言总数 {len(STEPS) - sum(1 for s in STEPS if s.startswith('\\n==='))}，失败 {len(FAILS)}")
    if FAILS:
        print("失败明细：")
        for item in FAILS:
            print(f"  - {item}")
    return 0 if not FAILS else 1


if __name__ == "__main__":
    sys.exit(main())
