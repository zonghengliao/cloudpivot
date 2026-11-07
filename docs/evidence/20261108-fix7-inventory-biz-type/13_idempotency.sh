#!/usr/bin/env bash
# ============================================================================
# FIX-7 145 取证 · 步骤 3：幂等性 + 空库首启
#
# 三件事：
#   ① 同库再启一次 → 迁移器「应用 0 个迁移」（迁移器指纹不变）；
#   ② 把 V0330 up SQL 直接再跑两遍 → 两条数据 UPDATE 都是 `UPDATE 0`（幂等），
#      约束 drop/add 可重入；down 再 up 一把（回滚可用，值清回原样后再次迁上来）；
#   ③ 空库首启 → 应用全量迁移（含 V0330），证明新环境也能建出这套约束。
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix7-inventory-biz-type/13_idempotency.sh'
# ============================================================================
set -uo pipefail
EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$EV/00_env.sh"

: > "$EV/raw/idempotency.log"
exec > >(tee -a "$EV/raw/idempotency.log") 2>&1

echo "=== FIX-7 幂等 / 空库首启 $(date '+%F %T') db=$DBNAME ==="

stop_instance() {
  [ -f /root/backup/.fix7_pid ] || return 0
  kill "$(cat /root/backup/.fix7_pid)" 2>/dev/null || true
  sleep 2
  kill -9 "$(cat /root/backup/.fix7_pid)" 2>/dev/null || true
  rm -f /root/backup/.fix7_pid
}

# 端口级兜底：按进程名全清（这些进程只可能来自本卡的隔离实例）。
# 不做的话，上一次 run 留在 8049 的实例会占着端口，新一轮的健康检查读到**旧进程**（假 200），
# 空库其实没起起来（实测：FATAL 服务启动失败: Address already in use + 旧进程继续应答 health）。
kill_all() { pkill -f 'target/debug/cloudpivot-fix7' 2>/dev/null || true; sleep 2; }

start_instance() { # $1=port $2=db $3=log
  ( cd /opt/cloudpivot/backend && \
    NIK_DATABASE_NAME="$2" NIK_SERVER_LISTEN="127.0.0.1:$1" \
    setsid nohup ./target/debug/cloudpivot-fix7 > "$3" 2>&1 < /dev/null &
    echo $! > /root/backup/.fix7_pid )
  if grep -q '服务启动失败' "$3" 2>/dev/null; then echo "启动失败：$3"; tail -5 "$3"; return 1; fi
  for _ in $(seq 1 120); do
    code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$1/health" || true)
    if [ "$code" = "200" ]; then
      if grep -q '服务启动失败' "$3" 2>/dev/null; then echo "启动失败：$3"; tail -5 "$3"; return 1; fi
      echo "health=200 ($2 @ $1)"; return 0
    fi
    if grep -q '服务启动失败' "$3" 2>/dev/null; then echo "启动失败：$3"; tail -5 "$3"; return 1; fi
    sleep 1
  done
  echo "health 未就绪：$3"; tail -20 "$3"; return 1
}

kill_all
echo "--- ① 同库再启一次：迁移器应「应用 0 个迁移」---"
FP_BEFORE=$(sq "select count(*)||'|'||md5(string_agg(version||name, ',' order by version)) from schema_migrations")
stop_instance
start_instance 8048 "$DBNAME" /root/backup/fix7_instance_restart.log
FP_AFTER=$(sq "select count(*)||'|'||md5(string_agg(version||name, ',' order by version)) from schema_migrations")
check_eq "$FP_BEFORE" "$FP_AFTER" "schema_migrations 指纹前后不变（$FP_AFTER）"
grep -E "数据库迁移完成" /root/backup/fix7_instance_restart.log | tail -2 | sed 's/^/    /'
check_eq "0" "$(grep -cE '应用 [1-9][0-9]* 个迁移' /root/backup/fix7_instance_restart.log)" "本次启动没有新应用迁移"

