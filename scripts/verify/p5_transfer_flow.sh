#!/usr/bin/env bash
# ============================================================
# P5 验收：库存调拨审批流（真机 HTTP + psql 双证）
#
#   覆盖卡面「三、验收（真机）」全部 6 条：
#     ① 建调拨单 → 提交审批 → biz_stock_transfer.approval_instance_id 非空
#     ② 审批中心待办能看到该调拨单（GET /business/flow/task/todo）
#     ③ 审批通过 → 调拨单状态变 APPROVED（并写 approved_at / approver_id）
#     ④ 发货 → IN_TRANSIT → 收货 → DONE
#     ⑤ 库存正确扣减/增加（W3c 的库存双写没坏）
#   外加两条负向：
#     ⑥ 审批中人工直推 APPROVED → 409 + 65030（P5 门闸）
#     ⑦ 撤回（PENDING → CANCELLED）终止在跑审批单、不留 PENDING 待办
#
# 前置：一个**已应用 V0157** 的隔离库 + 跑在该库上的隔离实例（库里有 `stock_transfer`
#       流程种子，不要再手工建流程 —— 本脚本要验的正是「种子里带来的流程」能用）。
#
# 用法：
#   export P5_BASE=http://127.0.0.1:18098/api
#   export P5_DB_PASSWORD='<cloudpivot 库口令>'
#   export P5_DB_NAME=cloudpivot_p5e2e        # 默认 cloudpivot_p5e2e（**必须是隔离库**）
#   export P5_ADMIN_PASSWORD='<admin 口令>'    # 默认取 P5Admin#2026（本卡隔离库 bootstrap 口令）
#   bash scripts/verify/p5_transfer_flow.sh
#
# 前置（脚本第 0.5 步硬校验，登录之前）：库里持 SUPER_ADMIN 的账号必须**恰好 1 个**。
#   种子流程审批节点是 `assignee_type=ADMIN`、`approval_mode=NULL`（引擎按会签 ALL），
#   超管 > 1 时单账号批不完 → 单据卡 PENDING、其余超管的待办永久悬挂（t_15be8cc3 的成因）。
#
# 收尾：默认**用完即删**（先终止审批实例/待办/节点，再删单据）。要保留数据看页面：
#   P5_KEEP_DATA=1（审批照样终止，不会把手持超管的夹具用户钉成待办）。
#
# 退出码 = 是否有断言失败（0 = 全过）。夹具按 `P5-E2E-` 前缀自建自清。
# ============================================================
set -uo pipefail

BASE="${P5_BASE:-http://127.0.0.1:18098/api}"
DBN="${P5_DB_NAME:-cloudpivot_p5e2e}"
DBH="${P5_DB_HOST:-127.0.0.1}"
DBU="${P5_DB_USER:-cloudpivot}"
ADMIN_USER="${P5_ADMIN:-admin}"
ADMIN_PASS="${P5_ADMIN_PASSWORD:-P5Admin#2026}"
ADMIN_NEW="${P5_ADMIN_NEW_PASSWORD:-P5Admin#2026!e2e}"
: "${P5_DB_PASSWORD:?请先 export P5_DB_PASSWORD（cloudpivot 库口令）}"

WH_FROM="f50000a0-0000-4000-8000-000000000001"
WH_TO="f50000a0-0000-4000-8000-000000000002"
UNIT="f50000a0-0000-4000-8000-000000000003"
MAT="f50000a0-0000-4000-8000-000000000011"
SEED_DEF="20000000-0000-4000-8000-00000000d157"
SEED_VER="20000000-0000-4000-8000-00000000e157"

PASS=0
FAIL=0
TOKEN=""
REQ_CODE=0
REQ_BODY=""

psql_q() {
    PGPASSWORD="$P5_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -Atc "$1" 2>&1 | head -1
}

psql_x() {
    PGPASSWORD="$P5_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -v ON_ERROR_STOP=1 -c "$1" 2>&1 | head -3
}

ok()  { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }

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

# assert_num <期望> <实际> <说明>（NUMERIC 经 JSON 回来是 "5.0000"，逐字比会假红）
assert_num() {
    if awk -v a="$1" -v b="$2" 'BEGIN{exit !(a+0==b+0)}'; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi
}

