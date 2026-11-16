"""M2-07b 真接口验收脚本（在 99 上跑，打本机隔离实例 127.0.0.1:18207）。

只做 HTTP：登录 → 改密（空库首启必须）→ 对账接口全量 / include_matched / 筛选 /
非法参数 / 未授权。断言写死在脚本里，输出 PASS/FAIL 逐条打印，末尾汇总。
"""
import json
import os
import urllib.error
import urllib.request

# 默认打本机隔离实例（99 上的 `backend/target/debug/cloudpivot`，NIK_SERVER_LISTEN=127.0.0.1:18207）
BASE = os.environ.get("M207B_API_BASE", "http://127.0.0.1:18207/api")
OLD_PASSWORD = os.environ.get("M207B_ADMIN_OLD_PASSWORD", "Admin@123456")
NEW_PASSWORD = os.environ.get("M207B_ADMIN_PASSWORD", "M207b@Reconcile1")

result = {"pass": 0, "fail": 0}


def check(name, ok, detail=""):
    if ok:
        result["pass"] += 1
        print(f"  PASS {name}")
    else:
        result["fail"] += 1
        print(f"  FAIL {name} {detail}")


def call(method, path, body=None, token=None, expect_json=True):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if expect_json and raw else raw)
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw)
        except json.JSONDecodeError:
            return err.code, raw


def login(username, password):
    status, body = call("POST", "/system/auth/login", {"username": username, "password": password})
    assert status == 200, f"登录失败 {status} {body}"
    return body["data"]


print("== 登录（空库首启 admin 必须先改密）==")
first_boot = True
try:
    sess = login("admin", OLD_PASSWORD)
except AssertionError:
    # 第二次跑：admin 已是新口令（脚本可重跑）
    first_boot = False
    sess = login("admin", NEW_PASSWORD)
token = sess.get("access_token") or sess.get("token")
check("登录 admin", bool(token))
if first_boot:
    check("空库首启 must_change_password=true", bool(sess.get("must_change_password")))

    status, body = call(
        "PUT",
        "/system/auth/password",
        {"old_password": OLD_PASSWORD, "new_password": NEW_PASSWORD},
        token,
    )
    check("改密 200", status == 200, f"{status} {body}")
    sess = login("admin", NEW_PASSWORD)
    token = sess.get("access_token") or sess.get("token")
    check("重新登录后 must_change_password=false", sess.get("must_change_password") is False)
else:
    print("  SKIP 改密（admin 已是新口令，脚本可重跑）")

print("== 菜单树 / 权限：在途页 + 对账码 ==")
status, body = call("GET", "/system/auth/menus", token=token)
menus = json.dumps(body, ensure_ascii=False)
check("菜单树 200", status == 200, str(status))
check("菜单树含 /business/erp/inventory-transit", "/business/erp/inventory-transit" in menus)
check("菜单页面行 auth_code = :page", "business:erp:inventory-transit:page" in menus)
status, me = call("GET", "/system/auth/me", token=token)
perms = me["data"]["permissions"] if isinstance(me, dict) else []
check("超管权限走 *:*:* 通配（前端 hasPermission 短路）", "*:*:*" in perms, str(perms)[:120])

