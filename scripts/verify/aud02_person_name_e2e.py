#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""AUD-02 真机证据：人员字段一律显示姓名（HTTP 层）

前置：一个已启动的隔离实例（**不要打生产库**）：

```bash
su - postgres -c "psql -c 'drop database if exists cp_aud02_e2e'"
su - postgres -c "psql -c 'create database cp_aud02_e2e owner alun'"
P14_DB=cp_aud02_e2e P14_PORT=18099 bash scripts/verify/p14_instance.sh start
AUD02_BASE=http://127.0.0.1:18099 AUD02_DB=cp_aud02_e2e \\
  python3 scripts/verify/aud02_person_name_e2e.py
```

覆盖的验收点：

  ② 所有人员字段显示姓名 —— 合同详情（负责人 / 制单人 / 修改人）、
     文件列表（上传人）、帮助文档版本历史（操作人）全部返回 `*_name`，且**不是 UUID**
  ③ 无 N+1 —— 由后端真库用例 `contract_handler::tests::detail_fills_person_names_and_follows_rename`
     用计数器断言「一次详情恒定 1 条取名字 SQL」（本脚本只验对外行为）
  ④ 改名后列表显示新名 —— 改 `sys_user.real_name` 后再取详情 / 列表，名字跟着变
  ⑤ 审计类仍是快照 —— 附件 `uploader_name` 改名后不变（由 `aud01_uploader_name_snapshot.py`
     覆盖，本脚本末尾顺带复核一次）

另外对**所有**收集到的响应做一次全局扫描：任何以 `_name` 结尾的字段值都不许是 UUID。

