"""批次 C · C2「公告用户侧」验收脚本（真机 HTTP + 真库断言，可重复执行）。

用法（在 144 上执行；服务监听 127.0.0.1:8029、库 cloudpivot_c2v）：
    python3 scripts/batchc/c2_accept.py seed      # 夹具：3 部门 / 2 角色 / 4 用户 / 13 条公告
    python3 scripts/batchc/c2_accept.py verify    # A-01 / A-02 / A-03 / A-10 + XSS + 附件越权
    python3 scripts/batchc/c2_accept.py explain   # EXPLAIN 断言走 idx_sys_announcement_scope_reverse
    python3 scripts/batchc/c2_accept.py all

设计说明：
- 夹具里**直接 SQL 写公告**：C2 只交付用户侧，管理侧（发布 / 编辑）是 C3 的票面；
  用 SQL 造公告能让「状态 / 有效期 / 范围」这些维度独立受控，不受发布接口约束影响。
- 角色与用户走**真实接口**创建（密码哈希只有服务端会算），
  权限点用 V0087 种下的 700（页面）/ 701（读按钮）授予。
- 每条断言都打印「期望 vs 实际」，输出即为 docs/批次C-C2-验证证据.md 的原始证据。
"""

import json
import os
import subprocess as sp
import sys
import urllib.error
import urllib.request
import uuid

BASE = "http://127.0.0.1:8029/api"
DB = "cloudpivot_c2v"
ENVF = "/etc/cloudpivot/cloudpivot-test.env"
UPLOAD_ROOT = "/var/tmp/c2run/uploads"

# V0087 种下的两个权限节点
MENU_PAGE = "20000000-0000-0000-0000-000000000700"
MENU_READ = "20000000-0000-0000-0000-000000000701"

ADMIN_PASSWORD = None
USER_PASSWORD = None
FINAL_USER_PASSWORD = None
INITIAL_ADMIN_PASSWORD = None


def env(key):
    for line in open(ENVF, encoding="utf-8"):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    raise SystemExit("缺少环境变量 " + key)


def _init_passwords():
    global ADMIN_PASSWORD, USER_PASSWORD, INITIAL_ADMIN_PASSWORD, FINAL_USER_PASSWORD
    if ADMIN_PASSWORD is None:
        INITIAL_ADMIN_PASSWORD = env("ALUN_ADMIN_PASSWORD")
        ADMIN_PASSWORD = INITIAL_ADMIN_PASSWORD + "-c2admin"
        USER_PASSWORD = INITIAL_ADMIN_PASSWORD + "-c2user"
        FINAL_USER_PASSWORD = USER_PASSWORD + "-v1"


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


def call_raw(method, path, token):
    """取原始响应（附件下载要读头 + 字节）。"""
    request = urllib.request.Request(
        BASE + path, headers={"Authorization": "Bearer " + token}, method=method
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, {k.lower(): v for k, v in response.headers.items()}, response.read()
    except urllib.error.HTTPError as error:
        return error.code, {k.lower(): v for k, v in error.headers.items()}, error.read()


def login(username, password):
    _init_passwords()
    status, body = call("POST", "/system/auth/login", body={"username": username, "password": password})
    if status != 200 or body.get("code") != 0:
        raise SystemExit(f"登录失败 {username}: {status} {body}")
    return body["data"]["access_token"]


def login_user(username):
    """登录夹具用户；首次登录会被底座强制改密（安全审计 CODE-06），这里自动完成。"""
    _init_passwords()
    try:
        token = login(username, USER_PASSWORD)
        status, body = call("PUT", "/system/auth/password", token,
                            {"old_password": USER_PASSWORD, "new_password": FINAL_USER_PASSWORD})
        print(f"[{username}] 首登改密 → HTTP {status} code={body.get('code')} msg={body.get('msg')}")
    except SystemExit:
        print(f"[{username}] 初始口令已失效（此前已改密），直接使用改密后的口令登录")
    return login(username, FINAL_USER_PASSWORD)


def psql(sql):
    result = sp.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-At", "-F", "|", "-c", sql],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": env("ALUN_DATABASE_PASSWORD")},
        check=True,
    )
    return [line.split("|") for line in result.stdout.strip().splitlines() if line]


def section(title):
    print("\n" + "=" * 78)
    print(title)
    print("=" * 78)


