#!/usr/bin/env python3
"""P1-FIX-9 验收主流程：期初 + 盘点，真 HTTP + 真库。

用法: python3 flow.py <round>
输出: /root/st9-run/round<N>.txt  (每步请求/响应实测)
"""
import json
import sys
import datetime

sys.path.insert(0, '/root/st9-run')
from apilib import Api

ROUND = sys.argv[1] if len(sys.argv) > 1 else "1"
LOG_PATH = f"/root/st9-run/round{ROUND}.txt"
LINES = []
RESULTS = {}
FAILS = []


def emit(title, payload):
    text = json.dumps(payload, ensure_ascii=False, indent=2) if not isinstance(payload, str) else payload
    block = f"\n### {title}\n{text}"
    print(block)
    LINES.append(block)


def brief(obj, keys):
    if not isinstance(obj, dict):
        return obj
    return {k: obj.get(k) for k in keys if k in obj}


def check(name, cond, detail):
    RESULTS[name] = {"pass": bool(cond), "detail": detail}
    if not cond:
        FAILS.append(name)
    emit(f"[{'PASS' if cond else 'FAIL'}] {name}", detail)


def flush():
    with open(LOG_PATH, "w") as fh:
        fh.write("\n".join(LINES) + "\n")
        fh.write("\n## VERDICT\n" + json.dumps({"round": ROUND, "checks": RESULTS, "fails": FAILS}, ensure_ascii=False, indent=2) + "\n")


def voucher_entries(v):
    """凭证分录：字段名以 VoucherEntryRes 为准（account_code / debit_amount / credit_amount）。"""
    return [{"line": e.get("line_no"), "account": e.get("account_code"), "account_name": e.get("account_name"),
             "debit": e.get("debit_amount"), "credit": e.get("credit_amount"),
             "summary": e.get("summary"), "aux_material_id": e.get("aux_material_id"),
             "aux_warehouse_id": e.get("aux_warehouse_id")} for e in (v.get("entries") or v.get("items") or [])]


def die(reason):
    emit("FATAL", reason)
    flush()
    sys.exit(2)


today = datetime.date.today().isoformat()
PERIOD = today[:7]

admin = Api()
admin.login("admin", "St9@Admin#2026")
emit("0.1 登录 admin", {"user": admin.who, "token_head": admin.token[:24] + "..."})

# ---------- 主数据（幂等：已存在则复用） ----------
def ensure_unit():
    page = admin.ok("GET", "/business/erp/unit/page?page=1&page_size=200")
    rows = page.get("list") or page.get("rows") or []
    for r in rows:
        if r.get("unit_code") == "ST9-PCS":
            return r
    r = admin.ok("POST", "/business/erp/unit", {"unit_code": "ST9-PCS", "unit_name": "个", "is_base": True, "status": 1})
    emit("0.2 新建计量单位", brief(r, ["id", "unit_code", "unit_name", "is_base"]))
    return r


def ensure_warehouse():
    page = admin.ok("GET", "/business/erp/warehouse/page?page=1&page_size=200")
    rows = page.get("list") or page.get("rows") or []
    for r in rows:
        if r.get("warehouse_code") == "ST9-WH":
            return r
    r = admin.ok("POST", "/business/erp/warehouse", {
        "warehouse_code": "ST9-WH", "warehouse_name": "P1-FIX-9 验收总仓",
        "warehouse_type": "NORMAL", "status": 1, "level": 1,
    })
    emit("0.3 新建仓库", brief(r, ["id", "warehouse_code", "warehouse_name", "level"]))
    return r


def ensure_material(unit_id):
    page = admin.ok("GET", "/business/erp/material/page?page=1&page_size=200")
    rows = page.get("list") or page.get("rows") or []
    for r in rows:
        if r.get("material_code") == "ST9-M-001":
            return r
    r = admin.ok("POST", "/business/erp/material", {
        "material_code": "ST9-M-001", "material_name": "验收物料A", "unit_id": unit_id,
        "material_type": "RAW", "status": 1, "costing_method": "MOVING_AVG",
    })
    emit("0.4 新建物料", brief(r, ["id", "material_code", "material_name", "unit_id"]))
    return r


unit = ensure_unit()
wh = ensure_warehouse()
mat = ensure_material(unit["id"])
MID, WID = mat["id"], wh["id"]
emit("0.5 主数据", {"material_id": MID, "warehouse_id": WID, "unit_id": unit["id"]})

