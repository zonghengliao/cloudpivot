#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""成本核算端到端演练（COST-B4-5 · ARCH-9 §7.4 第 4 批门槛）。

一条命令跑完整链路：**calc → 审 → 结算 → 对平 → 红冲 → 重算 → 再结算**，
每步都对**库里的真实数据**做断言（金额 / 凭证 / 库存三本账），失败时打印
`[FAIL] STEP n` + 期望 vs 实际。

由 `scripts/verify/cost_e2e.sh` 调用（它为演练起一个隔离实例 + 隔离库）。
也可以直接跑：见 `scripts/verify/cost_e2e.sh --help`。

# 判据来源（不看接口自报）

| 步 | 断言 | 数据来源 |
|---|---|---|
| 1 calc | 料 19000 / 工 4000 / 费 4000 / 完工 22311.1104 / 在制 4688.8896 | `biz_cost_collect`、`biz_cost_alloc_result`、`biz_cost_period_item` |
| 2 审 | 批次 SUBMITTED → APPROVED，审批实例两级留痕 | `biz_cost_period`、`biz_flow_instance`、`biz_flow_task` |
| 3 结算 | 生产入库单 POSTED + 三本账一致；3 张凭证 POSTED、借贷各自平衡 | `biz_stock_in(_item)`、`biz_inventory(_location/_txn)`、`biz_voucher(_entry)` |
| 4 对平 | `erp_invariants.py --group 成本` exit 0（C-COST-1~6 全绿） | 巡检脚本（只读） |
| 5 红冲 | 凭证红冲（原凭证 reversed_by + 红字 POSTED）、入库单 CANCELLED、库存回零、批次 REVERSED | 同上 |
| 6 重算 | 同期间第二张活跃批次能建（**排除 REVERSED 的唯一键生效**）、再算金额与第一轮一致 | `biz_cost_period`、`biz_cost_period_item` |
| 7 再结算 | 第二轮结算成功、库存/凭证回到第一轮的数、第三张活跃批次被唯一键拒绝 | 同上 |

# 夹具（输入侧）

演练要证明的是「结算链」，上游单据用 SQL 夹具（前缀 `E2E-`，`[FIXTURE]` 标注）：
成本对象 1 个、领料单 1 张（含 `4001.01` 领料凭证）、工时台账 1 条、手工费用台账 2 条、
分摊规则 2 条。**账务产物（三张结算凭证、生产入库单、库存三本账、红冲红字）全部由
真实接口产生**，不使用夹具。
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
BACKEND = ROOT / "backend"

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "alun"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "alun"),
    name=os.environ.get("NIK_DATABASE_NAME", "cost_e2e_drill"),
)
BASE = os.environ.get("COST_E2E_BASE", "http://127.0.0.1:18140") + "/api"
ADMIN = (os.environ.get("COST_E2E_ADMIN", "admin"),
         os.environ.get("COST_E2E_ADMIN_PASSWORD", "Admin@123456"))

PERIOD = os.environ.get("COST_E2E_PERIOD", time.strftime("%Y-%m"))
PREFIX = "E2E"
OBJ_NO = f"{PREFIX}-CB-0001"
OUT_NO = f"{PREFIX}-OUT-0001"
RULE_1 = f"{PREFIX}-RULE-01"
RULE_2 = f"{PREFIX}-RULE-02"
VOUCHER_NO = f"{PREFIX}-V-OUT-0001"
INJECT_VOUCHER = f"{PREFIX}-V-INJ-0001"
# 造错模式（`--inject bypass4101|drop_alloc`）：验证「失败可定位 + 修好能重试」
INJECT = os.environ.get("COST_E2E_INJECT", "").strip()

# 算例 A（设计 §4.7.5）：材料 19000 / 人工 4000 / 费用 4000 / 完工 22311.1104 / 在制 4688.8896
EXPECT = dict(material="19000.0000", labor="4000.0000", overhead="4000.0000",
              cost="27000.0000", finished="22311.1104", wip="4688.8896",
              qty_completed="800", unit_finished="27.888888")

PASSED: list[str] = []
FAILED: list[str] = []
# 夹具主数据 id（装夹具时填；造错模式的修复动作也要用）
IDS: dict = {}


# ══════════════════════════════════════════════════════════════════
# 库
# ══════════════════════════════════════════════════════════════════
def ensure_safe_db() -> None:
    """演练库才允许写（脚本会写业务表 + 造凭证，指到生产就是事故）。"""
    if not re.search(r"(drill|test|e2e)", DB["name"]):
        sys.exit(f"拒绝执行：库名 {DB['name']} 不像演练库（须含 drill/test/e2e）")
    if DB["name"] in ("cloudpivot", "cloudpivot_prod"):
        sys.exit("拒绝执行：这是生产库名")


def q(sql: str) -> list[list[str]]:
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
         "-d", DB["name"], "-v", "ON_ERROR_STOP=1", "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(f"SQL 失败：{r.stderr.strip()[:800]}\n---\n{sql[:600]}")
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def one(sql: str, default: str = "") -> str:
    rows = q(sql)
    return rows[0][0] if rows and rows[0] else default


def scalar(sql: str) -> str:
    return one(sql, "")


# ══════════════════════════════════════════════════════════════════
# 断言
# ══════════════════════════════════════════════════════════════════
STEP = {"no": 0, "name": ""}


def step(title: str) -> None:
    STEP["no"] += 1
    STEP["name"] = title
    print(f"\n{'=' * 86}\nSTEP {STEP['no']}  {title}\n{'=' * 86}")


def ok(name: str, condition: bool, expect: str = "", actual: str = "") -> bool:
    if condition:
        PASSED.append(name)
        print(f"  ✅ {name}" + (f"（{actual}）" if actual else ""))
        return True
    FAILED.append(name)
    print(f"  🚨 [FAIL] STEP {STEP['no']}「{STEP['name']}」{name}")
    if expect or actual:
        print(f"       期望：{expect or '—'}")
        print(f"       实际：{actual or '—'}")
    return False


