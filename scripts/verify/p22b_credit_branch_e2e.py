#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P2-2b 真机验收：po_approve v2「超信用额度 → 财务复核」条件分支。

要证明的事（每条 `[PASS]/[FAIL]` + 原始数字）：

  ① v2 已发布且 v1 原样归档：`biz_flow_def.current_version_no = 2`，v2 hash = 4db3…，
     v1 仍是 ARCHIVED 且 hash 逐字节还是 8f65…（旧版本快照一行不动）；
  ② **超限单**（额度 1 万 / 下单 2 万）走 `e_credit_over`：实例 version_no = 2，
     `c_credit` 的 `taken_edge_key = e_credit_over`，第一道待办落在「财务复核」`n_fin`；
     此时 `n_pm`（采购经理）**还没有**待办 —— 串行，不是并行；
  ③ 财务经理同意 → 下一道待办是 `n_pm`（2 万落在 1–10 万档）；采购经理同意 → 单据 APPROVED；
  ④ **额度内单**（额度 10 万 / 下单 5 千）走 `e_credit_else`：`c_credit` 之后**没有** `n_fin` 节点，
     第一道待办直接是 `n_dept`（部门主管）—— 证明条件分支真的按布尔分流，没给正常单加负担；
  ⑤ **复核是真闸**：超限小额单（额度 1000 / 下单 5 千）由财务**驳回** → 实例 REJECTED、
     单据回到草稿、不再产生后续待办（如果复核是摆设，这里会继续往下走）；
  ⑥ 额度 0（= 不限）不校验：5 万单也走 `e_credit_else` → 直接进金额档。

跑法：bash scripts/verify/p21_instance.sh start（或 p22b_instance.sh），再
      python3 scripts/verify/p22b_credit_branch_e2e.py --port 18100 --db cloudpivot_p22b_b
脚本自带前缀清场（`P22B-`），可重复跑。
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from decimal import Decimal

BASE = "http://127.0.0.1:18100"
DB = "cloudpivot_p22b_b"
DB_USER = "alun"
DB_PASS = "alun"
TOKEN = ""
RESULTS = []

PREFIX = "P22B"
V2_HASH = "4db3fe5731dc8efc7844d6a51a326df20638d8b1eabd4a553444ead503554b7d"
V1_HASH = "8f65819ea25e1145d441d9b947b89b21bd4bfdc5075ac321367ea106a06366de"
DEF_ID = "20000000-0000-4000-8000-0000000a0182"

FIN_USER = "p22b_fin"
FIN_PASS = "P22bFin@2026x"
PM_USER = "p22b_pm"
PM_PASS = "P22bPm@2026x"
ROLE_FINANCE_MANAGER = "20000000-0000-4000-8000-0000000f0005"
ROLE_PURCHASE_MANAGER = "20000000-0000-4000-8000-0000000f0002"

WAREHOUSE_ID = "d1370001-0000-4000-8000-000000000001"
MATERIAL_ID = "d1370003-0000-4000-8000-000000000005"
SUPPLIER_ID = None


# ---------------------------------------------------------------- 基础设施
def psql(sql, db=None):
    env = dict(os.environ)
    env["PGPASSWORD"] = DB_PASS
    r = subprocess.run(["psql", "-h", "127.0.0.1", "-p", "5432", "-U", DB_USER,
                        "-d", db or DB, "-tAF", "\x1f", "-c", sql],
                       capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(f"psql 失败：{r.stderr.strip()[:400]}\nSQL: {sql[:200]}")
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line != ""]


def scalar(sql, db=None):
    rows = psql(sql, db)
    return rows[0][0] if rows and rows[0] else None


def rows(sql, db=None):
    return psql(sql, db)


def ok(name, cond, detail=""):
    RESULTS.append((bool(cond), name, detail))
    print(f"  {'[PASS]' if cond else '[FAIL]'} {name}" + (f"  — {detail}" if detail else ""))


