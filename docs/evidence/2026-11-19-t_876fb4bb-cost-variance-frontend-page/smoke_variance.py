#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-19 差异分析页 · HTTP 冒烟（真接口 + 真库）。

跑法（实例已起在旁边，库已迁到 V0466，且 `erp.cost.enabled = true`）：

    CV_BASE=http://127.0.0.1:8023 NIK_DATABASE_NAME=cp_m519_smoke \\
      python3 docs/evidence/2026-11-19-t_876fb4bb-cost-variance-frontend-page/smoke_variance.py

本页只有一个数据源：`GET /business/erp/report/cost-variance`。冒烟覆盖页面上
**每一个显示位**都确实拿得到数据、且口径与页面话术一致：

  ① 期间必填：缺 / 空 / 非法（`2026-13`）→ 400 + `10001`（页面据此「不发请求 + 画引导语」）
  ② 无批次期间：`cost_ready = false`、行全为「缺实际」、`note` 指向 `/cost-period/calc`
     （页面据此画 warning 条 + 「去成本核算」按钮）
  ③ 可比行：标准 19.5 × 800 = 15600、实际 16320、差异 720（不利）、差异率 4.62%、
     三要素 400 / 320 / 0（相加 = 720）、单位差异 0.9、尾差 0
  ④ 缺标准行（有实际没卡）：`state = NO_STANDARD`、`standard = null`、`hint` 指向「标准成本」页
  ⑤ 缺实际行（有卡没计算单）：`state = NO_ACTUAL`、`actual = null`、`hint` 指向成本核算
  ⑥ 合计：rows 3 / 可比 1 / 缺标准 1 / 缺实际 1；缺标准的实际金额单列（300），不混进差异
  ⑦ 四档筛选：comparable（可比**且有差异**）/ no_standard / no_actual 各自命中；
     关键字收窄；分页 total 恒定、合计不随页变
  ⑧ 权限：只持 EMPLOYEE 的账号 → 403（菜单不给他，接口也拦）
  ⑨ 清场：夹具删净，再查 total = 0

夹具用**当前月 2026-11**（页面首屏默认本月）：三张物料 E2E02-M1 / -M2 / -M3，
标准卡直接置 `PUBLISHED`（真人审批链属 M5-10 用例），成本批次置 `DRAFT`（活跃批次，
不触发 `ck_biz_cost_period_settle` 的凭证要求）。
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
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m519_smoke"),
)
BASE = os.environ.get("CV_BASE", "http://127.0.0.1:8023") + "/api"
USER = (os.environ.get("CV_USER", "e2e01_appr"), os.environ.get("CV_PASSWORD", "E2e01@Appr123"))
LOW_USER = ("e2e06_emp_only", "Drill@123456")

PERIOD = "2026-11"
PERIOD_END = "2026-11-30"
EMPTY_PERIOD = "2026-10"
BATCH_NO = "CS-202611-9901"
OBJ_PREFIX = "CB-M519-"
STD_REMARK = "M519 fixture"

# 物料编码（模板库里已有；本卡不新建物料，避免脏主数据）
CODE_A, CODE_B, CODE_C = "E2E02-M1", "E2E02-M2", "E2E02-M3"

# 夹具金额（断言里的期望值全部由这些输入推导，不硬编码「魔法数」）
A_COMPLETED = 800
A_STD = dict(material="12", labor="5", overhead="2.5")
A_ACT = dict(unit_material="12.5", unit_labor="5.4", unit_overhead="2.5")
C_COMPLETED = 100

PASS, FAIL = [], []


# ==================== 小工具 ====================

def q(sql: str) -> list[list[str]]:
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"], "-d", DB["name"],
         "-v", "ON_ERROR_STOP=1", "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(f"SQL 失败：{r.stderr.strip()[:600]}\n---\n{sql[:500]}")
    return [ln.split("\x1f") for ln in r.stdout.strip().split("\n") if ln]


