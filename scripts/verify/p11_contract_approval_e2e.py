#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-1 真机验收：合同审批流（ARCH-3 §6 R-P1-1）。

跑在隔离实例上（默认 127.0.0.1:18097 / 库 cp_p11），全部走真实 HTTP：
  ① 新建合同 → ② 不审批直接点生效/签订 → 被拒 → ③ 提交审批（挂 FLW 单号）
  → ④ 审批人待办里批掉 → ⑤ 合同才变已签订 → ⑥ 才允许生效 ACTIVE
  ⑦ 金额 ≥ 10 万必须两级审批（一级批完仍在审批中）
  ⑧ 造一条「审批已通过但单据停在审批中」的脏数据 → 不变量 H4 必须抓到 → 清掉后回到全绿

用法：python3 scripts/verify/p11_contract_approval_e2e.py
退出码：0 全过 / 1 有失败。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("P11_BASE", "http://127.0.0.1:18097")
DB = dict(
    host=os.environ.get("P11_DB_HOST", "127.0.0.1"),
    port=os.environ.get("P11_DB_PORT", "5432"),
    user=os.environ.get("P11_DB_USER", "alun"),
    name=os.environ.get("P11_DB_NAME", "cp_p11"),
    password=os.environ.get("P11_DB_PASSWORD", "alun"),
)
ADMIN_USER = os.environ.get("P11_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("P11_ADMIN_PASS", "Admin@123456")
NEW_PASS = os.environ.get("P11_NEW_PASS", "P11@Passw0rd!")
APPROVER = "p11_approver"
APPROVER_ID = "a2a9aaaa-0000-4000-8000-0000000000aa"
SUPPLIER_IDS = "a2a9bbb0-0000-4000-8000-0000000000b0"

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
    if os.environ.get("P11_DEBUG"):
        print(f"[psql] rc={out.returncode} out={out.stdout.strip()[:80]!r} "
              f"err={out.stderr.strip()[:80]!r}")
    return out.stdout.strip()


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(username, password):
    """登录（自动处理「首登强制改密」与「上次已改过口令」两种状态）。"""
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
    # 首登强制改密：把初始口令改成固定的测试口令（本脚本在隔离库里反复跑）
    if (body.get("data") or {}).get("must_change_password") and used != NEW_PASS:
        st, resp = call("PUT", "/api/system/auth/password",
                        {"old_password": used, "new_password": NEW_PASS}, token)
        if st != 200:
            raise SystemExit(f"{username} 改初始口令失败：{st} {resp}")
        status, body = call("POST", "/api/system/auth/login",
                            {"username": username, "password": NEW_PASS})
        token = (body.get("data") or {}).get("access_token") or ""
    return status, body, token


def invariants(group="审批"):
    """跑不变量套件的某一组，返回 (退出码, 报告全文)。退出码 1 = 有 BLOCKER。"""
    env = dict(os.environ,
               NIK_DATABASE_HOST=DB["host"], NIK_DATABASE_PORT=DB["port"],
               NIK_DATABASE_USER=DB["user"], NIK_DATABASE_NAME=DB["name"],
               NIK_DATABASE_PASSWORD=DB["password"])
    out = subprocess.run([sys.executable, "scripts/verify/erp_invariants.py", "--group", group],
                         capture_output=True, text=True, env=env, cwd="/opt/cloudpivot")
    return out.returncode, out.stdout + out.stderr


def h4_is_blocker(text):
    """H4 是否被列为 BLOCKER（报告里「🚨 必须立刻修」那一段是否点名 H4）。"""
    if "🚨 必须立刻修" not in text:
        return False
    tail = text.split("🚨 必须立刻修", 1)[1]
    return "H4" in tail


def prepare_fixtures():
    # 审批人：口令抄 admin 的哈希（同一口令可登录）；角色给超管 + 两个审批角色
    psql(
        "INSERT INTO sys_user (id, username, password, real_name, status) "
        "SELECT 'a2a9aaaa-0000-4000-8000-0000000000aa'::uuid, 'p11_approver', u.password, "
        "'P11 审批人', 1 FROM sys_user u WHERE u.username = 'admin' "
        "AND NOT EXISTS (SELECT 1 FROM sys_user WHERE username = 'p11_approver')"
    )
    psql(
        "UPDATE sys_user SET must_change_password = false "
        "WHERE username IN ('p11_approver', 'admin')"
    )
    psql(
        "INSERT INTO sys_user_role (user_id, role_id) "
        "SELECT "
        f"'{APPROVER_ID}'::uuid, r.id FROM sys_role r "
        "WHERE r.role_code IN ('SUPER_ADMIN','PURCHASE_MANAGER','GENERAL_MANAGER') "
        "AND r.deleted_at IS NULL "
        "AND NOT EXISTS (SELECT 1 FROM sys_user_role x "
        f"                WHERE x.user_id = '{APPROVER_ID}'::uuid AND x.role_id = r.id)"
    )
    psql(
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days) "
        f"VALUES ('{SUPPLIER_IDS}'::uuid, 'P11-E2E-SUP', 'P11 真机验收供应商', 1, 30) "
        "ON CONFLICT DO NOTHING"
    )


