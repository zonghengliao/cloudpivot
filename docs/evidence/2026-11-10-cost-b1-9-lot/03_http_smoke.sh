#!/usr/bin/env bash
# COST-B1-9 批次主数据 · HTTP 冒烟（真实例 + admin 真登录）
#
# 覆盖卡面验收三条：CRUD 全通 / 按物料取批号 / 唯一键冲突友好文案。
# 依赖：本机已起 backend（BASE），库里有 biz_unit 种子。
#
# 用法：BASE=http://127.0.0.1:18025 DB=costb19_http2 bash http_smoke_b19_v3.sh
set -uo pipefail

BASE="${BASE:-http://127.0.0.1:18025}"
DB="${DB:-costb19_http2}"
API="$BASE/api"
SUF="$(date +%s)$RANDOM"
NEW_PW='Admin@123456b19'
SCRATCH=/root/.hermes/profiles/backend-developer/cache/scratch

py() { python3 -c "$1"; }

echo "1) 登录 admin"
# 空库首启的 admin 带 must_change_password=true；本脚本第一次跑会顺手改密到 $NEW_PW，
# 所以第二次起要用新口令登录 ⇒ 两个口令都试（以真实响应码为准，不猜）。
login_with() {
  curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$1\"}"
}
LOGIN=$(login_with 'Admin@123456')
CODE=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["code"])')
if [ "$CODE" != "0" ]; then
  LOGIN=$(login_with "$NEW_PW")
  CODE=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["code"])')
fi
TOKEN=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["data"]["access_token"])')
MUST=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["data"]["must_change_password"])')
echo "   login code=$CODE token 长度 ${#TOKEN}  must_change_password=$MUST"
if [ -z "$TOKEN" ]; then echo "登录失败：$LOGIN"; exit 1; fi

if [ "$MUST" = "True" ]; then
  echo "   首次登录必须先改密：PUT /system/auth/password"
  CHANGED=$(curl -s -X PUT "$API/system/auth/password" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"Admin@123456\",\"new_password\":\"$NEW_PW\"}")
  echo "   改密结果 code=$(echo "$CHANGED" | py 'import sys,json;print(json.load(sys.stdin)["code"])')"
  LOGIN=$(login_with "$NEW_PW")
  echo "   重新登录 must_change_password=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["data"]["must_change_password"])')"
  TOKEN=$(echo "$LOGIN" | py 'import sys,json;print(json.load(sys.stdin)["data"]["access_token"])')
  echo "   新 token 长度 ${#TOKEN}"
fi
AUTH="Authorization: Bearer $TOKEN"

# ---------- 夹具：直接插一条物料（批次只关心 material 存在） ----------
echo "2) 造夹具物料（psql 直插，编码前缀 COSTB19-$SUF）"
MAT=$(python3 -c 'import uuid;print(uuid.uuid4())')
UNIT=$(su - postgres -c "psql -d $DB -Atc 'select id from biz_unit where deleted_at is null limit 1'")
su - postgres -c "psql -d $DB -q -c \"insert into biz_material (id, material_code, material_name, unit_id, material_type, status, serial_required, created_at, updated_at) values ('$MAT', 'COSTB19-$SUF', '批次冒烟物料 $SUF', '$UNIT', 'RAW', 1, false, now(), now())\""
echo "   material=$MAT  unit=$UNIT"

LOT="COSTB19-LOT-$SUF"
echo "3) POST /business/erp/lot（新建）"
CREATE=$(curl -s -X POST "$API/business/erp/lot" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"lot_no\":\"$LOT\",\"material_id\":\"$MAT\",\"source_type\":\"PURCHASE\",\"source_doc_type\":\"STOCK_IN\",\"source_doc_no\":\"SI-$SUF\",\"prod_date\":\"2026-11-01\",\"expire_date\":\"2027-11-01\",\"qc_status\":\"PASSED\",\"remark\":\"B1-9 冒烟\"}")
echo "$CREATE" | py 'import sys,json;d=json.load(sys.stdin);r=d["data"];print("   code=%s lot_no=%s status=%s qc=%s material_name=%s material_code=%s prod=%s expire=%s"%(d["code"],r["lot_no"],r["status"],r["qc_status"],r["material_name"],r["material_code"],r["prod_date"],r["expire_date"]))'
LOT_ID=$(echo "$CREATE" | py 'import sys,json;print(json.load(sys.stdin)["data"]["id"])')
echo "   lot id = $LOT_ID"

