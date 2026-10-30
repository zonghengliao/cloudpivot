#!/bin/bash
# ============================================================
# W3a · V0139 / V0140 验收（云枢 TSK-202610-000681）
#
# 卡面 §四 的 8 项验收，逐条真跑、打印原始 psql / 日志输出：
#   ① 空库：全新空库上走**真实迁移器**（0 → 最新，含 V0139 + V0140）能成功
#   ② 幂等：两个 up 连跑两次 rc=0；重启实例「应用 0 个迁移」
#   ③ down：V0140.down → V0139.down 完整回滚（三表 / location_id / 三列索引 / 在途仓零残留，
#      两列唯一索引恢复），回滚后再 up 能重建
#   ④ CHECK 拦住非法 status
#   ⑤ CHECK 拦住 from = to（同仓调拨）
#   ⑥ CHECK 拦住 qty <= 0
#   ⑦ 在途仓种子插入成功且唯一
#   ⑧ 库存三列唯一索引生效（同物料同仓同库位不能重复；不同库位可共存；旧两列索引已消失）
#
# 附加（本卡实现细节的证据，不属卡面 8 项）：
#   A. `ON CONFLICT (material_id, warehouse_id)` 的旧推断在三列索引下**必然失败**（42P10），
#      表达式式目标可用 —— 这条证明迁移头注释 ④ 的「连带修复」不是拍脑袋
#   B. 状态机 CHECK（status 与 6 个时间戳一一对应）真的会拦
#   C. 明细 CHECK 链（shipped ≤ qty、received ≤ shipped）会拦
#   D. 单号部分唯一索引：软删后单号可复用
#
# 用法：
#   REPO=/opt/cloudpivot bash scripts/verify/w3a_migrations.sh
#   隔离仓（不碰共享工作区）：
#   REPO=/root/w3a-stage W3A_BIN=/opt/cloudpivot/backend/target/release/cloudpivot \
#       bash scripts/verify/w3a_migrations.sh
# 可覆盖变量：
#   W3A_DB（默认 cloudpivot_w3a）· W3A_PORT（默认 18093）· W3A_BIN（默认 $REPO/backend/target/release/cloudpivot）
#   W3A_ENV（默认 /etc/cloudpivot/cloudpivot-dev.env）· KEEP_DB=1 保留隔离库以便手工复查
#
# 安全：只动自建隔离库 + 隔离端口 + Redis db9；prod(8023/cloudpivot) 与 dev(8024/cloudpivot_dev)
#       一律拒绝（库名撞车直接退出）。
# ============================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
ENV_FILE=${W3A_ENV:-/etc/cloudpivot/cloudpivot-dev.env}
DB=${W3A_DB:-cloudpivot_w3a}
PORT=${W3A_PORT:-18093}
APP=$REPO/backend
BIN=${W3A_BIN:-$APP/target/release/cloudpivot}
UP39=$APP/migrations/business/V0139__erp_inventory_location.up.sql
DN39=$APP/migrations/business/V0139__erp_inventory_location.down.sql
UP40=$APP/migrations/business/V0140__erp_stock_transfer.up.sql
DN40=$APP/migrations/business/V0140__erp_stock_transfer.down.sql
RUN1=/tmp/w3a_run1.log
RUN2=/tmp/w3a_run2.log
PIDF=/tmp/w3a.pid
KEEP_DB=${KEEP_DB:-0}

PASS=0
FAIL=0
ok()  { PASS=$((PASS + 1)); echo "  [PASS] $*"; }
bad() { FAIL=$((FAIL + 1)); echo "  [FAIL] $*"; }

hr() { echo; echo "################ $* ################"; }

# ---------- 0. 前置检查 ----------
case "$DB" in
    cloudpivot|cloudpivot_dev|cloudpivot_test)
        echo "!! 拒绝在 $DB 上执行验收（这是 prod/dev/test 库，本脚本只用自己的隔离库）"
        exit 2
        ;;
esac
for f in "$UP39" "$DN39" "$UP40" "$DN40" "$BIN"; do
    if [ ! -f "$f" ]; then echo "!! 缺少文件：$f"; exit 2; fi
done
if [ ! -f "$ENV_FILE" ]; then echo "!! 缺少环境文件：$ENV_FILE"; exit 2; fi

set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a

export NIK_DATABASE_NAME=$DB
export NIK_SERVER_LISTEN=127.0.0.1:$PORT
export NIK_REDIS_URL=redis://127.0.0.1:6379/9
export RUST_LOG=info
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

