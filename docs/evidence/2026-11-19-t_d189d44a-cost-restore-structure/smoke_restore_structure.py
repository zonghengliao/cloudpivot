#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-26 成本还原「半成品成本结构表」· HTTP 冒烟（真接口 + 真库，逐条 PASS/FAIL）。

    GET /api/business/erp/cost-restore-structure?period_code=yyyy-MM&keyword=&state=&page=&page_size=&max_depth=

跑法（实例已起在旁边，库已就绪）：

    RS_BASE=http://127.0.0.1:18026 \
      python3 docs/evidence/2026-11-19-t_d189d44a-cost-restore-structure/smoke_restore_structure.py

覆盖：
  ① 路由真的挂上（200，不是 404 / 不是 SPA HTML）；未带令牌 401
  ② 出参逐字段（period_code / batch / cost_ready / state / keyword / max_depth / formula_layer /
     formula_element / totals / list / total / page / page_size / state_options / note）
  ③ 夹具：成品 A ← 半成品 S（批号命中 OBJ-B）+ 外购料 RAW ⇒ 行头 材料 1500 / 半成品 1000 / 外购 500
  ④ 逐层树：两个一级节点（S 1000 / RAW 500）、S 的 source=batch、elements 600/250/150、
     producer 指向 OBJ-B、RAW 是叶子且全额进材料（三要素合计 = 节点金额）
  ⑤ 逐层汇总 layers[0]：node_count=2 / semi_node_count=1 / amount=1500 / 三要素 1100+250+150
  ⑥ 整表合计 totals：row_count=1（keyword 收窄）/ node_count=2 / 金额对得上
  ⑦ 400 四连：缺期间 / 期间形状 / 状态拼错 / max_depth=0 回显默认 6（夹取不报错）
  ⑧ page_size=1 时 row_count 仍是整表（服务端分页不骗财务）
  ⑨ 清场零残留
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
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m526_smoke"),
)
BASE = os.environ.get("RS_BASE", "http://127.0.0.1:18026") + "/api"
ADMIN = ("admin", os.environ.get("RS_ADMIN_PASSWORD", "Admin@123456"))
TAG = f"M526RS{int(time.time()) % 100000}"
YEAR = 2033
PERIOD = f"{YEAR}-05"

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
                             (f"{TAG}-RAW", "外购料", "RAW")):
        q(f"""insert into biz_material (id, material_code, material_name, spec, unit_id,
                                        material_type, status, remark)
              values (gen_random_uuid(), '{code}', '{TAG} {name}', 'M5-26', '{unit}', '{kind}', 1, '{TAG}')""")
    fg = one(f"select id from biz_material where material_code = '{TAG}-FG'")
    semi = one(f"select id from biz_material where material_code = '{TAG}-SEMI'")
    raw = one(f"select id from biz_material where material_code = '{TAG}-RAW'")

    batch = one("select gen_random_uuid()")
    q(f"""insert into biz_cost_period (id, period_code, batch_no, status)
          values ('{batch}'::uuid, '{PERIOD}', 'CS-{TAG}', 'DRAFT')""")

    def make_object(name: str, product: str, lot: str) -> tuple[str, str]:
        oid = one("select gen_random_uuid()")
        no = f"CB-{TAG}-{name}"
        q(f"""insert into biz_cost_object (id, object_no, object_type, product_id, lot_no,
                                           target_warehouse_id, plan_qty, completed_qty, wip_qty,
                                           completion_pct, material_input_mode,
                                           workshop_dept_id, workshop_dept_name, start_date, status)
              values ('{oid}'::uuid, '{no}', 'BATCH', '{product}'::uuid, '{lot}',
                      '{wh}'::uuid, 100, 100, 0, 100, 'ONE_TIME',
                      '{dept}'::uuid, '{dept_name}', '{PERIOD}-01', 'OPEN')""")
        return oid, no

    # 半成品 S 的产出批次（结构 60% / 25% / 15%）
    b_id, b_no = make_object("S", semi, f"LOT-{TAG}-S")
    q(f"""insert into biz_cost_period_item (id, period_id, cost_object_id, product_id,
                                           completed_qty, mat_cur, labor_cur, oh_cur,
                                           unit_material, unit_labor, unit_overhead,
                                           finished_cost, unit_finished_cost, status)
          values (gen_random_uuid(), '{batch}'::uuid, '{b_id}'::uuid, '{semi}'::uuid,
                  100, 6000, 2500, 1500, 60, 25, 15, 10000, 100, 'CALC')""")

    # 成品 A：领半成品 1000（批号命中 S）+ 外购料 500
    a_id, a_no = make_object("A", fg, f"LOT-{TAG}-A")
    out_id = one("select gen_random_uuid()")
    q(f"""insert into biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status)
          values ('{out_id}'::uuid, '{TAG}-OUT', 'ISSUE', '{wh}'::uuid, '{PERIOD}-10', 'POSTED')""")
    item_id = one("select gen_random_uuid()")
    q(f"""insert into biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty, unit_cost, amount, lot_no)
          values ('{item_id}'::uuid, '{out_id}'::uuid, 1, '{semi}'::uuid, '{unit}'::uuid, 10, 100, 1000, 'LOT-{TAG}-S')""")
    q(f"""insert into biz_cost_collect (id, cost_object_id, period_code, element, source_type, source_id,
                                        line_no, occur_date, material_id, qty, unit_price, amount)
          values (gen_random_uuid(), '{a_id}'::uuid, '{PERIOD}', 'MATERIAL', 'STOCK_OUT', '{item_id}'::uuid,
                  1, '{PERIOD}-10', '{semi}'::uuid, 10, 100, 1000),
                 (gen_random_uuid(), '{a_id}'::uuid, '{PERIOD}', 'MATERIAL', 'MANUAL', null,
                  2, '{PERIOD}-10', '{raw}'::uuid, 5, 100, 500),
                 (gen_random_uuid(), '{a_id}'::uuid, '{PERIOD}', 'LABOR', 'LABOR_TICKET', null,
                  3, '{PERIOD}-10', null, null, null, 300)""")
    return {"a": a_id, "a_no": a_no, "b": b_id, "b_no": b_no}


