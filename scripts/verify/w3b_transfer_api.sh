#!/usr/bin/env bash
# ============================================================
# W3b 调拨单接口 e2e（真实 curl，跑在隔离实例上）
#
#   覆盖：建单 → 改单 → 提交审批（真发起审批单）→ 审批通过（真回调）
#         → 发货 → 收货 → 终态拒绝 → 删除保护 → 序列号分组 → 无码 403
#
# 用法（库与实例由本卡自建，见交付说明 §验收）：
#
#   export W3B_BASE=http://127.0.0.1:18085/api
#   export W3B_DB_PASSWORD='<cloudpivot 库口令>'
#   export W3B_DB_NAME=cloudpivot_w3b          # 默认 cloudpivot_w3b
#   export W3B_ADMIN_PASSWORD='<admin 口令>'   # 默认 Nereus792379（dev/test 种子）
#   bash scripts/verify/w3b_transfer_api.sh
#
# 退出码 = 是否有断言失败（0 = 全过）。
# 脚本自建夹具（仓 / 物料 / 序列号 / 审批流程 / 只读账号），开头清理上一轮遗留，
# 结尾打印清理 SQL 并核对残留计数（不静默删别人的数据）。
# ============================================================
set -uo pipefail

BASE="${W3B_BASE:-http://127.0.0.1:18085/api}"
DBN="${W3B_DB_NAME:-cloudpivot_w3b}"
DBH="${W3B_DB_HOST:-127.0.0.1}"
DBU="${W3B_DB_USER:-cloudpivot}"
ADMIN_USER="${W3B_ADMIN:-admin}"
ADMIN_PASS="${W3B_ADMIN_PASSWORD:-Nereus792379}"
: "${W3B_DB_PASSWORD:?请先 export W3B_DB_PASSWORD（cloudpivot 库口令）}"

# 夹具（固定 UUID，前缀 W3B-E2E，重跑安全）
U_ADMIN_ROLE_LOOKUP=""
WH_FROM="d3be0000-0000-4000-8000-000000000001"
WH_TO="d3be0000-0000-4000-8000-000000000002"
UNIT="d3be0000-0000-4000-8000-000000000003"
M_PLAIN="d3be0000-0000-4000-8000-000000000011"
M_SN="d3be0000-0000-4000-8000-000000000012"
SERIAL_1="d3be0000-0000-4000-8000-000000000021"
SERIAL_2="d3be0000-0000-4000-8000-000000000022"
FLOW_CODE="w3b_e2e_stock_transfer"

PASS=0
FAIL=0
TOKEN=""
RO_TOKEN=""

psql_q() {
    PGPASSWORD="$W3B_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -Atc "$1" 2>&1 | head -1
}

psql_x() {
    PGPASSWORD="$W3B_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -v ON_ERROR_STOP=1 -c "$1" 2>&1 | head -3
}

ok() {
    PASS=$((PASS + 1))
    printf '[PASS] %s\n' "$1"
}

bad() {
    FAIL=$((FAIL + 1))
    printf '[FAIL] %s\n' "$1"
}

# assert_eq <期望> <实际> <说明>
assert_eq() {
    if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi
}

# assert_contains <文本> <子串> <说明>
assert_contains() {
    case "$1" in
        *"$2"*) ok "$3" ;;
        *) bad "$3：文本里没有 [$2]，实际 [$1]" ;;
    esac
}

# 发请求：REQ_CODE / REQ_BODY 写回全局（**不要用 $(...) 包住**，否则子 shell 里赋值丢失）
REQ_CODE=0
REQ_BODY=""
call() {
    local method="$1" path="$2" body="${3:-}" auth="${4:-$TOKEN}" tmp
    tmp=$(mktemp)
    if [ -n "$body" ]; then
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $auth" -H 'Content-Type: application/json' \
            --data-binary "$body" "$BASE$path")
    else
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $auth" "$BASE$path")
    fi
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}

jqv() { printf '%s' "$REQ_BODY" | jq -r "$1" 2>/dev/null; }

