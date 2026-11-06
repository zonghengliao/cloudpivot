#!/usr/bin/env bash
# SO-34 价格维护接口的真实 curl 留证（真实 token，库 cloudpivot_so34 @ :18241）
#
#   bash scripts/verify/so34_instance.sh up
#   bash scripts/verify/so34_curl_evidence.sh | tee docs/evidence/20261107-so34-price-ui/curl_transcript.txt
#
# 与 so34_price_admin_e2e.py 的分工：python 那份跑 46 条断言（含订单带价闭环），
# 这份是**人手可复制的 curl 原文**（含请求体与响应体），用于事后复核"接口到底长什么样"。
set -uo pipefail
BASE=${SO34_BASE:-http://127.0.0.1:18241}
C1=f0330000-0000-4000-8000-000000000001
M4=f0330000-0000-4000-8000-000000000014

say() { printf '\n### %s\n' "$1"; }

say "① 登录拿 token（admin / SUPER_ADMIN 短路权限）"
TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["access_token"])')
echo "token 长度 = ${#TOKEN}"

say "② 新建客户协议价（C1 × M4，起订量 500，单价 6.6600）"
AGREED_JSON=$(curl -s -X POST "$BASE/api/business/erp/price/agreed" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$C1\",\"material_id\":\"$M4\",\"price\":\"6.6600\",\"min_qty\":\"500\",\"effective_from\":\"2026-01-01\",\"remark\":\"curl 留证\"}")
echo "$AGREED_JSON"
AGREED_ID=$(echo "$AGREED_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["id"])')
echo "新建 id = $AGREED_ID"

say "③ 分页查（按物料 M4 筛，能看到客户 / 物料名称被 LEFT JOIN 补齐）"
curl -s "$BASE/api/business/erp/price/agreed/page?material_id=$M4&page=1&page_size=5" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool | head -40

say "④ 新建通用阶梯档（M4，[100, 500)，单价 5.5500）"
TIER_JSON=$(curl -s -X POST "$BASE/api/business/erp/price/tier" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"material_id\":\"$M4\",\"tier_type\":\"TIER\",\"min_qty\":\"100\",\"max_qty\":\"500\",\"price\":\"5.5500\",\"effective_from\":\"2026-01-01\"}")
echo "$TIER_JSON"
TIER_ID=$(echo "$TIER_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["id"])')
echo "档位 id = $TIER_ID"

say "⑤ 唯一键冲突（同客户 + 物料 + 起订量 + 生效日再建一次 → 409）"
curl -s -o /dev/null -w 'HTTP %{http_code}\n' -X POST "$BASE/api/business/erp/price/agreed" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$C1\",\"material_id\":\"$M4\",\"price\":\"1.0000\",\"min_qty\":\"500\",\"effective_from\":\"2026-01-01\"}"

say "⑥ 取价（C1 × M4，qty=600 → 命中协议价 6.6600）"
curl -s "$BASE/api/business/erp/price/resolve?customer_id=$C1&material_id=$M4&qty=600&date=2026-06-01" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

say "⑦ 取价（C1 × M4，qty=400 → 协议价起订量 500 不够，落通用阶梯 5.5500）"
curl -s "$BASE/api/business/erp/price/resolve?customer_id=$C1&material_id=$M4&qty=400&date=2026-06-01" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

say "⑧ 用取到的价开销售订单（600 件 × 6.6600 → 金额由服务端算）"
curl -s -X POST "$BASE/api/business/erp/sales-order" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$C1\",\"order_date\":\"2026-06-01\",\"remark\":\"SO-34 curl 留证\",\"items\":[{\"material_id\":\"$M4\",\"qty\":\"600\",\"price\":\"6.6600\",\"tax_rate\":\"13\"}]}" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin)["data"]; print({"so_no":d["so_no"],"item_price":d["items"][0]["price"],"amount":d["items"][0]["amount"],"total_with_tax":d["total_with_tax"]})'

say "⑨ 清理（软删本留证的协议价与档位）"
curl -s -o /dev/null -w 'DELETE agreed → HTTP %{http_code}\n' -X DELETE "$BASE/api/business/erp/price/agreed/$AGREED_ID" -H "Authorization: Bearer $TOKEN"
curl -s -o /dev/null -w 'DELETE tier   → HTTP %{http_code}\n' -X DELETE "$BASE/api/business/erp/price/tier/$TIER_ID" -H "Authorization: Bearer $TOKEN"

say "⑩ 无 token 调接口 → 401（权限不是只靠前端隐藏按钮）"
curl -s -o /dev/null -w 'HTTP %{http_code}\n' "$BASE/api/business/erp/price/agreed/page?page=1&page_size=1"
