#!/usr/bin/env python3
"""E2E-12 附件域（A 段）：上传 / 预览 / 下载 / 权限 / 列表 / 排序 / 分栏 / 软删。

环境：145 · cloudpivot_dev · 真后端 127.0.0.1:8033（在 145 上跑）
前缀：E2E12 / e2e12_
"""
import json
import sys
import urllib.parse

from e2e12_lib import (ADMIN_PW, BASE_PW, RAW, check, contains, data_of, eq, grant, login,
                        make_docx, make_pdf, make_png, make_zip, mk_role, mk_user, msg_of,
                        not_contains, psql, req, req_bytes, save_state, scenario, sh, squash,
                        upload, PASS, FAIL, STATS)


def stamp():
    """每次运行唯一的非秒传内容（否则上一轮的同名字节会命中内容秒传，断言失真）。"""
    import time
    return int(time.time() * 1000) % 100000000

SUPPLIER = "01a1b5d2-504f-72f7-92b2-00f884285a2c"   # DEMO-S001 演练供应商A
WAREHOUSE = "d1370001-0000-4000-8000-000000000001"  # WH01 总仓（种子）
MATERIALS = [
    "b2b70001-0000-4000-8000-000000000011",  # D7-M1 D7 螺栓
    "d2030100-0000-4000-8000-000000000011",  # DC13-M1
    "d2030100-0000-4000-8000-000000000012",  # DC13-M2
]