# ==================== ① 期初 ====================
emit("===== ① 期初余额 =====", "（导入/录入 → 建账 → 库存+凭证齐）")

opening_entries = {
    "period": PERIOD,
    "entries": [
        {"account_code": "1002", "debit": "5000.00", "credit": "0"},
        {"account_code": "3001", "debit": "0", "credit": "5000.00"},
    ],
}
r = admin.ok("POST", "/business/erp/opening-balance", opening_entries)
emit("1.1 录入科目期初（方式A：借 1002 5000 / 贷 3001 5000）", {
    "period": r.get("period"), "period_status": r.get("period_status"), "state": r.get("state"),
    "debit_total": r.get("debit_total"), "credit_total": r.get("credit_total"),
    "combined_debit_total": r.get("combined_debit_total"),
    "combined_credit_total": r.get("combined_credit_total"),
    "difference": r.get("difference"), "balanced": r.get("balanced"),
    "voucher": r.get("voucher"),
})

r = admin.ok("POST", "/business/erp/opening-balance/inventory", {
    "period": PERIOD,
    "items": [{"material_id": MID, "warehouse_id": WID, "qty": "100", "unit_cost": "10", "remark": f"P1-FIX-9 验收 R{ROUND}"}],
})
emit("1.2 录入库存期初（100 个 @ 10 元 = 1000）", {
    "period": r.get("period"), "state": r.get("state"),
    "inventory": r.get("inventory"),
    "inventory_amount": r.get("inventory_amount"),
    "inventory_debit_account": r.get("inventory_debit_account"),
    "inventory_credit_account": r.get("inventory_credit_account"),
    "combined_debit_total": r.get("combined_debit_total"),
    "combined_credit_total": r.get("combined_credit_total"),
    "difference": r.get("difference"), "balanced": r.get("balanced"),
    "voucher": r.get("voucher"), "hints": r.get("hints"),
})

r = admin.ok("GET", f"/business/erp/opening-balance?period={PERIOD}")
emit("1.3 期初总览（再次读取，独立复核）", {
    "state": r.get("state"), "debit_total": r.get("debit_total"), "credit_total": r.get("credit_total"),
    "inventory_amount": r.get("inventory_amount"),
    "combined_debit_total": r.get("combined_debit_total"),
    "combined_credit_total": r.get("combined_credit_total"),
    "difference": r.get("difference"), "balanced": r.get("balanced"),
    "entries": r.get("entries"), "inventory": r.get("inventory"),
    "voucher": r.get("voucher"), "hints": r.get("hints"),
})
opening_voucher_id = (r.get("voucher") or {}).get("id")

check("期初-合并借贷平衡（借=贷=6000）",
      float(r.get("combined_debit_total") or 0) == float(r.get("combined_credit_total") or 0) == 6000.0,
      {"combined_debit_total": r.get("combined_debit_total"), "combined_credit_total": r.get("combined_credit_total"),
       "difference": r.get("difference")})
check("期初-状态 ENTERED", r.get("state") == "ENTERED", {"state": r.get("state")})

if opening_voucher_id:
    v = admin.ok("GET", f"/business/erp/voucher/{opening_voucher_id}")
    entries = voucher_entries(v)
    emit("1.4 期初凭证详情", {
        "voucher_no": v.get("voucher_no"), "voucher_type": v.get("voucher_type"),
        "status": v.get("status"), "period": v.get("period"),
        "total_debit": v.get("total_debit"), "total_credit": v.get("total_credit"),
        "entries": entries,
    })
    codes = {str(e["account"]) for e in entries}
    check("期初凭证-含 1405 与 3103 且已记账（POSTED）", ("1405" in codes and "3103" in codes and v.get("status") == "POSTED"),
          {"codes": sorted(codes), "status": v.get("status")})
else:
    check("期初凭证-已生成", False, {"voucher_id": opening_voucher_id})