def eq(name: str, expect: str, actual: str) -> bool:
    a = (actual or "").strip()
    e = (expect or "").strip()
    # 金额：数值比较（19000 与 19000.0000 视为相等）
    try:
        same = abs(float(a) - float(e)) < 1e-6
    except ValueError:
        same = a == e
    return ok(name, same, e, a)


# ══════════════════════════════════════════════════════════════════
# HTTP
# ══════════════════════════════════════════════════════════════════
class Client:
    def __init__(self, base: str):
        self.base = base
        self.token = ""
        self.last = None

    def call(self, method: str, path: str, body=None):
        url = self.base + path
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Content-Type", "application/json")
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(req, timeout=90) as resp:
                payload = json.loads(resp.read().decode())
                status = resp.status
        except urllib.error.HTTPError as err:
            raw = err.read().decode()
            try:
                payload = json.loads(raw)
            except Exception:
                payload = {"raw": raw}
            status = err.code
        self.last = (status, payload)
        return status, payload

    def login(self, user: str, pwd: str) -> None:
        status, payload = self.call("POST", "/system/auth/login",
                                    {"username": user, "password": pwd})
        if status != 200:
            raise SystemExit(f"登录失败（{user}）：{status} {payload}")
        data = payload.get("data") or {}
        self.token = data.get("access_token") or data.get("token") or ""
        if not self.token:
            raise SystemExit(f"登录回包没有 token：{payload}")


def data_of(payload) -> dict:
    return (payload or {}).get("data") or {}


def err_of(payload) -> str:
    p = payload or {}
    return f"code={p.get('code')} msg={p.get('msg') or p.get('message')}"


# ══════════════════════════════════════════════════════════════════
# 夹具（输入侧，SQL）
# ══════════════════════════════════════════════════════════════════
def pick_master() -> dict:
    dept = one("select id from sys_dept where deleted_at is null order by dept_code limit 1")
    if not dept:
        sys.exit("演练库没有 sys_dept（迁移种子缺失）")
    raw = q("""select m.id, m.unit_id, m.material_code from biz_material m
                where m.deleted_at is null and m.material_type = 'RAW'
                order by m.material_code limit 1""")
    fin = q("""select m.id, m.unit_id, m.material_code from biz_material m
                where m.deleted_at is null and m.material_type = 'FINISHED'
                order by m.material_code limit 1""")
    wh = q("""select id, warehouse_code from biz_warehouse
               where deleted_at is null order by warehouse_code limit 1""")
    if not raw or not fin or not wh:
        sys.exit("演练库缺 物料（RAW/FINISHED）或 仓库 种子")
    acct = {}
    for code in ("4001.01", "4101.01", "4101.02", "1405", "2241.01"):
        row = q(f"select id from biz_account where account_code = '{code}' limit 1")
        if not row:
            sys.exit(f"演练库缺科目 {code}（V0375 等科目迁移没跑？）")
        acct[code] = row[0][0]
    return dict(dept=dept, raw=raw[0], fin=fin[0], wh=wh[0], acct=acct)


def clean_fixture(quiet: bool = True) -> None:
    """按 `E2E-` 标记 + 演练期间清场（FK 逆序，可重复执行）。"""
    periods = (f"(select id from biz_cost_period where period_code = '{PERIOD}')")
    stmts = [
        f"delete from biz_inventory_txn where doc_no like '{PREFIX}-%'"
        f" or doc_id in (select id from biz_stock_in where in_no like '{PREFIX}-%'"
        f"    or cost_period_id in {periods})",
        f"delete from biz_voucher_entry where voucher_id in (select id from biz_voucher"
        f" where voucher_no like '{PREFIX}-%' or (source_type = 'COST_PERIOD'"
        f"   and source_id in {periods}))",
        f"delete from biz_voucher where voucher_no like '{PREFIX}-%'"
        f" or (source_type = 'COST_PERIOD' and source_id in {periods})",
        f"delete from biz_stock_in_item where stock_in_id in (select id from biz_stock_in"
        f" where in_no like '{PREFIX}-%' or cost_period_id in {periods})",
        f"delete from biz_status_log where entity_id in (select id from biz_stock_in"
        f" where in_no like '{PREFIX}-%' or cost_period_id in {periods})",
        f"delete from biz_status_log where entity_id in {periods}",
        f"delete from biz_stock_in where in_no like '{PREFIX}-%' or cost_period_id in {periods}",
        f"delete from biz_stock_out_item where stock_out_id in (select id from biz_stock_out"
        f" where out_no like '{PREFIX}-%')",
        f"delete from biz_stock_out where out_no like '{PREFIX}-%'",
        f"delete from biz_cost_alloc_result where rule_id in (select id from biz_cost_alloc_rule"
        f" where rule_code like '{PREFIX}-%') or period_code = '{PERIOD}'",
        f"delete from biz_cost_alloc_rule where rule_code like '{PREFIX}-%'",
        f"delete from biz_flow_task where instance_id in (select approval_instance_id"
        f" from biz_cost_period where period_code = '{PERIOD}' and approval_instance_id is not null)",
        f"delete from biz_cost_period_item where period_id in {periods}",
        f"delete from biz_cost_collect where period_code = '{PERIOD}'",
        f"delete from biz_cost_labor where employee_name like '{PREFIX}%'",
        f"delete from biz_cost_overhead where remark like '{PREFIX}%'",
        f"delete from biz_flow_instance where biz_type = 'COST_SETTLE' and biz_id in {periods}",
        f"delete from biz_cost_period where period_code = '{PERIOD}'",
        f"delete from biz_cost_object where object_no like '{PREFIX}-%'",
        f"delete from biz_inventory_location where material_id in (select id from biz_material"
        f" where material_code like '{PREFIX}%')",
        f"delete from biz_inventory where material_id in (select id from biz_material"
        f" where material_code like '{PREFIX}%')",
        f"delete from biz_material where material_code like '{PREFIX}%'",
        f"delete from sys_dept where dept_code like '{PREFIX}-%'",
        f"delete from sys_user_role where user_id in (select id from sys_user where username like 'e2e_%')",
        f"delete from sys_user where username like 'e2e_%'",
    ]
    for stmt in stmts:
        try:
            q(stmt)
        except RuntimeError as error:
            if not quiet:
                print(f"    （清场跳过一条：{str(error).splitlines()[0][:120]}）")
    if not quiet:
        print("✔ 夹具已清场")


