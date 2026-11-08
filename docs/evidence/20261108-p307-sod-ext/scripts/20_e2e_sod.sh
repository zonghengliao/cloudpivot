#!/usr/bin/env bash
# ============================================================================
# P3-07 验收：SoD 扩展开关（approve_execute / stocktake / purchase_accept）真 HTTP 实测
#
# 卡面验收原文逐条对照：
#   ① 同一人审批付款又执行 → 409；换人 → 通过
#   ② 制单人确认自己的盘点单 → 409；换人 → 通过
#   ③ 采购员验收自己 PO 的收货 → 409；换人 → 通过
#   ④ 三个开关**关闭时三条都必须放行**（证明默认不改变行为）
#   ⑤ 回归：原有 approve / post / pay 三闸行为不变；总闸 enabled 一键开启仍生效
#
# 环境：库 cloudpivot_p307_ut / 端口 18311（与 P3-05 的 18306、P3-06 的 18307 分开）
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

BASE=${BASE:-http://127.0.0.1:18311}
DB=${DB:-cloudpivot_p307_ut}
OUT="$RAW/20_e2e_sod.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

ADMIN_NEW=${ADMIN_NEW:-P307new@2026}
U_PW=${U_PW:-P307@2026}

ENVF=${ENVF:-/root/p307/instance.env}
BIN=${BIN:-/root/p307/bin/cloudpivot}
RUN_DIR=/opt/cloudpivot/backend
ILOG=/root/p307/e2e_instance.log
IPID=/root/p307/e2e.pid

ADMIN_PW=${ADMIN_PW:-$(grep '^NIK_ADMIN_PASSWORD=' "$ENVF" | cut -d= -f2-)}

stop_instance() {
  local pid
  pid=$(cat "$IPID" 2>/dev/null) || return 0
  kill "$pid" 2>/dev/null || return 0
  for _ in $(seq 1 30); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  kill -9 "$pid" 2>/dev/null || true
}

start_instance() {
  local i pid
  ( cd "$RUN_DIR" && setsid env $(grep -v '^#' "$ENVF" | grep -v '^$' | tr '\n' ' ') \
      NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:18311" \
      "$BIN" >"$ILOG" 2>&1 </dev/null & )
  for i in $(seq 1 120); do
    pid=$(ss -ltnp 2>/dev/null | sed -n "s/.*127\.0\.0\.1:18311 .*pid=\([0-9]*\).*/\1/p" | head -1)
    if [ -n "$pid" ]; then echo "$pid" >"$IPID"; return 0; fi
    sleep 1
  done
  return 1
}

wait_http() {
  local i=0
  while [ "$i" -lt 180 ]; do
    if curl -s -o /dev/null "$BASE/health"; then return 0; fi
    sleep 1
    i=$((i + 1))
  done
  return 1
}

say "================ P3-07 SoD 扩展开关真 HTTP 实测（$BASE / $DB）================"
stop_instance
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun $DB" || { bad "建空库失败"; summary; }
start_instance || { bad "实例没起来"; tail -20 "$ILOG"; summary; }
wait_http || { bad "实例 180s 内没起来"; tail -40 "$ILOG"; summary; }
say "空库首启：$(grep -oE '应用 [0-9]+ 个迁移' "$ILOG" | tail -1)"
ok "隔离实例就绪（:18311 / $DB）"

# ---------- 0) 迁移后参数就位（V0340） ----------
eq "V0340 三条扩展开关默认值 = false（3 条）" \
  "$(q "$DB" "select count(*) from sys_config where config_key in (
        'erp.separation_of_duties.approve_execute',
        'erp.separation_of_duties.stocktake',
        'erp.separation_of_duties.purchase_accept') and config_value='false' and status=1 and deleted_at is null")" "3"
eq "erp.separation_of_duties.* 共 8 条（V0196 四条 + P3-05 一条 + 本卡三条）" \
  "$(q "$DB" "select count(*) from sys_config where config_key like 'erp.separation_of_duties.%' and deleted_at is null")" "8"
eq "三条扩展开关落在「ERP 内控」分组且类型 BOOLEAN（系统参数页可直接开关）" \
  "$(q "$DB" "select count(*) from sys_config where config_key in (
        'erp.separation_of_duties.approve_execute',
        'erp.separation_of_duties.stocktake',
        'erp.separation_of_duties.purchase_accept')
        and category='erp_internal_control' and config_type='BOOLEAN' and is_modifiable")" "3"

# ---------- 1) 管理员登录 ----------
RC=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"admin\",\"password\":\"$ADMIN_PW\"}")
if [ "$RC" = "200" ]; then
  A=$(body | jget data.access_token)
  MUST=$(body | jget data.must_change_password | tr 'A-Z' 'a-z')
else
  RC=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"admin\",\"password\":\"$ADMIN_NEW\"}")
  A=$(body | jget data.access_token)
  MUST=false
