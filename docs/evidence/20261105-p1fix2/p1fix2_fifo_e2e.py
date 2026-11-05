#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-FIX-2 真机验收：出库 FIFO 自动分配（建单预填 + 跨库位自动拆行 + 库存不足文案）。

场景（库存用 psql 造到指定库位，出库全走 HTTP —— 与生产同一条路径）：

  A. 跨库位够量：A 库位 30（早）+ B 库位 30（晚），领用 60（不指定库位）
     → 建单 2 行（FIFO 顺序、各 30）→ 过账 200 一次走通（**修复前这里 409 要用户手工拆行**）
  B. 手改覆盖：显式传 `location_id` = 后建的 B → 只扣 B，A 一分不动
  C. 部分库存：A 20 + B 20，领用 60（差 20）→ 建单带 `stock_warnings`，
     过账 409，文案点名「差多少 / 现有在哪些库位 / 该怎么办」
  D. 完全没货：0 库存领用 5 → 提示「无库存」，过账 409
  E. 账实一致：Σ 库位账 = 成本账（B1）逐物料核对

用法：python3 p1fix2_fifo_e2e.py <base_url> <db_name> [round]
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18095"
DB = sys.argv[2] if len(sys.argv) > 2 else "cloudpivot_p1fix2_a"
ROUND = sys.argv[3] if len(sys.argv) > 3 else "1"
ADMIN_PASS = os.environ.get("NIK_ADMIN_PASSWORD", "Admin@123456")
NEW_PASS = ADMIN_PASS + "-p1fix2"

# ---------- 夹具（与其它卡的 UUID 段不重叠） ----------
UNIT = "d2f2f200-0000-4000-8000-000000000001"
WH = "d2f2f200-0000-4000-8000-000000000002"
M_CROSS = "d2f2f200-0000-4000-8000-000000000011"   # A/B 各 30 → 领 60（够）
M_OVERRIDE = "d2f2f200-0000-4000-8000-000000000012"  # A/B 各 50 → 手指定 B 领 30
M_SHORT = "d2f2f200-0000-4000-8000-000000000013"   # A/B 各 20 → 领 60（差 20）
M_EMPTY = "d2f2f200-0000-4000-8000-000000000014"   # 无库存 → 领 5
LOC_A = "d2f2f200-0000-4000-8000-0000000000a1"
LOC_B = "d2f2f200-0000-4000-8000-0000000000a2"
MATS = (M_CROSS, M_OVERRIDE, M_SHORT, M_EMPTY)

PASS, FAIL = [], []


def psql(sql):
    env = dict(os.environ, PGPASSWORD="alun")
    proc = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=120,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip()[:800])
    return [ln.split("\x1f") for ln in proc.stdout.strip().split("\n") if ln]


def scalar(sql, default=None):
    rows = psql(sql)
    return rows[0][0] if rows else default


def http(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=90) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return err.code, {"raw": raw}


def ok(name, cond, detail=None):
    (PASS if cond else FAIL).append(name)
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}{('   ' + str(detail)) if detail else ''}")


def login(username, password):
    status, res = http("POST", "/api/system/auth/login", {"username": username, "password": password})
    if status == 200 and res.get("code") == 0:
        return res["data"]["access_token"], True
    return "", False


def auth_token():
    token, good = login("admin", ADMIN_PASS)
    if good:
        # 首启账号 must_change_password=true：不改密业务接口一律 403
        status, res = http("PUT", "/api/system/auth/password",
                           {"old_password": ADMIN_PASS, "new_password": NEW_PASS}, token)
        print(f"  · admin 首次改密 HTTP {status} {res.get('msg', '')}")
        token, good = login("admin", NEW_PASS)
    else:
        token, good = login("admin", NEW_PASS)
    assert good, "admin 登录失败（初始密码与已改密口令都不通）"
    return token