def admin_token():
    _init_passwords()
    status, body = call("POST", "/system/auth/login",
                        body={"username": "admin", "password": INITIAL_ADMIN_PASSWORD})
    if status == 200 and body.get("code") == 0:
        token = body["data"]["access_token"]
        status, changed = call("PUT", "/system/auth/password", token,
                               {"old_password": INITIAL_ADMIN_PASSWORD, "new_password": ADMIN_PASSWORD})
        print(f"[admin] 首登改密 → HTTP {status} code={changed.get('code')} msg={changed.get('msg')}")
    else:
        print("[admin] 初始口令已失效（此前已改密），直接使用新口令登录")
    return login("admin", ADMIN_PASSWORD)


# ==================== 夹具 ====================

DEPTS = [
    ("C2-HQ", "C2 总部", None),
    ("C2-RD", "C2 研发部", "C2-HQ"),
    ("C2-RD-BE", "C2 后端组", "C2-RD"),
    ("C2-SALES", "C2 销售部", "C2-HQ"),
]
ROLES = [
    # code, name, 需要的权限码（只给公告读权限：这两个码由 V0087 种下）
    ("C2_R1", "C2 公告读者一", ["business:announcement:read"]),
    ("C2_R2", "C2 公告读者二", ["business:announcement:read"]),
]
USERS = [
    # username, dept_code, role_code（None = 无角色，用于 403 对照）
    ("c2_rd", "C2-RD", "C2_R1"),
    ("c2_be", "C2-RD-BE", "C2_R2"),
    ("c2_sales", "C2-SALES", "C2_R2"),
    ("c2_none", "C2-RD", None),
]

# 公告夹具：(ann_no, title, status, scope_type, expire_offset, content_md, content_html)
# expire_offset=None → 不过期；负数 = 已过期
ANNOUNCEMENTS = [
    ("C2-A01-ALL", "A01 全员公告", "PUBLISHED", "ALL", None, "## 全员\n\n所有人可见。", ""),
    ("C2-A01-ROLE", "A01 角色公告(R1)", "PUBLISHED", "ROLE", None, "仅 C2_R1 可见。", ""),
    ("C2-A01-DEPT", "A01 部门公告(研发含下级)", "PUBLISHED", "DEPT", None, "研发部及其下级可见。", ""),
    ("C2-A01-DEPTSUB", "A01 部门公告(仅后端组)", "PUBLISHED", "DEPT", None, "仅后端组可见。", ""),
    ("C2-A01-USER", "A01 指定用户公告", "PUBLISHED", "USER", None, "仅 c2_be 可见。", ""),
    ("C2-A02-DRAFT", "A02 草稿", "DRAFT", "ALL", None, "草稿不可见。", ""),
    ("C2-A02-OFFLINE", "A02 已下线", "OFFLINE", "ALL", None, "已下线不可见。", ""),
    ("C2-A02-EXPIRED", "A02 已过期(状态)", "EXPIRED", "ALL", -1, "已过期不可见。", ""),
    ("C2-A03-EXPIRE", "A03 过期但任务未跑", "PUBLISHED", "ALL", -1, "过期双保险不可见。", ""),
    ("C2-A10-UNREAD", "A10 未读口径", "PUBLISHED", "ALL", None, "用于未读数交叉断言。", ""),
    # XSS-1：Markdown 里内嵌原始 HTML（ENABLE_HTML=false → 转义成文本）
    ("C2-XSS-MD", "XSS Markdown 内嵌 HTML", "PUBLISHED", "ALL", None,
     "<img src=x onerror=alert(1)>\n\n正文段落。", ""),
    # XSS-2：库里被手工塞了脏快照（模拟绕过发布接口改库）→ 出参必须被闸 3 净化
    ("C2-XSS-DIRTY", "XSS 脏快照", "PUBLISHED", "ALL", None,
     "正文", '<p>看图</p><img src="x" onerror="alert(1)"><script>alert(2)</script>'),
    ("C2-ATT", "附件代理下载", "PUBLISHED", "ALL", None, "带附件。", ""),
    # 附件越权用：只对研发部可见，销售部拿它的附件 → 403 / 62004
    ("C2-ATT-DEPT", "附件(仅研发部)", "PUBLISHED", "DEPT", None, "部门附件。", ""),
]


def dept_ids_by_code(token):
    status, body = call("GET", "/system/dept/tree", token)
    if status != 200 or body.get("code") != 0:
        raise SystemExit(f"部门树失败: {status} {body}")
    found = {}

    def walk(nodes):
        for node in nodes:
            found[node["dept_code"]] = node["id"]
            walk(node.get("children") or [])

    walk(body["data"])
    return found


def menu_ids_by_code(token):
    status, body = call("GET", "/system/menu/tree", token)
    if status != 200 or body.get("code") != 0:
        raise SystemExit(f"菜单树失败: {status} {body}")
    found = {}

    def walk(nodes):
        for node in nodes:
            if node.get("auth_code"):
                found.setdefault(node["auth_code"], node["id"])
            walk(node.get("children") or [])

    walk(body["data"])
    return found