PSQL="psql -h 127.0.0.1 -p ${NIK_DATABASE_PORT:-5432} -U $NIK_DATABASE_USER -d $DB -At -v ON_ERROR_STOP=1"

q()      { $PSQL -c "$1" 2>&1; }
q_ok()   { # $1=标签 $2=SQL $3=期望输出（比对输出的第一行）
    local out rc first
    out=$(q "$2"); rc=$?
    first=$(printf '%s\n' "$out" | head -1)
    if [ $rc -eq 0 ] && [ "$first" = "$3" ]; then
        ok "$1 → $first"
    else
        bad "$1（期望 [$3]，实得 rc=$rc [$out]）"
    fi
}
q_fail() { # $1=标签 $2=SQL $3=期望的错误特征(ERE)
    local out rc
    out=$(q "$2"); rc=$?
    if [ $rc -ne 0 ] && printf '%s' "$out" | grep -qE "$3"; then
        ok "$1 → 被拒：$(printf '%s' "$out" | head -1)"
    else
        bad "$1（期望被拒且匹配 /$3/，实得 rc=$rc [$out]）"
    fi
}

stop_instance() {
    if [ -f "$PIDF" ]; then
        kill "$(cat "$PIDF")" 2>/dev/null
        sleep 3
        rm -f "$PIDF"
    fi
}

wait_ready() { # $1=日志文件
    local i
    for i in $(seq 1 180); do
        if grep -qE "数据库迁移完成|服务启动失败|FATAL|panic" "$1" 2>/dev/null; then
            sleep 2
            return 0
        fi
        sleep 1
    done
    return 1
}

start_instance() { # $1=日志文件
    cd "$APP" || return 1
    setsid nohup "$BIN" > "$1" 2>&1 < /dev/null &
    echo $! > "$PIDF"
    wait_ready "$1"
}

trap 'stop_instance' EXIT

hr "0 准备：隔离库 $DB / 端口 $PORT / 二进制 $BIN"
ss -ltn 2>/dev/null | grep -q ":$PORT " && { echo "!! 端口 $PORT 已被占用，换 W3A_PORT"; exit 2; }
su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
    && su - postgres -c "psql -Atc \"drop database $DB\"" >/dev/null
su - postgres -c "createdb -O $NIK_DATABASE_USER $DB"
echo "库已重建（owner=$NIK_DATABASE_USER）：$(q 'select current_database()')"
echo "REPO = $REPO"
echo "up39 = $UP39"
echo "up40 = $UP40"
echo "up39 sha256 = $(sha256sum "$UP39" | cut -d' ' -f1)"
echo "up40 sha256 = $(sha256sum "$UP40" | cut -d' ' -f1)"
echo "bin md5     = $(md5sum "$BIN" | cut -d' ' -f1)"
echo "迁移前业务表数 = $(q "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r'")"

# ============================================================
hr "① 空库：真实迁移器从 0 跑到最新（含 V0139 + V0140）"
# ============================================================
if start_instance "$RUN1"; then
    echo "--- 迁移日志 ---"
    grep -nE "迁移" "$RUN1" | head -10
    if grep -q "数据库迁移完成: 应用" "$RUN1"; then
        ok "① 迁移器跑完：$(grep -o '数据库迁移完成: 应用 .*' "$RUN1" | head -1)"
    else
        bad "① 迁移器没跑完（见 $RUN1）"
    fi
    for tag in "V0139 自检通过" "V0140 自检通过"; do
        if grep -q "$tag" "$RUN1"; then
            ok "① 迁移期自检：$(grep -o "$tag.*" "$RUN1" | head -1)"
        else
            bad "① 日志里没有「$tag」"
        fi
    done
else
    bad "① 实例启动超时"
    tail -30 "$RUN1"
fi
curl -s -o /dev/null -w "   /health = %{http_code}\n" "http://127.0.0.1:$PORT/health" || true

echo "--- schema_migrations 里 V0139 / V0140 的记录 ---"
q "select 'version='||version||' name='||name||' success='||success from schema_migrations where version in ('139','140') order by version::int"
q_ok "① schema_migrations 记了 139 与 140" \
    "select count(*) from schema_migrations where version in ('139','140') and success" "2"
CK39=$(q "select checksum from schema_migrations where version='139'")
CK40=$(q "select checksum from schema_migrations where version='140'")
echo "   db 139 = $CK39 / file = $(sha256sum "$UP39" | cut -d' ' -f1)"
echo "   db 140 = $CK40 / file = $(sha256sum "$UP40" | cut -d' ' -f1)"
[ "$CK39" = "$(sha256sum "$UP39" | cut -d' ' -f1)" ] && ok "① V0139 checksum 一致（库里 = up 文件 sha256）" \
    || echo "   （注：checksum 口径不同，仅记录，不作为判定项）"
