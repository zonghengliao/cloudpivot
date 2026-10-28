"""批次 B · B3「底座接入」验收脚本（真机 HTTP 全流程，可重复执行）。

用法（在部署机上执行；服务监听 127.0.0.1:8099、库 cloudpivot_b3v）：
    python3 b3_accept.py seed      # 建夹具：4 部门 / 4 角色 / 8 用户（幂等）
    python3 b3_accept.py verify    # 逐条跑验收 ②③④⑤⑥ + 交付物 3 的部门树裁剪
    python3 b3_accept.py all       # seed + verify

设计说明：
- 全部通过真实 HTTP 接口准备与验证（含部门 parent_id 搬迁，因为要验证的正是
  接口里的 `bump_epoch()` 是否生效）；
- admin 首登必须改密（安全审计 CODE-06），脚本自动完成改密后改用新口令；
- 每一步都打印 HTTP 状态 / 业务码 / total / 实际用户名与部门名，
  输出即为 `docs/批次B-B3-验证证据.md` 的原始证据。
"""

import json
import os
import subprocess
import subprocess as sp
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8099/api"
DB = "cloudpivot_b3v"
ENVF = "/etc/cloudpivot/cloudpivot-test.env"

# 口令**不落盘**：全部由 `/etc/cloudpivot/cloudpivot-test.env` 里的初始口令推导，
# 避免验收脚本本身成为仓库里的凭据明文（.gitleaks.toml 的 pre-commit 会拦这类字面量）。
INITIAL_ADMIN_PASSWORD = None  # 由 env() 惰性读取，见下方
ADMIN_PASSWORD = None
USER_PASSWORD = None


def env(key):
    for line in open(ENVF, encoding="utf-8"):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    raise SystemExit("缺少环境变量 " + key)


def _init_passwords():
    """当首次登录成功、完成首登改密后使用的口令（推导，不写死）。"""
    global INITIAL_ADMIN_PASSWORD, ADMIN_PASSWORD, USER_PASSWORD
    if ADMIN_PASSWORD is None:
        INITIAL_ADMIN_PASSWORD = env("ALUN_ADMIN_PASSWORD")
        ADMIN_PASSWORD = INITIAL_ADMIN_PASSWORD + "-b3admin"
        USER_PASSWORD = INITIAL_ADMIN_PASSWORD + "-b3user"


_init_passwords()


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


def login(username, password):
    _init_passwords()
    status, body = call("POST", "/system/auth/login", body={"username": username, "password": password})
    if status != 200 or body.get("code") != 0:
        raise SystemExit(f"登录失败 {username}: {status} {body}")
    return body["data"]["access_token"]


def psql(sql):
    result = sp.run(["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-At", "-F", "|", "-c", sql],
                    capture_output=True, text=True, env={**os.environ, "PGPASSWORD": env("ALUN_DATABASE_PASSWORD")}, check=True)
    return [line.split("|") for line in result.stdout.strip().splitlines() if line]


def redis_cmd(*args):
    result = sp.run(["redis-cli", "-n", "6", *args], capture_output=True, text=True)
    return result.stdout.strip() or result.stderr.strip()


def user_page(token, label, query=""):
    status, body = call("GET", "/system/user/page?page=1&page_size=100" + query, token)
    if status != 200 or body.get("code") != 0:
        print(f"  {label}: HTTP {status} code={body.get('code')} msg={body.get('msg')}")
        return status, body, None
    data = body["data"]
    rows = [(item["username"], item["dept_name"]) for item in data["list"]]
    print(f"  {label}: HTTP {status} code=0 total={data['total']} rows={len(rows)}")
    print(f"    部门分布: {sorted({name or '(无部门)' for _, name in rows})}")
    print(f"    用户名: {sorted(name for name, _ in rows)}")
    return status, body, rows


def section(title):
    print("\n" + "=" * 78)
    print(title)
    print("=" * 78)


