#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""成本巡检 C-COST-1~6 的自测（COST-B3-10）：正向全绿 + 逐条造错报红 + 清场零误报。

为什么需要这个脚本
==================
`erp_invariants.py` 的 C 组是**只读**的审计判据。只证明「它在干净库上不报警」是不够的 ——
一条永远 PASS 的检查等于没查。本脚本用一套**内部自洽的最小成本数据集**（设计 §4.7.5 算例 A
的七张凭证 + 一张直接行场景）做三件事：

  1. **正向**：装夹具 → 巡检必须零 BLOCKER / 零 WARN（证明不误报）；
  2. **负向**：逐条注入一个「只该打红这一条」的缺口 → 巡检必须正好红那一条（证明真能抓）；
  3. **清场**：删掉夹具 → 巡检回到「无已结算批次，跳过」（证明夹具不留痕）。

用法（**只许指向演练库**，脚本本身会拒绝非 drill/test 库名）
---------------------------------------------------------------
    export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \\
           NIK_DATABASE_USER=alun NIK_DATABASE_NAME=cost_inspect_drill \\
           NIK_DATABASE_PASSWORD=alun
    python3 scripts/verify/cost/cost_invariants_selftest.py --selftest   # 全流程（含夹具）
    python3 scripts/verify/cost/cost_invariants_selftest.py --apply      # 只装夹具
    python3 scripts/verify/cost/cost_invariants_selftest.py --clean      # 只清夹具
    python3 scripts/verify/cost/cost_invariants_selftest.py --assert-green  # 只跑一次巡检断言全绿

退出码：0 全部符合预期；1 有不符合预期的（打印明细）。

夹具的算术（设计 §4.7.5 算例 A，逐项对平）
-------------------------------------------
    期间 2805-11（批次 CS-280511-0001，对象 CB-INSP-0001，完工 800 / 在制 200@50%）
      领料      借 4001.01 19000.00             贷 1405 19000.00
      人工计提  借 4001.02  4000.00             贷 2211.01 4000.00
      手工台账  借 4101.01  3000.00 + 4101.02 1000.00   贷 2241.01 4000.00
      费用结转  借 4001.03  4000.00             贷 4101.01 3000.00 + 4101.02 1000.00
      完工结转  借 1405 22311.1104              贷 4001.01 15200.00 + 4001.02 3555.5552
                                                    + 4001.03 3555.5552
      ⇒ 4001.01/.02/.03 余额 = 3800.00 + 444.4448 + 444.4448 = 4688.8896 = wip_end_cost
      ⇒ 4101.01/.02 清零；Σ 分摊 4000 = Σ 池行 4000
    期间 2805-12（批次 CS-280512-0001，对象 CB-INSP-0002）—— **直接行**场景
      手工台账  借 4101.09   500.00             贷 1002 500.00      （台账挂了成本对象）
      费用结转  借 4001.03   500.00             贷 4101.09 500.00
      完工结转  借 1405      500.00             贷 4001.03 500.00
      ⇒ 直接行不进池、不参与分摊，但 4101 借方同样要靠结转凭证清零（C-COST-1 必须认它）
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
INVARIANTS = ROOT / "scripts" / "verify" / "erp_invariants.py"

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", ""),
    db=os.environ.get("NIK_DATABASE_NAME", "cloudpivot"),
)

# 夹具标记（一切可识别、可清理的常量都集中在这里）
PERIOD_A = "2805-11"
PERIOD_B = "2805-12"
PERIOD_FLIGHT = "2806-01"          # 只用于「在途」场景（不建批次）
BATCH_A = "CS-280511-0001"
BATCH_B = "CS-280512-0001"
OBJ_A = "CB-INSP-0001"
OBJ_B = "CB-INSP-0002"
OBJ_FLIGHT = "CB-INSP-9001"
OUT_NO = "INSP-OUT-0001"
IN_NO_A = "INSP-IN-0001"
IN_NO_B = "INSP-IN-0002"
IN_NO_OUTSIDE = "INSP-IN-0003"     # 在途场景用
RULE_CODE = "INSP-RULE-1"
VOUCHER_PREFIX = "INSP-"
CHECK_NAMES = {
    "C1": "C-COST-1",
    "C2": "C-COST-2 Σ",
    "C2B": "C-COST-2·账",
    "C3": "C-COST-3",
    "C4A": "C-COST-4①",
    "C4B": "C-COST-4②",
    "C5": "C-COST-5",
    "C6": "C-COST-6",
}


# ══════════════════════════════════════════════════════════════════
# 基础：psql / 巡检
# ══════════════════════════════════════════════════════════════════
def ensure_safe_db():
    """**只许在演练库上跑**：夹具会写业务表，指到 prod 就是事故。"""
    if not re.search(r"(drill|test|selftest)", DB["db"]):
        sys.exit(f"拒绝执行：{DB['db']} 不像演练库（库名须含 drill/test）。本脚本会写业务表。")
    if DB["db"] in ("cloudpivot", "cloudpivot_prod"):
        sys.exit("拒绝执行：这是生产库名。")


def psql(sql, tuples_only=True):
    env = dict(os.environ)
    env["PGPASSWORD"] = DB["password"]
    args = ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
            "-d", DB["db"], "-v", "ON_ERROR_STOP=1"]
    if tuples_only:
        args += ["-tAF", "\x1f"]
    r = subprocess.run(args + ["-c", sql], capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(f"SQL 失败：{r.stderr.strip()[:600]}\n---\n{sql[:400]}")
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def one(sql, default=""):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] else default


def run_invariants():
    """跑 C 组，返回 (exit_code, 结果列表, 原始输出)。"""
    env = dict(os.environ)
    env.update({
        "NIK_DATABASE_HOST": DB["host"], "NIK_DATABASE_PORT": str(DB["port"]),
        "NIK_DATABASE_USER": DB["user"], "NIK_DATABASE_NAME": DB["db"],
        "NIK_DATABASE_PASSWORD": DB["password"],
    })
    r = subprocess.run([sys.executable, str(INVARIANTS), "--group", "成本", "--json"],
                       capture_output=True, text=True, env=env, timeout=300)
    out = r.stdout
    payload = None
    pos = out.rfind('\n{\n')
    if pos >= 0:
        try:
            payload = json.loads(out[pos:])
        except json.JSONDecodeError:
            payload = None
    if payload is None:
        raise RuntimeError(f"巡检输出解析失败：\n{out[-2000:]}\n{r.stderr[-1000:]}")
    return r.returncode, payload["results"], out


def reds(results, level=None):
    return [r for r in results if not r["ok"] and (level is None or r["level"] == level)]


