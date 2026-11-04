#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-2 真机验收：业务提醒任务包（应付到期 / 采购超期 / 合同到期 / 低库存）。

跑在隔离实例上（默认 127.0.0.1:18098 / 库 cp_p12），全部走真实 HTTP：
  ① 种子与结构：4 个模板 / 4 条 sys_job（cron 逐字）/ 1 个权限码 / 2 个参数 / 台账表 + 唯一索引
  ② 应付到期：造一张「3 天后到期」的应付单 → 手动触发 → 收到通知（单号 / 到期日 / 该做什么）
  ③ 幂等：同一张单同一天连跑 3 次 → 只发 1 条（第 2、3 次 sent=0 skipped_reminded=1）
  ④ 低库存：造一个「库存 < 安全库存」的物料 → 触发 → 仓管主管收到
  ⑤ 采购超期 / 合同到期：各造一张 → 触发 → 各收到 1 条
  ⑥ sys_job 手动执行：/support/job/{id}/trigger 走到 run_handler（不是 422）
  ⑦ kind=all 汇总 + 非法 kind → 400
  ⑧ 无权限用户 → 403（权限码真的在拦）

用法：python3 scripts/verify/p12_business_reminder_e2e.py
退出码：0 全过 / 1 有失败。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import datetime as dt
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("P12_BASE", "http://127.0.0.1:18098")
DB = dict(
    host=os.environ.get("P12_DB_HOST", "127.0.0.1"),
    port=os.environ.get("P12_DB_PORT", "5432"),
    user=os.environ.get("P12_DB_USER", "alun"),
    name=os.environ.get("P12_DB_NAME", "cp_p12"),
    password=os.environ.get("P12_DB_PASSWORD", "alun"),
)
ADMIN_USER = os.environ.get("P12_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("P12_ADMIN_PASS", "Admin@123456")
NEW_PASS = os.environ.get("P12_NEW_PASS", "P12@Passw0rd!")
FINANCE_USER = "p12_finance"
KEEPER_USER = "p12_keeper"
BUYER_USER = "p12_buyer"
LIMITED_USER = "p12_limited"

TZ = dt.timezone(dt.timedelta(hours=8))
TODAY = dt.datetime.now(TZ).date()

PASSED = []
FAILED = []


def check(ok, name, detail=""):
    (PASSED if ok else FAILED).append(name)
    print(("[PASS] " if ok else "[FAIL] ") + name + ("" if ok else f" — {detail}"))
    return ok


def psql(sql):
    args = ["psql", "-h", DB["host"], "-p", DB["port"], "-U", DB["user"], "-d", DB["name"],
            "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tAc", sql]
    out = subprocess.run(args, capture_output=True, text=True,
                         env=dict(os.environ, PGPASSWORD=DB["password"]), check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}\nSQL: {sql}")
    return out.stdout.strip()


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(username, password):
    """登录（自动处理「首登强制改密」与「口令已改过」两种状态）。"""
    status, body, token, used = 0, {}, "", password
    for candidate in (password, ADMIN_PASS, NEW_PASS):
        status, body = call("POST", "/api/system/auth/login",
                            {"username": username, "password": candidate})
        token = (body.get("data") or {}).get("access_token") or ""
        if token:
            used = candidate
            break
    if not token:
        return status, body, ""
    if (body.get("data") or {}).get("must_change_password") and used != NEW_PASS:
        st, resp = call("PUT", "/api/system/auth/password",
                        {"old_password": used, "new_password": NEW_PASS}, token)
        if st != 200:
            raise SystemExit(f"{username} 改初始口令失败：{st} {resp}")
        status, body = call("POST", "/api/system/auth/login",
                            {"username": username, "password": NEW_PASS})
        token = (body.get("data") or {}).get("access_token") or ""
    return status, body, token


# ==================== ① 夹具与清场 ====================

def cleanup():
    for stmt in [
        "DELETE FROM sys_notice WHERE biz_type IN ('payable_due','po_overdue','contract_expire','low_stock')",
        "DELETE FROM biz_reminder_log",
        "DELETE FROM biz_payable WHERE payable_no LIKE 'P12-%'",
        "DELETE FROM biz_purchase_order WHERE order_no LIKE 'P12-%'",
        "DELETE FROM biz_contract WHERE contract_no LIKE 'P12-%'",
        "DELETE FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'P12-%')",
        "DELETE FROM biz_material WHERE material_code LIKE 'P12-%'",
        "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'P12-%'",
        "DELETE FROM biz_unit WHERE unit_code LIKE 'P12-%'",
        "DELETE FROM biz_supplier WHERE supplier_code LIKE 'P12-%'",
        "DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'p12\\_%')",
        "DELETE FROM sys_user WHERE username LIKE 'p12\\_%'",
    ]:
        psql(stmt)


def seed_user(username, real_name, roles):
    """造夹具用户：口令哈希抄 admin（同一口令可登录），挂指定角色。"""
    psql(
        "INSERT INTO sys_user (id, username, password, real_name, status, must_change_password) "
        f"SELECT gen_random_uuid(), '{username}', u.password, '{real_name}', 1, false "
        "FROM sys_user u WHERE u.username = 'admin' "
        f"AND NOT EXISTS (SELECT 1 FROM sys_user WHERE username = '{username}')"
    )
    for role in roles:
        psql(
            "INSERT INTO sys_user_role (user_id, role_id) "
            f"SELECT (SELECT id FROM sys_user WHERE username = '{username}'), r.id FROM sys_role r "
            f"WHERE r.role_code = '{role}' AND r.deleted_at IS NULL "
            "AND NOT EXISTS (SELECT 1 FROM sys_user_role x "
            f"  WHERE x.user_id = (SELECT id FROM sys_user WHERE username = '{username}') "
            "    AND x.role_id = r.id)"
        )


def seed_master():
    psql(
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status) "
        "VALUES (gen_random_uuid(), 'P12-PCS', '个', 1, 1, 1) "
        "ON CONFLICT DO NOTHING"
    )
    psql(
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, "
        "                           warehouse_path, status) "
        "VALUES (gen_random_uuid(), 'P12-WH', 'P12 验收仓', 'NORMAL', '/P12-WH', 1) "
        "ON CONFLICT DO NOTHING"
    )
    psql(
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days) "
        "VALUES (gen_random_uuid(), 'P12-SUP', 'P12 验收供应商', 1, 30) "
        "ON CONFLICT DO NOTHING"
    )
    return psql("SELECT id FROM biz_supplier WHERE supplier_code = 'P12-SUP' AND deleted_at IS NULL")


