#!/bin/bash
# ============================================================
# W2a · V0138 验收（云枢 TSK-202610-000678）
#
# 卡面 §三 的 6 项验收，逐条真跑、打印原始 psql / 日志输出：
#   ① 空库：全新空库上走**真实迁移器**（0 → 最新，含 V0138）能成功
#   ② 幂等：V0138 up 连跑两次不报错；重启实例「应用 0 个迁移」
#   ③ down：`V0138__material_serial.down.sql` 完整回滚（表 / 两列 / 5 个索引 / SN 取号规则零残留），
#      回滚后再 up 能重建
#   ④ CHECK：直接 INSERT status='INVALID' → 必须被 DB 拒绝
#   ⑤ 唯一索引：同一 serial_no 插两条 → 第二条失败
#   ⑥ 软删后可重用号：deleted_at 非空后，同号能再插
#   ⑦（附加）硬外键：location_id 指向不存在的库位库存行 → 被拒
#
# 用法：
#   REPO=/opt/cloudpivot bash scripts/verify/w2a_v0138.sh
# 可覆盖变量：
#   W2A_DB（默认 cloudpivot_w2a）· W2A_PORT（默认 18098）· W2A_BIN（默认 release 二进制）
#   W2A_ENV（默认 /etc/cloudpivot/cloudpivot-dev.env）· KEEP_DB=1 保留隔离库以便手工复查
#
# 安全：只动自建隔离库 + 隔离端口 + Redis db9；prod(8023/cloudpivot) 与 dev(8024/cloudpivot_dev)
#       一律拒绝（库名白名单外的库不会用，库名撞车直接退出）。
# ============================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
ENV_FILE=${W2A_ENV:-/etc/cloudpivot/cloudpivot-dev.env}
DB=${W2A_DB:-cloudpivot_w2a}
PORT=${W2A_PORT:-18098}
APP=$REPO/backend
BIN=${W2A_BIN:-$APP/target/release/cloudpivot}
UP=$APP/migrations/business/V0138__material_serial.up.sql
DOWN=$APP/migrations/business/V0138__material_serial.down.sql
RUN1=/tmp/w2a_v0138_run1.log
RUN2=/tmp/w2a_v0138_run2.log
PIDF=/tmp/w2a_v0138.pid
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
for f in "$UP" "$DOWN" "$BIN"; do
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
q_ok()   { # $1=标签 $2=SQL $3=期望输出（与输出的第一行比对）
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
ss -ltn 2>/dev/null | grep -q ":$PORT " && { echo "!! 端口 $PORT 已被占用，换 W2A_PORT"; exit 2; }
su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
    && su - postgres -c "psql -Atc \"drop database $DB\"" >/dev/null
su - postgres -c "createdb -O $NIK_DATABASE_USER $DB"
echo "库已重建（owner=$NIK_DATABASE_USER）：$(q 'select current_database()')"
echo "up   = $UP"
echo "down = $DOWN"
echo "up sha256   = $(sha256sum "$UP" | cut -d' ' -f1)"
echo "down sha256 = $(sha256sum "$DOWN" | cut -d' ' -f1)"
echo "bin md5     = $(md5sum "$BIN" | cut -d' ' -f1)"
echo "迁移前业务表数 = $(q "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r'")"

# ============================================================
hr "① 空库：真实迁移器从 0 跑到最新（含 V0138）"
# ============================================================
if start_instance "$RUN1"; then
    echo "--- 迁移日志 ---"
    grep -nE "迁移" "$RUN1" | head -10
    if grep -q "数据库迁移完成: 应用" "$RUN1"; then
        ok "① 迁移器跑完：$(grep -o '数据库迁移完成: 应用 .*' "$RUN1" | head -1)"
    else
        bad "① 迁移器没跑完（见 $RUN1）"
    fi
    if grep -q "V0138 自检通过" "$RUN1"; then
        ok "① 迁移期自检：$(grep -o 'V0138 自检通过.*' "$RUN1" | head -1)"
    else
        bad "① 日志里没有 V0138 自检通过"
    fi
else
    bad "① 实例启动超时"
    tail -30 "$RUN1"
fi
curl -s -o /dev/null -w "   /health = %{http_code}\n" "http://127.0.0.1:$PORT/health" || true

echo "--- schema_migrations 里 V0138 的记录 ---"
q "select 'kind='||kind||' version='||version||' name='||name||' success='||success||' src='||source
     from schema_migrations where version = '138'"
q_ok "① schema_migrations 记了 version=138" \
    "select count(*) from schema_migrations where version='138' and success" "1"

echo "--- 迁移器记录的 checksum 与磁盘 up 文件 sha256 对比 ---"
CK_DB=$(q "select checksum from schema_migrations where version='138'")
CK_FS=$(sha256sum "$UP" | cut -d' ' -f1)
echo "   db  = $CK_DB"
echo "   file= $CK_FS"
[ "$CK_DB" = "$CK_FS" ] && ok "① checksum 一致（库里记录 = up 文件 sha256）" \
                        || echo "   （注：checksum 口径不同，仅记录，不作为判定项）"

echo "--- 结构断言（表 / 16 列 / 4 索引 / 状态 CHECK / biz_material 两列）---"
q_ok "① biz_material_serial 存在" "select to_regclass('public.biz_material_serial') is not null" "t"
q_ok "① 列数 = 16" \
    "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_material_serial'" "16"
q_ok "① 4 个索引齐" \
    "select count(*) from pg_indexes where schemaname='public' and indexname in ('uk_biz_material_serial_no','idx_biz_material_serial_material','idx_biz_material_serial_wh_loc','idx_biz_material_serial_status')" "4"
q_ok "① 唯一索引是部分索引（WHERE deleted_at IS NULL）" \
    "select count(*) from pg_indexes where indexname='uk_biz_material_serial_no' and indexdef like '%WHERE (deleted_at IS NULL)%'" "1"
q_ok "① 状态 CHECK 在位" \
    "select count(*) from pg_constraint where conname='ck_biz_material_serial_status'" "1"
q_ok "① biz_material.serial_required = boolean NOT NULL" \
    "select data_type||'/'||is_nullable||'/'||column_default from information_schema.columns where table_name='biz_material' and column_name='serial_required'" \
    "boolean/NO/false"
q_ok "① biz_material.external_sn = varchar(128) 可空" \
    "select data_type||'/'||coalesce(character_maximum_length::text,'-')||'/'||is_nullable from information_schema.columns where table_name='biz_material' and column_name='external_sn'" \
    "character varying/128/YES"
q_ok "① SN 取号规则已种（biz_seq seq_type=SN）" \
    "select count(*) from biz_seq where seq_type='SN' and deleted_at is null" "1"
q_ok "① 一物一码物料标记（M-W1A-004 serial_required=TRUE）" \
    "select serial_required from biz_material where material_code='M-W1A-004'" "t"
echo "--- 表结构原文 ---"
q "select column_name||' '||data_type||' null='||is_nullable from information_schema.columns where table_name='biz_material_serial' order by ordinal_position"

stop_instance
echo "--- ① 收尾：实例已停 ---"

# ============================================================
hr "② 幂等：up.sql 连跑两次（CREATE TABLE/INDEX IF NOT EXISTS · ADD COLUMN IF NOT EXISTS · CHECK 走 DO 判重）"
# ============================================================
echo "--- 第二次执行 up.sql ---"
OUT2=$($PSQL -f "$UP" 2>&1); RC2=$?
printf '%s\n' "$OUT2" | head -15
[ $RC2 -eq 0 ] && ok "② up.sql 第 2 次执行 rc=0" || bad "② up.sql 第 2 次执行失败 rc=$RC2"
n_skip=$(printf '%s' "$OUT2" | grep -c "already exists, skipping")
[ "$n_skip" -ge 6 ] && ok "② 幂等分支被触发（$n_skip 处 already exists, skipping）" \
                    || bad "② 幂等分支触发数异常（$n_skip）"

echo "--- 第三次执行 up.sql ---"
OUT3=$($PSQL -f "$UP" 2>&1); RC3=$?
[ $RC3 -eq 0 ] && ok "② up.sql 第 3 次执行 rc=0" || bad "② up.sql 第 3 次执行失败 rc=$RC3"

echo "--- 重启实例：迁移器应「应用 0 个迁移」 ---"
if start_instance "$RUN2"; then
    grep -n "数据库迁移完成" "$RUN2" | head -3
    if grep -q "数据库迁移完成: 应用 0 个迁移" "$RUN2"; then
        ok "② 重启后「应用 0 个迁移」（V0138 已被识别为已应用）"
    else
        bad "② 重启后迁移数不为 0：$(grep -o '数据库迁移完成.*' "$RUN2" | head -1)"
    fi
    grep -q "V0138" "$RUN2" && echo "   （重启日志里出现 V0138 字样：$(grep -m1 -o 'V0138.*' "$RUN2")）"
else
    bad "② 重启超时"
    tail -20 "$RUN2"
fi
q_ok "② 库结构仍在（幂等没破坏）" "select to_regclass('public.biz_material_serial') is not null" "t"
stop_instance

# ============================================================
hr "④⑤⑥⑦ 约束与索引行为（真 psql）"
# ============================================================
MAT=$(q "select id from biz_material where material_code='M-W1A-004' and deleted_at is null")
if [ -z "$MAT" ]; then
    bad "④⑤⑥ 取不到种子物料 M-W1A-004（后续用例无法执行）"
else
    echo "测试用物料 material_id = $MAT（种子 M-W1A-004，serial_required=TRUE）"

    echo "--- ④ 非法 status 必须被 DB 拒绝 ---"
    q_fail "④ INSERT status='INVALID' 被拒" \
        "INSERT INTO biz_material_serial (serial_no, material_id, status) VALUES ('SN-20261030-900001','$MAT','INVALID')" \
        'ck_biz_material_serial_status|violates check constraint'
    q_fail "④（附加）小写 status='in_stock' 同样被拒" \
        "INSERT INTO biz_material_serial (serial_no, material_id, status) VALUES ('SN-20261030-900002','$MAT','in_stock')" \
        'ck_biz_material_serial_status|violates check constraint'

    echo "--- ⑤ 同一 serial_no 插两条：第二条必须失败 ---"
    q_ok "⑤ 第一条 SN-20261030-000001 入库" \
        "INSERT INTO biz_material_serial (serial_no, material_id, status, inbound_at, source_type)
         VALUES ('SN-20261030-000001','$MAT','IN_STOCK', CURRENT_TIMESTAMP, 'PURCHASE') returning serial_no" \
        "SN-20261030-000001"
    q_fail "⑤ 第二条同号被唯一索引拒绝" \
        "INSERT INTO biz_material_serial (serial_no, material_id, status) VALUES ('SN-20261030-000001','$MAT','IN_STOCK')" \
        'uk_biz_material_serial_no|duplicate key value'
    q_ok "⑤ 库里该号只有 1 行（未软删）" \
        "select count(*) from biz_material_serial where serial_no='SN-20261030-000001' and deleted_at is null" "1"

    echo "--- ⑥ 软删后同号可重用 ---"
    q_ok "⑥ 把第一条软删（deleted_at 置值）" \
        "UPDATE biz_material_serial SET deleted_at = CURRENT_TIMESTAMP, status='SCRAPPED', outbound_at = CURRENT_TIMESTAMP
          WHERE serial_no='SN-20261030-000001' AND deleted_at IS NULL returning 'soft-deleted'" \
        "soft-deleted"
    q_ok "⑥ 同一 serial_no 再插一条（新一件货）" \
        "INSERT INTO biz_material_serial (serial_no, material_id, status, inbound_at, source_type)
         VALUES ('SN-20261030-000001','$MAT','IN_STOCK', CURRENT_TIMESTAMP, 'TRANSFER') returning 'reused-ok'" \
        "reused-ok"
    q_ok "⑥ 同号共 2 行（1 软删 + 1 在库）" \
        "select count(*) from biz_material_serial where serial_no='SN-20261030-000001'" "2"
    q_ok "⑥ 其中未软删的只有 1 行（唯一索引口径）" \
        "select count(*) from biz_material_serial where serial_no='SN-20261030-000001' and deleted_at is null" "1"

    echo "--- ⑦（附加）硬外键：location_id 指向不存在的库位库存行 → 被拒 ---"
    q_fail "⑦ location_id 悬空 FK 被拒" \
        "INSERT INTO biz_material_serial (serial_no, material_id, location_id, status)
         VALUES ('SN-20261030-900003','$MAT','00000000-0000-4000-8000-0000000fffff','IN_STOCK')" \
        'foreign key constraint|violates foreign key'
    q_fail "⑦ material_id 悬空 FK 被拒" \
        "INSERT INTO biz_material_serial (serial_no, material_id, status)
         VALUES ('SN-20261030-900004','00000000-0000-4000-8000-0000000ffffe','IN_STOCK')" \
        'foreign key constraint|violates foreign key'

    echo "--- 当前数据快照 ---"
    q "select serial_no||' | '||status||' | deleted='||(deleted_at is not null)::text||' | src='||coalesce(source_type,'-') from biz_material_serial order by created_at"
fi

# ============================================================
hr "③ down：完整回滚（带数据）→ 再 up 重建 → down 幂等"
# ============================================================
echo "--- 回滚前：表里有数据（证明 down 真的能带着数据回滚）---"
q "select 'rows='||count(*) from biz_material_serial"

echo "--- 执行 down.sql ---"
OUTD=$($PSQL -f "$DOWN" 2>&1); RCD=$?
printf '%s\n' "$OUTD" | head -10
[ $RCD -eq 0 ] && ok "③ down.sql rc=0（$(printf '%s' "$OUTD" | grep -o 'V0138 回滚完成.*' | head -1)）" \
               || bad "③ down.sql 失败 rc=$RCD"

echo "--- 回滚后零残留断言 ---"
q_ok "③ 表已删除" "select to_regclass('public.biz_material_serial') is null" "t"
q_ok "③ 两列已删除" \
    "select count(*) from information_schema.columns where table_name='biz_material' and column_name in ('serial_required','external_sn')" "0"
q_ok "③ 5 个索引零残留" \
    "select count(*) from pg_indexes where schemaname='public' and indexname in ('uk_biz_material_serial_no','idx_biz_material_serial_material','idx_biz_material_serial_wh_loc','idx_biz_material_serial_status','idx_biz_material_serial_req')" "0"
q_ok "③ SN 取号规则已删" "select count(*) from biz_seq where seq_type='SN'" "0"

echo "--- 再执行一次 down.sql（幂等）---"
OUTD2=$($PSQL -f "$DOWN" 2>&1); RCD2=$?
printf '%s\n' "$OUTD2" | head -6
[ $RCD2 -eq 0 ] && ok "③ down.sql 第 2 次执行 rc=0（幂等）" || bad "③ down.sql 第 2 次执行失败 rc=$RCD2"

echo "--- up 重建（回滚不是单向的）---"
OUTU=$($PSQL -f "$UP" 2>&1); RCU=$?
printf '%s\n' "$OUTU" | tail -3
[ $RCU -eq 0 ] && ok "③ down 之后 up 能重建" || bad "③ down 之后 up 失败 rc=$RCU"
q_ok "③ 重建后表回来了" "select to_regclass('public.biz_material_serial') is not null" "t"
q_ok "③ 重建后列回来了" \
    "select count(*) from information_schema.columns where table_name='biz_material' and column_name in ('serial_required','external_sn')" "2"

# ============================================================
hr "收尾"
# ============================================================
stop_instance
if [ "$KEEP_DB" = "1" ]; then
    echo "KEEP_DB=1 → 保留库 $DB（记住手工清理：su - postgres -c 'drop database $DB'）"
else
    su - postgres -c "psql -Atc \"drop database $DB\"" >/dev/null 2>&1
    echo "隔离库 $DB 已删除"
fi
echo "日志：$RUN1（首次）/ $RUN2（重启，应为 0 个迁移）"

hr "汇总"
echo "PASS = $PASS   FAIL = $FAIL"
if [ "$FAIL" -gt 0 ]; then
    echo "结论：有 $FAIL 项未通过，V0138 验收不通过"
    exit 1
fi
echo "结论：① 空库 ② 幂等 ③ down ④ CHECK ⑤ 唯一索引 ⑥ 软删重用号 全部通过"
