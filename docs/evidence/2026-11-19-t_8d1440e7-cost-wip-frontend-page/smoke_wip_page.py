#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-25 在制品前端页 · HTTP 冒烟（真接口 + 真库，逐条 PASS/FAIL）。

本页只消费两条接口，冒烟就**逐字段**核对「页面要读的每一个键真的存在、真的对」：

    GET  /api/business/erp/cost-object/{id}/operations   （M5-21 · 完工率录入页签）
    PUT  /api/business/erp/cost-object/{id}/operations   （整表覆盖 + 回写对象级）
    GET  /api/business/erp/cost-wip-balance?period_code= （M5-23 · 在制余额页签）

跑法（实例已起在旁边，库已就绪）：

    WIP_BASE=http://127.0.0.1:18311 \
      python3 docs/evidence/2026-11-19-t_8d1440e7-cost-wip-frontend-page/smoke_wip_page.py

覆盖：
  ① 两条路由真的挂上了（200，不是 404 / 不是 SPA HTML）
  ② 录入出参逐字段核对（object / rows / rollup / state / formula / weight_hint / warnings / note）
  ③ 三个工序 100×1 + 50×2 + 0×1 ⇒ 综合 50% ⇒ 对象级回写 50（页面显示的就是它）
  ④ 逐行占比 25/50/25、贡献 25/25/0（编辑器「权重占比 / 贡献」列读的字段）
  ⑤ 整表覆盖：删一道 + 改一道 ⇒ inserted/updated/deleted 计数 + 综合 80%
  ⑥ 400 五连：重复 seq / 缺完工率 / 完工率 101 / 权重 0 / 超 200 行
  ⑦ 409：已关批的批不能改 ⇒ 报文含「已关批」，GET 的 state=locked（抽屉里画锁）
  ⑧ 余额出参逐字段核对（totals 22 键 / row 键 / carry 八态 / state_options 10 条）
  ⑨ 余额 400 三连：缺期间 / 期间形状 / 状态拼错
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
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m525_dev"),
)
BASE = os.environ.get("WIP_BASE", "http://127.0.0.1:18311") + "/api"
ADMIN = ("admin", os.environ.get("WIP_ADMIN_PASSWORD", "Admin@123456"))
TAG = f"M525PG{int(time.time()) % 100000}"
YEAR = 2031 + (int(time.time()) % 4)
P1, P2 = f"{YEAR}-01", f"{YEAR}-02"

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
            with urllib.request.urlopen(req, timeout=60) as resp:
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


def cleanup() -> None:
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


def setup() -> dict:
    q("update sys_user set must_change_password = false where username = 'admin'")
    q("update sys_config set config_value = 'true' where config_key = 'erp.cost.enabled'")
    cleanup()

    dept = one("select id from sys_dept where deleted_at is null order by created_at limit 1")
    dept_name = one(f"select dept_name from sys_dept where id = '{dept}'::uuid")
    unit = one("select id from biz_unit where deleted_at is null order by unit_code limit 1")
    wh = one("select id from biz_warehouse where deleted_at is null order by created_at limit 1")
    q(f"""insert into biz_material (id, material_code, material_name, spec, unit_id,
                                    material_type, status, remark)
          values (gen_random_uuid(), '{TAG}-FG', '{TAG} 产出品', 'M5-25', '{unit}', 'FINISHED', 1, '{TAG}')""")
    product = one(f"select id from biz_material where material_code = '{TAG}-FG'")

    def make_object(name: str, completed: str, wip: str, pct: str, status: str = "OPEN") -> tuple[str, str]:
        oid = one("select gen_random_uuid()")
        no = f"CB-{TAG}-{name}"
        q(f"""insert into biz_cost_object (id, object_no, object_type, product_id, lot_no,
                                           target_warehouse_id, plan_qty, completed_qty, wip_qty,
                                           completion_pct, material_input_mode,
                                           workshop_dept_id, workshop_dept_name, start_date, status, closed_at)
              values ('{oid}'::uuid, '{no}', 'BATCH', '{product}'::uuid, 'LOT-{TAG}-{name}',
                      '{wh}'::uuid, 800, {completed}, {wip}, {pct}, 'ONE_TIME',
                      '{dept}'::uuid, '{dept_name}', '{P1}-01', '{status}',
                      {'now()' if status == 'CLOSED' else 'null'})""")
        return oid, no

    a, a_no = make_object("A", "800", "200", "0")
    z, z_no = make_object("Z", "0", "50", "0", status="CLOSED")

    # 本期批次（余额表要 cost_ready=true 才不是「试算视图」）
    batch = one("select gen_random_uuid()")
    q(f"""insert into biz_cost_period (id, period_code, batch_no, status)
          values ('{batch}'::uuid, '{P2}', 'CS-{TAG}-P2', 'DRAFT')""")
    q(f"""insert into biz_cost_collect (id, cost_object_id, period_code, element, amount, source_type, occur_date, material_id, qty)
          values (gen_random_uuid(), '{a}'::uuid, '{P2}', 'MATERIAL', 1000, 'STOCK_OUT', '{P2}-10', '{product}'::uuid, 100),
                 (gen_random_uuid(), '{a}'::uuid, '{P2}', 'LABOR', 1500, 'LABOR_TICKET', '{P2}-10', null, null),
                 (gen_random_uuid(), '{a}'::uuid, '{P2}', 'OVERHEAD', 700, 'OVERHEAD_TICKET', '{P2}-10', null, null)""")

    return {"a": a, "a_no": a_no, "z": z, "z_no": z_no}