# ══════════════════════════════════════════════════════════════════
# 夹具：装 / 清
# ══════════════════════════════════════════════════════════════════
def pick_ids():
    dept = one("select id from sys_dept where deleted_at is null order by dept_code limit 1")
    if not dept:
        sys.exit("演练库没有 sys_dept，无法装夹具（先跑迁移 + 种子）")
    pairs = psql("""
        select distinct on (m.id) m.id, w.id, m.unit_id, m.material_code, w.warehouse_code
          from biz_material m cross join biz_warehouse w
         where m.deleted_at is null and w.deleted_at is null
           and not exists (select 1 from biz_inventory i
                            where i.material_id = m.id and i.warehouse_id = w.id)
           and not exists (select 1 from biz_inventory_txn t
                            where t.material_id = m.id and t.warehouse_id = w.id)
         order by m.id, w.warehouse_code
         limit 2
    """)
    if len(pairs) < 2:
        sys.exit("演练库找不到两个「无库存流水」的 物料×仓库 组合，无法装夹具")
    raw, prod = pairs[0], pairs[1]
    if raw[0] == prod[0]:
        sys.exit("需要两个不同物料（领料用原材料 / 入库用产出品）")
    acct = {}
    for code in ("4001.01", "4001.02", "4001.03", "4101.01", "4101.02", "4101.09",
                 "1405", "2211.01", "2241.01", "1002"):
        row = psql(f"select id, account_name from biz_account where account_code = '{code}' limit 1")
        if not row:
            sys.exit(f"演练库缺科目 {code}（先跑 V0377 等科目迁移）")
        acct[code] = row[0][0]
    return dict(dept=dept, raw_m=raw[0], raw_u=raw[2], raw_code=raw[3], raw_w=raw[1],
                prod_m=prod[0], prod_u=prod[2], prod_code=prod[3], prod_w=prod[1],
                acct=acct)


def _voucher_sql(ids, no, period, date, source_type, summary, entries):
    """生成「凭证头 + N 条分录」的 SQL。

    entries = [(科目码, 'D'|'C', 金额, aux 子句)]，aux 子句给出
    `aux_dept_id, aux_material_id, aux_warehouse_id` 三个值（不用辅助核算就写 "null, null, null"）。
    """
    debit = round(sum(float(a) for _, side, a, _ in entries if side == "D"), 4)
    credit = round(sum(float(a) for _, side, a, _ in entries if side == "C"), 4)
    assert abs(debit - credit) < 1e-9, f"凭证 {no} 借贷不平：{debit} vs {credit}"
    total = debit
    stmts = [f"""
        insert into biz_voucher (voucher_no, period, voucher_date, voucher_type, source_type,
                                 summary, total_debit, total_credit, entry_count, status,
                                 is_red, posted_at, created_at, updated_at)
        values ('{VOUCHER_PREFIX}{no}', '{period}', '{date}', 'AUTO', '{source_type}',
                '{summary}', {total}, {total}, {len(entries)}, 'POSTED', 0, now(), now(), now())
    """]
    for idx, (code, side, amt, aux) in enumerate(entries, start=1):
        column = "debit_amount" if side == "D" else "credit_amount"
        stmts.append(f"""
            insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                                           account_name, {column}, aux_dept_id, aux_material_id,
                                           aux_warehouse_id)
            select v.id, {idx}, '{summary}', a.id, a.account_code, a.account_name, {amt},
                   {aux}
              from biz_voucher v, biz_account a
             where v.voucher_no = '{VOUCHER_PREFIX}{no}' and a.id = '{ids['acct'][code]}'
        """)
    return stmts


