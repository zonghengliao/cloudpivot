#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""A5 消息通知底座 —— 四条硬指标的实测脚本（只在 cloudpivot_test 上跑）。

用法（在 172.18.10.144 上）：python3 /root/work/a5/acceptance.py

做四件事：
 1. 在 cloudpivot_test 造数据：5 角色 / 20 有效用户（+1 停用 +1 软删）/ 3 层部门，
    然后逐个目标调 POST /api/business/notice，把「接口返回的接收人数」与
    「SQL 直查的接收人数」「按同一口径独立计数的 SQL」三方对比；
 2. 幂等重放 / 模板渲染 / 缺变量报错 / 未读数与全部已读 端到端验证；
 3. 灌 1 万接收行后跑 EXPLAIN (ANALYZE, BUFFERS)，证明未读数命中部分索引；
 4. 全部输出打屏，供归档为验收证据。
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
import uuid

BASE = "http://127.0.0.1:8025/api"
ENV_FILE = "/etc/cloudpivot/cloudpivot-test.env"

# ---------- 基础设施 ----------


def load_env(path):
    env = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            env[key.strip()] = value.strip()
    return env


ENV = load_env(ENV_FILE)
PG = ["psql", "-h", "127.0.0.1", "-U", ENV["ALUN_DATABASE_USER"], "-d", "cloudpivot_test",
      "-v", "ON_ERROR_STOP=1", "-tA"]


def sql(query):
    """执行 SQL，返回 (是否成功, 输出)。"""
    proc = subprocess.run(
        PG + ["-c", query],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": ENV["ALUN_DATABASE_PASSWORD"]},
    )
    out = (proc.stdout or "").strip()
    err = (proc.stderr or "").strip()
    if proc.returncode != 0:
        raise RuntimeError(f"SQL 失败: {query}\n{err}")
    return out


def sql_script(script):
    proc = subprocess.run(
        PG + ["-f", script],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": ENV["ALUN_DATABASE_PASSWORD"]},
    )
    if proc.returncode != 0:
        raise RuntimeError(f"SQL 脚本失败: {script}\n{proc.stderr}")


def api(method, path, body=None, token=None):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
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


def login(username, password):
    status, body = api("POST", "/system/auth/login", {"username": username, "password": password})
    if status != 200 or body.get("code") != 0:
        raise RuntimeError(f"登录失败: {status} {body}")
    data = body["data"]
    return data.get("access_token") or data.get("token")


def banner(title):
    print()
    print("=" * 78)
    print(title)
    print("=" * 78)


# ---------- 造数据 ----------

DEPTS = {
    "HQ": ("a5000000-0000-0000-0000-000000000001", "A5-总部", None),
    "RD": ("a5000000-0000-0000-0000-000000000002", "A5-研发部", "a5000000-0000-0000-0000-000000000001"),
    "BE": ("a5000000-0000-0000-0000-000000000003", "A5-后端组", "a5000000-0000-0000-0000-000000000002"),
    "SL": ("a5000000-0000-0000-0000-000000000004", "A5-销售部", "a5000000-0000-0000-0000-000000000001"),
}
ROLE_IDS = [f"a5c00000-0000-0000-0000-00000000000{i}" for i in range(1, 6)]
USER_IDS = [f"a5e00000-0000-0000-0000-0000000000{i:02d}" for i in range(1, 21)]
DISABLED_USER = "a5e00000-0000-0000-0000-000000000021"
DELETED_USER = "a5e00000-0000-0000-0000-000000000022"


def user_dept(index):
    """u01-u06 → 后端组(L3)；u07-u10 → 研发部(L2)；u11-u15 → 销售部(L2)；u16-u20 → 无部门。"""
    if index <= 6:
        return DEPTS["BE"][0]
    if index <= 10:
        return DEPTS["RD"][0]
    if index <= 15:
        return DEPTS["SL"][0]
    return None


