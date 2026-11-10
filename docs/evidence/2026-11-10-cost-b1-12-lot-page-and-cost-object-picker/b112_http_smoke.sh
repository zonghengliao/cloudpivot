#!/usr/bin/env bash
# COST-B1-12 HTTP 冒烟：批次列表按物料筛 + 领料单成本对象下拉只列 OPEN
#
# 依赖：本机已起 backend（BASE）且在 e2e 库（DB）上跑过 b112_fixtures.sql。
# admin 首启带 must_change_password=true → 本脚本顺手改密（第二次起用新口令，两个都试）。
#
# 用法：BASE=http://127.0.0.1:18032 DB=b112e2e bash b112_http_smoke.sh
set -uo pipefail

BASE="${BASE:-http://127.0.0.1:18032}"
DB="${DB:-b112e2e}"
API="$BASE/api"
NEW_PW='Admin@123456b112'
MAT='b1120000-0000-4000-8000-000000000011'
MAT_P='b1120000-0000-4000-8000-000000000021'

py() { python3 -c "$1"; }
login_with() {
  curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$1\"}"
}

echo "1) 登录 admin"
LOGIN=$(login_with 'Admin@123456')
CODE=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["code"])')
if [ "$CODE" != "0" ]; then
  LOGIN=$(login_with "$NEW_PW")
fi
TOKEN=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin).get("data",{}).get("access_token",""))')
MUST=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin).get("data",{}).get("must_change_password"))')
echo "   token 长度 ${#TOKEN}  must_change_password=$MUST"
[ -z "$TOKEN" ] && { echo "登录失败：$LOGIN"; exit 1; }
if [ "$MUST" = "True" ]; then
  curl -s -X PUT "$API/system/auth/password" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"Admin@123456\",\"new_password\":\"$NEW_PW\"}" >/dev/null
  LOGIN=$(login_with "$NEW_PW")
  TOKEN=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["data"]["access_token"])')
  echo "   已改密并重新登录，token 长度 ${#TOKEN}"
fi
AUTH="Authorization: Bearer $TOKEN"

echo
echo "2) 批次列表 —— 不带筛选（应含 3 条夹具批）"
curl -s "$API/business/erp/lot?page=1&page_size=50" -H "$AUTH" | py '
import sys,json
d=json.load(sys.stdin); rows=d["data"]["list"]
print("   code=%s total=%s 返回=%s" % (d["code"], d["data"]["total"], len(rows)))
for r in rows:
    if r["lot_no"].startswith("LOT-B112"):
        print("   · %s | %s %s | %s | qc=%s | status=%s" % (r["lot_no"], r["material_code"], r["material_name"], r["source_type"], r["qc_status"], r["status"]))
'

echo
echo "3) 批次列表 —— 按物料筛（本卡验收①）material_id=$MAT"
curl -s "$API/business/erp/lot?material_id=$MAT&page=1&page_size=50" -H "$AUTH" | py '
import sys,json
d=json.load(sys.stdin); rows=d["data"]["list"]
print("   code=%s total=%s" % (d["code"], d["data"]["total"]))
assert d["code"]==0, d
assert d["data"]["total"]==2, "该物料下应恰好 2 条批（B112-M1 的两条）"
for r in rows:
    assert r["material_name"]=="B112 钢板", r
    print("   · %s | %s | %s | qc=%s" % (r["lot_no"], r["material_code"], r["source_type"], r["qc_status"]))
print("   ✅ 按物料筛生效（只返回该物料的 2 条）")
'

echo
echo "4) 成本对象下拉 —— 出库表单数据源（本卡验收②：只列 OPEN）"
curl -s "$API/business/erp/stock-out/cost-object-options" -H "$AUTH" | py '
import sys,json
d=json.load(sys.stdin); rows=d["data"]
print("   code=%s 返回=%s" % (d["code"], len(rows)))
assert d["code"]==0, d
ids=[r["object_no"] for r in rows]
assert "CB-B112-000001" in ids, "OPEN 对象必须在下拉里：%s" % ids
assert "CB-B112-000002" not in ids, "CLOSED 对象不许出现：%s" % ids
hit=[r for r in rows if r["object_no"]=="CB-B112-000001"][0]
print("   · %s | 产品 %s %s | 计划 %s | 开工 %s" % (hit["object_no"], hit["product_code"], hit["product_name"], hit["plan_qty"], hit["start_date"]))
print("   ✅ 下拉只列 OPEN（CLOSED 被排除，产品名 join 出来）")
'

