#!/usr/bin/env python3
"""批次 C+ · T5「AI 执行者规则」真机验收脚本（真 HTTP + 真库断言，可重复执行）。

用法（在 144 上执行；服务 127.0.0.1:18081，库 cloudpivot_t5v）：
    python3 t5_e2e.py all        # 夹具 + 断言 + 清理
    python3 t5_e2e.py seed       # 只造夹具
    python3 t5_e2e.py clean      # 只清理

覆盖卡面验收：
  1  AI 账号登录网页 → 403 + 63201（+ auth_logs 留痕）
  2  人类账号登录不受影响
  3  AI token DELETE /business/task/{id} → 403 + 63202
  4  AI token PUT 数据权限接口 → 403 + 63203
  5  AI 无分派权限码 → 分派 403（且**不是** 632xx 硬拒）
  6  授予带 business:task:assign 的角色 → 分派成功（修正 11）
  7  AI 即使有 scope:write 权限码，改数据权限仍 403 + 63203（硬拒）
  8  AI 写操作 → changelog operator_type='ROBOT'
  9  人类写操作 → changelog operator_type='HUMAN'
  8c changelog 查询接口返回 operator_type

夹具策略：
  · 角色 / 授权 / sys_ai_agent：**直接 SQL**（固定 UUID，幂等）；
  · 用户：走真实 `POST /system/user`（密码哈希只有服务端会算），**建号时就带角色**
    —— 权限码在登录时从库里装载并进 Redis 缓存（`perm:user_perms:*`），
    建号后再补角色会让旧缓存生效，验收里就会出现「明明授了却没生效」的假失败；
  · 任务：走真实 `POST /business/task`，但**由 admin 创建**（owner=admin）：
    这样 AI 账号是任务的外人，验收 5 才能测到「只靠权限码」那条路径
    （若 AI 是创建人/负责人，分派本来就允许，测不出权限码）。
  · AI token 的取得方式：T5 **不实现** Key / 任务令牌（T5b 才做），所以先用 HUMAN
    身份登录拿到合法 JWT（含真实 claims），再把该账号的 `user_type` 翻成 ROBOT ——
    这正好也是「AI 账号持有合法令牌」的等价形态，四个硬拒判定的依据都是库里的
    `user_type`。
"""

import json
import os
import subprocess as sp
import sys
import time
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18081/api"
DB = "cloudpivot_t5v"
ENVF = "/etc/cloudpivot/cloudpivot-test.env"

# ---- 固定夹具 UUID（t5- 段）----
ROLE_EXEC = "5e000000-0000-0000-0000-00000000c001"      # 执行员：无分派 / 无删 / 无 scope:write
ROLE_ASSIGNER = "5e000000-0000-0000-0000-00000000c002"  # 主管：有分派 + 删 + scope:write

ROBOT_USER = "t5_robot"   # 只有 ROLE_EXEC
ROBOT2_USER = "t5_exec"   # ROLE_EXEC + ROLE_ASSIGNER
ROBOT_PWD = "T5robot#2026"
ADMIN_PWD = None

PASSED = 0
FAILED = 0


# ==================== 基础工具 ====================

def env(key):
    for line in open(ENVF, encoding="utf-8"):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    raise SystemExit("缺少环境变量 " + key)


def call(method, path, token=None, body=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read().decode())
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def psql(sql):
    result = sp.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-At", "-F", "|", "-c", sql],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": env("ALUN_DATABASE_PASSWORD")},
    )
    if result.returncode != 0:
        raise SystemExit(f"psql 失败：{result.stderr}")
    return [line.split("|") for line in result.stdout.strip().splitlines() if line]


def sql_one(sql):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] and rows[0][0] != "" else None


def check(name, condition, detail):
    global PASSED, FAILED
    if condition:
        PASSED += 1
        print(f"[PASS] {name}")
    else:
        FAILED += 1
        print(f"[FAIL] {name}")
    print(f"       {detail}")
    return condition


def section(title):
    print("\n" + "=" * 78)
    print(title)
    print("=" * 78)


# ==================== 登录 ====================

def login(username, password):
    return call("POST", "/system/auth/login", body={"username": username, "password": password})


