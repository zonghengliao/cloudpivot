#!/usr/bin/env bash
# ============================================================
# M4-03 采购计划 → 采购申请转换 —— 真接口演练（隔离实例 + 真 curl）
#
# 卡：t_03a60b7a（M4-03【M4·采购】采购计划：→ 采购申请转换）
#
# 跑什么（每一条都是卡面 / V0446 / V0448 的一句原话）：
#   ① 空库首启 → V0448 的两列（source_plan_id / plan_item_id）与按钮行落位
#   ② 建计划（2 行 × 2 仓库）→ 提交 → 审批人批 → APPROVED（真走 pp_approve）
#   ③ 草稿直接转申请 → 409（只有 APPROVED 能转）
#   ④ 转申请 → 200：一仓一单（2 张）、单号 PRQ-…、金额服务端算、备注带来源计划号
#   ⑤ 回写：明细 executed_qty / 单头 total_executed_qty 都对（库里查，不看响应自述）
#   ⑥ 溯源：申请单头 source_plan_id、申请明细 plan_item_id 都指回计划
#   ⑦ 转出来的单是**真申请单**：采购申请域详情接口读得到，仓库 / 部门 / 税率 / 数量对
#   ⑧ 再转一次 → 409（剩余可转量都是 0，不造空单）
#   ⑨ 部分转：指定数量 4/10 → 只转 4，剩余 6；再转剩余 → 6
#   ⑩ 超出剩余量 → 400（报文点出「超过剩余可转量」与计划行号）
#   ⑪ 公司级计划（无编制部门）不给 dept_id → 400；给了 → 200
#   ⑫ 跨计划的行 → 400（不属于这张采购计划）
#   ⑬ 权限：不带令牌 → 401；页面显形码 page 后端零引用（N14 台账）
#   ⑭ 迁移幂等：V0448 再跑 rc=0；down 两列消失；up 两列回来
#   ⑮ 菜单不变量 8 条
#
# 用法（99 开发机）：
#   bash scripts/verify/m403_plan_to_requisition_drill.sh
#   DBHOST=127.0.0.1 bash …          # 145 测试机上跑（走 loopback）
#   环境变量：DBUSER/DBPASS/DBOWNER/PORT 可覆盖
#
# 退出码：0 = 全过；1 = 有断言失败；2 = 环境问题（建库/起不来）
# ============================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DBHOST="${DBHOST:-172.18.10.145}"
DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-cloudpivot}"
DBPASS="${DBPASS:-fbb1263691ff75c31ec99eaf258138b4}"
DBOWNER="${DBOWNER:-cloudpivot}"
DB="${DB:-cp_m403_drill}"
PORT="${PORT:-18098}"
BASE="http://127.0.0.1:$PORT/api"
MROOT="$ROOT/backend/app/migrations"
LOG=/root/m403-drill-server.log
PIDFILE=/root/m403-drill-server.pid
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASS="${ADMIN_PASS:-Admin@123456}"
APPROVER_USER="m403_approver"
APPROVER_PASS="$ADMIN_PASS"

UNIT="d4030001-0000-4000-8000-000000000001"
WH_A="d4030001-0000-4000-8000-000000000002"
WH_B="d4030001-0000-4000-8000-000000000003"
MAT_A="d4030001-0000-4000-8000-000000000011"
MAT_B="d4030001-0000-4000-8000-000000000012"
DEPT="d4030001-0000-4000-8000-000000000021"
APPROVER_ID="d4030001-0000-4000-8000-0000000000a1"

PASS=0
FAIL=0
TOKEN=""
ATOKEN=""
REQ_CODE=0
REQ_BODY=""

ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi; }
assert_contains() {
    case "$1" in
        *"$2"*) ok "$3" ;;
        *) bad "$3：文本里没有 [$2]，实际 [$(printf '%s' "$1" | head -c 300)]" ;;
    esac
}
assert_num() {
    if awk -v a="$1" -v b="$2" 'BEGIN { if (a == "" || b == "" || b == "null") exit 1; exit !((a + 0) == (b + 0)) }'; then
        ok "$3（$2）"
    else
        bad "$3：期望 [$1] 实际 [$2]"
    fi
}