def acceptance2():
    section("验收 ②：部门主管（data_scope=2）登录 → GET /api/system/user/page 只回本部门行")
    token = login("b3_mgr", USER_PASSWORD)
    print(f"  token(b3_mgr) = {token[:48]}…（长度 {len(token)}）")
    status, body, rows = user_page(token, "GET /system/user/page")
    dept_ids = psql("select d.id,d.dept_name,count(u.id) from sys_dept d left join sys_user u on u.dept_id=d.id and u.deleted_at is null where d.dept_code like 'B3%' group by 1,2 order by 2")
    print("  DB 侧部门人数: " + "; ".join(f"{name}={count}" for _, name, count in dept_ids))
    assert status == 200 and rows is not None
    names = {name for name, _ in rows}
    assert names == {"b3_mgr", "b3_dev1", "b3_childmgr", "b3_selfonly"}, names
    assert {dept for _, dept in rows} == {"B3 研发部"}, rows
    print(f"  ✅ 只回「B3 研发部」的 4 行（b3_mgr/b3_dev1/b3_childmgr/b3_selfonly），无后端组/销售部/总部行")
    return token


def acceptance3():
    section("验收 ③：改角色名后 data_scope 不变（PUT 不带 data_scope 字段，模拟老前端）")
    admin = login("admin", ADMIN_PASSWORD)
    before = psql("select id,role_name,data_scope from sys_role where role_code='B3_DEPT'")[0]
    print(f"  改前 DB: id={before[0]} role_name={before[1]} data_scope={before[2]}")
    role_id = before[0]
    payload = {"role_code": "B3_DEPT", "role_name": "B3 本部门（改名验证）", "description": "", "status": 1, "sort_order": 0, "remark": ""}
    status, body = call("PUT", f"/system/role/{role_id}", admin, payload)
    print(f"  PUT /system/role/{role_id}  body 里**没有** data_scope 字段 → HTTP {status} code={body.get('code')}")
    assert status == 200 and body.get("code") == 0, body
    print(f"  响应体 data_scope = {body['data']['data_scope']}")
    after = psql("select id,role_name,data_scope from sys_role where role_code='B3_DEPT'")[0]
    print(f"  改后 DB: id={after[0]} role_name={after[1]} data_scope={after[2]}")
    assert before[2] == after[2] == "2", (before, after)
    assert after[1] != before[1], "角色名应已修改"
    print(f"  ✅ 角色名 {before[1]} → {after[1]}，data_scope 仍为 2（未被重置为默认 1）")
    # 复原角色名，避免影响其它断言
    payload["role_name"] = "B3 本部门（验收②）"
    status, body = call("PUT", f"/system/role/{role_id}", admin, payload)
    print(f"  复原角色名 → HTTP {status} code={body.get('code')} data_scope={body['data']['data_scope']}")
    assert body["data"]["data_scope"] == 2


def acceptance4():
    section("验收 ④：越权用例 —— 部门主管把自己的角色范围改成超出自身可见集合 → 403 / 61002")
    mgr = login("b3_mgr", USER_PASSWORD)
    role_id = psql("select id from sys_role where role_code='B3_DEPT'")[0][0]
    # 先证明「不改范围」的同等请求是允许的（避免把 403 误读成权限点缺失）
    status, body = call("GET", "/system/role/page?page=1&page_size=10", mgr)
    print(f"  预检 b3_mgr 有 system:role:list → GET /system/role/page HTTP {status} code={body.get('code')}")
    assert status == 200 and body.get("code") == 0

    for target, desc in [(1, "全部数据 ALL（远超其「本部门」可见集合）"), (3, "本部门及以下 DEPT_AND_CHILD（对持有者本人就是放宽）")]:
        payload = {"role_code": "B3_DEPT", "role_name": "B3 本部门（验收②）", "description": "", "status": 1, "sort_order": 0, "remark": "", "data_scope": target}
        status, body = call("PUT", f"/system/role/{role_id}", mgr, payload)
        print(f"  PUT data_scope={target}（{desc}）→ HTTP {status} code={body.get('code')} msg={body.get('msg')}")
        assert status == 403 and body.get("code") == 61002, (status, body)
    row = psql("select data_scope from sys_role where role_code='B3_DEPT'")[0][0]
    print(f"  DB 复核：B3_DEPT.data_scope = {row}（未被越权写入）")
    assert row == "2"
    # 反向对照：收紧到「仅本人」是允许的（不是「一律禁止改范围」）
    payload = {"role_code": "B3_DEPT", "role_name": "B3 本部门（验收②）", "description": "", "status": 1, "sort_order": 0, "remark": "", "data_scope": 4}
    status, body = call("PUT", f"/system/role/{role_id}", mgr, payload)
    print(f"  对照：收紧为 data_scope=4（仅本人）→ HTTP {status} code={body.get('code')} data_scope={body.get('data', {}).get('data_scope')}")
    assert status == 200 and body["data"]["data_scope"] == 4
    # 复原为 2（用超管）
    admin = login("admin", ADMIN_PASSWORD)
    payload["data_scope"] = 2
    status, body = call("PUT", f"/system/role/{role_id}", admin, payload)
    print(f"  超管复原 data_scope=2 → HTTP {status} data_scope={body['data']['data_scope']}")
    assert status == 200 and body["data"]["data_scope"] == 2
    print("  ✅ 越权（放宽）403/61002 且库未变；收紧允许；超管不受限")


