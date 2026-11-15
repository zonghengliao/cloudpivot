#!/usr/bin/env bash
# A4 验收 2：两个**独立进程**在同一绝对时刻认领同一条导出任务，只有一个拿到。
#
# 前置：18081 实例已停止（否则它每 5 秒的 cron 会抢先把任务领走，抢不到就没意义）。
#
# 用法：bash verify_a4_claim.sh
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
export CARGO_TARGET_DIR=/root/a4-target
mkdir -p /tmp/a4
cd /root/a4-build/backend

set -a
. /etc/cloudpivot/cloudpivot-test.env
set +a
export A4_DB_URL="postgres://${NIK_DATABASE_USER}:${NIK_DATABASE_PASSWORD}@${NIK_DATABASE_HOST}:${NIK_DATABASE_PORT}/${NIK_DATABASE_NAME}"
echo "A4_DB_URL 前缀: ${A4_DB_URL%%:*}://${NIK_DATABASE_USER}@${NIK_DATABASE_HOST}:${NIK_DATABASE_PORT}/${NIK_DATABASE_NAME}"

echo "===== 1. 编译 worker 集成测试二进制"
cargo test --test impexp_worker --no-run --jobs 2 2>&1 | tail -3
BIN=$(ls -t /root/a4-target/debug/deps/impexp_worker-* 2>/dev/null | grep -v '\.d$' | head -1)
echo "测试二进制: $BIN"

q() { su - postgres -c "psql -d cloudpivot_test -Atc \"$1\""; }

echo
echo "===== 2. 清场：只留一条 PENDING 导出任务"
q "DELETE FROM sys_export_task WHERE task_no LIKE 'EXP-A4CLAIM%'"
LEFT=$(q "SELECT count(*) FROM sys_export_task WHERE status='PENDING' AND deleted_at IS NULL")
echo "本次清理前库里 PENDING 任务数: $LEFT"
q "UPDATE sys_export_task SET status='CANCELED', updated_at=CURRENT_TIMESTAMP WHERE status='PENDING'"
TASK_ID="11111111-2222-3333-4444-555555555555"
q "INSERT INTO sys_export_task (id, task_no, biz_type, biz_name, export_name, format, query_params, status, progress, max_attempts) VALUES ('$TASK_ID', 'EXP-A4CLAIM-0001', 'system.dict_item', '字典项', '并发认领验收', 'XLSX', jsonb_build_object('keyword', 'a4_bench'), 'PENDING', 0, 3)"
echo "--- 认领前 DB 状态:"
q "SELECT task_no || ' status=' || status || ' locked_by=' || COALESCE(locked_by,'NULL') || ' attempt=' || attempt_count FROM sys_export_task WHERE id='$TASK_ID'"

echo
echo "===== 3. 两个进程同时认领（墙钟屏障 +4 秒）"
CLAIM_AT=$(python3 -c 'import time; print(int(time.time()*1000)+4000)')
echo "屏障时刻(Unix ms): $CLAIM_AT"
A4_TASK_ID="$TASK_ID" A4_CLAIM_AT="$CLAIM_AT" A4_WORKER_ID="worker-A-pid$$" \
  "$BIN" claim_is_exclusive_across_processes --nocapture --test-threads=1 > /tmp/a4/claim-a.log 2>&1 &
PID_A=$!
A4_TASK_ID="$TASK_ID" A4_CLAIM_AT="$CLAIM_AT" A4_WORKER_ID="worker-B-pid$$-b" \
  "$BIN" claim_is_exclusive_across_processes --nocapture --test-threads=1 > /tmp/a4/claim-b.log 2>&1 &
PID_B=$!
wait $PID_A
wait $PID_B

echo "--- 进程 A 日志:"
grep -E "CLAIMED|NO_TASK|DB status|panicked|assertion" /tmp/a4/claim-a.log | sed 's/^/  /'
echo "--- 进程 B 日志:"
grep -E "CLAIMED|NO_TASK|DB status|panicked|assertion" /tmp/a4/claim-b.log | sed 's/^/  /'

CLAIMS=$(cat /tmp/a4/claim-a.log /tmp/a4/claim-b.log | grep -c "CLAIMED task_id")
NOS=$(cat /tmp/a4/claim-a.log /tmp/a4/claim-b.log | grep -c "NO_TASK")
echo
echo "认领成功进程数=$CLAIMS   被抢先（NO_TASK）进程数=$NOS"

echo "--- 认领后 DB 状态:"
q "SELECT task_no || ' status=' || status || ' locked_by=' || COALESCE(locked_by,'NULL') || ' attempt=' || attempt_count || ' progress=' || progress FROM sys_export_task WHERE id='$TASK_ID'"

OK=0
NG=0
ok() { OK=$((OK + 1)); echo "PASS $1"; }
bad() { NG=$((NG + 1)); echo "FAIL $1"; }
[ "$CLAIMS" = "1" ] && ok "同一条任务只被一个进程认领（CLAIMED=1）" || bad "认领次数 $CLAIMS（应为 1）"
[ "$NOS" = "1" ] && ok "另一个进程被 SKIP LOCKED 跳过（NO_TASK=1）" || bad "NO_TASK=$NOS（应为 1）"
grep -q "status=RUNNING" /tmp/a4/claim-a.log /tmp/a4/claim-b.log && ok "DB 状态为 RUNNING" || bad "DB 状态异常"
LOCKED=$(q "SELECT COALESCE(locked_by,'NULL') FROM sys_export_task WHERE id='$TASK_ID'")
echo "locked_by=$LOCKED"
[ "$LOCKED" != "NULL" ] && ok "locked_by 记录了赢家（$LOCKED）" || bad "locked_by 为空"
DUP=$(q "SELECT count(*) FROM sys_export_task WHERE id='$TASK_ID' AND attempt_count=0")
[ "$DUP" = "1" ] && ok "attempt_count=0（认领不算一次尝试）" || bad "attempt_count 异常"

echo
echo "===== 4. 清理"
q "DELETE FROM sys_export_task WHERE task_no LIKE 'EXP-A4CLAIM%'"
echo "PASS=$OK FAIL=$NG"
[ "$NG" -eq 0 ] || exit 1
