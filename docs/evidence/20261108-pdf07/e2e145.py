#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
PDF-07 · 财务报表 PDF 145 实测（资产负债表 / 利润表 / 现金流量表）

形态照 docs/evidence/20261108-pdf06/e2e145.py：stdlib + urllib，无第三方依赖；
每条断言打印 [PASS]/[FAIL] 并计数，PDF 事实用 pdftotext / pdfinfo / pdffonts 断言。

覆盖卡面四条验收：
  ① 三张报表各导一份 PDF         → 三个 type 各 200 + %PDF- + Pages>=1
  ② 借贷平衡（资产负债表左右相等）→ 用**列切片**读「资产总计」与「负债和所有者权益总计」
                                   的金额列，断言相等且 = 1,500.00；勾稽块写「借贷平衡」「差额 0.00」
  ③ 金额格式（千分位、两位小数）  → 文本层每个金额 token 都匹配
                                   ^-?\\d{1,3}(,\\d{3})*\\.\\d{2}$，且真的出现了带逗号的千分位
  ④ 空白项显示「-」              → 逐行逐列切片：无数据的金额列必须**正好是 "-"**
                                   （不是 0.00、不是空串），且该列不含任何数字
另含：表头（公司名/报表名称/编制单位/日期/单位：元）、表尾（制表人/审核人/日期）、
      利润表与现金流量表的三列表头与合计行、勾稽结论、审计留痕、401/403/400 负面分支。

**为什么要按列切片而不是按行找金额**：`pdftotext -layout` 把左右两栏印在同一行，
「短期借款」所在行里同时有左栏的「货币资金 1,000.00」—— 按行找金额会把别人的钱
算到它头上（首版就是这么假红的）。切片按表头各列起始位置切，才逐列可判。

