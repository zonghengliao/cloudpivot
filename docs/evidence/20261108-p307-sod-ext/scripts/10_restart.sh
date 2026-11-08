#!/usr/bin/env bash
# ============================================================================
# P3-07 证据脚本 · 迁移幂等（重启「应用 0 个迁移」）
#
# 用法：先跑过 20_e2e_sod.sh（库 cloudpivot_p307_ut 已装到 V0340），再跑本脚本。
# 目的：证明 V0340 只应用一次；重启不重复执行、不改数据。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

DB=${DB:-cloudpivot_p307_ut}
ENVF=${ENVF:-/root/p307/instance.env}
BIN=${BIN:-/root/p307/bin/cloudpivot}
ILOG=/root/p307/restart_instance.log
IPID=/root/p307/e2e.pid
OUT="$RAW/10_restart.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-07 迁移幂等：重启（$DB）================"
CFG_BEFORE=$(q "$DB" "select count(*) from sys_config where config_key like 'erp.separation_of_duties.%' and deleted_at is null")
say "重启前 erp.separation_of_duties.* = $CFG_BEFORE 条"

pid=$(cat "$IPID" 2>/dev/null) && kill "$pid" 2>/dev/null
for _ in $(seq 1 30); do kill -0 "${pid:-0}" 2>/dev/null || break; sleep 1; done
sleep 2

( cd /opt/cloudpivot/backend && setsid env $(grep -v '^#' "$ENVF" | grep -v '^$' | tr '\n' ' ') \
    NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:18311" \
    "$BIN" >"$ILOG" 2>&1 </dev/null & )
for i in $(seq 1 120); do
  newpid=$(ss -ltnp 2>/dev/null | sed -n "s/.*127\.0\.0\.1:18311 .*pid=\([0-9]*\).*/\1/p" | head -1)
  [ -n "$newpid" ] && { echo "$newpid" >"$IPID"; break; }
  sleep 1
done
for i in $(seq 1 120); do curl -s -o /dev/null "http://127.0.0.1:18311/health" && break; sleep 1; done

say "重启日志里的迁移行："
grep -E "迁移|migrat" "$ILOG" | tail -6
grep -q "应用 0 个迁移" "$ILOG" && ok "重启「应用 0 个迁移」（V0340 幂等）" \
  || bad "重启仍应用了迁移：$(grep -oE '应用 [0-9]+ 个迁移' "$ILOG" | tail -1)"

CFG_AFTER=$(q "$DB" "select count(*) from sys_config where config_key like 'erp.separation_of_duties.%' and deleted_at is null")
eq "重启后条数不变" "$CFG_AFTER" "$CFG_BEFORE"
eq "重启后三条扩展开关仍是 false" \
  "$(q "$DB" "select count(*) from sys_config where config_key in (
        'erp.separation_of_duties.approve_execute',
        'erp.separation_of_duties.stocktake',
        'erp.separation_of_duties.purchase_accept') and config_value='false'")" "3"

say ""
say "--- down.sql 可回滚（在同一个库上跑一遍，再跑 up.sql 复原）---"
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0340__erp_sod_ext_switches.down.sql >/dev/null 2>&1 \
  && ok "down.sql 执行成功" || bad "down.sql 失败"
eq "down 后三条已删除" \
  "$(q "$DB" "select count(*) from sys_config where config_key in (
        'erp.separation_of_duties.approve_execute',
        'erp.separation_of_duties.stocktake',
        'erp.separation_of_duties.purchase_accept')")" "0"
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0340__erp_sod_ext_switches.up.sql >/dev/null 2>&1 \
  && ok "up.sql 重放成功（ON CONFLICT 幂等）" || bad "up.sql 重放失败"
eq "重放 up 后三条回来且仍是 false" \
  "$(q "$DB" "select count(*) from sys_config where config_key in (
        'erp.separation_of_duties.approve_execute',
        'erp.separation_of_duties.stocktake',
        'erp.separation_of_duties.purchase_accept') and config_value='false'")" "3"

summary
