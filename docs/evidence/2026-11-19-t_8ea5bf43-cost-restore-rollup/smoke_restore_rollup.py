#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-27 成本还原「逐层还原到料工费」· HTTP 冒烟（真接口 + 真库，逐条 PASS/FAIL）。

    GET /api/business/erp/cost-restore-rollup?period_code=yyyy-MM&keyword=&state=&page=&page_size=&max_depth=

跑法（实例已起在旁边，库已就绪）：

    RS_BASE=http://127.0.0.1:18031 \
      python3 docs/evidence/2026-11-19-t_8ea5bf43-cost-restore-rollup/smoke_restore_rollup.py

覆盖：
  ① 路由真的挂上（200，不是 404 / 不是 SPA HTML）；未带令牌 401
  ② 出参逐字段（period_code / batch / cost_ready / state / keyword / max_depth /
     formula_restore / formula_unresolved / totals / list / total / page / page_size /
     state_options / note）
  ③ 夹具甲：成品 FG ← 半成品 S（批号命中） + 外购料 RAW ⇒ 还原前 500/100/100、还原后 300/140/260
  ④ 逐层台账：第 1 层 500 拆成 外购 100 + 人工 40 + 费 160 + 下拨 200；第 2 层 200 全成外购料
  ⑤ 半成品明细：S 被卷积 400，还原成 200/40/160（S 的结构 500/100/400），stopped=false
  ⑥ 差额：材料 -200 / 人工 +40 / 费 +160 / 合计 0
  ⑦ 夹具乙：半成品没计算 ⇒ partial，300 停在半成品形态（不当外购料），仍守恒
  ⑧ 合计：row_count=2、还原前 1000、还原后 700、不彻底 300、check=0
  ⑨ 400 三连 + max_depth 夹取（0⇒6、99⇒12）
  ⑩ page_size=1 时合计仍是整表；state=partial 收窄
  ⑪ 清场零残留
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
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m527_smoke"),
)
BASE = os.environ.get("RS_BASE", "http://127.0.0.1:18031") + "/api"
ADMIN = ("admin", os.environ.get("RS_ADMIN_PASSWORD", "Admin@123456"))
TAG = f"M527RS{int(time.time()) % 100000}"


def pick_periods() -> tuple[str, str]:
    """挑两个**空**期间槽（模板库是别人跑过测试的库，固定年份可能已被占用）。"""
    for year in range(1700, 1800):
        prev, cur = f"{year:04}-05", f"{year:04}-06"
        taken = one(
            "select count(*) from biz_cost_period "
            f"where period_code in ('{prev}', '{cur}')")
        if taken == "0":
            return prev, cur
    raise SystemExit("找不到两个空期间槽（1700–1799 都被占了？）")

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


# 期间槽在导入时定下来（smoke 里的 URL / SQL 都用这两个常量）
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