def acceptance5():
    section("验收 ⑤：部门 parent_id 变更后，另一位用户的列表在 1 次请求内变为新范围（epoch 生效）")
    token = login("b3_childmgr", USER_PASSWORD)
    print(f"  b3_childmgr 范围=3（本部门及以下，部门=B3 研发部）")
    status, body, rows = user_page(token, "① 变更前 GET /system/user/page")
    assert status == 200 and rows is not None
    before_names = {name for name, _ in rows}
    assert before_names == {"b3_mgr", "b3_dev1", "b3_childmgr", "b3_selfonly", "b3_be1", "b3_be2"}, before_names
    keys = [k for k in redis_cmd("--scan", "--pattern", "cloudpivot:perm:user_scope:*").splitlines() if k]
    epoch_before = redis_cmd("get", "cloudpivot:perm:user_scope_epoch")
    print(f"  Redis: epoch={epoch_before or '(未设置)'}  perm:user_scope:* 键数={len(keys)}")
    assert keys, "缓存未被写入，说明第 1 次请求没有建立缓存（验收前提不成立）"

    admin = login("admin", ADMIN_PASSWORD)
    sales = psql("select id from sys_dept where dept_code='B3-SALES'")[0][0]
    be = psql("select id from sys_dept where dept_code='B3-RD-BE'")[0][0]
    status, body = call("GET", f"/system/dept/{sales}", admin)
    node = body["data"]
    print(f"  搬迁前 B3-SALES.parent_id = {node['parent_id']}")
    payload = {"dept_code": node["dept_code"], "dept_name": node["dept_name"], "parent_id": be,
               "leader_id": node.get("leader_id") or None, "phone": None, "email": None, "status": 1, "sort_order": 0}
    status, body = call("PUT", f"/system/dept/{sales}", admin, payload)
    print(f"  PUT /system/dept/{sales} parent_id → B3 后端组：HTTP {status} code={body.get('code')}")
    assert status == 200 and body.get("code") == 0, body
    epoch_after = redis_cmd("get", "cloudpivot:perm:user_scope_epoch")
    print(f"  Redis: epoch={epoch_after}（+1 = bump_epoch 已执行）")
    assert epoch_after and epoch_after != (epoch_before or "0"), (epoch_before, epoch_after)

    # **同一个 token、紧接着的下一次请求**，不等 TTL
    status, body, rows = user_page(token, "② 变更后第一次 GET（同一 token，未等待 TTL）")
    after_names = {name for name, _ in rows}
    print(f"  新增可见：{sorted(after_names - before_names)}；失去可见：{sorted(before_names - after_names)}")
    assert after_names == before_names | {"b3_sales1"}, after_names
    print("  ✅ parent_id 变更后，同一 token 的下一次请求即包含新子部门（销售部 1 人）——epoch 全量失效生效")

    # 复原：把 B3-SALES 搬回 B3 总部
    hq = psql("select id from sys_dept where dept_code='B3-HQ'")[0][0]
    payload["parent_id"] = hq
    status, body = call("PUT", f"/system/dept/{sales}", admin, payload)
    print(f"  复原 parent_id → B3 总部：HTTP {status} code={body.get('code')}；epoch={redis_cmd('get', 'cloudpivot:perm:user_scope_epoch')}")