echo "4) GET /business/erp/lot（列表，按物料 + 关键字）"
LIST=$(curl -s -G "$API/business/erp/lot" --data-urlencode "keyword=$LOT" --data-urlencode "material_id=$MAT" -H "$AUTH")
echo "$LIST" | py 'import sys,json;d=json.load(sys.stdin);rows=d["data"]["list"];print("   code=%s total=%s page=%s page_size=%s 首行=%s"%(d["code"],d["data"]["total"],d["data"]["page"],d["data"]["page_size"],rows[0]["lot_no"] if rows else "-"))'

echo "5) GET /business/erp/lot/options?material_id=（按物料取批号）"
OPT=$(curl -s -G "$API/business/erp/lot/options" --data-urlencode "material_id=$MAT" -H "$AUTH")
echo "$OPT" | py 'import sys,json;d=json.load(sys.stdin);rows=d["data"];print("   code=%s 条数=%s 批号=%s 质检=%s"%(d["code"],len(rows),[r["lot_no"] for r in rows],[r["qc_status"] for r in rows]))'

echo "6) PUT /business/erp/lot/{id}（改批号 + 质检 + 清空失效日期 + 停用）"
UPD=$(curl -s -X PUT "$API/business/erp/lot/$LOT_ID" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"lot_no\":\"$LOT-2\",\"qc_status\":\"REJECTED\",\"expire_date\":null,\"status\":0,\"remark\":\"B1-9 冒烟改单\"}")
echo "$UPD" | py 'import sys,json;d=json.load(sys.stdin);r=d["data"];print("   code=%s lot_no=%s qc=%s expire=%s status=%s remark=%s"%(d["code"],r["lot_no"],r["qc_status"],r["expire_date"],r["status"],r["remark"]))'

echo "7) GET /lot/options（停用后应从下拉消失）"
OPT2=$(curl -s -G "$API/business/erp/lot/options" --data-urlencode "material_id=$MAT" -H "$AUTH")
echo "$OPT2" | py 'import sys,json;d=json.load(sys.stdin);print("   code=%s 条数=%s"%(d["code"],len(d["data"])))'

echo "8) POST /lot（同物料同批号 ⇒ 应 409 + 66056 友好文案）"
DUP_FILE="$SCRATCH/b19_dup.json"
curl -s -o "$DUP_FILE" -w "   HTTP %{http_code}\n" -X POST "$API/business/erp/lot" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"lot_no\":\"$LOT-2\",\"material_id\":\"$MAT\"}"
py "import json;d=json.load(open('$DUP_FILE'));print('   code=%s msg=%s'%(d['code'],d['msg']))"

echo "9) POST /lot（物料不存在 ⇒ 应 400 指名）"
BAD_FILE="$SCRATCH/b19_bad.json"
curl -s -o "$BAD_FILE" -w "   HTTP %{http_code}\n" -X POST "$API/business/erp/lot" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"lot_no\":\"$LOT-X\",\"material_id\":\"$(python3 -c 'import uuid;print(uuid.uuid4())')\"}"
py "import json;d=json.load(open('$BAD_FILE'));print('   code=%s msg=%s'%(d['code'],d['msg']))"

echo "10) 未鉴权 / 未知路径（证明路由真挂上了，不是文件写了没挂）"
curl -s -o /dev/null -w "   GET  /lot 无 token => %{http_code}\n" "$API/business/erp/lot"
curl -s -o /dev/null -w "   GET  /lot/options 无 token => %{http_code}\n" "$API/business/erp/lot/options"
curl -s -o /dev/null -w "   POST /lot 无 token => %{http_code}\n" -X POST "$API/business/erp/lot" -H 'Content-Type: application/json' -d '{}'
curl -s -o /dev/null -w "   PUT  /lot/{id} 无 token => %{http_code}\n" -X PUT "$API/business/erp/lot/$LOT_ID" -H 'Content-Type: application/json' -d '{}'
curl -s -o /dev/null -w "   GET  /business/erp/lot-not-exist => %{http_code}（404 = 没有被通配吞掉）\n" "$API/business/erp/lot-not-exist"

echo "11) 清理冒烟数据"
su - postgres -c "psql -d $DB -q -c \"delete from biz_lot where material_id = '$MAT'\""
su - postgres -c "psql -d $DB -q -c \"delete from biz_material where id = '$MAT'\""
su - postgres -c "psql -d $DB -Atc \"select count(*) from biz_lot where lot_no like 'COSTB19-LOT-%'\"" | sed 's/^/   残留批次数=/'