def seed():
    token = admin_token()
    existing = dept_ids_by_code(token)
    dept_ids = dict(existing)
    for code, name, parent in DEPTS:
        if code in existing:
            print(f"[dept] 已存在 {code} → {existing[code]}")
            continue
        payload = {"dept_code": code, "dept_name": name,
                   "parent_id": dept_ids.get(parent) if parent else None,
                   "status": 1, "sort_order": 0}
        status, body = call("POST", "/system/dept", token, payload)
        if status != 200 or body.get("code") != 0:
            raise SystemExit(f"建部门失败 {code}: {status} {body}")
        dept_ids[code] = body["data"]["id"]
        print(f"[dept] + {code} → {dept_ids[code]}")

    code_to_menu = menu_ids_by_code(token)
    for needed in ("business:announcement:read",):
        if needed not in code_to_menu:
            raise SystemExit(f"V0087 未种下权限点 {needed}（菜单树里找不到）")
    menu_ids = sorted({code_to_menu["business:announcement:read"], MENU_PAGE, MENU_READ})
    print(f"[menu] business:announcement:read → {code_to_menu['business:announcement:read']}；授予集合={menu_ids}")

    role_ids = {}
    for code, name, perms in ROLES:
        status, body = call("GET", "/system/role/page?page=1&page_size=100&role_code=" + code, token)
        hit = [item for item in body["data"]["list"] if item["role_code"] == code]
        if hit:
            role_ids[code] = hit[0]["id"]
            print(f"[role] 已存在 {code} → {hit[0]['id']}")
        else:
            payload = {"role_code": code, "role_name": name, "description": "",
                       "status": 1, "sort_order": 0, "remark": "", "data_scope": 1}
            status, body = call("POST", "/system/role", token, payload)
            if status != 200 or body.get("code") != 0:
                raise SystemExit(f"建角色失败 {code}: {status} {body}")
            role_ids[code] = body["data"]["id"]
            print(f"[role] + {code} → {role_ids[code]}")
        status, body = call("PUT", f"/system/role/{role_ids[code]}/menus", token,
                            {"menu_ids": menu_ids})
        print(f"[role] {code} 授权 {len(menu_ids)} 个菜单 → HTTP {status} code={body.get('code')}")

    user_ids = {}
    for username, dept_code, role_code in USERS:
        status, body = call("GET", f"/system/user/page?page=1&page_size=100&username={username}", token)
        hit = [item for item in body["data"]["list"] if item["username"] == username]
        if hit:
            user_ids[username] = hit[0]["id"]
            status, body = call("PATCH", f"/system/user/{user_ids[username]}/password", token,
                                {"password": USER_PASSWORD})
            print(f"[user] 已存在 {username} → {user_ids[username]}（口令对齐 {status}/{body.get('code')}）")
        else:
            payload = {
                "username": username, "password": USER_PASSWORD, "real_name": username.upper(),
                "email": f"{username}@c2.local", "mobile": "13900000000",
                "dept_id": dept_ids[dept_code], "status": 1, "remark": "",
                "role_ids": [role_ids[role_code]] if role_code else [],
            }
            status, body = call("POST", "/system/user", token, payload)
            if status != 200 or body.get("code") != 0:
                raise SystemExit(f"建用户失败 {username}: {status} {body}")
            user_ids[username] = body["data"]["id"]
            print(f"[user] + {username} dept={dept_code} role={role_code} → {user_ids[username]}")

    # ---------- 公告夹具（直接 SQL） ----------
    i = 0
    for ann_no, title, status_, scope, expire_offset, md, html in ANNOUNCEMENTS:
        i += 1
        ann_id = str(uuid.uuid5(uuid.NAMESPACE_URL, "c2/" + ann_no))
        expire = "NULL" if expire_offset is None else f"now() + interval '{expire_offset} days'"
        if status_ == "DRAFT":
            publish = "NULL"
        elif expire_offset is not None:
            # ck_sys_announcement_expire 要求 expire_at > publish_at：过期的夹具必须让发布更早
            publish = f"now() - interval '{abs(expire_offset) + 1} days'"
        else:
            publish = "now() - interval '1 hour'"
        psql(
            "INSERT INTO sys_announcement (id, ann_no, title, summary, content_md, content_html, "
            "level, scope_type, status, publish_at, expire_at, publisher_name, receiver_count, version) "
            f"VALUES ('{ann_id}'::uuid, '{ann_no}', '{title}', '摘要 {ann_no}', $md${md}$md$, "
            f"$html${html}$html$, 'NORMAL', '{scope}', '{status_}', {publish}, {expire}, 'C2 夹具', 3, 1) "
            "ON CONFLICT (ann_no) DO UPDATE SET title = EXCLUDED.title, "
            "content_md = EXCLUDED.content_md, content_html = EXCLUDED.content_html, "
            "status = EXCLUDED.status, publish_at = EXCLUDED.publish_at, "
            "expire_at = EXCLUDED.expire_at, deleted_at = NULL"
        )
        print(f"[ann] {ann_no:16s} id={ann_id} status={status_} scope={scope} expire={expire}")

    # 范围明细
    scope_rows = {
        "C2-A01-ROLE": [("ROLE", role_ids["C2_R1"])],
        "C2-A01-DEPT": [("DEPT", dept_ids["C2-RD"]), ("DEPT", dept_ids["C2-RD-BE"])],
        "C2-A01-DEPTSUB": [("DEPT", dept_ids["C2-RD-BE"])],
        "C2-A01-USER": [("USER", user_ids["c2_be"])],
        "C2-ATT-DEPT": [("DEPT", dept_ids["C2-RD"]), ("DEPT", dept_ids["C2-RD-BE"])],
    }
    psql("DELETE FROM sys_announcement_scope WHERE announcement_id IN "
         "(SELECT id FROM sys_announcement WHERE ann_no LIKE 'C2-%')")
    for ann_no, rows in scope_rows.items():
        for scope_type, target in rows:
            psql(
                "INSERT INTO sys_announcement_scope (id, announcement_id, scope_type, target_id) "
                f"SELECT gen_random_uuid(), a.id, '{scope_type}', '{target}'::uuid "
                f"FROM sys_announcement a WHERE a.ann_no = '{ann_no}' "
                "ON CONFLICT (announcement_id, scope_type, target_id) DO NOTHING"
            )
    print(f"[scope] 写入 {sum(len(v) for v in scope_rows.values())} 行范围明细")

    # 已读回执清空（保证未读断言可重复）
    psql("DELETE FROM sys_announcement_read WHERE announcement_id IN "
         "(SELECT id FROM sys_announcement WHERE ann_no LIKE 'C2-%')")

    # ---------- 附件：真实文件 + sys_file 记录 + 绑定 ----------
    os.makedirs(os.path.join(UPLOAD_ROOT, "c2"), exist_ok=True)
    file_key = "c2/c2-attachment.pdf"
    body_bytes = b"%PDF-1.4\n% C2 attachment fixture\n%%EOF\n"
    with open(os.path.join(UPLOAD_ROOT, file_key), "wb") as handle:
        handle.write(body_bytes)
    file_id = str(uuid.uuid5(uuid.NAMESPACE_URL, "c2/file/attachment"))
    psql(
        "INSERT INTO sys_file (id, file_name, original_name, file_key, file_size, file_hash, "
        "mime_type, extension, storage_path, category, status, storage_code, upload_mode) VALUES "
        f"('{file_id}'::uuid, 'c2-attachment.pdf', '公告附件 说明.pdf', '{file_key}', "
        f"{len(body_bytes)}, 'c2-fixture-hash', 'application/pdf', 'pdf', '/{file_key}', "
        "'attachment', 1, NULL, 'SERVER') ON CONFLICT (id) DO UPDATE SET "
        "file_key = EXCLUDED.file_key, file_size = EXCLUDED.file_size, deleted_at = NULL"
    )
    for ann_no in ("C2-ATT", "C2-ATT-DEPT"):
        psql(
            "INSERT INTO sys_announcement_attachment (id, announcement_id, file_id, file_name, "
            "file_size, mime_type, sort_order) SELECT gen_random_uuid(), a.id, "
            f"'{file_id}'::uuid, '公告附件 说明.pdf', {len(body_bytes)}, 'application/pdf', 0 "
            f"FROM sys_announcement a WHERE a.ann_no = '{ann_no}' "
            "ON CONFLICT (announcement_id, file_id) DO NOTHING"
        )
    print(f"[file] sys_file {file_id} key={file_key} size={len(body_bytes)}；已绑定 C2-ATT / C2-ATT-DEPT")

    print("\n=== 夹具完成 ===")
    print("公告总数：", psql("SELECT count(*) FROM sys_announcement WHERE ann_no LIKE 'C2-%'")[0][0])
    print("用户：", psql("SELECT string_agg(username, ',') FROM sys_user WHERE username LIKE 'c2_%'")[0][0])


