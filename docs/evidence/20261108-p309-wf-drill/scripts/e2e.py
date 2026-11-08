#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
P3-09 · 工作流全链路回归演练（ARCH-8 八场景 + ARCH-5 并行/条件/版本 + 驳回四目标）

形态照 docs/evidence/20261108-p203-purchase-return/e2e.py：stdlib + requests 风格的
urllib 封装（无第三方依赖），每条断言打印 [PASS]/[FAIL] 并计数，DB 事实用 psql 断言。

覆盖：
  A. ARCH-8 §7.3 八条 ①-⑧（自审 SKIP/升级/兜底/ALLOW 审计/委托/FORBID/默认或签/留痕）
  B. ARCH-5 三项「已交付但零生产用例」：并行网关（AND 汇聚 / OR 汇聚）/ 条件动态路由 / 版本冻结
  C. 驳回四目标：PREV / INITIATOR / NONE(FINAL) / 指定 node_key（+ 非 admin 传 node_key → 403）

环境：隔离实例由 scripts/00_run.sh 用**全新空库**起在 $BASE；本脚本只消费它。
不读不写 144 生产库。
"""
import json
import os
import subprocess
import sys
import traceback
import urllib.error
import urllib.request
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone

BASE = os.environ.get("BASE", "http://127.0.0.1:18091")
DB = os.environ.get("DB", "cloudpivot_p309_drill")
HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.environ.get("RAW") or os.path.join(HERE, "..", "raw")
os.makedirs(RAW, exist_ok=True)
SQL_LOG_PATH = os.path.join(RAW, "21_sql_assertions.txt")
SQL_LOG = open(SQL_LOG_PATH, "w", encoding="utf-8")

PASS = 0
FAIL = 0
FAILS = []
SCEN = []
STAGE = sys.argv[1:]  # 可选：只跑指定场景（A1 B2 …）

BASE_PW = "P309@2026"
NEW_PW = "P309new@2026"
CST = timezone(timedelta(hours=8))
MONTH = datetime.now(CST).strftime("%Y-%m")

# admin 初始口令只从 600 权限的环境文件来（仓库不落口令）
_ENVF = os.environ.get("ENVF", "/root/p309/instance.env")
ADMIN_PW = os.environ.get("ADMIN_PW", "")
if not ADMIN_PW and os.path.exists(_ENVF):
    for _line in open(_ENVF, encoding="utf-8"):
        if _line.startswith("NIK_ADMIN_PASSWORD="):
            ADMIN_PW = _line.split("=", 1)[1].strip()


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
    return check(needle in str(text), label, f"{text!r} 不含 {needle!r}")


def not_contains(label, text, needle):
    return check(needle not in str(text), label, f"{text!r} 不该含 {needle!r}")


@contextmanager
def scenario(sid, title):
    p0, f0 = PASS, FAIL
    print(f"\n{'=' * 78}\n=== {sid} {title}\n{'=' * 78}")
    SQL_LOG.write(f"\n===== {sid} {title} =====\n")
    try:
        yield
    except Exception as exc:  # noqa: BLE001 —— 单场景炸掉不能中断整场演练
        FAIL_add = f"{sid} 场景异常：{exc!r}"
        FAILS.append(FAIL_add)
        globals()["FAIL"] = FAIL + 1
        print(f"  [FAIL] {FAIL_add}")
        traceback.print_exc()
    SCEN.append((sid, title, PASS - p0, FAIL - f0))


# ============================ 基础设施 ============================
def psql(sql, db=None):
    env = dict(os.environ)
    env["PGPASSWORD"] = "alun"
    env["PATH"] = "/usr/bin:/bin:/usr/local/bin:" + env.get("PATH", "")
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", db or DB, "-Atc", sql],
        capture_output=True, text=True, env=env,
    )
    if out.returncode != 0:
        return f"<ERR:{out.stderr.strip()}>"
    return out.stdout.strip()


def sql_eq(label, query, expect):
    actual = psql(query)
    SQL_LOG.write(f"[{label}]\n  SQL: {' '.join(query.split())}\n  实际: {actual}\n 期望: {expect}\n")
    return eq(label, actual, expect)


def sql_log(label, query):
    actual = psql(query)
    SQL_LOG.write(f"[快照 {label}]\n  SQL: {' '.join(query.split())}\n  结果:\n{actual}\n")
    print(f"  · {label}:")
    for line in actual.splitlines()[:12]:
        print(f"      {line}")
    return actual


def req(method, path, token=None, body=None, timeout=40):
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


def code_of(body):
    return (body or {}).get("code")


def msg_of(body):
    return (body or {}).get("msg") or (body or {}).get("message") or ""


def login(username, pw_candidates=(NEW_PW, BASE_PW)):
    last = None
    for pw in pw_candidates:
        st, body = req("POST", "/api/system/auth/login", body={"username": username, "password": pw})
        if st != 200:
            last = (st, body)
            continue
        data = body.get("data") or {}
        tok = data.get("access_token")
        if data.get("must_change_password"):
            st2, _ = req("PUT", "/api/system/auth/password", token=tok,
                         body={"old_password": pw, "new_password": NEW_PW})
            if st2 != 200:
                return None
            st3, body3 = req("POST", "/api/system/auth/login",
                             body={"username": username, "password": NEW_PW})
            if st3 != 200:
                return None
            tok = ((body3.get("data") or {}).get("access_token"))
        return tok
    print(f"    [login] {username} 失败：{last}")
    return None


# ---------- 夹具：角色 / 用户 / 授权 / 流程 ----------
FIX = {"roles": {}, "users": {}, "tokens": {}, "defs": {}}


def mk_role(code, name, escalation=None, sort=980):
    st, body = req("POST", "/api/system/role", FIX["admin"],
                   {"role_code": code, "role_name": name, "status": 1, "sort_order": sort,
                    "escalation_role_id": escalation or ""})
    rid = None
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
    st, body = req("PUT", f"/api/system/role/{rid}", FIX["admin"],
                   {"role_code": code, "role_name": code, "status": 1, "sort_order": 980,
                    "escalation_role_id": FIX["roles"][parent_code]})
    return st, body


def mk_user(username, real_name, role_codes):
    role_ids = [FIX["roles"][c] for c in role_codes]
    st, body = req("POST", "/api/system/user", FIX["admin"],
                   {"username": username, "password": BASE_PW, "real_name": real_name,
                    "status": 1, "role_ids": role_ids})
    uid = None
    if st == 200:
        uid = (body.get("data") or {}).get("id")
    else:
        uid = psql(f"select id from sys_user where username='{username}' and deleted_at is null limit 1")
        if not uid or uid.startswith("<ERR"):
            print(f"    [user] {username} 建失败 http={st} {body}")
            return None
    FIX["users"][username] = uid
    # 可重复执行：把该用户的 P309_* 角色同步成计划里的那一组（旧库里可能残留上一版夹具的角色）
    plan_roles = {FIX["roles"][c] for c in role_codes}
    existing = psql(
        "select coalesce(string_agg(ur.role_id::text, ','), '') from sys_user_role ur "
        f"where ur.user_id='{uid}'::uuid").split(",")
    stale = [r for r in existing if r and r not in plan_roles
             and psql(f"select count(*) from sys_role where id='{r}'::uuid and role_code like 'P309%'") == "1"]
    for r in stale:
        psql(f"delete from sys_user_role where user_id='{uid}'::uuid and role_id='{r}'::uuid")
    psql(
        "INSERT INTO sys_user_role (user_id, role_id) "
        f"SELECT '{uid}'::uuid, unnest(ARRAY[{','.join(chr(39) + r + chr(39) for r in plan_roles)}]::uuid[]) "
        "ON CONFLICT DO NOTHING")
    return uid


def grant(role_code, codes):
    rid = FIX["roles"][role_code]
    joined = ",".join(f"'{c}'" for c in codes)
    psql(
        "INSERT INTO sys_role_menu (role_id, menu_id, created_at) "
        f"SELECT '{rid}'::uuid, m.id, now() FROM sys_menu m "
        f"WHERE m.auth_code IN ({joined}) AND m.deleted_at IS NULL "
        "AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm "
        f"WHERE rm.role_id='{rid}'::uuid AND rm.menu_id=m.id)"
    )
    return psql(
        "SELECT count(DISTINCT m.auth_code) FROM sys_role_menu rm "
        "JOIN sys_menu m ON m.id = rm.menu_id "
        f"WHERE rm.role_id='{rid}'::uuid AND m.auth_code IN ({joined})"
    )


def tok(username):
    if username not in FIX["tokens"]:
        FIX["tokens"][username] = login(username)
    return FIX["tokens"][username]


def mk_def(code, name, biz):
    st, body = req("POST", "/api/business/flow/def", FIX["admin"],
                   {"flow_code": code, "flow_name": name, "biz_type": biz, "is_default": True})
    if st == 200:
        data = body.get("data") or {}
        FIX["defs"][code] = (data.get("id"), data.get("version_id"))
    else:
        did = psql(f"select id from biz_flow_def where flow_code='{code}' and deleted_at is null limit 1")
        # 可重复执行：优先复用未发布的 DRAFT；没有就「另存为新草稿」
        vid = psql(f"select id from biz_flow_def_version where def_id='{did}'::uuid "
                   "and status='DRAFT' order by version_no desc limit 1")
        if not vid or vid.startswith("<ERR") or not vid.strip():
            st2, body2 = req("POST", f"/api/business/flow/def/{did}/draft", FIX["admin"],
                             {"change_note": "P3-09 演练重跑：另存新草稿"})
            vid = (body2.get("data") or {}).get("version_id") if st2 == 200 else None
        FIX["defs"][code] = (did, vid)
    return FIX["defs"][code]


def save_graph(version_id, graph):
    return req("PUT", f"/api/business/flow/version/{version_id}/graph", FIX["admin"], graph)


def publish(def_id):
    return req("POST", f"/api/business/flow/def/{def_id}/publish", FIX["admin"])


def node(key, name, ntype, **kw):
    base = {"key": key, "name": name, "type": ntype, "x": 0, "y": 0, "sort_order": 1}
    base.update(kw)
    return base


def appr(key, name, role_code, mode=None, self_approval="SKIP", empty="ERROR", **kw):
    spec = node(key, name, "APPROVAL", assignee_type="ROLE",
                assignees=[{"type": "ROLE", "ref_id": FIX["roles"][role_code], "sort_order": 0}],
                self_approval=self_approval, empty_assignee=empty,
                join_mode="AND", fork_mode="AND", reject_veto=kw.pop("reject_veto", True))
    if mode:
        spec["approval_mode"] = mode
    spec.update(kw)
    return spec


def appr_roles(key, name, role_codes, mode=None, self_approval="SKIP", empty="ERROR", **kw):
    """一个审批节点配多个角色来源（用于「同一节点既有管理员又有非管理员」的用例）。"""
    spec = node(key, name, "APPROVAL", assignee_type="ROLE",
                assignees=[{"type": "ROLE", "ref_id": FIX["roles"][c], "sort_order": i}
                           for i, c in enumerate(role_codes)],
                self_approval=self_approval, empty_assignee=empty,
                join_mode="AND", fork_mode="AND", reject_veto=kw.pop("reject_veto", True))
    if mode:
        spec["approval_mode"] = mode
    spec.update(kw)
    return spec


def edge(key, frm, to, condition=None, is_default=False, priority=0, label=None):
    return {"key": key, "from": frm, "to": to, "condition": condition,
            "is_default": is_default, "priority": priority, "label": label}


def start(def_code, username, title, form=None):
    did, _ = FIX["defs"][def_code]
    st, body = req("POST", "/api/business/flow/instance", tok(username),
                   {"def_id": did, "title": title, "form_data": form or {}})
    iid = (body.get("data") or {}).get("instance_id") if st == 200 else None
    return st, body, iid


def tasks_of(instance_id, status=None):
    """[(id, assignee_id, assignee_name, node_key, round, status, comment, assigned_by_rule,
         escalated_from_role, escalate_reason, task_kind, is_self_approval)]"""
    where = f"instance_id='{instance_id}'::uuid"
    if status:
        where += f" and status='{status}'"
    out = psql(
        "select coalesce(string_agg(concat_ws('|', id, assignee_id, coalesce(assignee_name,''), node_key, "
        "round, status, coalesce(comment,''), coalesce(assigned_by_rule,''), "
        "coalesce(escalated_from_role,''), coalesce(escalate_reason,''), task_kind, is_self_approval), "
        "'\n' order by created_at), '') from biz_flow_task where " + where)
    rows = []
    if not out or out.startswith("<ERR"):
        return rows
    for line in out.splitlines():
        parts = line.split("|")
        if len(parts) >= 12:
            rows.append({
                "id": parts[0], "assignee_id": parts[1], "assignee_name": parts[2],
                "node_key": parts[3], "round": int(parts[4]), "status": parts[5],
                "comment": parts[6], "rule": parts[7], "esc_from": parts[8],
                "esc_reason": parts[9], "kind": parts[10], "self": parts[11],
            })
    return rows


def pending_task(instance_id, username=None, node_key=None):
    rows = tasks_of(instance_id, "PENDING")
    for row in rows:
        if username and row["assignee_id"] != FIX["users"][username]:
            continue
        if node_key and row["node_key"] != node_key:
            continue
        return row
    return None


def approve(task_id, username, comment="P3-09 演练审批通过"):
    return req("POST", f"/api/business/flow/task/{task_id}/approve", tok(username),
               {"comment": comment})


def reject(task_id, username, comment="P3-09 演练驳回意见", return_to=None):
    body = {"comment": comment}
    if return_to is not None:
        body["return_to"] = return_to
    return req("POST", f"/api/business/flow/task/{task_id}/reject", tok(username), body)


def inst_status(instance_id):
    return psql(f"select status from biz_flow_instance where id='{instance_id}'::uuid")


def node_status(instance_id, key, rnd=1):
    return psql(f"select status from biz_flow_instance_node "
                f"where instance_id='{instance_id}'::uuid and node_key='{key}' and round={rnd}")


def node_count(instance_id, key):
    return psql(f"select count(*) from biz_flow_instance_node "
                f"where instance_id='{instance_id}'::uuid and node_key='{key}'")


def graph_of(def_code, nodes, edges, note):
    return {"change_note": note, "nodes": nodes, "edges": edges}


# ============================ 夹具搭建 ============================
ROLE_PLAN = {
    # code: (name, [username...], extra_perms)
    "P309_THREE": ("P3-09 三人角色（场景①）", ["u_t1", "u_t2", "u_t3"], ["instance:create"]),
    "P309_ANY3": ("P3-09 三人角色（场景⑦ 默认或签）", ["u_a1", "u_a2", "u_a3"], []),
    "P309_MAKER": ("P3-09 单人角色（=制单人，场景②）", ["u_m1"], ["instance:create"]),
    "P309_BOSS": ("P3-09 上级角色（场景② 升级目标）", ["u_b1", "u_b2"], []),
    "P309_LONE": ("P3-09 单人角色（无上级，场景③）", ["u_l1"], ["instance:create"]),
    "P309_FB": ("P3-09 兜底人（场景③）", ["u_fb"], []),
    "P309_SOLO": ("P3-09 一人公司（场景④ ALLOW）", ["u_solo"], ["instance:create"]),
    "P309_DEL": ("P3-09 审批人设委托（场景⑤）", ["u_del"], ["instance:create", "delegate"]),
    "P309_FIN": ("P3-09 财务节点（场景⑥ FORBID）", ["u_fin"], ["instance:create"]),
    "P309_SUB": ("P3-09 通用发起人（只提交不审批）", ["u_init"], ["instance:create"]),
    "P309_PA": ("P3-09 并行/条件/驳回 · 甲", ["u_pa"], []),
    "P309_PB": ("P3-09 并行/条件/驳回 · 乙（含流程管理员）", ["u_pb"], ["admin"]),
    "P309_PBN": ("P3-09 驳回链 · 乙（非管理员，用于 403 用例）", ["u_pbn"], []),
    "P309_PC": ("P3-09 并行/条件/驳回 · 丙", ["u_pc"], []),
}

PERM_BASE = ["business:flow:instance:read", "business:flow:approve"]
PERM_EXTRA = {
    "instance:create": ["business:flow:instance:create"],
    "delegate": ["business:flow:delegate:list", "business:flow:delegate:save",
                 "business:flow:delegate:delete"],
    "admin": ["business:flow:admin"],
}


def build_fixtures():
    print("\n### 0) 夹具：管理员登录 → 角色 → 用户 → 权限 → 流程定义")
    FIX["admin"] = login("admin", (ADMIN_PW, NEW_PW, BASE_PW) if ADMIN_PW else (NEW_PW, BASE_PW))
    check(bool(FIX["admin"]), "admin 令牌就绪（含首登强制改密）")
    if not FIX["admin"]:
        raise SystemExit("admin 登录失败，无法继续")

    for code, (name, _users, _extra) in ROLE_PLAN.items():
        if not mk_role(code, name):
            raise SystemExit(f"角色 {code} 建失败")
    check(len(FIX["roles"]) == len(ROLE_PLAN), f"{len(ROLE_PLAN)} 个角色就绪")
    st, body = set_escalation("P309_MAKER", "P309_BOSS")
    eq("角色② 设置上级角色（P309_MAKER → P309_BOSS）→ 200", st, 200)
    eq("上级角色回读", psql(f"select escalation_role_id from sys_role where id='{FIX['roles']['P309_MAKER']}'::uuid"),
       FIX["roles"]["P309_BOSS"])

    for code, (name, users, extra) in ROLE_PLAN.items():
        for username in users:
            if not mk_user(username, f"{code} {username}", [code]):
                raise SystemExit(f"用户 {username} 建失败")
    check(len(FIX["users"]) == sum(len(v[1]) for v in ROLE_PLAN.values()),
          f"{len(FIX['users'])} 个用户就绪")

    for code, (name, users, extra) in ROLE_PLAN.items():
        codes = PERM_BASE + [c for e in extra for c in PERM_EXTRA[e]]
        n = grant(code, codes)
        check(int(n or 0) == len(set(codes)), f"{code} 授 {len(set(codes))} 个权限码（实际 {n}）")

    # 组织里 3 人 / 1 人的事实（场景①②③ 的前提）
    sql_eq("P309_THREE 在册成员数 = 3",
           "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id "
           f"where ur.role_id='{FIX['roles']['P309_THREE']}'::uuid and u.deleted_at is null", 3)
    sql_eq("P309_MAKER 在册成员数 = 1（= 制单人）",
           "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id "
           f"where ur.role_id='{FIX['roles']['P309_MAKER']}'::uuid and u.deleted_at is null", 1)
    sql_eq("P309_BOSS 在册成员数 = 2",
           "select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id "
           f"where ur.role_id='{FIX['roles']['P309_BOSS']}'::uuid and u.deleted_at is null", 2)
    for username in FIX["users"]:
        check(bool(tok(username)), f"用户 {username} 令牌就绪")


# ============================ A. ARCH-8 八条 ============================
def build_flow(code, biz, nodes, edges, note):
    did, vid = mk_def(code, code, biz)
    if not did or not vid:
        check(False, f"流程 {code} 建定义失败")
        return None
    st, body = save_graph(vid, graph_of(did, nodes, edges, note))
    if st != 200:
        check(False, f"流程 {code} 存图 → 200", f"http={st} {body}")
        return None
    st2, body2 = publish(did)
    if st2 != 200:
        check(False, f"流程 {code} 发布 → 200", f"http={st2} {body2}")
        return None
    check(True, f"流程 {code} 建定义 → 存图 → 发布（def={did} version={vid}）")
    return did


def simple_flow(code, biz, node_spec, note):
    return build_flow(
        code, biz,
        [node("s", "开始", "START", sort_order=1), node_spec,
         node("e", "结束", "END", x=440, sort_order=3)],
        [edge("e_s", "s", node_spec["key"]), edge("e_e", node_spec["key"], "e")],
        note,
    )


def s_a1():
    with scenario("A1", "ARCH-8 ①：角色 3 人、制单人 1 人 → 剩余 2 人收到待办，按该节点 approval_mode 签"):
        did = build_flow(
            "p309_a1", "P309_A1",
            [node("s", "开始", "START", sort_order=1),
             appr("n_app", "三人角色审批", "P309_THREE", mode="ALL"),
             node("e", "结束", "END", x=440, sort_order=3)],
            [edge("e_s", "s", "n_app"), edge("e_end", "n_app", "e")],
            "P3-09 A1：ROLE 3 人 + 显式 ALL（与 V0339 冻结口径一致：候选 ≥2 → ALL）")
        if not did:
            return
        st, body, iid = start("p309_a1", "u_t1", "P3-09 A1 三人角色/制单人 1 人")
        eq("A1 发起（制单人 u_t1 在角色内）→ 200", st, 200)
        if not iid:
            check(False, "A1 没拿到实例 id", str(body))
            return
        mode = psql(f"select coalesce(approval_mode,'<NULL>') from biz_flow_instance_node "
                    f"where instance_id='{iid}'::uuid and node_key='n_app'")
        check(mode in ("ALL", "ANY"), "A1 实例节点 approval_mode 有确定值（签法口径）", mode)
        print(f"  · 本节点签法 = {mode}（ALL → 剩余 2 人都要签；ANY → 1 人即过）")
        sql_eq("A1 PENDING 待办 2 行（剩余 2 人）",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='PENDING'", 2)
        rows = tasks_of(iid, "PENDING")
        check({r["assignee_name"] for r in rows} == {"P309_THREE u_t2", "P309_THREE u_t3"},
              "A1 待办的两个人 = u_t2 / u_t3", str([r["assignee_name"] for r in rows]))
        sql_eq("A1 制单人 u_t1 落 SKIPPED 留痕",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
               f"and status='SKIPPED' and assignee_id='{FIX['users']['u_t1']}'::uuid", 1)
        contains("A1 SKIPPED 行写明 self_approval=SKIP",
                 psql(f"select comment from biz_flow_task where instance_id='{iid}'::uuid and status='SKIPPED'"),
                 "self_approval=SKIP")
        sql_eq("A1 没有走升级链（escalated_from_role 全空）",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
               "and escalated_from_role is not null", 0)
        sql_eq("A1 assigned_by_rule 全是 ROLE（普通角色展开）",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
               "and status='PENDING' and assigned_by_rule='ROLE'", 2)
        # 按 mode 决定签法
        t2 = pending_task(iid, "u_t2")
        t3 = pending_task(iid, "u_t3")
        st2, _ = approve(t2["id"], "u_t2")
        eq("A1 u_t2 审批 → 200", st2, 200)
        if mode == "ALL":
            eq("A1 ALL：1 人签完实例仍 PENDING", inst_status(iid), "PENDING")
            eq("A1 ALL：节点仍 ACTIVE", node_status(iid, "n_app"), "ACTIVE")
            st3, _ = approve(t3["id"], "u_t3")
            eq("A1 u_t3 补签 → 200", st3, 200)
        else:
            eq("A1 ANY：1 人签完实例即 APPROVED", inst_status(iid), "APPROVED")
        eq("A1 实例 APPROVED", inst_status(iid), "APPROVED")
        eq("A1 节点 approved_count = 2",
           psql(f"select approved_count from biz_flow_instance_node "
                f"where instance_id='{iid}'::uuid and node_key='n_app'"), 2)


def s_a2():
    with scenario("A2", "ARCH-8 ②：角色 1 人（=制单人）→ 自动升级上级角色 → 上级成员审批成功"):
        did = simple_flow("p309_a2", "P309_A2", appr("n_app", "单人角色审批", "P309_MAKER"),
                          "P3-09 A2：ROLE 1 人（制单人）+ 上级角色 P309_BOSS(2 人)")
        if not did:
            return
        st, body, iid = start("p309_a2", "u_m1", "P3-09 A2 升级上级角色")
        eq("A2 发起 → 200", st, 200)
        if not iid:
            check(False, "A2 没拿到实例 id", str(body))
            return
        sql_eq("A2 PENDING 待办 2 行（上级角色 2 人）",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='PENDING'", 2)
        sql_eq("A2 escalated_from_role = P309_MAKER",
               f"select distinct escalated_from_role from biz_flow_task "
               f"where instance_id='{iid}'::uuid and status='PENDING'", "P309_MAKER")
        sql_eq("A2 escalate_reason = SKIP_SELF_APPROVAL",
               f"select distinct escalate_reason from biz_flow_task "
               f"where instance_id='{iid}'::uuid and status='PENDING'", "SKIP_SELF_APPROVAL")
        sql_eq("A2 assigned_by_rule = ESCALATED",
               f"select distinct assigned_by_rule from biz_flow_task "
               f"where instance_id='{iid}'::uuid and status='PENDING'", "ESCALATED")
        sql_eq("A2 2 行待办都派给 P309_BOSS 的成员",
               f"select count(*) from biz_flow_task t join sys_user_role ur on ur.user_id=t.assignee_id "
               f"where t.instance_id='{iid}'::uuid and t.status='PENDING' "
               f"and ur.role_id='{FIX['roles']['P309_BOSS']}'::uuid", 2)
        task = pending_task(iid, "u_b1")
        check(bool(task), "A2 上级成员 u_b1 收到待办")
        if task:
            st2, _ = approve(task["id"], "u_b1")
            eq("A2 上级角色成员审批 → 200", st2, 200)
            eq("A2 实例 APPROVED", inst_status(iid), "APPROVED")
        sql_eq("A2 升级留痕行仍在（2 行带 escalated_from_role）",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
               "and escalated_from_role='P309_MAKER'", 2)
        sql_log("A2 待办留痕", f"select assignee_name||' | '||status||' | rule='||coalesce(assigned_by_rule,'-')"
                f"||' | from_role='||coalesce(escalated_from_role,'-')||' | reason='||coalesce(escalate_reason,'-')"
                f" from biz_flow_task where instance_id='{iid}'::uuid order by status")


def s_a3():
    with scenario("A3", "ARCH-8 ③：角色 1 人 + 无上级 → 节点兜底人接管（assigned_by_rule=FALLBACK）"):
        spec = appr("n_app", "单人角色审批（配兜底人）", "P309_LONE")
        spec["fallback_user_id"] = FIX["users"]["u_fb"]
        did = simple_flow("p309_a3", "P309_A3", spec, "P3-09 A3：ROLE 1 人（无上级）+ fallback_user")
        if not did:
            return
        st, body, iid = start("p309_a3", "u_l1", "P3-09 A3 兜底人接管")
        eq("A3 发起 → 200", st, 200)
        if not iid:
            check(False, "A3 没拿到实例 id", str(body))
            return
        sql_eq("A3 待办审批人 = 兜底人 u_fb",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
               f"and status='PENDING' and assignee_id='{FIX['users']['u_fb']}'::uuid", 1)
        sql_eq("A3 assigned_by_rule = FALLBACK",
               f"select distinct assigned_by_rule from biz_flow_task "
               f"where instance_id='{iid}'::uuid and status='PENDING'", "FALLBACK")
        task = pending_task(iid, "u_fb")
        if check(bool(task), "A3 兜底人收到待办"):
            st2, _ = approve(task["id"], "u_fb")
            eq("A3 兜底人审批 → 200", st2, 200)
            eq("A3 实例 APPROVED", inst_status(iid), "APPROVED")
        sql_log("A3 兜底留痕", f"select assignee_name||' | '||status||' | rule='||coalesce(assigned_by_rule,'-')"
                f" from biz_flow_task where instance_id='{iid}'::uuid order by status")


def s_a4():
    with scenario("A4", "ARCH-8 ④：一人公司 + ALLOW → 自审成功 + operation_logs + /stats/self-approval"):
        did = simple_flow("p309_a4", "P309_A4",
                          appr("n_app", "一人公司审批", "P309_SOLO", self_approval="ALLOW"),
                          "P3-09 A4：ROLE 1 人（=制单人）+ self_approval=ALLOW")
        if not did:
            return
        st, body, iid = start("p309_a4", "u_solo", "P3-09 A4 一人公司自审", {"amount": 8888})
        eq("A4 发起（唯一审批人 = 发起人）→ 200", st, 200)
        if not iid:
            check(False, "A4 没拿到实例 id", str(body))
            return
        sql_eq("A4 待办派给发起人本人（is_self_approval=t）",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
               f"and status='PENDING' and assignee_id='{FIX['users']['u_solo']}'::uuid "
               "and is_self_approval", 1)
        task = pending_task(iid, "u_solo")
        st2, body2 = approve(task["id"], "u_solo")
        eq("A4 自审 → 200", st2, 200)
        eq("A4 实例 APPROVED（自审成功）", inst_status(iid), "APPROVED")
        sql_eq("A4 operation_logs 有 FW_SELF_APPROVAL_ALLOWED 1 行",
               "select count(*) from operation_logs where operation='FLOW_SELF_APPROVAL_ALLOWED' "
               f"and target_id='{iid}'", 1)
        sql_eq("A4 operation_logs 有 FLOW_SELF_APPROVAL_VOTED 1 行",
               "select count(*) from operation_logs where operation='FLOW_SELF_APPROVAL_VOTED' "
               f"and target_id='{iid}'", 1)
        sql_log("A4 自审审计原文",
                "select operation||' | '||coalesce(username,'')||' | node='||coalesce(detail->>'node_name','')"
                "||' | amount='||coalesce(detail->>'amount','')||' | vote='||coalesce(detail->>'vote','')"
                "||' | reason='||coalesce(detail->>'reason','')"
                f" from operation_logs where target_id='{iid}' order by created_at")
        st3, body3 = req("GET", f"/api/business/flow/stats/self-approval?month={MONTH}&biz_type=P309_A4",
                         FIX["admin"])
        eq("A4 GET /stats/self-approval → 200", st3, 200)
        data = (body3.get("data") or {})
        ids = [row.get("instance_id") for row in (data.get("list") or [])]
        check(iid in ids, "A4 月度自审报表里有该单", f"list instance_ids={ids}")
        check(int(data.get("self_approval_instances") or 0) >= 1,
              "A4 报表 self_approval_instances ≥ 1", str(data.get("self_approval_instances")))
        check(int(data.get("self_approval_amount") or 0) >= 8888,
              "A4 报表金额 ≥ 8888", str(data.get("self_approval_amount")))


def s_a5():
    with scenario("A5", "ARCH-8 ⑤：审批人设委托 → 受托人收到待办（代 X 审批）→ 委托期外回到本人"):
        did = simple_flow("p309_a5", "P309_A5", appr("n_app", "委托审批", "P309_DEL"),
                          "P3-09 A5：ROLE 1 人（A 设委托给 B）")
        if not did:
            return
        now = datetime.now(timezone.utc)
        st, body = req("POST", "/api/business/flow/delegate", tok("u_del"), {
            "delegate_id": FIX["users"]["u_pa"],
            "start_at": (now - timedelta(hours=1)).isoformat(),
            "end_at": (now + timedelta(days=3)).isoformat(),
            "reason": "P3-09 出差北京",
        })
        eq("A5 建委托（u_del → u_pa，今天起 3 天）→ 200", st, 200)
        del_id = (body.get("data") or {}).get("id") if st == 200 else None
        check(bool(del_id), "A5 委托 id 就绪", str(body))
        if not del_id:
            return
        st, body, iid = start("p309_a5", "u_init", "P3-09 A5 委托期内")
        eq("A5 委托期内发起 → 200", st, 200)
        if not iid:
            return
        sql_eq("A5 委托期内：待办派给受托人 u_pa",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
               f"and status='PENDING' and assignee_id='{FIX['users']['u_pa']}'::uuid", 1)
        sql_eq("A5 委托期内：assigned_by_rule = DELEGATED",
               f"select distinct assigned_by_rule from biz_flow_task "
               f"where instance_id='{iid}'::uuid and status='PENDING'", "DELEGATED")
        comment = psql(f"select comment from biz_flow_task where instance_id='{iid}'::uuid and status='PENDING'")
        FIX["delegation_comment"] = comment
        contains("A5 待办显示「代 X 审批」", comment, "代 P309_DEL u_del 审批")
        sql_log("A5 委托期内的待办行",
                f"select assignee_name||' | '||status||' | rule='||coalesce(assigned_by_rule,'-')"
                f"||' | '||coalesce(comment,'-') from biz_flow_task where instance_id='{iid}'::uuid")
        task = pending_task(iid, "u_pa")
        if task:
            st2, _ = approve(task["id"], "u_pa", "P3-09 受托人代 A 审批通过")
            eq("A5 受托人审批 → 200", st2, 200)
            eq("A5 实例 APPROVED", inst_status(iid), "APPROVED")
            sql_eq("A5 审批留痕记在受托人名下 + DELEGATED",
                   f"select assignee_name||'|'||status||'|'||coalesce(assigned_by_rule,'-') "
                   f"from biz_flow_task where instance_id='{iid}'::uuid and status='APPROVED'",
                   "P309_PA u_pa|APPROVED|DELEGATED")
        # 把委托改到过去 → 期内结束，新单回到本人
        st, body = req("PUT", f"/api/business/flow/delegate/{del_id}", tok("u_del"), {
            "delegate_id": FIX["users"]["u_pa"],
            "start_at": (now - timedelta(days=5)).isoformat(),
            "end_at": (now - timedelta(days=1)).isoformat(),
            "reason": "P3-09 出差北京",
            "enabled": True,
        })
        eq("A5 把委托改到已过期 → 200", st, 200)
        st, body, iid2 = start("p309_a5", "u_init", "P3-09 A5 委托期外")
        eq("A5 委托期外发起 → 200", st, 200)
        if iid2:
            sql_eq("A5 委托期外：待办回到本人 u_del",
                   f"select count(*) from biz_flow_task where instance_id='{iid2}'::uuid "
                   f"and status='PENDING' and assignee_id='{FIX['users']['u_del']}'::uuid", 1)
            sql_eq("A5 委托期外：不带「代 … 审批」comment",
                   f"select count(*) from biz_flow_task where instance_id='{iid2}'::uuid "
                   "and status='PENDING' and comment like '代 %审批%'", 0)


def s_a6():
    with scenario("A6", "ARCH-8 ⑥：财务节点 FORBID → 自审被拒 422 / 65019（回归不变）"):
        did = simple_flow("p309_a6", "P309_A6",
                          appr("n_app", "财务审批（FORBID）", "P309_FIN", self_approval="FORBID"),
                          "P3-09 A6：ROLE 1 人（=制单人）+ self_approval=FORBID")
        if not did:
            return
        st, body, iid = start("p309_a6", "u_fin", "P3-09 A6 财务节点自审")
        eq("A6 自审发起 → 422", st, 422)
        eq("A6 错误码 65019", code_of(body), 65019)
        contains("A6 报文点名 self_approval=FORBID", msg_of(body), "self_approval=FORBID")
        sql_eq("A6 没留下半截实例",
               "select count(*) from biz_flow_instance where title='P3-09 A6 财务节点自审'", 0)


def s_a7():
    with scenario("A7", "ARCH-8 ⑦：角色节点默认或签 → 3 人任 1 人审批即通过，另 2 条 CANCELLED"):
        did = simple_flow("p309_a7", "P309_A7", appr("n_app", "三人角色审批（默认模式）", "P309_ANY3"),
                          "P3-09 A7：ROLE 3 人 + 不填 approval_mode（NULL = 或签）")
        if not did:
            return
        st, body, iid = start("p309_a7", "u_init", "P3-09 A7 默认或签")
        eq("A7 发起（发起人不在角色内）→ 200", st, 200)
        if not iid:
            return
        sql_eq("A7 3 条 PENDING 待办（角色 3 人）",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='PENDING'", 3)
        sql_eq("A7 节点 approval_mode 落库为 NULL（不填就不编值）",
               f"select coalesce(approval_mode,'<NULL>') from biz_flow_instance_node "
               f"where instance_id='{iid}'::uuid and node_key='n_app'", "<NULL>")
        task = pending_task(iid, "u_a1")
        st2, _ = approve(task["id"], "u_a1")
        eq("A7 u_a1 审批 → 200", st2, 200)
        eq("A7 实例 APPROVED（任 1 人即过）", inst_status(iid), "APPROVED")
        sql_eq("A7 另 2 条待办 CANCELLED",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='CANCELLED'", 2)
        sql_log("A7 三人角色待办终态",
                f"select assignee_name||' | '||status from biz_flow_task "
                f"where instance_id='{iid}'::uuid order by assignee_name")


def s_a8():
    with scenario("A8", "ARCH-8 ⑧：留痕完整（升级 / 兜底 / 委托 / 自审 SKIP 都能查到原因）"):
        rows = psql(
            "select '升级 '||coalesce(escalated_from_role,'-')||' / '||coalesce(escalate_reason,'-')"
            "||' / '||coalesce(assigned_by_rule,'-') from biz_flow_task where escalated_from_role is not null "
            "union all select '兜底 '||coalesce(assigned_by_rule,'-') from biz_flow_task where assigned_by_rule='FALLBACK' "
            "union all select '委托 '||coalesce(assigned_by_rule,'-')||' / '||coalesce(comment,'-') "
            "from biz_flow_task where assigned_by_rule='DELEGATED' "
            "union all select '自审SKIP '||coalesce(comment,'-') from biz_flow_task "
            "where status='SKIPPED' and comment like '%self_approval=SKIP%' "
            "union all select '自审ALLOW '||operation||' / '||coalesce(detail->>'reason','-') "
            "from operation_logs where operation like 'FLOW_SELF_APPROVAL_%'")
        SQL_LOG.write(f"\n[A8 四类留痕汇总]\n{rows}\n")
        print("  · 四类留痕汇总：")
        for line in rows.splitlines():
            print(f"      {line}")
        check("升级" in rows, "A8 升级留痕可查（biz_flow_task.escalated_from_role / escalate_reason）", rows)
        check("兜底" in rows, "A8 兜底留痕可查（assigned_by_rule=FALLBACK）", rows)
        contains("A8 委托留痕可查（assigned_by_rule=DELEGATED + 代 X 审批）", rows, "代 ")
        check("自审SKIP" in rows, "A8 自审 SKIP 留痕可查（SKIPPED + self_approval=SKIP）", rows)
        check("自审ALLOW" in rows, "A8 自审 ALLOW 留痕可查（operation_logs）", rows)
        contains("A8 委托待办 comment 写明「代 X 审批（委托至 …）」",
                 FIX.get("delegation_comment", ""), "委托至")


# ============================ B. ARCH-5 三项 ============================
def s_b1():
    with scenario("B1", "ARCH-5：并行网关（AND 汇聚等齐 / 只签一臂不汇聚 / OR 汇聚先到先走）"):
        # --- 图 1：PARALLEL fork(AND) → 两臂审批 → PARALLEL join(AND) → 两个 END ---
        nodes = [
            node("s", "开始", "START", sort_order=1),
            node("f", "并行分叉", "PARALLEL", x=160, sort_order=2, fork_mode="AND", join_mode="AND"),
            appr("n_a", "并行臂甲", "P309_PA", x=340, sort_order=3),
            appr("n_b", "并行臂乙", "P309_PB", x=340, y=140, sort_order=4),
            node("j", "并行汇聚", "PARALLEL", x=540, sort_order=5, fork_mode="AND", join_mode="AND"),
            node("e1", "结束甲", "END", x=720, y=-60, sort_order=6),
            node("e2", "结束乙", "END", x=720, y=60, sort_order=7),
        ]
        edges = [
            edge("e_s", "s", "f"), edge("e_fa", "f", "n_a"), edge("e_fb", "f", "n_b"),
            edge("e_aj", "n_a", "j"), edge("e_bj", "n_b", "j"),
            edge("e_j1", "j", "e1"), edge("e_j2", "j", "e2"),
        ]
        did = build_flow("p309_b1_par", "P309_B1_PAR", nodes, edges,
                         "P3-09 B1：并行网关 AND 分叉 + AND 汇聚（汇聚节点出度 2 → 两个 END）")
        if not did:
            return
        st, body, iid = start("p309_b1_par", "u_init", "P3-09 B1 并行 AND 汇聚")
        eq("B1 发起（两臂同时激活）→ 200", st, 200)
        if not iid:
            return
        sql_eq("B1 两臂待办 2 条（臂甲 1 人 + 臂乙 1 人）",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='PENDING'", 2)
        eq("B1 臂甲节点 ACTIVE", node_status(iid, "n_a"), "ACTIVE")
        eq("B1 臂乙节点 ACTIVE", node_status(iid, "n_b"), "ACTIVE")
        eq("B1 汇聚节点尚未建行（只到达 0 臂）", node_count(iid, "j"), 0)
        task_a = pending_task(iid, "u_pa", "n_a")
        st2, _ = approve(task_a["id"], "u_pa", "P3-09 臂甲签完")
        eq("B1 只签一臂（臂甲）→ 200", st2, 200)
        eq("B1 只签一臂：实例仍 PENDING", inst_status(iid), "PENDING")
        sql_eq("B1 只签一臂：汇聚节点登记到达但保持 PENDING（不建待办）",
               f"select coalesce(max(status),'<无行>') from biz_flow_instance_node "
               f"where instance_id='{iid}'::uuid and node_key='j'", "PENDING")
        sql_eq("B1 只签一臂：臂乙待办仍 PENDING",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
               "and node_key='n_b' and status='PENDING'", 1)
        task_b = pending_task(iid, "u_pb", "n_b")
        st3, _ = approve(task_b["id"], "u_pb", "P3-09 臂乙签完")
        eq("B1 两臂都签完（臂乙）→ 200", st3, 200)
        eq("B1 实例 APPROVED（AND 汇聚放行）", inst_status(iid), "APPROVED")
        eq("B1 汇聚节点 APPROVED", node_status(iid, "j"), "APPROVED")
        sql_eq("B1 两个 END 都走到",
               f"select count(*) from biz_flow_instance_node where instance_id='{iid}'::uuid "
               "and node_key in ('e1','e2') and status='APPROVED'", 2)
        sql_log("B1 AND 汇聚时间线",
                f"select node_key||' | '||node_type||' | '||status||' | arrived='||to_char(activated_at,'HH24:MI:SS')"
                f" from biz_flow_instance_node where instance_id='{iid}'::uuid order by node_key")

        # --- 图 2：PARALLEL 汇聚用 OR → 发布期必须被 R8 拒（设计口径：并行只许 AND） ---
        bad_nodes = [dict(n) for n in nodes]
        for nd in bad_nodes:
            if nd["key"] == "j":
                nd["join_mode"] = "OR"
        did2, vid2 = mk_def("p309_b1_or", "P3-09 B1 并行 OR 汇聚（应被拒）", "P309_B1_OR")
        st4, body4 = save_graph(vid2, graph_of(did2, bad_nodes, edges,
                                               "P3-09 B1：并行汇聚故意配 OR"))
        eq("B1 OR 汇聚图存草稿 → 200（保存期不拦）", st4, 200)
        st5, body5 = publish(did2)
        eq("B1 OR 汇聚发布被拒 → 400", st5, 400)
        eq("B1 OR 汇聚错误码 = 65014（R8 禁止并行 OR 汇聚）", code_of(body5), 65014)
        contains("B1 报文写明「必须用 AND 汇聚」", msg_of(body5), "必须用 AND 汇聚")

        # --- 图 3：CONDITION 分叉 + OR 汇聚 → 先到先走（只一支到达也立即放行） ---
        nodes3 = [
            node("s", "开始", "START", sort_order=1),
            node("c1", "金额判定", "CONDITION", x=160, sort_order=2, fork_mode="AND", join_mode="OR"),
            appr("n_a", "小额分支审批", "P309_PA", x=340, sort_order=3),
            appr("n_b", "默认分支审批", "P309_PB", x=340, y=140, sort_order=4),
            node("j", "OR 汇聚", "PARALLEL", x=540, sort_order=5, fork_mode="AND", join_mode="OR"),
            node("e1", "结束甲", "END", x=720, y=-60, sort_order=6),
            node("e2", "结束乙", "END", x=720, y=60, sort_order=7),
        ]
        edges3 = [
            edge("e_s", "s", "c1"),
            edge("e_low", "c1", "n_a",
                 condition={"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10000}),
            edge("e_def", "c1", "n_b", is_default=True, priority=10, label="默认分支"),
            edge("e_aj", "n_a", "j"), edge("e_bj", "n_b", "j"),
            edge("e_j1", "j", "e1"), edge("e_j2", "j", "e2"),
        ]
        did3 = build_flow("p309_b1_or_cond", "P309_B1_ORC", nodes3, edges3,
                          "P3-09 B1：CONDITION 分叉 + OR 汇聚（先到先走）")
        if not did3:
            return
        st, body, iid3 = start("p309_b1_or_cond", "u_init", "P3-09 B1 OR 汇聚先到先走", {"amount": 5000})
        eq("B1 OR 汇聚场景发起 → 200", st, 200)
        if not iid3:
            return
        eq("B1 OR 汇聚：只激活命中的一支（臂甲）", node_status(iid3, "n_a"), "ACTIVE")
        eq("B1 OR 汇聚：未命中的另一支没有节点行", node_count(iid3, "n_b"), 0)
        sql_eq("B1 OR 汇聚：未命中分支没有待办",
               f"select count(*) from biz_flow_task where instance_id='{iid3}'::uuid and node_key='n_b'", 0)
        task = pending_task(iid3, "u_pa", "n_a")
        st, _ = approve(task["id"], "u_pa", "P3-09 一支签完")
        eq("B1 OR 汇聚：一支签完 → 200", st, 200)
        eq("B1 OR 汇聚：只 1 支到达即放行（实例 APPROVED）", inst_status(iid3), "APPROVED")
        eq("B1 OR 汇聚节点 APPROVED", node_status(iid3, "j"), "APPROVED")


def s_b2():
    with scenario("B2", "ARCH-5：条件动态路由（form.amount 三档 → 三条边 / 三个节点）"):
        nodes = [
            node("s", "开始", "START", sort_order=1),
            node("c1", "金额分级①（< 1 万）", "CONDITION", x=160, sort_order=2,
                 fork_mode="AND", join_mode="OR"),
            appr("n_low", "小额审批", "P309_PA", x=340, sort_order=3),
            node("c2", "金额分级②（< 10 万）", "CONDITION", x=340, y=160, sort_order=4,
                 fork_mode="AND", join_mode="OR"),
            appr("n_mid", "中额审批", "P309_PB", x=520, sort_order=5),
            appr("n_high", "大额审批", "P309_PC", x=520, y=160, sort_order=6),
            # END 有多条入边时必须显式 join_mode=OR（种子流程同款）：
            # 引擎对「入度 ≥ 2」的节点一律走汇聚闸，AND 会等另一支永远不来的分支 → 实例卡死。
            # 该口径未在发布校验里强制（见 B2b 缺陷复现），建模侧因此显式写 OR。
            node("e", "结束", "END", x=720, sort_order=7, join_mode="OR"),
        ]
        edges = [
            edge("e_s", "s", "c1"),
            edge("e_low", "c1", "n_low",
                 condition={"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10000},
                 label="< 1 万"),
            edge("e_c2", "c1", "c2", is_default=True, priority=10, label="≥ 1 万"),
            edge("e_mid", "c2", "n_mid",
                 condition={"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 100000},
                 label="1 万 ~ 10 万"),
            edge("e_high", "c2", "n_high", is_default=True, priority=10, label="≥ 10 万"),
            edge("e_low_end", "n_low", "e"), edge("e_mid_end", "n_mid", "e"),
            edge("e_high_end", "n_high", "e"),
        ]
        did = build_flow("p309_b2", "P309_B2", nodes, edges,
                         "P3-09 B2：条件动态路由三档（<1万 / 1万~10万 / ≥10万）")
        if not did:
            return
        cases = [
            (5000, "n_low", "e_low", None, None),
            (50000, "n_mid", "e_c2", "e_mid", "c2"),
            (500000, "n_high", "e_c2", "e_high", "c2"),
        ]
        for amount, expect_node, edge1, edge2, c2_expect in cases:
            st, body, iid = start("p309_b2", "u_init", f"P3-09 B2 金额 {amount}", {"amount": amount})
            eq(f"B2 amount={amount} 发起 → 200", st, 200)
            if not iid:
                continue
            sql_eq(f"B2 amount={amount}：c1 走的边 = {edge1}",
                   f"select taken_edge_key from biz_flow_instance_node "
                   f"where instance_id='{iid}'::uuid and node_key='c1'", edge1)
            eq(f"B2 amount={amount}：命中的审批节点 = {expect_node}（ACTIVE）",
               node_status(iid, expect_node), "ACTIVE")
            if edge2:
                sql_eq(f"B2 amount={amount}：c2 走的边 = {edge2}",
                       f"select taken_edge_key from biz_flow_instance_node "
                       f"where instance_id='{iid}'::uuid and node_key='c2'", edge2)
            others = {"n_low", "n_mid", "n_high"} - {expect_node}
            sql_eq(f"B2 amount={amount}：其它两个审批节点都没被激活",
                   f"select count(*) from biz_flow_instance_node where instance_id='{iid}'::uuid "
                   f"and node_key in ({','.join(chr(39) + o + chr(39) for o in others)})", 0)
            task = pending_task(iid, {"n_low": "u_pa", "n_mid": "u_pb", "n_high": "u_pc"}[expect_node],
                                expect_node)
            if task:
                st, _ = approve(task["id"], {"n_low": "u_pa", "n_mid": "u_pb", "n_high": "u_pc"}[expect_node],
                                "P3-09 分级审批通过")
                eq(f"B2 amount={amount}：命中节点审批 → 200", st, 200)
                eq(f"B2 amount={amount}：实例 APPROVED", inst_status(iid), "APPROVED")
            sql_log(f"B2 amount={amount} 时间线",
                    f"select node_key||' | '||status||' | edge='||coalesce(taken_edge_key,'-')"
                    f" from biz_flow_instance_node where instance_id='{iid}'::uuid order by node_key")


def s_b2b():
    """缺陷复现（B2 的对照）：条件三档汇到同一个 END，END 未显式 join_mode → 默认 AND →
    实例永久 PENDING。发布期不报（R8 的文档口径是「END 不参与汇聚门控」），运行期却门控。
    与「条件动态路由」这一常见建模直接冲突：只要用户没在 END 上手动选 OR 就会卡单。"""
    with scenario("B2b", "缺陷复现：多条入边的 END 默认 join=AND → 条件分支汇入后实例永久 PENDING"):
        nodes = [
            node("s", "开始", "START", sort_order=1),
            node("c1", "金额分级①（< 1 万）", "CONDITION", x=160, sort_order=2,
                 fork_mode="AND", join_mode="OR"),
            appr("n_low", "小额审批", "P309_PA", x=340, sort_order=3),
            node("c2", "金额分级②（< 10 万）", "CONDITION", x=340, y=160, sort_order=4,
                 fork_mode="AND", join_mode="OR"),
            appr("n_mid", "中额审批", "P309_PB", x=520, sort_order=5),
            appr("n_high", "大额审批", "P309_PC", x=520, y=160, sort_order=6),
            # ★ 故意不写 join_mode（= NULL → 引擎 unwrap_or(AND)）
            node("e", "结束（未配 join_mode）", "END", x=720, sort_order=7),
        ]
        edges = [
            edge("e_s", "s", "c1"),
            edge("e_low", "c1", "n_low",
                 condition={"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10000}),
            edge("e_c2", "c1", "c2", is_default=True, priority=10),
            edge("e_mid", "c2", "n_mid",
                 condition={"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 100000}),
            edge("e_high", "c2", "n_high", is_default=True, priority=10),
            edge("e_low_end", "n_low", "e"), edge("e_mid_end", "n_mid", "e"),
            edge("e_high_end", "n_high", "e"),
        ]
        did = build_flow("p309_b2_end_and", "P309_B2_END_AND", nodes, edges,
                         "P3-09 B2b 缺陷复现：END 未配 join_mode（默认 AND）")
        if not did:
            return
        st, body, iid = start("p309_b2_end_and", "u_init", "P3-09 B2b END 默认 AND", {"amount": 5000})
        eq("B2b 发起 → 200", st, 200)
        if not iid:
            return
        task = pending_task(iid, "u_pa", "n_low")
        st, _ = approve(task["id"], "u_pa", "B2b 唯一分支签完")
        eq("B2b 唯一分支审批 → 200", st, 200)
        eq("B2b 审批节点 n_low 已 APPROVED", node_status(iid, "n_low"), "APPROVED")
        eq("B2b 【缺陷】实例仍是 PENDING（应为 APPROVED）", inst_status(iid), "PENDING")
        eq("B2b 【缺陷】END 节点停在 PENDING（等人齐，但另两支永远不来）",
           node_status(iid, "e"), "PENDING")
        sql_eq("B2b END 入度 = 3、本轮只有 1 支到达",
               f"select count(*) from biz_flow_instance_node where instance_id='{iid}'::uuid "
               "and status in ('APPROVED','SKIPPED') and node_key in ('n_low','n_mid','n_high')", 1)
        sql_log("B2b 卡死事实",
                f"select node_key||' | '||status from biz_flow_instance_node "
                f"where instance_id='{iid}'::uuid order by node_key")
        print("  ⚠ 已知缺陷：发布校验 check_join_mode 明确跳过 END（注释写「END 不做汇聚门控」），"
              "但引擎 step() 对入度 ≥2 的节点一律走汇聚闸 → 两边口径矛盾。见修复卡。")


def s_b3():
    with scenario("B3", "ARCH-5：流程版本冻结（老实例走旧图 / 新单走新图）"):
        nodes_v1 = [node("s", "开始", "START", sort_order=1),
                    appr("n_a", "审批人甲（旧版）", "P309_PA", x=220, sort_order=2),
                    node("e", "结束", "END", x=440, sort_order=3)]
        edges_v1 = [edge("e_s", "s", "n_a"), edge("e_end", "n_a", "e")]
        did = build_flow("p309_b3", "P309_B3", nodes_v1, edges_v1,
                         "P3-09 B3 v1：单节点，审批人 = P309_PA，节点名带「旧版」")
        if not did:
            return
        _, v1 = FIX["defs"]["p309_b3"]
        st, body, iid_old = start("p309_b3", "u_init", "P3-09 B3 旧版实例")
        eq("B3 发起旧版实例 → 200", st, 200)
        if not iid_old:
            return
        sql_eq("B3 老实例 node_name 来自旧快照",
               f"select node_name from biz_flow_instance_node "
               f"where instance_id='{iid_old}'::uuid and node_key='n_a'", "审批人甲（旧版）")
        sql_eq("B3 老实例待办派给旧版审批人（u_pa）",
               f"select count(*) from biz_flow_task where instance_id='{iid_old}'::uuid "
               f"and status='PENDING' and assignee_id='{FIX['users']['u_pa']}'::uuid", 1)

        # 发布新版：改审批人 + 加一个节点
        st, body = req("POST", f"/api/business/flow/def/{did}/draft", FIX["admin"],
                       {"change_note": "P3-09 B3：改审批人 + 加节点"})
        eq("B3 另存新草稿 → 200", st, 200)
        v2 = (body.get("data") or {}).get("version_id") or (body.get("data") or {}).get("id")
        check(bool(v2), "B3 新草稿 version_id 就绪", str(body))
        if not v2:
            return
        nodes_v2 = [node("s", "开始", "START", sort_order=1),
                    appr("n_a", "审批人乙（新版）", "P309_PB", x=220, sort_order=2),
                    appr("n_extra", "新增节点（新版才有）", "P309_PC", x=340, sort_order=3),
                    node("e", "结束", "END", x=520, sort_order=4)]
        edges_v2 = [edge("e_s", "s", "n_a"), edge("e_new", "n_a", "n_extra"),
                    edge("e_end", "n_extra", "e")]
        st, body = save_graph(v2, graph_of(did, nodes_v2, edges_v2, "P3-09 B3 v2"))
        eq("B3 v2 存图 → 200", st, 200)
        st, body = publish(did)
        eq("B3 v2 发布 → 200", st, 200)
        sql_eq("B3 def.current_version_id 已指向 v2",
               f"select current_version_id from biz_flow_def where id='{did}'::uuid", v2)
        eq("B3 老实例仍绑发布时的 v1（版本不漂移）",
           psql(f"select version_id from biz_flow_instance where id='{iid_old}'::uuid"), v1)
        sql_eq("B3 新版本号 = 旧版本号 + 1",
               f"select v2.version_no - v1.version_no from biz_flow_def_version v1, "
               f"biz_flow_def_version v2 where v1.id='{v1}'::uuid and v2.id='{v2}'::uuid", 1)

        # 老实例继续走旧图
        task = pending_task(iid_old, "u_pa", "n_a")
        st, _ = approve(task["id"], "u_pa", "P3-09 旧版实例审批")
        eq("B3 老实例按旧图签完 → 200", st, 200)
        eq("B3 老实例 APPROVED（旧图直接到 END）", inst_status(iid_old), "APPROVED")
        eq("B3 老实例没有新版才有的节点行", node_count(iid_old, "n_extra"), 0)
        sql_eq("B3 老实例节点名仍是旧快照",
               f"select node_name from biz_flow_instance_node "
               f"where instance_id='{iid_old}'::uuid and node_key='n_a'", "审批人甲（旧版）")

        # 新单走新图
        st, body, iid_new = start("p309_b3", "u_init", "P3-09 B3 新版实例")
        eq("B3 新发起实例 → 200", st, 200)
        if not iid_new:
            return
        eq("B3 新实例绑 v2", 
           psql(f"select version_id from biz_flow_instance where id='{iid_new}'::uuid"), v2)
        sql_eq("B3 新实例节点名 = 新版快照",
               f"select node_name from biz_flow_instance_node "
               f"where instance_id='{iid_new}'::uuid and node_key='n_a'", "审批人乙（新版）")
        sql_eq("B3 新实例待办派给新版审批人（u_pb）",
               f"select count(*) from biz_flow_task where instance_id='{iid_new}'::uuid "
               f"and status='PENDING' and assignee_id='{FIX['users']['u_pb']}'::uuid", 1)
        task = pending_task(iid_new, "u_pb", "n_a")
        st, _ = approve(task["id"], "u_pb", "P3-09 新版实例审批①")
        eq("B3 新实例第一节点签完 → 200", st, 200)
        eq("B3 新实例走到新增节点（ACTIVE）", node_status(iid_new, "n_extra"), "ACTIVE")
        task = pending_task(iid_new, "u_pc", "n_extra")
        st, _ = approve(task["id"], "u_pc", "P3-09 新版实例审批②")
        eq("B3 新实例新增节点签完 → 200", st, 200)
        eq("B3 新实例 APPROVED", inst_status(iid_new), "APPROVED")
        sql_log("B3 新旧实例对照",
                f"select instance_no||' | v'||version_no||' | '||status||' | '||title "
                f"from biz_flow_instance where def_id='{did}'::uuid order by started_at")


# ============================ C. 驳回四目标 ============================
def reject_flow():
    nodes = [
        node("s", "开始", "START", sort_order=1),
        appr("n1", "第一节点（甲）", "P309_PA", x=180, sort_order=2),
        appr_roles("n2", "第二节点（乙：管理员 + 非管理员）", ["P309_PB", "P309_PBN"],
                   x=340, sort_order=3),
        appr("n3", "第三节点（丙）", "P309_PC", x=500, sort_order=4),
        node("e", "结束", "END", x=660, sort_order=5),
    ]
    edges = [edge("e_s", "s", "n1"), edge("e_12", "n1", "n2"),
             edge("e_23", "n2", "n3"), edge("e_3e", "n3", "e")]
    return build_flow("p309_c", "P309_C", nodes, edges,
                      "P3-09 C：三节点链（供驳回四目标使用）")


def advance_to_n2(code="p309_c"):
    """发起并让 n1 通过，停在 n2。返回 instance_id"""
    st, body, iid = start(code, "u_init", f"P3-09 C {code} {datetime.now().strftime('%H%M%S%f')}")
    if st != 200 or not iid:
        check(False, "C 发起失败", f"http={st} {body}")
        return None
    task = pending_task(iid, "u_pa", "n1")
    if not task:
        check(False, "C 第一节点没有待办", "")
        return None
    st, _ = approve(task["id"], "u_pa", "P3-09 第一节点通过")
    if st != 200:
        check(False, "C 第一节点审批失败", str(st))
        return None
    return iid


def s_c1():
    with scenario("C1", "驳回目标 PREV：退上一节点 → round 递增 + 上一节点重新派待办"):
        iid = advance_to_n2()
        if not iid:
            return
        eq("C1 停在第二节点（ACTIVE）", node_status(iid, "n2"), "ACTIVE")
        task = pending_task(iid, "u_pb", "n2")
        check(bool(task), "C1 第二节点待办就绪（u_pb）")
        st, body = reject(task["id"], "u_pb", "P3-09 退回上一节点重来", "PREV")
        eq("C1 驳回（return_to=PREV）→ 200", st, 200)
        eq("C1 实例仍 PENDING", inst_status(iid), "PENDING")
        eq("C1 第二节点本轮 REJECTED", node_status(iid, "n2", 1), "REJECTED")
        sql_eq("C1 第一节点 round 递增到 2", 
               f"select max(round) from biz_flow_instance_node "
               f"where instance_id='{iid}'::uuid and node_key='n1'", 2)
        eq("C1 第一节点第 2 轮 ACTIVE", node_status(iid, "n1", 2), "ACTIVE")
        sql_eq("C1 第一节点第 2 轮重新派待办给 u_pa",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and node_key='n1' "
               f"and round=2 and status='PENDING' and assignee_id='{FIX['users']['u_pa']}'::uuid", 1)
        sql_eq("C1 instance.round_total = 2",
               f"select round_total from biz_flow_instance where id='{iid}'::uuid", 2)
        sql_log("C1 节点轮次", f"select node_key||' | round='||round||' | '||status"
                f" from biz_flow_instance_node where instance_id='{iid}'::uuid order by node_key, round")


def s_c2():
    with scenario("C2", "驳回目标 INITIATOR：退发起人 → RESUBMIT 待办生成"):
        iid = advance_to_n2()
        if not iid:
            return
        task = pending_task(iid, "u_pb", "n2")
        st, body = reject(task["id"], "u_pb", "P3-09 退回发起人修改", "INITIATOR")
        eq("C2 驳回（return_to=INITIATOR）→ 200", st, 200)
        eq("C2 实例仍 PENDING（等重新提交）", inst_status(iid), "PENDING")
        sql_eq("C2 START 节点 round 递增到 2",
               f"select max(round) from biz_flow_instance_node "
               f"where instance_id='{iid}'::uuid and node_key='s'", 2)
        sql_eq("C2 生成 RESUBMIT 待办（发起人）",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
               f"and task_kind='RESUBMIT' and status='PENDING' "
               f"and assignee_id='{FIX['users']['u_init']}'::uuid", 1)
        contains("C2 RESUBMIT 待办 comment 写明退回原因",
                 psql(f"select comment from biz_flow_task where instance_id='{iid}'::uuid "
                      "and task_kind='RESUBMIT'"), "被驳回，请修改后重新提交")
        # 待办在发起人的 todo 列表里可见
        st, body = req("GET", "/api/business/flow/task/todo", tok("u_init"))
        eq("C2 发起人 GET /task/todo → 200", st, 200)
        rows = ((body.get("data") or {}).get("list")) or []
        check(any(r.get("instance_id") == iid and r.get("task_kind") == "RESUBMIT" for r in rows),
              "C2 发起人待办列表里能看到这条 RESUBMIT", f"n={len(rows)}")


def s_c3():
    with scenario("C3", "驳回目标 NONE(FINAL)：驳回并终止 → 实例 REJECTED"):
        iid = advance_to_n2()
        if not iid:
            return
        task = pending_task(iid, "u_pb", "n2")
        st, body = reject(task["id"], "u_pb", "P3-09 驳回并终止", "NONE")
        eq("C3 驳回（return_to=NONE，管理员）→ 200", st, 200)
        eq("C3 实例 REJECTED（终止）", inst_status(iid), "REJECTED")
        eq("C3 第二节点 REJECTED", node_status(iid, "n2"), "REJECTED")
        sql_eq("C3 没有开新轮次（round 仍只有 1）",
               f"select max(round) from biz_flow_instance_node where instance_id='{iid}'::uuid", 1)
        eq("C3 第三节点没被激活", node_count(iid, "n3"), 0)


def s_c4():
    with scenario("C4", "驳回目标 <node_key>：非 admin 403 / admin 退回指定节点"):
        iid = advance_to_n2()
        if not iid:
            return
        # 非 admin（u_pbn，同节点另一条待办）传 node_key → 403 / 65004
        task_np = pending_task(iid, "u_pbn", "n2")
        check(bool(task_np), "C4 非管理员 u_pbn 拿到第二节点待办")
        st, body = reject(task_np["id"], "u_pbn", "P3-09 非管理员退回指定节点", "n1")
        eq("C4 非 admin 传 node_key → 403", st, 403)
        eq("C4 非 admin 错误码 65004", code_of(body), 65004)
        sql_eq("C4 403 之后没开新轮次（无副作用）",
               f"select max(round) from biz_flow_instance_node where instance_id='{iid}'::uuid", 1)
        eq("C4 403 之后第一节点仍是第 1 轮 APPROVED", node_status(iid, "n1", 1), "APPROVED")
        # admin（u_pb，业务权限码 business:flow:admin）退回指定节点 n1 → 200
        task = pending_task(iid, "u_pb", "n2")
        st, body = reject(task["id"], "u_pb", "P3-09 管理员退回第一节点", "n1")
        eq("C4 admin 传 node_key=n1 → 200", st, 200)
        eq("C4 实例仍 PENDING", inst_status(iid), "PENDING")
        eq("C4 第一节点 round 递增到 2", node_status(iid, "n1", 2), "ACTIVE")
        sql_eq("C4 第一节点第 2 轮重新派待办",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and node_key='n1' "
               f"and round=2 and status='PENDING'", 1)
        # 非法目标节点 → 400 / 65025（另起一单，管理员发起驳回）
        iid2 = advance_to_n2()
        if iid2:
            task2 = pending_task(iid2, "u_pb", "n2")
            st, body = reject(task2["id"], "u_pb", "P3-09 退回不存在的节点", "n_not_exist")
            eq("C4 退回不存在节点 → 400", st, 400)
            eq("C4 错误码 65025", code_of(body), 65025)
            eq("C4 非法目标不改状态（实例仍 PENDING、无新轮次）",
               inst_status(iid2), "PENDING")


# ============================ main ============================
def main():
    print(f"P3-09 工作流全链路回归演练 · BASE={BASE} DB={DB}")
    print(f"SQL 断言明细 → {SQL_LOG_PATH}")
    health = BASE + "/health"
    try:
        with urllib.request.urlopen(health, timeout=10) as resp:
            st, body = resp.status, {}
    except Exception as exc:  # noqa: BLE001
        st, body = 0, {"error": str(exc)}
    if st != 200:
        print(f"实例 /health 不通（{st} {body}），先跑 scripts/00_run.sh")
        return 2
    build_fixtures()

    all_scenarios = [
        ("A1", s_a1), ("A2", s_a2), ("A3", s_a3), ("A4", s_a4),
        ("A5", s_a5), ("A6", s_a6), ("A7", s_a7), ("A8", s_a8),
        ("B1", s_b1), ("B2", s_b2), ("B2b", s_b2b), ("B3", s_b3),
        ("C1", s_c1), ("C2", s_c2), ("C3", s_c3), ("C4", s_c4),
    ]
    if STAGE:
        # 大小写不敏感（场景 id 形如 B2b / A1）
        wanted = {s.strip().upper() for s in STAGE}
        all_scenarios = [(sid, fn) for sid, fn in all_scenarios if sid.upper() in wanted]

    # B/C 组共用的驳回链流程只建一次
    if any(sid in {"C1", "C2", "C3", "C4"} for sid, _ in all_scenarios):
        reject_flow()
    for sid, fn in all_scenarios:
        fn()

    print(f"\n{'=' * 78}\n=== 汇总（每个场景的 PASS/FAIL）\n{'=' * 78}")
    print(f"{'场景':<6}{'PASS':>6}{'FAIL':>6}  说明")
    for sid, title, p, f in SCEN:
        print(f"{sid:<6}{p:>6}{f:>6}  {title}")
    print(f"\n总计：PASS={PASS} FAIL={FAIL}")
    if FAILS:
        print("\n失败项：")
        for item in FAILS:
            print(f"  - {item}")
    SQL_LOG.write(f"\n===== 总计 PASS={PASS} FAIL={FAIL} =====\n")
    SQL_LOG.close()
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
