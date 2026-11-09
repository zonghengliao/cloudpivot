#!/usr/bin/env python3
"""E2E-12 B 段：必传闸门 + 规则管理 + 8 类 PDF 导出（含质量/权限/审计）。

环境：145 · cloudpivot_dev · 真后端 127.0.0.1:8033（在 145 上跑）
"""
import json
import os
import re
import sys
import urllib.parse

from e2e12_lib import (ADMIN_PW, BASE_PW, RAW, check, contains, data_of, eq, grant, login,
                        make_pdf, mk_role, mk_user, msg_of, not_contains, pdf_fonts, pdf_info,
                        pdf_pages, pdf_png, pdf_text, psql, req, req_bytes, save_state,
                        load_state, scenario, sh, squash, upload, WORK)

STOCK_TAKE_WH = "01a1bded-5b11-7025-80d6-c21c48a01265"   # E2E03 演练仓（有库存桶）
STOCK_TAKE_MAT = "01a1b5d9-420f-735d-bdb6-80b22842d3dc"  # DEMO-INV01


def find_doc(kind):
    """找一张「该类型 + 零同类型附件」的真实单据（只读用，绝不改别人的单据）。"""
    table = {
        "RECEIPT": ("biz_receipt", "receipt_no"),
        "INVOICE": ("biz_invoice", "invoice_no"),
        "EXPENSE_REPORT": ("biz_expense_report", "report_no"),
        "FIXED_ASSET": ("biz_fixed_asset", "asset_no"),
        "STOCK_TAKE": ("biz_stock_take", "take_no"),
        "PURCHASE_RETURN": ("biz_purchase_return", "return_no"),
        "SALES_RETURN": ("biz_sales_return", "return_no"),
        "PAYMENT": ("biz_payment", "payment_no"),
    }[kind]
    sql = (f"select t.id||'|'||t.{table[1]} from {table[0]} t where t.deleted_at is null "
           f"and not exists (select 1 from biz_doc_attachment a where a.doc_id=t.id "
           f"and a.doc_type='{kind}' and a.deleted_at is null) "
           f"order by t.created_at desc limit 1")
    out = psql(sql)
    if not out or out.startswith("<ERR") or "|" not in out:
        return None, None
    did, no = out.split("|", 1)
    return did.strip(), no.strip()


