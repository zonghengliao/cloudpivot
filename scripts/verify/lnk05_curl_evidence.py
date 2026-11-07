#!/usr/bin/env python3
# LNK-05 真机取证：费用报销（EXPENSE_REPORT）提交 / 审批通过 / 付清 → 联动通知
#
# 走生产同款 HTTP 链路（不是直接调函数）：
#
#   配置页 wired 检查 → PUT 规则（SUBMITTED / APPROVED / PAID 三条 SEND_NOTICE
#   + 一条 APPROVED 上的 GENERATE_VOUCHER）
#   → 销售员甲建单 + 提交 → report_submit ⑥ run_excluding → commit → run_deferred
#     → sys_notice 落 LNK:EXPENSE:SUBMITTED:<id>
#   → 部门负责人（admin）审批通过 → execute_approval ① 计提凭证 ⑤ run_excluding
#     → commit → run_deferred → sys_notice 落 LNK:EXPENSE:APPROVED:<id>
#   → 财务付款全额 → report_pay ⑥ run_excluding（仅付清）→ commit → run_deferred
#     → sys_notice 落 LNK:EXPENSE:PAID:<id>
#
# 用法：python3 scripts/verify/lnk05_curl_evidence.py [base_url]
# 前置：bash scripts/verify/lnk05_start.sh（实例在 18052）+ 本机能 psql 连 cloudpivot_so44
#
# 夹具全部自建自清（部门负责人 / 报销单 / 审批实例 / 凭证 / 规则 / 通知）：
# 取证库 cloudpivot_so44 是 ERP 验收库（LNK-03 / LNK-04 也用它取证），跑完不留脏数据。
# ============================================================================
import atexit
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18052"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk05-expense/curl_evidence.txt"
DB = "cloudpivot_so44"

REPORTER = "aa440000-0000-4000-8000-000000000002"  # so44_sales_a（唯一带部门的用户）
DEPT = "aa440000-0000-4000-8000-000000000001"      # 销售一部

_STATE = {"admin": None, "sales": None, "rules": [], "report_id": None, "dept_leader": None, "done": False}
lines = []


def say(text=""):
    print(text)
    lines.append(text)


def call(method, path, body=None, token=None, expect: "int | None" = 0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            payload = json.loads(resp.read().decode())
            http = resp.status
    except urllib.error.HTTPError as error:
        payload = json.loads(error.read().decode())
        http = error.code
    code = payload.get("code")
    if expect is not None and code != expect:
        say(f"!! {method} {path} 期望 code={expect}，实际 http={http} {json.dumps(payload, ensure_ascii=False)[:800]}")
        raise SystemExit(1)
    return payload, http


def psql(sql):
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-Atqc", sql],
        capture_output=True,
        text=True,
        env={"PGPASSWORD": "alun", "PATH": "/usr/bin:/bin"},
    )
    if out.returncode != 0:
        say(f"!! psql 失败：{out.stderr.strip()}")
        raise SystemExit(1)
    return out.stdout.strip()


def login(username):
    for password in ("Admin@123456", "Admin@123456so44"):
        payload, _ = call(
            "POST",
            "/api/system/auth/login",
            {"username": username, "password": password},
            expect=None,
        )
        token = (payload.get("data") or {}).get("access_token")
        if token:
            return token
    say(f"!! {username} 登录失败")
    raise SystemExit(1)


def cleanup():
    if _STATE["done"]:
        return
    _STATE["done"] = True
    if _STATE.get("granted_role"):
        psql(
            "DELETE FROM sys_user_role WHERE user_id = '%s'::uuid AND role_id = "
            "(SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN')" % REPORTER
        )
        say("# so44_sales_a 的临时 SUPER_ADMIN 授权已删除")
    if _STATE["dept_leader"] is not None:
        psql(
            "UPDATE sys_dept SET leader_id = NULLIF('%s','')::uuid WHERE id = '%s'::uuid"
            % (_STATE["dept_leader"], DEPT)
        )
        say(f"# 销售一部负责人恢复为 {_STATE['dept_leader'] or '空'}")
    for rule_id in _STATE["rules"]:
        try:
            call("DELETE", f"/api/business/flow/linkage?id={rule_id}&confirm=true", token=_STATE["admin"])
            say(f"# 规则 {rule_id} 已删除（API）")
        except SystemExit:
            psql(f"DELETE FROM biz_flow_linkage_rule WHERE id = '{rule_id}'::uuid")
            say(f"# 规则 {rule_id} 已删除（SQL 兜底）")
    psql(f"DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK05%'")
    psql("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:EXPENSE:%'")
    report_id = _STATE["report_id"]
    if report_id:
        subprocess.run(
            ["bash", "/opt/cloudpivot/scripts/verify/lnk05_cleanup.sh", DB],
            capture_output=True,
            text=True,
        )
        say(f"# 报销单 {report_id} 及其审批实例 / 凭证 / 通知已清")