def one(sql: str, default: str = "") -> str:
    rows = q(sql)
    return rows[0][0] if rows and rows[0] and rows[0][0] is not None else default


def ok(name: str, cond: bool, detail: str = "") -> None:
    (PASS if cond else FAIL).append(name)
    print(f"[{'PASS' if cond else 'FAIL'}] {name}{('  · ' + detail) if detail else ''}")


class Client:
    def __init__(self) -> None:
        self.token = ""

    def call(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            BASE + path, data=data, method=method,
            headers={"Content-Type": "application/json",
                     **({"Authorization": f"Bearer {self.token}"} if self.token else {})})
        try:
            with urllib.request.urlopen(req, timeout=120) as resp:
                return resp.status, json.loads(resp.read().decode() or "{}")
        except urllib.error.HTTPError as err:
            raw = err.read()
            try:
                return err.code, json.loads(raw.decode() or "{}")
            except Exception:  # noqa: BLE001
                return err.code, {"_raw": raw[:300].decode(errors="replace")}

    def login(self, user: str, pwd: str) -> None:
        status, payload = self.call("POST", "/system/auth/login", {"username": user, "password": pwd})
        if status != 200:
            raise SystemExit(f"登录失败（{user}）：{status} {payload}")
        self.token = (payload.get("data") or {}).get("access_token", "")
        if not self.token:
            raise SystemExit(f"登录回包没有 token：{payload}")


def data_of(payload) -> dict:
    return (payload or {}).get("data") or {}


def rows_of(payload) -> list:
    return data_of(payload).get("list") or []


def totals_of(payload) -> dict:
    return data_of(payload).get("totals") or {}


def by_code(payload, code: str) -> dict | None:
    for row in rows_of(payload):
        if row.get("material_code") == code:
            return row
    return None


def get_variance(client: Client, **params):
    query = "&".join(f"{k}={urllib.parse.quote(str(v))}" for k, v in params.items())
    return client.call("GET", f"/business/erp/report/cost-variance?{query}")


import urllib.parse  # noqa: E402  （用在 get_variance，放在上面会挡住分组阅读）


# ==================== 夹具 ====================

def cleanup() -> None:
    q("delete from biz_cost_period_item where period_id in "
      "(select id from biz_cost_period where batch_no = "
      f"'{BATCH_NO}')")
    q(f"delete from biz_cost_period where batch_no = '{BATCH_NO}'")
    q(f"delete from biz_cost_standard where remark = '{STD_REMARK}'")
    q(f"delete from biz_cost_object where object_no like '{OBJ_PREFIX}%'")