psql_q() { PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -Atc "$1" 2>&1 | head -1; }
psql_x() { PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q -c "$1" 2>&1 | head -3; }

jqv() { printf '%s' "$REQ_BODY" | jq -r "$1" 2>/dev/null; }

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

# 用审批人令牌调（审批 / 以及任何需要另一个身份的动作）
call_as_approver() {
    local method="$1" path="$2" body="${3:-}" tmp
    tmp=$(mktemp)
    if [ -n "$body" ]; then
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $ATOKEN" -H 'Content-Type: application/json' \
            --data-binary "$body" "$BASE$path")
    else
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $ATOKEN" "$BASE$path")
    fi
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}

stop_server() {
    local pids=""
    # 按监听端口找进程（pidfile 里记的是 subshell 的 pid，不可靠）
    pids=$(ss -ltnp 2>/dev/null | grep ":$PORT " | grep -oE 'pid=[0-9]+' | cut -d= -f2 | tr '\n' ' ')
    if [ -f "$PIDFILE" ]; then pids="$pids $(cat "$PIDFILE")"; fi
    for p in $pids; do
        [ -n "$p" ] && kill "$p" 2>/dev/null
    done
    for _ in $(seq 1 30); do
        ss -ltn 2>/dev/null | grep -q ":$PORT " || break
        sleep 1
    done
    for p in $pids; do
        [ -n "$p" ] && kill -9 "$p" 2>/dev/null
    done
    # 兜底：踢掉残留的库会话（否则 dropdb 报「being accessed by other users」）
    psql_q "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB'" >/dev/null 2>&1
    rm -f "$PIDFILE"
}
trap 'stop_server' EXIT

echo '==> 0. 建**空**演练库（迁移与种子由实例首启自己跑完：这才是空库首启的真实路径）'
export PGPASSWORD="$DBPASS"
stop_server
dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" >/dev/null 2>&1
createdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -O "$DBOWNER" "$DB" || {
    echo "FAIL: 建库失败" >&2; exit 2; }
echo '    空库已建（0 表）；迁移由实例启动时按版本号升序执行'

echo '==> 1. 起隔离实例（端口 '"$PORT"'，库 '"$DB"'）'
[ -x "$ROOT/backend/target/debug/cloudpivot" ] || (cd "$ROOT/backend" && cargo build -q) || {
    echo 'FAIL: 后端没编译出来' >&2; exit 2; }
export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST="$DBHOST"
export NIK_DATABASE_PORT="$DBPORT"
export NIK_DATABASE_USER="$DBUSER"
export NIK_DATABASE_PASSWORD="$DBPASS"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
rm -f "$LOG"
(cd "$ROOT/backend" && setsid nohup ./target/debug/cloudpivot >"$LOG" 2>&1 </dev/null & echo $! >"$PIDFILE")
for _ in $(seq 1 180); do
    if [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)" = "200" ]; then
        break
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "FAIL: 实例退出了，看 $LOG" >&2; tail -30 "$LOG" >&2; exit 2; }
    sleep 1
done
[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")" = "200" ] || {
    echo 'FAIL: 健康检查超时' >&2; tail -30 "$LOG" >&2; exit 2; }
ok '实例已起（/health 200）'

echo '==> 2. V0448 落位自检（两列 / 两个索引 / 1 行菜单）'
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_purchase_requisition' AND column_name='source_plan_id'")" "1" '申请单头有 source_plan_id'
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_purchase_requisition_item' AND column_name='plan_item_id'")" "1" '申请明细有 plan_item_id'
assert_eq "$(psql_q "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND indexname IN ('idx_biz_purchase_requisition_source_plan','idx_biz_prqi_plan_item')")" "2" '两个溯源索引在位'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code='business:erp:purchase-plan:to-requisition' AND menu_type=3 AND deleted_at IS NULL")" "1" '转申请按钮行 1 行'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-00000000447%' AND deleted_at IS NULL")" "7" '采购计划菜单 7 行（1 页面 + 6 按钮）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code='pp_approve' AND deleted_at IS NULL")" "1" 'pp_approve 流程在位'