echo "--- ② V0330 up 直接再跑两遍（幂等）---"
SQL="$EV/../../../backend/migrations/business/V0330__inventory_biz_type_and_sourceless_in_type.up.sql"
for i in 1 2; do
  OUT=$(psqlc -v ON_ERROR_STOP=1 -f "$SQL" 2>&1)
  RC=$?
  echo "  第 $i 遍 exit=$RC"
  echo "$OUT" | grep -E "^UPDATE [0-9]+|ERROR" | sed 's/^/    /'
  check_eq "0" "$RC" "V0330 up 第 $i 遍执行成功"
done
check_eq "0" "$(sq "select count(*) from biz_inventory_txn t join biz_stock_out s on s.id=t.doc_id
  where t.doc_type='STOCK_OUT' and s.source_type='SALES' and t.biz_type='OTHER_OUT'")" "重跑后仍无 OTHER_OUT 的销售流水"
check_eq "0" "$(sq "select count(*) from biz_stock_in where is_sourceless and in_type='PURCHASE'")" "重跑后仍无「无源 + PURCHASE」"

echo "--- ②b down → up 往返 ---"
psqlc -v ON_ERROR_STOP=1 -f "$EV/../../../backend/migrations/business/V0330__inventory_biz_type_and_sourceless_in_type.down.sql" > /dev/null 2>&1
check_eq "0" "$(sq "select count(*) from biz_stock_in where in_type='SOURCELESS'")" "down 之后 SOURCELESS 归零"
check_eq "$(sed -n 's/^sales_txn_other_out|//p' "$EV/raw/pre_migration_state.txt" | tr -d '[:space:]')" \
  "$(sq "select count(*) from biz_inventory_txn t join biz_stock_out s on s.id=t.doc_id
    where t.doc_type='STOCK_OUT' and s.source_type='SALES' and t.biz_type='OTHER_OUT'")" "down 之后销售流水回到 OTHER_OUT"
psqlc -v ON_ERROR_STOP=1 -f "$EV/../../../backend/migrations/business/V0330__inventory_biz_type_and_sourceless_in_type.up.sql" > /dev/null 2>&1
check_eq "0" "$(sq "select count(*) from biz_stock_in where is_sourceless and in_type='PURCHASE'")" "再 up 之后无源单重新变成 SOURCELESS"

kill_all
echo "--- ③ 空库首启（新环境路径）---"
FRESH="cloudpivot_fix7_fresh_$(date +%H%M%S)"
psql -h 127.0.0.1 -U cloudpivot -d postgres -c "CREATE DATABASE $FRESH OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $FRESH OWNER cloudpivot\""
echo "$FRESH" > /root/backup/.fix7_fresh_db
stop_instance
start_instance 8049 "$FRESH" /root/backup/fix7_fresh.log
grep -E "数据库迁移完成" /root/backup/fix7_fresh.log | tail -2 | sed 's/^/    /'
ADMIN2=$(login admin "$ADMIN_PASS")
check_eq "0" "$( [ -n "$ADMIN2" ] && echo 0 || echo 1 )" "空库首启后 admin 能登录（迁移链建出的种子可用）"
FRESH_SQ() { psql -h 127.0.0.1 -U cloudpivot -d "$FRESH" -X -tAc "$1" | tr -d '[:space:]'; }
check_eq "1" "$(FRESH_SQ "select count(*) from schema_migrations where name='inventory_biz_type_and_sourceless_in_type'")" "空库含 V0330"
check_eq "1" "$(FRESH_SQ "select count(*) from pg_constraint where conname='ck_biz_si_type' and pg_get_constraintdef(oid) like '%SOURCELESS%'")" "空库 ck_biz_si_type 含 SOURCELESS"

# 收尾：空库实例停掉、库删掉；拷贝库实例重新起回 8048（后续收尾步骤用）
kill_all
psql -h 127.0.0.1 -U cloudpivot -d postgres -c "DROP DATABASE IF EXISTS $FRESH" 2>/dev/null || true
start_instance 8048 "$DBNAME" /root/backup/fix7_instance.log

summary
