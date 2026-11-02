#!/usr/bin/env python3
"""ERP-B2 真机验收：制单 ≠ 过账（内控可配开关）。

# 验的不是「接口返 200」，而是卡面 §四 的四步真机链路

```
① 参数页（/api/support/config/page）能看到「ERP 内控」四条开关，默认 false/false/true/true
② 开关开启后：laoliu 类账号（制单人）提交过账 → 409 + 66013 + 点名开关 + 给出「关闭该开关」
③ 同一张单换 admin 过账 → 200，posted_by = admin（≠ 制单人）
④ 关掉开关 → 制单人自己过账 → 200
```

外加卡面 §三 的三条只能在真机钉住的用例：

* ⑥ 参数不存在（软删该行）→ 回退代码默认（post=false）→ 制单人仍能过账；
* ⑦ 参数值非法（`yes`）→ 回退默认 + 服务端日志 `ERP 内控开关取值非法`；
* ⑧ 开关改完**立即生效**（不调 `/support/config/reload`，下一枪就变）。

# 关键背景（不写死会验错方向）

种子流程 `stock_in_post` 对 **< 1000 元**的单据「免审直过」（NOTIFY 节点不排审批），
过账人就是提交人本人 —— 这正是评审实测「收货单制单人 = 过账人 8 条」的来源。
所以本脚本刻意用 **< 1000 元**的小额单跑主链路：它走的是「提交 → 免审自动过账」
那条路，而不是「没配流程的降级直过」。

# 用法

```bash
# 先起隔离实例（库 erpb2_e2e / 端口 18097），见 scripts/verify/b2_dbsetup.sh
B2_BASE_URL=http://127.0.0.1:18097 B2_DB_NAME=erpb2_e2e \
B2_DB_USER=alun B2_DB_PASSWORD=alun B2_LOG=/tmp/b2_server.log \
  python3 scripts/verify/b2_separation_e2e.py
```

退出码：0 = 全绿；1 = 有断言失败；2 = 环境不满足。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("B2_BASE_URL", "http://127.0.0.1:18097").rstrip("/")
DB_NAME = os.environ.get("B2_DB_NAME", "erpb2_e2e")
DB_USER = os.environ.get("B2_DB_USER", "alun")
DB_PASS = os.environ.get("B2_DB_PASSWORD", "alun")
DB_HOST = os.environ.get("B2_DB_HOST", "127.0.0.1")
DB_PORT = os.environ.get("B2_DB_PORT", "5432")
LOG = os.environ.get("B2_LOG", "/tmp/b2_server.log")
KEEP = os.environ.get("B2_KEEP") == "1"

ADMIN_USER = os.environ.get("B2_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("B2_ADMIN_PASSWORD", "Admin@123456")
ADMIN_NEW_PASS = os.environ.get("B2_ADMIN_NEW_PASSWORD", "B2Admin#2026x")

MAKER_USER = "b2maker"
MAKER_PASS = "B2Maker@12345"

POST_KEY = "erp.separation_of_duties.post"
MASTER_KEY = "erp.separation_of_duties.enabled"
APPROVE_KEY = "erp.separation_of_duties.approve"
PAY_KEY = "erp.separation_of_duties.pay"
SEPARATION_CODE = 66013

UNIT_ID = "b2e2e001-0000-4000-8000-000000000001"
SUPPLIER_ID = "b2e2e002-0000-4000-8000-000000000001"
WAREHOUSE_ID = "b2e2e003-0000-4000-8000-000000000001"
MATERIAL_ID = "b2e2e004-0000-4000-8000-000000000001"

PASSED: list[str] = []
FAILED: list[str] = []


def check(ok: bool, name: str, detail: str = "") -> bool:
    if ok:
        PASSED.append(name)
        print(f"[PASS] {name}")
    else:
        FAILED.append(name)
        print(f"[FAIL] {name}" + (f" — {detail}" if detail else ""))
    return ok


def psql(sql: str) -> str:
    args = [
        "psql", "-h", DB_HOST, "-p", DB_PORT, "-U", DB_USER, "-d", DB_NAME,
        "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tAc", sql,
    ]
    env = dict(os.environ, PGPASSWORD=DB_PASS)
    out = subprocess.run(args, capture_output=True, text=True, env=env, check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}\nSQL: {sql}")
    return out.stdout.strip()


def scalar(sql: str) -> str:
    """首行首列（`-tAc` 会把 `INSERT 0 1` 这类命令标签也打出来，一律取第一行）。"""
    return (psql(sql).splitlines() or [""])[0].strip()


def call(method: str, path: str, body: dict | None = None,
         token: str | None = None) -> tuple[int, dict]:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(user: str, password: str, allow_change: bool = False) -> str | None:
    status, body = call("POST", "/api/system/auth/login",
                        {"username": user, "password": password})
    if status != 200:
        print(f"[info] 登录 {user} 失败：{status} {body}")
        return None
    data = body.get("data") or {}
    token = data.get("access_token")
    if data.get("must_change_password"):
        if not allow_change:
            raise SystemExit(f"{user} 被要求先改初始口令，但本脚本不允许改")
        st, resp = call("PUT", "/api/system/auth/password",
                        {"old_password": password, "new_password": ADMIN_NEW_PASS}, token)
        if st != 200:
            raise SystemExit(f"改初始口令失败：{st} {resp}")
        status, body = call("POST", "/api/system/auth/login",
                            {"username": user, "password": ADMIN_NEW_PASS})
        if status != 200:
            raise SystemExit(f"改完口令后登录失败：{status} {body}")
        token = (body.get("data") or {}).get("access_token")
    return token


def setup_fixtures() -> None:
    psql(
        """
        INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('%(unit)s', 'B2E2E-PCS', '个', 1, NULL, 1.000000, 1, 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('%(sup)s', 'B2E2E-SUP', 'B2 真机供应商', 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('%(wh)s', 'B2E2E-WH', 'B2 真机仓', '/%(wh)s/', 1, 1, 1)
        ON CONFLICT DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('%(mat)s', 'B2E2E-M1', 'B2 真机螺栓', '%(unit)s', 1, false)
        ON CONFLICT DO NOTHING;
        """
        % {"unit": UNIT_ID, "sup": SUPPLIER_ID, "wh": WAREHOUSE_ID, "mat": MATERIAL_ID}
    )


def cleanup_docs() -> None:
    stmts = [
        f"DELETE FROM biz_inventory_txn WHERE doc_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_inventory_location WHERE material_id = '{MATERIAL_ID}'::uuid AND warehouse_id = '{WAREHOUSE_ID}'::uuid",
        f"DELETE FROM biz_inventory WHERE material_id = '{MATERIAL_ID}'::uuid AND warehouse_id = '{WAREHOUSE_ID}'::uuid",
        f"DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid))",
        f"DELETE FROM biz_voucher WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND entity_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        # 实例/待办：biz_id 是弱引用，必须在删单据**之前**按单据定位（否则子查询查不到）
        f"DELETE FROM biz_flow_task WHERE instance_id IN (SELECT approval_instance_id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid AND approval_instance_id IS NOT NULL)",
        f"DELETE FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_stock_in WHERE supplier_id = '{SUPPLIER_ID}'::uuid",
    ]
    for sql in stmts:
        try:
            psql(sql)
        except RuntimeError as error:
            print(f"[warn] 清理失败：{error}")


def cleanup_all() -> None:
    cleanup_docs()
    for sql in (
        f"DELETE FROM biz_material WHERE id = '{MATERIAL_ID}'::uuid",
        f"DELETE FROM biz_warehouse WHERE id = '{WAREHOUSE_ID}'::uuid",
        f"DELETE FROM biz_supplier WHERE id = '{SUPPLIER_ID}'::uuid",
        f"DELETE FROM biz_unit WHERE id = '{UNIT_ID}'::uuid",
        f"DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username = '{MAKER_USER}')",
        f"DELETE FROM sys_user WHERE username = '{MAKER_USER}'",
    ):
        try:
            psql(sql)
        except RuntimeError as error:
            print(f"[warn] 清理失败：{error}")


def ensure_maker() -> str:
    """建一个只当「制单人」的账号（超管角色：本卡验的是内控闸，不是权限码）。"""
    if scalar(f"SELECT count(*) FROM sys_user WHERE username = '{MAKER_USER}' AND deleted_at IS NULL") == "1":
        return scalar(f"SELECT id::text FROM sys_user WHERE username = '{MAKER_USER}'")
    role_id = scalar("SELECT r.id::text FROM sys_role r WHERE r.role_code = 'SUPER_ADMIN' LIMIT 1")
    if not role_id:
        raise SystemExit("库里没有 SUPER_ADMIN 角色：先让实例把 bootstrap 跑完")
    status, body = call("POST", "/api/system/user", {
        "username": MAKER_USER, "password": MAKER_PASS,
        "real_name": "B2 制单人", "status": 1, "remark": "ERP-B2 真机验收夹具",
        "role_ids": [role_id],
    }, ADMIN_TOKEN)
    if status != 200:
        raise SystemExit(f"建制单人账号失败：{status} {body}")
    uid = scalar(f"SELECT id::text FROM sys_user WHERE username = '{MAKER_USER}'")
    # 角色绑定兜底（有的版本建用户时不接受 role_ids）
    bound = scalar(f"SELECT count(*) FROM sys_user_role WHERE user_id = '{uid}'::uuid")
    if bound == "0":
        psql(f"INSERT INTO sys_user_role (user_id, role_id) VALUES ('{uid}'::uuid, '{role_id}'::uuid) ON CONFLICT DO NOTHING")
    return uid


def config_rows() -> dict[str, dict]:
    status, body = call("GET", "/api/support/config/page?page=1&page_size=200", token=ADMIN_TOKEN)
    if status != 200:
        raise SystemExit(f"读参数页失败：{status} {body}")
    items = ((body.get("data") or {}).get("list")) or []
    wanted = {MASTER_KEY, POST_KEY, APPROVE_KEY, PAY_KEY}
    return {item["config_key"]: item for item in items if item.get("config_key") in wanted}


def set_switch(key: str, value: str) -> tuple[int, dict]:
    rows = config_rows()
    row = rows.get(key)
    if not row:
        raise SystemExit(f"参数 {key} 不存在：先跑迁移 V0196")
    return call("PUT", f"/api/support/config/{row['id']}", {
        "config_key": key,
        "config_value": value,
        "config_type": row.get("config_type") or "BOOLEAN",
        "description": row.get("description"),
        "category": row.get("category"),
        "sort_order": row.get("sort_order"),
        "config_level": row.get("config_level"),
        "is_modifiable": True,
        "is_encrypted": False,
        "is_restart": False,
        "status": 1,
        "ref_value": row.get("ref_value"),
        "select_value": row.get("select_value"),
    }, ADMIN_TOKEN)


def create_stock_in(token: str, qty: int, remark: str) -> str:
    status, body = call("POST", "/api/business/erp/stock-in", {
        "supplier_id": SUPPLIER_ID,
        "warehouse_id": WAREHOUSE_ID,
        "in_date": "2026-11-02",
        "remark": remark,
        "items": [{"material_id": MATERIAL_ID, "qty": qty, "price_excl": 10, "tax_rate": 13}],
    }, token)
    if status != 200:
        raise SystemExit(f"建收货单失败：{status} {body}")
    return (body.get("data") or {})["header"]["id"]


def post_doc(doc_id: str, token: str) -> tuple[int, dict]:
    return call("PATCH", f"/api/business/erp/stock-in/{doc_id}/status",
                {"status": "POSTED"}, token)


def doc_state(doc_id: str) -> tuple[str, str, str]:
    raw = psql(
        "SELECT status || '|' || coalesce(posted_by::text, '') || '|' || coalesce(in_no, '') "
        f"FROM biz_stock_in WHERE id = '{doc_id}'::uuid"
    )
    parts = (raw.splitlines() or ["||"])[0].split("|")
    padded = (parts + ["", "", ""])[:3]
    return (padded[0], padded[1], padded[2])


def main() -> int:
    global ADMIN_TOKEN
    print(f"== ERP-B2 制单≠过账 真机验收 @ {BASE} / {DB_NAME}")
    if not scalar("select 1") == "1":
        print("数据库不可用", file=sys.stderr)
        return 2

    ADMIN_TOKEN = login(ADMIN_USER, ADMIN_PASS, allow_change=True) or login(ADMIN_USER, ADMIN_NEW_PASS)
    if not ADMIN_TOKEN:
        print("admin 登录失败（NIK_ADMIN_PASSWORD 是否注入了？）", file=sys.stderr)
        return 2

    cleanup_all()
    setup_fixtures()
    maker_id = ensure_maker()
    maker_token = login(MAKER_USER, MAKER_PASS)
    if not maker_token:
        raise SystemExit("制单人账号登录失败")

    # ---------- ① 参数页：四条开关存在且默认值 = 改动前行为 ----------
    rows = config_rows()
    check(len(rows) == 4, "① 参数页有 4 条 ERP 内控开关", f"实际 {sorted(rows)}")
    defaults = {MASTER_KEY: "false", POST_KEY: "false", APPROVE_KEY: "true", PAY_KEY: "true"}
    for key, expect in defaults.items():
        row = rows.get(key) or {}
        check(
            row.get("config_value") == expect,
            f"① {key} 默认 {expect}",
            f"实际 {row.get('config_value')!r}",
        )
        check(
            row.get("config_type") == "BOOLEAN",
            f"① {key} 类型 BOOLEAN（前端渲染成开关）",
            f"实际 {row.get('config_type')!r}",
        )
    check(
        all((rows.get(k) or {}).get("category") == "erp_internal_control" for k in defaults),
        "① 四条都在 erp_internal_control 分组（配置页自动多出「ERP 内控」）",
        str({k: (v or {}).get("category") for k, v in rows.items()}),
    )

    # ---------- ② 开关关闭（默认）→ 制单人能自己过账 ----------
    doc1 = create_stock_in(maker_token, 2, "ERP-B2 默认关闭")
    status, body = post_doc(doc1, maker_token)
    state, posted_by, in_no = doc_state(doc1)
    check(status == 200 and state == "POSTED", "② 开关默认关闭：制单人过账 200 且已过账", f"{status} {body} {state}")
    check(posted_by == maker_id, "② posted_by = 制单人本人（存量行为不变）", f"{posted_by} != {maker_id}")

    # ---------- ③ 开关开启 → 制单人过账 409 ----------
    st, resp = set_switch(POST_KEY, "true")
    check(st == 200, "③ 参数页打开「制单与过账分离」", f"{st} {resp}")

    doc2 = create_stock_in(maker_token, 2, "ERP-B2 开启后制单人过账")
    status, body = post_doc(doc2, maker_token)
    msg = str(body.get("msg") or "")
    check(status == 409, "③ 制单人过账被拦：HTTP 409", f"{status} {body}")
    check(body.get("code") == SEPARATION_CODE, "③ 业务码 66013（ERP_SEPARATION_OF_DUTIES）", str(body))
    check("系统已开启「制单与过账分离」" in msg, "③ 文案说明「系统已开启制单与过账分离」", msg)
    check("不能自己过账" in msg, "③ 文案说明不能自己过账", msg)
    check("关闭该开关" in msg, "③ 文案给出放行办法（或关闭该开关）", msg)
    check(POST_KEY in msg, "③ 文案带开关键名（管理员知道去哪改）", msg)
    state, posted_by, in_no2 = doc_state(doc2)
    check(
        state == "DRAFT" and posted_by == "",
        "③ 被拦后单据回到 DRAFT（免审分支已打回，不是停在 PENDING_POST 的死单）",
        f"status={state} posted_by={posted_by}",
    )
    txn = scalar(f"SELECT count(*) FROM biz_inventory_txn WHERE doc_id = '{doc2}'::uuid")
    check(txn == "0", "③ 被拦时六本账一动没动（库存流水 0 条）", txn)

    # ---------- ④ 换 admin 过账 → 200 ----------
    status, body = post_doc(doc2, ADMIN_TOKEN)
    state, posted_by, _ = doc_state(doc2)
    check(status == 200 and state == "POSTED", "④ 换 admin 过账 200 且已过账", f"{status} {body} {state}")
    check(posted_by != maker_id and posted_by != "", "④ posted_by = admin（≠ 制单人，内控达成）", posted_by)
    txn = scalar(f"SELECT count(*) FROM biz_inventory_txn WHERE doc_id = '{doc2}'::uuid")
    check(int(txn or 0) > 0, "④ 换人过账后库存流水已落库", txn)

    # ---------- ⑤ 关掉开关 → 制单人又能过账（立即生效，不调 reload） ----------
    st, resp = set_switch(POST_KEY, "false")
    check(st == 200, "⑤ 参数页关闭开关", f"{st} {resp}")
    doc3 = create_stock_in(maker_token, 2, "ERP-B2 关掉后制单人过账")
    status, body = post_doc(doc3, maker_token)
    state, posted_by, _ = doc_state(doc3)
    check(
        status == 200 and state == "POSTED" and posted_by == maker_id,
        "⑤ 关掉后制单人立刻能过账 200（改完立即生效）",
        f"{status} {body} {state} posted_by={posted_by}",
    )

    # ---------- ⑥ 参数不存在 → 回退代码默认（post=false） ----------
    row = config_rows().get(POST_KEY)
    if row:
        st, resp = call("DELETE", f"/api/support/config/{row['id']}", token=ADMIN_TOKEN)
        check(st == 200, "⑥ 软删 post 参数（模拟参数不存在）", f"{st} {resp}")
        doc4 = create_stock_in(maker_token, 2, "ERP-B2 参数不存在")
        status, body = post_doc(doc4, maker_token)
        state, _, _ = doc_state(doc4)
        check(
            status == 200 and state == "POSTED",
            "⑥ 参数不存在 → 用代码默认 false → 制单人仍能过账",
            f"{status} {body} {state}",
        )
        # 复原参数（同一 key 软删后可再插）
        st, resp = call("POST", "/api/support/config", {
            "config_key": POST_KEY, "config_value": "false", "config_type": "BOOLEAN",
            "description": "【制单≠过账】开启后制单人不能自己过账（收货过账 / 调拨发货收货）；小团队可不开，内控要求上建议开启",
            "category": "erp_internal_control", "sort_order": 20, "config_level": 1,
            "is_modifiable": True, "is_encrypted": False, "is_restart": False, "status": 1,
            "ref_value": "true|false", "select_value": "true|开启,false|关闭",
        }, ADMIN_TOKEN)
        check(st == 200, "⑥ 参数已复原", f"{st} {resp}")

    # ---------- ⑦ 非法值（非 bool）→ 回退默认 + 服务端 warn ----------
    st, resp = set_switch(POST_KEY, "yes")
    if st != 200:
        # 参数页对 BOOLEAN 可能做值校验；那就直改库再 reload，模拟「有人手工改库」
        print(f"[info] 参数页不接受非 bool 值（{st}），改走直改库 + reload")
        psql(f"UPDATE sys_config SET config_value = 'yes' WHERE config_key = '{POST_KEY}' AND deleted_at IS NULL")
        st2, resp2 = call("POST", "/api/support/config/reload", token=ADMIN_TOKEN)
        check(st2 == 200, "⑦ 直改库后 reload 触发快照重载", f"{st2} {resp2}")
    doc5 = create_stock_in(maker_token, 2, "ERP-B2 非法值")
    status, body = post_doc(doc5, maker_token)
    state, _, _ = doc_state(doc5)
    check(
        status == 200 and state == "POSTED",
        "⑦ 非法值不参与判定 → 回退默认 false → 制单人仍能过账",
        f"{status} {body} {state}",
    )
    warn_hit = False
    if os.path.exists(LOG):
        with open(LOG, encoding="utf-8", errors="replace") as fh:
            warn_hit = "ERP 内控开关取值非法" in fh.read()
    check(warn_hit, "⑦ 服务端打了 warn：ERP 内控开关取值非法（不是静默当 false）", f"日志 {LOG}")

    # ---------- 清理 ----------
    st, resp = set_switch(POST_KEY, "false")
    if not KEEP:
        cleanup_all()
    print(f"\n== 结果 PASS={len(PASSED)} FAIL={len(FAILED)}")
    for item in FAILED:
        print("  - " + item)
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