[ "$CK40" = "$(sha256sum "$UP40" | cut -d' ' -f1)" ] && ok "① V0140 checksum 一致（库里 = up 文件 sha256）" \
    || echo "   （注：checksum 口径不同，仅记录，不作为判定项）"

echo "--- 结构断言：biz_inventory 库位维度 ---"
q_ok "① biz_inventory.location_id = uuid 可空" \
    "select data_type||'/'||is_nullable from information_schema.columns where table_name='biz_inventory' and column_name='location_id'" \
    "uuid/YES"
q_ok "① 外键指向 biz_inventory_location（不是 biz_location）" \
    "select count(*) from pg_constraint where conname='fk_biz_inv_location_id' and confrelid='public.biz_inventory_location'::regclass" "1"
q_ok "① 旧两列唯一索引 uk_biz_inv_mat_wh 已消失" \
    "select count(*) from pg_indexes where schemaname='public' and indexname='uk_biz_inv_mat_wh'" "0"
q_ok "① 新三列唯一索引在位且 unique" \
    "select count(*) from pg_indexes where schemaname='public' and indexname='uk_biz_inv_mat_wh_loc' and indexdef like 'CREATE UNIQUE INDEX%'" "1"
echo "   $(q "select indexdef from pg_indexes where indexname='uk_biz_inv_mat_wh_loc'")"

echo "--- 结构断言：调拨三表 ---"
q_ok "① 三张调拨表齐" \
    "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r' and c.relname in ('biz_stock_transfer','biz_stock_transfer_item','biz_stock_transfer_item_serial')" "3"
echo "--- biz_stock_transfer 列清单 ---"
q "select column_name||' '||data_type||' null='||is_nullable from information_schema.columns where table_name='biz_stock_transfer' order by ordinal_position"
echo "--- biz_stock_transfer_item 列清单 ---"
q "select column_name||' '||data_type||' null='||is_nullable from information_schema.columns where table_name='biz_stock_transfer_item' order by ordinal_position"
echo "--- biz_stock_transfer_item_serial 列清单 ---"
q "select column_name||' '||data_type||' null='||is_nullable from information_schema.columns where table_name='biz_stock_transfer_item_serial' order by ordinal_position"
echo "--- 约束清单 ---"
q "select conrelid::regclass||' | '||conname||' | '||pg_get_constraintdef(oid) from pg_constraint where conrelid in ('public.biz_stock_transfer'::regclass,'public.biz_stock_transfer_item'::regclass,'public.biz_stock_transfer_item_serial'::regclass) and contype in ('c','u','f') order by conrelid::regclass::text, conname"
echo "--- 索引清单 ---"
q "select indexname||' :: '||indexdef from pg_indexes where tablename in ('biz_stock_transfer','biz_stock_transfer_item','biz_stock_transfer_item_serial') order by tablename, indexname"

stop_instance
echo "--- ① 收尾：实例已停 ---"

# ============================================================
hr "② 幂等：up 连跑两次（IF NOT EXISTS / DO 判重 / 种子 WHERE NOT EXISTS）"
# ============================================================
echo "--- 第 2 次执行 V0139.up + V0140.up ---"
OUT2=$($PSQL -f "$UP39" -f "$UP40" 2>&1); RC2=$?
printf '%s\n' "$OUT2" | grep -vE "^(INSERT|CREATE|ALTER|DO|DROP)" | head -12
[ $RC2 -eq 0 ] && ok "② 第 2 次执行 rc=0" || bad "② 第 2 次执行失败 rc=$RC2（$OUT2）"

echo "--- 第 3 次执行 V0139.up + V0140.up ---"
OUT3=$($PSQL -f "$UP39" -f "$UP40" 2>&1); RC3=$?
[ $RC3 -eq 0 ] && ok "② 第 3 次执行 rc=0" || bad "② 第 3 次执行失败 rc=$RC3（$OUT3）"

echo "--- 幂等副作用核对：在途仓仍 1 行 / 索引仍各 1 个 / 调拨表仍 3 张 ---"
q_ok "② 在途仓仍恰好 1 行（没插重）" \
    "select count(*) from biz_warehouse where warehouse_code='WH-TRANSIT' and deleted_at is null" "1"
q_ok "② 三列唯一索引仍 1 个" \
    "select count(*) from pg_indexes where indexname='uk_biz_inv_mat_wh_loc'" "1"
