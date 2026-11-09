#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""E2E01-FIX-4 真机验收：ERP 出参三段式口径（`header` / `items` / `meta`）+ 兼容层。

卡面 t_84ddfbad。逐项对应：

| # | 验收 | 本脚本的做法 |
|---|---|---|
| ① | 三个接口同时返回 header/items 和旧字段 | 真 token 打三个接口，断言新旧键**同时存在** |
| ② | 旧字段值 = 新字段值（不是两套逻辑） | 每个接口做**深度相等**比较（`header == purchase_order` / `items == lines` / `header == order` / `header == head`） |
| ③ | 前端三页面改用 header/items 后功能正常 | 前端由 `pnpm typecheck` / `vite build` 覆盖（本脚本只验后端契约，产物断言见汇报） |
| ④ | docs/编码规范.md 有「出参口径」一节 + 待迁移清单 | §1.12（脚本顺带 grep 一下小节标题在不在） |
| ⑤ | cargo test + 前端 build | 由 CI 侧执行，脚本不重复跑 |

# 三个接口

```
POST /business/erp/price-inquiry                       → PriceInquiryDetailRes（旧 head）
POST /business/erp/purchase-requisition/{id}/to-po     → ToPoRes（旧 purchase_order / lines）
POST /business/erp/purchase-order/{id}/submit-approval → PurchaseOrderDetailRes（旧 order）
```

# 夹具

主数据（仓库 / 物料 / 部门 / 供应商）**只读选取**现存的；单据全部是本脚本自建的
`E2E4-<8 位随机>` 前缀行，收尾按外键顺序硬删并回查 count = 0。

采购申请要 `APPROVED` 才能转单：本卡验的是**出参形状**不是审批链，所以提交后
如果拿不到实例就直接把自建的那张申请单 `UPDATE status='APPROVED'`（夹具自备状态，
脚本会把它打印出来）。采购订单的 `submit-approval` 走真接口（145 上有 `po_approve` 流程）。

# 怎么跑（在 145 上）

```bash
cd /opt/cloudpivot
python3 scripts/verify/e2e01fix4_doc_shape.py --base http://127.0.0.1:8033/api
```

默认 `--env-file /etc/cloudpivot/cloudpivot-dev.env`（读 `NIK_DATABASE_*` / `NIK_ADMIN_PASSWORD`）。
**不要指向生产**：`db_guard.require_non_prod` 会先拦一道。
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from db_guard import require_non_prod  # noqa: E402

require_non_prod(script=os.path.basename(__file__))

import argparse  # noqa: E402
import json  # noqa: E402
import subprocess  # noqa: E402
import time  # noqa: E402
import urllib.error  # noqa: E402
import urllib.request  # noqa: E402
import uuid  # noqa: E402

DEFAULTS = {
    "base": os.environ.get("E2E14_BASE", "http://127.0.0.1:8033/api"),
    "env_file": os.environ.get("E2E14_ENV_FILE", "/etc/cloudpivot/cloudpivot-dev.env"),
}

SUFFIX = uuid.uuid4().hex[:8]
MARK = "E2E4-" + SUFFIX
PASSED = 0
FAILED = 0
FAILS = []

CFG = dict(DEFAULTS)
ENV = {}


# ==================== 基础设施 ====================

def load_env_file(path):
    values = {}
    if not os.path.exists(path):
        return values
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def db_conf():
    return {
        "host": os.environ.get("DB_HOST") or ENV.get("NIK_DATABASE_HOST", "127.0.0.1"),
        "port": os.environ.get("DB_PORT") or ENV.get("NIK_DATABASE_PORT", "5432"),
        "user": os.environ.get("DB_USER") or ENV.get("NIK_DATABASE_USER", "cloudpivot"),
        "name": os.environ.get("DB_NAME") or ENV.get("NIK_DATABASE_NAME", ""),
        "password": os.environ.get("DB_PASS") or ENV.get("NIK_DATABASE_PASSWORD", ""),
    }