echo "==> 0. 清理上一轮 e2e 残留（只动 W3B-E2E 前缀的夹具）"
psql_x "DELETE FROM biz_flow_instance WHERE flow_code = '$FLOW_CODE'" >/dev/null
psql_x "DELETE FROM biz_stock_transfer_item_serial WHERE item_id IN (
          SELECT i.id FROM biz_stock_transfer_item i JOIN biz_stock_transfer t ON t.id = i.transfer_id
           WHERE t.from_warehouse_id IN ('$WH_FROM', '$WH_TO'))" >/dev/null
psql_x "DELETE FROM biz_stock_transfer_item WHERE transfer_id IN (
          SELECT id FROM biz_stock_transfer WHERE from_warehouse_id IN ('$WH_FROM', '$WH_TO'))" >/dev/null
psql_x "DELETE FROM biz_stock_transfer WHERE from_warehouse_id IN ('$WH_FROM', '$WH_TO')" >/dev/null
psql_x "DELETE FROM biz_flow_def_version WHERE def_id IN (SELECT id FROM biz_flow_def WHERE flow_code = '$FLOW_CODE')" >/dev/null
psql_x "DELETE FROM biz_flow_def WHERE flow_code = '$FLOW_CODE'" >/dev/null

echo "==> 1. 登录 admin"
ADMIN_NEW="${W3B_ADMIN_NEW_PASSWORD:-Nereus792379!W3b}"
# 密库里 admin 可能已被改过密（首登强制改密是一次性的）：先试新口令，不行再用旧口令 +
# 走 `PUT /system/auth/password` 改密（旧脚本「先清 TOKEN 再改密」实测 401，顺序不能反）
call POST /system/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_NEW\"}" ""
if [ "$REQ_CODE" = "200" ]; then
    ok "登录 admin（新口令）"
    TOKEN=$(jqv '.data.access_token')
else
    call POST /system/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" ""
    if [ "$REQ_CODE" != "200" ]; then
        bad "admin 登录失败（旧/新口令都不通，响应：$REQ_BODY）"
        echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
        exit 1
    fi
    ok "登录 admin（初始口令）"
    TOKEN=$(jqv '.data.access_token')
    if [ "$(jqv '.data.must_change_password')" = "true" ]; then
        call PUT /system/auth/password "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$ADMIN_NEW\"}" ""
        assert_eq "200" "$REQ_CODE" "首登强制改密（改成 $ADMIN_NEW）"
        call POST /system/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_NEW\"}" ""
        assert_eq "200" "$REQ_CODE" "用新口令重新登录（改密会让旧令牌失效）"
        TOKEN=$(jqv '.data.access_token')
    fi
fi
if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
    bad "拿不到 access_token，后续全部跳过（响应：$REQ_BODY）"
    echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
    exit 1
fi
ME_ID=$(psql_q "SELECT id FROM sys_user WHERE username = '$ADMIN_USER' AND deleted_at IS NULL")
echo "    admin id = $ME_ID"

echo "==> 2. 夹具：两个仓 / 单位 / 两个物料 / 两个在库序列号"
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order)
        VALUES ('$UNIT', 'W3B-E2E-PCS', '个', 1, 1, 1, 1) ON CONFLICT (id) DO NOTHING" >/dev/null
# 注意：V0137 的 allow_negative / is_leaf 是 SMALLINT（0/1），不是 BOOLEAN
psql_x "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, allow_negative,
                                  status, sort_order, parent_id, level, warehouse_path, is_leaf)
        VALUES ('$WH_FROM', 'WH-W3B-E2E-1', 'W3B e2e 调出仓', 'NORMAL', 0, 1, 90, NULL, 1, '/w3be1/', 1),
               ('$WH_TO',   'WH-W3B-E2E-2', 'W3B e2e 调入仓', 'NORMAL', 0, 1, 91, NULL, 1, '/w3be2/', 1)
        ON CONFLICT (id) DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, safety_stock,
                                  costing_method, tax_rate, status, serial_required)
        VALUES ('$M_PLAIN', 'W3B-E2E-M1', 'W3B e2e 按数量物料', '$UNIT', 'RAW', 0, 'MOVING_AVG', 0, 1, false),
               ('$M_SN',    'W3B-E2E-M2', 'W3B e2e 一物一码物料', '$UNIT', 'FINISHED', 0, 'MOVING_AVG', 0, 1, true)
        ON CONFLICT (id) DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status)
        VALUES ('$SERIAL_1', 'W3B-E2E-SN-0001', '$M_SN', '$WH_FROM', 'IN_STOCK'),
               ('$SERIAL_2', 'W3B-E2E-SN-0002', '$M_SN', '$WH_FROM', 'IN_STOCK')
        ON CONFLICT (id) DO NOTHING" >/dev/null