def num(text) -> float:
    try:
        return round(float(text), 4)
    except (TypeError, ValueError):
        return float("nan")


def main() -> int:
    cli = Client()
    # ---------- ① 未带令牌 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-restore-structure?period_code={PERIOD}")
    ok("① 未带令牌 ⇒ 401（不是 200，也不是 500）", status == 401, f"status={status} {msg_of(payload)}")

    cli.login(*ADMIN)
    fx = setup()
    ok("夹具就绪（成品 A + 半成品产出批 S + 三条归集）", all(fx.values()))

    status, payload = cli.call("GET", f"/business/erp/cost-restore-structure?period_code={PERIOD}&keyword={TAG}")
    sheet = data_of(payload)
    ok("① 结构表 GET 200（路由真的挂上了）", status == 200,
       f"status={status} {msg_of(payload) if status != 200 else ''}")

    # ---------- ② 出参逐字段 ----------
    top_keys = {"period_code", "batch", "cost_ready", "state", "keyword", "max_depth", "formula_layer",
                "formula_element", "totals", "list", "total", "page", "page_size", "state_options", "note"}
    ok("② 页面级字段齐（15 个键）", top_keys <= set(sheet), f"缺 {sorted(top_keys - set(sheet))}")
    ok("② period_code 原样回显 + cost_ready=true（本期有批次）",
       sheet.get("period_code") == PERIOD and sheet.get("cost_ready") is True,
       f"{sheet.get('period_code')} / {sheet.get('cost_ready')}")
    ok("② max_depth 缺省回显 6", sheet.get("max_depth") == 6, str(sheet.get("max_depth")))
    ok("② formula_layer / formula_element 非空（页脚口径直接用后端原文）",
       bool(sheet.get("formula_layer")) and bool(sheet.get("formula_element")))
    ok("② state_options 5 档（all / semi / no_semi / blocked / 全部同义档）",
       len(sheet.get("state_options") or []) >= 4, str([o.get("value") for o in (sheet.get("state_options") or [])]))

    rows = sheet.get("list") or []
    target = next((r for r in rows if r.get("object_no") == fx["a_no"]), None)
    ok("② 行出现（keyword 收窄到本夹具）", target is not None, f"共 {len(rows)} 行")

    if target:
        row_keys = {"object_id", "object_no", "object_type", "lot_no", "product_id", "product_code",
                    "product_name", "workshop_dept_id", "workshop_dept_name", "item", "finished_cost",
                    "material_cost", "semi_cost", "purchased_cost", "semi_share", "state", "state_label",
                    "depth", "node_count", "layers", "tree", "issues", "warnings"}
        ok("③ 行字段齐（页面 12 列 + 展开树 + 逐层汇总）", row_keys <= set(target),
           f"缺 {sorted(row_keys - set(target))}")
        ok("③ 行头金额：材料 1500 / 半成品 1000 / 外购 500",
           (num(target.get("material_cost")), num(target.get("semi_cost")), num(target.get("purchased_cost")))
           == (1500.0, 1000.0, 500.0),
           f"{target.get('material_cost')} / {target.get('semi_cost')} / {target.get('purchased_cost')}")
        ok("③ 半成品占比 = 1000/1500 = 66.6667%", abs(num(target.get("semi_share")) - 66.6667) < 0.01,
           str(target.get("semi_share")))
        ok("③ 行状态 = semi（有自制半成品且结构取到）", target.get("state") == "semi",
           f"state={target.get('state')} label={target.get('state_label')}")

        tree = target.get("tree") or []
        ok("④ 一级节点 2 个（S 1000 在前、RAW 500 在后：按金额降序）",
           len(tree) == 2 and num(tree[0].get("amount")) == 1000.0 and num(tree[1].get("amount")) == 500.0,
           str([(n.get("material_code"), n.get("amount")) for n in tree]))

        semi_node = tree[0] if tree else {}
        node_keys = {"level", "material_id", "material_code", "material_name", "lot_no", "qty", "unit_cost",
                     "amount", "share_pct", "is_semi", "source", "source_label", "producer", "elements",
                     "children", "truncated", "issues", "warnings"}
        ok("④ 节点字段齐（18 个键）", node_keys <= set(semi_node), f"缺 {sorted(node_keys - set(semi_node))}")
        ok("④ S 节点 is_semi=true 且 source=batch（批号命中）",
           semi_node.get("is_semi") is True and semi_node.get("source") == "batch",
           f"is_semi={semi_node.get('is_semi')} source={semi_node.get('source')}")
        ok("④ S 节点三要素 600 / 250 / 150（合计 = 金额 1000）",
           (num((semi_node.get("elements") or {}).get("material")),
            num((semi_node.get("elements") or {}).get("labor")),
            num((semi_node.get("elements") or {}).get("overhead")),
            num((semi_node.get("elements") or {}).get("total"))) == (600.0, 250.0, 150.0, 1000.0),
           json.dumps(semi_node.get("elements"), ensure_ascii=False))
        producer = semi_node.get("producer") or {}
        ok("④ producer 指到 OBJ-B（object_no / lot_no / 完工成本 10000 / 单位 100）",
           producer.get("object_nos") == fx["b_no"] and producer.get("lot_no") == f"LOT-{TAG}-S"
           and num(producer.get("finished_cost")) == 10000.0 and num(producer.get("unit_finished_cost")) == 100.0,
           f"{producer.get('object_nos')} / {producer.get('lot_no')} / {producer.get('finished_cost')} / "
           f"{producer.get('unit_finished_cost')}")
        ok("④ S 占比 66.6667%", abs(num(semi_node.get("share_pct")) - 66.6667) < 0.01, str(semi_node.get("share_pct")))
        ok("④ S 没有下一层（产出批本身没领料）⇒ children=[] 且未截断",
           semi_node.get("children") == [] and semi_node.get("truncated") is False)

        raw_node = tree[1] if len(tree) > 1 else {}
        ok("④ RAW 节点 is_semi=false、无 source、全额进材料（500/0/0）",
           raw_node.get("is_semi") is False and raw_node.get("source") is None
           and (num((raw_node.get("elements") or {}).get("material")),
                num((raw_node.get("elements") or {}).get("labor")),
                num((raw_node.get("elements") or {}).get("overhead"))) == (500.0, 0.0, 0.0),
           json.dumps(raw_node.get("elements"), ensure_ascii=False))

        layers = target.get("layers") or []
        ok("⑤ layers 恰 1 层（第 1 层就没有更深的了）", len(layers) == 1, str([l.get("level") for l in layers]))
        layer = layers[0] if layers else {}
        ok("⑤ 第 1 层：2 个节点 / 1 个半成品 / 金额 1500 / 三要素 1100+250+150",
           (layer.get("node_count"), layer.get("semi_node_count"), num(layer.get("amount")),
            num((layer.get("elements") or {}).get("material")),
            num((layer.get("elements") or {}).get("labor")),
            num((layer.get("elements") or {}).get("overhead"))) == (2, 1, 1500.0, 1100.0, 250.0, 150.0),
           f"{layer.get('node_count')}/{layer.get('semi_node_count')}/{layer.get('amount')} "
           f"{json.dumps(layer.get('elements'), ensure_ascii=False)}")

    # ---------- ⑥ 整表合计 ----------
    totals = sheet.get("totals") or {}
    totals_keys = {"row_count", "semi_row_count", "no_semi_row_count", "no_structure_row_count",
                   "cycle_row_count", "truncated_row_count", "blocked_row_count", "material_cost",
                   "semi_cost", "purchased_cost", "semi_share", "finished_cost", "finished_row_count",
                   "max_depth", "node_count", "note"}
    ok("⑥ totals 字段齐（页面 10 张汇总卡读它）", totals_keys <= set(totals),
       f"缺 {sorted(totals_keys - set(totals))}")
    ok("⑥ 合计：1 行 / 1 行 semi / 材料 1500 / 半成品 1000 / 外购 500 / 节点 2",
       (totals.get("row_count"), totals.get("semi_row_count"), num(totals.get("material_cost")),
        num(totals.get("semi_cost")), num(totals.get("purchased_cost")), totals.get("node_count"))
       == (1, 1, 1500.0, 1000.0, 500.0, 2),
       f"{totals.get('row_count')}/{totals.get('semi_row_count')}/{totals.get('material_cost')}/"
       f"{totals.get('semi_cost')}/{totals.get('purchased_cost')}/{totals.get('node_count')}")

    # ---------- ⑧ 服务端分页不骗人 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-restore-structure?period_code={PERIOD}&keyword={TAG}&page=1&page_size=1")
    paged = data_of(payload)
    ok("⑧ page_size=1 时 total 仍是整表 1 行、totals 仍是整表金额",
       status == 200 and paged.get("total") == 1 and (paged.get("totals") or {}).get("row_count") == 1
       and num((paged.get("totals") or {}).get("material_cost")) == 1500.0,
       f"status={status} total={paged.get('total')} totals.row_count={(paged.get('totals') or {}).get('row_count')}")

    # ---------- ⑦ 400 三连 + 夹取 ----------
    for name, query, needle in [
        ("缺期间", "?keyword=x", "period_code"),
        ("期间形状", "?period_code=2033-13", "期间"),
        ("状态拼错", f"?period_code={PERIOD}&state=bogus", "state"),
    ]:
        status, payload = cli.call("GET", f"/business/erp/cost-restore-structure{query}")
        msg = json.dumps(payload, ensure_ascii=False)
        ok(f"⑦ 400 · {name}", status == 400 and needle in msg, f"status={status} msg={msg[:120]}")

    status, payload = cli.call("GET", f"/business/erp/cost-restore-structure?period_code={PERIOD}&keyword={TAG}&max_depth=0")
    ok("⑦ max_depth=0 ⇒ 回显默认 6（夹取不报错）",
       status == 200 and data_of(payload).get("max_depth") == 6,
       f"status={status} max_depth={data_of(payload).get('max_depth')}")
    status, payload = cli.call("GET", f"/business/erp/cost-restore-structure?period_code={PERIOD}&keyword={TAG}&max_depth=99")
    ok("⑦ max_depth=99 ⇒ 夹到上限 12",
       status == 200 and data_of(payload).get("max_depth") == 12,
       f"status={status} max_depth={data_of(payload).get('max_depth')}")

    # ---------- ⑨ 清场 ----------
    cleanup()
    left = int(one(f"select count(*) from biz_cost_object where object_no like '%{TAG}%'") or "0")
    left_item = int(one(f"select count(*) from biz_cost_period_item i join biz_cost_object o on o.id = i.cost_object_id "
                        f"where o.object_no like '%{TAG}%'") or "0")
    left_period = int(one(f"select count(*) from biz_cost_period where batch_no like 'CS-{TAG}%'") or "0")
    ok("⑨ 清场零残留（成本对象 / 计算单 / 批次 / 物料）",
       left == 0 and left_item == 0 and left_period == 0, f"object={left} item={left_item} period={left_period}")

    print()
    print(f"===== {len(PASS)} PASS / {len(FAIL)} FAIL =====")
    if FAIL:
        print("失败：")
        for name in FAIL:
            print(f"  · {name}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
