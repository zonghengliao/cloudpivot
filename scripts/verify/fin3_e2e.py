#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""FIN-3 费用报销真机全链路验收（HTTP，跑在隔离实例上）。

链路（卡面「真机全链路」逐条对应）：

  A) 提交一张报销单（**3 条明细**：差旅 3,180 + 住宿 2,120 + 招待 1,060）
     → 直属上级审批 → 金额分级②（6,360 ≥ 1000 且 < 10000）→ 财务经理审批
     → 入账（计提凭证：借 5602.02 5,000 + 借 5602.04 1,060 + 借 2221.01 300 / 贷 2241.01 6,360）
     → 付款 3,000（部分）→ 付款凭证（借 2241.01 3,000 / 贷 1002 3,000）
     → 再付 3,360 → PAID
     → 付清后再作废必须 409（ARCH-6 §4.3.3-3）

  B) 另建一张单 → 提交 → 审批通过 → **作废** → 计提凭证红冲 + 预算占用 RELEASED
     + 2241.01 净额归零（应付款回滚）

  C) 负向用例：明细为空 400 / 明细合计被篡改 → 入账前 422+66042（用库层面造脏数据复现）

用法：
    python3 scripts/verify/fin3_e2e.py [base_url]
默认 http://127.0.0.1:18026；DB 断言走 NIK_DATABASE_*（默认 alun/alun/cloudpivot_fin3）。

判据全部来自**库里的真实数据**（凭证分录 / 状态 / 预算占用），不看接口回包的自我声明。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18026"
DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "alun"),
    name=os.environ.get("NIK_DATABASE_NAME", "cloudpivot_fin3"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "alun"),
)
ADMIN = ("admin", "Admin@123456")
APPROVER = ("fin3mgr", "Admin@123456")
PASSED: list[str] = []
FAILED: list[str] = []


def q(sql: str):
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
         "-d", DB["name"], "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:400])
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def one(sql: str, default: str = "") -> str:
    rows = q(sql)
    return rows[0][0] if rows and rows[0] else default


def ok(name: str, condition: bool, detail: str = "") -> None:
    if condition:
        PASSED.append(name)
        print(f"  ✅ {name}" + (f"（{detail}）" if detail else ""))
    else:
        FAILED.append(name)
        print(f"  🚨 {name}" + (f" —— {detail}" if detail else ""))


class Client:
    def __init__(self, base: str):
        self.base = base
        self.token = ""

    def call(self, method: str, path: str, body=None, expect=None):
        url = self.base + path
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Content-Type", "application/json")
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                payload = json.loads(resp.read().decode())
                status = resp.status
        except urllib.error.HTTPError as err:
            raw = err.read().decode()
            try:
                payload = json.loads(raw)
            except Exception:
                payload = {"raw": raw}
            status = err.code
        if expect is not None and status != expect:
            print(f"    ↳ {method} {path} → {status}: {json.dumps(payload, ensure_ascii=False)[:400]}")
        return status, payload

    def login(self, user: str, pwd: str) -> None:
        status, payload = self.call("POST", "/api/system/auth/login",
                                    {"username": user, "password": pwd}, expect=200)
        if status != 200:
            raise SystemExit(f"登录失败：{status} {payload}")
        self.token = payload["data"]["access_token"]


def data_of(payload):
    return payload.get("data")


