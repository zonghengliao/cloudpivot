#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-28 成本还原「还原后报表：料工费占比」· HTTP 冒烟（真接口 + 真库，逐条 PASS/FAIL）。

    GET /api/business/erp/cost-restore-report?period_code=yyyy-MM&group_by=dept&keyword=&state=&page=&page_size=&max_depth=

跑法（实例已起在旁边，库已就绪）：

    RS_BASE=http://127.0.0.1:18033 \
      python3 docs/evidence/2026-11-19-t_3e57dec1-cost-restore-report/smoke_restore_report.py

覆盖：
  ① 未带令牌 ⇒ 401；路由真的挂上（200，不是 404 / 不是 SPA HTML）
  ② 出参逐字段（period_code / batch / cost_ready / group_by / group_by_label / group_options /
     state / state_label / keyword / max_depth / basis / formula_* / totals / groups /
     total / page / page_size / state_options / note）
  ③ 按车间：一车间组 = 成品 FG（还原后 300/140/260，占比 42.86 / 20.00 / 37.14，材料 −28.57pp）；
     二车间组 = 成品 FG2（300 停在半成品形态 ⇒ 不彻底占比 100.00%、主导要素 none）
  ④ 整表合计：还原前 1000、还原后 700、不彻底 300、check 0、四项占比 100.00
  ⑤ 按产品：FG 与 FG2 同一产出品 ⇒ 合并成一组（1000）
  ⑥ 按对象：逐行与明细表（cost-restore-rollup）同一对象的三要素 / 不彻底一分不差
  ⑦ 整表：一组，等于 totals
  ⑧ 400 三连 + group_by 拼错 + state 拼错；page_size / max_depth 夹取
  ⑨ page_size=1 时合计仍是整表；state=partial 只留二车间
  ⑩ 清场零残留
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
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m528_smoke"),
)
BASE = os.environ.get("RS_BASE", "http://127.0.0.1:18033") + "/api"
ADMIN = ("admin", os.environ.get("RS_ADMIN_PASSWORD", "Admin@123456"))
TAG = f"M528RP{int(time.time()) % 100000}"

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


def pick_periods() -> tuple[str, str]:
    """挑两个**空**期间槽（模板库是别人跑过测试的库，固定年份可能已被占用）。"""
    for year in range(1600, 1700):
        prev, cur = f"{year:04}-05", f"{year:04}-06"
        taken = one("select count(*) from biz_cost_period "
                    f"where period_code in ('{prev}', '{cur}')")
        if taken == "0":
            return prev, cur
    raise SystemExit("找不到两个空期间槽（1600–1699 都被占了？）")


PREV, PERIOD = pick_periods()


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
            with urllib.request.urlopen(req, timeout=120) as resp:
                raw = resp.read().decode()
                return resp.status, (json.loads(raw) if raw.strip().startswith("{") else {"raw": raw[:200]})
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


def msg_of(payload) -> str:
    p = payload or {}
    return f"code={p.get('code')} msg={(p.get('msg') or p.get('message') or '')[:140]}"


def num(text) -> float:
    try:
        return round(float(text), 4)
    except (TypeError, ValueError):
        return float("nan")


def cleanup() -> None:
    like = f"%{TAG}%"
    q(f"delete from biz_cost_period_item where cost_object_id in "
      f"(select id from biz_cost_object where object_no like '{like}')")
    q(f"delete from biz_cost_collect where cost_object_id in "
      f"(select id from biz_cost_object where object_no like '{like}')")
    q(f"delete from biz_stock_out_item where lot_no like '{like}'")
    q(f"delete from biz_stock_out where out_no like '{like}'")
    q(f"delete from biz_cost_period where batch_no like 'CS-{TAG}%'")
    q(f"delete from biz_cost_object where object_no like '{like}'")
    q(f"delete from biz_material where material_code like '{TAG}%'")
    q(f"delete from sys_dept where dept_code = 'DEPT-{TAG}'")