def api(method, path, token=None, body=None, timeout=60):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(CFG["base"] + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read().decode()
            return response.status, json.loads(raw or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw[:500]}


def psql(sql):
    conf = db_conf()
    result = subprocess.run(
        ["psql", "-h", conf["host"], "-p", str(conf["port"]), "-U", conf["user"],
         "-d", conf["name"], "-At", "-F", "|", "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True, text=True,
        env={**os.environ, "PGPASSWORD": conf["password"]})
    if result.returncode != 0:
        raise SystemExit("psql 失败：" + result.stderr.strip() + "\nSQL: " + sql)
    return [line.split("|") for line in result.stdout.strip().splitlines() if line]


def sql_one(sql):
    rows = psql(sql)
    if not rows or not rows[0]:
        return None
    return rows[0][0] if rows[0][0] != "" else None


def check(name, condition, detail=""):
    global PASSED, FAILED
    if condition:
        PASSED += 1
        print("[PASS] " + name)
    else:
        FAILED += 1
        FAILS.append(name)
        print("[FAIL] " + name)
    if detail:
        print("       " + str(detail))
    return bool(condition)


def step(title):
    print()
    print("-" * 78)
    print(title)
    print("-" * 78)


def ok_call(name, method, path, token=None, body=None):
    status, payload = api(method, path, token, body)
    data = payload.get("data")
    good = status == 200 and payload.get("code") == 0
    check(name, good,
          f"{method} {path} → HTTP {status} code={payload.get('code')} "
          f"msg={payload.get('msg') or payload.get('message') or ''}")
    return data if good else None


def login(username, password, attempts=2):
    last = (0, {})
    for index in range(attempts):
        status, payload = api("POST", "/system/auth/login",
                              body={"username": username, "password": password})
        last = (status, payload)
        if status != 429:
            return last
        print(f"       登录限流 429，等满 62s 窗口再试（{index + 1}/{attempts}）")
        time.sleep(62)
    return last


def login_and_settle(username, candidates):
    last = None
    for candidate in candidates:
        status, payload = login(username, candidate)
        last = (status, payload)
        if status != 200 or payload.get("code") != 0:
            continue
        data = payload["data"]
        token = data["access_token"]
        if data.get("must_change_password"):
            fresh = candidate + "-t4"
            status, payload = api("PUT", "/system/auth/password", token,
                                  {"old_password": candidate, "new_password": fresh})
            if status != 200 or payload.get("code") != 0:
                return token, candidate
            status, payload = login(username, fresh)
            if status != 200 or payload.get("code") != 0:
                raise SystemExit(f"{username} 改密后重登失败：{status} {payload}")
            return payload["data"]["access_token"], fresh
        return token, candidate
    raise SystemExit(f"{username} 登录失败：{last}")


def admin_session():
    initial = ENV.get("NIK_ADMIN_PASSWORD", "")
    candidates = [initial, initial + "-t3", initial + "-t4", initial + "-e2e", initial + "-d6a"]
    token, password = login_and_settle("admin", [c for c in candidates if c])
    check("0.1 admin 登录（含首登改密兜底）", bool(token), f"生效密码长度={len(password)}")
    return token


# ==================== 主数据（只读选取） ====================

def master_data():
    step("0. 主数据（只读选取现存的仓库 / 物料 / 部门 / 供应商）")
    warehouse = sql_one(
        "SELECT w.id || '|' || w.warehouse_name FROM biz_warehouse w "
        "WHERE w.deleted_at IS NULL AND w.status = 1 "
        "ORDER BY w.created_at LIMIT 1")
    material = sql_one(
        "SELECT m.id || '|' || m.material_code || '|' || u.id FROM biz_material m "
        "JOIN biz_unit u ON u.id = m.unit_id AND u.is_base = 1 "
        "WHERE m.deleted_at IS NULL AND m.status = 1 "
        "ORDER BY m.created_at LIMIT 1")
    dept = sql_one("SELECT id || '|' || dept_name FROM sys_dept WHERE deleted_at IS NULL "
                   "ORDER BY sort_order, created_at LIMIT 1")
    supplier = sql_one("SELECT id || '|' || supplier_name FROM biz_supplier WHERE deleted_at IS NULL "
                       "ORDER BY created_at LIMIT 1")
    for label, raw in (("仓库", warehouse), ("物料", material), ("部门", dept), ("供应商", supplier)):
        check(f"0.2 取到{label}主数据", bool(raw), raw or "—")
    warehouse_id = warehouse.split("|")[0] if warehouse else None
    material_id = material.split("|")[0] if material else None
    dept_id = dept.split("|")[0] if dept else None
    supplier_id = supplier.split("|")[0] if supplier else None
    return {"warehouse_id": warehouse_id, "material_id": material_id,
            "dept_id": dept_id, "supplier_id": supplier_id}


# ==================== ① 询价单建单 ====================

def case_price_inquiry(token, master):
    step("① POST /business/erp/price-inquiry（询价单：旧 head → 新 header）")
    body = {
        "warehouse_id": master["warehouse_id"],
        "remark": MARK + " 出参口径验收",
        "items": [{"material_id": master["material_id"], "qty": "2"}],
    }
    data = ok_call("①.1 建询价单 200", "POST", "/business/erp/price-inquiry", token, body)
    if not data:
        return None
    inquiry_id = data.get("header", {}).get("id")

    check("①.2 同时有新字段 header 和旧字段 head",
          isinstance(data.get("header"), dict) and isinstance(data.get("head"), dict),
          f"keys={sorted(data.keys())[:9]}")
    check("①.3 明细在新字段 items 里", isinstance(data.get("items"), list) and len(data["items"]) == 1,
          f"items={len(data.get('items') or [])}")
    check("①.4 meta 段存在且是统计口径（quote_count/comparison_count/can_to_po）",
          isinstance(data.get("meta"), dict)
          and data["meta"].get("quote_count") == 0
          and "comparison_count" in data["meta"]
          and data["meta"].get("can_to_po") is False,
          json.dumps(data.get("meta"), ensure_ascii=False))
    check("②.1 旧字段 head == 新字段 header（深度相等）",
          data.get("header") == data.get("head"),
          f"header.inquiry_no={data.get('header', {}).get('inquiry_no')}")
    check("②.2 返回值与库内同一张单（单号一致）",
          data.get("header", {}).get("inquiry_no") ==
          sql_one(f"SELECT inquiry_no FROM biz_price_inquiry WHERE id = '{inquiry_id}'"),
          data.get("header", {}).get("inquiry_no"))
    return inquiry_id


# ==================== ② 采购申请转采购订单 ====================

def case_requisition_to_po(token, master):
    step("② POST /business/erp/purchase-requisition/{id}/to-po（旧 purchase_order+lines → 新 header+items）")
    body = {
        "dept_id": master["dept_id"],
        "warehouse_id": master["warehouse_id"],
        "remark": MARK + " 转单出参验收",
        "items": [{
            "material_id": master["material_id"],
            "qty": "2",
            "est_price": "50",
            "tax_rate": "13",
            "suggest_supplier_id": master["supplier_id"],
        }],
    }
    data = ok_call("②.1 建采购申请 200", "POST", "/business/erp/purchase-requisition", token, body)
    if not data:
        return None, None
    req_id = data["requisition"]["id"]
    req_no = data["requisition"]["req_no"]

    # 夹具自备状态：本卡验出参形状，不走审批链；直接把自己的草稿推到 APPROVED
    status, payload = api("POST", f"/business/erp/purchase-requisition/{req_id}/submit",
                          token, {"reason": MARK + " 出参口径验收"})
    print(f"       提交审批尝试：HTTP {status} code={payload.get('code')} "
          f"msg={payload.get('msg') or ''}")
    promoted = sql_one(
        f"UPDATE biz_purchase_requisition SET status = 'APPROVED', "
        f"approval_instance_id = NULL, updated_at = now() "
        f"WHERE id = '{req_id}' AND status IN ('DRAFT','SUBMITTED') AND deleted_at IS NULL "
        f"RETURNING status")
    check("②.2 夹具自备状态（自建申请单 → APPROVED，不经审批链）", promoted == "APPROVED",
          f"req_no={req_no} 提交返回值={status}/{payload.get('code')}")

    to_po_body = {"supplier_id": master["supplier_id"], "warehouse_id": master["warehouse_id"],
                  "remark": MARK + " 转单"}
    data = ok_call("②.3 转采购订单 200", "POST",
                   f"/business/erp/purchase-requisition/{req_id}/to-po", token, to_po_body)
    if not data:
        return req_id, None

    check("②.4 同时有新字段 header 和旧字段 purchase_order",
          isinstance(data.get("header"), dict) and isinstance(data.get("purchase_order"), dict),
          f"keys={sorted(data.keys())}")
    check("②.5 同时有新字段 items 和旧字段 lines",
          isinstance(data.get("items"), list) and isinstance(data.get("lines"), list)
          and len(data["items"]) >= 1,
          f"items={len(data.get('items') or [])} lines={len(data.get('lines') or [])}")
    check("②.6 旧字段 purchase_order == 新字段 header（深度相等）",
          data.get("header") == data.get("purchase_order"),
          f"po_no={data.get('header', {}).get('po_no')}")
    check("②.7 旧字段 lines == 新字段 items（深度相等）",
          data.get("items") == data.get("lines"),
          f"rows={len(data.get('items') or [])}")
    check("②.8 meta 带合并统计（merged_count / requisition_nos）",
          data.get("meta", {}).get("merged_count") == 1
          and isinstance(data.get("meta", {}).get("requisition_nos"), list),
          json.dumps(data.get("meta"), ensure_ascii=False))
    check("②.9 旧字段顶层统计未删（merged_count / requisition_nos 仍在 data 上）",
          data.get("merged_count") == 1 and isinstance(data.get("requisition_nos"), list),
          f"merged_count={data.get('merged_count')}")
    po_id = data.get("header", {}).get("po_id")
    check("②.10 生成的采购订单真的落库了",
          sql_one(f"SELECT order_no FROM biz_purchase_order WHERE id = '{po_id}' "
                  f"AND deleted_at IS NULL") == data.get("header", {}).get("po_no"),
          f"po_id={po_id}")
    return req_id, po_id


# ==================== ③ 采购订单提交审批 ====================

def case_po_submit(token, master, po_id):
    step("③ POST /business/erp/purchase-order/{id}/submit-approval（旧 order → 新 header）")
    if po_id:
        # 用 ② 转出来的那张单（还是 DRAFT，可以提交）
        order_id = po_id
    else:
        body = {
            "supplier_id": master["supplier_id"],
            "warehouse_id": master["warehouse_id"],
            "remark": MARK + " 提交审批出参验收",
            "items": [{"material_id": master["material_id"], "qty": "1",
                       "price_excl": "50", "tax_rate": "13"}],
        }
        data = ok_call("③.0 建采购订单 200", "POST", "/business/erp/purchase-order", token, body)
        if not data:
            return None
        order_id = data["order"]["id"]

    data = ok_call("③.1 提交采购订单审批 200", "POST",
                   f"/business/erp/purchase-order/{order_id}/submit-approval", token, {})
    if not data:
        return order_id

    check("③.2 同时有新字段 header 和旧字段 order",
          isinstance(data.get("header"), dict) and isinstance(data.get("order"), dict),
          f"keys={sorted(data.keys())}")
    check("③.3 明细在新字段 items 里", isinstance(data.get("items"), list) and len(data["items"]) >= 1,
          f"items={len(data.get('items') or [])}")
    check("③.4 旧字段 order == 新字段 header（深度相等）",
          data.get("header") == data.get("order"),
          f"order_no={data.get('header', {}).get('order_no')}")
    meta = data.get("meta") or {}
    check("③.5 meta 带审批实例三元组（instance_id / flow_status）",
          bool(meta.get("instance_id")) and meta.get("flow_status") in
          ("PENDING", "APPROVED", "REJECTED", "CANCELLED", "WITHDRAWN"),
          json.dumps(meta, ensure_ascii=False))
    check("③.6 新 header 里的审批实例号与 meta.instance_id 一致",
          data.get("header", {}).get("approval_instance_id") == meta.get("instance_id"),
          f"header={data.get('header', {}).get('approval_instance_id')} meta={meta.get('instance_id')}")
    check("③.7 单据状态确实是 SUBMITTED（提交真的生效）",
          data.get("header", {}).get("status") == "SUBMITTED",
          data.get("header", {}).get("status"))
    return order_id


# ==================== ④ 文档 ====================

def case_docs():
    step("④ docs/编码规范.md「ERP 接口出参口径」小节 + 待迁移清单")
    path = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__)))), "docs", "编码规范.md")
    text = open(path, encoding="utf-8").read() if os.path.exists(path) else ""
    check("④.1 有小节标题 1.12 ERP 接口出参口径", "### 1.12 ERP 接口出参口径" in text, path)
    check("④.2 有三段式定义（header / items / meta）",
          '"header"' in text and '"items"' in text and '"meta"' in text)
    check("④.3 有待迁移清单（含 SalesOrderRes 等实测项）",
          "待迁移清单" in text and "SalesOrderRes" in text and "RequisitionDetailRes" in text)