print("== 对账接口：缺省只给差异组 ==")
status, body = call("GET", "/business/erp/inventory-transit/reconcile", token=token)
check("对账 200", status == 200, str(status))
data = body.get("data") if isinstance(body, dict) else None
check("出参带 data", isinstance(data, dict), str(body)[:200])
if isinstance(data, dict):
    rows = data["rows"]
    totals = data["totals"]
    by_no = {}
    for row in rows:
        by_no.setdefault(row["ship_no"], []).append(row)
    print(f"    checked={data['checked']} matched={data['matched']} mismatched={data['mismatched']} "
          f"balanced={data['balanced']} truncated={data['truncated']} rows={len(rows)}")
    print(f"    totals={json.dumps(totals, ensure_ascii=False)}")
    for row in rows:
        print(f"    {row['ship_no']} {row['material_code']} wh={row['warehouse_code']} "
              f"item={row['item_qty']} recv={row['received_qty']} transit={row['qty_in_transit']} "
              f"diff={row['diff']} issues={row['issues']} labels={row['issue_labels']}")
    check("缺省 5 行差异组（6 组里 1 组对平被滤掉）", len(rows) == 5, f"实际 {len(rows)}")
    check("checked=6", data["checked"] == 6, str(data["checked"]))
    check("matched=1", data["matched"] == 1, str(data["matched"]))
    check("mismatched=4", data["mismatched"] == 4, str(data["mismatched"]))
    check("cancelled=1", totals["cancelled"] == 1, str(totals["cancelled"]))
    check("untracked=1", totals["untracked"] == 1, str(totals["untracked"]))
    check("balanced=false", data["balanced"] is False)
    check("truncated=false", data["truncated"] is False)
    check("scan_limit=2000", data["scan_limit"] == 2000, str(data["scan_limit"]))
    check("对平组（M207B-SH1 螺丝）不在缺省结果里", "M207B-SH1" not in by_no)
    check("QTY_MISMATCH 行：M207B-SH2 diff=-5", by_no.get("M207B-SH2", [{}])[0].get("diff") == "-5.0000",
          str(by_no.get("M207B-SH2")))
    check("MISSING_TRANSIT 行：M207B-SH3 没有仓库", by_no.get("M207B-SH3", [{}])[0].get("warehouse_id") is None)
    check("CANCELLED_ONLY 行：M207B-SH4 cancelled_qty=10",
          by_no.get("M207B-SH4", [{}])[0].get("cancelled_qty") == "10.0000")
    check("ORPHAN_TRANSIT 行：M207B-SH5 item_qty=0",
          float(by_no.get("M207B-SH5", [{}])[0].get("item_qty", "x")) == 0.0,
          str(by_no.get("M207B-SH5", [{}])[0].get("item_qty")))
    check("MULTI_WAREHOUSE 行：M207B-SH6 两个码", by_no.get("M207B-SH6", [{}])[0].get("issues") == ["MULTI_WAREHOUSE"],
          str(by_no.get("M207B-SH6")))
    check("差异码中文由后端给（非空 issue_labels）",
          all(row["issue_labels"] for row in rows), "有空标签")
    check("totals.item_qty=180（只算已纳入在途账的组）", totals["item_qty"] == "180.0000", totals["item_qty"])
    check("totals.untracked_item_qty=20", totals["untracked_item_qty"] == "20.0000", totals["untracked_item_qty"])
    check("totals.diff=-12（后端算，前端不重算）", totals["diff"] == "-12.0000", totals["diff"])

    print("== include_matched=true：连对平的组一起给 ==")
    status, body2 = call("GET", "/business/erp/inventory-transit/reconcile?include_matched=true", token=token)
    data2 = body2.get("data") if isinstance(body2, dict) else None
    check("include_matched 200", status == 200, str(status))
    check("include_matched=1 出 6 行", len(data2["rows"]) == 6, str(len(data2["rows"])))
    check("include_matched 里出现对平组 SH1",
          any(r["ship_no"] == "M207B-SH1" and r["matched"] is True for r in data2["rows"]))
    check("include_matched 下 totals 不变（口径不随返回集合变）", data2["totals"] == totals)

    print("== 筛选 ==")
    status, body3 = call("GET", "/business/erp/inventory-transit/reconcile?shipment_id=" + by_no["M207B-SH2"][0]["shipment_id"], token=token)
    data3 = body3.get("data") if isinstance(body3, dict) else None
    check("按 shipment_id 筛 → 1 行", len(data3["rows"]) == 1, str(len(data3["rows"])))
    check("按 shipment_id 筛只覆盖该单 checked=1", data3["checked"] == 1, str(data3["checked"]))

    status, body4 = call(
        "GET",
        "/business/erp/inventory-transit/reconcile?ship_date_from=2000-01-01&ship_date_to=2000-01-31",
        token=token,
    )
    data4 = body4.get("data") if isinstance(body4, dict) else None
    check("日期区间外 → 0 组（不是报错）", status == 200 and data4["rows"] == [] and data4["checked"] == 0,
          f"{status} {str(data4)[:120]}")

    print("== 非法参数 ==")
    for query, label in [
        ("include_matched=maybe", "include_matched 非布尔"),
        ("shipment_id=not-a-uuid", "shipment_id 非 UUID"),
        ("ship_date_from=2026-13-01", "日期非法"),
        ("ship_date_from=2026-11-30&ship_date_to=2026-11-01", "区间反了"),
    ]:
        status, body5 = call("GET", "/business/erp/inventory-transit/reconcile?" + query, token=token)
        code = body5.get("code") if isinstance(body5, dict) else None
        check(f"非法参数 400+66006：{label}", status == 400 and code == 66006, f"{status} {str(body5)[:160]}")

print("== 未授权访问 ==")
status, _ = call("GET", "/business/erp/inventory-transit/reconcile", expect_json=False)
check("匿名调对账 → 401", status == 401, str(status))

print("== 只读角色（有页面码 + 取数码、无对账码）：页面能开、对账 403 ==")
sess2 = login("m207b_noaudit", NEW_PASSWORD)
token2 = sess2.get("access_token") or sess2.get("token")
status, me2 = call("GET", "/system/auth/me", token=token2)
perms2 = me2["data"]["permissions"] if isinstance(me2, dict) else []
check("只读角色登录 200", status == 200, str(status))
check("只读角色持 :page", "business:erp:inventory-transit:page" in perms2, str(perms2))
check("只读角色持 :list", "business:erp:inventory-transit:list" in perms2, str(perms2))
check("只读角色**不**持 :audit（前端据此隐藏第三个 Tab）",
      "business:erp:inventory-transit:audit" not in perms2, str(perms2))
status, body_page = call("GET", "/business/erp/inventory-transit/page", token=token2)
check("只读角色调明细 200（页面照常能开，不是整页 403）", status == 200, str(status))
status, body_reconcile = call("GET", "/business/erp/inventory-transit/reconcile", token=token2)
check("只读角色调对账 → 403（对账是独立授权面）", status == 403, f"{status} {str(body_reconcile)[:160]}")

print(f"\n═══ PASS={result['pass']} FAIL={result['fail']} ═══")
raise SystemExit(0 if result["fail"] == 0 else 1)