# W3c 起 IN_TRANSIT → DONE 会做库存双写，所以 e2e 必须先把库账造好：
#   M_PLAIN 100 件 @5（本脚本用它调 7 件）、M_SN 2 件 @30 + 2 个在库序列号（不变式③要求相等）
# 序列号与库账每轮都要重置：上一轮 DONE 会把序列号挪到调入仓、并在调入仓建库存行，
# 若不清掉调入仓的旧行，本轮「qty 与在库序列号件数」会对不上（不变式③直接拒）。
psql_x "DELETE FROM biz_inventory_txn WHERE warehouse_id IN ('$WH_FROM', '$WH_TO')" >/dev/null
psql_x "DELETE FROM biz_inventory WHERE warehouse_id IN ('$WH_FROM', '$WH_TO')" >/dev/null
psql_x "UPDATE biz_material_serial SET warehouse_id = '$WH_FROM', location_id = NULL,
                                       status = 'IN_STOCK', updated_at = now()
         WHERE id IN ('$SERIAL_1', '$SERIAL_2')" >/dev/null
psql_x "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty,
                                   avg_cost, amount, last_txn_at)
        VALUES (gen_random_uuid(), '$M_PLAIN', '$WH_FROM', NULL, 100, 0, 5, 500, now()),
               (gen_random_uuid(), '$M_SN',    '$WH_FROM', NULL,   2, 0, 30,  60, now())
        ON CONFLICT (material_id, warehouse_id,
                     COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
        DO UPDATE SET qty = EXCLUDED.qty, locked_qty = 0,
                      avg_cost = EXCLUDED.avg_cost, amount = EXCLUDED.amount,
                      updated_at = now()" >/dev/null
assert_eq "2" "$(psql_q "SELECT count(*) FROM biz_inventory WHERE warehouse_id = '$WH_FROM'")" \
    "夹具库存台账就位（M_PLAIN 100 + M_SN 2）"
assert_eq "2" "$(psql_q "SELECT count(*) FROM biz_warehouse WHERE id IN ('$WH_FROM','$WH_TO')")" "夹具两个仓就位"

echo "==> 3. 发布 STOCK_TRANSFER 审批流程（走真实接口，不动库）"
call POST /business/flow/def "{\"flow_code\":\"$FLOW_CODE\",\"flow_name\":\"W3B e2e 调拨审批\",\"biz_type\":\"STOCK_TRANSFER\",\"is_default\":true}" ""
assert_eq "200" "$REQ_CODE" "新建流程定义"
DEF_ID=$(jqv '.data.id')
VID=$(jqv '.data.version_id')
if [ -z "$DEF_ID" ] || [ "$DEF_ID" = "null" ]; then
    bad "拿不到流程定义 id（$REQ_BODY）——后续审批相关断言会跳过"
else
    GRAPH=$(jq -nc --arg uid "$ME_ID" '{
        nodes: [
          {key:"n_start", name:"开始", type:"START", sort_order:1},
          {key:"n_approve", name:"仓管审批", type:"APPROVAL", assignee_type:"USER",
           empty_assignee:"ERROR", sort_order:2,
           assignees:[{type:"USER", ref_id:$uid, sort_order:1}]},
          {key:"n_end", name:"结束", type:"END", sort_order:3}
        ],
        edges: [
          {key:"e1", from:"n_start", to:"n_approve"},
          {key:"e2", from:"n_approve", to:"n_end"}
        ]}')
    call PUT "/business/flow/version/$VID/graph" "$GRAPH" ""
    assert_eq "200" "$REQ_CODE" "保存流程图（START→APPROVAL→END）"
    call POST "/business/flow/def/$DEF_ID/publish" "" ""
    assert_eq "200" "$REQ_CODE" "发布流程（PUBLISHED）"
fi

echo "==> 4. 建单（含 1 行按数量 + 1 行带 2 个序列号）"
BODY=$(jq -nc --arg f "$WH_FROM" --arg t "$WH_TO" --arg m1 "$M_PLAIN" --arg m2 "$M_SN" \
    --arg s1 "$SERIAL_1" --arg s2 "$SERIAL_2" '{
    from_warehouse_id:$f, to_warehouse_id:$t, reason:"W3B e2e 调拨",
    items:[
      {material_id:$m1, qty:"3"},
      {material_id:$m2, qty:"2", serial_ids:[$s1,$s2]}
    ]}')
call POST /business/erp/transfer "$BODY" ""
assert_eq "200" "$REQ_CODE" "POST /business/erp/transfer"
TID=$(jqv '.data.id')
NO=$(jqv '.data.transfer_no')
assert_eq "DRAFT" "$(jqv '.data.status')" "新建即 DRAFT"
case "$NO" in
    TK-[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-[0-9][0-9][0-9][0-9][0-9][0-9]) ok "单号格式 TK-yyyyMMdd-6位（$NO）" ;;
    *) bad "单号格式不对：[$NO]" ;;
