#!/usr/bin/env python3
"""M3-07 报价单 PDF 卡的验收演练（真后端 + 真库 + 真 Chromium，不是模拟）。

跑法（三步：起库 → 起后端 → 跑本脚本）：

    # 0) 库：从 145 的模板库克隆（库名必须含 drill，脚本自己 DROP）
    PGPASSWORD=$PGPASSWORD createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m307_drill_1

    # 1) 后端必须跑在**有 Chromium(:9222)** 的机器上（PDF 是 CDP 打出来的）。
    #    99 上没有 chromium-pdf.service，所以演练在 145 上起一个临时实例：
    #      cd /opt/cloudpivot/backend && set -a && . /etc/cloudpivot/cloudpivot-dev.env && set +a
    #      NIK_DATABASE_NAME=cp_m307_drill_1 NIK_SERVER_LISTEN=0.0.0.0:18023 \
    #      PDF_CDP_URL=http://127.0.0.1:9222 /tmp/m307/cloudpivot &

    # 2) 本脚本
    python3 scripts/verify/m307_quotation_pdf_drill.py

验的是**这条链路真的通了**，而不是「代码看起来对」：

  1. PDF 引擎健康（Chromium 连得上）；
  2. 建一张 2 行报价单 → `GET /quotation/{id}/pdf` 返回 **真 PDF**（`%PDF-` 文件头 +
     `application/pdf` + RFC 5987 中文文件名）；
  3. PDF **内容**对（pypdf 抽文本 + NFKC 归一）：页眉「报价单」、单号、客户、物料编码、
     **税率 / 税额两列**（报价单比订单多的两列）、三项合计、大写金额、有效期横幅；
  4. 已过期的报价单照样能导出，且单面印「已过有效期，价格仅供参考」；
  5. 跨页：60 行单据 >=2 页，**每页都有表头**（thead 重复）且页码是真页数；
  6. 错误口径：不存在 → 404；非法 id → 400+66006；未登录 → 401；缺
     `business:erp:quotation:list` → 403（PDF 复用列表码，不另开 :export）；
  7. 审计：`operation_logs` 真落下 `EXPORT_QUOTATION_PDF`（导出是 GET，中间件不记）；
  8. 布局：把第 1 页渲成 PNG（PyMuPDF），人工/视觉核对不折行、不串列。

# PDF 文本抽取的两个坑（本脚本已处理，改脚本时别踩回去）

- **Kangxi 部首**：Chromium 子集字体的 ToUnicode 会把「户 / 页 / 民 / 角」映射成
  罗马字部首码位（`戶` `⻚` `⺠` `⻆`），NFKC 也救不回来 → 断言只用**抽得准**的字
  （报价单 / 税率 / 税额 / 合计 / 有效期 / 制单 / 审核 / 确认 / 共）；
- **字母间距**：标题印的是「报 价 单」，抽取带空格 → 断言前先把所有空白删掉。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import unicodedata
import urllib.error
import urllib.parse
import urllib.request

BASE = os.environ.get("M307_BASE", "http://172.18.10.145:18023")
DB_HOST = os.environ.get("M307_DB_HOST", "172.18.10.145")
DB_NAME = os.environ.get("M307_DB", "cp_m307_drill_1")
DB_USER = os.environ.get("M307_DB_USER", "cloudpivot")
DB_PASS = os.environ.get("M307_DB_PASS", "fbb1263691ff75c31ec99eaf258138b4")
OUT_DIR = os.environ.get("M307_OUT", "/root/.hermes/profiles/backend-developer/cache/scratch/m307_pdf")

CUSTOMER = "d0260000-0000-4000-8000-000000000001"
MAT_A = "01a1bdda-332c-7093-bdc0-ad514a7fc979"
MAT_B = "d2a20001-0000-4000-8000-000000000011"

FAILS: list[str] = []
CHECKS = 0


def call(method: str, path: str, token: str | None = None, body: dict | None = None) -> tuple[int, dict]:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode()
        try:
            return exc.code, json.loads(raw)
        except json.JSONDecodeError:
            return exc.code, {"raw": raw}


def call_pdf(path: str, token: str | None = None) -> tuple[int, bytes, dict[str, str]]:
    req = urllib.request.Request(BASE + path, method="GET")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=180) as resp:
            return resp.status, resp.read(), {k.lower(): v for k, v in resp.headers.items()}
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(), {k.lower(): v for k, v in exc.headers.items()}


def check(name: str, ok: bool, detail: str = "") -> None:
    global CHECKS
    CHECKS += 1
    if ok:
        print(f"  PASS  {name}")
    else:
        FAILS.append(f"{name}{(' — ' + detail) if detail else ''}")
        print(f"  FAIL  {name}  {detail}")


def psql(sql: str) -> str:
    out = subprocess.run(
        ["psql", "-h", DB_HOST, "-U", DB_USER, "-d", DB_NAME, "-tAc", sql],
        env={**os.environ, "PGPASSWORD": DB_PASS, "PATH": "/usr/bin:/bin"},
        check=True,
        capture_output=True,
        text=True,
    )
    return out.stdout.strip()


def pdf_text(data: bytes) -> tuple[str, str, int]:
    """返回 (原始文本, 归一文本, 页数)。

    归一 = NFKC + 去掉所有空白：字母间距与全角括号都不再影响断言。
    """
    import io

    from pypdf import PdfReader

    reader = PdfReader(io.BytesIO(data))
    raw = "\n".join(page.extract_text() or "" for page in reader.pages)
    norm = "".join(unicodedata.normalize("NFKC", raw).split())
    return raw, norm, len(reader.pages)


def quotation_payload(remark: str, valid_until: str = "2026-12-01", quote_date: str = "2026-11-17", lines: int = 2):
    items = [
        {"material_id": MAT_A, "qty": 3, "price": "12.3456", "tax_rate": 13, "remark": "行一 加急"},
        {"material_id": MAT_B, "qty": 2, "price": "100", "tax_rate": 13},
    ]
    for index in range(2, lines):
        items.append(
            {
                "material_id": MAT_B,
                "qty": index + 1,
                "price": "88.8",
                "tax_rate": 13,
                "remark": f"第{index + 1}行",
            }
        )
    return {
        "customer_id": CUSTOMER,
        "quote_date": quote_date,
        "valid_until": valid_until,
        "remark": remark,
        "items": items,
    }


def main() -> int:
    os.makedirs(OUT_DIR, exist_ok=True)

    # ---------- 1. 登录 ----------
    status, body = call("POST", "/api/system/auth/login", body={"username": "admin", "password": "Admin@123456"})
    check("登录 200", status == 200, f"status={status} body={body}")
    token = body.get("data", {}).get("access_token", "")
    check("拿到 access_token", bool(token))

    # ---------- 2. PDF 引擎健康（Chromium 必须连得上，否则后面 503） ----------
    status, body = call("GET", "/api/business/pdf/health", token)
    health = body.get("data", {}) or {}
    check("PDF 引擎健康 200", status == 200, f"status={status} body={body}")
    check(
        "引擎连上了真 Chromium（有 browser 版本号）",
        str(health.get("browser", "")).startswith("Chrome/"),
        str(health)[:200],
    )
    check("CDP 地址就是本机 :9222", "9222" in str(health.get("cdp_url", "")), str(health.get("cdp_url")))

    # ---------- 3. 建单 ----------
    status, body = call("POST", "/api/business/erp/quotation", token, quotation_payload("M3-07 PDF 演练"))
    check("建单 200", status == 200, f"status={status} body={body}")
    created = body.get("data", {})
    qid = created.get("id", "")
    quote_no = created.get("quote_no", "")
    check("单号 QT-20261117-…", quote_no.startswith("QT-20261117-"), quote_no)
    check("明细 2 行", len(created.get("items", [])) == 2, str(len(created.get("items", []))))
    check("价税合计 267.8516", str(created.get("total_with_tax")) == "267.8516", str(created.get("total_with_tax")))
    customer_name = created.get("customer_name") or ""
    mat_a_code, mat_b_code = (
        psql(f"SELECT material_code FROM biz_material WHERE id = '{MAT_A}'"),
        psql(f"SELECT material_code FROM biz_material WHERE id = '{MAT_B}'"),
    )

    # ---------- 4. 导出 PDF ----------
    status, data, headers = call_pdf(f"/api/business/erp/quotation/{qid}/pdf", token)
    check("导出 200", status == 200, f"status={status} body={data[:200]!r}")
    check(
        "Content-Type = application/pdf",
        headers.get("content-type", "").startswith("application/pdf"),
        headers.get("content-type", ""),
    )
    check("报文是 PDF（%PDF- 文件头）", data[:5] == b"%PDF-", repr(data[:8]))
    disposition = headers.get("content-disposition", "")
    check("Content-Disposition 带中文下载名（RFC 5987）", "filename*=UTF-8''" in disposition, disposition)
    decoded_name = urllib.parse.unquote(disposition.split("UTF-8''")[-1]) if "UTF-8''" in disposition else disposition
    check("下载名是「报价单-<单号>.pdf」", decoded_name.startswith(f"报价单-{quote_no}") and decoded_name.endswith(".pdf"), decoded_name)
    check("PDF 大小 > 20KB（有真内容，不是空壳）", len(data) > 20_000, f"{len(data)} 字节")
    path_one = os.path.join(OUT_DIR, f"quotation-{quote_no}.pdf")
    with open(path_one, "wb") as fh:
        fh.write(data)

    # ---------- 5. PDF 内容对 ----------
    _raw, flat, pages = pdf_text(data)
    check("页数 >= 1", pages >= 1, str(pages))
    check("页眉标题「报价单」（印的是「报 价 单」，归一后比对）", "报价单" in flat, flat[:120])
    check("单号在单面上", quote_no in flat, quote_no)
    check("客户名称在单面上", bool(customer_name) and customer_name in flat, customer_name)
    check("物料编码两行都在", mat_a_code in flat and mat_b_code in flat, f"{mat_a_code}/{mat_b_code}")
    check("表头 11 列：税率 + 税额两列在", "税率" in flat and "税额" in flat)
    check("税率带百分号（13%）", "13%" in flat, "13% 未出现")
    check(
        "三项合计都在（不含税 237.04 / 税额 30.81 / 价税合计 267.85）",
        "237.04" in flat and "30.81" in flat and "267.85" in flat,
        "合计金额未印全",
    )
    check("大写金额（贰佰陆拾柒元捌角伍分）", "贰佰陆拾柒元" in flat and "伍分" in flat, flat[:200])
    check("大写与小数同源（(267.85) 与大写并列）", "(267.85)" in flat or "267.85" in flat)
    check("有效期横幅（2026-12-01 含当天）", "2026-12-01" in flat and "含当天" in flat)
    check("有效期内提示语", "有效期内可按本单价格成交" in flat)
    check("签字位 3 个（制单 / 审核 / 确认）", "制单" in flat and "审核" in flat and "确认" in flat)
    check("页脚页码是真页数（共 N）", f"共{pages}" in flat, flat[-160:])
    check("行备注印在明细行上", "行一加急" in flat, "行备注丢失")

    # ---------- 6. 审计真的落库（导出是 GET，中间件不记） ----------
    row = psql(
        "SELECT operation || '|' || target_type || '|' || coalesce(detail->>'doc_no','') "
        "|| '|' || coalesce(detail->>'row_count','') || '|' || coalesce(detail->>'total_with_tax','') "
        f"FROM operation_logs WHERE target_id = '{qid}' AND operation = 'EXPORT_QUOTATION_PDF' "
        "ORDER BY created_at DESC LIMIT 1"
    )
    parts = row.split("|") if row else []
    check("审计落库 EXPORT_QUOTATION_PDF", bool(row), row or "（查不到）")
    check("审计 target_type = biz_quotation", len(parts) > 1 and parts[1] == "biz_quotation", row)
    check("审计 detail.doc_no 对", len(parts) > 2 and parts[2] == quote_no, row)
    check("审计 detail.row_count = 2", len(parts) > 3 and parts[3] == "2", row)
    check("审计 detail.total_with_tax ≈ 267.85", len(parts) > 4 and parts[4].startswith("267.85"), row)

    # ---------- 7. 过期报价单：照样能导出，单面印「仅供参考」 ----------
    status, body = call(
        "POST",
        "/api/business/erp/quotation",
        token,
        quotation_payload("M3-07 PDF 演练：已过期", valid_until="2026-01-31", quote_date="2026-01-01", lines=1),
    )
    expired_id = body.get("data", {}).get("id", "")
    check("建一张已过期的单", status == 200 and bool(expired_id), f"status={status} body={body}")
    status, data, _headers = call_pdf(f"/api/business/erp/quotation/{expired_id}/pdf", token)
    check("过期单也能导出 200", status == 200, f"status={status} body={data[:200]!r}")
    if status == 200:
        _raw, flat, _pages = pdf_text(data)
        check("过期单印「已过有效期…仅供参考」", "已过有效期" in flat and "仅供参考" in flat, flat[:200])
        check("过期单不再印「有效期内可按本单」", "有效期内可按本单价格成交" not in flat)
        with open(os.path.join(OUT_DIR, "quotation-expired.pdf"), "wb") as fh:
            fh.write(data)

    # ---------- 8. 跨页：60 行 >=2 页，每页都有表头 + 真页码 ----------
    status, body = call("POST", "/api/business/erp/quotation", token, quotation_payload("M3-07 PDF 演练：跨页", lines=60))
    many_id = body.get("data", {}).get("id", "")
    many_no = body.get("data", {}).get("quote_no", "")
    check("建一张 60 行的单", status == 200 and bool(many_id), f"status={status} body={body}")
    status, data, _headers = call_pdf(f"/api/business/erp/quotation/{many_id}/pdf", token)
    check("60 行导出 200", status == 200, f"status={status}")
    if status == 200:
        _raw, flat, pages = pdf_text(data)
        check("60 行跨页（页数 >= 2）", pages >= 2, str(pages))
        heads = flat.count("物料编码")
        check("每页都重复表头（thead display:table-header-group）", heads == pages, f"表头 {heads} 次 / 共 {pages} 页")
        check("页脚总页数是真页数", f"共{pages}" in flat, flat[-160:])
        check("最后一行没被腰斩（序号 60 与末行备注都在）", "60" in flat and "第60行" in flat)
        with open(os.path.join(OUT_DIR, f"quotation-many-{many_no}.pdf"), "wb") as fh:
            fh.write(data)

    # ---------- 9. 错误口径 ----------
    status, body, _headers = call_pdf("/api/business/erp/quotation/00000000-0000-4000-8000-0000000000ff/pdf", token)
    check("不存在的单 → 404", status == 404, f"status={status}")
    status, body, _headers = call_pdf("/api/business/erp/quotation/not-a-uuid/pdf", token)
    parsed = json.loads(body.decode()) if body[:1] == b"{" else {}
    check("非法 id → 400", status == 400, f"status={status}")
    check("非法 id 错误码 66006", parsed.get("code") == 66006, str(parsed.get("code")))
    status, _body, _headers = call_pdf(f"/api/business/erp/quotation/{qid}/pdf")
    check("未登录 → 401", status == 401, f"status={status}")

    # ---------- 10. 缺码 → 403（PDF 复用 quotation:list，不另开 :export） ----------
    probe = "m307pdfprobe"
    psql(f"DELETE FROM sys_user WHERE username = '{probe}'")
    call(
        "POST",
        "/api/system/user",
        token,
        {
            "username": probe,
            "password": "Drill@123456",
            "real_name": "M3-07 PDF 探针",
            "status": 1,
            "role_ids": [],
            "remark": "M3-07 验证：无 quotation:list 码的用户点导出必须 403",
        },
    )
    psql(f"UPDATE sys_user SET must_change_password = false WHERE username = '{probe}'")
    status, body = call("POST", "/api/system/auth/login", body={"username": probe, "password": "Drill@123456"})
    probe_token = body.get("data", {}).get("access_token", "")
    check("探针用户能登录（证明 403 不是登录失败）", status == 200 and bool(probe_token), f"status={status} body={body}")
    if probe_token:
        status, body, _headers = call_pdf(f"/api/business/erp/quotation/{qid}/pdf", probe_token)
        parsed = json.loads(body.decode()) if body[:1] == b"{" else {}
        check("缺 business:erp:quotation:list → 403", status == 403, f"status={status}")
        print(f"        （403 错误码 = {parsed.get('code')}，文案 = {parsed.get('msg')}）")
    psql(f"DELETE FROM sys_user WHERE username = '{probe}'")

    # ---------- 11. 布局：第 1 页渲成 PNG（人眼/视觉核对不折行不串列） ----------
    try:
        import pymupdf

        doc = pymupdf.open(path_one)
        pix = doc.load_page(0).get_pixmap(dpi=120)
        png = os.path.join(OUT_DIR, "quotation-page1.png")
        pix.save(png)
        print(f"\n  第 1 页已渲成 PNG：{png}（{pix.width}×{pix.height}）")
        doc.close()
        check("PyMuPDF 渲染第 1 页 PNG", os.path.exists(png))
    except Exception as exc:  # noqa: BLE001
        check("PyMuPDF 渲染第 1 页 PNG", False, str(exc))

    print()
    print(f"共 {CHECKS} 条断言，失败 {len(FAILS)} 条")
    if FAILS:
        for item in FAILS:
            print("  ✗ " + item)
        return 1
    print("全部通过 ✓")
    return 0


if __name__ == "__main__":
    sys.exit(main())