def insert_material_voucher(ids: dict) -> None:
    """领料凭证（借 4001.01 19000 / 贷 1405 19000）——[FIXTURE]，也是造错模式的修复动作。"""
    q(f"""insert into biz_voucher (voucher_no, period, voucher_date, voucher_type, source_type,
                                    summary, total_debit, total_credit, entry_count, status,
                                    is_red, posted_at)
          values ('{VOUCHER_NO}', '{PERIOD}', '{PERIOD}-05', 'AUTO', 'STOCK_OUT',
                  '{PREFIX} 领料', 19000, 19000, 2, 'POSTED', 0, now())""")
    for line, (code, column, amount) in enumerate(
            (("4001.01", "debit_amount", 19000), ("1405", "credit_amount", 19000)), start=1):
        q(f"""insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                                             account_name, {column})
              select v.id, {line}, '{PREFIX} 领料', a.id, '{code}', a.account_name, {amount}
                from biz_voucher v, biz_account a
               where v.voucher_no = '{VOUCHER_NO}' and a.id = '{ids['acct'][code]}'""")


def apply_fixture(ids: dict) -> dict:
    """装输入侧夹具：车间 / 成本对象 / 领料单 + 领料凭证 / 工时 / 费用台账 / 分摊规则。"""
    dept_id = ids["dept"]
    q(f"""insert into sys_dept (id, dept_code, dept_name, status, sort_order)
          values (gen_random_uuid(), '{PREFIX}-DEPT-1', '{PREFIX} 演练车间', 1, 900)""")
    dept_id = one(f"select id from sys_dept where dept_code = '{PREFIX}-DEPT-1'")
    ids["dept"] = dept_id

    # 成本对象（完工 800 / 在制 200 @ 50% —— 设计 §4.7.5 算例 A）
    q(f"""insert into biz_cost_object (object_no, object_type, product_id, target_warehouse_id,
                                       plan_qty, completed_qty, wip_qty, completion_pct,
                                       material_input_mode, workshop_dept_id, workshop_dept_name,
                                       start_date, status)
          values ('{OBJ_NO}', 'BATCH', '{ids['fin'][0]}', '{ids['wh'][0]}',
                  1000, 800, 200, 50, 'ONE_TIME', '{dept_id}', '{PREFIX} 演练车间',
                  '{PERIOD}-01', 'OPEN')""")
    obj_id = one(f"select id from biz_cost_object where object_no = '{OBJ_NO}'")
    ids["obj"] = obj_id

    # 领料单（POSTED）+ 明细 + 领料凭证（借 4001.01 / 贷 1405）—— [FIXTURE]
    q(f"""insert into biz_stock_out (out_no, out_type, warehouse_id, out_date, status,
                                     total_qty, total_amount, cost_object_id, posted_at)
          values ('{OUT_NO}', 'ISSUE', '{ids['wh'][0]}', '{PERIOD}-05', 'POSTED',
                  1000, 19000, '{obj_id}', now())""")
    q(f"""insert into biz_stock_out_item (stock_out_id, line_no, material_id, unit_id, qty,
                                          unit_cost, amount)
          select o.id, 1, '{ids['raw'][0]}', '{ids['raw'][1]}', 1000, 19, 19000
            from biz_stock_out o where o.out_no = '{OUT_NO}'""")
    insert_material_voucher(ids)

    # 工时台账（已确认）：100 小时 × 40 元 = 4000
    q(f"""insert into biz_cost_labor (period_code, work_date, cost_object_id, employee_name,
                                      dept_id, dept_name, hours, machine_hours, rate, amount,
                                      source_type, status)
          values ('{PERIOD}', '{PERIOD}-15', '{obj_id}', '{PREFIX}张三',
                  '{dept_id}', '{PREFIX} 演练车间', 100, 0, 40, 4000, 'MANUAL', 'CONFIRMED')""")

    # 手工费用台账（两路池行：4101.01 3000 / 4101.02 1000）
    q(f"""insert into biz_cost_overhead (period_code, cost_object_id, dept_id, dept_name,
                                         expense_item, debit_account_code, credit_account_code,
                                         amount, source_type, remark)
          values ('{PERIOD}', null, '{dept_id}', '{PREFIX} 演练车间', 'DEPRECIATION',
                  '4101.01', '2241.01', 3000, 'MANUAL', '{PREFIX} 折旧'),
                 ('{PERIOD}', null, '{dept_id}', '{PREFIX} 演练车间', 'UTILITY',
                  '4101.02', '2241.01', 1000, 'MANUAL', '{PREFIX} 水电')""")

    # 分摊规则（两路池各一条，按人工工时）
    q(f"""insert into biz_cost_alloc_rule (rule_code, rule_name, biz_type, source_dept_id,
                                           source_dept_name, account_code, basis, priority,
                                           effective_from, status)
          values ('{RULE_1}', '{PREFIX} 折旧分摊', 'OVERHEAD', '{dept_id}',
                  '{PREFIX} 演练车间', '4101.01', 'LABOR_HOURS', 10, '{PERIOD}-01', 1),
                 ('{RULE_2}', '{PREFIX} 水电分摊', 'OVERHEAD', '{dept_id}',
                  '{PREFIX} 演练车间', '4101.02', 'LABOR_HOURS', 10, '{PERIOD}-01', 1)""")
    return ids