def acceptance6():
    section("验收 ⑥：清空 Redis 缓存后列表仍正确（回退查库）")
    token = login("b3_mgr", USER_PASSWORD)
    status, body, rows = user_page(token, "① 清空前的列表（应有 scope 缓存）")
    scope_keys = [k for k in redis_cmd("--scan", "--pattern", "cloudpivot:perm:user_scope:*").splitlines() if k]
    print(f"  Redis cloudpivot:perm:user_scope:* 键数 = {len(scope_keys)}：{scope_keys}")
    assert scope_keys, "scope 缓存未被写入（验收前提不成立）"

    # ⑥a 只清「数据权限」的那一族键（保留会话/黑名单键，隔离出数据权限层的行为）
    print(f"  逐个 DEL scope 键 → {[redis_cmd('del', *scope_keys)]}")
    print(f"  再 DEL epoch 键 → {redis_cmd('del', 'cloudpivot:perm:user_scope_epoch')}")
    left = [k for k in redis_cmd("--scan", "--pattern", "cloudpivot:perm:user_scope*").splitlines() if k]
    print(f"  清空后 cloudpivot:perm:user_scope* 键数 = {len(left)}")
    assert not left
    status, body, rows2 = user_page(token, "② scope 缓存清空后同一 token 的列表")
    print(f"  行数 清空前={len(rows)} 清空后={len(rows2)}")
    assert status == 200 and rows2 is not None
    assert {n for n, _ in rows} == {n for n, _ in rows2}, (rows, rows2)
    refilled = [k for k in redis_cmd("--scan", "--pattern", "cloudpivot:perm:user_scope:*").splitlines() if k]
    print(f"  请求后回填的 scope 键数 = {len(refilled)}：{refilled}")
    assert refilled, "未命中后应回填缓存"
    print("  ✅ scope 缓存清空后仍回本部门 4 行（cache-aside 未命中 → 查库重算 → 回填）")

    # ⑥b 整库清空（FLUSHDB，等价「Redis 数据全丢」）：再取一次列表
    print(f"\n  redis-cli -n 6 FLUSHDB → {redis_cmd('flushdb')}")
    print(f"  清空后 db6 dbsize = {redis_cmd('dbsize')}")
    status, body, rows3 = user_page(token, "③ 整库清空后同一 token 的列表")
    if status != 200:
        print("  （认证层 fail-closed：会话/吊销键随整库清空一并消失 → 既有取向，非数据权限问题）")
        token = login("b3_mgr", USER_PASSWORD)
        status, body, rows3 = user_page(token, "④ 重新登录后的列表")
    assert status == 200 and rows3 is not None
    assert {n for n, _ in rows3} == {n for n, _ in rows}, (rows, rows3)
    print("  ✅ Redis 整库清空后列表仍为同 4 行（数据权限不依赖缓存在不在）")


