#!/usr/bin/env python3
"""批次 D · D6a 端到端联调：定义 → 发起 → 审批（会签） → 驳回退回重提 → 任务验收 → 清理。

依据：`docs/批次D-工作流审批-技术架构.md` §4（运行时）/ §5.2.2（任务与审批）/ §10（接口）。
卡面：t_92b276ff「① 建流程→3 节点→发布 ② 发起→主管通过→财务会签2/2→完成
③ 再造一单→驳回→退回→重提→通过 ④ 任务验收：REVIEW→自动发起→通过→DONE ⑤ PASS/FAIL 汇总+清理」。

# 怎么跑

    FLOW_E2E_BASE=http://127.0.0.1:8030/api \\
    FLOW_E2E_DB=cloudpivot_e2e \\
    FLOW_E2E_ENV_FILE=/root/d6a-run/d6a.env \\
    python3 scripts/verify/flow_e2e.py

或用参数：`--base` / `--db` / `--env-file` / `--pg-host` / `--pg-port` / `--pg-user`。

环境文件只需两行：`ALUN_DATABASE_PASSWORD=`（psql 用）与 `ALUN_ADMIN_PASSWORD=`（登录 admin 用）。
**不要**把这个脚本指向生产库：它会在收尾阶段**硬删**自己造的全部行（这是「零残留」的实现方式）。

# 断言口径

每一步都断言「接口 200 + code 0」**并且**断言业务语义（实例状态 / 节点状态 / 票数 / 轮次），
不是只看 HTTP 码。汇总打印 PASS/FAIL，任一 FAIL → 退出码 1。

# 数据残留

脚本结束时按 id 逐个表清理（flow 定义侧 5 表 / 运行侧 4 表 / biz_task 及其评论 /
sys_notice 三表 / sys_change_log / sys_user 夹具账号），然后**逐表回查 count = 0**，
回查不通过同样算 FAIL。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

# ==================== 配置 ====================

DEFAULTS = {
    "base": os.environ.get("FLOW_E2E_BASE", "http://127.0.0.1:8029/api"),
    "db": os.environ.get("FLOW_E2E_DB", "cloudpivot_d2v"),
    "env_file": os.environ.get("FLOW_E2E_ENV_FILE", "/etc/cloudpivot/cloudpivot-test.env"),
    "pg_host": os.environ.get("FLOW_E2E_PG_HOST", "127.0.0.1"),
    "pg_port": os.environ.get("FLOW_E2E_PG_PORT", "5432"),
    "pg_user": os.environ.get("FLOW_E2E_PG_USER", "cloudpivot"),
}

# 夹具账号（收尾会硬删；用户名带 d6a 前缀，便于人工核对）
FIXTURE_PASSWORD = "E2e#Flow2026"
FIXTURE_PREFIX = "d6a_"
FIXTURE_USERS = {
    "sup": ("d6a_sup", "D6a 主管"),
    "fin1": ("d6a_fin1", "D6a 财务甲"),
    "fin2": ("d6a_fin2", "D6a 财务乙"),
}

FLOW_CODE = "d6a_e2e_main"
FLOW_CODE_ACCEPT = "d6a_e2e_task_accept"

PASSED = 0
FAILED = 0
FAILS = []


# ==================== 基础设施 ====================

def load_env_file(path):
    """读 key=value 环境文件（只取值，不导出）。"""
    values = {}
    if not os.path.exists(path):
        return values
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip()
    return values


CFG = dict(DEFAULTS)
ENV = {}


def api_raw(method, url, token=None, body=None, timeout=30):
    """打一次绝对 URL（用于 `/health` 这类不在 `/api` 前缀下的探针）。"""
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read().decode()
            return response.status, json.loads(raw or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw[:500]}


def api(method, path, token=None, body=None, timeout=60):
    """打一次接口，返回 (status, json)。"""
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(CFG["base"] + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read().decode()
            return response.status, json.loads(raw or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw[:500]}


def psql(sql):
    """跑一条 SQL，返回二维列表（`-At -F'|'`）。"""
    result = subprocess.run(
        ["psql", "-h", CFG["pg_host"], "-p", str(CFG["pg_port"]), "-U", CFG["pg_user"],
         "-d", CFG["db"], "-At", "-F", "|", "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True, text=True,
        env={**os.environ, "PGPASSWORD": ENV.get("ALUN_DATABASE_PASSWORD", "")})
    if result.returncode != 0:
        raise SystemExit(f"psql 失败：{result.stderr.strip()}\nSQL: {sql}")
    return [line.split("|") for line in result.stdout.strip().splitlines() if line]


def sql_one(sql):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] and rows[0][0] != "" else None


def check(name, condition, detail=""):
    """断言 + 记一行结果。"""
    global PASSED, FAILED
    if condition:
        PASSED += 1
        print(f"[PASS] {name}")
    else:
        FAILED += 1
        FAILS.append(name)
        print(f"[FAIL] {name}")
    if detail:
        print(f"       {detail}")
    return bool(condition)


def step(title):
    print()
    print("-" * 78)
    print(title)
    print("-" * 78)


def ok_call(name, method, path, token=None, body=None):
    """调接口并要求 HTTP 200 + code 0；返回 data（失败返回 None，不抛异常）。"""
    status, payload = api(method, path, token, body)
    data = payload.get("data")
    good = status == 200 and payload.get("code") == 0
    check(name, good,
          f"{method} {path} → HTTP {status} code={payload.get('code')} "
          f"msg={payload.get('msg') or payload.get('message') or ''}")
    return data if good else None


# ==================== 登录 ====================

def login(username, password, attempts=3):
    """登录；命中「登录尝试过于频繁（429）」时等满限流窗口再重试。

    限流键 = `login:rate:{用户名}:{IP}`，窗口 60 秒、上限 10 次（shared/constants.rs）。
    踩到 429 时不要连打：每打一次都在加计数，只会把窗口一直顶住。
    """
    last = (0, {})
    for index in range(attempts):
        status, payload = api("POST", "/system/auth/login",
                              body={"username": username, "password": password})
        last = (status, payload)
        if status != 429:
            return last
        print(f"       登录限流 429，等满 62s 窗口再试（{index + 1}/{attempts}）")
        time.sleep(62)
    return last


def change_password(token, old, new):
    return api("PUT", "/system/auth/password", token,
               {"old_password": old, "new_password": new})


def login_and_settle(username, candidates):
    """登录；首登要求改密则改密后重登。返回 (token, 生效密码)。"""
    last = None
    for candidate in candidates:
        status, payload = login(username, candidate)
        last = (status, payload)
        if status != 200 or payload.get("code") != 0:
            continue
        data = payload["data"]
        token = data["access_token"]
        if data.get("must_change_password"):
            fresh = candidate + "-d6a"
            status, payload = change_password(token, candidate, fresh)
            if status != 200 or payload.get("code") != 0:
                return token, candidate
            status, payload = login(username, fresh)
            if status != 200 or payload.get("code") != 0:
                raise SystemExit(f"{username} 改密后重登失败：{status} {payload}")
            return payload["data"]["access_token"], fresh
        return token, candidate
    raise SystemExit(f"{username} 登录失败：{last}")


def admin_session():
    initial = ENV.get("ALUN_ADMIN_PASSWORD", "")
    token, password = login_and_settle(
        "admin", [initial + "-d6a", initial, initial + "-e2e", initial + "-d2"])
    check("0.1 admin 登录（含首登改密）", bool(token), f"生效密码长度={len(password)}")
    return token


# ==================== 夹具 ====================

def ensure_fixtures(admin_token):
    """建 3 个夹具账号（主管 1 + 会签人 2），返回 {key: user_id} 与各自 token。"""
    role_id = sql_one("SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN'")
    check("1.1 取到超级管理员角色（夹具账号要能点审批按钮）", bool(role_id), f"role_id={role_id}")

    tokens, ids = {}, {}
    for key, (username, real_name) in FIXTURE_USERS.items():
        user_id = sql_one(f"SELECT id FROM sys_user WHERE username = '{username}' AND deleted_at IS NULL")
        if not user_id:
            created = ok_call(f"1.2 建夹具账号 {username}", "POST", "/system/user", admin_token, {
                "username": username,
                "password": FIXTURE_PASSWORD,
                "real_name": real_name,
                "status": 1,
                "role_ids": [role_id] if role_id else [],
                "remark": "D6a 端到端联调夹具（脚本自动清理）",
            })
            if created:
                user_id = created.get("id")
            if not user_id:
                user_id = sql_one(f"SELECT id FROM sys_user WHERE username = '{username}'")
        if not user_id:
            raise SystemExit(f"夹具账号 {username} 建不出来，后续无法继续")
        ids[key] = user_id
        tokens[key] = login_and_settle(username, [FIXTURE_PASSWORD, FIXTURE_PASSWORD + "-d6a"])[0]
        print(f"       夹具 {username} → user_id={user_id}")
    check("1.3 三个夹具账号全部可登录",
          all(tokens.get(k) for k in FIXTURE_USERS), f"账号={list(tokens)}")
    return ids, tokens


# ==================== 图 ====================

def save_graph(token, version_id, nodes, edges, note):
    return ok_call(f"保存整图（{note}）", "PUT", f"/business/flow/version/{version_id}/graph",
                   token, {"nodes": nodes, "edges": edges, "change_note": note})


def publish_def(token, def_id, label):
    data = ok_call(f"{label} · 发布", "POST", f"/business/flow/def/{def_id}/publish", token, {})
    if data:
        digest = data.get("graph_hash") or ""
        check(f"{label} · 发布返回 64 位 graph_hash",
              len(digest) == 64 and all(c in "0123456789abcdef" for c in digest),
              f"version_no={data.get('version_no')} hash={digest[:16]}…")
    return data


def make_main_graph(users):
    """START → 主管审批 → 财务会签(2 人 ALL) → END。

    卡面「3 节点」= 主干三个业务节点（主管审批 / 财务会签 / 结束），START 是引擎占位。
    """
    return [
        {"key": "s", "name": "开始", "type": "START", "x": 0, "y": 0},
        {"key": "n_sup", "name": "主管审批", "type": "APPROVAL", "x": 200, "y": 0,
         "assignee_type": "USER", "self_approval": "ALLOW",
         "assignees": [{"type": "USER", "ref_id": users["sup"]}]},
        {"key": "n_fin", "name": "财务会签", "type": "COUNTERSIGN", "x": 400, "y": 0,
         "assignee_type": "USER", "approval_mode": "ALL", "reject_veto": True,
         "self_approval": "ALLOW",
         "assignees": [{"type": "USER", "ref_id": users["fin1"]},
                       {"type": "USER", "ref_id": users["fin2"]}]},
        {"key": "e", "name": "结束", "type": "END", "x": 600, "y": 0},
    ], [
        {"key": "e1", "from": "s", "to": "n_sup"},
        {"key": "e2", "from": "n_sup", "to": "n_fin"},
        {"key": "e3", "from": "n_fin", "to": "e"},
    ]


# ==================== 待办 ====================

def find_todo(token, instance_id, kind=None):
    """在该用户的待办里找指定实例的 PENDING 待办。"""
    status, payload = api("GET", "/business/flow/task/todo?page=1&page_size=100", token)
    if status != 200 or payload.get("code") != 0:
        return None
    for item in payload["data"]["list"]:
        if item.get("instance_id") != instance_id:
            continue
        if kind and item.get("task_kind") != kind:
            continue
        return item
    return None


def approve(token, task_id, comment):
    return ok_call(f"同意待办 {task_id[:8]}…", "POST",
                   f"/business/flow/task/{task_id}/approve", token, {"comment": comment})


# ==================== 主流程 ====================

def main():
    parser = argparse.ArgumentParser(description="批次 D · D6a 端到端联调")
    for key in DEFAULTS:
        parser.add_argument("--" + key.replace("_", "-"), default=DEFAULTS[key])
    args = parser.parse_args()
    for key in DEFAULTS:
        CFG[key] = getattr(args, key)

    ENV.update(load_env_file(CFG["env_file"]))
    print("=" * 78)
    print("批次 D · D6a 端到端联调（flow_e2e.py）")
    print(f"接口={CFG['base']}  库={CFG['db']}  环境文件={CFG['env_file']}")
    print("=" * 78)

    created = {"defs": [], "versions": [], "instances": [], "tasks": [], "users": []}

    # ---------- 0. 前置 ----------
    step("0. 前置：服务健康 + 鉴权生效")
    root = CFG["base"][:-4] if CFG["base"].endswith("/api") else CFG["base"]
    status, payload = api_raw("GET", root + "/health", timeout=15)
    check("0.0 /health 可达", status == 200,
          f"{root}/health → HTTP {status} {payload.get('data')}")
    status, _ = api("GET", "/business/flow/def/page?page=1&page_size=1")
    check("0.2 匿名访问流程定义 401（鉴权生效，不是 404/500）", status == 401, f"HTTP {status}")

    admin = admin_session()

    # ---------- 1. 夹具 ----------
    step("1. 夹具：主管 1 + 会签人 2（临时账号，收尾硬删）")
    users, tokens = ensure_fixtures(admin)
    created["users"] = list(users.values())

    # ---------- 2. ① 建流程 → 3 节点 → 发布 ----------
    step("① 建流程 → 3 节点 → 发布")
    def_data = ok_call("2.1 新建流程定义（biz_type=D6A_E2E）", "POST", "/business/flow/def", admin, {
        "flow_code": FLOW_CODE,
        "flow_name": "D6a 端到端联调流程",
        "biz_type": "D6A_E2E",
        "description": "D6a 脚本自动创建/清理",
    })
    if not def_data:
        raise SystemExit("建流程失败，无法继续")
    def_id, version_id = def_data["id"], def_data["version_id"]
    created["defs"].append(def_id)
    created["versions"].append(version_id)
    check("2.2 新建流程返回 DRAFT v1 + 版本 id", bool(def_id and version_id),
          f"def_id={def_id} version_id={version_id} status={def_data.get('status')}")

    nodes, edges = make_main_graph(users)
    graph = save_graph(admin, version_id, nodes, edges, "D6a 主管审批 + 财务会签(2 人)")
    if graph:
        check("2.3 图落地：4 个节点（START/主管审批/财务会签/END）+ 3 条边、无保存期告警",
              graph.get("node_count") == 4 and graph.get("edge_count") == 3
              and not graph.get("warnings"),
              f"node_count={graph.get('node_count')} edge_count={graph.get('edge_count')} "
              f"warnings={graph.get('warnings')}")

    published = publish_def(admin, def_id, "2.4 主流程")
    detail = ok_call("2.5 发布后查详情", "GET", f"/business/flow/def/{def_id}", admin)
    if detail:
        check("2.6 定义状态 PUBLISHED + 当前版本已挂",
              detail.get("status") == "PUBLISHED" and bool(detail.get("current_version_id")),
              f"status={detail.get('status')} current_version_id={detail.get('current_version_id')}")

    # ---------- 3. ② 发起 → 主管通过 → 财务会签 2/2 → 完成 ----------
    step("② 发起 → 主管通过 → 财务会签 2/2 → 完成")
    start = ok_call("3.1 发起审批单", "POST", "/business/flow/instance", admin,
                    {"def_id": def_id, "title": "D6a-联调-全通过"})
    if not start:
        raise SystemExit("发起失败，无法继续")
    inst1 = start["instance_id"]
    created["instances"].append(inst1)
    detail = ok_call("3.2 发起后查实例详情", "GET", f"/business/flow/instance/{inst1}", admin)
    nodes1 = {n["node_key"]: n for n in (detail or {}).get("nodes", [])}
    brief1 = {k: (v["status"], v["approver_count"]) for k, v in nodes1.items()}
    check("3.2 发起后停在主管审批（实例 PENDING / START 自动过 / n_sup ACTIVE 应到 1 人）",
          start["instance_status"] == "PENDING" and start["auto_passed_nodes"] == 0
          and nodes1.get("s", {}).get("status") == "APPROVED"
          and nodes1.get("n_sup", {}).get("status") == "ACTIVE"
          and nodes1.get("n_sup", {}).get("approver_count") == 1,
          f"status={start['instance_status']} auto_passed={start['auto_passed_nodes']} "
          f"节点={brief1}")

    todo = find_todo(tokens["sup"], inst1, "APPROVAL")
    check("3.3 主管待办可查（assignee_source=USER）", todo is not None,
          f"task_id={(todo or {}).get('id')} node={(todo or {}).get('node_key')} "
          f"source={(todo or {}).get('assignee_source')}")
    if not todo:
        raise SystemExit("主管待办查不到，无法继续")
    out = approve(tokens["sup"], todo["id"], "主管同意，转财务会签")
    check("3.4 主管通过（待办动作：节点 APPROVED / 实例仍 PENDING）",
          bool(out) and out["instance_status"] == "PENDING" and out["node_status"] == "APPROVED",
          f"inst={(out or {}).get('instance_status')} node={(out or {}).get('node_status')}")
    if out:
        detail = ok_call("3.4b 查实例详情（会签节点已激活）", "GET",
                         f"/business/flow/instance/{inst1}", admin)
        fin_node = next((n for n in (detail or {}).get("nodes", [])
                         if n["node_key"] == "n_fin"), {})
        check("3.4b 下游财务会签激活：ACTIVE / 应到 2 人 / 已审 0 人 / ALL 模式",
              fin_node.get("status") == "ACTIVE" and fin_node.get("approver_count") == 2
              and fin_node.get("approved_count") == 0
              and fin_node.get("approval_mode") == "ALL",
              f"n_fin={fin_node.get('status')} "
              f"{fin_node.get('approved_count')}/{fin_node.get('approver_count')} "
              f"mode={fin_node.get('approval_mode')}")

    fin1_todo = find_todo(tokens["fin1"], inst1, "APPROVAL")
    fin2_todo = find_todo(tokens["fin2"], inst1, "APPROVAL")
    check("3.5 两位会签人各有一条待办（会签不是单点）",
          fin1_todo is not None and fin2_todo is not None,
          f"fin1={(fin1_todo or {}).get('id')} fin2={(fin2_todo or {}).get('id')}")
    if not (fin1_todo and fin2_todo):
        raise SystemExit("会签待办缺失，无法继续")

    out = approve(tokens["fin1"], fin1_todo["id"], "财务甲同意")
    if out:
        # 关键断言：会签 ALL 模式下 1/2 不能过节点（ActionRes.done 只表示「这条待办处理完」）
        check("3.6 会签 1/2 未达阈值：节点仍 ACTIVE、实例仍 PENDING",
              out["approved_count"] == 1 and out["approver_count"] == 2
              and out["node_status"] == "ACTIVE" and out["instance_status"] == "PENDING",
              f"{out['approved_count']}/{out['approver_count']} node={out['node_status']} "
              f"inst={out['instance_status']} done={out['done']}")

    out = approve(tokens["fin2"], fin2_todo["id"], "财务乙同意")
    if out:
        check("3.7 会签 2/2 达成：节点 APPROVED + 实例 APPROVED",
              out["approved_count"] == 2 and out["node_status"] == "APPROVED"
              and out["instance_status"] == "APPROVED" and out["done"] is True,
              f"{out['approved_count']}/{out['approver_count']} node={out['node_status']} "
              f"inst={out['instance_status']} next={out['next_nodes']}")

    detail = ok_call("3.8 查实例详情", "GET", f"/business/flow/instance/{inst1}", admin)
    if detail:
        statuses = {n["node_key"]: n["status"] for n in detail["nodes"]}
        check("3.9 详情：4 个节点全 APPROVED、无自动通过、时间线留痕 ≥3 条",
              all(statuses.get(k) == "APPROVED" for k in ("s", "n_sup", "n_fin", "e"))
              and detail["auto_passed_nodes"] == 0 and len(detail["timeline"]) >= 3,
              f"节点={statuses} auto_passed={detail['auto_passed_nodes']} "
              f"timeline={len(detail['timeline'])}")

    # ---------- 4. ③ 再造一单 → 驳回 → 退回发起人 → 重提 → 通过 ----------
    step("③ 再造一单 → 驳回 → 退回发起人 → 重提 → 通过")
    start = ok_call("4.1 再造一单", "POST", "/business/flow/instance", admin,
                    {"def_id": def_id, "title": "D6a-联调-驳回重提"})
    if not start:
        raise SystemExit("第二单发起失败，无法继续")
    inst2 = start["instance_id"]
    created["instances"].append(inst2)

    todo = find_todo(tokens["sup"], inst2, "APPROVAL")
    reject = ok_call("4.2 主管驳回并退回发起人（return_to=INITIATOR）", "POST",
                     f"/business/flow/task/{todo['id']}/reject", tokens["sup"],
                     {"comment": "金额与合同不一致，请修改后再提", "return_to": "INITIATOR"})
    if reject:
        check("4.3 驳回后实例未终止（PENDING，退回 = 同实例 round+1）",
              reject["instance_status"] == "PENDING" and reject["node_status"] == "REJECTED"
              and "s" in reject["next_nodes"],
              f"inst={reject['instance_status']} node={reject['node_status']} "
              f"next={reject['next_nodes']}")

    resubmit = find_todo(admin, inst2, "RESUBMIT")
    check("4.4 发起人收到 RESUBMIT 待办（round 2）", resubmit is not None,
          f"task_id={(resubmit or {}).get('id')} node={(resubmit or {}).get('node_key')} "
          f"round={(resubmit or {}).get('round')}")
    if not resubmit:
        raise SystemExit("重提交待办缺失，无法继续")
    before = ok_call("4.5 查实例详情（退回后）", "GET", f"/business/flow/instance/{inst2}", admin)
    if before:
        rounds = [(n["node_key"], n["round"], n["status"]) for n in before["nodes"]]
        check("4.6 退回后时间线保留第 1 轮、并新增第 2 轮节点",
              any(r[1] == 1 for r in rounds) and any(r[1] == 2 for r in rounds),
              f"节点(轮次/状态)={rounds}")

    out = approve(admin, resubmit["id"], "已按意见修改，重新提交")
    resubmit_detail = ok_call("4.7 查实例详情（重提后）", "GET",
                              f"/business/flow/instance/{inst2}", admin)
    if out and resubmit_detail:
        sup_round2 = [n for n in resubmit_detail["nodes"]
                      if n["node_key"] == "n_sup" and n["round"] == 2]
        check("4.7 重提后回到主管审批（实例 PENDING + n_sup 第 2 轮 ACTIVE）",
              out["instance_status"] == "PENDING" and bool(sup_round2)
              and sup_round2[0]["status"] == "ACTIVE",
              f"inst={out['instance_status']} next={out['next_nodes']} "
              f"n_sup第2轮={sup_round2[0]['status'] if sup_round2 else 'MISSING'}")

    todo = find_todo(tokens["sup"], inst2, "APPROVAL")
    out = approve(tokens["sup"], todo["id"], "修改后同意")
    fin1_todo = find_todo(tokens["fin1"], inst2, "APPROVAL")
    fin2_todo = find_todo(tokens["fin2"], inst2, "APPROVAL")
    check("4.8 第 2 轮会签待办重新生成（旧轮次不复活）",
          fin1_todo is not None and fin2_todo is not None,
          f"fin1={(fin1_todo or {}).get('round') if fin1_todo else None} "
          f"fin2={(fin2_todo or {}).get('round') if fin2_todo else None}")
    if fin1_todo and fin2_todo:
        approve(tokens["fin1"], fin1_todo["id"], "财务甲同意（第 2 轮）")
        out = approve(tokens["fin2"], fin2_todo["id"], "财务乙同意（第 2 轮）")
        if out:
            check("4.9 重提链路走完：实例 APPROVED", out["instance_status"] == "APPROVED",
                  f"inst={out['instance_status']} {out['approved_count']}/{out['approver_count']}")
    detail = ok_call("4.10 查实例详情（重提完成后）", "GET", f"/business/flow/instance/{inst2}", admin)
    if detail:
        approved_rounds = sorted({n["round"] for n in detail["nodes"] if n["status"] == "APPROVED"})
        check("4.11 详情：第 2 轮全通过、第 1 轮驳回留痕仍在",
              detail["instance"]["status"] == "APPROVED"
              and any(n["status"] == "REJECTED" for n in detail["nodes"])
              and 2 in approved_rounds,
              f"inst={detail['instance']['status']} 通过轮次={approved_rounds} "
              f"驳回留痕={any(n['status'] == 'REJECTED' for n in detail['nodes'])}")

    # ---------- 5. ④ 任务验收：REVIEW → 自动发起 → 通过 → DONE ----------
    step("④ 任务验收：REVIEW → 自动发起审批 → 通过 → DONE")
    def_data = ok_call("5.1 建 TASK_ACCEPT 流程（默认流程）", "POST", "/business/flow/def", admin, {
        "flow_code": FLOW_CODE_ACCEPT,
        "flow_name": "D6a 任务验收流程",
        "biz_type": "TASK_ACCEPT",
        "is_default": True,
        "description": "D6a 脚本自动创建/清理",
    })
    if not def_data:
        raise SystemExit("TASK_ACCEPT 流程建不出来，无法继续")
    accept_def, accept_version = def_data["id"], def_data["version_id"]
    created["defs"].append(accept_def)
    created["versions"].append(accept_version)

    accept_nodes = [
        {"key": "s", "name": "开始", "type": "START", "x": 0, "y": 0},
        {"key": "n_accept", "name": "验收审批", "type": "APPROVAL", "x": 200, "y": 0,
         "assignee_type": "USER", "self_approval": "ALLOW",
         "assignees": [{"type": "USER", "ref_id": users["sup"]}]},
        {"key": "e", "name": "结束", "type": "END", "x": 400, "y": 0},
    ]
    accept_edges = [{"key": "e1", "from": "s", "to": "n_accept"},
                    {"key": "e2", "from": "n_accept", "to": "e"}]
    save_graph(admin, accept_version, accept_nodes, accept_edges, "D6a 任务验收图")
    publish_def(admin, accept_def, "5.2 验收流程")

    task = ok_call("5.3 建任务（need_review=true）", "POST", "/business/task", admin,
                   {"title": "D6a-联调-任务验收", "need_review": True})
    task_id = (task or {}).get("id")
    if not task_id:
        raise SystemExit("任务建不出来，无法继续")
    created["tasks"].append(task_id)
    print(f"       task_id={task_id} status={(task or {}).get('status')}")

    ok_call("5.4 任务 TODO → DOING", "PATCH", f"/business/task/{task_id}/status", admin,
            {"to": "DOING"})
    ok_call("5.5 任务 DOING → REVIEW（触发自动发起审批）", "PATCH",
            f"/business/task/{task_id}/status", admin, {"to": "REVIEW"})

    detail = ok_call("5.6 查任务详情", "GET", f"/business/task/{task_id}", admin)
    approval_instance = (detail or {}).get("approval_instance_id")
    check("5.7 REVIEW 时自动发起审批单并回写 approval_instance_id",
          (detail or {}).get("status") == "REVIEW" and bool(approval_instance),
          f"task_status={(detail or {}).get('status')} approval_instance_id={approval_instance}")
    if not approval_instance:
        raise SystemExit("任务验收没有自动发起审批单，D3b 接线断了")
    created["instances"].append(approval_instance)

    inst_detail = ok_call("5.8 查审批单详情", "GET", f"/business/flow/instance/{approval_instance}", admin)
    if inst_detail:
        check("5.9 审批单挂在任务上（biz_type=TASK_ACCEPT / biz_id=任务 id / PENDING）",
              inst_detail["instance"]["biz_type"] == "TASK_ACCEPT"
              and inst_detail["instance"].get("biz_id") == task_id
              and inst_detail["instance"]["status"] == "PENDING",
              f"biz_type={inst_detail['instance']['biz_type']} "
              f"biz_id={inst_detail['instance'].get('biz_id')} "
              f"status={inst_detail['instance']['status']}")

    todo = find_todo(tokens["sup"], approval_instance, "APPROVAL")
    check("5.10 验收审批待办可查", todo is not None, f"task_id={(todo or {}).get('id')}")
    if todo:
        out = approve(tokens["sup"], todo["id"], "验收通过")
        check("5.11 审批通过 → 实例 APPROVED", bool(out) and out["instance_status"] == "APPROVED",
              f"inst={(out or {}).get('instance_status')}")

    detail = ok_call("5.12 再查任务详情（等回调推进）", "GET", f"/business/task/{task_id}", admin)
    if detail:
        check("5.13 流程回调把任务推到 DONE（REVIEW → DONE 不是人工点的）",
              detail.get("status") == "DONE",
              f"task_status={detail.get('status')} approval_instance_id={detail.get('approval_instance_id')}")

    # ---------- 6. ⑤ 清理 + 零残留 ----------
    step("⑤ 清理测试数据 + 零残留回查")
    cleanup(created)

    print()
    print("=" * 78)
    print(f"汇总：PASS {PASSED} / FAIL {FAILED}")
    if FAILS:
        for name in FAILS:
            print(f"  FAIL · {name}")
    print("=" * 78)
    return 0 if FAILED == 0 else 1


def sql_list(values):
    """Python 列表 → SQL 数组字面量（uuid/text 均按文本比较）。"""
    return "ARRAY[" + ",".join("'" + value + "'" for value in values) + "]::uuid[]"


def cleanup(created):
    """按依赖顺序硬删本次跑出来的全部行，然后逐表回查。"""
    defs, versions = created["defs"], created["versions"]
    instances, tasks, users = created["instances"], created["tasks"], created["users"]

    if instances:
        insts = sql_list(instances)
        psql("DELETE FROM sys_notice_receiver WHERE notice_id IN "
             f"(SELECT id FROM sys_notice WHERE biz_id = ANY({insts}::text[]))")
        psql("DELETE FROM sys_notice_delivery WHERE notice_id IN "
             f"(SELECT id FROM sys_notice WHERE biz_id = ANY({insts}::text[]))")
        psql(f"DELETE FROM sys_notice WHERE biz_id = ANY({insts}::text[])")
        psql(f"DELETE FROM biz_flow_cc WHERE instance_id = ANY({insts})")
        psql(f"DELETE FROM biz_flow_task WHERE instance_id = ANY({insts})")
        psql(f"DELETE FROM biz_flow_instance_node WHERE instance_id = ANY({insts})")
        psql(f"DELETE FROM biz_flow_instance WHERE id = ANY({insts})")

    if versions:
        vers = sql_list(versions)
        psql(f"DELETE FROM biz_flow_node_assignee WHERE node_id IN "
             f"(SELECT id FROM biz_flow_node WHERE version_id = ANY({vers}))")
        psql(f"DELETE FROM biz_flow_edge WHERE version_id = ANY({vers})")
        psql(f"DELETE FROM biz_flow_node WHERE version_id = ANY({vers})")

    if tasks:
        task_ids = sql_list(tasks)
        psql(f"DELETE FROM biz_task_comment WHERE task_id = ANY({task_ids})")
        psql(f"DELETE FROM biz_task_member WHERE task_id = ANY({task_ids})")
        psql(f"DELETE FROM biz_task WHERE id = ANY({task_ids})")

    if defs:
        def_ids = sql_list(defs)
        psql(f"DELETE FROM biz_flow_def_version WHERE def_id = ANY({def_ids})")
        psql(f"DELETE FROM biz_flow_def WHERE id = ANY({def_ids})")

    if users:
        user_ids = sql_list(users)
        psql(f"DELETE FROM sys_user_role WHERE user_id = ANY({user_ids})")
        psql(f"DELETE FROM sys_user WHERE id = ANY({user_ids})")
        # 夹具账号的登录留痕（登录页会写 auth_logs + security_logs）
        psql(f"DELETE FROM auth_logs WHERE username LIKE '{FIXTURE_PREFIX}%'")
        psql(f"DELETE FROM security_logs WHERE username LIKE '{FIXTURE_PREFIX}%'")

    # 审计 / 变更日志：按 resource_id 清（flow 三表 / biz_task / 夹具账号的变更都记在这里）
    all_ids = (created["defs"] + created["versions"] + created["instances"]
               + created["tasks"] + created["users"])
    if all_ids:
        ids = sql_list(all_ids)
        psql(f"DELETE FROM sys_change_log WHERE resource_id::text = ANY({ids}::text[])")

    # 回查
    probes = []
    if instances:
        insts = sql_list(instances)
        probes += [
            ("biz_flow_instance", f"SELECT count(*) FROM biz_flow_instance WHERE id = ANY({insts})"),
            ("biz_flow_instance_node",
             f"SELECT count(*) FROM biz_flow_instance_node WHERE instance_id = ANY({insts})"),
            ("biz_flow_task", f"SELECT count(*) FROM biz_flow_task WHERE instance_id = ANY({insts})"),
            ("biz_flow_cc", f"SELECT count(*) FROM biz_flow_cc WHERE instance_id = ANY({insts})"),
            ("sys_notice（审批通知）",
             f"SELECT count(*) FROM sys_notice WHERE biz_id = ANY({insts}::text[])"),
            ("sys_notice_receiver（送达）",
             "SELECT count(*) FROM sys_notice_receiver WHERE notice_id IN "
             f"(SELECT id FROM sys_notice WHERE biz_id = ANY({insts}::text[]))"),
        ]
    if versions:
        vers = sql_list(versions)
        probes += [
            ("biz_flow_node", f"SELECT count(*) FROM biz_flow_node WHERE version_id = ANY({vers})"),
            ("biz_flow_edge", f"SELECT count(*) FROM biz_flow_edge WHERE version_id = ANY({vers})"),
            ("biz_flow_node_assignee",
             "SELECT count(*) FROM biz_flow_node_assignee WHERE node_id IN "
             f"(SELECT id FROM biz_flow_node WHERE version_id = ANY({vers}))"),
        ]
    if defs:
        def_ids = sql_list(defs)
        probes += [
            ("biz_flow_def", f"SELECT count(*) FROM biz_flow_def WHERE id = ANY({def_ids})"),
            ("biz_flow_def_version",
             f"SELECT count(*) FROM biz_flow_def_version WHERE def_id = ANY({def_ids})"),
        ]
    if tasks:
        task_ids = sql_list(tasks)
        probes += [
            ("biz_task", f"SELECT count(*) FROM biz_task WHERE id = ANY({task_ids})"),
            ("biz_task_comment", f"SELECT count(*) FROM biz_task_comment WHERE task_id = ANY({task_ids})"),
        ]
    if users:
        user_ids = sql_list(users)
        probes += [("sys_user（夹具账号）", f"SELECT count(*) FROM sys_user WHERE id = ANY({user_ids})")]

    all_ids = (created["defs"] + created["versions"] + created["instances"]
               + created["tasks"] + created["users"])
    if all_ids:
        ids = sql_list(all_ids)
        probes += [("sys_change_log（变更日志）",
                    f"SELECT count(*) FROM sys_change_log WHERE resource_id::text = ANY({ids}::text[])")]
        probes += [("auth_logs（夹具登录留痕）",
                    f"SELECT count(*) FROM auth_logs WHERE username LIKE '{FIXTURE_PREFIX}%'")]
        probes += [("security_logs（夹具登录留痕）",
                    f"SELECT count(*) FROM security_logs WHERE username LIKE '{FIXTURE_PREFIX}%'")]

    residue = {}
    for label, sql in probes:
        residue[label] = int(sql_one(sql) or 0)
    leaked = {k: v for k, v in residue.items() if v != 0}
    check(f"6.1 零残留：{len(probes)} 张表逐个回查 count=0", not leaked,
          f"残留={leaked if leaked else '无'} / 回查表={list(residue)}")
    print("       回查明细：" + ", ".join(f"{k}={v}" for k, v in residue.items()))


if __name__ == "__main__":
    sys.exit(main())