def apply_fixture():
    ids = pick_ids()
    a = ids["acct"]
    stmts = []
    # ① 主数据：两个成本对象（各自的产品 = 完工入库的物料）
    stmts.append(f"""
        insert into biz_cost_object (object_no, product_id, target_warehouse_id, plan_qty,
                                     completed_qty, wip_qty, completion_pct, workshop_dept_id,
                                     workshop_dept_name, start_date, status)
        values ('{OBJ_A}', '{ids['prod_m']}', '{ids['prod_w']}', 1000, 800, 200, 50,
                '{ids['dept']}', '夹具车间A', '{PERIOD_A}-01', 'OPEN'),
               ('{OBJ_B}', '{ids['prod_m']}', '{ids['prod_w']}', 10, 5, 0, 0,
                '{ids['dept']}', '夹具车间A', '{PERIOD_B}-01', 'OPEN')
    """)
    # ② 费用台账（手工来源）：2 张池行 + 1 张直接行
    stmts.append(f"""
        insert into biz_cost_overhead (period_code, cost_object_id, dept_id, dept_name,
                                       expense_item, debit_account_code, credit_account_code,
                                       amount, source_type)
        values ('{PERIOD_A}', null, '{ids['dept']}', '夹具车间A', 'DEPRECIATION',
                '4101.01', '2241.01', 3000, 'MANUAL'),
               ('{PERIOD_A}', null, '{ids['dept']}', '夹具车间A', 'UTILITY',
                '4101.02', '2241.01', 1000, 'MANUAL'),
               ('{PERIOD_B}', (select id from biz_cost_object where object_no = '{OBJ_B}'),
                '{ids['dept']}', '夹具车间A', 'REPAIR', '4101.09', '1002', 500, 'MANUAL')
    """)
    # ③ 归集行：料 / 工 / 费池行 / 费直接行 / 分摊结果行
    stmts.append(f"""
        insert into biz_stock_out (out_no, out_type, warehouse_id, out_date, status, total_qty,
                                   total_amount, cost_object_id, posted_at)
        values ('{OUT_NO}', 'ISSUE', '{ids['raw_w']}', '{PERIOD_A}-05', 'POSTED', 1000, 19000,
                (select id from biz_cost_object where object_no = '{OBJ_A}'), now())
    """)
    stmts.append(f"""
        insert into biz_stock_out_item (stock_out_id, line_no, material_id, unit_id, qty,
                                        unit_cost, amount)
        select o.id, 1, '{ids['raw_m']}', '{ids['raw_u']}', 1000, 19, 19000
          from biz_stock_out o where o.out_no = '{OUT_NO}'
    """)
    stmts.append(f"""
        insert into biz_cost_collect (cost_object_id, period_code, element, source_type,
                                      source_id, source_no, line_no, occur_date, material_id,
                                      qty, unit_price, amount, dept_id, dept_name)
        select o.cost_object_id, '{PERIOD_A}', 'MATERIAL', 'STOCK_OUT', i.id, o.out_no, 1,
               o.out_date, i.material_id, i.qty, i.unit_cost, i.amount, '{ids['dept']}', '夹具车间A'
          from biz_stock_out o join biz_stock_out_item i on i.stock_out_id = o.id
         where o.out_no = '{OUT_NO}'
    """)
    stmts.append(f"""
        insert into biz_cost_collect (cost_object_id, period_code, element, source_type,
                                      source_id, source_no, line_no, occur_date, amount,
                                      dept_id, dept_name)
        select id, '{PERIOD_A}', 'LABOR', 'LABOR_TICKET', null, 'INSP-LABOR-1', 1,
               '{PERIOD_A}-28', 4000, '{ids['dept']}', '夹具车间A'
          from biz_cost_object where object_no = '{OBJ_A}'
    """)
    stmts.append(f"""
        insert into biz_cost_collect (cost_object_id, period_code, element, source_type,
                                      source_id, source_no, line_no, occur_date, amount,
                                      dept_id, dept_name)
        select null, '{PERIOD_A}', 'OVERHEAD', 'OVERHEAD_TICKET', oh.id,
               'INSP-OH-' || right(oh.debit_account_code, 2),
               1, '{PERIOD_A}-28', oh.amount, oh.dept_id, oh.dept_name
          from biz_cost_overhead oh
         where oh.period_code = '{PERIOD_A}' and oh.cost_object_id is null
    """)
    stmts.append(f"""
        insert into biz_cost_collect (cost_object_id, period_code, element, source_type,
                                      source_id, source_no, line_no, occur_date, amount,
                                      dept_id, dept_name)
        select oh.cost_object_id, '{PERIOD_B}', 'OVERHEAD', 'OVERHEAD_TICKET', oh.id,
               'INSP-OH-DIRECT', 1, '{PERIOD_A}-28', oh.amount, oh.dept_id, oh.dept_name
          from biz_cost_overhead oh
         where oh.period_code = '{PERIOD_B}' and oh.cost_object_id is not null
    """)
    stmts.append(f"""
        insert into biz_cost_collect (cost_object_id, period_code, element, source_type,
                                      source_no, line_no, occur_date, amount, dept_id, dept_name)
        select c.id, '{PERIOD_A}', 'OVERHEAD', 'ALLOC', 'INSP-ALLOC-1', 1, '{PERIOD_A}-28',
               4000, '{ids['dept']}', '夹具车间A'
          from biz_cost_object c where c.object_no = '{OBJ_A}'
    """)
    # ④ 分摊规则 + 分摊结果（池 4000 按两个科目摊给对象 A）
    stmts.append(f"""
        insert into biz_cost_alloc_rule (rule_code, rule_name, biz_type, source_dept_id,
                                         source_dept_name, account_code, basis, priority,
                                         effective_from, status)
        values ('{RULE_CODE}', '夹具分摊规则', 'OVERHEAD', '{ids['dept']}', '夹具车间A',
                '4101.01', 'LABOR_HOURS', 10, '{PERIOD_A}-01', 1)
    """)
    stmts.append(f"""
        insert into biz_cost_alloc_result (period_code, rule_id, cost_object_id, dept_id,
                                           dept_name, account_code, basis_value, basis_total,
                                           pool_amount, alloc_amount, rounding_adj)
        select '{PERIOD_A}', r.id, c.id, '{ids['dept']}', '夹具车间A', acc.code,
               80, 80, acc.pool, acc.pool, 0
          from biz_cost_alloc_rule r, biz_cost_object c,
               (values ('4101.01', 3000), ('4101.02', 1000)) as acc(code, pool)
         where r.rule_code = '{RULE_CODE}' and c.object_no = '{OBJ_A}'
    """)
    # ⑤ 批次 + 计算单（算例 A：完工 800 / 在制 200@50%）
    stmts.append(f"""
        insert into biz_cost_period (period_code, batch_no, status, labor_source, object_count,
                                     total_material, total_labor, total_overhead, total_cost,
                                     total_finished, total_wip, approval_instance_id,
                                     settled_at, settled_by)
        values ('{PERIOD_A}', '{BATCH_A}', 'APPROVED', 'MANUAL', 1,
                19000, 4000, 4000, 27000, 22311.1104, 4688.8896, gen_random_uuid(),
                null, null),
               ('{PERIOD_B}', '{BATCH_B}', 'APPROVED', 'MANUAL', 1,
                0, 0, 500, 500, 500, 0, gen_random_uuid(), null, null)
    """)
    stmts.append(f"""
        insert into biz_cost_period_item (period_id, cost_object_id, product_id, wip_beg_qty,
                                          wip_beg_cost, completed_qty, wip_end_qty,
                                          completion_pct, equiv_material, equiv_conversion,
                                          mat_cur, labor_cur, oh_cur, unit_material, unit_labor,
                                          unit_overhead, finished_cost, wip_end_cost,
                                          unit_finished_cost, status)
        select b.id, c.id, c.product_id, 0, 0, 800, 200, 50, 1000, 900,
               19000, 4000, 4000, 19, 4, 4, 22311.1104, 4688.8896, 27.888888, 'CALC'
          from biz_cost_period b, biz_cost_object c
         where b.batch_no = '{BATCH_A}' and c.object_no = '{OBJ_A}'
    """)
    stmts.append(f"""
        insert into biz_cost_period_item (period_id, cost_object_id, product_id, wip_beg_qty,
                                          wip_beg_cost, completed_qty, wip_end_qty,
                                          completion_pct, equiv_material, equiv_conversion,
                                          mat_cur, labor_cur, oh_cur, unit_material, unit_labor,
                                          unit_overhead, finished_cost, wip_end_cost,
                                          unit_finished_cost, status)
        select b.id, c.id, c.product_id, 0, 0, 5, 0, 0, 5, 5,
               0, 0, 500, 0, 0, 100, 500, 0, 100, 'CALC'
          from biz_cost_period b, biz_cost_object c
         where b.batch_no = '{BATCH_B}' and c.object_no = '{OBJ_B}'
    """)
    # ⑥ 完工入库（单据 + 明细 + 库存三本账）
    stmts.append(f"""
        insert into biz_stock_in (in_no, in_type, warehouse_id, in_date, status, total_qty,
                                  total_amount, total_tax, total_with_tax, cost_object_id,
                                  cost_period_id, posted_at, is_sourceless)
        values ('{IN_NO_A}', 'PRODUCTION', '{ids['prod_w']}', '{PERIOD_A}-28', 'POSTED',
                800, 22311.1104, 0, 22311.1104,
                (select id from biz_cost_object where object_no = '{OBJ_A}'),
                (select id from biz_cost_period where batch_no = '{BATCH_A}'), now(), false),
               ('{IN_NO_B}', 'PRODUCTION', '{ids['prod_w']}', '{PERIOD_B}-28', 'POSTED',
                5, 500, 0, 500,
                (select id from biz_cost_object where object_no = '{OBJ_B}'),
                (select id from biz_cost_period where batch_no = '{BATCH_B}'), now(), false)
    """)
    stmts.append(f"""
        insert into biz_stock_in_item (stock_in_id, line_no, material_id, unit_id, qty, price,
                                       amount, cost_price, with_tax_amount)
        select s.id, 1, '{ids['prod_m']}', '{ids['prod_u']}', s.total_qty, s.total_amount,
               s.total_amount, round(s.total_amount / s.total_qty, 6), s.total_amount
          from biz_stock_in s where s.in_no in ('{IN_NO_A}', '{IN_NO_B}')
    """)
    stmts.append(f"""
        insert into biz_inventory (material_id, warehouse_id, qty, locked_qty, amount, avg_cost,
                                   last_txn_at)
        values ('{ids['prod_m']}', '{ids['prod_w']}', 805, 0, 22811.1104,
                round(22811.1104 / 805, 6), now())
    """)
    stmts.append(f"""
        insert into biz_inventory_location (material_id, warehouse_id, qty, locked_qty, last_txn_at)
        values ('{ids['prod_m']}', '{ids['prod_w']}', 805, 0, now())
    """)
    stmts.append(f"""
        insert into biz_inventory_txn (material_id, warehouse_id, txn_type, biz_type, doc_type,
                                       doc_id, doc_no, line_no, qty, unit_cost, amount,
                                       after_qty, after_amount, period_code)
        select '{ids['prod_m']}', '{ids['prod_w']}', 'IN', 'PRODUCTION_IN', 'STOCK_IN', s.id,
               s.in_no, 1, s.total_qty, round(s.total_amount / s.total_qty, 6),
               s.total_amount, s.total_qty, s.total_amount,
               case when s.in_no = '{IN_NO_A}' then '{PERIOD_A}' else '{PERIOD_B}' end
          from biz_stock_in s where s.in_no in ('{IN_NO_A}', '{IN_NO_B}')
    """)
    # ⑦ 七张凭证（领料 / 人工 / 台账A / 结转A / 完工A / 台账B(直接行) / 结转B / 完工B）
    d = f"'{ids['dept']}', null, null"          # 车间辅助核算
    n = "null, null, null"                       # 无辅助核算
    pw = f"null, '{ids['prod_m']}', '{ids['prod_w']}'"   # 物料 + 仓库辅助核算
    stmts += _voucher_sql(ids, "0001", PERIOD_A, f"{PERIOD_A}-05", "STOCK_OUT", "领料-夹具", [
        ("4001.01", "D", 19000, n), ("1405", "C", 19000, n)])
    stmts += _voucher_sql(ids, "0002", PERIOD_A, f"{PERIOD_A}-28", "COST_PERIOD", "人工计提-夹具", [
        ("4001.02", "D", 4000, d), ("2211.01", "C", 4000, n)])
    stmts += _voucher_sql(ids, "0003", PERIOD_A, f"{PERIOD_A}-28", "COST_PERIOD", "手工台账-夹具", [
        ("4101.01", "D", 3000, d), ("4101.02", "D", 1000, d), ("2241.01", "C", 4000, n)])
    stmts += _voucher_sql(ids, "0004", PERIOD_A, f"{PERIOD_A}-28", "COST_PERIOD", "费用结转-夹具", [
        ("4001.03", "D", 4000, d), ("4101.01", "C", 3000, d), ("4101.02", "C", 1000, d)])
    stmts += _voucher_sql(ids, "0005", PERIOD_A, f"{PERIOD_A}-28", "COST_PERIOD", "完工结转-夹具", [
        ("1405", "D", 22311.1104, pw), ("4001.01", "C", 15200, n),
        ("4001.02", "C", 3555.5552, n), ("4001.03", "C", 3555.5552, n)])
    stmts += _voucher_sql(ids, "0006", PERIOD_B, f"{PERIOD_B}-28", "COST_PERIOD", "台账-直接行", [
        ("4101.09", "D", 500, d), ("1002", "C", 500, n)])
    stmts += _voucher_sql(ids, "0007", PERIOD_B, f"{PERIOD_B}-28", "COST_PERIOD", "费用结转B", [
        ("4001.03", "D", 500, d), ("4101.09", "C", 500, d)])
    stmts += _voucher_sql(ids, "0008", PERIOD_B, f"{PERIOD_B}-28", "COST_PERIOD", "完工结转B", [
        ("1405", "D", 500, pw), ("4001.03", "C", 500, n)])
    # 批次回写凭证 id（SETTLED 的库层 CHECK 要求 finish_voucher_id 非空）
    stmts.append(f"""
        update biz_cost_period set status = 'SETTLED', settled_at = now(),
               labor_voucher_id = (select id from biz_voucher where voucher_no = '{VOUCHER_PREFIX}0002'),
               overhead_voucher_id = (select id from biz_voucher where voucher_no = '{VOUCHER_PREFIX}0004'),
               finish_voucher_id = (select id from biz_voucher where voucher_no = '{VOUCHER_PREFIX}0005')
         where batch_no = '{BATCH_A}'
    """)
    stmts.append(f"""
        update biz_cost_period set status = 'SETTLED', settled_at = now(),
               labor_voucher_id = null,
               overhead_voucher_id = (select id from biz_voucher where voucher_no = '{VOUCHER_PREFIX}0007'),
               finish_voucher_id = (select id from biz_voucher where voucher_no = '{VOUCHER_PREFIX}0008')
         where batch_no = '{BATCH_B}'
    """)
    # 入库单挂完工凭证（§4.7.4 末段）
    stmts.append(f"""
        update biz_stock_in set voucher_id = (select id from biz_voucher
                                               where voucher_no = '{VOUCHER_PREFIX}0005')
         where in_no = '{IN_NO_A}'
    """)
    stmts.append(f"""
        update biz_stock_in set voucher_id = (select id from biz_voucher
                                               where voucher_no = '{VOUCHER_PREFIX}0008')
         where in_no = '{IN_NO_B}'
    """)

    clean_fixture(quiet=True)
    for stmt in stmts:
        psql(stmt)
    print(f"✔ 夹具已装：{PERIOD_A}/{BATCH_A}（算例 A）+ {PERIOD_B}/{BATCH_B}（直接行场景）")
    print(f"  物料：原料 {ids['raw_code']} / 产出 {ids['prod_code']}；车间 id={ids['dept']}")


