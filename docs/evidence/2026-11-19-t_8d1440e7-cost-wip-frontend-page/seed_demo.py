#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-25 截图用夹具（留着数据不清理；用完由 cleanup_demo.py 或本脚本 --clean 清场）。

    python3 docs/evidence/2026-11-19-t_8d1440e7-cost-wip-frontend-page/seed_demo.py
    python3 docs/evidence/2026-11-19-t_8d1440e7-cost-wip-frontend-page/seed_demo.py --clean
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "172.18.10.145"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "fbb1263691ff75c31ec99eaf258138b4"),
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m525_dev"),
)
BASE = os.environ.get("WIP_BASE", "http://127.0.0.1:18311") + "/api"
TAG = os.environ.get("WIP_TAG", "M525DEMO")
P2 = os.environ.get("WIP_PERIOD", "2026-11")
ADMIN = ("admin", os.environ.get("WIP_ADMIN_PASSWORD", "Admin@123456"))


def q(sql: str) -> list[list[str]]:
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"], "-d", DB["name"],
         "-v", "ON_ERROR_STOP=1", "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(f"SQL 失败：{r.stderr.strip()[:600]}")
    return [ln.split("\x1f") for ln in r.stdout.strip().split("\n") if ln]


def one(sql: str, default: str = "") -> str:
    rows = q(sql)
    return rows[0][0] if rows and rows[0] else default


def clean() -> None:
    like = f"%{TAG}%"
    q(f"delete from biz_cost_object_operation where cost_object_id in "
      f"(select id from biz_cost_object where object_no like '{like}')")
    q(f"delete from biz_cost_collect where cost_object_id in "
      f"(select id from biz_cost_object where object_no like '{like}')")
    q(f"delete from biz_cost_period_item where cost_object_id in "
      f"(select id from biz_cost_object where object_no like '{like}')")
    q(f"delete from biz_cost_period where batch_no like 'CS-{TAG}%'")
    q(f"delete from biz_cost_object where object_no like '{like}'")
    q(f"delete from biz_material where material_code like '{TAG}%'")


def call(method: str, path: str, token: str = "", body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method,
                                 headers={"Content-Type": "application/json",
                                          **({"Authorization": f"Bearer {token}"} if token else {})})
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as exc:
        return exc.code, json.loads(exc.read().decode() or "{}")


def main() -> int:
    q("update sys_user set must_change_password = false where username = 'admin'")
    q("update sys_config set config_value = 'true' where config_key = 'erp.cost.enabled'")
    clean()
    if "--clean" in sys.argv:
        print(f"已清场 {TAG}")
        return 0

    dept = one("select id from sys_dept where deleted_at is null order by created_at limit 1")
    dept_name = one(f"select dept_name from sys_dept where id = '{dept}'::uuid")
    unit = one("select id from biz_unit where deleted_at is null order by unit_code limit 1")
    wh = one("select id from biz_warehouse where deleted_at is null order by created_at limit 1")
    q(f"""insert into biz_material (id, material_code, material_name, spec, unit_id,
                                    material_type, status, remark)
          values (gen_random_uuid(), '{TAG}-FG', '半成品 A（截图用）', 'M5-25', '{unit}', 'FINISHED', 1, '{TAG}')""")
    product = one(f"select id from biz_material where material_code = '{TAG}-FG'")

    def make(name: str, completed: str, wip: str, pct: str, status: str = "OPEN") -> tuple[str, str]:
        oid = one("select gen_random_uuid()")
        no = f"CB-{TAG}-{name}"
        q(f"""insert into biz_cost_object (id, object_no, object_type, product_id, lot_no,
                                           target_warehouse_id, plan_qty, completed_qty, wip_qty,
                                           completion_pct, material_input_mode,
                                           workshop_dept_id, workshop_dept_name, start_date, status, closed_at)
              values ('{oid}'::uuid, '{no}', 'BATCH', '{product}'::uuid, 'LOT-{TAG}-{name}',
                      '{wh}'::uuid, 800, {completed}, {wip}, {pct}, 'ONE_TIME',
                      '{dept}'::uuid, '{dept_name}', '{P2}-01', '{status}',
                      {'now()' if status == 'CLOSED' else 'null'})""")
        return oid, no

    a, a_no = make("A", "800", "200", "50")
    b, b_no = make("B", "0", "300", "20")
    make("C", "500", "0", "0", status="CLOSED")

    batch = one("select gen_random_uuid()")
    q(f"""insert into biz_cost_period (id, period_code, batch_no, status)
          values ('{batch}'::uuid, '{P2}', 'CS-{TAG}-P2', 'DRAFT')""")
    for element, amount, src, mat, qty in [
        ("MATERIAL", 1000, "STOCK_OUT", f"'{product}'::uuid", "100"),
        ("LABOR", 1500, "LABOR_TICKET", "null", "null"),
        ("OVERHEAD", 700, "OVERHEAD_TICKET", "null", "null"),
    ]:
        q(f"""insert into biz_cost_collect (id, cost_object_id, period_code, element, amount,
                                            source_type, occur_date, material_id, qty)
              values (gen_random_uuid(), '{a}'::uuid, '{P2}', '{element}', {amount},
                      '{src}', '{P2}-10', {mat}, {qty})""")

    # 登录 → 给 A 批录三道工序（100/1 + 50/2 + 0/1 ⇒ 50%）
    status, payload = call("POST", "/system/auth/login", body={"username": ADMIN[0], "password": ADMIN[1]})
    token = (payload.get("data") or {}).get("access_token", "")
    if status != 200 or not token:
        raise SystemExit(f"登录失败：{status} {payload}")
    status, payload = call("PUT", f"/business/erp/cost-object/{a}/operations", token, {"rows": [
        {"seq": 1, "operation_name": "下料", "weight": "1", "completion_pct": "100"},
        {"seq": 2, "operation_name": "焊接", "weight": "2", "completion_pct": "50", "remark": "关键工序"},
        {"seq": 3, "operation_name": "装配", "weight": "1", "completion_pct": "0"},
    ]})
    print(f"工序录入：status={status} pct={(payload.get('data') or {}).get('rollup', {})}")
    print(json.dumps({"a": a, "a_no": a_no, "b_no": b_no, "period": P2}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