# ---------- 造库存 ----------
def seed_fixtures():
    """清掉本脚本自己的单据/两本账/流水，然后重新造夹具与库存。"""
    mats = ", ".join(f"'{m}'::uuid" for m in MATS)
    psql(f"delete from biz_voucher where source_type='STOCK_OUT' and source_id in "
         f"(select stock_out_id from biz_stock_out_item where material_id in ({mats}))")
    psql(f"delete from biz_status_log where entity_type='STOCK_OUT' and entity_id in "
         f"(select stock_out_id from biz_stock_out_item where material_id in ({mats}))")
    psql(f"delete from biz_stock_out where id in "
         f"(select stock_out_id from biz_stock_out_item where material_id in ({mats}))")
    psql(f"delete from biz_inventory_txn where material_id in ({mats})")
    psql(f"delete from biz_inventory where material_id in ({mats})")
    psql(f"delete from biz_inventory_location where material_id in ({mats})")

    psql(f"insert into biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) "
         f"values ('{UNIT}'::uuid,'P1F2E-PCS','个',1,null,1.000000,1,1) on conflict do nothing")
    psql(f"insert into biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, "
         f"status, allow_negative) values ('{WH}'::uuid,'P1F2E-WH','P1F2 真机仓',"
         f"'/{WH}/',1,1,1,0) on conflict do nothing")
    for code, name, mat in [("P1F2E-M1", "跨库位物料", M_CROSS), ("P1F2E-M2", "手改库位物料", M_OVERRIDE),
                            ("P1F2E-M3", "部分库存物料", M_SHORT), ("P1F2E-M4", "零库存物料", M_EMPTY)]:
        psql(f"insert into biz_material (id, material_code, material_name, unit_id, status, serial_required) "
             f"values ('{mat}'::uuid,'{code}','{name}','{UNIT}'::uuid,1,false) on conflict do nothing")
    for loc, code, name, sort in [(LOC_A, "P1F2E-L01", "A 库位", 1), (LOC_B, "P1F2E-L02", "B 库位", 2)]:
        psql(f"insert into biz_location (id, warehouse_id, location_code, location_name, is_default, status, "
             f"sort_order) values ('{loc}'::uuid,'{WH}'::uuid,'{code}','{name}',0,1,{sort}) on conflict do nothing")


def seed_stock(material, loc, qty, age_minutes):
    """往 (物料, 库位) 桶里放货：库位账 + 成本账（成本账的 location_id = 库位账行 id）。"""
    psql(f"insert into biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty, "
         f"last_txn_at, created_at, updated_at) values (gen_random_uuid(),'{material}'::uuid,'{WH}'::uuid,"
         f"'{loc}'::uuid,{qty},0,now(),now() - interval '{age_minutes} minutes',now())")
    psql(f"insert into biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, "
         f"amount, last_txn_at, created_at, updated_at) "
         f"select gen_random_uuid(), il.material_id, il.warehouse_id, il.id, il.qty, 0, 10, il.qty * 10, "
         f"       now(), now(), now() from biz_inventory_location il "
         f" where il.material_id='{material}'::uuid and il.warehouse_id='{WH}'::uuid "
         f"   and il.location_id='{loc}'::uuid")


def ledger_row(material, loc):
    return scalar(f"select id from biz_inventory_location where material_id='{material}'::uuid "
                  f"and warehouse_id='{WH}'::uuid and location_id='{loc}'::uuid")


def loc_qty(material, loc):
    return float(scalar(f"select qty from biz_inventory_location where material_id='{material}'::uuid "
                        f"and warehouse_id='{WH}'::uuid and location_id='{loc}'::uuid", "0"))


def create_doc(token, material, qty, location_row=None):
    item = {"material_id": material, "qty": str(qty)}
    if location_row:
        item["location_id"] = location_row
    return http("POST", "/api/business/erp/stock-out",
                {"out_type": "ISSUE", "warehouse_id": WH, "reason": f"P1FIX2 真机 round{ROUND}",
                 "items": [item]}, token)


def post_doc(token, doc_id):
    return http("PATCH", f"/api/business/erp/stock-out/{doc_id}/status", {"status": "POSTED"}, token)


def legacy_message():
    """评审证据里的旧 409 原文（前后对比用）。"""
    path = ("/opt/cloudpivot/scripts/verify/audit_features_20261104/05_chains_run3.txt")
    try:
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                if "库存不足，过账已拒绝" in line:
                    return line.strip()
    except OSError:
        return "(旧证据文件缺失)"
    return "(旧证据里没有这条文案)"


def items_of(detail):
    rows = [(it["line_no"], it.get("location_id"), float(it["qty"])) for it in detail["items"]]
    return sorted(rows)


