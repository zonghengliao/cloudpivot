import json
import subprocess
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18090/api"
DB = "cloudpivot_prt03"
PASS = 0
FAIL = 0
FAILS = []


def ok(cond, label, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"[PASS] {label}")
    else:
        FAIL += 1
        FAILS.append(f"{label} :: {detail}")
        print(f"[FAIL] {label} :: {detail}")


def psql(sql):
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-Atc", sql],
        capture_output=True, text=True, env={"PGPASSWORD": "alun", "PATH": "/usr/bin:/bin"},
    )
    if out.returncode != 0:
        return f"<ERR:{out.stderr.strip()}>"
    return out.stdout.strip()


def req(method, path, token=None, body=None):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        payload = e.read().decode()
        try:
            return e.code, json.loads(payload or "{}")
        except json.JSONDecodeError:
            return e.code, {"raw": payload}
    except Exception as e:  # noqa: BLE001
        return 0, {"error": str(e)}


def login(username, password):
    status, body = req("POST", "/system/auth/login", body={"username": username, "password": password})
    if status != 200:
        return None, body
    data = body.get("data") or {}
    token = data.get("access_token")
    if data.get("must_change_password"):
        new = "Prt@12345678"
        st, _ = req("PUT", "/system/auth/password", token=token, body={"old_password": password, "new_password": new})
        if st == 200:
            status, body = req("POST", "/system/auth/login", body={"username": username, "password": new})
            data = (body.get("data") or {})
            token = data.get("access_token")
    return token, body


def todos(token):
    status, body = req("GET", "/business/flow/task/todo", token=token)
    data = body.get("data") or {}
    return data.get("list") or data.get("rows") or []


def approve_all(token, instance_biz_no):
    approved = 0
    for _ in range(6):
        items = todos(token)
        target = [t for t in items if (t.get("biz_no") == instance_biz_no or instance_biz_no in json.dumps(t))]
        if not target:
            break
        st, body = req("POST", f"/business/flow/task/{target[0]['id']}/approve", token=token, body={"comment": "同意退货"})
        if st != 200:
            print("  approve failed", st, body)
            break
        approved += 1
    return approved