def clean_fixture(quiet=False):
    """按标记清场（FK 逆序）。可重复执行。"""
    stmts = [
        # 用 4101 约束先恢复（万一上次中断在 C-COST-6 故障里）
        """do $$ begin
             if not exists (select 1 from pg_constraint
                             where conname = 'ck_biz_cost_item_cons'
                               and conrelid = 'biz_cost_period_item'::regclass) then
               alter table biz_cost_period_item add constraint ck_biz_cost_item_cons
                 check (finished_cost + wip_end_cost
                        = wip_beg_cost + mat_cur + labor_cur + oh_cur);
             end if;
           end $$""",
        f"delete from biz_inventory_txn where doc_no like 'INSP-%' or material_id in "
        f"(select material_id from biz_inventory_txn where doc_no like 'INSP-%')",
        f"delete from biz_voucher_entry where voucher_id in "
        f"(select id from biz_voucher where voucher_no like '{VOUCHER_PREFIX}%')",
        f"delete from biz_voucher where voucher_no like '{VOUCHER_PREFIX}%'",
        f"delete from biz_stock_in_item where stock_in_id in "
        f"(select id from biz_stock_in where in_no like 'INSP-%')",
        f"delete from biz_stock_in where in_no like 'INSP-%'",
        f"delete from biz_cost_alloc_result where period_code in "
        f"('{PERIOD_A}', '{PERIOD_B}', '{PERIOD_FLIGHT}')",
        f"delete from biz_cost_alloc_rule where rule_code = '{RULE_CODE}'",
        f"delete from biz_cost_period_item where period_id in "
        f"(select id from biz_cost_period where batch_no like 'CS-INSP%' "
        f" or period_code in ('{PERIOD_A}', '{PERIOD_B}', '{PERIOD_FLIGHT}'))",
        f"delete from biz_cost_collect where period_code in "
        f"('{PERIOD_A}', '{PERIOD_B}', '{PERIOD_FLIGHT}')",
        f"delete from biz_cost_overhead where period_code in ('{PERIOD_A}', '{PERIOD_B}')",
        f"delete from biz_cost_period where batch_no like 'CS-INSP%' "
        f"or period_code in ('{PERIOD_A}', '{PERIOD_B}', '{PERIOD_FLIGHT}')",
        f"delete from biz_cost_object where object_no like 'CB-INSP%'",
        f"delete from biz_stock_out_item where stock_out_id in "
        f"(select id from biz_stock_out where out_no like 'INSP-%')",
        f"delete from biz_stock_out where out_no like 'INSP-%'",
    ]
    for stmt in stmts:
        psql(stmt)
    # 库存三本账：只删夹具动过的 物料×仓库（用「夹具已不存在的入库单」反推不可靠，
    # 所以按 vtx/流水口径清：先清夹具物料的库位账与库存行，再由流水为 0 佐证）
    psql(f"""
        delete from biz_inventory_location l
         where not exists (select 1 from biz_inventory_txn t
                            where t.material_id = l.material_id
                              and t.warehouse_id = l.warehouse_id)
           and not exists (select 1 from biz_inventory i
                            where i.material_id = l.material_id
                              and i.warehouse_id = l.warehouse_id
                              and i.qty <> 0)
           and l.qty = 805
    """)
    psql("""
        delete from biz_inventory i
         where i.qty = 805 and abs(i.amount - 22811.1104) < 0.01
           and not exists (select 1 from biz_inventory_txn t
                            where t.material_id = i.material_id
                              and t.warehouse_id = i.warehouse_id)
    """)
    if not quiet:
        left = residue()
        print("✔ 清场完成，残留：" + ", ".join(f"{k}={v}" for k, v in left.items()))