echo '==> 3. 夹具（基本单位 / 两个仓库 / 两个物料 / 一个部门 / 一个审批人）'
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status)
        VALUES ('$UNIT', 'M403-U', 'M403 基本单位', 1, 1, 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, is_leaf, level, status)
        VALUES ('$WH_A', 'M403-WH-A', 'M403 仓库 A', '/M403-WH-A', 1, 1, 1),
               ('$WH_B', 'M403-WH-B', 'M403 仓库 B', '/M403-WH-B', 1, 1, 1)
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status)
        VALUES ('$MAT_A', 'M403-MAT-A', 'M403 物料 A', '$UNIT', 'RAW', 1),
               ('$MAT_B', 'M403-MAT-B', 'M403 物料 B', '$UNIT', 'RAW', 1)
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO sys_dept (id, dept_code, dept_name, status)
        VALUES ('$DEPT', 'M403-DEPT', 'M403 采购部', 1) ON CONFLICT (id) DO NOTHING;"
# 审批人：复制 admin 的口令哈希（不知道明文就登录不了，只能这么造一个可用账号）
psql_x "INSERT INTO sys_user (id, username, real_name, password, status, must_change_password)
        SELECT '$APPROVER_ID'::uuid, '$APPROVER_USER', 'M403 审批人', password, 1, false
          FROM sys_user WHERE username = '$ADMIN_USER'
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO sys_user_role (user_id, role_id)
        SELECT '$APPROVER_ID'::uuid, r.id FROM sys_role r
         WHERE r.role_code IN ('PURCHASE_MANAGER', 'FINANCE_MANAGER') AND r.deleted_at IS NULL
        ON CONFLICT DO NOTHING;"
assert_eq "$(psql_q "SELECT count(*) FROM sys_user WHERE username='$APPROVER_USER'")" "1" '审批人夹具已建'

echo '==> 4. 登录 admin'
tmp=$(mktemp)
REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" "$BASE/system/auth/login")
REQ_BODY=$(cat "$tmp"); rm -f "$tmp"
TOKEN=$(jqv '.data.access_token')
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo "FAIL: 登录失败 $REQ_BODY" >&2; exit 2; }
ok '登录拿到令牌'
if [ "$(jqv '.data.must_change_password')" = "true" ]; then
    NEW_PASS="M403_${ADMIN_PASS}_new"
    curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" "$BASE/system/auth/password"
    ADMIN_PASS="$NEW_PASS"
    tmp=$(mktemp)
    curl -s -o "$tmp" -X POST -H 'Content-Type: application/json' \
        --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" "$BASE/system/auth/login"
    TOKEN=$(jq -r '.data.access_token' <"$tmp"); rm -f "$tmp"
    ok '首登强制改密后重新登录'
fi

# 审批人登录（复制的是改密前的哈希 → 口令恒等于原始 ADMIN_PASS 的初始值）
APPROVER_PASS="Admin@123456"
tmp=$(mktemp)
curl -s -o "$tmp" -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$APPROVER_USER\",\"password\":\"$APPROVER_PASS\"}" "$BASE/system/auth/login"
ATOKEN=$(jq -r '.data.access_token // empty' <"$tmp"); rm -f "$tmp"
if [ -n "$ATOKEN" ]; then ok '审批人登录成功'; else bad '审批人登录失败（后面走审批的用例会被跳过）'; fi

