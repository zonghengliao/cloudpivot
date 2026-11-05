#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""FIN-3 缺陷回归（t_b9c992d1）：**撤回后重新提交 500**。

缺陷：`biz_budget_usage` 的唯一索引 `uk_biz_budget_usage_source` 的谓词只有
`deleted_at IS NULL`，撤回只把行置 `RELEASED` 不删行 → 再次提交会再插一条 `OCCUPIED`
撞唯一键；PG 里第一行 23505 把提交事务打成 aborted，第二行只报 `25P02`，整个提交 500、
单据卡在 DRAFT。

本脚本走真机 HTTP（+ 直接查库判据，不信接口自述）：

  A) 一张两明细的单，连续 **3 轮「提交 → 撤回」** 后再提交：每轮都必须 200，
     且库里 usage 恒为 **2 行**（差旅 3180 / 招待 1060 各一行），状态 OCCUPIED↔RELEASED 往返；
  B) 第 4 次提交后走完审批 → APPROVED：usage 变 CONSUMED 仍 2 行、合计 = 4240、
     单据绑定计提凭证；
  C) 草稿作废（DRAFT→CANCELLED）后再提交 → **409**（状态机不因修法放宽）；
  D) 审批中作废（SUBMITTED→CANCELLED）后再提交 → **409**，usage 保持 RELEASED 2 行；
  E) 全库五元组唯一性复核：不存在 (source_type, source_id, dept_id, account_id, fiscal_year)
     重复的有效行。

用法：
    python3 scripts/verify/fin3_resubmit_e2e.py [base_url]
默认 http://127.0.0.1:18027；库断言走 NIK_DATABASE_*（默认 alun/alun/cloudpivot_fin3fe）。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18027"
DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "alun"),
    name=os.environ.get("NIK_DATABASE_NAME", "cloudpivot_fin3fe"),
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

    def call(self, method: str, path: str, body=None) -> tuple[int, dict]:
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.base + path, data=data, method=method)
        req.add_header("Content-Type", "application/json")
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return resp.status, json.loads(resp.read().decode())
        except urllib.error.HTTPError as err:
            raw = err.read().decode()
            try:
                return err.code, json.loads(raw)
            except Exception:
                return err.code, {"raw": raw[:400]}

    def login(self, user: str, pwd: str) -> None:
        status, payload = self.call("POST", "/api/system/auth/login",
                                    {"username": user, "password": pwd})
        if status != 200:
            raise SystemExit(f"登录失败：{status} {payload}")
        self.token = payload["data"]["access_token"]


def ensure_fixtures() -> str:
    """夹具：验收部门 + admin 挂部门 + 审批人 fin3mgr（与 fin3_e2e.py 同口径）。"""
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
    # 模板库里别的卡留下的超管（如 fin2gm）会让「直属上级」这类节点多派一个待办，
    # 把用例卡在别人的待办上。本库是一次性夹具库，直接解绑（与 fin3_e2e.py 同口径）。
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
    data = payload.get("data") or {}
    report = data.get("report") or {}
    return report.get("id", "")


def usage_rows(report_id: str):
    """这张单的全部有效 usage 行：(id, account_id, status, amount, release_reason)。"""
    return q(f"""
        select id, account_id, status, amount::text, coalesce(release_reason, '')
          from biz_budget_usage
         where source_type = 'EXPENSE_REPORT' and source_id = '{report_id}'::uuid
           and deleted_at is null
         order by account_id
    """)


def submit(cli: Client, report_id: str):
    return cli.call("POST", f"/api/business/erp/expense/{report_id}/submit", {})


def withdraw(cli: Client, report_id: str):
    return cli.call("POST", f"/api/business/erp/expense/{report_id}/withdraw", {})


def status_of(report_id: str) -> str:
    return one(f"select status from biz_expense_report where id = '{report_id}'::uuid")


def approve_all(client: Client, report_id: str, comment: str = "同意报销") -> int:
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
            status, _payload = client.call(
                "POST", f"/api/business/flow/task/{task_id}/approve", {"comment": comment})
            if status == 200:
                approved += 1
    return approved


def create_report(cli: Client, types: dict, title: str):
    status, payload = cli.call("POST", "/api/business/erp/expense", {
        "title": title,
        "expense_date": "2026-11-03",
        "items": [
            {"expense_type_id": types["TRAVEL"], "with_tax_amount": "3180", "tax_rate": "0.06",
             "invoice_no": "INV-RS-001", "invoice_type": "VAT_SPECIAL",
             "summary": "高铁（撤回重提回归）", "expense_date": "2026-11-03"},
            {"expense_type_id": types["MEAL"], "with_tax_amount": "1060", "tax_rate": "0.06",
             "invoice_no": "INV-RS-002", "invoice_type": "VAT_NORMAL",
             "summary": "客户招待（不可抵扣）", "expense_date": "2026-11-04"},
        ],
        "remark": "t_b9c992d1 撤回重提回归",
    })
    if status != 200:
        raise SystemExit(f"建单失败：{status} {payload}")
    return report_id_of(payload)