def seed():
    banner("① 造数据（cloudpivot_test）：3 层部门 / 5 角色 / 20 有效用户（+1 停用 +1 软删）")
    statements = ["DELETE FROM sys_notice_receiver WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'a5\\_%')",
                  "DELETE FROM sys_notice WHERE idempotency_key LIKE 'a5-%' OR notice_no LIKE 'NTA5%'",
                  "DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'a5\\_%')",
                  "DELETE FROM sys_role_menu WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'A5\\_%')",
                  "DELETE FROM sys_role WHERE role_code LIKE 'A5\\_%'",
                  "DELETE FROM sys_user WHERE username LIKE 'a5\\_%'",
                  "DELETE FROM sys_dept WHERE dept_name LIKE 'A5-%'"]

    for name, (dept_id, dept_name, parent) in DEPTS.items():
        parent_sql = f"'{parent}'::uuid" if parent else "NULL"
        # dept_code 唯一性由部分唯一索引保证（软删后可复用），这里先物理删再插
        statements.append(
            "INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, dept_path, status, sort_order) "
            f"VALUES ('{dept_id}'::uuid, 'A5-{name}', '{dept_name}', {parent_sql}, '/{name}/', 1, 0)"
        )

    for index, role_id in enumerate(ROLE_IDS, start=1):
        statements.append(
            "INSERT INTO sys_role (id, role_code, role_name, status, sort_order) "
            f"VALUES ('{role_id}'::uuid, 'A5_ROLE_{index}', 'A5 测试角色 {index}', 1, 0)"
        )

    for index, user_id in enumerate(USER_IDS, start=1):
        dept_id = user_dept(index)
        dept_sql = f"'{dept_id}'::uuid" if dept_id else "NULL"
        statements.append(
            "INSERT INTO sys_user (id, username, password, real_name, email, mobile, dept_id, status) "
            f"VALUES ('{user_id}'::uuid, 'a5_u{index:02d}', 'x', 'A5 用户{index:02d}', "
            f"'a5_u{index:02d}@example.com', '1380000{index:04d}', {dept_sql}, 1)"
        )
    # 停用用户（在 3 层部门的叶子节点里）与软删用户（同部门），用于证明扇出会把它们排除
    statements.append(
        "INSERT INTO sys_user (id, username, password, real_name, dept_id, status) "
        f"VALUES ('{DISABLED_USER}'::uuid, 'a5_disabled', 'x', 'A5 停用用户', '{DEPTS['BE'][0]}'::uuid, 0)"
    )
    statements.append(
        "INSERT INTO sys_user (id, username, password, real_name, dept_id, status, deleted_at) "
        f"VALUES ('{DELETED_USER}'::uuid, 'a5_deleted', 'x', 'A5 软删用户', '{DEPTS['BE'][0]}'::uuid, 1, CURRENT_TIMESTAMP)"
    )

    # 角色授权：role1 → u01,u02,u03；role2 → u03,u04,u05（u03 重叠，验证 DISTINCT）；
    #          role3 → u06；role4 → u07 + u08（其中 u08 的授权已过期）；role5 → 无人
    bindings = [
        (ROLE_IDS[0], USER_IDS[0], None), (ROLE_IDS[0], USER_IDS[1], None), (ROLE_IDS[0], USER_IDS[2], None),
        (ROLE_IDS[1], USER_IDS[2], None), (ROLE_IDS[1], USER_IDS[3], None), (ROLE_IDS[1], USER_IDS[4], None),
        (ROLE_IDS[2], USER_IDS[5], None),
        (ROLE_IDS[3], USER_IDS[6], None), (ROLE_IDS[3], USER_IDS[7], "CURRENT_TIMESTAMP - INTERVAL '1 day'"),
    ]
    for role_id, user_id, expire in bindings:
        expire_sql = expire if expire else "NULL"
        statements.append(
            "INSERT INTO sys_user_role (user_id, role_id, expire_time) "
            f"VALUES ('{user_id}'::uuid, '{role_id}'::uuid, {expire_sql})"
        )

    statements.append("ANALYZE sys_user")
    statements.append("ANALYZE sys_role")
    statements.append("ANALYZE sys_user_role")
    statements.append("ANALYZE sys_dept")

    with open("/tmp/a5_seed.sql", "w", encoding="utf-8") as handle:
        handle.write("BEGIN;\n" + ";\n".join(statements) + ";\nCOMMIT;\n")
    sql_script("/tmp/a5_seed.sql")

    print("部门（3 层）：总部 → 研发部 → 后端组；总部 → 销售部")
    print("  " + sql("SELECT string_agg(dept_name, ' / ' ORDER BY dept_path) FROM sys_dept WHERE dept_name LIKE 'A5-%'"))
    print("用户统计：")
    print(sql(
        "SELECT '有效用户=' || count(*) FILTER (WHERE deleted_at IS NULL AND status = 1) "
        "|| '，停用=' || count(*) FILTER (WHERE status = 0) "
        "|| '，软删=' || count(*) FILTER (WHERE deleted_at IS NOT NULL) "
        "FROM sys_user WHERE username LIKE 'a5\\_%'"
    ))
    print(sql(
        "SELECT '角色数=' || count(*) FROM sys_role WHERE role_code LIKE 'A5\\_%'"
    ))
    print("部门内有效用户数：")
    print(sql(
        "SELECT d.dept_name || ' → ' || count(u.id) "
        "FROM sys_dept d LEFT JOIN sys_user u ON u.dept_id = d.id AND u.deleted_at IS NULL AND u.status = 1 "
        "WHERE d.dept_name LIKE 'A5-%' GROUP BY d.dept_name ORDER BY d.dept_name"
    ).replace("\n", " | "))


