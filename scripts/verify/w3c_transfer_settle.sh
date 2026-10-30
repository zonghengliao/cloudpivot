#!/usr/bin/env bash
# ============================================================
# W3c 调拨结算 e2e（真实 curl + DB 直查验证）
#
#   覆盖：建单（成品D 5 件，分仓 → 总仓）→ PENDING → APPROVED → IN_TRANSIT → DONE
#         ① 调出仓 qty / amount 按原价减少
#         ② 调入仓 qty / amount 增加、avg_cost 移动加权重算
#         ③ 序列号 warehouse_id 全部到总仓
#         ④ 流水 2 条（OUT TRANSFER_OUT + IN TRANSFER_IN）、金额守恒
#         ⑤ 明细 shipped_qty / received_qty / unit_cost / amount 回填 + 单据头金额
#         ⑥ DONE 幂等：重复推 DONE → 409，库存不动
#         ⑦ 库存不足 → 409 + 调拨单仍 IN_TRANSIT（回滚）
#
# 用法：
#
#   export W3C_DB_PASSWORD='<cloudpivot 库口令>'
#   bash scripts/verify/w3c_transfer_settle.sh
#
#   可选：
#     W3C_BASE=http://127.0.0.1:18087/api   实例地址（默认 18087）
#     W3C_DB_NAME=cloudpivot_w3c_e2e        库名（默认 cloudpivot_w3c_e2e）
#     W3C_ADMIN_PASSWORD=Nereus792379       admin 口令（dev/test 种子）
#     W3C_BOOTSTRAP=1                       自建库 + 自起隔离实例（默认 1）
#     W3C_BIN=/root/w3c-target/debug/cloudpivot   实例二进制（默认值同）
#     W3C_KEEP_SERVER=1                     跑完不杀实例（默认杀掉自己起的那个）
#
# 退出码 = 是否有断言失败（0 = 全过）。
#
# 审批这一步为什么是「人工推 APPROVED」：W3b 实测「只用快照发布的流程，engine::start
# 不派生 biz_flow_task 待办行（节点停留 PENDING / approver_count=0）」，所以 e2e 走卡面
# 允许的降级路径（PENDING → APPROVED 人工推）。审批回调路径（on_transfer_settled）由
# transfer_handler 的真库单测 approval_callback_moves_transfer 覆盖。
# ============================================================
set -uo pipefail

BASE="${W3C_BASE:-http://127.0.0.1:18087/api}"
DBN="${W3C_DB_NAME:-cloudpivot_w3c_e2e}"
DBH="${W3C_DB_HOST:-127.0.0.1}"
DBU="${W3C_DB_USER:-cloudpivot}"
ADMIN_USER="${W3C_ADMIN:-admin}"
ADMIN_PASS="${W3C_ADMIN_PASSWORD:-Nereus792379}"
BOOTSTRAP="${W3C_BOOTSTRAP:-1}"
BIN="${W3C_BIN:-/root/w3c-target/debug/cloudpivot}"
PORT="${W3C_PORT:-18087}"
: "${W3C_DB_PASSWORD:?请先 export W3C_DB_PASSWORD（cloudpivot 库口令）}"

# 夹具（固定 UUID，前缀 W3C-E2E，重跑安全）
WH_FROM="d3ce0000-0000-4000-8000-000000000001"
WH_TO="d3ce0000-0000-4000-8000-000000000002"
UNIT="d3ce0000-0000-4000-8000-000000000003"
M_D="d3ce0000-0000-4000-8000-000000000011"
S1="d3ce0000-0000-4000-8000-000000000021"
S2="d3ce0000-0000-4000-8000-000000000022"
S3="d3ce0000-0000-4000-8000-000000000023"
S4="d3ce0000-0000-4000-8000-000000000024"
S5="d3ce0000-0000-4000-8000-000000000025"
S6="d3ce0000-0000-4000-8000-000000000026"
S7="d3ce0000-0000-4000-8000-000000000027"