inv = admin.ok("GET", f"/business/erp/inventory/page?page=1&page_size=50&material_id={MID}&warehouse_id={WID}")
rows = inv.get("list") or inv.get("rows") or []
row = rows[0] if rows else {}
emit("1.5 库存（成本账）", brief(row, ["material_id", "warehouse_id", "qty", "amount", "avg_cost"]))
loc = admin.ok("GET", f"/business/erp/inventory-location/page?page=1&page_size=50&material_id={MID}&warehouse_id={WID}")
lrows = loc.get("list") or loc.get("rows") or []
emit("1.6 库存（库位账）", [brief(x, ["location_id", "qty"]) for x in lrows])
txn = admin.ok("GET", f"/business/erp/inventory-txn/page?page=1&page_size=50&material_id={MID}&warehouse_id={WID}")
trows = txn.get("list") or txn.get("rows") or []
emit("1.7 库存流水", [brief(x, ["txn_no", "txn_type", "biz_type", "qty", "amount", "period_code"]) for x in trows])

check("期初-库存三本账齐（成本账 100/1000）",
      str(row.get("qty")) in ("100", "100.0000") and float(row.get("amount") or 0) == 1000.0,
      {"qty": row.get("qty"), "amount": row.get("amount"), "avg_cost": row.get("avg_cost")})
check("期初-流水 OPENING/IN 一条", any(x.get("biz_type") == "OPENING" for x in trows),
      [brief(x, ["txn_type", "biz_type", "qty"]) for x in trows])

# ==================== ② 盘点 ====================
emit("===== ② 盘点单 =====", "（建单 → 实盘 → 差异 → 审批 → 过账 → 库存调整 → 作废回滚）")

take = admin.ok("POST", "/business/erp/stock-take", {
    "warehouse_id": WID, "take_date": today, "take_type": "FULL",
    "remark": f"P1-FIX-9 验收 全盘 R{ROUND}",
})
tid = take.get("id") or take.get("take_id")
emit("2.1 建单（全盘，账面数由服务端快照）", brief(take, ["id", "take_no", "status", "status_label", "take_type", "take_date", "total_book_qty"]))

d = admin.ok("GET", f"/business/erp/stock-take/{tid}")
items = d.get("items") or d.get("lines") or []
emit("2.2 详情：账面快照", {
    "take_no": d.get("take_no"), "status": d.get("status"),
    "items": [{"item_id": i.get("id"), "line_no": i.get("line_no"), "material_id": i.get("material_id"),
               "book_qty": i.get("book_qty"), "actual_qty": i.get("actual_qty"),
               "diff_qty": i.get("diff_qty"), "diff_amount": i.get("diff_amount"),
               "unit_cost": i.get("unit_cost")} for i in items],
})
if not items:
    die("盘点单没有明细（全盘应自动快照库存桶）")
book_qty = float(items[0].get("book_qty") or 0)
check("盘点-账面数=库存 100（服务端快照）", book_qty == 100.0, {"book_qty": book_qty})

# 实盘 92（盘亏 8）
d = admin.ok("PUT", f"/business/erp/stock-take/{tid}", {"lines": [{"item_id": items[0]["id"], "actual_qty": "92"}]})
dit = d.get("items") or d.get("lines") or []
emit("2.3 录入实盘数 92", {
    "status": d.get("status"),
    "total_actual_qty": d.get("total_actual_qty"), "total_loss_qty": d.get("total_loss_qty"),
    "total_gain_qty": d.get("total_gain_qty"), "total_diff_amount": d.get("total_diff_amount"),
    "items": [{"actual_qty": i.get("actual_qty"), "diff_qty": i.get("diff_qty"),
               "diff_amount": i.get("diff_amount"), "unit_cost": i.get("unit_cost")} for i in dit],
})
# 口径（stocktake_dto 文件头）：diff_amount 由**过账**时按账上真实差额写，
# 草稿阶段恒为 0；这里只断言生成列 diff_qty（= actual − book），过账后再断言金额。
check("盘点-差异数量 = -8（生成列 actual−book）",
      float((dit[0].get("diff_qty") if dit else 0) or 0) == -8.0,
      {"diff_qty": dit[0].get("diff_qty") if dit else None,
       "diff_amount_草稿阶段": dit[0].get("diff_amount") if dit else None})

# 提交审批
r = admin.call("PATCH", f"/business/erp/stock-take/{tid}/status", {"status": "PENDING_POST"})
emit("2.4 提交审批（DRAFT → PENDING_POST）", {"http": r["_http"], "ok": r["_ok"], "resp": r.get("_error") or "OK"})
check("盘点-提交审批成功", r["_ok"], r.get("_error") or "OK")
d = admin.ok("GET", f"/business/erp/stock-take/{tid}")
emit("2.5 提交后详情", {"status": d.get("status"), "approval_instance_id": d.get("approval_instance_id")})
inst_id = d.get("approval_instance_id")