esac
assert_eq "5.0000" "$(jqv '.data.total_qty')" "数量合计 = 3 + 2"
assert_eq "2" "$(jqv '.data.items | length')" "明细 2 行"
assert_eq "2" "$(jqv '.data.items[1].serial_ids | length')" "第 2 行随行 2 个序列号"

echo "==> 5. 改单（DRAFT 能改，明细整体重建）"
BODY=$(jq -nc --arg f "$WH_FROM" --arg t "$WH_TO" --arg m1 "$M_PLAIN" --arg m2 "$M_SN" \
    --arg s1 "$SERIAL_1" --arg s2 "$SERIAL_2" '{
    from_warehouse_id:$f, to_warehouse_id:$t, reason:"W3B e2e 改单",
    items:[
      {material_id:$m1, qty:"7"},
      {material_id:$m2, qty:"2", serial_ids:[$s1,$s2]}
    ]}')
call PUT "/business/erp/transfer/$TID" "$BODY" ""
assert_eq "200" "$REQ_CODE" "PUT 改单（DRAFT）"
assert_eq "1" "$(jqv '.data.items[0].line_no')" "明细被重建（行号从 1 开始）"
assert_eq "9.0000" "$(jqv '.data.total_qty')" "数量合计跟着改（7 + 2）"
assert_eq "2" "$(jqv '.data.items[1].serial_ids | length')" "改单后序列号重新落库"

echo "==> 6. 提交审批（DRAFT → PENDING，真发起审批单）"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"PENDING"}' ""
assert_eq "200" "$REQ_CODE" "PATCH status=PENDING"
assert_eq "PENDING" "$(jqv '.data.status')" "状态 = PENDING"
IID=$(jqv '.data.approval_instance_id')
case "$IID" in
    ""|null) bad "提交后没有回写 approval_instance_id（配了已发布流程就必须有）" ;;
    *) ok "回写 approval_instance_id（$IID）" ;;
esac
assert_eq "$NO" "$(psql_q "SELECT biz_no FROM biz_flow_instance WHERE id = '$IID'")" "审批实例的 biz_no = 调拨单号"
assert_eq "STOCK_TRANSFER" "$(psql_q "SELECT biz_type FROM biz_flow_instance WHERE id = '$IID'")" "审批实例 biz_type = STOCK_TRANSFER"

