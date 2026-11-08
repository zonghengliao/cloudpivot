#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""PDF-03 @145 真实 token 端到端验收（合同 / 收货入库单 / 出库单）。

运行环境：145 本机（python3 + poppler-utils），实例 127.0.0.1:8078（库 cloudpivot_pdf03）。
只发 HTTP + 调 pdftotext/pdfinfo/pdffonts，不碰数据库（夹具由 seed145.sql 预置）。
"""
import json
import re
import subprocess
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8078"
OUT = "/root/pdf03-evidence"
CONTRACT = "0f050000-0000-4000-8000-000000000050"
STOCK_IN = "0f050000-0000-4000-8000-000000000010"
STOCK_OUT = "0f050000-0000-4000-8000-000000000020"

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
        with urllib.request.urlopen(request, timeout=60) as response:
            body = response.read()
            if raw:
                return response.status, {k.lower(): v for k, v in response.headers.items()}, body
            return response.status, dict(response.headers), json.loads(body.decode() or "{}")
    except urllib.error.HTTPError as error:
        body = error.read()
        if raw:
            return error.code, {k.lower(): v for k, v in error.headers.items()}, body
        try:
            return error.code, {k.lower(): v for k, v in error.headers.items()}, json.loads(body.decode() or "{}")
        except Exception:
            return error.code, dict(error.headers), {"raw": body.decode(errors="replace")}


def flatten(text):
    """去空白：pdftotext 会把 letter-spacing / 相邻 span 抽成多个空格。"""
    return re.sub(r"\s+", "", text)


def pdf_text(path):
    return subprocess.run(["pdftotext", "-layout", path, "-"], capture_output=True, text=True).stdout


def pdf_pages(path):
    info = subprocess.run(["pdfinfo", path], capture_output=True, text=True).stdout
    match = re.search(r"Pages:\s+(\d+)", info)
    return int(match.group(1)) if match else -1


def page_texts(path):
    pages = pdf_pages(path)
    return [pdf_text_page(path, index) for index in range(1, pages + 1)]


def pdf_text_page(path, index):
    return subprocess.run(
        ["pdftotext", "-layout", "-f", str(index), "-l", str(index), path, "-"],
        capture_output=True,
        text=True,
    ).stdout


def db_password():
    for line in open("/root/pdf03.env", encoding="utf-8"):
        if line.startswith("NIK_DATABASE_PASSWORD="):
            return line.split("=", 1)[1].strip()
    return ""


def sql(statement):
    """145 本机直连隔离库跑一条 SQL（只用于测试夹具：关首登改密闸门）。"""
    env = dict(**__import__("os").environ, PGPASSWORD=db_password())
    return subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", "cloudpivot_pdf03", "-tAc", statement],
        capture_output=True,
        text=True,
        env=env,
    ).stdout.strip()


def login(username, password):
    status, _, body = req("POST", "/api/system/auth/login", {"username": username, "password": password})
    return (((body.get("data") or {}).get("access_token")) or ""), status, body


def main():
    subprocess.run(["mkdir", "-p", OUT], check=False)

    # ---------- 0. 健康 ----------
    status, _, health = req("GET", "/health")
    check("实例健康（GET /health）", status == 200 and health.get("code") == 0, health.get("data", {}).get("status"))

    # ---------- 1. 登录 ----------
    # 首登强制改密（CODE-06）：实例起来后先按**真实用户路径**改密（PUT /system/auth/password），
    # 再等 60s（会话安全字段的短 TTL 缓存窗口）重新登录 —— 本卡验的是 PDF，不是密码策略。
    admin_password = "Admin@12345678"
    token, status, login_body = login("admin", admin_password)
    if not token:
        bootstrap_token, _, _ = login("admin", "Admin@123456")
        if bootstrap_token:
            req("PUT", "/api/system/auth/password",
                {"old_password": "Admin@123456", "new_password": admin_password}, token=bootstrap_token)
            import time as _time
            _time.sleep(70)
            token, status, login_body = login("admin", admin_password)
    check("admin 登录拿到 access_token", status == 200 and len(token) > 50, "len=%d" % len(token) if token else login_body)

    # ---------- 2. 合同 PDF ----------
    path = OUT + "/contract.pdf"
    status, headers, body = req("GET", "/api/business/erp/contract/%s/pdf" % CONTRACT, token=token, raw=True)
    open(path, "wb").write(body)
    check("合同导出 200", status == 200, status)
    check("合同 Content-Type = application/pdf", headers.get("content-type") == "application/pdf", headers.get("content-type"))
    disposition = headers.get("content-disposition", "")
    check(
        "合同文件名（中文走 RFC 5987）",
        "filename*=UTF-8''" in disposition and "%E5%90%88%E5%90%8C" in disposition,
        disposition,
    )
    check("合同 PDF 文件头 %PDF-", body[:5] == b"%PDF-", body[:5])

    text = pdf_text(path)
    flat = flatten(text)
    pages = pdf_pages(path)
    check("合同多页（60 条条款 → 跨页）", pages >= 2, "pages=%d" % pages)

    # ① 签字盖章位 + 附件清单（验收①）
    check("合同含「甲方（盖章）」", "甲方（盖章）" in flat)
    check("合同含「乙方（盖章）」", "乙方（盖章）" in flat)
    check("合同含两个签署日期位", flat.count("日期：") >= 2, flat.count("日期："))
    check("合同含骑缝章提示", "骑缝章" in flat)
    check("合同含附件清单标题", "附件清单（共2份）" in flat, flat[flat.find("附件清单") : flat.find("附件清单") + 30] if "附件清单" in flat else "缺")
    check("附件清单列出第 1 个文件名", "PDF03-合同扫描件-第1页.pdf" in flat)
    check("附件清单列出第 2 个文件名", "PDF03-用印授权书.pdf" in flat)
    check("附件角色标签（合同扫描件 / 补充协议）", "合同扫描件" in flat and "补充协议" in flat)
    # 正文完整渲染（首尾条款都在）
    check("条款正文首条在（第1条）", "第1条" in flat)
    check("条款正文末条在（第60条）", "第60条" in flat)
    check("付款方式从正文识别", "货到验收后30日内电汇付清" in flat)
    check("合同金额小写", "123,456.78" in flat or "123456.78" in flat)
    check("合同金额大写", "壹拾贰万叁仟肆佰伍拾陆元柒角捌分" in flat)
    check("合同期限", "2026-11-01至2027-10-31" in flat)

    # 每页页脚页码
    pages_text = page_texts(path)
    footers = []
    for index, page in enumerate(pages_text, start=1):
        match = re.search(r"第(\d+)页/共(\d+)页", flatten(page))
        footers.append((match.group(1), match.group(2)) if match else None)
    check(
        "合同每页页脚页码正确（第 X 页 / 共 Y 页）",
        pages >= 2
        and len(footers) == pages
        and all(item is not None and item[0] == str(index) and item[1] == str(pages) for index, item in enumerate(footers, start=1)),
        footers,
    )

    # ---------- 3. 收货入库单 PDF ----------
    path = OUT + "/stock-in.pdf"
    status, headers, body = req("GET", "/api/business/erp/stock-in/%s/pdf" % STOCK_IN, token=token, raw=True)
    open(path, "wb").write(body)
    check("入库单导出 200", status == 200, status)
    check("入库单 Content-Type = application/pdf", headers.get("content-type") == "application/pdf", headers.get("content-type"))
    check("入库单 PDF 文件头 %PDF-", body[:5] == b"%PDF-", body[:5])
    text = pdf_text(path)
    flat = flatten(text)
    check("入库单标题「收货入库单」", "收货入库单" in flat)
    check("入库单含供应商", "PDF03测试供应商" in flat)
    check("入库单含仓库", "PDF03一号仓" in flat)
    check("入库单含来源单据号（采购单号）", "PDF03-PO-000001" in flat)
    check("入库单含经办人", "出入库测试员" in flat)
    check("入库单含批次号列", "批次号" in flat)
    check("入库单第一批次号 LOT-20261108-A", "LOT-20261108-A" in flat)
    check("入库单第二批次号 LOT-20261108-B", "LOT-20261108-B" in flat)
    check("入库单含库位 A-01-03", "A-01-03" in flat)
    check("入库单含序列号清单段", "序列号清单" in flat)
    check("入库单序列号 3 个全在", all(("PDF03-SN-00000%d" % i) in flat for i in (1, 2, 3)))
    check("入库单收货签字栏（收货人/质检/仓管/供应商送货人/制单）",
          all(cap in flat for cap in ("收货人：", "质检：", "仓管：", "供应商送货人：", "制单：")))
    check("入库单合计价税合计 422.00", "422.00" in flat or "422" in flat)
    check("入库单大写金额", "肆佰贰拾贰元整" in flat)
    check("入库单页脚页码", re.search(r"第1页/共\d+页", flatten(pdf_text_page(path, 1))) is not None)

    # ---------- 4. 出库单 PDF（50 行 → 跨页） ----------
    path = OUT + "/stock-out.pdf"
    status, headers, body = req("GET", "/api/business/erp/stock-out/%s/pdf" % STOCK_OUT, token=token, raw=True)
    open(path, "wb").write(body)
    check("出库单导出 200", status == 200, status)
    check("出库单 Content-Type = application/pdf", headers.get("content-type") == "application/pdf", headers.get("content-type"))
    text = pdf_text(path)
    flat = flatten(text)
    pages = pdf_pages(path)
    check("出库单跨页（50 行 > 1 页）", pages >= 2, "pages=%d" % pages)
    check("出库单标题「出库单」", "出库单" in flat)
    check("出库单含来源单据号（发货单号）", "PDF03-SH-000001" in flat)
    check("出库单含客户", "PDF03测试客户" in flat)
    check("出库单含出库事由", "销售发货过账自动生成" in flat)
    check("出库单成本结转口径（无税额行）", "税额" not in flat and "价税合计" not in flat)
    check("出库单发货签字栏（发货人/领用人/审核/制单）",
          all(cap in flat for cap in ("发货人：", "领用人／收货人：", "审核：", "制单：")))
    # 每页表头重复
    pages_text = page_texts(path)
    header_hits = [("物料编码" in flatten(page) and "批次号" in flatten(page)) for page in pages_text]
    check("出库单每页都重复表头（物料编码/批次号）", all(header_hits) and len(header_hits) == pages, header_hits)
    # 序号 1..50 连续（跨页不重来）
    seqs = re.findall(r"^\s*(\d{1,3})\s", text, flags=re.MULTILINE)
    nums = [int(value) for value in seqs if 1 <= int(value) <= 50]
    unique = sorted(set(nums))
    check("出库单序号 1..50 连续", unique[:1] == [1] and unique[-1:] == [50] and len(unique) == 50, "unique=%d" % len(unique))
    check("出库单 50 行明细都在（MAT 出现 50 次）", flat.count("PDF03-MAT-0") >= 50, flat.count("PDF03-MAT-0"))
    check("出库单页脚页码（含总页数）", all(re.search(r"第%d页/共%d页" % (i, pages), flatten(p)) for i, p in enumerate(pages_text, 1)))

    # ---------- 5. 字体嵌入 / 不乱码 ----------
    for label, file in (("合同", "contract.pdf"), ("入库单", "stock-in.pdf"), ("出库单", "stock-out.pdf")):
        fonts = subprocess.run(["pdffonts", OUT + "/" + file], capture_output=True, text=True).stdout
        rows = [line for line in fonts.splitlines()[2:] if line.strip()]
        embedded = bool(rows) and all(" yes " in line or "yes yes" in line for line in rows)
        check("%s PDF 字体全部嵌入（emb=yes）" % label, embedded, rows[0] if rows else "no font rows")
        check("%s 中文可无损抽取" % label, "云枢" in flatten(pdf_text(OUT + "/" + file)))

    # ---------- 6. 无权限账号 → 403（三个接口） ----------
    role_code = "PDF03_NO_PERM"
    status, _, role = req("POST", "/api/system/role",
                          {"role_code": role_code, "role_name": "PDF03 无权限", "status": 1}, token=token)
    role_id = ((role.get("data") or {}).get("id")) if status == 200 else None
    if not role_id:
        _, _, page = req("GET", "/api/system/role/page?page=1&page_size=50&keyword=" + role_code, token=token)
        for row in ((page.get("data") or {}).get("list") or []):
            if row.get("role_code") == role_code:
                role_id = row.get("id")
    check("无权限角色就绪（PDF03_NO_PERM）", bool(role_id), role_id or role)

    status, _, user = req("POST", "/api/system/user",
                          {"username": "pdf03_noperm", "password": "NoPerm@123456",
                           "real_name": "PDF03 无权限", "status": 1, "role_ids": [role_id]}, token=token)
    if status != 200:
        check("无权限账号已存在（幂等跳过创建）", "已存在" in json.dumps(user, ensure_ascii=False), user.get("msg"))
    # 新建账号由接口落库时 `must_change_password = false`（system/user/handler.rs），无需改密
    noperm_token, status, noperm_login = login("pdf03_noperm", "NoPerm@123456")
    check("无权限账号可登录（证明不是 401 混成 403）", status == 200 and bool(noperm_token), status)

    for label, url, permission in (
        ("contract", "/api/business/erp/contract/%s/pdf" % CONTRACT, "business:erp:contract:list"),
        ("stock-in", "/api/business/erp/stock-in/%s/pdf" % STOCK_IN, "business:erp:stock-in:list"),
        ("stock-out", "/api/business/erp/stock-out/%s/pdf" % STOCK_OUT, "business:erp:stock-out:list"),
    ):
        status, _, body_json = req("GET", url, token=noperm_token, raw=False)
        msg = (body_json.get("msg") or "") if isinstance(body_json, dict) else ""
        check("无权导出 %s → 403 且点名权限码" % label, status == 403 and permission in msg, "%s %s" % (status, msg))

    # ---------- 7. 404 / 400 ----------
    status, _, _ = req("GET", "/api/business/erp/contract/0f050000-0000-4000-8000-00000000ffff/pdf", token=token, raw=True)
    check("不存在的合同 → 404", status == 404, status)
    status, _, _ = req("GET", "/api/business/erp/stock-in/not-a-uuid/pdf", token=token, raw=True)
    check("非法 id → 400", status == 400, status)

    # ---------- 汇总 ----------
    failed = [item for item in results if not item[1]]
    print("\n==== 汇总：%d 项，失败 %d 项 ====" % (len(results), len(failed)))
    json.dump(
        [{"name": name, "ok": ok, "detail": str(detail)} for name, ok, detail in results],
        open(OUT + "/result.json", "w"),
        ensure_ascii=False,
        indent=2,
    )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
