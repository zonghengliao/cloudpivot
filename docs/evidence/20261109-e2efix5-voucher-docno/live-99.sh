#!/usr/bin/env bash
# =============================================================================
# E2E-FIX-5 实测：① 凭证摘要取名称（不写 UUID）· ② 凭证 source_doc_no（退货单号）
# 卡 t_6e6b43a1（P3+P4）
#
# 打的是**全新隔离库 + 隔离端口**的实例（不碰 144 生产 / 不碰共享库 / 不碰 :8023），
# 覆盖卡面 4 条验收：
#   ① 新建收款 / 退款 / 退货 → 凭证摘要显示客户名称（不是 UUID）
#   ② 退货过账 → 两张凭证 source_doc_no = 退货单号（一张入库、一张红字发票）
#   ③ 按退货单号能查到凭证（API 精确筛 + 关键词两条路 + SQL）
#   ④ 存量数据已回填（摘要 UUID → 名称、source_doc_no = 单据号），迁移可重放
#
# 用法（99 / 任意有 PG + 源码的机器）：
#   bash docs/evidence/20261109-e2efix5-voucher-docno/live-99.sh
#   可覆盖：DB / PORT / ENVF / REPO
# =============================================================================
set -uo pipefail

DB="${DB:-cp_e2efix5}"
PORT="${PORT:-18322}"
ENVF="${ENVF:-/etc/cloudpivot/cloudpivot.env}"
REPO="${REPO:-/opt/cloudpivot}"
BASE="http://127.0.0.1:$PORT/api"
WORK="${WORK:-/tmp/e2efix5}"
RAW="$WORK/raw"; mkdir -p "$RAW"
LOG="$WORK/instance.log"