# ==================== 验收 ====================

EXPECT = {
    # 期望可见的 ann_no（顺序无关，只比集合）
    "c2_rd": {"C2-A01-ALL", "C2-A01-ROLE", "C2-A01-DEPT", "C2-A10-UNREAD", "C2-XSS-MD",
              "C2-XSS-DIRTY", "C2-ATT", "C2-ATT-DEPT"},
    "c2_be": {"C2-A01-ALL", "C2-A01-DEPT", "C2-A01-DEPTSUB", "C2-A01-USER", "C2-A10-UNREAD",
              "C2-XSS-MD", "C2-XSS-DIRTY", "C2-ATT", "C2-ATT-DEPT"},
    "c2_sales": {"C2-A01-ALL", "C2-A10-UNREAD", "C2-XSS-MD", "C2-XSS-DIRTY", "C2-ATT"},
}


def visible(token, query=""):
    status, body = call("GET", f"/business/announcement/visible?page=1&page_size=100{query}", token)
    if status != 200 or body.get("code") != 0:
        raise SystemExit(f"列表失败: {status} {body}")
    data = body["data"]
    return {item["ann_no"] for item in data["list"]}, data["total"], data["list"]


def find_id(ann_no):
    return psql(f"SELECT id FROM sys_announcement WHERE ann_no = '{ann_no}'")[0][0]


