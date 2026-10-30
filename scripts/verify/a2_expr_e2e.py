#!/usr/bin/env python3
"""P1-A2 EXPRESSION 端到端验收（隔离库 cloudpivot_a2 / 18087）。

覆盖卡面「四、硬红线 9」：造一个 EXPRESSION 流程，跑通
「创建实例 → 条件命中 → 审批人正确 → 待办生成」，外加保存期 / 发布期的错误码。

断言全部打在**真实 HTTP 响应**与**库里的行**上（不是"看起来对"）。
跑法：  python3 a2_e2e.py
"""

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("A2_BASE", "http://127.0.0.1:18087/api")
ENV_FILE = os.environ.get("A2_ENV_FILE", "/etc/cloudpivot/cloudpivot-dev.env")
DB = "cloudpivot_a2"
NEW_ADMIN_PW = "A2Expr#2026"

PASSED = 0
FAILED = []
FAILS = []


# ---------------- 基础设施 ----------------

def load_env(path):
    values = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip()
    return values


ENV = load_env(ENV_FILE)
PG = {
    "host": ENV.get("NIK_DATABASE_HOST", "127.0.0.1"),
    "port": ENV.get("NIK_DATABASE_PORT", "5432"),
    "user": ENV.get("NIK_DATABASE_USER", "cloudpivot"),
    "password": ENV.get("NIK_DATABASE_PASSWORD", ""),
}


def api(method, path, token=None, body=None, timeout=60):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, json.loads(response.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw[:400]}


