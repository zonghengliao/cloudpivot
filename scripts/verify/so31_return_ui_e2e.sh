#!/usr/bin/env bash
# =============================================================================
# SO-31 验收：退货单前端页面的**全部接口**用真实 token 走一遍（卡面「手工验证建单/提交/过账」的
# 机器版；页面点的是同一批 URL）
# =============================================================================
#   SO30_DB=cloudpivot_so31 bash scripts/verify/so30_db_setup.sh
#   SO30_DB=cloudpivot_so31 SO30_PORT=18231 SO30_LOG=/root/so31_server.log \
#   SO30_PIDF=/root/so31_server.pid bash scripts/verify/so30_instance.sh up
#   psql -d cloudpivot_so31 -f scripts/verify/so30_posting_fixture.sql
#   bash scripts/verify/so31_return_ui_e2e.sh
#
# 验什么（= 前端页面实际调用的 5 个接口 + 按状态显隐的动作）：
#   ① 列表 `GET  /sales-return/page`            分页 / 单号模糊 / 状态精确；行含
#      `approval_instance_id`（SO-31 新下发字段，FlowProgress 的读数）、客户与发货单号已补齐
#   ② 可退明细 `GET /sales-return/returnable/{shipment_id}`（新建弹窗唯一读数）
#      + 未过账发货单必须 409（前端下拉只列 POSTED 的依据）
#   ③ 建单 `POST /sales-return`（items 显式带出 = 前端行为）：DRAFT / 取号 SR- / 金额 = qty×price
#      + 超退 400（前端 validateReturnForm 的同一道闸）
#   ④ 状态流转 `PATCH /sales-return/{id}/status`：
#      DRAFT→SUBMITTED→APPROVED→PENDING_POST→POSTED 全通；
#      非法边（重复提交 / 待过账作废）409；POSTED 后回链三列非空
#   ⑤ 详情 `GET /sales-return/{id}`：明细行数 / 金额 / 状态与列表同口径
#   ⑥ 空库迁移：V0306 补齐菜单 path/component/is_visible 并归位「销售管理」
#
# 证据：docs/evidence/20261107-so31-return-ui/（响应体落盘，JWT 已脱敏）
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
PORT=${SO30_PORT:-18231}
BASE="http://127.0.0.1:$PORT/api"
DB=${SO30_DB:-cloudpivot_so31}
EVID="$ROOT/docs/evidence/20261107-so31-return-ui"
RAW="$EVID/raw"
mkdir -p "$RAW"
export PGPASSWORD=alun
PSQLQ() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' "$@"; }

# 夹具 UUID（与 so30_posting_fixture.sql 逐字一致）
CUST_A='d0300000-0000-4000-8000-000000000001'
SH_A='d0302000-0000-4000-8000-000000000001'
SH_B='d0302000-0000-4000-8000-000000000002'
I_A1='d0301100-0000-4000-8000-000000000001'
WH='d1370001-0000-4000-8000-000000000002'

PASS=0; FAIL=0; FAILED=()
ok()   { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); FAILED+=("$*"); printf '  ❌ %s\n' "$*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }

HTTP=''; CODE=''; BODY=''; MSG=''
# api <label> <METHOD> <PATH> <token> [json]
api() {
  local label=$1 method=$2 path=$3 tok=$4 payload=${5:-}
  local out="$RAW/$label.json"
  local args=(-s -m 30 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$tok" ] && args+=(-H "Authorization: Bearer $tok")
  if [ -n "$payload" ]; then
    args+=(-H 'Content-Type: application/json' -d "$payload")
  fi
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null || printf '')
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  MSG=$(printf '%s' "$BODY" | jq -r '.msg // .message // empty' 2>/dev/null)
}
d() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
# 数值比较：×10000 取整（Decimal 最多 4 位）
check_n() { # check_n <标签> <jq 表达式> <期望>
  local label=$1 expr=$2 want=$3 got expect
  got=$(printf '%s' "$BODY" | jq -r "($expr|tonumber)*10000|round" 2>/dev/null)
  expect=$(awk -v v="$want" 'BEGIN{printf "%d", (v*10000)+((v*10000>=0)?0.5:-0.5)}')
  check "$label" "$got" "$expect"
}

echo "== SO-31 退货单前端页接口验收（$BASE · 库 $DB） =="

