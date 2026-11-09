#!/usr/bin/env python3
"""E2E-11 任务+协作域演练 · 公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）

口径（与 e2e09/e2e10 一致）：
- 全部走真实 HTTP（urllib，无第三方依赖）+ psql 直查；
- 前缀 E2E12 / e2e12_ 标识本卡数据，收尾按外键顺序自清；
- 断言逐条落 /tmp/e2e12/asserts.log；状态落 state.json 供 a/b 两段接力。
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

WORK = os.environ.get("E2E12_WORK", "/tmp/e2e12")
RAW = os.path.join(WORK, "raw")
LOG = os.path.join(WORK, "asserts.log")
STATE = os.path.join(WORK, "state.json")
BASE = os.environ.get("E2E12_BASE", "http://127.0.0.1:8033")
ENV_PATH = os.environ.get("E2E12_ENV", "/etc/cloudpivot/cloudpivot-dev.env")

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
    boundary = "----E2E12Boundary7d9f3a1c"
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
             "(select id from sys_role where role_code like 'E2E12%%')" % uid)
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


# ---------- 二进制下载 / 上传（PDF、附件本体） ----------
def _headers_lower(headers):
    return {str(k).lower(): v for k, v in headers.items()}


def req_bytes(path, token=None, method="GET", body=None, ctype=None, timeout=180, name=None):
    """返回 (status, headers_lower, bytes, saved_path)。二进制接口（PDF / 附件下载）用。"""
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


def upload(token, doc_type, doc_id, file_name, content, file_role=None, remark=None, mime=None, timeout=180, name=None):
    """POST /api/business/attachment/upload（真实 multipart）。"""
    fields = {"doc_type": doc_type, "doc_id": doc_id}
    if file_role:
        fields["file_role"] = file_role
    if remark:
        fields["remark"] = remark
    raw, ctype = multipart(fields, "file", file_name, content, mime or guess_mime(file_name))
    st, headers, payload, _ = req_bytes("/api/business/attachment/upload", token,
                                       method="POST", body=raw, ctype=ctype,
                                       timeout=timeout, name=name)
    try:
        parsed = json.loads(payload.decode("utf-8", "replace"))
    except Exception:  # noqa: BLE001
        parsed = {"_raw": payload.decode("utf-8", "replace")[:1500]}
    if name:
        _save_raw(name, parsed)
    return st, parsed, headers


def guess_mime(name):
    ext = name.rsplit(".", 1)[-1].lower() if "." in name else ""
    return {
        "pdf": "application/pdf", "png": "image/png", "jpg": "image/jpeg", "jpeg": "image/jpeg",
        "gif": "image/gif", "webp": "image/webp", "bmp": "image/bmp",
        "docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "zip": "application/zip", "txt": "text/plain", "ofd": "application/ofd", "xml": "text/xml",
    }.get(ext, "application/octet-stream")


# ---------- 造测试文件（纯标准库） ----------
def make_pdf(text="E2E12 sample", pages=1):
    """生成带 xref 的合法单页/多页 PDF（pdf.js 与 pdftotext 都能读）。"""
    objs = []
    kids = []
    page_objs = []
    font_id = 3
    next_id = 4
    for i in range(pages):
        pid = next_id
        next_id += 1
        cid = next_id
        next_id += 1
        kids.append(pid)
        stream = f"BT /F1 14 Tf 72 720 Td ({text} p{i + 1}) Tj ET"
        page_objs.append((pid, cid, stream))
    objs.append((1, "<< /Type /Catalog /Pages 2 0 R >>"))
    objs.append((2, f"<< /Type /Pages /Kids [{' '.join(f'{k} 0 R' for k in kids)}] /Count {len(kids)} >>"))
    objs.append((font_id, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"))
    for pid, cid, stream in page_objs:
        objs.append((pid, f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] "
                          f"/Resources << /Font << /F1 {font_id} 0 R >> >> /Contents {cid} 0 R >>"))
        objs.append((cid, f"<< /Length {len(stream)} >>\nstream\n{stream}\nendstream"))
    objs.sort()
    out = bytearray(b"%PDF-1.4\n")
    offsets = {}
    for oid, body in objs:
        offsets[oid] = len(out)
        out += f"{oid} 0 obj\n{body}\nendobj\n".encode()
    xref_at = len(out)
    total = max(offsets) + 1
    out += f"xref\n0 {total}\n".encode()
    out += b"0000000000 65535 f \n"
    for oid in range(1, total):
        out += f"{offsets[oid]:010d} 00000 n \n".encode()
    out += f"trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n".encode()
    return bytes(out)


def make_png(w=60, h=40, rgb=(30, 120, 200)):
    """最小合法 PNG（8bit RGB）。"""
    import struct
    import zlib

    raw = b""
    row = bytes(rgb) * w
    for _ in range(h):
        raw += b"\x00" + row

    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def make_docx(text="E2E12 office"):
    """最小合法 docx（OOXML zip）。"""
    import io
    import zipfile

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml",
                    '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
                    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
                    '<Default Extension="xml" ContentType="application/xml"/>'
                    '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
        zf.writestr("_rels/.rels",
                    '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                    '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>')
        zf.writestr("word/document.xml",
                    '<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
                    f'<w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>')
    return buf.getvalue()


def make_zip(text="E2E12 zip"):
    import io
    import zipfile

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("readme.txt", text)
    return buf.getvalue()


# ---------- PDF 取证 ----------
def sh(cmd, timeout=90):
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return proc.returncode, proc.stdout, proc.stderr
    except Exception as exc:  # noqa: BLE001
        return -1, "", str(exc)


def pdf_text(path, mode="-raw"):
    rc, out, err = sh(["pdftotext", mode, path, "-"])
    return out if rc == 0 else f"<pdftotext err {err[:200]}>"


def pdf_info(path):
    rc, out, _ = sh(["pdfinfo", path])
    return out if rc == 0 else ""


def pdf_fonts(path):
    rc, out, _ = sh(["pdffonts", path])
    return out if rc == 0 else ""


def pdf_pages(path):
    for line in pdf_info(path).splitlines():
        if line.startswith("Pages:"):
            try:
                return int(line.split(":", 1)[1].strip())
            except ValueError:
                return 0
    return 0


def pdf_png(path, page, tag):
    out = os.path.join(RAW, f"{tag}-p{page}.png")
    sh(["pdftoppm", "-r", "100", "-png", "-f", str(page), "-l", str(page), path, out[:-4]])
    for cand in (out, out[:-4] + "-" + str(page) + ".png", out[:-4] + f"-{page:02d}.png"):
        if os.path.exists(cand):
            return cand
    return out


def squash(text):
    import re
    return re.sub(r"\s+", "", text or "")