echo
echo "5) 下拉按关键字筛（对象号 / 产品名）"
for kw in 'CB-B112-000001' '产成品' 'CB-B112-000002'; do
  curl -s --get "$API/business/erp/stock-out/cost-object-options" --data-urlencode "keyword=$kw" -H "$AUTH" | py "
import sys,json
d=json.load(sys.stdin)
print('   keyword=%s → %s 条：%s' % ('$kw', len(d['data']), [r['object_no'] for r in d['data']]))
"
done

echo
echo "6) 建一张挂对象的领用单（借方应为 4001.01），再建一张不挂的（6401）"
mk() {
  curl -s -X POST "$API/business/erp/stock-out" -H "$AUTH" -H 'Content-Type: application/json' -d "$1"
}
P1=$(mk "{\"out_type\":\"ISSUE\",\"warehouse_id\":\"b1120000-0000-4000-8000-0000000000f1\",\"out_date\":\"2026-11-10\",\"cost_object_id\":\"b1120000-0000-4000-8000-0000000000a1\",\"reason\":\"B1-12 冒烟：挂对象\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":\"5\",\"lot_no\":\"LOT-B112-A\"}]}")
P2=$(mk "{\"out_type\":\"ISSUE\",\"warehouse_id\":\"b1120000-0000-4000-8000-0000000000f1\",\"out_date\":\"2026-11-10\",\"reason\":\"B1-12 冒烟：不挂\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":\"1\",\"lot_no\":\"LOT-B112-A\"}]}")
for body in "$P1" "$P2"; do
  echo "$body" | py '
import sys,json
d=json.load(sys.stdin)
if d["code"]!=0:
    print("   ✗ 建单失败：", d["code"], d["msg"]); raise SystemExit(0)
h=d["data"]
print("   · %s | 对象=%s | 借方=%s %s" % (h["out_no"], h["cost_object_no"], h["debit_account_code"], h["debit_account_name"]))
'
done
echo "$P1" | py '
import sys,json
d=json.load(sys.stdin); h=d.get("data") or {}
assert h.get("debit_account_code")=="4001.01", "挂对象必须借 4001.01：%s" % h.get("debit_account_code")
print("   ✅ 挂对象 ⇒ 借 4001.01 生产成本-直接材料")
'
echo "$P2" | py '
import sys,json
d=json.load(sys.stdin); h=d.get("data") or {}
assert h.get("debit_account_code")=="6401", "不挂必须借 6401：%s" % h.get("debit_account_code")
print("   ✅ 不挂 ⇒ 借 6401 主营业务成本（存量口径不变）")
'

echo
echo "7) 挂一个已关批的对象 ⇒ 409 + 66057（下拉里选不到，但接口要兜住）"
mk "{\"out_type\":\"ISSUE\",\"warehouse_id\":\"b1120000-0000-4000-8000-0000000000f1\",\"out_date\":\"2026-11-10\",\"cost_object_id\":\"b1120000-0000-4000-8000-0000000000a2\",\"reason\":\"B1-12 冒烟：关批对象\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":\"1\"}]}" | py '
import sys,json
d=json.load(sys.stdin)
print("   code=%s msg=%s" % (d["code"], (d["msg"] or "")[:80]))
assert d["code"]==66057, d
print("   ✅ 非 OPEN 对象被拦（66057）")
'

echo
echo "8) 鉴权：无 token 访问下拉 ⇒ 401"
curl -s -o /dev/null -w "   HTTP %{http_code}\n" "$API/business/erp/stock-out/cost-object-options"
curl -s -o /dev/null -w "   批次列表 HTTP %{http_code}\n" "$API/business/erp/lot?page=1"
echo "✅ 冒烟结束"