q_ok "② 调拨表仍 3 张" \
    "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r' and c.relname in ('biz_stock_transfer','biz_stock_transfer_item','biz_stock_transfer_item_serial')" "3"

echo "--- 重启实例：迁移器应「应用 0 个迁移」---"
if start_instance "$RUN2"; then
    grep -n "数据库迁移完成" "$RUN2" | head -3
    if grep -q "数据库迁移完成: 应用 0 个迁移" "$RUN2"; then
        ok "② 重启后「应用 0 个迁移」（V0139/V0140 已被识别为已应用）"
    else
        bad "② 重启后迁移数不为 0：$(grep -o '数据库迁移完成.*' "$RUN2" | head -1)"
    fi
else
    bad "② 重启超时"
    tail -20 "$RUN2"
fi
stop_instance

# ============================================================
hr "④⑤⑥⑧ 约束与索引行为（真 psql）"
# 用种子仓 WH01 / WH02 做调拨两端（V0137 种子，永远存在）
WHA=$(q "select id from biz_warehouse where warehouse_code='WH01' and deleted_at is null")
WHB=$(q "select id from biz_warehouse where warehouse_code='WH02' and deleted_at is null")
MAT=$(q "select id from biz_material where material_code='M-W1A-004' and deleted_at is null")
echo "调出仓 WH01 = $WHA"
echo "调入仓 WH02 = $WHB"
echo "测试物料 M-W1A-004 = $MAT"

if [ -z "$WHA" ] || [ -z "$WHB" ] || [ -z "$MAT" ]; then
    bad "④⑤⑥⑧ 取不到种子仓/物料（后续用例无法执行）"