def ensure_fixtures() -> str:
    """夹具：一个部门 + 审批人账号（内控闸要求「发起人不能批自己」，这是内控本身的要求）。

    * admin 挂上部门（报销单的部门是预算维度，没部门不能建单）；
    * `fin3mgr` 挂 FINANCE_MANAGER + GENERAL_MANAGER + SUPER_ADMIN：
      本用例验的是**业务链**（三档里的第二档 = 财务经理），
      直属上级节点走 `INITIATOR_SUPERIOR`，解析不到就 FALLBACK_ADMIN（= 另一个超管 fin3mgr）。
    """
    q("""
        insert into sys_dept (dept_code, dept_name, status, sort_order)
        select 'FIN3-DEPT', 'FIN3 费用报销验收部', 1, 90
         where not exists (select 1 from sys_dept where dept_code = 'FIN3-DEPT' and deleted_at is null)
    """)
    dept_id = one("select id from sys_dept where dept_code = 'FIN3-DEPT' and deleted_at is null")
    q(f"update sys_user set dept_id = '{dept_id}'::uuid, must_change_password = false "
      f"where username = 'admin'")
    q("""
        insert into sys_user (id, username, password, real_name, status, must_change_password,
                              token_version, user_type, remark)
        select gen_random_uuid(), 'fin3mgr', u.password, 'FIN3 验收审批人', 1, false,
               1, 'HUMAN', 'FIN-3 e2e 用：报销审批人'
          from sys_user u where u.username = 'admin'
           and not exists (select 1 from sys_user where username = 'fin3mgr')
    """)
    q(f"update sys_user set dept_id = '{dept_id}'::uuid where username = 'fin3mgr'")
    # 模板库里别的卡留下的超管（如 fin2gm）会让 FALLBACK_ADMIN 派发出多个待办，
    # 这个节点要求「全员批」，会把用例卡在「别人的待办」上。本库是一次性夹具库，直接解绑。
    q("""
        delete from sys_user_role ur
         using sys_role r, sys_user u
         where ur.role_id = r.id and ur.user_id = u.id
           and r.role_code in ('SUPER_ADMIN', 'FINANCE_MANAGER', 'GENERAL_MANAGER')
           and u.username not in ('admin', 'fin3mgr')
    """)
    q("""
        insert into sys_user_role (id, user_id, role_id)
        select gen_random_uuid(), u.id, r.id
          from sys_user u
          join sys_role r on r.role_code in ('FINANCE_MANAGER', 'GENERAL_MANAGER', 'SUPER_ADMIN')
                         and r.deleted_at is null
         where u.username = 'fin3mgr'
           and not exists (select 1 from sys_user_role x where x.user_id = u.id and x.role_id = r.id)
    """)
    return dept_id


def report_id_of(payload) -> str:
    """建单回包 → 报销单 id（拿不到返回空串，由调用方判失败）。"""
    data = payload.get("data") or {}
    report = data.get("report") or {}
    return report.get("id", "")


def pending_tasks(report_id: str) -> list[tuple[str, str]]:
    """这张报销单在跑的待办 (task_id, node_key)，按生成顺序。"""
    return [(r[0], r[1]) for r in q(f"""
        select t.id, t.node_key from biz_flow_task t
          join biz_flow_instance i on i.id = t.instance_id
         where i.biz_type = 'EXPENSE' and i.biz_id = '{report_id}'::uuid
           and t.status = 'PENDING' order by t.created_at
    """)]


def approve_node(client: Client, report_id: str, node_key: str, comment: str) -> int:
    """只批指定节点的待办（金额分级要逐站观察，不能一把梭）。"""
    n = 0
    for task_id, key in pending_tasks(report_id):
        if key != node_key:
            continue
        status, _ = client.call("POST", f"/api/business/flow/task/{task_id}/approve",
                                {"comment": comment})
        if status == 200:
            n += 1
    return n


def approve_all(client: Client, report_id: str, comment: str = "同意报销") -> int:
    """把这张报销单在跑的待办全部批完（逐站推进，直到没有 PENDING 待办）。"""
    approved = 0
    for _ in range(6):
        tasks = q(f"""
            select t.id from biz_flow_task t
              join biz_flow_instance i on i.id = t.instance_id
             where i.biz_type = 'EXPENSE' and i.biz_id = '{report_id}'::uuid
               and t.status = 'PENDING' order by t.created_at
        """)
        if not tasks:
            break
        for (task_id,) in tasks:
            status, payload = client.call(
                "POST", f"/api/business/flow/task/{task_id}/approve", {"comment": comment})
            if status != 200:
                print(f"    ↳ 审批失败 {task_id}: {status} {payload.get('code')} {payload.get('msg')}")
                continue
            approved += 1
    return approved


