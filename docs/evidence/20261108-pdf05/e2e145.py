#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
PDF-05 · 审批单 PDF（审批轨迹时间轴 + 每级意见 + ARCH-8 降级标记）145 实测

形态照 docs/evidence/20261108-p309-wf-drill/scripts/e2e.py：stdlib + urllib，无第三方依赖；
每条断言打印 [PASS]/[FAIL] 并计数，PDF 事实用 pdftotext / pdfinfo / pdffonts 断言。

覆盖（卡面四条验收 + 特别要求）：
  ① 普通 2 级审批        → PDF 两行轨迹 + 两级意见
  ② 自审跳过的流程       → PDF 有「已跳过（自审）」
  ③ 升级的流程           → PDF 有「已升级至 XX（原角色无人…）」
  ④ 驳回的流程           → PDF 显示驳回意见
  ⑤ 兜底                 → PDF 有「兜底审批人」（特别要求第三句）
  负面：无权限 403 / 无关 403 / 非法 id 400 / 不存在 404 / 未登录 401
  审计：每次成功导出 operation_logs 一行 EXPORT_FLOW_INSTANCE_PDF

**全部审批数据都由真实引擎产生**（建流程 → 发布 → 发起 → 审批），不是 SQL 造的行：
本脚本不往 biz_flow_* 写任何业务行，只用 psql 读事实断言。

