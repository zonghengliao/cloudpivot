#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-4 真机验收：业务单据附件（ARCH-3 DC-17）—— 上传 / 列表 / 下载 / 删除 / 重启不丢。

跑在隔离实例上（默认 127.0.0.1:18098 / 库 cp_p14_e2e），**全部走真实 HTTP**：

```
步骤                                         期望
① 合同详情上传一张图（CONTRACT_SCAN）        200 + 绑定 id
② 列表（doc_type=CONTRACT）                  1 条，文件名 / 角色中文名 / 上传人对
③ 下载                                        字节与原文件完全一致
④ 采购单 / 收货单 / 付款单 各传一个            四类单据列表各 1 条
⑤ doc_type 非法 / doc_id 非法 / 缺文件字段     400（消息带可选值）
⑥ 不存在的单据                               404（不给不存在的单据挂号）
⑦ 无权限账号（无任何角色）访问                403
⑧ 删除合同附件                                200 deleted=true；列表归零；再下载 404
⑨ 重启服务后再列表 + 下载（--phase after-restart）  附件仍在、字节仍一致
```

用法：
    python3 scripts/verify/p14_attachment_e2e.py                  # 主流程（含造夹具）
    python3 scripts/verify/p14_attachment_e2e.py --phase after-restart
退出码：0 全过 / 1 有失败。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import base64
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("P14_BASE", "http://127.0.0.1:18098")
DB = dict(
    host=os.environ.get("P14_DB_HOST", "127.0.0.1"),
    port=os.environ.get("P14_DB_PORT", "5432"),
    user=os.environ.get("P14_DB_USER", "alun"),
    name=os.environ.get("P14_DB_NAME", "cp_p14_e2e"),
    password=os.environ.get("P14_DB_PASSWORD", "alun"),
)
ADMIN_USER = os.environ.get("P14_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("P14_ADMIN_PASS", "Admin@123456")
NEW_PASS = os.environ.get("P14_NEW_PASS", "P14@Passw0rd!")
STATE_FILE = os.environ.get("P14_STATE", "/root/p14-e2e-state.json")

# ---------- 夹具 UUID（与单测的 d212x 区分，避免同库互相覆盖） ----------
SUPPLIER = "d2140200-0000-4000-8000-000000000001"
WAREHOUSE = "d2140200-0000-4000-8000-000000000002"
DOCS = {
    "CONTRACT": "d2140101-0000-4000-8000-000000000001",
    "PURCHASE_ORDER": "d2140102-0000-4000-8000-000000000001",
    "STOCK_IN": "d2140103-0000-4000-8000-000000000001",
    "PAYABLE": "d2140104-0000-4000-8000-000000000001",
    "PAYMENT": "d2140105-0000-4000-8000-000000000001",
}
GUEST_USER = "p14_guest"

# 1x1 PNG（极小但真实可解码的图片，验「发票影像/扫描件」场景）
PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
)

# 每类单据用**内容不同**的字节：storage 按内容 hash 秒传会复用 `sys_file` 记录，
# 同一份字节被第二次上传时列表显示的是**首次**上传的 original_name（任务附件 E14 同款语义）。
# 真机上四类单据的文件本来就不同，这里用「PNG + 各自标记」保证夹具与真实情况一致。
def sample_bytes(tag):
    return PNG + f"\n#p14-{tag}".encode()


FILENAMES = {
    "CONTRACT": "合同扫描件.png",
    "PURCHASE_ORDER": "采购签收单.png",
    "STOCK_IN": "收货签收单.png",
    "PAYMENT": "付款发票影像.png",
}

PASSED = []
FAILED = []
ADMIN_TOKEN = ""
ADMIN_CURRENT_PASS = ADMIN_PASS


def check(ok, name, detail=""):
    (PASSED if ok else FAILED).append(name)
    print(("[PASS] " if ok else "[FAIL] ") + name + ("" if ok else f" — {detail}"))
    return ok


# ==================== 基础设施 ====================