# ==================== 收尾 ====================

CLEANUP_SQL = [
    # 审批：待办 → 节点 → 实例（只删本夹具单据挂的实例：采购订单 + 采购申请两条来源）
    "DELETE FROM biz_flow_task WHERE instance_id IN ("
    "SELECT id FROM biz_flow_instance WHERE biz_id IN ("
    "SELECT id FROM biz_purchase_order WHERE remark LIKE '{mark}%' "
    "UNION SELECT id FROM biz_purchase_requisition WHERE remark LIKE '{mark}%'))",
    "DELETE FROM biz_flow_instance_node WHERE instance_id IN ("
    "SELECT id FROM biz_flow_instance WHERE biz_id IN ("
    "SELECT id FROM biz_purchase_order WHERE remark LIKE '{mark}%' "
    "UNION SELECT id FROM biz_purchase_requisition WHERE remark LIKE '{mark}%'))",
    "DELETE FROM biz_flow_instance WHERE biz_id IN ("
    "SELECT id FROM biz_purchase_order WHERE remark LIKE '{mark}%' "
    "UNION SELECT id FROM biz_purchase_requisition WHERE remark LIKE '{mark}%')",
    # 明细 → 单头
    "DELETE FROM biz_purchase_order_item WHERE order_id IN (SELECT id FROM biz_purchase_order WHERE remark LIKE '{mark}%')",
    "DELETE FROM biz_purchase_order WHERE remark LIKE '{mark}%'",
    "DELETE FROM biz_purchase_requisition_item WHERE requisition_id IN "
    "(SELECT id FROM biz_purchase_requisition WHERE remark LIKE '{mark}%')",
    "DELETE FROM biz_purchase_requisition WHERE remark LIKE '{mark}%'",
    "DELETE FROM biz_price_quote WHERE inquiry_id IN (SELECT id FROM biz_price_inquiry WHERE remark LIKE '{mark}%')",
    "DELETE FROM biz_price_inquiry_item WHERE inquiry_id IN (SELECT id FROM biz_price_inquiry WHERE remark LIKE '{mark}%')",
    "DELETE FROM biz_price_inquiry WHERE remark LIKE '{mark}%'",
]

