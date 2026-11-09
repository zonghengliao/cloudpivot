#!/usr/bin/env python3
"""E2E-13 平台功能域演练 · 公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）

口径（与 e2e09~e2e12 一致）：
- 全部走真实 HTTP（urllib，无第三方依赖）+ psql 直查；
- 前缀 E2E13 / e2e13_ 标识本卡数据，收尾按外键顺序自清；
- 断言逐条落 /tmp/e2e13/asserts.log；状态落 state.json 供 a/b 两段接力。
"""
import hashlib
import json
import os
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from xml.sax.saxutils import escape

WORK = os.environ.get("E2E13_WORK", "/tmp/e2e13")
RAW = os.path.join(WORK, "raw")
LOG = os.path.join(WORK, "asserts.log")
STATE = os.path.join(WORK, "state.json")
BASE = os.environ.get("E2E13_BASE", "http://127.0.0.1:8033")
ENV_PATH = os.environ.get("E2E13_ENV", "/etc/cloudpivot/cloudpivot-dev.env")
PREFIX = "E2E13"

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
BASE_PW = "E2e13@Passw0rd"

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


def req(method, path, token=None, body=None, timeout=180, name=None, raw_body=None, ctype=None):
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


def _headers_lower(headers):
    return {str(k).lower(): v for k, v in headers.items()}


def req_bytes(path, token=None, method="GET", body=None, ctype=None, timeout=300, name=None):
    """返回 (status, headers_lower, bytes, saved_path)。二进制接口用。"""
    safe_path = urllib.parse.quote(path, safe="/?&=:+,%-_.~")
    url = BASE + safe_path if path.startswith("/") else BASE + "/" + safe_path
    data = None
    if body is not None:
        data = body if isinstance(body, bytes) else json.dumps(body, ensure_ascii=False).encode()
    request = urllib.request.Request(url, data=data, method=method)
    if token:
        request.add_header("Authorization", "Bearer " + token)
    if data is not None:
        request.add_header("Content-Type", ctype or "application/json")
    try:
        with urllib.request.urlopen(request, timeout=timeout) as resp:
            payload = resp.read()
            status = resp.status
            headers = _headers_lower(resp.headers)
    except urllib.error.HTTPError as exc:
        payload = exc.read()
        status = exc.code
        headers = _headers_lower(exc.headers)
    except Exception as exc:  # noqa: BLE001
        payload = json.dumps({"transport_error": str(exc)}).encode()
        status = 0
        headers = {}
    saved = None
    if name:
        safe = "".join(ch if ch.isalnum() or ch in "-_." else "_" for ch in str(name))[:80]
        if status == 200:
            saved = os.path.join(RAW, safe)
            with open(saved, "wb") as fh:
                fh.write(payload)
        else:
            _save_raw(safe + "_err", payload.decode("utf-8", "replace"))
    return status, headers, payload, saved