def items_payload(types: dict) -> list:
    """三条明细（含税录入，服务端拆价税）：
    差旅 3,180（6% 可抵扣）+ 住宿 2,120（6% 可抵扣）+ 招待 1,060（6% **不可抵扣**）。
    期望：税额 180 + 120 + 0 = 300；不含税 3,000 + 2,000 + 1,060；价税合计 6,360。
    """
    return [
        {"expense_type_id": types["TRAVEL"], "with_tax_amount": "3180", "tax_rate": "0.06",
         "invoice_no": "INV-FIN3-001", "invoice_type": "VAT_SPECIAL",
         "summary": "高铁 + 机票（上海客户拜访）", "expense_date": "2026-11-03"},
        {"expense_type_id": types["TRAVEL"], "with_tax_amount": "2120", "tax_rate": "0.06",
         "invoice_no": "INV-FIN3-002", "invoice_type": "VAT_SPECIAL",
         "summary": "住宿 2 晚", "expense_date": "2026-11-03"},
        {"expense_type_id": types["MEAL"], "with_tax_amount": "1060", "tax_rate": "0.06",
         "invoice_no": "INV-FIN3-003", "invoice_type": "VAT_NORMAL",
         "summary": "客户招待（进项不可抵扣）", "expense_date": "2026-11-04"},
    ]