def verify():
    _init_passwords()
    tokens = {name: login_user(name) for name in ("c2_rd", "c2_be", "c2_sales")}

    # ---------- A-01 范围判定矩阵 ----------
    section("A-01 范围判定矩阵：{ALL, ROLE, DEPT(含下级), USER} × {命中, 不命中}")
    for name, expected in EXPECT.items():
        got, total, _ = visible(tokens[name])
        print(f"  {name:9s} 可见 {total} 条：{sorted(got)}")
        assert got == expected, f"{name} 期望 {sorted(expected)} 实际 {sorted(got)}"
    print("  ✅ ALL：3 人全见")
    print("  ✅ ROLE(C2_R1)：c2_rd 命中；c2_be / c2_sales（持 C2_R2）不命中")
    print("  ✅ DEPT(C2-RD，含下级 C2-RD-BE)：c2_rd / c2_be 命中；c2_sales 不命中")
    print("  ✅ DEPT(C2-RD-BE 单节点)：只有 c2_be 命中，父部门 c2_rd **不命中**（证明「含下级」是发布期展开，不是读取期向上递归）")
    print("  ✅ USER(c2_be)：只有 c2_be 命中")

    # ---------- A-02 状态过滤 ----------
    section("A-02 状态过滤：DRAFT / OFFLINE / EXPIRED 对普通用户全部不可见")
    got, _, _ = visible(tokens["c2_rd"])
    for ann_no in ("C2-A02-DRAFT", "C2-A02-OFFLINE", "C2-A02-EXPIRED"):
        assert ann_no not in got, f"{ann_no} 不应可见"
        print(f"  {ann_no:16s} 不在列表里 ✓")
    print("  ✅ 三种非 PUBLISHED 状态各断言 0 行")

    # ---------- A-03 到期双保险 ----------
    section("A-03 到期双保险：expire_at 已过但 status 仍是 PUBLISHED（定时任务未跑）→ 查询不可见")
    row = psql("SELECT status, expire_at < now() FROM sys_announcement WHERE ann_no='C2-A03-EXPIRE'")[0]
    print(f"  DB 事实：status={row[0]} expired={row[1]}（故意留成 PUBLISHED）")
    assert row[0] == "PUBLISHED" and row[1] == "t", row
    assert "C2-A03-EXPIRE" not in got, "过期公告不应可见"
    print("  ✅ 查询侧带 expire_at > now() 兜住了定时任务的最多 10 分钟延迟")

    # ---------- A-10 未读口径 ----------
    section("A-10 未读口径：未读数 == 可见未过期 − 已读，且与 only_unread 的 total 恒等")
    token = tokens["c2_rd"]
    status, body = call("GET", "/business/announcement/unread-count", token)
    assert status == 200 and body.get("code") == 0, body
    unread = body["data"]["announcement_unread"]
    _, total_all, _ = visible(token)
    _, total_unread, _ = visible(token, "&only_unread=true")
    print(f"  未读接口 announcement_unread={unread}  列表 total={total_all}  only_unread total={total_unread}")
    assert unread == total_unread == total_all, (unread, total_unread, total_all)
    print("  ✅ 三者相等（夹具已清空已读回执 → 全部未读）")

    target_id = find_id("C2-A01-ALL")
    status, body = call("POST", f"/business/announcement/{target_id}/read", token)
    print(f"  POST /{target_id}/read → HTTP {status} code={body.get('code')} data={body.get('data')}")
    assert status == 200 and body["data"]["first_read"] is True
    first_count = body["data"]["read_count"]
    status, body = call("POST", f"/business/announcement/{target_id}/read", token)
    print(f"  重复 POST（幂等）→ HTTP {status} first_read={body['data']['first_read']} "
          f"read_count={body['data']['read_count']}（首次 {first_count}）")
    assert body["data"]["first_read"] is False and body["data"]["read_count"] == first_count
    rows = psql(f"SELECT count(*) FROM sys_announcement_read WHERE announcement_id='{target_id}'::uuid")
    print(f"  DB 复核：sys_announcement_read 行数={rows[0][0]}（幂等 → 恒为 1）")
    assert rows[0][0] == "1"

    status, body = call("GET", "/business/announcement/unread-count", token)
    unread_after = body["data"]["announcement_unread"]
    _, total_unread_after, _ = visible(token, "&only_unread=true")
    print(f"  标已读后：未读接口={unread_after}  only_unread total={total_unread_after}（之前 {unread}）")
    assert unread_after == total_unread_after == unread - 1, (unread_after, total_unread_after, unread)
    print("  ✅ 已读 -1，两接口仍恒等")

    status, body = call("PUT", "/business/announcement/read-all", token)
    print(f"  PUT /read-all → HTTP {status} code={body.get('code')} affected={body.get('data', {}).get('affected')}")
    assert status == 200 and body["data"]["affected"] == unread - 1
    status, body = call("PUT", "/business/announcement/read-all", token)
    print(f"  重复 read-all（幂等）→ affected={body['data']['affected']}")
    assert body["data"]["affected"] == 0
    status, body = call("GET", "/business/announcement/unread-count", token)
    print(f"  read-all 后未读数={body['data']['announcement_unread']}")
    assert body["data"]["announcement_unread"] == 0
    print("  ✅ 全部已读只作用于我的可见集合，且幂等")

    # ---------- 详情 + XSS ----------
    section("XSS 验证：正文写入 <img src=x onerror=alert(1)> → 读取时 content_html 无可执行 onerror")
    case_md = find_id("C2-XSS-MD")
    status, body = call("GET", f"/business/announcement/{case_md}", token)
    assert status == 200 and body.get("code") == 0, body
    html = body["data"]["content_html"]
    print(f"  [Markdown 内嵌 HTML] content_html = {html!r}")
    assert "<img" not in html, html
    assert "&lt;img" in html, html
    print("  ✅ 原始 HTML 被转义成文本节点（ENABLE_HTML=false），浏览器不会解析成元素")

    case_dirty = find_id("C2-XSS-DIRTY")
    status, body = call("GET", f"/business/announcement/{case_dirty}", token)
    assert status == 200 and body.get("code") == 0, body
    html = body["data"]["content_html"]
    print(f"  [脏快照（模拟改库）] content_html = {html!r}")
    assert "onerror" not in html, html
    assert "<script" not in html, html
    assert "alert" not in html.lower(), html
    assert "<p>看图</p>" in html, html
    print("  ✅ 闸 3（出参前再净化）生效：脏快照里的 onerror / script 全部剥离，合法部分保留")

    status, body = call("GET", f"/business/announcement/{case_md}?x=1", token)
    assert body["data"]["content_html"] == html or True  # 仅热身，不断言

    # 详情可见性：不可见的公告 → 403/62004；不存在 → 404/62002
    status, body = call("GET", f"/business/announcement/{find_id('C2-A01-USER')}", token)
    print(f"  c2_rd 读「仅 c2_be 可见」的公告 → HTTP {status} code={body.get('code')} msg={body.get('msg')}")
    assert status == 403 and body.get("code") == 62004, (status, body)
    fake = str(uuid.uuid4())
    status, body = call("GET", f"/business/announcement/{fake}", token)
    print(f"  读不存在的公告 → HTTP {status} code={body.get('code')} msg={body.get('msg')}")
    assert status == 404 and body.get("code") == 62002, (status, body)
    status, body = call("GET", f"/business/announcement/{find_id('C2-A03-EXPIRE')}", token)
    print(f"  读过期公告（status 仍 PUBLISHED）→ HTTP {status} code={body.get('code')}")
    assert status == 403 and body.get("code") == 62005, (status, body)

    # ---------- 附件代理下载 ----------
    section("附件代理下载：可见性 + 归属校验 + 强制 attachment + 不暴露存储后端")
    ann_id = find_id("C2-ATT")
    detail_status, detail_body = call("GET", f"/business/announcement/{ann_id}", token)
    assert detail_body["data"]["attachments"], detail_body
    att = detail_body["data"]["attachments"][0]
    print(f"  详情里的附件对象 = {att}")
    assert att["download_url"].startswith("/api/business/announcement/"), att
    for forbidden in ("http://", "https://", "minio", "bucket", "X-Amz"):
        assert forbidden not in att["download_url"], att
    code, headers, payload = call_raw("GET", att["download_url"].replace("/api", ""), token)
    print(f"  GET {att['download_url']} → HTTP {code}")
    print(f"    content-type={headers.get('content-type')} content-disposition={headers.get('content-disposition')}")
    print(f"    content-length={headers.get('content-length')} x-content-type-options={headers.get('x-content-type-options')}")
    print(f"    字节数={len(payload)} 前 16 字节={payload[:16]!r}")
    assert code == 200
    assert headers.get("content-disposition", "").startswith("attachment"), headers
    assert "filename*=UTF-8''" in headers.get("content-disposition", ""), headers
    assert headers.get("content-type") != "text/html"
    assert headers.get("x-content-type-options") == "nosniff"
    assert "private" in headers.get("cache-control", ""), headers
    assert len(payload) == att["file_size"]

    # 越权 1：拿别的公告的 id 拼同一个附件 id → 404/62003
    other = find_id("C2-A01-ALL")
    code, _, raw = call_raw("GET", f"/business/announcement/{other}/attachment/{att['id']}", token)
    code_body = json.loads(raw)
    print(f"  拿 C2-A01-ALL 的 id 拼 C2-ATT 的附件 id → HTTP {code} code={code_body.get('code')}")
    assert code == 404 and code_body.get("code") == 62003

    # 越权 2：看不见公告的人 → 403/62004
    dept_att_ann = find_id("C2-ATT-DEPT")
    status, body = call("GET", f"/business/announcement/{dept_att_ann}", tokens["c2_sales"])
    dept_att = psql("SELECT id FROM sys_announcement_attachment "
                    f"WHERE announcement_id='{dept_att_ann}'::uuid LIMIT 1")[0][0]
    code, _, raw = call_raw("GET", f"/business/announcement/{dept_att_ann}/attachment/{dept_att}",
                            tokens["c2_sales"])
    code_body = json.loads(raw)
    print(f"  c2_sales 下载「仅研发部」公告的附件 → HTTP {code} code={code_body.get('code')}")
    assert code == 403 and code_body.get("code") == 62004

    # ---------- 权限点 ----------
    section("权限点：无 business:announcement:read 的用户 → 403")
    none_token = login_user("c2_none")
    status, body = call("GET", "/business/announcement/visible?page=1&page_size=10", none_token)
    print(f"  c2_none（无角色）GET /visible → HTTP {status} code={body.get('code')} msg={body.get('msg')}")
    assert status == 403

    # ---------- 兼容路径 ----------
    section("路径契约：/visible/page、/mine/* 别名与主路径语义一致")
    status, body = call("GET", "/business/announcement/visible/page?page=1&page_size=100", token)
    assert status == 200 and body.get("code") == 0, body
    alias_total = body["data"]["total"]
    status, body = call("GET", "/business/announcement/mine/page?page=1&page_size=100", token)
    assert status == 200 and body.get("code") == 0, body
    mine_total = body["data"]["total"]
    status, body = call("GET", "/business/announcement/mine/unread-count", token)
    mine_unread = body["data"]["announcement_unread"]
    status, body = call("GET", f"/business/announcement/mine/{ann_id}", token)
    assert status == 200 and body.get("code") == 0, body
    print(f"  /visible total={total_all}  /visible/page total={alias_total}  /mine/page total={mine_total}"
          f"  /mine/unread-count={mine_unread}  /mine/{{id}} HTTP 200")
    assert alias_total == mine_total == total_all
    print("  ✅ 三套路径同一实现、同一份数据")

    print("\n=== 验收全部通过 ===")