PASS=0
FAIL=0
TOKEN=""
AUTH_HEADER=""

psql_q() {
    PGPASSWORD="$W3C_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -Atc "$1" 2>&1 | head -1
}

psql_x() {
    PGPASSWORD="$W3C_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -q -v ON_ERROR_STOP=1 -c "$1" 2>&1 | head -3
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

# 发请求：REQ_CODE / REQ_BODY 写回全局（**不要用 $(...) 包住**，子 shell 里赋值会丢）
REQ_CODE=0
REQ_BODY=""
call() {
    local method="$1" path="$2" body="${3:-}" auth="${4:-}" tmp
    tmp=$(mktemp)
    if [ -n "$auth" ]; then
        AUTH_HEADER="Authorization: Bearer $auth"
    else
        AUTH_HEADER="Authorization: Bearer $TOKEN"
    fi
    if [ -n "$body" ]; then
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "$AUTH_HEADER" -H 'Content-Type: application/json' \
            --data-binary "$body" "$BASE$path")
    else
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "$AUTH_HEADER" "$BASE$path")
    fi
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}

jqv() { printf '%s' "$REQ_BODY" | jq -r "$1" 2>/dev/null; }

# ---------- 0. 自建库 + 自起隔离实例（迁移随启动自动跑到最新） ----------
SERVER_PID=""
if [ "$BOOTSTRAP" = "1" ]; then
    echo "==> 0. 准备隔离库 $DBN 与实例 :$PORT"
    if [ ! -x "$BIN" ]; then
        bad "实例二进制不存在：$BIN（先 cargo build，或 export W3C_BIN=…）"
        echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
        exit 1
    fi
    EXISTS=$(PGPASSWORD="$W3C_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d postgres -Atc \
        "SELECT 1 FROM pg_database WHERE datname = '$DBN'" 2>/dev/null | head -1)
    if [ "$EXISTS" != "1" ]; then
        su - postgres -c "createdb -O $DBU $DBN" || {
            bad "建库失败（createdb -O $DBU $DBN）"
            echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
            exit 1
        }
        ok "建库 $DBN（owner=$DBU）"
    else
        ok "复用已存在的库 $DBN"
    fi
    if curl -s -o /dev/null -m 2 "$BASE/health"; then
        ok "实例已在 $BASE 上（不再另起）"
    else
        set -a
        # shellcheck disable=SC1091
        . /etc/cloudpivot/cloudpivot-dev.env
        set +a
        export NIK_PROFILE=dev
        export NIK_DATABASE_NAME="$DBN"
        export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
        export NIK_REDIS_URL="redis://127.0.0.1:6379/9"
        # config/ 是按 CWD 解析的，必须在 backend/ 目录下起（否则退回默认配置、DB 不连）
        cd "${W3C_REPO:-/root/w3c-clone}/backend" || {
            bad "仓库目录不存在：${W3C_REPO:-/root/w3c-clone}/backend"
            echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
            exit 1
        }
        setsid nohup "$BIN" > /tmp/w3c-e2e-server.log 2>&1 < /dev/null &
        sleep 2
        SERVER_PID=$(pgrep -f "^$BIN$" | head -1)
        echo "${SERVER_PID:-}" > /tmp/w3c-e2e.pid
        for _ in $(seq 1 40); do
            if curl -s -o /dev/null -m 2 "$BASE/health"; then break; fi
            sleep 1
        done
        if curl -s -o /dev/null -m 2 "$BASE/health"; then
            ok "起隔离实例 :$PORT（pid=$SERVER_PID，日志 /tmp/w3c-e2e-server.log）"
        else
            bad "实例未起来（看 /tmp/w3c-e2e-server.log）"
            tail -5 /tmp/w3c-e2e-server.log
            echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
            exit 1
        fi
    fi
    VERSION=$(psql_q "SELECT max(version::int) FROM schema_migrations")
    if [ "${VERSION:-0}" -ge 144 ] 2>/dev/null; then
        ok "迁移已到 V$VERSION（含 V0139/V0140 调拨三表）"
    else
        bad "迁移版本异常：[$VERSION]（期望 >= 144）"
    fi
fi

# ---------- 1. 清理上一轮 e2e 残留（只动 W3C-E2E 固定夹具） ----------
echo "==> 1. 清理上一轮 e2e 残留"
psql_x "DELETE FROM biz_inventory_txn WHERE warehouse_id IN ('$WH_FROM', '$WH_TO')" >/dev/null
psql_x "DELETE FROM biz_stock_transfer_item_serial WHERE item_id IN (
          SELECT i.id FROM biz_stock_transfer_item i JOIN biz_stock_transfer t ON t.id = i.transfer_id
           WHERE t.from_warehouse_id IN ('$WH_FROM', '$WH_TO'))" >/dev/null