def residue():
    return {
        "cost_period": int(one(f"select count(*) from biz_cost_period where period_code in "
                               f"('{PERIOD_A}','{PERIOD_B}','{PERIOD_FLIGHT}')")),
        "period_item": int(one(f"select count(*) from biz_cost_period_item i join biz_cost_period b "
                               f"on b.id=i.period_id where b.period_code in "
                               f"('{PERIOD_A}','{PERIOD_B}','{PERIOD_FLIGHT}')")),
        "collect": int(one(f"select count(*) from biz_cost_collect where period_code in "
                           f"('{PERIOD_A}','{PERIOD_B}','{PERIOD_FLIGHT}')")),
        "alloc_result": int(one(f"select count(*) from biz_cost_alloc_result where period_code in "
                                f"('{PERIOD_A}','{PERIOD_B}','{PERIOD_FLIGHT}')")),
        "overhead": int(one(f"select count(*) from biz_cost_overhead where period_code in "
                            f"('{PERIOD_A}','{PERIOD_B}')")),
        "object": int(one("select count(*) from biz_cost_object where object_no like 'CB-INSP%'")),
        "stock_in": int(one("select count(*) from biz_stock_in where in_no like 'INSP-%'")),
        "stock_out": int(one("select count(*) from biz_stock_out where out_no like 'INSP-%'")),
        "voucher": int(one(f"select count(*) from biz_voucher where voucher_no like "
                           f"'{VOUCHER_PREFIX}%'")),
        "txn": int(one("select count(*) from biz_inventory_txn where doc_no like 'INSP-%'")),
    }


# ══════════════════════════════════════════════════════════════════
# 造错场景：每条只该打红一个编号（或一个 WARN）
# ══════════════════════════════════════════════════════════════════
def _voucher_pair(no, period, summary, debit_code, debit_amt, credit_code, credit_amt, aux="null"):
    """临时造一张旁路凭证（不带 aux），返回 (sql, 反向 sql)。"""
    sql = f"""
        with v as (
            insert into biz_voucher (voucher_no, period, voucher_date, voucher_type, source_type,
                                     summary, total_debit, total_credit, entry_count, status,
                                     is_red, posted_at)
            values ('{VOUCHER_PREFIX}{no}', '{period}', '{period}-28', 'MANUAL', 'MANUAL',
                    '{summary}', {debit_amt}, {credit_amt}, 2, 'POSTED', 0, now())
            returning id
        )
        insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                                       account_name, debit_amount, credit_amount, aux_dept_id)
        select v.id, 1, '{summary}', a1.id, a1.account_code, a1.account_name, {debit_amt}, 0,
               {aux}::uuid
          from v, biz_account a1 where a1.account_code = '{debit_code}'
        union all
        select v.id, 2, '{summary}', a2.id, a2.account_code, a2.account_name, 0, {credit_amt},
               {aux}::uuid
          from v, biz_account a2 where a2.account_code = '{credit_code}'
    """
    undo = f"""
        delete from biz_voucher_entry where voucher_id in
            (select id from biz_voucher where voucher_no = '{VOUCHER_PREFIX}{no}');
        delete from biz_voucher where voucher_no = '{VOUCHER_PREFIX}{no}';
    """
    return sql, undo