def seed_payable(no, days_left, supplier_id):
    due = TODAY + dt.timedelta(days=days_left)
    psql(
        "INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no, "
        "                         amount, paid_amount, unpaid_amount, due_date, period_code, status, create_by) "
        f"VALUES (gen_random_uuid(), '{no}', '{supplier_id}', 'STOCK_IN', gen_random_uuid(), "
        f"        '{no}-SRC', 12345.6, 0, 12345.6, '{due}', '2039-01', 'OPEN', "
        f"        (SELECT id FROM sys_user WHERE username = '{BUYER_USER}'))"
    )
    return psql(f"SELECT id FROM biz_payable WHERE payable_no = '{no}'")


def seed_purchase_order(no, overdue_days, supplier_id):
    expect = TODAY - dt.timedelta(days=overdue_days)
    psql(
        "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, expect_date, "
        "                                warehouse_id, status, create_by) "
        f"VALUES (gen_random_uuid(), '{no}', '{supplier_id}', CURRENT_DATE - 30, '{expect}', "
        f"        (SELECT id FROM biz_warehouse WHERE warehouse_code = 'P12-WH'), 'APPROVED', "
        f"        (SELECT id FROM sys_user WHERE username = '{BUYER_USER}'))"
    )
    return psql(f"SELECT id FROM biz_purchase_order WHERE order_no = '{no}'")


