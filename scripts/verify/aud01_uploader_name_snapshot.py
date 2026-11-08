#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""AUD-01 真机证据：上传人姓名**快照**（HTTP 层）

前置：一个已启动的隔离实例（**不要打生产库**），例如：

```bash
P14_DB=cp_aud01_e2e P14_PORT=18098 bash scripts/verify/p14_instance.sh start
P14_BASE=http://127.0.0.1:18098 P14_DB_NAME=cp_aud01_e2e \\
  python3 scripts/verify/p14_attachment_e2e.py          # 先造夹具（单据 + 存量附件）
AUD01_BASE=http://127.0.0.1:18098 AUD01_DB_NAME=cp_aud01_e2e \\
  python3 scripts/verify/aud01_uploader_name_snapshot.py
```

在隔离实例（默认 127.0.0.1:18098 / 库 cp_aud01_e2e）上验证三件事：

  ② 上传一个新附件 → API 落库的 `uploader_name` 有值（不是 NULL、不是 UUID）
  ④ 附件列表 API 每行带 `uploader_name`
  ⑤ 把上传人改名后再看旧附件 → 列表仍返回**改名前的名字**（快照生效），
     而改名后新上传的附件显示新名字（证明快照取自「上传当时」）

用法：python3 aud01_http_snapshot_proof.py
退出码：0 全过 / 1 有失败
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("AUD01_BASE", "http://127.0.0.1:18098")
DB = os.environ.get("AUD01_DB_NAME", "cp_aud01_e2e")
DOC_PO = "d2140102-0000-4000-8000-000000000001"      # 采购单（e2e 夹具，已有 1 个附件）
DOC_IN = "d2140103-0000-4000-8000-000000000001"      # 收货单（e2e 夹具，已有 1 个附件）
ADMIN = os.environ.get("AUD01_ADMIN_USER", "admin")
CANDIDATES = [os.environ.get("AUD01_NEW_PASS", "P14@Passw0rd!"), "Admin@123456"]
NEW_NAME = "超级管理员（改名后）"
OLD_NAME = "超级管理员"

fails = []


def ok(cond, msg):
    print(("[PASS] " if cond else "[FAIL] ") + msg)
    if not cond:
        fails.append(msg)


def http(method, path, token=None, body=None):
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read()
            return resp.status, (json.loads(raw.decode()) if raw else {})
    except urllib.error.HTTPError as err:
        raw = err.read()
        try:
            return err.code, json.loads(raw.decode())
        except Exception:
            return err.code, {"raw": raw.decode(errors="replace")}


def login():
    for pwd in CANDIDATES:
        st, resp = http("POST", "/api/system/auth/login", body={"username": ADMIN, "password": pwd})
        token = ((resp or {}).get("data") or {}).get("access_token")
        if st == 200 and token:
            print(f"    · 登录成功（口令候选项命中，状态 {st}）")
            return token
    raise RuntimeError("登录失败：admin 口令候选项都不通")


def psql(sql):
    env = dict(os.environ, PGPASSWORD=os.environ.get("AUD01_DB_PASSWORD", "alun"))
    out = subprocess.run(
        ["psql", "-X", "-q", "-t", "-A", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-c", sql],
        capture_output=True, text=True, env=env, check=True,
    )
    return out.stdout.strip()


def list_attachments(token, doc_type, doc_id):
    st, resp = http("GET", f"/api/business/attachment/list?doc_type={doc_type}&doc_id={doc_id}", token)
    if st != 200:
        raise RuntimeError(f"列表失败 {st} {resp}")
    return (resp.get("data") or {}).get("list") or []


def upload(token, doc_type, doc_id, name, payload):
    boundary = "----aud01boundary"
    parts = []
    for field, value in (("doc_type", doc_type), ("doc_id", doc_id), ("file_role", "OTHER")):
        parts.append(
            f"--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\n{value}\r\n".encode()
        )
    parts.append(
        f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n"
        f"Content-Type: image/png\r\n\r\n".encode() + payload + b"\r\n"
    )
    parts.append(f"--{boundary}--\r\n".encode())
    body = b"".join(parts)
    req = urllib.request.Request(
        BASE + "/api/business/attachment/upload",
        data=body,
        headers={"Content-Type": f"multipart/form-data; boundary={boundary}", "Authorization": "Bearer " + token},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as err:
        return err.code, json.loads(err.read().decode() or "{}")


def main():
    token = login()

    print("== ④ 列表 API 每行带 uploader_name（存量行） ==")
    rows = list_attachments(token, "PURCHASE_ORDER", DOC_PO)
    ok(bool(rows), f"采购单列表非空（{len(rows)} 条）")
    before = rows[0]
    ok("uploader_name" in before, "响应字段里有 uploader_name")
    ok(bool(before.get("uploader_name")), f"uploader_name 有值：{before.get('uploader_name')!r}")
    ok(before.get("uploader_name") == OLD_NAME, f"存量行显示当前姓名 {OLD_NAME!r}")
    old_name = before.get("uploader_name")

    print("== ⑤ 上传人改名 → 旧附件仍显示改名前的名字 ==")
    psql(f"UPDATE sys_user SET real_name = '{NEW_NAME}' WHERE username = '{ADMIN}'")
    try:
        rows = list_attachments(token, "PURCHASE_ORDER", DOC_PO)
        after_rename = rows[0].get("uploader_name")
        ok(after_rename == old_name,
           f"改名后旧附件仍显示 {old_name!r}（实际 {after_rename!r}）——快照生效")
        ok(psql(f"SELECT real_name FROM sys_user WHERE username = '{ADMIN}'") == NEW_NAME,
           "sys_user 里的现值确实已改成新名字（对照）")

        print("== ② 改名后新上传一个附件 → 快照取的是新名字 ==")
        # 内容每轮不同（storage 按内容 hash 秒传，避免复用已有 sys_file）
        payload = b"\x89PNG\r\n\x1a\n" + os.urandom(64)
        status, resp = upload(token, "STOCK_IN", DOC_IN, "aud01-uploader-snapshot.png", payload)
        ok(status == 200, f"上传成功（HTTP {status}）")
        rows = list_attachments(token, "STOCK_IN", DOC_IN)
        new_row = next((r for r in rows if r.get("file_name") == "aud01-uploader-snapshot.png"), None)
        ok(new_row is not None, "新附件出现在列表里")
        if new_row:
            ok(new_row.get("uploader_name") == NEW_NAME,
               f"新附件显示上传当时的新名字 {NEW_NAME!r}（实际 {new_row.get('uploader_name')!r}）")
            ok(len(new_row.get("uploader_name") or "") != 36,
               f"uploader_name 不是 UUID（长度 {len(new_row.get('uploader_name') or '')}）")
    finally:
        psql(f"UPDATE sys_user SET real_name = '{OLD_NAME}' WHERE username = '{ADMIN}'")
        print("    · 已把上传人姓名复原")

    print("==================== 汇总 ====================")
    if fails:
        print(f"失败 {len(fails)} 项：")
        for item in fails:
            print("  - " + item)
        return 1
    print("全部通过（② 新上传有快照 / ④ 列表带 uploader_name / ⑤ 改名后旧名不变）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