def set_cost_switch(cli: Client, value: str) -> None:
    """把 `erp.cost.enabled` 切成 `value`（**必须走接口**）。

    直连 SQL 改 `sys_config` 不会刷新进程内参数快照 —— 成本开关守卫读的是快照，
    改了库也还是 403（实测等待 2 分钟仍不生效）。参数接口的写操作会重建快照，
    所以这里走 `PUT /support/config/{id}`（与运维在「参数配置」页点保存等价）。
    """
    row_id = one("select id from sys_config where config_key = 'erp.cost.enabled'")
    if not row_id:
        raise SystemExit("参数 erp.cost.enabled 不存在（迁移 V0385 没跑？）")
    status, payload = cli.call("GET", f"/support/config/{row_id}")
    data = (payload or {}).get("data") or {}
    if status != 200 or not data:
        raise SystemExit(f"读成本开关失败：{status} {msg_of(payload)}")
    if str(data.get("config_value")) == value:
        # 值已一致：仍要走一次 PUT —— 否则快照可能还停在旧值（例如上一条 SQL 改的）
        pass
    body = {key: data.get(key) for key in
            ("config_key", "config_type", "description", "category", "sort_order",
             "config_level", "is_modifiable", "is_encrypted", "is_restart", "status",
             "ref_value", "select_value")}
    body["config_value"] = value
    status, payload = cli.call("PUT", f"/support/config/{row_id}", body)
    if status != 200:
        raise SystemExit(f"切换成本开关失败：{status} {msg_of(payload)}")