def seed() -> dict:
    cleanup()
    mats = {}
    for code in (CODE_A, CODE_B, CODE_C):
        mid = one(f"select id from biz_material where deleted_at is null and material_code = '{code}'")
        if not mid:
            raise SystemExit(f"模板库缺物料 {code}（夹具前提）")
        mats[code] = mid

    # ① 标准成本卡：A 可比、B 只有标准（都要「已发布 + 启用 + 覆盖期末」）
    std_labor_hours, std_labor_rate = "0.1", "50"
    for code in (CODE_A, CODE_B):
        q(f"""
        insert into biz_cost_standard
            (id, material_id, version_no, effective_from, effective_to,
             std_material_cost, std_labor_cost, std_overhead_cost, std_total_cost,
             std_labor_hours, std_labor_rate, status, remark,
             created_at, updated_at, publish_state, published_at)
        values
            (gen_random_uuid(), '{mats[code]}', 1, '{PERIOD}-01', '{PERIOD_END}',
             {A_STD['material']}, {A_STD['labor']}, {A_STD['overhead']},
             {A_STD['material']} + {A_STD['labor']} + {A_STD['overhead']},
             {std_labor_hours}, {std_labor_rate}, 1, '{STD_REMARK}',
             now(), now(), 'PUBLISHED', now())
        """)

    # ② 成本核算批次（DRAFT = 活跃批次；SETTLED 会要求凭证，夹具不造凭证）
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

    # ③ 成本对象（A 与 C 各一个；字段值照抄库里已有的对象，只换 id / 单号 / 产品）
    tpl = q("select target_warehouse_id, workshop_dept_id, workshop_dept_name, object_type, "
            "material_input_mode from biz_cost_object where deleted_at is null limit 1")
    if not tpl:
        raise SystemExit("模板库没有可参考的成本对象（夹具前提）")
    wh, dept, dept_name, obj_type, input_mode = tpl[0]

    objs = {}
    for idx, code in enumerate((CODE_A, CODE_C), start=1):
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

    # ④ 成本计算单行：A（完工 800）+ C（完工 100，只有实际、没有标准）
    q(f"""
        insert into biz_cost_period_item
            (id, period_id, cost_object_id, product_id,
             wip_beg_qty, wip_beg_cost, completed_qty, wip_end_qty, completion_pct,
             equiv_material, equiv_conversion, mat_cur, labor_cur, oh_cur,
             unit_material, unit_labor, unit_overhead, finished_cost, wip_end_cost,
             unit_finished_cost, status, created_at, updated_at)
        values
            (gen_random_uuid(), '{period_id}', '{objs[CODE_A]}', '{mats[CODE_A]}',
             0, 0, {A_COMPLETED}, 0, 100, {A_COMPLETED}, {A_COMPLETED},
             10000, 4320, 2000,
             {A_ACT['unit_material']}, {A_ACT['unit_labor']}, {A_ACT['unit_overhead']},
             16320, 0, 20.4, 'CALC', now(), now()),
            (gen_random_uuid(), '{period_id}', '{objs[CODE_C]}', '{mats[CODE_C]}',
             0, 0, {C_COMPLETED}, 0, 100, {C_COMPLETED}, {C_COMPLETED},
             100, 100, 100, 1, 1, 1, 300, 0, 3, 'CALC', now(), now())
    """)
    print(f"夹具：{CODE_A}（可比）/ {CODE_B}（缺实际）/ {CODE_C}（缺标准）· 批次 {BATCH_NO}")
    return mats


# ==================== 断言 ====================