def login_or_change(username, password, new_password):
    """登录；若底座要求首登改密则改密后重登。返回 (token 或 None, 最后一次响应)。"""
    status, body = login(username, password)
    if status == 200 and body.get("code") == 0:
        if body["data"].get("must_change_password"):
            token = body["data"]["access_token"]
            changed = call(
                "PUT",
                "/system/auth/password",
                token,
                {"old_password": password, "new_password": new_password},
            )
            print(f"[{username}] 首登强制改密 → HTTP {changed[0]} code={changed[1].get('code')}")
            status, body = login(username, new_password)
        if status == 200 and body.get("code") == 0:
            return body["data"]["access_token"], (status, body)
    return None, (status, body)


def admin_token():
    """admin 登录：首次是 ALUN_ADMIN_PASSWORD（会被强制改密），之后是改后的口令。"""
    global ADMIN_PWD
    initial = env("ALUN_ADMIN_PASSWORD")
    final = initial + "-t5"
    last = None
    for candidate in (initial, final):
        token, response = login_or_change("admin", candidate, final)
        last = response
        if token is not None:
            # 无论这次是否触发改密，admin 的可用口令最终都是 final
            ADMIN_PWD = final
            print("[admin] 登录成功（人类账号，验收 2 的对照组）")
            return token
    raise SystemExit(f"admin 登录失败：{last}")


# ==================== 夹具 ====================

TASK_TITLE = "T5-AI执行者验收任务"


def clean():
    ids = "SELECT id FROM biz_task WHERE title LIKE 'T5-%'"
    psql("DELETE FROM sys_change_log WHERE resource = 'biz_task' "
         "  AND resource_id IN (SELECT id::text FROM biz_task WHERE title LIKE 'T5-%')")
    for table in (
        "biz_task_comment",
        "biz_task_member",
        "biz_task_worklog",
        "biz_task_dependency",
        "biz_task_attachment",
    ):
        psql(f"DELETE FROM {table} WHERE task_id IN ({ids})")
    psql("DELETE FROM biz_task WHERE title LIKE 'T5-%'")
    psql(f"DELETE FROM sys_user_role WHERE role_id IN ('{ROLE_EXEC}'::uuid, '{ROLE_ASSIGNER}'::uuid)")
    psql(f"DELETE FROM sys_role_menu WHERE role_id IN ('{ROLE_EXEC}'::uuid, '{ROLE_ASSIGNER}'::uuid)")
    psql(f"DELETE FROM sys_role WHERE id IN ('{ROLE_EXEC}'::uuid, '{ROLE_ASSIGNER}'::uuid)")
    names = f"('{ROBOT_USER}', '{ROBOT2_USER}')"
    psql(f"DELETE FROM sys_ai_agent WHERE user_id IN (SELECT id FROM sys_user WHERE username IN {names})")
    psql(f"DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username IN {names})")
    psql(f"DELETE FROM sys_user WHERE username IN {names}")
    print("[clean] 夹具已清理（只删 t5- 段）")


def menu_ids(codes):
    quoted = ",".join("'" + c + "'" for c in codes)
    rows = psql(f"SELECT DISTINCT id FROM sys_menu WHERE auth_code IN ({quoted})")
    return [row[0] for row in rows]


EXEC_CODES = [
    "business:task:read",
    "business:task:create",
    "business:task:update",
    "business:task:status",
]
ASSIGNER_CODES = EXEC_CODES + [
    "business:task:assign",
    "business:task:delete",
    "business:permission:scope:write",
]


