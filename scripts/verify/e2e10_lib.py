#!/usr/bin/env python3
"""E2E-10 流程引擎域演练 · 公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）

设计口径（与 e2e09 一致）：
- 全部走真实 HTTP（urllib，无第三方依赖）+ psql 直查；
- 前缀 E2E10_ / e2e10_ / "E2E10 " 标识本卡数据，收尾按外键顺序自清；
- 断言逐条落 /tmp/e2e10/asserts.log；状态（角色/用户/流程/实例 id）落 state.json，
  供 e2e10_a.py / e2e10_b.py 两段脚本接力。
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

WORK = os.environ.get("E2E10_WORK", "/tmp/e2e10")
RAW = os.path.join(WORK, "raw")
LOG = os.path.join(WORK, "asserts.log")
STATE = os.path.join(WORK, "state.json")
BASE = os.environ.get("E2E10_BASE", "http://127.0.0.1:8033")
ENV_PATH = os.environ.get("E2E10_ENV", "/etc/cloudpivot/cloudpivot-dev.env")

os.makedirs(RAW, exist_ok=True)
os.makedirs(WORK, exist_ok=True)


def load_env(path=ENV_PATH):
    env = {}
    with open(path, "r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, val = line.split("=", 1)
            env[key.strip()] = val.strip().strip('"').strip("'")
    return env


ENV = load_env()
ADMIN_PW = ENV.get("NIK_ADMIN_PASSWORD", "")
BASE_PW = "E2e10@Passw0rd"

PASS = 0
FAIL = 0
CUR = "?"
STATS = {}


def _log(line):
    with open(LOG, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")


def scenario(sid, title):
    global CUR
    CUR = sid
    print(f"\n### {sid} · {title}")
    _log(f"\n## {sid} {title}")


def check(cond, label, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        STATS[CUR] = STATS.get(CUR, [0, 0])
        STATS[CUR][0] += 1
        print(f"  ✅ {label}" + (f"｜{detail}" if detail else ""))
        _log(f"PASS\t{CUR}\t{label}" + (f"\t{detail}" if detail else ""))
    else:
        FAIL += 1
        STATS[CUR] = STATS.get(CUR, [0, 0])
        STATS[CUR][1] += 1
        print(f"  ❌ {label}" + (f"｜{detail}" if detail else ""))
        _log(f"FAIL\t{CUR}\t{label}" + (f"\t{detail}" if detail else ""))
    return cond


def eq(label, actual, expect):
    return check(str(actual) == str(expect), label, f"实际={actual} 期望={expect}")


def ne(label, actual, unexpected):
    return check(str(actual) != str(unexpected), label, f"实际={actual} 不应={unexpected}")


def contains(label, text, needle):
    text = "" if text is None else str(text)
    return check(needle in text, label, f"含「{needle}」" if needle in text else f"未含「{needle}」实际={text[:200]}")


def not_contains(label, text, needle):
    text = "" if text is None else str(text)
    return check(needle not in text, label, f"不含「{needle}」" if needle not in text else f"意外含「{needle}」实际={text[:200]}")


# ---------- HTTP ----------
def _save_raw(name, text):
    if not name:
        return
    safe = "".join(ch if ch.isalnum() or ch in "-_." else "_" for ch in str(name))[:80]
    path = os.path.join(RAW, f"{safe}.json")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text if isinstance(text, str) else json.dumps(text, ensure_ascii=False, indent=2))


def req(method, path, token=None, body=None, timeout=60, name=None):
    url = BASE + path if path.startswith("/") else BASE + "/" + path
    data = None if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(url, data=data, method=method)
    if token:
        request.add_header("Authorization", "Bearer " + token)
    if data is not None:
        request.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(request, timeout=timeout) as resp:
            raw = resp.read().decode("utf-8", "replace")
            status = resp.status
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode("utf-8", "replace")
        status = exc.code
    except Exception as exc:  # noqa: BLE001
        raw = json.dumps({"transport_error": str(exc)})
        status = 0
    try:
        parsed = json.loads(raw)
    except Exception:  # noqa: BLE001
        parsed = {"_raw": raw[:2000]}
    _save_raw(name, raw)
    return status, parsed


def code_of(body):
    if not isinstance(body, dict):
        return None
    return body.get("code")


def msg_of(body):
    if not isinstance(body, dict):
        return ""
    return str(body.get("msg") or body.get("message") or body.get("_raw") or "")


def data_of(body):
    if not isinstance(body, dict):
        return {}
    return body.get("data") or {}


# ---------- DB ----------
def psql(sql, timeout=40):
    env = dict(os.environ)
    env["PGPASSWORD"] = ENV.get("NIK_DATABASE_PASSWORD", "")
    proc = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", ENV.get("NIK_DATABASE_USER", "cloudpivot"),
         "-d", ENV.get("NIK_DATABASE_NAME", "cloudpivot_dev"),
         "-At", "-F", "|", "-P", "pager=off", "-c", sql],
        capture_output=True, text=True, env=env, timeout=timeout,
    )
    if proc.returncode != 0:
        lines = [x for x in proc.stderr.strip().splitlines() if x.strip()]
        keep = " / ".join(lines[:2])[:260]
        return "<ERR " + keep + ">"
    return proc.stdout.strip()


# ---------- 状态接力 ----------
def load_state():
    if os.path.exists(STATE):
        try:
            with open(STATE, "r", encoding="utf-8") as fh:
                return json.load(fh)
        except Exception:  # noqa: BLE001
            return {}
    return {}


def save_state(state):
    tmp = STATE + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(state, fh, ensure_ascii=False, indent=1)
    os.replace(tmp, STATE)


# ---------- 登录 / 夹具 ----------
def login(username, password=None, new_password=BASE_PW, name=None):
    password = password or BASE_PW
    st, body = req("POST", "/api/system/auth/login", None,
                   {"username": username, "password": password}, name=name)
    if st != 200:
        return ""
    token = data_of(body).get("access_token") or ""
    if data_of(body).get("must_change_password") and token:
        st2, _ = req("PUT", "/api/system/auth/password", token,
                     {"old_password": password, "new_password": new_password}, name=(name or username) + "_chpwd")
        if st2 != 200:
            return ""
        st3, body3 = req("POST", "/api/system/auth/login", None,
                         {"username": username, "password": new_password}, name=(name or username) + "_relogin")
        if st3 != 200:
            return ""
        token = data_of(body3).get("access_token") or ""
    return token


def mk_role(admin, code, name, sort=980, escalation=""):
    st, body = req("POST", "/api/system/role", admin,
                   {"role_code": code, "role_name": name, "status": 1, "sort_order": sort,
                    "escalation_role_id": escalation},
                   name="mkrole_" + code)
    rid = data_of(body).get("id")
    if not rid:
        rid = psql(f"select id from sys_role where role_code='{code}' and deleted_at is null limit 1")
        if rid.startswith("<ERR"):
            rid = None
    return rid, st, body


def set_escalation(admin, rid, code, name, parent_id, sort=980):
    st, body = req("PUT", f"/api/system/role/{rid}", admin,
                   {"role_code": code, "role_name": name, "status": 1, "sort_order": sort,
                    "escalation_role_id": parent_id},
                   name="esc_" + code)
    return st, body


def mk_user(admin, username, real_name, role_ids):
    st, body = req("POST", "/api/system/user", admin,
                   {"username": username, "password": BASE_PW, "real_name": real_name,
                    "status": 1, "role_ids": role_ids}, name="mkuser_" + username)
    uid = data_of(body).get("id")
    if not uid:
        uid = psql(f"select id from sys_user where username='{username}' and deleted_at is null limit 1")
        if uid.startswith("<ERR"):
            uid = None
    if uid:
        # 可重复执行：把该用户的 E2E10_* 角色同步成计划里的那一组
        plan = "'" + "','".join(str(r) for r in role_ids) + "'"
        psql("delete from sys_user_role where user_id='%s'::uuid and role_id in "
             "(select id from sys_role where role_code like 'E2E10%%')" % uid)
        psql("insert into sys_user_role (user_id, role_id) select '%s'::uuid, unnest(array[%s]::uuid[]) "
             "on conflict do nothing" % (uid, plan))
    return uid, st, body


def grant(admin, rid, codes):
    """按 auth_code 从菜单树反查 menu_id，走真实「角色授权」接口。"""
    st, body = req("GET", "/api/system/menu/tree", admin, name="menu_tree")
    flat = []

    def walk(items):
        for item in items or []:
            flat.append(item)
            walk(item.get("children"))

    walk(data_of(body) if isinstance(data_of(body), list) else
         (data_of(body).get("tree") or data_of(body).get("list") or []))
    ids = []
    for item in flat:
        if item.get("auth_code") in codes:
            ids.append(item.get("id"))
    if not ids:
        return 0, body
    st2, body2 = req("PUT", f"/api/system/role/{rid}/menus", admin, {"menu_ids": ids}, name=f"grant_{str(rid)[:8]}")
    return len(ids), body2


# ---------- 流程图 ----------
def node(key, name, ntype, **kw):
    base = {"key": key, "name": name, "type": ntype, "x": 0, "y": 0, "sort_order": 1}
    base.update(kw)
    return base


def appr(key, name, role_id, mode=None, self_approval="SKIP", empty="ERROR", **kw):
    spec = node(key, name, "APPROVAL", assignee_type="ROLE",
                assignees=[{"type": "ROLE", "ref_id": role_id, "sort_order": 0}],
                self_approval=self_approval, empty_assignee=empty,
                join_mode="AND", fork_mode="AND", reject_veto=kw.pop("reject_veto", True))
    if mode:
        spec["approval_mode"] = mode
    spec.update(kw)
    return spec


def edge(key, frm, to, condition=None, is_default=False, priority=0, label=None):
    return {"key": key, "from": frm, "to": to, "condition": condition,
            "is_default": is_default, "priority": priority, "label": label}


def graph_of(nodes, edges, note):
    return {"change_note": note, "nodes": nodes, "edges": edges}


def mk_def(admin, code, name, biz):
    st, body = req("POST", "/api/business/flow/def", admin,
                   {"flow_code": code, "flow_name": name, "biz_type": biz, "is_default": True},
                   name="mkdef_" + code)
    did = data_of(body).get("id")
    vid = data_of(body).get("version_id")
    if not did:
        did = psql(f"select id from biz_flow_def where flow_code='{code}' and deleted_at is null limit 1")
        vid = psql(f"select id from biz_flow_def_version where def_id='{did}'::uuid and version_status='DRAFT' "
                   "order by version_no desc limit 1") if did and not did.startswith("<ERR") else ""
        if did and not did.startswith("<ERR") and (not vid or vid.startswith("<ERR") or not vid.strip()):
            st2, body2 = req("POST", f"/api/business/flow/def/{did}/draft", admin,
                             {"change_note": "E2E10 重跑：另存新草稿"}, name="mkdef_draft_" + code)
            vid = data_of(body2).get("version_id") if st2 == 200 else ""
    return did, vid, st, body


def save_graph(admin, vid, graph):
    return req("PUT", f"/api/business/flow/version/{vid}/graph", admin, graph, name="save_graph_" + str(vid)[:8])


def publish(admin, did):
    return req("POST", f"/api/business/flow/def/{did}/publish", admin, None, name="publish_" + str(did)[:8])


def build_flow(admin, code, biz, nodes, edges, note):
    did, vid, st, body = mk_def(admin, code, code, biz)
    if not did or not vid:
        check(False, f"流程 {code} 建定义", f"http={st} {msg_of(body)}")
        return None, None
    st, body = save_graph(admin, vid, graph_of(nodes, edges, note))
    if st != 200:
        check(False, f"流程 {code} 存图 → 200", f"http={st} code={code_of(body)} {msg_of(body)}")
        return None, None
    st, body = publish(admin, did)
    if st != 200:
        check(False, f"流程 {code} 发布 → 200", f"http={st} code={code_of(body)} {msg_of(body)}")
        return None, None
    check(True, f"流程 {code} 建定义 → 存图 → 发布（def={did} v={vid}）")
    return did, vid


def start(token, def_id, title, form=None):
    st, body = req("POST", "/api/business/flow/instance", token,
                   {"def_id": def_id, "title": title, "form_data": form or {}},
                   name="start_" + title.replace(" ", "_")[:40])
    return st, body, data_of(body).get("instance_id")


def approve(token, task_id, comment="E2E10 演练审批通过"):
    return req("POST", f"/api/business/flow/task/{task_id}/approve", token, {"comment": comment},
               name="approve_" + str(task_id)[:8])


def reject(token, task_id, comment="E2E10 演练驳回", return_to=None):
    body = {"comment": comment}
    if return_to is not None:
        body["return_to"] = return_to
    return req("POST", f"/api/business/flow/task/{task_id}/reject", token, body,
               name="reject_" + str(task_id)[:8])


def tasks_of(instance_id, status=None):
    where = f"instance_id='{instance_id}'::uuid"
    if status:
        where += f" and status='{status}'"
    out = psql("select coalesce(string_agg(concat_ws('~', id, assignee_id, coalesce(assignee_name,''), node_key, "
               "round, status, coalesce(comment,''), coalesce(assigned_by_rule,''), "
               "coalesce(escalated_from_role,''), coalesce(escalate_reason,''), task_kind, is_self_approval), "
               "'\n' order by created_at), '') from biz_flow_task where " + where)
    rows = []
    if not out or out.startswith("<ERR"):
        return rows
    for line in out.splitlines():
        parts = line.split("~")
        if len(parts) >= 12:
            rows.append({"id": parts[0], "assignee_id": parts[1], "assignee_name": parts[2],
                         "node_key": parts[3], "round": parts[4], "status": parts[5],
                         "comment": parts[6], "rule": parts[7], "esc_from": parts[8],
                         "esc_reason": parts[9], "kind": parts[10], "self": parts[11]})
    return rows


def pending_task(instance_id, user_id=None, node_key=None):
    for row in tasks_of(instance_id, "PENDING"):
        if user_id and row["assignee_id"] != user_id:
            continue
        if node_key and row["node_key"] != node_key:
            continue
        return row
    return None


def inst_status(instance_id):
    return psql(f"select status from biz_flow_instance where id='{instance_id}'::uuid")


def node_status(instance_id, key, rnd=1):
    return psql(f"select status from biz_flow_instance_node where instance_id='{instance_id}'::uuid "
                f"and node_key='{key}' and round={rnd}")


def node_count(instance_id, key):
    return psql(f"select count(*) from biz_flow_instance_node where instance_id='{instance_id}'::uuid "
                f"and node_key='{key}'")


def sql_eq(label, sql, expect):
    return eq(label, psql(sql), expect)


def summary():
    print("\n================ 汇总 ================")
    for sid in sorted(STATS):
        p, f = STATS[sid]
        print(f"  {sid}: PASS={p} FAIL={f}")
    print(f"  TOTAL: PASS={PASS} FAIL={FAIL}")
    _log(f"\nSUMMARY\tPASS={PASS}\tFAIL={FAIL}")
    return FAIL
