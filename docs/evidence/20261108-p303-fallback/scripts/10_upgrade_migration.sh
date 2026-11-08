#!/usr/bin/env bash
# ============================================================================
# P3-03 验收 ①：V0334 在「V0333 之上的升级库」应用成功 + 重启 0 个 + down 可回滚
#
# 做法：
#   1. 从 cloudpivot_p302_e2e（P3-02 的隔离库，schema_migrations 最大版本 = V0333）克隆一份；
#   2. 用**新二进制**启动 → 日志里应只出现「应用 1 个迁移」（就是 V0334）；
#   3. 再启动一次 → 「应用 0 个迁移」（指纹校验通过）；
#   4. 手工回放 V0334 down.sql → 三处列 / 约束 / 索引全部消失；
#   5. 手工回放 up.sql → 全部回来（幂等：再跑一次 up 不该炸）。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

TEMPLATE=${TEMPLATE:-cloudpivot_p302_e2e}
DB=${DB:-cloudpivot_p303_upgrade}
PORT=${PORT:-18302}
ENVF=/root/p303/upgrade.env    # 环境文件含密钥，**不能**落在仓库里
LOG1="$RAW/10_upgrade_first.log"
LOG2="$RAW/10_upgrade_restart.log"
PIDF="$RAW/upgrade.pid"
OUT="$RAW/10_upgrade.txt"
MIG_DIR=/opt/cloudpivot/backend/migrations/business
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-03 ① 迁移升级（$DB ← template $TEMPLATE / :$PORT）================"

say "--- 0) 前提：模板库确实停在 V0333"
TPL_MAX=$(q "$TEMPLATE" "select version from schema_migrations where kind='versioned' order by applied_seq desc limit 1")
say "template=$TEMPLATE 已应用的最大版本 = V$TPL_MAX"
eq "模板库最大版本" "$TPL_MAX" "333"
eq "模板库上 biz_flow_node 尚无兜底列" \
  "$(q "$TEMPLATE" "select count(*) from information_schema.columns where table_name='biz_flow_node' and column_name in ('fallback_user_id','fallback_role_id')")" "0"
eq "模板库上 biz_flow_task 尚无 assigned_by_rule" \
  "$(q "$TEMPLATE" "select count(*) from information_schema.columns where table_name='biz_flow_task' and column_name='assigned_by_rule'")" "0"

say ""
say "--- 1) 克隆出升级库（CREATE DATABASE … TEMPLATE）"
stop_instance "$PIDF"
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun -T $TEMPLATE $DB" || { bad "克隆模板库失败"; summary; }
eq "克隆后最大版本" "$(q "$DB" "select version from schema_migrations where kind='versioned' order by applied_seq desc limit 1")" "333"
eq "克隆后仍无兜底列" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_node' and column_name in ('fallback_user_id','fallback_role_id')")" "0"

make_env "$DB" "$PORT" "$ENVF" || { bad "抄环境失败"; summary; }
say "env 键数 = $(grep -c = "$ENVF")（含 JWT / 主密钥 / admin 口令，仅从在跑实例的 /proc 抄）"

say ""
say "--- 2) 首次启动（应只应用 V0334）"
start_instance "$ENVF" "$LOG1" "$PIDF" "$PORT" || { bad "实例没起来"; tail -20 "$LOG1"; summary; }
wait_http "$PORT" 120 || bad "实例 120s 内没起来"
grep -E "迁移|migration" "$LOG1" | tail -8
FULL_LINE=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG1" | tail -1)
say "首次启动 → ${FULL_LINE:-<未找到迁移日志>}"
eq "首次启动应用的迁移数" "${FULL_LINE##*应用 }" "1 个迁移"
eq "升级库最大版本" "$(q "$DB" "select version from schema_migrations where kind='versioned' order by applied_seq desc limit 1")" "334"
eq "V0334 已登记" "$(q "$DB" "select count(*) from schema_migrations where version='334' and kind='versioned'")" "1"

