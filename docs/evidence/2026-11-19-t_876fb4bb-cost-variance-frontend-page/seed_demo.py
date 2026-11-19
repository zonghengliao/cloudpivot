#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-19 浏览器验收夹具：在当前月（2026-11）造「可比 / 缺实际 / 缺标准」三行。

用三个已有物料（不新建主数据）：

| 物料 | 造什么 | 页面上应看到 |
|---|---|---|
| `E2E02-M1` | 已发布标准卡（19.5/件）+ 成本计算单（完工 800，单位 20.4） | **可比**：差 720（不利）、率 4.62%、三要素 400/320/0 |
| `E2E02-M2` | 只有已发布标准卡，没有计算单 | **缺实际**：hint 让他去做成本核算 |
| `E2E02-M3` | 只有成本计算单（完工 100），没有标准卡 | **缺标准**：hint 让他去标准成本页建卡发布 |

跑法（实例在 8023，库已迁到 V0466，`erp.cost.enabled = true`）：

    python3 docs/evidence/2026-11-19-t_876fb4bb-cost-variance-frontend-page/seed_demo.py

跑完**不清场**（页面截图要用），验完执行 `seed_demo.py --clean` 或复用
`smoke_variance.py` 的清场函数删干净（`--clean` 只删这三个夹具，不碰别的数据）。
"""

from __future__ import annotations

import os
import subprocess
import sys

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "172.18.10.145"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "fbb1263691ff75c31ec99eaf258138b4"),
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m519_smoke"),
)

PERIOD = "2026-11"
PERIOD_END = "2026-11-30"
BATCH_NO = "CS-202611-9901"
OBJ_PREFIX = "CB-M519-"
STD_REMARK = "M519 fixture"
CODES = ("E2E02-M1", "E2E02-M2", "E2E02-M3")


def q(sql: str) -> list[list[str]]:
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"], "-d", DB["name"],
         "-v", "ON_ERROR_STOP=1", "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(f"SQL 失败：{r.stderr.strip()[:600]}\n---\n{sql[:400]}")
    return [ln.split("\x1f") for ln in r.stdout.strip().split("\n") if ln]


def one(sql: str, default: str = "") -> str:
    rows = q(sql)
    return rows[0][0] if rows and rows[0] and rows[0][0] is not None else default


def cleanup() -> None:
    q("delete from biz_cost_period_item where period_id in "
      f"(select id from biz_cost_period where batch_no = '{BATCH_NO}')")
    q(f"delete from biz_cost_period where batch_no = '{BATCH_NO}'")
    q(f"delete from biz_cost_standard where remark = '{STD_REMARK}'")
    q(f"delete from biz_cost_object where object_no like '{OBJ_PREFIX}%'")


def seed() -> None:
    if one("select config_value from sys_config where config_key = 'erp.cost.enabled'") != "true":
        raise SystemExit("erp.cost.enabled 不是 true —— 先开开关并重启实例")
    cleanup()

    mats = {}
    for code in CODES:
        mid = one(f"select id from biz_material where deleted_at is null and material_code = '{code}'")
        if not mid:
            raise SystemExit(f"模板库缺物料 {code}")
        mats[code] = mid

    # ① 标准卡（M1 可比 + M2 缺实际）：已发布 + 启用 + 覆盖期末
    for code in ("E2E02-M1", "E2E02-M2"):
        q(f"""
        insert into biz_cost_standard
            (id, material_id, version_no, effective_from, effective_to,
             std_material_cost, std_labor_cost, std_overhead_cost, std_total_cost,
             std_labor_hours, std_labor_rate, status, remark,
             created_at, updated_at, publish_state, published_at)
        values
            (gen_random_uuid(), '{mats[code]}', 1, '{PERIOD}-01', '{PERIOD_END}',
             12, 5, 2.5, 19.5, 0.1, 50, 1, '{STD_REMARK}',
             now(), now(), 'PUBLISHED', now())
        """)

    # ② 成本核算批次（DRAFT = 活跃批次）
    period_id = one(f"""
        insert into biz_cost_period
            (id, org_id, period_code, batch_no, status, labor_source, object_count,
             total_material, total_labor, total_overhead, total_cost,
             total_finished, total_wip, created_at, updated_at)
        values
            (gen_random_uuid(), '00000000-0000-0000-0000-000000000000', '{PERIOD}',
             '{BATCH_NO}', 'DRAFT', 'MANUAL', 2,
             10100, 4420, 2100, 16620, 16620, 0, now(), now())
        returning id
    """)

    # ③ 成本对象（M1 = 可比，M3 = 缺标准）
    tpl = q("select target_warehouse_id, workshop_dept_id, workshop_dept_name, object_type, "
            "material_input_mode from biz_cost_object where deleted_at is null limit 1")
    if not tpl:
        raise SystemExit("模板库没有可参考的成本对象")
    wh, dept, dept_name, obj_type, input_mode = tpl[0]
    objs = {}
    for idx, code in enumerate(("E2E02-M1", "E2E02-M3"), start=1):
        objs[code] = one(f"""
            insert into biz_cost_object
                (id, object_no, object_type, product_id, target_warehouse_id, plan_qty,
                 completed_qty, wip_qty, completion_pct, material_input_mode,
                 workshop_dept_id, workshop_dept_name, start_date, status,
                 created_at, updated_at)
            values
                (gen_random_uuid(), '{OBJ_PREFIX}{idx:06d}', '{obj_type}', '{mats[code]}',
                 '{wh}', 1000, 0, 0, 0, '{input_mode}', '{dept}', '{dept_name}',
                 '{PERIOD}-01', 'OPEN', now(), now())
            returning id
        """)

    # ④ 计算单行：M1 完工 800（单位 12.5 / 5.4 / 2.5）；M3 完工 100（单位 1 / 1 / 1）
    q(f"""
        insert into biz_cost_period_item
            (id, period_id, cost_object_id, product_id,
             wip_beg_qty, wip_beg_cost, completed_qty, wip_end_qty, completion_pct,
             equiv_material, equiv_conversion, mat_cur, labor_cur, oh_cur,
             unit_material, unit_labor, unit_overhead, finished_cost, wip_end_cost,
             unit_finished_cost, status, created_at, updated_at)
        values
            (gen_random_uuid(), '{period_id}', '{objs['E2E02-M1']}', '{mats['E2E02-M1']}',
             0, 0, 800, 0, 100, 800, 800, 10000, 4320, 2000,
             12.5, 5.4, 2.5, 16320, 0, 20.4, 'CALC', now(), now()),
            (gen_random_uuid(), '{period_id}', '{objs['E2E02-M3']}', '{mats['E2E02-M3']}',
             0, 0, 100, 0, 100, 100, 100, 100, 100, 100,
             1, 1, 1, 300, 0, 3, 'CALC', now(), now())
    """)
    print(f"夹具已就位：{PERIOD} · 批次 {BATCH_NO}")
    print("  可比  E2E02-M1：标准 19.5×800=15600 / 实际 16320 / 差异 720（不利）")
    print("  缺实际 E2E02-M2：只有已发布标准卡")
    print("  缺标准 E2E02-M3：只有计算单，实际 300")


if __name__ == "__main__":
    if "--clean" in sys.argv:
        cleanup()
        print("夹具已清场（只删本卡的批次 / 标准卡 / 成本对象）")
    else:
        seed()