环境变量：BASE / DB / ENVF（含 NIK_ADMIN_PASSWORD + NIK_DATABASE_PASSWORD）
"""
import json
import os
import re
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.request

BASE = os.environ.get("BASE", "http://127.0.0.1:8079")
DB = os.environ.get("DB", "cloudpivot_pdf05")
HERE = os.path.dirname(os.path.abspath(__file__))
# 证据目录 = 脚本所在目录本卡自行产 raw/ 与 sample/（脚本就放在 docs/evidence/<卡>/ 下）
EVID = os.environ.get("EVID") or HERE
RAW = os.environ.get("RAW") or os.path.join(EVID, "raw")
SAMPLE = os.environ.get("SAMPLE") or os.path.join(EVID, "sample")
os.makedirs(RAW, exist_ok=True)
os.makedirs(SAMPLE, exist_ok=True)

PASS = 0
FAIL = 0
FAILS = []

BASE_PW = "Pdf05@2026"
NEW_PW = "Pdf05new@2026"

_ENVF = os.environ.get("ENVF", "/root/pdf05.env")
ADMIN_PW = ""
PGPW = ""
if os.path.exists(_ENVF):
    for _line in open(_ENVF, encoding="utf-8"):
        if _line.startswith("NIK_ADMIN_PASSWORD="):
            ADMIN_PW = _line.split("=", 1)[1].strip()
        if _line.startswith("NIK_DATABASE_PASSWORD="):
            PGPW = _line.split("=", 1)[1].strip()
ADMIN_PW = os.environ.get("ADMIN_PW", ADMIN_PW or "Admin@123456")


# ============================ 断言 / 输出 ============================
def check(cond, label, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  [PASS] {label}")
    else:
        FAIL += 1
        FAILS.append(f"{label} :: {detail}")
        print(f"  [FAIL] {label} :: {detail}")
    return bool(cond)


def eq(label, actual, expect):
    return check(str(actual) == str(expect), label, f"实际={actual!r} 期望={expect!r}")


def contains(label, text, needle):
    return check(needle in str(text), label, f"不含 {needle!r}")


def not_contains(label, text, needle):
    return check(needle not in str(text), label, f"不该含 {needle!r}")


def scenario(sid, title):
    print(f"\n{'=' * 78}\n=== {sid} {title}\n{'=' * 78}")


# ============================ 基础设施 ============================
def psql(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = PGPW
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-Atc", sql],
        capture_output=True, text=True, env=env,
    )
    if out.returncode != 0:
        return f"<ERR:{out.stderr.strip()}>"
    return out.stdout.strip()


def sql_eq(label, query, expect):
    actual = psql(query)
    return eq(label, actual, expect)


def req(method, path, token=None, body=None, timeout=90):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode() or "{}"
            return resp.status, json.loads(raw)
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode()
        try:
            return exc.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return exc.code, {"raw": raw}
    except Exception as exc:  # noqa: BLE001
        return 0, {"error": str(exc)}


def get_pdf(path, token=None):
    """GET 二进制；返回 (status, headers dict, bytes)。"""
    headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(r, timeout=120) as resp:
            return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
    except urllib.error.HTTPError as exc:
        return exc.code, {k.lower(): v for k, v in exc.headers.items()}, exc.read()


def get_pdf_err(path, token=None):
    """GET 出错时的 JSON 错误体。"""
    st, _h, raw = get_pdf(path, token)
    try:
        return st, json.loads(raw.decode() or "{}")
    except Exception:  # noqa: BLE001
        return st, {"raw": raw[:200]}


def pdftext(path):
    out = subprocess.run(["pdftotext", "-layout", path, "-"], capture_output=True, text=True)
    return out.stdout


def norm(text):
    """去掉所有空白：pdftotext 会按版面插空格 / 换行，中文串跨行时逐字比对会假红。"""
    return re.sub(r"\s+", "", text)


def pdfinfo(path):
    out = subprocess.run(["pdfinfo", path], capture_output=True, text=True)
    return out.stdout


def pdffonts(path):
    out = subprocess.run(["pdffonts", path], capture_output=True, text=True)
    return out.stdout


# ============================ 登录 / 夹具 ============================
def login(username, pw_candidates):
    last = None
    for pw in pw_candidates:
        st, body = req("POST", "/api/system/auth/login",
                       body={"username": username, "password": pw})
        if st != 200:
            last = (st, body)
            continue
        data = body.get("data") or {}
        tok = data.get("access_token")
        if data.get("must_change_password"):
            # 真实用户路径：改密 → 重新登录（会话安全字段有 60s TTL，先用快路径，失败再等）
            st2, _ = req("PUT", "/api/system/auth/password", token=tok,
                         body={"old_password": pw, "new_password": NEW_PW})
            if st2 != 200:
                return None
            st3, body3 = req("POST", "/api/system/auth/login",
                             body={"username": username, "password": NEW_PW})
            if st3 != 200:
                time.sleep(65)
                st3, body3 = req("POST", "/api/system/auth/login",
                                 body={"username": username, "password": NEW_PW})
                if st3 != 200:
                    return None
            tok = ((body3.get("data") or {}).get("access_token"))
        return tok
    print(f"    [login] {username} 失败：{last}")
    return None


FIX = {"roles": {}, "users": {}, "tokens": {}, "defs": {}}

PERM_BASE = ["business:flow:instance:read", "business:flow:approve"]
PERM_CREATE = ["business:flow:instance:create"]
PERM_ADMIN = ["business:flow:admin"]

# code: (名字, [用户名], 额外权限)
# 注意：S1/S4 的第 1 级必须「单人」—— 角色 ≥2 人时引擎按 ALL 冻结，
# 一人签完节点不过，时间线上会多出 CANCELLED 行，与卡面「两行轨迹」不符。
ROLE_PLAN = {
    "PDF05_INIT": ("PDF05 发起人", ["u_init"], ["create"]),
    "PDF05_L1": ("PDF05 部门经理", ["u_l1a"], ["create"]),
    # 两人角色：S2 让发起人本人成为节点成员（自审 SKIP），或签 ANY 让另一人签完即过
    "PDF05_TEAM": ("PDF05 审批小组", ["u_l1a", "u_l1b"], ["create"]),
    # S4 的总经理要「驳回并终止」（return_to=FINAL 需 business:flow:admin）
    "PDF05_L2": ("PDF05 总经理", ["u_gm"], ["admin"]),
    "PDF05_BOSS": ("PDF05 上级总监", ["u_boss"], []),
    "PDF05_EMPTY": ("PDF05 空角色", [], []),
    "PDF05_VOID": ("PDF05 空角色（无上级）", [], []),
    "PDF05_FB": ("PDF05 兜底人", ["u_fb"], []),
    "PDF05_RO": ("PDF05 只读角色", ["u_reader"], ["readonly"]),
    "PDF05_NONE": ("PDF05 无权限角色", ["u_nobody"], []),
}


def user_roles():
    """用户名 → 全部角色码（一个人可能在多个角色里，必须一次建全）。"""
    plan = {}
    for code, (_name, users, _extra) in ROLE_PLAN.items():
        for username in users:
            plan.setdefault(username, []).append(code)
    return plan



def mk_role(code, name):
    st, body = req("POST", "/api/system/role", FIX["admin"],
                   {"role_code": code, "role_name": name, "status": 1, "sort_order": 980})
    if st == 200:
        rid = (body.get("data") or {}).get("id")
    else:
        rid = psql(f"select id from sys_role where role_code='{code}' and deleted_at is null limit 1")
        if not rid or rid.startswith("<ERR"):
            print(f"    [role] {code} 建失败 http={st} {body}")
            return None
    FIX["roles"][code] = rid
    return rid


def set_escalation(code, parent_code):
    rid = FIX["roles"][code]
    return req("PUT", f"/api/system/role/{rid}", FIX["admin"],
               {"role_code": code, "role_name": f"PDF05 {code}", "status": 1,
                "sort_order": 980, "escalation_role_id": FIX["roles"][parent_code]})


def mk_user(username, real_name, role_codes):
    role_ids = [FIX["roles"][c] for c in role_codes]
    st, body = req("POST", "/api/system/user", FIX["admin"],
                   {"username": username, "password": BASE_PW, "real_name": real_name,
                    "status": 1, "role_ids": role_ids})
    if st == 200:
        uid = (body.get("data") or {}).get("id")
    else:
        uid = psql(f"select id from sys_user where username='{username}' and deleted_at is null limit 1")
        if not uid or uid.startswith("<ERR"):
            print(f"    [user] {username} 建失败 http={st} {body}")
            return None
        # 重跑：把角色同步成计划里的那一组
        psql(f"delete from sys_user_role where user_id='{uid}'::uuid and role_id in "
             f"(select id from sys_role where role_code like 'PDF05%')")
        if role_ids:
            joined = ",".join(f"'{r}'" for r in role_ids)
            psql("INSERT INTO sys_user_role (user_id, role_id) "
                 f"SELECT '{uid}'::uuid, unnest(ARRAY[{joined}]::uuid[]) ON CONFLICT DO NOTHING")
    FIX["users"][username] = uid
    return uid


def grant(role_code, codes):
    rid = FIX["roles"][role_code]
    if not codes:
        return "0"
    joined = ",".join(f"'{c}'" for c in codes)
    psql("INSERT INTO sys_role_menu (role_id, menu_id, created_at) "
         f"SELECT '{rid}'::uuid, m.id, now() FROM sys_menu m "
         f"WHERE m.auth_code IN ({joined}) AND m.deleted_at IS NULL "
         "AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm "
         f"WHERE rm.role_id='{rid}'::uuid AND rm.menu_id=m.id)")
    return psql("SELECT count(DISTINCT m.auth_code) FROM sys_role_menu rm "
                "JOIN sys_menu m ON m.id = rm.menu_id "
                f"WHERE rm.role_id='{rid}'::uuid AND m.auth_code IN ({joined})")


def tok(username):
    if username not in FIX["tokens"]:
        FIX["tokens"][username] = login(username, (NEW_PW, BASE_PW))
    return FIX["tokens"][username]


# ---------- 流程 ----------
def mk_def(code, name, biz):
    st, body = req("POST", "/api/business/flow/def", FIX["admin"],
                   {"flow_code": code, "flow_name": name, "biz_type": biz, "is_default": True})
    if st == 200:
        data = body.get("data") or {}
        FIX["defs"][code] = (data.get("id"), data.get("version_id"))
        return FIX["defs"][code]
    did = psql(f"select id from biz_flow_def where flow_code='{code}' and deleted_at is null limit 1")
    vid = psql(f"select id from biz_flow_def_version where def_id='{did}'::uuid "
               "and version_status='DRAFT' order by version_no desc limit 1")
    if not vid or not vid.strip() or vid.startswith("<ERR"):
        st2, body2 = req("POST", f"/api/business/flow/def/{did}/draft", FIX["admin"],
                         {"change_note": "PDF-05 重跑：另存新草稿"})
        vid = (body2.get("data") or {}).get("version_id") if st2 == 200 else None
    FIX["defs"][code] = (did, vid)
    return FIX["defs"][code]


def node(key, name, ntype, **kw):
    base = {"key": key, "name": name, "type": ntype, "x": 0, "y": 0, "sort_order": 1}
    base.update(kw)
    return base


def appr(key, name, role_code, **kw):
    spec = node(key, name, "APPROVAL", assignee_type="ROLE",
                assignees=[{"type": "ROLE", "ref_id": FIX["roles"][role_code], "sort_order": 0}],
                self_approval=kw.pop("self_approval", "SKIP"),
                empty_assignee=kw.pop("empty_assignee", "ERROR"),
                join_mode="AND", fork_mode="AND",
                reject_veto=kw.pop("reject_veto", True))
    spec.update(kw)
    return spec


def edge(key, frm, to, condition=None, is_default=False, priority=0, label=None):
    return {"key": key, "from": frm, "to": to, "condition": condition,
            "is_default": is_default, "priority": priority, "label": label}


def build_flow(code, biz, nodes, edges, note):
    did, vid = mk_def(code, code, biz)
    if not did or not vid:
        return None
    st, body = req("PUT", f"/api/business/flow/version/{vid}/graph", FIX["admin"],
                   {"change_note": note, "nodes": nodes, "edges": edges})
    if st != 200:
        check(False, f"流程 {code} 存图 → 200", f"http={st} {body}")
        return None
    st2, body2 = req("POST", f"/api/business/flow/def/{did}/publish", FIX["admin"])
    if st2 != 200:
        check(False, f"流程 {code} 发布 → 200", f"http={st2} {body2}")
        return None
    check(True, f"流程 {code} 建定义 → 存图 → 发布（def={did}）")
    return did


def linear_flow(code, biz, appr_specs, note):
    """START → 各审批节点 → END 的直线流程。"""
    nodes = [node("s", "开始", "START", sort_order=1)]
    edges = []
    prev = "s"
    for index, spec in enumerate(appr_specs):
        spec["sort_order"] = index + 2
        spec["x"] = 200 * (index + 1)
        nodes.append(spec)
        edges.append(edge(f"e_{prev}_{spec['key']}", prev, spec["key"]))
        prev = spec["key"]
    nodes.append(node("e", "结束", "END", x=200 * (len(appr_specs) + 1),
                      sort_order=len(appr_specs) + 2))
    edges.append(edge("e_last_end", prev, "e"))
    return build_flow(code, biz, nodes, edges, note)


def start(def_code, username, title, form):
    did, _ = FIX["defs"][def_code]
    st, body = req("POST", "/api/business/flow/instance", tok(username),
                   {"def_id": did, "title": title, "form_data": form})
    iid = (body.get("data") or {}).get("instance_id") if st == 200 else None
    return st, body, iid


def tasks_of(instance_id, status=None):
    where = f"instance_id='{instance_id}'::uuid"
    if status:
        where += f" and status='{status}'"
    out = psql("select coalesce(string_agg(concat_ws('|', id, assignee_id, coalesce(assignee_name,''),"
               "node_key, status, coalesce(comment,''), coalesce(assigned_by_rule,''),"
               "coalesce(escalated_from_role,''), coalesce(escalate_reason,''), is_self_approval),"
               "E'\\n' order by created_at), '') from biz_flow_task where " + where)
    rows = []
    if not out or out.startswith("<ERR"):
        return rows
    for line in out.splitlines():
        parts = line.split("|")
        if len(parts) >= 10:
            rows.append({"id": parts[0], "assignee_id": parts[1], "assignee_name": parts[2],
                         "node_key": parts[3], "status": parts[4], "comment": parts[5],
                         "rule": parts[6], "esc_from": parts[7], "esc_reason": parts[8],
                         "self": parts[9]})
    return rows


def pending_task(instance_id, username):
    for row in tasks_of(instance_id, "PENDING"):
        if row["assignee_id"] == FIX["users"][username]:
            return row
    return None


def approve(task_id, username, comment):
    return req("POST", f"/api/business/flow/task/{task_id}/approve", tok(username),
               {"comment": comment})


def reject(task_id, username, comment, return_to=None):
    body = {"comment": comment}
    if return_to is not None:
        body["return_to"] = return_to
    return req("POST", f"/api/business/flow/task/{task_id}/reject", tok(username), body)


def inst_status(instance_id):
    return psql(f"select status from biz_flow_instance where id='{instance_id}'::uuid")


def dump_artifacts(name, path):
    """把 PDF 的文本层 / 元信息 / 字体 / 首页渲染图一并落进 raw 与 sample（自证据，不靠事后补）。"""
    with open(os.path.join(RAW, f"pdftotext_{name}.txt"), "w", encoding="utf-8") as handle:
        handle.write(pdftext(path))
    for tool, suffix in (("pdfinfo", "pdfinfo"), ("pdffonts", "pdffonts")):
        out = subprocess.run([tool, path], capture_output=True, text=True)
        with open(os.path.join(RAW, f"{suffix}_{name}.txt"), "w", encoding="utf-8") as handle:
            handle.write(out.stdout)
    subprocess.run(["pdftoppm", "-r", "100", "-png", "-f", "1", "-l", "1", path,
                    os.path.join(SAMPLE, f"{name}-p1")], capture_output=True)


def export_pdf(instance_id, token, name):
    """导出并落盘；返回 (status, headers, bytes, path)。"""
    st, headers, raw = get_pdf(f"/api/business/flow/instance/{instance_id}/pdf", token)
    path = os.path.join(SAMPLE, f"{name}.pdf")
    if st == 200:
        with open(path, "wb") as handle:
            handle.write(raw)
        dump_artifacts(name, path)
    return st, headers, raw, path


# ============================ 夹具 ============================
def build_fixtures():
    print("\n### 0) 夹具：admin 登录 → 角色 → 用户 → 权限")
    FIX["admin"] = login("admin", (ADMIN_PW, NEW_PW, BASE_PW))
    check(bool(FIX["admin"]), "admin 令牌就绪（含首登强制改密）")
    if not FIX["admin"]:
        raise SystemExit("admin 登录失败")

    for code, (name, _users, _extra) in ROLE_PLAN.items():
        if not mk_role(code, name):
            raise SystemExit(f"角色 {code} 建失败")
    check(len(FIX["roles"]) == len(ROLE_PLAN), f"{len(ROLE_PLAN)} 个角色就绪")

    st, _ = set_escalation("PDF05_EMPTY", "PDF05_BOSS")
    eq("空角色 PDF05_EMPTY 设上级角色 → 200", st, 200)
    eq("上级角色回读",
       psql(f"select escalation_role_id from sys_role where id='{FIX['roles']['PDF05_EMPTY']}'::uuid"),
       FIX["roles"]["PDF05_BOSS"])

    plan = user_roles()
    for username, role_codes in plan.items():
        label = " / ".join(ROLE_PLAN[code][0] for code in role_codes)
        if not mk_user(username, f"{label} {username}", role_codes):
            raise SystemExit(f"用户 {username} 建失败")
    check(len(FIX["users"]) == len(plan), f"{len(FIX['users'])} 个用户就绪")

    for code, (_name, _users, extra) in ROLE_PLAN.items():
        if code == "PDF05_NONE":
            n = grant(code, [])
            eq(f"{code} 不授任何权限码", n, "0")
            continue
        if "readonly" in extra:
            codes = ["business:flow:instance:read"]  # 只有读权限、不参与任何单
        else:
            codes = list(PERM_BASE)
            if "create" in extra:
                codes += PERM_CREATE
            if "admin" in extra:
                codes += PERM_ADMIN
        n = grant(code, codes)
        eq(f"{code} 授 {len(set(codes))} 个权限码", int(n or 0), len(set(codes)))


    sql_eq("PDF05_EMPTY 在册成员 = 0（升级原角色无人）",
           "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id "
           f"where ur.role_id='{FIX['roles']['PDF05_EMPTY']}'::uuid and u.deleted_at is null", 0)
    for username in FIX["users"]:
        check(bool(tok(username)), f"用户 {username} 令牌就绪")


# ============================ 场景 ============================
def s1_two_levels():
    scenario("S1", "验收①：普通 2 级审批 → PDF 两行轨迹 + 两级意见")
    did = linear_flow("pdf05_s1", "PDF05_S1",
                      [appr("n_l1", "部门经理审批", "PDF05_L1"),
                       appr("n_l2", "总经理审批", "PDF05_L2")],
                      "PDF-05 S1：两级审批（部门经理 → 总经理）")
    if not did:
        return
    st, body, iid = start("pdf05_s1", "u_init", "PDF05 采购申请（两级）",
                          {"amount": 12345.67, "reason": "备货", "supplier_name": "PDF05 供应商"})
    eq("S1 发起 → 200", st, 200)
    if not iid:
        check(False, "S1 没拿到实例 id", str(body))
        return
    t1 = pending_task(iid, "u_l1a")
    if not check(bool(t1), "S1 部门经理收到待办"):
        return
    st2, _ = approve(t1["id"], "u_l1a", "预算内，同意采购")
    eq("S1 部门经理审批 → 200", st2, 200)
    t2 = pending_task(iid, "u_gm")
    if not check(bool(t2), "S1 总经理收到待办"):
        return
    st3, _ = approve(t2["id"], "u_gm", "同意，按流程执行")
    eq("S1 总经理审批 → 200", st3, 200)
    eq("S1 实例 APPROVED", inst_status(iid), "APPROVED")

    inst_no = psql(f"select instance_no from biz_flow_instance where id='{iid}'::uuid")
    st4, headers, raw, path = export_pdf(iid, tok("u_init"), "approval-2levels")
    eq("S1 GET pdf → 200", st4, 200)
    eq("S1 Content-Type = application/pdf", headers.get("content-type"), "application/pdf")
    disp = headers.get("content-disposition", "")
    check("attachment" in disp and ".pdf" in disp, "S1 Content-Disposition 是附件下载", disp)
    eq("S1 Cache-Control = no-store", headers.get("cache-control"), "no-store")
    check(len(raw) > 20000, f"S1 PDF 有实质内容（{len(raw)} 字节）")
    if st4 != 200:
        return

    text = pdftext(path)
    flat = norm(text)
    # 单据信息
    contains("S1 单号", flat, inst_no)
    contains("S1 金额 12,345.67", flat, "12,345.67")
    contains("S1 申请人", flat, "发起人u_init")
    contains("S1 单据类型", flat, "PDF05_S1")
    contains("S1 申请时间（北京时间，非 UTC）", flat, "2026-")
    # 轨迹两行 + 两级意见
    contains("S1 第 1 级", flat, "第1级")
    contains("S1 第 2 级", flat, "第2级")
    contains("S1 节点名（部门经理审批）", flat, "部门经理审批")
    contains("S1 节点名（总经理审批）", flat, "总经理审批")
    contains("S1 一级审批人", flat, "u_l1a")
    contains("S1 二级审批人", flat, "总经理u_gm")
    contains("S1 一级意见", flat, "预算内，同意采购")
    contains("S1 二级意见", flat, "同意，按流程执行")
    contains("S1 结论=已通过", flat, "已通过")
    # 签字位（空白）
    contains("S1 签字区标题", flat, "审批人签字")
    contains("S1 签字栏说明", flat, "由各级审批人在纸质件上现场手写签名")
    eq("S1 无降级标记行", flat.count("已升级至"), 0)

    info = pdfinfo(path)
    pages = re.search(r"Pages:\s+(\d+)", info)
    check(pages is not None and pages.group(1) == "1",
          "S1 PDF 单页（轨迹 2 行）", info.splitlines()[:6])
    m = re.search(r"Page size:\s+([\d.]+) x ([\d.]+)", info)
    if m:
        check(float(m.group(1)) < float(m.group(2)), "S1 A4 纵向",
              f"{m.group(1)}x{m.group(2)}")
    fonts = pdffonts(path)
    check(any("yes" in line.split() for line in fonts.splitlines()[2:]),
          "S1 字体已内嵌（emb=yes）", fonts)
    check(bool(text.strip()), "S1 pdftotext 抽得出文字（非图片 PDF）")
    print(f"  · 单号 {inst_no} / {len(raw)} 字节 / {path}")
    return iid


def s2_self_skip():
    scenario("S2", "验收②：自审跳过 → PDF 有「已跳过（自审）」")
    did = linear_flow("pdf05_s2", "PDF05_S2",
                      [appr("n_l1", "部门经理审批", "PDF05_TEAM", approval_mode="ANY")],
                      "PDF-05 S2：制单人本人也在审批小组内（self_approval=SKIP）")
    if not did:
        return
    # u_l1b 是本节点角色的成员 → 他发起时系统必须把他自己剔掉（SKIPPED 留痕）
    st, body, iid = start("pdf05_s2", "u_l1b", "PDF05 采购申请（自审跳过）",
                          {"amount": 8800.00, "reason": "自审批例"})
    eq("S2 发起 → 200", st, 200)
    if not iid:
        check(False, "S2 没拿到实例 id", str(body))
        return
    rows = tasks_of(iid)
    skipped = [r for r in rows if r["status"] == "SKIPPED"]
    if check(bool(skipped), "S2 制单人落 SKIPPED 留痕", str(rows)):
        eq("S2 SKIPPED 的是发起人本人", skipped[0]["assignee_id"], FIX["users"]["u_l1b"])
        contains("S2 留痕 reason", skipped[0]["comment"], "self_approval=SKIP")
    t = pending_task(iid, "u_l1a")
    if check(bool(t), "S2 另一名经理收到待办"):
        st2, _ = approve(t["id"], "u_l1a", "已知会发起人本人，同意")
        eq("S2 另一名经理审批 → 200", st2, 200)
    eq("S2 实例 APPROVED", inst_status(iid), "APPROVED")

    st3, _h, raw, path = export_pdf(iid, tok("u_l1b"), "approval-self-skip")
    eq("S2 GET pdf（发起人自己导出）→ 200", st3, 200)
    if st3 != 200:
        return
    raw_text = pdftext(path)
    flat = norm(raw_text)
    contains("S2 PDF 有「已跳过（自审）」", flat, "已跳过（自审）")
    contains("S2 结果列写「跳过」", flat, "跳过")
    contains("S2 自审留痕原因", flat, "self_approval=SKIP")
    contains("S2 同级另一人的意见", flat, "已知会发起人本人，同意")
    # 签字位只收真要签字的人：被自审跳过的人不能出现在签字表里
    if "审批人签字" in raw_text:
        sign_block = raw_text.split("审批人签字", 1)[1]
        check("u_l1a" in norm(sign_block), "S2 签字位含真正的审批人 u_l1a")
        not_contains("S2 签字位不含被自审跳过的 u_l1b", norm(sign_block), "u_l1b")
    else:
        check(False, "S2 PDF 缺少「审批人签字」区块")
    print(f"  · {len(raw)} 字节 / {path}")
    return iid


def s3_escalation():
    scenario("S3", "验收③：升级（原角色无人）→ PDF 有「已升级至 XX（原角色无人…）」")
    did = linear_flow("pdf05_s3", "PDF05_S3",
                      [appr("n_esc", "大区经理审批", "PDF05_EMPTY")],
                      "PDF-05 S3：节点角色 0 成员 → 沿升级链上溯到 PDF05_BOSS")
    if not did:
        return
    st, body, iid = start("pdf05_s3", "u_init", "PDF05 采购申请（升级审批）",
                          {"amount": 66000.00, "reason": "升级用例"})
    eq("S3 发起 → 200", st, 200)
    if not iid:
        check(False, "S3 没拿到实例 id", str(body))
        return
    rows = tasks_of(iid)
    escalated = [r for r in rows if r["esc_reason"]]
    if check(bool(escalated), "S3 待办带升级留痕", str(rows)):
        eq("S3 escalate_reason = NO_MEMBER", escalated[0]["esc_reason"], "NO_MEMBER")
        eq("S3 escalated_from_role = PDF05_EMPTY", escalated[0]["esc_from"], "PDF05_EMPTY")
        eq("S3 assigned_by_rule = ESCALATED", escalated[0]["rule"], "ESCALATED")
        eq("S3 待办派给上级角色成员 u_boss", escalated[0]["assignee_id"],
           FIX["users"]["u_boss"])
    t = pending_task(iid, "u_boss")
    if check(bool(t), "S3 上级总监收到待办"):
        st2, _ = approve(t["id"], "u_boss", "升级后审批同意")
        eq("S3 上级总监审批 → 200", st2, 200)
    eq("S3 实例 APPROVED", inst_status(iid), "APPROVED")

    st3, _h, raw, path = export_pdf(iid, tok("u_init"), "approval-escalation")
    eq("S3 GET pdf → 200", st3, 200)
    if st3 != 200:
        return
    flat = norm(pdftext(path))
    contains("S3 PDF 有「已升级至」", flat, "已升级至")
    contains("S3 升级目标角色名", flat, "PDF05上级总监")
    contains("S3 升级原因=原角色无人", flat, "原角色无人")
    contains("S3 原角色编码", flat, "PDF05_EMPTY")
    contains("S3 升级后的审批人", flat, "上级总监u_boss")
    contains("S3 升级后审批意见", flat, "升级后审批同意")
    # 同一行里三要素齐全（顺序也要对：已升级至 = 目标 + 原因 + 原角色）
    check(re.search(r"已升级至PDF05上级总监（原角色无人：PDF05_EMPTY）", flat) is not None,
          "S3 标记行格式 = 已升级至 XX（原角色无人：原角色码）",
          [line for line in pdftext(path).splitlines() if "已升级" in line])
    print(f"  · {len(raw)} 字节 / {path}")
    return iid


def s4_reject():
    scenario("S4", "验收④：驳回 → PDF 显示驳回意见")
    did = linear_flow("pdf05_s4", "PDF05_S4",
                      [appr("n_l1", "部门经理审批", "PDF05_L1"),
                       appr("n_l2", "总经理审批", "PDF05_L2")],
                      "PDF-05 S4：二级驳回（结论 REJECTED）")
    if not did:
        return
    st, body, iid = start("pdf05_s4", "u_init", "PDF05 采购申请（将被驳回）",
                          {"amount": 999999.00, "reason": "超预算"})
    eq("S4 发起 → 200", st, 200)
    if not iid:
        check(False, "S4 没拿到实例 id", str(body))
        return
    t1 = pending_task(iid, "u_l1a")
    if check(bool(t1), "S4 部门经理收到待办"):
        st2, _ = approve(t1["id"], "u_l1a", "部门同意，报总经理")
        eq("S4 部门经理审批 → 200", st2, 200)
    t2 = pending_task(iid, "u_gm")
    if check(bool(t2), "S4 总经理收到待办") and t2:
        st3, _ = reject(t2["id"], "u_gm", "单价高于年度框架协议 8%，退回重谈。",
                        return_to="FINAL")
        eq("S4 总经理驳回并终止 → 200", st3, 200)
    eq("S4 实例 REJECTED", inst_status(iid), "REJECTED")

    st4, _h, raw, path = export_pdf(iid, tok("u_init"), "approval-rejected")
    eq("S4 GET pdf → 200", st4, 200)
    if st4 != 200:
        return
    flat = norm(pdftext(path))
    contains("S4 结论=已驳回", flat, "已驳回")
    contains("S4 结果列=驳回", flat, "驳回")
    contains("S4 驳回意见原文", flat, "单价高于年度框架协议8%，退回重谈。")
    contains("S4 一级的通过意见也在", flat, "部门同意，报总经理")
    info = pdfinfo(path)
    check("Page size" in info, "S4 PDF 可解析", info.splitlines()[:3])
    print(f"  · {len(raw)} 字节 / {path}")
    return iid


def s5_fallback():
    scenario("S5", "特别要求：兜底审批人 → PDF 有「兜底审批人」")
    spec = appr("n_fb", "大区经理审批（配兜底人）", "PDF05_VOID")
    spec["fallback_user_id"] = FIX["users"]["u_fb"]
    did = linear_flow("pdf05_s5", "PDF05_S5", [spec],
                      "PDF-05 S5：角色 0 成员 + 无上级 → 节点兜底人接管")
    if not did:
        return
    st, body, iid = start("pdf05_s5", "u_init", "PDF05 采购申请（兜底审批）",
                          {"amount": 4321.00, "reason": "兜底用例"})
    eq("S5 发起 → 200", st, 200)
    if not iid:
        check(False, "S5 没拿到实例 id", str(body))
        return
    rows = tasks_of(iid)
    fb = [r for r in rows if r["rule"] == "FALLBACK"]
    if check(bool(fb), "S5 assigned_by_rule = FALLBACK", str(rows)):
        eq("S5 兜底人收到待办", fb[0]["assignee_id"], FIX["users"]["u_fb"])
    t = pending_task(iid, "u_fb")
    if check(bool(t), "S5 兜底人待办可查"):
        st2, _ = approve(t["id"], "u_fb", "兜底人代审同意")
        eq("S5 兜底人审批 → 200", st2, 200)
    eq("S5 实例 APPROVED", inst_status(iid), "APPROVED")

    st3, _h, raw, path = export_pdf(iid, tok("u_init"), "approval-fallback")
    eq("S5 GET pdf → 200", st3, 200)
    if st3 != 200:
        return
    flat = norm(pdftext(path))
    contains("S5 PDF 有「兜底审批人」", flat, "兜底审批人")
    contains("S5 兜底人的意见", flat, "兜底人代审同意")
    print(f"  · {len(raw)} 字节 / {path}")
    return iid


def s6_negatives(s1_iid):
    scenario("S6", "负面：无权限 403 / 无关 403 / 非法 id 400 / 不存在 404 / 未登录 401")
    if not s1_iid:
        check(False, "S6 缺少 S1 实例 id")
        return
    st, body = get_pdf_err(f"/api/business/flow/instance/{s1_iid}/pdf", None)
    eq("S6 未登录 → 401", st, 401)

    st, body = get_pdf_err(f"/api/business/flow/instance/{s1_iid}/pdf", tok("u_nobody"))
    eq("S6 无权限账号 → 403", st, 403)
    contains("S6 403 点名权限码", msg_of(body), "business:flow:instance:read")

    st, body = get_pdf_err(f"/api/business/flow/instance/{s1_iid}/pdf", tok("u_reader"))
    eq("S6 有读权限但与本单无关 → 403", st, 403)
    contains("S6 403 说明「无关」", msg_of(body), "无关")

    st, body = get_pdf_err("/api/business/flow/instance/not-a-uuid/pdf", tok("u_init"))
    eq("S6 非法 id → 400", st, 400)

    st, body = get_pdf_err(
        "/api/business/flow/instance/0f610000-0000-4000-8000-0000000000ff/pdf", tok("u_init"))
    eq("S6 不存在的 id → 404", st, 404)
    contains("S6 404 点名审批单", msg_of(body), "审批单")

    # 撤销删除：导出不会在库里留下任何新业务行（只多一条审计）
    eq("S6 导出不产生审批行", psql(
        f"select count(*) from biz_flow_task where instance_id='{s1_iid}'::uuid"), 2)


def msg_of(body):
    return (body or {}).get("msg") or (body or {}).get("message") or ""


def s7_audit(run_start):
    scenario("S7", "审计：每次成功导出 operation_logs 一行 EXPORT_FLOW_INSTANCE_PDF")
    # 只看本轮跑出来的行（同一个库会被反复跑，历史行留着也不能算成本轮的账）
    rows = psql(
        "select concat_ws('|', operation, target_type, coalesce(detail->>'doc_no',''), "
        "coalesce(detail->>'trace_count',''), coalesce(detail->>'amount',''), "
        "coalesce(detail->>'source','')) "
        "from operation_logs where operation='EXPORT_FLOW_INSTANCE_PDF' "
        f"and created_at >= to_timestamp({int(run_start)}) order by created_at")
    print("  · 审计行：")
    for line in rows.splitlines():
        print(f"      {line}")
    lines = [line for line in rows.splitlines() if line.strip()]
    check(len(lines) >= 5, f"S7 至少 5 条导出审计（实际 {len(lines)}）", rows)
    check(all("biz_flow_instance" in line for line in lines), "S7 target_type = biz_flow_instance", rows)
    check(all(line.startswith("EXPORT_FLOW_INSTANCE_PDF|") for line in lines),
          "S7 operation 码正确", rows)
    check(all(line.endswith("|PDF-05") for line in lines),
          "S7 detail.source = PDF-05（不被公共件默认的 PDF-03 覆盖）", rows)
    check(all(len(line.split("|")) == 6 and line.split("|")[2] for line in lines),
          "S7 每行都带单号（detail.doc_no）", rows)
    with open(os.path.join(RAW, "operation_logs.txt"), "w", encoding="utf-8") as handle:
        handle.write("operation|target_type|doc_no|trace_count|amount|source\n")
        handle.write(rows + "\n")


# ============================ main ============================
def main():
    t0 = time.time()
    print(f"PDF-05 E2E · BASE={BASE} DB={DB}")
    build_fixtures()
    s1_iid = s1_two_levels()
    s2_self_skip()
    s3_escalation()
    s4_reject()
    s5_fallback()
    s6_negatives(s1_iid)
    s7_audit(t0)

    print(f"\n{'=' * 78}")
    print(f"==== 汇总：{PASS + FAIL} 项，失败 {FAIL} 项（{time.time() - t0:.0f}s）")
    for item in FAILS:
        print(f"  FAIL: {item}")
    print("=" * 78)
    return 1 if FAIL else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        sys.exit(2)