def acceptance7():
    section("验收 ⑦（交付物 3）：部门树按 scope 裁剪 + 越权部门过滤不穿透")
    mgr = login("b3_mgr", USER_PASSWORD)
    admin = login("admin", ADMIN_PASSWORD)

    def tree(token, label):
        status, body = call("GET", "/system/dept/tree", token)
        assert status == 200 and body.get("code") == 0, body
        names = []

        def walk(nodes, depth=0):
            for node in nodes:
                names.append(("  " * depth) + node["dept_name"])
                walk(node.get("children") or [], depth + 1)
        walk(body["data"])
        print(f"  {label}: {names}")
        return names

    admin_tree = tree(admin, "超管（范围=ALL，走全量树缓存）")
    mgr_tree = tree(mgr, "b3_mgr（范围=本部门，按 scope 裁剪）")
    assert len(admin_tree) == 4, admin_tree
    assert mgr_tree == ["B3 研发部"], mgr_tree
    print("  ✅ 超管看到 4 个部门；部门主管只看到「B3 研发部」一行，且被提升为树根（上级不可见）")

    # 越权部门过滤：显式传看不见的部门 id → 业务过滤与 scope 是 AND，不会穿透
    sales = psql("select id from sys_dept where dept_code='B3-SALES'")[0][0]
    status, body, rows = user_page(mgr, f"b3_mgr 带 dept_id={sales[:8]}…（销售部，看不见）", f"&dept_id={sales}")
    assert status == 200 and body["data"]["total"] == 0, body["data"]
    print("  ✅ 显式传越权部门 id → total=0（scope 片段与业务条件是 AND，过滤条件不能绕过范围）")

    # 角色列表回显 data_scope（RoleRes 暴露字段）
    status, body = call("GET", "/system/role/page?page=1&page_size=5&role_code=B3_DEPT", admin)
    item = body["data"]["list"][0]
    print(f"  GET /system/role/page → role_code={item['role_code']} data_scope={item['data_scope']}（RoleRes 已下发该字段）")
    assert item["data_scope"] == 2




def admin_token():
    """登录 admin；若仍是环境变量里的初始口令，先完成首登改密（幂等）。"""
    _init_passwords()
    initial = INITIAL_ADMIN_PASSWORD
    status, body = call("POST", "/system/auth/login", body={"username": "admin", "password": initial})
    if status == 200 and body.get("code") == 0:
        token = body["data"]["access_token"]
        status, changed = call("PUT", "/system/auth/password", token,
                               {"old_password": initial, "new_password": ADMIN_PASSWORD})
        print(f"[admin] 首登改密 → HTTP {status} code={changed.get('code')} msg={changed.get('msg')}")
    else:
        print("[admin] 初始口令已失效（此前已改密），直接使用新口令登录")
    token = login("admin", ADMIN_PASSWORD)
    return token, {}


# ==================== 夹具 ====================
DEPTS = [
    ("B3-HQ", "B3 总部", None),
    ("B3-RD", "B3 研发部", "B3-HQ"),
    ("B3-RD-BE", "B3 后端组", "B3-RD"),
    ("B3-SALES", "B3 销售部", "B3-HQ"),
]
ROLES = [
    # code, name, data_scope, 需要的权限码
    ("B3_ALL", "B3 全部数据", 1, ["system:user:list", "system:dept:list"]),
    ("B3_DEPT", "B3 本部门（验收②）", 2, ["system:user:list", "system:dept:list", "system:role:update", "system:role:list"]),
    ("B3_CHILD", "B3 本部门及以下", 3, ["system:user:list", "system:dept:list"]),
    ("B3_SELF", "B3 仅本人", 4, ["system:user:list"]),
]
USERS = [
    # username, dept, role_code
    ("b3_mgr", "B3-RD", "B3_DEPT"),          # 部门主管：本部门 = 研发部（2 人：b3_mgr + b3_dev1）
    ("b3_dev1", "B3-RD", "B3_SELF"),
    ("b3_be1", "B3-RD-BE", "B3_SELF"),
    ("b3_be2", "B3-RD-BE", "B3_SELF"),
    ("b3_sales1", "B3-SALES", "B3_SELF"),
    ("b3_hq1", "B3-HQ", "B3_SELF"),
    ("b3_childmgr", "B3-RD", "B3_CHILD"),    # 本部门及以下：研发部 + 后端组
    ("b3_selfonly", "B3-RD", "B3_SELF"),
]