def make_contract(token, tag, amount):
    body = {
        "name": f"P11E2E-{tag}",
        "contract_type": "PURCHASE",
        "party_b_id": SUPPLIER_IDS,
        "amount": amount,
        "sign_date": "2026-01-01",
        "start_date": "2026-01-02",
        "end_date": "2026-12-31",
        "remark": "P1-1 真机验收夹具",
    }
    status, res = call("POST", "/api/business/erp/contract", body, token)
    if status != 200:
        raise SystemExit(f"建合同失败：{status} {res}")
    return res["data"]["id"], res["data"]["contract_no"]


def patch_status(token, cid, target):
    return call("PATCH", f"/api/business/erp/contract/{cid}/status",
                {"target_status": target}, token)


def get_contract(token, cid):
    status, res = call("GET", f"/api/business/erp/contract/{cid}", None, token)
    if status != 200:
        raise SystemExit(f"查合同失败：{status} {res}")
    return res["data"]


def todo_tasks(token):
    status, res = call("GET", "/api/business/flow/task/todo?page=1&page_size=50", None, token)
    if status != 200:
        raise SystemExit(f"查待办失败：{status} {res}")
    data = res.get("data") or {}
    return data.get("list") or data.get("records") or []


def task_for(token, instance_no):
    for item in todo_tasks(token):
        if (item.get("instance_no") or "") == instance_no:
            return item
    return None


def approve(token, task_id, comment="P1-1 真机验收：同意"):
    return call("POST", f"/api/business/flow/task/{task_id}/approve", {"comment": comment}, token)


def cleanup():
    psql("DELETE FROM sys_notice_receiver WHERE notice_id IN "
         "(SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN "
         "(SELECT id::text FROM biz_flow_instance WHERE biz_type = 'CONTRACT_APPROVE'))")
    psql("DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN "
         "(SELECT id::text FROM biz_flow_instance WHERE biz_type = 'CONTRACT_APPROVE')")
    psql("UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE prev_instance_id IN "
         "(SELECT id FROM biz_flow_instance WHERE biz_type = 'CONTRACT_APPROVE')")
    psql("DELETE FROM biz_flow_instance WHERE biz_type = 'CONTRACT_APPROVE'")
    psql("DELETE FROM biz_status_log WHERE entity_type = 'CONTRACT' AND entity_id IN "
         "(SELECT id FROM biz_contract WHERE contract_name LIKE 'P11E2E-%')")
    psql("DELETE FROM biz_contract WHERE contract_name LIKE 'P11E2E-%'")