def psql(sql):
    args = [
        "psql", "-h", DB["host"], "-p", DB["port"], "-U", DB["user"], "-d", DB["name"],
        "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tAF", "\x1f", "-c", sql,
    ]
    out = subprocess.run(args, capture_output=True, text=True,
                         env=dict(os.environ, PGPASSWORD=DB["password"]), check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()[:300]}\nSQL: {sql}")
    rows = []
    for line in out.stdout.strip().split("\n"):
        if line == "":
            continue
        rows.append(line.split("\x1f"))
    return rows


def one(sql, default="0"):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] else default


def http(method, path, token=None, body=None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except Exception:
            return err.code, {"raw": raw}


def http_raw(method, path, token=None, body=None, content_type="application/json"):
    """返回 (status, headers, bytes)，用于下载与 multipart 上传。

    `headers` 是 urllib 的 `Message`（**大小写不敏感**）—— 直接 `dict()` 会把键名
    压成响应原样的小写，`headers.get("Content-Disposition")` 就永远取不到。
    """
    url = BASE + path
    req = urllib.request.Request(url, data=body, method=method)
    req.add_header("Content-Type", content_type)
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, resp.headers, resp.read()
    except urllib.error.HTTPError as err:
        return err.code, err.headers, err.read()


def multipart_body(fields, filename, content, mime="image/png"):
    boundary = "----p14attachmentboundary"
    parts = b""
    for key, value in fields.items():
        parts += (
            f"--{boundary}\r\n"
            f'Content-Disposition: form-data; name="{key}"\r\n\r\n{value}\r\n'
        ).encode()
    parts += (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'
        f"Content-Type: {mime}\r\n\r\n"
    ).encode()
    parts += content + b"\r\n"
    parts += f"--{boundary}--\r\n".encode()
    return parts, f"multipart/form-data; boundary={boundary}"


def upload(token, doc_type, doc_id, file_role, filename, content, remark=None, with_file=True):
    fields = {"doc_type": doc_type, "doc_id": doc_id}
    if file_role:
        fields["file_role"] = file_role
    if remark:
        fields["remark"] = remark
    if with_file:
        body, content_type = multipart_body(fields, filename, content)
    else:
        # 故意不带 file 字段（验 400）
        boundary = "----p14nofile"
        body = b""
        for key, value in fields.items():
            body += (
                f"--{boundary}\r\n"
                f'Content-Disposition: form-data; name="{key}"\r\n\r\n{value}\r\n'
            ).encode()
        body += f"--{boundary}--\r\n".encode()
        content_type = f"multipart/form-data; boundary={boundary}"
    status, _headers, raw = http_raw("POST", "/api/business/attachment/upload", token, body, content_type)
    try:
        payload = json.loads(raw.decode() or "{}")
    except Exception:
        payload = {"raw": raw[:200].decode(errors="replace")}
    return status, payload


def login(username, password):
    """登录（自动处理「首登强制改密」，与其它卡同口径）。"""
    global ADMIN_CURRENT_PASS
    status, body, token, used = 0, {}, "", password
    for candidate in (password, ADMIN_PASS, NEW_PASS):
        status, body = http("POST", "/api/system/auth/login",
                            body={"username": username, "password": candidate})
        token = ((body or {}).get("data") or {}).get("access_token") or ""
        if token:
            used = candidate
            break
    if not token:
        raise RuntimeError(f"登录失败 {username}: {status} {body}")
    if ((body or {}).get("data") or {}).get("must_change_password") and used != NEW_PASS:
        st, resp = http("PUT", "/api/system/auth/password", token,
                        {"old_password": used, "new_password": NEW_PASS})
        if st != 200:
            raise RuntimeError(f"{username} 改初始口令失败：{st} {resp}")
        status, body = http("POST", "/api/system/auth/login",
                            body={"username": username, "password": NEW_PASS})
        token = ((body or {}).get("data") or {}).get("access_token") or ""
        used = NEW_PASS
    if username == ADMIN_USER:
        ADMIN_CURRENT_PASS = used
    return token


# ==================== 夹具 ====================

def seed_docs():
    """造五类单据 + 供应商 / 仓库。**只造本脚本前缀的夹具**。"""
    psql("DELETE FROM biz_doc_attachment WHERE doc_id IN ("
         + ",".join(f"'{doc}'::uuid" for doc in DOCS.values()) + ")")
    names = ",".join(f"'{name}'" for name in list(FILENAMES.values()) + ["合同补充协议.png", "发票影像.png"])
    psql(f"DELETE FROM sys_file WHERE original_name IN ({names}) AND category = 'attachment'")
    psql(f"DELETE FROM biz_payment WHERE id = '{DOCS['PAYMENT']}'::uuid")
    psql(f"DELETE FROM biz_payable WHERE id = '{DOCS['PAYABLE']}'::uuid")
    psql(f"DELETE FROM biz_stock_in WHERE id = '{DOCS['STOCK_IN']}'::uuid")
    psql(f"DELETE FROM biz_purchase_order WHERE id = '{DOCS['PURCHASE_ORDER']}'::uuid")
    psql(f"DELETE FROM biz_contract WHERE id = '{DOCS['CONTRACT']}'::uuid")
    psql(f"DELETE FROM biz_warehouse WHERE id = '{WAREHOUSE}'::uuid")
    psql(f"DELETE FROM biz_supplier WHERE id = '{SUPPLIER}'::uuid")

    psql(f"INSERT INTO biz_supplier (id, supplier_code, supplier_name, credit_days, credit_limit, status) "
         f"VALUES ('{SUPPLIER}'::uuid, 'P14-S01', 'P14 附件供应商', 0, 0, 1)")
    psql(f"INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative) "
         f"VALUES ('{WAREHOUSE}'::uuid, 'P14-W01', 'P14 附件仓', '/{WAREHOUSE}/', 1, 1, 1, 0)")
    psql(f"INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, "
         f"party_b_id, party_b_name, amount, status) VALUES "
         f"('{DOCS['CONTRACT']}'::uuid, 'P14-CT-0001', 'P14 附件测试合同', 'PURCHASE', 'SUPPLIER', "
         f"'{SUPPLIER}'::uuid, 'P14 附件供应商', 1000, 'DRAFT')")
    psql(f"INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status) "
         f"VALUES ('{DOCS['PURCHASE_ORDER']}'::uuid, 'P14-PO-0001', '{SUPPLIER}'::uuid, "
         f"'2033-04-01', '{WAREHOUSE}'::uuid, 'DRAFT')")
    psql(f"INSERT INTO biz_stock_in (id, in_no, in_type, warehouse_id, in_date, status, is_sourceless) "
         f"VALUES ('{DOCS['STOCK_IN']}'::uuid, 'P14-IN-0001', 'OTHER', '{WAREHOUSE}'::uuid, "
         f"'2033-04-01', 'DRAFT', TRUE)")
    psql(f"INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no, "
         f"amount, paid_amount, unpaid_amount, due_date, period_code, status) VALUES "
         f"('{DOCS['PAYABLE']}'::uuid, 'P14-AP-0001', '{SUPPLIER}'::uuid, 'STOCK_IN', "
         f"'{DOCS['STOCK_IN']}'::uuid, 'P14-IN-0001', 0, 0, 0, '2033-04-30', '2033-04', 'OPEN')")
    psql(f"INSERT INTO biz_payment (id, payment_no, payable_id, amount, pay_date) VALUES "
         f"('{DOCS['PAYMENT']}'::uuid, 'P14-PAY-0001', '{DOCS['PAYABLE']}'::uuid, 100, '2033-04-10')")


def ensure_guest():
    """建一个没有任何角色的账号（验「超权限」一侧：403 且消息带权限码）。"""
    psql(f"DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username = '{GUEST_USER}')")
    psql(f"DELETE FROM sys_user WHERE username = '{GUEST_USER}'")
    status, body = http("POST", "/api/system/user", ADMIN_TOKEN, {
        "username": GUEST_USER, "password": "Guest@123456", "real_name": "P14 无权限账号",
        "role_ids": [],
    })
    print(f"    · 建无权限账号 {GUEST_USER}: HTTP {status}")


# ==================== 主流程 ====================

def get_list(token, doc_type, doc_id):
    query = f"/api/business/attachment/list?doc_type={doc_type}&doc_id={doc_id}"
    status, body = http("GET", query, token)
    return status, body


def download(token, attachment_id):
    return http_raw("GET", f"/api/business/attachment/download/{attachment_id}", token)


def phase_main():
    global ADMIN_TOKEN
    print("== 前置：登录 + 造夹具 ==")
    ADMIN_TOKEN = login(ADMIN_USER, ADMIN_CURRENT_PASS)
    print(f"    · admin 登录成功（当前口令 {ADMIN_CURRENT_PASS}）")
    seed_docs()
    ensure_guest()
    backend = one("SELECT coalesce(max(backend_type),'') FROM sys_storage_config WHERE deleted_at IS NULL AND is_default")
    print(f"    · 默认存储后端：{backend or '（无）'}；库 {DB['name']}")

    state = {"docs": DOCS, "attachments": {}}

    print("\n== ① 合同：上传一张图（CONTRACT_SCAN） ==")
    status, payload = upload(ADMIN_TOKEN, "CONTRACT", DOCS["CONTRACT"], "CONTRACT_SCAN",
                             FILENAMES["CONTRACT"], sample_bytes("CONTRACT"), remark="盖章页")
    data = (payload or {}).get("data") or {}
    check(status == 200 and (payload or {}).get("code") == 0, "上传合同扫描件 200",
          f"{status} {payload}")
    check(data.get("file_role") == "CONTRACT_SCAN", "角色回显 CONTRACT_SCAN", str(data))
    check(data.get("file_name") == FILENAMES["CONTRACT"], "文件名回显原始名", str(data))
    check(data.get("duplicated") is False, "首次上传非秒传", str(data))
    contract_att = data.get("id")
    state["attachments"]["CONTRACT"] = contract_att
    print(f"    · 绑定 id={contract_att} file_id={data.get('file_id')} size={data.get('file_size')}")

    print("\n== ② 合同：列表能看到 ==")
    status, body = get_list(ADMIN_TOKEN, "CONTRACT", DOCS["CONTRACT"])
    data = (body or {}).get("data") or {}
    items = data.get("list") or []
    check(status == 200 and data.get("total") == 1, "合同列表 1 条", f"{status} {body}")
    item = items[0] if items else {}
    check(item.get("file_name") == FILENAMES["CONTRACT"], "列表文件名正确", str(item))
    check(item.get("file_role_label") == "合同扫描件", "列表角色中文名正确", str(item))
    check(item.get("uploader_name") == "系统管理员" or bool(item.get("uploader_name")),
          "列表带上传人姓名", str(item))
    check(item.get("can_delete") is True, "上传人可删自己的附件", str(item))
    check(data.get("max_count") == 50, "上限回显 50", str(data))

    print("\n== ③ 合同：能下载且字节一致 ==")
    status, headers, raw = download(ADMIN_TOKEN, contract_att)
    check(status == 200, "下载 200", str(status))
    contract_bytes = sample_bytes("CONTRACT")
    check(raw == contract_bytes, "下载字节与原文件一致", f"{len(raw)} vs {len(contract_bytes)}")
    disposition = headers.get("Content-Disposition", "")
    check("attachment" in disposition.lower(), "Content-Disposition 为 attachment", disposition)
    check(headers.get("X-Content-Type-Options") == "nosniff", "X-Content-Type-Options: nosniff",
          str(headers.get("X-Content-Type-Options")))

    print("\n== ④ 采购单 / 收货单 / 付款单 各传一个 ==")
    for doc_type, role in [
        ("PURCHASE_ORDER", "SIGN_RECEIPT"),
        ("STOCK_IN", "SIGN_RECEIPT"),
        ("PAYMENT", "INVOICE"),
    ]:
        name = FILENAMES[doc_type]
        status, payload = upload(ADMIN_TOKEN, doc_type, DOCS[doc_type], role, name,
                                 sample_bytes(doc_type))
        data = (payload or {}).get("data") or {}
        ok = status == 200 and data.get("id")
        check(bool(ok), f"{doc_type} 上传 {name} 200", f"{status} {payload}")
        state["attachments"][doc_type] = data.get("id")
        status, body = get_list(ADMIN_TOKEN, doc_type, DOCS[doc_type])
        info = (body or {}).get("data") or {}
        total = info.get("total")
        first = (info.get("list") or [{}])[0]
        check(total == 1, f"{doc_type} 列表能看到 1 条", f"{status} {body}")
        check(first.get("file_name") == name, f"{doc_type} 列表文件名正确", str(first))

    print("\n== ⑤ 负例：类型非法 / 主键非法 / 缺文件字段 ==")
    status, payload = upload(ADMIN_TOKEN, "PURCHASE", DOCS["CONTRACT"], None, "x.png", PNG)
    code = (payload or {}).get("code")
    check(status == 400 and code == 10001, "doc_type 非法 → 400 + 10001", f"{status} {payload}")
    check("PURCHASE_ORDER" in ((payload or {}).get("msg") or ""), "消息列出可选类型",
          str((payload or {}).get("msg")))
    status, payload = upload(ADMIN_TOKEN, "CONTRACT", "not-a-uuid", None, "x.png", PNG)
    check(status == 400, "doc_id 非法 → 400", f"{status} {payload}")
    status, payload = upload(ADMIN_TOKEN, "CONTRACT", DOCS["CONTRACT"], None, "x.png", PNG,
                             with_file=False)
    check(status == 400, "缺 file 字段 → 400", f"{status} {payload}")

    print("\n== ⑥ 负例：单据不存在 → 404 ==")
    ghost = "d2140fff-0000-4000-8000-0000000000ff"
    status, payload = upload(ADMIN_TOKEN, "CONTRACT", ghost, None, "x.png", PNG)
    check(status == 404, "不存在的合同 → 404", f"{status} {payload}")
    status, body = get_list(ADMIN_TOKEN, "STOCK_IN", ghost)
    check(status == 404, "不存在的收货单列表 → 404", f"{status} {body}")
    left = one("SELECT count(*) FROM biz_doc_attachment WHERE doc_id = '" + ghost + "'::uuid")
    check(left == "0", "不存在的单据没有落下任何绑定行", f"残留 {left}")

    print("\n== ⑦ 超权限：无角色账号 → 403 ==")
    guest_token = login(GUEST_USER, "Guest@123456")
    status, body = get_list(guest_token, "CONTRACT", DOCS["CONTRACT"])
    check(status == 403, "无权限账号列表 → 403", f"{status} {body}")
    status, payload = upload(guest_token, "CONTRACT", DOCS["CONTRACT"], None, "x.png", PNG)
    check(status == 403, "无权限账号上传 → 403", f"{status} {payload}")
    status, headers, raw = download(guest_token, contract_att)
    check(status == 403, "无权限账号下载 → 403", str(status))

    print("\n== ⑧ 删除合同附件（上传人本人） ==")
    status, body = http("DELETE", f"/api/business/attachment/{contract_att}", ADMIN_TOKEN)
    data = (body or {}).get("data") or {}
    check(status == 200 and data.get("deleted") is True, "删除 200 + deleted=true", f"{status} {body}")
    print(f"    · 文件记录是否同时软删：{data.get('file_deleted')}（还有别的单据引用时为 false）")
    status, body = get_list(ADMIN_TOKEN, "CONTRACT", DOCS["CONTRACT"])
    check(((body or {}).get("data") or {}).get("total") == 0, "删除后列表归零", f"{body}")
    status, _headers, _raw = download(ADMIN_TOKEN, contract_att)
    check(status == 404, "删除后再下载 → 404", str(status))
    status, body = http("DELETE", f"/api/business/attachment/{contract_att}", ADMIN_TOKEN)
    check(status == 404, "重复删除 → 404（幂等语义）", f"{status} {body}")

    print("\n== ⑨ 数据落库口径（psql 侧核对） ==")
    rows = psql("SELECT a.doc_type, a.file_role, a.deleted_at IS NULL AS alive, "
                "coalesce(f.original_name,'') FROM biz_doc_attachment a "
                "LEFT JOIN sys_file f ON f.id = a.file_id "
                "WHERE a.doc_id IN (" + ",".join(f"'{d}'::uuid" for d in DOCS.values()) + ") "
                "ORDER BY a.doc_type, a.file_role")
    for row in rows:
        print("    · " + " | ".join(row))
    alive = one("SELECT count(*) FROM biz_doc_attachment WHERE deleted_at IS NULL AND doc_id IN ("
                + ",".join(f"'{d}'::uuid" for d in DOCS.values()) + ")")
    check(alive == "3", "现存未删绑定 3 条（采购/收货/付款，合同已删）", f"实际 {alive}")

    with open(STATE_FILE, "w") as handle:
        json.dump(state, handle, ensure_ascii=False, indent=1)
    print(f"\n    · 状态已存 {STATE_FILE}（重启后校验用）")


def phase_after_restart():
    global ADMIN_TOKEN
    print("== 重启后：附件不丢 ==")
    with open(STATE_FILE) as handle:
        state = json.load(handle)
    ADMIN_TOKEN = login(ADMIN_USER, ADMIN_CURRENT_PASS)
    expected = {
        "PURCHASE_ORDER": FILENAMES["PURCHASE_ORDER"],
        "STOCK_IN": FILENAMES["STOCK_IN"],
        "PAYMENT": FILENAMES["PAYMENT"],
    }
    for doc_type, name in expected.items():
        status, body = get_list(ADMIN_TOKEN, doc_type, DOCS[doc_type])
        data = (body or {}).get("data") or {}
        items = data.get("list") or []
        ok = status == 200 and data.get("total") == 1 and items and items[0].get("file_name") == name
        check(bool(ok), f"重启后 {doc_type} 列表仍有 1 条（{name}）", f"{status} {body}")
        att_id = state["attachments"].get(doc_type)
        status, _headers, raw = download(ADMIN_TOKEN, att_id)
        check(status == 200 and raw == sample_bytes(doc_type),
              f"重启后 {doc_type} 仍能下载且字节一致", f"{status} {len(raw)}")
    # 合同那条已经删了：重启后仍应是 0 条（软删不因重启复活）
    status, body = get_list(ADMIN_TOKEN, "CONTRACT", DOCS["CONTRACT"])
    check(((body or {}).get("data") or {}).get("total") == 0, "重启后已删除的合同附件不复活",
          str(body))


def phase_ui_fixture():
    """给前端截图准备数据：每类单据各留一条附件（不删），并保证契约上有 2 条。"""
    global ADMIN_TOKEN
    print("== 截图夹具：四类单据各留一条附件 ==")
    ADMIN_TOKEN = login(ADMIN_USER, ADMIN_CURRENT_PASS)
    seed_docs()
    plan = [
        ("CONTRACT", "CONTRACT_SCAN", "合同扫描件.png"),
        ("PURCHASE_ORDER", "SIGN_RECEIPT", "采购签收单.png"),
        ("STOCK_IN", "SIGN_RECEIPT", "收货签收单.png"),
        ("PAYABLE", "INVOICE", "发票影像.png"),
        ("PAYMENT", "INVOICE", "付款发票影像.png"),
    ]
    for doc_type, role, name in plan:
        status, payload = upload(ADMIN_TOKEN, doc_type, DOCS[doc_type], role, name,
                                 sample_bytes(doc_type))
        data = (payload or {}).get("data") or {}
        check(status == 200 and bool(data.get("id")), f"{doc_type} 截图夹具上传成功",
              f"{status} {payload}")
    # 合同再补一条「补充协议」，让截图里能看到多条 + 不同角色
    status, payload = upload(ADMIN_TOKEN, "CONTRACT", DOCS["CONTRACT"], "OTHER",
                             "合同补充协议.png", sample_bytes("CONTRACT-2"), remark="补充条款第 3 页")
    check(status == 200, "合同第二条附件（OTHER）", f"{status} {payload}")


def main():
    phase = "main"
    if "--phase" in sys.argv:
        phase = sys.argv[sys.argv.index("--phase") + 1]
    if phase == "main":
        phase_main()
    elif phase == "after-restart":
        phase_after_restart()
    elif phase == "ui-fixture":
        phase_ui_fixture()
    else:
        print(f"未知 phase：{phase}")
        return 2

    print("\n==================== 汇总 ====================")
    print(f"通过 {len(PASSED)} / 失败 {len(FAILED)}")
    for name in FAILED:
        print("  ✗ " + name)
    return 0 if not FAILED else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:  # noqa: BLE001
        print(f"[FATAL] {error}")
        sys.exit(1)