def bulk_fill():
    """灌 POC-8 量级的数据（2000 公告 / 6000 范围行）让 planner 走出有意义的计划。

    量太小时 PG 一律 Seq Scan，索引是否可用就无从断言；灌完立刻 ANALYZE。
    用完由 [`bulk_clear`] 删除（否则会把 verify 的「可见集合」断言打乱）。
    """
    role_id = psql("SELECT id FROM sys_role WHERE role_code='C2_R1'")[0][0]
    dept_rd = psql("SELECT id FROM sys_dept WHERE dept_code='C2-RD'")[0][0]
    user_be = psql("SELECT id FROM sys_user WHERE username='c2_be'")[0][0]
    psql(
        "INSERT INTO sys_announcement (id, ann_no, title, summary, content_md, level, scope_type, "
        "status, publish_at, expire_at, receiver_count, version) "
        "SELECT gen_random_uuid(), 'C2BULK-'||lpad(g::text,4,'0'), '批量公告 '||g, '摘要', '正文', "
        "'NORMAL', (ARRAY['ALL','ROLE','DEPT','USER'])[1 + (g % 4)], 'PUBLISHED', "
        "now() - (g || ' minutes')::interval, NULL, 10, 1 "
        "FROM generate_series(1,2000) g ON CONFLICT (ann_no) DO NOTHING"
    )
    psql(
        "INSERT INTO sys_announcement_scope (id, announcement_id, scope_type, target_id) "
        "SELECT gen_random_uuid(), a.id, t.scope_type, t.target_id "
        "FROM sys_announcement a "
        "CROSS JOIN (VALUES ('ROLE', '" + role_id + "'::uuid), "
        "                   ('DEPT', '" + dept_rd + "'::uuid), "
        "                   ('USER', '" + user_be + "'::uuid)) AS t(scope_type, target_id) "
        "WHERE a.ann_no LIKE 'C2BULK-%' "
        "ON CONFLICT (announcement_id, scope_type, target_id) DO NOTHING"
    )
    psql("ANALYZE sys_announcement")
    psql("ANALYZE sys_announcement_scope")
    counts = psql("SELECT (SELECT count(*) FROM sys_announcement), "
                  "(SELECT count(*) FROM sys_announcement_scope)")[0]
    print(f"  灌数据完成：sys_announcement={counts[0]} 行，sys_announcement_scope={counts[1]} 行")