# ══════════════════════════════════════════════════════════════════
# calc：六段流水线的真实代码（example 探针）
# ══════════════════════════════════════════════════════════════════
def run_calc() -> str:
    env = dict(os.environ)
    env.update({
        "NIK_DATABASE_HOST": DB["host"], "NIK_DATABASE_PORT": str(DB["port"]),
        "NIK_DATABASE_USER": DB["user"], "NIK_DATABASE_PASSWORD": DB["password"],
        "NIK_DATABASE_NAME": DB["name"],
    })
    r = subprocess.run(
        ["cargo", "run", "--quiet", "--example", "cost_calc_probe", "--", PERIOD],
        cwd=str(BACKEND), capture_output=True, text=True, env=env, timeout=900)
    out = (r.stdout or "").strip()
    for line in out.splitlines():
        print(f"    {line[:220]}")
    if r.returncode != 0:
        print((r.stderr or "")[-3000:])
        raise SystemExit(f"calc 探针失败（exit {r.returncode}）")
    marker = next((l for l in out.splitlines() if l.startswith("CALC_OK ")), "")
    if not marker:
        raise SystemExit("calc 探针没有 CALC_OK 输出")
    return marker


def ensure_approvers() -> None:
    """造两个审批人账号（复用 admin 的口令哈希）。

    为什么需要：`guard_self_approval` 拒绝制单人批自己的单（内控），所以制单（admin）之外
    必须有人在岗。两人同时挂 `FINANCE_MANAGER` / `FINANCE_DIRECTOR`（流程按角色派单）
    与 `SUPER_ADMIN`（`business:flow:approve` 的菜单授权不该决定本链路的成败）。
    """
    for user, real, role in (("e2e_fm", f"{PREFIX} 财务经理", "FINANCE_MANAGER"),
                             ("e2e_fd", f"{PREFIX} 财务总监", "FINANCE_DIRECTOR")):
        q(f"""insert into sys_user (id, username, password, real_name, status,
                                     must_change_password, token_version, user_type, remark)
              select gen_random_uuid(), '{user}', u.password, '{real}', 1, false, 1, 'HUMAN',
                     'COST-B4-5 演练审批人'
                from sys_user u
               where u.username = 'admin'
                 and not exists (select 1 from sys_user where username = '{user}')""")
        q(f"""insert into sys_user_role (id, user_id, role_id)
              select gen_random_uuid(), u.id, r.id
                from sys_user u join sys_role r on r.deleted_at is null
               where u.username = '{user}' and r.role_code in ('{role}', 'SUPER_ADMIN')
                 and not exists (select 1 from sys_user_role x
                                  where x.user_id = u.id and x.role_id = r.id)""")


def net_balance(code: str) -> str:
    """期间内某科目的净发生（含红字；用于「红冲后应回到 0」的断言）。"""
    return scalar(f"""select (coalesce(sum(e.debit_amount),0) - coalesce(sum(e.credit_amount),0))::text
                        from biz_voucher_entry e join biz_voucher v on v.id = e.voucher_id
                       where e.account_code = '{code}' and v.period = '{PERIOD}'
                         and v.status = 'POSTED'""")


def submit_and_approve(cli: Client, period_id: str, tag: str = "") -> tuple[bool, str]:
    """提交 + 两级审批（财务经理 → 财务总监）。返回（是否已 APPROVED，审批实例 id）。"""
    status, payload = cli.call("POST", f"/business/erp/cost-period/{period_id}/submit")
    if status != 200:
        return False, f"提交失败 {status} {err_of(payload)}"
    instance = scalar(
        f"select coalesce(approval_instance_id::text,'') from biz_cost_period where id = '{period_id}'")
    for user in ("e2e_fm", "e2e_fd"):
        approver = Client(BASE)
        approver.login(user, ADMIN[1])
        me = scalar(f"select id from sys_user where username = '{user}'")
        tasks = [r[0] for r in q(
            f"select id from biz_flow_task where instance_id = '{instance}'"
            f" and assignee_id = '{me}' and status = 'PENDING' order by created_at")]
        for task in tasks:
            approver.call("POST", f"/business/flow/task/{task}/approve",
                          {"comment": f"演练{tag}同意"})
    return (scalar(f"select status from biz_cost_period where id = '{period_id}'") == "APPROVED"), instance


def inject_break(kind: str) -> None:
    """按场景注入一个「只该被某一条断言抓住」的缺口（造错）。"""
    if kind == "bypass4101":
        # 有人绕过成本模块手工记了一笔 4101.01（R-3）⇒ 池 4000 ≠ 账面 4100
        q(f"""insert into biz_voucher (voucher_no, period, voucher_date, voucher_type, source_type,
                                        summary, total_debit, total_credit, entry_count, status,
                                        is_red, posted_at)
              values ('{INJECT_VOUCHER}', '{PERIOD}', '{PERIOD}-20', 'AUTO', 'MANUAL',
                      '{PREFIX} 旁路记账（演练造错）', 100, 100, 2, 'POSTED', 0, now())""")
        for line, (code, side) in enumerate((("4101.01", "D"), ("2241.01", "C")), start=1):
            column = "debit_amount" if side == "D" else "credit_amount"
            q(f"""insert into biz_voucher_entry (voucher_id, line_no, summary, account_id,
                                                 account_code, account_name, {column},
                                                 aux_dept_id)
                  select v.id, {line}, '{PREFIX} 旁路', a.id, '{code}', a.account_name, 100,
                         '{one(f"select id from sys_dept where dept_code = '{PREFIX}-DEPT-1'")}'
                    from biz_voucher v, biz_account a
                   where v.voucher_no = '{INJECT_VOUCHER}' and a.account_code = '{code}'""")
    elif kind == "drop_material_voucher":
        # 领料凭证丢了（料钱没进在制）⇒ C-COST-3 该红：4001 净发生 ≠ Σ(期末在制 − 期初在制)
        q(f"""delete from biz_voucher_entry where voucher_id in
                (select id from biz_voucher where voucher_no = '{VOUCHER_NO}')""")
        q(f"delete from biz_voucher where voucher_no = '{VOUCHER_NO}'")
    else:
        raise SystemExit(f"未知造错场景：{kind}（支持 bypass4101 / drop_material_voucher）")


