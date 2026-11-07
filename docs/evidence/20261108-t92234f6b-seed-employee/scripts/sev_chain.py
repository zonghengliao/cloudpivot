#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""t_92234f6b 真机验收：员工自助提报全链（demo_sales 提报 → demo_lead 审批 → demo_finance 付款）

用法：
  BASE=http://127.0.0.1:8053 DB=cloudpivot_seed_emp PGPASSWORD=... \
  ADMIN_PW=... python3 sev_chain.py

断言：
  ① N2 = 0（所有在用 HUMAN 账号都有部门；含 admin 与 9 个演示账号）
  ② demo_sales（EMPLOYEE + SALES_CLERK，**非负责人**）能建单 + 提交自己的报销单 → 200
  ③ demo_lead（直属上级）在待办里看到该单并审批通过 → 单据 APPROVED
  ④ demo_finance 付款 → PAID + 凭证借贷平衡
  ⑤ 反向：demo_sales 碰付款 / 红冲作废 → 403（没有过度授权）
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("BASE", "http://127.0.0.1:18134")
DB = os.environ["DB"]
CHANGED_PW = os.environ.get("CHANGED_PW", "SeedEmp@2026")
ADMIN_PW = os.environ.get("ADMIN_PW", "Admin@123456")
DEMO_PW = os.environ.get("DEMO_PW", "ChangeMe@2026")

PASS = {"ok": 0, "bad": 0}
LINES = []


def say(msg=""):
    print(msg, flush=True)


def ok(name, detail=""):
    PASS["ok"] += 1
    LINES.append(f"PASS | {name} | {detail}")
    say(f"  ✅ {name}" + (f"  [{detail}]" if detail else ""))


def bad(name, detail=""):
    PASS["bad"] += 1
    LINES.append(f"FAIL | {name} | {detail}")
    say(f"  ❌ {name}" + (f"  [{detail}]" if detail else ""))


def eq(name, got, want):
    if got == want:
        ok(name, f"{got}")
    else:
        bad(name, f"got={got!r} want={want!r}")


def sq(sql):
    """只读/写单条标量的 psql 直查（脚本与库同机）。"""
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", os.environ.get("PGUSER", "cloudpivot"),
         "-d", DB, "-At", "-F", "|", "-c", sql],
        capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"psql 失败：{r.stderr.strip()}")
    return r.stdout.strip()