def scenarios():
    """[(说明, 注入 sql, 还原 sql, 期望红的检查名前缀, 期望级别)]"""
    out = []
    ids = pick_ids()
    dept = ids["dept"]

    # ① C-COST-1：有人绕过成本模块直接记 4101（借方多出来）
    sql, undo = _voucher_pair("9001", PERIOD_A, "旁路记4101", "4101.01", 100, "2241.01", 100, f"'{dept}'")
    out.append(("C-COST-1 旁路记账：多借 4101.01 100 元", sql, undo, CHECK_NAMES["C1"], "BLOCKER"))

    # ② C-COST-2：入库单金额被改（凭证与计算单没动）
    out.append(("C-COST-2 入库金额漂移：入库单 -1 元",
                f"update biz_stock_in set total_amount = total_amount - 1, "
                f"total_with_tax = total_with_tax - 1 where in_no = '{IN_NO_A}'",
                f"update biz_stock_in set total_amount = total_amount + 1, "
                f"total_with_tax = total_with_tax + 1 where in_no = '{IN_NO_A}'",
                CHECK_NAMES["C2"], "BLOCKER"))

    # ③ C-COST-4①：4101 只有贷方多一笔（借方不变 ⇒ C-COST-1 看不到，只有清零能抓到）
    sql, undo = _voucher_pair("9002", PERIOD_A, "多贷4101", "2241.01", 200, "4101.01", 200, f"'{dept}'")
    out.append(("C-COST-4① 4101 只贷不清：多贷 4101.01 200 元", sql, undo, CHECK_NAMES["C4A"], "BLOCKER"))

    # ④ C-COST-4②：少写一行分摊结果（池没摊完，不参与任何凭证）
    out.append(("C-COST-4② 分摊少一行：删掉 4101.02 那行 1000 元",
                f"delete from biz_cost_alloc_result where period_code = '{PERIOD_A}' "
                f"and account_code = '4101.02'",
                f"""insert into biz_cost_alloc_result (period_code, rule_id, cost_object_id,
                        dept_id, dept_name, account_code, basis_value, basis_total, pool_amount,
                        alloc_amount, rounding_adj)
                    select '{PERIOD_A}', r.id, c.id, '{dept}', '夹具车间A', '4101.02',
                           80, 80, 1000, 1000, 0
                      from biz_cost_alloc_rule r, biz_cost_object c
                     where r.rule_code = '{RULE_CODE}' and c.object_no = '{OBJ_A}'""",
                CHECK_NAMES["C4B"], "BLOCKER"))

    # ⑤ C-COST-3：4001 被手工记了一笔（余额比在制多 1000）
    sql, undo = _voucher_pair("9003", PERIOD_B, "手工记4001", "4001.01", 1000, "1405", 1000)
    out.append(("C-COST-3 4001 余额漂移：手工借 4001.01 1000 元", sql, undo, CHECK_NAMES["C3"], "BLOCKER"))

    # ⑥ C-COST-2·账：完工入库的流水金额被改成 0（三本账 / 单据级都失衡）
    out.append(("C-COST-2·账 入库流水的金额被清零",
                f"update biz_inventory_txn set amount = 0 where doc_no = '{IN_NO_A}'",
                f"""update biz_inventory_txn set amount = 22311.1104
                     where doc_no = '{IN_NO_A}'""",
                CHECK_NAMES["C2B"], "BLOCKER"))

    # ⑦ C-COST-5（WARN）：料归集行被软删（漏归集，不影响账）
    out.append(("C-COST-5 料归集漏一行（WARN）",
                f"""update biz_cost_collect set deleted_at = now()
                     where period_code = '{PERIOD_A}' and element = 'MATERIAL'""",
                f"""update biz_cost_collect set deleted_at = null
                     where period_code = '{PERIOD_A}' and element = 'MATERIAL'""",
                CHECK_NAMES["C5"], "WARN"))

    # ⑧ C-COST-6：库层 CHECK 先摘掉再改库（模拟「手工改库 / 迁移跳过约束」的老数据）
    out.append(("C-COST-6 计算单被手工改库（先摘库层 CHECK）",
                f"""alter table biz_cost_period_item drop constraint ck_biz_cost_item_cons;
                    update biz_cost_period_item set wip_end_cost = wip_end_cost - 1
                     where cost_object_id = (select id from biz_cost_object
                                              where object_no = '{OBJ_A}')""",
                f"""update biz_cost_period_item set wip_end_cost = wip_end_cost + 1
                     where cost_object_id = (select id from biz_cost_object
                                              where object_no = '{OBJ_A}');
                    alter table biz_cost_period_item add constraint ck_biz_cost_item_cons
                      check (finished_cost + wip_end_cost
                             = wip_beg_cost + mat_cur + labor_cur + oh_cur)""",
                CHECK_NAMES["C6"], "BLOCKER"))
    return out


def flight_scenario():
    """在途闸：未结算期间的已领料投入**不许**被 C-COST-3 当成差错（零误报的关键一条）。"""
    ids = pick_ids()
    apply_sql = f"""
        insert into biz_cost_object (object_no, product_id, target_warehouse_id, plan_qty,
                                     completed_qty, wip_qty, workshop_dept_id,
                                     workshop_dept_name, start_date, status)
        values ('{OBJ_FLIGHT}', '{ids['prod_m']}', '{ids['prod_w']}', 100, 0, 100,
                '{ids['dept']}', '夹具车间A', '{PERIOD_FLIGHT}-01', 'OPEN');
        -- 已归集（calc 已跑）但期间未结算：借 4001.01 已经上账
        insert into biz_cost_collect (cost_object_id, period_code, element, source_type,
                                      source_no, line_no, occur_date, material_id, qty,
                                      unit_price, amount, dept_id, dept_name)
        select id, '{PERIOD_FLIGHT}', 'MATERIAL', 'STOCK_OUT', 'INSP-FLIGHT-1', 1,
               '{PERIOD_FLIGHT}-02', '{ids['raw_m']}', 10, 19, 190, '{ids['dept']}', '夹具车间A'
          from biz_cost_object where object_no = '{OBJ_FLIGHT}';
        -- 已过账、但 calc 还没跑（没有归集行）：同样已经借了 4001.01
        insert into biz_stock_out (out_no, out_type, warehouse_id, out_date, status, total_qty,
                                   total_amount, cost_object_id, posted_at)
        values ('INSP-OUT-9002', 'ISSUE', '{ids['raw_w']}', '{PERIOD_FLIGHT}-03', 'POSTED',
                10, 190, (select id from biz_cost_object where object_no = '{OBJ_FLIGHT}'), now());
        insert into biz_stock_out_item (stock_out_id, line_no, material_id, unit_id, qty,
                                        unit_cost, amount)
        select o.id, 1, '{ids['raw_m']}', '{ids['raw_u']}', 10, 19, 190
          from biz_stock_out o where o.out_no = 'INSP-OUT-9002';
    """
    sql1, _ = _voucher_pair("9004", PERIOD_FLIGHT, "在途领料A", "4001.01", 190, "1405", 190)
    sql2, _ = _voucher_pair("9005", PERIOD_FLIGHT, "在途领料B", "4001.01", 190, "1405", 190)
    apply_sql += ";" + sql1 + ";" + sql2
    undo_sql = f"""
        delete from biz_voucher_entry where voucher_id in
            (select id from biz_voucher where voucher_no in ('{VOUCHER_PREFIX}9004',
                                                             '{VOUCHER_PREFIX}9005'));
        delete from biz_voucher where voucher_no in ('{VOUCHER_PREFIX}9004',
                                                     '{VOUCHER_PREFIX}9005');
        delete from biz_stock_out_item where stock_out_id in
            (select id from biz_stock_out where out_no = 'INSP-OUT-9002');
        delete from biz_stock_out where out_no = 'INSP-OUT-9002';
        delete from biz_cost_collect where period_code = '{PERIOD_FLIGHT}';
        delete from biz_cost_object where object_no = '{OBJ_FLIGHT}';
    """
    return ("在途闸：未结算期间的已领料投入（已归集 190 + 未归集 190）不计为差错",
            apply_sql, undo_sql)