def setup() -> dict:
    q("update sys_user set must_change_password = false where username = 'admin'")
    q("update sys_config set config_value = 'true' where config_key = 'erp.cost.enabled'")
    cleanup()

    dept = one("select id from sys_dept where deleted_at is null order by created_at limit 1")
    dept_name = one(f"select dept_name from sys_dept where id = '{dept}'::uuid")
    unit = one("select id from biz_unit where deleted_at is null order by unit_code limit 1")
    wh = one("select id from biz_warehouse where deleted_at is null order by created_at limit 1")
    if not (dept and unit and wh):
        raise SystemExit("夹具前提缺失：sys_dept / biz_unit / biz_warehouse 至少各需一行")

    for code, name, kind in ((f"{TAG}-FG", "成品", "FINISHED"),
                             (f"{TAG}-SEMI", "半成品", "SEMI"),
                             (f"{TAG}-UNC", "未计算半成品", "SEMI"),
                             (f"{TAG}-RAW", "外购料", "RAW")):
        q(f"""insert into biz_material (id, material_code, material_name, spec, unit_id,
                                        material_type, status, remark)
              values (gen_random_uuid(), '{code}', '{TAG} {name}', 'M5-27', '{unit}', '{kind}', 1, '{TAG}')""")
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

    def make_object(name: str, product: str, lot: str) -> tuple[str, str]:
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

    # ---- 半成品 S 的产出批（上期）：外购料 500 + 工 100 + 费 400，完工 50 ⇒ 结构 500/100/400
    s_id, s_no = make_object("S", semi, f"LOT-{TAG}-S")
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

    # ---- 成品 FG：本期领 S 400（批号命中）+ 外购料 100 + 工 100 + 费 100
    fg_id, fg_no = make_object("FG", fg, f"LOT-{TAG}-FG")
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

    # ---- 半成品 UNC 的产出批：有对象、**没有计算单**（还原不彻底的那一档）
    unc_id, unc_no = make_object("UNC", unc, f"LOT-{TAG}-UNC")
    item_unc = stock_out(raw, f"LOT-{TAG}-R2", 10, 100, f"{PREV}-10")
    q(f"""insert into biz_cost_collect (id, cost_object_id, period_code, element, source_type, source_id,
                                        line_no, occur_date, material_id, qty, unit_price, amount)
          values (gen_random_uuid(), '{unc_id}'::uuid, '{PREV}', 'MATERIAL', 'STOCK_OUT', '{item_unc}'::uuid,
                  1, '{PREV}-10', '{raw}'::uuid, 10, 10, 100)""")

    # ---- 成品 FG2：本期领 UNC 300（批号命中，但产出批没算成本）
    fg2_id, fg2_no = make_object("FG2", fg, f"LOT-{TAG}-FG2")
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

    return {"fg": fg_no, "fg2": fg2_no, "s": s_no, "unc": unc_no}


def num(text) -> float:
    try:
        return round(float(text), 4)
    except (TypeError, ValueError):
        return float("nan")