def main() -> int:
    if one("select config_value from sys_config where config_key = 'erp.cost.enabled'") != "true":
        raise SystemExit("erp.cost.enabled 不是 true —— 先开开关并重启实例（配置读的是进程内缓存）")

    client = Client()
    client.login(*USER)
    mats = seed()
    a_code, b_code, c_code = mats[CODE_A], mats[CODE_B], mats[CODE_C]
    print(f"物料 id：A={a_code[:8]} B={b_code[:8]} C={c_code[:8]}\n")

    # ---------- ① 期间必填 ----------
    status, payload = get_variance(client, page=1, page_size=20)
    ok("① 缺期间 ⇒ 400 + 10001（页面据此不发请求）",
       status == 400 and (payload.get("code") == 10001), f"{status} {payload.get('code')} {payload.get('msg')}")
    status, payload = get_variance(client, period_code="2026-13")
    ok("① 非法期间 2026-13 ⇒ 400", status == 400, f"{status} {payload.get('msg')}")

    # ---------- ② 无批次期间 ----------
    status, payload = get_variance(client, period_code=EMPTY_PERIOD, page=1, page_size=20)
    empty = data_of(payload)
    ok("② 无批次期间 ⇒ cost_ready=false + 无批次", status == 200 and empty.get("cost_ready") is False
       and empty.get("batch") is None, f"{status}")
    ok("② note 指向成本核算计算入口", "cost-period/calc" in (empty.get("note") or ""),
       (empty.get("note") or "")[:80])

    # ---------- ③④⑤ 当月全量 ----------
    status, payload = get_variance(client, period_code=PERIOD, page=1, page_size=20)
    payload_ok = status == 200 and data_of(payload).get("total") == 3
    ok("③ 当月全量 ⇒ 3 行（可比 / 缺实际 / 缺标准 各一）", payload_ok,
       f"{status} total={(data_of(payload)).get('total')}")
    if not payload_ok:
        print(json.dumps(payload, ensure_ascii=False)[:1500])
        return finish()

    d = data_of(payload)
    ok("③ 批次信息回显", (d.get("batch") or {}).get("batch_no") == BATCH_NO,
       str(d.get("batch")))
    ok("③ 期间窗口 = 整月", d.get("period_start") == f"{PERIOD}-01" and d.get("period_end") == PERIOD_END,
       f"{d.get('period_start')} ~ {d.get('period_end')}")
    ok("③ 取数口径含「期间末 + 已发布」",
       "期间末" in (d.get("standard_basis") or "") and "已发布" in (d.get("standard_basis") or ""),
       d.get("standard_basis") or "")

    row_a = by_code(payload, CODE_A)
    row_b = by_code(payload, CODE_B)
    row_c = by_code(payload, CODE_C)
    ok("③④⑤ 三行都回来了", all([row_a, row_b, row_c]),
       f"A={bool(row_a)} B={bool(row_b)} C={bool(row_c)}")
    if not all([row_a, row_b, row_c]):
        return finish()

    # ③ 可比行：标准 19.5×800=15600 / 实际 16320 / 差异 720（不利）/ 率 4.62%
    var_a = row_a.get("variance") or {}
    std_expected = (float(A_STD["material"]) + float(A_STD["labor"]) + float(A_STD["overhead"])) * A_COMPLETED
    act_expected = round(A_COMPLETED * float(A_ACT["unit_material"]), 4) \
        + round(A_COMPLETED * float(A_ACT["unit_labor"]), 4) \
        + round(A_COMPLETED * float(A_ACT["unit_overhead"]), 4)
    amount_expected = act_expected - std_expected
    ok("③ 可比行状态 = BOTH + 方向 = 不利",
       row_a.get("state") == "BOTH" and var_a.get("direction") == "UNFAVORABLE"
       and var_a.get("direction_label") == "不利",
       f"{row_a.get('state')} {var_a.get('direction')}")
    ok("③ 标准应发生额 = 15600.0000", var_a.get("std_amount") == "15600.0000", var_a.get("std_amount"))
    ok("③ 实际完工成本 = 16320.0000", var_a.get("act_amount") == "16320.0000", var_a.get("act_amount"))
    ok("③ 差异额 = 720.0000（实际 − 标准）",
       var_a.get("amount") == "720.0000" and amount_expected == 720,
       f"{var_a.get('amount')} / 期望 {amount_expected}")
    ok("③ 差异率 = 0.046154（= 720/15600，比值不是百分数）",
       var_a.get("rate") == "0.046154", str(var_a.get("rate")))
    ok("③ 单位差异 = 0.900000（20.4 − 19.5）", var_a.get("unit_amount") == "0.900000",
       str(var_a.get("unit_amount")))
    ok("③ 尾差 = 0（账面完工成本与单位口径对平）", var_a.get("rounding_diff") == "0.0000",
       str(var_a.get("rounding_diff")))

    elems = {e.get("element"): e for e in (var_a.get("elements") or [])}
    ok("③ 三要素齐全（料 / 工 / 费，顺序固定）",
       [e.get("element") for e in (var_a.get("elements") or [])] == ["MATERIAL", "LABOR", "OVERHEAD"],
       str([e.get("element") for e in (var_a.get("elements") or [])]))
    ok("③ 材料差异 = 400（std 9600 / act 10000）",
       elems.get("MATERIAL", {}).get("std_amount") == "9600.0000"
       and elems.get("MATERIAL", {}).get("act_amount") == "10000.0000"
       and elems.get("MATERIAL", {}).get("amount") == "400.0000",
       json.dumps(elems.get("MATERIAL"), ensure_ascii=False)[:160])
    ok("③ 人工差异 = 320（std 4000 / act 4320）",
       elems.get("LABOR", {}).get("std_amount") == "4000.0000"
       and elems.get("LABOR", {}).get("act_amount") == "4320.0000"
       and elems.get("LABOR", {}).get("amount") == "320.0000",
       json.dumps(elems.get("LABOR"), ensure_ascii=False)[:160])
    ok("③ 制造费用差异 = 0（持平）",
       elems.get("OVERHEAD", {}).get("amount") == "0.0000"
       and elems.get("OVERHEAD", {}).get("direction") == "EVEN",
       json.dumps(elems.get("OVERHEAD"), ensure_ascii=False)[:160])
    ok("③ 要素相加 = 总差异（400 + 320 + 0 = 720）",
       sum(float(elems[k]["amount"]) for k in ("MATERIAL", "LABOR", "OVERHEAD")) == 720.0)
    ok("③ 标准一侧带版本 / 生效期（页面展开行要用）",
       (row_a.get("standard") or {}).get("version_no") == 1
       and (row_a.get("standard") or {}).get("switch_in_period") is False,
       json.dumps(row_a.get("standard"), ensure_ascii=False)[:160])

    # ④ 缺标准行
    ok("④ 缺标准行 = NO_STANDARD / 无标准无差异块",
       row_c.get("state") == "NO_STANDARD" and row_c.get("standard") is None
       and row_c.get("variance") is None, row_c.get("state"))
    ok("④ 缺标准行仍带实际完工成本（300）",
       (row_c.get("actual") or {}).get("book_amount") == "300.0000",
       (row_c.get("actual") or {}).get("book_amount"))
    ok("④ hint 指向「标准成本」页并说清怎么修",
       "标准成本" in (row_c.get("hint") or ""), (row_c.get("hint") or "")[:100])

    # ⑤ 缺实际行
    ok("⑤ 缺实际行 = NO_ACTUAL / 有标准无实际",
       row_b.get("state") == "NO_ACTUAL" and row_b.get("actual") is None
       and row_b.get("standard") is not None, row_b.get("state"))
    ok("⑤ hint 指向成本核算（本期没有计算单）",
       "成本核算" in (row_b.get("hint") or ""), (row_b.get("hint") or "")[:100])

    # ---------- ⑥ 合计 ----------
    t = totals_of(payload)
    ok("⑥ 合计计数：3 行 / 可比 1 / 缺标准 1 / 缺实际 1",
       (t.get("rows"), t.get("comparable_rows"), t.get("no_standard_rows"), t.get("no_actual_rows"))
       == (3, 1, 1, 1), json.dumps({k: t.get(k) for k in
                                    ("rows", "comparable_rows", "no_standard_rows", "no_actual_rows")}))
    ok("⑥ 合计金额只算可比行：std 15600 / act 16320 / 差异 720",
       (t.get("std_amount"), t.get("act_amount"), t.get("amount"))
       == ("15600.0000", "16320.0000", "720.0000"),
       f"{t.get('std_amount')} / {t.get('act_amount')} / {t.get('amount')}")
    ok("⑥ 缺标准行的实际金额单列（300，不混进差异）",
       t.get("no_standard_amount") == "300.0000", str(t.get("no_standard_amount")))
    ok("⑥ 合计三要素与整表同源（3 行）", len(t.get("elements") or []) == 3,
       str([e.get("element") for e in (t.get("elements") or [])]))
    ok("⑥ note 写明符号约定与「不入账」",
       "正数 = 不利" in (d.get("note") or "") and "不入账" in (d.get("note") or ""),
       (d.get("note") or "")[:100])

    # ---------- ⑦ 筛选 / 关键字 / 分页 ----------
    status, payload = get_variance(client, period_code=PERIOD, state="comparable", page=1, page_size=20)
    ok("⑦ state=comparable ⇒ 只有可比且有差异的 1 行",
       status == 200 and data_of(payload).get("total") == 1 and by_code(payload, CODE_A) is not None,
       f"{status} total={data_of(payload).get('total')}")
    status, payload = get_variance(client, period_code=PERIOD, state="no_standard", page=1, page_size=20)
    ok("⑦ state=no_standard ⇒ 1 行（C）",
       status == 200 and data_of(payload).get("total") == 1 and by_code(payload, CODE_C) is not None)
    status, payload = get_variance(client, period_code=PERIOD, state="no_actual", page=1, page_size=20)
    ok("⑦ state=no_actual ⇒ 1 行（B）",
       status == 200 and data_of(payload).get("total") == 1 and by_code(payload, CODE_B) is not None)
    status, payload = get_variance(client, period_code=PERIOD, state="variance", page=1, page_size=20)
    ok("⑦ 非法 state ⇒ 400（不静默回落 all）", status == 400, f"{status} {payload.get('msg')}")

    status, payload = get_variance(client, period_code=PERIOD, keyword=CODE_A, page=1, page_size=20)
    ok("⑦ 关键字命中 1 行", status == 200 and data_of(payload).get("total") == 1,
       f"total={data_of(payload).get('total')}")
    status, payload = get_variance(client, period_code=PERIOD, keyword="NO-SUCH-MATERIAL-XYZ", page=1, page_size=20)
    ok("⑦ 关键字无命中 ⇒ 0 行且合计全 0",
       status == 200 and data_of(payload).get("total") == 0
       and totals_of(payload).get("rows") == 0,
       f"total={data_of(payload).get('total')} rows={totals_of(payload).get('rows')}")

    status, page1 = get_variance(client, period_code=PERIOD, page=1, page_size=1)
    status2, page2 = get_variance(client, period_code=PERIOD, page=2, page_size=1)
    first_code = (rows_of(page1)[0] or {}).get("material_code") if rows_of(page1) else None
    second_code = (rows_of(page2)[0] or {}).get("material_code") if rows_of(page2) else None
    ok("⑦ page_size=1 翻页：total 恒定 3、两页不同行",
       data_of(page1).get("total") == 3 and data_of(page2).get("total") == 3
       and first_code and second_code and first_code != second_code,
       f"{first_code} → {second_code}")
    ok("⑦ 可比行排第一（报表用途 = 先看偏得最多的）", first_code == CODE_A, str(first_code))
    ok("⑦ 合计不随分页变（按筛选后全集聚合）",
       totals_of(page1).get("amount") == "720.0000" and totals_of(page2).get("amount") == "720.0000",
       f"{totals_of(page1).get('amount')} / {totals_of(page2).get('amount')}")

    # ---------- ⑧ 权限 ----------
    low = Client()
    low.login(*LOW_USER)
    status, payload = get_variance(low, period_code=PERIOD, page=1, page_size=20)
    ok("⑧ 只持 EMPLOYEE 的账号 ⇒ 403（菜单不给他，接口也拦）", status == 403,
       f"{status} {payload.get('code')}")

    # ---------- ⑨ 清场 ----------
    cleanup()
    status, payload = get_variance(client, period_code=PERIOD, page=1, page_size=20)
    ok("⑨ 清场后当月 0 行 / cost_ready=false",
       status == 200 and data_of(payload).get("total") == 0
       and data_of(payload).get("cost_ready") is False,
       f"total={data_of(payload).get('total')}")
    for table, where in (
        ("biz_cost_period", f"batch_no = '{BATCH_NO}'"),
        ("biz_cost_standard", f"remark = '{STD_REMARK}'"),
        ("biz_cost_object", f"object_no like '{OBJ_PREFIX}%'"),
    ):
        ok(f"⑨ 零残留：{table}", one(f"select count(*) from {table} where {where}") == "0",
           one(f"select count(*) from {table} where {where}"))
    return finish()


def finish() -> int:
    print(f"\n===== {len(PASS)} PASS / {len(FAIL)} FAIL =====")
    if FAIL:
        print("FAIL 列表：" + " | ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
