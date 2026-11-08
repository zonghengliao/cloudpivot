#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
PDF-08 · 「下载 PDF」145 实测（12 个接口 + 导出审计 + 负面分支）

形态照 docs/evidence/20261108-pdf07/e2e145.py：stdlib + urllib，无第三方依赖；
每条断言打印 [PASS]/[FAIL] 并计数，PDF 事实用 pdftotext / pdfinfo 断言。

覆盖卡面验收：
  ① 采购订单 / 销售订单 / 合同 / 入库 / 出库 / 调拨 / 盘点 / 退货（采购 + 销售）
  ② 审批实例 / 任务 / 三张报表
  ③ 点击能下载：200 + application/pdf + %PDF- + `filename*=UTF-8''` 解出的中文名
     （含单号、中文不乱码）+ 文本层里能搜到单号
  ④ operation_logs 有导出记录（导出前 / 后计数 +1，operation / target_type 逐条点名）
  ⑤ 无权限 → 403 + 提示（真实低权限账号），未登录 → 401，乱 id → 404

instance 分配（复用各 PDF 卡留下的库，避免二次造数据）：
  8091 cloudpivot_pdf03 → 采购/销售订单、合同、入库、出库、调拨、盘点
  8092 cloudpivot_pdf04 → 采购退货、销售退货
  8093 cloudpivot_pdf05 → 审批实例
  8094 cloudpivot_pdf06 → 任务
  8095 cloudpivot_pdf07 → 三张报表（2026-10 有 PDF-07 造的 1,500.00 凭证）

