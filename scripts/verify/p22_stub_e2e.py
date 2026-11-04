#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
P2-2 摆设字段处置端到端验收（隔离实例 + 真 HTTP，不碰生产）
============================================================

要证明的事（每条都给 `[PASS]/[FAIL]` 与原始数字）：

  ① `biz_supplier.credit_limit` **真的生效**：额度 1 万、下单 2 万 → 提交后单据
     `over_credit_limit = true`、`credit_exposure = 20000`、`credit_limit_snapshot = 10000`；
     草稿阶段是「未评估」（`credit_exposure` 为空），不是「评估过没超」；
  ② 审批单 `form_data` 里带 `over_credit_limit = true`（JSON 布尔）+ `credit_exposure` /
     `credit_limit`（JSON 数字）—— 这是流程管理员配「超信用额度加一道复核」的变量；
  ③ 额度 10 万、下单 8 千 → **不**标记超限，但累计照记（审计用）；
  ④ 额度 0（= 不限）→ 不校验；
  ⑤ **作废的采购单不计入累计**：额度 3 万，先下 2 万作废、再下 2 万 → 累计仍是 2 万、不超限；
  ⑥ `biz_material.safety_stock` 走真实物料接口能写能读（50 → 回读 50；改 0 = 关掉提醒）；
  ⑦ `biz_inventory.locked_qty` 仍是预留：接口照常返回该列且恒为 0（前端已不显示该列，
     后端保留列 + 注释「预留」）。

跑法：

    bash scripts/verify/p21_instance.sh start          # 隔离实例 127.0.0.1:18099 + 副本库
    python3 scripts/verify/p22_stub_e2e.py             # 默认端口 18099 / 库 cloudpivot_p16md
    bash scripts/verify/p21_instance.sh stop

脚本自带前缀清场（`P22E2E-`），可重复跑；**不**碰生产库、不改流程定义。
退出码：全 PASS → 0；有任何 FAIL → 1。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from decimal import Decimal

BASE = "http://127.0.0.1:18099"
DB = "cloudpivot_p16md"
DB_USER = "alun"
DB_PASS = "alun"
TOKEN = ""
RESULTS = []

# 夹具（按前缀可清场；主数据用固定 UUID，供应商/采购单由**真实接口**创建）
PREFIX = "P22E2E"
SUPPLIER_CODE = "P22E2E-SUP"
SUPPLIER_ID = "e22e0001-0000-4000-8000-000000000002"
WAREHOUSE_ID = "d1370001-0000-4000-8000-000000000001"   # 副本库既有 WH01（NORMAL）
MATERIAL_ID = "d1370003-0000-4000-8000-000000000005"    # 副本库既有物料（先存一份原 safety_stock）
MATERIAL_SAFETY_BACKUP = None


# ---------------------------------------------------------------- 基础设施

def psql(sql, db=None):
    env = dict(os.environ)
    env["PGPASSWORD"] = DB_PASS
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-p", "5432", "-U", DB_USER, "-d", db or DB,
         "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(f"psql 失败：{r.stderr.strip()[:400]}\nSQL: {sql[:200]}")
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line != ""]


def scalar(sql, db=None):
    rows = psql(sql, db)
    return rows[0][0] if rows and rows[0] else None


def num(value):
    return Decimal(str(value if value is not None else "0").strip() or "0")


def ok(name, cond, detail=""):
    RESULTS.append((bool(cond), name, detail))
    print(f"  {'[PASS]' if cond else '[FAIL]'} {name}" + (f"  — {detail}" if detail and not cond else ""))