def main() -> int:
    cli = Client()
    # ---------- ① 未带令牌 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-restore-rollup?period_code={PERIOD}")
    ok("① 未带令牌 ⇒ 401（不是 200，也不是 500）", status == 401, f"status={status} {msg_of(payload)}")

    cli.login(*ADMIN)
    fx = setup()
    ok("夹具就绪（成品 FG / FG2 + 半成品产出批 S / UNC + 八条归集）", all(fx.values()))

    status, payload = cli.call("GET", f"/business/erp/cost-restore-rollup?period_code={PERIOD}&keyword={TAG}")
    sheet = data_of(payload)
    ok("① 还原表 GET 200（路由真的挂上了）", status == 200,
       f"status={status} {msg_of(payload) if status != 200 else ''}")

    # ---------- ② 出参逐字段 ----------
    top_keys = {"period_code", "batch", "cost_ready", "state", "keyword", "max_depth", "formula_restore",
                "formula_unresolved", "totals", "list", "total", "page", "page_size", "state_options", "note"}
    ok("② 页面级字段齐（15 个键）", top_keys <= set(sheet), f"缺 {sorted(top_keys - set(sheet))}")
    ok("② period_code 原样回显 + cost_ready=true（本期有批次）",
       sheet.get("period_code") == PERIOD and sheet.get("cost_ready") is True,
       f"{sheet.get('period_code')} / {sheet.get('cost_ready')}")
    ok("② max_depth 缺省回显 6", sheet.get("max_depth") == 6, str(sheet.get("max_depth")))
    ok("② formula_restore / formula_unresolved 非空（页脚口径直接用后端原文）",
       bool(sheet.get("formula_restore")) and bool(sheet.get("formula_unresolved")))
    ok("② state_options 4 档（all / restored / partial / no_semi）",
       [o.get("value") for o in (sheet.get("state_options") or [])] == ["all", "restored", "partial", "no_semi"],
       str([o.get("value") for o in (sheet.get("state_options") or [])]))

    rows = sheet.get("list") or []
    target = next((r for r in rows if r.get("object_no") == fx["fg"]), None)
    ok("② 行出现（keyword 收窄到本夹具）", target is not None, f"共 {len(rows)} 行")

    if target:
        row_keys = {"object_id", "object_no", "object_type", "lot_no", "product_id", "product_code",
                    "product_name", "workshop_dept_id", "workshop_dept_name", "item", "finished_cost",
                    "before", "semi_in_material", "purchased_in_material", "semi_share", "after",
                    "after_material_share", "after_labor_share", "after_overhead_share", "delta",
                    "restored_labor", "restored_overhead", "unresolved", "state", "state_label",
                    "depth", "node_count", "flows", "semis", "issues", "warnings"}
        ok("③ 行字段齐（还原前/还原后/差额/台账/明细）", row_keys <= set(target),
           f"缺 {sorted(row_keys - set(target))}")
        before, after = target.get("before") or {}, target.get("after") or {}
        ok("③ 还原前：材料 500 / 人工 100 / 制造费用 100 / 合计 700",
           (num(before.get("material")), num(before.get("labor")), num(before.get("overhead")),
            num(before.get("total"))) == (500.0, 100.0, 100.0, 700.0),
           str(before))
        ok("③ 材料里：半成品 400 / 外购料 100 / 占比 80.00%",
           (num(target.get("semi_in_material")), num(target.get("purchased_in_material"))) == (400.0, 100.0)
           and num(target.get("semi_share")) == 80.0,
           f"{target.get('semi_in_material')} / {target.get('purchased_in_material')} / {target.get('semi_share')}")
        ok("③ 还原后：外购料 300 / 人工 140 / 制造费用 260 / 合计 700",
           (num(after.get("material")), num(after.get("labor")), num(after.get("overhead")),
            num(after.get("total"))) == (300.0, 140.0, 260.0, 700.0),
           str(after))
        ok("③ 行状态 = restored（钻到底了）", target.get("state") == "restored",
           f"state={target.get('state')} label={target.get('state_label')}")
        ok("③ 还原不彻底 = 0", num(target.get("unresolved")) == 0.0, str(target.get("unresolved")))

        # ---------- ④ 逐层台账 ----------
        flows = target.get("flows") or []
        ok("④ 台账两层（半成品 S 钻到外购料为止）",
           len(flows) == 2 and [f.get("level") for f in flows] == [1, 2],
           str([(f.get("level"), f.get("imputed")) for f in flows]))
        if len(flows) == 2:
            l1, l2 = flows[0], flows[1]
            ok("④ 第 1 层：流入 500 = 外购 100 + 人工 40 + 费 160 + 下拨 200 + 不彻底 0",
               (num(l1.get("imputed")), num(l1.get("purchased")), num(l1.get("labor")),
                num(l1.get("overhead")), num(l1.get("carried_down")), num(l1.get("unresolved")))
               == (500.0, 100.0, 40.0, 160.0, 200.0, 0.0), str(l1))
            ok("④ 第 2 层：流入 200 全部确认成外购料",
               (num(l2.get("imputed")), num(l2.get("purchased")), num(l2.get("carried_down")))
               == (200.0, 200.0, 0.0), str(l2))
            ok("④ 每层自洽（外购 + 人工 + 费 + 下拨 + 不彻底 = 流入）",
               all(abs((num(f.get("purchased")) + num(f.get("labor")) + num(f.get("overhead"))
                        + num(f.get("carried_down")) + num(f.get("unresolved"))) - num(f.get("imputed"))) < 0.0001
                   for f in flows))

        # ---------- ⑤ 半成品明细 ----------
        semis = target.get("semis") or []
        ok("⑤ 半成品明细 1 条（只列自制半成品，不列外购料）",
           len(semis) == 1 and semis[0].get("level") == 1, str(len(semis)))
        if semis:
            item = semis[0]
            ok("⑤ S 被卷积 400 ⇒ 还原成 200 料 / 40 工 / 160 费，stopped=false",
               (num(item.get("imputed")), num(item.get("elements", {}).get("material")),
                num(item.get("elements", {}).get("labor")), num(item.get("elements", {}).get("overhead")))
               == (400.0, 200.0, 40.0, 160.0) and item.get("stopped") is False,
               f"{item.get('imputed')} / {item.get('elements')} / stopped={item.get('stopped')}")
            ok("⑤ 明细带结构来源与产出对象号",
               item.get("source") == "batch" and item.get("producer_nos") == fx["s"],
               f"{item.get('source')} / {item.get('producer_nos')}")

        # ---------- ⑥ 差额 ----------
        delta = target.get("delta") or {}
        ok("⑥ 差额：材料 -200 / 人工 +40 / 费 +160 / 合计 0（还原不动总额）",
           (num(delta.get("material")), num(delta.get("labor")), num(delta.get("overhead")),
            num(delta.get("total"))) == (-200.0, 40.0, 160.0, 0.0), str(delta))
        ok("⑥ restored_labor / restored_overhead 与差额一致",
           (num(target.get("restored_labor")), num(target.get("restored_overhead"))) == (40.0, 160.0),
           f"{target.get('restored_labor')} / {target.get('restored_overhead')}")

    # ---------- ⑦ 缺结构 ⇒ partial ----------
    partial = next((r for r in rows if r.get("object_no") == fx["fg2"]), None)
    ok("⑦ 缺结构的行出现", partial is not None, str([r.get("object_no") for r in rows]))
    if partial:
        p_before, p_after = partial.get("before") or {}, partial.get("after") or {}
        ok("⑦ 状态 partial + 300 停在半成品形态（不当外购料）",
           partial.get("state") == "partial"
           and num(partial.get("unresolved")) == 300.0
           and num(p_after.get("material")) == 0.0,
           f"state={partial.get('state')} unresolved={partial.get('unresolved')} after={p_after}")
        ok("⑦ 仍守恒（还原后 0 + 不彻底 300 = 还原前 300）",
           abs(num(p_after.get("total")) + num(partial.get("unresolved")) - num(p_before.get("total"))) < 0.0001,
           f"{p_after.get('total')} + {partial.get('unresolved')} vs {p_before.get('total')}")
        ok("⑦ 行内说清为什么没还原", any("取不到完工成本结构" in w for w in (partial.get("warnings") or [])),
           str(partial.get("warnings"))[:200])

    # ---------- ⑧ 合计 ----------
    totals = sheet.get("totals") or {}
    ok("⑧ 合计：两行 / 还原前 1000 / 还原后 700 / 不彻底 300 / check 0",
       (num(totals.get("row_count")) if isinstance(totals.get("row_count"), str) else totals.get("row_count")) == 2
       and (num(totals.get("before_total")), num(totals.get("after_total")), num(totals.get("unresolved")),
            num(totals.get("check"))) == (1000.0, 700.0, 300.0, 0.0),
       f"rows={totals.get('row_count')} before={totals.get('before_total')} "
       f"after={totals.get('after_total')} unresolved={totals.get('unresolved')} check={totals.get('check')}")
    ok("⑧ 分档：restored 1 行 / partial 1 行 / no_semi 0 行",
       (totals.get("restored_row_count"), totals.get("partial_row_count"),
        totals.get("no_semi_row_count")) == (1, 1, 0),
       f"{totals.get('restored_row_count')} / {totals.get('partial_row_count')} / {totals.get('no_semi_row_count')}")
    ok("⑧ 最深层数 2", totals.get("max_depth") == 2, str(totals.get("max_depth")))

    # ---------- ⑨ 400 三连 + 夹取 ----------
    status, payload = cli.call("GET", "/business/erp/cost-restore-rollup")
    ok("⑨ 缺 period_code ⇒ 400", status == 400 and "period_code" in json.dumps(payload, ensure_ascii=False),
       f"status={status} {msg_of(payload)}")
    status, payload = cli.call("GET", "/business/erp/cost-restore-rollup?period_code=2026-13")
    ok("⑨ 期间形状错 ⇒ 400", status == 400, f"status={status} {msg_of(payload)}")
    status, payload = cli.call("GET",
                               f"/business/erp/cost-restore-rollup?period_code={PERIOD}&state=done&keyword={TAG}")
    ok("⑨ 状态拼错 ⇒ 400（不静默回落成 all）",
       status == 400 and "done" in json.dumps(payload, ensure_ascii=False), f"status={status} {msg_of(payload)}")

    _, payload = cli.call("GET", f"/business/erp/cost-restore-rollup?period_code={PERIOD}&keyword={TAG}&max_depth=0")
    ok("⑨ max_depth=0 ⇒ 回显默认 6（夹取不报错）", (data_of(payload) or {}).get("max_depth") == 6,
       str((data_of(payload) or {}).get("max_depth")))
    _, payload = cli.call("GET", f"/business/erp/cost-restore-rollup?period_code={PERIOD}&keyword={TAG}&max_depth=99")
    ok("⑨ max_depth=99 ⇒ 夹到上限 12", (data_of(payload) or {}).get("max_depth") == 12,
       str((data_of(payload) or {}).get("max_depth")))

    # ---------- ⑩ 分页 / 筛选 ----------
    _, payload = cli.call("GET",
                          f"/business/erp/cost-restore-rollup?period_code={PERIOD}&keyword={TAG}&page_size=1")
    paged = data_of(payload) or {}
    ok("⑩ page_size=1：当前页 1 行，但 total 与合计仍是整表 2 行",
       len(paged.get("list") or []) == 1 and paged.get("total") == 2
       and (paged.get("totals") or {}).get("row_count") == 2
       and num((paged.get("totals") or {}).get("before_total")) == 1000.0,
       f"total={paged.get('total')} totals.row_count={(paged.get('totals') or {}).get('row_count')}")

    _, payload = cli.call("GET",
                          f"/business/erp/cost-restore-rollup?period_code={PERIOD}&keyword={TAG}&state=partial")
    narrowed = data_of(payload) or {}
    ok("⑩ state=partial 收窄到 1 行（FG2）",
       narrowed.get("total") == 1 and (narrowed.get("list") or [{}])[0].get("object_no") == fx["fg2"],
       str([r.get("object_no") for r in (narrowed.get("list") or [])]))

    _, payload = cli.call("GET",
                          f"/business/erp/cost-restore-rollup?period_code={PERIOD}&keyword={TAG}&state=restored")
    restored = data_of(payload) or {}
    ok("⑩ state=restored 收窄到 1 行（FG）",
       restored.get("total") == 1 and (restored.get("list") or [{}])[0].get("object_no") == fx["fg"],
       str([r.get("object_no") for r in (restored.get("list") or [])]))

    # ---------- ⑪ 清场 ----------
    cleanup()
    residue = one(f"select (select count(*) from biz_cost_object where object_no like '%{TAG}%')"
                  f" + (select count(*) from biz_cost_collect c join biz_cost_object o on o.id = c.cost_object_id"
                  f"    where o.object_no like '%{TAG}%')"
                  f" + (select count(*) from biz_cost_period where batch_no like 'CS-{TAG}%')"
                  f" + (select count(*) from biz_material where material_code like '{TAG}%')")
    ok("⑪ 清场零残留", residue == "0", f"残留 {residue} 行")

    print(f"\n== PASS {len(PASS)} / FAIL {len(FAIL)} ==")
    if FAIL:
        for name in FAIL:
            print(f"  FAIL: {name}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