psql_x "DELETE FROM biz_stock_transfer_item WHERE transfer_id IN (
          SELECT id FROM biz_stock_transfer WHERE from_warehouse_id IN ('$WH_FROM', '$WH_TO'))" >/dev/null
psql_x "DELETE FROM biz_stock_transfer WHERE from_warehouse_id IN ('$WH_FROM', '$WH_TO')" >/dev/null
psql_x "DELETE FROM biz_inventory WHERE warehouse_id IN ('$WH_FROM', '$WH_TO')" >/dev/null
psql_x "DELETE FROM biz_material_serial WHERE id IN ('$S1','$S2','$S3','$S4','$S5','$S6','$S7')" >/dev/null
psql_x "DELETE FROM biz_inventory_location WHERE warehouse_id IN ('$WH_FROM', '$WH_TO')" >/dev/null
psql_x "DELETE FROM biz_material WHERE id = '$M_D'" >/dev/null
psql_x "DELETE FROM biz_warehouse WHERE id IN ('$WH_FROM', '$WH_TO')" >/dev/null
ok "清掉上一轮夹具（仓 / 物料 / 序列号 / 库存 / 单据）"

# ---------- 2. 夹具：仓 / 单位 / 一物一码物料（成品D）/ 2+5 个序列号 / 库存台账 ----------
echo "==> 2. 造夹具（成品D：一物一码；分仓 5 件 @10，总仓 2 件 @20）"
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order)
        VALUES ('$UNIT', 'W3C-PCS', '个', 1, 1, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type,
                                 allow_negative, status, sort_order, parent_id, level,
                                 warehouse_path, is_leaf)
        VALUES ('$WH_FROM', 'WH-W3C-1', 'W3C 分仓', 'NORMAL', 0, 1, 1, NULL, 1, '/w3c1/', 1),
               ('$WH_TO',   'WH-W3C-2', 'W3C 总仓', 'NORMAL', 0, 1, 2, NULL, 1, '/w3c2/', 1)
        ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type,
                                 safety_stock, costing_method, tax_rate, status, serial_required)
        VALUES ('$M_D', 'W3C-E2E-D', 'W3C 成品D', '$UNIT', 'FINISHED', 0, 'MOVING_AVG', 0, 1, true)
        ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status)
        VALUES ('$S1', 'W3C-E2E-SN-1', '$M_D', '$WH_FROM', 'IN_STOCK'),
               ('$S2', 'W3C-E2E-SN-2', '$M_D', '$WH_FROM', 'IN_STOCK'),
               ('$S3', 'W3C-E2E-SN-3', '$M_D', '$WH_FROM', 'IN_STOCK'),
               ('$S4', 'W3C-E2E-SN-4', '$M_D', '$WH_FROM', 'IN_STOCK'),
               ('$S5', 'W3C-E2E-SN-5', '$M_D', '$WH_FROM', 'IN_STOCK'),
               ('$S6', 'W3C-E2E-SN-6', '$M_D', '$WH_TO',   'IN_STOCK'),
               ('$S7', 'W3C-E2E-SN-7', '$M_D', '$WH_TO',   'IN_STOCK')
        ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty,
                                   avg_cost, amount, last_txn_at)
        VALUES (gen_random_uuid(), '$M_D', '$WH_FROM', NULL, 5, 0, 10, 50, now()),
               (gen_random_uuid(), '$M_D', '$WH_TO',   NULL, 2, 0, 20, 40, now())" >/dev/null