def call(method: str, path: str, body=None) -> tuple[int, dict]:
    # ⚠ 后端路由宏路径不含 `/api` 前缀（由 [router] prefix 注入），脚本里必须写全
    if not path.startswith("/api/"):
        path = "/api" + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if TOKEN:
        req.add_header("Authorization", f"Bearer {TOKEN}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except Exception:
            return err.code, {"raw": raw}


def login():
    global TOKEN

    def attempt(password):
        for _ in range(10):
            status, body = call("POST", "/api/system/auth/login",
                                {"username": "admin", "password": password})
            if status == 429:
                time.sleep(12)
                continue
            return status, body
        return 429, {"msg": "登录限流窗口过长"}

    status, body = attempt("Admin@123456")
    if status != 200:
        status, body = attempt("Admin@1234567")
    if status != 200:
        raise RuntimeError(f"登录失败：{status} {body}")
    data = body.get("data") or {}
    TOKEN = data.get("access_token") or data.get("token")
    if not TOKEN:
        raise RuntimeError(f"登录响应里没有令牌：{body}")
    print(f"  登录成功（令牌 {TOKEN[:12]}…）")


# ---------------------------------------------------------------- 夹具

def cleanup():
    """按前缀清场（可重复跑）。

    ⚠️ **必须按 `supplier_code` 前缀定位，不能按记忆里的供应商 id**：供应商 id 是接口生成的
    uuidv7，进程重启后 `SUPPLIER_ID` 常量就过期了 —— 上一次中断的运行留下的单据/实例会
    清不掉（实测：`zz_teardown` 的全库断言因此红）。所以这里一律用「编码前缀」这条稳定锚点，
    并先把审批实例/待办清掉再删单据（实例 `biz_id` 是弱引用，删完单据就再也找不到它）。
    """
    scope = f"(select id from biz_supplier where supplier_code like '{PREFIX}%')"
    psql(f"""
    delete from biz_flow_task where instance_id in (
        select id from biz_flow_instance where biz_type = 'PO_APPROVE' and biz_id in (
            select id from biz_purchase_order where supplier_id in {scope}));
    delete from biz_flow_cc where instance_id in (
        select id from biz_flow_instance where biz_type = 'PO_APPROVE' and biz_id in (
            select id from biz_purchase_order where supplier_id in {scope}));
    delete from biz_flow_instance where biz_type = 'PO_APPROVE' and biz_id in (
        select id from biz_purchase_order where supplier_id in {scope});
    delete from biz_status_log where entity_type = 'PURCHASE_ORDER' and entity_id in (
        select id from biz_purchase_order where supplier_id in {scope});
    delete from biz_purchase_order where supplier_id in {scope};
    delete from biz_supplier where supplier_code like '{PREFIX}%';
    """)


def fixture():
    """副本库里的主数据（只读校验 + 素材）。物料原安全库存先备份，跑完还原。"""
    global MATERIAL_SAFETY_BACKUP
    MATERIAL_SAFETY_BACKUP = scalar(
        f"select safety_stock from biz_material where id = '{MATERIAL_ID}'::uuid")
    wh = scalar(f"select warehouse_code from biz_warehouse where id = '{WAREHOUSE_ID}'::uuid")
    mat = scalar(f"select material_code from biz_material where id = '{MATERIAL_ID}'::uuid")
    if not wh or not mat:
        raise RuntimeError("副本库缺少夹具主数据（WH01 / M-W1A-005），先跑一次迁移 + 种子")
    print(f"  夹具：仓库 {wh} / 物料 {mat}（原安全库存 {MATERIAL_SAFETY_BACKUP}）")


def create_supplier(credit_limit):
    """建夹具供应商（走真实接口；额度用 credit_limit 传，0 = 不限）。

    接口自己生成 id —— 这里**不接受**外部指定 id，所以下面 `SUPPLIER_ID` 只用于清场查询
    前的兜底（真实 id 由响应回填）。
    """
    global SUPPLIER_ID
    psql(f"delete from biz_supplier where supplier_code like '{PREFIX}%';")
    status, body = call("POST", "/api/business/erp/supplier", {
        "supplier_code": f"{PREFIX}-SUP",
        "supplier_name": "P2-2 验收供应商",
        "credit_days": 30,
        "credit_limit": credit_limit,
        "status": 1,
    })
    if status != 200:
        raise RuntimeError(f"建供应商失败：{status} {body}")
    data = body.get("data") or {}
    SUPPLIER_ID = data.get("id") or SUPPLIER_ID
    return data


def set_supplier_credit(credit_limit):
    """改额度（走真实接口 PUT /supplier/{id}）。"""
    status, body = call("PUT", f"/api/business/erp/supplier/{SUPPLIER_ID}", {
        "credit_limit": credit_limit,
    })
    if status != 200:
        raise RuntimeError(f"改供应商额度失败：{status} {body}")
    return body.get("data") or {}


def create_order(amount):
    """下一张含税金额 = amount 的现货采购单（单行、税率 0）。"""
    status, body = call("POST", "/api/business/erp/purchase-order", {
        "supplier_id": SUPPLIER_ID,
        "warehouse_id": WAREHOUSE_ID,
        "price_mode": "EXCL",
        "items": [{
            "material_id": MATERIAL_ID,
            "qty": 1,
            "price_excl": amount,
            "tax_rate": 0,
        }],
    })
    if status != 200:
        raise RuntimeError(f"建采购单失败：{status} {body}")
    return body["data"]["order"]


def submit(order_id):
    return call("POST", f"/api/business/erp/purchase-order/{order_id}/submit-approval", {})


def detail(order_id):
    status, body = call("GET", f"/api/business/erp/purchase-order/{order_id}")
    if status != 200:
        raise RuntimeError(f"查采购单失败：{status} {body}")
    return body.get("data") or {}


def instance_form_data(instance_id):
    """审批单详情里的 form_data（真机读的就是审批人看到的那个）。"""
    status, body = call("GET", f"/api/business/flow/instance/{instance_id}")
    if status != 200:
        return {}
    data = body.get("data") or {}
    return data.get("form_data") or (data.get("instance") or {}).get("form_data") or {}


# ---------------------------------------------------------------- 主流程

def main():
    global BASE, DB
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=18099)
    parser.add_argument("--db", default="cloudpivot_p16md")
    args = parser.parse_args()
    BASE = f"http://127.0.0.1:{args.port}"
    DB = args.db

    print("== 0. 登录 + 清场 + 夹具 ==")
    login()
    cleanup()
    fixture()

    # ---------------- ① 额度 1 万 / 下单 2 万 → 超限标记 ----------------
    print("\n== ① 超限：额度 1 万，下单 2 万 ==")
    create_supplier(10000)
    order1 = create_order("20000")
    ok("① 建单成功", bool(order1.get("id")), str(order1)[:200])
    ok("① 草稿阶段「未评估」（credit_exposure 为空,不是 0）",
       order1.get("credit_exposure") in (None, ""),
       f"credit_exposure={order1.get('credit_exposure')!r}")
    ok("① 草稿阶段 over_credit_limit = false",
       order1.get("over_credit_limit") is False,
       f"over_credit_limit={order1.get('over_credit_limit')!r}")

    status, body = submit(order1["id"])
    ok("① 超限单允许提交（策略 B：允许 + 强制审批）", status == 200, f"{status} {body}")
    got = detail(order1["id"])
    ok("① 提交后 over_credit_limit = true", got.get("over_credit_limit") is True,
       f"over_credit_limit={got.get('over_credit_limit')!r}")
    ok("① credit_exposure = 20000（累计含本单）",
       num(got.get("credit_exposure")) == Decimal("20000"), f"{got.get('credit_exposure')!r}")
    ok("① credit_limit_snapshot = 10000（提交时点额度快照）",
       num(got.get("credit_limit_snapshot")) == Decimal("10000"), f"{got.get('credit_limit_snapshot')!r}")
    ok("① 单据状态 = SUBMITTED", got.get("status") == "SUBMITTED", str(got.get("status")))

    # ---------------- ② 审批单 form_data 的信用三键 ----------------
    print("\n== ② 审批单 form_data（流程条件分支的变量） ==")
    instance_id = got.get("approval_instance_id")
    ok("② 单据上有审批实例 id", bool(instance_id), str(instance_id))
    form = instance_form_data(instance_id) if instance_id else {}
    ok("② form_data.over_credit_limit 是 JSON 布尔 true",
       form.get("over_credit_limit") is True, f"{form.get('over_credit_limit')!r}")
    ok("② form_data.credit_exposure 是 JSON 数字 20000",
       isinstance(form.get("credit_exposure"), (int, float))
       and num(form.get("credit_exposure")) == Decimal("20000"),
       f"{form.get('credit_exposure')!r}")
    ok("② form_data.credit_limit 是 JSON 数字 10000",
       isinstance(form.get("credit_limit"), (int, float))
       and num(form.get("credit_limit")) == Decimal("10000"),
       f"{form.get('credit_limit')!r}")
    # 库里再确认一次（审批人零 ERP 权限也能看到的冻结快照）
    db_form = scalar(
        f"select form_data::text from biz_flow_instance where id = '{instance_id}'::uuid") if instance_id else None
    ok("② 库内 form_data 同样带 over_credit_limit=true",
       bool(db_form) and json.loads(db_form).get("over_credit_limit") is True,
       str(db_form)[:120])

    # ---------------- ③ 额度内不标记 ----------------
    print("\n== ③ 额度 10 万 / 下单 8 千 → 不超限，但累计照记 ==")
    set_supplier_credit(100000)
    order2 = create_order("8000")
    status, body = submit(order2["id"])
    ok("③ 额度内提交成功", status == 200, f"{status} {body}")
    got2 = detail(order2["id"])
    ok("③ over_credit_limit = false", got2.get("over_credit_limit") is False,
       f"{got2.get('over_credit_limit')!r}")
    ok("③ credit_exposure = 28000（前一张 2 万 + 本单 8 千；不含作废单）",
       num(got2.get("credit_exposure")) == Decimal("28000"), f"{got2.get('credit_exposure')!r}")
    ok("③ credit_limit_snapshot = 100000",
       num(got2.get("credit_limit_snapshot")) == Decimal("100000"),
       f"{got2.get('credit_limit_snapshot')!r}")

    # ---------------- ④ 额度 0 = 不限 ----------------
    print("\n== ④ 额度 0（= 不限）→ 不校验 ==")
    set_supplier_credit(0)
    order3 = create_order("50000")
    status, body = submit(order3["id"])
    ok("④ 不限额度时任何金额都能提交", status == 200, f"{status} {body}")
    got3 = detail(order3["id"])
    ok("④ over_credit_limit = false（0 不能被当成阈值）",
       got3.get("over_credit_limit") is False, f"{got3.get('over_credit_limit')!r}")
    ok("④ credit_limit_snapshot = 0（不限）",
       num(got3.get("credit_limit_snapshot")) == 0, f"{got3.get('credit_limit_snapshot')!r}")

    # ---------------- ⑤ 作废单不计入累计 ----------------
    print("\n== ⑤ 作废单不计入累计 ==")
    cleanup()
    create_supplier(30000)
    dead = create_order("20000")
    status, body = call("PATCH", f"/api/business/erp/purchase-order/{dead['id']}/status",
                        {"status": "CANCELLED"})
    ok("⑤ 第一张 2 万单作废成功", status == 200, f"{status} {body}")
    live = create_order("20000")
    status, body = submit(live["id"])
    ok("⑤ 额度 3 万 / 在跑 2 万：第二张 2 万不该被拦", status == 200, f"{status} {body}")
    got5 = detail(live["id"])
    ok("⑤ credit_exposure = 20000（作废的 2 万没算进去）",
       num(got5.get("credit_exposure")) == Decimal("20000"), f"{got5.get('credit_exposure')!r}")
    ok("⑤ over_credit_limit = false",
       got5.get("over_credit_limit") is False, f"{got5.get('over_credit_limit')!r}")

    # ---------------- ⑥ 安全库存走真实接口 ----------------
    print("\n== ⑥ 安全库存：物料接口能写能读 ==")
    status, body = call("PUT", f"/api/business/erp/material/{MATERIAL_ID}", {"safety_stock": 50})
    ok("⑥ 物料改安全库存 = 50 成功", status == 200, f"{status} {body}")
    status, body = call("GET", f"/api/business/erp/material/{MATERIAL_ID}")
    got6 = body.get("data") or {}
    ok("⑥ 回读 safety_stock = 50（提醒任务下一轮扫描就用这个阈值）",
       num(got6.get("safety_stock")) == Decimal("50"), f"{got6.get('safety_stock')!r}")
    # 留空（0）= 关掉提醒：这里只验「能改成 0 且回读 0」，扫描口径由单测覆盖
    status, _ = call("PUT", f"/api/business/erp/material/{MATERIAL_ID}", {"safety_stock": 0})
    ok("⑥ 改回 0（= 不提醒）成功", status == 200, str(status))

    # ---------------- ⑦ locked_qty 仍是预留列 ----------------
    print("\n== ⑦ 锁定量：后端照常返回，恒为 0（前端已不显示该列） ==")
    status, body = call("GET", "/api/business/erp/inventory/page?page=1&page_size=50")
    rows = (body.get("data") or {}).get("list") or []
    total_locked = sum(num(row.get("locked_qty")) for row in rows)
    ok("⑦ 库存接口仍返回 locked_qty 且全部为 0",
       all("locked_qty" in row for row in rows) and total_locked == 0,
       f"行数 {len(rows)} / 合计锁定量 {total_locked}")

    # ---------------- 清场 ----------------
    print("\n== 收尾：清场 + 还原夹具 ==")
    cleanup()
    if MATERIAL_SAFETY_BACKUP is not None:
        psql(f"update biz_material set safety_stock = {MATERIAL_SAFETY_BACKUP} "
             f"where id = '{MATERIAL_ID}'::uuid;")
        print(f"  物料安全库存已还原为 {MATERIAL_SAFETY_BACKUP}")
    left = scalar(f"select count(*) from biz_purchase_order where supplier_id = '{SUPPLIER_ID}'::uuid")
    ok("清理：夹具采购单归零", num(left) == 0, f"剩 {left}")

    passed = sum(1 for okflag, _, _ in RESULTS if okflag)
    failed = len(RESULTS) - passed
    print(f"\n==== P2-2 e2e 结果：PASS {passed} / FAIL {failed}（共 {len(RESULTS)} 条） ====")
    if failed:
        for passed_flag, fail_name, fail_detail in RESULTS:
            if not passed_flag:
                print(f"  FAIL: {fail_name} — {fail_detail}")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