# ---------- 期望值（独立 SQL 口径） ----------


def expected_counts():
    banner("② 期望值（与架构文档 §4.2 同口径的独立 SQL 直查）")
    checks = [
        ("USER 指定用户 u01/u02/u19（3 个有效）+ 停用 + 软删", """
            SELECT count(*) FROM sys_user
            WHERE id = ANY(ARRAY['{u1}','{u2}','{u19}','{disabled}','{deleted}']::uuid[])
              AND deleted_at IS NULL AND status = 1
        """.format(u1=USER_IDS[0], u2=USER_IDS[1], u19=USER_IDS[18],
                   disabled=DISABLED_USER, deleted=DELETED_USER)),
        ("ROLE role1+role2（含重叠 u03，期望去重后 5）", f"""
            SELECT count(DISTINCT u.id) FROM sys_user_role ur
            JOIN sys_user u ON u.id = ur.user_id
            WHERE ur.role_id = ANY(ARRAY['{ROLE_IDS[0]}','{ROLE_IDS[1]}']::uuid[])
              AND (ur.expire_time IS NULL OR ur.expire_time > CURRENT_TIMESTAMP)
              AND u.deleted_at IS NULL AND u.status = 1
        """),
        ("ROLE role4（u08 授权已过期，期望 1）", f"""
            SELECT count(DISTINCT u.id) FROM sys_user_role ur
            JOIN sys_user u ON u.id = ur.user_id
            WHERE ur.role_id = ANY(ARRAY['{ROLE_IDS[3]}']::uuid[])
              AND (ur.expire_time IS NULL OR ur.expire_time > CURRENT_TIMESTAMP)
              AND u.deleted_at IS NULL AND u.status = 1
        """),
        ("DEPT 总部（3 层子树，期望 15）", f"""
            WITH RECURSIVE dept_tree AS (
                SELECT id FROM sys_dept WHERE id = '{DEPTS['HQ'][0]}'::uuid AND deleted_at IS NULL
                UNION ALL
                SELECT c.id FROM sys_dept c JOIN dept_tree t ON c.parent_id = t.id WHERE c.deleted_at IS NULL
            )
            SELECT count(DISTINCT u.id) FROM sys_user u JOIN dept_tree t ON u.dept_id = t.id
            WHERE u.deleted_at IS NULL AND u.status = 1
        """),
        ("DEPT 后端组（叶子，期望 6）", f"""
            WITH RECURSIVE dept_tree AS (
                SELECT id FROM sys_dept WHERE id = '{DEPTS['BE'][0]}'::uuid AND deleted_at IS NULL
                UNION ALL
                SELECT c.id FROM sys_dept c JOIN dept_tree t ON c.parent_id = t.id WHERE c.deleted_at IS NULL
            )
            SELECT count(DISTINCT u.id) FROM sys_user u JOIN dept_tree t ON u.dept_id = t.id
            WHERE u.deleted_at IS NULL AND u.status = 1
        """),
        ("ALL 全员（整个库里所有有效用户，含 admin 等既有账号）", """
            SELECT count(*) FROM sys_user WHERE deleted_at IS NULL AND status = 1
        """),
    ]
    expectations = {}
    for label, query in checks:
        value = int(sql(query))
        expectations[label] = value
        print(f"  {label}: {value}")
    return expectations


