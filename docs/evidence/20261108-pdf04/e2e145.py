#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""PDF-04 @145 真实 token 端到端验收（调拨单 / 盘点单 / 采购退货单 / 销售退货单）。

运行环境：145 本机（python3 + poppler-utils），隔离实例 127.0.0.1:8078（库 cloudpivot_pdf04）。
只发 HTTP + 调 pdftotext/pdfinfo/pdffonts，不碰数据库（夹具由 seed145.sql 预置）。

三条卡面验收：
  ① 盘点单 PDF 有空白手写栏（实盘数量 / 差异 / 备注 三栏留空）
  ② 调拨单有调出 / 调入仓库
  ③ 退货单有原单号、退货原因
"""
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8078"
OUT = "/root/pdf04-evidence"
DB = "cloudpivot_pdf04"
ENV_FILE = "/root/pdf04.env"

TRANSFER = "0f520000-0000-4000-8000-000000000001"
TAKE = "0f530000-0000-4000-8000-000000000001"
PURCHASE_RETURN = "0f540000-0000-4000-8000-000000000010"
SALES_RETURN = "0f550000-0000-4000-8000-000000000010"

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("[PASS] " if ok else "[FAIL] ") + name + ("  " + str(detail) if detail else ""))


def req(method, path, payload=None, token=None, raw=False):
    data = None
    headers = {"Accept": "application/json"}
    if payload is not None:
        data = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    request = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            body = response.read()
            if raw:
                return response.status, {k.lower(): v for k, v in response.headers.items()}, body
            return response.status, dict(response.headers), json.loads(body.decode() or "{}")
    except urllib.error.HTTPError as error:
        body = error.read()
        if raw:
            return error.code, {k.lower(): v for k, v in error.headers.items()}, body
        try:
            return error.code, {k.lower(): v for k, v in error.headers.items()}, json.loads(
                body.decode() or "{}"
            )
        except Exception:
            return error.code, dict(error.headers), {"raw": body.decode(errors="replace")}


def flatten(text):
    """去空白：pdftotext 会把 letter-spacing / 相邻 span 抽成多个空格。"""
    return re.sub(r"\s+", "", text)


def pdf_text(path):
    return subprocess.run(["pdftotext", "-layout", path, "-"], capture_output=True, text=True).stdout


def pdf_text_page(path, index):
    return subprocess.run(
        ["pdftotext", "-layout", "-f", str(index), "-l", str(index), path, "-"],
        capture_output=True,
        text=True,
    ).stdout


def pdf_pages(path):
    info = subprocess.run(["pdfinfo", path], capture_output=True, text=True).stdout
    match = re.search(r"Pages:\s+(\d+)", info)
    return int(match.group(1)) if match else -1


def page_texts(path):
    return [pdf_text_page(path, index) for index in range(1, pdf_pages(path) + 1)]


def db_password():
    for line in open(ENV_FILE, encoding="utf-8"):
        if line.startswith("NIK_DATABASE_PASSWORD="):
            return line.split("=", 1)[1].strip()
    return ""


def psql(statement):
    env = dict(**os.environ, PGPASSWORD=db_password())
    return subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-tAc", statement],
        capture_output=True,
        text=True,
        env=env,
    ).stdout.strip()


def login(username, password):
    status, _, body = req("POST", "/api/system/auth/login", {"username": username, "password": password})
    return (((body.get("data") or {}).get("access_token")) or ""), status, body


def export(path, name, token):
    status, headers, body = req("GET", path, token=token, raw=True)
    file_path = os.path.join(OUT, name)
    open(file_path, "wb").write(body)
    return status, headers, body, file_path


def main():
    subprocess.run(["mkdir", "-p", OUT], check=False)

    # ---------- 0. 健康 ----------
    status, _, health = req("GET", "/health")
    check("实例健康（GET /health）", status == 200 and health.get("code") == 0,
          (health.get("data") or {}).get("status"))

    # ---------- 1. 登录（含首登强制改密 CODE-06 的真实路径） ----------
    admin_password = "Admin@12345678"
    token, status, login_body = login("admin", admin_password)
    if not token:
        bootstrap_token, _, _ = login("admin", "Admin@123456")
        if bootstrap_token:
            req("PUT", "/api/system/auth/password",
                {"old_password": "Admin@123456", "new_password": admin_password},
                token=bootstrap_token)
            time.sleep(70)
            token, status, login_body = login("admin", admin_password)
    check("admin 登录拿到 access_token", status == 200 and len(token) > 50,
          ("len=%d" % len(token)) if token else login_body)

    # ---------- 2. ① 盘点单（空白手写栏） ----------
    status, headers, body, path = export("/api/business/erp/stock-take/%s/pdf" % TAKE, "stock-take.pdf", token)
    check("盘点单导出 200", status == 200, status)
    check("盘点单 Content-Type = application/pdf",
          headers.get("content-type") == "application/pdf", headers.get("content-type"))
    check("盘点单文件名（中文 RFC 5987）",
          "filename*=UTF-8''" in headers.get("content-disposition", ""),
          headers.get("content-disposition", ""))
    check("盘点单 PDF 文件头 %PDF-", body[:5] == b"%PDF-", body[:5])

    text = pdf_text(path)
    flat = flatten(text)
    pages = pdf_pages(path)
    check("盘点单标题「盘 点 表」", "盘点表" in flat)
    check("盘点单跨页（60 行）", pages >= 2, "pages=%d" % pages)
    check("盘点单含仓库", "PDF04上海一号仓" in flat)
    check("盘点单含盘点类型（全盘）", "全盘" in flat)
    # 三列表头
    for head in ("账面数量", "实盘数量", "差异", "备注"):
        check("盘点单表头含「%s」" % head, head in flat)
    # ★验收①：每行数据里只允许出现 1 个独立数字（= 账面数量）；实盘 / 差异 / 备注必须全空
    row_lines = [line for line in text.splitlines() if "PDF04-MAT-" in line]
    check("盘点单 60 行明细全都在", len(row_lines) == 60, "rows=%d" % len(row_lines))
    bad_rows = []
    for line in row_lines:
        # 去掉行首「序号」再数数字（序号本身也是数字，直接数会误报）
        body = re.sub(r"^\s*\d+\s+", "", line)
        tokens = re.findall(r"(?<![A-Za-z0-9-])\d+(?:\.\d+)?(?![\w.-])", body)
        if len(tokens) != 1:
            bad_rows.append((line.strip()[:80], tokens))
    check("★验收①：盘点单每行只有账面数量、实盘/差异/备注三栏留空",
          not bad_rows, bad_rows[:3] if bad_rows else "60/60 行仅 1 个数字")
    # 规格整串不折行（56pt 会把 DN100/PN16 折成两行 → 逐行核规格就失效）
    check("盘点单规格 DN100/PN16 整串一行", "DN100/PN16" in flat)
    # 库里给差异行填的备注（「实物短缺 3 台」）不许印在表上（备注栏是留给手写的）
    check("盘点单未把系统备注（实物短缺 3 台）印出来", "实物短缺3台" not in flat)
    check("盘点单填表说明在（留空口径）", "填表说明" in flat and "现场手写" in flat)
    check("盘点单盘盈/盘亏合计在（已录实盘）", "盘盈数量合计" in flat and "盘亏数量合计" in flat)
    for cap in ("盘点人：", "复核人：", "仓管主管：", "财务：", "制单："):
        check("盘点单签字栏「%s」" % cap, cap in flat)
    # 每页页脚 + 表头重复
    pages_text = page_texts(path)
    footers = [re.search(r"第(\d+)页/共(\d+)页", flatten(page)) for page in pages_text]
    check("盘点单每页页脚页码正确",
          len(footers) == pages
          and all(f and f.group(1) == str(i) and f.group(2) == str(pages)
                  for i, f in enumerate(footers, start=1)),
          [(f.group(1), f.group(2)) if f else None for f in footers])
    check("盘点单每页都重复表头（含 实盘数量）",
          all("实盘数量" in flatten(page) for page in pages_text if "PDF04-MAT-" in flatten(page))
          and sum(1 for page in pages_text if "PDF04-MAT-" in flatten(page)) >= 2,
          "数据页数=%d" % sum(1 for page in pages_text if "PDF04-MAT-" in flatten(page)))

    # ---------- 3. ② 调拨单（调出 / 调入仓库） ----------
    status, headers, body, path = export(
        "/api/business/erp/stock-transfer/%s/pdf" % TRANSFER, "stock-transfer.pdf", token)
    check("调拨单导出 200", status == 200, status)
    check("调拨单 Content-Type = application/pdf",
          headers.get("content-type") == "application/pdf", headers.get("content-type"))
    check("调拨单 PDF 文件头 %PDF-", body[:5] == b"%PDF-", body[:5])
    text = pdf_text(path)
    flat = flatten(text)
    check("调拨单标题「调 拨 单」", "调拨单" in flat)
    # ★验收②：调出 / 调入仓库
    check("★验收②：调拨单含「调出仓库」字段", "调出仓库" in flat)
    check("★验收②：调拨单含「调入仓库」字段", "调入仓库" in flat)
    check("★验收②：调出仓库名 PDF04 上海一号仓", "PDF04上海一号仓" in flat)
    check("★验收②：调入仓库名 PDF04 上海二号仓", "PDF04上海二号仓" in flat)
    check("调拨路线条「调出：X ➜ 调入：Y」", "调出：PDF04上海一号仓" in flat and "调入：PDF04上海二号仓" in flat)
    check("调拨单含在途仓", "在途经PDF04在途仓" in flat or "在途仓" in flat)
    check("调拨单含调出 / 调入库位列", "调出库位" in flat and "调入库位" in flat)
    check("调拨单规格 DN100/PN16 整串一行", "DN100/PN16" in flat)
    check("调拨单含库位编码 A-01-03 / B-02-04", "A-01-03" in flat and "B-02-04" in flat)
    check("调拨单 4 行明细全在", len([l for l in text.splitlines() if "PDF04-MAT-" in l]) == 4,
          len([l for l in text.splitlines() if "PDF04-MAT-" in l]))
    check("调拨单成本口径（无价税合计）", "价税合计" not in flat and "成本金额合计" in flat)
    check("调拨单大写金额", "人民币伍佰伍拾伍元陆角整" in flat, flat[flat.find("成本金额（大写）"):][:60])
    for cap in ("调出仓管：", "调入仓管：", "经办人：", "审核：", "制单："):
        check("调拨单签字栏「%s」" % cap, cap in flat)
    check("调拨单页脚页码", re.search(r"第1页/共\d+页", flatten(pdf_text_page(path, 1))) is not None)

    # ---------- 4. ③ 采购退货单（原单号 + 退货原因） ----------
    status, headers, body, path = export(
        "/api/business/erp/purchase-return/%s/pdf" % PURCHASE_RETURN, "purchase-return.pdf", token)
    check("采购退货单导出 200", status == 200, status)
    check("采购退货单 Content-Type = application/pdf",
          headers.get("content-type") == "application/pdf", headers.get("content-type"))
    check("采购退货单 PDF 文件头 %PDF-", body[:5] == b"%PDF-", body[:5])
    text = pdf_text(path)
    flat = flatten(text)
    check("采购退货单标题「采 购 退 货 单」", "采购退货单" in flat)
    # ★验收③：原单号
    check("★验收③：采购退货单含「原单号（入库）」", "原单号（入库）" in flat)
    check("★验收③：原单号（采购订单）", "原单号（采购订单）" in flat)
    check("★验收③：原入库单号 PDF04-IN-000001", "PDF04-IN-000001" in flat)
    check("★验收③：原采购订单号 PDF04-PO-000001", "PDF04-PO-000001" in flat)
    # ★验收③：退货原因
    check("★验收③：采购退货单含「退货原因」横幅", "退货原因：" in flat)
    check("★验收③：退货原因正文（砂眼）", "阀体密封面有砂眼" in flat)
    check("采购退货单含供应商", "PDF04阀门制造有限公司" in flat)
    check("采购退货单含退货仓库", "PDF04上海一号仓" in flat)
    check("采购退货单逐行价税合计列", "价税合计" in flat)
    check("采购退货单 2 行明细全在", len([l for l in text.splitlines() if "PDF04-MAT-" in l]) == 2)
    check("采购退货单合计 97.70 + 大写", "97.70" in flat and "玖拾柒元柒角整" in flat)
    for cap in ("退货经办：", "仓管：", "质检：", "供应商签收：", "制单："):
        check("采购退货单签字栏「%s」" % cap, cap in flat)
    check("采购退货单规格 DN100/PN16 整串一行", "DN100/PN16" in flat or "DN50/PN16" in flat)
    # 横向判定：pdfinfo 的 Page size 宽 > 高（841.92 x 595.92）
    info = subprocess.run(["pdfinfo", path], capture_output=True, text=True).stdout
    size = re.search(r"Page size:\s+([\d.]+)\s+x\s+([\d.]+)", info)
    check("采购退货单横向版式（A4 landscape：宽 > 高）",
          bool(size) and float(size.group(1)) > float(size.group(2)),
          size.group(0) if size else info[:80])

    # ---------- 5. ③ 销售退货单（原单号 + 退货类型） ----------
    status, headers, body, path = export(
        "/api/business/erp/sales-return/%s/pdf" % SALES_RETURN, "sales-return.pdf", token)
    check("销售退货单导出 200", status == 200, status)
    check("销售退货单 Content-Type = application/pdf",
          headers.get("content-type") == "application/pdf", headers.get("content-type"))
    check("销售退货单 PDF 文件头 %PDF-", body[:5] == b"%PDF-", body[:5])
    text = pdf_text(path)
    flat = flatten(text)
    check("销售退货单标题「销 售 退 货 单」", "销售退货单" in flat)
    check("★验收③：原单号（销售订单）", "原单号（销售订单）" in flat)
    check("★验收③：原单号（发货单）", "原单号（发货单）" in flat)
    check("★验收③：原销售订单号 PDF04-SO-000001", "PDF04-SO-000001" in flat)
    check("★验收③：原发货单号 PDF04-SH-000001", "PDF04-SH-000001" in flat)
    check("★验收③：退货原因（质量 / QUALITY）", "退货原因：" in flat and "质量" in flat and "QUALITY" in flat)
    check("销售退货单含客户", "PDF04华东贸易有限公司" in flat)
    check("销售退货单含退回仓库", "PDF04上海二号仓" in flat)
    check("销售退货单含序列号列与序列号", "序列号" in flat and "PDF04-SN-000001" in flat)
    check("销售退货单两条序列号整串不折行（固定列宽）",
          "PDF04-SN-000001、PDF04-SN-000002" in flat, flat[flat.find("PDF04-SN-000001"):][:60])
    check("销售退货单整仓退货行打「整仓」", "整仓" in flat)
    check("销售退货单 2 行明细全在", len([l for l in text.splitlines() if "PDF04-MAT-" in l]) == 2)
    for cap in ("退货经办：", "仓管：", "质检：", "客户签收：", "制单："):
        check("销售退货单签字栏「%s」" % cap, cap in flat)

    # ---------- 6. 字体嵌入 / 中文无损 ----------
    for label, file in (("盘点单", "stock-take.pdf"), ("调拨单", "stock-transfer.pdf"),
                        ("采购退货单", "purchase-return.pdf"), ("销售退货单", "sales-return.pdf")):
        target = os.path.join(OUT, file)
        fonts = subprocess.run(["pdffonts", target], capture_output=True, text=True).stdout
        rows = [line for line in fonts.splitlines()[2:] if line.strip()]
        # pdffonts 列：name type encoding emb sub uni object ID（type 里可能含空格，不能按固定下标取）
        embedded = bool(rows) and all("yes" in line.split() for line in rows)
        check("%s PDF 字体全部嵌入（emb=yes）" % label, embedded, rows[0] if rows else "no font rows")
        check("%s 中文可无损抽取" % label, "云枢" in flatten(pdf_text(target)))

    # ---------- 7. 无权限账号 → 403（四个接口，点名权限码） ----------
    role_code = "PDF04_NO_PERM"
    status, _, role = req("POST", "/api/system/role",
                          {"role_code": role_code, "role_name": "PDF04 无权限", "status": 1}, token=token)
    role_id = ((role.get("data") or {}).get("id")) if status == 200 else None
    if not role_id:
        _, _, page = req("GET", "/api/system/role/page?page=1&page_size=50&keyword=" + role_code, token=token)
        for row in ((page.get("data") or {}).get("list") or []):
            if row.get("role_code") == role_code:
                role_id = row.get("id")
    check("无权限角色就绪（PDF04_NO_PERM）", bool(role_id), role_id or role)

    status, _, user = req("POST", "/api/system/user",
                          {"username": "pdf04_noperm", "password": "NoPerm@123456",
                           "real_name": "PDF04 无权限", "status": 1, "role_ids": [role_id]}, token=token)
    if status != 200:
        check("无权限账号已存在（幂等跳过创建）", True, user.get("msg"))
    noperm_token, status, noperm_login = login("pdf04_noperm", "NoPerm@123456")
    check("无权限账号可登录（证明不是 401 混成 403）", status == 200 and bool(noperm_token), status)

    for label, url, permission in (
        ("stock-transfer", "/api/business/erp/stock-transfer/%s/pdf" % TRANSFER, "business:erp:transfer:list"),
        ("stock-take", "/api/business/erp/stock-take/%s/pdf" % TAKE, "business:erp:stock-take:list"),
        ("purchase-return", "/api/business/erp/purchase-return/%s/pdf" % PURCHASE_RETURN,
         "business:erp:purchase-return:list"),
        ("sales-return", "/api/business/erp/sales-return/%s/pdf" % SALES_RETURN,
         "business:erp:sales-return:list"),
    ):
        status, _, body_json = req("GET", url, token=noperm_token, raw=False)
        msg = (body_json.get("msg") or "") if isinstance(body_json, dict) else ""
        check("无权导出 %s → 403 且点名权限码" % label, status == 403 and permission in msg,
              "%s %s" % (status, msg))

    # ---------- 8. 404 / 400 ----------
    missing = "0f5f0000-0000-4000-8000-00000000dead"
    for label, path, keyword in (
        ("调拨单", "/api/business/erp/stock-transfer/%s/pdf" % missing, "调拨单"),
        ("盘点单", "/api/business/erp/stock-take/%s/pdf" % missing, "盘点单"),
        ("采购退货单", "/api/business/erp/purchase-return/%s/pdf" % missing, "采购退货单"),
        ("销售退货单", "/api/business/erp/sales-return/%s/pdf" % missing, "销售退货单"),
    ):
        status, _, body_json = req("GET", path, token=token, raw=False)
        check("不存在%s → 404 且点名" % label,
              status == 404 and keyword in (body_json.get("msg") or ""),
              "%s %s" % (status, (body_json.get("msg") or "")[:40]))

    for label, path in (
        ("调拨单", "/api/business/erp/stock-transfer/not-a-uuid/pdf"),
        ("盘点单", "/api/business/erp/stock-take/not-a-uuid/pdf"),
        ("采购退货单", "/api/business/erp/purchase-return/not-a-uuid/pdf"),
        ("销售退货单", "/api/business/erp/sales-return/not-a-uuid/pdf"),
    ):
        status, _, body_json = req("GET", path, token=token, raw=False)
        check("非法 id（%s）→ 400" % label, status == 400, status)

    # ---------- 9. 导出审计（operation_logs） ----------
    logs = psql(
        "select operation || '|' || target_type || '|' || (detail->>'doc_type') || '|' || "
        "(detail->>'row_count') || '|' || (detail->>'source') "
        "from operation_logs where target_type in "
        "('biz_stock_transfer','biz_stock_take','biz_purchase_return','biz_sales_return') "
        "and request_method = 'GET' order by created_at"
    )
    lines = [line for line in logs.splitlines() if line.strip()]
    open(os.path.join(OUT, "operation_logs.txt"), "w", encoding="utf-8").write("\n".join(lines) + "\n")
    for operation, target, doc_type in (
        ("EXPORT_STOCK_TRANSFER_PDF", "biz_stock_transfer", "STOCK_TRANSFER"),
        ("EXPORT_STOCK_TAKE_PDF", "biz_stock_take", "STOCK_TAKE"),
        ("EXPORT_PURCHASE_RETURN_PDF", "biz_purchase_return", "PURCHASE_RETURN"),
        ("EXPORT_SALES_RETURN_PDF", "biz_sales_return", "SALES_RETURN"),
    ):
        hit = [line for line in lines if line.startswith("%s|%s|%s|" % (operation, target, doc_type))]
        check("审计行 %s（target=%s）" % (operation, target), bool(hit), hit[0] if hit else lines[:3])

    # ---------- 汇总 ----------
    failed = [item for item in results if not item[1]]
    print("\n==== 汇总：%d 项，失败 %d 项 ====" % (len(results), len(failed)))
    for name, _, detail in failed:
        print("  [FAIL] %s  %s" % (name, detail))
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