else
    echo "--- ④ 非法 status 必须被 DB 拒绝 ---"
    q_fail "④ status='INVALID' 被拒" \
        "INSERT INTO biz_stock_transfer (transfer_no, from_warehouse_id, to_warehouse_id, status) VALUES ('__W3A_V_BAD_STATUS__','$WHA','$WHB','INVALID')" \
        'ck_transfer_status|ck_biz_transfer_state'
    q_fail "④（附加）小写 status='draft' 同样被拒" \
        "INSERT INTO biz_stock_transfer (transfer_no, from_warehouse_id, to_warehouse_id, status) VALUES ('__W3A_V_BAD_STATUS2__','$WHA','$WHB','draft')" \
        'ck_transfer_status|ck_biz_transfer_state'

    echo "--- ⑤ 同仓调拨（from = to）必须被拒 ---"
    q_fail "⑤ from = to 被拒" \
        "INSERT INTO biz_stock_transfer (transfer_no, from_warehouse_id, to_warehouse_id) VALUES ('__W3A_V_SAME_WH__','$WHA','$WHA')" \
        'ck_transfer_diff_wh|violates check constraint'

    echo "--- ⑥ 明细 qty <= 0 必须被拒（先建一张合法单做父行）---"
    q_ok "⑥ 先建一张合法 DRAFT 单" \
        "INSERT INTO biz_stock_transfer (id, transfer_no, from_warehouse_id, to_warehouse_id, status, reason) VALUES ('00000000-0000-4000-8000-0000000005c1'::uuid,'__W3A_V_TR_1__','$WHA','$WHB','DRAFT','W3a 验收') returning transfer_no" \
        "__W3A_V_TR_1__"
    q_fail "⑥ qty = 0 被拒" \
        "INSERT INTO biz_stock_transfer_item (transfer_id, material_id, qty) VALUES ('00000000-0000-4000-8000-0000000005c1'::uuid,'$MAT',0)" \
        'ck_transfer_item_qty|violates check constraint'
    q_fail "⑥ qty = -5 被拒" \
        "INSERT INTO biz_stock_transfer_item (transfer_id, material_id, qty) VALUES ('00000000-0000-4000-8000-0000000005c1'::uuid,'$MAT',-5)" \
        'ck_transfer_item_qty|violates check constraint'
    q_ok "⑥ qty = 3 合法" \
        "INSERT INTO biz_stock_transfer_item (id, transfer_id, line_no, material_id, qty, unit_cost, amount) VALUES ('00000000-0000-4000-8000-0000000005c2'::uuid,'00000000-0000-4000-8000-0000000005c1'::uuid,1,'$MAT',3,100,300) returning qty" \
        "3.0000"

    echo "--- 附加 B：状态机 CHECK（status 与时间戳一一对应）---"
    q_fail "B status=DONE 但缺 received_at 被拒" \
        "INSERT INTO biz_stock_transfer (transfer_no, from_warehouse_id, to_warehouse_id, status, submitted_at, approved_at, shipped_at, transit_warehouse_id) VALUES ('__W3A_V_BAD_STATE__','$WHA','$WHB','DONE',now(),now(),now(),'$WHA')" \
        'ck_biz_transfer_state|violates check constraint'
    q_fail "B status=CANCELLED 但缺 cancel_reason 被拒" \
        "INSERT INTO biz_stock_transfer (transfer_no, from_warehouse_id, to_warehouse_id, status, cancelled_at) VALUES ('__W3A_V_BAD_CANCEL__','$WHA','$WHB','CANCELLED',now())" \
        'ck_biz_transfer_state|violates check constraint'
    q_fail "B status=IN_TRANSIT 但缺 transit_warehouse_id 被拒" \
        "INSERT INTO biz_stock_transfer (transfer_no, from_warehouse_id, to_warehouse_id, status, submitted_at, approved_at, shipped_at) VALUES ('__W3A_V_NO_TRANSIT__','$WHA','$WHB','IN_TRANSIT',now(),now(),now())" \
        'ck_biz_transfer_state|violates check constraint'
    q_ok "B 合法 PENDING（有 submitted_at）通过" \
        "INSERT INTO biz_stock_transfer (transfer_no, from_warehouse_id, to_warehouse_id, status, submitted_at) VALUES ('__W3A_V_OK_PENDING__','$WHA','$WHB','PENDING',now()) returning status" \
        "PENDING"

    echo "--- 附加 C：明细 CHECK 链（shipped ≤ qty、received ≤ shipped）---"
    q_fail "C shipped_qty > qty 被拒" \
        "UPDATE biz_stock_transfer_item SET shipped_qty = 4 WHERE id='00000000-0000-4000-8000-0000000005c2'::uuid" \
        'ck_biz_transfer_item_ship|violates check constraint'
    q_ok "C shipped_qty = 3 合法" \
        "UPDATE biz_stock_transfer_item SET shipped_qty = 3 WHERE id='00000000-0000-4000-8000-0000000005c2'::uuid returning shipped_qty" \
        "3.0000"
    q_fail "C received_qty > shipped_qty 被拒" \
        "UPDATE biz_stock_transfer_item SET received_qty = 4 WHERE id='00000000-0000-4000-8000-0000000005c2'::uuid" \
        'ck_biz_transfer_item_recv|violates check constraint'

    echo "--- 附加 A：三列索引下 ON CONFLICT 的推断口径（连带修复的证据）---"
    echo "   A1 旧写法 ON CONFLICT (material_id, warehouse_id) —— 必须失败（42P10）"
    out=$(q "INSERT INTO biz_inventory (material_id, warehouse_id, qty) VALUES ('00000000-0000-4000-8000-0000000005d1'::uuid,'$WHA','1') ON CONFLICT (material_id, warehouse_id) DO UPDATE SET qty = EXCLUDED.qty"); rc=$?
    if [ $rc -ne 0 ] && printf '%s' "$out" | grep -qE 'no unique or exclusion constraint|ON CONFLICT'; then
        ok "A1 旧 upsert 目标已失效（$(printf '%s' "$out" | head -1)）"
    else
        bad "A1 旧 upsert 目标居然还能用（rc=$rc [$out]）—— 说明三列索引没生效？"
    fi
    echo "   A2 新写法（表达式目标，= serial_model.rs::recompute_quantity 修复后的形状）—— 必须成功"
    SQL_UP1="INSERT INTO biz_inventory AS i (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount, last_txn_at) SELECT gen_random_uuid(), '00000000-0000-4000-8000-0000000005d1'::uuid, '$WHA'::uuid, 5, 0, 0, 0, now() ON CONFLICT (material_id, warehouse_id, COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid)) DO UPDATE SET qty = EXCLUDED.qty, amount = EXCLUDED.qty * i.avg_cost, last_txn_at = now(), updated_at = now() RETURNING qty"
    q_ok "A2 第一次 upsert（插入汇总桶）" "$SQL_UP1" "5.0000"
    q_ok "A2 第二次 upsert（命汇总桶，走 DO UPDATE）" "$SQL_UP1" "5.0000"
    q_ok "A2 汇总桶只有 1 行（ON CONFLICT 真命中，没有插重）" \
        "select count(*) from biz_inventory where material_id='00000000-0000-4000-8000-0000000005d1' and warehouse_id='$WHA'" "1"

    echo "--- ⑧ 三列唯一索引生效 ---"
    # （A2 已经占用了 NULL 汇总桶，所以同库位的第二行必须被拦）
    q_fail "⑧ 同物料同仓同库位（NULL）重复插入被拒" \
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty) VALUES (gen_random_uuid(),'00000000-0000-4000-8000-0000000005d1'::uuid,'$WHA',NULL,1)" \
        'uk_biz_inv_mat_wh_loc|duplicate key value'

    LOC1=$(q "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty) VALUES ('00000000-0000-4000-8000-0000000005e1'::uuid,'00000000-0000-4000-8000-0000000005d1'::uuid,'$WHA','00000000-0000-4000-8000-0000000005f1'::uuid,0) ON CONFLICT (id) DO NOTHING returning id")
    LOC2=$(q "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty) VALUES ('00000000-0000-4000-8000-0000000005e2'::uuid,'00000000-0000-4000-8000-0000000005d1'::uuid,'$WHA','00000000-0000-4000-8000-0000000005f2'::uuid,0) ON CONFLICT (id) DO NOTHING returning id")
    q_ok "⑧ 不同库位可以共存（第 1 个库位行）" \
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty) VALUES (gen_random_uuid(),'00000000-0000-4000-8000-0000000005d1'::uuid,'$WHA','00000000-0000-4000-8000-0000000005e1'::uuid,2) returning 'ok1'" \
        "ok1"
    q_ok "⑧ 不同库位可以共存（第 2 个库位行）" \
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty) VALUES (gen_random_uuid(),'00000000-0000-4000-8000-0000000005d1'::uuid,'$WHA','00000000-0000-4000-8000-0000000005e2'::uuid,3) returning 'ok2'" \
        "ok2"
    q_ok "⑧ 同物料同仓现有 3 行（NULL 汇总桶 + 2 个库位）" \
        "select count(*) from biz_inventory where material_id='00000000-0000-4000-8000-0000000005d1' and warehouse_id='$WHA'" "3"
    q_fail "⑧ 同物料同仓同非空库位重复插入被拒" \
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty) VALUES (gen_random_uuid(),'00000000-0000-4000-8000-0000000005d1'::uuid,'$WHA','00000000-0000-4000-8000-0000000005e1'::uuid,9)" \
        'uk_biz_inv_mat_wh_loc|duplicate key value'
    q_fail "⑧ location_id 悬空引用被硬外键拒绝" \
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty) VALUES (gen_random_uuid(),'00000000-0000-4000-8000-0000000005d1'::uuid,'$WHA','00000000-0000-4000-8000-0000000fffff'::uuid,1)" \
        'fk_biz_inv_location_id|violates foreign key'

    echo "--- 附加 D：单号部分唯一索引（软删后可复用）---"
    q_fail "D 同号未软删重复 → 被拒" \
        "INSERT INTO biz_stock_transfer (transfer_no, from_warehouse_id, to_warehouse_id) VALUES ('__W3A_V_TR_1__','$WHA','$WHB')" \
        'uk_stock_transfer_no|duplicate key value'
    q_ok "D 把第 1 张单软删（DRAFT → DRAFT + deleted_at）" \
        "UPDATE biz_stock_transfer SET deleted_at = now() WHERE transfer_no='__W3A_V_TR_1__' returning 'soft-deleted'" \
        "soft-deleted"
    q_ok "D 软删后同号可再用" \
        "INSERT INTO biz_stock_transfer (transfer_no, from_warehouse_id, to_warehouse_id) VALUES ('__W3A_V_TR_1__','$WHA','$WHB') returning transfer_no" \
        "__W3A_V_TR_1__"

    echo "--- 序列号子表唯一（同一明细同一件货录两次 → 被拒）---"
    q_ok "先备一件序列号" \
        "INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status, inbound_at) VALUES ('00000000-0000-4000-8000-0000000005f9'::uuid,'__W3A_V_SN_0001__','$MAT','$WHA','IN_STOCK',now()) ON CONFLICT (id) DO NOTHING returning 'sn-ready'" \
        "sn-ready"
    q_ok "序列号挂到明细" \
        "INSERT INTO biz_stock_transfer_item_serial (item_id, serial_id) VALUES ('00000000-0000-4000-8000-0000000005c2'::uuid,'00000000-0000-4000-8000-0000000005f9'::uuid) returning 'linked'" \
        "linked"
    q_fail "同一明细重复录同一序列号被拒" \
        "INSERT INTO biz_stock_transfer_item_serial (item_id, serial_id) VALUES ('00000000-0000-4000-8000-0000000005c2'::uuid,'00000000-0000-4000-8000-0000000005f9'::uuid)" \
        'uk_transfer_item_serial|duplicate key value'
