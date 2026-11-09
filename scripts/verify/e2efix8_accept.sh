#!/usr/bin/env bash
# E2E-FIX-8 真机验收（145 · cloudpivot_dev · :8033 · HEAD 69fbf1e1）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="http://127.0.0.1:8033/api"
OUT=/tmp/e2efix8; mkdir -p "$OUT"
PASS=0; FAIL=0
ok(){ PASS=$((PASS+1)); echo "  [PASS] $*"; }
bad(){ FAIL=$((FAIL+1)); echo "  [FAIL] $*"; }
PSQLQ(){ psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
# req <tag> <method> <path> [body] → 打印 HTTP 码 + 响应体，存 raw
req(){
  local tag=$1 m=$2 p=$3 body=${4:-}
  local args=(-s -m 30 -o "$OUT/$tag.json" -w '%{http_code}' -X "$m" "$BASE$p" -H "Authorization: Bearer $TOKEN")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  curl "${args[@]}"
}

echo "== 0) 登录取 token"
LOGIN=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
        -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}")
TOKEN=$(echo "$LOGIN" | jq -r '.data.access_token // empty')
echo "$LOGIN" | jq -c '{code, msg, has_token: (.data.access_token != null)}'
[ -n "$TOKEN" ] && ok "登录成功（token 长度 ${#TOKEN}）" || { bad "登录失败"; echo "$LOGIN" > "$OUT/login.json"; exit 1; }

echo
echo "===== ③ 汇率预览参数（对齐 + 未知参数 400） ====="
code=$(req quote_from GET "/business/erp/exchange-rate/quote?from_currency=USD"); body=$(cat "$OUT/quote_from.json")
echo "GET ?from_currency=USD → HTTP $code"; echo "$body" | jq -c '{code, data: {from_currency: .data.from_currency, to_currency: .data.to_currency, exchange_rate: .data.exchange_rate, source: .data.source}}'
[ "$code" = "200" ] && ok "正确参数名 from_currency 被接受（HTTP 200）" || bad "正确参数名被拒（HTTP $code）"
[ "$(echo "$body" | jq -r '.data.from_currency')" = "USD" ] && ok "回显 from_currency = USD（不再静默回本位币）" || bad "from_currency 回显错误"
[ "$(echo "$body" | jq -r '.data.source')" != "BASE" ] && ok "source=$(echo "$body" | jq -r '.data.source')（不是 BASE 兜底）" || bad "仍是 BASE：参数可能又被忽略"

code=$(req quote_typo GET "/business/erp/exchange-rate/quote?from_currnecy=USD"); body=$(cat "$OUT/quote_typo.json")
echo "GET ?from_currnecy=USD（拼错）→ HTTP $code"; echo "$body" | jq -c '{code, msg}'
[ "$code" = "400" ] && ok "拼错参数 → HTTP 400" || bad "拼错参数未拦（HTTP $code）"
[ "$(echo "$body" | jq -r '.code')" = "10001" ] && ok "业务码 10001（PARAM_INVALID）" || bad "业务码不是 10001"
echo "$body" | jq -r '.msg' | grep -q "未知查询参数" && ok "报文点名「未知查询参数」" || bad "报文没点名未知参数"
echo "$body" | jq -r '.msg' | grep -q "from_currnecy" && ok "报文回显了错误参数名 from_currnecy" || bad "报文未回显错误参数名"

code=$(req quote_alias GET "/business/erp/exchange-rate/quote?currency_code=USD"); body=$(cat "$OUT/quote_alias.json")
echo "GET ?currency_code=USD（废弃别名）→ HTTP $code"; echo "$body" | jq -c '{code, from_currency: .data.from_currency, source: .data.source}'
[ "$code" = "200" ] && ok "废弃别名 currency_code 仍可用（老脚本不炸）" || bad "别名不可用（HTTP $code）"

code=$(req quote_cross GET "/business/erp/exchange-rate/quote?from_currency=USD&to_currency=HKD"); body=$(cat "$OUT/quote_cross.json")
echo "GET ?from_currency=USD&to_currency=HKD（交叉汇率）→ HTTP $code"; echo "$body" | jq -c '{code, msg}'
[ "$code" = "400" ] && ok "交叉汇率明确 400（不硬算倒数）" || bad "交叉汇率未拦（HTTP $code）"

code=$(req quote_none GET "/business/erp/exchange-rate/quote"); body=$(cat "$OUT/quote_none.json")
echo "GET 无参数 → HTTP $code"; echo "$body" | jq -c '{code, data: {from_currency: .data.from_currency, exchange_rate: .data.exchange_rate, source: .data.source}}'
[ "$code" = "200" ] && [ "$(echo "$body" | jq -r '.data.source')" = "BASE" ] && ok "无参数 = 本位币报价（source=BASE）" || bad "无参数报价异常"

echo
echo "===== ② 退款单列表带审批状态（一条 SQL，无需二次查询） ====="
code=$(req refund_page GET "/business/erp/receipt-refund/page?page=1&page_size=10"); body=$(cat "$OUT/refund_page.json")
echo "GET /receipt-refund/page → HTTP $code"
echo "$body" | jq -c '.data.list[0] | {refund_no, status, approval_instance_id, approval_instance_no, approval_status, approval_status_label}'
echo "$body" | jq -r '.data.list[0] | has("approval_status") and has("approval_instance_no") and has("approval_status_label")' | grep -q true \
  && ok "列表行含 approval_status / approval_instance_no / approval_status_label 三字段" || bad "列表行缺审批字段"