环境变量：EVID（证据目录）/ ENVF（读 NIK_ADMIN_PASSWORD、NIK_DATABASE_PASSWORD）
"""
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
EVID = os.environ.get("EVID") or HERE
RAW = os.path.join(EVID, "raw")
SAMPLE = os.path.join(EVID, "sample")
os.makedirs(RAW, exist_ok=True)
os.makedirs(SAMPLE, exist_ok=True)

PASS = 0
FAIL = 0
FAILS = []

_ENVF = os.environ.get("ENVF", "/root/pdf07.env")
ADMIN_PW = ""
PGPW = ""
if os.path.exists(_ENVF):
    for _line in open(_ENVF, encoding="utf-8"):
        if _line.startswith("NIK_ADMIN_PASSWORD="):
            ADMIN_PW = _line.split("=", 1)[1].strip()
        if _line.startswith("NIK_DATABASE_PASSWORD="):
            PGPW = _line.split("=", 1)[1].strip()
ADMIN_PW = os.environ.get("ADMIN_PW", ADMIN_PW or "Admin@123456")
NEW_PW = "Pdf08@2026"
# 各 PDF 卡实测时改过的管理员口令（DB 复用，口令也沿用）
ADMIN_CANDIDATES = [
    ADMIN_PW,
    "Admin@12345678",  # cloudpivot_pdf04（本卡把其余库 admin 的密码哈希对齐到它）
    "Pdf07@2026",
    "Pdf06new@2026",
    "Pdf05new@2026",
    NEW_PW,
]


def check(cond, label, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  [PASS] {label}")
    else:
        FAIL += 1
        FAILS.append(f"{label} :: {detail}")
        print(f"  [FAIL] {label} :: {detail}")
    return bool(cond)


def eq(label, actual, expect):
    return check(str(actual) == str(expect), label, f"实际={actual!r} 期望={expect!r}")


def dump(name, text):
    with open(os.path.join(RAW, name), "w", encoding="utf-8") as fp:
        fp.write(text)


def psql(db, sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = PGPW
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", db, "-Atc", sql],
        capture_output=True, text=True, env=env,
    )
    if out.returncode != 0:
        return f"<ERR:{out.stderr.strip()}>"
    return out.stdout.strip()


def req(base, method, path, token=None, body=None, timeout=120):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode() or "{}"
            return resp.status, json.loads(raw)
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode()
        try:
            return exc.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return exc.code, {"raw": raw}
    except Exception as exc:  # noqa: BLE001
        return 0, {"error": str(exc)}


def get_pdf(base, path, token=None):
    headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(base + path, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(r, timeout=180) as resp:
            return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
    except urllib.error.HTTPError as exc:
        return exc.code, {k.lower(): v for k, v in exc.headers.items()}, exc.read()
    except Exception as exc:  # noqa: BLE001
        return 0, {}, str(exc).encode()


def pdftext(path):
    return subprocess.run(["pdftotext", "-layout", path, "-"], capture_output=True, text=True).stdout


def pdfpages(path):
    info = subprocess.run(["pdfinfo", path], capture_output=True, text=True).stdout
    m = re.search(r"Pages:\s+(\d+)", info)
    return int(m.group(1)) if m else -1


def login(base, username, pw_candidates):
    last = None
    for pw in pw_candidates:
        st, body = req(base, "POST", "/api/system/auth/login",
                       body={"username": username, "password": pw})
        if st != 200:
            last = (st, body)
            continue
        data = body.get("data") or {}
        tok = data.get("access_token")
        if data.get("must_change_password"):
            st2, _ = req(base, "PUT", "/api/system/auth/password", token=tok,
                         body={"old_password": pw, "new_password": NEW_PW})
            if st2 != 200:
                return None
            st3, body3 = req(base, "POST", "/api/system/auth/login",
                             body={"username": username, "password": NEW_PW})
            if st3 != 200:
                return None
            tok = ((body3.get("data") or {}).get("access_token"))
        return tok
    print(f"    [login] {username} 失败：{last}")
    return None


def decode_filename(disposition):
    """按 RFC 5987 解出 filename*=UTF-8'' 的名字（取不到返回 None）。"""
    if not disposition:
        return None
    m = re.search(r"filename\*\s*=\s*UTF-8''([^;]+)", disposition, re.I)
    if not m:
        return None
    return urllib.parse.unquote(m.group(1).strip().strip('"'))


# ============================ 单据矩阵 ============================
# (标签, 路径, 单据 id, 单号, 期望中文片段, 期望 operation, 期望 target_type)
DOCS = {
    "http://127.0.0.1:8091": [
        ("采购订单", "/api/business/erp/purchase-order/{id}/pdf",
         "0f050000-0000-4000-8000-000000000040", "PDF03-PO-000001", "采购订单",
         "EXPORT_PURCHASE_ORDER_PDF", "biz_purchase_order"),
        ("销售订单", "/api/business/erp/sales-order/{id}/pdf",
         "0f050000-0000-4000-8000-000000000031", "PDF03-SO-000001", "销售订单",
         "EXPORT_SALES_ORDER_PDF", "biz_sales_order"),
        ("合同", "/api/business/erp/contract/{id}/pdf",
         "0f050000-0000-4000-8000-000000000050", "PDF03-HT-000001", "合同",
         "EXPORT_CONTRACT_PDF", "biz_contract"),
        ("入库单", "/api/business/erp/stock-in/{id}/pdf",
         "d2a30001-0000-4000-8000-000000000031", "P1FIX-IN-1", "入库",
         "EXPORT_STOCK_IN_PDF", "biz_stock_in"),
        ("出库单", "/api/business/erp/stock-out/{id}/pdf",
         "0f050000-0000-4000-8000-000000000020", "PDF03-OUT-000001", "出库",
         "EXPORT_STOCK_OUT_PDF", "biz_stock_out"),
        ("调拨单", "/api/business/erp/stock-transfer/{id}/pdf",
         "d2a30001-0000-4000-8000-000000000041", "P1FIX-TR-1", "调拨",
         "EXPORT_STOCK_TRANSFER_PDF", "biz_stock_transfer"),
        ("盘点单", "/api/business/erp/stock-take/{id}/pdf",
         "0f420000-0000-4000-8000-000000000010", "PDF04-ST-000001", "盘点",
         "EXPORT_STOCK_TAKE_PDF", "biz_stock_take"),
    ],
    "http://127.0.0.1:8092": [
        ("采购退货单", "/api/business/erp/purchase-return/{id}/pdf",
         "0f540000-0000-4000-8000-000000000010", "PDF04-PRT-000001", "采购退货",
         "EXPORT_PURCHASE_RETURN_PDF", "biz_purchase_return"),
        ("销售退货单", "/api/business/erp/sales-return/{id}/pdf",
         "0f550000-0000-4000-8000-000000000010", "PDF04-SR-000001", "销售退货",
         "EXPORT_SALES_RETURN_PDF", "biz_sales_return"),
    ],
    "http://127.0.0.1:8093": [
        ("审批实例", "/api/business/flow/instance/{id}/pdf",
         "01a1bbb2-80e3-722d-bef7-3df5fa45d88e", "FLW-20261108-AD5A4448", "审批单",
         "EXPORT_FLOW_INSTANCE_PDF", "biz_flow_instance"),
    ],
    "http://127.0.0.1:8094": [
        ("任务（派工单）", "/api/business/task/{id}/pdf",
         "01a1bbda-c951-70e4-aa2b-c06c5a0cdb3e", "TSK-202611-000001", "派工单",
         "EXPORT_TASK_PDF", "biz_task"),
    ],
}
DB_BY_BASE = {
    "http://127.0.0.1:8091": "cloudpivot_pdf03",
    "http://127.0.0.1:8092": "cloudpivot_pdf04",
    "http://127.0.0.1:8093": "cloudpivot_pdf05",
    "http://127.0.0.1:8094": "cloudpivot_pdf06",
    "http://127.0.0.1:8095": "cloudpivot_pdf07",
}

REPORT_BASE = "http://127.0.0.1:8095"
REPORTS = [
    ("资产负债表", "balance-sheet", "2026-10", "资产负债表"),
    ("利润表", "income-statement", "2026-10", "利润表"),
    ("现金流量表", "cash-flow", "2026-10", "现金流量表"),
]


def audit_count(db, operation, target_id):
    return psql(
        db,
        "select count(*) from operation_logs "
        f"where operation = '{operation}' and target_id = '{target_id}'",
    )


def run_doc_group(base, docs):
    db = DB_BY_BASE[base]
    print(f"\n## {db} @ {base}")
    token = login(base, "admin", ADMIN_CANDIDATES)
    if not check(bool(token), f"{db} 管理员登录"):
        return None
    out = {}
    for label, path_tpl, doc_id, doc_no, cn_label, op, target_type in docs:
        path = path_tpl.format(id=doc_id)
        before = audit_count(db, op, doc_id)
        st, headers, raw = get_pdf(base, path, token)
        eq(f"{label}：HTTP 200", st, 200)
        eq(f"{label}：content-type", headers.get("content-type"), "application/pdf")
        check(raw.startswith(b"%PDF-"), f"{label}：PDF 魔数", repr(raw[:20]))
        disp = headers.get("content-disposition", "")
        name = decode_filename(disp)
        check("filename*=UTF-8''" in disp, f"{label}：响应头带 filename*=UTF-8''", disp[:120])
        check(bool(name) and cn_label in (name or ""), f"{label}：中文名解出（{name!r}）", disp[:120])
        check(bool(name) and doc_no in name, f"{label}：文件名含单号 {doc_no}", str(name))
        safe = f"{label}-{doc_no}".replace("/", "-")
        pdf_path = os.path.join(SAMPLE, f"{safe}.pdf")
        with open(pdf_path, "wb") as fp:
            fp.write(raw)
        pages = pdfpages(pdf_path)
        check(pages >= 1, f"{label}：PDF 可解析（页数 {pages}）")
        text = pdftext(pdf_path)
        check(doc_no in re.sub(r"\s+", "", text), f"{label}：文本层含单号 {doc_no}", text[:120].replace("\n", " "))
        after = audit_count(db, op, doc_id)
        check(str(after) == str(int(before) + 1) if before.isdigit() else False,
              f"{label}：operation_logs +1（{op}）", f"before={before} after={after}")
        row = psql(db, "select operation||'|'||target_type||'|'||coalesce(detail->>'doc_no','')"
                       "||'|'||coalesce(username,'')||'|'||to_char(created_at,'YYYY-MM-DD HH24:MI:SS') "
                       f"from operation_logs where operation='{op}' and target_id='{doc_id}' "
                       "order by created_at desc limit 1")
        check(row.startswith(f"{op}|{target_type}|"), f"{label}：审计 target_type={target_type}", row)
        check(row.split("|")[3] == "admin", f"{label}：审计记录导出人 username=admin", row)
        check(row.split("|")[2] == doc_no, f"{label}：审计记录单号 {doc_no}", row)
        out[label] = {"name": name, "pages": pages, "audit": row, "path": path}
        print(f"    name={name} pages={pages} audit={row}")
    return token


def run_reports():
    base = REPORT_BASE
    db = DB_BY_BASE[base]
    print(f"\n## {db} @ {base}（三张报表）")
    token = login(base, "admin", ADMIN_CANDIDATES)
    if not check(bool(token), f"{db} 管理员登录"):
        return None, {}
    out = {}
    for label, kind, period, cn_label in REPORTS:
        path = f"/api/business/erp/report/{kind}/pdf?period={period}"
        before = psql(db, "select count(*) from operation_logs "
                          f"where operation='EXPORT_REPORT_PDF' and detail->>'report_type'='{kind}'")
        st, headers, raw = get_pdf(base, path, token)
        eq(f"{label}：HTTP 200", st, 200)
        eq(f"{label}：content-type", headers.get("content-type"), "application/pdf")
        check(raw.startswith(b"%PDF-"), f"{label}：PDF 魔数", repr(raw[:20]))
        name = decode_filename(headers.get("content-disposition", ""))
        check(bool(name) and cn_label in (name or ""), f"{label}：中文名（{name!r}）", str(name))
        safe = f"{label}-{period}"
        pdf_path = os.path.join(SAMPLE, f"{safe}.pdf")
        with open(pdf_path, "wb") as fp:
            fp.write(raw)
        pages = pdfpages(pdf_path)
        check(pages >= 1, f"{label}：PDF 可解析（页数 {pages}）")
        after = psql(db, "select count(*) from operation_logs "
                         f"where operation='EXPORT_REPORT_PDF' and detail->>'report_type'='{kind}'")
        check(str(after) == str(int(before) + 1) if before.isdigit() else False,
              f"{label}：operation_logs +1（EXPORT_REPORT_PDF）", f"before={before} after={after}")
        out[label] = {"name": name, "pages": pages}
        print(f"    name={name} pages={pages} audit {before}->{after}")
    return token, out


def mk_role(base, code, name, perms):
    st, body = req(base, "POST", "/api/system/role", token=ADMIN_TOKENS.get(base),
                   body={"role_code": code, "role_name": name, "status": 1, "sort_order": 971,
                         "remark": "PDF-08 实测夹具"})
    rid = psql(DB_BY_BASE[base], f"select id from sys_role where role_code='{code}' and deleted_at is null limit 1")
    if not rid.isdigit() and "-" not in rid:
        print(f"    [role] {code} 建失败 http={st} {body}")
        return None
    psql(DB_BY_BASE[base], f"delete from sys_role_menu where role_id = '{rid}'")
    for perm in perms:
        psql(DB_BY_BASE[base],
             "insert into sys_role_menu (role_id, menu_id, created_at) "
             "select '{rid}', id, now() from sys_menu where auth_code = '{perm}' and deleted_at is null "
             "on conflict (role_id, menu_id) do nothing".format(rid=rid, perm=perm))
    got = psql(DB_BY_BASE[base],
               "select coalesce(string_agg(m.auth_code, ','), '') from sys_role_menu p "
               f"join sys_menu m on m.id = p.menu_id where p.role_id = '{rid}'")
    print(f"    [role] {code} id={rid} 权限=[{got}]")
    return rid


def mk_user(base, username, real_name, role_id, password="Pdf08@123456"):
    st, body = req(base, "POST", "/api/system/user", token=ADMIN_TOKENS.get(base),
                   body={"username": username, "real_name": real_name, "password": password,
                         "status": 1, "role_ids": [role_id]})
    uid = psql(DB_BY_BASE[base], f"select id from sys_user where username='{username}' and deleted_at is null limit 1")
    if not uid:
        print(f"    [user] {username} 建失败 http={st} {body}")
        return None
    psql(DB_BY_BASE[base], f"delete from sys_user_role where user_id = '{uid}'")
    psql(DB_BY_BASE[base],
         f"insert into sys_user_role (user_id, role_id, created_at) values ('{uid}', '{role_id}', now())")
    return uid


ADMIN_TOKENS = {}


def main():
    print("=" * 78)
    print("PDF-08 · 单据 PDF 下载 + 导出审计 · 145 实测")
    print("=" * 78)
    for base, docs in DOCS.items():
        tok = run_doc_group(base, docs)
        if tok:
            ADMIN_TOKENS[base] = tok
    rtok, _ = run_reports()
    if rtok:
        ADMIN_TOKENS[REPORT_BASE] = rtok

    # ---------------- 负面分支 ----------------
    print("\n## 负面分支（401 / 403 / 404）")
    base = "http://127.0.0.1:8091"
    db = DB_BY_BASE[base]
    path = "/api/business/erp/purchase-order/0f050000-0000-4000-8000-000000000040/pdf"
    st, _, raw = get_pdf(base, path, None)
    eq("未登录 → 401", st, 401)
    check(not raw.startswith(b"%PDF-"), "401 不返回 PDF", repr(raw[:40]))

    st, _, raw = get_pdf(base, "/api/business/erp/purchase-order/00000000-0000-4000-8000-000000000099/pdf",
                         ADMIN_TOKENS.get(base))
    eq("不存在的单 id → 404", st, 404)

    rid = mk_role(base, "PDF08_NOEXPORT", "PDF08 无导出权限", ["business:erp:material:list"]) if ADMIN_TOKENS.get(base) else None
    if rid:
        mk_user(base, "pdf08_noperm", "PDF08 无权限", rid)
        tok = login(base, "pdf08_noperm", ["Pdf08@123456", NEW_PW])
        if check(bool(tok), "低权限账号登录"):
            st, _, raw = get_pdf(base, path, tok)
            eq("无权限 → 403", st, 403)
            check(not raw.startswith(b"%PDF-"), "403 不返回 PDF", repr(raw[:40]))
            try:
                msg = json.loads(raw.decode() or "{}").get("msg", "")
            except Exception:  # noqa: BLE001
                msg = raw.decode(errors="replace")[:200]
            check("权限" in msg or "purchase-order" in msg, "403 提示点名权限", msg[:200])
            print(f"    403 msg={msg[:160]}")
            # 同一账号对报表 PDF 也应 403（权限码不同，这里验证「不是全站禁」
            st2, _, _ = get_pdf(base, "/api/business/erp/contract/0f050000-0000-4000-8000-000000000050/pdf", tok)
            eq("同一低权限账号导出合同 → 403", st2, 403)

    # 报表页「看得见按钮但无导出权限」的浏览器用例夹具（浏览器 403 用例要用它登录）
    if ADMIN_TOKENS.get(REPORT_BASE):
        rid_rpt = mk_role(REPORT_BASE, "PDF08_RPT_NOEXPORT", "PDF08 报表无导出权限",
                          ["business:erp:report:balance-sheet", "business:erp:period:list"])
        if rid_rpt:
            mk_user(REPORT_BASE, "pdf08_rpt_noperm", "PDF08 报表无导出", rid_rpt)
            st, body = req(REPORT_BASE, "POST", "/api/system/auth/login",
                           body={"username": "pdf08_rpt_noperm", "password": "Pdf08@123456"})
            check(st == 200, "无导出权限账号可登录（报表页能看见按钮）", json.dumps(body, ensure_ascii=False)[:200])
            if st == 200:
                tok_rpt = (body.get("data") or {}).get("access_token")
                st_pdf, _, raw_pdf = get_pdf(REPORT_BASE,
                                             "/api/business/erp/report/balance-sheet/pdf?period=2026-10", tok_rpt)
                eq("低权限账号导出报表 → 403", st_pdf, 403)

    print("\n" + "=" * 78)
    print(f"断言 {PASS + FAIL} 项：PASS={PASS} FAIL={FAIL}")
    if FAILS:
        print("失败清单：")
        for item in FAILS:
            print(f"  - {item}")
    with open(os.path.join(EVID, "raw_e2e.txt"), "w", encoding="utf-8") as fp:
        fp.write(f"PASS={PASS} FAIL={FAIL}\n")
        for item in FAILS:
            fp.write(f"FAIL: {item}\n")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
