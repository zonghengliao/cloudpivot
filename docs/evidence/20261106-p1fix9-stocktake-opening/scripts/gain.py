#!/usr/bin/env python3
"""P1-FIX-9 验收（分支覆盖）：盘盈分支 105 > 100，走 stockin 侧记账（借1405 / 贷1901），
再作废回滚（走 stock_in_cancel 的 INV_SHORTFALL 闸）。"""
import json
import sys
import datetime

sys.path.insert(0, '/root/st9-run')
from apilib import Api

LOG_PATH = "/root/st9-run/gain.txt"
LINES = []
RESULTS = {}
FAILS = []


def emit(title, payload):
    text = json.dumps(payload, ensure_ascii=False, indent=2) if not isinstance(payload, str) else payload
    block = f"\n### {title}\n{text}"
    print(block)
    LINES.append(block)


def check(name, cond, detail):
    RESULTS[name] = {"pass": bool(cond), "detail": detail}
    if not cond:
        FAILS.append(name)
    emit(f"[{'PASS' if cond else 'FAIL'}] {name}", detail)


def voucher_entries(v):
    return [{"line": e.get("line_no"), "account": e.get("account_code"), "debit": e.get("debit_amount"),
             "credit": e.get("credit_amount"), "summary": e.get("summary")}
            for e in (v.get("entries") or v.get("items") or [])]


today = datetime.date.today().isoformat()
admin = Api()
admin.login("admin", "St9@Admin#2026")

unit = (admin.ok("GET", "/business/erp/unit/page?page=1&page_size=200").get("list") or [])
mid = [r for r in (admin.ok("GET", "/business/erp/material/page?page=1&page_size=200").get("list") or []) if r.get("material_code") == "ST9-M-001"][0]["id"]
wid = [r for r in (admin.ok("GET", "/business/erp/warehouse/page?page=1&page_size=200").get("list") or []) if r.get("warehouse_code") == "ST9-WH"][0]["id"]

take = admin.ok("POST", "/business/erp/stock-take", {"warehouse_id": wid, "take_date": today, "take_type": "FULL",
                                                    "remark": "P1-FIX-9 验收 盘盈分支（实盘 105 > 账面 100）"})
tid = take["id"]
emit("G1 建单", {"take_no": take.get("take_no"), "total_book_qty": take.get("total_book_qty")})

d = admin.ok("GET", f"/business/erp/stock-take/{tid}")
item = (d.get("items") or [])[0]
d = admin.ok("PUT", f"/business/erp/stock-take/{tid}", {"lines": [{"item_id": item["id"], "actual_qty": "105"}]})
dit = (d.get("items") or [])[0]
emit("G2 录入实盘 105（盘盈 5）", {"diff_qty": dit.get("diff_qty"), "unit_cost": dit.get("unit_cost")})
check("盘盈-差异数量 +5", float(dit.get("diff_qty") or 0) == 5.0, {"diff_qty": dit.get("diff_qty")})

r = admin.call("PATCH", f"/business/erp/stock-take/{tid}/status", {"status": "PENDING_POST"})
check("盘盈-提交审批", r["_ok"], r.get("_error") or "OK")

wh_user = Api()
wh_user.login("st_wh", "St9@Wh#2026")
todo = wh_user.ok("GET", "/business/flow/task/todo?page=1&page_size=20")
mine = [t for t in (todo.get("list") or []) if str(tid) in json.dumps(t, ensure_ascii=False)]
emit("G3 待办", [{"id": t.get("id"), "node": t.get("node_name")} for t in mine])
if not mine:
    emit("FATAL", "盘盈单没有生成审批待办")
    open(LOG_PATH, "w").write("\n".join(LINES))
    sys.exit(3)
apr = wh_user.call("POST", f"/business/flow/task/{mine[0]['id']}/approve", {"comment": "P1-FIX-9 验收：盘盈 5 属实"})
check("盘盈-审批通过", apr["_ok"], apr.get("_error") or apr.get("data"))

r = admin.call("PATCH", f"/business/erp/stock-take/{tid}/status", {"status": "POSTED"})
check("盘盈-过账", r["_ok"], r.get("_error") or "OK")

d = admin.ok("GET", f"/business/erp/stock-take/{tid}")
inv = admin.ok("GET", f"/business/erp/inventory/page?page=1&page_size=50&material_id={mid}&warehouse_id={wid}")
row = inv["list"][0]
emit("G4 过账后", {"status": d.get("status"), "total_gain_amount": d.get("total_gain_amount"),
                  "inventory": {"qty": row.get("qty"), "amount": row.get("amount"), "avg_cost": row.get("avg_cost")},
                  "voucher_id": d.get("voucher_id")})
check("盘盈-库存 100→105/金额 1050", float(row.get("qty") or 0) == 105.0 and float(row.get("amount") or 0) == 1050.0,
      {"qty": row.get("qty"), "amount": row.get("amount")})
v = admin.ok("GET", f"/business/erp/voucher/{d['voucher_id']}")
entries = voucher_entries(v)
emit("G5 盘盈差异凭证", {"voucher_no": v.get("voucher_no"), "status": v.get("status"), "entries": entries})
check("盘盈-借1405 50 / 贷1901 50",
      any(e["account"] == "1405" and float(e["debit"] or 0) == 50.0 for e in entries)
      and any(e["account"] == "1901" and float(e["credit"] or 0) == 50.0 for e in entries), {"entries": entries})

r = admin.call("PATCH", f"/business/erp/stock-take/{tid}/status", {"status": "CANCELLED", "reason": "P1-FIX-9 验收：盘盈单作废回滚"})
check("盘盈-作废", r["_ok"], r.get("_error") or "OK")
d = admin.ok("GET", f"/business/erp/stock-take/{tid}")
inv = admin.ok("GET", f"/business/erp/inventory/page?page=1&page_size=50&material_id={mid}&warehouse_id={wid}")
row = inv["list"][0]
txn = admin.ok("GET", f"/business/erp/inventory-txn/page?page=1&page_size=50&material_id={mid}&warehouse_id={wid}")
emit("G6 作废后回滚", {"status": d.get("status"), "reverse_voucher_id": d.get("reverse_voucher_id"),
                      "inventory": {"qty": row.get("qty"), "amount": row.get("amount")},
                      "txns": [{"txn_type": t.get("txn_type"), "biz_type": t.get("biz_type"), "qty": t.get("qty")} for t in (txn.get("list") or [])]})
check("盘盈-作废回滚：库存回 100 / 1000", float(row.get("qty") or 0) == 100.0 and float(row.get("amount") or 0) == 1000.0,
      {"qty": row.get("qty"), "amount": row.get("amount")})
check("盘盈-红冲凭证已生成", bool(d.get("reverse_voucher_id")), {"reverse_voucher_id": d.get("reverse_voucher_id")})

emit("===== 结论 =====", {"checks": {k: v["pass"] for k, v in RESULTS.items()}, "fails": FAILS})
open(LOG_PATH, "w").write("\n".join(LINES) + "\n")
print("FAILS:", FAILS)
sys.exit(0 if not FAILS else 1)