print("== 0. 夹具：采购入库 100 个 @10 含税 13%（PO + POSTED 入库单 + 应付 + 库存三本账） ==")
fixture = r"""
DO $$
DECLARE
  v_mat uuid := '30000000-0000-4000-8000-0000000a0001';
  v_sup uuid := '30000000-0000-4000-8000-0000000a0002';
  v_wh  uuid := '30000000-0000-4000-8000-0000000a0003';
  v_unit uuid := '30000000-0000-4000-8000-0000000a0004';
  v_po  uuid := '30000000-0000-4000-8000-0000000a0005';
  v_poi uuid := '30000000-0000-4000-8000-0000000a0006';
  v_si  uuid := '30000000-0000-4000-8000-0000000a0007';
  v_sii uuid := '30000000-0000-4000-8000-0000000a0008';
  v_ap  uuid := '30000000-0000-4000-8000-0000000a0009';
  v_inv uuid := '30000000-0000-4000-8000-0000000a000a';
  v_il  uuid := '30000000-0000-4000-8000-0000000a000b';
BEGIN
  DELETE FROM biz_purchase_return_item WHERE return_id IN (SELECT id FROM biz_purchase_return WHERE return_no LIKE 'PRT-%');
  DELETE FROM biz_purchase_return WHERE return_no LIKE 'PRT-%';
  DELETE FROM biz_supplier_receivable WHERE source_type = 'PURCHASE_RETURN';
  DELETE FROM biz_stock_out_item WHERE stock_out_id IN (SELECT id FROM biz_stock_out WHERE source_type = 'PURCHASE_RETURN');
  DELETE FROM biz_stock_out WHERE source_type = 'PURCHASE_RETURN';
  DELETE FROM biz_inventory_txn WHERE doc_no LIKE 'OUT-%' AND biz_type = 'PURCHASE_RETURN';
  DELETE FROM biz_inventory_txn WHERE doc_id IN (SELECT id FROM biz_stock_out WHERE source_type = 'PURCHASE_RETURN');
  DELETE FROM biz_payment WHERE payable_id IN (SELECT id FROM biz_payable WHERE source_id IN
    ('30000000-0000-4000-8000-0000000a0007'::uuid, '30000000-0000-4000-8000-0000000a0017'::uuid));
  DELETE FROM biz_payable WHERE source_id IN ('30000000-0000-4000-8000-0000000a0007'::uuid,
                                              '30000000-0000-4000-8000-0000000a0017'::uuid);
  DELETE FROM biz_stock_in_item WHERE stock_in_id IN ('30000000-0000-4000-8000-0000000a0007'::uuid,
                                                     '30000000-0000-4000-8000-0000000a0017'::uuid);
  DELETE FROM biz_stock_in WHERE id IN ('30000000-0000-4000-8000-0000000a0007'::uuid,
                                       '30000000-0000-4000-8000-0000000a0017'::uuid);
  DELETE FROM biz_purchase_order_item WHERE order_id = v_po;
  DELETE FROM biz_purchase_order WHERE id = v_po;
  DELETE FROM biz_inventory_location WHERE material_id = v_mat;
  DELETE FROM biz_inventory WHERE material_id = v_mat;

  INSERT INTO biz_unit (id, unit_code, unit_name) VALUES (v_unit, 'PRT-PCS', '个') ON CONFLICT DO NOTHING;
  INSERT INTO biz_material (id, material_code, material_name, unit_id, tax_rate)
    VALUES (v_mat, 'M-PRT-001', '退货测试物料', v_unit, 0) ON CONFLICT DO NOTHING;
  INSERT INTO biz_supplier (id, supplier_code, supplier_name) VALUES (v_sup, 'SUP-PRT', '退货测试供应商') ON CONFLICT DO NOTHING;
  INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path) VALUES (v_wh, 'WH-PRT', '退货测试仓', '/PRT') ON CONFLICT DO NOTHING;

  INSERT INTO biz_purchase_order (id, order_no, supplier_id, warehouse_id, order_date, status,
      total_qty, total_amount, total_tax, total_with_tax, currency_code)
    VALUES (v_po, 'PO-PRT-0001', v_sup, v_wh, '2026-11-01', 'RECEIVED', 100, 1000, 130, 1130, 'CNY');
  INSERT INTO biz_purchase_order_item (id, order_id, line_no, material_id, unit_id, qty, price, price_incl,
      tax_rate, amount, tax_amount, with_tax_amount)
    VALUES (v_poi, v_po, 1, v_mat, v_unit, 100, 10, 11.3, 13, 1000, 130, 1130);

  INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status,
      currency_code, total_qty, total_amount, total_tax, total_with_tax, posted_at)
    VALUES (v_si, 'IN-PRT-0001', 'PURCHASE', v_po, v_sup, v_wh, '2026-11-02', 'POSTED', 'CNY', 100, 1000, 130, 1130, now());
  INSERT INTO biz_stock_in_item (id, stock_in_id, line_no, source_item_id, material_id, unit_id, qty, price,
      price_incl, tax_rate, amount, tax_amount, with_tax_amount, cost_price)
    VALUES (v_sii, v_si, 1, v_poi, v_mat, v_unit, 100, 10, 11.3, 13, 1000, 130, 1130, 10);

  INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no, amount,
      paid_amount, unpaid_amount, credit_days, due_date, period_code, status)
    VALUES (v_ap, 'AP-PRT-0001', v_sup, 'STOCK_IN', v_si, 'IN-PRT-0001', 1130, 0, 1130, 30, '2026-12-02', '2026-11', 'OPEN');
  UPDATE biz_stock_in SET payable_id = v_ap WHERE id = v_si;

  INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, amount, avg_cost)
    VALUES (v_inv, v_mat, v_wh, NULL, 100, 1000, 10);
  INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
    VALUES (v_il, v_mat, v_wh, NULL, 100);
  RAISE NOTICE 'PRT fixture ready';
END $$;
"""
print(psql(fixture)[:300])

# 审批人夹具：采购经理 / 财务经理（密码与 admin 相同哈希）
psql(
    "INSERT INTO sys_user (id, username, password, real_name, status) "
    "SELECT '30000000-0000-4000-8000-0000000a0011'::uuid, 'prt_pm', password, '退货采购经理', 1 "
    "  FROM sys_user WHERE username = 'admin' ON CONFLICT (username) DO NOTHING;"
)
psql(
    "INSERT INTO sys_user (id, username, password, real_name, status) "
    "SELECT '30000000-0000-4000-8000-0000000a0012'::uuid, 'prt_fin', password, '退货财务经理', 1 "
    "  FROM sys_user WHERE username = 'admin' ON CONFLICT (username) DO NOTHING;"
)
psql(
    "INSERT INTO sys_user_role (user_id, role_id) SELECT u.id, r.id FROM sys_user u, sys_role r "
    " WHERE u.username = 'prt_pm' AND r.role_code = 'PURCHASE_MANAGER' ON CONFLICT DO NOTHING;"
)
psql(
    "INSERT INTO sys_user_role (user_id, role_id) SELECT u.id, r.id FROM sys_user u, sys_role r "
    " WHERE u.username = 'prt_fin' AND r.role_code = 'FINANCE_MANAGER' ON CONFLICT DO NOTHING;"
)
print("approvers:", psql("select count(*) from sys_user where username in ('prt_pm','prt_fin')"))