def setup(cli: Client) -> dict:
    q("update sys_user set must_change_password = false where username = 'admin'")
    set_cost_switch(cli, "true")
    status, payload = cli.call("GET", "/business/erp/cost-switch")
    if not ((payload.get("data") or {}).get("enabled")):
        raise SystemExit(f"成本开关没生效，后续接口会全 403：{status} {payload}")
    cleanup()

    depts = q("select id, dept_name from sys_dept where deleted_at is null order by created_at limit 2")
    if len(depts) < 2:
        # 空库首启只有一个「总部」部门：本脚本自己补一个（清场时删掉），
        # 不依赖模板库里恰好有多个部门。
        q(f"insert into sys_dept (id, dept_code, dept_name, sort_order) "
          f"values (gen_random_uuid(), 'DEPT-{TAG}', '{TAG} 第二车间', 99)")
        depts = q("select id, dept_name from sys_dept where deleted_at is null order by created_at limit 2")
    if len(depts) < 2:
        raise SystemExit("夹具前提缺失：sys_dept 至少需要两个部门（用来验分组）")
    (dept_a, dept_a_name), (dept_b, dept_b_name) = depts
    unit = one("select id from biz_unit where deleted_at is null order by unit_code limit 1")
    wh = one("select id from biz_warehouse where deleted_at is null order by created_at limit 1")
    if not (unit and wh):
        raise SystemExit("夹具前提缺失：biz_unit / biz_warehouse 至少各需一行")

    for code, name, kind in ((f"{TAG}-FG", "成品", "FINISHED"),
                             (f"{TAG}-SEMI", "半成品", "SEMI"),
                             (f"{TAG}-UNC", "未计算半成品", "SEMI"),
                             (f"{TAG}-RAW", "外购料", "RAW")):
        q(f"""insert into biz_material (id, material_code, material_name, spec, unit_id,
                                        material_type, status, remark)
              values (gen_random_uuid(), '{code}', '{TAG} {name}', 'M5-28', '{unit}', '{kind}', 1, '{TAG}')""")
    fg = one(f"select id from biz_material where material_code = '{TAG}-FG'")
    semi = one(f"select id from biz_material where material_code = '{TAG}-SEMI'")
    unc = one(f"select id from biz_material where material_code = '{TAG}-UNC'")
    raw = one(f"select id from biz_material where material_code = '{TAG}-RAW'")

    batch_prev = one("select gen_random_uuid()")
    q(f"""insert into biz_cost_period (id, period_code, batch_no, status)
          values ('{batch_prev}'::uuid, '{PREV}', 'CS-{TAG}-P1', 'DRAFT')""")
    batch_cur = one("select gen_random_uuid()")
    q(f"""insert into biz_cost_period (id, period_code, batch_no, status)
          values ('{batch_cur}'::uuid, '{PERIOD}', 'CS-{TAG}-P2', 'DRAFT')""")

    def make_object(name: str, product: str, lot: str, dept: str, dept_name: str) -> tuple[str, str]:
        oid = one("select gen_random_uuid()")
        no = f"CB-{TAG}-{name}"
        q(f"""insert into biz_cost_object (id, object_no, object_type, product_id, lot_no,
                                           target_warehouse_id, plan_qty, completed_qty, wip_qty,
                                           completion_pct, material_input_mode,
                                           workshop_dept_id, workshop_dept_name, start_date, status)
              values ('{oid}'::uuid, '{no}', 'BATCH', '{product}'::uuid, '{lot}',
                      '{wh}'::uuid, 1000, 100, 0, 100, 'ONE_TIME',
                      '{dept}'::uuid, '{dept_name}', '{PREV}-01', 'OPEN')""")
        return oid, no

    def stock_out(material: str, lot: str, qty, amount, date: str) -> str:
        out_id = one("select gen_random_uuid()")
        item_id = one("select gen_random_uuid()")
        q(f"""insert into biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status)
              values ('{out_id}'::uuid, 'OUT-{TAG}-{item_id[:8]}', 'ISSUE', '{wh}'::uuid, '{date}', 'POSTED')""")
        q(f"""insert into biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty,
                                              unit_cost, amount, lot_no)
              values ('{item_id}'::uuid, '{out_id}'::uuid, 1, '{material}'::uuid, '{unit}'::uuid,
                      {qty}, {round(amount / qty, 6)}, {amount}, '{lot}')""")
        return item_id

    # ---- 半成品 S 的产出批（上期，一车间）：外购料 500 + 工 100 + 费 400，完工 50 ⇒ 结构 500/100/400
    s_id, s_no = make_object("S", semi, f"LOT-{TAG}-S", dept_a, dept_a_name)
    item_raw = stock_out(raw, f"LOT-{TAG}-R1", 100, 500, f"{PREV}-10")
    q(f"""insert into biz_cost_collect (id, cost_object_id, period_code, element, source_type, source_id,
                                        line_no, occur_date, material_id, qty, unit_price, amount)
          values (gen_random_uuid(), '{s_id}'::uuid, '{PREV}', 'MATERIAL', 'STOCK_OUT', '{item_raw}'::uuid,
                  1, '{PREV}-10', '{raw}'::uuid, 100, 5, 500),
                 (gen_random_uuid(), '{s_id}'::uuid, '{PREV}', 'LABOR', 'MANUAL', null,
                  2, '{PREV}-10', null, null, null, 100),
                 (gen_random_uuid(), '{s_id}'::uuid, '{PREV}', 'OVERHEAD', 'MANUAL', null,
                  3, '{PREV}-10', null, null, null, 400)""")
    q(f"""insert into biz_cost_period_item (id, period_id, cost_object_id, product_id,
                                           completed_qty, mat_cur, labor_cur, oh_cur,
                                           unit_material, unit_labor, unit_overhead,
                                           finished_cost, unit_finished_cost, status)
          values (gen_random_uuid(), '{batch_prev}'::uuid, '{s_id}'::uuid, '{semi}'::uuid,
                  50, 500, 100, 400, 10, 2, 8, 1000, 20, 'CALC')""")

    # ---- 成品 FG（一车间）：本期领 S 400（批号命中）+ 外购料 100 + 工 100 + 费 100
    fg_id, fg_no = make_object("FG", fg, f"LOT-{TAG}-FG", dept_a, dept_a_name)
    item_semi = stock_out(semi, f"LOT-{TAG}-S", 40, 400, f"{PERIOD}-10")
    q(f"""insert into biz_cost_collect (id, cost_object_id, period_code, element, source_type, source_id,
                                        line_no, occur_date, material_id, qty, unit_price, amount)
          values (gen_random_uuid(), '{fg_id}'::uuid, '{PERIOD}', 'MATERIAL', 'STOCK_OUT', '{item_semi}'::uuid,
                  1, '{PERIOD}-10', '{semi}'::uuid, 40, 10, 400),
                 (gen_random_uuid(), '{fg_id}'::uuid, '{PERIOD}', 'MATERIAL', 'MANUAL', null,
                  2, '{PERIOD}-10', '{raw}'::uuid, 20, 5, 100),
                 (gen_random_uuid(), '{fg_id}'::uuid, '{PERIOD}', 'LABOR', 'MANUAL', null,
                  3, '{PERIOD}-10', null, null, null, 100),
                 (gen_random_uuid(), '{fg_id}'::uuid, '{PERIOD}', 'OVERHEAD', 'MANUAL', null,
                  4, '{PERIOD}-10', null, null, null, 100)""")
    q(f"""insert into biz_cost_period_item (id, period_id, cost_object_id, product_id,
                                           completed_qty, mat_cur, labor_cur, oh_cur,
                                           unit_material, unit_labor, unit_overhead,
                                           finished_cost, unit_finished_cost, status)
          values (gen_random_uuid(), '{batch_cur}'::uuid, '{fg_id}'::uuid, '{fg}'::uuid,
                  70, 500, 100, 100, 7.142857, 1.428571, 1.428571, 700, 10, 'CALC')""")

    # ---- 半成品 UNC 的产出批：有对象、**没有计算单**（还原不彻底那一档）
    unc_id, unc_no = make_object("UNC", unc, f"LOT-{TAG}-UNC", dept_a, dept_a_name)
    item_unc = stock_out(raw, f"LOT-{TAG}-R2", 10, 100, f"{PREV}-10")
    q(f"""insert into biz_cost_collect (id, cost_object_id, period_code, element, source_type, source_id,
                                        line_no, occur_date, material_id, qty, unit_price, amount)
          values (gen_random_uuid(), '{unc_id}'::uuid, '{PREV}', 'MATERIAL', 'STOCK_OUT', '{item_unc}'::uuid,
                  1, '{PREV}-10', '{raw}'::uuid, 10, 10, 100)""")

    # ---- 成品 FG2（二车间）：本期领 UNC 300（批号命中，但产出批没算成本）⇒ 落在二车间那组
    fg2_id, fg2_no = make_object("FG2", fg, f"LOT-{TAG}-FG2", dept_b, dept_b_name)
    item_unc2 = stock_out(unc, f"LOT-{TAG}-UNC", 10, 300, f"{PERIOD}-10")
    q(f"""insert into biz_cost_collect (id, cost_object_id, period_code, element, source_type, source_id,
                                        line_no, occur_date, material_id, qty, unit_price, amount)
          values (gen_random_uuid(), '{fg2_id}'::uuid, '{PERIOD}', 'MATERIAL', 'STOCK_OUT', '{item_unc2}'::uuid,
                  1, '{PERIOD}-10', '{unc}'::uuid, 10, 30, 300)""")
    q(f"""insert into biz_cost_period_item (id, period_id, cost_object_id, product_id,
                                           completed_qty, mat_cur, labor_cur, oh_cur,
                                           unit_material, unit_labor, unit_overhead,
                                           finished_cost, unit_finished_cost, status)
          values (gen_random_uuid(), '{batch_cur}'::uuid, '{fg2_id}'::uuid, '{fg}'::uuid,
                  10, 300, 0, 0, 30, 0, 0, 300, 30, 'CALC')""")

    return {
        "fg": fg_no, "fg2": fg2_no, "s": s_no, "unc": unc_no,
        "fg_id": fg_id, "fg2_id": fg2_id,
        "dept_a": dept_a, "dept_a_name": dept_a_name,
        "dept_b": dept_b, "dept_b_name": dept_b_name,
        "fg_product": fg,
    }