# 发请求：REQ_CODE / REQ_BODY 写回全局（**不要用 $(...) 包住**，否则子 shell 里赋值丢失）
call() {
    local method="$1" path="$2" body="${3:-}" tmp
    tmp=$(mktemp)
    if [ -n "$body" ]; then
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
            --data-binary "$body" "$BASE$path")
    else
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $TOKEN" "$BASE$path")
    fi
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}

# 登录前用的匿名请求
call_anon() {
    local method="$1" path="$2" body="${3:-}" tmp
    tmp=$(mktemp)
    REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
        -H 'Content-Type: application/json' --data-binary "$body" "$BASE$path")
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}

jqv() { printf '%s' "$REQ_BODY" | jq -r "$1" 2>/dev/null; }

# ---------- 收尾：调拨 e2e「用完即删」（公共函数见 lib/flow_cleanup.sh） ----------
# shellcheck source=lib/flow_cleanup.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/flow_cleanup.sh"
# 只收敛本脚本自己的夹具（固定 P5-E2E 仓），不许全表
SCOPE="SELECT id FROM biz_stock_transfer WHERE from_warehouse_id IN ('$WH_FROM', '$WH_TO')"
CLEANED=0

p5_cleanup_on_exit() {
    local rc=$?
    if [ "$CLEANED" = "1" ]; then return "$rc"; fi
    CLEANED=1
    echo
    echo "==> [exit] 收尾（trap EXIT：失败/中断也会走，避免审批实例把超管夹具用户钉成待办）"
    if [ "${P5_KEEP_DATA:-0}" = "1" ]; then
        e2e_terminate_transfer_flow "$SCOPE"
        echo "    P5_KEEP_DATA=1：单据保留，审批实例/待办已终止"
    else
        e2e_cleanup_transfer_flow "$SCOPE" || echo "    WARN: 收尾后仍有残留（见上一行计数）"
    fi
    e2e_report_pinned_fixtures
    return "$rc"
}
trap p5_cleanup_on_exit EXIT


echo "==> 0. 清理上一轮 e2e 残留（先撤审批实例/待办，再删单据；只动 P5-E2E 夹具）"
e2e_cleanup_transfer_flow "$SCOPE" || bad "上一轮 e2e 残留没清干净（见上一行计数）"
psql_x "DELETE FROM biz_inventory_txn WHERE warehouse_id IN ('$WH_FROM','$WH_TO')" >/dev/null
psql_x "DELETE FROM biz_inventory WHERE warehouse_id IN ('$WH_FROM','$WH_TO')" >/dev/null

# ---------- 0.5 前置硬校验：库里持 SUPER_ADMIN 的账号必须恰好 1 个 ----------
# 种子流程 `stock_transfer` 的审批节点是 `assignee_type=ADMIN`、`approval_mode=NULL`
# → 引擎按**会签 ALL** 处理（engine.rs：unwrap_or("ALL")），ADMIN 又展开成所有超管。
# 库里超管 > 1 时单账号批不完 → 单据永远 PENDING、其余超管的待办永久悬挂
# （t_15be8cc3 的成因：role/user 用例的 operator 夹具必持超管，被调拨 e2e 挂成待办后钉死）。
# 这条前置放在登录之前，共用库（cloudpivot_test）会当场被拦住，而不是跑一半留残留。
SA_COUNT=$(psql_q "SELECT count(*) FROM sys_user u
                     JOIN sys_user_role ur ON ur.user_id = u.id
                     JOIN sys_role r ON r.id = ur.role_id
                    WHERE r.role_code = 'SUPER_ADMIN' AND u.deleted_at IS NULL AND u.status = 1")
if [ "${SA_COUNT:-0}" != "1" ]; then
    bad "前置不满足：库里持 SUPER_ADMIN 的账号 ${SA_COUNT} 个（本脚本要求恰好 1 个）"
    echo "    审批节点 ADMIN → 全部超管都挂待办；会签 ALL 时单账号批不完，单据会卡 PENDING 且待办悬挂。"
    echo "    用隔离库跑（默认 P5_DB_NAME=cloudpivot_p5e2e），或先按 scripts/verify/lib/flow_cleanup.sh"
    echo "    的收尾口径清掉共用库里的既往 e2e 审批实例后重试。"
    exit 1
fi
ok "前置：库里恰好 1 个超管（ADMIN 节点展开唯一审批人，会签一次批完）"

echo "==> 1. 登录 admin（首登强制改密按需处理）"
call_anon POST /system/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_NEW\"}"
if [ "$REQ_CODE" = "200" ]; then
    ok "登录 admin（改密后的口令）"
    TOKEN=$(jqv '.data.access_token')
else
    call_anon POST /system/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}"
    if [ "$REQ_CODE" != "200" ]; then
        bad "admin 登录失败（初始/新口令都不通）：$REQ_BODY"
        echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
        exit 1
    fi
    ok "登录 admin（初始口令）"
    TOKEN=$(jqv '.data.access_token')
    if [ "$(jqv '.data.must_change_password')" = "true" ]; then
        call PUT /system/auth/password "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$ADMIN_NEW\"}"
        assert_eq "200" "$REQ_CODE" "首登强制改密"
        call_anon POST /system/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_NEW\"}"
        assert_eq "200" "$REQ_CODE" "用新口令重新登录"
        TOKEN=$(jqv '.data.access_token')
    fi