环境变量：BASE（默认 http://127.0.0.1:8082）/ DB / ENVF / EVID
"""
import json
import os
import re
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.request

BASE = os.environ.get("BASE", "http://127.0.0.1:8082")
DB = os.environ.get("DB", "cloudpivot_pdf07")
HERE = os.path.dirname(os.path.abspath(__file__))
EVID = os.environ.get("EVID") or HERE
RAW = os.environ.get("RAW") or os.path.join(EVID, "raw")
SAMPLE = os.environ.get("SAMPLE") or os.path.join(EVID, "sample")
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
NEW_PW = "Pdf07@2026"

PERIOD = "2026-11"
MONEY_RE = re.compile(r"^-?\d{1,3}(,\d{3})*\.\d{2}$")
MONEY_FIND = re.compile(r"-?\d[\d,]*\.\d+")


# ============================ 断言 / 输出 ============================
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


def norm(text):
    return re.sub(r"\s+", "", text)


def contains(label, text, needle):
    return check(norm(needle) in norm(text), label, f"不含 {needle!r}")


def psql(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = PGPW
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-Atc", sql],
        capture_output=True, text=True, env=env,
    )
    if out.returncode != 0:
        return f"<ERR:{out.stderr.strip()}>"
    return out.stdout.strip()


def req(method, path, token=None, body=None, timeout=90):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
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


def get_pdf(path, token=None):
    headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(r, timeout=180) as resp:
            return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
    except urllib.error.HTTPError as exc:
        return exc.code, {k.lower(): v for k, v in exc.headers.items()}, exc.read()
    except Exception as exc:  # noqa: BLE001
        return 0, {}, str(exc).encode()


def pdftext(path):
    return subprocess.run(["pdftotext", "-layout", path, "-"],
                          capture_output=True, text=True).stdout


def pdfpages(path):
    info = subprocess.run(["pdfinfo", path], capture_output=True, text=True).stdout
    m = re.search(r"Pages:\s+(\d+)", info)
    return int(m.group(1)) if m else -1


def pdfinfo(path):
    return subprocess.run(["pdfinfo", path], capture_output=True, text=True).stdout


def pdffonts(path):
    return subprocess.run(["pdffonts", path], capture_output=True, text=True).stdout


def dump(name, text):
    with open(os.path.join(RAW, name), "w", encoding="utf-8") as fp:
        fp.write(text)


def keep_pdf(name, raw, pages_to_png=(1,)):
    path = os.path.join(SAMPLE, name)
    with open(path, "wb") as fp:
        fp.write(raw)
    text = pdftext(path)
    dump(f"pdftotext_{name}.txt", text)
    dump(f"pdfinfo_{name}.txt", pdfinfo(path))
    dump(f"pdffonts_{name}.txt", pdffonts(path))
    for page in pages_to_png:
        subprocess.run(
            ["pdftoppm", "-r", "110", "-png", "-f", str(page), "-l", str(page),
             path, os.path.join(SAMPLE, f"{name[:-4]}-p{page}")],
            capture_output=True,
        )
    return path, text


def money_tokens(text):
    """文本层所有像金额的 token（不要求合法，用来找不合格的）。"""
    return MONEY_FIND.findall(text)


def bad_money(text):
    return [tok for tok in money_tokens(text) if not MONEY_RE.match(tok)]


# ============================ 资产负债表：按列切片 ============================
def balance_table(text):
    """解析 -layout 文本里的资产负债表：返回 (表头行, [列起点], [每行 6 个单元格])。"""
    lines = text.splitlines()
    header = None
    for line in lines:
        if "负债及所有者权益" in line and "期末余额" in line and "资产" in line:
            header = line
            break
    if header is None:
        return None, [], []
    labels = ["资产", "期末余额", "期初余额", "负债及所有者权益", "期末余额", "期初余额"]
    starts = []
    cursor = 0
    for label in labels:
        pos = header.find(label, cursor)
        if pos < 0:
            return header, [], []
        starts.append(pos)
        cursor = pos + 1
    rows = []
    started = False
    for line in lines:
        if line == header:
            started = True
            continue
        if not started:
            continue
        if "勾稽校验" in line:
            break
        if not line.strip():
            continue
        cells = []
        for index, start in enumerate(starts):
            end = starts[index + 1] if index + 1 < len(starts) else len(line)
            cells.append(line[start:end].strip())
        rows.append(cells)
    return header, starts, rows


def find_row(rows, col, label_part):
    for cells in rows:
        if len(cells) > col and label_part in cells[col]:
            return cells
    return None


# ============================ 登录 / 夹具 ============================
def login(username, pw_candidates):
    last = None
    for pw in pw_candidates:
        st, body = req("POST", "/api/system/auth/login",
                       body={"username": username, "password": pw})
        if st != 200:
            last = (st, body)
            continue
        data = body.get("data") or {}
        tok = data.get("access_token")
        if data.get("must_change_password"):
            st2, body2 = req("PUT", "/api/system/auth/password", token=tok,
                             body={"old_password": pw, "new_password": NEW_PW})
            if st2 != 200:
                print(f"    [login] {username} 改密失败 {st2} {json.dumps(body2, ensure_ascii=False)[:200]}")
                return None
            st3, body3 = req("POST", "/api/system/auth/login",
                             body={"username": username, "password": NEW_PW})
            if st3 != 200:
                time.sleep(65)
                st3, body3 = req("POST", "/api/system/auth/login",
                                 body={"username": username, "password": NEW_PW})
                if st3 != 200:
                    return None
            tok = ((body3.get("data") or {}).get("access_token"))
        return tok
    print(f"    [login] {username} 失败：{last}")
    return None


FIXTURE_SQL = """
DELETE FROM biz_voucher WHERE voucher_word = 'PDF07';
INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
VALUES (2026, 11, '2026-11', '2026-11-01', '2026-11-30', 'OPEN')
ON CONFLICT (period_code) DO NOTHING;
INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
VALUES (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'CLOSED')
ON CONFLICT (period_code) DO NOTHING;
WITH v AS (
  INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, summary,
                           total_debit, total_credit, entry_count, status)
  VALUES ('PDF07', 'PDF07-1', '2026-11', DATE '2026-11-05', 'PDF-07 实收资本', 1000, 1000, 2, 'POSTED')
  RETURNING id
), d AS (SELECT id, account_code, account_name FROM biz_account WHERE account_code = '1001' AND deleted_at IS NULL),
   c AS (SELECT id, account_code, account_name FROM biz_account WHERE account_code = '3001' AND deleted_at IS NULL)
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT v.id, 1, 'PDF07 借 库存现金', d.id, d.account_code, d.account_name, 1000, 0 FROM v, d
UNION ALL
SELECT v.id, 2, 'PDF07 贷 实收资本', c.id, c.account_code, c.account_name, 0, 1000 FROM v, c;
WITH v AS (
  INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, summary,
                           total_debit, total_credit, entry_count, status)
  VALUES ('PDF07', 'PDF07-2', '2026-11', DATE '2026-11-06', 'PDF-07 营业收入', 500, 500, 2, 'POSTED')
  RETURNING id
), d AS (SELECT id, account_code, account_name FROM biz_account WHERE account_code = '1122' AND deleted_at IS NULL),
   c AS (SELECT id, account_code, account_name FROM biz_account WHERE account_code = '5001' AND deleted_at IS NULL)
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT v.id, 1, 'PDF07 借 应收账款', d.id, d.account_code, d.account_name, 500, 0 FROM v, d
UNION ALL
SELECT v.id, 2, 'PDF07 贷 主营业务收入', c.id, c.account_code, c.account_name, 0, 500 FROM v, c;
"""


def seed_fixture():
    print("— 夹具：2026-11 两张 POSTED 凭证（借 1001 1000 / 贷 3001 1000；借 1122 500 / 贷 5001 500）")
    out = psql(FIXTURE_SQL)
    if out.startswith("<ERR"):
        print(f"    夹具 SQL 失败：{out}")
        return False
    n = psql("select count(*) from biz_voucher where voucher_word='PDF07' and status='POSTED'")
    check(n == "2", "夹具凭证 2 张 POSTED 已入库", f"实际 {n}")
    per = psql("select count(*) from biz_accounting_period where period_code in ('2026-11','2026-10')")
    check(per == "2", "2026-11 / 2026-10 期间行存在", f"实际 {per}")
    return True


def mk_role(code, name, perms):
    st, body = req("POST", "/api/system/role", FIX["admin"],
                   {"role_code": code, "role_name": name, "status": 1, "sort_order": 971,
                    "data_scope": 1})
    if st == 200:
        rid = (body.get("data") or {}).get("id")
    else:
        rid = psql(f"select id from sys_role where role_code='{code}' and deleted_at is null limit 1")
        if not rid or rid.startswith("<ERR"):
            print(f"    [role] {code} 建失败 http={st} {body}")
            return None
    psql(f"delete from sys_role_menu where role_id = '{rid}'")
    for perm in perms:
        psql(
            "insert into sys_role_menu (role_id, menu_id, created_at) "
            f"select '{rid}', id, now() from sys_menu where auth_code = '{perm}' "
            "and deleted_at is null on conflict (role_id, menu_id) do nothing"
        )
    got = psql(f"select coalesce(string_agg(m.auth_code, ','), '') from sys_role_menu p "
               f"join sys_menu m on m.id = p.menu_id where p.role_id = '{rid}'")
    print(f"    [role] {code} id={rid} 权限=[{got}]")
    check(got == ",".join(sorted(perms)) or set(got.split(",")) == set(perms),
          f"角色 {code} 权限与计划一致", f"实际=[{got}] 期望={perms}")
    return rid


def mk_user(username, real_name, role_id, password="Pdf07@123456"):
    st, body = req("POST", "/api/system/user", FIX["admin"], {
        "username": username, "real_name": real_name, "password": password,
        "status": 1, "role_ids": [role_id],
    })
    if st == 200:
        uid = (body.get("data") or {}).get("id")
    else:
        uid = psql(f"select id from sys_user where username='{username}' and deleted_at is null limit 1")
        if not uid or uid.startswith("<ERR"):
            print(f"    [user] {username} 建失败 http={st} {body}")
            return None
        psql(f"update sys_user set status = 1, must_change_password = false where id = '{uid}'")
        psql(f"delete from sys_user_role where user_id = '{uid}'")
        psql(f"insert into sys_user_role (user_id, role_id, created_at) values ('{uid}', '{role_id}', now())")
    tok = login(username, [password, NEW_PW])
    if not tok:
        print(f"    [user] {username} 登录失败")
        return None
    return uid, tok


# ============================ 主流程 ============================
def main():
    global PASS, FAIL
    print("=" * 78)
    print("PDF-07 财务报表 PDF 145 实测")
    print(f"BASE={BASE} DB={DB}")
    print("=" * 78)

    FIX["admin"] = login("admin", [ADMIN_PW, NEW_PW, "Admin@123456"])
    if not FIX["admin"]:
        print("admin 登录失败，无法继续")
        return 2
    print("  admin 登录成功")

    if not seed_fixture():
        return 2

    print("— 权限夹具")
    rid_ok = mk_role("PDF07_REPORT", "PDF07 报表导出", ["business:erp:report:export"])
    rid_no = mk_role("PDF07_NONE", "PDF07 无导出权限", ["business:erp:report:balance-sheet"])
    u_ok = mk_user("pdf07_report", "PDF07 报表员", rid_ok) if rid_ok else None
    u_no = mk_user("pdf07_none", "PDF07 无导出员", rid_no) if rid_no else None

    # ---------- ① 三张报表各导一份 ----------
    docs = {}
    for kind, title, headers in [
        ("balance-sheet", "资 产 负 债 表", ["资产", "负债及所有者权益", "期末余额", "期初余额"]),
        ("income-statement", "利 润 表", ["项目", "本期金额", "本年累计"]),
        ("cash-flow", "现 金 流 量 表", ["项目", "本期金额", "本年累计"]),
    ]:
        print(f"— ① {kind} PDF")
        st, hd, raw = get_pdf(f"/api/business/erp/report/{kind}/pdf?period={PERIOD}", FIX["admin"])
        eq(f"{kind}：HTTP 200", st, 200)
        check(raw.startswith(b"%PDF-"), f"{kind}：响应是合法 PDF（%PDF-）", f"前 8 字节={raw[:8]}")
        eq(f"{kind}：Content-Type", hd.get("content-type"), "application/pdf")
        eq(f"{kind}：Content-Length = 实际字节数", hd.get("content-length"), str(len(raw)))
        cd = hd.get("content-disposition", "")
        check(cd.startswith("attachment;"), f"{kind}：Content-Disposition attachment", cd)
        check("filename*=UTF-8''" in cd, f"{kind}：中文文件名走 RFC 5987", cd)
        eq(f"{kind}：禁止缓存 no-store", hd.get("cache-control"), "no-store")
        path, text = keep_pdf(f"report-{kind}.pdf", raw, pages_to_png=(1,))
        pages = pdfpages(path)
        check(pages >= 1, f"{kind}：pages >= 1", f"pages={pages}")
        docs[kind] = {"path": path, "text": text, "headers": hd, "bytes": len(raw)}

        # 表头 / 表尾 / 列头（卡面「表头」「表尾」两行要求）
        contains(f"{kind}：页眉大标题「{title}」", text, title)
        contains(f"{kind}：表头有公司名", text, "云枢")
        contains(f"{kind}：表头有编制单位", text, "编制单位：")
        contains(f"{kind}：表头有期间", text, "2026年11月")
        contains(f"{kind}：表头有报表日期", text, "2026-11-30")
        contains(f"{kind}：表头有金额单位", text, "单位：元")
        for label in ["制表人", "审核人", "日期"]:
            contains(f"{kind}：表尾有「{label}」", text, label)
        for header in headers:
            contains(f"{kind}：列头「{header}」", text, header)
        bad = bad_money(text)
        check(not bad, f"{kind}：文本层所有金额都符合千分位+两位小数（③）", f"不合格={bad[:8]}")
        big = [tok for tok in money_tokens(text) if abs(float(tok.replace(",", ""))) >= 1000]
        if big:
            check(all("," in tok for tok in big),
                  f"{kind}：>=1000 的金额都带千分位逗号（③）", f"缺逗号={[t for t in big if ',' not in t][:5]}")

    # ---------- ② 借贷平衡 ----------
    print("— ② 借贷平衡（资产负债表左右相等）")
    text = docs["balance-sheet"]["text"]
    header, starts, rows = balance_table(text)
    check(bool(rows), "资产负债表主体解析成功（6 列切片）", f"starts={starts} rows={len(rows)}")
    check(len(starts) == 6, "表头解析出 6 列起点", f"starts={starts}")
    total_left = find_row(rows, 0, "资产总计")
    total_right = find_row(rows, 3, "负债和所有者权益总计")
    check(total_left is not None, "左栏有「资产总计」行", f"{total_left}")
    check(total_right is not None, "右栏有「负债和所有者权益总计」行", f"{total_right}")
    if total_left and total_right:
        eq("卡面②：资产总计（期末） == 负债和所有者权益总计（期末）",
           total_left[1], total_right[4])
        eq("资产总计（期末） = 1,500.00（1000 现金 + 500 应收）", total_left[1], "1,500.00")
        eq("负债和所有者权益总计（期末） = 1,500.00（1000 实收 + 500 利润）",
           total_right[4], "1,500.00")
        # 两个总计必须印在**同一行**（对齐），不是一高一低
        same_row = [cells for cells in rows
                    if "资产总计" in cells[0] and "负债和所有者权益总计" in cells[3]]
        check(len(same_row) == 1, "两个总计确实在表格的同一行（版面对齐）", f"match={len(same_row)}")
    contains("勾稽块写明「借贷平衡」", text, "借贷平衡")
    contains("勾稽块写明差额 0.00", text, "差额 0.00")
    contains("勾稽块标题（资产总计 ≡ 负债和所有者权益总计）", text, "资产总计 ≡ 负债和所有者权益总计")
    contains("勾稽块打印资产总计", text, "资产总计（期末） 1,500.00")
    contains("勾稽块打印负债和权益总计", text, "负债和所有者权益总计（期末） 1,500.00")

    # 关键行金额（逐列切片读，避免左右栏串行）
    cash_row = find_row(rows, 0, "货币资金")
    ar_row = find_row(rows, 0, "应收账款")
    capital_row = find_row(rows, 3, "实收资本")
    profit_row = find_row(rows, 3, "未分配利润")
    for row, name in [(cash_row, "货币资金"), (ar_row, "应收账款"),
                      (capital_row, "实收资本"), (profit_row, "未分配利润")]:
        check(row is not None, f"表内有「{name}」行")
    if cash_row and ar_row and capital_row and profit_row:
        eq("货币资金 期末 = 1,000.00", cash_row[1], "1,000.00")
        eq("应收账款 期末 = 500.00", ar_row[1], "500.00")
        eq("实收资本 期末 = 1,000.00", capital_row[4], "1,000.00")
        eq("未分配利润 期末 = 500.00", profit_row[4], "500.00")

    # ---------- ④ 空白项显示「-」 ----------
    print("— ④ 空白项显示「-」（不是 0 也不是空）")
    left_blanks = ["应收票据", "预付账款", "其他应收款", "存货"]
    right_blanks = ["短期借款", "应付账款", "预收账款", "应付职工薪酬", "应交税费", "盈余公积"]
    for label in left_blanks:
        row = find_row(rows, 0, label)
        if row is None:
            check(False, f"左栏有「{label}」行（报表项缺失）", "")
            continue
        for index, col in [(1, "期末"), (2, "期初")]:
            eq(f"左栏「{label}」{col}列 = \"-\"", row[index], "-")
    for label in right_blanks:
        row = find_row(rows, 3, label)
        if row is None:
            check(False, f"右栏有「{label}」行（报表项缺失）", "")
            continue
        for index, col in [(4, "期末"), (5, "期初")]:
            eq(f"右栏「{label}」{col}列 = \"-\"", row[index], "-")
    # 有数据的行的期初列（2026-10 无凭证）也必须是 "-"，不是 0.00
    if cash_row:
        eq("货币资金 期初 = \"-\"（上期无凭证，不冒充 0.00）", cash_row[2], "-")
    # 全表不存在金额恰为 0.00（零一律印 -；校验块里的「差额 0.00」另算）
    body_cells = [cell for cells in rows for cell in cells]
    zero_cells = [cell for cell in body_cells if cell == "0.00"]
    check(not zero_cells, "报表主体里没有任何金额恰为 0.00（零一律印 -）",
          f"命中={zero_cells[:5]}")
    dash_count = sum(1 for cells in rows for cell in cells if cell == "-")
    check(dash_count >= 20, "空白项数量 >= 20（期初整列 + 空项）", f"count={dash_count}")

    # ---------- ③ 利润表 / 现金流量表 ----------
    print("— ③ 利润表 / 现金流量表金额")
    inc = docs["income-statement"]["text"]

    def inc_amounts(label_part):
        for line in inc.splitlines():
            if label_part in line:
                return MONEY_FIND.findall(line)
        return []

    revenue = inc_amounts("营业收入")
    check(len(revenue) >= 2, "利润表 营业收入行有两列金额", f"{revenue}")
    if len(revenue) >= 2:
        eq("利润表 营业收入 本期 = 500.00", revenue[0], "500.00")
        eq("利润表 营业收入 本年累计 = 500.00", revenue[1], "500.00")
    net = inc_amounts("四、净利润")
    check(len(net) >= 2, "利润表 净利润行有两列金额", f"{net}")
    if len(net) >= 2:
        eq("利润表 净利润 本期 = 500.00", net[0], "500.00")
        eq("利润表 净利润 本年累计 = 500.00", net[1], "500.00")
    contains("利润表勾稽：净利润 ≡ 未分配利润本年变动", inc, "未分配利润本年变动")
    contains("利润表勾稽结论一致", inc, "勾稽一致")
    contains("利润表勾稽差额 0.00", inc, "差额 0.00")

    cf = docs["cash-flow"]["text"]
    for keyword in ["经营活动", "投资活动", "筹资活动"]:
        contains(f"现金流量表有「{keyword}」节", cf, keyword)
    closing_line = [line for line in cf.splitlines()
                    if "期末现金及现金等价物余额" in line and len(MONEY_FIND.findall(line)) >= 2]
    check(len(closing_line) == 1, "现金流量表有且仅有一行「期末现金及现金等价物余额」（两列金额）",
          f"{closing_line}")
    if closing_line:
        vals = MONEY_FIND.findall(closing_line[0])
        check(len(vals) >= 2, "期末现金行有两列金额", f"{vals}")
        if len(vals) >= 2:
            eq("现金流量表 期末现金 本期 = 1,000.00", vals[0], "1,000.00")
            eq("现金流量表 期末现金 本年累计 = 1,000.00", vals[1], "1,000.00")
    contains("现金流量表勾稽：期末现金 ≡ 资产负债表货币资金", cf, "与资产负债表货币资金一致")
    contains("现金流量表带简化口径说明（不许静默）", cf, "简化")

    # ---------- 负面分支 ----------
    print("— 负面分支")
    st, _hd, _raw = get_pdf(f"/api/business/erp/report/balance-sheet/pdf?period={PERIOD}")
    eq("401：不带 token", st, 401)
    if u_no:
        st, body = req("GET", f"/api/business/erp/report/balance-sheet/pdf?period={PERIOD}",
                       token=u_no[1])
        eq("403：有报表读权限但没有导出权限", st, 403)
        msg = json.dumps(body, ensure_ascii=False)
        check("report:export" in msg or "权限" in msg, "403 错误信息点名权限码", msg[:200])
    else:
        check(False, "403 用例缺少权限夹具用户", "mk_user 失败")
    if u_ok:
        st, _hd, raw = get_pdf(f"/api/business/erp/report/income-statement/pdf?period={PERIOD}", u_ok[1])
        eq("200：只有 business:erp:report:export 的用户可以导出", st, 200)
        check(raw.startswith(b"%PDF-"), "普通用户拿到的也是合法 PDF")
    else:
        check(False, "200 用例缺少导出权限用户", "mk_user 失败")

    for bad_type in ["profit", "balance-sheetx"]:
        st, body = req("GET", f"/api/business/erp/report/{bad_type}/pdf?period={PERIOD}",
                       token=FIX["admin"])
        eq(f"未知 type「{bad_type}」→ 400", st, 400)
        eq(f"未知 type「{bad_type}」→ 错误码 10001", body.get("code"), 10001)
        check("balance-sheet / income-statement / cash-flow" in json.dumps(body, ensure_ascii=False),
              f"未知 type「{bad_type}」：msg 列出合法值")

    for query, expect_code, label in [
        ("", 10001, "period 缺失 → 400 + 10001"),
        ("?period=2026-13", 10001, "period 月份非法 → 400 + 10001"),
        ("?period=2026-11-01", 10001, "period 带日期 → 400 + 10001"),
        ("?period=1998-07", 66010, "期间不存在 → 400 + 66010（与页面同码）"),
    ]:
        st, body = req("GET", f"/api/business/erp/report/balance-sheet/pdf{query}", token=FIX["admin"])
        eq(label, st, 400)
        eq(f"{label}：错误码", body.get("code"), expect_code)

    # ---------- 审计留痕 ----------
    print("— 审计")
    rows_sql = psql(
        "select operation || '|' || target_type || '|' || count(*) from operation_logs "
        "where operation = 'EXPORT_REPORT_PDF' group by operation, target_type"
    )
    print(f"    审计汇总：{rows_sql}")
    check("EXPORT_REPORT_PDF|biz_report" in rows_sql, "导出写入了 EXPORT_REPORT_PDF 审计行", rows_sql)
    detail = psql(
        "select string_agg(distinct detail->>'report_type', ',') from operation_logs "
        "where operation = 'EXPORT_REPORT_PDF'"
    )
    print(f"    report_type 种类：{detail}")
    check(detail.count(",") == 2, "三张报表的 report_type 都出现在审计里", detail)
    src = psql("select count(*) from operation_logs where operation = 'EXPORT_REPORT_PDF' "
               "and detail->>'source' = 'PDF-07'")
    check(src.isdigit() and int(src) >= 3, "审计 detail.source = PDF-07", f"count={src}")

    print("=" * 78)
    print(f"PASS={PASS} FAIL={FAIL}")
    if FAIL:
        print("失败项：")
        for item in FAILS:
            print(f"  - {item}")
        return 1
    return 0


FIX = {"admin": None}

if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        sys.exit(3)