def inject_fix(cli: Client, period_id: str, kind: str) -> tuple[bool, str]:
    """把造错的地方修回去（外部数据修好即可重试，不需要动批次状态）。"""
    if kind == "bypass4101":
        q(f"""delete from biz_voucher_entry where voucher_id in
                (select id from biz_voucher where voucher_no = '{INJECT_VOUCHER}')""")
        q(f"delete from biz_voucher where voucher_no = '{INJECT_VOUCHER}'")
        return False, "删除旁路凭证"
    if kind == "drop_material_voucher":
        insert_material_voucher(IDS)
        return False, "补回领料凭证（借 4001.01 19000 / 贷 1405）"
    raise SystemExit(f"未知造错场景：{kind}")


def injection_check(cli: Client, period_id: str, kind: str) -> int:
    """造错模式：结算必须被**指定断言**拦下（报文点名），修好后同一张单能重试成功。"""
    expect_code = {"bypass4101": "66065", "drop_material_voucher": "66077"}[kind]
    # 报文判据：不要求代码里出现检查编号（用户看不懂），而是要求**点名了具体对象/数字**：
    #   bypass4101 → 说清「4101 借方 / 差额」；drop_material_voucher → 说清「4001 / 在制」。
    expect_keys = {"bypass4101": ["4101", "差额"],
                   "drop_material_voucher": ["4001", "在制"]}[kind]
    step(f"造错：{kind} → 结算必须被拦下（{'/'.join(expect_keys)} 一类报文）")
    ok("提交 + 两级审批完成", submit_and_approve(cli, period_id, tag="（造错）")[0], "APPROVED",
       scalar(f"select status from biz_cost_period where id = '{period_id}'"))
    inject_break(kind)
    print(f"    已注入：{kind}")
    status, payload = cli.call("POST", f"/business/erp/cost-period/{period_id}/settle")
    code = str((payload or {}).get("code", ""))
    msg = (payload or {}).get("msg") or ""
    ok(f"结算被拦（409 + {expect_code}）", status == 409 and code == expect_code,
       f"409 + {expect_code}", f"{status} + {code}")
    ok("报文点名了具体对象与差额（可定位）", all(key in msg for key in expect_keys),
       "/".join(expect_keys), msg[:160])
    eq("批次仍是 APPROVED（不留死单，可重试）", "APPROVED",
       scalar(f"select status from biz_cost_period where id = '{period_id}'"))
    eq("失败时零写入：没有生产入库单", "0",
       scalar(f"select count(*) from biz_stock_in where cost_period_id = '{period_id}'"))
    eq("失败时零写入：没有结算凭证", "0",
       scalar(f"""select count(*) from biz_voucher where source_type = 'COST_PERIOD'
                   and source_id = '{period_id}'"""))

    need_reapprove, how = inject_fix(cli, period_id, kind)
    print(f"    修法：{how}")
    if need_reapprove:
        eq("撤回后批次回 DRAFT（可重算）", "DRAFT",
           scalar(f"select status from biz_cost_period where id = '{period_id}'"))
        eq("重算后分摊补齐 = 池 4000", "4000.0000",
           scalar(f"""select coalesce(sum(alloc_amount),0)::text from biz_cost_alloc_result
                       where period_code = '{PERIOD}'"""))
        ok("重新提交 + 审批完成", submit_and_approve(cli, period_id, tag="（修好后）")[0],
           "APPROVED", scalar(f"select status from biz_cost_period where id = '{period_id}'"))
    status, payload = cli.call("POST", f"/business/erp/cost-period/{period_id}/settle")
    ok("修好后同一张单重试成功（200）", status == 200, err_of(payload), f"{status}")
    eq("重试后批次 = SETTLED", "SETTLED",
       scalar(f"select status from biz_cost_period where id = '{period_id}'"))
    code_inv, _ = run_invariants()
    ok("修好并结算后巡检 exit 0", code_inv == 0, "exit 0", f"exit {code_inv}")

    print(f"\n通过 {len(PASSED)} 项，失败 {len(FAILED)} 项")
    return 0 if not FAILED else 1


def run_invariants() -> tuple[int, str]:
    """跑成本巡检（`--group 成本`，只读）：C-COST-1~6 必须全绿（exit 0）。"""
    env = dict(os.environ, PGPASSWORD=DB["password"],
               NIK_DATABASE_HOST=DB["host"], NIK_DATABASE_PORT=str(DB["port"]),
               NIK_DATABASE_USER=DB["user"], NIK_DATABASE_NAME=DB["name"])
    r = subprocess.run(
        ["python3", str(ROOT / "scripts" / "verify" / "erp_invariants.py"), "--group", "成本"],
        capture_output=True, text=True, env=env, timeout=300)
    out = (r.stdout or "") + (r.stderr or "")
    tail = "\n".join(out.strip().splitlines()[-12:])
    return r.returncode, tail