echo "==> 7. 审批通过（PENDING → APPROVED）"
# 优先走审批中心（真实待办 → approve → on_transfer_settled 回调）；
# 用只含快照的流程定义时引擎不一定派生出待办行（平台 D1/D2 的既有行为，与本卡无关），
# 此时按卡面 §二 ⑤ 的「也可人工推」降级：PATCH status=APPROVED。
FTASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id = '$IID' AND status = 'PENDING' ORDER BY created_at LIMIT 1")
if [ -n "$FTASK" ] && [ "$FTASK" != "null" ]; then
    ok "审批中心已派生待办行（$FTASK）"
    call POST "/business/flow/task/$FTASK/approve" '{"comment":"e2e 通过"}' ""
    assert_eq "200" "$REQ_CODE" "审批同意（走审批中心）"
    assert_eq "APPROVED" "$(psql_q "SELECT status FROM biz_stock_transfer WHERE id = '$TID'")" "回调把调拨单推到 APPROVED"
else
    printf '     （审批中心没有派生待办行 → 走卡面允许的「人工推 APPROVED」降级路径）\n'
    call PATCH "/business/erp/transfer/$TID/status" '{"status":"APPROVED"}' ""
    assert_eq "200" "$REQ_CODE" "人工推 APPROVED（PENDING → APPROVED 合法边）"
fi
assert_eq "APPROVED" "$(psql_q "SELECT status FROM biz_stock_transfer WHERE id = '$TID'")" "调拨单状态 = APPROVED"

echo "==> 8. 发货（APPROVED → IN_TRANSIT，回填在途仓）"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"IN_TRANSIT"}' ""
assert_eq "200" "$REQ_CODE" "PATCH status=IN_TRANSIT"
assert_eq "IN_TRANSIT" "$(jqv '.data.status')" "状态 = IN_TRANSIT"
TRANSIT=$(psql_q "SELECT id FROM biz_warehouse WHERE warehouse_code = 'WH-TRANSIT' AND deleted_at IS NULL")
assert_eq "$TRANSIT" "$(jqv '.data.transit_warehouse_id')" "在途仓已回填"

echo "==> 9. 收货（IN_TRANSIT → DONE）"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"DONE"}' ""
assert_eq "200" "$REQ_CODE" "PATCH status=DONE"
assert_eq "DONE" "$(jqv '.data.status')" "状态 = DONE"
[ -n "$(jqv '.data.received_at')" ] && [ "$(jqv '.data.received_at')" != "null" ] \
    && ok "received_at 已写入" || bad "DONE 但 received_at 为空"

echo "==> 10. 负向：终态 / 跳级 / 删除保护 / 改单保护"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"CANCELLED","reason":"终态试探"}' ""
assert_eq "409" "$REQ_CODE" "DONE 之后任何流转 → 409"
call DELETE "/business/erp/transfer/$TID" "" ""
assert_eq "409" "$REQ_CODE" "DONE 不能删 → 409"

call POST /business/erp/transfer "$(jq -nc --arg f "$WH_FROM" --arg t "$WH_TO" --arg m "$M_PLAIN" \
    '{from_warehouse_id:$f, to_warehouse_id:$t, items:[{material_id:$m, qty:"1"}]}')" ""
T2=$(jqv '.data.id')
assert_eq "200" "$REQ_CODE" "再建一张单（负向用）"
call PATCH "/business/erp/transfer/$T2/status" '{"status":"DONE"}' ""
assert_eq "409" "$REQ_CODE" "DRAFT → DONE 跳级 → 409"
assert_contains "$(jqv '.msg')" "非法状态流转" "跳级文案点名「非法状态流转」"
call PATCH "/business/erp/transfer/$T2/status" '{"status":"PENDING"}' ""
assert_eq "200" "$REQ_CODE" "第二张单提交 PENDING"
call PUT "/business/erp/transfer/$T2" "$(jq -nc --arg f "$WH_FROM" --arg t "$WH_TO" --arg m "$M_PLAIN" \
    '{from_warehouse_id:$f, to_warehouse_id:$t, items:[{material_id:$m, qty:"9"}]}')" ""