FIX_FROM=$(psql_q "SELECT qty::numeric(18,4) FROM biz_inventory
                    WHERE material_id = '$M_D' AND warehouse_id = '$WH_FROM' AND location_id IS NULL")
FIX_TO=$(psql_q "SELECT qty::numeric(18,4) FROM biz_inventory
                  WHERE material_id = '$M_D' AND warehouse_id = '$WH_TO' AND location_id IS NULL")
assert_eq "5.0000" "$FIX_FROM" "夹具：分仓库存 5"
assert_eq "2.0000" "$FIX_TO" "夹具：总仓库存 2"

# ---------- 3. 登录 ----------
echo "==> 3. 登录 admin"
ADMIN_NEW="${W3C_ADMIN_NEW_PASSWORD:-Nereus792379!W3c}"
call POST /system/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_NEW\"}"
if [ "$REQ_CODE" = "200" ]; then
    ok "登录 admin（新口令）"
    TOKEN=$(jqv '.data.access_token')
else
    call POST /system/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}"
    if [ "$REQ_CODE" != "200" ]; then
        bad "admin 登录失败（响应：$REQ_BODY）"
        echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
        exit 1
    fi
    TOKEN=$(jqv '.data.access_token')
    if [ "$(jqv '.data.must_change_password')" = "true" ]; then
        ok "登录 admin（首登，需改密）"
        call PUT /system/auth/password \
            "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$ADMIN_NEW\"}"
        assert_eq "200" "$REQ_CODE" "首登改密"
        TOKEN=""
        call POST /system/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_NEW\"}"
        assert_eq "200" "$REQ_CODE" "用新口令重登"
        TOKEN=$(jqv '.data.access_token')
    else
        ok "登录 admin（原口令）"
    fi
fi
if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
    bad "没拿到 access_token（响应：$REQ_BODY）"
    echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
    exit 1
fi

# ---------- 4. 建单（成品D 5 件，分仓 → 总仓，随行 5 个序列号） ----------
echo "==> 4. 建单"
BODY=$(cat <<JSON
{"from_warehouse_id":"$WH_FROM","to_warehouse_id":"$WH_TO","reason":"W3c 结算 e2e",
 "items":[{"material_id":"$M_D","qty":"5","serial_ids":["$S1","$S2","$S3","$S4","$S5"]}]}
JSON
)
call POST /business/erp/transfer "$BODY"
assert_eq "200" "$REQ_CODE" "建单 200"
TID=$(jqv '.data.id')
TNO=$(jqv '.data.transfer_no')
assert_eq "DRAFT" "$(jqv '.data.status')" "建单后是 DRAFT"

# ---------- 5. 状态链：PENDING → APPROVED → IN_TRANSIT → DONE ----------
echo "==> 5. 状态链 PENDING → APPROVED → IN_TRANSIT → DONE"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"PENDING"}'
assert_eq "200" "$REQ_CODE" "提交审批 PENDING"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"APPROVED"}'
assert_eq "200" "$REQ_CODE" "审批通过 APPROVED（人工推，见脚本头说明）"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"IN_TRANSIT"}'
assert_eq "200" "$REQ_CODE" "发货 IN_TRANSIT"
QTY_AT_SHIP=$(psql_q "SELECT qty::numeric(18,4) FROM biz_inventory
                       WHERE material_id='$M_D' AND warehouse_id='$WH_FROM' AND location_id IS NULL")
assert_eq "5.0000" "$QTY_AT_SHIP" "发货不动库存（结算在 DONE）"
TXN_AT_SHIP=$(psql_q "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_id = '$TID'")
assert_eq "0" "$TXN_AT_SHIP" "发货不写流水"

call PATCH "/business/erp/transfer/$TID/status" '{"status":"DONE"}'
assert_eq "200" "$REQ_CODE" "收货 DONE（触发库存双写 + 成本转移）"
assert_eq "DONE" "$(jqv '.data.status')" "单据终态 DONE"

# ---------- 6. DB 直查：数量 / 金额 / 均价 / 序列号 / 流水 ----------
echo "==> 6. DB 直查验证（数量 / 金额 / 均价 / 序列号 / 流水）"
Q_FROM=$(psql_q "SELECT qty::numeric(18,4) FROM biz_inventory
                  WHERE material_id='$M_D' AND warehouse_id='$WH_FROM' AND location_id IS NULL")
A_FROM=$(psql_q "SELECT amount::numeric(18,4) FROM biz_inventory
                  WHERE material_id='$M_D' AND warehouse_id='$WH_FROM' AND location_id IS NULL")
assert_eq "0.0000" "$Q_FROM" "分仓 qty 5 → 0"
assert_eq "0.0000" "$A_FROM" "分仓 amount 50 → 0"

Q_TO=$(psql_q "SELECT qty::numeric(18,4) FROM biz_inventory
                WHERE material_id='$M_D' AND warehouse_id='$WH_TO' AND location_id IS NULL")
A_TO=$(psql_q "SELECT amount::numeric(18,4) FROM biz_inventory
                WHERE material_id='$M_D' AND warehouse_id='$WH_TO' AND location_id IS NULL")
C_TO=$(psql_q "SELECT avg_cost::numeric(18,6) FROM biz_inventory
                WHERE material_id='$M_D' AND warehouse_id='$WH_TO' AND location_id IS NULL")
assert_eq "7.0000" "$Q_TO" "总仓 qty 2 → 7"
assert_eq "90.0000" "$A_TO" "总仓 amount 40 → 90（原价 10 × 5 转入）"
assert_eq "12.857143" "$C_TO" "总仓 avg_cost = 90 / 7（移动加权）"

SN_TO=$(psql_q "SELECT COUNT(*) FROM biz_material_serial
                 WHERE material_id='$M_D' AND warehouse_id='$WH_TO'
                   AND status='IN_STOCK' AND deleted_at IS NULL")
SN_FROM=$(psql_q "SELECT COUNT(*) FROM biz_material_serial
                   WHERE material_id='$M_D' AND warehouse_id='$WH_FROM'
                     AND status='IN_STOCK' AND deleted_at IS NULL")
assert_eq "7" "$SN_TO" "总仓在库序列号 2 + 5 = 7"
assert_eq "0" "$SN_FROM" "分仓在库序列号 5 → 0"
assert_eq "5" "$(psql_q "SELECT COUNT(*) FROM biz_material_serial
                         WHERE id IN ('$S1','$S2','$S3','$S4','$S5') AND warehouse_id='$WH_TO'")" \
    "本次调拨的 5 个序列号 warehouse_id 全部到总仓"

TXN_TOTAL=$(psql_q "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_type='STOCK_TRANSFER' AND doc_id='$TID'")
assert_eq "2" "$TXN_TOTAL" "流水 2 条（OUT + IN）"
OUT_ROW=$(psql_q "SELECT txn_type || '~' || biz_type || '~' || qty::numeric(18,4) || '~' || amount::numeric(18,4)
                    FROM biz_inventory_txn WHERE doc_id='$TID' AND txn_type='OUT'")
IN_ROW=$(psql_q "SELECT txn_type || '~' || biz_type || '~' || qty::numeric(18,4) || '~' || amount::numeric(18,4)
                   FROM biz_inventory_txn WHERE doc_id='$TID' AND txn_type='IN'")
assert_eq "OUT~TRANSFER_OUT~5.0000~50.0000" "$OUT_ROW" "调出流水 OUT / TRANSFER_OUT / 数量 5 / 金额 50"
assert_eq "IN~TRANSFER_IN~5.0000~50.0000" "$IN_ROW" "调入流水 IN / TRANSFER_IN / 数量 5 / 金额 50"
CONSERVED=$(psql_q "SELECT (COALESCE(SUM(amount) FILTER (WHERE txn_type='OUT'),0)
                          - COALESCE(SUM(amount) FILTER (WHERE txn_type='IN'),0))::numeric(18,4)::text
                      FROM biz_inventory_txn WHERE doc_id='$TID'")
assert_eq "0.0000" "$CONSERVED" "金额守恒（OUT 合计 - IN 合计 = 0）"
DOC_NO=$(psql_q "SELECT doc_no FROM biz_inventory_txn WHERE doc_id='$TID' LIMIT 1")
assert_eq "$TNO" "$DOC_NO" "流水挂的是调拨单号"

ITEM_ROW=$(psql_q "SELECT shipped_qty::numeric(18,4) || '~' || received_qty::numeric(18,4) || '~' ||
                          unit_cost::numeric(18,6) || '~' || amount::numeric(18,4)
                     FROM biz_stock_transfer_item WHERE transfer_id='$TID'")
assert_eq "5.0000~5.0000~10.000000~50.0000" "$ITEM_ROW" "明细回填 shipped/received/原价/金额"
TOTAL_AMT=$(psql_q "SELECT total_amount::numeric(18,4) FROM biz_stock_transfer WHERE id='$TID'")
assert_eq "50.0000" "$TOTAL_AMT" "单据头 total_amount = 明细金额合计"
RECEIVED_AT=$(psql_q "SELECT (received_at IS NOT NULL)::text FROM biz_stock_transfer WHERE id='$TID'")
assert_eq "true" "$RECEIVED_AT" "received_at 已写"

# ---------- 7. 幂等：DONE 之后重复推 → 409，库存与流水都不动 ----------
echo "==> 7. 幂等与回滚"
call PATCH "/business/erp/transfer/$TID/status" '{"status":"DONE"}'
assert_eq "409" "$REQ_CODE" "DONE 之后重复推 DONE → 409"
assert_contains "$REQ_BODY" "非法状态流转" "409 文案点名非法流转"
Q_AGAIN=$(psql_q "SELECT qty::numeric(18,4) FROM biz_inventory
                   WHERE material_id='$M_D' AND warehouse_id='$WH_TO' AND location_id IS NULL")
TXN_AGAIN=$(psql_q "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_id='$TID'")
assert_eq "7.0000" "$Q_AGAIN" "重复推 DONE 不重复扣（总仓仍 7）"
assert_eq "2" "$TXN_AGAIN" "重复推 DONE 不重复写流水（仍 2 条）"

# ---------- 8. 库存不足：分仓已 0，再调 3 件 → 409 且仍 IN_TRANSIT ----------
echo "==> 8. 库存不足回滚"
BODY2=$(cat <<JSON
{"from_warehouse_id":"$WH_FROM","to_warehouse_id":"$WH_TO","reason":"W3c 库存不足 e2e",
 "items":[{"material_id":"$M_D","qty":"3","serial_ids":[]}]}
JSON
)
call POST /business/erp/transfer "$BODY2"
assert_eq "200" "$REQ_CODE" "一物一码物料没选序列号 → 建单放行（W3b 不校验个数）"
TID_SN=$(jqv '.data.id')
for st in PENDING APPROVED IN_TRANSIT; do
    call PATCH "/business/erp/transfer/$TID_SN/status" "{\"status\":\"$st\"}"
done
call PATCH "/business/erp/transfer/$TID_SN/status" '{"status":"DONE"}'
assert_eq "400" "$REQ_CODE" "一物一码物料序列号个数 0 != 数量 3 → 结算 400"
assert_contains "$REQ_BODY" "序列号" "400 文案点名序列号"
assert_eq "IN_TRANSIT" "$(psql_q "SELECT status FROM biz_stock_transfer WHERE id='$TID_SN'")" \
    "序列号不符时单据仍 IN_TRANSIT（校验先于任何库存写入）"
assert_eq "0" "$(psql_q "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_id='$TID_SN'")" \
    "序列号不符时一条流水都不写"

# 用「按数量物料」验「库存不足 → 409」
M_Q="d3ce0000-0000-4000-8000-000000000012"
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type,
                                 safety_stock, costing_method, tax_rate, status, serial_required)
        VALUES ('$M_Q', 'W3C-E2E-Q', 'W3C 按数量物料', '$UNIT', 'RAW', 0, 'MOVING_AVG', 0, 1, false)
        ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty,
                                   avg_cost, amount, last_txn_at)
        VALUES (gen_random_uuid(), '$M_Q', '$WH_FROM', NULL, 1, 0, 3, 3, now())
        ON CONFLICT (material_id, warehouse_id,
                     COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
        DO UPDATE SET qty = 1, amount = 3, avg_cost = 3" >/dev/null
BODY3=$(cat <<JSON
{"from_warehouse_id":"$WH_FROM","to_warehouse_id":"$WH_TO","reason":"W3c 库存不足 e2e",
 "items":[{"material_id":"$M_Q","qty":"9"}]}
JSON
)
call POST /business/erp/transfer "$BODY3"
assert_eq "200" "$REQ_CODE" "按数量物料建单 9 件（分仓只有 1）"
TID2=$(jqv '.data.id')
call PATCH "/business/erp/transfer/$TID2/status" '{"status":"PENDING"}'
call PATCH "/business/erp/transfer/$TID2/status" '{"status":"APPROVED"}'
call PATCH "/business/erp/transfer/$TID2/status" '{"status":"IN_TRANSIT"}'
call PATCH "/business/erp/transfer/$TID2/status" '{"status":"DONE"}'
assert_eq "409" "$REQ_CODE" "库存 1 调 9 → 409"
assert_contains "$REQ_BODY" "库存不足" "409 文案点名库存不足"
STILL=$(psql_q "SELECT status FROM biz_stock_transfer WHERE id='$TID2'")
assert_eq "IN_TRANSIT" "$STILL" "结算失败后单据仍 IN_TRANSIT（整笔回滚）"
Q_STILL=$(psql_q "SELECT qty::numeric(18,4) FROM biz_inventory
                   WHERE material_id='$M_Q' AND warehouse_id='$WH_FROM' AND location_id IS NULL")
assert_eq "1.0000" "$Q_STILL" "库存没被扣"
TXN2=$(psql_q "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_id='$TID2'")
assert_eq "0" "$TXN2" "没有流水"

# ---------- 9. 收尾 ----------
echo "==> 9. 收尾"
if [ -n "$SERVER_PID" ] && [ "${W3C_KEEP_SERVER:-0}" != "1" ]; then
    kill "$SERVER_PID" 2>/dev/null && ok "已停掉本脚本起的隔离实例（pid=$SERVER_PID）"
fi
cat <<TIP

   清理（如需）：PGPASSWORD=… psql -d $DBN -c "
     DELETE FROM biz_inventory_txn  WHERE warehouse_id IN ('$WH_FROM','$WH_TO');
     DELETE FROM biz_stock_transfer WHERE from_warehouse_id IN ('$WH_FROM','$WH_TO');
     DELETE FROM biz_inventory        WHERE warehouse_id IN ('$WH_FROM','$WH_TO');
     DELETE FROM biz_material_serial  WHERE material_id IN ('$M_D','$M_Q');
     DELETE FROM biz_material        WHERE id IN ('$M_D','$M_Q');
     DELETE FROM biz_warehouse       WHERE id IN ('$WH_FROM','$WH_TO');"
TIP

echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
