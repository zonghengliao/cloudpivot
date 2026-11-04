#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""AUDIT-ALL 第七节用：能力缺口扫描（有 / 无 / 半截）。

判据：
  · 命中「后端代码」= 有实现；命中「只有前端/文档」= 半截或纯设计；
  · 零命中 = 无。
每个能力给：后端命中文件数 + 前端命中文件数 + 文档命中 + 结论。
"""
import re, sys, glob, os

ROOT = sys.argv[1] if len(sys.argv) > 1 else "/opt/cp_audit_head"

CAPS = [
    # (模块, 能力, [关键词正则])
    ("主数据", "客户主数据", [r"biz_customer", r"CustomerModel", r"客户主数据"]),
    ("主数据", "物料分类/物料组", [r"material_category", r"biz_material_group", r"物料分类"]),
    ("主数据", "BOM 物料清单", [r"\bBOM\b", r"biz_bom", r"物料清单"]),
    ("主数据", "工艺路线/工序", [r"biz_routing", r"process_route", r"工艺路线", r"\brouting\b"]),
    ("主数据", "多计量单位换算", [r"base_unit_id", r"\brate\b"]),
    ("采购", "请购单（PR）", [r"purchase_requisition", r"biz_requisition", r"请购"]),
    ("采购", "询价比价", [r"biz_quote\b", r"询价", r"比价"]),
    ("采购", "采购退货", [r"purchase_return", r"退货"]),
    ("采购", "采购计划/预测", [r"purchase_plan", r"采购计划", r"\bMRP\b"]),
    ("采购", "合同（仅采购）", [r"biz_contract", r"contract_create"]),
    ("销售", "销售订单", [r"sales_order", r"biz_sales", r"销售订单"]),
    ("销售", "发货单/出库给客户", [r"delivery_note", r"biz_shipment", r"发货单"]),
    ("销售", "报价单", [r"quotation", r"报价单"]),
    ("销售", "销售退货", [r"sales_return", r"销售退货"]),
    ("销售", "应收（AR）", [r"biz_receivable", r"\bAR\b", r"应收账款", r"应收单"]),
    ("销售", "发票/税务（销项）", [r"invoice", r"销项", r"发票"]),
    ("库存", "库位管理", [r"biz_location", r"inventory_location"]),
    ("库存", "库位预留/锁定", [r"locked_qty", r"预留"]),
    ("库存", "批次管理", [r"batch_no", r"biz_batch", r"批次"]),
    ("库存", "库龄分析", [r"inventory_age", r"库龄"]),
    ("库存", "循环盘点", [r"stock_take", r"盘点"]),
    ("库存", "序列号（一物一码）", [r"biz_material_serial", r"serial_no"]),
    ("库存", "负库存拦截", [r"allow_negative", r"负库存"]),
    ("生产", "工单/生产订单", [r"work_order", r"biz_work_order", r"生产订单"]),
    ("生产", "工序派工/报工", [r"dispatch", r"报工", r"biz_worklog_process"]),
    ("生产", "生产领料/退料", [r"领料", r"退料"]),
    ("生产", "委外加工", [r"outsourc", r"委外"]),
    ("生产", "MRP 排产", [r"\bMRP\b", r"排产", r"scheduling"]),
    ("生产", "产能负荷", [r"capacity_load", r"产能"]),
    ("质量", "IQC/IPQC/FQC 检验", [r"\bIQC\b", r"\bIPQC\b", r"\bFQC\b", r"检验单"]),
    ("质量", "不合格品/让步接收", [r"nonconform", r"不合格品"]),
    ("质量", "质量追溯", [r"traceability", r"质量追溯"]),
    ("质量", "SPC 统计过程控制", [r"\bSPC\b"]),
    ("设备", "设备台账", [r"equipment", r"设备台账"]),
    ("设备", "保养/维修", [r"maintenance", r"保养", r"维修工单"]),
    ("设备", "OEE", [r"\bOEE\b"]),
    ("MES", "批次追溯 Lot/Wafer", [r"\bLot\b", r"wafer", r"批次追溯"]),
    ("MES", "WIP 在制", [r"\bWIP\b", r"在制"]),
    ("MES", "良率/Bin 管理", [r"yield", r"良率", r"\bBin\b"]),
    ("MES", "设备数据采集（SCADA）", [r"SCADA", r"OPC[ -]?UA", r"采集网关"]),
    ("财务", "总账/凭证", [r"biz_voucher", r"biz_voucher_entry"]),
    ("财务", "应付（AP）", [r"biz_payable"]),
    ("财务", "付款/核销", [r"biz_payment", r"核销"]),
    ("财务", "应收（AR）", [r"biz_receivable", r"biz_receipt", r"收款单"]),
    ("财务", "固定资产", [r"fixed_asset", r"固定资产"]),
    ("财务", "费用报销", [r"expense_claim", r"费用报销", r"报销单"]),
    ("财务", "预算控制", [r"\bbudget\b", r"预算"]),
    ("财务", "资金管理/银行对账", [r"bank_reconcil", r"资金管理", r"银行对账"]),
    ("财务", "工单成本/成本核算", [r"job_cost", r"成本核算", r"cost_sheet"]),
    ("财务", "多币种", [r"currency_code", r"exchange_rate", r"汇率"]),
    ("财务", "多组织/多账簿", [r"org_id", r"biz_org", r"多组织"]),
    ("平台", "多租户", [r"tenant_id", r"多租户"]),
    ("平台", "i18n 国际化", [r"i18n", r"locale", r"\$t\("]),
    ("平台", "合规审计日志", [r"operation_logs", r"audit_logs", r"sys_change_log"]),
    ("平台", "备份/恢复界面", [r"backup", r"备份"]),
    ("平台", "数据权限（行级）", [r"data_scope", r"role_dept", r"scope::"]),
    ("平台", "字段脱敏", [r"sys_field_mask"]),
    ("协同", "站内信/通知", [r"sys_notice"]),
    ("协同", "IM 集成（企微/钉钉）", [r"wechat", r"dingtalk", r"企微", r"钉钉"]),
    ("协同", "移动端", [r"uni-app", r"移动端", r"mobile_app"]),
]


def count(patterns, roots, exts=(".rs",)):
    files = set()
    for root in roots:
        for ext in exts:
            files |= set(glob.glob(os.path.join(ROOT, root, "**", "*" + ext), recursive=True))
    hits = 0
    for f in files:
        try:
            txt = open(f, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        if any(re.search(p, txt) for p in patterns):
            hits += 1
    return hits


print("# AUDIT-ALL 第七节 · 能力缺口扫描（代码 != 文档）")
print(f"# repo_root = {ROOT}")
print()
print(f"{'模块':<6} {'能力':<26} {'后端':>4} {'前端':>4} {'文档':>4}  判定")
print("-" * 78)
for mod, cap, pats in CAPS:
    be = count(pats, ["backend/src"])
    fe = count(pats, ["frontend/src"], (".vue", ".ts"))
    dc = count(pats, ["docs", "skill"], (".md",))
    if be >= 3:
        verdict = "有（后端实现）"
    elif be > 0 and fe > 0:
        verdict = "半截"
    elif be > 0:
        verdict = "半截（仅后端局部）"
    elif fe > 0:
        verdict = "半截（仅前端壳）"
    elif dc > 0:
        verdict = "无（仅文档/设计）"
    else:
        verdict = "无"
    print(f"{mod:<6} {cap:<26} {be:>4} {fe:>4} {dc:>4}  {verdict}")