# ---------- 四种目标扇出 ----------


def fanout_checks(token, expectations):
    banner("③ 四种目标逐条发送（POST /api/business/notice）并与 SQL 直查比对")
    cases = [
        ("USER 指定用户", "USER", [USER_IDS[0], USER_IDS[1], USER_IDS[18], DISABLED_USER, DELETED_USER],
         expectations["USER 指定用户 u01/u02/u19（3 个有效）+ 停用 + 软删"]),
        ("ROLE 角色 role1+role2", "ROLE", [ROLE_IDS[0], ROLE_IDS[1]],
         expectations["ROLE role1+role2（含重叠 u03，期望去重后 5）"]),
        ("ROLE 角色 role4（含过期授权）", "ROLE", [ROLE_IDS[3]],
         expectations["ROLE role4（u08 授权已过期，期望 1）"]),
        ("DEPT 总部（3 层子树）", "DEPT", [DEPTS["HQ"][0]],
         expectations["DEPT 总部（3 层子树，期望 15）"]),
        ("DEPT 后端组（叶子）", "DEPT", [DEPTS["BE"][0]],
         expectations["DEPT 后端组（叶子，期望 6）"]),
        ("ALL 全员", "ALL", [], expectations["ALL 全员（整个库里所有有效用户，含 admin 等既有账号）"]),
    ]

    failures = 0
    notice_ids = {}
    for label, target_type, target_ids, expected in cases:
        payload = {
            "notice_type": "SYSTEM",
            "level": "INFO",
            "title": f"A5 冒烟：{label}",
            "content": f"扇出验证 {label}",
            "content_format": "TEXT",
            "target_type": target_type,
            "target_ids": target_ids,
            "channel_mask": 1,
            "idempotency_key": f"a5-fanout-{target_type}-{uuid.uuid4()}",
        }
        status, body = api("POST", "/business/notice", payload, token)
        if status != 200 or body.get("code") != 0:
            print(f"  [FAIL] {label}: HTTP {status} {body}")
            failures += 1
            continue
        data = body["data"]
        notice_id = data["notice_id"]
        notice_ids[label] = notice_id
        api_count = data["receiver_count"]
        db_count = int(sql(
            f"SELECT count(*) FROM sys_notice_receiver WHERE notice_id = '{notice_id}'::uuid"))
        notice_row = sql(
            f"SELECT receiver_count || '|' || target_type || '|' || coalesce(target_ids::text,'') "
            f"FROM sys_notice WHERE id = '{notice_id}'::uuid")
        stored_count, stored_type, stored_targets = notice_row.split("|", 2)
        ok = api_count == expected == db_count == int(stored_count)
        print(f"  [{'PASS' if ok else 'FAIL'}] {label}")
        print(f"         接口返回 receiver_count={api_count} | SQL 直查 sys_notice_receiver={db_count} "
              f"| sys_notice.receiver_count={stored_count} | 期望={expected}")
        print(f"         notice_id={notice_id} target_type={stored_type} target_ids={stored_targets[:80]}")
        if not ok:
            failures += 1

    # 幂等重放
    banner("④ 幂等重放（同一 idempotency_key 再发一次）")
    key = f"a5-idem-{uuid.uuid4()}"
    payload = {
        "notice_type": "SYSTEM", "level": "INFO",
        "title": "A5 幂等验证", "content": "只应产生一条通知",
        "target_type": "ROLE", "target_ids": [ROLE_IDS[0]],
        "idempotency_key": key,
    }
    first = api("POST", "/business/notice", payload, token)[1]["data"]
    second = api("POST", "/business/notice", payload, token)[1]["data"]
    notice_total = int(sql("SELECT count(*) FROM sys_notice WHERE idempotency_key = '%s'" % key))
    receiver_total = int(sql(
        "SELECT count(*) FROM sys_notice_receiver WHERE notice_id = '%s'::uuid" % first["notice_id"]))
    ok = (first["notice_id"] == second["notice_id"] and second["duplicated"] is True
          and notice_total == 1 and first["receiver_count"] == receiver_total)
    print(f"  首次：notice_id={first['notice_id']} receiver_count={first['receiver_count']} duplicated={first['duplicated']}")
    print(f"  重放：notice_id={second['notice_id']} receiver_count={second['receiver_count']} duplicated={second['duplicated']}")
    print(f"  SQL：sys_notice 行数={notice_total}（期望 1）| 接收行数={receiver_total}（期望 {first['receiver_count']}）")
    print(f"  [{'PASS' if ok else 'FAIL'}] 幂等键重放不重复发送")
    if not ok:
        failures += 1

    return failures, notice_ids