def main():
    print("=" * 78)
    print("P1-1 合同审批流 真机验收")
    print("=" * 78)
    cleanup()
    prepare_fixtures()

    status, body, admin = login(ADMIN_USER, ADMIN_PASS)
    if not admin:
        raise SystemExit(f"admin 登录失败：{status} {body}")
    status, body, approver = login(APPROVER, ADMIN_PASS)
    if not approver:
        raise SystemExit(f"审批人登录失败：{status} {body}")
    print(f"登录 OK：admin={ADMIN_USER} / 审批人={APPROVER}")

    # ---------- ① 新建合同（草稿） ----------
    cid, cno = make_contract(admin, "SMALL", 50000)
    detail = get_contract(admin, cid)
    check(detail["status"] == "DRAFT", f"① 新建合同是草稿（{cno}）", detail["status"])
    check(detail["next_statuses"] == ["PENDING_APPROVAL"],
          "① 草稿的唯一去向是「提交审批」", detail["next_statuses"])

    # ---------- ② 不审批直接点签订 / 生效 → 被拒 ----------
    st, res = patch_status(admin, cid, "SIGNED")
    check(st == 409, "② 不审批直接「登记签订」被拒（409）", f"{st} {res}")
    msg = (res.get("msg") or res.get("message") or res.get("raw") or "")
    check("PENDING_APPROVAL" in msg and "审批" in msg,
          "② 报文点名「先提交审批」", msg[:160])
    st2, res2 = patch_status(admin, cid, "ACTIVE")
    check(st2 == 409, "② 不审批直接「生效」被拒（409）", f"{st2} {res2}")
    check(get_contract(admin, cid)["status"] == "DRAFT", "② 被拒后状态没变（仍是草稿）")

    # ---------- ③ 提交审批 ----------
    st, res = patch_status(admin, cid, "PENDING_APPROVAL")
    check(st == 200, "③ 提交审批 200", f"{st} {res}")
    detail = get_contract(admin, cid)
    check(detail["status"] == "PENDING_APPROVAL", "③ 状态 → 审批中", detail["status"])
    check(bool(detail.get("approval_instance_id")), "③ 挂上审批实例 id",
          str(detail.get("approval_instance_id")))
    instance_no = detail.get("approval_instance_no") or ""
    check(instance_no.startswith("FLW-"), "③ 下发 FLW 审批单号", instance_no)
    check(detail["usable_for_order"] is False, "③ 审批中不能被采购单引用")
    st_dup, res_dup = patch_status(admin, cid, "PENDING_APPROVAL")
    check(st_dup == 409, "③ 重复提交被拒（幂等）", f"{st_dup} {res_dup}")

    # ---------- ④ 审批人批掉 ----------
    task = task_for(approver, instance_no)
    check(task is not None, "④ 审批人待办里能看到这张合同", instance_no)
    if task is None:
        raise SystemExit("没有待办，后续断言无法进行")
    check(task.get("assignee_name") not in (None, ""), "④ 待办带审批人姓名",
          str(task.get("assignee_name")))
    st, res = approve(approver, task["id"])
    check(st == 200, "④ 审批通过 200", f"{st} {res}")

    # ---------- ⑤ 合同才变「已签订」 ----------
    detail = get_contract(admin, cid)
    check(detail["status"] == "SIGNED", "⑤ 审批通过 → 已签订", detail["status"])
    check(detail["usable_for_order"] is True, "⑤ 已签订才能被采购单引用")

    # ---------- ⑥ 才允许生效 ----------
    st, res = patch_status(admin, cid, "ACTIVE")
    check(st == 200, "⑥ 已签订 → 生效 200", f"{st} {res}")
    check(get_contract(admin, cid)["status"] == "ACTIVE", "⑥ 状态 → 已生效")

    # ---------- ⑦ 金额 ≥ 10 万：两级审批 ----------
    cid2, cno2 = make_contract(admin, "LARGE", 200000)
    st, res = patch_status(admin, cid2, "PENDING_APPROVAL")
    check(st == 200, f"⑦ ≥10 万合同提交审批（{cno2}）", f"{st} {res}")
    d2 = get_contract(admin, cid2)
    inst2 = d2["approval_instance_no"]
    t1 = task_for(approver, inst2)
    check(t1 is not None, "⑦ 第一级（采购经理）待办存在")
    if t1:
        st, res = approve(approver, t1["id"])
        check(st == 200, "⑦ 第一级审批通过 200", f"{st} {res}")
    check(get_contract(admin, cid2)["status"] == "PENDING_APPROVAL",
          "⑦ 一级批完仍停在审批中（两级审批没被一级绕过）")
    t2 = task_for(approver, inst2)
    check(t2 is not None, "⑦ 第二级（总经理）待办出现")
    if t2:
        st, res = approve(approver, t2["id"])
        check(st == 200, "⑦ 第二级审批通过 200", f"{st} {res}")
    check(get_contract(admin, cid2)["status"] == "SIGNED", "⑦ 两级都批完 → 已签订")

    # ---------- ⑧ H4 巡检：造脏数据 → 必须抓到 → 清掉 → 全绿 ----------
    rc0, text0 = invariants()
    summary0 = next((line for line in text0.splitlines() if line.startswith("结果：")), "")
    check(rc0 == 0 and not h4_is_blocker(text0),
          "⑧ 干净库里 H4 巡检通过（原不变量全绿）", summary0)
    cid3, cno3 = make_contract(admin, "DIRTY", 60000)
    patch_status(admin, cid3, "PENDING_APPROVAL")
    inst3 = get_contract(admin, cid3)["approval_instance_no"]
    # 手工把审批单改成 APPROVED（模拟「回调失败 / 被人手改库」的形态）
    psql("UPDATE biz_flow_task SET status = 'APPROVED', completed_at = now() "
         "WHERE instance_id = (SELECT id FROM biz_flow_instance "
         f"WHERE instance_no = '{inst3}') AND status = 'PENDING'")
    psql("UPDATE biz_flow_instance SET status = 'APPROVED', ended_at = now() "
         f"WHERE instance_no = '{inst3}'")
    rc1, text1 = invariants()
    h4_line = next((line for line in text1.splitlines() if "H4" in line), "")
    check(rc1 == 1 and h4_is_blocker(text1),
          "⑧ 脏数据（审批已通过、合同还在审批中）被 H4 抓到", h4_line)
    check(cno3 in text1 or "CONTRACT_APPROVE" in text1, "⑧ H4 报文点名了那张单据",
          h4_line[:160])
    # 清掉脏数据（连带实例），恢复干净
    psql(f"DELETE FROM biz_flow_instance WHERE instance_no = '{inst3}'")
    psql("DELETE FROM biz_status_log WHERE entity_type = 'CONTRACT' "
         f"AND entity_id = '{cid3}'::uuid")
    psql(f"DELETE FROM biz_contract WHERE id = '{cid3}'::uuid")
    rc2, text2 = invariants()
    summary2 = next((line for line in text2.splitlines() if line.startswith("结果：")), "")
    check(rc2 == 0 and not h4_is_blocker(text2), "⑧ 清掉脏数据后 H4 恢复全绿", summary2)

    print()
    print("=" * 78)
    print(f"结果：{len(PASSED)} 通过 / {len(FAILED)} 失败")
    for name in FAILED:
        print("  ✗ " + name)
    print("=" * 78)
    return 0 if not FAILED else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    finally:
        pass