print("== 1. 登录 ==")
admin, body = login("admin", "Admin@123456")
ok(admin is not None, "admin 登录", str(body)[:200])
pm, body_pm = login("prt_pm", "Admin@123456")
ok(pm is not None, "采购经理登录", str(body_pm)[:200])
fin, body_fin = login("prt_fin", "Admin@123456")
ok(fin is not None, "财务经理登录", str(body_fin)[:200])

print("== 2. 可退明细：入库 100 / 可退 100 ==")
st, body = req("GET", "/business/erp/purchase-return/source/30000000-0000-4000-8000-0000000a0007", token=admin)
data = body.get("data") or {}
items = data.get("items") or []
ok(st == 200 and len(items) == 1, "可退明细返回 1 行", f"{st} {body}")
if items:
    ok(float(items[0]["received_qty"]) == 100, "原入库 100", items[0])
    ok(float(items[0]["returnable_qty"]) == 100, "可退 100", items[0])
    ok(float(items[0]["price"]) == 10 and float(items[0]["price_incl"]) == 11.3, "单价取入库快照", items[0])

print("== 3. 验收②：退 30 个 → 库存 -30 / 应付 -339 / 凭证平衡 ==")
st, body = req(
    "POST", "/business/erp/purchase-return", token=admin,
    body={"stock_in_id": "30000000-0000-4000-8000-0000000a0007", "reason": "质量不合格退货",
          "items": [{"stock_in_item_id": "30000000-0000-4000-8000-0000000a0008", "qty": 30}]},
)
d = body.get("data") or {}
r1 = (d.get("purchase_return") or {})
rid = r1.get("id")
ok(st == 200 and rid, "建退货单 30 个", f"{st} {body}")
ok(str(r1.get("total_with_tax")) in ("339", "339.0000"), "价税合计 339", r1.get("total_with_tax"))
ok(r1.get("status") == "DRAFT", "初始 DRAFT", r1.get("status"))

# 价格锁死：传 9 元必须被拦（66053）
st, body = req(
    "POST", "/business/erp/purchase-return", token=admin,
    body={"stock_in_id": "30000000-0000-4000-8000-0000000a0007", "reason": "改价尝试",
          "items": [{"stock_in_item_id": "30000000-0000-4000-8000-0000000a0008", "qty": 1, "unit_price": 9}]},
)
ok(st == 400 and body.get("code") == 66053, "改价被拦（66053）", f"{st} {body}")

st, body = req("POST", f"/business/erp/purchase-return/{rid}/submit", token=admin, body={})
ok(st == 200 and ((body.get("data") or {}).get("purchase_return") or {}).get("status") == "SUBMITTED",
   "提交审批 → SUBMITTED", f"{st} {body}")

n_pm = approve_all(pm, r1.get("return_no"))
n_fin = approve_all(fin, r1.get("return_no"))
ok(n_pm >= 1, "采购经理审批通过", str(n_pm))
ok(n_fin >= 1, "财务经理审批通过", str(n_fin))
st, body = req("GET", f"/business/erp/purchase-return/{rid}", token=admin)
row = (body.get("data") or {}).get("purchase_return") or {}
ok(row.get("status") == "APPROVED", "两级审批后 APPROVED", str(row.get("status")))

st, body = req("POST", f"/business/erp/purchase-return/{rid}/confirm", token=admin, body={"reason": "确认退货"})
row = (body.get("data") or {}).get("purchase_return") or {}
ok(st == 200 and row.get("status") == "CONFIRMED", "确认退货 → CONFIRMED", f"{st} {body}")
print("  confirmed:", {k: row.get(k) for k in ("stock_out_no", "payable_no", "voucher_no", "supplier_receivable_no")})