RESIDUE_SQL = [
    ("采购订单", "SELECT count(*) FROM biz_purchase_order WHERE remark LIKE '{mark}%'"),
    ("采购申请", "SELECT count(*) FROM biz_purchase_requisition WHERE remark LIKE '{mark}%'"),
    ("询价单", "SELECT count(*) FROM biz_price_inquiry WHERE remark LIKE '{mark}%'"),
]


def cleanup():
    step("⑤ 收尾：删掉本脚本自建的夹具并回查残留")
    for sql in CLEANUP_SQL:
        try:
            psql(sql.format(mark=MARK))
        except SystemExit as error:
            print("       [warn] 清理语句失败（继续）：" + str(error)[:200])
    residue = 0
    for label, sql in RESIDUE_SQL:
        count = sql_one(sql.format(mark=MARK))
        residue += int(count or 0)
        print(f"       {label} 残留 = {count}")
    check("⑤.1 夹具零残留", residue == 0, f"{MARK} 合计残留 = {residue}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default=DEFAULTS["base"])
    parser.add_argument("--env-file", default=DEFAULTS["env_file"])
    args = parser.parse_args()
    CFG["base"] = args.base.rstrip("/")
    ENV.update(load_env_file(args.env_file))

    print(f"base = {CFG['base']}")
    print(f"db   = {db_conf()['user']}@{db_conf()['host']}:{db_conf()['port']}/{db_conf()['name']}")
    print(f"夹具前缀 = {MARK}")

    token = admin_session()
    master = master_data()
    if not all(master.values()):
        raise SystemExit("缺主数据，无法继续")

    inquiry_id = None
    req_id = None
    po_id = None
    try:
        inquiry_id = case_price_inquiry(token, master)
        req_id, po_id = case_requisition_to_po(token, master)
        case_po_submit(token, master, po_id)
        case_docs()
    finally:
        cleanup()

    print()
    print("=" * 78)
    print(f"PASS={PASSED} FAIL={FAILED}")
    if FAILS:
        print("失败项：")
        for name in FAILS:
            print("  · " + name)
    print("=" * 78)
    return 0 if FAILED == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
