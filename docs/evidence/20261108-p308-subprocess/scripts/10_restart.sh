#!/usr/bin/env bash
# ============================================================================
# P3-08 证据脚本 · 迁移（V0341）升级 / 幂等 / 回滚
#
# 用法：
#   1) 先准备一个「停在 V0340」的库（本卡实测用 `createdb -T cloudpivot_p307_ut
#      cloudpivot_p308_upg`，P3-07 的库正好到 V0340）；
#   2) `DB=cloudpivot_p308_upg PGPASSWORD=alun bash 10_restart.sh`
#
# 断言四件事：
#   ① 升级库首启只应用 **1 个**迁移（V0341），且结构自检通过；
#   ② 重启「应用 0 个迁移」；
#   ③ down.sql 可回滚（父子列 / 部分索引 / 子流程约束全部清除）；
#   ④ up.sql 重放复原（幂等）。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

DB=${DB:-cloudpivot_p308_upg}
PORT=${PORT:-18313}
ENVF=${ENVF:-/root/p307/instance.env}
BIN=${BIN:-/root/p308/bin/cloudpivot}
RUN_DIR=/opt/cloudpivot/backend
ILOG=/root/p308/upgrade_instance.log
IPID=/root/p308/upgrade.pid
OUT="$RAW/10_restart.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

port_pid() {
  ss -ltnp 2>/dev/null | sed -n "s/.*127\.0\.0\.1:$PORT .*pid=\([0-9]*\).*/\1/p" | head -1
}

stop_instance() {
  local pid
  pid=$(cat "$IPID" 2>/dev/null) || pid=$(port_pid)
  [ -n "${pid:-}" ] || return 0
  kill "$pid" 2>/dev/null || return 0
  for _ in $(seq 1 30); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
}

start_instance() {
  local i pid
  ( cd "$RUN_DIR" && setsid env $(grep -v '^#' "$ENVF" | grep -v '^$' | tr '\n' ' ') \
      NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
      "$BIN" >"$ILOG" 2>&1 </dev/null & )
  for i in $(seq 1 120); do
    pid=$(port_pid)
    if [ -n "$pid" ]; then echo "$pid" >"$IPID"; return 0; fi
    sleep 1
  done
  return 1
}

wait_http() {
  local i=0
  while [ "$i" -lt 180 ]; do
    curl -s -o /dev/null "http://127.0.0.1:$PORT/health" && return 0
    sleep 1
    i=$((i + 1))
  done
  return 1
}

say "================ P3-08 迁移 V0341：升级 / 幂等 / 回滚（$DB）================"

# ---------- ① 升级库已经应用过 V0341（1 条） ----------
eq "升级库最新版本 = 341" \
  "$(q "$DB" "select version from schema_migrations where kind='versioned' order by applied_seq desc limit 1")" "341"
eq "V0341 只记录了一次" \
  "$(q "$DB" "select count(*) from schema_migrations where kind='versioned' and version='341'")" "1"
eq "node_type CHECK 含 SUBPROCESS（10 值）" \
  "$(q "$DB" "select count(*) from pg_constraint where conrelid='biz_flow_node'::regclass
      and conname='biz_flow_node_node_type_check' and pg_get_constraintdef(oid) like '%SUBPROCESS%'")" "1"
eq "分区 CHECK 含 sub 一条" \
  "$(q "$DB" "select count(*) from pg_constraint where conrelid='biz_flow_node'::regclass
      and conname='ck_biz_flow_node_config_partition' and pg_get_constraintdef(oid) like '%sub%'")" "1"
eq "biz_flow_node.subprocess_def_id 存在" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_node'
      and column_name='subprocess_def_id'")" "1"
eq "biz_flow_instance 父子两列存在" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_instance'
      and column_name in ('parent_instance_id','parent_node_key')")" "2"
eq "父子部分索引存在" \
  "$(q "$DB" "select count(*) from pg_indexes where indexname='idx_biz_flow_instance_parent'")" "1"

# ---------- ② 重启：应用 0 个迁移 ----------
INSTANCES_BEFORE=$(q "$DB" "select count(*) from biz_flow_instance")
stop_instance
sleep 2
start_instance || { bad "实例没起来"; tail -20 "$ILOG"; summary; }
wait_http || { bad "实例 180s 内没起来"; summary; }
grep -E "应用 [0-9]+ 个迁移" "$ILOG" | tail -2
grep -q "应用 0 个迁移" "$ILOG" && ok "重启「应用 0 个迁移」（V0341 幂等）" \
  || bad "重启仍应用了迁移：$(grep -oE '应用 [0-9]+ 个迁移' "$ILOG" | tail -1)"
eq "重启后实例行数不变（迁移不改业务数据）" \
  "$(q "$DB" "select count(*) from biz_flow_instance")" "$INSTANCES_BEFORE"

# ---------- ③ down.sql 回滚 ----------
say ""
say "--- down.sql 回滚 ---"
stop_instance
sleep 2
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0341__flow_subprocess_node.down.sql 2>&1 | tail -3
eq "down 后父子列清除" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_instance'
      and column_name in ('parent_instance_id','parent_node_key')")" "0"
eq "down 后部分索引清除" \
  "$(q "$DB" "select count(*) from pg_indexes where indexname='idx_biz_flow_instance_parent'")" "0"
eq "down 后 subprocess_def_id 清除" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_node'
      and column_name='subprocess_def_id'")" "0"
eq "down 后 node_type 退回 9 值（不再含 SUBPROCESS）" \
  "$(q "$DB" "select (pg_get_constraintdef(oid) like '%SUBPROCESS%') from pg_constraint
      where conrelid='biz_flow_node'::regclass and conname='biz_flow_node_node_type_check'")" "f"
eq "down 后子流程约束清除" \
  "$(q "$DB" "select count(*) from pg_constraint where conrelid='biz_flow_node'::regclass
      and conname in ('ck_biz_flow_node_subprocess_config','fk_biz_flow_node_subprocess_def')")" "0"

# ---------- ④ up.sql 重放复原 ----------
say ""
say "--- up.sql 重放（幂等）---"
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0341__flow_subprocess_node.up.sql 2>&1 | tail -3
eq "重放后父子列回来" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_instance'
      and column_name in ('parent_instance_id','parent_node_key')")" "2"
eq "重放后 subprocess_def_id 回来" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_node'
      and column_name='subprocess_def_id'")" "1"
eq "重放后 node_type 又含 SUBPROCESS" \
  "$(q "$DB" "select count(*) from pg_constraint where conrelid='biz_flow_node'::regclass
      and conname='biz_flow_node_node_type_check' and pg_get_constraintdef(oid) like '%SUBPROCESS%'")" "1"

start_instance || true
wait_http || true
grep -q "应用 0 个迁移" "$ILOG" && ok "回滚重放后重启仍「应用 0 个迁移」（指纹未变）" \
  || bad "回滚重放后重启的应用行：$(grep -oE '应用 [0-9]+ 个迁移' "$ILOG" | tail -1)"

summary