def api(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + "/api" + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            raw = r.read()
        out = (200, json.loads(raw or b"{}"))
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            out = (e.code, json.loads(raw or b"{}"))
        except Exception:
            out = (e.code, {"raw": raw.decode("utf-8", "ignore")})
    return out


def data_of(r):
    d = r.get("data")
    return d if isinstance(d, dict) else {}


def login(user, pw):
    st, r = api("POST", "/system/auth/login", None, {"username": user, "password": pw})
    if st != 200 or r.get("code") != 0:
        bad(f"登录 {user}", f"http={st} code={r.get('code')} msg={r.get('msg')}")
        return None
    d = data_of(r)
    if d.get("must_change_password"):
        st2, r2 = api("PUT", "/system/auth/password", d["access_token"],
                      {"old_password": pw, "new_password": CHANGED_PW})
        if st2 != 200 or r2.get("code") != 0:
            bad(f"{user} 强制改密", f"http={st2} code={r2.get('code')} msg={r2.get('msg')}")
            return None
        st, r = api("POST", "/system/auth/login", None, {"username": user, "password": CHANGED_PW})
        if st != 200 or r.get("code") != 0:
            bad(f"{user} 改密后登录", f"http={st} code={r.get('code')}")
            return None
        d = data_of(r)
        ok(f"{user} 首次登录强制改密 + 重登", "must_change_password 流程真实走通")
    return d["access_token"]


def main():
    say("=" * 78)
    say(f"t_92234f6b 员工自助提报全链  BASE={BASE} DB={DB}")
    say("=" * 78)

    # ---------- ① 部门归属 / N2 ----------
    say("\n[①] 在用 HUMAN 账号的部门归属 + N2 不变量")
    rows = sq("""select u.username || '|' || coalesce(d.dept_code,'<NULL>')
                   from sys_user u left join sys_dept d on d.id = u.dept_id
                  where u.deleted_at is null order by u.username""").splitlines()
    for row in rows:
        say(f"    {row}")
    n2 = int(sq("""select count(*) from sys_user u
                    where u.status = 1 and u.is_locked = false and u.user_type = 'HUMAN'
                      and u.deleted_at is null and u.dept_id is null"""))
    eq("① N2 不变量：在用 HUMAN 账号无部门数 = 0", n2, 0)
    nulls = [r for r in rows if r.endswith("|") or r.endswith("<NULL>")]
    eq("① 9 个演示账号 + admin 全部挂 DEPT_DEMO", len(nulls), 0)
    total_users = len(rows)
    dept_users = len([r for r in rows if r.endswith("|DEPT_DEMO")])
    eq(f"① 账号数 = 部门归属数（{total_users} 个）", dept_users, total_users)
    eq("① 演示部门负责人 = demo_lead",
       sq("select coalesce(u.username,'<NULL>') from sys_dept d left join sys_user u on u.id=d.leader_id "
          "where d.dept_code='DEPT_DEMO'"), "demo_lead")

    # ---------- 授权侧：EMPLOYEE 角色 ----------
    say("\n[授权] EMPLOYEE 角色与授权行")
    eq("EMPLOYEE 角色存在且启用",
       sq("select role_code || '|' || role_name || '|' || data_scope || '|' || status "
          "from sys_role where role_code='EMPLOYEE' and deleted_at is null"), "EMPLOYEE|员工自助|4|1")
    for code in ["business:erp:expense:list", "business:erp:expense:create",
                 "business:erp:expense:update", "business:erp:expense:delete",
                 "business:erp:expense:submit", "business:erp:expense:detail",
                 "business:erp:expense-type:list", "business:flow:instance:read"]:
        n = int(sq(f"select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id "
                   f"join sys_menu m on m.id=rm.menu_id where r.role_code='EMPLOYEE' "
                   f"and m.auth_code='{code}' and m.deleted_at is null"))
        eq(f"EMPLOYEE 持有 {code}", n >= 1, True)
    eq("EMPLOYEE 未持有 expense:pay（不碰钱）",
       int(sq("select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id "
              "join sys_menu m on m.id=rm.menu_id where r.role_code='EMPLOYEE' "
              "and m.auth_code='business:erp:expense:pay'")), 0)
    eq("demo_sales 的角色 = EMPLOYEE + SALES_CLERK",
       sq("select string_agg(r.role_code, ',' order by r.role_code) from sys_user u "
          "join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id "
          "where u.username='demo_sales'"), "EMPLOYEE,SALES_CLERK")

    # ---------- 登录 ----------
    say("\n[登录] admin / demo_sales / demo_lead / demo_finance / demo_finance_mgr")
    sq("update sys_user set login_fail_count=0, is_locked=false, lock_time=null")
    tokens = {}
    for user, pw in [("admin", ADMIN_PW), ("demo_sales", DEMO_PW), ("demo_lead", DEMO_PW),
                     ("demo_finance", DEMO_PW), ("demo_finance_mgr", DEMO_PW)]:
        tok = login(user, pw)
        if not tok:
            say("❌ 令牌不齐，链路断言无意义 —— 退出 1")
            return 1
        tokens[user] = tok
        time.sleep(0.4)

    # ---------- ② demo_sales 建单 + 提交 ----------
    say("\n[②] demo_sales（普通员工）自助建单 + 提交")
    st, r = api("GET", "/business/erp/expense-type/list", tokens["demo_sales"])
    eq("② 员工能取费用类型下拉（expense-type:list）http", st, 200)
    types = data_of(r).get("list") or data_of(r).get("items") or []
    etp = sq("select id from biz_expense_type where type_code='TRAVEL' and deleted_at is null limit 1")
    if not etp:
        bad("费用类型 TRAVEL 不存在（种子缺失）")
        return 1
    say(f"    费用类型下拉条数 = {len(types) if isinstance(types, list) else 'n/a'}")

    st, r = api("POST", "/business/erp/expense", tokens["demo_sales"], {
        "title": "SEEDEMP 员工自助报销 300",
        "expense_date": "2026-11-08",
        "items": [{"expense_type_id": etp, "with_tax_amount": 300, "tax_rate": 0,
                   "summary": "SEEDEMP 打车费", "invoice_no": "SEEDEMP-INV-0001",
                   "invoice_date": "2026-11-08", "invoice_type": "VAT_NORMAL"}],
    })
    eq("② 员工建单 http", st, 200)
    rep = data_of(r).get("report") or data_of(r)
    rid = rep.get("id")
    say(f"    单号 = {rep.get('report_no')}  id={rid}")
    me = sq("select id from sys_user where username='demo_sales'")
    dept = sq("select id from sys_dept where dept_code='DEPT_DEMO'")
    eq("② 建单人（report_id 默认取当前用户）= demo_sales",
       sq(f"select reporter_id::text from biz_expense_report where id='{rid}'"), me)
    eq("② 单据部门 = DEPT_DEMO（不是 NULL）",
       sq(f"select dept_id::text from biz_expense_report where id='{rid}'"), dept)

    st, r = api("POST", f"/business/erp/expense/{rid}/submit", tokens["demo_sales"], {})
    eq("② 员工本人提交审批 http（曾经 403）", st, 200)
    eq("② 提交后单据 SUBMITTED",
       sq(f"select status from biz_expense_report where id='{rid}'"), "SUBMITTED")
    inst = sq(f"select id from biz_flow_instance where biz_id='{rid}' order by created_at desc limit 1")
    eq("② 生成审批实例", bool(inst), True)
    who = sq(f"select coalesce(u.username,'<NULL>') from biz_flow_task t "
             f"left join sys_user u on u.id=t.assignee_id "
             f"where t.instance_id='{inst}' and t.status='PENDING' order by t.created_at limit 1")
    eq("② 待办审批人 = 直属上级 demo_lead（INITIATOR_SUPERIOR 解析成功）", who, "demo_lead")

    # ---------- ③ demo_lead 待办里看到并审批 ----------
    say("\n[③] demo_lead 在待办里看到该单并审批通过")
    st, r = api("GET", "/business/flow/task/todo?page=1&page_size=100", tokens["demo_lead"])
    eq("③ 部门经理待办列表 http", st, 200)
    todo = data_of(r).get("list") or data_of(r).get("records") or []
    ids = {str(t.get("instance_id") or t.get("instanceId") or "") for t in todo} if isinstance(todo, list) else set()
    say(f"    待办条数 = {len(todo) if isinstance(todo, list) else 'n/a'}")
    eq("③ 待办里能看到该实例", inst in ids, True)
    task = sq(f"select id from biz_flow_task where instance_id='{inst}' and status='PENDING' "
              f"order by created_at limit 1")
    st, r = api("POST", f"/business/flow/task/{task}/approve", tokens["demo_lead"],
                {"comment": "SEEDEMP 部门经理同意（员工自助链）"})
    eq("③ 部门经理审批 http", st, 200)
    eq("③ 单据 APPROVED",
       sq(f"select status from biz_expense_report where id='{rid}'"), "APPROVED")

    # ---------- ④ demo_finance 付款 ----------
    say("\n[④] demo_finance 付款 → PAID + 凭证借贷平衡")
    st, r = api("POST", f"/business/erp/expense/{rid}/pay", tokens["demo_finance"], {
        "amount": 300, "pay_date": "2026-11-08", "pay_method": "BANK_TRANSFER",
        "pay_account_code": "1002"})
    eq("④ 财务付款 http", st, 200)
    eq("④ 单据 pay_status = PAID",
       sq(f"select pay_status from biz_expense_report where id='{rid}'"), "PAID")
    eq("④ 全库已过账凭证借贷平衡（差异行 = 0）",
       sq("select count(*) from biz_voucher where status='POSTED' and total_debit<>total_credit"), "0")
    eq("④ 该单已生成付款凭证",
       int(sq(f"select count(*) from biz_expense_payment where report_id='{rid}'")) >= 1, True)

    # ---------- ⑤ 反向：员工不越权 ----------
    say("\n[⑤] 反向断言：员工没有付款 / 红冲作废权")
    st, r = api("POST", f"/business/erp/expense/{rid}/pay", tokens["demo_sales"], {
        "amount": 300, "pay_date": "2026-11-08", "pay_method": "BANK_TRANSFER",
        "pay_account_code": "1002"})
    eq("⑤ demo_sales 付款 → 403", st, 403)
    st, r = api("POST", f"/business/erp/expense/{rid}/cancel", tokens["demo_sales"],
                {"reason": "SEEDEMP 越权探测"})
    eq("⑤ demo_sales 作废（红冲）→ 403", st, 403)
    st, r = api("DELETE", f"/business/erp/expense/{rid}", tokens["demo_sales"])
    eq("⑤ demo_sales 删已审批单 → 非 200（状态机拒绝）", st != 200, True)

    say("\n" + "=" * 78)
    say(f"结果：{PASS['ok']} 通过 / {PASS['bad']} 失败")
    say("=" * 78)
    for line in LINES:
        say(line)
    return 0 if PASS["bad"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
