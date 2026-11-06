#!/usr/bin/env bash
# SO-35 销售员字段的真实 curl 留证（真实 token，库 cloudpivot_so35 @ :18242）
#
#   bash scripts/verify/so35_instance.sh up
#   bash scripts/verify/so35_curl_evidence.sh | tee docs/evidence/20261107-so35-salesperson/curl_transcript.txt
#
# 覆盖：① 登录 → ② 取真实销售员（sys_user 列表）→ ③ 建单挂销售员（回显 id + 姓名）
#       ④ 详情回读 → ⑤ 分页列表补名 → ⑥ 改单解绑（空串）→ ⑦ 重新挂上
#       ⑧ 不存在的销售员 → 400 → ⑨ 无 token → 401
set -uo pipefail
BASE=${SO35_BASE:-http://127.0.0.1:18242}
# 夹具客户 / 物料沿用 SO-34 隔离库里已有的行（模板克隆自带）
C1=${SO35_CUSTOMER:-f0330000-0000-4000-8000-000000000001}
M4=${SO35_MATERIAL:-f0330000-0000-4000-8000-000000000014}

say() { printf '\n### %s\n' "$1"; }

say "① 登录拿 token（admin / SUPER_ADMIN 短路权限）"
TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["access_token"])')
echo "token 长度 = ${#TOKEN}"

say "② 取一个真实启用的销售员（GET /api/system/user/page，取 admin 自己的 id / 姓名）"
USER_JSON=$(curl -s "$BASE/api/system/user/page?page=1&page_size=1" -H "Authorization: Bearer $TOKEN")
echo "$USER_JSON" | python3 -c 'import json,sys; d=json.load(sys.stdin)["data"]["list"][0]; print({"id":d["id"],"username":d["username"],"real_name":d.get("real_name")})'
SALES_ID=$(echo "$USER_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["list"][0]["id"])')
echo "salesperson_id = $SALES_ID"

say "③ 建单挂销售员（POST /api/business/erp/sales-order，salesperson_id=$SALES_ID）"
SO_JSON=$(curl -s -X POST "$BASE/api/business/erp/sales-order" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$C1\",\"salesperson_id\":\"$SALES_ID\",\"order_date\":\"2026-11-07\",\"remark\":\"SO-35 curl 留证\",\"items\":[{\"material_id\":\"$M4\",\"qty\":\"2\",\"price\":\"6.6600\",\"tax_rate\":\"13\"}]}")
echo "$SO_JSON" | python3 -c 'import json,sys; d=json.load(sys.stdin)["data"]; print({"so_no":d["so_no"],"salesperson_id":d["salesperson_id"],"salesperson_name":d["salesperson_name"],"total_with_tax":d["total_with_tax"]})'
SO_ID=$(echo "$SO_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["id"])')
echo "订单 id = $SO_ID"

say "④ 详情回读（GET /api/business/erp/sales-order/$SO_ID）"
curl -s "$BASE/api/business/erp/sales-order/$SO_ID" -H "Authorization: Bearer $TOKEN" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin)["data"]; print({"so_no":d["so_no"],"salesperson_id":d["salesperson_id"],"salesperson_name":d["salesperson_name"]})'

say "⑤ 分页列表补名（GET /api/business/erp/sales-order/page?so_no=…）"
SO_NO=$(echo "$SO_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["so_no"])')
curl -s "$BASE/api/business/erp/sales-order/page?so_no=$SO_NO&page=1&page_size=5" -H "Authorization: Bearer $TOKEN" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin)["data"]; print({"total":d["total"],"salesperson_id":d["list"][0]["salesperson_id"],"salesperson_name":d["list"][0]["salesperson_name"]})'

say "⑥ 改单解绑（PUT，salesperson_id = 空串 → NULL）"
curl -s -X PUT "$BASE/api/business/erp/sales-order/$SO_ID" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"salesperson_id":""}' \
  | python3 -c 'import json,sys; d=json.load(sys.stdin)["data"]; print({"salesperson_id":d["salesperson_id"],"salesperson_name":d["salesperson_name"]})'

say "⑦ 重新挂上（PUT，salesperson_id = $SALES_ID）"
curl -s -X PUT "$BASE/api/business/erp/sales-order/$SO_ID" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"salesperson_id\":\"$SALES_ID\"}" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin)["data"]; print({"salesperson_id":d["salesperson_id"],"salesperson_name":d["salesperson_name"]})'

say "⑧ 销售员不存在 → 400（不是 500，也不是静默落 NULL）"
BAD=$(python3 -c 'import uuid; print(uuid.uuid4())')
curl -s -o /tmp/so35_bad.json -w 'HTTP %{http_code}\n' -X POST "$BASE/api/business/erp/sales-order" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$C1\",\"salesperson_id\":\"$BAD\",\"order_date\":\"2026-11-07\",\"items\":[{\"material_id\":\"$M4\",\"qty\":\"1\",\"price\":\"1\",\"tax_rate\":\"0\"}]}"
cat /tmp/so35_bad.json | python3 -m json.tool | head -8

say "⑨ 销售员传乱码 → 400"
curl -s -o /tmp/so35_garbage.json -w 'HTTP %{http_code}\n' -X POST "$BASE/api/business/erp/sales-order" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$C1\",\"salesperson_id\":\"not-a-uuid\",\"order_date\":\"2026-11-07\",\"items\":[{\"material_id\":\"$M4\",\"qty\":\"1\",\"price\":\"1\",\"tax_rate\":\"0\"}]}"
cat /tmp/so35_garbage.json | python3 -m json.tool | head -8

say "⑩ 无 token → 401（权限不是只靠前端隐藏按钮）"
curl -s -o /dev/null -w 'HTTP %{http_code}\n' "$BASE/api/business/erp/sales-order/page?page=1&page_size=1"