fi

# ============================================================
hr "⑦ 在途仓种子（唯一 + 类型 + 层级 + 路径）"
# ============================================================
q "select id||' | '||warehouse_code||' | '||warehouse_name||' | '||warehouse_type||' | level='||level||' | is_leaf='||is_leaf||' | '||warehouse_path from biz_warehouse where warehouse_code='WH-TRANSIT'"
q_ok "⑦ 在途仓恰好 1 行" \
    "select count(*) from biz_warehouse where warehouse_code='WH-TRANSIT' and deleted_at is null" "1"
q_ok "⑦ 类型 = IN_TRANSIT / 根节点 / 叶子 / 路径自洽" \
    "select count(*) from biz_warehouse where warehouse_code='WH-TRANSIT' and warehouse_type='IN_TRANSIT' and parent_id is null and level=1 and is_leaf=1 and warehouse_path='/'||id::text||'/' and id='d1390001-0000-4000-8000-000000000001'::uuid" "1"
q_fail "⑦ 再插一个同编码在途仓 → 被 uk_biz_warehouse_code 拒绝" \
    "INSERT INTO biz_warehouse (warehouse_code, warehouse_name, warehouse_type, level, warehouse_path) VALUES ('WH-TRANSIT','重复在途仓','IN_TRANSIT',1,'/dup/')" \
    'uk_biz_warehouse_code|duplicate key value'