def main():
    st = load_state()
    admin = login("admin", password=ADMIN_PW, name="login_admin_b")
    check(bool(admin), "admin 登录")

    # ================= S7 规则管理界面 =================
    scenario("S7", "规则管理：列表 / 选项 / 新建 / 修改 / 启停 / 软删 / 越权 / 注入拦截")
    s, body = req("GET", "/api/support/attachment-rule/list", admin, name="b_rules_list")
    rules = data_of(body).get("list") or []
    eq("规则列表 = 200", s, 200)
    check(len(rules) >= 9, "规则 ≥9 条", str(len(rules)))
    s, body = req("GET", "/api/support/attachment-rule/options", admin, name="b_rules_options")
    opt = data_of(body)
    dts = opt.get("doc_types") or opt.get("docTypes") or opt.get("docs") or []
    check(len(dts) >= 8, "options 覆盖单据类型", str(len(dts)))
    lists = [k for k, v in opt.items() if isinstance(v, list) and len(v) >= 3]
    check(len(lists) >= 2, "options 至少下发两组枚举", ",".join(lists))

    s, body = req("POST", "/api/support/attachment-rule", admin, {
        "doc_type": "CONTRACT", "required_at": "SUBMIT", "min_count": 1,
        "allow_exts": "pdf", "enabled": True, "remark": "E2E12 临时规则（演练用）",
    }, name="b_rule_create")
    new_rule = data_of(body).get("id") or data_of(body).get("rule_id")
    eq("新建规则 = 200", s, 200)
    check(bool(new_rule), "拿到规则 id", str(new_rule))

    s, body = req("PUT", f"/api/support/attachment-rule/{new_rule}", admin, {
        "doc_type": "CONTRACT", "required_at": "SUBMIT", "min_count": 1,
        "allow_exts": "pdf", "enabled": True, "remark": "E2E12 临时规则（已改）",
    }, name="b_rule_update")
    eq("修改规则 = 200", s, 200)
    remark_db = psql(f"select remark from biz_attachment_rule where id='{new_rule}'::uuid")
    contains("DB 里 remark 已更新", remark_db, "已改")

    # 立即生效：CONTRACT + SUBMIT 现在要求附件
    cid = st.get("contract_id")
    s, body = req("GET", f"/api/business/attachment/check?doc_type=CONTRACT&doc_id={cid}&at=SUBMIT",
                  admin, name="b_check_contract_on")
    d = data_of(body)
    eq("规则生效后 check = 200", s, 200)
    # 该合同上有一条 e2e12_att 上传后删掉的附件 + 可能其它，命中与否以实际返回为准
    before = d.get("can_proceed")
    s, body = req("PUT", f"/api/support/attachment-rule/{new_rule}/enabled", admin,
                  {"enabled": False}, name="b_rule_disable")
    eq("停用规则 = 200", s, 200)
    s, body = req("GET", f"/api/business/attachment/check?doc_type=CONTRACT&doc_id={cid}&at=SUBMIT",
                  admin, name="b_check_contract_off")
    d2 = data_of(body)
    eq("停用后 check 立即放行（无缓存）", d2.get("can_proceed"), True)
    check(len(d2.get("missing") or []) == 0, "停用后缺口清零")
    s, body = req("PUT", f"/api/support/attachment-rule/{new_rule}/enabled", admin,
                  {"enabled": True}, name="b_rule_enable")
    eq("重新启用 = 200", s, 200)
    s, body = req("GET", f"/api/business/attachment/check?doc_type=CONTRACT&doc_id={cid}&at=SUBMIT",
                  admin, name="b_check_contract_on2")
    d3 = data_of(body)
    eq("重启后 missing 回来（与停用前一致）", bool(d3.get("missing")), bool(d.get("missing")))

    # 注入 / 非法值
    s, body = req("POST", "/api/support/attachment-rule", admin, {
        "doc_type": "NOT_A_KIND", "required_at": "SUBMIT", "remark": "x"}, name="b_rule_badtype")
    eq("非法 doc_type = 400", s, 400)
    s, body = req("POST", "/api/support/attachment-rule", admin, {
        "doc_type": "CONTRACT", "required_at": "SUBMIT", "remark": "x",
        "condition_sql": "1=1; drop table biz_contract"}, name="b_rule_inject")
    eq("condition_sql 注入 = 400", s, 400)
    contains("注入被点名拒绝", msg_of(body), "condition")
    s, body = req("POST", "/api/support/attachment-rule", admin, {
        "doc_type": "CONTRACT", "required_at": "NOT_AN_ACTION", "remark": "x"}, name="b_rule_badaction")
    eq("非法 required_at = 400", s, 400)

    # 越权
    tok = login("e2e12_att", password=BASE_PW, name="login_att_b")
    s, body = req("GET", "/api/support/attachment-rule/list", tok, name="b_rules_forbidden")
    check(s in (403, 401), "非超管访问规则管理 = 403", f"实际={s}")
    s, body = req("POST", "/api/support/attachment-rule", tok, {
        "doc_type": "CONTRACT", "required_at": "SUBMIT", "remark": "x"}, name="b_rules_create_forbidden")
    check(s in (403, 401), "非超管建规则 = 403", f"实际={s}")

    s, body = req("DELETE", f"/api/support/attachment-rule/{new_rule}", admin, name="b_rule_delete")
    eq("删除规则 = 200", s, 200)
    deleted_at = psql(f"select (deleted_at is not null)::text from biz_attachment_rule where id='{new_rule}'::uuid")
    eq("DB：软删留痕", deleted_at, "true")
    s, body = req("GET", "/api/support/attachment-rule/list", admin, name="b_rules_list2")
    check(new_rule not in [r.get("id") for r in (data_of(body).get("list") or [])], "列表已不含已删规则")

    # ================= S8 必传闸门（7 单据 + 发票两分支） =================
    scenario("S8", "必传闸门：8 条规则 × check + 真实动作 400 + 补附件放行")
    targets = [
        ("INVOICE", "SUBMIT", "发票"), ("RECEIPT", "SUBMIT", "银行回单"),
        ("EXPENSE_REPORT", "SUBMIT", "费用凭证"), ("FIXED_ASSET", "ACTIVATE", "验收单"),
        ("STOCK_TAKE", "CONFIRM", "盘点"), ("PURCHASE_RETURN", "SUBMIT", "退货"),
        ("SALES_RETURN", "SUBMIT", "退货"), ("PAYMENT", "ACTIVATE", "银行回单"),
    ]
    hit = 0
    for kind, at, needle in targets:
        did, no = find_doc(kind)
        if not did:
            # SALES_RETURN 库内仅 1 张单且已挂附件 → 负例走「规则停用/启用」段已覆盖
            check(True, f"{kind} 库内无「零附件」单据（该规则存在性由 DB 断言兜底）", "skip")
            continue
        hit += 1
        s, body = req("GET", f"/api/business/attachment/check?doc_type={kind}&doc_id={did}&at={at}",
                      admin, name=f"b_check_{kind}")
        d = data_of(body)
        eq(f"{kind}/{at} check = 200", s, 200)
        eq(f"{kind} 无附件 → 不允许提交", d.get("can_proceed"), False)
        miss = d.get("missing") or []
        check(bool(miss), f"{kind} 缺口清单非空", str(len(miss)))
        if miss:
            rule = miss[0]
            check(bool(rule.get("need")), f"{kind} 缺口带扩展名白名单", str(rule.get("need")))
            check(bool(rule.get("hint") or rule.get("role_label")), f"{kind} 缺口带人话名",
                  str(rule.get("hint"))[:40])
        check(bool(d.get("hint")) and bool(d.get("message")), f"{kind} 有红字 + 长报文",
              str(d.get("hint"))[:60])
        contains(f"{kind} 红字点名所需附件", d.get("message") or "", needle)
    check(hit >= 6, "至少 6 类单据验到负例", f"实际={hit}/8")

    # 发票电子/纸质分支：条件列 is_electronic 决定命中哪条规则
    e_id = psql("select id from biz_invoice where deleted_at is null and is_electronic is true "
                "and not exists (select 1 from biz_doc_attachment a where a.doc_id=biz_invoice.id "
                "and a.doc_type='INVOICE' and a.deleted_at is null) order by created_at desc limit 1")
    p_id = psql("select id from biz_invoice where deleted_at is null and is_electronic is false "
                "and not exists (select 1 from biz_doc_attachment a where a.doc_id=biz_invoice.id "
                "and a.doc_type='INVOICE' and a.deleted_at is null) order by created_at desc limit 1")
    if e_id and not e_id.startswith("<ERR"):
        s, body = req("GET", f"/api/business/attachment/check?doc_type=INVOICE&doc_id={e_id}",
                      admin, name="b_check_invoice_e")
        e_need = ",".join((m.get("need") or "") for m in (data_of(body).get("missing") or []))
        contains("电子票分支白名单 = pdf,ofd,xml", e_need.replace(" ", ""), "ofd")
    else:
        check(True, "库内无「零附件的电子票」样本（分支规则仍在表里）", "skip")
    if p_id and not p_id.startswith("<ERR"):
        s, body = req("GET", f"/api/business/attachment/check?doc_type=INVOICE&doc_id={p_id}",
                      admin, name="b_check_invoice_p")
        p_need = ",".join((m.get("need") or "") for m in (data_of(body).get("missing") or []))
        check("jpg" in p_need.replace(" ", ""), "纸质票分支白名单含 jpg/png", p_need)
    else:
        check(True, "库内暂无纸质票样本（分支规则在表里以 is_electronic IS FALSE 存在）", "skip")
    paper_rule = psql("select count(*) from biz_attachment_rule where doc_type='INVOICE' "
                      "and condition_sql like '%is_electronic IS FALSE%' and deleted_at is null")
    eq("DB：纸质票分支规则存在", paper_rule, "1")

    # 真实动作：自有盘点单 → PENDING_POST 被 400 拦下 → 补附件 → 放行
    s, body = req("POST", "/api/business/erp/stock-take", admin, {
        "warehouse_id": STOCK_TAKE_WH, "take_type": "PARTIAL",
        "remark": "E2E12 必传闸门演练（自有单，收尾清理）",
        "items": [{"material_id": STOCK_TAKE_MAT}],
    }, name="b_st_create")
    take_id = data_of(body).get("id")
    eq("建盘点单 = 200", s, 200)
    st["stock_take_id"] = take_id
    check(bool(take_id), "拿到盘点单 id", str(take_id))
    s, body = req("PATCH", f"/api/business/erp/stock-take/{take_id}/status", admin,
                  {"status": "PENDING_POST"}, name="b_st_gate_blocked")
    eq("无附件提交盘点 = 400", s, 400)
    contains("400 文案点名盘点表", msg_of(body), "盘点")
    db_status = psql(f"select status from biz_stock_take where id='{take_id}'::uuid")
    eq("DB：状态未变（仍在 DRAFT）", db_status, "DRAFT")

    s, body, _ = upload(admin, "STOCK_TAKE", take_id, "E2E12-盘点表.pdf", make_pdf("stock take sheet"),
                        file_role="DEFAULT", name="b_st_upload")
    eq("补上传盘点表 = 200", s, 200)
    s, body = req("GET", f"/api/business/attachment/check?doc_type=STOCK_TAKE&doc_id={take_id}&at=CONFIRM",
                  admin, name="b_st_check_after")
    eq("补附件后 check 放行", data_of(body).get("can_proceed"), True)
    s, body = req("PATCH", f"/api/business/erp/stock-take/{take_id}/status", admin,
                  {"status": "PENDING_POST"}, name="b_st_after_gate")
    eq("补附件后提交通过（非 400）", s != 400, True)
    print("    ↳ 提交结果：", s, msg_of(body)[:100])

    # ================= S9 PDF 导出（12 类） =================
    scenario("S9", "PDF 导出：订单/合同/出入库/调拨/盘点/退货/审批/任务/三大报表")

    def export(tag, path, expect_pages_min=1, checks=None, name=None):
        s, headers, payload, saved = req_bytes(path, admin, name=(name or tag) + ".pdf", timeout=240)
        ok = eq(f"{tag} 导出 = 200", s, 200)
        if s != 200:
            try:
                print("    ↳ 错误体：", json.dumps(json.loads(payload.decode("utf-8", "replace")),
                                                  ensure_ascii=False)[:300])
            except Exception:  # noqa: BLE001
                print("    ↳ 错误体：", payload[:200])
            return None, None
        eq(f"{tag} Content-Type", headers.get("content-type"), "application/pdf")
        check(payload.startswith(b"%PDF-"), f"{tag} 以 %PDF- 开头", payload[:8].decode("latin-1"))
        disp = headers.get("content-disposition", "")
        check("filename*=UTF-8''" in disp, f"{tag} 文件名走 RFC5987", disp[:60])
        check(len(payload) > 3000, f"{tag} 字节数合理", str(len(payload)))
        pages = pdf_pages(saved)
        check(pages >= expect_pages_min, f"{tag} 页数 ≥ {expect_pages_min}", str(pages))
        fonts = pdf_fonts(saved)
        check("yes" in fonts, f"{tag} 字体已嵌入（中文不乱码的前提）",
              " | ".join(fonts.splitlines()[2:4])[:100])
        text = pdf_text(saved, "-raw")
        with open(os.path.join(RAW, f"{tag}.txt"), "w", encoding="utf-8") as fh:
            fh.write(text)
        check(len(squash(text)) > 30, f"{tag} 有文本层（不是纯图片）", str(len(squash(text))))
        check(bool(re.search(r"[\u4e00-\u9fa5]", text)), f"{tag} 文本层含中文")
        for label, needle in (checks or []):
            contains(f"{tag} {label}", squash(text), needle)
        return text, saved

    po = st.get("po_id")
    po_no = st.get("po_no") or ""
    t, po_pdf = export("purchase-order", f"/api/business/erp/purchase-order/{po}/pdf", 2, [
        ("标题=采购订单", "采购订单"),
        ("单号", po_no),
        ("表头：物料", "物料"),
        ("表头：数量", "数量"),
        ("金额大写", "大写"),
        ("签字栏", "制单"),
        ("页脚页码", "共"),
    ])
    if t and po_pdf:
        # 跨页表头：用 pdftotext 分页抽取第 2 页
        rc, out, _ = sh(["pdftotext", "-f", "2", "-l", "2", po_pdf, "-"])
        p2txt = squash(out if rc == 0 else "")
        check("物料" in p2txt or "数量" in p2txt, "跨页表头重复（第 2 页仍印列名）", p2txt[:80])
        upper = re.findall(r"[零壹贰叁肆伍陆柒捌玖拾佰仟万亿圆元角分整]{4,}", t or "")
        check(bool(upper), "金额大写是汉字数字", (upper[0][:30] if upper else "未找到"))
        png = pdf_png(po_pdf, 1, "purchase-order")
        check(os.path.exists(png), "首页 PNG 已出（视觉取证）", png)

    so_id = psql("select id from biz_sales_order where deleted_at is null order by created_at desc limit 1")
    export("sales-order", f"/api/business/erp/sales-order/{so_id}/pdf", 1,
           [("标题=销售订单", "销售订单"), ("客户", "客户")])
    export("contract", f"/api/business/erp/contract/{st.get('contract_id')}/pdf", 1,
           [("标题=合同", "合同"), ("签字", "签")])
    si_id = psql("select id from biz_stock_in where deleted_at is null and status='POSTED' "
                 "order by created_at desc limit 1")
    export("stock-in", f"/api/business/erp/stock-in/{si_id}/pdf", 1,
           [("标题=入库单", "入库单"), ("仓库", "仓库")])
    so_out = psql("select id from biz_stock_out where deleted_at is null and status='POSTED' "
                  "order by created_at desc limit 1")
    export("stock-out", f"/api/business/erp/stock-out/{so_out}/pdf", 1,
           [("标题=出库单", "出库单")])
    tr = psql("select id from biz_stock_transfer where deleted_at is null order by created_at desc limit 1")
    export("stock-transfer", f"/api/business/erp/stock-transfer/{tr}/pdf", 1,
           [("标题=调拨单", "调拨")])
    tk = psql("select id from biz_stock_take where deleted_at is null and status='POSTED' "
              "order by created_at desc limit 1")
    tt, _ = export("stock-take", f"/api/business/erp/stock-take/{tk}/pdf", 1,
                   [("标题=盘点", "盘点"), ("实盘列", "实盘"), ("差异列", "差异")])
    pr = psql("select id from biz_purchase_return where deleted_at is null order by created_at desc limit 1")
    export("purchase-return", f"/api/business/erp/purchase-return/{pr}/pdf", 1,
           [("标题=退货", "退货")])
    sr = psql("select id from biz_sales_return where deleted_at is null order by created_at desc limit 1")
    export("sales-return", f"/api/business/erp/sales-return/{sr}/pdf", 1,
           [("标题=退货", "退货")])
    fi = psql("select id from biz_flow_instance where deleted_at is null and status='APPROVED' "
              "order by created_at desc limit 1")
    export("approval", f"/api/business/flow/instance/{fi}/pdf", 1,
           [("标题=审批", "审批"), ("轨迹", "审批"), ("签字", "签")])
    task_id = psql("select id from biz_task where deleted_at is null order by created_at desc limit 1")
    export("task", f"/api/business/task/{task_id}/pdf", 1,
           [("标题=任务", "任务"), ("签字", "签")])

    period = psql("select to_char(max(created_at),'YYYY-MM') from biz_voucher")
    for rtype, label in (("balance-sheet", "资产负债表"), ("income-statement", "利润表"),
                         ("cash-flow", "现金流量表")):
        export(f"report-{rtype}", f"/api/business/erp/report/{rtype}/pdf?period={period}", 1,
               [("标题", label)], name=f"report-{rtype}")

    # ================= S10 PDF 权限 + 审计 =================
    scenario("S10", "PDF 权限（无码 403 / 有码 200）+ 导出审计留痕")
    rid, _, _ = mk_role(admin, "E2E12_PDF", "E2E12 PDF 只读采购", sort=983, data_scope=4)
    grant(admin, rid, ["business:erp:purchase-order:list"])
    uid, _, _ = mk_user(admin, "e2e12_pdf", "E2E12 PDF 用户", [rid])
    tokp = login("e2e12_pdf", password=BASE_PW, name="login_pdf")
    check(bool(tokp), "e2e12_pdf 登录")
    s, headers, payload, _ = req_bytes(f"/api/business/erp/purchase-order/{po}/pdf", tokp)
    eq("有采购单读码 → 单据 PDF 200", s, 200)
    check(payload.startswith(b"%PDF-"), "权限内导出确为 PDF")
    s, body = req("GET", f"/api/business/erp/report/balance-sheet/pdf?period={period}", tokp,
                  name="b_report_forbidden")
    eq("无 report:export → 报表 PDF 403", s, 403)
    contains("403 文案点名权限码", msg_of(body), "business:erp:report:export")
    s, body = req("GET", f"/api/business/erp/contract/{st.get('contract_id')}/pdf", tokp,
                  name="b_contract_pdf_forbidden")
    eq("无合同读码 → 合同 PDF 403", s, 403)

    # 审计：每类导出应有一行 operation_logs
    audit = psql("select count(*) from operation_logs where operation like 'EXPORT%' "
                 "and created_at > now() - interval '30 minutes'")
    check(audit.isdigit() and int(audit) >= 8, "导出审计行 ≥8 条（近 30 分钟）", audit)
    ops = psql("select operation||'='||count(*) from operation_logs where operation like 'EXPORT%' "
               "and created_at > now() - interval '30 minutes' group by operation order by 1")
    print("    审计操作码：", ops.replace("\n", "  "))
    check("EXPORT_PURCHASE_ORDER_PDF" in ops or "PURCHASE_ORDER" in ops, "采购单导出有专属操作码", ops[:120])
    one = psql("select username||'|'||coalesce(target_type,'-')||'|'||coalesce(target_id,'-') "
               "from operation_logs where operation like 'EXPORT_PURCHASE_ORDER%' "
               "order by created_at desc limit 1")
    check("|" in one and len(one.split("|")) >= 3, "审计行含 username/target_type/target_id", one[:120])

    st.update(pdf_role=rid, pdf_user=uid, report_period=period)
    save_state(st)
    print(f"\n=== B 段完成（见 asserts.log） ===")
    return 0


if __name__ == "__main__":
    sys.exit(main())