# 审批人（仓管主管，≠ 制单人 admin）
wh_user = Api()
wh_user.login("st_wh", "St9@Wh#2026")
todo = wh_user.ok("GET", "/business/flow/task/todo?page=1&page_size=20")
trows2 = todo.get("list") or todo.get("rows") or []
mine = [t for t in trows2 if str(t.get("biz_id") or t.get("doc_id") or "") == str(tid) or str(tid) in json.dumps(t, ensure_ascii=False)]
emit("2.6 仓管主管待办", [brief(t, ["id", "node_key", "node_name", "biz_type", "biz_id", "status", "assignee_id"]) for t in trows2])
if not mine:
    check("盘点-审批待办生成", False, {"todo_total": todo.get("total"), "take_id": tid})
    flush()
    sys.exit(3)
task_id = mine[0]["id"]
apr = wh_user.call("POST", f"/business/flow/task/{task_id}/approve", {"comment": f"P1-FIX-9 验收：差异 -8 属实，同意过账调整 R{ROUND}"})
emit("2.7 审批同意", {"task_id": task_id, "http": apr["_http"], "ok": apr["_ok"], "resp": apr.get("_error") or apr.get("data")})
check("盘点-审批通过", apr["_ok"], apr.get("_error") or "OK")

d = admin.ok("GET", f"/business/erp/stock-take/{tid}")
emit("2.8 审批后详情", {"status": d.get("status"), "approval_instance_id": d.get("approval_instance_id")})

# 过账
r = admin.call("PATCH", f"/business/erp/stock-take/{tid}/status", {"status": "POSTED"})
emit("2.9 过账（PENDING_POST → POSTED）", {"http": r["_http"], "ok": r["_ok"], "resp": r.get("_error") or "OK"})
check("盘点-过账成功", r["_ok"], r.get("_error") or "OK")

d = admin.ok("GET", f"/business/erp/stock-take/{tid}")
dit2 = d.get("items") or d.get("lines") or []
emit("2.10 过账后详情（差异凭证）", {
    "status": d.get("status"), "voucher_id": d.get("voucher_id"),
    "total_diff_amount": d.get("total_diff_amount"),
    "total_gain_amount": d.get("total_gain_amount"), "total_loss_amount": d.get("total_loss_amount"),
    "items": [{"book_qty": i.get("book_qty"), "actual_qty": i.get("actual_qty"),
               "diff_qty": i.get("diff_qty"), "diff_amount": i.get("diff_amount")} for i in dit2],
    "posted_by": d.get("posted_by"), "posted_at": d.get("posted_at"),
})
check("盘点-过账后差异金额 = 80（盘亏）",
      float(d.get("total_loss_amount") or 0) == 80.0
      and abs(float((dit2[0].get("diff_amount") if dit2 else 0) or 0)) == 80.0,
      {"total_loss_amount": d.get("total_loss_amount"),
       "item_diff_amount": dit2[0].get("diff_amount") if dit2 else None})
diff_voucher = d.get("voucher_id")

inv = admin.ok("GET", f"/business/erp/inventory/page?page=1&page_size=50&material_id={MID}&warehouse_id={WID}")
row2 = (inv.get("list") or inv.get("rows") or [{}])[0]
txn = admin.ok("GET", f"/business/erp/inventory-txn/page?page=1&page_size=50&material_id={MID}&warehouse_id={WID}")
trows3 = txn.get("list") or txn.get("rows") or []
emit("2.11 过账后库存调整", {
    "inventory": brief(row2, ["qty", "amount", "avg_cost"]),
    "txns": [brief(x, ["txn_type", "biz_type", "doc_no", "qty", "amount"]) for x in trows3],
})
check("盘点-库存已调整 100 → 92（成本账）", float(row2.get("qty") or 0) == 92.0, {"qty": row2.get("qty"), "amount": row2.get("amount")})
check("盘点-出库流水 8 存在", any(x.get("txn_type") == "OUT" and float(x.get("qty") or 0) == 8.0 for x in trows3),
      [brief(x, ["txn_type", "biz_type", "qty"]) for x in trows3])