def reverse_scenario():
    """红冲未重结：批次 A 全部凭证红冲 + 批次 REVERSED + 入库单作废 —— 巡检必须仍全绿。

    这是生产上很常见的中间态（红冲后当天没来得及重结）。领料凭证**不**随结算红冲
    （它来自出库单过账），所以 4001.01 仍挂 19000 —— 正好该被 C-COST-3 的在途项解释掉。
    漏了在途，这条会天天假红；这正是本场景存在的意义。
    """
    apply_sql = """
        insert into biz_voucher (voucher_no, period, voucher_date, voucher_type, source_type,
                                 source_id, summary, total_debit, total_credit, entry_count,
                                 status, is_red, reverse_of, posted_at)
        select 'INSP-R' || right(v.voucher_no, 4), v.period, v.voucher_date, v.voucher_type,
               v.source_type, v.source_id, '红冲 ' || v.voucher_no, v.total_debit,
               v.total_credit, v.entry_count, 'POSTED', 1, v.id, now()
          from biz_voucher v
         where v.voucher_no in ('INSP-0002', 'INSP-0003', 'INSP-0004', 'INSP-0005');
        insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                                       account_name, debit_amount, credit_amount, aux_dept_id,
                                       aux_material_id, aux_warehouse_id)
        select r.id, e.line_no, '红冲 ' || e.summary, e.account_id, e.account_code,
               e.account_name, e.credit_amount, e.debit_amount, e.aux_dept_id,
               e.aux_material_id, e.aux_warehouse_id
          from biz_voucher_entry e
          join biz_voucher o on o.id = e.voucher_id
          join biz_voucher r on r.reverse_of = o.id and r.is_red = 1
         where o.voucher_no in ('INSP-0002', 'INSP-0003', 'INSP-0004', 'INSP-0005');
        update biz_voucher o set reversed_by = r.id
          from biz_voucher r
         where r.reverse_of = o.id and r.is_red = 1 and o.reversed_by is null;
        update biz_cost_period set status = 'REVERSED', reversed_at = now(),
               reverse_reason = 'B3-10 自测：红冲后未重结'
         where batch_no = 'CS-280511-0001';
        update biz_stock_in set status = 'CANCELLED', cancelled_at = now(),
               cancel_reason = '红冲'
         where in_no = 'INSP-IN-0001';
    """
    undo_sql = """
        update biz_voucher set reversed_by = null
         where voucher_no in ('INSP-0002', 'INSP-0003', 'INSP-0004', 'INSP-0005');
        delete from biz_voucher_entry where voucher_id in
            (select id from biz_voucher where voucher_no like 'INSP-R%');
        delete from biz_voucher where voucher_no like 'INSP-R%';
        update biz_cost_period set status = 'SETTLED', reversed_at = null,
               reverse_reason = null
         where batch_no = 'CS-280511-0001';
        update biz_stock_in set status = 'POSTED', cancelled_at = null, cancel_reason = null
         where in_no = 'INSP-IN-0001';
    """
    return ("红冲未重结：四张凭证全红冲 + 批次 REVERSED + 入库单作废（在途 19000 解释余额）",
            apply_sql, undo_sql)


# ══════════════════════════════════════════════════════════════════
# 主流程
# ══════════════════════════════════════════════════════════════════
def expect_green(label, allow_warn=False):
    code, results, out = run_invariants()
    blockers = reds(results, "BLOCKER")
    warns = reds(results, "WARN")
    names = [r["name"] for r in results]
    ok = code == 0 and not blockers and (allow_warn or not warns)
    print(f"  {'✅' if ok else '🚨'} {label}：{len(results)} 条检查 / BLOCKER {len(blockers)}"
          f" / WARN {len(warns)}" + ("" if ok else f"  → 红：{[r['name'] for r in blockers + warns]}"))
    if not ok:
        for r in blockers + warns:
            print(f"      · [{r['level']}] {r['name']}: {r['detail'][:400]}")
    for need in (CHECK_NAMES["C1"], CHECK_NAMES["C2"], CHECK_NAMES["C2B"], CHECK_NAMES["C3"],
                 CHECK_NAMES["C4A"], CHECK_NAMES["C4B"], CHECK_NAMES["C5"], CHECK_NAMES["C6"]):
        if not any(n.startswith(need) for n in names):
            print(f"  🚨 缺少检查项 {need}")
            ok = False
    return ok


