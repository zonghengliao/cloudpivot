#!/usr/bin/env python3
"""E2E-11 任务+协作域演练 · 公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）

口径（与 e2e09/e2e10 一致）：
- 全部走真实 HTTP（urllib，无第三方依赖）+ psql 直查；
- 前缀 E2E11 / e2e11_ 标识本卡数据，收尾按外键顺序自清；
- 断言逐条落 /tmp/e2e11/asserts.log；状态落 state.json 供 a/b 两段接力。
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

WORK = os.environ.get("E2E11_WORK", "/tmp/e2e11")
RAW = os.path.join(WORK, "raw")
LOG = os.path.join(WORK, "asserts.log")
STATE = os.path.join(WORK, "state.json")
BASE = os.environ.get("E2E11_BASE", "http://127.0.0.1:8033")
ENV_PATH = os.environ.get("E2E11_ENV", "/etc/cloudpivot/cloudpivot-dev.env")

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
BASE_PW = "E2e11@Passw0rd"

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
        STATS.setdefault(CUR, [0, 0])[0] += 1
        print(f"  ✅ {label}" + (f"｜{detail}" if detail else ""))
        _log(f"PASS\t{CUR}\t{label}" + (f"\t{detail}" if detail else ""))
    else:
        FAIL += 1
        STATS.setdefault(CUR, [0, 0])[1] += 1
        print(f"  ❌ {label}" + (f"｜{detail}" if detail else ""))
        _log(f"FAIL\t{CUR}\t{label}" + (f"\t{detail}" if detail else ""))
    return cond


def eq(label, actual, expect):
    return check(str(actual) == str(expect), label, f"实际={actual} 期望={expect}")


def ne(label, actual, unexpected):
    return check(str(actual) != str(unexpected), label, f"实际={actual} 不应={unexpected}")


def contains(label, text, needle):
    text = "" if text is None else str(text)
    hit = needle in text
    return check(hit, label, f"含「{needle}」" if hit else f"未含「{needle}」实际={text[:200]}")


def not_contains(label, text, needle):
    text = "" if text is None else str(text)
    hit = needle in text
    return check(not hit, label, f"不含「{needle}」" if not hit else f"意外含「{needle}」实际={text[:200]}")


# ---------- HTTP ----------
def _save_raw(name, text):
    if not name:
        return
    safe = "".join(ch if ch.isalnum() or ch in "-_." else "_" for ch in str(name))[:80]
    with open(os.path.join(RAW, f"{safe}.json"), "w", encoding="utf-8") as fh:
        fh.write(text if isinstance(text, str) else json.dumps(text, ensure_ascii=False, indent=2))


def req(method, path, token=None, body=None, timeout=60, name=None, raw_body=None, ctype=None):
    # 中文 / 空格关键词必须百分号编码（urllib 只接受 ASCII URL）
    safe_path = urllib.parse.quote(path, safe="/?&=:+,%-_.~")
    url = BASE + safe_path if path.startswith("/") else BASE + "/" + safe_path
    data = None
    if raw_body is not None:
        data = raw_body
    elif body is not None:
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(url, data=data, method=method)
    if token:
        request.add_header("Authorization", "Bearer " + token)
    if data is not None:
        request.add_header("Content-Type", ctype or "application/json")
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


def multipart(fields, file_field, file_name, content, mime="application/octet-stream"):
    """构造 multipart/form-data 请求体（标准库无现成实现）。"""
    boundary = "----E2E11Boundary7d9f3a1c"
    out = []
    for key, val in (fields or {}).items():
        out.append(f"--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{val}\r\n".encode())
    out.append(
        f"--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{file_name}\"\r\n"
        f"Content-Type: {mime}\r\n\r\n".encode()
        + content
        + b"\r\n"
    )
    out.append(f"--{boundary}--\r\n".encode())
    return b"".join(out), f"multipart/form-data; boundary={boundary}"


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
    data = body.get("data")
    return {} if data is None else data


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
        return "<ERR " + " / ".join(lines[:2])[:260] + ">"
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
        json.dump(state, fh, ensure_ascii=False, indent=1, default=str)
    os.replace(tmp, STATE)


# ---------- 登录 / 夹具 ----------
def login(username, password=None, new_password=BASE_PW, name=None):
    password = password or BASE_PW
    st, body = req("POST", "/api/system/auth/login", None,
                   {"username": username, "password": password}, name=name or f"login_{username}")
    if st != 200:
        return ""
    token = data_of(body).get("access_token") or ""
    if data_of(body).get("must_change_password") and token:
        st2, _ = req("PUT", "/api/system/auth/password", token,
                     {"old_password": password, "new_password": new_password},
                     name=(name or username) + "_chpwd")
        if st2 != 200:
            return ""
        st3, body3 = req("POST", "/api/system/auth/login", None,
                         {"username": username, "password": new_password},
                         name=(name or username) + "_relogin")
        if st3 != 200:
            return ""
        token = data_of(body3).get("access_token") or ""
    return token


def mk_role(admin, code, name, sort=960, data_scope=4):
    st, body = req("POST", "/api/system/role", admin,
                   {"role_code": code, "role_name": name, "status": 1, "sort_order": sort,
                    "data_scope": data_scope},
                   name="mkrole_" + code)
    rid = data_of(body).get("id")
    if not rid:
        rid = psql(f"select id from sys_role where role_code='{code}' and deleted_at is null limit 1")
        if str(rid).startswith("<ERR") or not rid:
            rid = None
    if rid:
        # 走真实「角色编辑」接口写 data_scope：直接改库不会失效权限/范围缓存
        st2, body2 = req("PUT", f"/api/system/role/{rid}", admin,
                         {"role_code": code, "role_name": name, "status": 1, "sort_order": sort,
                          "data_scope": data_scope}, name="setscope_" + code)
        if st2 != 200:
            psql(f"update sys_role set data_scope={int(data_scope)} where id='{rid}'::uuid")
    return rid, st, body


def mk_user(admin, username, real_name, role_ids, password=BASE_PW):
    st, body = req("POST", "/api/system/user", admin,
                   {"username": username, "password": password, "real_name": real_name,
                    "status": 1, "role_ids": role_ids}, name="mkuser_" + username)
    uid = data_of(body).get("id")
    if not uid:
        uid = psql(f"select id from sys_user where username='{username}' and deleted_at is null limit 1")
        if str(uid).startswith("<ERR") or not uid:
            uid = None
    if uid:
        plan = "'" + "','".join(str(r) for r in role_ids) + "'"
        psql("delete from sys_user_role where user_id='%s'::uuid and role_id in "
             "(select id from sys_role where role_code like 'E2E11%%')" % uid)
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

    payload = data_of(body)
    walk(payload if isinstance(payload, list) else (payload.get("tree") or payload.get("list") or []))
    ids = [item.get("id") for item in flat if item.get("auth_code") in codes]
    if not ids:
        return 0, body
    st2, body2 = req("PUT", f"/api/system/role/{rid}/menus", admin, {"menu_ids": ids},
                     name=f"grant_{str(rid)[:8]}")
    return len(ids), body2