def call(method, path, body=None, token=None):
    if path != "/health" and not path.startswith("/api/"):
        path = "/api" + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    tok = token or TOKEN
    if tok:
        req.add_header("Authorization", f"Bearer {tok}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except Exception:
            return err.code, {"raw": raw}


def login(username, password, tries=8):
    for _ in range(tries):
        st, body = call("POST", "/system/auth/login",
                        {"username": username, "password": password}, token="")
        if st == 429:
            time.sleep(10)
            continue
        return st, body
    return 429, {"msg": "登录限流"}


def admin_token():
    global TOKEN
    for pwd in ("Admin@123456", "Admin@1234567", "P2bAdmin#2026x"):
        st, body = login("admin", pwd)
        if st == 200 and (body.get("data") or {}).get("access_token"):
            TOKEN = body["data"]["access_token"]
            if body["data"].get("must_change_password"):
                st2, _ = call("PUT", "/system/auth/password",
                              {"old_password": pwd, "new_password": "P2bAdmin#2026x"})
                st, body = login("admin", "P2bAdmin#2026x")
                TOKEN = (body.get("data") or {}).get("access_token", TOKEN)
            return TOKEN
    raise SystemExit("管理员登录失败")


def ensure_user(username, real_name, role_id, password):
    """建（或复用）审批账号，绑「业务角色 + 超管」（超管是为了拿到 flow:approve 权限）。"""
    super_role = scalar("SELECT id FROM sys_role WHERE role_code='SUPER_ADMIN' AND deleted_at IS NULL LIMIT 1")
    st, body = call("POST", "/system/user", {
        "username": username, "password": password, "real_name": real_name,
        "status": 1, "role_ids": [role_id, super_role],
    })
    if st == 200:
        return (body.get("data") or {}).get("id")
    uid = scalar(f"SELECT id FROM sys_user WHERE username='{username}' AND deleted_at IS NULL LIMIT 1")
    if not uid:
        raise SystemExit(f"建账号 {username} 失败：{st} {body}")
    # 已经存在（重复跑）→ 角色重置成同一套（业务角色 + 超管，超管是为了 flow:approve 权限）
    st2, resp = call("PUT", f"/system/user/{uid}/roles", {"role_ids": [role_id, super_role]})
    if st2 != 200:
        raise SystemExit(f"重置 {username} 角色失败：{st2} {resp}")
    return uid


# ---------------------------------------------------------------- 清场 / 夹具
def cleanup():
    scope = f"(select id from biz_supplier where supplier_code like '{PREFIX}%')"
    stmts = [
        f"""delete from biz_flow_task where instance_id in (
              select id from biz_flow_instance where biz_type='PO_APPROVE' and biz_id in (
                select id from biz_purchase_order where supplier_id in {scope}))""",
        f"""delete from biz_flow_cc where instance_id in (
              select id from biz_flow_instance where biz_type='PO_APPROVE' and biz_id in (
                select id from biz_purchase_order where supplier_id in {scope}))""",
        f"""delete from biz_flow_instance_node where instance_id in (
              select id from biz_flow_instance where biz_type='PO_APPROVE' and biz_id in (
                select id from biz_purchase_order where supplier_id in {scope}))""",
        f"""delete from biz_flow_instance where biz_type='PO_APPROVE' and biz_id in (
              select id from biz_purchase_order where supplier_id in {scope})""",
        f"""delete from biz_status_log where entity_type='PURCHASE_ORDER' and entity_id in (
              select id from biz_purchase_order where supplier_id in {scope})""",
        f"delete from biz_purchase_order where supplier_id in {scope}",
        f"delete from biz_supplier where supplier_code like '{PREFIX}%'",
    ]
    for sql in stmts:
        psql(sql)


def create_supplier(credit_limit):
    global SUPPLIER_ID
    psql(f"delete from biz_supplier where supplier_code like '{PREFIX}%'")
    st, body = call("POST", "/business/erp/supplier", {
        "supplier_code": f"{PREFIX}-SUP", "supplier_name": "P2-2b 验收供应商",
        "credit_days": 30, "credit_limit": credit_limit, "status": 1,
    })
    if st != 200:
        raise SystemExit(f"建供应商失败：{st} {body}")
    SUPPLIER_ID = (body.get("data") or {}).get("id")
    return SUPPLIER_ID


def set_credit(credit_limit):
    st, body = call("PUT", f"/business/erp/supplier/{SUPPLIER_ID}", {"credit_limit": credit_limit})
    if st != 200:
        raise SystemExit(f"改额度失败：{st} {body}")


def create_order(amount):
    st, body = call("POST", "/business/erp/purchase-order", {
        "supplier_id": SUPPLIER_ID, "warehouse_id": WAREHOUSE_ID, "price_mode": "EXCL",
        "items": [{"material_id": MATERIAL_ID, "qty": 1, "price_excl": amount, "tax_rate": 0}],
    })
    if st != 200:
        raise SystemExit(f"建采购单失败：{st} {body}")
    return body["data"]["order"]


def submit(order_id):
    return call("POST", f"/business/erp/purchase-order/{order_id}/submit-approval", {})


def order_detail(order_id):
    st, body = call("GET", f"/business/erp/purchase-order/{order_id}")
    if st != 200:
        raise SystemExit(f"查采购单失败：{st} {body}")
    return body["data"]


def instance_of(order_id):
    row = scalar(f"select id from biz_flow_instance where biz_type='PO_APPROVE' "
                 f"and biz_id='{order_id}'::uuid and deleted_at is null order by created_at desc limit 1")
    return row


def instance_row(instance_id):
    r = scalar(f"select version_no||'|'||status||'|'||coalesce(flow_code,'') from biz_flow_instance "
               f"where id='{instance_id}'::uuid")
    return r.split("|") if r else ["", "", ""]


def nodes_of(instance_id):
    """[(node_key, status, taken_edge_key)]，按激活时间。"""
    return rows(f"select node_key, status, coalesce(taken_edge_key,'-') from biz_flow_instance_node "
                f"where instance_id='{instance_id}'::uuid order by activated_at, node_key")


def active_tasks(instance_id):
    return rows(f"select t.id, t.node_key, coalesce(t.assignee_name,''), t.status, "
                f"coalesce(u.username,''), coalesce(t.task_kind,'') from biz_flow_task t "
                f"left join sys_user u on u.id = t.assignee_id "
                f"where t.instance_id='{instance_id}'::uuid and t.status='PENDING' "
                f"order by t.created_at")


def approve(token, task_id, comment="同意（P2-2b 真机）"):
    return call("POST", f"/business/flow/task/{task_id}/approve", {"comment": comment}, token)


def reject(token, task_id, comment="驳回（P2-2b 真机：额度风险不接受）", return_to="INITIATOR"):
    return call("POST", f"/business/flow/task/{task_id}/reject",
                {"comment": comment, "return_to": return_to}, token)


def user_token(username, password):
    st, body = login(username, password)
    if st != 200 or not (body.get("data") or {}).get("access_token"):
        raise SystemExit(f"{username} 登录失败：{st} {body}")
    return body["data"]["access_token"]


# ---------------------------------------------------------------- 主流程
def main():
    global BASE, DB
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18100)
    ap.add_argument("--db", default="cloudpivot_p22b_b")
    args = ap.parse_args()
    BASE, DB = f"http://127.0.0.1:{args.port}", args.db

    print(f"== P2-2b 真机验收 @ {BASE}（库 {DB}）==")
    st, _ = call("GET", "/health")
    if st != 200:
        print("环境不满足：/health 不是 200")
        return 2

    admin_token()

    # 财务经理 / 采购经理必须是**真人**：角色 0 人时 `FALLBACK_ADMIN` 会兜底到超管，而超管正好是
    # 制单人 → 内控闸把「自己批自己」剔掉 → 财务复核节点无人可派 → 提交阶段就 422 + 65018
    # （fail-closed，见 ⑦ 的显式断言）。
    fin_id = ensure_user(FIN_USER, "P2-2b 财务经理", ROLE_FINANCE_MANAGER, FIN_PASS)
    pm_id = ensure_user(PM_USER, "P2-2b 采购经理", ROLE_PURCHASE_MANAGER, PM_PASS)
    # 采购经理岗在真库里有两个人（p3a_0003 是别的卡的夹具）：`APPROVAL` 节点的缺省会签方式
    # 是 ALL（`approval_mode` 为空 → ALL），两人都要签字，节点才会往下走。本卡的验收点是
    # 「超限分支怎么走」，不是会签人数，所以测试期间把该角色收成单成员，用完恢复。
    pm_extra = psql(f"select ur.user_id from sys_user_role ur where ur.role_id='{ROLE_PURCHASE_MANAGER}'::uuid "
                    f"and ur.user_id <> '{pm_id}'::uuid")
    for (uid,) in pm_extra:
        psql(f"delete from sys_user_role where user_id='{uid}'::uuid "
             f"and role_id='{ROLE_PURCHASE_MANAGER}'::uuid")
    if pm_extra:
        print(f"  采购经理岗临时收成单成员（移出 {len(pm_extra)} 人，用完恢复）")
    cleanup()

    # ---------------- ① 版本态：v2 已发布、v1 原样归档 ----------------
    print("\n== ① 版本态 ==")
    def_info = scalar(f"select coalesce(current_version_no::text,'')||'|'||coalesce(status,'') "
                      f"from biz_flow_def where id='{DEF_ID}'::uuid")
    ok("① 流程定义指向 v2 且仍 PUBLISHED", def_info == "2|PUBLISHED", def_info)
    v2 = scalar(f"select graph_hash||'|'||version_status||'|'||is_current::text||'|'||node_count||'|'||edge_count "
                f"from biz_flow_def_version where def_id='{DEF_ID}'::uuid and version_no=2")
    ok("① v2 = PUBLISHED / is_current / 9 节点 11 边 / hash 与设计器发布结果一致",
       v2 == f"{V2_HASH}|PUBLISHED|true|9|11", v2)
    v1 = scalar(f"select graph_hash||'|'||version_status||'|'||is_current::text "
                f"from biz_flow_def_version where def_id='{DEF_ID}'::uuid and version_no=1")
    ok("① v1 归档但快照逐字节不变（旧版本没被改道）",
       v1 == f"{V1_HASH}|ARCHIVED|false", v1)

    # ---------------- ② 超限单：条件边命中 → 财务复核（串行第一道） ----------------
    print("\n== ② 超限单（额度 1 万 / 下单 2 万）→ 财务复核 ==")
    create_supplier(10000)
    o1 = create_order("20000")
    st, body = submit(o1["id"])
    ok("② 超限单提交成功（策略 B：允许 + 强制审批）", st == 200, f"{st} {str(body)[:150]}")
    d1 = order_detail(o1["id"])
    ok("② 单据 over_credit_limit = true", d1.get("over_credit_limit") is True, str(d1.get("over_credit_limit")))
    i1 = instance_of(o1["id"])
    ok("② 生成审批实例", bool(i1), str(i1))
    if not i1:
        raise SystemExit("② 没有实例，后续断言无法继续（先看上面的提交报文）")
    ver_no, inst_status, flow_code = instance_row(i1)
    ok("② 实例跑在 v2 上（version_no = 2）", ver_no == "2", ver_no)
    seq1 = nodes_of(i1)
    print(f"     节点序列：{seq1}")
    ok("② c_credit 走的是「超信用额度」分支（taken_edge_key = e_credit_over）",
       any(k == "c_credit" and e == "e_credit_over" for k, _, e in seq1), str(seq1))
    ok("② 第一道待办落在「财务复核」n_fin",
       any(k == "n_fin" and s in ("ACTIVE", "PENDING") for k, s, _ in seq1), str(seq1))
    ok("② 串行：此时还没有 n_pm（采购经理）节点被激活",
       not any(k == "n_pm" for k, _, _ in seq1), str(seq1))
    fin_tasks = active_tasks(i1)
    print(f"     待办：{fin_tasks}")
    ok("② 恰好 1 条待办且 node_key = n_fin", len(fin_tasks) == 1 and fin_tasks[0][1] == "n_fin",
       str(fin_tasks))
    ok("② 复核待办派给财务经理账号（p22b_fin）",
       len(fin_tasks) == 1 and fin_tasks[0][4] == FIN_USER, str(fin_tasks))
    form = scalar(f"select form_data::text from biz_flow_instance where id='{i1}'::uuid") or "{}"
    form_json = json.loads(form)
    ok("② 审批单 form_data 里 over_credit_limit 是 JSON 布尔 true（条件求值的输入）",
       form_json.get("over_credit_limit") is True, str(form_json.get("over_credit_limit")))

    # ---------------- ③ 财务同意 → 采购经理 → 通过 ----------------
    print("\n== ③ 财务同意 → 采购经理同意 → 单据 APPROVED ==")
    fin_tok = user_token(FIN_USER, FIN_PASS)
    st, body = approve(fin_tok, fin_tasks[0][0])
    ok("③ 财务经理（真实 HTTP）审批通过", st == 200, f"{st} {str(body)[:150]}")
    seq2 = nodes_of(i1)
    print(f"     节点序列：{seq2}")
    ok("③ 财务复核已终态（n_fin = APPROVED）",
       any(k == "n_fin" and s == "APPROVED" for k, s, _ in seq2), str(seq2))
    ok("③ 复核通过后才出现 n_pm（2 万 → 1–10 万档 = 采购经理）",
       any(k == "n_pm" for k, _, _ in seq2), str(seq2))
    ok("③ 金额档位没被复核改道：c_low 走 e_low_else（≥ 1 万）、c_mid 走 e_mid（< 10 万）",
       any(k == "c_low" and e == "e_low_else" for k, _, e in seq2)
       and any(k == "c_mid" and e == "e_mid" for k, _, e in seq2), str(seq2))
    pm_tasks = active_tasks(i1)
    ok("③ 待办转给采购经理（n_pm，p22b_pm）",
       len(pm_tasks) == 1 and pm_tasks[0][1] == "n_pm" and pm_tasks[0][4] == PM_USER, str(pm_tasks))
    pm_tok = user_token(PM_USER, PM_PASS)
    st, body = approve(pm_tok, pm_tasks[0][0])
    ok("③ 采购经理审批通过", st == 200, f"{st} {str(body)[:150]}")
    ver_no, inst_status, _ = instance_row(i1)
    ok("③ 实例终态 APPROVED", inst_status == "APPROVED", inst_status)
    d1b = order_detail(o1["id"])
    ok("③ 采购单状态 APPROVED", d1b.get("status") == "APPROVED", str(d1b.get("status")))
    # ---------------- ④ 对照：额度内单不加复核 ----------------
    print("\n== ④ 对照：额度 10 万 / 下单 5 千（额度内）→ 直接进金额档 ==")
    set_credit(100000)
    o2 = create_order("5000")
    st, body = submit(o2["id"])
    ok("④ 额度内单提交成功", st == 200, f"{st} {str(body)[:150]}")
    i2 = instance_of(o2["id"])
    seq3 = nodes_of(i2)
    print(f"     节点序列：{seq3}")
    ok("④ over_credit_limit = false（前提成立）",
       order_detail(o2["id"]).get("over_credit_limit") is False,
       str(order_detail(o2["id"]).get("over_credit_limit")))
    ok("④ c_credit 走兜底边 e_credit_else", 
       any(k == "c_credit" and e == "e_credit_else" for k, _, e in seq3), str(seq3))
    ok("④ **没有** n_fin 节点（额度内单不白跑一道复核）",
       not any(k == "n_fin" for k, _, _ in seq3), str(seq3))
    ok("④ 第一道待办是 n_dept（< 1 万档 = 部门主管）",
       any(k == "n_dept" for k, _, _ in seq3), str(seq3))

    # ---------------- ⑤ 复核是真闸：财务驳回 → 单据回草稿 ----------------
    print("\n== ⑤ 复核是真闸：超限小额单（额度 1000 / 下单 5 千）被财务驳回 ==")
    set_credit(1000)
    o3 = create_order("5000")
    st, body = submit(o3["id"])
    ok("⑤ 超限单提交成功", st == 200, f"{st} {str(body)[:150]}")
    i3 = instance_of(o3["id"])
    seq4 = nodes_of(i3)
    ok("⑤ 5 千的超限单也先过财务复核（不分金额档，一律复核）",
       any(k == "n_fin" for k, _, _ in seq4), str(seq4))
    tasks3 = active_tasks(i3)
    ok("⑤ 财务复核有待办", len(tasks3) == 1 and tasks3[0][1] == "n_fin", str(tasks3))
    st, body = reject(fin_tok, tasks3[0][0], return_to="INITIATOR")
    ok("⑤ 财务经理驳回（退回发起人）", st == 200, f"{st} {str(body)[:150]}")
    seq4b = nodes_of(i3)
    ok("⑤ 驳回后没有进入任何金额档位节点（复核真的把单子挡住）",
       not any(k in ("n_dept", "n_pm", "n_board") for k, _, _ in seq4b), str(seq4b))
    ok("⑤ 复核节点终态 REJECTED",
       any(k == "n_fin" and s == "REJECTED" for k, s, _ in seq4b), str(seq4b))
    d3 = order_detail(o3["id"])
    ok("⑤ 采购单**没有**变成 APPROVED（停在草稿/已提交，等发起人重提）",
       d3.get("status") in ("DRAFT", "SUBMITTED"), str(d3.get("status")))
    new_tasks = active_tasks(i3)
    ok("⑤ 退回发起人：新待办落在发起人（重提），不是又派回财务",
       len(new_tasks) == 1 and new_tasks[0][4] == "admin" and new_tasks[0][5] in ("RESUBMIT", "APPROVE"),
       str(new_tasks))
    ok("⑤ 采购单未被审批链路继续推进（仍是 SUBMITTED，不会被静默通过）",
       d3.get("status") == "SUBMITTED", str(d3.get("status")))

    # ---------------- ⑥ 额度 0 = 不限 ----------------
    print("\n== ⑥ 额度 0（= 不限）/ 下单 5 万 → 不校验、不加复核 ==")
    set_credit(0)
    o4 = create_order("50000")
    st, body = submit(o4["id"])
    ok("⑥ 不限额度时提交成功", st == 200, f"{st} {str(body)[:150]}")
    i4 = instance_of(o4["id"])
    seq5 = nodes_of(i4)
    ok("⑥ 走 e_credit_else（额度 0 不校验）",
       any(k == "c_credit" and e == "e_credit_else" for k, _, e in seq5), str(seq5))
    ok("⑥ 没有财务复核节点，直接进 1–10 万档（采购经理）",
       not any(k == "n_fin" for k, _, _ in seq5) and any(k == "n_pm" for k, _, _ in seq5), str(seq5))

    # ---------------- ⑦ 失败关闭：复核岗 0 人、兜底超管只剩制单人 → 提交 422（不是静默放行） ----------------
    print("\n== ⑦ 失败关闭：复核岗没人 + 兜底超管只剩制单人 → 超限单提交 422 ==")
    set_credit(1000)
    super_role = scalar("SELECT id FROM sys_role WHERE role_code='SUPER_ADMIN' AND deleted_at IS NULL LIMIT 1")
    strip = [
        ("FINANCE_MANAGER", fin_id), ("SUPER_ADMIN", fin_id),
        ("SUPER_ADMIN", pm_id),
    ]
    try:
        for code, uid in strip:
            psql(f"delete from sys_user_role where user_id='{uid}'::uuid "
                 f"and role_id=(select id from sys_role where role_code='{code}' and deleted_at is null limit 1)")
        o5 = create_order("5000")
        st, body = submit(o5["id"])
        code = (body.get("code") if isinstance(body, dict) else None)
        ok("⑦ 复核岗 0 人 + 兜底超管只剩制单人 → 提交被拒（422 + 65018，fail-closed）",
           st == 422 and code == 65018, f"{st} code={code} {str(body.get('msg'))[:160]}")
        ok("⑦ 被拒时没有生成审批实例（不留半截单）", instance_of(o5["id"]) is None,
           str(instance_of(o5["id"])))
        d5 = order_detail(o5["id"])
        ok("⑦ 单据留在草稿（不会变成 SUBMITTED 却没人审）", d5.get("status") == "DRAFT",
           str(d5.get("status")))
    finally:
        for code, uid in strip:
            psql(f"insert into sys_user_role (id, role_id, user_id) "
                 f"select gen_random_uuid(), id, '{uid}'::uuid from sys_role "
                 f"where role_code='{code}' and deleted_at is null "
                 f"and not exists (select 1 from sys_user_role x where x.user_id='{uid}'::uuid "
                 f"and x.role_id = sys_role.id)")
        # 采购经理岗的既有成员恢复
        for (uid,) in pm_extra:
            psql(f"insert into sys_user_role (id, role_id, user_id) values "
                 f"(gen_random_uuid(), '{ROLE_PURCHASE_MANAGER}'::uuid, '{uid}'::uuid) "
                 f"on conflict do nothing")
        print("  测试期间的临时角色变更已恢复")

    # ---------------- 汇总 ----------------
    passed = sum(1 for c, _, _ in RESULTS if c)
    total = len(RESULTS)
    print(f"\n== 汇总：{passed}/{total} 条断言通过 ==")
    for cond, name, detail in RESULTS:
        if not cond:
            print(f"  [FAIL] {name} — {detail}")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