# ---------------- 0) 登录（真实 token） ----------------
T_ADMIN=''; FIRST_PWD=''; MUST_CHANGE=false
try_login() {
  local out="$RAW/login_$3.json" code tok must
  code=$(curl -s -m 20 -o "$out" -w '%{http_code}' -X POST "$BASE/system/auth/login" \
           -H 'Content-Type: application/json' \
           -d "{\"username\":\"$1\",\"password\":\"$2\"}")
  [ "$code" = "200" ] || { printf ''; return; }
  tok=$(jq -r '.data.access_token // empty' "$out")
  must=$(jq -r '.data.must_change_password // false' "$out")
  jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
      | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
     "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  printf '%s\t%s' "$tok" "$must"
}
login() {
  local line
  line=$(try_login "$1" "$2" "${3:-try}")
  T_ADMIN=${line%%$'\t'*}
  MUST_CHANGE=${line##*$'\t'}
  [ -z "$T_ADMIN" ] && MUST_CHANGE=false
}

for cand in 'Admin@123456' 'Admin@So31!2026' 'Admin@So30!2026'; do
  login admin "$cand" "admin_$(printf '%s' "$cand" | tr -cd 'A-Za-z0-9')"
  if [ -n "$T_ADMIN" ]; then FIRST_PWD="$cand"; break; fi
done
[ -z "$T_ADMIN" ] && { echo "FATAL: admin 口令未知，无法继续"; exit 1; }
ok "admin 登录成功（must_change_password=$MUST_CHANGE）"
if [ "$MUST_CHANGE" = "true" ]; then
  api pre_pwd_change PUT /system/auth/password "$T_ADMIN" \
      "{\"old_password\":\"$FIRST_PWD\",\"new_password\":\"Admin@So31!2026\"}"
  [ "$CODE" = "0" ] && ok "前置：首登改密 → Admin@So31!2026" || bad "前置改密失败 http=$HTTP $(d '.msg')"
  login admin 'Admin@So31!2026' admin_so31
  [ -n "$T_ADMIN" ] && ok "新口令重登成功" || { bad "新口令重登失败"; exit 1; }
fi

# ---------------- ① 列表接口（含 SO-31 新字段） ----------------
api page_empty GET '/business/erp/sales-return/page?page=1&page_size=20' "$T_ADMIN"
check '列表 http' "$HTTP" '200'
check '列表 code' "$CODE" '0'
check '返回结构含 list' "$(printf '%s' "$BODY" | jq -r 'has("data") and (.data|has("list"))')" 'true'

# ---------------- ② 可退明细（新建弹窗的唯一读数） ----------------
api returnable_a GET "/business/erp/sales-return/returnable/$SH_A" "$T_ADMIN"
check '可退明细 http' "$HTTP" '200'
check '可退明细行数' "$(d '.data|length')" '2'
check '可退明细含 remaining/price/tax_rate' \
      "$(d '.data[0] | has("remaining") and has("price") and has("tax_rate") and has("so_item_id")')" 'true'
REM1=$(d '.data[0].remaining')
PRICE1=$(d '.data[0].price')
RATE1=$(d '.data[0].tax_rate')
echo "     首行可退=$REM1 单价=${PRICE1} 税率=${RATE1}"

# 夹具 B 是已过账（可退），再补一条「未过账发货单必须 409」的反证
NOPOST_SHIP=$(PSQLQ -c "select id from biz_shipment where status <> 'POSTED' and deleted_at is null limit 1")
if [ -n "$NOPOST_SHIP" ]; then
  api returnable_draft GET "/business/erp/sales-return/returnable/$NOPOST_SHIP" "$T_ADMIN"
  check '未过账发货单可退明细 http' "$HTTP" '409'
else
  echo "     （库里没有未过账发货单，跳过 409 反证）"
fi

# ---------------- ③ 建单（前端 = 显式带出 items，只改数量） ----------------
QTY1='3'
PAYLOAD=$(jq -nc \
  --arg sh "$SH_A" --arg wh "$WH" --arg type 'QUALITY' \
  --arg i1 "$I_A1" --arg q1 "$QTY1" \
  '{shipment_id:$sh, warehouse_id:$wh, return_type:$type, remark:"SO-31 前端页验收：部分退",
    items:[{so_item_id:$i1, qty:$q1, remark:"外箱破损"}]}')
api create POST '/business/erp/sales-return' "$T_ADMIN" "$PAYLOAD"
check '建单 http' "$HTTP" '200'
check '建单 code' "$CODE" '0'
check '建单状态' "$(d '.data.status')" 'DRAFT'
check '建单状态中文' "$(d '.data.status_label')" '草稿'
check '单号前缀 SR-' "$(d '.data.return_no' | cut -c1-3)" 'SR-'
check '建单类型中文' "$(d '.data.return_type_label')" '质量'
check '建单来源发货单号已补齐' "$(d '.data.shipment_no')" 'SO30-SH-A'
check '建单客户名已补齐' "$(d '.data.customer_name')" 'SO30 未开票客户'
check '建单审批实例为空（未接引擎）' "$(d '.data.approval_instance_id')" 'null'
check '建单明细行数' "$(d '.data.items|length')" '1'
check '建单明细数量（Decimal 4 位）' "$(d '.data.items[0].qty')" '3.0000'
# 金额 = qty × price（不含税），税额 = 金额 × 税率/100
EXP_AMOUNT=$(awk -v q="$QTY1" -v p="$PRICE1" 'BEGIN{printf "%.4f", q*p}')
EXP_TAX=$(awk -v a="$EXP_AMOUNT" -v r="$RATE1" 'BEGIN{printf "%.4f", a*r/100}')
check_n '不含税金额 = qty×price' '.data.total_amount' "$EXP_AMOUNT"
check_n '税额 = 金额×税率/100' '.data.total_tax' "$EXP_TAX"
RID=$(d '.data.id')
RNO=$(d '.data.return_no')
echo "     建单成功：$RNO ($RID)"

# 超退 400（前端 validateReturnForm 同一条闸）
OVER=$(jq -nc --arg sh "$SH_A" --arg wh "$WH" --arg i1 "$I_A1" \
  '{shipment_id:$sh, warehouse_id:$wh, return_type:"OTHER", items:[{so_item_id:$i1, qty:"9999"}]}')
api create_over POST '/business/erp/sales-return' "$T_ADMIN" "$OVER"
check '超退建单 http' "$HTTP" '400'
echo "     超退报文：$(d '.msg')"

# ---------------- ④ 列表（建单后可见 + 状态过滤 + 行内字段齐全） ----------------
api page_one GET '/business/erp/sales-return/page?page=1&page_size=20' "$T_ADMIN"
check '列表 http（建单后）' "$HTTP" '200'
check '列表总数' "$(d '.data.total')" '1'
check '列表首行单号' "$(d '.data.list[0].return_no')" "$RNO"
check '列表行含 approval_instance_id 字段' "$(d '.data.list[0] | has("approval_instance_id")')" 'true'
check '列表行客户名已补齐' "$(d '.data.list[0].customer_name')" 'SO30 未开票客户'
check '列表行发货单号已补齐' "$(d '.data.list[0].shipment_no')" 'SO30-SH-A'
check_n '列表行价税合计' '.data.list[0].total_with_tax' \
        "$(awk -v a="$EXP_AMOUNT" -v t="$EXP_TAX" 'BEGIN{printf "%.4f", a+t}')"
api page_fuzzy GET '/business/erp/sales-return/page?page=1&page_size=20&return_no=SR-' "$T_ADMIN"
check '单号模糊过滤命中' "$(d '.data.total')" '1'
api page_miss GET '/business/erp/sales-return/page?page=1&page_size=20&return_no=NOPE' "$T_ADMIN"
check '单号模糊过滤空' "$(d '.data.total')" '0'
api page_status GET '/business/erp/sales-return/page?page=1&page_size=20&status=POSTED' "$T_ADMIN"
check '状态过滤（POSTED 尚无）' "$(d '.data.total')" '0'

# ---------------- ⑤ 详情 ----------------
api detail GET "/business/erp/sales-return/$RID" "$T_ADMIN"
check '详情 http' "$HTTP" '200'
check '详情单号' "$(d '.data.return_no')" "$RNO"
check '详情明细行数' "$(d '.data.items|length')" '1'
check '详情明细物料 = 订单行带出' "$(d '.data.items[0].material_id')" 'd1370003-0000-4000-8000-000000000001'
check '详情行号' "$(d '.data.items[0].line_no')" '1'

# ---------------- ⑥ 状态流转（页面上 5 个按钮的实际调用） ----------------
st() { # st <label> <target> [reason]
  local label=$1 target=$2 reason=${3:-}
  local payload
  if [ -n "$reason" ]; then
    payload=$(jq -nc --arg t "$target" --arg r "$reason" '{target_status:$t, reason:$r}')
  else
    payload=$(jq -nc --arg t "$target" '{target_status:$t}')
  fi
  api "$label" PATCH "/business/erp/sales-return/$RID/status" "$T_ADMIN" "$payload"
}

st s_submit SUBMITTED
check '提交 http' "$HTTP" '200'
check '提交后状态' "$(d '.data.status')" 'SUBMITTED'
check '提交后状态中文' "$(d '.data.status_label')" '已提交'

# 非法边：重复提交（SUBMITTED → SUBMITTED 不在边表）
st s_submit_again SUBMITTED
check '重复提交 http（非法边）' "$HTTP" '409'

st s_approve APPROVED
check '批准后状态' "$(d '.data.status')" 'APPROVED'

st s_pending PENDING_POST
check '进入待过账后状态' "$(d '.data.status')" 'PENDING_POST'

# 非法边：待过账作废（边表里没有 PENDING_POST → CANCELLED）
st s_cancel_bad CANCELLED '试图待过账作废'
check '待过账作废 http（非法边）' "$HTTP" '409'
echo "     非法边报文：$(d '.msg')"

st s_post POSTED
check '过账 http' "$HTTP" '200'
check '过账后状态' "$(d '.data.status')" 'POSTED'
check '过账后状态中文' "$(d '.data.status_label')" '已过账'
check '过账回填入库单 id 非空' "$(d '.data.stock_in_id != null')" 'true'
check '过账回填应收 id 非空' "$(d '.data.receivable_id != null')" 'true'
echo "     入库单=$(d '.data.stock_in_id') 应收=$(d '.data.receivable_id') 红票=$(d '.data.red_invoice_id')"

# 幂等 / 终态：再流转一律 409
st s_post_again POSTED
check '重复过账 http（终态）' "$HTTP" '409'

# ---------------- ⑦ 过账后的详情与列表（页面刷新后的读数） ----------------
api detail_posted GET "/business/erp/sales-return/$RID" "$T_ADMIN"
check '过账后详情状态' "$(d '.data.status')" 'POSTED'
api page_posted GET '/business/erp/sales-return/page?page=1&page_size=20&status=POSTED' "$T_ADMIN"
check '状态过滤（POSTED）命中' "$(d '.data.total')" '1'

# 三本账回库可观测（页面不展示，但证明按钮背后的动作是真的：流水 doc_no = 入库单号）
PSQLQ -c "select 'stock_in='||in_no||' qty='||total_qty||' status='||status from biz_stock_in where source_type='SALES_RETURN' and source_id='$RID'::uuid"
PSQLQ -c "select 'txn='||count(*)||' qty_sum='||coalesce(sum(t.qty),0)||' after_sum='||coalesce(sum(t.after_qty),0) from biz_inventory_txn t join biz_stock_in s on s.in_no = t.doc_no where t.biz_type='SALES_RETURN_IN' and s.source_type='SALES_RETURN' and s.source_id='$RID'::uuid"
PSQLQ -c "select 'cost_book='||qty||'/'||amount from biz_inventory where material_id='d1370003-0000-4000-8000-000000000001'::uuid and warehouse_id='$WH'::uuid"

# ---------------- ⑧ 菜单迁移（V0306） ----------------
MENU=$(PSQLQ -c "select path||'|'||component||'|'||is_visible::text||'|'||coalesce(parent_id::text,'-')||'|'||sort_order from sys_menu where id='20000000-0000-0000-0000-000000003041'")
check '菜单 path|component|visible|parent|sort' "$MENU" \
      '/business/erp/sales-return|business/erp/sales-return/index|true|20000000-0000-0000-0000-000000002701|70'
check '迁移 306 已应用（versioned/executed）' \
      "$(PSQLQ -c "select kind||'/'||applied_via from schema_migrations where version like '%306%'" | head -1)" \
      'versioned/executed'
check 'applied_via 非法行数' \
      "$(PSQLQ -c "select count(*) from schema_migrations where applied_via not in ('executed','imported')")" '0'

echo
echo "== 结果：$PASS 通过 / $FAIL 失败 =="
if [ "$FAIL" -ne 0 ]; then
  printf '   失败项：\n'; for f in "${FAILED[@]}"; do printf '     - %s\n' "$f"; done
  exit 1
fi