def seed_roles():
    for role_id, code, name, codes in [
        (ROLE_EXEC, "T5_EXEC", "T5 AI 执行员（无分派权）", EXEC_CODES),
        (ROLE_ASSIGNER, "T5_ASSIGNER", "T5 AI 主管（可分派 + 可删 + 数据权限写）", ASSIGNER_CODES),
    ]:
        psql(
            f"INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, data_scope) "
            f"VALUES ('{role_id}'::uuid, '{code}', '{name}', 'T5 验收夹具', 1, 900, 1) "
            f"ON CONFLICT (id) DO UPDATE SET role_name = EXCLUDED.role_name, "
            f"  data_scope = EXCLUDED.data_scope, deleted_at = NULL"
        )
        ids = menu_ids(codes)
        if not ids:
            raise SystemExit(f"权限码没有对应的 sys_menu 节点：{codes}")
        values = ",".join(f"('{role_id}'::uuid, '{mid}'::uuid)" for mid in ids)
        psql(f"INSERT INTO sys_role_menu (role_id, menu_id) VALUES {values} ON CONFLICT DO NOTHING")
        print(f"[seed] 角色 {code} ← {len(ids)} 个权限节点 {codes}")


def create_user(admin, username, real_name, role_ids):
    status, body = call(
        "POST",
        "/system/user",
        admin,
        {
            "username": username,
            "password": ROBOT_PWD,
            "real_name": real_name,
            "remark": "T5 验收夹具（AI 执行者）",
            "role_ids": role_ids,
        },
    )
    if status != 200 or body.get("code") != 0:
        raise SystemExit(f"建用户 {username} 失败：{status} {body}")
    user_id = sql_one(f"SELECT id FROM sys_user WHERE username = '{username}'")
    print(f"[seed] POST /system/user → {username} ({user_id}) 已创建，角色 {role_ids}（user_type=HUMAN）")
    return user_id


def seed():
    clean()
    seed_roles()
    admin = admin_token()
    create_user(admin, ROBOT_USER, "T5 机器人（无分派权）", [ROLE_EXEC])
    create_user(admin, ROBOT2_USER, "T5 机器人（有分派权）", [ROLE_EXEC, ROLE_ASSIGNER])
    return admin


def flip_to_robot(user_id, profile):
    """把账号翻成 ROBOT 并补 sys_ai_agent 行（满足修正 1 的不变式）。"""
    psql(f"UPDATE sys_user SET user_type = 'ROBOT' WHERE id = '{user_id}'::uuid")
    psql(
        "INSERT INTO sys_ai_agent (user_id, hermes_profile, workspace_path, soul_md, agent_description) "
        f"VALUES ('{user_id}'::uuid, '{profile}', '/tmp/{profile}-ws/', '# {profile}', 'T5 验收夹具') "
        "ON CONFLICT (user_id) DO UPDATE SET deleted_at = NULL"
    )
    print(f"[fixture] {profile}: user_type → ROBOT，已补 sys_ai_agent（不变式成立）")


# ==================== 断言 ====================