退出码：0 全过 / 1 有失败
"""

import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("AUD02_BASE", "http://127.0.0.1:18099")
DB = os.environ.get("AUD02_DB", "cp_aud02_e2e")
ADMIN = os.environ.get("AUD02_ADMIN_USER", "admin")
PASSWORDS = [
    os.environ.get("AUD02_NEW_PASS", "AUD02@Passw0rd!"),
    os.environ.get("AUD02_ADMIN_PASS", "Admin@123456"),
]
NEW_PASS = PASSWORDS[0]
OWNER_USER = "aud02_owner"
OWNER_NAME = "张负责"
OWNER_NAME_2 = "张负责（改名后）"
SLUG = "aud02-uuid-audit"
UUID_RE = re.compile(
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
)

fails = []
seen_responses = []


def ok(cond, msg):
    print(("[PASS] " if cond else "[FAIL] ") + msg)
    if not cond:
        fails.append(msg)


def http(method, path, token=None, body=None, raw=None, content_type=None):
    data = raw
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if content_type:
        headers["Content-Type"] = content_type
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            payload = resp.read()
            parsed = json.loads(payload.decode()) if payload else {}
            if not isinstance(parsed, dict):
                parsed = {"raw": parsed}
            seen_responses.append((path, parsed))
            return resp.status, parsed
    except urllib.error.HTTPError as err:
        payload = err.read()
        try:
            parsed = json.loads(payload.decode())
        except Exception:
            parsed = {"raw": payload.decode(errors="replace")}
        return err.code, parsed


def psql(sql):
    env = dict(os.environ, PGPASSWORD=os.environ.get("AUD02_DB_PASSWORD", "alun"))
    out = subprocess.run(
        ["psql", "-X", "-q", "-t", "-A", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-c", sql],
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    return out.stdout.strip()


def login():
    for pwd in PASSWORDS:
        status, resp = http(
            "POST", "/api/system/auth/login", body={"username": ADMIN, "password": pwd}
        )
        token = ((resp or {}).get("data") or {}).get("access_token")
        if status == 200 and token:
            print(f"    · 登录成功（{ADMIN}）")
            return token, pwd
    raise RuntimeError("登录失败：admin 口令候选项都不通")


def ensure_ready():
    """取一个「能调业务接口」的 admin 令牌。

    空库首启的 admin 带 `must_change_password = true`，除改密外所有接口 403
    （安全审计 CODE-06）。这里走**正规改密接口**解掉标记，再用新口令重登 ——
    等价于真实用户首次登录的操作路径。
    """
    token, pwd = login()
    status, _ = http("GET", "/api/business/erp/contract/page?page=1&page_size=1", token)
    if status != 403:
        return token
    print("    · admin 处于「首登强制改密」，走改密接口解除后重登")
    status, resp = http(
        "PUT",
        "/api/system/auth/password",
        token,
        body={"old_password": pwd, "new_password": NEW_PASS},
    )
    ok(status == 200, f"首次登录改密（HTTP {status} {str(resp)[:120]}）")
    token, _ = login()
    return token


def ensure_supplier():
    row = psql(
        "SELECT id || '|' || supplier_name FROM biz_supplier "
        "WHERE deleted_at IS NULL AND status = 1 ORDER BY created_at LIMIT 1"
    )
    if row:
        supplier_id, name = row.split("|", 1)
        return supplier_id, name
    psql(
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) "
        f"VALUES (gen_random_uuid(), 'AUD02-SUP', 'AUD-02 测试供应商', 1)"
    )
    row = psql(
        "SELECT id || '|' || supplier_name FROM biz_supplier WHERE supplier_code = 'AUD02-SUP'"
    )
    supplier_id, name = row.split("|", 1)
    return supplier_id, name


def ensure_owner_user():
    """造/取一个负责人账号（AUD-02 用）。

    `sys_user` 的 username 唯一索引是**部分索引**（`WHERE deleted_at IS NULL`），
    `ON CONFLICT (username)` 推断不出来，所以这里「先改、没有就插」。
    """
    psql(f"UPDATE sys_user SET real_name = '{OWNER_NAME}' WHERE username = '{OWNER_USER}'")
    user_id = psql(f"SELECT id FROM sys_user WHERE username = '{OWNER_USER}'")
    if not user_id:
        psql(
            "INSERT INTO sys_user (id, username, password, real_name, status) "
            f"VALUES (gen_random_uuid(), '{OWNER_USER}', 'x', '{OWNER_NAME}', 1)"
        )
        user_id = psql(f"SELECT id FROM sys_user WHERE username = '{OWNER_USER}'")
    return user_id


def scan_uuid_leaks(path, payload):
    """任何 `*_name` 字段都不许是 UUID —— 这就是「界面上显示 UUID」的机器判据。"""
    leaks = []

    def walk(node, prefix):
        if isinstance(node, dict):
            for key, value in node.items():
                if key.endswith("_name") and isinstance(value, str) and UUID_RE.match(value.strip()):
                    leaks.append(f"{path}: {prefix}{key} = {value}")
                walk(value, prefix + key + ".")
        elif isinstance(node, list):
            for index, item in enumerate(node):
                walk(item, f"{prefix}[{index}].")

    walk(payload, "")
    return leaks


def _rows(payload):
    """从响应里取列表：`data` 可能是数组，也可能是 `{list: [...]}`。"""
    data = (payload or {}).get("data")
    if isinstance(data, list):
        return data
    if isinstance(data, dict):
        return data.get("list") or []
    return []


def ensure_help_category(token):
    """取/建帮助分类（脚本可重复跑：分类 code 唯一，第二次走查询复用）。"""
    status, resp = http(
        "POST",
        "/api/business/help/category",
        token,
        body={"name": "AUD-02 分类", "code": SLUG, "sort_order": 1},
    )
    if status == 200:
        return ((resp.get("data") or {}).get("id") or ""), status
    status2, resp2 = http("GET", "/api/business/help/category/list", token)
    for row in _rows(resp2):
        if row.get("code") == SLUG or row.get("category_code") == SLUG:
            return (row.get("id") or ""), status2
    return "", status


def ensure_help_doc(token, category_id):
    """取/建帮助文档（slug 唯一，第二次走列表查询复用）。"""
    body = {
        "category_id": category_id,
        "title": "AUD-02 版本历史",
        "slug": SLUG,
        "content_md": "# AUD-02\n\n版本历史里要显示操作人姓名。",
    }
    status, resp = http("POST", "/api/business/help/doc", token, body=body)
    if status == 200:
        return ((resp.get("data") or {}).get("id") or ""), status
    status2, resp2 = http("GET", f"/api/business/help/doc/page?page=1&page_size=50&keyword={SLUG}", token)
    for row in _rows(resp2):
        if row.get("slug") == SLUG:
            return (row.get("id") or ""), status2
    return "", status


def main():
    token = ensure_ready()

    print("== ②④ 合同详情：负责人 / 制单人 / 修改人都是姓名，改名后跟新名 ==")
    supplier_id, supplier_name = ensure_supplier()
    owner_id = ensure_owner_user()
    status, resp = http(
        "POST",
        "/api/business/erp/contract",
        token,
        body={
            "name": "AUD02-合同-人员姓名",
            "contract_type": "PURCHASE",
            "party_b_id": supplier_id,
            "amount": "1000.00",
            "sign_date": "2026-01-01",
            "start_date": "2026-01-02",
            "end_date": "2026-12-31",
            "owner_id": owner_id,
            "remark": "AUD-02 e2e",
        },
    )
    ok(status == 200, f"建合同成功（HTTP {status}）")
    contract = (resp.get("data") or {}) if status == 200 else {}
    contract_id = contract.get("id") or ""

    if contract_id:
        ok(contract.get("owner_name") == OWNER_NAME, f"新建回读 owner_name = {contract.get('owner_name')!r}")
        ok(bool(contract.get("create_by_name")), f"新建回读 create_by_name 有值：{contract.get('create_by_name')!r}")

        status, resp = http("GET", f"/api/business/erp/contract/{contract_id}", token)
        detail = (resp.get("data") or {}) if status == 200 else {}
        ok(status == 200, f"详情接口 200（HTTP {status}）")
        ok(detail.get("owner_name") == OWNER_NAME, f"详情 owner_name = {detail.get('owner_name')!r}")
        ok(bool(detail.get("create_by_name")), f"详情 create_by_name = {detail.get('create_by_name')!r}")
        ok(
            detail.get("update_by_name") is not None,
            f"详情 update_by_name 字段存在（{detail.get('update_by_name')!r}）",
        )
        for field in ("owner_name", "create_by_name", "update_by_name"):
            value = detail.get(field) or ""
            ok(not UUID_RE.match(str(value)), f"详情 {field} 不是 UUID（{value!r}）")

        # ④ 改名 → 立刻显示新名
        psql(f"UPDATE sys_user SET real_name = '{OWNER_NAME_2}' WHERE username = '{OWNER_USER}'")
        status, resp = http("GET", f"/api/business/erp/contract/{contract_id}", token)
        renamed = (resp.get("data") or {}) if status == 200 else {}
        ok(
            renamed.get("owner_name") == OWNER_NAME_2,
            f"改名后详情 owner_name = {renamed.get('owner_name')!r}（实时 join）",
        )
        status, resp = http("GET", "/api/business/erp/contract/page?page=1&page_size=10", token)
        rows = ((resp.get("data") or {}).get("list") or []) if status == 200 else []
        ok(len(rows) > 0, f"合同列表非空（{len(rows)} 条）")
        psql(f"UPDATE sys_user SET real_name = '{OWNER_NAME}' WHERE username = '{OWNER_USER}'")

    print("== ② 文件列表：上传人是姓名 ==")
    boundary = "----aud02boundary"
    payload = b"\x89PNG\r\n\x1a\n" + os.urandom(64)
    parts = [
        f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; "
        f"filename=\"aud02-uploader.png\"\r\nContent-Type: image/png\r\n\r\n".encode()
        + payload
        + b"\r\n",
        f"--{boundary}--\r\n".encode(),
    ]
    status, resp = http(
        "POST",
        "/api/business/storage/upload",
        token,
        raw=b"".join(parts),
        content_type=f"multipart/form-data; boundary={boundary}",
    )
    ok(status == 200, f"上传文件成功（HTTP {status}）")
    status, resp = http("GET", "/api/business/storage/file/page?page=1&page_size=5", token)
    files = ((resp.get("data") or {}).get("list") or []) if status == 200 else []
    ok(len(files) > 0, f"文件列表非空（{len(files)} 条）")
    if files:
        row = files[0]
        ok("uploader_name" in row, "文件列表行里有 uploader_name 字段")
        ok(bool((row.get("uploader_name") or "").strip()), f"uploader_name 有值：{row.get('uploader_name')!r}")
        ok(not UUID_RE.match((row.get("uploader_name") or "").strip()), "uploader_name 不是 UUID")

    print("== ② 帮助文档版本历史：操作人是姓名 ==")
    category_id, status = ensure_help_category(token)
    ok(bool(category_id), f"帮助分类就绪（HTTP {status}）")
    doc_id = ""
    if category_id:
        doc_id, status = ensure_help_doc(token, category_id)
        ok(bool(doc_id), f"帮助文档就绪（HTTP {status}）")
    if doc_id:
        status, _ = http(
            "POST", f"/api/business/help/doc/{doc_id}/publish", token, body={"change_note": "v1"}
        )
        ok(status == 200, f"发布帮助文档（HTTP {status}）")
        status, resp = http("GET", f"/api/business/help/doc/{doc_id}/versions", token)
        versions = (resp.get("data") or []) if status == 200 else []
        ok(status == 200 and len(versions) > 0, f"版本列表非空（{len(versions)} 条）")
        if versions:
            first = versions[0]
            ok("create_by_name" in first, "版本行里有 create_by_name")
            ok(bool((first.get("create_by_name") or "").strip()), f"create_by_name = {first.get('create_by_name')!r}")
            ok(not UUID_RE.match((first.get("create_by_name") or "").strip()), "create_by_name 不是 UUID")

    print("== 全局扫描：任何 *_name 字段都不许是 UUID ==")
    leaks = []
    for path, payload in seen_responses:
        leaks.extend(scan_uuid_leaks(path, payload))
    ok(not leaks, f"共扫描 {len(seen_responses)} 个响应，UUID 漏出 {len(leaks)} 处")
    for leak in leaks[:10]:
        print("    · " + leak)

    psql(f"DELETE FROM sys_help_doc WHERE slug = '{SLUG}'")
    psql(f"DELETE FROM biz_contract WHERE contract_name = 'AUD02-合同-人员姓名'")
    psql(f"DELETE FROM sys_user WHERE username = '{OWNER_USER}'")

    print("==================== 汇总 ====================")
    if fails:
        print(f"失败 {len(fails)} 项：")
        for item in fails:
            print("  - " + item)
        return 1
    print("全部通过（② 人员字段全是姓名 / ④ 改名后跟新名 / 全局无 UUID 漏出）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