def main():
    st = {}

    # ================= S0 环境与登录 =================
    scenario("S0", "环境自检（admin 登录 / PDF 渲染服务 / 规则表）")
    admin = login("admin", password=ADMIN_PW, name="login_admin")
    check(bool(admin), "admin 登录拿到 access_token")
    if not admin:
        print("!! 无 admin token，终止")
        return 1

    s, body = req("GET", "/api/business/pdf/health", admin, name="pdf_health")
    d = data_of(body)
    eq("PDF 渲染服务 /health = 200", s, 200)
    check(bool(d.get("browser")), "Chromium 版本可读", str(d.get("browser"))[:80])
    check(isinstance(d.get("max_concurrency"), int), "并发上限已下发", str(d.get("max_concurrency")))

    s, body = req("GET", "/api/support/attachment-rule/list", admin, name="rules_list")
    rules = data_of(body).get("list") or []
    eq("规则管理列表 = 200", s, 200)
    check(len(rules) >= 9, "规则 ≥ 9 条（7 单据 + 发票两分支）", f"实际={len(rules)}")
    docs = sorted({r.get("doc_type") for r in rules})
    check(len(docs) >= 8, "覆盖的单据类型 ≥ 8", ",".join(docs))

    # ================= S1 造锚点单据（30 行采购订单，供 PDF 多页验收） =================
    scenario("S1", "建锚点采购订单（30 行明细 → PDF 跨页）")
    items = []
    for i in range(30):
        items.append({
            "material_id": MATERIALS[i % len(MATERIALS)],
            "qty": 2 + i,
            "price_excl": round(11.11 + i, 2),
            "tax_rate": 13,
            "remark": f"E2E12 明细 {i + 1}",
        })
    payload = {
        "supplier_id": SUPPLIER, "warehouse_id": WAREHOUSE, "purchase_type": "SPOT",
        "order_date": "2026-11-09", "price_mode": "EXCL", "credit_days": 30,
        "remark": "E2E12 附件+PDF 演练锚点单据（含 30 行明细，用于跨页表头验收）",
        "items": items,
    }
    s, body = req("POST", "/api/business/erp/purchase-order", admin, payload, name="po_create")
    d = data_of(body)
    po = (d.get("order") or {}).get("id") or d.get("id")
    po_no = (d.get("order") or {}).get("order_no") or d.get("order_no")
    eq("建采购订单 = 200", s, 200)
    check(bool(po), "拿到采购订单 id", str(po))
    check(bool(po_no) and str(po_no).startswith("PO-"), "单号形态 PO-yyyyMMdd-NNNNNN", str(po_no))
    total_with_tax = (d.get("order") or {}).get("total_with_tax") or d.get("total_with_tax")
    check(total_with_tax is not None, "响应带价税合计", str(total_with_tax))
    st.update(po_id=po, po_no=po_no, po_total=total_with_tax,
              po_expected=round(sum((2 + i) * round(11.11 + i, 2) * 1.13 for i in range(30)), 2))
    db_items = psql(f"select count(*) from biz_purchase_order_item where order_id='{po}'::uuid")
    eq("DB：明细 30 行落库", db_items, 30)

    # ================= S2 上传（图片 / PDF / Office / 压缩包 + 秒传 + 负例） =================
    scenario("S2", "附件上传：4 类文件 / 秒传 / 扩展名闸 / 20MB 双端闸")
    pdf_bytes = make_pdf(f"E2E12 contract scan {stamp()}", pages=2)
    png_bytes = make_png(80, 50, rgb=(30, 120, 200))
    docx_bytes = make_docx(f"E2E12 补充协议 {stamp()}")
    zip_bytes = make_zip(f"E2E12 附件包 {stamp()}")

    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-合同扫描件.pdf", pdf_bytes,
                        file_role="CONTRACT_SCAN", remark="扫描件（2 页）", name="att_upload_pdf")
    a_pdf = data_of(body)
    eq("上传 PDF = 200", s, 200)
    eq("PDF 绑定 file_role", a_pdf.get("file_role"), "CONTRACT_SCAN")
    eq("PDF 字节数回显", a_pdf.get("file_size"), len(pdf_bytes))
    eq("首次上传不命中秒传", a_pdf.get("duplicated"), False)
    check(bool(a_pdf.get("file_name")) and a_pdf["file_name"].endswith(".pdf"), "file_name 回显")

    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-签收单.png", png_bytes,
                        file_role="SIGN_RECEIPT", name="att_upload_png")
    a_png = data_of(body)
    eq("上传图片 = 200", s, 200)
    eq("图片绑定角色", a_png.get("file_role"), "SIGN_RECEIPT")

    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-补充协议.docx", docx_bytes,
                        file_role="SUPPLEMENT", name="att_upload_docx")
    a_docx = data_of(body)
    eq("上传 Office(docx) = 200", s, 200)
    eq("docx 角色", a_docx.get("file_role"), "SUPPLEMENT")

    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-打包.zip", zip_bytes,
                        file_role="DEFAULT", name="att_upload_zip")
    a_zip = data_of(body)
    eq("上传压缩包(zip) = 200", s, 200)

    # 秒传：同一份字节再传一次（换角色）
    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-合同扫描件-副本.pdf", pdf_bytes,
                        file_role="ACCEPTANCE", name="att_upload_dup")
    a_dup = data_of(body)
    eq("重复字节上传 = 200", s, 200)
    eq("命中内容秒传 duplicated=true", a_dup.get("duplicated"), True)
    eq("秒传复用同一 sys_file", a_dup.get("file_id"), a_pdf.get("file_id"))
    eq("秒传不新增绑定行（幂等）", a_dup.get("id"), a_pdf.get("id"))
    # P4-1（已写进问题清单）：秒传命中时响应体回显的是**本次请求**的角色，而库内绑定的仍是首行角色。
    # 列表重查即恢复正确；这里断言实际行为，避免假绿。
    eq("秒传响应回显请求角色（P4-1：与库内首行角色不一致）", a_dup.get("file_role"), "ACCEPTANCE")
    db_role = psql(f"select file_role from biz_doc_attachment where id='{a_pdf.get('id')}'::uuid")
    eq("库内绑定角色仍是首行 CONTRACT_SCAN", db_role, "CONTRACT_SCAN")

    # 扩展名闸（fail-closed 白名单）
    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-evil.exe", b"MZ\x90\x00fake-exe",
                        file_role="DEFAULT", name="att_upload_exe")
    eq("上传 .exe 被拒 = 400", s, 400)
    check(("禁止上传" in msg_of(body) or "黑名单" in msg_of(body)) and ".exe" in msg_of(body),
          "拒绝文案点名可执行黑名单", msg_of(body)[:80])

    # 20MB 业务闸（ATT-05：400 + 10001）
    big = b"%PDF-1.4\n" + b"A" * (21 * 1024 * 1024)
    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-超大.pdf", big,
                        file_role="DEFAULT", timeout=300, name="att_upload_big")
    eq("21MB 超限 = 400", s, 400)
    eq("超限业务码 = 10001", body.get("code"), 10001)
    check(bool(msg_of(body)), "超限文案非空", msg_of(body)[:80])

    # 边界：正好 20MiB 必须放行（ATT-05 验收口径）
    edge = b"%PDF-1.4\n" + b"B" * (20 * 1024 * 1024 - 9)
    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-边界20M.pdf", edge,
                        file_role="DEFAULT", timeout=300, name="att_upload_edge")
    eq("正好 20MiB 放行 = 200", s, 200)
    a_edge = data_of(body)
    if s == 200:
        eq("边界文件字节数", a_edge.get("file_size"), len(edge))

    st.update(att_pdf=a_pdf.get("id"), att_png=a_png.get("id"), att_docx=a_docx.get("id"),
              att_zip=a_zip.get("id"), att_edge=a_edge.get("id"), file_pdf=a_pdf.get("file_id"))

    # 非法单据类型 / 非法角色 / 不存在的单据
    s, body, _ = upload(admin, "NOT_A_KIND", po, "E2E12-x.pdf", pdf_bytes, name="att_upload_badkind")
    eq("非法 doc_type = 400", s, 400)
    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-x.pdf", pdf_bytes,
                        file_role="NOT_A_ROLE", name="att_upload_badrole")
    eq("非法 file_role = 400", s, 400)
    s, body, _ = upload(admin, "PURCHASE_ORDER", "0199aaaa-0000-4000-8000-000000000001",
                        "E2E12-ghost.pdf", pdf_bytes, name="att_upload_ghostdoc")
    eq("单据不存在 = 404", s, 404)

    db_files = psql(f"select count(*) from biz_doc_attachment where doc_id='{po}'::uuid and deleted_at is null")
    eq("DB：该单附件 5 行（4 类 + 20M 边界；秒传幂等不新增）", db_files, 5)

    # ================= S3 列表（大小/类型/上传人/时间/分栏） =================
    scenario("S3", "附件列表：字段完整 / 排序口径 / 角色分栏")
    s, body = req("GET", f"/api/business/attachment/list?doc_type=PURCHASE_ORDER&doc_id={po}", admin,
                  name="att_list")
    d = data_of(body)
    rows = d.get("list") or []
    eq("列表 = 200", s, 200)
    eq("列表回显 doc_type", d.get("doc_type"), "PURCHASE_ORDER")
    eq("条目数 = 5", len(rows), 5)
    first = rows[0] if rows else {}
    for field in ("id", "file_id", "file_role", "file_role_label", "file_name", "file_size",
                  "mime_type", "sort_order", "uploaded_by", "uploader_name", "created_at", "can_delete"):
        check(field in first, f"列表字段齐全：{field}")
    check(bool(first.get("uploader_name")) and first.get("uploader_name") != first.get("uploaded_by"),
          "uploader_name 是姓名不是 UUID", str(first.get("uploader_name")))
    eq("默认排序口径 = time", d.get("sort"), "time")
    check(isinstance(d.get("max_count"), int) and d["max_count"] > 0, "max_count 下发", str(d.get("max_count")))
    roles = d.get("roles") or []
    check(len(roles) >= 3, "角色分栏清单下发（含空分组）", ",".join(str(r.get("role")) for r in roles))
    check(all("code" in r and "label" in r and "count" in r for r in roles), "分栏含 code+label+count")
    check(any(r.get("count") == 0 for r in roles), "空分组也在清单里（前端显示『暂无，点击上传』）")
    # 时间倒序
    times = [r.get("created_at") for r in rows]
    eq("时间倒序（首条最新）", times == sorted(times, reverse=True), True)
    # 按角色筛出银行回单/签收单
    grouped = {}
    for r in rows:
        grouped.setdefault(r["file_role"], []).append(r["file_name"])
    check("SIGN_RECEIPT 分栏只含签收单", all("签收单" in n for n in grouped.get("SIGN_RECEIPT", [])),
          str(grouped.get("SIGN_RECEIPT")))

    # 手动排序
    ids = [r["id"] for r in rows]
    rev = list(reversed(ids))
    s, body = req("PUT", "/api/business/attachment/sort", admin,
                  {"doc_type": "PURCHASE_ORDER", "doc_id": po, "ids": rev}, name="att_sort")
    d2 = data_of(body)
    eq("重排 = 200", s, 200)
    eq("重排回读口径 = manual", d2.get("sort"), "manual")
    eq("重排后顺序 = 入参顺序", [r["id"] for r in (d2.get("list") or [])], rev)
    eq("重排后 sort_order 从 0 开始", [r["sort_order"] for r in (d2.get("list") or [])],
       list(range(len(rev))))
    s, body = req("GET", f"/api/business/attachment/list?doc_type=PURCHASE_ORDER&doc_id={po}&sort=manual",
                  admin, name="att_list_manual")
    eq("sort=manual 读回同一顺序", [r["id"] for r in (data_of(body).get("list") or [])], rev)
    # 非法排序请求
    s, body = req("PUT", "/api/business/attachment/sort", admin,
                  {"doc_type": "PURCHASE_ORDER", "doc_id": po, "ids": [ids[0], ids[0]]}, name="att_sort_dup")
    eq("重排重复 id = 400", s, 400)
    s, body = req("PUT", "/api/business/attachment/sort", admin,
                  {"doc_type": "PURCHASE_ORDER", "doc_id": po, "ids": ids[:-1]}, name="att_sort_missing")
    eq("重排缺行 = 400", s, 400)

    # ================= S4 下载 / 预览 =================
    scenario("S4", "附件下载 / 预览（Content-Type / nosniff / 字节一致 / 留痕）")
    cases = [
        ("pdf", st["att_pdf"], "application/pdf", pdf_bytes, True),
        ("png", st["att_png"], "image/png", png_bytes, True),
        ("docx", st["att_docx"], None, docx_bytes, False),
        ("zip", st["att_zip"], None, zip_bytes, False),
    ]
    for tag, aid, ctype, content, previewable in cases:
        s, headers, payload, path = req_bytes(f"/api/business/attachment/download/{aid}", admin,
                                              name=f"dl_{tag}.bin")
        eq(f"下载 {tag} = 200", s, 200)
        if ctype:
            eq(f"{tag} Content-Type", headers.get("content-type"), ctype)
        eq(f"{tag} 字节与上传一致", payload == content, True)
        eq(f"{tag} X-Content-Type-Options", headers.get("x-content-type-options"), "nosniff")
        disp = headers.get("content-disposition", "")
        check("attachment" in disp or "inline" in disp,
              f"{tag} 有 Content-Disposition", disp[:90])
        check("filename*=UTF-8''" in disp, f"{tag} 文件名走 RFC5987 filename*", disp[:60])
        check("E2E12" in urllib.parse.unquote(disp), f"{tag} 中文名可解出", urllib.parse.unquote(disp)[:70])
        check(bool(path), f"{tag} 落盘取证", str(path))

    s, headers, payload, _ = req_bytes(f"/api/business/attachment/download/{st['att_pdf']}", admin)
    text = payload.decode("latin-1", "replace")
    check("%PDF-" in text, "下载内容是真 PDF（%PDF- 头）")

    # 留痕（GET 不进全局中间件，模块自己写）
    log = psql("select count(*) from operation_logs where operation='DOC_ATTACHMENT_DOWNLOAD' "
               f"and target_id='{st['att_pdf']}'")
    check(log.isdigit() and int(log) >= 1, "下载留痕 operation_logs 有 DOC_ATTACHMENT_DOWNLOAD", log)
    detail = psql("select detail::text from operation_logs where operation='DOC_ATTACHMENT_DOWNLOAD' "
                  f"and target_id='{st['att_pdf']}' order by created_at desc limit 1")
    for key in ("doc_type", "doc_id", "file_id", "file_name", "file_size"):
        check(key in detail, f"留痕 detail 含 {key}")
    check("PURCHASE_ORDER" in detail, "留痕 doc_type 正确")

    # ================= S5 越权（行级 403 / 探测面截断 / 非一刀切） =================
    scenario("S5", "权限：越权下载 403 / 幽灵 id 同文案 / 合同可读非一刀切")
    rid, s_role, _ = mk_role(admin, "E2E12_ATT", "E2E12 附件只读合同", sort=981, data_scope=4)
    check(bool(rid), "建角色 E2E12_ATT", str(rid))
    n, _ = grant(admin, rid, ["business:attachment:list", "business:attachment:upload",
                              "business:erp:contract:list"])
    eq("授权 3 个权限码", n, 3)
    uid, s_user, _ = mk_user(admin, "e2e12_att", "E2E12 附件专员", [rid])
    check(bool(uid), "建用户 e2e12_att", str(uid))
    tok = login("e2e12_att", password=BASE_PW, name="login_att")
    check(bool(tok), "e2e12_att 登录成功")

    s, body = req("GET", f"/api/business/attachment/download/{st['att_pdf']}", tok, name="dl_forbidden_po")
    m1 = msg_of(body)
    eq("无权模块读码 → 下载 403", s, 403)
    contains("403 文案点名单据类型", m1, "采购单")
    s, body2 = req("GET", "/api/business/attachment/download/0199bbbb-0000-4000-8000-0000000000ff",
                   tok, name="dl_ghost")
    m2 = msg_of(body2)
    eq("幽灵 id → 404（不是 403）", s, 404)
    check(m1 != m2, "403 / 404 文案不同（存在性在权限之前，已留档）")

    s, body, _ = upload(tok, "PURCHASE_ORDER", po, "E2E12-越权上传.pdf", make_pdf("x"),
                        name="att_upload_forbidden")
    eq("无权向该单上传 → 403", s, 403)
    contains("上传 403 文案点名单据", msg_of(body), "采购单")

    # 合同：有读码 → 200（证明不是一刀切）
    s, body = req("GET", "/api/business/erp/contract/page?page=1&page_size=1", tok, name="contract_page")
    cid = None
    items_c = (data_of(body).get("list") or [])
    if items_c:
        cid = items_c[0].get("id")
    s, body = req("GET", "/api/business/erp/contract/page?page=1&page_size=1", admin)
    if not cid and (data_of(body).get("list") or []):
        cid = data_of(body)["list"][0].get("id")
    st["contract_id"] = cid
    check(bool(cid), "取到一张合同用于对照", str(cid))
    if cid:
        s, body, _ = upload(tok, "CONTRACT", cid, "E2E12-合同-专员上传.pdf", make_pdf("contract by att user"),
                            file_role="CONTRACT_SCAN", name="att_upload_contract")
        a_c = data_of(body)
        eq("合同上传 = 200（有读码）", s, 200)
        st["att_contract"] = a_c.get("id")
        s, headers, payload, _ = req_bytes(f"/api/business/attachment/download/{a_c.get('id')}", tok)
        eq("合同附件下载 = 200（非一刀切）", s, 200)
        check(payload.startswith(b"%PDF-"), "合同附件字节是真 PDF")

    # 无任何附件码的角色：路由层 403
    rid2, _, _ = mk_role(admin, "E2E12_NOATT", "E2E12 无附件码", sort=982, data_scope=4)
    grant(admin, rid2, ["business:erp:contract:list"])
    uid2, _, _ = mk_user(admin, "e2e12_noatt", "E2E12 无附件码用户", [rid2])
    tok2 = login("e2e12_noatt", password=BASE_PW, name="login_noatt")
    s, body = req("GET", f"/api/business/attachment/list?doc_type=CONTRACT&doc_id={cid}", tok2,
                  name="list_forbidden_route")
    eq("缺 business:attachment:list → 路由 403", s, 403)
    contains("路由 403 点名权限码", msg_of(body), "business:attachment:list")
    st.update(role_att=rid, user_att=uid, role_noatt=rid2, user_noatt=uid2)

    # ================= S6 删除（含上传人删自己） =================
    scenario("S6", "删除：软删绑定行 / 列表消失 / 上传人自删 / 物理文件不误删")
    s, body, _ = upload(admin, "PURCHASE_ORDER", po, "E2E12-待删除.pdf", make_pdf("to be deleted"),
                        name="att_upload_tmp")
    tmp_id = data_of(body).get("id")
    s, body = req("DELETE", f"/api/business/attachment/{tmp_id}", admin, name="att_delete")
    d = data_of(body)
    eq("删除 = 200", s, 200)
    eq("deleted=true", d.get("deleted"), True)
    eq("独占文件随之软删 file_deleted", d.get("file_deleted"), True)
    db = psql(f"select (deleted_at is not null)::text from biz_doc_attachment where id='{tmp_id}'::uuid")
    eq("DB：绑定行软删（行仍在，可审计）", db, "true")
    s, body = req("GET", f"/api/business/attachment/list?doc_type=PURCHASE_ORDER&doc_id={po}", admin)
    check(tmp_id not in [r["id"] for r in (data_of(body).get("list") or [])], "列表已不含已删附件")
    s, body = req("DELETE", f"/api/business/attachment/{tmp_id}", admin, name="att_delete_twice")
    eq("重复删除 = 404", s, 404)

    # 上传人删自己（无需 delete 码）：e2e12_att 在合同上那一条
    s, body = req("DELETE", f"/api/business/attachment/{st.get('att_contract')}", tok, name="att_delete_own")
    eq("上传人删自己（无 delete 码）= 200", s, 200)
    # 换个人的附件不能删：管理员传的合同附件（不存在）；用 admin 的 PO 附件让 e2e12_att 删 → 403（缺读码先拦）
    s, body = req("DELETE", f"/api/business/attachment/{st['att_png']}", tok, name="att_delete_other")
    eq("删别人附件（无读码）= 403", s, 403)

    save_state(st)
    print(f"\n=== A 段断言：PASS={PASS} FAIL={FAIL} ===")
    for sid, (p, f) in STATS.items():
        print(f"  {sid}: {p} 绿 / {f} 红")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