def case_a(cli: Client, types: dict) -> str:
    """A) 3 轮「提交 → 撤回」后第 4 次提交成功；usage 恒 2 行。"""
    print("\nⒶ 连续 3 轮「提交 → 撤回」→ 再提交（唯一键不再撞 RELEASED 行）")
    rid = create_report(cli, types, "撤回重提回归 A（3 轮）")
    slots = one(f"""select count(distinct account_id) from biz_expense_item
                     where report_id = '{rid}'::uuid""")
    ok("建单：2 条明细落在 2 个预算槽", slots == "2", f"{slots} 槽")
    first_ids = ""
    for round_no in range(1, 4):
        status, payload = submit(cli, rid)
        ok(f"第 {round_no} 轮提交 200", status == 200,
           f"{status} {str(payload.get('msg'))[:90]}")
        ok(f"第 {round_no} 轮提交后 SUBMITTED", status_of(rid) == "SUBMITTED")
        rows = usage_rows(rid)
        ok(f"第 {round_no} 轮 usage 仍 2 行且全 OCCUPIED",
           len(rows) == 2 and all(r[2] == "OCCUPIED" for r in rows), str(rows))
        ids = ",".join(sorted(r[0] for r in rows))
        if round_no == 1:
            first_ids = ids
        ok(f"第 {round_no} 轮复用同一批 usage 行（不新插）", ids == first_ids,
           f"{ids[:40]}…")
        status, payload = withdraw(cli, rid)
        ok(f"第 {round_no} 轮撤回 200", status == 200,
           f"{status} {str(payload.get('msg'))[:90]}")
        ok(f"第 {round_no} 轮撤回后 DRAFT", status_of(rid) == "DRAFT")
        rows = usage_rows(rid)
        ok(f"第 {round_no} 轮撤回后 usage 仍 2 行且全 RELEASED(WITHDRAWN)",
           len(rows) == 2 and all(r[2] == "RELEASED" and r[4] == "WITHDRAWN" for r in rows),
           str(rows))
    status, payload = submit(cli, rid)
    ok("第 4 次提交 200（旧实现此处 500：23505 → 事务 aborted → 25P02）", status == 200,
       f"{status} {str(payload.get('msg'))[:90]}")
    return rid


def case_b(mgr: Client, rid: str) -> None:
    """B) 审批通过：usage CONSUMED 仍 2 行、合计 = 价税合计、单据绑凭证。"""
    print("\nⒷ 再提交后走完审批 → APPROVED（usage 转 CONSUMED，仍 2 行）")
    hops = approve_all(mgr, rid, "同意报销（撤回重提回归）")
    ok("待办全部批完（≥2 站）", hops >= 2, f"{hops} 站")
    ok("单据 APPROVED", status_of(rid) == "APPROVED")
    voucher = one(f"select coalesce(voucher_id::text, '') from biz_expense_report "
                  f"where id = '{rid}'::uuid")
    ok("APPROVED 必绑计提凭证", voucher not in ("", "-"), voucher)
    rows = usage_rows(rid)
    ok("usage 仍 2 行且全 CONSUMED", len(rows) == 2 and all(r[2] == "CONSUMED" for r in rows),
       str(rows))
    total = one(f"""select coalesce(sum(amount), 0)::text from biz_budget_usage
                     where source_type = 'EXPENSE_REPORT' and source_id = '{rid}'::uuid
                       and status in ('OCCUPIED','CONSUMED') and deleted_at is null""")
    ok("占用合计 = 单头价税合计 4240（L8 口径）", total in ("4240.0000", "4240"), total)


def case_c(cli: Client, types: dict) -> None:
    """C) 草稿作废后再提交 → 409。"""
    print("\nⒸ 草稿作废（DRAFT→CANCELLED）后再提交必须 409")
    rid = create_report(cli, types, "撤回重提回归 C（草稿作废）")
    status, payload = cli.call("POST", f"/api/business/erp/expense/{rid}/cancel",
                               {"reason": "单据填错部门，整单作废重开"})
    ok("草稿作废 200", status == 200, f"{status} {str(payload.get('msg'))[:90]}")
    ok("单据 CANCELLED", status_of(rid) == "CANCELLED")
    status, payload = submit(cli, rid)
    ok("作废后再提交 → 409（状态机不放宽）", status == 409,
       f"{status} code={payload.get('code')} {str(payload.get('msg'))[:80]}")
    ok("作废的单状态仍是 CANCELLED", status_of(rid) == "CANCELLED")


def case_d(cli: Client, types: dict) -> None:
    """D) 审批中作废后再提交 → 409，usage 保持 RELEASED。"""
    print("\nⒹ 审批中作废（SUBMITTED→CANCELLED）后再提交必须 409")
    rid = create_report(cli, types, "撤回重提回归 D（审批中作废）")
    status, payload = submit(cli, rid)
    ok("先提交 200", status == 200, f"{status} {str(payload.get('msg'))[:90]}")
    status, payload = cli.call("POST", f"/api/business/erp/expense/{rid}/cancel",
                               {"reason": "审批中作废：重复报销，走新单"})
    ok("审批中作废 200", status == 200, f"{status} code={payload.get('code')} "
                                        f"{str(payload.get('msg'))[:80]}")
    ok("单据 CANCELLED", status_of(rid) == "CANCELLED")
    rows = usage_rows(rid)
    ok("作废后 usage 2 行全 RELEASED(CANCELLED)",
       len(rows) == 2 and all(r[2] == "RELEASED" and r[4] == "CANCELLED" for r in rows), str(rows))
    status, payload = submit(cli, rid)
    ok("作废后再提交 → 409", status == 409,
       f"{status} code={payload.get('code')} {str(payload.get('msg'))[:80]}")
    ok("作废的 usage 行数没变（仍是 2 行，没有被修法偷偷复活）", len(usage_rows(rid)) == 2)