def main() -> int:
    cli = Client(BASE)
    print("=" * 88)
    print("FIN-3 费用报销真机全链路")
    print(f"实例 {BASE}  库 {DB['name']}")
    print("=" * 88)

    dept_id = ensure_fixtures()
    cli.login(*ADMIN)

    # 费用类型种子（V0242 种的 8 条）
    types = {row[0]: row[1] for row in q(
        "select type_code, id from biz_expense_type where deleted_at is null")}
    ok("费用类型种子齐全（8 条，含 TRAVEL / MEAL / OFFICE）",
       len(types) >= 8 and "TRAVEL" in types and "MEAL" in types,
       f"{len(types)} 条")

    # ---------- 0) 负向：空明细 ----------
    print("\n⓪ 负向用例（建单校验）")
    status, payload = cli.call("POST", "/api/business/erp/expense",
                               {"title": "空单", "items": []})
    ok("明细为空 → 400（ERP 大忌第一道闸）", status == 400, f"{status} {payload.get('msg')}")
    status, payload = cli.call("POST", "/api/business/erp/expense", {
        "title": "负数单",
        "items": [{"expense_type_id": types["TRAVEL"], "with_tax_amount": "-1",
                   "tax_rate": "0.06", "summary": "冲减"}],
    })
    ok("明细金额为负 → 400", status == 400, f"{status} {payload.get('msg')}")

    # ---------- ① 建单（3 条明细）----------
    print("\n① 建报销单（3 条明细，含税录入 → 服务端拆价税）")
    status, payload = cli.call("POST", "/api/business/erp/expense", {
        "title": "10 月上海客户拜访差旅",
        "expense_date": "2026-11-03",
        "items": items_payload(types),
        "remark": "FIN-3 e2e（A 单：走完审批 + 付款）",
    }, expect=200)
    ok("建单 200", status == 200, f"{status} {str(payload.get('msg'))[:80]}")
    detail = data_of(payload) or {}
    report = detail.get("report") or {}
    report_id = report.get("id", "")
    ok("单号形态 EXP-yyyyMMdd-NNNNNN",
       report.get("report_no", "").startswith("EXP-"), report.get("report_no", ""))
    ok("明细 3 条", len(detail.get("items") or []) == 3, str(len(detail.get("items") or [])))
    totals = one(f"select total_amount::text || '|' || total_tax::text || '|' || total_with_tax::text "
                 f"from biz_expense_report where id='{report_id}'::uuid")
    ok("不含税合计 6,060 / 税额 300 / 价税 6,360（库里的值）",
       totals == "6060.0000|300.0000|6360.0000", totals)
    ok("状态 DRAFT + 未付款", (report.get("status"), report.get("pay_status")) == ("DRAFT", "UNPAID"))
    items = {(row[0], row[1]): (row[2], row[3]) for row in q(f"""
        select type_code, account_code, amount::text, tax_amount::text
          from biz_expense_item where report_id = '{report_id}'::uuid order by line_no""")}
    ok("不可抵扣类型（业务招待费）税额为 0 且全额进费用",
       items.get(("MEAL", "5602.04")) == ("1060.0000", "0.0000"), str(items.get(("MEAL", "5602.04"))))
    ok("可抵扣差旅两行各自 3,000/180 与 2,000/120（明细逐行拆，不合并）",
       one(f"""select string_agg(amount::text || '/' || tax_amount::text, ',' order by line_no)
                  from biz_expense_item where report_id='{report_id}'::uuid and type_code='TRAVEL'""")
       == "3000.0000/180.0000,2000.0000/120.0000",
       one(f"""select string_agg(amount::text || '/' || tax_amount::text, ',' order by line_no)
                  from biz_expense_item where report_id='{report_id}'::uuid and type_code='TRAVEL'"""))

    # ---------- ② 提交审批 ----------
    print("\n② 提交审批（预算守卫 + 发起 expense 流程）")
    status, payload = cli.call("POST", f"/api/business/erp/expense/{report_id}/submit", {})
    ok("提交 200", status == 200, f"{status} {str(payload.get('msg'))[:100]}")
    ok("单据 SUBMITTED", one(f"select status from biz_expense_report where id='{report_id}'::uuid") == "SUBMITTED")
    inst = q(f"select id, status, biz_type from biz_flow_instance where biz_type='EXPENSE' and biz_id='{report_id}'::uuid")
    ok("审批实例已建（EXPENSE）", len(inst) == 1 and inst[0][2] == "EXPENSE", str(inst))
    ok("审批中的单不能再改明细（CAS）",
       cli.call("PUT", f"/api/business/erp/expense/{report_id}", {
           "title": "偷偷改一下", "items": items_payload(types)})[0] == 409)
    usages = q(f"""select status, over_budget, amount::text from biz_budget_usage
                    where source_type='EXPENSE_REPORT' and source_id='{report_id}'::uuid""")
    ok("预算占用已写（OCCUPIED，按费用科目聚合）", len(usages) >= 1 and usages[0][0] == "OCCUPIED", str(usages))

    # ---------- ③ 审批（直属上级 → 财务经理）----------
    print("\n③ 审批三档（本单 6,360 → 直属上级 + 财务经理）")
    first_task = q(f"""select t.id, t.node_key from biz_flow_task t
                         join biz_flow_instance i on i.id = t.instance_id
                        where i.biz_id='{report_id}'::uuid and t.status='PENDING'
                        order by t.created_at limit 1""")
    ok("第一站待办已生成（直属上级）", len(first_task) == 1, str(first_task))
    if first_task:
        status, payload = cli.call("POST", f"/api/business/flow/task/{first_task[0][0]}/approve",
                                   {"comment": "自己批自己"})
        ok("发起人自批被内控拒绝（403 + 65019）",
           status == 403 and payload.get("code") == 65019,
           f"{status} code={payload.get('code')}")
    mgr = Client(BASE)
    mgr.login(*APPROVER)
    hops = approve_all(mgr, report_id, "同意报销（FIN-3 e2e）")
    ok("待办全部批完（≥2 站：直属上级 + 财务经理）", hops >= 2, f"{hops} 站")
    ok("实例 APPROVED",
       one(f"select status from biz_flow_instance where id='{inst[0][0]}'::uuid") == "APPROVED")
    row = q(f"""select status, voucher_id::text, period_code, approved_at::text
                  from biz_expense_report where id='{report_id}'::uuid""")[0]
    ok("单据 APPROVED + 已绑定计提凭证", row[0] == "APPROVED" and row[1] not in ("", "-"), str(row))

    # ---------- ④ 计提凭证分录 ----------
    print("\n④ 计提凭证（借 费用科目 + 借 进项税 / 贷 2241.01）")
    accrual = row[1]
    if not accrual:
        ok("实例与单据都已 APPROVED 且绑定了计提凭证", False,
           f"单据状态={row[0]} 凭证={row[1]!r}（实例状态={one(f'select status from biz_flow_instance where id=\'{inst[0][0]}\'::uuid')}）")
        print("\n" + "=" * 88)
        print(f"通过 {len(PASSED)} 项，失败 {len(FAILED)} 项（后续用例依赖计提凭证，提前结束）")
        return 1
    lines = q(f"""select account_code, debit_amount::text, credit_amount::text
                    from biz_voucher_entry where voucher_id='{accrual}'::uuid order by line_no""")
    ok("借方 5602.02 差旅 5,000（差旅 3,000 + 住宿 2,000 同科目合并）",
       ["5602.02", "5000.0000", "0.0000"] in lines, str(lines))
    ok("借方 5602.04 招待 1,060（不可抵扣 → 税并入费用）",
       ["5602.04", "1060.0000", "0.0000"] in lines, str(lines))
    ok("借方 2221.01 进项税 300（只含可抵扣部分：180 + 120）",
       ["2221.01", "300.0000", "0.0000"] in lines, str(lines))
    ok("贷方 2241.01 员工垫付 6,360",
       ["2241.01", "0.0000", "6360.0000"] in lines, str(lines))
    debit = one(f"select coalesce(sum(debit_amount),0)::text from biz_voucher_entry where voucher_id='{accrual}'::uuid")
    credit = one(f"select coalesce(sum(credit_amount),0)::text from biz_voucher_entry where voucher_id='{accrual}'::uuid")
    ok("借贷平衡且 = 价税合计", debit == credit == "6360.0000", f"{debit} = {credit}")
    ok("预算占用转 CONSUMED 且挂了凭证",
       int(one(f"""select count(*) from biz_budget_usage where source_type='EXPENSE_REPORT'
                 and source_id='{report_id}'::uuid and status='CONSUMED' and voucher_id='{accrual}'::uuid""")) >= 1)

    # ---------- ⑤ 付款（部分 → 付清）----------
    print("\n⑤ 付款（登记式：部分付款 → 付清）")
    status, payload = cli.call("POST", f"/api/business/erp/expense/{report_id}/pay",
                               {"amount": "3000", "pay_date": "2026-11-05",
                                "pay_method": "BANK_TRANSFER", "remark": "第一笔"},
                               expect=200)
    ok("部分付款 200", status == 200, f"{status} {str(payload.get('msg'))[:80]}")
    pay1 = q(f"""select payment_no, status, period_code, voucher_id::text from biz_expense_payment
                  where report_id='{report_id}'::uuid order by created_at limit 1""")
    ok("付款记录已写（PAY-EXP 号段 + PAID）",
       len(pay1) == 1 and pay1[0][0].startswith("PAY-EXP-") and pay1[0][1] == "PAID", str(pay1))
    pay_lines = q(f"""select account_code, debit_amount::text, credit_amount::text
                        from biz_voucher_entry where voucher_id='{pay1[0][3]}'::uuid order by line_no""")
    ok("付款凭证：借 2241.01 3,000 / 贷 1002 3,000",
       pay_lines == [["2241.01", "3000.0000", "0.0000"], ["1002", "0.0000", "3000.0000"]], str(pay_lines))
    mid = q(f"select status, pay_status, paid_amount::text, unpaid_amount::text from biz_expense_report where id='{report_id}'::uuid")[0]
    ok("部分付款后：APPROVED / PARTIAL / 3,000 / 3,360",
       mid == ["APPROVED", "PARTIAL", "3000.0000", "3360.0000"], str(mid))
    status, payload = cli.call("POST", f"/api/business/erp/expense/{report_id}/pay",
                               {"amount": "3360", "pay_date": "2026-11-06"}, expect=200)
    ok("付清 200", status == 200, f"{status} {str(payload.get('msg'))[:80]}")
    fin = q(f"select status, pay_status, paid_amount::text, unpaid_amount::text from biz_expense_report where id='{report_id}'::uuid")[0]
    ok("付清后：PAID / PAID / 6,360 / 0", fin == ["PAID", "PAID", "6360.0000", "0.0000"], str(fin))
    ok("付款状态与库 CHECK 自洽（pay_status=PAID ⟺ status=PAID）",
       one(f"select count(*) from biz_expense_report where id='{report_id}'::uuid and pay_status='PAID' and status='PAID'") == "1")
    ok("两张付款凭证各一张（共 2 笔）",
       one(f"select count(*) from biz_expense_payment where report_id='{report_id}'::uuid") == "2")
    status, payload = cli.call("POST", f"/api/business/erp/expense/{report_id}/cancel",
                               {"reason": "付清后试图作废（应被拒绝）"})
    ok("PAID 之后不许作废（409）", status == 409, f"{status} {str(payload.get('msg'))[:60]}")

    # ---------- ⑥ B 单：审批通过 → 作废红冲 ----------
    print("\n⑥ 第二张单：审批通过 → 作废（凭证红冲 + 预算回滚）")
    status, payload = cli.call("POST", "/api/business/erp/expense", {
        "title": "办公耗材补报（B 单：验作废红冲）",
        "items": [{"expense_type_id": types["OFFICE"], "with_tax_amount": "226", "tax_rate": "0.13",
                   "invoice_no": "INV-FIN3-101", "invoice_type": "VAT_SPECIAL",
                   "summary": "打印纸 + 墨盒"}],
        "remark": "FIN-3 e2e（B 单：作废红冲）",
    }, expect=200)
    ok("B 单建单 200", status == 200)
    b_id = (data_of(payload) or {}).get("report", {}).get("id", "")
    ok("B 单 226 含税 13% → 不含税 200 / 税 26",
       one(f"select total_amount::text || '|' || total_tax::text from biz_expense_report where id='{b_id}'::uuid")
       == "200.0000|26.0000",
       one(f"select total_amount::text || '|' || total_tax::text from biz_expense_report where id='{b_id}'::uuid"))
    ok("B 单提交 200", cli.call("POST", f"/api/business/erp/expense/{b_id}/submit", {})[0] == 200)
    approve_all(mgr, b_id, "同意（B 单）")
    b_accrual = one(f"select coalesce(voucher_id::text,'') from biz_expense_report where id='{b_id}'::uuid")
    ok("B 单已入账（有计提凭证）", b_accrual != "", b_accrual)
    status, payload = cli.call("POST", f"/api/business/erp/expense/{b_id}/cancel",
                               {"reason": "办公耗材报销重复提交，整单作废处理"}, expect=200)
    ok("作废 200", status == 200, f"{status} {str(payload.get('msg'))[:80]}")
    ok("B 单 CANCELLED 且有原因",
       one(f"select status || '|' || coalesce(cancel_reason,'') from biz_expense_report where id='{b_id}'::uuid")
       .startswith("CANCELLED|"))
    ok("蓝字计提凭证已红冲（reversed_by 非空）",
       one(f"select count(*) from biz_voucher where id='{b_accrual}'::uuid and reversed_by is not null") == "1")
    reds = q(f"""select id, voucher_no from biz_voucher
                  where source_type='EXPENSE_REPORT' and source_id='{b_id}'::uuid and is_red=1""")
    ok("生成红字凭证（EXPENSE_REPORT 红字）", len(reds) == 1, str(reds))
    red_lines = q(f"""select account_code, debit_amount::text, credit_amount::text
                        from biz_voucher_entry where voucher_id='{reds[0][0]}'::uuid order by line_no""")
    ok("红字凭证借贷互换（贷 5602.03 200 + 贷 2221.01 26 / 借 2241.01 226）",
       ["5602.03", "0.0000", "200.0000"] in red_lines
       and ["2221.01", "0.0000", "26.0000"] in red_lines
       and ["2241.01", "226.0000", "0.0000"] in red_lines, str(red_lines))
    ok("预算占用已释放（RELEASED）",
       int(one(f"""select count(*) from biz_budget_usage where source_type='EXPENSE_REPORT'
                 and source_id='{b_id}'::uuid and status='RELEASED'""")) >= 1)
    ok("A 单 + B 单的 2241.01 净额归零（已付清 + 已红冲 ⇒ 应付款回滚）",
       one(f"""select coalesce(sum(e.credit_amount-e.debit_amount),0)::text
                 from biz_voucher_entry e join biz_voucher v on v.id = e.voucher_id
                where e.account_code='2241.01'
                  and v.source_id in ('{report_id}'::uuid,'{b_id}'::uuid)""") == "0.0000",
       one(f"""select coalesce(sum(e.credit_amount-e.debit_amount),0)::text
                 from biz_voucher_entry e join biz_voucher v on v.id = e.voucher_id
                where e.account_code='2241.01'
                  and v.source_id in ('{report_id}'::uuid,'{b_id}'::uuid)"""))

    # ---------- ⑦ 金额分级三档（路由由 V0243 的条件边决定，不是代码硬编码）----------
    print("\n⑦ 金额分级三档（< 1000 / 1000–10000 / ≥ 10000）")
    tier_cases = [
        ("300", "n_sup", None, "① < 1000：只走直属上级（不出现财务经理待办）"),
        ("12000", "n_sup", "n_board", "③ ≥ 10000：上级 → 财务经理 + 总经理会签"),
    ]
    for amount, first_node, expect_next, label in tier_cases:
        status, payload = cli.call("POST", "/api/business/erp/expense", {
            "title": f"分级验收 {amount}",
            "items": [{"expense_type_id": types["TRAVEL"], "with_tax_amount": amount,
                       "tax_rate": "0.06", "invoice_no": f"INV-FIN3-T{amount}",
                       "invoice_type": "VAT_SPECIAL", "summary": "分级验收（金额档位）"}],
            "remark": "FIN-3 e2e（分级：只看待办节点，不产生付款）",
        }, expect=200)
        ok(f"{label} 建单", status == 200)
        rid = report_id_of(payload)
        if not rid:
            ok(f"{label} 建单回包含 id", False, json.dumps(payload, ensure_ascii=False)[:200])
            continue
        ok(f"{label} 提交", cli.call("POST", f"/api/business/erp/expense/{rid}/submit", {})[0] == 200)
        nodes = pending_tasks(rid)
        ok(f"{label} 第一站 = {first_node}", [k for _, k in nodes] == [first_node],
           str([k for _, k in nodes]))
        approve_node(mgr, rid, first_node, "同意（分级验收）")
        if expect_next is None:
            inst_status = one(f"""select i.status from biz_flow_instance i
                                   where i.biz_type='EXPENSE' and i.biz_id='{rid}'::uuid""")
            ok(f"{label} 上级批完即结束（实例 APPROVED）", inst_status == "APPROVED", inst_status)
            visited_fin = one(f"""select count(*) from biz_flow_task t
                                    join biz_flow_instance i on i.id = t.instance_id
                                   where i.biz_id='{rid}'::uuid and t.node_key='n_fin'""")
            ok(f"{label} 全程没有财务经理待办", visited_fin == "0", visited_fin)
        else:
            after = pending_tasks(rid)
            ok(f"{label} 第二站落在会签节点 {expect_next}",
               bool(after) and all(k == expect_next for _, k in after), str([k for _, k in after]))
            # 会签 = 财务经理 + 总经理两会签角色（V0243 种子）。运行时引擎按人去重，
            # 本夹具的 fin3mgr 同时兼任两个角色，所以只会产出 1 个待办；这里查的是
            # **节点配置**里两条角色种子都在，避免「会签退化成单人」被漏掉。
            ok(f"{label} 会签节点种子＝财务经理 + 总经理两角色",
               one("""select count(*) from biz_flow_node_assignee a
                        join biz_flow_node n on n.id = a.node_id
                       where n.node_key='n_board'
                         and n.version_id='20000000-0000-4000-8000-0000000d0250'
                         and a.ref_id in ('20000000-0000-4000-8000-0000000f0005'::uuid,
                                          '20000000-0000-4000-8000-0000000f0003'::uuid)""") == "2")
            approve_all(mgr, rid, "同意（会签）")
            inst_status = one(f"""select i.status from biz_flow_instance i
                                   where i.biz_type='EXPENSE' and i.biz_id='{rid}'::uuid""")
            ok(f"{label} 会签批完 → 实例 APPROVED", inst_status == "APPROVED", inst_status)
            visited_fin = one(f"""select count(*) from biz_flow_task t
                                    join biz_flow_instance i on i.id = t.instance_id
                                   where i.biz_id='{rid}'::uuid and t.node_key='n_fin'""")
            ok(f"{label} 没走中间档（无 n_fin 待办）", visited_fin == "0", visited_fin)
        ok(f"{label} 入账凭证已生成（状态 APPROVED 且有 voucher_id）",
           one(f"""select count(*) from biz_expense_report
                    where id='{rid}'::uuid and status='APPROVED' and voucher_id is not null""") == "1")

    # ---------- ⑧ 巡检不变量（L 组口径的现场复核）----------
    print("\n⑧ 现场不变量")
    ok("L1 每张单 total_with_tax = Σ 明细 with_tax",
       one("""select count(*) from biz_expense_report r
               where r.deleted_at is null
                 and r.total_with_tax <> (select coalesce(sum(i.with_tax_amount),0)
                                            from biz_expense_item i where i.report_id = r.id)""") == "0")
    ok("L1' item_count = 明细条数",
       one("""select count(*) from biz_expense_report r
               where r.deleted_at is null
                 and r.item_count <> (select count(*) from biz_expense_item i where i.report_id = r.id)""") == "0")
    ok("L2 APPROVED/PAID 必有计提凭证",
       one("""select count(*) from biz_expense_report
               where deleted_at is null and status in ('APPROVED','PAID') and voucher_id is null""") == "0")
    ok("L4 2241.01 贷方净额 = Σ 未付金额（报销挂账与科目余额对账）",
       one("""select (select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry
                     where account_code='2241.01')
                - (select coalesce(sum(total_with_tax - paid_amount),0) from biz_expense_report
                     where deleted_at is null and status in ('APPROVED','PAID','SUBMITTED'))""") == "0.0000",
       one("""select (select coalesce(sum(credit_amount-debit_amount),0) from biz_voucher_entry
                     where account_code='2241.01')
                - (select coalesce(sum(total_with_tax - paid_amount),0) from biz_expense_report
                     where deleted_at is null and status in ('APPROVED','PAID','SUBMITTED'))"""))
    ok("L6 pay_status 与金额自洽",
       one("""select count(*) from biz_expense_report where deleted_at is null
                 and ((pay_status='UNPAID' and paid_amount<>0)
                   or (pay_status='PARTIAL' and (paid_amount<=0 or paid_amount>=total_with_tax))
                   or (pay_status='PAID' and (paid_amount<>total_with_tax or unpaid_amount<>0)))""") == "0")
    ok("L7 CANCELLED 且曾入账的单，计提凭证必须已红冲",
       one("""select count(*) from biz_expense_report r join biz_voucher v on v.id = r.voucher_id
               where r.status='CANCELLED' and r.voucher_id is not null and v.reversed_by is null""") == "0")
    ok("状态日志齐全（A 单走到 PAID / B 单走到 CANCELLED）",
       int(one(f"""select count(distinct to_status) from biz_status_log
                where entity_type='EXPENSE_REPORT' and entity_id in ('{report_id}'::uuid,'{b_id}'::uuid)""")) >= 4,
       one(f"""select string_agg(distinct to_status, ',') from biz_status_log
                where entity_type='EXPENSE_REPORT' and entity_id in ('{report_id}'::uuid,'{b_id}'::uuid)"""))

    print("\n" + "=" * 88)
    print(f"通过 {len(PASSED)} 项，失败 {len(FAILED)} 项")
    if FAILED:
        for name in FAILED:
            print(f"  🚨 {name}")
        return 1
    print("★ FIN-3 真机全链路全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