def multipart(fields, file_field, file_name, content, mime="application/octet-stream",
              extra_files=None):
    boundary = "----E2E13Boundary4a7c2e9b"
    out = []
    for key, val in (fields or {}).items():
        out.append(f"--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{val}\r\n".encode())
    files = [(file_field, file_name, content, mime)]
    for item in extra_files or []:
        files.append(item)
    for fname, fpath, fbytes, fmime in files:
        out.append(
            f"--{boundary}\r\nContent-Disposition: form-data; name=\"{fname}\"; filename=\"{fpath}\"\r\n"
            f"Content-Type: {fmime}\r\n\r\n".encode() + fbytes + b"\r\n"
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
def psql(sql, timeout=60):
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


def one(sql):
    out = psql(sql)
    return out.splitlines()[0] if out and not out.startswith("<ERR") else out


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


def merge_state(**kwargs):
    st = load_state()
    st.update(kwargs)
    save_state(st)
    return st


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


def mk_role(admin, code, name, sort=961, data_scope=4):
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
    if uid and role_ids:
        plan = "'" + "','".join(str(r) for r in role_ids) + "'"
        psql("delete from sys_user_role where user_id='%s'::uuid and role_id in "
             "(select id from sys_role where role_code like '%s%%')" % (uid, PREFIX))
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


# ---------- xlsx（纯标准库，与后端 calamine 兼容） ----------
CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>"""

ROOT_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

WORKBOOK = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="数据" sheetId="1" r:id="rId1"/></sheets>
</workbook>"""

WORKBOOK_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
</Relationships>"""


def _column_name(index):
    name = ""
    index += 1
    while index:
        index, rem = divmod(index - 1, 26)
        name = chr(ord("A") + rem) + name
    return name


def make_xlsx(headers, rows, sheet_name="数据"):
    """生成最小合法 xlsx 字节串（全部走 sharedStrings，calamine 可读）。"""
    strings = []
    index_of = {}

    def sid(value):
        if value not in index_of:
            index_of[value] = len(strings)
            strings.append(value)
        return index_of[value]

    row_xml = []
    cells = "".join(f'<c r="{_column_name(c)}1" t="s"><v>{sid(h)}</v></c>' for c, h in enumerate(headers))
    row_xml.append(f'<row r="1">{cells}</row>')
    for r, row in enumerate(rows, start=2):
        cells = []
        for c, value in enumerate(row):
            if value is None or value == "":
                continue
            cells.append(f'<c r="{_column_name(c)}{r}" t="s"><v>{sid(str(value))}</v></c>')
        row_xml.append(f'<row r="{r}">{"".join(cells)}</row>')
    total = len(strings)
    shared = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
              f'<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
              f'count="{total}" uniqueCount="{total}">'
              + "".join(f"<si><t>{escape(s)}</t></si>" for s in strings) + "</sst>")
    sheet = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
             f'<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
             f'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
             f'<sheetData>{"".join(row_xml)}</sheetData></worksheet>')
    wb = WORKBOOK.replace('name="数据"', f'name="{sheet_name}"')
    import io
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml", CONTENT_TYPES)
        zf.writestr("_rels/.rels", ROOT_RELS)
        zf.writestr("xl/workbook.xml", wb)
        zf.writestr("xl/_rels/workbook.xml.rels", WORKBOOK_RELS)
        zf.writestr("xl/sharedStrings.xml", shared)
        zf.writestr("xl/worksheets/sheet1.xml", sheet)
    return buf.getvalue()


def read_xlsx(path_or_bytes):
    """读回 xlsx：返回 {sheet_name: [[cell,...], ...]}（只支持前端/服务端产物里的 inline/shared 字符串与数字）。"""
    import io
    import re

    data = path_or_bytes
    if isinstance(path_or_bytes, str):
        with open(path_or_bytes, "rb") as fh:
            data = fh.read()
    out = {}
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        names = zf.namelist()
        shared = []
        if "xl/sharedStrings.xml" in names:
            xml = zf.read("xl/sharedStrings.xml").decode("utf-8", "replace")
            for si in re.findall(r"<si>(.*?)</si>", xml, re.S):
                texts = re.findall(r"<t[^>]*>(.*?)</t>", si, re.S)
                shared.append("".join(texts))
        wb = zf.read("xl/workbook.xml").decode("utf-8", "replace")
        sheet_names = re.findall(r'<sheet[^>]*name="([^"]+)"', wb)
        sheets = sorted([n for n in names if n.startswith("xl/worksheets/sheet")])
        for idx, sname in enumerate(sheets):
            xml = zf.read(sname).decode("utf-8", "replace")
            rows = []
            for row_xml in re.findall(r"<row[^>]*>(.*?)</row>", xml, re.S):
                cells = {}
                for cell in re.findall(r"<c ([^>]*)>(.*?)</c>", row_xml, re.S):
                    attrs, inner = cell
                    ref = re.search(r'r="([A-Z]+)(\d+)"', attrs)
                    if not ref:
                        continue
                    col = ref.group(1)
                    v = re.search(r"<v>(.*?)</v>", inner, re.S)
                    t = re.search(r't="([^"]+)"', attrs)
                    if v is None:
                        inline = re.search(r"<is>.*?<t[^>]*>(.*?)</t>", inner, re.S)
                        val = inline.group(1) if inline else ""
                    elif t and t.group(1) == "s":
                        try:
                            val = shared[int(v.group(1))]
                        except Exception:  # noqa: BLE001
                            val = ""
                    else:
                        val = v.group(1)
                    cells[col] = val
                if cells:
                    width = 0
                    for col in cells:
                        w = 0
                        for ch in col:
                            w = w * 26 + (ord(ch) - 64)
                        width = max(width, w)
                    rows.append([cells.get(_column_name(i), "") for i in range(width)])
                else:
                    rows.append([])
            key = sheet_names[idx] if idx < len(sheet_names) else f"sheet{idx + 1}"
            out[key] = rows
    return out


def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


# ---------- 轮询 ----------
def poll(fn, ok, timeout=120, interval=1.0, label=""):
    """反复调用 fn() 直到 ok(result) 为真或超时；返回最后一次结果。"""
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = fn()
        if ok(last):
            return last
        time.sleep(interval)
    _log(f"TIMEOUT\t{CUR}\t{label}\t{json.dumps(last, ensure_ascii=False, default=str)[:300]}")
    return last
