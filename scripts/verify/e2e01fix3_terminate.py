#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""E2E01-FIX-3 真机验收：审批实例**人工终止**（会签缺人卡死的出路）。

卡面 t_96198b53。验收项与实现一一对应：

| # | 验收 | 本脚本的做法 |
|---|---|---|
| ① | 会签缺人 → 提交后卡住 | 3 人会签（COUNTERSIGN/ALL）自定义流程，发起后只有 1 人签，另 2 人停用 |
| ② | terminate → 实例终态 | `POST /api/business/flow/instance/{id}/terminate` → 实例 `CANCELLED`（口径见 docs） |
| ③ | 关联单据退回 DRAFT | 采购订单提交审批 → 终止审批单 → `biz_purchase_order.status = DRAFT` |
| ④ | 操作日志有记录 | `operation_logs` 出现「终止审批实例」+ 原因 + 操作人 |
| ⑤ | 无权限 → 403 | 无任何角色的账号调 terminate → HTTP 403 |
| ⑥ | 已结束实例 → 400 | 同一实例再终止 → HTTP 400 + 65024 |
| ⑦ | 前端按钮 | 前端走 build + typecheck（本脚本覆盖后端行为）；按钮可见性由单测/构建产物佐证 |

另外还会**只读扫描**现存「卡住的实例」并打印出来（卡面 ③「处理同类卡住实例」）。

# 怎么跑（在 145 上）

```bash
cd /opt/cloudpivot
python3 scripts/verify/e2e01fix3_terminate.py --base http://127.0.0.1:8033/api
```

默认值：`--base http://127.0.0.1:8033/api`、`--env-file /etc/cloudpivot/cloudpivot-dev.env`
（从里面读 `NIK_DATABASE_*` 与 `NIK_ADMIN_PASSWORD`）。

**不要指向生产库**：脚本会造夹具并在收尾阶段硬删自己造的全部行；`db_guard` 会先拦一道。

# 残留

收尾按外键顺序删：实例 → 节点 → 待办 → 流程定义侧 5 表 → 采购订单 → 夹具账号 → 通知/操作日志，
最后逐项回查 count = 0，回查不通过也算 FAIL。
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from db_guard import require_non_prod  # noqa: E402

require_non_prod(script=os.path.basename(__file__))

import argparse  # noqa: E402
import json  # noqa: E402
import subprocess  # noqa: E402
import time  # noqa: E402
import urllib.error  # noqa: E402
import urllib.request  # noqa: E402
import uuid  # noqa: E402

DEFAULTS = {
    "base": os.environ.get("E2E13_BASE", "http://127.0.0.1:8033/api"),
    "env_file": os.environ.get("E2E13_ENV_FILE", "/etc/cloudpivot/cloudpivot-dev.env"),
    "db": "",
}

FIXTURE_PASSWORD = "E2e#Term32026"
SUFFIX = uuid.uuid4().hex[:8]
FLOW_CODE = "term3_cs_" + SUFFIX
BIZ_TYPE = "TERM3_CS_" + SUFFIX.upper()

PASSED = 0
FAILED = 0
FAILS = []

CFG = dict(DEFAULTS)
ENV = {}

TERMINATE_REASON = "会签节点缺人（3 人会签只有 1 人在职），人工终止"


# ==================== 基础设施 ====================

def load_env_file(path):
    values = {}
    if not os.path.exists(path):
        return values
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def db_conf():
    return {
        "host": os.environ.get("DB_HOST") or ENV.get("NIK_DATABASE_HOST", "127.0.0.1"),
        "port": os.environ.get("DB_PORT") or ENV.get("NIK_DATABASE_PORT", "5432"),
        "user": os.environ.get("DB_USER") or ENV.get("NIK_DATABASE_USER", "cloudpivot"),
        "name": CFG["db"] or os.environ.get("DB_NAME") or ENV.get("NIK_DATABASE_NAME", ""),
        "password": os.environ.get("DB_PASS") or ENV.get("NIK_DATABASE_PASSWORD", ""),
    }