echo "$body" | jq -r '[.data.list[] | select(.approval_status != null)] | length' | { read n; [ "$n" -ge 1 ] && ok "至少 1 行带出非空审批状态（共 $n 行）" || bad "没有任何行带出审批状态"; }
echo "   DB 侧对照："
PSQLQ -c "SELECT refund_no, approval_instance_id IS NOT NULL AS has_instance FROM biz_receipt_refund WHERE deleted_at IS NULL ORDER BY created_at DESC LIMIT 3"
echo "   列表/DB 一致性："
L=$(echo "$body" | jq -r '.data.list[0].approval_instance_id // "NULL"')
P=$(echo "$body" | jq -r '.data.list[] | select(.approval_instance_no != null) | .approval_instance_no' | head -1)
echo "   列表 approval_instance_id=$L / approval_instance_no=$P"
[ -n "$P" ] && ok "列表能直接显示审批单号（$P），前端无需二次查询" || bad "列表没带出审批单号"

echo
echo "===== ⑤ 盘点单缺附件 → 闸门 can_proceed=false + 红字（自建自删草稿） ====="
WH=$(PSQLQ -c "SELECT w.id FROM biz_warehouse w WHERE w.deleted_at IS NULL AND w.status = 1 AND EXISTS (SELECT 1 FROM biz_inventory i WHERE i.warehouse_id = w.id) ORDER BY w.warehouse_code LIMIT 1")
code=$(req take_create POST "/business/erp/stock-take" "{\"warehouse_id\":\"$WH\",\"take_type\":\"FULL\",\"remark\":\"E2E-FIX-8 ⑤ 验收草稿（脚本自删）\"}")
TAKE_ID=$(jq -r '.data.id // empty' "$OUT/take_create.json")
TAKE_NO=$(jq -r '.data.take_no // empty' "$OUT/take_create.json")
echo "建草稿盘点单 $TAKE_NO（$TAKE_ID）→ HTTP $code"
if [ -n "$TAKE_ID" ]; then
  ATT=$(PSQLQ -c "SELECT count(*) FROM biz_doc_attachment WHERE doc_type='STOCK_TAKE' AND doc_id='$TAKE_ID'::uuid AND deleted_at IS NULL")
  echo "该单现有附件数 = $ATT"
  code=$(req att_check GET "/business/attachment/check?doc_type=STOCK_TAKE&doc_id=$TAKE_ID&at=CONFIRM"); body=$(cat "$OUT/att_check.json")
  echo "GET /attachment/check?doc_type=STOCK_TAKE&at=CONFIRM → HTTP $code"
  echo "$body" | jq -c '{code, can_proceed: .data.can_proceed, at_label: .data.at_label, hint: .data.hint}'
  [ "$(echo "$body" | jq -r '.data.can_proceed')" = "false" ] && ok "缺附件 → can_proceed=false（前端按钮禁用 + 红字的唯一依据）" || bad "缺附件仍放行"
  [ "$(echo "$body" | jq -r '.data.hint // empty')" != "" ] && ok "红字提示非空：$(echo "$body" | jq -r '.data.hint')" || bad "没有红字提示"
  code=$(req att_patch PATCH "/business/erp/stock-take/$TAKE_ID/status" '{"status":"PENDING_POST"}'); body=$(cat "$OUT/att_patch.json")
  echo "PATCH /status=PENDING_POST（不带附件）→ HTTP $code"; echo "$body" | jq -c '{code, msg}'
  [ "$code" = "400" ] && ok "后端闸门仍然兜底（HTTP 400，绕过前端也会被拦）" || bad "后端闸门失效（HTTP $code）"
  code=$(req take_del DELETE "/business/erp/stock-take/$TAKE_ID")
  echo "清理草稿 → HTTP $code"
  [ "$code" = "200" ] && ok "验收草稿已删除（不留夹具）" || bad "草稿清理失败（HTTP $code）"
else
  bad "建草稿盘点单失败，无法验证附件闸门"
fi

echo
echo "===== ④ 调拨不出凭证（库侧口径） ====="
echo -n "biz_voucher source_type=STOCK_TRANSFER 张数 = "; PSQLQ -c "SELECT count(*) FROM biz_voucher WHERE source_type='STOCK_TRANSFER'"
echo -n "biz_stock_transfer.voucher_id 列数 = "; PSQLQ -c "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_stock_transfer' AND column_name='voucher_id'"
echo -n "调拨流水不配对行数 = "; PSQLQ -c "SELECT count(*) FROM (SELECT doc_no, SUM(CASE WHEN biz_type='TRANSFER_OUT' THEN qty ELSE 0 END) o, SUM(CASE WHEN biz_type='TRANSFER_IN' THEN qty ELSE 0 END) i FROM biz_inventory_txn WHERE doc_type='STOCK_TRANSFER' GROUP BY doc_no) t WHERE o <> i"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = 0 ] || exit 1