def seed_contract(no, days_left):
    end = TODAY + dt.timedelta(days=days_left)
    start = end - dt.timedelta(days=365)
    psql(
        "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, "
        "                          party_b_id, party_b_name, amount, sign_date, start_date, end_date, "
        "                          status, owner_id, create_by) "
        f"VALUES (gen_random_uuid(), '{no}', 'P12 验收采购合同', 'PURCHASE', 'SUPPLIER', "
        f"        gen_random_uuid(), 'P12 验收供应商', 1000, '{start}', '{start}', '{end}', 'ACTIVE', "
        f"        (SELECT id FROM sys_user WHERE username = '{BUYER_USER}'), "
        f"        (SELECT id FROM sys_user WHERE username = '{BUYER_USER}'))"
    )
    return psql(f"SELECT id FROM biz_contract WHERE contract_no = '{no}'")


def seed_low_stock(code):
    psql(
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, "
        "                          safety_stock, costing_method, status) "
        f"VALUES (gen_random_uuid(), '{code}', 'P12 验收物料', "
        "        (SELECT id FROM biz_unit WHERE unit_code = 'P12-PCS'), 'RAW', 50, 'MOVING_AVG', 1)"
    )
    material = psql(f"SELECT id FROM biz_material WHERE material_code = '{code}'")
    psql(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount) "
        f"VALUES (gen_random_uuid(), '{material}', "
        "        (SELECT id FROM biz_warehouse WHERE warehouse_code = 'P12-WH'), 12, 0, 1, 12)"
    )
    return material, psql(
        "SELECT i.id FROM biz_inventory i JOIN biz_material m ON m.id = i.material_id "
        f"WHERE m.material_code = '{code}'"
    )


# ==================== 断言辅助 ====================

def notice_rows(biz_type, biz_id):
    return int(psql(
        "SELECT COUNT(*) FROM sys_notice "
        f"WHERE biz_type = '{biz_type}' AND biz_id = '{biz_id}'"
    ) or 0)


def notice_first(biz_type, biz_id):
    out = psql(
        "SELECT title || E'\\n' || content FROM sys_notice "
        f"WHERE biz_type = '{biz_type}' AND biz_id = '{biz_id}' ORDER BY created_at LIMIT 1"
    )
    return out


def notice_receivers(biz_type, biz_id):
    out = psql(
        "SELECT string_agg(u.username, ',') FROM sys_notice n "
        "JOIN sys_notice_receiver r ON r.notice_id = n.id "
        "JOIN sys_user u ON u.id = r.user_id "
        f"WHERE n.biz_type = '{biz_type}' AND n.biz_id = '{biz_id}'"
    )
    return out or ""


def ledger_rows(kind, doc_id):
    return int(psql(
        "SELECT COUNT(*) FROM biz_reminder_log "
        f"WHERE remind_kind = '{kind}' AND doc_id = '{doc_id}'"
    ) or 0)


def run_kind(kind, token):
    return call("POST", f"/api/business/erp/reminder/{kind}/run", None, token)


def single_run(body, kind):
    for run in (body.get("data") or {}).get("runs") or []:
        if run.get("kind") == kind:
            return run
    return {}


# ==================== 主流程 ====================