def api(method, path, token=None, body=None, timeout=60):
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
    conf = db_conf()
    result = subprocess.run(
        ["psql", "-h", conf["host"], "-p", str(conf["port"]), "-U", conf["user"],
         "-d", conf["name"], "-At", "-F", "|", "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True, text=True,
        env={**os.environ, "PGPASSWORD": conf["password"]})
    if result.returncode != 0:
        raise SystemExit("psql 失败：" + result.stderr.strip() + "\nSQL: " + sql)
    return [line.split("|") for line in result.stdout.strip().splitlines() if line]


def sql_one(sql):
    rows = psql(sql)
    if not rows or not rows[0]:
        return None
    return rows[0][0] if rows[0][0] != "" else None


def check(name, condition, detail=""):
    global PASSED, FAILED
    if condition:
        PASSED += 1
        print("[PASS] " + name)
    else:
        FAILED += 1
        FAILS.append(name)
        print("[FAIL] " + name)
    if detail:
        print("       " + str(detail))
    return bool(condition)


def step(title):
    print()
    print("-" * 78)
    print(title)
    print("-" * 78)


def ok_call(name, method, path, token=None, body=None):
    status, payload = api(method, path, token, body)
    data = payload.get("data")
    good = status == 200 and payload.get("code") == 0
    check(name, good,
          f"{method} {path} → HTTP {status} code={payload.get('code')} "
          f"msg={payload.get('msg') or payload.get('message') or ''}")
    return data if good else None


# ==================== 登录 ====================

def login(username, password, attempts=2):
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


def login_and_settle(username, candidates):
    last = None
    for candidate in candidates:
        status, payload = login(username, candidate)
        last = (status, payload)
        if status != 200 or payload.get("code") != 0:
            continue
        data = payload["data"]
        token = data["access_token"]
        if data.get("must_change_password"):
            fresh = candidate + "-t3"
            status, payload = api("PUT", "/system/auth/password", token,
                                  {"old_password": candidate, "new_password": fresh})
            if status != 200 or payload.get("code") != 0:
                return token, candidate
            status, payload = login(username, fresh)
            if status != 200 or payload.get("code") != 0:
                raise SystemExit(f"{username} 改密后重登失败：{status} {payload}")
            return payload["data"]["access_token"], fresh
        return token, candidate
    raise SystemExit(f"{username} 登录失败：{last}")


def admin_session():
    initial = ENV.get("NIK_ADMIN_PASSWORD", "")
    candidates = [initial, initial + "-t3", initial + "-e2e", initial + "-d6a"]
    token, password = login_and_settle("admin", [c for c in candidates if c])
    check("0.1 admin 登录（含首登改密兜底）", bool(token), f"生效密码长度={len(password)}")
    return token


# ==================== 扫描：现存卡住的实例（只读） ====================

STUCK_SCAN_SQL = """
WITH pending AS (
    SELECT i.id, i.instance_no, i.title, i.flow_code, i.status, i.started_at, i.updated_at,
           (SELECT count(*) FROM biz_flow_task t
             WHERE t.instance_id = i.id AND t.status = 'PENDING') AS pending_tasks,
           (SELECT count(*) FROM biz_flow_instance_node n
             WHERE n.instance_id = i.id AND n.status = 'ACTIVE') AS active_nodes
      FROM biz_flow_instance i
     WHERE i.deleted_at IS NULL AND i.status = 'PENDING'
)
SELECT instance_no || '|' || flow_code || '|' || title || '|' || pending_tasks || '|'
       || active_nodes || '|' || to_char(started_at, 'YYYY-MM-DD HH24:MI') || '|'
       || CASE
            WHEN pending_tasks = 0 THEN '无任何待办（没人能推进）'
            WHEN active_nodes = 0 THEN '没有活动节点（引擎不会再动）'
            WHEN started_at < now() - interval '7 days' THEN '挂了 7 天以上'
            ELSE '正常在跑'
          END
  FROM pending
 WHERE pending_tasks = 0 OR active_nodes = 0 OR started_at < now() - interval '7 days'
 ORDER BY started_at
"""


def scan_stuck():
    step("③ 只读扫描：现存「卡住的实例」（卡面第三条：汇报 + 可人工清理）")
    rows = psql(STUCK_SCAN_SQL)
    stuck = [row for row in rows if not row[0].endswith("|正常在跑")]
    total = sql_one("SELECT count(*) FROM biz_flow_instance "
                    "WHERE deleted_at IS NULL AND status = 'PENDING'")
    print(f"       实例总数（PENDING）= {total}")
    print(f"       可疑卡住 = {len(stuck)}")
    for row in stuck[:20]:
        print("       · " + row[0])
    if len(stuck) > 20:
        print(f"       … 另有 {len(stuck) - 20} 条")
    check("③.1 扫描可执行（只读，无写入）", True,
          "口径：PENDING 且（无 PENDING 待办 / 无 ACTIVE 节点 / 超过 7 天未结束）")
    return stuck


# ==================== 夹具 ====================

def ensure_user(admin, username, real_name, role_ids, status=1):
    user_id = sql_one(f"SELECT id FROM sys_user WHERE username = '{username}' AND deleted_at IS NULL")
    if not user_id:
        created = ok_call(f"建夹具账号 {username}", "POST", "/system/user", admin, {
            "username": username,
            "password": FIXTURE_PASSWORD,
            "real_name": real_name,
            "status": status,
            "role_ids": role_ids,
            "remark": "E2E01-FIX-3 终止验收夹具（脚本自动清理）",
        })
        if created:
            user_id = created.get("id")
        if not user_id:
            user_id = sql_one(f"SELECT id FROM sys_user WHERE username = '{username}'")
    return user_id


def ensure_fixtures(admin):
    super_role = sql_one("SELECT id FROM sys_role WHERE role_code = 'SUPER_ADMIN' AND deleted_at IS NULL")
    check("1.1 取到 SUPER_ADMIN 角色（会签人要能点同意）", bool(super_role), f"role_id={super_role}")
    users = {}
    for key, name in (("a", "会签甲"), ("b", "会签乙"), ("c", "会签丙")):
        username = f"term3_{key}_{SUFFIX}"
        users[key] = ensure_user(admin, username, f"T3 {name}", [super_role] if super_role else [])
        print(f"       夹具 {username} → {users[key]}")
    # 无任何角色的账号（用来证明「没权限 → 403」）
    users["plain"] = ensure_user(admin, f"term3_plain_{SUFFIX}", "T3 无权账号", [])
    check("1.2 四个夹具账号就位", all(users.values()), f"ids={users}")
    return users


# ==================== 流程定义 ====================

def build_flow(admin, users):
    step("① 造「3 人会签、只有 1 人在职」的卡死场景")
    created = ok_call("2.1 新建流程定义", "POST", "/business/flow/def", admin, {
        "flow_code": FLOW_CODE,
        "flow_name": "T3 会签缺人终止验收",
        "biz_type": BIZ_TYPE,
        "description": "E2E01-FIX-3 脚本自动创建/清理",
    })
    if not created:
        raise SystemExit("建流程失败，无法继续")
    def_id, version_id = created["id"], created["version_id"]
    nodes = [
        {"key": "s", "name": "开始", "type": "START", "x": 0, "y": 0},
        {"key": "n_cs", "name": "三人会签", "type": "COUNTERSIGN", "x": 200, "y": 0,
         "assignee_type": "USER", "approval_mode": "ALL", "reject_veto": True,
         "self_approval": "ALLOW",
         "assignees": [{"type": "USER", "ref_id": users["a"]},
                       {"type": "USER", "ref_id": users["b"]},
                       {"type": "USER", "ref_id": users["c"]}]},
        {"key": "e", "name": "结束", "type": "END", "x": 400, "y": 0},
    ]
    edges = [
        {"key": "e1", "from": "s", "to": "n_cs"},
        {"key": "e2", "from": "n_cs", "to": "e"},
    ]
    graph = ok_call("2.2 保存整图（START → 三人会签(ALL) → END）", "PUT",
                    f"/business/flow/version/{version_id}/graph", admin,
                    {"nodes": nodes, "edges": edges, "change_note": "E2E01-FIX-3 会签缺人"})
    if graph:
        check("2.3 图落地 3 节点 / 2 边 / 无告警",
              graph.get("node_count") == 3 and graph.get("edge_count") == 2 and not graph.get("warnings"),
              f"nodes={graph.get('node_count')} edges={graph.get('edge_count')} "
              f"warnings={graph.get('warnings')}")
    published = ok_call("2.4 发布流程", "POST", f"/business/flow/def/{def_id}/publish", admin, {})
    check("2.5 发布返回 64 位 graph_hash",
          bool(published) and len(published.get("graph_hash") or "") == 64,
          f"hash={(published or {}).get('graph_hash', '')[:16]}…")
    return def_id, version_id


# ==================== 待办 ====================

def my_todo(token, instance_id):
    status, payload = api("GET", "/business/flow/task/todo?page=1&page_size=100", token)
    if status != 200 or payload.get("code") != 0:
        return None
    for item in payload["data"]["list"]:
        if item.get("instance_id") == instance_id:
            return item
    return None


def instance_status(instance_id):
    return sql_one(f"SELECT status FROM biz_flow_instance WHERE id = '{instance_id}'::uuid")


def main():
    parser = argparse.ArgumentParser(description="E2E01-FIX-3 审批实例终止验收")
    for key in DEFAULTS:
        parser.add_argument("--" + key.replace("_", "-"), default=DEFAULTS[key])
    args = parser.parse_args()
    for key in DEFAULTS:
        CFG[key] = getattr(args, key)

    ENV.update(load_env_file(CFG["env_file"]))
    print("=" * 78)
    print("E2E01-FIX-3 审批实例人工终止 · 真机验收")
    print(f"接口={CFG['base']}  库={db_conf()['name']}  环境文件={CFG['env_file']}")
    print("=" * 78)

    admin = admin_session()

    scan_stuck()

    users = ensure_fixtures(admin)
    tokens = {}
    for key in ("a", "b", "c"):
        tokens[key] = login_and_settle(f"term3_{key}_{SUFFIX}",
                                       [FIXTURE_PASSWORD, FIXTURE_PASSWORD + "-t3"])[0]
    check("1.3 三个会签人可登录", all(tokens.values()), f"tokens={list(tokens)}")
    plain_token, _ = login_and_settle(f"term3_plain_{SUFFIX}",
                                      [FIXTURE_PASSWORD, FIXTURE_PASSWORD + "-t3"])
    check("1.4 无权账号可登录（用于 403 断言）", bool(plain_token))

    def_id, version_id = build_flow(admin, users)

    # ---------- 发起 ----------
    step("① 发起 → 1 人会签 → 卡在 1/3")
    created = ok_call("3.1 发起审批单", "POST", "/business/flow/instance", tokens["a"], {
        "def_id": def_id,
        "title": f"T3 会签缺人 {SUFFIX}",
        "form_data": {},
    })
    if not created:
        raise SystemExit("发起失败，无法继续")
    instance_id = created["instance_id"]
    check("3.2 发起后实例 PENDING", created.get("instance_status") == "PENDING",
          f"status={created.get('instance_status')} id={instance_id}")
    tasks_total = sql_one(f"SELECT count(*) FROM biz_flow_task WHERE instance_id = '{instance_id}'::uuid")
    check("3.3 会签应到 3 票（3 条 PENDING 待办）", tasks_total == "3", f"task_count={tasks_total}")

    todo = my_todo(tokens["a"], instance_id)
    check("3.4 会签甲拿到待办", bool(todo), f"task_id={(todo or {}).get('id')}")
    if todo:
        ok_call("3.5 会签甲同意（只签 1 票）", "POST",
                f"/business/flow/task/{todo['id']}/approve", tokens["a"], {"comment": "先签一票"})
    check("3.6 实例仍 PENDING（1/3，永远批不完）", instance_status(instance_id) == "PENDING",
          f"status={instance_status(instance_id)}")

    # ---------- 两人停用：真「缺人」 ----------
    for key in ("b", "c"):
        status, payload = api("PATCH", f"/system/user/{users[key]}/status", admin, {"status": 0})
        check(f"3.7 停用会签人 {key}（模拟离职，再也签不了）",
              status == 200 and payload.get("code") == 0, f"HTTP {status}")

    # ---------- 撤回被拦（现状痛点） ----------
    status, payload = api("POST", f"/business/flow/instance/{instance_id}/withdraw", tokens["a"],
                          {"reason": "想撤回重来"})
    check("4.1 有 1 票后撤回被 409/65023 拦下（这就是本卡的由来）",
          status == 409 and payload.get("code") == 65023,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")

    # ---------- 无权限 → 403 ----------
    status, payload = api("POST", f"/business/flow/instance/{instance_id}/terminate", plain_token,
                          {"reason": "越权尝试"})
    check("5.1 无 business:flow:admin 的账号终止 → 403",
          status == 403, f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")
    check("5.2 越权尝试没有改动实例", instance_status(instance_id) == "PENDING",
          f"status={instance_status(instance_id)}")

    # ---------- 原因必填 ----------
    status, payload = api("POST", f"/business/flow/instance/{instance_id}/terminate", admin,
                          {"reason": "   "})
    check("5.3 空原因 → 400 + 65005", status == 400 and payload.get("code") == 65005,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")

    # ---------- 正常终止 ----------
    step("② 管理员终止 → 实例 CANCELLED + 待办全撤 + 通知发起人与已审人")
    data = ok_call("6.1 终止审批单", "POST", f"/business/flow/instance/{instance_id}/terminate",
                   admin, {"reason": TERMINATE_REASON})
    check("6.2 返回实例状态 CANCELLED", (data or {}).get("instance_status") == "CANCELLED",
          f"instance_status={(data or {}).get('instance_status')}")
    check("6.3 库里实例 = CANCELLED", instance_status(instance_id) == "CANCELLED",
          f"db_status={instance_status(instance_id)}")
    row = psql(f"SELECT (ended_at IS NOT NULL)::text || '|' || coalesce(result_comment,'') "
               f"FROM biz_flow_instance WHERE id = '{instance_id}'::uuid")[0]
    check("6.4 ended_at 已写（终态约束）", row[0] == "true", f"has_end={row[0]}")
    check("6.5 终止原因落进 result_comment", row[1] == TERMINATE_REASON, f"result_comment={row[1]}")
    left_pending = sql_one(f"SELECT count(*) FROM biz_flow_task "
                           f"WHERE instance_id = '{instance_id}'::uuid AND status = 'PENDING'")
    check("6.6 未处理待办全部撤销（PENDING = 0）", left_pending == "0", f"pending={left_pending}")
    kept = sql_one(f"SELECT count(*) FROM biz_flow_task "
                   f"WHERE instance_id = '{instance_id}'::uuid AND status = 'APPROVED'")
    check("6.7 已投过的票保留（审计要能回溯谁批过）", kept == "1", f"approved={kept}")
    node_left = sql_one(f"SELECT count(*) FROM biz_flow_instance_node "
                        f"WHERE instance_id = '{instance_id}'::uuid AND status = 'ACTIVE'")
    check("6.8 不留「处理中」的孤儿节点", node_left == "0", f"active_nodes={node_left}")

    # ---------- 通知 ----------
    time.sleep(2)
    receivers = sql_one(
        "SELECT count(DISTINCT r.receiver_id) FROM sys_notice n "
        "JOIN sys_notice_receiver r ON r.notice_id = n.id "
        f"WHERE n.biz_id = '{instance_id}' AND n.biz_type = 'flow'")
    check("6.9 通知已发（收件人 ≥ 2：发起人 + 已审批过的人）",
          (receivers or "0").isdigit() and int(receivers) >= 2, f"receivers={receivers}")

    # ---------- 操作日志 ----------
    time.sleep(2)
    log = psql("SELECT coalesce(operation,'') || '|' || coalesce(username,'') || '|' || "
               "coalesce(request_params::text,'') FROM operation_logs "
               f"WHERE target_id = '{instance_id}' AND request_path LIKE '%/terminate' "
               "ORDER BY created_at DESC LIMIT 1")
    log_ok = bool(log) and log[0][0] == "终止审批实例" and log[0][1] == "admin" \
        and TERMINATE_REASON in log[0][2]
    check("④ 操作日志有记录（操作名 / 操作人 / 原因）", log_ok,
          f"row={log[0] if log else None}")

    # ---------- 已结束 → 400 ----------
    status, payload = api("POST", f"/business/flow/instance/{instance_id}/terminate", admin,
                          {"reason": "再点一次"})
    check("⑥ 已结束实例再终止 → 400 + 65024",
          status == 400 and payload.get("code") == 65024,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg')}")

    # ---------- 业务单据联动 ----------
    step("③ 关联单据联动：采购订单审批单被终止 → 单据回 DRAFT")
    supplier = sql_one("SELECT id FROM biz_supplier WHERE deleted_at IS NULL LIMIT 1")
    warehouse = sql_one("SELECT id FROM biz_warehouse WHERE deleted_at IS NULL LIMIT 1")
    material = sql_one("SELECT id FROM biz_material WHERE deleted_at IS NULL AND status = 1 LIMIT 1")
    check("7.1 主数据可用（供应商 / 仓库 / 物料）",
          bool(supplier and warehouse and material),
          f"supplier={supplier} warehouse={warehouse} material={material}")
    po = None
    po_inst = None
    if supplier and warehouse and material:
        po = ok_call("7.2 建采购订单（DRAFT）", "POST", "/business/erp/purchase-order", admin, {
            "supplier_id": supplier,
            "warehouse_id": warehouse,
            "purchase_type": "SPOT",
            "remark": f"E2E01-FIX-3 终止联动夹具 {SUFFIX}",
            "items": [{"material_id": material, "qty": "1", "price_excl": "10", "tax_rate": "13"}],
        })
    if po:
        po_id = po["id"]
        check("7.3 建单后是 DRAFT", (po.get("base") or po).get("status") == "DRAFT",
              f"status={(po.get('base') or po).get('status')}")
        submitted = ok_call("7.4 提交审批（DRAFT → SUBMITTED）", "POST",
                            f"/business/erp/purchase-order/{po_id}/submit-approval", admin, {})
        po_inst = sql_one("SELECT id FROM biz_flow_instance "
                          f"WHERE biz_type = 'PO_APPROVE' AND biz_id = '{po_id}'::uuid "
                          "AND status = 'PENDING' LIMIT 1")
        po_status_now = sql_one(f"SELECT status FROM biz_purchase_order WHERE id = '{po_id}'::uuid")
        check("7.5 采购单进入审批中并生成审批实例",
              bool(po_inst) and po_status_now == "SUBMITTED",
              f"instance={po_inst} po_status={po_status_now} submitted={bool(submitted)}")
        if po_inst:
            ok_call("7.6 终止采购审批实例", "POST",
                    f"/business/flow/instance/{po_inst}/terminate", admin,
                    {"reason": "供应商取消合作，人工终止采购审批"})
            check("7.7 审批实例 = CANCELLED", instance_status(po_inst) == "CANCELLED",
                  f"status={instance_status(po_inst)}")
            po_status = sql_one(f"SELECT status FROM biz_purchase_order WHERE id = '{po_id}'::uuid")
            check("7.8 ★ 关联采购单退回 DRAFT（不是停在 SUBMITTED / PENDING_*）",
                  po_status == "DRAFT", f"po_status={po_status}")
            check("7.9 采购单上没有残留实例指针",
                  sql_one(f"SELECT coalesce(approval_instance_id::text,'-') FROM biz_purchase_order "
                          f"WHERE id = '{po_id}'::uuid") in ("-", ""),
                  "approval_instance_id 已清空")
            leftovers = sql_one("SELECT count(*) FROM biz_flow_task "
                               f"WHERE instance_id = '{po_inst}'::uuid AND status = 'PENDING'")
            check("7.10 采购审批单的待办也被撤", leftovers == "0", f"pending={leftovers}")

    # ---------- 收尾 ----------
    step("清理（按外键顺序硬删本脚本造的行）")
    defs = [(def_id, version_id)]
    instances = [instance_id]
    if po_inst:
        instances.append(po_inst)

    for inst in instances:
        psql(f"DELETE FROM sys_notice_receiver WHERE notice_id IN "
             f"(SELECT id FROM sys_notice WHERE biz_id = '{inst}')")
        psql(f"DELETE FROM sys_notice WHERE biz_id = '{inst}'")
        psql(f"DELETE FROM operation_logs WHERE target_id = '{inst}'")
    for inst in instances:
        psql(f"DELETE FROM biz_flow_instance WHERE id = '{inst}'::uuid")
    if po and po.get("id"):
        psql(f"DELETE FROM biz_purchase_order_item WHERE order_id = '{po['id']}'::uuid")
        psql(f"DELETE FROM biz_purchase_order WHERE id = '{po['id']}'::uuid")
    for d, v in defs:
        psql(f"DELETE FROM biz_flow_node_assignee WHERE node_id IN "
             f"(SELECT id FROM biz_flow_node WHERE version_id = '{v}'::uuid)")
        psql(f"DELETE FROM biz_flow_edge WHERE version_id = '{v}'::uuid")
        psql(f"DELETE FROM biz_flow_node WHERE version_id = '{v}'::uuid")
        psql(f"DELETE FROM biz_flow_def_version WHERE id = '{v}'::uuid")
        psql(f"DELETE FROM biz_flow_def WHERE id = '{d}'::uuid")
    for uid in users.values():
        if uid:
            psql(f"DELETE FROM sys_user_role WHERE user_id = '{uid}'::uuid")
            psql(f"DELETE FROM sys_user WHERE id = '{uid}'::uuid")

    residual = sql_one("SELECT count(*) FROM biz_flow_instance "
                       f"WHERE title = 'T3 会签缺人 {SUFFIX}'")
    check("8.1 零残留：本脚本的实例已删净",
          residual in ("0", None), f"residual_instances={residual}")
    check("8.2 零残留：夹具账号已删净",
          sql_one(f"SELECT count(*) FROM sys_user WHERE username LIKE 'term3\\_%\\_{SUFFIX}'") == "0",
          "term3_* 账号 0 行")
    check("8.3 零残留：流程定义已删净",
          sql_one(f"SELECT count(*) FROM biz_flow_def WHERE flow_code = '{FLOW_CODE}'") == "0",
          f"flow_code={FLOW_CODE}")

    print()
    print("=" * 78)
    print(f"PASS={PASSED} FAIL={FAILED}")
    if FAILS:
        for name in FAILS:
            print("  FAILED: " + name)
    print("=" * 78)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