# ---------- 模板渲染 / 未读数 ----------


def template_and_unread_checks(token):
    banner("⑤ 模板渲染（走 NOTICE_WELCOME 模板发送）与缺变量报错")
    status, body = api("POST", "/business/notice", {
        "template_code": "NOTICE_WELCOME",
        "template_vars": {"realName": "张三", "systemName": "云枢"},
        "target_type": "USER",
        "target_ids": [USER_IDS[0]],
        "idempotency_key": f"a5-tpl-{uuid.uuid4()}",
    }, token)
    ok = status == 200 and body.get("code") == 0
    if ok:
        notice_id = body["data"]["notice_id"]
        rendered = sql(f"SELECT title || '||' || content FROM sys_notice WHERE id = '{notice_id}'::uuid")
        print(f"  渲染结果：{rendered}")
        ok = "欢迎加入 云枢" in rendered and "张三" in rendered
    print(f"  [{'PASS' if ok else 'FAIL'}] 模板发送并渲染（title/content 用 {{var}} 替换）")

    status, body = api("POST", "/business/notice", {
        "template_code": "NOTICE_WELCOME",
        "template_vars": {"systemName": "云枢"},  # 故意缺 realName
        "target_type": "USER",
        "target_ids": [USER_IDS[0]],
    }, token)
    missing_ok = status == 422 and body.get("code") == 60023
    print(f"  缺变量返回：HTTP {status} code={body.get('code')} msg={body.get('msg')}")
    print(f"  [{'PASS' if missing_ok else 'FAIL'}] 缺变量报 422 + 60023（不静默留空）")

    banner("⑤b 通道预留（channel_mask=15：邮件 / Webhook / 短信未配置 → SKIPPED，不报错）")
    status, body = api("POST", "/business/notice", {
        "title": "A5 通道预留验证", "content": "四个通道位全开",
        "target_type": "ROLE", "target_ids": [ROLE_IDS[0]],
        "channel_mask": 15,
        "idempotency_key": f"a5-channel-{uuid.uuid4()}",
    }, token)
    channel_ok = status == 200 and body.get("code") == 0
    if channel_ok:
        data = body["data"]
        print("  接口返回 channels：" + json.dumps(data["channels"], ensure_ascii=False))
        print("  投递记录（SQL 直查）：")
        print("    " + sql(
            "SELECT channel || ' | ' || status || ' | rows=' || count(*) "
            f"FROM sys_notice_delivery WHERE notice_id = '{data['notice_id']}'::uuid "
            "GROUP BY channel, status ORDER BY channel").replace("\n", "\n    "))
        statuses = sql(
            f"SELECT string_agg(DISTINCT status, ',') FROM sys_notice_delivery "
            f"WHERE notice_id = '{data['notice_id']}'::uuid")
        channel_ok = statuses.strip() == "SKIPPED"
        print(f"  投递状态集合={statuses}（期望 SKIPPED —— 通道未配置不报错）")
    print(f"  [{'PASS' if channel_ok else 'FAIL'}] 未配置通道记 SKIPPED 且发送成功")

    banner("⑥ 未读数 / 全部已读（以 admin 为接收人端到端验证）")
    admin_id = sql("SELECT id FROM sys_user WHERE username = 'admin'")
    status, body = api("POST", "/business/notice", {
        "title": "A5 未读数验证", "content": "应该只有一条未读",
        "target_type": "USER", "target_ids": [admin_id],
        "idempotency_key": f"a5-unread-{uuid.uuid4()}",
    }, token)
    before = api("GET", "/business/notice/unread-count", None, token)[1]["data"]["unread_count"]
    read_all = api("PUT", "/business/notice/read-all", None, token)[1]["data"]["affected"]
    after = api("GET", "/business/notice/unread-count", None, token)[1]["data"]["unread_count"]
    print(f"  发送后未读数={before} | 全部已读 affected={read_all} | 之后未读数={after}")
    unread_ok = before >= 1 and after == 0
    print(f"  [{'PASS' if unread_ok else 'FAIL'}] 未读数与全部已读闭环")

    mine = api("GET", "/business/notice/mine?page=1&page_size=5", None, token)[1]
    print(f"  我的通知：total={mine['data']['total']} 首条标题={mine['data']['list'][0]['title'] if mine['data']['list'] else '-'}")

    return (0 if ok else 1) + (0 if missing_ok else 1) + (0 if channel_ok else 1) + (
        0 if unread_ok else 1)