echo '==> 5. 建计划（2 行 × 2 仓库）+ 草稿直接转申请 → 409'
BODY=$(cat <<JSON
{"plan_name":"M403 2026 年度采购计划","plan_type":"ANNUAL","plan_year":2026,"dept_id":"$DEPT",
 "remark":"M403 演练",
 "items":[
   {"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"10","planned_price":"2.5"},
   {"material_id":"$MAT_B","warehouse_id":"$WH_B","planned_qty":"4","planned_price":"100"}]}
JSON
)
call POST /business/erp/purchase-plan "$BODY"
assert_eq "$REQ_CODE" "200" '建计划 200'
PLAN_ID=$(jqv '.data.plan.id')
PLAN_NO=$(jqv '.data.plan.plan_no')
assert_eq "$(jqv '.data.plan.status')" "DRAFT" '计划是草稿'
assert_num "$(jqv '.data.plan.total_planned_amount')" "425" '计划金额 = 25 + 400'
call POST "/business/erp/purchase-plan/$PLAN_ID/to-requisition" '{}'
assert_eq "$REQ_CODE" "409" '草稿不能转申请（409）'
assert_contains "$REQ_BODY" "APPROVED" '409 报文点出前置条件'

echo '==> 6. 提交 + 审批人到 APPROVED（真走 pp_approve）'
call POST "/business/erp/purchase-plan/$PLAN_ID/submit" '{"reason":"M403 演练提交"}'
assert_eq "$REQ_CODE" "200" '提交 200'
INSTANCE=$(jqv '.data.plan.approval_instance_id')
assert_eq "$(jqv '.data.plan.status')" "SUBMITTED" '提交后 SUBMITTED'
call POST "/business/erp/purchase-plan/$PLAN_ID/to-requisition" '{}'
assert_eq "$REQ_CODE" "409" '审批中不能转申请（409）'
if [ -n "$ATOKEN" ]; then
    for _ in 1 2 3; do
        TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INSTANCE'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
        [ -n "$TASK" ] || break
        call_as_approver POST "/business/flow/task/$TASK/approve" '{"comment":"M403 演练同意"}'
        [ "$REQ_CODE" = "200" ] || bad "审批接口返回 $REQ_CODE：$REQ_BODY"
    done
    assert_eq "$(psql_q "SELECT status FROM biz_purchase_plan WHERE id='$PLAN_ID'::uuid")" "APPROVED" '审批通过后计划 APPROVED'
fi

echo '==> 7. 转申请（全量）→ 一仓一单 2 张'
call POST "/business/erp/purchase-plan/$PLAN_ID/to-requisition" '{"tax_rate":"13","remark":"M403 演练转单"}'
assert_eq "$REQ_CODE" "200" '转申请 200'
assert_eq "$(jqv '.data.requisitions | length')" "2" '一仓一单：2 张申请单'
assert_eq "$(jqv '.data.converted_lines')" "2" '转出 2 行'
assert_num "$(jqv '.data.converted_qty')" "14" '转出数量 = 10 + 4'
assert_num "$(jqv '.data.converted_amount')" "425" '转出金额 = 25 + 400'
REQ1=$(jqv '.data.requisitions[0].requisition_id')
REQ1_NO=$(jqv '.data.requisitions[0].req_no')
REQ2=$(jqv '.data.requisitions[1].requisition_id')
REQ2_NO=$(jqv '.data.requisitions[1].req_no')
assert_eq "$(printf '%s' "$REQ1_NO" | wc -c | tr -d ' ')" "19" '申请单号 19 位（PRQ-yyyyMMdd-NNNNNN）'
assert_contains "$REQ1_NO" "PRQ-" '申请单号以 PRQ- 开头'
assert_contains "$REQ2_NO" "PRQ-" '第二张单号以 PRQ- 开头'
assert_eq "$(jqv '.data.requisitions[0].warehouse_id')" "$WH_A" '第一张是仓库 A'
assert_eq "$(jqv '.data.requisitions[1].warehouse_id')" "$WH_B" '第二张是仓库 B'
assert_num "$(jqv '.data.requisitions[0].est_amount')" "25" '仓库 A 单金额 25'
assert_num "$(jqv '.data.requisitions[1].est_amount')" "400" '仓库 B 单金额 400'
assert_eq "$(jqv '.data.requisitions[0].dept_id')" "$DEPT" '申请部门继承计划的编制部门'
assert_eq "$(jqv '.data.requisitions[0].lines[0].plan_line_no')" "1" '明细带回计划行号'

echo '==> 8. 回写与溯源（库里查，不看响应自述）'
assert_num "$(jqv '.data.plan.total_executed_qty')" "14" '响应里单头已执行量 = 14'
assert_num "$(psql_q "SELECT total_executed_qty FROM biz_purchase_plan WHERE id='$PLAN_ID'::uuid")" "14" '库里单头 total_executed_qty = 14'
assert_num "$(psql_q "SELECT sum(executed_qty) FROM biz_purchase_plan_item WHERE plan_id='$PLAN_ID'::uuid")" "14" '库里明细已执行量合计 = 14'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_plan_item WHERE plan_id='$PLAN_ID'::uuid AND executed_qty = planned_qty")" "2" '两行都已执行满'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_requisition WHERE source_plan_id='$PLAN_ID'::uuid")" "2" '两张申请单都记了来源计划'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_requisition_item it JOIN biz_purchase_requisition r ON r.id=it.requisition_id WHERE r.source_plan_id='$PLAN_ID'::uuid AND it.plan_item_id IS NOT NULL")" "2" '申请明细都记了来源计划行'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_requisition_item it JOIN biz_purchase_requisition r ON r.id=it.requisition_id WHERE r.source_plan_id='$PLAN_ID'::uuid AND it.tax_rate = 13")" "2" '整单税率取请求值 13'

echo '==> 9. 转出来的单是真申请单（采购申请域详情接口）'
call GET "/business/erp/purchase-requisition/$REQ1"
assert_eq "$REQ_CODE" "200" '采购申请详情 200'
assert_eq "$(jqv '.data.requisition.status')" "DRAFT" '转出来的申请单是草稿（要再走审批）'
assert_eq "$(jqv '.data.requisition.warehouse_id')" "$WH_A" '收货仓库 = 计划行仓库'
assert_eq "$(jqv '.data.requisition.dept_id')" "$DEPT" '申请部门 = 计划编制部门'
assert_num "$(jqv '.data.requisition.est_amount')" "25" '预估金额 25'
assert_contains "$(jqv '.data.requisition.remark')" "由采购计划" '备注写清来源'
assert_contains "$(jqv '.data.requisition.remark')" "$PLAN_NO" '备注带计划单号'
assert_contains "$(jqv '.data.requisition.remark')" "M403 演练转单" '用户备注带上了'

echo '==> 10. 再转一次 → 409（剩余可转量都是 0，不造空单）'
call POST "/business/erp/purchase-plan/$PLAN_ID/to-requisition" '{}'
assert_eq "$REQ_CODE" "409" '全转完再转 = 409'
assert_contains "$REQ_BODY" "剩余可转量都是 0" '409 报文说清原因'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_requisition WHERE source_plan_id='$PLAN_ID'::uuid")" "2" '没有造出第三张空单'

echo '==> 11. 部分转：指定 4/10 → 剩余 6；再转剩余 → 6'
BODY2=$(cat <<JSON
{"plan_name":"M403 部分转","plan_type":"ANNUAL","plan_year":2027,"dept_id":"$DEPT",
 "items":[{"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"10","planned_price":"3"}]}
JSON
)
call POST /business/erp/purchase-plan "$BODY2"
PLAN2=$(jqv '.data.plan.id')
call POST "/business/erp/purchase-plan/$PLAN2/submit" '{}'
INSTANCE2=$(jqv '.data.plan.approval_instance_id')
if [ -n "$ATOKEN" ]; then
    for _ in 1 2 3; do
        TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INSTANCE2'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
        [ -n "$TASK" ] || break
        call_as_approver POST "/business/flow/task/$TASK/approve" '{"comment":"M403 部分转同意"}'
    done
fi
assert_eq "$(psql_q "SELECT status FROM biz_purchase_plan WHERE id='$PLAN2'::uuid")" "APPROVED" '第二张计划 APPROVED'
ITEM2=$(psql_q "SELECT id FROM biz_purchase_plan_item WHERE plan_id='$PLAN2'::uuid")
PART=$(cat <<JSON
{"items":[{"plan_item_id":"$ITEM2","qty":"4"}]}
JSON
)
call POST "/business/erp/purchase-plan/$PLAN2/to-requisition" "$PART"
assert_eq "$REQ_CODE" "200" '部分转 200'
assert_num "$(jqv '.data.converted_qty')" "4" '只转了 4'
assert_num "$(jqv '.data.requisitions[0].lines[0].remaining_after')" "6" '剩余 6'
assert_num "$(psql_q "SELECT executed_qty FROM biz_purchase_plan_item WHERE id='$ITEM2'::uuid")" "4" '库里已执行量 4'
call POST "/business/erp/purchase-plan/$PLAN2/to-requisition" '{}'
assert_eq "$REQ_CODE" "200" '再转剩余 200'
assert_num "$(jqv '.data.converted_qty')" "6" '第二次只转剩余的 6'
assert_num "$(psql_q "SELECT executed_qty FROM biz_purchase_plan_item WHERE id='$ITEM2'::uuid")" "10" '两次合计把计划量转完'

echo '==> 12. 负向：超出剩余量 / 公司级计划缺部门 / 跨计划的行 / 无令牌'
BODY3=$(cat <<JSON
{"plan_name":"M403 超量","plan_type":"ANNUAL","plan_year":2028,"dept_id":"$DEPT",
 "items":[{"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"5","planned_price":"1"}]}
JSON
)
call POST /business/erp/purchase-plan "$BODY3"
PLAN3=$(jqv '.data.plan.id')
call POST "/business/erp/purchase-plan/$PLAN3/submit" '{}'
INSTANCE3=$(jqv '.data.plan.approval_instance_id')
if [ -n "$ATOKEN" ]; then
    for _ in 1 2 3; do
        TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INSTANCE3'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
        [ -n "$TASK" ] || break
        call_as_approver POST "/business/flow/task/$TASK/approve" '{"comment":"M403 超量同意"}'
    done
fi
ITEM3=$(psql_q "SELECT id FROM biz_purchase_plan_item WHERE plan_id='$PLAN3'::uuid")
OVER=$(cat <<JSON
{"items":[{"plan_item_id":"$ITEM3","qty":"6"}]}
JSON
)
call POST "/business/erp/purchase-plan/$PLAN3/to-requisition" "$OVER"
assert_eq "$REQ_CODE" "400" '超出剩余量 = 400'
assert_contains "$REQ_BODY" "超过剩余可转量" '报文点出剩余量'
FOREIGN=$(cat <<JSON
{"items":[{"plan_item_id":"$ITEM2","qty":"1"}]}
JSON
)
call POST "/business/erp/purchase-plan/$PLAN3/to-requisition" "$FOREIGN"
assert_eq "$REQ_CODE" "400" '跨计划的行 = 400'
assert_contains "$REQ_BODY" "不属于这张采购计划" '报文说清原因'
assert_num "$(psql_q "SELECT executed_qty FROM biz_purchase_plan_item WHERE id='$ITEM3'::uuid")" "0" '失败两次都没写已执行量'

# 公司级计划（没有编制部门）→ 不给 dept_id 必须 400
BODY4=$(cat <<JSON
{"plan_name":"M403 公司级","plan_type":"ANNUAL","plan_year":2029,
 "items":[{"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"2","planned_price":"5"}]}
JSON
)
call POST /business/erp/purchase-plan "$BODY4"
PLAN4=$(jqv '.data.plan.id')
call POST "/business/erp/purchase-plan/$PLAN4/submit" '{}'
INSTANCE4=$(jqv '.data.plan.approval_instance_id')
if [ -n "$ATOKEN" ]; then
    for _ in 1 2 3; do
        TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INSTANCE4'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
        [ -n "$TASK" ] || break
        call_as_approver POST "/business/flow/task/$TASK/approve" '{"comment":"M403 公司级同意"}'
    done
fi
call POST "/business/erp/purchase-plan/$PLAN4/to-requisition" '{}'
assert_eq "$REQ_CODE" "400" '公司级计划不给 dept_id = 400'
assert_contains "$REQ_BODY" "dept_id" '报文点出缺哪个字段'
call POST "/business/erp/purchase-plan/$PLAN4/to-requisition" "{\"dept_id\":\"$DEPT\"}"
assert_eq "$REQ_CODE" "200" '给了 dept_id 就能转'

NOPERM=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/business/erp/purchase-plan/$PLAN2/to-requisition")
assert_eq "$NOPERM" "401" '不带令牌 = 401'

echo '==> 13. 库内落点核对（迁移往返之前查，往返会丢指针 —— 见 15）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_plan WHERE plan_name LIKE 'M403%' AND deleted_at IS NULL")" "4" '演练库 4 张计划（2026 全转 / 2027 分两批 / 2028 超量被拒 / 2029 公司级）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_requisition WHERE source_plan_id IS NOT NULL")" "5" '演练库 5 张申请单（2 + 2 + 1）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_requisition_item WHERE plan_item_id IS NOT NULL")" "5" '申请明细 5 行、逐行都带来源计划行'

echo '==> 14. 迁移幂等 + down/up 往返'
if PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
    -f "$MROOT/business/V0448__purchase_plan_to_requisition.up.sql" >/tmp/m403_reapply.log 2>&1; then
    ok 'V0448 再跑一遍 rc=0（幂等）'
else
    bad "V0448 重跑失败：$(tail -3 /tmp/m403_reapply.log | tr '\n' ' ')"
fi
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004477'::uuid AND deleted_at IS NULL")" "1" '重跑后按钮仍 1 行'
if PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
    -f "$MROOT/business/V0448__purchase_plan_to_requisition.down.sql" >/tmp/m403_down.log 2>&1; then
    ok 'V0448 down rc=0'
else
    bad "V0448 down 失败：$(tail -3 /tmp/m403_down.log | tr '\n' ' ')"
fi
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_purchase_requisition' AND column_name='source_plan_id'")" "0" 'down 后 source_plan_id 没了'
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_purchase_requisition_item' AND column_name='plan_item_id'")" "0" 'down 后 plan_item_id 没了'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004477'::uuid")" "0" 'down 后按钮没了'
if PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
    -f "$MROOT/business/V0448__purchase_plan_to_requisition.up.sql" >/tmp/m403_up.log 2>&1; then
    ok 'V0448 再 up rc=0（可复位）'
else
    bad "V0448 再 up 失败：$(tail -3 /tmp/m403_up.log | tr '\n' ' ')"
fi
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_purchase_requisition' AND column_name='source_plan_id'")" "1" '复位后 source_plan_id 回来'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004477'::uuid AND deleted_at IS NULL")" "1" '复位后按钮回来'

echo '==> 15. 死权限码闸（perm_menu_gate：注解里的码必须都能授出去）+ 菜单不变量（8 条）'
if NIK_DATABASE_HOST="$DBHOST" NIK_DATABASE_PORT="$DBPORT" NIK_DATABASE_USER="$DBUSER" \
   NIK_DATABASE_PASSWORD="$DBPASS" NIK_DATABASE_NAME="$DB" \
   python3 "$ROOT/scripts/verify/perm_menu_gate.py" 2>&1 | tee /tmp/m403_perm.out | grep -q 'missing_in_menu = 0'; then
    ok 'perm_menu_gate：missing_in_menu = 0（含新增的 to-requisition 码）'
else
    bad "perm_menu_gate 判红：$(grep -E 'missing_in_menu|·' /tmp/m403_perm.out | head -5 | tr '\n' ' ')"
fi
if PGPASSWORD="$DBPASS" PGHOST="$DBHOST" PGPORT="$DBPORT" PGUSER="$DBUSER" \
    bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" 2>&1 | tee /tmp/m403_menu.out | grep -q '结果: 全绿'; then
    ok 'menu_invariants 8 条全绿'
else
    bad "menu_invariants 未全绿：$(grep -E '❌|结果' /tmp/m403_menu.out | tr '\n' ' ')"
fi

echo '==> 16. 收尾（down/up 的副作用：指针列被重建，值归 NULL —— 迁移注释已写明）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_requisition WHERE source_plan_id IS NOT NULL")" "0" 'down 重建列后 source_plan_id 归 NULL（down 只用于开发/演练复位）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_requisition WHERE deleted_at IS NULL")" "5" '申请单本身一张没少（down 删的是指针列，不是单据）'
stop_server
psql_q "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB'" >/dev/null 2>&1
sleep 1
dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" && ok '演练库已删'

echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