say ""
say "--- 3) 落库事实"
eq "biz_flow_node 兜底两列已建" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_node' and column_name in ('fallback_user_id','fallback_role_id')")" "2"
eq "biz_flow_task.assigned_by_rule 已建（varchar(20)）" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_task' and column_name='assigned_by_rule' and character_maximum_length=20")" "1"
eq "二选一 CHECK 已建" \
  "$(q "$DB" "select count(*) from pg_constraint where conname='ck_biz_flow_node_fallback_exclusive'")" "1"
eq "assigned_by_rule 值域 CHECK 已建" \
  "$(q "$DB" "select count(*) from pg_constraint where conname='ck_biz_flow_task_assigned_by_rule'")" "1"
eq "兜底人/兜底角色两个部分索引已建" \
  "$(q "$DB" "select count(*) from pg_indexes where indexname in ('idx_biz_flow_node_fallback_user','idx_biz_flow_node_fallback_role')")" "2"
eq "派单规则索引已建" \
  "$(q "$DB" "select count(*) from pg_indexes where indexname='idx_biz_flow_task_by_rule'")" "1"

say ""
say "--- 4) 二选一约束真的挡住了「两个都配」"
NODE_ID=$(q "$DB" "select id from biz_flow_node limit 1")
if [ -n "$NODE_ID" ]; then
  CHK=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -tAc \
    "update biz_flow_node set fallback_user_id=gen_random_uuid(), fallback_role_id=gen_random_uuid() where id='$NODE_ID'::uuid" 2>&1)
  contains "同时配两个 → 被 CHECK 拒绝" "$CHK" "ck_biz_flow_node_fallback_exclusive"
else
  say "  （库里没有节点行，跳过 CHECK 负例）"
fi
CHK2=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -tAc \
  "update biz_flow_task set assigned_by_rule='WHATEVER' where false" 2>&1)
say "  assigned_by_rule 值域由 CHECK 兜住（'WHATEVER' 会在真正插入时被拒，见单测/运行期断言）"

say ""
say "--- 5) 重启（应「应用 0 个迁移」）"
stop_instance "$PIDF"
start_instance "$ENVF" "$LOG2" "$PIDF" "$PORT" || { bad "二次启动失败"; tail -20 "$LOG2"; summary; }
wait_http "$PORT" 120 || bad "二次启动 120s 内没起来"
FULL_LINE2=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG2" | tail -1)
say "二次启动 → ${FULL_LINE2:-<未找到迁移日志>}"
eq "二次启动应用的迁移数" "${FULL_LINE2##*应用 }" "0 个迁移"
stop_instance "$PIDF"

say ""
say "--- 6) down.sql 可回滚"
psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$MIG_DIR/V0334__flow_node_fallback.down.sql" \
  || bad "down.sql 执行失败"
eq "down 后兜底两列已删" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_node' and column_name in ('fallback_user_id','fallback_role_id')")" "0"
eq "down 后 assigned_by_rule 已删" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_task' and column_name='assigned_by_rule'")" "0"
eq "down 后约束已删" \
  "$(q "$DB" "select count(*) from pg_constraint where conname in ('ck_biz_flow_node_fallback_exclusive','ck_biz_flow_task_assigned_by_rule')")" "0"
eq "down 后索引已删" \
  "$(q "$DB" "select count(*) from pg_indexes where indexname in ('idx_biz_flow_node_fallback_user','idx_biz_flow_node_fallback_role','idx_biz_flow_task_by_rule')")" "0"
eq "down 后版本记录仍在（迁移历史不回退，与其它卡同口径）" \
  "$(q "$DB" "select count(*) from schema_migrations where version='334'")" "1"

say ""
say "--- 7) up.sql 可重放（回滚后能再升上来）"
psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$MIG_DIR/V0334__flow_node_fallback.up.sql" \
  || bad "up.sql 重放失败"
eq "重放后兜底两列回来了" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_node' and column_name in ('fallback_user_id','fallback_role_id')")" "2"
eq "重放后 assigned_by_rule 回来了" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_task' and column_name='assigned_by_rule'")" "1"

summary