def main():
    print("=" * 96)
    print(f"P1-FIX-2 真机出库 FIFO 自动分配   接口={BASE}   库={DB}   round={ROUND}")
    print("=" * 96)
    token = auth_token()
    seed_fixtures()
    # 库存：跨库位（30 早 + 30 晚）、手改（50 + 50）、部分（20 + 20）、零库存
    seed_stock(M_CROSS, LOC_A, 30, 120)
    seed_stock(M_CROSS, LOC_B, 30, 60)
    seed_stock(M_OVERRIDE, LOC_A, 50, 120)
    seed_stock(M_OVERRIDE, LOC_B, 50, 60)
    seed_stock(M_SHORT, LOC_A, 20, 120)
    seed_stock(M_SHORT, LOC_B, 20, 60)
    row_a = ledger_row(M_CROSS, LOC_A)
    row_b = ledger_row(M_CROSS, LOC_B)
    row_ovr_b = ledger_row(M_OVERRIDE, LOC_B)
    print(f"  · 夹具：仓={WH} 跨库位料 A/B 各 30（A 建账早 120 分钟） 库位账行 A={row_a} B={row_b}")

    # ---------------- A. 跨库位够量：自动拆行 + 一次过账 ----------------
    print("\n## A. 跨库位物料领用 60（不指定库位）—— 修复前这里 409 要手工拆行")
    status, res = create_doc(token, M_CROSS, 60)
    ok("建单 HTTP 200", status == 200 and res.get("code") == 0, f"HTTP {status} {res.get('msg', '')}")
    detail = res.get("data", {})
    rows = items_of(detail) if detail else []
    print(f"  · 建单返回明细（行号, 库位账行, 数量）：{rows}")
    ok("建单按 FIFO 自动拆成 2 行", len(rows) == 2, f"实际 {len(rows)} 行")
    if len(rows) == 2:
        ok("第 1 行 = 最早建账的 A 库位 30", rows[0][1] == row_a and abs(rows[0][2] - 30) < 1e-9, rows[0])
        ok("第 2 行 = 后建的 B 库位 30", rows[1][1] == row_b and abs(rows[1][2] - 30) < 1e-9, rows[1])
    ok("单头合计仍是 60（拆行不改变合计）", float(detail.get("total_qty", 0)) == 60, detail.get("total_qty"))
    ok("库存够量时没有 stock_warnings", detail.get("stock_warnings") == [], detail.get("stock_warnings"))

    doc_id = detail.get("id")
    status, res = post_doc(token, doc_id)
    ok("过账 HTTP 200（不再要求手工拆行）", status == 200, f"HTTP {status} {res.get('msg', '')}")
    ok("单据落 POSTED", res.get("data", {}).get("status") == "POSTED", res.get("data", {}).get("status"))
    ok("A 库位清零", loc_qty(M_CROSS, LOC_A) == 0, loc_qty(M_CROSS, LOC_A))
    ok("B 库位清零", loc_qty(M_CROSS, LOC_B) == 0, loc_qty(M_CROSS, LOC_B))

    # ---------------- B. 手改覆盖 ----------------
    print("\n## B. 手改库位（显式指定后建的 B）—— 默认自动、允许覆盖")
    status, res = create_doc(token, M_OVERRIDE, 30, row_ovr_b)
    ok("建单 HTTP 200", status == 200, f"HTTP {status} {res.get('msg', '')}")
    rows = items_of(res.get("data", {})) if res.get("data") else []
    print(f"  · 建单返回明细：{rows}")
    ok("只 1 行且库位 = 手填的 B", len(rows) == 1 and rows[0][1] == row_ovr_b, rows)
    doc_id = res.get("data", {}).get("id")
    status, res = post_doc(token, doc_id)
    ok("过账 HTTP 200", status == 200, f"HTTP {status} {res.get('msg', '')}")
    ok("B 剩 20", loc_qty(M_OVERRIDE, LOC_B) == 20, loc_qty(M_OVERRIDE, LOC_B))
    ok("A 一分未动（50）", loc_qty(M_OVERRIDE, LOC_A) == 50, loc_qty(M_OVERRIDE, LOC_A))

    # ---------------- C. 部分库存 → 提示清晰 ----------------
    print("\n## C. 库存不足（A 20 + B 20，领 60）—— 差 20")
    status, res = create_doc(token, M_SHORT, 60)
    ok("建单 HTTP 200（草稿可存）", status == 200, f"HTTP {status} {res.get('msg', '')}")
    warnings = (res.get("data") or {}).get("stock_warnings") or []
    print(f"  · 建单 stock_warnings：{warnings}")
    ok("建单带回 1 条库存提示", len(warnings) == 1, len(warnings))
    if warnings:
        w = warnings[0]
        ok("提示点名缺口 20", "差 20.0000" in w, w[:160])
        ok("提示点名现有库位分布", "P1F2E-L01" in w and "P1F2E-L02" in w, w[:160])
    doc_id = (res.get("data") or {}).get("id")
    status, res = post_doc(token, doc_id)
    msg = res.get("msg", "")
    print(f"  · 过账 409 原文（新的）：\n    {msg}")
    ok("过账被拒 409", status == 409, f"HTTP {status}")
    ok("文案点名缺口 20", "差 20.0000" in msg, msg[:200])
    ok("文案点名各库位分布", "P1F2E-L01: 20.0000" in msg and "P1F2E-L02: 20.0000" in msg, msg[:200])
    ok("文案给出「重新保存草稿会自动拆行」的动作", "自动" in msg and "不需要手工拆行" in msg, msg[:200])
    ok("旧口径「请把这一行拆成多行」已消失", "请把这一行拆成多行" not in msg, msg[:200])
    ok("过账失败整笔回滚（单据仍 DRAFT）",
       scalar(f"select status from biz_stock_out where id='{doc_id}'::uuid") == "DRAFT",
       scalar(f"select status from biz_stock_out where id='{doc_id}'::uuid"))
    ok("A 仍是 20（账没动）", loc_qty(M_SHORT, LOC_A) == 20, loc_qty(M_SHORT, LOC_A))

    # ---------------- D. 完全没货 ----------------
    print("\n## D. 零库存物料领用 5")
    status, res = create_doc(token, M_EMPTY, 5)
    warnings = (res.get("data") or {}).get("stock_warnings") or []
    print(f"  · 建单 stock_warnings：{warnings}")
    ok("建单 HTTP 200", status == 200, f"HTTP {status}")
    ok("提示说清「无库存」", bool(warnings) and "无库存" in warnings[0], warnings[:1])
    status, res = post_doc(token, (res.get("data") or {}).get("id"))
    msg = res.get("msg", "")
    print(f"  · 过账 409 原文：\n    {msg}")
    ok("过账被拒 409", status == 409, f"HTTP {status}")
    ok("文案点名「可用合计 0 / 差 5」", "可用合计 0" in msg and "差 5" in msg, msg[:200])

    # ---------------- E. 账实一致 ----------------
    print("\n## E. 账实一致（B1：Σ 库位账 = 成本账）")
    mats = ", ".join(f"'{m}'::uuid" for m in MATS)
    rows = psql(f"select m.material_id, coalesce(l.total,0), coalesce(i.total,0) from "
                f"(select unnest(array[{mats}]) as material_id) m "
                f"left join (select material_id, sum(qty) total from biz_inventory_location "
                f"           where material_id in ({mats}) group by 1) l using (material_id) "
                f"left join (select material_id, sum(qty) total from biz_inventory "
                f"           where material_id in ({mats}) group by 1) i using (material_id)")
    for material, loc_total, cost_total in rows:
        ok(f"B1 {material[:8]}…：库位账 {loc_total} = 成本账 {cost_total}",
           abs(float(loc_total) - float(cost_total)) < 1e-9, f"{loc_total} vs {cost_total}")

    print("\n## F. 文案前后对比（旧证据 vs 本轮）")
    print(f"  旧的（05_chains_run3.txt ③ 段）：\n    {legacy_message()}")
    print("  新的：见上面 C 段 409 原文（点名缺口 + 各库位分布 + 可执行动作）")

    print("\n" + "=" * 96)
    print(f"RESULT round{ROUND}: PASS={len(PASS)} FAIL={len(FAIL)}")
    if FAIL:
        for name in FAIL:
            print(f"  FAILED: {name}")
    print("=" * 96)
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
