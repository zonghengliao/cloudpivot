#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-20 在制品「约当产量表」· HTTP 冒烟（真接口 + 真库，逐条 PASS/FAIL）。

跑法（实例已起在旁边，库已就绪）：

    WIP_BASE=http://127.0.0.1:18250 \
      python3 docs/evidence/2026-11-19-t_d9c1c697-cost-wip-equiv/smoke_wip_equiv.py

覆盖：
  ① 路由真的挂上了（`GET /api/business/erp/cost-wip-equiv` 200，不是 404 / 不是 SPA HTML）
  ② 期间必填 / 期间形状 / 状态拼错 三种 400 的报文
  ③ 完工率 × 数量：材料 1000 / 加工 900（一行一行对）
  ④ 跨月：前期已结算完工 300 被扣掉，本期完工 700
  ⑤ 与真计算单对拍一致 ⇒ calc.agrees = true
  ⑥ 在制 > 0 但完工率 0 ⇒ blocked + 加工约当 null（不是 0）+ 问题码
  ⑦ 合计是整张表（page_size=1 时 row_count 仍是 3）+ 约当合计跳过 blocked 行
  ⑧ 四档筛选（wip / finished / blocked / drift）+ 关键字
  ⑨ 算完又改完工率 ⇒ drift 命中「完工率」
  ⑩ 没建批次的期间 ⇒ 试算视图（cost_ready=false、batch=null、页脚说明）
  ⑪ page_size 上限 200
  ⑫ 清场零残留
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "172.18.10.145"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "fbb1263691ff75c31ec99eaf258138b4"),
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m520_dev"),
)
BASE = os.environ.get("WIP_BASE", "http://127.0.0.1:18250") + "/api"
ADMIN = ("admin", os.environ.get("WIP_ADMIN_PASSWORD", "Admin@123456"))
TAG = f"M520SM{int(time.time()) % 100000}"
YEAR = 2030 + (int(time.time()) % 5)
P1, P2, P3 = f"{YEAR}-01", f"{YEAR}-02", f"{YEAR}-03"

PASS, FAIL = [], []


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
    return rows[0][0] if rows and rows[0] else default


def ok(name: str, cond: bool, detail: str = "") -> None:
    (PASS if cond else FAIL).append(name)
    print(f"[{'PASS' if cond else 'FAIL'}] {name}{('  · ' + detail) if detail else ''}")