fi
[ -n "$A" ] || { bad "管理员登录失败：$(body)"; summary; }
ok "管理员登录成功"
if [ "$MUST" = "true" ]; then
  RC=$(http PUT "$BASE/api/system/auth/password" "$A" \
    "{\"old_password\":\"$ADMIN_PW\",\"new_password\":\"$ADMIN_NEW\"}")
  eq "首登改密 -> 200" "$RC" "200"
  RC=$(http POST "$BASE/api/system/auth/login" "" "{\"username\":\"admin\",\"password\":\"$ADMIN_NEW\"}")
  A=$(body | jget data.access_token)
  [ -n "$A" ] || { bad "改密后重登失败"; summary; }
  ok "改密后新令牌就绪"
fi

# ---------- 2) 角色 / 用户 / 权限码 ----------
mk_role() {
  local code=$1 name=$2 rc
  rc=$(http POST "$BASE/api/system/role" "$A" \
    "{\"role_code\":\"$code\",\"role_name\":\"$name\",\"status\":1,\"sort_order\":975}")
  [ "$rc" = "200" ] || { say "  [curl] role $code http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}
mk_user() {
  local uname=$1 real=$2 role=$3 rc
  rc=$(http POST "$BASE/api/system/user" "$A" \
    "{\"username\":\"$uname\",\"password\":\"$U_PW\",\"real_name\":\"$real\",\"status\":1,\"role_ids\":[\"$role\"]}")
  [ "$rc" = "200" ] || { say "  [curl] user $uname http=$rc $(body)" >&2; return 1; }
  body | jget data.id
}
grant() { # grant ROLE_ID CODES... -> 实际持有的码数
  local role=$1; shift
  local list
  list=$(printf "'%s'," "$@")
  q "$DB" "INSERT INTO sys_role_menu (role_id, menu_id, created_at)
           SELECT '$role'::uuid, m.id, now() FROM sys_menu m
            WHERE m.auth_code IN (${list%,}) AND m.deleted_at IS NULL
              AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id='$role'::uuid AND rm.menu_id=m.id)" >/dev/null
  q "$DB" "SELECT count(DISTINCT m.auth_code) FROM sys_role_menu rm
             JOIN sys_menu m ON m.id = rm.menu_id
            WHERE rm.role_id='$role'::uuid AND m.auth_code IN (${list%,})"
}

R_MAKER=$(mk_role P307_MAKER "P3-07 制单人（采购/盘点/付款）")
R_BOSS=$(mk_role P307_BOSS "P3-07 审批人")
R_CASH=$(mk_role P307_CASH "P3-07 执行人（出纳/仓管）")
is_uuid "$R_MAKER" && is_uuid "$R_BOSS" && is_uuid "$R_CASH" && ok "3 个角色就绪" \
  || { bad "建角色失败：$(body)"; summary; }

U_MAKER=$(mk_user p307_maker "P3-07 制单人甲" "$R_MAKER") || { bad "建用户 maker 失败"; summary; }
U_BOSS=$(mk_user p307_boss "P3-07 审批人乙" "$R_BOSS") || { bad "建用户 boss 失败"; summary; }
U_CASH=$(mk_user p307_cash "P3-07 执行人丙" "$R_CASH") || { bad "建用户 cash 失败"; summary; }
ok "3 个用户就绪（maker=$U_MAKER boss=$U_BOSS cash=$U_CASH）"

GRANT_BIZ=(business:flow:instance:create business:flow:instance:read business:flow:approve \
  business:erp:purchase-order:create business:erp:purchase-order:list \
  business:erp:purchase-order:status business:erp:purchase-order:submit-approval \
  business:erp:stock-in:create business:erp:stock-in:update business:erp:stock-in:list \
  business:erp:stock-in:detail business:erp:stock-in:status business:erp:stock-in:submit-post-approval \
  business:erp:stock-take:create business:erp:stock-take:update business:erp:stock-take:delete \
  business:erp:stock-take:status business:erp:stock-take:list business:erp:stock-take:detail \
  business:erp:payable:pay business:erp:payable:detail)
eq "制单人持有 21 个业务码" "$(grant "$R_MAKER" "${GRANT_BIZ[@]}")" "21"
eq "执行人持有同一批业务码" "$(grant "$R_CASH" "${GRANT_BIZ[@]}")" "21"
eq "审批人持有 3 个流程码" "$(grant "$R_BOSS" business:flow:instance:create business:flow:instance:read business:flow:approve)" "3"
# 种子流程（po_approve / stock_in_post）的审批节点是角色口径：把 boss 放进这些角色，
# 让「审批」这件事有唯一、确定的执行人（不新造流程，避免和种子默认流程打架）
for rc in DEPT_LEADER FINANCE_MANAGER PURCHASE_MANAGER GENERAL_MANAGER WAREHOUSE_MANAGER FINANCE_DIRECTOR; do
  q "$DB" "insert into sys_user_role (user_id, role_id)
           select '$U_BOSS'::uuid, id from sys_role where role_code='$rc' and deleted_at is null
           on conflict do nothing" >/dev/null
done
ok "boss 已进入种子审批角色（DEPT_LEADER / FINANCE_MANAGER / PURCHASE_MANAGER / GENERAL_MANAGER / WAREHOUSE_MANAGER / FINANCE_DIRECTOR）"

TOK_MAKER=$(login p307_maker "$U_PW" "$U_PW") || { bad "maker 登录失败"; summary; }
TOK_BOSS=$(login p307_boss "$U_PW" "$U_PW") || { bad "boss 登录失败"; summary; }
TOK_CASH=$(login p307_cash "$U_PW" "$U_PW") || { bad "cash 登录失败"; summary; }
ok "3 个业务令牌就绪"

# ---------- 3) 审批流程夹具（只用自建的两条，po_approve / stock_in_post 用种子流程） ----------
mk_flow() { # mk_flow CODE NAME BIZ ROLE -> def_id
  local code=$1 name=$2 biz=$3 role=$4 rc def vid graph
  rc=$(http POST "$BASE/api/business/flow/def" "$A" \
    "{\"flow_code\":\"$code\",\"flow_name\":\"$name\",\"biz_type\":\"$biz\",\"is_default\":true}")
  [ "$rc" = "200" ] || { say "  [curl] def $code http=$rc $(body)" >&2; return 1; }
  def=$(body | jget data.id); vid=$(body | jget data.version_id)
  graph=$(cat <<JSON
{
  "change_note": "P3-07 夹具：单审批节点（ROLE=$role）",
  "nodes": [
    {"key":"s","name":"开始","type":"START","x":0,"y":0,"sort_order":1},
    {"key":"n_approve","name":"主管审批","type":"APPROVAL","x":220,"y":0,"sort_order":2,
     "assignee_type":"ROLE","assignees":[{"type":"ROLE","ref_id":"$role","sort_order":0}],
     "approval_mode":"ANY","self_approval":"SKIP","empty_assignee":"ERROR",
     "join_mode":"AND","fork_mode":"AND","reject_veto":true},
    {"key":"e","name":"结束","type":"END","x":440,"y":0,"sort_order":3}
  ],
  "edges": [
    {"key":"e_start","from":"s","to":"n_approve"},
    {"key":"e_end","from":"n_approve","to":"e"}
  ]
}
JSON
)
  rc=$(http PUT "$BASE/api/business/flow/version/$vid/graph" "$A" "$graph")
  [ "$rc" = "200" ] || { say "  [curl] graph $code http=$rc $(body)" >&2; return 1; }
  rc=$(http POST "$BASE/api/business/flow/def/$def/publish" "$A" "")
  [ "$rc" = "200" ] || { say "  [curl] publish $code http=$rc $(body)" >&2; return 1; }
  printf '%s' "$def"
}
F_ST=$(mk_flow p307_stock_take_post "P3-07 盘点过账审批" STOCK_TAKE_POST "$R_BOSS")
is_uuid "$F_ST" && ok "夹具流程 p307_stock_take_post 已发布（单节点 boss，is_default=true）" \
  || { bad "盘点流程发布失败：$(body)"; summary; }
F_PAY=$(mk_flow p307_payable_pay "P3-07 付款审批" PAYABLE_PAY "$R_BOSS")
is_uuid "$F_PAY" && ok "夹具流程 p307_payable_pay 已发布（单节点 boss，is_default=true）" \
  || { bad "付款流程发布失败：$(body)"; summary; }

# ---------- 4) 主数据（admin 建，SUPER_ADMIN 跳过权限） ----------
UNIT=$(q "$DB" "select id from biz_unit where unit_code='PCS' and deleted_at is null limit 1")
[ -n "$UNIT" ] || UNIT=$(q "$DB" "select id from biz_unit where deleted_at is null order by unit_code limit 1")
is_uuid "$UNIT" && ok "计量单位就绪（$UNIT）" || { bad "库里没有任何计量单位"; summary; }

RC=$(http POST "$BASE/api/business/erp/supplier" "$A" \
  "{\"supplier_code\":\"SUP-P307\",\"supplier_name\":\"P3-07 证据供应商\",\"status\":1}")
[ "$RC" = "200" ] || { bad "建供应商失败：$(body)"; summary; }
SUP=$(body | jget data.id); ok "供应商 SUP-P307 就绪"

RC=$(http POST "$BASE/api/business/erp/warehouse" "$A" \
  "{\"warehouse_code\":\"WH-P307\",\"warehouse_name\":\"P3-07 证据仓\",\"warehouse_type\":\"NORMAL\",\"level\":1,\"status\":1}")
[ "$RC" = "200" ] || { bad "建仓库失败：$(body)"; summary; }
WH=$(body | jget data.id); ok "仓库 WH-P307 就绪"

RC=$(http POST "$BASE/api/business/erp/location" "$A" \
  "{\"location_code\":\"LOC-P307-01\",\"location_name\":\"P3-07 一号位\",\"warehouse_id\":\"$WH\",\"location_type\":\"STORAGE\",\"is_default\":true,\"status\":1}")
[ "$RC" = "200" ] || { bad "建库位失败：$(body)"; summary; }
LOC=$(body | jget data.id); ok "库位 LOC-P307-01 就绪"

RC=$(http POST "$BASE/api/business/erp/material" "$A" \
  "{\"material_code\":\"MAT-P307\",\"material_name\":\"P3-07 证据物料\",\"spec\":\"SOD\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"costing_method\":\"MOVING_AVG\",\"tax_rate\":0,\"safety_stock\":0,\"status\":1}")
[ "$RC" = "200" ] || { bad "建物料失败：$(body)"; summary; }
MAT=$(body | jget data.id); ok "物料 MAT-P307 就绪"

# ---------- 5) 通用工具：审批 / 开关 / 采购单 ----------
approve_all() { # approve_all TOKEN INSTANCE_ID -> 连续同意所有 PENDING 待办，最多 12 轮
  local tok=$1 inst=$2 i tid rc
  for i in $(seq 1 12); do
    tid=$(q "$DB" "select id from biz_flow_task where instance_id='$inst'::uuid and status='PENDING' order by created_at limit 1")
    [ -n "$tid" ] || return 0
    rc=$(http POST "$BASE/api/business/flow/task/$tid/approve" "$tok" "{\"comment\":\"P3-07 证据：同意\"}")
    [ "$rc" = "200" ] || { say "  [curl] approve $tid http=$rc $(body)"; return 1; }
  done
  return 0
}
set_switch() { # set_switch KEY true|false
  q "$DB" "update sys_config set config_value='$2', updated_at=now() where config_key='$1'" >/dev/null
  local rc
  rc=$(http POST "$BASE/api/support/config/reload" "$A" "")
  [ "$rc" = "200" ] || say "  [warn] config reload http=$rc"
}
K_PA="erp.separation_of_duties.purchase_accept"
K_ST="erp.separation_of_duties.stocktake"
K_AE="erp.separation_of_duties.approve_execute"
K_POST="erp.separation_of_duties.post"
K_PAY="erp.separation_of_duties.pay"
K_MASTER="erp.separation_of_duties.enabled"

TODAY=$(date +%F)
mk_po() { # mk_po TOKEN QTY -> 打印 PO id（建单 + 提交 + 审批全部走完）
  local tok=$1 qty=$2 rc po inst
  rc=$(http POST "$BASE/api/business/erp/purchase-order" "$tok" \
    "{\"purchase_type\":\"SPOT\",\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\",\"order_date\":\"$TODAY\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":$qty,\"price_excl\":10,\"tax_rate\":0}]}")
  [ "$rc" = "200" ] || { say "  [curl] PO create http=$rc $(body)" >&2; return 1; }
  po=$(body | jget data.order.id)
  rc=$(http PATCH "$BASE/api/business/erp/purchase-order/$po/status" "$tok" "{\"status\":\"SUBMITTED\"}")
  [ "$rc" = "200" ] || { say "  [curl] PO submit http=$rc $(body)" >&2; return 1; }
  inst=$(q "$DB" "select approval_instance_id from biz_purchase_order where id='$po'::uuid")
  if is_uuid "$inst"; then
    approve_all "$TOK_BOSS" "$inst" || { say "  [curl] PO approve 失败" >&2; return 1; }
  fi
  printf '%s' "$po"
}

PO=$(mk_po "$TOK_MAKER" 100)
is_uuid "$PO" || { bad "采购单没走通（建单 / 提交 / 审批）"; summary; }
PO_NO=$(q "$DB" "select order_no from biz_purchase_order where id='$PO'::uuid")
eq "PO 采购员口径 = biz_purchase_order.create_by" \
  "$(q "$DB" "select create_by from biz_purchase_order where id='$PO'::uuid")" "$U_MAKER"
eq "PO 审批后状态 = APPROVED（可收货）" \
  "$(q "$DB" "select status from biz_purchase_order where id='$PO'::uuid")" "APPROVED"
ok "采购订单 $PO_NO（采购员 = maker）就绪"
ITEM=$(q "$DB" "select id from biz_purchase_order_item where order_id='$PO'::uuid order by line_no limit 1")

mk_receipt_for() { # mk_receipt_for TOKEN PO_ID ITEM_ID QTY
  http POST "$BASE/api/business/erp/stock-in" "$1" \
    "{\"source_order_id\":\"$2\",\"warehouse_id\":\"$WH\",\"in_date\":\"$TODAY\",\"items\":[{\"purchase_order_item_id\":\"$3\",\"material_id\":\"$MAT\",\"qty\":$4,\"price_excl\":10,\"tax_rate\":0}]}"
}

# ============================================================================
say ""
say "=== S1 采购 ↔ 验收（purchase_accept）：采购员不能验收自己采的货 ==="
# ============================================================================
set_switch "$K_PA" false
RC=$(mk_receipt_for "$TOK_MAKER" "$PO" "$ITEM" 100)
eq "① 开关关闭（默认）→ 采购员收货 -> 200（默认不改变行为）" "$RC" "200"
SI_OFF=$(body | jget data.header.id)
is_uuid "$SI_OFF" && ok "关闭态收货单已建（$SI_OFF，制单人 = maker）" || bad "关闭态没建出来：$(body)"

set_switch "$K_PA" true
RC=$(mk_receipt_for "$TOK_MAKER" "$PO" "$ITEM" 100)
eq "③ 开关打开 → 采购员验收自己的 PO -> 409" "$RC" "409"
eq "   业务码 = 66013（SEPARATION_OF_DUTIES）" "$(body | jget code)" "66013"
contains "   文案点名职责对" "$(body | jget msg)" "采购与验收分离"
contains "   文案点名采购单" "$(body | jget msg)" "$PO_NO"
contains "   文案给出放行办法（开关键名）" "$(body | jget msg)" "$K_PA"
eq "   被拦后没有多出收货单" \
  "$(q "$DB" "select count(*) from biz_stock_in where source_order_id='$PO'::uuid and deleted_at is null")" "1"

RC=$(mk_receipt_for "$TOK_CASH" "$PO" "$ITEM" 100)
eq "④ 开关打开 → 换人（仓管）收货 -> 200" "$RC" "200"
SI_ON=$(body | jget data.header.id)
is_uuid "$SI_ON" && ok "打开态收货单已建（$SI_ON，制单人 = cash）" || { bad "换人也没建成：$(body)"; summary; }
set_switch "$K_PA" false

# ============================================================================
say ""
say "=== 前置：收货单过账（走种子 stock_in_post 流程，审批后自动过账 → 产生库存）==="
# ============================================================================
RC=$(http PATCH "$BASE/api/business/erp/stock-in/$SI_ON/status" "$TOK_CASH" "{\"status\":\"POSTED\"}")
eq "提交过账（走审批）-> 200" "$RC" "200"
SI_INST=$(q "$DB" "select approval_instance_id from biz_stock_in where id='$SI_ON'::uuid")
if is_uuid "$SI_INST"; then
  ok "收货单过账已起审批实例 $SI_INST"
  approve_all "$TOK_BOSS" "$SI_INST" || bad "过账审批没走完"
else
  ok "收货单过账由种子流程直接放行（金额低于阈值 → 只发知会，auto_finished 分支过账）"
fi
eq "收货单状态 = POSTED" "$(q "$DB" "select status from biz_stock_in where id='$SI_ON'::uuid")" "POSTED"
eq "库存已入账（MAT-P307 数量 100）" \
  "$(q "$DB" "select qty::text from biz_inventory where material_id='$MAT'::uuid")" "100.0000"

# ============================================================================
say ""
say "=== S2 制单 ↔ 盘点确认（stocktake）：制单人不能确认自己建的盘点单 ==="
# ============================================================================
mk_take() { # mk_take TOKEN -> take id
  http POST "$BASE/api/business/erp/stock-take" "$1" \
    "{\"warehouse_id\":\"$WH\",\"take_type\":\"FULL\",\"remark\":\"P3-07 证据盘点\"}" >/dev/null
  body | jget data.id
}
submit_take() { http PATCH "$BASE/api/business/erp/stock-take/$2/status" "$1" "{\"status\":\"PENDING_POST\"}"; }
task_of() { q "$DB" "select id from biz_flow_task where instance_id='$1'::uuid and status='PENDING' order by created_at limit 1"; }
inst_of_take() { q "$DB" "select approval_instance_id from biz_stock_take where id='$1'::uuid"; }

set_switch "$K_ST" false
TK_OFF=$(mk_take "$TOK_MAKER")
is_uuid "$TK_OFF" && ok "① 关闭态盘点单由 maker 建（$TK_OFF）" || { bad "建盘点单失败：$(body)"; summary; }
RC=$(submit_take "$TOK_MAKER" "$TK_OFF")
eq "① 开关关闭（默认）→ 制单人确认自己的盘点单 -> 200" "$RC" "200"
eq "   盘点单状态 = PENDING_POST" "$(q "$DB" "select status from biz_stock_take where id='$TK_OFF'::uuid")" "PENDING_POST"
I_OFF=$(inst_of_take "$TK_OFF"); is_uuid "$I_OFF" && ok "   已起审批实例 $I_OFF" || bad "   没起审批实例"

set_switch "$K_ST" true
TK_ON=$(mk_take "$TOK_MAKER")
RC=$(submit_take "$TOK_MAKER" "$TK_ON")
eq "③ 开关打开 → 制单人确认自己的盘点单 -> 409" "$RC" "409"
eq "   业务码 = 66013" "$(body | jget code)" "66013"
contains "   文案点名职责对" "$(body | jget msg)" "制单与盘点确认分离"
contains "   文案给出放行办法（开关键名）" "$(body | jget msg)" "$K_ST"
eq "   被拦后单据仍是 DRAFT（不许半推半就）" \
  "$(q "$DB" "select status from biz_stock_take where id='$TK_ON'::uuid")" "DRAFT"
eq "   被拦后没有起审批实例" "$(inst_of_take "$TK_ON")" ""

RC=$(submit_take "$TOK_CASH" "$TK_ON")
eq "④ 开关打开 → 换人（仓管）确认 -> 200" "$RC" "200"
eq "   盘点单状态 = PENDING_POST" "$(q "$DB" "select status from biz_stock_take where id='$TK_ON'::uuid")" "PENDING_POST"
set_switch "$K_ST" false

# ============================================================================
say ""
say "=== S3 审批 ↔ 执行（approve_execute）：盘点过账（人工执行点）==="
# ============================================================================
I_ON=$(inst_of_take "$TK_ON"); is_uuid "$I_ON" && ok "盘点单 $TK_ON 的审批实例 = $I_ON" || { bad "没实例"; summary; }
T_ON=$(task_of "$I_ON"); is_uuid "$T_ON" && ok "审批待办落在 boss（$T_ON）" || { bad "没有待办"; summary; }
RC=$(http POST "$BASE/api/business/flow/task/$T_ON/approve" "$TOK_BOSS" "{\"comment\":\"P3-07 证据：同意\"}")
eq "boss 审批通过 -> 200" "$RC" "200"
eq "审批实例 = APPROVED" "$(q "$DB" "select status from biz_flow_instance where id='$I_ON'::uuid")" "APPROVED"

set_switch "$K_AE" true
RC=$(http PATCH "$BASE/api/business/erp/stock-take/$TK_ON/status" "$TOK_BOSS" "{\"status\":\"POSTED\"}")
eq "③ 开关打开 → 审批人自己过账 -> 409" "$RC" "409"
eq "   业务码 = 66013" "$(body | jget code)" "66013"
contains "   文案点名职责对" "$(body | jget msg)" "审批与执行分离"
contains "   文案给出放行办法（开关键名）" "$(body | jget msg)" "$K_AE"
eq "   账一分没动（仍是 PENDING_POST）" \
  "$(q "$DB" "select status from biz_stock_take where id='$TK_ON'::uuid")" "PENDING_POST"

RC=$(http PATCH "$BASE/api/business/erp/stock-take/$TK_ON/status" "$TOK_CASH" "{\"status\":\"POSTED\"}")
eq "④ 开关打开 → 换人（没批过这张单的人）过账 -> 200" "$RC" "200"
eq "   盘点单状态 = POSTED（账已动）" "$(q "$DB" "select status from biz_stock_take where id='$TK_ON'::uuid")" "POSTED"

# 关闭态回归：审批人自己过账必须放行
TK_AE=$(mk_take "$TOK_CASH"); RC=$(submit_take "$TOK_CASH" "$TK_AE"); eq "关闭态夹具：换人建+确认 -> 200" "$RC" "200"
I_AE=$(inst_of_take "$TK_AE"); T_AE=$(task_of "$I_AE")
RC=$(http POST "$BASE/api/business/flow/task/$T_AE/approve" "$TOK_BOSS" "{\"comment\":\"P3-07 关闭态回归\"}")
eq "关闭态夹具：boss 审批 -> 200" "$RC" "200"
set_switch "$K_AE" false
RC=$(http PATCH "$BASE/api/business/erp/stock-take/$TK_AE/status" "$TOK_BOSS" "{\"status\":\"POSTED\"}")
eq "① 开关关闭（默认）→ 审批人自己过账 -> 200（默认不改变行为）" "$RC" "200"
eq "   盘点单状态 = POSTED" "$(q "$DB" "select status from biz_stock_take where id='$TK_AE'::uuid")" "POSTED"

# ============================================================================
say ""
say "=== S4 审批 ↔ 执行（approve_execute）：付款执行（人工执行入口）==="
# ============================================================================
set_switch "$K_AE" true
PAYABLE=$(q "$DB" "select id from biz_payable where source_id='$SI_ON'::uuid and deleted_at is null limit 1")
is_uuid "$PAYABLE" && ok "收货过账已生成应付单（$PAYABLE，未付 $(q "$DB" "select unpaid_amount from biz_payable where id='$PAYABLE'::uuid")）" \
  || { bad "没找到应付单"; summary; }
pay_amount() { q "$DB" "select paid_amount::text from biz_payable where id='$1'::uuid"; }
eq "付款前已付 = 0" "$(pay_amount "$PAYABLE")" "0.0000"

RC=$(http POST "$BASE/api/business/erp/payable/$PAYABLE/pay" "$TOK_MAKER" "{\"amount\":10}")
eq "maker 发起付款（有 payable_pay 流程）-> 202" "$RC" "202"
PM=$(q "$DB" "select id from biz_payment where payable_id='$PAYABLE'::uuid order by created_at desc limit 1")
is_uuid "$PM" && ok "付款记录 $PM（审批中）" || { bad "没建出付款记录：$(body)"; summary; }
I_PAY=$(q "$DB" "select approval_instance_id from biz_payment where id='$PM'::uuid")
T_PAY=$(task_of "$I_PAY"); is_uuid "$T_PAY" && ok "付款审批待办落在 boss" || { bad "没待办"; summary; }

RC=$(http POST "$BASE/api/business/flow/task/$T_PAY/approve" "$TOK_BOSS" "{\"comment\":\"P3-07 证据：付款同意\"}")
eq "boss 审批通过 -> 200" "$RC" "200"
eq "③ 开关打开（approve_execute）→ 审批通过**不自动记账**：付款记录仍 PENDING" \
  "$(q "$DB" "select approval_status from biz_payment where id='$PM'::uuid")" "PENDING"
eq "   应付已付金额未变（账一分没动）" "$(pay_amount "$PAYABLE")" "0.0000"

RC=$(http POST "$BASE/api/business/erp/payable/payment/$PM/execute" "$TOK_BOSS" "")
eq "③ 同一人（审批人）执行付款 -> 409" "$RC" "409"
eq "   业务码 = 66013" "$(body | jget code)" "66013"
contains "   文案点名职责对" "$(body | jget msg)" "审批与执行分离"
contains "   文案给出放行办法（开关键名）" "$(body | jget msg)" "$K_AE"
eq "   仍然没记账" "$(q "$DB" "select approval_status from biz_payment where id='$PM'::uuid")" "PENDING"

RC=$(http POST "$BASE/api/business/erp/payable/payment/$PM/execute" "$TOK_CASH" "")
eq "④ 换人（出纳）执行付款 -> 200" "$RC" "200"
eq "   付款记录 = APPROVED" "$(q "$DB" "select approval_status from biz_payment where id='$PM'::uuid")" "APPROVED"
eq "   应付已付金额 +10" "$(pay_amount "$PAYABLE")" "10.0000"
eq "   付款凭证已生成" \
  "$(q "$DB" "select case when voucher_id is null then 'no' else 'yes' end from biz_payment where id='$PM'::uuid")" "yes"

# 关闭态回归：审批通过即自动记账（改动前行为）
set_switch "$K_AE" false
RC=$(http POST "$BASE/api/business/erp/payable/$PAYABLE/pay" "$TOK_CASH" "{\"amount\":5}")
eq "关闭态：另起一笔付款 -> 202" "$RC" "202"
PM2=$(q "$DB" "select id from biz_payment where payable_id='$PAYABLE'::uuid order by created_at desc limit 1")
I_PAY2=$(q "$DB" "select approval_instance_id from biz_payment where id='$PM2'::uuid")
T_PAY2=$(task_of "$I_PAY2")
RC=$(http POST "$BASE/api/business/flow/task/$T_PAY2/approve" "$TOK_BOSS" "{\"comment\":\"P3-07 关闭态回归\"}")
eq "关闭态：boss 审批 -> 200" "$RC" "200"
eq "① 开关关闭（默认）→ 审批通过即自动记账（改动前行为）" \
  "$(q "$DB" "select approval_status from biz_payment where id='$PM2'::uuid")" "APPROVED"
eq "   应付已付金额 +5 = 15" "$(pay_amount "$PAYABLE")" "15.0000"

# ============================================================================
say ""
say "=== S5 回归：原有 approve / post / pay 三闸行为不变 + 总闸一键开启 ==="
# ============================================================================
T_OFF=$(task_of "$I_OFF"); is_uuid "$T_OFF" && ok "关闭态盘点单的待办在（$T_OFF）" || { bad "没待办"; summary; }
RC=$(http POST "$BASE/api/business/flow/task/$T_OFF/approve" "$TOK_MAKER" "{\"comment\":\"制单人想自己批\"}")
eq "⑤a approve 闸（默认开）：制单人审批自己的单 -> 403" "$RC" "403"
contains "   原有文案点名「制单与审批分离」" "$(body | jget msg)" "制单与审批分离"

TK_POST=$(mk_take "$TOK_CASH"); RC=$(submit_take "$TOK_CASH" "$TK_POST"); eq "⑤b 夹具确认 -> 200" "$RC" "200"
I_POST=$(inst_of_take "$TK_POST"); T_POST=$(task_of "$I_POST")
RC=$(http POST "$BASE/api/business/flow/task/$T_POST/approve" "$TOK_BOSS" "{\"comment\":\"回归\"}")
eq "⑤b 夹具审批 -> 200" "$RC" "200"
set_switch "$K_POST" true
RC=$(http PATCH "$BASE/api/business/erp/stock-take/$TK_POST/status" "$TOK_CASH" "{\"status\":\"POSTED\"}")
eq "⑤b post 闸打开：制单人自己过账 -> 409" "$RC" "409"
contains "   原有文案点名「制单与过账分离」" "$(body | jget msg)" "制单与过账分离"
set_switch "$K_POST" false

RC=$(http POST "$BASE/api/business/erp/payable/$PAYABLE/pay" "$TOK_MAKER" "{\"amount\":1}")
eq "⑤c 夹具：maker 发起付款 -> 202" "$RC" "202"
PM3=$(q "$DB" "select id from biz_payment where payable_id='$PAYABLE'::uuid order by created_at desc limit 1")
I_PAY3=$(q "$DB" "select approval_instance_id from biz_payment where id='$PM3'::uuid")
T_PAY3=$(task_of "$I_PAY3")
RC=$(http POST "$BASE/api/business/flow/task/$T_PAY3/approve" "$TOK_MAKER" "{\"comment\":\"制单人想自己批付款\"}")
eq "⑤c pay 闸（默认开）：制单人审批自己的付款 -> 403" "$RC" "403"
contains "   原有文案点名「不能审批自己提交的付款」" "$(body | jget msg)" "不能审批自己提交的付款申请"
contains "   并提示 pay 开关键名" "$(body | jget msg)" "$K_PAY"

# 总闸：显式 false 压过总闸 / 子键未配时总闸让三项默认开
q "$DB" "update sys_config set config_value='false' where config_key in ('$K_PA','$K_ST','$K_AE')" >/dev/null
q "$DB" "update sys_config set config_value='true' where config_key='$K_MASTER'" >/dev/null
http POST "$BASE/api/support/config/reload" "$A" "" >/dev/null
TK_M=$(mk_take "$TOK_MAKER")
RC=$(submit_take "$TOK_MAKER" "$TK_M")
eq "总闸 enabled=true 但 stocktake 显式 false → 制单人确认 -> 200（显式优先）" "$RC" "200"

q "$DB" "update sys_config set config_value=NULL where config_key in ('$K_PA','$K_ST','$K_AE')" >/dev/null
http POST "$BASE/api/support/config/reload" "$A" "" >/dev/null
TK_M2=$(mk_take "$TOK_MAKER")
RC=$(submit_take "$TOK_MAKER" "$TK_M2")
eq "总闸 enabled=true 且子键未配 → 制单人确认 -> 409（一键开启生效）" "$RC" "409"
contains "   文案点名职责对" "$(body | jget msg)" "制单与盘点确认分离"
RC=$(mk_receipt_for "$TOK_MAKER" "$PO" "$ITEM" 1)
eq "总闸打开 → 采购员收货 -> 409（purchase_accept 也默认开）" "$RC" "409"
contains "   文案点名职责对" "$(body | jget msg)" "采购与验收分离"

# 复原
q "$DB" "update sys_config set config_value='false' where config_key in ('$K_PA','$K_ST','$K_AE','$K_MASTER')" >/dev/null
http POST "$BASE/api/support/config/reload" "$A" "" >/dev/null
PO_FRESH=$(mk_po "$TOK_MAKER" 100)
ITEM_FRESH=$(q "$DB" "select id from biz_purchase_order_item where order_id='$PO_FRESH'::uuid order by line_no limit 1")
RC=$(mk_receipt_for "$TOK_MAKER" "$PO_FRESH" "$ITEM_FRESH" 1)
eq "复原（总闸 false + 三条 false）→ 采购员收货 -> 200" "$RC" "200"

say ""
say "=== 证据小结 ==="
say "采购单：$PO_NO（$PO）"
say "收货单（关闭态）：$SI_OFF / 收货单（打开态+换人）：$SI_ON"
say "盘点单：关闭态 $TK_OFF / 打开态 $TK_ON / 关闭态审批人过账 $TK_AE"
say "付款：$PM（换人执行）/ $PM2（关闭态自动执行）/ $PM3（pay 闸回归夹具）"

summary