if diff_voucher:
    v = admin.ok("GET", f"/business/erp/voucher/{diff_voucher}")
    entries = voucher_entries(v)
    emit("2.12 盘点差异凭证", {
        "voucher_no": v.get("voucher_no"), "status": v.get("status"), "period": v.get("period"),
        "total_debit": v.get("total_debit"), "total_credit": v.get("total_credit"),
        "entries": entries,
    })
    codes = {str(e["account"]) for e in entries}
    debit_1901 = any(e["account"] == "1901" and float(e["debit"] or 0) == 80.0 for e in entries)
    credit_1405 = any(e["account"] == "1405" and float(e["credit"] or 0) == 80.0 for e in entries)
    check("盘点-差异凭证 借1901 80 / 贷1405 80 且 POSTED",
          (debit_1901 and credit_1405 and v.get("status") == "POSTED"),
          {"codes": sorted(codes), "status": v.get("status"),
           "借1901_80": debit_1901, "贷1405_80": credit_1405})
else:
    check("盘点-差异凭证已生成", False, {"voucher_id": diff_voucher})

# 作废回滚（POSTED → CANCELLED）
r = admin.call("PATCH", f"/business/erp/stock-take/{tid}/status", {"status": "CANCELLED", "reason": f"P1-FIX-9 验收：作废回滚验证 R{ROUND}"})
emit("2.13 作废（POSTED → CANCELLED，必填原因）", {"http": r["_http"], "ok": r["_ok"], "resp": r.get("_error") or "OK"})
check("盘点-作废成功", r["_ok"], r.get("_error") or "OK")

d = admin.ok("GET", f"/business/erp/stock-take/{tid}")
inv = admin.ok("GET", f"/business/erp/inventory/page?page=1&page_size=50&material_id={MID}&warehouse_id={WID}")
row3 = (inv.get("list") or inv.get("rows") or [{}])[0]
txn = admin.ok("GET", f"/business/erp/inventory-txn/page?page=1&page_size=50&material_id={MID}&warehouse_id={WID}")
trows4 = txn.get("list") or txn.get("rows") or []
emit("2.14 作废后回滚结果", {
    "status": d.get("status"), "reverse_voucher_id": d.get("reverse_voucher_id"),
    "inventory": brief(row3, ["qty", "amount", "avg_cost"]),
    "txn_count": len(trows4),
    "txns": [brief(x, ["txn_type", "biz_type", "doc_no", "qty"]) for x in trows4],
})
check("盘点-作废回滚：库存回到 100", float(row3.get("qty") or 0) == 100.0, {"qty": row3.get("qty"), "amount": row3.get("amount")})
check("盘点-红冲凭证已生成", bool(d.get("reverse_voucher_id")), {"reverse_voucher_id": d.get("reverse_voucher_id")})

rev_id = d.get("reverse_voucher_id")
if rev_id:
    v = admin.ok("GET", f"/business/erp/voucher/{rev_id}")
    entries = voucher_entries(v)
    emit("2.15 红冲凭证", {
        "voucher_no": v.get("voucher_no"), "status": v.get("status"),
        "total_debit": v.get("total_debit"), "total_credit": v.get("total_credit"),
        "entries": entries,
    })
    red = any(e["account"] == "1901" and float(e["credit"] or 0) == 80.0 for e in entries) and \
          any(e["account"] == "1405" and float(e["debit"] or 0) == 80.0 for e in entries)
    check("盘点-红冲凭证方向相反（贷1901 80 / 借1405 80）", red, {"entries": entries})

# 列表页复核
page = admin.ok("GET", "/business/erp/stock-take/page?page=1&page_size=10")
prows = page.get("list") or page.get("rows") or []
emit("2.16 盘点单列表（列表接口复核）", {"total": page.get("total"),
      "rows": [brief(x, ["take_no", "status", "status_label", "total_diff_amount", "voucher_id"]) for x in prows]})
check("盘点-列表可见本单", any(x.get("id") == tid or x.get("take_no") == d.get("take_no") for x in prows),
      {"total": page.get("total")})

emit("===== 结论 =====", {"round": ROUND, "checks": {k: v["pass"] for k, v in RESULTS.items()}, "fails": FAILS,
                          "take_no": d.get("take_no"), "take_id": tid})
flush()
print("FAILS:", FAILS)
sys.exit(0 if not FAILS else 1)