class Client:
    def __init__(self) -> None:
        self.token = ""

    def call(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(BASE + path, data=data, method=method,
                                     headers={"Content-Type": "application/json",
                                              **({"Authorization": f"Bearer {self.token}"} if self.token else {})})
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return resp.status, json.loads(resp.read().decode() or "{}")
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode()
            try:
                return exc.code, json.loads(raw or "{}")
            except json.JSONDecodeError:
                return exc.code, {"raw": raw[:300]}

    def login(self, user: str, pwd: str) -> None:
        status, payload = self.call("POST", "/system/auth/login", {"username": user, "password": pwd})
        if status != 200:
            raise SystemExit(f"登录失败：{status} {payload}")
        self.token = (payload.get("data") or {}).get("access_token", "")
        if not self.token:
            raise SystemExit(f"登录回包没有 token：{payload}")


def data_of(payload) -> dict:
    return (payload or {}).get("data") or {}


def err_of(payload) -> str:
    p = payload or {}
    return f"code={p.get('code')} msg={(p.get('msg') or p.get('message') or '')[:140]}"


def row_of(sheet: dict, object_no: str) -> dict:
    for row in sheet.get("list") or []:
        if row.get("object_no") == object_no:
            return row
    raise AssertionError(f"表里找不到 {object_no}（共 {len(sheet.get('list') or [])} 行）")


def fixture_cleanup() -> None:
    like = f"%{TAG}%"
    q(f"delete from biz_cost_period_item where cost_object_id in "
      f"(select id from biz_cost_object where object_no like '{like}')")
    q(f"delete from biz_cost_collect where cost_object_id in "
      f"(select id from biz_cost_object where object_no like '{like}')")
    q(f"delete from biz_cost_period where batch_no like 'CS-{TAG}%'")
    q(f"delete from biz_cost_object where object_no like '{like}'")
    q(f"delete from biz_material where material_code like '{TAG}%'")


def setup() -> dict:
    q("update sys_user set must_change_password = false where username = 'admin'")
    q("update sys_config set config_value = 'true' where config_key = 'erp.cost.enabled'")
    fixture_cleanup()

    dept = one("select id from sys_dept where deleted_at is null order by created_at limit 1")
    dept_name = one(f"select dept_name from sys_dept where id = '{dept}'::uuid")
    unit = one("select id from biz_unit where deleted_at is null order by unit_code limit 1")
    wh = one("select id from biz_warehouse where deleted_at is null order by created_at limit 1")
    q(f"""insert into biz_material (id, material_code, material_name, spec, unit_id,
                                    material_type, status, remark)
          values (gen_random_uuid(), '{TAG}-FG', '{TAG} 产出品', 'M5-20', '{unit}', 'FINISHED', 1, '{TAG}')""")
    product = one(f"select id from biz_material where material_code = '{TAG}-FG'")

    def object_no(name: str) -> str:
        return f"CB-{TAG}-{name}"

    def make_object(name: str, completed: str, wip: str, pct: str) -> str:
        oid = one("select gen_random_uuid()")
        q(f"""insert into biz_cost_object (id, object_no, object_type, product_id, lot_no,
                                           target_warehouse_id, plan_qty, completed_qty, wip_qty,
                                           completion_pct, material_input_mode,
                                           workshop_dept_id, workshop_dept_name, start_date, status)
              values ('{oid}'::uuid, '{object_no(name)}', 'BATCH', '{product}'::uuid, 'LOT-{TAG}-{name}',
                      '{wh}'::uuid, 800, {completed}, {wip}, {pct}, 'ONE_TIME',
                      '{dept}'::uuid, '{dept_name}', '{P1}-01', 'OPEN')""")
        return oid

    # A：本期有完工也在制（可与计算单对拍）；B：跨月全完工；C：在制但完工率 0
    a = make_object("A", "800", "200", "50")
    b = make_object("B", "1000", "0", "0")
    c = make_object("C", "100", "200", "0")

    # 前期（P1）已结算批次 + B 的已结算完工 300 / 期末在制 100
    prior_batch = one("select gen_random_uuid()")
    q(f"""insert into biz_cost_period (id, period_code, batch_no, status, approval_instance_id,
                                       settled_at, finish_voucher_id)
          values ('{prior_batch}'::uuid, '{P1}', 'CS-{TAG}-P1', 'SETTLED', gen_random_uuid(),
                  now(), gen_random_uuid())""")
    q(f"""insert into biz_cost_period_item (id, period_id, cost_object_id, product_id,
                                             completed_qty, wip_end_qty, completion_pct,
                                             equiv_material, equiv_conversion, status, stock_in_id)
          values (gen_random_uuid(), '{prior_batch}'::uuid, '{b}'::uuid, '{product}'::uuid,
                  300, 100, 50, 350, 350, 'SETTLED', gen_random_uuid())""")

    # 本期（P2）批次 + A 的计算单（800 / 200 / 50% ⇒ 1000 / 900）
    batch = one("select gen_random_uuid()")
    q(f"""insert into biz_cost_period (id, period_code, batch_no, status)
          values ('{batch}'::uuid, '{P2}', 'CS-{TAG}-P2', 'DRAFT')""")
    q(f"""insert into biz_cost_period_item (id, period_id, cost_object_id, product_id,
                                             completed_qty, wip_end_qty, completion_pct,
                                             equiv_material, equiv_conversion, status)
          values (gen_random_uuid(), '{batch}'::uuid, '{a}'::uuid, '{product}'::uuid,
                  800, 200, 50, 1000, 900, 'CALC')""")

    return {
        "A": object_no("A"), "B": object_no("B"), "C": object_no("C"),
        "a_id": a, "dept": dept,
    }


def main() -> int:
    cli = Client()
    cli.login(*ADMIN)
    fx = setup()
    ok("夹具就绪（三个成本对象 + 前期已结算批次 + 本期批次）", all(fx[k] for k in ("A", "B", "C", "a_id")))
    q("select 1")
    status, payload = cli.call("POST", "/support/config/reload")
    ok("参数重载 200（模块开关立即生效）", status == 200, err_of(payload))

    # ---------- ① 路由与参数护栏 ----------
    status, payload = cli.call("GET", "/business/erp/cost-wip-equiv?period_code=" + P2)
    ok("① 路由挂上了（200 + data）", status == 200 and isinstance(data_of(payload), dict), err_of(payload))

    status, payload = cli.call("GET", "/business/erp/cost-wip-equiv")
    ok("② 缺期间 ⇒ 400 且说清 period_code", status == 400 and "period_code" in err_of(payload), err_of(payload))

    status, payload = cli.call("GET", "/business/erp/cost-wip-equiv?period_code=2026-13")
    ok("② 非法期间 2026-13 ⇒ 400", status == 400, err_of(payload))

    status, payload = cli.call("GET", "/business/erp/cost-wip-equiv?period_code=" + P2 + "&state=wipp")
    ok("② 状态拼错 ⇒ 400", status == 400, err_of(payload))

    # ---------- ③④⑤⑥ 逐行 ----------
    kw = TAG
    status, sheet = cli.call("GET", f"/business/erp/cost-wip-equiv?period_code={P2}&keyword={kw}")
    ok("③ 期间 + 关键字 200", status == 200, err_of(sheet))
    sheet = data_of(sheet)
    ok("③ 命中本夹具 3 行", sheet.get("total") == 3, f"total={sheet.get('total')}")
    ok("③ cost_ready=true（P2 有批次）", sheet.get("cost_ready") is True)
    ok("③ 批次号回显", (sheet.get("batch") or {}).get("batch_no", "").startswith("CS-"), str(sheet.get("batch")))

    a = row_of(sheet, fx["A"])
    ok("③ A 本期完工 800.0000", a.get("completed_qty") == "800.0000", str(a.get("completed_qty")))
    ok("③ A 在制折材料约当 200.0000", a.get("wip_equiv_material") == "200.0000", str(a.get("wip_equiv_material")))
    ok("③ A 材料约当 1000.0000（800 + 200×100%）", a.get("equiv_material") == "1000.0000", str(a.get("equiv_material")))
    ok("③ A 在制折加工约当 100.0000（200×50%）", a.get("wip_equiv_conversion") == "100.0000", str(a.get("wip_equiv_conversion")))
    ok("③ A 加工约当 900.0000（800 + 100）", a.get("equiv_conversion") == "900.0000", str(a.get("equiv_conversion")))
    ok("⑤ A 与计算单一致（calc.agrees）", (a.get("calc") or {}).get("agrees") is True, str(a.get("calc")))
    ok("⑤ A 不需要重算", a.get("recalc_required") is False)
    ok("③ A 状态 wip", a.get("state") == "wip", str(a.get("state")))

    b = row_of(sheet, fx["B"])
    ok("④ B 前期已结算完工 300.0000", b.get("prior_completed_qty") == "300.0000", str(b.get("prior_completed_qty")))
    ok("④ B 本期完工 700.0000（累计 1000 − 前期 300）", b.get("completed_qty") == "700.0000", str(b.get("completed_qty")))
    ok("④ B 材料约当 700.0000（在制 0）", b.get("equiv_material") == "700.0000", str(b.get("equiv_material")))
    ok("④ B 状态 finished（全完工）", b.get("state") == "finished", str(b.get("state")))

    c = row_of(sheet, fx["C"])
    ok("⑥ C 状态 blocked", c.get("state") == "blocked", str(c.get("state")))
    ok("⑥ C 加工约当 null（不是 0）", c.get("equiv_conversion") is None, str(c.get("equiv_conversion")))
    ok("⑥ C 问题码 WIP_PCT_MISSING",
       any(i.get("code") == "WIP_PCT_MISSING" for i in c.get("issues") or []), str(c.get("issues")))
    ok("⑥ C 带「怎么修」", bool((c.get("issues") or [{}])[0].get("fix")))

    # ---------- ⑦ 合计 ----------
    totals = sheet.get("totals") or {}
    ok("⑦ 合计 row_count=3", totals.get("row_count") == 3, str(totals.get("row_count")))
    ok("⑦ 合计 wip/finished/blocked = 1/1/1",
       (totals.get("wip_row_count"), totals.get("finished_row_count"), totals.get("blocked_row_count")) == (1, 1, 1),
       str({k: totals.get(k) for k in ("wip_row_count", "finished_row_count", "blocked_row_count")}))
    ok("⑦ 完工合计 1600.0000（800+700+100，blocked 行的数量也是事实）", totals.get("completed_qty") == "1600.0000", str(totals.get("completed_qty")))
    ok("⑦ 在制合计 400.0000（200+0+200，blocked 也计数量）", totals.get("wip_qty") == "400.0000", str(totals.get("wip_qty")))
    ok("⑦ 材料约当合计 1700.0000（blocked 行整行不进）", totals.get("equiv_material") == "1700.0000", str(totals.get("equiv_material")))
    ok("⑦ 加工约当合计 1600.0000", totals.get("equiv_conversion") == "1600.0000", str(totals.get("equiv_conversion")))
    ok("⑦ 合计口径说人话", "算不出来" in (totals.get("note") or ""), (totals.get("note") or "")[:80])

    # 分页：合计不受分页影响
    status, page1 = cli.call("GET", f"/business/erp/cost-wip-equiv?period_code={P2}&keyword={kw}&page_size=1")
    page1 = data_of(page1)
    ok("⑦ page_size=1 只回 1 行", len(page1.get("list") or []) == 1, str(len(page1.get("list") or [])))
    ok("⑦ page_size=1 时合计仍是整表 3 行", (page1.get("totals") or {}).get("row_count") == 3)
    status, cap = cli.call("GET", f"/business/erp/cost-wip-equiv?period_code={P2}&keyword={kw}&page_size=9999")
    ok("⑪ page_size 上限 200", data_of(cap).get("page_size") == 200, str(data_of(cap).get("page_size")))

    # ---------- ⑧ 筛选 ----------
    def count(state: str) -> int:
        _, body = cli.call("GET", f"/business/erp/cost-wip-equiv?period_code={P2}&keyword={kw}&state={state}")
        return data_of(body).get("total", -1)

    ok("⑧ state=wip 命中 1", count("wip") == 1, str(count("wip")))
    ok("⑧ state=finished 命中 1", count("finished") == 1, str(count("finished")))
    ok("⑧ state=blocked 命中 1", count("blocked") == 1, str(count("blocked")))
    ok("⑧ state=drift 命中 0（还没改数）", count("drift") == 0, str(count("drift")))

    status, single = cli.call("GET", f"/business/erp/cost-wip-equiv?period_code={P2}&keyword={TAG}-A")
    ok("⑧ 关键字收窄到 1 行", data_of(single).get("total") == 1, str(data_of(single).get("total")))

    # ---------- ⑨ 改完工率 ⇒ 漂移 ----------
    q(f"update biz_cost_object set completion_pct = 60 where id = '{fx['a_id']}'::uuid")
    _, body = cli.call("GET", f"/business/erp/cost-wip-equiv?period_code={P2}&keyword={TAG}-A")
    a2 = row_of(data_of(body), fx["A"])
    ok("⑨ 改完完工率 ⇒ recalc_required=true", a2.get("recalc_required") is True)
    drift = (a2.get("calc") or {}).get("drift") or []
    ok("⑨ 漂移命中「完工率」", any(d.get("field") == "完工率" for d in drift), str(drift))
    pct_pair = next((d for d in drift if d.get("field") == "完工率"), {})
    ok("⑨ 表值 60 / 计算单 50", (pct_pair.get("table_value"), pct_pair.get("calc_value")) == ("60.00", "50.00"), str(pct_pair))
    ok("⑨ 加工约当跟着变 920.0000", a2.get("equiv_conversion") == "920.0000", str(a2.get("equiv_conversion")))
    ok("⑨ drift 档能筛出来", count("drift") == 1, str(count("drift")))
    q(f"update biz_cost_object set completion_pct = 50 where id = '{fx['a_id']}'::uuid")

    # ---------- ⑩ 没批次 ⇒ 试算视图 ----------
    status, trial = cli.call("GET", f"/business/erp/cost-wip-equiv?period_code={P3}&keyword={kw}")
    trial = data_of(trial)
    ok("⑩ 没批次仍 200（试算视图）", status == 200, err_of(trial))
    ok("⑩ cost_ready=false / batch=null", trial.get("cost_ready") is False and trial.get("batch") is None)
    ok("⑩ 页脚说明「试算」", "试算" in (trial.get("note") or ""), (trial.get("note") or "")[:80])
    ok("⑩ 行还在（按对象现状算）", trial.get("total") == 3, str(trial.get("total")))
    t_a = row_of(trial, fx["A"])
    ok("⑩ calc 列 null", t_a.get("calc") is None)

    # ---------- ⑬ 权限闸：没有 business:erp:cost-object:list 的用户必须 403 ----------
    dept = fx["dept"]
    uname = f"m520np{int(time.time()) % 100000}"
    employee_role = one("select id from sys_role where role_code = 'EMPLOYEE' and deleted_at is null")
    if employee_role:
        status, created = cli.call("POST", "/system/user", {
            "username": uname, "password": "Admin@123456", "real_name": f"{TAG} 无码用户",
            "dept_id": dept, "role_ids": [employee_role], "remark": TAG,
        })
        ok("⑬ 建无码用户（EMPLOYEE 角色）", status == 200, err_of(created))
        no_perm = Client()
        try:
            no_perm.login(uname, "Admin@123456")
            q(f"update sys_user set must_change_password = false where username = '{uname}'")
            no_perm.login(uname, "Admin@123456")
            status, payload = no_perm.call("GET", f"/business/erp/cost-wip-equiv?period_code={P2}")
            ok("⑬ 无成本读码 ⇒ 403（不是 200、不是 500）", status == 403, f"{status} {err_of(payload)}")
        except SystemExit as exc:
            ok("⑬ 无成本读码 ⇒ 403（不是 200、不是 500）", False, f"登录失败：{exc}")
        q(f"delete from sys_user_role where user_id in (select id from sys_user where username = '{uname}')")
        q(f"delete from sys_user where username = '{uname}'")
    else:
        ok("⑬ 无成本读码 ⇒ 403（不是 200、不是 500）", False, "本库没有 EMPLOYEE 角色，无法造无码用户")

    # ---------- ⑫ 清场 ----------
    fixture_cleanup()
    residual = one(f"select (select count(*) from biz_cost_object where object_no like '%{TAG}%') + "
                   f"(select count(*) from biz_cost_period where batch_no like '%{TAG}%') + "
                   f"(select count(*) from biz_material where material_code like '%{TAG}%')")
    ok("⑫ 清场零残留", residual == "0", f"residual={residual}")

    print(f"\n===== {len(PASS)} PASS / {len(FAIL)} FAIL =====")
    if FAIL:
        print("失败项：")
        for name in FAIL:
            print("  -", name)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