def bulk_clear():
    psql("DELETE FROM sys_announcement WHERE ann_no LIKE 'C2BULK-%'")
    psql("ANALYZE sys_announcement")
    psql("ANALYZE sys_announcement_scope")
    left = psql("SELECT count(*) FROM sys_announcement WHERE ann_no LIKE 'C2BULK-%'")[0][0]
    print(f"  清理批量数据：剩余 {left} 行")


def explain():
    section("EXPLAIN 断言：可见性判定走 idx_sys_announcement_scope_reverse")
    bulk_fill()
    try:
        role_id = psql("SELECT id FROM sys_role WHERE role_code='C2_R1'")[0][0]
        dept_rd = psql("SELECT id FROM sys_dept WHERE dept_code='C2-RD'")[0][0]
        user_be = psql("SELECT id FROM sys_user WHERE username='c2_be'")[0][0]
        sql = f"""
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
SELECT 1 FROM sys_announcement_scope s
 WHERE s.scope_type = 'ROLE' AND s.target_id = ANY('{{{role_id}}}'::text::uuid[]);
"""
        out = sp.run(["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-c", sql.strip()],
                     capture_output=True, text=True,
                     env={**os.environ, "PGPASSWORD": env("ALUN_DATABASE_PASSWORD")}, check=True).stdout
        print(out)
        assert "idx_sys_announcement_scope_reverse" in out, out

        sql2 = f"""
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
SELECT a.id FROM sys_announcement a
 WHERE a.deleted_at IS NULL AND a.status = 'PUBLISHED'
   AND (a.publish_at IS NULL OR a.publish_at <= now())
   AND (a.expire_at IS NULL OR a.expire_at > now())
   AND ( a.scope_type = 'ALL'
      OR EXISTS (SELECT 1 FROM sys_announcement_scope s
                  WHERE s.announcement_id = a.id AND s.scope_type = 'ROLE'
                    AND s.target_id = ANY('{{{role_id}}}'::text::uuid[]))
      OR EXISTS (SELECT 1 FROM sys_announcement_scope s
                  WHERE s.announcement_id = a.id AND s.scope_type = 'DEPT'
                    AND s.target_id = ANY('{{{dept_rd}}}'::text::uuid[]))
      OR EXISTS (SELECT 1 FROM sys_announcement_scope s
                  WHERE s.announcement_id = a.id AND s.scope_type = 'USER'
                    AND s.target_id = '{user_be}'::uuid) )
 ORDER BY a.is_top DESC, a.publish_at DESC
 LIMIT 20;
"""
        out2 = sp.run(["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-c", sql2.strip()],
                      capture_output=True, text=True,
                      env={**os.environ, "PGPASSWORD": env("ALUN_DATABASE_PASSWORD")}, check=True).stdout
        print(out2)
        assert "idx_sys_announcement_scope_reverse" in out2, out2
        print("  ✅ 两条查询计划里都出现 idx_sys_announcement_scope_reverse")
    finally:
        bulk_clear()


if __name__ == "__main__":
    action = sys.argv[1] if len(sys.argv) > 1 else "all"
    if action in ("seed", "all"):
        seed()
    if action in ("verify", "all"):
        verify()
    if action in ("explain", "all"):
        explain()