PASS=0; FAIL=0
say(){ printf '%s\n' "$*"; }
ok(){ PASS=$((PASS+1)); say "  ✅ $*"; }
bad(){ FAIL=$((FAIL+1)); say "  ❌ $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
show(){ printf '%s' "$BODY" | jq -c '{code,msg}' 2>/dev/null | head -c 300; }

if [ -f "$ENVF" ]; then
    set -a; . "$ENVF"; set +a
    SAY_ENV="env 文件 $ENVF"
else
    export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432
    export NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="e2efix5_local_jwt_secret_0123456789abcdefghijklmn"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    export X_CONFIG_SECRET_KEY="e2efix5_local_config_secret"
    SAY_ENV="本机兜底（无 $ENVF，DB 账号 alun）"
fi
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
export PATH="/root/.cargo/bin:$PATH"

# 用 **debug** 二进制（本机 cargo build 产物，含本次改动）；release 常是别的卡早先编的旧版本。
BIN="${BIN:-$REPO/backend/target/debug/cloudpivot}"
[ -x "$BIN" ] || { echo "缺少二进制 $BIN（先 cargo build --bin cloudpivot）"; exit 1; }

ADMIN_PASS='E2eFix5@Admin'
NEW_PASS='E2eFix5@Pass99'
APP_PASS='E2eFix5@Appr99'
export NIK_ADMIN_PASSWORD="$ADMIN_PASS"

PSQL(){ psql -q -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
    -U "${NIK_DATABASE_USER:-alun}" -d "$DB" -At -F'|' -P pager=off -c "$1"; }
PSQLF(){ psql -q -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
    -U "${NIK_DATABASE_USER:-alun}" -d "$DB" -At -F'|' -P pager=off -f "$1"; }

api(){ # api <name> <METHOD> <path> [token] [json]
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data(){ printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
login(){ # login <user> <pass> <tag>
  local user=$1 pass=$2 tag=$3 tok=""
  for _ in $(seq 1 15); do
    api "login_$tag" POST /system/auth/login "" "{\"username\":\"$user\",\"password\":\"$pass\"}"
    tok=$(data '.data.access_token')
    [ -n "$tok" ] && [ "$tok" != "null" ] && break
    sleep 2
  done
  printf '%s' "$tok"
}
mkpdf(){
  local f=$WORK/e2efix5-doc.pdf
  printf '%%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n4 0 obj<</Length 44>>stream\nBT /F1 12 Tf 20 100 Td (E2E-FIX-5 doc) Tj ET\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%%%EOF\n' > "$f"
  printf '%s' "$f"
}
attach_upload(){ # attach_upload <name> <doc_type> <doc_id> <file>
  local name=$1 dtype=$2 did=$3 file=$4 out="$RAW/$1.json"
  HTTP=$(curl -s -m 60 -o "$out" -w '%{http_code}' -X POST "$BASE/business/attachment/upload" \
    --oauth2-bearer "$T_ADMIN" \
    -F "doc_type=$dtype" -F "doc_id=$did" -F "file_role=INVOICE_SCAN" -F "file=@$file;type=application/pdf")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
approve_instance(){ # approve_instance <token> <instance_id> <tag>
  local t=$1 iid=$2 tag=$3 n=0 tid node
  while [ $n -lt 8 ]; do
    api "${tag}_todo$n" GET "/business/flow/task/todo?page=1&page_size=200" "$t"
    [ "$CODE" = "0" ] || { bad "$tag 待办查询 code=$CODE $(show)"; return 1; }
    tid=$(data ".data.list[]? | select(.instance_id==\"$iid\") | .id" | head -1)
    [ -z "$tid" ] && return 0
    node=$(data ".data.list[]? | select(.id==\"$tid\") | .current_node_name")
    api "${tag}_ap$n" POST "/business/flow/task/$tid/approve" "$t" '{"comment":"E2E-FIX-5 演练审批通过"}'
    if [ "$CODE" = "0" ]; then ok "审批通过节点 [$node]"; else bad "审批节点 [$node] 失败 http=$HTTP $(show)"; return 1; fi
    n=$((n+1))
  done
}

say "==== 0. 隔离库 + 隔离实例（$SAY_ENV）"
su - postgres -c "dropdb --force $DB" >/dev/null 2>&1
su - postgres -c "createdb -O ${NIK_DATABASE_USER:-alun} $DB" || { echo "createdb 失败"; exit 1; }
cd "$REPO/backend" || exit 1
export NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT"
: > "$LOG"
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
PID=$!
HEALTH=000
for _ in $(seq 1 150); do
  HEALTH=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")
  [ "$HEALTH" = "200" ] && break
  sleep 2
done
say "  pid=$PID bin=$BIN health=$HEALTH"
[ "$HEALTH" = "200" ] || { bad "实例没起来（health=$HEALTH），日志：$(tail -5 "$LOG")"; kill "$PID" 2>/dev/null; exit 1; }
say "  库指纹：迁移 $(PSQL 'select count(*) from schema_migrations') 条 / 最新 $(PSQL 'select max(version::int) from schema_migrations')"
check "V0357 已应用（source_doc_no 列）" \
  "$(PSQL "select count(*) from information_schema.columns where table_name='biz_voucher' and column_name='source_doc_no'")" "1"
check "V0357 索引在" \
  "$(PSQL "select count(*) from pg_indexes where indexname='idx_biz_voucher_source_doc_no'")" "1"

say "==== 1. admin 登录（全新库首登带 must_change_password）"
TOK=$(login admin "$ADMIN_PASS" admin0)
[ -n "$TOK" ] && [ "$TOK" != "null" ] || { bad "admin 登录失败：$BODY"; kill "$PID" 2>/dev/null; exit 1; }
api chpass PUT /system/auth/password "$TOK" "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}"
say "  改密 http=$HTTP msg=$(data '.msg')"
T_ADMIN=$(login admin "$NEW_PASS" admin1)
[ -n "$T_ADMIN" ] && [ "$T_ADMIN" != "null" ] && ok "admin 已登录" || { bad "二次登录失败：$BODY"; kill "$PID" 2>/dev/null; exit 1; }

say "==== 2. 前置主数据（审批人 / 仓库+库位 / 单位 / 物料 / 供应商 / 客户）"
api d_dept POST /system/dept "$T_ADMIN" '{"dept_code":"E2E5-DEPT","dept_name":"E2E5 财务演练部","status":1,"sort_order":950}'
DEPT_ID=$(data '.data.id')
[ -z "$DEPT_ID" ] || [ "$DEPT_ID" = "null" ] && DEPT_ID=$(PSQL "select id from sys_dept where dept_code='E2E5-DEPT' and deleted_at is null")
say "  部门 $DEPT_ID"
# 审批人：销售主管 + 财务经理（覆盖 so_approve / receipt_refund 节点）
ROLES='"20000000-0000-4000-8000-0000000f0007","20000000-0000-4000-8000-0000000f0005"'
api d_appr POST /system/user "$T_ADMIN" "{\"username\":\"e2efix5_appr\",\"password\":\"$APP_PASS\",\"real_name\":\"E2E5 演练审批人\",\"dept_id\":\"$DEPT_ID\",\"status\":1,\"role_ids\":[$ROLES],\"remark\":\"E2E-FIX-5 演练审批人\"}"
APP_ID=$(data '.data.id')
[ -z "$APP_ID" ] || [ "$APP_ID" = "null" ] && APP_ID=$(PSQL "select id from sys_user where username='e2efix5_appr' and deleted_at is null")
say "  审批人 $APP_ID"
T_APP=$(login e2efix5_appr "$APP_PASS" appr0)
[ -n "$T_APP" ] && [ "$T_APP" != "null" ] && ok "审批人已登录" || bad "审批人登录失败：$BODY"

api d_wh POST /business/erp/warehouse "$T_ADMIN" '{"warehouse_code":"E2E5-WH","warehouse_name":"E2E5 演练仓","warehouse_type":"NORMAL","level":1,"status":1}'
WH_ID=$(data '.data.id')
[ -z "$WH_ID" ] || [ "$WH_ID" = "null" ] && WH_ID=$(PSQL "select id from biz_warehouse where warehouse_code='E2E5-WH' and deleted_at is null")
api d_loc POST /business/erp/location "$T_ADMIN" "{\"location_code\":\"E2E5-L01\",\"warehouse_id\":\"$WH_ID\",\"location_name\":\"默认库位\",\"location_type\":\"STORAGE\",\"is_default\":true,\"status\":1}"
say "  仓库 $WH_ID / 库位 $(data '.data.id')"
api d_unit POST /business/erp/unit "$T_ADMIN" '{"unit_code":"E2E5-PCS","unit_name":"个(E2E5)","is_base":true,"status":1,"precision":0}'
UNIT_ID=$(data '.data.id')
[ -z "$UNIT_ID" ] || [ "$UNIT_ID" = "null" ] && UNIT_ID=$(PSQL "select id from biz_unit where unit_code='E2E5-PCS' and deleted_at is null")
api d_mat POST /business/erp/material "$T_ADMIN" "{\"material_code\":\"E2E5-M1\",\"material_name\":\"E2E5 演练物料\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"FINISHED\",\"tax_rate\":13,\"status\":1,\"serial_required\":false}"
MAT_ID=$(data '.data.id')
[ -z "$MAT_ID" ] || [ "$MAT_ID" = "null" ] && MAT_ID=$(PSQL "select id from biz_material where material_code='E2E5-M1' and deleted_at is null")
say "  物料 $MAT_ID"
api d_sup POST /business/erp/supplier "$T_ADMIN" '{"supplier_code":"E2E5-SUP","supplier_name":"E2E5 演练供应商","contact_person":"供货联系人","contact_phone":"01000000005","credit_days":30,"status":1}'
SUP_ID=$(data '.data.id')
[ -z "$SUP_ID" ] || [ "$SUP_ID" = "null" ] && SUP_ID=$(PSQL "select id from biz_supplier where supplier_code='E2E5-SUP' and deleted_at is null")
say "  供应商 $SUP_ID"
# 客户名刻意起得可搜：摘要里必须出现它
CUST_NAME='E2E5 北京云枢示例客户'
api d_cus POST /business/erp/customer "$T_ADMIN" "{\"code\":\"E2E5-C1\",\"name\":\"$CUST_NAME\",\"tax_no\":\"91110000E2E5C1XX\",\"contact_name\":\"张三\",\"contact_phone\":\"13800000005\",\"credit_limit\":100000,\"credit_days\":30,\"status\":\"ACTIVE\"}"
CUST_ID=$(data '.data.id')
[ -z "$CUST_ID" ] || [ "$CUST_ID" = "null" ] && CUST_ID=$(PSQL "select id from biz_customer where code='E2E5-C1' and deleted_at is null")
say "  客户 $CUST_ID（$CUST_NAME）"
[ -n "$CUST_ID" ] && [ "$CUST_ID" != "null" ] && ok "主数据就绪" || { bad "主数据不全：$BODY"; }

say "==== 3. 可用库存：无源入库 200@4（价税合计 800 < 1000 → 免审自动过账）"
api s_in POST /business/erp/stock-in "$T_ADMIN" \
  "{\"is_sourceless\":true,\"supplier_id\":\"$SUP_ID\",\"warehouse_id\":\"$WH_ID\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-FIX-5 期初库存\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":200,\"price_excl\":4,\"tax_rate\":0}]}"
IN_ID=$(data '.data.header.id // .data.id // empty')
[ -z "$IN_ID" ] || [ "$IN_ID" = "null" ] && IN_ID=$(PSQL "select id from biz_stock_in where remark='E2E-FIX-5 期初库存' and deleted_at is null order by created_at desc limit 1")
ST=$(PSQL "select status from biz_stock_in where id='$IN_ID'")
say "  入库单 $IN_ID 状态=$ST http=$HTTP"
if [ "$ST" != "POSTED" ]; then
  api s_in_post POST "/business/erp/stock-in/$IN_ID/submit-post" "$T_ADMIN" '{}'
  IID=$(PSQL "select approval_instance_id from biz_stock_in where id='$IN_ID'")
  [ -n "$IID" ] && [ "$IID" != "null" ] && [ "$IID" != "" ] && approve_instance "$T_APP" "$IID" s_in_ap
fi
check "库存 200" "$(PSQL "select coalesce(sum(qty),0)::numeric(20,4)::text from biz_inventory where material_id='$MAT_ID' and warehouse_id='$WH_ID'")" "200.0000"

say "==== 4. 销售订单 10×100（税 13%）→ 提交审批 → 换人批 → APPROVED"
api s_so POST /business/erp/sales-order "$T_ADMIN" \
  "{\"customer_id\":\"$CUST_ID\",\"order_date\":\"2026-11-09\",\"remark\":\"E2E-FIX-5 销售订单\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":10,\"price\":100,\"tax_rate\":13}]}"
SO_ID=$(data '.data.id // empty')
[ -z "$SO_ID" ] || [ "$SO_ID" = "null" ] && SO_ID=$(PSQL "select id from biz_sales_order where remark='E2E-FIX-5 销售订单' and deleted_at is null order by created_at desc limit 1")
say "  订单 $(data '.data.so_no') id=$SO_ID 金额=$(data '.data.total_amount') 税=$(data '.data.total_tax') 价税=$(data '.data.total_with_tax')"
api s_so_sub POST "/business/erp/sales-order/$SO_ID/submit-approval" "$T_ADMIN" '{"reason":"E2E-FIX-5 提交审批"}'
SO_IID=$(PSQL "select approval_instance_id from biz_sales_order where id='$SO_ID'")
[ -n "$SO_IID" ] && [ "$SO_IID" != "" ] && approve_instance "$T_APP" "$SO_IID" s_so_ap
check "订单状态" "$(PSQL "select status from biz_sales_order where id='$SO_ID'")" "APPROVED"

say "==== 5. 发货单 → PENDING_POST → POSTED（扣库存 + 成本凭证 + 挂应收）"
api s_ship POST /business/erp/shipment "$T_ADMIN" "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH_ID\",\"ship_date\":\"2026-11-09\",\"remark\":\"E2E-FIX-5 发货单\",\"items\":[]}"
SHIP_ID=$(data '.data.id // empty')
[ -z "$SHIP_ID" ] || [ "$SHIP_ID" = "null" ] && SHIP_ID=$(PSQL "select id from biz_shipment where remark='E2E-FIX-5 发货单' order by created_at desc limit 1")
say "  发货单 $SHIP_ID"
api s_ship_p PATCH "/business/erp/shipment/$SHIP_ID/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
say "  → PENDING_POST http=$HTTP $(show)"
api s_ship_post PATCH "/business/erp/shipment/$SHIP_ID/status" "$T_ADMIN" '{"target_status":"POSTED"}'
say "  → POSTED http=$HTTP $(show)"
check "发货单 POSTED" "$(PSQL "select status from biz_shipment where id='$SHIP_ID'")" "POSTED"
SOUT_ID=$(PSQL "select id from biz_stock_out where source_type='SALES' and source_id='$SHIP_ID' and deleted_at is null")
AR_ID=$(PSQL "select id from biz_receivable where source_type='SALES_OUT' and source_id='$SOUT_ID' and deleted_at is null")
say "  出库单 $SOUT_ID / 应收 $AR_ID 金额=$(PSQL "select amount from biz_receivable where id='$AR_ID'")"
check "应收挂出 1130" "$(PSQL "select amount from biz_receivable where id='$AR_ID'")" "1130.0000"

say "==== 6. 开票（ISSUED）→ 收入凭证摘要取客户名称（验收 ①·开票路径）"
api s_inv POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR_ID\",\"invoice_date\":\"2026-11-09\",\"is_electronic\":true,\"total_with_tax\":1130,\"tax_rate\":13,\"remark\":\"E2E-FIX-5 电子票\"}"
INV_ID=$(data '.data.id // empty')
[ -z "$INV_ID" ] || [ "$INV_ID" = "null" ] && INV_ID=$(PSQL "select id from biz_invoice where remark='E2E-FIX-5 电子票' order by created_at desc limit 1")
say "  发票 $(data '.data.invoice_no') id=$INV_ID"
attach_upload s_inv_att INVOICE "$INV_ID" "$(mkpdf)"
say "  附件 http=$HTTP $(show)"
api s_inv_issue PATCH "/business/erp/invoice/$INV_ID/status" "$T_ADMIN" '{"status":"ISSUED"}'
say "  → ISSUED http=$HTTP $(show)"
INV_NO=$(PSQL "select invoice_no from biz_invoice where id='$INV_ID'")
INV_SUM=$(PSQL "select summary from biz_voucher where source_type='INVOICE' and source_id='$INV_ID'")
say "  收入凭证摘要：$INV_SUM"
printf '%s' "$INV_SUM" | grep -q "$CUST_NAME" && ok "开票凭证摘要含客户名称「$CUST_NAME」" || bad "开票凭证摘要没有客户名称：$INV_SUM"
printf '%s' "$INV_SUM" | grep -qE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' && bad "摘要里仍有 UUID：$INV_SUM" || ok "摘要里没有 UUID"
check "开票凭证 source_doc_no = 发票号（源单即业务单）" \
  "$(PSQL "select source_doc_no from biz_voucher where source_type='INVOICE' and source_id='$INV_ID'")" "$INV_NO"

say "==== 7. ★ 销售退货过账（验收 ②③）"
api s_retble GET "/business/erp/sales-return/returnable/$SHIP_ID" "$T_ADMIN"
say "  可退明细 $(data '.data | length' 2>/dev/null) 行"
SOI_ID=$(PSQL "select id from biz_sales_order_item where so_id='$SO_ID' order by line_no limit 1")
api s_ret POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SHIP_ID\",\"warehouse_id\":\"$WH_ID\",\"return_type\":\"QUALITY\",\"remark\":\"E2E-FIX-5 退货单（退 2 件）\",\"items\":[{\"so_item_id\":\"$SOI_ID\",\"qty\":2}]}"
RET_ID=$(data '.data.id // empty')
[ -z "$RET_ID" ] || [ "$RET_ID" = "null" ] && RET_ID=$(PSQL "select id from biz_sales_return where remark='E2E-FIX-5 退货单（退 2 件）' order by created_at desc limit 1")
RET_NO=$(PSQL "select return_no from biz_sales_return where id='$RET_ID'")
say "  退货单 $RET_NO id=$RET_ID 金额=$(data '.data.total_amount') 税=$(data '.data.total_tax') 价税=$(data '.data.total_with_tax')"
check "退货金额 2×100 / 税 26 / 价税 226" \
  "$(PSQL "select total_amount||'/'||total_tax||'/'||total_with_tax from biz_sales_return where id='$RET_ID'")" "200.0000/26.0000/226.0000"
attach_upload s_ret_att SALES_RETURN "$RET_ID" "$(mkpdf)"
say "  退货单附件 http=$HTTP $(show)"
api s_ret_sub PATCH "/business/erp/sales-return/$RET_ID/status" "$T_ADMIN" '{"target_status":"SUBMITTED"}'
say "  → SUBMITTED http=$HTTP $(show)"
api s_ret_apv PATCH "/business/erp/sales-return/$RET_ID/status" "$T_ADMIN" '{"target_status":"APPROVED"}'
say "  → APPROVED http=$HTTP $(show)"
api s_ret_pp PATCH "/business/erp/sales-return/$RET_ID/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
say "  → PENDING_POST http=$HTTP $(show)"
api s_ret_postd PATCH "/business/erp/sales-return/$RET_ID/status" "$T_ADMIN" '{"target_status":"POSTED"}'
say "  → POSTED http=$HTTP $(show)"
check "退货单 POSTED" "$(PSQL "select status from biz_sales_return where id='$RET_ID'")" "POSTED"

say "  -- ② 两张凭证的 source_doc_no 都必须是退货单号"
say "  凭证清单:"
PSQLF <(printf '%s\n' "select voucher_no||' | '||source_type||' | source_no='||coalesce(source_no,'-')||' | source_doc_no='||coalesce(source_doc_no,'-')||' | '||summary from biz_voucher where source_doc_no='$RET_NO' order by voucher_no") | sed 's/^/    /'
N2=$(PSQL "select count(*) from biz_voucher where source_doc_no='$RET_NO'")
check "按 source_doc_no 命中凭证张数" "$N2" "2"
check "来源类型是入库 + 红字发票两张" \
  "$(PSQL "select string_agg(source_type,',' order by source_type) from biz_voucher where source_doc_no='$RET_NO'")" "INVOICE,STOCK_IN"
check "两张凭证的 source_no 仍是子单据号（≠ 退货单号）" \
  "$(PSQL "select count(*) from biz_voucher where source_doc_no='$RET_NO' and source_no<>'$RET_NO'")" "2"
check "成本冲减凭证摘要含客户名称" \
  "$(PSQL "select count(*) from biz_voucher where source_doc_no='$RET_NO' and source_type='STOCK_IN' and summary like '%$CUST_NAME%'")" "1"
check "收入冲减凭证摘要含客户名称" \
  "$(PSQL "select count(*) from biz_voucher where source_doc_no='$RET_NO' and source_type='INVOICE' and summary like '%$CUST_NAME%'")" "1"
check "两张凭证摘要都不含 UUID" \
  "$(PSQL "select count(*) from biz_voucher where source_doc_no='$RET_NO' and summary ~ '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'")" "0"
say "  摘要样例：$(PSQL "select string_agg(summary,' ‖ ') from biz_voucher where source_doc_no='$RET_NO'")"

say "  -- ③ 按退货单号查凭证：API 两条查询面"
api s_q_exact GET "/business/erp/voucher/page?page=1&page_size=20&source_doc_no=$RET_NO" "$T_ADMIN"
say "  精确筛 http=$HTTP total=$(data '.data.total')"
check "API 精确筛 total" "$(data '.data.total')" "2"
check "API 精确筛返回行的 source_doc_no" \
  "$(data '[.data.list[].source_doc_no] | unique | join(",")')" "$RET_NO"
api s_q_kw GET "/business/erp/voucher/page?page=1&page_size=20&voucher_no=$RET_NO" "$T_ADMIN"
say "  关键词 http=$HTTP total=$(data '.data.total')"
check "关键词（voucher_no 也匹配 source_doc_no）total" "$(data '.data.total')" "2"

say "==== 8. 收款单 POSTED → 收款凭证摘要取客户名称（验收 ①·收款路径）"
api s_rec POST /business/erp/receipt "$T_ADMIN" \
  "{\"customer_id\":\"$CUST_ID\",\"receipt_date\":\"2026-11-09\",\"amount\":100,\"method\":\"BANK_TRANSFER\",\"bank_account\":\"6222000000000001\",\"remark\":\"E2E-FIX-5 收款\"}"
REC_ID=$(data '.data.id // empty')
[ -z "$REC_ID" ] || [ "$REC_ID" = "null" ] && REC_ID=$(PSQL "select id from biz_receipt where remark='E2E-FIX-5 收款' and deleted_at is null order by created_at desc limit 1")
REC_NO=$(PSQL "select receipt_no from biz_receipt where id='$REC_ID'")
say "  收款单 $REC_NO id=$REC_ID"
api s_rec_alloc POST "/business/erp/receipt/$REC_ID/allocate" "$T_ADMIN" "{\"items\":[{\"receivable_id\":\"$AR_ID\",\"amount\":100}]}"
say "  核销 http=$HTTP $(show)"
# ATT 规则：收款单确认必须上传银行回单（pdf/jpg/png）——先把附件挂上
attach_upload s_rec_att RECEIPT "$REC_ID" "$(mkpdf)"
say "  收款单附件 http=$HTTP $(show)"
api s_rec_conf PATCH "/business/erp/receipt/$REC_ID/status" "$T_ADMIN" '{"status":"CONFIRMED"}'
say "  → CONFIRMED http=$HTTP $(show)"
api s_rec_post PATCH "/business/erp/receipt/$REC_ID/status" "$T_ADMIN" '{"status":"POSTED"}'
say "  → POSTED http=$HTTP $(show)"
REC_SUM=$(PSQL "select summary from biz_voucher where source_type='RECEIPT' and source_id='$REC_ID'")
say "  收款凭证摘要：$REC_SUM"
[ -n "$REC_SUM" ] && ok "收款凭证已生成（$REC_NO）" || bad "收款单没有生成凭证（状态 $(PSQL "select status from biz_receipt where id='$REC_ID'")）"
printf '%s' "$REC_SUM" | grep -q "$CUST_NAME" && ok "收款凭证摘要含客户名称（不是 UUID）" || bad "收款凭证摘要：$REC_SUM"
printf '%s' "$REC_SUM" | grep -qE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' && bad "收款摘要里仍有 UUID" || ok "收款摘要无 UUID"
check "收款凭证 source_doc_no = 收款单号" \
  "$(PSQL "select source_doc_no from biz_voucher where source_type='RECEIPT' and source_id='$REC_ID'")" "$REC_NO"

say "==== 9. 收款退款单 POSTED → 退款凭证摘要取客户名称（验收 ①·退款路径）"
api s_ref POST /business/erp/receipt-refund "$T_ADMIN" \
  "{\"customer_id\":\"$CUST_ID\",\"source_receipt_id\":\"$REC_ID\",\"refund_date\":\"2026-11-09\",\"amount\":100,\"method\":\"BANK_TRANSFER\",\"bank_account\":\"6222000000000001\",\"reason\":\"E2E-FIX-5 演练退款\"}"
REF_ID=$(data '.data.id // empty')
[ -z "$REF_ID" ] || [ "$REF_ID" = "null" ] && REF_ID=$(PSQL "select id from biz_receipt_refund where reason='E2E-FIX-5 演练退款' and deleted_at is null order by created_at desc limit 1")
REF_NO=$(PSQL "select refund_no from biz_receipt_refund where id='$REF_ID'")
say "  退款单 $REF_NO id=$REF_ID http=$HTTP"
api s_ref_alloc POST "/business/erp/receipt-refund/$REF_ID/allocate" "$T_ADMIN" "{\"allocations\":[{\"receivable_id\":\"$AR_ID\",\"amount\":100}]}"
say "  冲减 http=$HTTP $(show)"
# 退款单要「确认」之后才能送审（DRAFT → CONFIRMED → 审批 → 自动过账）
api s_ref_conf PATCH "/business/erp/receipt-refund/$REF_ID/status" "$T_ADMIN" '{"status":"CONFIRMED"}'
say "  → CONFIRMED http=$HTTP $(show)"
api s_ref_sub POST "/business/erp/receipt-refund/$REF_ID/submit-approval" "$T_ADMIN" '{"reason":"E2E-FIX-5 退款提交审批"}'
say "  提交审批 http=$HTTP $(show)"
REF_IID=$(PSQL "select id from biz_flow_instance where biz_type='RECEIPT_REFUND' and biz_id='$REF_ID' order by created_at desc limit 1")
[ -n "$REF_IID" ] && [ "$REF_IID" != "" ] && approve_instance "$T_APP" "$REF_IID" s_ref_ap
REF_ST=$(PSQL "select status from biz_receipt_refund where id='$REF_ID'")
say "  退款单状态=$REF_ST"
if [ "$REF_ST" = "POSTED" ]; then
  REF_SUM=$(PSQL "select summary from biz_voucher where source_type='RECEIPT_REFUND' and source_id='$REF_ID'")
  say "  退款凭证摘要：$REF_SUM"
  printf '%s' "$REF_SUM" | grep -q "$CUST_NAME" && ok "退款凭证摘要含客户名称" || bad "退款凭证摘要：$REF_SUM"
  check "退款凭证 source_doc_no = 退款单号" \
    "$(PSQL "select source_doc_no from biz_voucher where source_type='RECEIPT_REFUND' and source_id='$REF_ID'")" "$REF_NO"
else
  say "  ⚠ 退款单未过账（状态 $REF_ST），退款凭证断言跳过 —— 见 raw/s_ref_*.json"
  bad "退款单没有走到 POSTED（状态 $REF_ST）：$(show)"
fi

say "==== 10. ④ 存量数据回填（摘要 UUID → 名称 / source_doc_no = 单据号）+ 迁移可重放"
# 造一条「迁移前形状」的历史凭证：摘要里写客户 UUID，source_no 有值、source_doc_no 为空
LEGACY_ID=$(PSQL "select gen_random_uuid()")
PSQL "insert into biz_voucher (id, voucher_no, period, voucher_date, voucher_type, source_type, source_no, summary, total_debit, total_credit, entry_count, status, is_red, posted_at)
      values ('$LEGACY_ID', 'PZ-202611-999999', '2026-11', '2026-11-01', 'AUTO', 'RECEIPT', 'RC-LEGACY-0001',
              '收款 RC-LEGACY-0001 客户 $CUST_ID', 10, 10, 2, 'POSTED', 0, now())" >/dev/null
PSQL "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
      select '$LEGACY_ID', 1, '收款 RC-LEGACY-0001 客户 $CUST_ID 银行收款', id, '1122', '应收账款', 10, 0 from biz_account where account_code='1122' and deleted_at is null limit 1" >/dev/null
check "回填前：摘要里有 UUID" "$(PSQL "select count(*) from biz_voucher where id='$LEGACY_ID' and summary like '%$CUST_ID%'")" "1"
PSQLF "$REPO/backend/migrations/business/V0357__voucher_source_doc_no.up.sql" > "$RAW/mig_rerun1.txt" 2>&1
say "  重放迁移（第 1 次）exit=$?"
say "    $(grep -E 'NOTICE|ERROR' "$RAW/mig_rerun1.txt" | tail -2 | tr '\n' ' ')"
check "回填后：摘要换成客户名称" "$(PSQL "select summary from biz_voucher where id='$LEGACY_ID'")" "收款 RC-LEGACY-0001 客户 $CUST_NAME"
check "回填后：分录摘要也换成名称" "$(PSQL "select summary from biz_voucher_entry where voucher_id='$LEGACY_ID'")" "收款 RC-LEGACY-0001 客户 $CUST_NAME 银行收款"
check "回填后：source_doc_no 回落成 source_no" "$(PSQL "select source_doc_no from biz_voucher where id='$LEGACY_ID'")" "RC-LEGACY-0001"
PSQLF "$REPO/backend/migrations/business/V0357__voucher_source_doc_no.up.sql" > "$RAW/mig_rerun2.txt" 2>&1
say "  重放迁移（第 2 次）exit=$?（幂等：不覆盖、不重复、不报错）"
check "二次重放后仍是名称（不二次替换）" "$(PSQL "select summary from biz_voucher where id='$LEGACY_ID'")" "收款 RC-LEGACY-0001 客户 $CUST_NAME"
check "二次重放后 source_doc_no 不变" "$(PSQL "select source_doc_no from biz_voucher where id='$LEGACY_ID'")" "RC-LEGACY-0001"

say "==== 11. 全库扫描：凭证 / 分录摘要里不许有 UUID"
check "biz_voucher 摘要含 UUID 的行数" \
  "$(PSQL "select count(*) from biz_voucher where deleted_at is null and summary ~ '(客户|供应商|物料) [0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'")" "0"
check "biz_voucher_entry 摘要含 UUID 的行数" \
  "$(PSQL "select count(*) from biz_voucher_entry where summary ~ '(客户|供应商|物料) [0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'")" "0"
check "有来源单号的 AUTO 凭证 source_doc_no 均非空" \
  "$(PSQL "select count(*) from biz_voucher where deleted_at is null and voucher_type='AUTO' and source_no is not null and btrim(source_no) <> '' and source_doc_no is null")" "0"

say
say "== 收尾：停实例（库 $DB 保留取证）"
kill "$PID" 2>/dev/null
sleep 2
ss -ltnp 2>/dev/null | grep -q ":$PORT " && say "  ⚠ 端口 $PORT 仍在监听" || say "  ✅ 实例已停"
say
say "== PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = 0 ] || say "== 有失败项（退出码 1）"
[ "$FAIL" = 0 ]