q_fail "⑦ 非法 warehouse_type 仍被 CHECK 拒绝（枚举没被放宽）" \
    "INSERT INTO biz_warehouse (warehouse_code, warehouse_name, warehouse_type, level, warehouse_path) VALUES ('__W3A_BAD_TYPE__','坏类型','TRANSIT',1,'/x/')" \
    'ck_biz_warehouse_type|violates check constraint'

# ============================================================
hr "③ down：完整回滚（带数据）→ 零残留 → 再 up 重建"
# ============================================================
echo "--- 回滚前数据快照 ---"
q "select 'biz_stock_transfer='||count(*) from biz_stock_transfer"
q "select 'biz_stock_transfer_item='||count(*) from biz_stock_transfer_item"
q "select 'biz_stock_transfer_item_serial='||count(*) from biz_stock_transfer_item_serial"
q "select 'biz_inventory(测试物料)='||count(*) from biz_inventory where material_id='00000000-0000-4000-8000-0000000005d1'"

echo "--- 先证明安全阀：有多库位库存行时 V0139.down 必须**中止**（不静默丢库位数据）---"
OUTD39A=$($PSQL -f "$DN39" 2>&1); RCD39A=$?
if [ $RCD39A -ne 0 ] && printf '%s' "$OUTD39A" | grep -q '回滚中止'; then
    ok "③ 安全阀生效：$(printf '%s' "$OUTD39A" | grep -o 'V0139 回滚中止.*' | head -1)"
else
    bad "③ 安全阀没生效（rc=$RCD39A，${OUTD39A}）"
fi
q_ok "③ 中止后 location_id 仍在（down 是原子的，没留半截）" \
    "select count(*) from information_schema.columns where table_name='biz_inventory' and column_name='location_id'" "1"

echo "--- 清掉本脚本自己的 ⑧ 夹具（验收数据不留给回滚测试）---"
q "delete from biz_inventory where material_id='00000000-0000-4000-8000-0000000005d1'"
q "delete from biz_inventory_location where material_id='00000000-0000-4000-8000-0000000005d1'"
q "delete from biz_stock_transfer_item_serial where serial_id='00000000-0000-4000-8000-0000000005f9'"
q "delete from biz_stock_transfer_item where transfer_id in (select id from biz_stock_transfer where transfer_no like '__W3A_V_%')"
q "delete from biz_stock_transfer where transfer_no like '__W3A_V_%'"
q "delete from biz_material_serial where serial_no='__W3A_V_SN_0001__'"
q_ok "③ 夹具已清空（biz_inventory 无异物料多行）" \
    "select count(*) from (select 1 from biz_inventory group by material_id, warehouse_id having count(*) > 1) d" "0"

echo "--- 执行 V0140.down（卡面顺序：serial → item → transfer）---"
OUTD40=$($PSQL -f "$DN40" 2>&1); RCD40=$?
printf '%s\n' "$OUTD40" | grep -E "NOTICE|ERROR" | head -8
[ $RCD40 -eq 0 ] && ok "③ V0140.down rc=0（$(printf '%s' "$OUTD40" | grep -o 'V0140 回滚完成.*' | head -1)）" \
    || bad "③ V0140.down 失败 rc=$RCD40"