def static_sync_check():
    """静态同步闸：巡检里的科目 / 编号常量必须与后端源码逐字一致（不连库）。

    为什么要有它：`4101.` 前缀、`4001.01/.02/.03`、`1405` 这几个值在
    `cost_settle_voucher.rs`（写凭证的一方）和 `erp_invariants.py`（判账的一方）各有一份。
    任何一边改了、另一边没跟，巡检就会拿旧科目去对账 —— 那种「永远 PASS」的假绿比红更危险
    （E4 的 biz_type 白名单就是这么漏过三次的，见 erp_invariants_e4_regression.py）。
    """
    print("\n[0] 静态同步：巡检常量 ↔ 后端常量（不连库）")
    backing = ROOT / "backend/src/business/erp/cost_settle_voucher.rs"
    run_src = ROOT / "backend/src/business/erp/cost_settle_run.rs"
    inv_src = INVARIANTS
    failures = 0
    if not backing.exists() or not run_src.exists():
        print("  ⚠️ 找不到后端源码，跳过静态同步（只跑库内自测）")
        return 0
    rust = backing.read_text(encoding="utf-8") + run_src.read_text(encoding="utf-8")
    py = inv_src.read_text(encoding="utf-8")

    def rust_const(name):
        m = re.search(rf'pub const {name}: &str = "([^"]+)";', rust)
        return m.group(1) if m else None

    pairs = [
        ("4001.01", "ACCOUNT_MATERIAL"),
        ("4001.02", "ACCOUNT_LABOR"),
        ("4001.03", "ACCOUNT_OVERHEAD"),
        ("1405", "ACCOUNT_FINISHED_GOODS"),
    ]
    for expected, name in pairs:
        actual = rust_const(name)
        py_declared = expected in py
        ok = actual == expected and py_declared
        print(f"  {'✅' if ok else '🚨'} {name} = {actual}（巡检硬编码 {expected}：{py_declared}）")
        failures += 0 if ok else 1
    # 4101 前缀（制造费用末级）—— 后端在 cost_settle_run::PREFIX_OVERHEAD
    m = re.search(r'pub const PREFIX_OVERHEAD: &str = "([^"]+)";', rust)
    prefix = m.group(1) if m else None
    ok = prefix == "4101." and 'COST_OVERHEAD_PREFIX = "4101."' in py
    print(f"  {'✅' if ok else '🚨'} PREFIX_OVERHEAD = {prefix}（巡检 COST_OVERHEAD_PREFIX = 4101.）")
    failures += 0 if ok else 1
    # 6 条编号：C-COST-1~4 两侧都有（后端事务内自检 + 巡检，同一判据两个时间点）；
    # C-COST-5/6 是**巡检独有**（5 是 WARN 的料归集完整性，只影响成本准确性、结算不许拦；
    # 6 由库层 `ck_biz_cost_item_cons` 在写入时拦死，事务内无需再判一遍）—— 只要求巡检有。
    for ident, both_sides in (("C-COST-1", True), ("C-COST-2", True), ("C-COST-3", True),
                              ("C-COST-4", True), ("C-COST-5", False), ("C-COST-6", False)):
        ok = ident in py and (ident in rust or not both_sides)
        scope = "两侧" if both_sides else "巡检独有（设计 §4.11：结算不拦）"
        print(f"  {'✅' if ok else '🚨'} 编号 {ident}（{scope}）")
        failures += 0 if ok else 1
    print(f"  静态同步：{'通过' if failures == 0 else f'{failures} 处不一致'}")
    return failures


def selftest():
    ensure_safe_db()
    print("=" * 92)
    print(f"成本巡检自测（C-COST-1~6） 库 = {DB['db']}")
    print("=" * 92)
    failures = static_sync_check()

    clean_fixture(quiet=True)
    apply_fixture()

    print("\n[1] 正向：夹具装好后巡检必须全绿（零误报）")
    if not expect_green("正向全绿"):
        failures += 1

    print("\n[2] 在途闸：未结算期间的已领料投入不许被当成差错")
    desc, sql, undo = flight_scenario()
    for stmt in filter(None, (s.strip() for s in sql.split(";"))):
        psql(stmt)
    if not expect_green(desc):
        failures += 1
    for stmt in filter(None, (s.strip() for s in undo.split(";"))):
        psql(stmt)

    print("\n[2b] 红冲未重结：批次已红冲、还没重结 —— 巡检必须仍全绿（在途项解释余额）")
    desc, sql, undo = reverse_scenario()
    for stmt in filter(None, (s.strip() for s in sql.split(";"))):
        psql(stmt)
    if not expect_green(desc):
        failures += 1
    for stmt in filter(None, (s.strip() for s in undo.split(";"))):
        psql(stmt)

    print("\n[3] 负向：逐条注入缺口，巡检必须正好红那一条")
    for idx, (desc, sql, undo, expected, level) in enumerate(scenarios(), start=1):
        for stmt in filter(None, (s.strip() for s in sql.split(";"))):
            psql(stmt)
        code, results, out = run_invariants()
        hit = [r for r in reds(results, level) if r["name"].startswith(expected)]
        others = [r for r in reds(results) if not r["name"].startswith(expected)]
        ok = bool(hit) and code != 0 or (level == "WARN" and hit and code == 0)
        print(f"  {'✅' if ok else '🚨'} #{idx} {desc}")
        print(f"      期望红：{expected}（{level}）→ 命中 {len(hit)} 条；同时红的其它检查："
              f"{[r['name'][:28] for r in others] if others else '无'}")
        if hit:
            print(f"      报文：{hit[0]['detail'][:260]}")
        if not ok:
            failures += 1
        for stmt in filter(None, (s.strip() for s in undo.split(";"))):
            psql(stmt)

    print("\n[4] 还原后必须回到全绿（证明造错可逆、不污染判定）")
    if not expect_green("造错全部还原后"):
        failures += 1

    print("\n[5] 清场：夹具删掉后巡检回到「无已结算批次，跳过」")
    clean_fixture()
    left = residue()
    if any(v for v in left.values()):
        print(f"  🚨 夹具残留：{left}")
        failures += 1
    else:
        print("  ✅ 残留全 0")
    code, results, out = run_invariants()
    skip = len(results) == 1 and results[0]["name"].startswith("C-COST-0")
    print(f"  {'✅' if code == 0 and skip else '🚨'} 清场后巡检：{len(results)} 条，"
          f"{results[0]['name'] if results else '-'}")
    if not (code == 0 and skip):
        failures += 1

    print("\n" + "=" * 92)
    print(f"自测结论：{'全部符合预期 ✅' if failures == 0 else f'{failures} 项不符合预期 🚨'}")
    print("=" * 92)
    return 0 if failures == 0 else 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--clean", action="store_true")
    ap.add_argument("--assert-green", action="store_true")
    ap.add_argument("--residue", action="store_true")
    args = ap.parse_args()
    if args.apply:
        ensure_safe_db()
        apply_fixture()
    elif args.clean:
        ensure_safe_db()
        clean_fixture()
    elif args.assert_green:
        ensure_safe_db()
        sys.exit(0 if expect_green("巡检全绿") else 1)
    elif args.residue:
        ensure_safe_db()
        print(residue())
    elif args.selftest:
        sys.exit(selftest())
    else:
        ap.print_help()


if __name__ == "__main__":
    main()