atexit.register(cleanup)


def rule_upsert(token, status, action, params, name):
    payload, _ = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": name,
            "trigger_entity": "EXPENSE_REPORT",
            "trigger_status": status,
            "action_code": action,
            "action_params": params,
            "enabled": True,
            "seq": 50,
            "confirm": True,
        },
        token,
    )
    return payload["data"]


def main():
    say("## LNK-05 真机取证：费用报销（EXPENSE_REPORT）提交 / 通过 / 付清 → 联动")
    say()
    admin = login("admin")
    _STATE["admin"] = admin
    say(f"# 超管 admin 登录成功，token 长度 = {len(admin)}")

    # ---------- ⓪ 夹具：销售员甲临时获得超管角色（否则「报销建单」权限不通过，403） ----------
    say()
    say("## ⓪ 夹具：so44_sales_a 临时授 SUPER_ADMIN（取证完毕删除该授权）")
    role_id = psql("SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN'")
    _STATE["granted_role"] = psql(
        f"SELECT count(*) FROM sys_user_role WHERE user_id = '{REPORTER}'::uuid AND role_id = '{role_id}'::uuid"
    ) == "0"
    psql(
        "INSERT INTO sys_user_role (user_id, role_id) "
        f"VALUES ('{REPORTER}'::uuid, '{role_id}'::uuid) ON CONFLICT DO NOTHING"
    )
    say(f"# 角色 {role_id} 已授予 {REPORTER}（原未授予 = {_STATE['granted_role']}）")

    sales = login("so44_sales_a")
    _STATE["sales"] = sales
    say(f"# 销售员甲 so44_sales_a 登录成功（唯一带部门的用户），token 长度 = {len(sales)}")

    # ---------- ① 配置页：EXPENSE_REPORT 必须显示 wired=true ----------
    say()
    say("## ① GET /api/business/flow/linkage?trigger_entity=EXPENSE_REPORT —— 配置页显示 wired=true")
    data, _ = call("GET", "/api/business/flow/linkage?trigger_entity=EXPENSE_REPORT", token=admin)
    trigger = next(item for item in data["data"]["triggers"] if item["entity"] == "EXPENSE_REPORT")
    say(json.dumps({"entity": trigger["entity"], "wired": trigger["wired"],
                    "wiring": trigger["wiring"][:180] + "…"}, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示 EXPENSE_REPORT 未接线")
        raise SystemExit(1)

    # ---------- ② 规则（走写入口：未接线触发点会被拒） ----------
    say()
    say("## ② PUT /api/business/flow/linkage —— 三条 SEND_NOTICE + 一条 GENERATE_VOUCHER")
    rules = []
    for status in ("SUBMITTED", "APPROVED", "PAID"):
        rules.append(rule_upsert(
            admin, status, "SEND_NOTICE",
            {"mode": "TAKEOVER", "recipient": "CREATOR"},
            f"LNK05 报销{ {'SUBMITTED': '提交', 'APPROVED': '通过', 'PAID': '付清'}[status] } → 通知制单人",
        ))
    # 故意在 APPROVED 上配一条 GENERATE_VOUCHER（凭证由 expense_bridge 出，排除清单应拦住）
    rules.append(rule_upsert(
        admin, "APPROVED", "GENERATE_VOUCHER",
        {"mode": "TAKEOVER", "biz_type": "EXPENSE_REPORT"},
        "LNK05 报销通过 → 生成凭证（应被排除）",
    ))
    _STATE["rules"] = [rule["id"] for rule in rules]
    for rule in rules:
        say(json.dumps({"id": rule["id"], "trigger": rule["trigger_entity"] + "/" + rule["trigger_status"],
                        "action": rule["action_code"], "phase": rule["phase"],
                        "implemented": rule["implemented"], "problems": rule["problems"]},
                       ensure_ascii=False))
        if rule["problems"]:
            say("!! 写入口报 problems，规则不可用")
            raise SystemExit(1)

    # ---------- ③ 夹具：部门负责人 = admin（直属上级审批节点走 DEPT_LEADER） ----------
    say()
    say("## ③ 夹具：销售一部负责人临时设为 admin（审批人是报销人的上级，不是本人）")
    _STATE["dept_leader"] = psql(f"SELECT coalesce(leader_id::text,'') FROM sys_dept WHERE id = '{DEPT}'::uuid")
    admin_id = psql("SELECT id FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL")
    psql(f"UPDATE sys_dept SET leader_id = '{admin_id}'::uuid WHERE id = '{DEPT}'::uuid")
    say(f"# 销售一部 leader_id：{_STATE['dept_leader'] or '空'} → {admin_id}")

    period_row = psql(
        "SELECT period_code || '|' || start_date FROM biz_accounting_period "
        "WHERE status = 'OPEN' AND start_date <= now()::date "
        "ORDER BY start_date DESC LIMIT 1"
    )
    period, biz_date = period_row.split("|")
    say(f"# 费用发生日取未结账期间 {period} 的首日 {biz_date}")

    kind = psql(
        "SELECT id || '|' || type_code FROM biz_expense_type "
        "WHERE deleted_at IS NULL AND type_code = 'TRAVEL' LIMIT 1"
    )
    if not kind:
        kind = psql(
            "SELECT id || '|' || type_code FROM biz_expense_type "
            "WHERE deleted_at IS NULL ORDER BY type_code LIMIT 1"
        )
    kind_id, kind_code = kind.split("|")
    say(f"# 费用类型 {kind_code}（{kind_id}）")

    # ---------- ④ 建单 + 提交 ----------
    say()
    say("## ④ POST /api/business/erp/expense（销售员甲）+ POST /{id}/submit")
    created, http = call(
        "POST",
        "/api/business/erp/expense",
        {
            "title": "LNK05 真机取证：差旅报销",
            "expense_date": biz_date,
            "remark": "LNK-05 联动接线取证",
            "items": [
                {
                    "expense_date": biz_date,
                    "expense_type_id": kind_id,
                    "with_tax_amount": 500,
                    "tax_rate": 0.06,
                    "invoice_no": "LNK05-INV-001",
                    "invoice_type": "VAT_SPECIAL",
                    "summary": "高铁票（取证）",
                }
            ],
        },
        sales,
    )
    report = created["data"]
    # `ReportDetailRes` 是 { report: {...}, items: [...] }（单测里 detail.report.status 同口径）；
    # 历史版本可能扁平化，两种都兜住，免得取证脚本因为外层字段改名就 KeyError。
    head = report.get("report") or report
    report_no = head["report_no"]
    _STATE["report_id"] = psql(
        f"SELECT id FROM biz_expense_report WHERE report_no = '{report_no}'"
    )
    say(f"# HTTP {http} 单号 {report_no} 状态 {head['status']} 含税合计 {head['total_with_tax']}")

    submitted, http = call("POST", f"/api/business/erp/expense/{_STATE['report_id']}/submit", None, sales)
    submitted = (submitted["data"] or {})
    submitted = submitted.get("report") or submitted
    say(f"# HTTP {http} 提交后状态 {submitted['status']} 审批单 {submitted.get('approval_instance_id')}")

    # ---------- ⑤ 库内物证：提交通知 ----------
    say()
    say("## ⑤ 库内物证（提交后）")
    for status in ("SUBMITTED",):
        count = psql(
            "SELECT count(*) FROM sys_notice WHERE idempotency_key = "
            f"'LNK:EXPENSE:{status}:{_STATE['report_id']}'"
        )
        title = psql(
            "SELECT coalesce(title,'') FROM sys_notice WHERE idempotency_key = "
            f"'LNK:EXPENSE:{status}:{_STATE['report_id']}' LIMIT 1"
        )
        say(f"# 通知 LNK:EXPENSE:{status}:<id> 条数 {count} 标题「{title}」")
        if count != "1":
            say("!! 提交没有触发联动通知")
            raise SystemExit(1)

    # ---------- ⑥ 审批通过（admin 作为部门负责人） ----------
    say()
    say("## ⑥ POST /api/business/flow/task/{id}/approve（admin，报销人不是 admin）")
    task_row = psql(
        "SELECT t.id || '|' || t.assignee_id || '|' || coalesce(u.username,'-') "
        "FROM biz_flow_task t LEFT JOIN sys_user u ON u.id = t.assignee_id "
        "WHERE t.instance_id = (SELECT approval_instance_id FROM biz_expense_report "
        f"WHERE id = '{_STATE['report_id']}'::uuid) AND t.status = 'PENDING' LIMIT 1"
    )
    task_id, assignee_id, assignee_name = task_row.split("|")
    say(f"# 待办 {task_id} 审批人 {assignee_name}（{assignee_id}）")
    if assignee_id != admin_id:
        say(f"!! 待办应指派给 admin（{admin_id}），实际 {assignee_id}")
        raise SystemExit(1)
    approved, http = call("POST", f"/api/business/flow/task/{task_id}/approve",
                          {"comment": "LNK05 真机取证：同意"}, admin)
    say(f"# HTTP {http} 审批结果 {json.dumps(approved.get('data'), ensure_ascii=False)[:200]}")

    status_now = psql(f"SELECT status FROM biz_expense_report WHERE id = '{_STATE['report_id']}'::uuid")
    say(f"# 报销单状态 {status_now}")
    if status_now != "APPROVED":
        say("!! 审批通过后状态应为 APPROVED")
        raise SystemExit(1)
    count = psql(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = "
        f"'LNK:EXPENSE:APPROVED:{_STATE['report_id']}'"
    )
    title = psql(
        "SELECT coalesce(title,'') FROM sys_notice WHERE idempotency_key = "
        f"'LNK:EXPENSE:APPROVED:{_STATE['report_id']}' LIMIT 1"
    )
    say(f"# 通知 LNK:EXPENSE:APPROVED:<id> 条数 {count} 标题「{title}」")
    if count != "1":
        say("!! 审批通过没有触发联动通知")
        raise SystemExit(1)
    vouchers = psql(
        "SELECT count(*) FROM biz_voucher WHERE source_id = "
        f"'{_STATE['report_id']}'::uuid AND deleted_at IS NULL"
    )
    say(f"# 该报销单名下凭证总数 {vouchers}（配了 GENERATE_VOUCHER 规则，排除清单应挡掉 → 恰好 1 张）")
    if vouchers != "1":
        say("!! 凭证被重复生成")
        raise SystemExit(1)

    # ---------- ⑦ 付清 ----------
    say()
    say("## ⑦ POST /api/business/erp/expense/{id}/pay（全额 500）→ PAID")
    paid, http = call(
        "POST",
        f"/api/business/erp/expense/{_STATE['report_id']}/pay",
        {"amount": 500, "pay_date": biz_date, "pay_method": "BANK_TRANSFER",
         "pay_account_code": "1002", "remark": "LNK05 取证：全额付款"},
        admin,
    )
    paid = paid["data"]
    paid = paid.get("report") or paid
    say(f"# HTTP {http} 状态 {paid['status']} 已付 {paid.get('paid_amount')} 未付 {paid.get('unpaid_amount')}")
    count = psql(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = "
        f"'LNK:EXPENSE:PAID:{_STATE['report_id']}'"
    )
    say(f"# 通知 LNK:EXPENSE:PAID:<id> 条数 {count}")
    if count != "1":
        say("!! 付清没有触发联动通知")
        raise SystemExit(1)
    vouchers = psql(
        "SELECT count(*) FROM biz_voucher WHERE source_id = "
        f"'{_STATE['report_id']}'::uuid AND deleted_at IS NULL"
    )
    say(f"# 凭证总数 {vouchers}（计提 1 + 付款 1 = 2）")
    if vouchers != "2":
        say("!! 付款凭证条数不符")
        raise SystemExit(1)

    say()
    say("ALL PASS：报销提交 / 审批通过 / 付清三段都触发了联动通知，凭证未被重复生成")
    lines.insert(0, f"# 取证时间：{psql('SELECT now()::text')}")
    open(OUT, "w", encoding="utf-8").write("\n".join(lines) + "\n")


main()