echo "--- 再执行 V0139.down ---"
OUTD39=$($PSQL -f "$DN39" 2>&1); RCD39=$?
printf '%s\n' "$OUTD39" | grep -E "NOTICE|ERROR" | head -8
[ $RCD39 -eq 0 ] && ok "③ V0139.down rc=0（$(printf '%s' "$OUTD39" | grep -o 'V0139 回滚完成.*' | head -1)）" \
    || bad "③ V0139.down 失败 rc=$RCD39"

echo "--- 回滚后零残留断言 ---"
q_ok "③ 三张调拨表已删" \
    "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r' and c.relname in ('biz_stock_transfer','biz_stock_transfer_item','biz_stock_transfer_item_serial')" "0"
q_ok "③ biz_inventory.location_id 已删" \
    "select count(*) from information_schema.columns where table_name='biz_inventory' and column_name='location_id'" "0"
q_ok "③ 外键 fk_biz_inv_location_id 已删" \
    "select count(*) from pg_constraint where conname='fk_biz_inv_location_id'" "0"
q_ok "③ 三列索引 + 库位反查索引已删" \
    "select count(*) from pg_indexes where schemaname='public' and indexname in ('uk_biz_inv_mat_wh_loc','idx_biz_inv_location')" "0"
q_ok "③ 两列唯一索引 uk_biz_inv_mat_wh 已恢复" \
    "select count(*) from pg_indexes where schemaname='public' and indexname='uk_biz_inv_mat_wh' and indexdef like '%(material_id, warehouse_id)%'" "1"
q_ok "③ 在途仓已删" \
    "select count(*) from biz_warehouse where id='d1390001-0000-4000-8000-000000000001'::uuid" "0"
q_ok "③ V0138 的序列号表未受影响" "select to_regclass('public.biz_material_serial') is not null" "t"

echo "--- down 幂等：再执行一次两个 down ---"
OUTD2=$($PSQL -f "$DN40" -f "$DN39" 2>&1); RCD2=$?
printf '%s\n' "$OUTD2" | grep -E "NOTICE|ERROR" | head -6
[ $RCD2 -eq 0 ] && ok "③ down 第 2 次执行 rc=0（幂等）" || bad "③ down 第 2 次执行失败 rc=$RCD2"

echo "--- up 重建（回滚不是单向的）---"
OUTU=$($PSQL -f "$UP39" -f "$UP40" 2>&1); RCU=$?
printf '%s\n' "$OUTU" | grep -E "NOTICE|ERROR" | tail -6
[ $RCU -eq 0 ] && ok "③ down 之后 up 能重建" || bad "③ down 之后 up 失败 rc=$RCU"
q_ok "③ 重建后三表回来" \
    "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r' and c.relname in ('biz_stock_transfer','biz_stock_transfer_item','biz_stock_transfer_item_serial')" "3"
q_ok "③ 重建后 location_id + 三列索引回来" \
    "select count(*) from information_schema.columns where table_name='biz_inventory' and column_name='location_id'" "1"
q_ok "③ 重建后在途仓回来（且仍 1 行）" \
    "select count(*) from biz_warehouse where warehouse_code='WH-TRANSIT'" "1"

echo "--- 重启实例：迁移器应「应用 0 个迁移」，且 schema 自洽 ---"
if start_instance /tmp/w3a_run3.log; then
    grep -o '数据库迁移完成.*' /tmp/w3a_run3.log | head -1
    grep -q "数据库迁移完成: 应用 0 个迁移" /tmp/w3a_run3.log \
        && ok "③ 回滚+重建后重启：应用 0 个迁移" \
        || bad "③ 回滚+重建后重启迁移数不为 0"
else
    bad "③ 回滚+重建后重启超时"
    tail -20 /tmp/w3a_run3.log
fi
stop_instance

# ============================================================
hr "收尾"
# ============================================================
stop_instance
ss -ltn 2>/dev/null | grep -q ":$PORT " && echo "   ⚠ 端口 $PORT 仍被占用" || echo "   端口 $PORT 已释放"
if [ "$KEEP_DB" = "1" ]; then
    echo "   KEEP_DB=1 → 保留隔离库 $DB（手工复查后请 drop database $DB）"
else
    su - postgres -c "psql -Atc \"drop database $DB\"" >/dev/null && echo "   隔离库 $DB 已删除"
fi

echo
echo "==================== 汇总：PASS=$PASS  FAIL=$FAIL ===================="
if [ "$FAIL" -ne 0 ]; then
    echo "!! 有 $FAIL 项失败"
    exit 1
fi
echo "全部通过"