fi

echo "==> 2. 前置：V0157 的 stock_transfer 流程已就位"
assert_eq "stock_transfer|STOCK_TRANSFER|PUBLISHED|true" \
    "$(psql_q "SELECT flow_code||'|'||biz_type||'|'||status||'|'||is_default::text FROM biz_flow_def WHERE id='$SEED_DEF' AND deleted_at IS NULL")" \
    "① biz_flow_def 有 stock_transfer（PUBLISHED + 默认）"
assert_eq "e1947064684e326c4ad4248e354d0215f1a3f3a7f668fb467aefe854adbd90da" \
    "$(psql_q "SELECT graph_hash FROM biz_flow_def_version WHERE id='$SEED_VER'")" \
    "① 版本 graph_hash 与迁移一致"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE biz_type='STOCK_TRANSFER' AND status='PUBLISHED' AND current_version_id IS NOT NULL AND deleted_at IS NULL")" \
    "① 调拨业务类型只有 1 个已发布流程（不会取错）"

echo "==> 3. 夹具：仓 / 单位 / 物料 / 库存台账"
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order)
        VALUES ('$UNIT', 'P5-E2E-PCS', '个', 1, 1, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, allow_negative,
                                  status, sort_order, parent_id, level, warehouse_path, is_leaf)
        VALUES ('$WH_FROM', 'WH-P5-E2E-1', 'P5 e2e 调出仓', 'NORMAL', 0, 1, 95, NULL, 1, '/p5e1/', 1),
               ('$WH_TO',   'WH-P5-E2E-2', 'P5 e2e 调入仓', 'NORMAL', 0, 1, 96, NULL, 1, '/p5e2/', 1)
        ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, safety_stock,
                                  costing_method, tax_rate, status, serial_required)
        VALUES ('$MAT', 'P5-E2E-M1', 'P5 e2e 物料', '$UNIT', 'RAW', 0, 'MOVING_AVG', 0, 1, false)
        ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty,
                                   avg_cost, amount, last_txn_at)
        VALUES (gen_random_uuid(), '$MAT', '$WH_FROM', NULL, 100, 0, 10, 1000, now())
        ON CONFLICT (material_id, warehouse_id,
                     COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
        DO UPDATE SET qty = 100, locked_qty = 0, avg_cost = 10, amount = 1000, updated_at = now()" >/dev/null
assert_num "100" "$(psql_q "SELECT qty FROM biz_inventory WHERE material_id='$MAT' AND warehouse_id='$WH_FROM' AND location_id IS NULL")" \
    "夹具：调出仓 100 件 @10"

echo "==> 4. ① 建调拨单 → 提交审批（DRAFT → PENDING）"
BODY=$(jq -nc --arg f "$WH_FROM" --arg t "$WH_TO" --arg m "$MAT" \
    '{from_warehouse_id:$f, to_warehouse_id:$t, reason:"P5 e2e 调拨", items:[{material_id:$m, qty:"5"}]}')
call POST /business/erp/transfer "$BODY"
assert_eq "200" "$REQ_CODE" "POST /business/erp/transfer"
TID=$(jqv '.data.id')
TNO=$(jqv '.data.transfer_no')
assert_eq "DRAFT" "$(jqv '.data.status')" "新建即 DRAFT（$TNO）"
assert_eq "null" "$(jqv '.data.approval_instance_id')" "① 草稿没有审批实例"

call PATCH "/business/erp/transfer/$TID/status" '{"status":"PENDING"}'
assert_eq "200" "$REQ_CODE" "提交审批（DRAFT → PENDING）"
IID=$(jqv '.data.approval_instance_id')
if [ -z "$IID" ] || [ "$IID" = "null" ]; then
    bad "① 提交审批后 approval_instance_id 竟然是空（这正是本卡要修的现象）"
else
    ok "① 提交审批后 approval_instance_id 非空（$IID）"
fi
assert_eq "PENDING" "$(psql_q "SELECT status FROM biz_stock_transfer WHERE id='$TID'")" "库里状态 PENDING"
assert_eq "STOCK_TRANSFER|$TID|PENDING" \
    "$(psql_q "SELECT biz_type||'|'||biz_id::text||'|'||status FROM biz_flow_instance WHERE id='$IID'")" \
    "① 审批实例的 biz_type / biz_id / 状态对得上"
assert_eq "$SEED_VER" "$(psql_q "SELECT version_id FROM biz_flow_instance WHERE id='$IID'")" \
    "① 实例钉的是种子版本"

echo "==> 5. ② 审批中心待办里出现该调拨单"
call GET "/business/flow/task/todo?page=1&page_size=100"
assert_eq "200" "$REQ_CODE" "GET /business/flow/task/todo"
TODO_ID=$(printf '%s' "$REQ_BODY" | jq -r --arg iid "$IID" '.data.list[] | select(.instance_id == $iid) | .id' 2>/dev/null | head -1)
if [ -n "$TODO_ID" ]; then
    ok "② 待办列表里有该调拨单的待办（task=$TODO_ID）"
else
    bad "② 审批待办里找不到实例 $IID（响应：$REQ_BODY）"
fi
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_task WHERE instance_id='$IID' AND status='PENDING'")" \
    "② 待办行 1 条 PENDING"
assert_eq "ADMIN" "$(psql_q "SELECT assignee_source FROM biz_flow_task WHERE instance_id='$IID' AND status='PENDING'")" \
    "② 审批人来源 ADMIN（超管）"
TODO_TITLE=$(psql_q "SELECT title FROM biz_flow_instance WHERE id='$IID'")
assert_contains "$TODO_TITLE" "$TNO" "② 待办标题带调拨单号（审批中心一眼能对上单据）"

echo "==> 5.5 ④ 详情抽屉的「审批进度」数据源（前端 buildApprovalSteps 调的就是这个接口）"
call GET "/business/flow/instance/$IID"
assert_eq "200" "$REQ_CODE" "GET /business/flow/instance/{id}"
assert_eq "2" "$(jqv '.data.nodes | length')" "④ 时间线节点：已到达 2 个（开始 + 超管审批；末尾 END 要审批通过后才落节点行）"
assert_eq "1" "$(jqv '[.data.timeline[] | select(.status == "PENDING")] | length')" \
    "④ 时间线上有 1 条待审批（抽屉能画出「等待超管审批」）"
assert_eq "库存调拨审批" "$(jqv '.data.instance.flow_name')" "④ 抽屉能显示流程名（种子里带来的）"

echo "==> 6. ⑥ 审批中人工直推 APPROVED → 409 + 65030"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"APPROVED"}'
assert_eq "409" "$REQ_CODE" "审批中人工直推 APPROVED 被拒"
assert_eq "65030" "$(jqv '.code')" "业务码 65030"
assert_contains "$(jqv '.msg')" "审批中" "文案说明「正在审批中」"
assert_eq "PENDING" "$(psql_q "SELECT status FROM biz_stock_transfer WHERE id='$TID'")" "被拒后单据仍是 PENDING"