def case_f(cli: Client, types: dict) -> None:
    """F) 撤回后**改明细科目**再提交：旧槽留着 RELEASED 留痕，新槽插一条 OCCUPIED。"""
    print("\nⒻ 撤回后改费用科目（预算槽变了）再提交：旧行留痕 + 新行占用")
    rid = create_report(cli, types, "撤回重提回归 F（换科目）")
    status, _ = submit(cli, rid)
    ok("先提交 200", status == 200, str(status))
    status, _ = withdraw(cli, rid)
    ok("撤回 200", status == 200, str(status))
    status, payload = cli.call("PUT", f"/api/business/erp/expense/{rid}", {
        "title": "撤回重提回归 F（换科目→招待费）",
        "expense_date": "2026-11-03",
        "items": [{"expense_type_id": types["MEAL"], "with_tax_amount": "1060",
                   "tax_rate": "0.06", "invoice_no": "INV-RS-F1", "invoice_type": "VAT_NORMAL",
                   "summary": "把差旅改成招待费（槽变了）", "expense_date": "2026-11-04"}],
    })
    ok("草稿改明细 200（换槽）", status == 200, f"{status} {str(payload.get('msg'))[:90]}")
    status, payload = submit(cli, rid)
    ok("换槽后再提交 200（不能撞旧科目的 RELEASED 行）", status == 200,
       f"{status} {str(payload.get('msg'))[:90]}")
    rows = usage_rows(rid)
    travel_account = one("select coalesce(account_id::text,'') from biz_expense_type "
                         "where type_code = 'TRAVEL'")
    meal_account = one("select coalesce(account_id::text,'') from biz_expense_type "
                       "where type_code = 'MEAL'")
    ok("usage = 旧槽 RELEASED + 新槽 OCCUPIED 各 1 行（MEAL 行被复用而非新插）",
       len(rows) == 2
       and sorted(r[2] for r in rows) == ["OCCUPIED", "RELEASED"]
       and {r[1] for r in rows} == {travel_account, meal_account}
       and [r[1] for r in rows if r[2] == "OCCUPIED"] == [meal_account]
       and [r[1] for r in rows if r[2] == "RELEASED"] == [travel_account],
       str(rows))
    occupied = sum(float(r[3]) for r in rows if r[2] == "OCCUPIED")
    ok("有效占用合计 = 改后明细 1060（L8 口径）", abs(occupied - 1060.0) < 0.005, f"{occupied}")


def case_e() -> None:
    """E) 全库唯一性复核：有效 usage 行不允许五元组重复。"""
    dupes = q("""
        select source_type, source_id::text, dept_id::text, account_id::text,
               fiscal_year::text, count(*)::text
          from biz_budget_usage
         where deleted_at is null
         group by 1, 2, 3, 4, 5
        having count(*) > 1
    """)
    ok("无重复 usage 行（同单同槽只有一行）", not dupes, str(dupes[:3]))
    orphan = q("""
        select r.report_no
          from biz_expense_report r
          join biz_budget_usage u on u.source_id = r.id and u.source_type = 'EXPENSE_REPORT'
         where r.deleted_at is null and r.status in ('DRAFT','CANCELLED')
           and u.status = 'OCCUPIED' and u.deleted_at is null
    """)
    ok("DRAFT/CANCELLED 的单不许挂着 OCCUPIED 占用", not orphan, str(orphan[:3]))


def main() -> int:
    cli = Client(BASE)
    print("=" * 88)
    print("FIN-3 撤回后重新提交回归（t_b9c992d1）")
    print(f"实例 {BASE}  库 {DB['name']}")
    print("=" * 88)
    ensure_fixtures()
    cli.login(*ADMIN)
    mgr = Client(BASE)
    mgr.login(*APPROVER)
    types = {row[0]: row[1] for row in q(
        "select type_code, id from biz_expense_type where deleted_at is null")}
    if "TRAVEL" not in types or "MEAL" not in types:
        raise SystemExit("缺费用类型种子（TRAVEL / MEAL），先跑 fin3_dbsetup.sh")
    rid_a = case_a(cli, types)
    case_b(mgr, rid_a)
    case_c(cli, types)
    case_d(cli, types)
    case_f(cli, types)
    case_e()
    print("\n" + "=" * 88)
    print(f"通过 {len(PASSED)} 条 / 失败 {len(FAILED)} 条")
    for name in FAILED:
        print(f"  🚨 {name}")
    print("=" * 88)
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