def seed():
    token, _ = admin_token()
    dept_ids = {}
    for code, name, parent in DEPTS:
        status, body = call("GET", f"/system/dept/tree", token)
        existing = {}

        def walk(nodes):
            for node in nodes:
                existing[node["dept_code"]] = node["id"]
                walk(node.get("children") or [])
        walk(body["data"])
        if code in existing:
            dept_ids[code] = existing[code]
            print(f"[dept] 已存在 {code} → {existing[code]}")
            continue
        payload = {"dept_code": code, "dept_name": name, "parent_id": dept_ids.get(parent) if parent else None, "status": 1, "sort_order": 0}
        status, body = call("POST", "/system/dept", token, payload)
        if status != 200 or body.get("code") != 0:
            raise SystemExit(f"建部门失败 {code}: {status} {body}")
        dept_ids[code] = body["data"]["id"]
        print(f"[dept] + {code} → {body['data']['id']} (parent={payload['parent_id']})")

    # 菜单 id 映射（auth_code → id）
    status, body = call("GET", "/system/menu/tree", token)
    code_to_menu = {}

    def walk_menu(nodes):
        for node in nodes:
            if node.get("auth_code"):
                code_to_menu[node["auth_code"]] = node["id"]
            walk_menu(node.get("children") or [])
    walk_menu(body["data"])

    role_ids = {}
    for code, name, data_scope, perms in ROLES:
        status, body = call("GET", "/system/role/page?page=1&page_size=100&role_code=" + code, token)
        hit = [item for item in body["data"]["list"] if item["role_code"] == code]
        if hit:
            role_ids[code] = hit[0]["id"]
            print(f"[role] 已存在 {code} → {hit[0]['id']} data_scope={hit[0]['data_scope']}")
        else:
            payload = {"role_code": code, "role_name": name, "description": "", "status": 1, "sort_order": 0, "remark": "", "data_scope": data_scope}
            status, body = call("POST", "/system/role", token, payload)
            if status != 200 or body.get("code") != 0:
                raise SystemExit(f"建角色失败 {code}: {status} {body}")
            role_ids[code] = body["data"]["id"]
            print(f"[role] + {code} data_scope={body['data']['data_scope']} → {body['data']['id']}")
        menu_ids = [code_to_menu[p] for p in perms if p in code_to_menu]
        missing = [p for p in perms if p not in code_to_menu]
        if missing:
            raise SystemExit(f"权限点不存在于种子: {missing}")
        status, body = call("PUT", f"/system/role/{role_ids[code]}/menus", token, {"menu_ids": menu_ids})
        print(f"[role] {code} 授权 {len(menu_ids)} 个菜单 → {status} {body.get('code')}")

    for username, dept_code, role_code in USERS:
        status, body = call("GET", f"/system/user/page?page=1&page_size=100&username={username}", token)
        hit = [item for item in body["data"]["list"] if item["username"] == username]
        if hit:
            # 幂等：已存在则把口令重置为本脚本推导的值，保证重复执行后仍能登录
            user_id = hit[0]["id"]
            status, body = call("PATCH", f"/system/user/{user_id}/password", token, {"password": USER_PASSWORD})
            print(f"[user] 已存在 {username} → {user_id}（口令已对齐：{status} {body.get('code')}）")
            continue
        payload = {
            "username": username, "password": USER_PASSWORD, "real_name": username.upper(),
            "email": f"{username}@b3.local", "mobile": "13900000000",
            "dept_id": dept_ids[dept_code], "status": 1, "remark": "", "role_ids": [role_ids[role_code]],
        }
        status, body = call("POST", "/system/user", token, payload)
        if status != 200 or body.get("code") != 0:
            raise SystemExit(f"建用户失败 {username}: {status} {body}")
        print(f"[user] + {username} dept={dept_code} role={role_code} → {body['data']['id']}")

    print("\n=== 夹具完成 ===")
    for row in psql("select r.role_code,r.data_scope,count(ur.user_id) from sys_role r left join sys_user_role ur on ur.role_id=r.id where r.role_code like 'B3%' group by 1,2 order by 1"):
        print("  role", row)




STEP_FUNCS = {"2": acceptance2, "3": acceptance3, "4": acceptance4,
              "5": acceptance5, "6": acceptance6, "7": acceptance7}


def run_steps():
    for step in ["2", "3", "4", "5", "6", "7"]:
        STEP_FUNCS[step]()
    print("\n全部断言通过。")


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else "verify"
    if command == "seed":
        seed()
    elif command == "verify":
        run_steps()
    elif command == "all":
        seed()
        run_steps()
    else:
        raise SystemExit(f"未知命令 {command}（seed / verify / all）")