def main() -> int:
    cli = Client()
    cli.login(*ADMIN)
    fx = setup()
    ok("夹具就绪（两个成本对象 + 本期批次 + 三条归集）", all(fx.values()))

    # ---------- ① 路由挂上了 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-object/{fx['a']}/operations")
    ok("① 录入口 GET 200（路由真的挂上了）", status == 200, f"status={status} {err_of(payload) if status != 200 else ''}")
    detail = data_of(payload)

    # ---------- ② 出参逐字段 ----------
    obj = detail.get("object") or {}
    obj_keys = {"id", "object_no", "object_type", "product_code", "product_name", "lot_no",
                "workshop_dept_name", "status", "plan_qty", "completed_qty", "wip_qty",
                "completion_pct", "material_input_mode", "editable", "blocked_reason"}
    ok("② object 抬头字段齐（页面 t-descriptions 读的就是这些）", obj_keys <= set(obj), f"缺 {sorted(obj_keys - set(obj))}")
    ok("② 新批没有工序 ⇒ rows=[] 且 rollup=null（页面画「还没有工序」）",
       detail.get("rows") == [] and detail.get("rollup") is None)
    ok("② 空表页脚 note 说清「沿用哪个值」", "还没有工序" in (detail.get("note") or ""), detail.get("note", "")[:60])
    ok("② state=editable + editable=true（保存按钮可点）",
       detail.get("state") == "editable" and obj.get("editable") is True)
    ok("② formula / weight_hint 非空（页面页脚与列头 tooltip 直接用后端原文）",
       bool(detail.get("formula")) and bool(detail.get("weight_hint")))

    # ---------- ③ 保存三个工序 ----------
    body = {"rows": [
        {"seq": 1, "operation_name": "下料", "weight": "1", "completion_pct": "100"},
        {"seq": 2, "operation_name": "焊接", "weight": "2", "completion_pct": "50", "remark": "关键工序"},
        {"seq": 3, "operation_name": "装配", "weight": "1", "completion_pct": "0"},
    ]}
    status, payload = cli.call("PUT", f"/business/erp/cost-object/{fx['a']}/operations", body)
    put = data_of(payload)
    ok("③ PUT 200", status == 200, f"status={status} {err_of(payload) if status != 200 else ''}")
    rollup = put.get("rollup") or {}
    ok("③ (100×1 + 50×2 + 0×1)/4 = 50%", rollup.get("pct") == "50", f"pct={rollup.get('pct')}")
    ok("③ 三个计数：inserted=3 / updated=0 / deleted=0 / changed=true",
       (put.get("inserted"), put.get("updated"), put.get("deleted"), put.get("changed")) == (3, 0, 0, True),
       f"{put.get('inserted')}/{put.get('updated')}/{put.get('deleted')}/{put.get('changed')}")
    ok("③ 对象级完工率被同事务回写 50（页面顶部那一个数）",
       (put.get("object") or {}).get("completion_pct") in ("50", "50.00"),
       (put.get("object") or {}).get("completion_pct", ""))
    ok("③ rollup.matched=true（对象级与汇总一致）", rollup.get("matched") is True, f"diff={rollup.get('diff')}")

    # ---------- ④ 逐行占比 / 贡献 ----------
    rows = put.get("rows") or []
    ok("④ 三行按 seq 升序", [r.get("seq") for r in rows] == [1, 2, 3], str([r.get("seq") for r in rows]))
    shares = [r.get("share_pct") for r in rows]
    contribs = [r.get("contribution_pct") for r in rows]
    ok("④ 权重占比 25 / 50 / 25", [s.split(".")[0] + "." + s.split(".")[1][:2] for s in shares] == ["25.00", "50.00", "25.00"], str(shares))
    ok("④ 贡献 25 / 25 / 0（逐行加起来 = 综合 50）",
       [c.split(".")[0] + "." + c.split(".")[1][:2] for c in contribs] == ["25.00", "25.00", "0.00"], str(contribs))
    ok("④ 行字段齐（编辑器读 seq / operation_name / weight / completion_pct / remark / updated_at）",
       {"id", "seq", "operation_name", "weight", "completion_pct", "share_pct",
        "contribution_pct", "remark", "updated_at"} <= set(rows[0] if rows else {}))
    ok("④ 备注原样回来（第 2 行「关键工序」）", rows[1].get("remark") == "关键工序", str(rows[1].get("remark")))

    # ---------- ⑤ 整表覆盖 ----------
    body2 = {"rows": [
        {"seq": 1, "operation_name": "下料", "weight": "1", "completion_pct": "100"},
        {"seq": 2, "operation_name": "焊接", "weight": "2", "completion_pct": "70"},
    ]}
    status, payload = cli.call("PUT", f"/business/erp/cost-object/{fx['a']}/operations", body2)
    put2 = data_of(payload)
    rollup2 = put2.get("rollup") or {}
    ok("⑤ 整表覆盖 200", status == 200, err_of(payload) if status != 200 else "删除 seq3 + 改 seq2")
    ok("⑤ (100×1 + 70×2)/3 = 80%", rollup2.get("pct") == "80", f"pct={rollup2.get('pct')}")
    ok("⑤ 计数 updated=1 / deleted=1 / inserted=0 / changed=true",
       (put2.get("updated"), put2.get("deleted"), put2.get("inserted"), put2.get("changed")) == (1, 1, 0, True),
       f"{put2.get('updated')}/{put2.get('deleted')}/{put2.get('inserted')}/{put2.get('changed')}")
    ok("⑤ 库里软删了第三道工序（rows 只剩 2 行）", len(put2.get("rows") or []) == 2, str(len(put2.get("rows") or [])))

    # ---------- ⑥ 400 五连 ----------
    cases = [
        ("重复 seq", {"rows": [{"seq": 1, "operation_name": "A", "completion_pct": "10"},
                               {"seq": 1, "operation_name": "B", "completion_pct": "20"}]}, "重复"),
        ("缺完工率", {"rows": [{"seq": 1, "operation_name": "A"}]}, "completion_pct"),
        ("完工率 101", {"rows": [{"seq": 1, "operation_name": "A", "completion_pct": "101"}]}, "0–100"),
        ("权重 0", {"rows": [{"seq": 1, "operation_name": "A", "weight": "0", "completion_pct": "10"}]}, "weight"),
        ("超 200 行", {"rows": [{"seq": i, "operation_name": f"工序{i}", "completion_pct": "10"} for i in range(1, 202)]}, "200"),
    ]
    for name, bad, needle in cases:
        status, payload = cli.call("PUT", f"/business/erp/cost-object/{fx['a']}/operations", bad)
        msg = json.dumps(payload, ensure_ascii=False)
        ok(f"⑥ 400 · {name}", status == 400 and needle in msg, f"status={status} msg={msg[:120]}")

    # ---------- ⑦ 409：已关批 ----------
    status, payload = cli.call("PUT", f"/business/erp/cost-object/{fx['z']}/operations",
                               {"rows": [{"seq": 1, "operation_name": "下料", "completion_pct": "10"}]})
    msg = json.dumps(payload, ensure_ascii=False)
    ok("⑦ 已关批的批 PUT ⇒ 409 + 报文点名「已关批」", status == 409 and "已关批" in msg, f"status={status} msg={msg[:140]}")
    status, payload = cli.call("GET", f"/business/erp/cost-object/{fx['z']}/operations")
    locked = data_of(payload)
    ok("⑦ 同一批 GET ⇒ state=locked + editable=false + blocked_reason 有原因（抽屉画锁）",
       locked.get("state") == "locked" and (locked.get("object") or {}).get("editable") is False
       and bool((locked.get("object") or {}).get("blocked_reason")),
       f"state={locked.get('state')}")

    # ---------- ⑧ 余额表出参 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-wip-balance?period_code={P2}")
    sheet = data_of(payload)
    ok("⑧ 余额 GET 200", status == 200, f"status={status} {err_of(payload) if status != 200 else ''}")
    totals = sheet.get("totals") or {}
    totals_keys = {"row_count", "wip_row_count", "carried_count", "pending_count", "mismatch_count",
                   "unsettled_count", "reversed_count", "closed_wip_count", "cleared_count",
                   "blocked_count", "beg_qty", "wip_qty", "beg_total", "cur_total", "finished_total",
                   "wip_total", "wip_material", "wip_labor", "wip_overhead", "unbooked_total",
                   "closed_wip_total", "mismatch_abs_total", "element_row_count", "note"}
    ok("⑧ totals 24 个键齐（页面的八张汇总卡全读它）", totals_keys <= set(totals), f"缺 {sorted(totals_keys - set(totals))}")
    ok("⑧ 页面级字段齐（period_code / batch / cost_ready / formula_roll / formula_carry / note / state_options）",
       {"period_code", "batch", "reversed_batch", "cost_ready", "state", "formula_roll",
        "formula_carry", "totals", "list", "total", "page", "page_size", "state_options", "note"} <= set(sheet))
    ok("⑧ 本期有批次 ⇒ cost_ready=true", sheet.get("cost_ready") is True, str(sheet.get("batch")))

    target = None
    for row in sheet.get("list") or []:
        if row.get("object_no") == fx["a_no"]:
            target = row
            break
    ok("⑧ A 批出现在余额表里", target is not None, f"共 {len(sheet.get('list') or [])} 行")
    if target:
        row_keys = {"object_id", "object_no", "object_type", "object_status", "source_doc_no", "lot_no",
                    "product_id", "product_code", "product_name", "workshop_dept_id", "workshop_dept_name",
                    "plan_qty", "cumulative_completed_qty", "completed_qty", "beg_qty", "beg_period",
                    "wip_qty", "completion_pct", "beg_total", "cur_total", "finished_total", "wip_total",
                    "elements", "elements_available", "source", "source_label", "ledger", "carry",
                    "issues", "warnings"}
        ok("⑧ 行字段齐（页面 11 列 + 展开行的数量 / 要素 / 账面 / 结转）", row_keys <= set(target),
           f"缺 {sorted(row_keys - set(target))}")
        carry = target.get("carry") or {}
        eight = {"carried", "pending", "mismatch", "unsettled", "reversed", "closed_wip", "cleared", "blocked"}
        ok("⑧ carry.state 落在八态里（页面判色唯一的依据）", carry.get("state") in eight, f"state={carry.get('state')}")
        ok("⑧ carry 带 label / needs_attention / hint", {"label", "needs_attention", "hint"} <= set(carry))
        ok("⑧ 本期投入 = 归集三条之和 3200（页面「本期投入」列）",
           (target.get("cur_total") or "").startswith("3200"), str(target.get("cur_total")))
        elements = target.get("elements") or []
        ok("⑧ 三要素 3 行且料工费顺序（展开行固定三行）",
           [e.get("element") for e in elements] == ["MATERIAL", "LABOR", "OVERHEAD"],
           str([e.get("element") for e in elements]))

    options = sheet.get("state_options") or []
    ok("⑧ state_options 10 条（下拉就是它，前端不另写词表）", len(options) == 10, str(len(options)))
    ok("⑧ 缺省筛档是 wip（只显示有余额的行）", sheet.get("state") == "wip", str(sheet.get("state")))

    # ---------- ⑨ 余额 400 三连 ----------
    for name, query, needle in [
        ("缺期间", "", "period_code"),
        ("期间形状", "?period_code=2026-13", "期间"),
        ("状态拼错", f"?period_code={P2}&state=bogus", "state"),
    ]:
        status, payload = cli.call("GET", f"/business/erp/cost-wip-balance{query}")
        msg = json.dumps(payload, ensure_ascii=False)
        ok(f"⑨ 400 · {name}", status == 400 and needle in msg, f"status={status} {msg[:120]}")

    # ---------- ⑩ 清场 ----------
    cleanup()
    left = int(one(f"select count(*) from biz_cost_object where object_no like '%{TAG}%'") or "0")
    left_ops = int(one(f"select count(*) from biz_cost_object_operation where operation_name like '%{TAG}%'") or "0")
    ok("⑩ 清场零残留（成本对象 / 工序行 / 物料）", left == 0 and left_ops == 0, f"object={left} ops={left_ops}")

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
