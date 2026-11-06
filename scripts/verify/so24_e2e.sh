#!/usr/bin/env bash
# =============================================================================
# SO-24 端到端链路测试（ARCH-7 §12.2 十步）—— 真后端 + 真 token curl
# =============================================================================
#   bash scripts/verify/so24_e2e.sh up      # 建隔离库 cloudpivot_so24 + 起后端 :18224
#   bash scripts/verify/so24_e2e.sh chain   # 跑 10 步链路（每步快照落 docs/evidence/sales-e2e/raw/）
#   bash scripts/verify/so24_e2e.sh down    # 停实例（幂等）
#
# 库：cloudpivot_so24 = 从 cloudpivot_so23fresh（空库 + 225 迁移）复制；owner alun
# 端口 18224 ∈ 隔离段 18000-18999（18223 是 SO-23 基线库端口，不撞）
# 基线数据：仅迁移种子（5 物料 / 3 仓库 / 9 角色 / admin）；库存靠「期初」接口建，
#         三本账由 opening 模块（inventory_writer 同一写入口）落地，绝不直写。
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
DB=${SO24_DB:-cloudpivot_so24}
PORT=${SO24_PORT:-18224}
BASE="http://127.0.0.1:$PORT/api"
SRC=${SO24_SRC_DB:-cloudpivot_so23fresh}
LOG=/tmp/so24_server.log
PIDF=/tmp/so24_server.pid
EVID="$ROOT/docs/evidence/sales-e2e"
RAW="$EVID/raw"
mkdir -p "$RAW"
export PGPASSWORD=alun
PSQLQ() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' "$@"; }

PASS=0; FAIL=0; FAILED_STEPS=()
say() { printf '%s\n' "$*"; }
psql_super() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }

ok()   { PASS=$((PASS+1)); say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); FAILED_STEPS+=("$*"); say "  ❌ $*"; }
check() { # check <desc> <actual> <expected>
  if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi
}

stop_instance() {
  [ -f "$PIDF" ] || return 0
  kill "$(cat "$PIDF")" 2>/dev/null || true
  sleep 2
  kill -9 "$(cat "$PIDF")" 2>/dev/null || true
  rm -f "$PIDF"
}

up() {
  stop_instance
  psql_super "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
  psql_super "-c 'DROP DATABASE IF EXISTS $DB'" >/dev/null || exit 1
  psql_super "-c 'CREATE DATABASE $DB TEMPLATE $SRC OWNER alun'" >/dev/null || exit 1
  say "✅ 库 $DB 已从 $SRC 复制（$(PSQLQ -c 'select count(*) from schema_migrations') 条迁移）"

  cd "$ROOT/backend" || exit 1
  NIK_PROFILE=test \
  NIK_DATABASE_NAME="$DB" \
  NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun \
  NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
  NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDF"

  code=""
  for _ in $(seq 1 90); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
    [ "$code" = "200" ] && break
    kill -0 "$(cat "$PIDF")" 2>/dev/null || { say "FATAL: 实例提前退出"; tail -30 "$LOG"; exit 1; }
    sleep 2
  done
  say "health=$code pid=$(cat "$PIDF") log=$LOG"
}

# ---------------- HTTP 原语（真 token curl，全程留证） ----------------
api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 30 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  if [ -n "$body" ]; then
    args+=(-H 'Content-Type: application/json' -d "$body")
  fi
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out")
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  jq -c '{code,msg,data}' "$out" >/dev/null 2>&1 || say "  ⚠ 非 JSON 响应（$name）: $(head -c 200 "$out")"
}
data() { printf '%s' "$BODY" | jq -r "$1"; }

login() { # login <user> <pass> <name>
  api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  [ "$HTTP" = "200" ] && [ "$CODE" = "0" ] || { say "FATAL: 登录失败 $1 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 300)"; exit 1; }
  data '.data.access_token'
}