def main() -> int:
    print("=" * 86)
    print(f"成本核算端到端演练 期间={PERIOD} 库={DB['name']} 接口={BASE}")
    print("=" * 86)
    ensure_safe_db()

    # ---------- 0 准备 ----------
    step("准备：清场 + 装输入侧夹具（[FIXTURE] 上游单据）")
    clean_fixture()
    ids = pick_master()
    ids = apply_fixture(ids)
    IDS.clear()
    IDS.update(ids)
    ok("夹具已装（成本对象 / 领料 / 工时 / 费用 / 分摊规则）",
       scalar(f"select count(*) from biz_cost_object where object_no = '{OBJ_NO}'") == "1")
    ensure_approvers()

    # 批次（DRAFT）—— 用真实接口建
    cli = Client(BASE)
    q("update sys_user set must_change_password = false where username = 'admin'")
    cli.login(*ADMIN)
    status, payload = cli.call("POST", "/business/erp/cost-period", {"period_code": PERIOD})
    period_id = data_of(payload).get("id", "")
    ok("建批次接口 200 且返回 DRAFT 批次", status == 200 and bool(period_id), err_of(payload),
       f"{status} id={period_id}")
    if not period_id:
        return 1

    # ---------- 1 calc ----------
    step("calc：料 → 工 → 费 → 池对账 → 分摊 → 计算单（六段真实代码）")
    marker = run_calc()
    after = dict(pair.split("=", 1) for pair in marker.split()[1:] if "=" in pair)
    eq("calc 料归集 = 19000", EXPECT["material"], after.get("material", ""))
    eq("calc 工归集 = 4000", EXPECT["labor"], after.get("labor", ""))
    eq("calc 费归集 = 4000", EXPECT["overhead"], after.get("overhead", ""))
    eq("calc 完工成本 = 22311.1104", EXPECT["finished"], after.get("finished", ""))
    eq("calc 期末在制 = 4688.8896", EXPECT["wip"], after.get("wip", ""))

    items = q(f"""select i.completed_qty::text, i.unit_finished_cost::text, i.status
                    from biz_cost_period_item i where i.period_id = '{period_id}'""")
    if items:
        eq("计算单完工数量 = 800", EXPECT["qty_completed"], items[0][0])
        eq("计算单完工单位成本 = 27.888888", EXPECT["unit_finished"], items[0][1])
        eq("计算单状态 = CALC", "CALC", items[0][2])
    else:
        ok("计算单已落库", False, "1 行", "0 行")

    pool = scalar(
        f"""select coalesce(sum(amount), 0)::text from biz_cost_collect
             where period_code = '{PERIOD}' and element = 'OVERHEAD'
               and source_type <> 'ALLOC' and cost_object_id is null""")
    allocated = scalar(f"""select coalesce(sum(alloc_amount), 0)::text
                            from biz_cost_alloc_result where period_code = '{PERIOD}'""")
    eq("Σ 分摊 = Σ 池行（分摊守恒）", pool, allocated)

    # ---------- 造错模式（可选）：验证失败可定位 + 修好能重试 ----------
    if INJECT:
        return injection_check(cli, period_id, INJECT)

    # ---------- 2 审（提交 → 财务经理 → 财务总监） ----------
    step("审：提交 → 财务经理 → 财务总监（真实审批引擎，两级）")
    status, payload = cli.call("POST", f"/business/erp/cost-period/{period_id}/submit")
    ok("提交接口 200", status == 200, err_of(payload), f"{status}")
    eq("批次状态 = SUBMITTED", "SUBMITTED",
       scalar(f"select status from biz_cost_period where id = '{period_id}'"))
    instance = scalar(f"select coalesce(approval_instance_id::text,'') from biz_cost_period where id = '{period_id}'")
    ok("审批实例已启动（approval_instance_id 非空）", bool(instance), "非空", instance or "空")
    ok("实例挂在成本批次上（biz_type/biz_id 同源）",
       scalar(f"select biz_type || '/' || biz_id from biz_flow_instance where id = '{instance}'")
       == f"COST_SETTLE/{period_id}",
       f"COST_SETTLE/{period_id}",
       scalar(f"select biz_type || '/' || biz_id from biz_flow_instance where id = '{instance}'"))

    for user, pwd, role_name in (("e2e_fm", ADMIN[1], "财务经理"),
                                 ("e2e_fd", ADMIN[1], "财务总监")):
        approver = Client(BASE)
        approver.login(user, pwd)
        me = scalar(f"select id from sys_user where username = '{user}'")
        tasks = [r[0] for r in q(
            f"select id from biz_flow_task where instance_id = '{instance}'"
            f" and assignee_id = '{me}' and status = 'PENDING' order by created_at")]
        ok(f"{role_name}收到待办", len(tasks) >= 1, "≥1 条待办", f"{len(tasks)} 条")
        for task in tasks:
            code, body = approver.call("POST", f"/business/flow/task/{task}/approve",
                                       {"comment": f"演练：{role_name}同意"})
            ok(f"{role_name}审批通过（task {task[:8]}）", code == 200, err_of(body), f"{code}")

    eq("两级批完批次 = APPROVED", "APPROVED",
       scalar(f"select status from biz_cost_period where id = '{period_id}'"))
    ok("审批实例结束且保留（作审计痕迹）",
       scalar(f"select status from biz_flow_instance where id = '{instance}'") in ("APPROVED", "FINISHED"),
       "APPROVED", scalar(f"select status from biz_flow_instance where id = '{instance}'"))

    # ---------- 3 结算 ----------
    step("结算：完工入库 + 凭证 + 断言（真实接口，一个事务）")
    status, payload = cli.call("POST", f"/business/erp/cost-period/{period_id}/settle")
    ok("结算接口 200", status == 200, err_of(payload), f"{status}")
    if status != 200:
        print(json.dumps(payload, ensure_ascii=False)[:900])
        return 1
    eq("批次状态 = SETTLED", "SETTLED",
       scalar(f"select status from biz_cost_period where id = '{period_id}'"))
    eq("批次合计：完工 = 22311.1104", EXPECT["finished"],
       scalar(f"select total_finished::text from biz_cost_period where id = '{period_id}'"))
    eq("批次合计：在制 = 4688.8896", EXPECT["wip"],
       scalar(f"select total_wip::text from biz_cost_period where id = '{period_id}'"))

    # 生产入库单 + 库存三本账
    stock = q(f"""select in_no, status, total_qty::text, total_amount::text,
                         coalesce(voucher_id::text,''), in_type, cost_object_id::text
                    from biz_stock_in where cost_period_id = '{period_id}'""")
    if stock:
        ok("生成生产入库单（PRODUCTION / POSTED / 挂成本对象）",
           stock[0][1] == "POSTED" and stock[0][5] == "PRODUCTION"
           and stock[0][6] == ids["obj"],
           "POSTED/PRODUCTION/成本对象", f"{stock[0][1]}/{stock[0][5]}/{stock[0][6][:8]}")
        eq("入库数量 = 800", EXPECT["qty_completed"], stock[0][2])
        eq("入库金额 = 22311.1104", EXPECT["finished"], stock[0][3])
        ok("入库单已回填完工凭证", bool(stock[0][4]), "非空", stock[0][4] or "空")
        in_no = stock[0][0]
    else:
        ok("生成生产入库单", False, "1 张", "0 张")
        in_no = ""

    txn = q(f"""select txn_type, biz_type, qty::text, amount::text, after_qty::text
                  from biz_inventory_txn where doc_no = '{in_no}'""")
    if txn:
        eq("库存流水 txn_type = IN", "IN", txn[0][0])
        eq("库存流水 biz_type = PRODUCTION_IN", "PRODUCTION_IN", txn[0][1])
        eq("库存流水数量 = 800", EXPECT["qty_completed"], txn[0][2])
        eq("库存流水金额 = 22311.1104", EXPECT["finished"], txn[0][3])
        eq("库存流水 after_qty = 800", EXPECT["qty_completed"], txn[0][4])
    else:
        ok("已写库存流水", False, "1 行", "0 行")

    inv = q(f"""select qty::text, amount::text from biz_inventory
                 where material_id = '{ids['fin'][0]}' and warehouse_id = '{ids['wh'][0]}'""")
    eq("库存账（物料×仓库）数量 = 800", EXPECT["qty_completed"], inv[0][0] if inv else "")
    eq("库存账金额 = 22311.1104", EXPECT["finished"], inv[0][1] if inv else "")
    loc = q(f"""select qty::text from biz_inventory_location
                 where material_id = '{ids['fin'][0]}' and warehouse_id = '{ids['wh'][0]}'""")
    eq("库位账数量 = 800", EXPECT["qty_completed"], loc[0][0] if loc else "")

    # 凭证：四张（人工计提 / 手工台账 / 分摊 / 完工结转），全部 POSTED、各自借贷平衡
    vouchers = q(f"""select v.voucher_no, v.status, v.total_debit::text, v.total_credit::text,
                            v.entry_count::text
                       from biz_voucher v
                      where v.source_type = 'COST_PERIOD' and v.source_id = '{period_id}'
                        and v.is_red = 0 order by v.voucher_no""")
    eq("结算凭证张数 = 4（人工计提/手工台账/分摊/完工结转）", "4", str(len(vouchers)))
    ok("四张凭证全部 POSTED", all(v[1] == "POSTED" for v in vouchers),
       "4 x POSTED", "/".join(v[1] for v in vouchers))
    balanced = all(abs(float(v[2]) - float(v[3])) < 0.0001 and float(v[2]) > 0 for v in vouchers)
    ok("四张凭证借贷各自平衡（Δ<0.0001）", balanced, "Σ借 = Σ贷",
       " / ".join(f"{v[2]}={v[3]}" for v in vouchers))

    # 分录级：4001 三末级余额 = 算例 A 在制；4101 清零
    def balance(code: str) -> str:
        return scalar(f"""select (coalesce(sum(e.debit_amount),0) - coalesce(sum(e.credit_amount),0))::text
                            from biz_voucher_entry e join biz_voucher v on v.id = e.voucher_id
                           where e.account_code = '{code}' and v.period = '{PERIOD}'
                             and v.status = 'POSTED' and v.is_red = 0""")

    eq("4001.01 余额 = 3800.0000（在制-直接材料）", "3800.0000", balance("4001.01"))
    eq("4001.02 余额 = 444.4448（在制-直接人工）", "444.4448", balance("4001.02"))
    eq("4001.03 余额 = 444.4448（在制-制造费用）", "444.4448", balance("4001.03"))
    eq("4101.01 清零", "0", balance("4101.01"))
    eq("4101.02 清零", "0", balance("4101.02"))

    # ---------- 4 对平（巡检） ----------
    step("对平：成本巡检 C-COST-1~6（erp_invariants.py --group 成本）")
    code, tail = run_invariants()
    ok("巡检 exit 0（零 BLOCKER / 零 WARN）", code == 0, "exit 0", f"exit {code}")
    print("    " + tail.replace("\n", "\n    ")[:2000])

    # ---------- 5 红冲 ----------
    step("红冲：凭证红字 + 库存回冲 + 入库单作废 + 批次 REVERSED")
    status, payload = cli.call("POST", f"/business/erp/cost-period/{period_id}/reverse",
                               {"reason": "演练：红冲第一轮结算"})
    ok("红冲接口 200", status == 200, err_of(payload), f"{status}")
    if status != 200:
        print(json.dumps(payload, ensure_ascii=False)[:900])
        return 1
    eq("批次状态 = REVERSED", "REVERSED",
       scalar(f"select status from biz_cost_period where id = '{period_id}'"))
    eq("红冲原因已落库", "演练：红冲第一轮结算",
       scalar(f"select coalesce(reverse_reason,'') from biz_cost_period where id = '{period_id}'"))
    eq("生产入库单已作废", "CANCELLED",
       scalar(f"select status from biz_stock_in where cost_period_id = '{period_id}'"))
    eq("计算单回到 CALC", "CALC",
       scalar(f"select status from biz_cost_period_item where period_id = '{period_id}'"))

    red = q(f"""select count(*) from biz_voucher
                 where source_type = 'COST_PERIOD' and source_id = '{period_id}'
                   and is_red = 1 and status = 'POSTED'""")
    eq("红字凭证张数 = 4", "4", red[0][0] if red else "0")
    linked = scalar(f"""select count(*) from biz_voucher
                         where source_type = 'COST_PERIOD' and source_id = '{period_id}'
                           and is_red = 0 and reversed_by is not null""")
    eq("原凭证全部打上 reversed_by", "4", linked)

    inv = q(f"""select qty::text, amount::text from biz_inventory
                 where material_id = '{ids['fin'][0]}' and warehouse_id = '{ids['wh'][0]}'""")
    eq("库存回冲到 0 件", "0", inv[0][0] if inv else "0")
    eq("库存金额回冲到 0", "0", inv[0][1] if inv else "0")
    eq("回冲流水（PRODUCE_CANCEL）已写", "1",
       scalar(f"""select count(*) from biz_inventory_txn
                   where doc_no = '{in_no}' and biz_type = 'PRODUCE_CANCEL'"""))
    # 红冲只回冲**结算自己产生的**凭证；领料凭证（借 4001.01 19000）是上游单据的账，
    # 不该被成本红冲带走 ⇒ 红冲后 4001.01 净额仍是 19000（料还躺在在制里，等下一轮结算）。
    eq("红冲后 4001.01 净发生 = 19000（领料凭证不回冲，料仍在在制）", "19000.0000", net_balance("4001.01"))
    for code_name in ("4001.02", "4001.03", "4101.01", "4101.02"):
        eq(f"红冲后 {code_name} 净发生 = 0（含红字）", "0", net_balance(code_name))

    code, tail = run_invariants()
    ok("红冲后巡检仍 exit 0（无脏账）", code == 0, "exit 0", f"exit {code}")

    # ---------- 6 重算（唯一键不冲突） ----------
    step("重算：红冲后建第二张活跃批次 + 再算（唯一键不冲突）")
    status, payload = cli.call("POST", "/business/erp/cost-period", {"period_code": PERIOD})
    period2 = data_of(payload).get("id", "")
    ok("红冲后同期间能建新批次（排除 REVERSED 的唯一键生效）",
       status == 200 and bool(period2), err_of(payload), f"{status}")
    if not period2:
        return 1
    status2, payload2 = cli.call("POST", "/business/erp/cost-period", {"period_code": PERIOD})
    ok("同期间第二张活跃批次被唯一键拒绝（不是放水）", status2 in (409, 400),
       "409/400", f"{status2} {err_of(payload2)}")
    eq("同期间只剩 1 张活跃批次（另一张是 REVERSED）", "1",
       scalar(f"""select count(*) from biz_cost_period
                   where period_code = '{PERIOD}' and status not in ('REVERSED','CANCELLED')"""))

    marker2 = run_calc()
    after2 = dict(pair.split("=", 1) for pair in marker2.split()[1:] if "=" in pair)
    ok("重算批次号不同于第一轮", after2.get("batch") != after.get("batch"),
       f"{after2.get('batch')} != {after.get('batch')}", after2.get("batch", ""))
    eq("重算完工成本一致 = 22311.1104", EXPECT["finished"], after2.get("finished", ""))
    eq("重算在制一致 = 4688.8896", EXPECT["wip"], after2.get("wip", ""))

    # ---------- 7 第二轮结算（链路可重复） ----------
    step("再结算：第二轮跑完，库存/凭证/对平回到第一轮的数")
    status, payload = cli.call("POST", f"/business/erp/cost-period/{period2}/submit")
    ok("第二轮提交 200", status == 200, err_of(payload), f"{status}")
    instance2 = scalar(f"select coalesce(approval_instance_id::text,'') from biz_cost_period where id = '{period2}'")
    for user in ("e2e_fm", "e2e_fd"):
        approver = Client(BASE)
        approver.login(user, ADMIN[1])
        me = scalar(f"select id from sys_user where username = '{user}'")
        for task in [r[0] for r in q(
                f"select id from biz_flow_task where instance_id = '{instance2}'"
                f" and assignee_id = '{me}' and status = 'PENDING'")]:
            approver.call("POST", f"/business/flow/task/{task}/approve", {"comment": "演练：第二轮同意"})
    eq("第二轮批次 = APPROVED", "APPROVED",
       scalar(f"select status from biz_cost_period where id = '{period2}'"))
    status, payload = cli.call("POST", f"/business/erp/cost-period/{period2}/settle")
    ok("第二轮结算 200", status == 200, err_of(payload), f"{status}")
    eq("第二轮批次 = SETTLED", "SETTLED",
       scalar(f"select status from biz_cost_period where id = '{period2}'"))
    eq("第二轮入库金额 = 22311.1104", EXPECT["finished"],
       scalar(f"select total_amount::text from biz_stock_in where cost_period_id = '{period2}'"))
    inv = q(f"""select qty::text, amount::text from biz_inventory
                 where material_id = '{ids['fin'][0]}' and warehouse_id = '{ids['wh'][0]}'""")
    eq("第二轮库存回到 800 件", EXPECT["qty_completed"], inv[0][0] if inv else "")
    eq("第二轮库存金额 = 22311.1104", EXPECT["finished"], inv[0][1] if inv else "")
    eq("第二轮凭证张数 = 4", "4",
       scalar(f"""select count(*) from biz_voucher
                   where source_type = 'COST_PERIOD' and source_id = '{period2}'
                     and is_red = 0"""))
    code, tail = run_invariants()
    ok("第二轮结算后巡检 exit 0", code == 0, "exit 0", f"exit {code}")

    # ---------- 收尾 ----------
    step("收尾：清场（不留夹具）")
    clean_fixture(quiet=False)
    leftover = scalar(f"""select
            (select count(*) from biz_cost_period where period_code = '{PERIOD}')
          + (select count(*) from biz_cost_object where object_no like '{PREFIX}-%')
          + (select count(*) from biz_voucher where voucher_no like '{PREFIX}-%' and true)""")
    eq("清场后残留 = 0", "0", leftover)

    print(f"\n{'=' * 86}")
    print(f"通过 {len(PASSED)} 项，失败 {len(FAILED)} 项")
    if FAILED:
        print("失败项：")
        for name in FAILED:
            print(f"  · {name}")
    print("=" * 86)
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
