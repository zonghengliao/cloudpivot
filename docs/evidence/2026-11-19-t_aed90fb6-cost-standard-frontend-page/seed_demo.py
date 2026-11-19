#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-18 浏览器验收夹具：在本月（2026-11）造三张已发布标准成本卡，供页面截图用。

- DEMO-M001：v1（11-01~11-15）+ v2（11-16~11-30）→ 期间视图出现「期间内换版」标记
- DEMO-M002：一整期（11-01~11-30）→ 普通行

审批通过用 SQL 模拟（真人审批链属 M5-10 用例），跑完不清理 —— 页面验完由
`cleanup_seed.py` 硬删。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

DB = dict(host="172.18.10.145", port="5432", user="cloudpivot",
          password="fbb1263691ff75c31ec99eaf258138b4",
          name=os.environ.get("NIK_DATABASE_NAME", "cp_m518_smoke"))
BASE = os.environ.get("CS_BASE", "http://127.0.0.1:8023") + "/api"


def q(sql: str) -> list[list[str]]:
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(["psql", "-h", DB["host"], "-p", DB["port"], "-U", DB["user"],
                        "-d", DB["name"], "-tAF", "\x1f", "-v", "ON_ERROR_STOP=1", "-c", sql],
                       capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:400])
    return [ln.split("\x1f") for ln in r.stdout.strip().split("\n") if ln]


def call(token: str, method: str, path: str, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method,
                                 headers={"Content-Type": "application/json",
                                          "Authorization": f"Bearer {token}"})
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        return err.code, json.loads(err.read().decode() or "{}")


def main() -> int:
    status, payload = call("", "POST", "/system/auth/login",
                           {"username": "e2e01_appr", "password": "E2e01@Appr123"})
    if status != 200:
        raise SystemExit(f"登录失败：{status} {payload}")
    token = (payload.get("data") or {})["access_token"]

    mats = q("select id, material_code from biz_material where deleted_at is null and status = 1 "
             "and material_type = 'FINISHED' and material_code in ('DEMO-M001','DEMO-M002') "
             "order by material_code")
    comp = q("select id from biz_material where deleted_at is null and status = 1 "
             "and material_type = 'RAW' order by material_code limit 1")[0][0]
    (a_id, a_code), (b_id, b_code) = mats

    for mid in (a_id, b_id):
        q(f"delete from biz_cost_standard_item where standard_id in "
          f"(select id from biz_cost_standard where material_id = '{mid}')")
        q(f"delete from biz_cost_standard where material_id = '{mid}'")

    plan = [
        (a_id, "2026-11-01", "2026-11-15", "30.000000", "M518 demo A v1"),
        (a_id, "2026-11-16", "2026-11-30", "35.000000", "M518 demo A v2"),
        (b_id, "2026-11-01", "2026-11-30", "28.000000", "M518 demo B v1"),
    ]
    for mid, frm, to, rate, remark in plan:
        status, payload = call(token, "POST", "/business/erp/cost-standard", {
            "material_id": mid, "effective_from": frm, "effective_to": to,
            "std_labor_hours": "2.0000", "std_labor_rate": rate,
            "std_overhead_cost": "12.000000",
            "items": [{"component_material_id": comp, "std_qty": "3.000000", "std_price": "5.000000"}],
            "remark": remark,
        })
        if status != 200:
            raise SystemExit(f"建卡失败（{remark}）：{status} {payload}")

    q("update biz_cost_standard set publish_state = 'PUBLISHED', status = 1, published_at = now(), "
      "publish_instance_id = null where remark like 'M518 demo%'")
    rows = q("select material_id, version_no, effective_from, effective_to, publish_state, "
             "std_total_cost from biz_cost_standard where remark like 'M518 demo%' "
             "and deleted_at is null order by material_id, version_no")
    print(f"夹具就绪：{a_code} / {b_code}（期间 2026-11）")
    for row in rows:
        print("  ", " | ".join(row))
    return 0


if __name__ == "__main__":
    sys.exit(main())