snap() { # snap <文件名> <SQL>...（多行 = 多条语句，逐行单独执行）
  local f="$RAW/$1.txt"; shift
  : > "$f"
  local sql one
  for sql in "$@"; do
    echo "--- $(printf '%s' "$sql" | tr '\n' ' ' | tr -s ' ')" >> "$f"
    while IFS= read -r one; do
      one="$(printf '%s' "$one" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
      [ -z "$one" ] && continue
      PSQLQ -c "$one" >> "$f" 2>&1 || true
    done <<< "$sql"
  done
  if grep -q '^ERROR' "$f"; then say "  ⚠ 快照里有 SQL 报错：$f"; fi
  say "  📄 快照 $f"
}

# =============================================================================
chain() {
  set +e
  say "== 环境：$BASE 库=$DB"
  local T_ADMIN T_APP OK_ID
  T_ADMIN=$(login admin 'Admin@123456' admin0)
  say "  · 种子管理员 must_change_password=TRUE：先走真实改密接口（首登强制改密闸，session_guard.rs:112）"
  api pre_pwd_change PUT /system/auth/password "$T_ADMIN" '{"old_password":"Admin@123456","new_password":"Admin@So24!2026"}'
  if [ "$CODE" = "0" ]; then ok "前置：admin 首登改密 Admin@123456 → Admin@So24!2026"; else bad "前置改密 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 200)"; fi
  T_ADMIN=$(login admin 'Admin@So24!2026' admin)
  say "  ✅ admin token 取得（首登改密后重新登录）"

  # ---- 前置：审批人账号（销售主管角色成员 + SUPER_ADMIN，用于「制单人 ≠ 审批人」）----
  api pre_user_create POST /system/user "$T_ADMIN" '{
    "username":"so24_sales_mgr","password":"So24@Mgr123","real_name":"SO24 销售主管",
    "role_ids":["20000000-0000-4000-8000-0000000f0007","20000000-0000-4000-8000-0000000f0005","20000000-0000-4000-8000-0000000f0003","ef075fc7-a37e-451d-8568-5fd20b4fa0d5"],
    "status":1,"remark":"SO-24 e2e 审批人夹具（SALES_SUPERVISOR + SUPER_ADMIN）"}'
  if [ "$CODE" = "0" ]; then ok "前置：审批人 so24_sales_mgr 建号"; else bad "前置：审批人建号 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 200)"; fi
  T_APP=$(login so24_sales_mgr 'So24@Mgr123' app)

  # =========================== ① 建客户 ===========================
  say; say "① 建客户「测试客户A」，信用额度 10 万"
  api step01_customer POST /business/erp/customer "$T_ADMIN" '{
    "code":"C-SO24-001","name":"测试客户A","tax_no":"91110000SO24TEST1X",
    "contact_name":"张三","contact_phone":"13800000000","address":"北京市朝阳区测试路 1 号",
    "credit_days":30,"credit_limit":100000,"remark":"SO-24 端到端测试客户"}'
  CUST=$(data '.data.id')
  if [ "$CODE" = "0" ] && [ -n "$CUST" ]; then ok "客户建档 id=$CUST 信用额度=$(data '.data.credit_limit')"; else bad "客户建档 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 300)"; fi
  snap step01  "select 'customer: '||code||'|'||name||'|credit_limit='||credit_limit||'|credit_days='||credit_days||'|status='||status from biz_customer where code='C-SO24-001'
                select 'docs_at_step01: so='||(select count(*) from biz_sales_order)||' sh='||(select count(*) from biz_shipment)||' ar='||(select count(*) from biz_receivable)||' 库存行='||(select count(*) from biz_inventory)"

  # ---- 前置：库存（走期初接口，三本账由 opening 模块写）----
  say; say "前置：期初建库存（2026-11，WH01：M-W1A-001 ×200 @300 / M-W1A-002 ×100 @400）"
  api pre_opening POST /business/erp/opening-balance/inventory "$T_ADMIN" '{
    "period":"2026-11","items":[
      {"material_id":"d1370003-0000-4000-8000-000000000001","warehouse_id":"d1370001-0000-4000-8000-000000000001","qty":200,"unit_cost":300,"remark":"SO-24 期初"},
      {"material_id":"d1370003-0000-4000-8000-000000000002","warehouse_id":"d1370001-0000-4000-8000-000000000001","qty":100,"unit_cost":400,"remark":"SO-24 期初"}]}'
  if [ "$CODE" = "0" ]; then ok "期初库存落地（$(data '.data.inventory | length // 0') 行）"; else bad "期初库存 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 400)"; fi
  snap pre_inventory "select 'inv: '||m.material_code||' wh='||w.warehouse_code||' qty='||i.qty||' avg='||i.avg_cost||' amt='||i.amount from biz_inventory i join biz_material m on m.id=i.material_id join biz_warehouse w on w.id=i.warehouse_id order by m.material_code
                      select 'loc: '||m.material_code||' qty='||l.qty||' loc='||coalesce(l.location_id::text,'-') from biz_inventory_location l join biz_material m on m.id=l.material_id order by 1
                      select 'txn: '||txn_type||'|'||biz_type||'|'||qty||'|'||amount from biz_inventory_txn order by created_at
                      select 'voucher: '||voucher_no||'|'||voucher_type||'|'||total_debit||'/'||total_credit from biz_voucher order by created_at"

  # =========================== ② 销售订单 ===========================
  say; say "② 建销售订单（2 物料，不含税 5 万，税 13% ⇒ 价税合计 5.65 万），提交 → 审批通过"
  api step02_so_create POST /business/erp/sales-order "$T_ADMIN" "{
    \"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"delivery_date\":\"2026-11-20\",
    \"remark\":\"SO-24 e2e 销售订单\",
    \"items\":[
      {\"material_id\":\"d1370003-0000-4000-8000-000000000001\",\"qty\":100,\"price\":300,\"tax_rate\":13},
      {\"material_id\":\"d1370003-0000-4000-8000-000000000002\",\"qty\":50,\"price\":400,\"tax_rate\":13}]}"
  SO_ID=$(data '.data.id')
  if [ "$CODE" = "0" ] && [ -n "$SO_ID" ]; then
    ok "订单 $(data '.data.so_no') id=$SO_ID 不含税=$(data '.data.total_amount') 税额=$(data '.data.total_tax') 价税合计=$(data '.data.total_with_tax') 状态=$(data '.data.status')"
  else
    bad "订单创建 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 400)"
  fi
  SO_ITEM1=$(data '.data.items[0].id'); SO_ITEM2=$(data '.data.items[1].id')
  say "    明细：line1=$SO_ITEM1 line2=$SO_ITEM2"
  snap step02_so "select 'so: '||so_no||'|status='||status||'|amt='||total_amount||'|tax='||total_tax||'|total='||total_with_tax||'|cust='||(select name from biz_customer where id=customer_id) from biz_sales_order where id='$SO_ID'
                  select 'item: '||i.line_no||'|'||m.material_code||'|qty='||i.qty||'|price='||i.price||'|amt='||i.amount||'|tax_rate='||i.tax_rate||'|tax_amt='||i.tax_amount||'|delivered='||i.delivered_qty from biz_sales_order_item i join biz_material m on m.id=i.material_id where i.so_id='$SO_ID' order by i.line_no"

  api step02_submit POST "/business/erp/sales-order/$SO_ID/submit-approval" "$T_ADMIN" '{}'
  check "提交审批后订单状态" "$(data '.data.status')" "SUBMITTED"
  INST=$(data '.data.approval_instance_no // empty')
  TASK=$(PSQLQ -c "select t.id from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.biz_id='$SO_ID' and t.status='PENDING' order by t.created_at desc limit 1")
  if [ -n "$TASK" ]; then ok "审批待办 task=$TASK 实例=${INST:-（见快照）}"; else bad "未取到审批待办（so=$SO_ID）"; fi
  snap step02_submitted "select 'so:'||status from biz_sales_order where id='$SO_ID'
                         select 'inst: '||instance_no||'|'||status||'|'||biz_type||'|'||flow_code from biz_flow_instance where biz_id='$SO_ID'
                         select 'task: '||node_key||'|'||status||'|assignee='||coalesce(assignee_id::text,'-') from biz_flow_task where instance_id in (select id from biz_flow_instance where biz_id='$SO_ID') order by created_at"

  # so_approve = 销售主管(n_sales) → 金额分级 → 财务经理(n_finance)，≥10 万再加总经理(n_gm)
  # 逐节点审批直到实例无待办（审批人恒为 so24_sales_mgr ≠ 制单人 admin）
  local r=0 t tasks
  while [ "$r" -lt 6 ]; do
    tasks=$(PSQLQ -c "select t.id from biz_flow_task t where t.status='PENDING' and t.instance_id in (select id from biz_flow_instance where biz_id='$SO_ID') order by t.created_at")
    [ -z "$tasks" ] && break
    for t in $tasks; do
      api "step02_approve_r$((r+1))" POST "/business/flow/task/$t/approve" "$T_APP" '{"comment":"SO-24 e2e 审批同意"}'
      if [ "$CODE" = "0" ]; then ok "审批通过 task=$t（第 $((r+1)) 轮）"; else bad "审批失败 task=$t http=$HTTP body=$(printf '%s' "$BODY" | head -c 400)"; fi
    done
    r=$((r+1))
  done
  SO_STATUS=$(PSQLQ -c "select status from biz_sales_order where id='$SO_ID'")
  check "审批后订单状态" "$SO_STATUS" "APPROVED"
  snap step02_approved "select 'so:'||status||'|approval_inst='||coalesce(approval_instance_id::text,'-') from biz_sales_order where id='$SO_ID'
                        select 'inst:'||status||'|ended='||coalesce(ended_at::text,'-') from biz_flow_instance where biz_id='$SO_ID'
                        select 'task: '||node_key||'|'||status||'|'||coalesce(assignee_name,'-')||'|'||coalesce(comment,'-') from biz_flow_task where instance_id in (select id from biz_flow_instance where biz_id='$SO_ID') order by created_at"

  # =========================== ③ 发货单（部分发货）===========================
  say; say "③ 建发货单，只发 1 个物料（line1 全量 100）"
  api step03_shipment_create POST /business/erp/shipment "$T_ADMIN" "{
    \"so_id\":\"$SO_ID\",\"warehouse_id\":\"d1370001-0000-4000-8000-000000000001\",\"ship_date\":\"2026-11-06\",
    \"remark\":\"SO-24 部分发货①\",\"items\":[{\"so_item_id\":\"$SO_ITEM1\",\"qty\":100}]}"
  SH1=$(data '.data.id')
  if [ "$CODE" = "0" ] && [ -n "$SH1" ]; then ok "发货单 $(data '.data.ship_no') id=$SH1 状态=$(data '.data.status')"; else bad "发货单创建 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 400)"; fi
  snap step03 "select 'sh: '||ship_no||'|'||status||'|so='||(select so_no from biz_sales_order where id=so_id) from biz_shipment where id='$SH1'
               select 'sh_item: so_line='||coalesce((select line_no from biz_sales_order_item where id=so_item_id)::text,'-')||'|qty='||qty||'|mat='||material_id from biz_shipment_item where shipment_id='$SH1'
               select 'so:'||status||'|delivered(line1)='||(select delivered_qty from biz_sales_order_item where id='$SO_ITEM1') from biz_sales_order where id='$SO_ID'
               select 'inv_qty: '||(select string_agg(m.material_code||'='||i.qty,', ' order by m.material_code) from biz_inventory i join biz_material m on m.id=i.material_id)"

  # =========================== ④ 过账 ===========================
  say; say "④ 提交 + 过账（DRAFT → PENDING_POST → POSTED）"
  api step04_submit PATCH "/business/erp/shipment/$SH1/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
  check "发货单提交后状态" "$(data '.data.status')" "PENDING_POST"
  api step04_post PATCH "/business/erp/shipment/$SH1/status" "$T_ADMIN" '{"target_status":"POSTED"}'
  if [ "$CODE" = "0" ]; then ok "过账成功 状态=$(data '.data.status') 出库单=$(data '.data.stock_out_no')"; else bad "过账失败 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 400)"; fi
  snap step04_after_post \
    "select 'sh:'||status||'|out='||coalesce(stock_out_id::text,'-') from biz_shipment where id='$SH1'
     select 'stockout: '||out_no||'|out_type='||out_type||'|source_type='||source_type||'|amt='||total_amount||'|status='||status from biz_stock_out where id=(select stock_out_id from biz_shipment where id='$SH1')
     select 'inv: '||m.material_code||' qty='||i.qty||' amt='||i.amount||' avg='||i.avg_cost from biz_inventory i join biz_material m on m.id=i.material_id order by m.material_code
     select 'loc: '||m.material_code||' qty='||l.qty from biz_inventory_location l join biz_material m on m.id=l.material_id order by 1
     select 'txn: '||txn_type||'|'||biz_type||'|'||m.material_code||'|'||qty||'|'||amount from biz_inventory_txn t join biz_material m on m.id=t.material_id order by t.created_at
     select 'voucher: '||voucher_no||'|'||voucher_type||'|'||total_debit||'/'||total_credit||'|'||source_type from biz_voucher order by created_at
     select 'entry: '||e.account_code||'|'||e.account_name||'|D='||e.debit_amount||'|C='||e.credit_amount from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id order by v.created_at, e.line_no
     select 'ar: '||ar_no||'|'||status||'|amt='||amount||'|received='||received_amount||'|unsettled='||unsettled_amount||'|src='||source_type from biz_receivable order by created_at
     select 'so:'||status||'|delivered(line1)='||(select delivered_qty from biz_sales_order_item where id='$SO_ITEM1') from biz_sales_order where id='$SO_ID'"

  check "订单状态（部分发货）" "$(PSQLQ -c "select status from biz_sales_order where id='$SO_ID'")" "PARTIAL_OUT"
  check "line1 delivered_qty" "$(PSQLQ -c "select delivered_qty from biz_sales_order_item where id='$SO_ITEM1'")" "100.0000"
  check "M-W1A-001 库存 qty（200-100）" "$(PSQLQ -c "select i.qty from biz_inventory i where i.material_id='d1370003-0000-4000-8000-000000000001'")" "100.0000"
  check "销售出库单生成数（source_type=SALES）" "$(PSQLQ -c "select count(*) from biz_stock_out where source_type='SALES'")" "1"
  check "应收生成数" "$(PSQLQ -c "select count(*) from biz_receivable where source_type='SALES_OUT'")" "1"
  check "AR 金额（line1 含税 33900）" "$(PSQLQ -c "select amount from biz_receivable order by created_at limit 1")" "33900.0000"
  check "成本结转凭证分录（source_type=STOCK_OUT，借6401/贷1405）" "$(PSQLQ -c "select count(*) from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id where v.source_type='STOCK_OUT' and e.account_code in ('6401','1405')")" "2"
  check "成本结转成本额（借6401 = 100×300）" "$(PSQLQ -c "select sum(e.debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='STOCK_OUT' and e.account_code='6401'")" "30000.0000"
  check "成本结转凭证张数（每张出库单 1 张）" "$(PSQLQ -c "select count(*) from biz_voucher where source_type='STOCK_OUT'")" "1"
  # 三账一致（库存表 = 库位账 Σ = 流水净额）
  check "三账一致 M1（inv=loc=txn）" "$(PSQLQ -c "select i.qty||'/'||coalesce(l.qty,0)||'/'||coalesce(t.net,0) from biz_inventory i left join (select material_id,sum(qty) qty from biz_inventory_location group by material_id) l on l.material_id=i.material_id left join (select material_id,sum(case when txn_type='IN' then qty else -qty end) net from biz_inventory_txn group by material_id) t on t.material_id=i.material_id where i.material_id='d1370003-0000-4000-8000-000000000001'")" "100.0000/100.0000/100.0000"

  # =========================== ⑤ 第二张发货单 ===========================
  say; say "⑤ 第二张发货单发剩余（line2 全量 50）→ 订单 OUTBOUND"
  api step05_shipment_create POST /business/erp/shipment "$T_ADMIN" "{
    \"so_id\":\"$SO_ID\",\"warehouse_id\":\"d1370001-0000-4000-8000-000000000001\",\"ship_date\":\"2026-11-06\",
    \"remark\":\"SO-24 剩余发货②\",\"items\":[{\"so_item_id\":\"$SO_ITEM2\",\"qty\":50}]}"
  SH2=$(data '.data.id')
  [ -n "$SH2" ] && [ "$SH2" != "null" ] && ok "发货单② $(data '.data.ship_no') id=$SH2" || bad "发货单②创建 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 300)"
  api step05_submit PATCH "/business/erp/shipment/$SH2/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
  api step05_post PATCH "/business/erp/shipment/$SH2/status" "$T_ADMIN" '{"target_status":"POSTED"}'
  if [ "$CODE" = "0" ]; then ok "发货单②过账 状态=$(data '.data.status')"; else bad "发货单②过账 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 300)"; fi
  check "订单状态（全部发完）" "$(PSQLQ -c "select status from biz_sales_order where id='$SO_ID'")" "OUTBOUND"
  check "line2 delivered_qty" "$(PSQLQ -c "select delivered_qty from biz_sales_order_item where id='$SO_ITEM2'")" "50.0000"
  check "M-W1A-002 库存 qty（100-50）" "$(PSQLQ -c "select i.qty from biz_inventory i where i.material_id='d1370003-0000-4000-8000-000000000002'")" "50.0000"
  check "应收合计（2 张 = 56500）" "$(PSQLQ -c "select sum(amount) from biz_receivable where source_type='SALES_OUT'")" "56500.0000"
  snap step05 "select 'so:'||status from biz_sales_order where id='$SO_ID'
               select 'sh: '||ship_no||'|'||status from biz_shipment where so_id='$SO_ID' order by created_at
               select 'ar: '||ar_no||'|'||status||'|'||amount||'|'||unsettled_amount from biz_receivable order by created_at
               select 'inv: '||m.material_code||' qty='||i.qty||' amt='||i.amount from biz_inventory i join biz_material m on m.id=i.material_id order by 1"

  # =========================== ⑥ 开票 ===========================
  say; say "⑥ 开票：按 2 张应收分别开（不含税 30000 + 20000 = 50000，税率 13%，价税合计 56500）"
  AR1=$(PSQLQ -c "select id from biz_receivable order by created_at limit 1")
  AR2=$(PSQLQ -c "select id from biz_receivable order by created_at desc limit 1")
  api step06_invoice1 POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR1\",\"invoice_date\":\"2026-11-06\",\"amount\":30000,\"tax_rate\":0.13,\"remark\":\"SO-24 发票①\"}"
  IV1=$(data '.data.id')
  if [ "$CODE" = "0" ]; then ok "发票① $(data '.data.invoice_no') 不含税=30000 税=3900 合计=$(data '.data.total_with_tax') 状态=$(data '.data.status')"; else bad "发票① http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 400)"; fi
  api step06_invoice2 POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR2\",\"invoice_date\":\"2026-11-06\",\"amount\":20000,\"tax_rate\":0.13,\"remark\":\"SO-24 发票②\"}"
  IV2=$(data '.data.id')
  if [ "$CODE" = "0" ]; then ok "发票② $(data '.data.invoice_no') 不含税=20000 税=2600 合计=$(data '.data.total_with_tax') 状态=$(data '.data.status')"; else bad "发票② http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 400)"; fi
  api step06_issue1 PATCH "/business/erp/invoice/$IV1/status" "$T_ADMIN" '{"status":"ISSUED"}'
  check "发票① 开票后状态" "$(data '.data.status')" "ISSUED"
  api step06_issue2 PATCH "/business/erp/invoice/$IV2/status" "$T_ADMIN" '{"status":"ISSUED"}'
  check "发票② 开票后状态" "$(data '.data.status')" "ISSUED"

  # =========================== ⑦ 收入凭证 ===========================
  say; say "⑦ 验证收入凭证（2 张合计：借 1122 56500 / 贷 5001 50000 + 2221.02 6500）"
  check "收入凭证张数（source_type=INVOICE）" "$(PSQLQ -c "select count(*) from biz_voucher where source_type='INVOICE'")" "2"
  check "借 1122 合计" "$(PSQLQ -c "select sum(e.debit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and e.account_code='1122'")" "56500.0000"
  check "贷 5001 合计（收入）" "$(PSQLQ -c "select sum(e.credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and e.account_code='5001'")" "50000.0000"
  check "贷 2221.02 合计（销项税）" "$(PSQLQ -c "select sum(e.credit_amount) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and e.account_code='2221.02'")" "6500.0000"
  snap step07_revenue_voucher \
    "select 'v: '||v.voucher_no||'|'||v.voucher_type||'|'||v.total_debit||'/'||v.total_credit||'|src='||v.source_type from biz_voucher v where v.source_type='INVOICE' order by v.created_at
     select 'e: '||v.voucher_no||'|'||e.account_code||'|'||e.account_name||'|D='||e.debit_amount||'|C='||e.credit_amount from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' order by v.created_at, e.line_no
     select 'iv: '||invoice_no||'|'||status||'|amt='||amount||'|tax='||tax_amount||'|total='||total_with_tax||'|ar='||coalesce((select ar_no from biz_receivable where id=receivable_id),'-') from biz_invoice order by created_at"

  # =========================== ⑧ 收款 + 核销 ===========================
  say; say "⑧ 收款 5.65 万，核销到 2 张应收"
  api step08_receipt_create POST /business/erp/receipt "$T_ADMIN" "{\"customer_id\":\"$CUST\",\"receipt_date\":\"2026-11-06\",\"amount\":56500,\"method\":\"BANK_TRANSFER\",\"bank_account\":\"6222020200000000001\",\"remark\":\"SO-24 全额收款\"}"
  RC=$(data '.data.id')
  if [ "$CODE" = "0" ]; then ok "收款单 $(data '.data.receipt_no') id=$RC 金额=$(data '.data.amount') 状态=$(data '.data.status')"; else bad "收款单 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 400)"; fi
  api step08_allocate POST "/business/erp/receipt/$RC/allocate" "$T_ADMIN" "{\"items\":[{\"receivable_id\":\"$AR1\",\"amount\":33900},{\"receivable_id\":\"$AR2\",\"amount\":22600}]}"
  if [ "$CODE" = "0" ]; then ok "核销成功（2 张应收，合计 56500）"; else bad "核销 http=$HTTP body=$(printf \'%s\' "$BODY" | head -c 400)"; fi
  api step08_confirm PATCH "/business/erp/receipt/$RC/status" "$T_ADMIN" '{"status":"CONFIRMED"}'
  check "收款单确认后状态" "$(data '.data.status')" "CONFIRMED"
  api step08_post PATCH "/business/erp/receipt/$RC/status" "$T_ADMIN" '{"status":"POSTED"}'
  check "收款单过账后状态" "$(data '.data.status')" "POSTED"

  # =========================== ⑨ AR SETTLED / credit_used ===========================
  say; say "⑨ 验证：AR 全部 SETTLED，客户信用占用归零"
  check "AR SETTLED 张数" "$(PSQLQ -c "select count(*) from biz_receivable where status='SETTLED'")" "2"
  check "AR 未收余额合计" "$(PSQLQ -c "select coalesce(sum(unsettled_amount),0) from biz_receivable")" "0.0000"
  check "客户信用占用（未结应收，应归零）" "$(PSQLQ -c "select coalesce(sum(unsettled_amount),0) from biz_receivable where customer_id='$CUST'")" "0.0000"
  check "收款凭证（借1002/贷1122）" "$(PSQLQ -c "select count(*) from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id where v.source_type='RECEIPT' and e.account_code in ('1002','1122')")" "2"
  # 三账一致：全物料（库存表 = 库位账 Σ = 流水净额），不一致行数必须为 0
  check "三账一致（全物料不一致行数）" "$(PSQLQ -c "select count(*) from biz_inventory i left join (select material_id,warehouse_id,sum(qty) qty from biz_inventory_location group by 1,2) l on l.material_id=i.material_id and l.warehouse_id=i.warehouse_id left join (select material_id,warehouse_id,sum(case when txn_type='IN' then qty else -qty end) net from biz_inventory_txn group by 1,2) t on t.material_id=i.material_id and t.warehouse_id=i.warehouse_id where i.qty <> coalesce(l.qty,0) or i.qty <> coalesce(t.net,0)")" "0"
  snap step09_final \
    "select 'ar: '||ar_no||'|'||status||'|amt='||amount||'|received='||received_amount||'|unsettled='||unsettled_amount from biz_receivable order by created_at
     select 'alloc: '||(select ar_no from biz_receivable where id=a.receivable_id)||'|'||a.amount from biz_receipt_allocation a order by a.created_at
     select 'rc: '||receipt_no||'|'||status||'|amt='||amount||'|allocated='||allocated_amount from biz_receipt
     select 'cust_credit: '||code||'|limit='||credit_limit||'|open_ar='||(select coalesce(sum(unsettled_amount),0) from biz_receivable where customer_id=biz_customer.id) from biz_customer where code='C-SO24-001'
     select 'voucher_all: '||voucher_no||'|'||voucher_type||'|'||total_debit||'/'||total_credit from biz_voucher order by created_at"

  # =========================== ⑩ 不变量 ===========================
  say; say "⑩ erp_invariants.py（销售组 + 全量）"
  local inv_log="$RAW/step10_invariants.txt"
  NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME="$DB" \
    python3 "$DIR/erp_invariants.py" --group 销售 > "$inv_log" 2>&1
  tail -25 "$inv_log"
  local summary=$(grep -E '结果：[0-9]+/[0-9]+ 通过' "$inv_log" | tail -1 | sed 's/^ *//')
  say "    销售组：$summary"
  if printf '%s' "$summary" | grep -q '8/8 通过'; then ok "⑩ 销售组 8/8 全绿（非空覆盖）"; else bad "⑩ 销售组未全绿：$summary"; fi

  # 全量巡检（看是否引入新红；H6/H5/N2 是 SO-23 已登记的既有项，见 README §7）
  local full_log="$RAW/step10_invariants_full.txt"
  NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME="$DB" \
    python3 "$DIR/erp_invariants.py" > "$full_log" 2>&1
  say "    全量：$(grep -E '结果：[0-9]+/[0-9]+ 通过' "$full_log" | tail -1 | sed 's/^ *//')"
  grep -E '❌|BLOCKER [1-9]|WARN [1-9]' "$full_log" | head -12 | sed 's/^/      /' 

  say; say "单据汇总：$(PSQLQ -c "select 'SO='||(select count(*) from biz_sales_order)||' SH='||(select count(*) from biz_shipment)||' 出库单='||(select count(*) from biz_stock_out)||' 应收='||(select count(*) from biz_receivable)||' 发票='||(select count(*) from biz_invoice)||' 收款='||(select count(*) from biz_receipt)||' 核销='||(select count(*) from biz_receipt_allocation)||' 凭证='||(select count(*) from biz_voucher)")"
  # 证据脱敏：refresh/access token 不落盘（JWT 是凭据，不是证据）
  local f
  for f in "$RAW"/*.json; do
    sed -i 's/"access_token":"[^"]*"/"access_token":"<redacted>"/g; s/"refresh_token":"[^"]*"/"refresh_token":"<redacted>"/g' "$f"
  done
  say; say "===== 汇总：PASS=$PASS FAIL=$FAIL ====="
  [ "$FAIL" -eq 0 ] || say "失败项：${FAILED_STEPS[*]}"
  return 0
}

case "${1:-up}" in
  up) up ;;
  chain) chain ;;
  down) stop_instance; say "stopped" ;;
  *) say "用法：$0 {up|chain|down}"; exit 2 ;;
esac