echo "==> 7. ③ 审批通过 → 调拨单 APPROVED"
call POST "/business/flow/task/$TODO_ID/approve" '{"comment":"同意调拨，请按期发货"}'
assert_eq "200" "$REQ_CODE" "POST /business/flow/task/{id}/approve"
assert_eq "APPROVED" "$(psql_q "SELECT status FROM biz_stock_transfer WHERE id='$TID'")" "③ 调拨单变 APPROVED"
assert_eq "true" "$(psql_q "SELECT (approved_at IS NOT NULL)::text FROM biz_stock_transfer WHERE id='$TID'")" \
    "③ approved_at 有值"
assert_eq "true" "$(psql_q "SELECT (approver_id IS NOT NULL)::text FROM biz_stock_transfer WHERE id='$TID'")" \
    "③ approver_id 记了实际审批人"
assert_eq "APPROVED" "$(psql_q "SELECT status FROM biz_flow_instance WHERE id='$IID'")" "③ 审批单落终态 APPROVED"

echo "==> 8. ④ 发货 → IN_TRANSIT → 收货 → DONE"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"IN_TRANSIT"}'
assert_eq "200" "$REQ_CODE" "发货（APPROVED → IN_TRANSIT）"
assert_eq "IN_TRANSIT" "$(jqv '.data.status')" "④ 状态在途"
assert_eq "true" "$(psql_q "SELECT (transit_warehouse_id IS NOT NULL)::text FROM biz_stock_transfer WHERE id='$TID'")" \
    "④ 在途仓已回填"