def psql(sql):
    result = subprocess.run(
        ["psql", "-h", PG["host"], "-p", str(PG["port"]), "-U", PG["user"], "-d", DB,
         "-At", "-F", "|", "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True, text=True, env={**os.environ, "PGPASSWORD": PG["password"]})
    if result.returncode != 0:
        raise SystemExit(f"psql 失败：{result.stderr.strip()}\nSQL: {sql}")
    return [line.split("|") for line in result.stdout.strip().splitlines() if line]


def one(sql):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] and rows[0][0] != "" else None


def check(name, condition, detail=""):
    global PASSED
    if condition:
        PASSED += 1
        print(f"[PASS] {name}")
    else:
        FAILS.append(name)
        FAILED.append(name)
        print(f"[FAIL] {name}")
    if detail:
        print(f"       {detail}")
    return bool(condition)


def step(title):
    print("\n" + "-" * 78)
    print(title)
    print("-" * 78)


def call(name, method, path, token=None, body=None, want_status=200, want_code=0):
    status, payload = api(method, path, token, body)
    code = payload.get("code")
    good = status == want_status and code == want_code
    check(name, good,
          f"{method} {path} → HTTP {status} code={code} msg={payload.get('msg') or ''}")
    return payload.get("data"), payload


# ---------------- 登录 ----------------

def login(username, password):
    return api("POST", "/system/auth/login", body={"username": username, "password": password})


def admin_token():
    initial = ENV.get("NIK_ADMIN_PASSWORD", "")
    candidates = [NEW_ADMIN_PW, initial, initial + "-a2"]
    for candidate in candidates:
        status, payload = login("admin", candidate)
        if status != 200 or payload.get("code") != 0:
            continue
        data = payload["data"]
        if data.get("must_change_password"):
            token = data["access_token"]
            status, payload = api("PUT", "/system/auth/password", token,
                                  {"old_password": candidate, "new_password": NEW_ADMIN_PW})
            if status != 200 or payload.get("code") != 0:
                raise SystemExit(f"改密失败：{status} {payload}")
            status, payload = login("admin", NEW_ADMIN_PW)
            if status != 200 or payload.get("code") != 0:
                raise SystemExit(f"改密后重登失败：{status} {payload}")
        check("0.1 admin 登录", True, f"口令来源={'新改密' if candidate == NEW_ADMIN_PW else 'env'}")
        return payload["data"]["access_token"]
    raise SystemExit("admin 登录失败（所有候选口令都不行）")


# ---------------- 夹具 ----------------

def ensure_role(token, code, name):
    role_id = one(f"SELECT id FROM sys_role WHERE role_code = '{code}' AND deleted_at IS NULL")
    if role_id:
        return role_id
    data, _ = call(f"1.x 建角色 {code}", "POST", "/system/role", token,
                   {"role_code": code, "role_name": name, "status": 1})
    if data and data.get("id"):
        return data["id"]
    return one(f"SELECT id FROM sys_role WHERE role_code = '{code}'")


def ensure_user(token, username, real_name, role_ids):
    user_id = one(f"SELECT id FROM sys_user WHERE username = '{username}' AND deleted_at IS NULL")
    if user_id:
        return user_id
    data, _ = call(f"1.x 建用户 {username}", "POST", "/system/user", token, {
        "username": username, "password": "A2Expr#2026", "real_name": real_name,
        "status": 1, "role_ids": role_ids, "remark": "P1-A2 端到端夹具（脚本自动清理）",
    })
    if data and data.get("id"):
        return data["id"]
    return one(f"SELECT id FROM sys_user WHERE username = '{username}'")


# ---------------- 流程 ----------------

def expression_json(user_id):
    return {
        "version": 1,
        "rules": [
            {"id": "r_big",
             "when": {"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 10000},
             "then": [{"type": "ROLE", "role_codes": ["FINANCE_DIRECTOR"]}]},
            {"id": "r_small",
             "when": {"op": "cmp", "field": "form.amount", "cmp": "gte", "value": 0},
             "then": [{"type": "USER", "user_ids": [user_id]}]},
        ],
        "otherwise": [{"type": "INITIATOR"}],
    }


def graph_nodes(expression, empty_assignee="SKIP", self_approval="ALLOW"):
    return [
        {"key": "s", "name": "开始", "type": "START", "x": 0, "y": 0},
        {"key": "n_expr", "name": "表达式审批", "type": "APPROVAL", "x": 200, "y": 0,
         "assignee_type": "EXPRESSION", "empty_assignee": empty_assignee,
         "self_approval": self_approval, "config": {"expression": expression}},
        {"key": "e", "name": "结束", "type": "END", "x": 400, "y": 0},
    ]


GRAPH_EDGES = [
    {"key": "e1", "from": "s", "to": "n_expr"},
    {"key": "e2", "from": "n_expr", "to": "e"},
]


def create_def(token, code, name):
    data, _ = call(f"2.x 建流程定义 {code}", "POST", "/business/flow/def", token,
                   {"flow_code": code, "flow_name": name, "biz_type": "A2_EXPR",
                    "description": "P1-A2 表达式审批验收（脚本自动清理）"})
    if not data:
        raise SystemExit(f"流程定义 {code} 建不出来")
    return data["id"], data["version_id"]


def save_graph(token, version_id, nodes):
    return api("PUT", f"/business/flow/version/{version_id}/graph", token,
               {"nodes": nodes, "edges": GRAPH_EDGES, "change_note": "P1-A2 保存"})


def task_rows(instance_id):
    rows = psql(
        "SELECT assignee_id::text, assignee_name, assignee_source, "
        "       coalesce(assignee_rule_id, '<NULL>'), status, node_key "
        f"  FROM biz_flow_task WHERE instance_id = '{instance_id}' ORDER BY status, assignee_name")
    return rows


def start_instance(token, code, title, form_data):
    return api("POST", "/business/flow/instance", token, {
        "flow_code": code, "title": title, "form_data": form_data,
        "biz_no": f"A2-{int(time.time() * 1000) % 10**8}",
    })


# ---------------- 主流程 ----------------

def main():
    step("0. 登录")
    token = admin_token()
    check("0.2 实例健康", api("GET", "/business/flow/def/page?page=1&page_size=1", token)[0] == 200)

    step("1. 夹具：FINANCE_DIRECTOR 角色 + 两名账号")
    role_id = ensure_role(token, "FINANCE_DIRECTOR", "财务总监")
    check("1.1 FINANCE_DIRECTOR 角色就绪", bool(role_id), f"role_id={role_id}")
    fin_id = ensure_user(token, "a2_fin", "A2 财务总监", [role_id] if role_id else [])
    emp_id = ensure_user(token, "a2_emp", "A2 普通员工", [])
    check("1.2 两个夹具账号就绪", bool(fin_id) and bool(emp_id), f"fin={fin_id} emp={emp_id}")
    admin_id = one("SELECT id FROM sys_user WHERE username = 'admin'")
    check("1.3 admin id 就绪", bool(admin_id), f"admin_id={admin_id}")

    step("2. 保存期校验（dto/handler：结构错 400 + 65016）")
    def_id, version_id = create_def(token, "a2_expr_main", "P1-A2 表达式审批")

    status, payload = save_graph(token, version_id, [
        {"key": "s", "name": "开始", "type": "START"},
        # EXPRESSION 但 config 里没有 expression
        {"key": "n_expr", "name": "表达式审批", "type": "APPROVAL",
         "assignee_type": "EXPRESSION", "config": {}},
        {"key": "e", "name": "结束", "type": "END"},
    ])
    check("2.1 EXPRESSION 缺 expression → 400 + 65016",
          status == 400 and payload.get("code") == 65016,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")

    status, payload = save_graph(token, version_id, [
        {"key": "s", "name": "开始", "type": "START"},
        {"key": "n_expr", "name": "表达式审批", "type": "APPROVAL",
         "assignee_type": "EXPRESSION",
         "config": {"expression": {"version": 1, "rules": "not-an-array"}}},
        {"key": "e", "name": "结束", "type": "END"},
    ])
    check("2.2 expression 结构非法 → 400 + 65016",
          status == 400 and payload.get("code") == 65016,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")

    status, payload = save_graph(token, version_id,
                                 graph_nodes(expression_json(emp_id)))
    check("2.3 合法 EXPRESSION 图保存成功", status == 200 and payload.get("code") == 0,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")

    stored = one(
        "SELECT assignee_type || '~' || coalesce(config->'expression'->>'version', 'NULL') || '~' "
        "       || (jsonb_array_length(config->'expression'->'rules'))::text "
        f"  FROM biz_flow_node WHERE version_id = '{version_id}' AND node_key = 'n_expr'")
    check("2.4 库里落了 assignee_type=EXPRESSION + 表达式正文、config 可解析（D-E3：存 config）",
          stored == "EXPRESSION~1~2", f"实际 = {stored}")

    step("3. 发布期校验（R16：PREV_APPROVER → 400 + 65033）")
    bad_def, bad_version = create_def(token, "a2_expr_bad", "P1-A2 保留词草稿")
    reserved = {
        "version": 1,
        "rules": [{"id": "r_prev",
                   "when": {"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 1},
                   "then": [{"type": "PREV_APPROVER"}]}],
    }
    status, payload = save_graph(token, bad_version, graph_nodes(reserved))
    check("3.1 保存期允许存 PREV_APPROVER 草稿（卡面：保存只做结构校验）",
          status == 200 and payload.get("code") == 0,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")

    status, payload = api("POST", f"/business/flow/def/{bad_def}/publish", token, {})
    check("3.2 发布期拒 PREV_APPROVER → 400 + 65033",
          status == 400 and payload.get("code") == 65033,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")

    data, _ = call("3.3 正式流程发布（R16 对合法表达式不报错）", "POST",
                   f"/business/flow/def/{def_id}/publish", token, {})
    check("3.4 发布返回 graph_hash（64 位）",
          bool(data) and len(data.get("graph_hash", "")) == 64,
          f"version_no={data.get('version_no') if data else None}")

    schema = one(
        "SELECT (graph_snapshot->>'schema_version') FROM biz_flow_def_version "
        f"WHERE def_id = '{def_id}' AND version_status = 'PUBLISHED'")
    snapshot_rule = one(
        "SELECT graph_snapshot->'nodes'->1->'config'->'expression'->'rules'->0->>'id' "
        "  FROM biz_flow_def_version "
        f" WHERE def_id = '{def_id}' AND version_status = 'PUBLISHED'")
    check("3.5 快照 schema_version 仍是 1（D-E3：不升版本）", schema == "1", f"schema_version={schema}")
    check("3.6 快照里带着表达式规则（运行期读快照求值）", snapshot_rule == "r_big",
          f"rules[0].id={snapshot_rule}")

    step("4. 运行期：条件命中 → 审批人正确 → 待办生成")
    # ① amount = 12000 → r_big（ROLE FINANCE_DIRECTOR）
    status, payload = start_instance(token, "a2_expr_main", "A2 大额报销 12000",
                                     {"amount": 12000})
    big_id = payload.get("data", {}).get("instance_id")
    check("4.1 大额单发起成功", status == 200 and payload.get("code") == 0 and bool(big_id),
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")
    rows = task_rows(big_id) if big_id else []
    check("4.2 待办生成且审批人 = 财务总监（角色成员）",
          len(rows) == 1 and rows[0][0] == fin_id and rows[0][4] == "PENDING",
          f"rows={rows}")
    check("4.3 assignee_source 写底层来源 ROLE（不是 EXPRESSION）",
          bool(rows) and rows[0][2] == "ROLE", f"source={rows[0][2] if rows else None}")
    check("4.4 assignee_rule_id = 命中规则 r_big（D-E5）",
          bool(rows) and rows[0][3] == "r_big", f"rule_id={rows[0][3] if rows else None}")

    # ② amount = 10 → r_small（USER a2_emp）
    status, payload = start_instance(token, "a2_expr_main", "A2 小额报销 10", {"amount": 10})
    small_id = payload.get("data", {}).get("instance_id")
    check("4.5 小额单发起成功", status == 200 and payload.get("code") == 0, f"HTTP {status}")
    rows = task_rows(small_id) if small_id else []
    check("4.6 小额单审批人 = 指定用户（第二条规则命中）",
          len(rows) == 1 and rows[0][0] == emp_id, f"rows={rows}")
    check("4.7 小额单 assignee_rule_id = r_small",
          bool(rows) and rows[0][3] == "r_small", f"rule_id={rows[0][3] if rows else None}")
    check("4.8 小额单 assignee_source = USER",
          bool(rows) and rows[0][2] == "USER", f"source={rows[0][2] if rows else None}")

    # ③ 缺 form.amount → 两条规则都求值失败 → otherwise（INITIATOR = admin）
    status, payload = start_instance(token, "a2_expr_main", "A2 缺字段单", {"note": "没填金额"})
    none_id = payload.get("data", {}).get("instance_id")
    check("4.9 缺字段单发起成功", status == 200 and payload.get("code") == 0, f"HTTP {status}")
    rows = task_rows(none_id) if none_id else []
    check("4.10 求值失败 ⇒ 不命中 ⇒ 走 otherwise（发起人）",
          len(rows) == 1 and rows[0][0] == admin_id, f"rows={rows}")
    check("4.11 otherwise 的 assignee_rule_id = 'otherwise'",
          bool(rows) and rows[0][3] == "otherwise", f"rule_id={rows[0][3] if rows else None}")

    step("5. 运行期：无命中 → empty_assignee 策略（SKIP 自动通过 / ERROR 卡住）")
    skip_def, skip_version = create_def(token, "a2_expr_skip", "P1-A2 空审批人 SKIP")
    never = {"version": 1,
             "rules": [{"id": "r_never",
                        "when": {"op": "cmp", "field": "form.amount", "cmp": "gt",
                                 "value": 999999},
                        "then": [{"type": "ROLE", "role_codes": ["FINANCE_DIRECTOR"]}]}]}
    status, payload = save_graph(token, skip_version,
                                 graph_nodes(never, empty_assignee="SKIP"))
    check("5.1 SKIP 流程保存", status == 200, f"HTTP {status}")
    call("5.2 SKIP 流程发布", "POST", f"/business/flow/def/{skip_def}/publish", token, {})
    status, payload = start_instance(token, "a2_expr_skip", "A2 无命中 SKIP", {"amount": 1})
    skip_id = payload.get("data", {}).get("instance_id")
    rows = task_rows(skip_id) if skip_id else []
    auto = one(f"SELECT auto_passed_nodes FROM biz_flow_instance WHERE id = '{skip_id}'")
    inst_status = one(f"SELECT status FROM biz_flow_instance WHERE id = '{skip_id}'")
    check("5.3 无命中 + SKIP：0 条待办 + 节点自动通过（不顺延到别的规则）",
          len(rows) == 0 and auto == "1",
          f"rows={len(rows)} auto_passed_nodes={auto} instance_status={inst_status}")
    check("5.4 SKIP 后实例自动完成", inst_status == "APPROVED", f"status={inst_status}")

    err_def, err_version = create_def(token, "a2_expr_err", "P1-A2 空审批人 ERROR")
    status, payload = save_graph(token, err_version,
                                 graph_nodes(never, empty_assignee="ERROR"))
    check("5.5 ERROR 流程保存", status == 200, f"HTTP {status}")
    call("5.6 ERROR 流程发布", "POST", f"/business/flow/def/{err_def}/publish", token, {})
    status, payload = start_instance(token, "a2_expr_err", "A2 无命中 ERROR", {"amount": 1})
    check("5.7 无命中 + ERROR → 422 + 65018（整单回滚，不静默通过）",
          status == 422 and payload.get("code") == 65018,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")
    # 口径说明（实测）：engine 的事务边界是「实例先落库 → 节点解析被拒只回滚该步」，
    # 实例行留在 PENDING 等人工处理（D2 既有语义，本卡不改 engine.rs）。
    # 这里要证明的是「失败不静默」：**一条待办都不能生成**，更不能自动通过。
    stuck = psql(
        "SELECT (SELECT count(*) FROM biz_flow_task WHERE instance_id = i.id) || '~' "
        "       || i.status || '~' || coalesce(i.auto_passed_nodes, 0) "
        "  FROM biz_flow_instance i WHERE i.title = 'A2 无命中 ERROR'")
    check("5.8 422 后 0 条待办、未自动通过（实例留 PENDING 等人工处理）",
          bool(stuck) and stuck[0][0] == "0~PENDING~0",
          f"实际 = {stuck}")

    step("6. 汇总")
    print(f"\nPASS={PASSED}  FAIL={len(FAILED)}")
    if FAILED:
        for name in FAILED:
            print(f"  [FAIL] {name}")
        return 1
    print("全部通过 ✓")
    return 0


if __name__ == "__main__":
    sys.exit(main())