assert_eq "409" "$REQ_CODE" "PENDING 不能改 → 409"
call PATCH "/business/erp/transfer/$T2/status" '{"status":"CANCELLED","reason":"e2e 撤回"}' ""
assert_eq "200" "$REQ_CODE" "PENDING → CANCELLED（撤回，带原因）"
assert_eq "CANCELLED" "$(jqv '.data.status')" "撤回后状态 = CANCELLED"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE id = (
    SELECT approval_instance_id FROM biz_stock_transfer WHERE id = '$T2') AND status = 'PENDING'")" \
    "撤回时在跑审批单已被终止（否则重新提交会永久 409）"

echo "==> 11. 删单（CANCELLED 能删；软删后详情 404）"
call DELETE "/business/erp/transfer/$T2" "" ""
assert_eq "200" "$REQ_CODE" "CANCELLED 能删"
call GET "/business/erp/transfer/$T2" "" ""
assert_eq "404" "$REQ_CODE" "软删后详情 404"

echo "==> 12. 查询：详情 / 序列号分组 / 列表筛选"
call GET "/business/erp/transfer/$TID" "" ""
assert_eq "200" "$REQ_CODE" "详情 200"
assert_eq "2" "$(jqv '.data.items | length')" "详情带 2 行明细"
call GET "/business/erp/transfer/$TID/serials" "" ""
assert_eq "200" "$REQ_CODE" "序列号分组 200"
assert_eq "2" "$(jqv '.data.total')" "序列号总数 = 2"
assert_eq "2" "$(jqv '.data.groups[1].serials | length')" "第 2 行分组带 2 个序列号"
call GET "/business/erp/transfer/page?status=DONE&from_warehouse_id=$WH_FROM&page=1&page_size=5" "" ""
assert_eq "200" "$REQ_CODE" "列表筛选 200"
assert_eq "DONE" "$(jqv '.data.list[0].status')" "列表按状态筛选生效"

echo "==> 13. 权限：无码账号 → 403"
RO_USER="w3b_e2e_ro"
psql_x "DELETE FROM sys_user WHERE username = '$RO_USER'" >/dev/null
call POST /system/user "{\"username\":\"$RO_USER\",\"password\":\"W3bE2eRead#1\",\"real_name\":\"W3B e2e 只读\",\"role_ids\":[]}" ""
if [ "$REQ_CODE" = "200" ]; then
    ok "建无角色账号"
else
    # 已存在时接口 409：直接复用
    printf '     （建号返回 %s，按已存在处理）\n' "$REQ_CODE"
fi
call POST /system/auth/login "{\"username\":\"$RO_USER\",\"password\":\"W3bE2eRead#1\"}" ""
RO_TOKEN=$(jqv '.data.access_token')
if [ -z "$RO_TOKEN" ] || [ "$RO_TOKEN" = "null" ]; then
    bad "无码账号登录失败（$REQ_BODY）"
else
    call POST /business/erp/transfer "$(jq -nc --arg f "$WH_FROM" --arg t "$WH_TO" --arg m "$M_PLAIN" \
        '{from_warehouse_id:$f, to_warehouse_id:$t, items:[{material_id:$m, qty:"1"}]}')" "$RO_TOKEN"
    assert_eq "403" "$REQ_CODE" "无码账号建调拨单 → 403"
    call GET /business/erp/transfer/page "" "$RO_TOKEN"
    assert_eq "403" "$REQ_CODE" "无码账号看列表 → 403"
fi

echo "==> 14. 残留核对（清理请手工执行下面的 SQL）"
printf '    DELETE FROM biz_flow_instance WHERE flow_code = %s;\n' "'$FLOW_CODE'"
printf '    DELETE FROM biz_stock_transfer WHERE from_warehouse_id IN (%s, %s);\n' "'$WH_FROM'" "'$WH_TO'"
printf '    DELETE FROM sys_user WHERE username = %s;\n' "'$RO_USER'"
printf '    当前库内本卡夹具：transfer=%s  instance=%s  flow_def=%s\n' \
    "$(psql_q "SELECT count(*) FROM biz_stock_transfer WHERE from_warehouse_id IN ('$WH_FROM','$WH_TO')")" \
    "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE flow_code = '$FLOW_CODE'")" \
    "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = '$FLOW_CODE'")"

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