call PATCH "/business/erp/transfer/$TID/status" '{"status":"DONE"}'
assert_eq "200" "$REQ_CODE" "收货（IN_TRANSIT → DONE）"
assert_eq "DONE" "$(jqv '.data.status')" "④ 状态已完成"

echo "==> 9. ⑤ 库存双写（W3c 没坏）"
assert_num "95" "$(psql_q "SELECT qty FROM biz_inventory WHERE material_id='$MAT' AND warehouse_id='$WH_FROM' AND location_id IS NULL")" \
    "⑤ 调出仓 100 - 5 = 95"
assert_num "5" "$(psql_q "SELECT qty FROM biz_inventory WHERE material_id='$MAT' AND warehouse_id='$WH_TO' AND location_id IS NULL")" \
    "⑤ 调入仓 0 + 5 = 5"
assert_num "950" "$(psql_q "SELECT amount FROM biz_inventory WHERE material_id='$MAT' AND warehouse_id='$WH_FROM' AND location_id IS NULL")" \
    "⑤ 调出仓金额 1000 - 5×10 = 950"
assert_num "50" "$(psql_q "SELECT amount FROM biz_inventory WHERE material_id='$MAT' AND warehouse_id='$WH_TO' AND location_id IS NULL")" \
    "⑤ 调入仓金额（成本原价转移 5×10 = 50）"
assert_eq "IN|OUT" "$(psql_q "SELECT string_agg(txn_type, '|' ORDER BY txn_type) FROM biz_inventory_txn WHERE doc_type='STOCK_TRANSFER' AND doc_id='$TID'")" \
    "⑤ 两条流水（OUT + IN）"

echo "==> 10. ⑦ 撤回（PENDING → CANCELLED）终止审批单"
BODY=$(jq -nc --arg f "$WH_FROM" --arg t "$WH_TO" --arg m "$MAT" \
    '{from_warehouse_id:$f, to_warehouse_id:$t, reason:"P5 e2e 撤回用", items:[{material_id:$m, qty:"1"}]}')
call POST /business/erp/transfer "$BODY"
TID2=$(jqv '.data.id')
call PATCH "/business/erp/transfer/$TID2/status" '{"status":"PENDING"}'
IID2=$(jqv '.data.approval_instance_id')
if [ -z "$IID2" ] || [ "$IID2" = "null" ]; then bad "⑦ 第二张单提交后没有实例"; fi
call PATCH "/business/erp/transfer/$TID2/status" '{"status":"CANCELLED","reason":"临时不调了"}'
assert_eq "200" "$REQ_CODE" "撤回（PENDING → CANCELLED）"
assert_eq "CANCELLED" "$(psql_q "SELECT status FROM biz_stock_transfer WHERE id='$TID2'")" "⑦ 单据已取消"
assert_eq "CANCELLED" "$(psql_q "SELECT status FROM biz_flow_instance WHERE id='$IID2'")" "⑦ 在跑审批单被终止"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_task WHERE instance_id='$IID2' AND status='PENDING'")" \
    "⑦ 撤回后不留 PENDING 待办"

echo "==> 11. 收尾：先撤审批实例/待办，再删单据（用完即删）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_transfer WHERE from_warehouse_id='$WH_FROM' AND status NOT IN ('DONE','CANCELLED')")" \
    "收尾：本轮造的单据都已到终态（无悬挂单）"
CLEANED=1
if [ "${P5_KEEP_DATA:-0}" = "1" ]; then
    e2e_terminate_transfer_flow "$SCOPE"
    echo "    P5_KEEP_DATA=1：单据与实例保留供人工查看，但审批已终止（不会钉住超管夹具用户）"
else
    e2e_cleanup_transfer_flow "$SCOPE" || bad "收尾删数据没干净（见上一行残留计数）"
    e2e_assert_scope_clean "$SCOPE" ok bad
fi
e2e_report_pinned_fixtures

echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