def run_all():
    admin = seed()
    robot_id = sql_one(f"SELECT id FROM sys_user WHERE username = '{ROBOT_USER}'")
    exec_id = sql_one(f"SELECT id FROM sys_user WHERE username = '{ROBOT2_USER}'")
    admin_id = sql_one("SELECT id FROM sys_user WHERE username = 'admin'")

    section("准备：AI 账号的合法令牌（T5 不实现 Key/任务令牌 → 先以 HUMAN 登录取合法 JWT）")
    tok_noperm, (st, bd) = login_or_change(ROBOT_USER, ROBOT_PWD, ROBOT_PWD)
    if tok_noperm is None:
        raise SystemExit(f"{ROBOT_USER} 登录失败：{st} {bd}")
    print(f"[token] tok_noperm = {ROBOT_USER}（角色 T5_EXEC：无 assign / 无 delete / 无 scope:write）")

    tok_perm, (st, bd) = login_or_change(ROBOT2_USER, ROBOT_PWD, ROBOT_PWD)
    if tok_perm is None:
        raise SystemExit(f"{ROBOT2_USER} 登录失败：{st} {bd}")
    print(f"[token] tok_perm   = {ROBOT2_USER}（角色 T5_EXEC + T5_ASSIGNER）")

    # 确认 claims 里真有那三个权限码（否则后面的 403 是「没授权」而不是「硬拒」）
    st, bd = call("GET", "/system/auth/me", tok_perm)
    perms = set((bd.get("data") or {}).get("permissions") or [])
    check(
        "准备 tok_perm 的 claims 含 assign / delete / scope:write",
        {"business:task:assign", "business:task:delete", "business:permission:scope:write"} <= perms,
        f"HTTP {st} permissions={sorted(perms)}",
    )

    flip_to_robot(robot_id, "t5-robot")
    flip_to_robot(exec_id, "t5-exec")

    # ---- 验收 1：AI 登录网页 → 403 + 63201 + auth_logs ----
    section("验收 1：AI 账号尝试登录网页")
    st, bd = login(ROBOT_USER, ROBOT_PWD)
    check(
        "1a AI 账号用**正确密码**登录 → 403 + 63201",
        st == 403 and bd.get("code") == 63201,
        f"HTTP {st} body={json.dumps(bd, ensure_ascii=False)}",
    )
    time.sleep(2)  # 认证日志是异步总线投递，给落库留时间（卡面要求「必须记 auth_logs」）
    rows = psql(
        "SELECT username, auth_status, error_message, to_char(created_at, 'HH24:MI:SS') "
        f"FROM auth_logs WHERE username = '{ROBOT_USER}' ORDER BY created_at DESC LIMIT 3"
    )
    check(
        "1b auth_logs 有 FAIL 行（error_message=AI 用户禁止网页登录）",
        any(r[1] == "FAIL" and r[2] == "AI 用户禁止网页登录" for r in rows),
        f"auth_logs 最近 3 条：{rows}",
    )

    # ---- 验收 2：人类登录不受影响 ----
    section("验收 2：人类账号登录")
    st, bd = login("admin", ADMIN_PWD)
    check(
        "2 人类账号登录成功",
        st == 200 and bd.get("code") == 0,
        f"HTTP {st} code={bd.get('code')} msg={bd.get('msg')}",
    )

    # 任务由 admin 创建（owner=admin）→ AI 是「外人」，验收 5 才能测到纯权限码路径
    st, bd = call("POST", "/business/task", admin, {"title": TASK_TITLE})
    if st != 200 or bd.get("code") != 0:
        raise SystemExit(f"建任务失败：{st} {bd}")
    task_id = bd["data"]["id"]
    print(f"[fixture] 任务 {task_id} 由 admin 创建（owner=admin）")

    # ---- 验收 5：AI 无分派权限码 → 403（非 632xx）----
    section("验收 5：AI 账号（T5_EXEC，无 business:task:assign）分派")
    st, bd = call("PATCH", f"/business/task/{task_id}/assign", tok_noperm, {"owner_id": robot_id})
    check(
        "5 无权限码 → 403（普通权限拒绝，不是 632xx 硬拒）",
        st == 403 and bd.get("code") not in (63201, 63202, 63203, 63204),
        f"HTTP {st} body={json.dumps(bd, ensure_ascii=False)}",
    )

    # ---- 验收 6：AI 有 assign 权限码 → 分派成功（修正 11）----
    section("验收 6：AI 账号（T5_ASSIGNER，有 business:task:assign）分派")
    st, bd = call("PATCH", f"/business/task/{task_id}/assign", tok_perm, {"owner_id": exec_id})
    check(
        "6 有权限码 → 200 分派成功（修正 11：AI 分派不硬拒）",
        st == 200 and bd.get("code") == 0,
        f"HTTP {st} body={json.dumps(bd, ensure_ascii=False)[:220]}",
    )
    owner = sql_one(f"SELECT owner_id FROM biz_task WHERE id = '{task_id}'::uuid")
    check("6b 任务负责人已变成 t5_exec", owner == exec_id, f"owner_id={owner}")

    # ---- 验收 3：AI 删任务 → 403 + 63202 ----
    section("验收 3：AI 账号删除任务（claims 里有 business:task:delete）")
    st, bd = call("DELETE", f"/business/task/{task_id}", tok_perm)
    check(
        "3 DELETE /business/task/{id} → 403 + 63202",
        st == 403 and bd.get("code") == 63202,
        f"HTTP {st} body={json.dumps(bd, ensure_ascii=False)}",
    )
    alive = sql_one(f"SELECT (deleted_at IS NULL)::text FROM biz_task WHERE id = '{task_id}'::uuid")
    check("3b 任务没有被删（deleted_at 仍为空）", alive == "true", f"deleted_at IS NULL = {alive}")

    # ---- 验收 4 / 7：AI 改数据权限 → 403 + 63203（有 scope:write 也拒）----
    section("验收 4 + 7：AI 账号改数据权限（claims 里有 business:permission:scope:write）")
    before = sql_one(f"SELECT data_scope FROM sys_role WHERE id = '{ROLE_EXEC}'::uuid")
    st, bd = call(
        "PUT",
        f"/business/permission/scope/role/{ROLE_EXEC}",
        tok_perm,
        {"data_scope": 4, "custom_dept_ids": None},
    )
    check(
        "4/7 PUT /business/permission/scope/role/{id} → 403 + 63203",
        st == 403 and bd.get("code") == 63203,
        f"HTTP {st} body={json.dumps(bd, ensure_ascii=False)}",
    )
    after = sql_one(f"SELECT data_scope FROM sys_role WHERE id = '{ROLE_EXEC}'::uuid")
    check("7b 库内数据范围未被改动", before == after, f"before={before} after={after}")

    # ---- 验收 8：AI 写操作 → changelog operator_type='ROBOT' ----
    section("验收 8：AI 账号改任务状态（TODO → DOING）→ changelog")
    st, bd = call("PATCH", f"/business/task/{task_id}/status", tok_perm, {"to": "DOING"})
    check(
        "8a AI 改状态 → 200",
        st == 200 and bd.get("code") == 0,
        f"HTTP {st} body={json.dumps(bd, ensure_ascii=False)[:220]}",
    )
    rows = psql(
        "SELECT operator_name, operator_type, action, action_label, changed_fields::text "
        "FROM sys_change_log WHERE resource = 'biz_task' AND resource_id = '" + task_id + "' "
        "ORDER BY created_at DESC LIMIT 5"
    )
    check(
        "8b changelog 有 ROBOT 行（operator_type=ROBOT）",
        any(r[1] == "ROBOT" for r in rows),
        f"最近 5 行：{rows}",
    )

    # ---- 验收 9：人类操作 → operator_type='HUMAN' ----
    section("验收 9：人类账号改同一任务状态（DOING → DONE）→ changelog")
    st, bd = call("PATCH", f"/business/task/{task_id}/status", admin, {"to": "DONE"})
    check(
        "9a admin 改状态 → 200",
        st == 200 and bd.get("code") == 0,
        f"HTTP {st} body={json.dumps(bd, ensure_ascii=False)[:220]}",
    )
    rows = psql(
        "SELECT operator_name, operator_type FROM sys_change_log "
        "WHERE resource = 'biz_task' AND resource_id = '" + task_id + "' "
        "ORDER BY created_at DESC LIMIT 5"
    )
    check(
        "9b changelog 有 HUMAN 行（operator_name=admin, operator_type=HUMAN）",
        any(r[0] == "admin" and r[1] == "HUMAN" for r in rows),
        f"最近 5 行：{rows}",
    )

    # ---- 验收 8c：查询接口返回 operator_type ----
    section("验收 8c：changelog 查询接口返回 operator_type（前端「AI」标签的数据源）")
    st, bd = call(
        "GET", f"/business/changelog/page?resource=biz_task&resource_id={task_id}&page_size=10", admin
    )
    items = (bd.get("data") or {}).get("list") or []
    types = [(it.get("operator_name"), it.get("operator_type")) for it in items]
    check(
        "8c GET /business/changelog/page 每项都带 operator_type，且两种类型都出现",
        st == 200
        and bool(items)
        and all(it.get("operator_type") in ("HUMAN", "ROBOT") for it in items)
        and {it.get("operator_type") for it in items} == {"HUMAN", "ROBOT"},
        f"HTTP {st} items={types}",
    )

    # ---- 收尾 ----
    section("汇总")
    print(f"通过 {PASSED} 条，失败 {FAILED} 条")
    clean()
    return 0 if FAILED == 0 else 1


if __name__ == "__main__":
    action = sys.argv[1] if len(sys.argv) > 1 else "all"
    if action == "clean":
        clean()
    elif action == "seed":
        seed()
    else:
        sys.exit(run_all())