def report(cli: Client, query: str) -> tuple[int, dict]:
    status, payload = cli.call("GET", f"/business/erp/cost-restore-report?{query}")
    return status, data_of(payload)


def group_by_key(data: dict, key: str) -> dict:
    for group in data.get("groups") or []:
        if group.get("key") == key:
            return group
    raise SystemExit(f"报表里没有分组 {key}；实际 = {[g.get('key') for g in data.get('groups') or []]}")


def main() -> int:
    cli = Client()
    # ---------- ① 未带令牌 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-restore-report?period_code={PERIOD}")
    ok("① 未带令牌 ⇒ 401（不是 200，也不是 500）", status == 401, f"status={status} {msg_of(payload)}")

    cli.login(*ADMIN)
    fixture = setup(cli)

    # ---------- ② 路由 + 出参骨架 ----------
    status, data = report(cli, f"period_code={PERIOD}&keyword={TAG}")
    ok("② 路由挂上且 200（不是 404 / 不是 SPA HTML）", status == 200, f"status={status}")
    keys = ["period_code", "batch", "cost_ready", "group_by", "group_by_label", "group_options",
            "state", "state_label", "keyword", "max_depth", "basis", "formula_restore",
            "formula_unresolved", "formula_share", "formula_shift", "totals", "groups",
            "total", "page", "page_size", "state_options", "note"]
    missing = [key for key in keys if key not in data]
    ok("② 出参 22 个键齐全", not missing, f"缺 {missing}" if missing else "")
    ok("② 缺省维度 = dept、缺省一页 50、缺省层数 6",
       data.get("group_by") == "dept" and data.get("page_size") == 50 and data.get("max_depth") == 6,
       f"{data.get('group_by')}/{data.get('page_size')}/{data.get('max_depth')}")
    ok("② 维度选项 4 档", [item.get("value") for item in data.get("group_options") or []]
       == ["dept", "product", "object", "all"], str(data.get("group_options")))
    ok("② 状态选项 4 档", len(data.get("state_options") or []) == 4)
    ok("② 本期有批次（cost_ready）", data.get("cost_ready") is True and (data.get("batch") or {}).get("batch_no"),
       str(data.get("batch")))
    ok("② 口径文案齐（basis + 4 条公式）",
       all(str(data.get(key) or "").strip() for key in
           ("basis", "formula_restore", "formula_unresolved", "formula_share", "formula_shift")))
    ok("② 页脚写明占比分母", "还原前合计" in str(data.get("note")), str(data.get("note"))[:120])

    # ---------- ③ 按车间分组 ----------
    ok("③ 两个车间两组", data.get("total") == 2, f"total={data.get('total')}")

    group_a = group_by_key(data, fixture["dept_a"])
    ok("③ 一车间组名 = 部门名", group_a.get("label") == fixture["dept_a_name"], str(group_a.get("label")))
    ok("③ 一车间 1 个成本对象", group_a.get("object_count") == 1)
    before, restored, shift = group_a["before"], group_a["restored"], group_a["shift"]
    ok("③ 还原前 500 / 100 / 100 = 700",
       (num(before["material"]), num(before["labor"]), num(before["overhead"]), num(before["total"]))
       == (500.0, 100.0, 100.0, 700.0), str(before))
    ok("③ 还原后 300 / 140 / 260 = 700",
       (num(restored["material"]), num(restored["labor"]), num(restored["overhead"]), num(restored["total"]))
       == (300.0, 140.0, 260.0, 700.0), str(restored))
    ok("③ 占比分母 = 还原前合计 700", num(restored["denominator"]) == 700.0, str(restored["denominator"]))
    ok("③ 占比 42.86 / 20.00 / 37.14，四项合计 100.00",
       (restored["material_share"], restored["labor_share"], restored["overhead_share"],
        restored["unresolved_share"], group_a["share_sum"]) == ("42.86", "20.00", "37.14", "0.00", "100.00"),
       str([restored["material_share"], restored["labor_share"], restored["overhead_share"],
            restored["unresolved_share"], group_a["share_sum"]]))
    ok("③ 还原前占比 71.43 / 14.29 / 14.29",
       (before["material_share"], before["labor_share"], before["overhead_share"]) == ("71.43", "14.29", "14.29"),
       str([before["material_share"], before["labor_share"], before["overhead_share"]]))
    ok("③ 占比变化 −28.57 / +5.71 / +22.85",
       (shift["material_pp"], shift["labor_pp"], shift["overhead_pp"]) == ("-28.57", "5.71", "22.85"),
       str([shift["material_pp"], shift["labor_pp"], shift["overhead_pp"]]))
    ok("③ 从材料里拆出的人工 40 / 制造费用 160",
       (num(group_a["restored_labor"]), num(group_a["restored_overhead"])) == (40.0, 160.0))
    ok("③ 半成品 400 / 材料 500 ⇒ 80.00%",
       num(group_a["semi_in_material"]) == 400.0 and group_a["semi_share"] == "80.00")
    ok("③ 主导要素 = 材料（材料 300 最大）", group_a["dominant"] == "material" and group_a["dominant_label"] == "材料最高")
    ok("③ 一车间行状态：已还原 1 / 不彻底 0 / 无需还原 0",
       (group_a["restored_rows"], group_a["partial_rows"], group_a["no_semi_rows"]) == (1, 0, 0))
    ok("③ 一车间守恒 check = 0", num(group_a["check"]) == 0.0, str(group_a["check"]))

    group_b = group_by_key(data, fixture["dept_b"])
    restored_b = group_b["restored"]
    ok("③ 二车间组 = FG2（300 停在半成品形态）",
       (num(restored_b["material"]), num(restored_b["total"]), num(restored_b["unresolved"])) == (0.0, 0.0, 300.0),
       str(restored_b))
    ok("③ 二车间不彻底占比 100.00（不当外购料）", restored_b["unresolved_share"] == "100.00")
    ok("③ 二车间主导要素 none", group_b["dominant"] == "none" and group_b["dominant_label"] == "无数据")
    ok("③ 二车间不彻底行数 1", group_b["partial_rows"] == 1)
    ok("③ 二车间守恒 check = 0", num(group_b["check"]) == 0.0)
    ok("③ 分组按还原后合计降序（一车间 700 在前）", data["groups"][0]["key"] == fixture["dept_a"])

    # ---------- ④ 整表合计 ----------
    totals = data["totals"]
    ok("④ 合计：还原前 800 / 100 / 100 = 1000",
       (num(totals["before"]["material"]), num(totals["before"]["labor"]),
        num(totals["before"]["overhead"]), num(totals["before"]["total"])) == (800.0, 100.0, 100.0, 1000.0),
       str(totals["before"]))
    ok("④ 合计：还原后 300 / 140 / 260 = 700",
       (num(totals["restored"]["material"]), num(totals["restored"]["labor"]),
        num(totals["restored"]["overhead"]), num(totals["restored"]["total"])) == (300.0, 140.0, 260.0, 700.0),
       str(totals["restored"]))
    ok("④ 合计：不彻底 300、check 0",
       num(totals["restored"]["unresolved"]) == 300.0 and num(totals["check"]) == 0.0)
    ok("④ 合计占比 30.00 / 14.00 / 26.00 / 30.00 = 100.00",
       (totals["restored"]["material_share"], totals["restored"]["labor_share"],
        totals["restored"]["overhead_share"], totals["restored"]["unresolved_share"],
        totals["share_sum"]) == ("30.00", "14.00", "26.00", "30.00", "100.00"),
       str([totals["restored"]["material_share"], totals["restored"]["labor_share"],
            totals["restored"]["overhead_share"], totals["restored"]["unresolved_share"], totals["share_sum"]]))
    ok("④ 合计行数 2（= 各组对象数之和）",
       totals["object_count"] == 2
       and totals["object_count"] == sum(group["object_count"] for group in data["groups"]))

    # ---------- ⑤ 按产品（FG 与 FG2 同一产出品 ⇒ 合并） ----------
    _, by_product = report(cli, f"period_code={PERIOD}&keyword={TAG}&group_by=product")
    ok("⑤ 按产品：1 组（FG 与 FG2 同一个产出品）", by_product.get("total") == 1, f"total={by_product.get('total')}")
    product_group = by_product["groups"][0]
    ok("⑤ 产品组合并后 1000 = 700 + 300", num(product_group["before"]["total"]) == 1000.0)
    ok("⑤ 产品组 key = 产出品 id", product_group["key"] == fixture["fg_product"])
    ok("⑤ 产品组两组对象", product_group["object_count"] == 2)
    ok("⑤ 产品组合计 = 整表合计", num(product_group["restored"]["material"]) == num(totals["restored"]["material"]))

    # ---------- ⑥ 按对象（与明细表对拍） ----------
    _, by_object = report(cli, f"period_code={PERIOD}&keyword={TAG}&group_by=object")
    ok("⑥ 按对象：2 组", by_object.get("total") == 2)
    status, rollup = cli.call(
        "GET", f"/business/erp/cost-restore-rollup?period_code={PERIOD}&keyword={TAG}&page_size=200")
    rollup_rows = {row["object_no"]: row for row in (data_of(rollup).get("list") or [])}
    ok("⑥ 明细表拿到 2 行", status == 200 and len(rollup_rows) == 2, f"status={status} rows={len(rollup_rows)}")
    mismatched = []
    for group in by_object["groups"]:
        label = group["label"]
        row = rollup_rows.get(label)
        if row is None:
            mismatched.append(f"{label}: 明细表没有")
            continue
        checks = [
            ("before.material", group["before"]["material"], row["before"]["material"]),
            ("after.material", group["restored"]["material"], row["after"]["material"]),
            ("after.labor", group["restored"]["labor"], row["after"]["labor"]),
            ("after.overhead", group["restored"]["overhead"], row["after"]["overhead"]),
            ("after.total", group["restored"]["total"], row["after"]["total"]),
            ("unresolved", group["restored"]["unresolved"], row["unresolved"]),
            ("restored_labor", group["restored_labor"], row["restored_labor"]),
            ("restored_overhead", group["restored_overhead"], row["restored_overhead"]),
        ]
        for name, left, right in checks:
            if num(left) != num(right):
                mismatched.append(f"{label}.{name}: {left} ≠ {right}")
    ok("⑥ 对象组逐字段 = 明细表同一行（8 个字段）", not mismatched, "; ".join(mismatched[:6]))
    ok("⑥ 对象组带副标（产出品 · 批号）",
       all("批号" in (group.get("hint") or "") for group in by_object["groups"]))

    # ---------- ⑦ 整表（一组） ----------
    _, whole = report(cli, f"period_code={PERIOD}&keyword={TAG}&group_by=all")
    ok("⑦ 整表：1 组，key = ALL", whole.get("total") == 1 and whole["groups"][0]["key"] == "ALL")
    ok("⑦ 整表组 = totals（还原前 1000 / 还原后 700 / 不彻底 300）",
       (num(whole["groups"][0]["before"]["total"]), num(whole["groups"][0]["restored"]["total"]),
        num(whole["groups"][0]["restored"]["unresolved"])) == (1000.0, 700.0, 300.0))

    # ---------- ⑧ 400 / 夹取 ----------
    for name, query, needle in (
        ("⑧ 缺期间 ⇒ 400", "", "period_code"),
        ("⑧ 期间形状错 ⇒ 400", "period_code=2026-13", "期间编码"),
        ("⑧ group_by 拼错 ⇒ 400", f"period_code={PERIOD}&group_by=workshop", "group_by"),
        ("⑧ state 拼错 ⇒ 400", f"period_code={PERIOD}&state=done&keyword={TAG}", "state"),
    ):
        status, payload = cli.call("GET", f"/business/erp/cost-restore-report?{query}")
        ok(name, status == 400 and needle in json.dumps(payload, ensure_ascii=False), f"status={status} {msg_of(payload)}")

    _, capped = report(cli, f"period_code={PERIOD}&keyword={TAG}&max_depth=99&page_size=9999")
    ok("⑧ max_depth 夹到 12 / page_size 夹到 500",
       capped.get("max_depth") == 12 and capped.get("page_size") == 500,
       f"{capped.get('max_depth')}/{capped.get('page_size')}")
    _, defaulted = report(cli, f"period_code={PERIOD}&keyword={TAG}&max_depth=0&page_size=0")
    ok("⑧ max_depth=0 ⇒ 6 / page_size=0 ⇒ 50",
       defaulted.get("max_depth") == 6 and defaulted.get("page_size") == 50)

    # ---------- ⑨ 分页 / 状态 ----------
    _, paged = report(cli, f"period_code={PERIOD}&keyword={TAG}&page_size=1")
    ok("⑨ page_size=1：当前页 1 组、分组总数 2", len(paged["groups"]) == 1 and paged.get("total") == 2)
    ok("⑨ page_size=1 时合计仍是整表（1000 / 2 个对象）",
       num(paged["totals"]["before"]["total"]) == 1000.0 and paged["totals"]["object_count"] == 2)
    _, only_partial = report(cli, f"period_code={PERIOD}&keyword={TAG}&state=partial")
    ok("⑨ state=partial 只留二车间（1 组、1000→300）",
       only_partial.get("total") == 1
       and only_partial["groups"][0]["key"] == fixture["dept_b"]
       and num(only_partial["totals"]["before"]["total"]) == 300.0,
       f"total={only_partial.get('total')}")
    _, only_restored = report(cli, f"period_code={PERIOD}&keyword={TAG}&state=restored")
    ok("⑨ state=restored 只留一车间（还原后人工 140）",
       only_restored.get("total") == 1 and num(only_restored["totals"]["restored"]["labor"]) == 140.0)
    _, empty = report(cli, f"period_code={PERIOD}&keyword={TAG}&state=no_semi&group_by=product")
    ok("⑨ 空结果不画假 0.00%（占比 None、合计 0）",
       empty.get("total") == 0 and empty["totals"]["restored"]["material_share"] is None
       and empty["totals"]["share_sum"] is None,
       str(empty["totals"].get("restored")))
    ok("⑨ 空结果给可读说明（不是接口故障）",
       "没有可汇总的成本对象" in str(empty.get("note")), str(empty.get("note"))[:90])

    # ---------- ⑩ 清场 ----------
    cleanup()
    residue = one("select (select count(*) from biz_cost_object where object_no like "
                  f"'CB-{TAG}%') + (select count(*) from biz_cost_period where batch_no like 'CS-{TAG}%')"
                  f" + (select count(*) from biz_material where material_code like '{TAG}%')"
                  f" + (select count(*) from sys_dept where dept_code = 'DEPT-{TAG}')")
    ok("⑩ 清场零残留", residue == "0", f"residue={residue}")

    print(f"\n==== {len(PASS)} PASS / {len(FAIL)} FAIL ====")
    if FAIL:
        print("FAILED:", ", ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