qty = psql("select qty::text || '|' || amount::text from biz_inventory where material_id='30000000-0000-4000-8000-0000000a0001'")
ok(qty.startswith("70.0000|700"), "库存 -30（70 / 700）", qty)
ap = psql("select amount::text || '|' || unpaid_amount::text || '|' || status from biz_payable where id='30000000-0000-4000-8000-0000000a0009'")
ok(ap.startswith("791.0000|791.0000|OPEN"), "应付 -339（791 / OPEN）", ap)
txn = psql("select count(*) || '|' || coalesce(sum(qty),0)::text || '|' || coalesce(sum(amount),0)::text from biz_inventory_txn where biz_type='PURCHASE_RETURN'")
ok(txn == "1|30.0000|300.0000", "流水 1 条 OUT 30 / 成本 300", txn)
so = psql("select status || '|' || out_type || '|' || source_type || '|' || total_qty::text from biz_stock_out where source_type='PURCHASE_RETURN'")
ok(so == "POSTED|OTHER|PURCHASE_RETURN|30.0000", "出库单 POSTED/OTHER/PURCHASE_RETURN/30", so)
bal = psql(
    "select (sum(debit_amount)=sum(credit_amount))::text || '|' || sum(debit_amount)::text from biz_voucher_entry e "
    "join biz_voucher v on v.id=e.voucher_id where v.source_type='STOCK_OUT' and v.source_no like 'OUT-%' "
    "and v.id = (select voucher_id from biz_stock_out where source_type='PURCHASE_RETURN' limit 1)"
)
ok(bal.startswith("true|339"), "凭证借贷平衡且借方 339", bal)
recv = psql("select count(*) from biz_supplier_receivable where source_type='PURCHASE_RETURN'")
ok(recv == "0", "未付款场景不生成供应商应退款", recv)
rq = psql(
    "select (coalesce((select sum(si.qty) from biz_stock_in s join biz_stock_in_item si on si.stock_in_id=s.id "
    "where s.source_order_id=i.order_id and si.source_item_id=i.id and s.status='POSTED' and s.deleted_at is null),0) "
    "- coalesce((select sum(pri.qty) from biz_purchase_return_item pri join biz_purchase_return pr on pr.id=pri.return_id "
    "where pri.purchase_order_item_id=i.id and pr.status='CONFIRMED' and pr.deleted_at is null),0))::text "
    "from biz_purchase_order_item i where i.id='30000000-0000-4000-8000-0000000a0006'"
)
ok(rq.startswith("70"), "采购订单已收数量派生为 70", rq)

print("== 4. 验收③：再退 80 个 → 必须被拦截（可退只剩 70） ==")
st, body = req(
    "POST", "/business/erp/purchase-return", token=admin,
    body={"stock_in_id": "30000000-0000-4000-8000-0000000a0007", "reason": "超退尝试",
          "items": [{"stock_in_item_id": "30000000-0000-4000-8000-0000000a0008", "qty": 80}]},
)
ok(st == 400 and body.get("code") == 66051, "超退被拦（66051 可退只剩 70）", f"{st} {body}")
ok("可退只剩 70" in str(body.get("msg") or ""), "报文点明可退 70", str(body.get("msg"))[:120])

print("== 5. 验收④：退满剩余 70 → 应付归零 ==")
st, body = req(
    "POST", "/business/erp/purchase-return", token=admin,
    body={"stock_in_id": "30000000-0000-4000-8000-0000000a0007", "reason": "剩余全部退货",
          "items": [{"stock_in_item_id": "30000000-0000-4000-8000-0000000a0008", "qty": 70}]},
)
rid2 = ((body.get("data") or {}).get("purchase_return") or {}).get("id")
no2 = ((body.get("data") or {}).get("purchase_return") or {}).get("return_no")
ok(st == 200 and rid2, "建第二张退货单 70", f"{st} {body}")
req("POST", f"/business/erp/purchase-return/{rid2}/submit", token=admin, body={})
approve_all(pm, no2)
approve_all(fin, no2)
st, body = req("POST", f"/business/erp/purchase-return/{rid2}/confirm", token=admin, body={})
ok(st == 200 and ((body.get("data") or {}).get("purchase_return") or {}).get("status") == "CONFIRMED",
   "第二张确认", f"{st} {body}")
ap = psql("select amount::text || '|' || unpaid_amount::text || '|' || status from biz_payable where id='30000000-0000-4000-8000-0000000a0009'")
ok(ap.startswith("0.0000|0.0000|SETTLED"), "全额退货后应付归零 / SETTLED", ap)
inv = psql("select qty::text from biz_inventory where material_id='30000000-0000-4000-8000-0000000a0001'")
ok(inv.startswith("0"), "库存归零", inv)

