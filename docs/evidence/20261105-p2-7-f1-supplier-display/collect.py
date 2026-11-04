#!/usr/bin/env python3
"""P2-7-F1 证据采集：采购单供应商名「现值 → 快照回落，禁止落 uuid」。

对着本机隔离实例 127.0.0.1:18171（库 cloudpivot_p27f1）跑，**只动这个库**：
1. 建一张现货采购单（供应商 = 种子供应商），记下 `supplier_name`（= 下单时快照）；
2. 供应商改名 → 主档现值变新名，采购单快照仍是旧名（后端不改历史快照）；
3. 把供应商软删（DB 级，API 对「被采购单引用」的供应商是 409 拒绝，见 evidence README）→
   主档清单查不到它 → 前端必须回落到单据快照名，而不是 uuid。

输出：`step-1` … `step-3` 的接口 JSON 摘要（脱敏，不含 token）。
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18171"
OUT = os.path.dirname(os.path.abspath(__file__))
DB = "cloudpivot_p27f1"
ADMIN_USER = os.environ.get("P27_ADMIN_USER", "admin")

SUPPLIER_ID = "d1370005-0000-4000-8000-000000000001"
WAREHOUSE_ID = "d1370001-0000-4000-8000-000000000001"
MATERIAL_ID = "d1370003-0000-4000-8000-000000000001"
OLD_NAME = "示例供应商（种子）"
NEW_NAME = "华信智造"


def api(method: str, path: str, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode()
        raise SystemExit(f"HTTP {exc.code} {method} {path} → {detail}") from exc


def psql(sql: str) -> str:
    env = dict(os.environ, PGPASSWORD="alun")
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-At", "-F", "|", "-c", sql],
        capture_output=True, text=True, env=env, check=True,
    )
    return out.stdout.strip()


def supplier_ids() -> list[str]:
    page = api("GET", "/api/business/erp/supplier/page?status=1&page=1&page_size=200", token=TOKEN)
    return [row["id"] for row in page["data"]["list"]]


def po_rows() -> list[dict]:
    page = api("GET", "/api/business/erp/purchase-order/page?page=1&page_size=20", token=TOKEN)
    return page["data"]["list"]


def po_row(order_id: str) -> dict:
    return api("GET", f"/api/business/erp/purchase-order/{order_id}", token=TOKEN)["data"]


def pick(row: dict) -> dict:
    """只留本卡口径相关的字段，别把整行 40 个字段塞进证据。"""
    return {
        "order_no": row["order_no"],
        "supplier_id": row["supplier_id"],
        "supplier_name": row["supplier_name"],
        "supplier_name_snapshot": row["supplier_name_snapshot"],
    }


def dump(name: str, payload) -> None:
    path = os.path.join(OUT, name)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(payload, fh, ensure_ascii=False, indent=2)
    print(f"wrote {path}")


def login_token(password: str) -> str:
    return api("POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": password})["data"]["access_token"]


# 口令不落仓：从环境变量读（隔离库的初始管理员口令 / 改密后的口令）。
# 初始口令在空库迁移时由后端 `NIK_ADMIN_PASSWORD` 决定，本脚本不知道它的值。
INITIAL_PASSWORD = os.environ.get("P27_INITIAL_PASSWORD", "")
APP_PASSWORD = os.environ.get("P27_APP_PASSWORD", "")
if not INITIAL_PASSWORD or not APP_PASSWORD:
    raise SystemExit("请先导出 P27_INITIAL_PASSWORD / P27_APP_PASSWORD（隔离库管理员口令，勿写进仓库）")

# 首次登录强制改初始密码（否则业务写接口 403）；改成本次运行的口令，脚本可重跑
try:
    TOKEN = login_token(APP_PASSWORD)
except SystemExit:
    TOKEN = login_token(INITIAL_PASSWORD)
    api("PUT", "/api/system/auth/password", {"old_password": INITIAL_PASSWORD, "new_password": APP_PASSWORD}, token=TOKEN)
    TOKEN = login_token(APP_PASSWORD)

# ---- 前置：把种子供应商恢复成「旧名 + 未删」，避免脚本重跑时状态叠加 ----
psql(f"UPDATE biz_supplier SET supplier_name = '{OLD_NAME}', deleted_at = NULL WHERE id = '{SUPPLIER_ID}'")

# ---- step 1：建单（快照 = 下单时主档现值） ----
created = api(
    "POST",
    "/api/business/erp/purchase-order",
    {
        "supplier_id": SUPPLIER_ID,
        "purchase_type": "SPOT",
        "warehouse_id": WAREHOUSE_ID,
        "price_mode": "EXCL",
        "credit_days": 30,
        "remark": "P2-7-F1 供应商名口径证据",
        "items": [{"material_id": MATERIAL_ID, "qty": 10, "price_excl": 100, "tax_rate": 13}],
    },
    token=TOKEN,
)
ORDER_ID = created["data"]["order"]["id"]
ORDER_NO = created["data"]["order"]["order_no"]

step1 = {
    "说明": "建单后：快照 = 下单时主档现值（后端写 supplier_name_snapshot）",
    "db": psql(
        f"SELECT order_no, supplier_id, supplier_name_snapshot FROM biz_purchase_order WHERE id = '{ORDER_ID}'"
    ),
    "list_row": pick(next(row for row in po_rows() if row["id"] == ORDER_ID)),
    "detail_row": pick(po_row(ORDER_ID)),
}
dump("step-1-after-create.json", step1)

# ---- step 2：供应商改名 → 现值变新名，单据快照不动 ----
api("PUT", f"/api/business/erp/supplier/{SUPPLIER_ID}", {"supplier_name": NEW_NAME}, token=TOKEN)
masters = api("GET", "/api/business/erp/supplier/page?status=1&page=1&page_size=200", token=TOKEN)["data"]["list"]
master = next(row for row in masters if row["id"] == SUPPLIER_ID)
step2 = {
    "说明": "改名后：主档现值 = 新名；采购单列表/详情下发的 supplier_name 仍是旧名（快照，历史不被改写）",
    "master_current_name": master["supplier_name"],
    "list_row": pick(next(row for row in po_rows() if row["id"] == ORDER_ID)),
    "detail_row": pick(po_row(ORDER_ID)),
    "db": psql(
        f"SELECT order_no, supplier_name_snapshot FROM biz_purchase_order WHERE id = '{ORDER_ID}'"
    ),
}
dump("step-2-after-rename.json", step2)

# ---- step 3：主档软删（走产品 API）→ 主档清单查不到 → 必须回落快照名 ----
try:
    api("DELETE", f"/api/business/erp/supplier/{SUPPLIER_ID}", token=TOKEN)
    step3_delete_via_api = "DELETE /erp/supplier/{id} 软删成功（本版后端未拦截被草稿单引用）"
except SystemExit as exc:
    step3_delete_via_api = f"API 拒绝（按设计拦被引用主档，改为 DB 级软删复现历史脏数据）：{exc}"
    psql(f"UPDATE biz_supplier SET deleted_at = now() WHERE id = '{SUPPLIER_ID}'")
ids_after = supplier_ids()
step3 = {
    "说明": "主档软删后：supplier/page 已查不到该 id（前端 Map 未命中）→ 列表/详情必须显示快照名，禁止 uuid",
    "delete_via_api": step3_delete_via_api,
    "master_list_contains_supplier": SUPPLIER_ID in ids_after,
    "master_list_size": len(ids_after),
    "list_row": pick(next(row for row in po_rows() if row["id"] == ORDER_ID)),
    "detail_row": pick(po_row(ORDER_ID)),
    "db_deleted_at": psql(f"SELECT deleted_at IS NOT NULL FROM biz_supplier WHERE id = '{SUPPLIER_ID}'"),
}
dump("step-3-after-soft-delete.json", step3)

# ---- 断言：三步的口径都对得上（脚本自己判，别让读证据的人替我们判） ----
checks = {
    "step1_快照=下单时现值": step1["list_row"]["supplier_name"] == OLD_NAME
    and step1["db"].endswith(OLD_NAME),
    "step2_主档现值=新名": step2["master_current_name"] == NEW_NAME,
    "step2_单据仍下发旧名快照": step2["list_row"]["supplier_name"] == OLD_NAME
    and step2["detail_row"]["supplier_name"] == OLD_NAME,
    "step3_主档清单已不含该供应商": step3["master_list_contains_supplier"] is False,
    "step3_列表/详情仍下发旧名快照（非 uuid）": step3["list_row"]["supplier_name"] == OLD_NAME
    and step3["detail_row"]["supplier_name"] == OLD_NAME,
    "step3_后端确实软删": step3["db_deleted_at"] == "t",
}
dump("checks.json", checks)
print(json.dumps({"order_id": ORDER_ID, "order_no": ORDER_NO, "supplier_id": SUPPLIER_ID}, ensure_ascii=False))
print(json.dumps(checks, ensure_ascii=False, indent=2))
if not all(checks.values()):
    raise SystemExit("口径断言失败：" + json.dumps(checks, ensure_ascii=False))