def main():
    print(f"== P1-2 真机验收：BASE={BASE} DB={DB['name']} 业务日={TODAY}")
    cleanup()
    supplier = seed_master()
    seed_user(FINANCE_USER, "P12 财务经理", ["FINANCE_MANAGER"])
    seed_user(KEEPER_USER, "P12 仓管主管", ["WAREHOUSE_MANAGER"])
    seed_user(BUYER_USER, "P12 采购员", ["PURCHASE_MANAGER"])
    seed_user(LIMITED_USER, "P12 无权限用户", ["DEPT_LEADER"])

    # ---------- ① 种子与结构 ----------
    n_tpl = int(psql("SELECT COUNT(*) FROM sys_notice_template WHERE deleted_at IS NULL "
                     "AND template_code IN ('ERP_REMINDER_PAYABLE_DUE','ERP_REMINDER_PO_OVERDUE',"
                     "'ERP_REMINDER_CONTRACT_EXPIRE','ERP_REMINDER_LOW_STOCK')") or 0)
    check(n_tpl == 4, "①a 4 个通知模板已种", f"实际 {n_tpl}")

    jobs = psql("SELECT job_name || '|' || cron_expr || '|' || invoke_target || '|' || status "
                "FROM sys_job WHERE job_name LIKE 'erp\\_%\\_scan' AND deleted_at IS NULL "
                "ORDER BY job_name")
    expected_jobs = {
        "erp_payable_due_scan|0 10 7 * * *|erp_payable_due_scan|0",
        "erp_po_overdue_scan|0 20 7 * * *|erp_po_overdue_scan|0",
        "erp_contract_expire_scan|0 30 7 * * *|erp_contract_expire_scan|0",
        "erp_low_stock_scan|0 40 7 * * *|erp_low_stock_scan|0",
    }
    got_jobs = set(filter(None, jobs.splitlines()))
    check(got_jobs == expected_jobs, "①b sys_job 4 条种子（名称|cron|调用目标|停用）",
          f"实际 {sorted(got_jobs)}")

    check(psql("SELECT COUNT(*) FROM sys_menu WHERE auth_code = 'business:erp:reminder:run' "
               "AND menu_type = 3") == "1", "①c 权限节点 business:erp:reminder:run 存在")
    check(psql("SELECT COUNT(*) FROM sys_config WHERE config_key LIKE 'erp.reminder.%' "
               "AND deleted_at IS NULL") == "2", "①d 2 个重提醒间隔参数存在")
    check(psql("SELECT COUNT(*) FROM pg_indexes WHERE tablename = 'biz_reminder_log' "
               "AND indexname = 'uk_biz_reminder_log_dedupe'") == "1",
          "①e 台账幂等唯一索引存在")

    # ---------- 登录 ----------
    status, body, token = login(ADMIN_USER, ADMIN_PASS)
    if not check(bool(token), "②a 管理员登录成功", f"{status} {body}"):
        return finish()
    status, _, limited_token = login(LIMITED_USER, ADMIN_PASS)
    check(bool(limited_token), "②b 无权限用户登录成功", f"{status}")

    # ---------- ③ 应付到期：真机 + 幂等 ----------
    payable = seed_payable("P12-AP-0001", 3, supplier)
    st, res = run_kind("payable_due", token)
    run = single_run(res, "payable_due")
    check(st == 200 and run.get("sent") == 1, "③a 应付到期：首次触发发出 1 条通知",
          f"{st} {res}")
    check(notice_rows("payable_due", payable) == 1, "③b 通知落库 1 条")
    text = notice_first("payable_due", payable)
    due = (TODAY + dt.timedelta(days=3)).isoformat()
    check(all(("P12-AP-0001" in text, due in text, "安排付款" in text, "3" in text)),
          "③c 通知含 单号 / 到期日 / 剩余天数 / 该做什么", text.replace("\n", " | "))
    receivers = notice_receivers("payable_due", payable)
    check(FINANCE_USER in receivers and BUYER_USER in receivers,
          "③d 收件人 = 财务经理 + 供应商对应采购员", receivers)
    check(ledger_rows("PAYABLE_DUE", payable) == 1, "③e 台账写回 1 行（已提醒时间戳）")

    for round_no in (2, 3):
        st, res = run_kind("payable_due", token)
        run = single_run(res, "payable_due")
        check(st == 200 and run.get("sent") == 0 and run.get("skipped_reminded") == 1,
              f"③f 第 {round_no} 次触发：sent=0 / 已提醒跳过=1（幂等）", f"{st} {res}")
    check(notice_rows("payable_due", payable) == 1, "③g 连跑 3 次后通知仍只有 1 条（防轰炸）")
    check(ledger_rows("PAYABLE_DUE", payable) == 1, "③h 台账仍只有 1 行")

    # ---------- ④ 低库存 ----------
    material, inventory = seed_low_stock("P12-M-0001")
    st, res = run_kind("low_stock", token)
    run = single_run(res, "low_stock")
    check(st == 200 and run.get("sent") == 1, "④a 低库存：首次触发发出 1 条", f"{st} {res}")
    text = notice_first("low_stock", inventory)
    check(all(("P12-M-0001" in text, "P12 验收仓" in text, "12" in text, "50" in text,
               "38" in text, "补货" in text)),
          "④b 通知含 物料 / 仓库 / 当前量 / 安全库存 / 缺口 / 该做什么", text.replace("\n", " | "))
    check(KEEPER_USER in notice_receivers("low_stock", inventory),
          "④c 收件人 = 仓管主管", notice_receivers("low_stock", inventory))
    st, res = run_kind("low_stock", token)
    run = single_run(res, "low_stock")
    check(run.get("sent") == 0 and run.get("skipped_reminded") == 1,
          "④d 同一天再触发：不再发（幂等）", f"{res}")

    # ---------- ⑤ 采购超期 / 合同到期 ----------
    order = seed_purchase_order("P12-PO-0001", 5, supplier)
    st, res = run_kind("po_overdue", token)
    run = single_run(res, "po_overdue")
    check(st == 200 and run.get("sent") >= 1, "⑤a 采购超期：触发发出通知", f"{st} {res}")
    text = notice_first("po_overdue", order)
    check(("P12-PO-0001" in text and "催供应商交货" in text),
          "⑤b 采购超期通知含单号 + 该做什么", text.replace("\n", " | "))
    check(KEEPER_USER not in notice_receivers("po_overdue", order)
          and BUYER_USER in notice_receivers("po_overdue", order),
          "⑤c 采购超期收件人 = 制单人（+采购经理），不发仓管", notice_receivers("po_overdue", order))

    contract = seed_contract("P12-HT-20260101-000001", 30)
    st, res = run_kind("contract_expire", token)
    run = single_run(res, "contract_expire")
    check(st == 200 and run.get("sent") >= 1, "⑤d 合同到期：触发发出通知", f"{st} {res}")
    text = notice_first("contract_expire", contract)
    end = (TODAY + dt.timedelta(days=30)).isoformat()
    check(("P12-HT-20260101-000001" in text and end in text and "续签" in text),
          "⑤e 合同到期通知含合同号 / 到期日 / 该做什么", text.replace("\n", " | "))

    # ---------- ⑥ sys_job 手动执行（走 run_handler） ----------
    job_id = psql("SELECT id FROM sys_job WHERE job_name = 'erp_payable_due_scan' "
                  "AND deleted_at IS NULL")
    st, res = call("POST", f"/api/support/job/{job_id}/trigger", None, token)
    check(st == 200, "⑥a 定时任务页「立即执行」可用（run_handler 已接上）", f"{st} {res}")
    log_msg = psql(f"SELECT COALESCE(message, '') FROM sys_job_log WHERE job_id = '{job_id}' "
                   "ORDER BY created_at DESC LIMIT 1")
    check("erp_payable_due_scan" in log_msg and "scanned=" in log_msg,
          "⑥b 执行日志里有本任务的摘要", log_msg)

    # ---------- ⑦ kind=all 与非法 kind ----------
    st, res = run_kind("all", token)
    kinds = sorted(run.get("kind") for run in (res.get("data") or {}).get("runs") or [])
    check(st == 200 and kinds == ["contract_expire", "low_stock", "payable_due", "po_overdue"],
          "⑦a kind=all 一次跑完 4 类", f"{st} {kinds}")
    st, res = run_kind("no_such_kind", token)
    check(st == 400, "⑦b 非法 kind → 400", f"{st} {res}")

    # ---------- ⑧ 权限 ----------
    st, res = run_kind("payable_due", limited_token)
    check(st == 403, "⑧ 无 business:erp:reminder:run 的用户 → 403", f"{st} {res}")

    print(f"\n== 结果：{len(PASSED)} 通过 / {len(FAILED)} 失败")
    if FAILED:
        print("失败项：")
        for name in FAILED:
            print("  -", name)
    return finish()


def finish():
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