print("== 6. 验收⑤：已付款场景 → 生成供应商应退款 ==")
# 夹具：第二张入库单 10 个 @100（含税 1130）+ 应付已全额付清
psql(r"""
INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status,
    currency_code, total_qty, total_amount, total_tax, total_with_tax, posted_at)
  VALUES ('30000000-0000-4000-8000-0000000a0017','IN-PRT-0002','PURCHASE',
          '30000000-0000-4000-8000-0000000a0005','30000000-0000-4000-8000-0000000a0002',
          '30000000-0000-4000-8000-0000000a0003','2026-11-03','POSTED','CNY',10,1000,130,1130,now())
  ON CONFLICT DO NOTHING;
INSERT INTO biz_stock_in_item (id, stock_in_id, line_no, source_item_id, material_id, unit_id, qty, price,
    price_incl, tax_rate, amount, tax_amount, with_tax_amount, cost_price)
  VALUES ('30000000-0000-4000-8000-0000000a0018','30000000-0000-4000-8000-0000000a0017',1,
          '30000000-0000-4000-8000-0000000a0006','30000000-0000-4000-8000-0000000a0001',
          '30000000-0000-4000-8000-0000000a0004',10,100,113,13,1000,130,1130,100)
  ON CONFLICT DO NOTHING;
INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no, amount,
    paid_amount, unpaid_amount, credit_days, due_date, period_code, status, settled_at)
  VALUES ('30000000-0000-4000-8000-0000000a0019','AP-PRT-0002','30000000-0000-4000-8000-0000000a0002',
          'STOCK_IN','30000000-0000-4000-8000-0000000a0017','IN-PRT-0002',1130,1130,0,30,'2026-12-03','2026-11','SETTLED',now())
  ON CONFLICT DO NOTHING;
UPDATE biz_inventory SET qty = 10, amount = 1000, avg_cost = 100
 WHERE material_id='30000000-0000-4000-8000-0000000a0001';
UPDATE biz_inventory_location SET qty = 10 WHERE material_id='30000000-0000-4000-8000-0000000a0001';
""")
st, body = req(
    "POST", "/business/erp/purchase-return", token=admin,
    body={"stock_in_id": "30000000-0000-4000-8000-0000000a0017", "reason": "已付款后部分退货",
          "items": [{"stock_in_item_id": "30000000-0000-4000-8000-0000000a0018", "qty": 3}]},
)
r3 = ((body.get("data") or {}).get("purchase_return") or {})
ok(st == 200 and r3.get("id"), "建已付款场景退货单 3 个", f"{st} {body}")
req("POST", f"/business/erp/purchase-return/{r3.get('id')}/submit", token=admin, body={})
approve_all(pm, r3.get("return_no"))
approve_all(fin, r3.get("return_no"))
st, body = req("POST", f"/business/erp/purchase-return/{r3.get('id')}/confirm", token=admin, body={})
row3 = (body.get("data") or {}).get("purchase_return") or {}
ok(st == 200 and row3.get("supplier_receivable_no"), "确认后生成供应商应退款", f"{st} {body}")
ap2 = psql("select amount::text || '|' || paid_amount::text || '|' || unpaid_amount::text || '|' || status from biz_payable where id='30000000-0000-4000-8000-0000000a0019'")
ok(ap2.startswith("1130.0000|1130.0000|0.0000|SETTLED"), "已付款应付不被减到 paid 以下", ap2)
sr = psql("select count(*) from biz_supplier_receivable where source_type='PURCHASE_RETURN'")
sr2 = psql("select amount::text || '|' || status || '|' || source_no from biz_supplier_receivable where source_type='PURCHASE_RETURN'")
ok(sr2.startswith("339"), "供应商应退款 339", sr2)
bal2 = psql(
    "select (sum(debit_amount)=sum(credit_amount))::text || '|' || sum(debit_amount)::text from biz_voucher_entry e "
    "join biz_voucher v on v.id=e.voucher_id where v.id = "
    "(select voucher_id from biz_stock_out where source_id = (select id from biz_purchase_return where id = "
    "(select return_id from biz_purchase_return_item where stock_in_item_id='30000000-0000-4000-8000-0000000a0018' limit 1)))"
)
ok(bal2.startswith("true|339"), "已付款场景凭证同样平衡（339）", bal2)

print("== 7. 状态闸：重复确认必须被拦 ==")
st, body = req("POST", f"/business/erp/purchase-return/{r3.get('id')}/confirm", token=admin, body={})
ok(st == 409 and body.get("code") == 66052, "重复确认 409/66052", f"{st} {body}")

print(f"\n==> 汇总：PASS={PASS} FAIL={FAIL}")
if FAILS:
    for f in FAILS:
        print("   FAIL:", f)
print("ROWS:", psql("select count(*) from biz_purchase_return"), "returns,",
      psql("select count(*) from biz_stock_out where source_type='PURCHASE_RETURN'"), "stock_outs")