# ---------- EXPLAIN：未读数走部分索引 ----------


def explain_unread():
    banner("⑦ 未读数查询计划（1 万接收行，EXPLAIN ANALYZE BUFFERS）")
    target_user = USER_IDS[0]
    statements = [
        # 400 条通知 × 库内全部有效用户 = 1 万+ 接收行；对该用户保留 3 条未读
        "DELETE FROM sys_notice_receiver WHERE notice_id IN (SELECT id FROM sys_notice WHERE notice_no LIKE 'NTA5EXPL%')",
        "DELETE FROM sys_notice WHERE notice_no LIKE 'NTA5EXPL%'",
        """
        INSERT INTO sys_notice (id, notice_no, notice_type, level, title, content, target_type, channel_mask)
        SELECT gen_random_uuid(), 'NTA5EXPL' || lpad(i::text, 6, '0'), 'SYSTEM', 'INFO',
               'EXPLAIN 样本 ' || i, '样本正文', 'ALL', 1
        FROM generate_series(1, 400) AS s(i)
        """,
        """
        INSERT INTO sys_notice_receiver (notice_id, user_id, read_flag, read_at)
        SELECT n.id, u.id, TRUE, CURRENT_TIMESTAMP
        FROM (SELECT id FROM sys_notice WHERE notice_no LIKE 'NTA5EXPL%') n
        CROSS JOIN (SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY username LIMIT 60) u
        """,
        # 目标用户保留 3 条未读（模拟「只有几条没看」的真实分布）
        f"""
        UPDATE sys_notice_receiver SET read_flag = FALSE, read_at = NULL
        WHERE user_id = '{target_user}'::uuid
          AND notice_id IN (
              SELECT notice_id FROM sys_notice_receiver
              WHERE user_id = '{target_user}'::uuid ORDER BY notice_id LIMIT 3)
        """,
        "ANALYZE sys_notice_receiver",
        "ANALYZE sys_notice",
    ]
    with open("/tmp/a5_explain_seed.sql", "w", encoding="utf-8") as handle:
        handle.write("BEGIN;\n" + ";\n".join(statements) + ";\nCOMMIT;\n")
    sql_script("/tmp/a5_explain_seed.sql")

    print("接收人表规模：")
    print("  " + sql("SELECT 'sys_notice_receiver=' || count(*) || ' 行，未读(该用户)=' || "
                     f"(SELECT count(*) FROM sys_notice_receiver WHERE user_id='{target_user}'::uuid "
                     "AND read_flag = FALSE AND is_deleted = FALSE) FROM sys_notice_receiver"))

    print()
    print("--- 与 handler 中 model::count_unread 逐字一致的 SQL ---")
    query = ("SELECT COUNT(*) FROM sys_notice_receiver "
             f"WHERE user_id = '{target_user}'::uuid AND read_flag = FALSE AND is_deleted = FALSE")
    print(query)
    print()
    plan = sql(f"EXPLAIN (ANALYZE, BUFFERS) {query}")
    print(plan)

    hit = "idx_sys_notice_receiver_unread" in plan and (
        "Bitmap Heap Scan" in plan or "Index Only Scan" in plan or "Index Scan" in plan)
    print()
    print(f"  [{'PASS' if hit else 'FAIL'}] 命中部分索引 idx_sys_notice_receiver_unread"
          f"（计划含索引扫描：{'是' if hit else '否'}）")
    return 0 if hit else 1


def main():
    token = login("admin", ENV["ALUN_ADMIN_PASSWORD"])
    print("admin 登录成功，token 前缀：", token[:24], "...")

    seed()
    expectations = expected_counts()
    failures, _ = fanout_checks(token, expectations)
    failures += template_and_unread_checks(token)
    failures += explain_unread()

    banner("结论")
    print(f"失败项：{failures}")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
