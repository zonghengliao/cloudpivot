#!/usr/bin/env bash
# ============================================================================
# P3-02 验收 ①：V0333 在「V0332 之上的升级库」应用成功 + 重启 0 个 + down 可回滚
#
# 做法：
#   1. 从 cloudpivot_prt03（P2-03 的隔离库，schema_migrations 最大版本 = V0332）克隆一份；
#   2. 用**新二进制**启动 → 日志里应只出现「应用 1 个迁移」（就是 V0333）；
#   3. 再启动一次 → 「应用 0 个迁移」（指纹校验通过）；
#   4. 手工回放 V0333 down.sql → 三个列 / 约束 / 索引全部消失，并且引擎查询不依赖残留；
#   5. 手工回放 up.sql → 全部回来（预置链条数与升级前一致）。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

TEMPLATE=${TEMPLATE:-cloudpivot_prt03}
DB=${DB:-cloudpivot_p302_upgrade}
PORT=${PORT:-18202}
ENVF="$RAW/upgrade.env"
LOG1="$RAW/10_upgrade_first.log"
LOG2="$RAW/10_upgrade_restart.log"
PIDF="$RAW/upgrade.pid"
OUT="$RAW/10_upgrade.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-02 ① 迁移升级（$DB ← template $TEMPLATE / :$PORT）================"

say "--- 0) 前提：模板库确实停在 V0332"
TPL_MAX=$(q "$TEMPLATE" "select version from schema_migrations where kind='versioned' order by applied_seq desc limit 1")
say "template=$TEMPLATE 已应用的最大版本 = V$TPL_MAX"
eq "模板库最大版本" "$TPL_MAX" "332"
say "template 上已有 V0333 痕迹（应为 0）：$(q "$TEMPLATE" "select count(*) from schema_migrations where name like '%escalation%'")"
say "template 上 sys_role.escalation_role_id（应为 0）：$(q "$TEMPLATE" "select count(*) from information_schema.columns where table_name='sys_role' and column_name='escalation_role_id'")"

say ""
say "--- 1) 克隆出升级库（CREATE DATABASE … TEMPLATE）"
stop_instance "$PIDF"
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun -T $TEMPLATE $DB" || { bad "克隆模板库失败"; summary; }
eq "克隆后最大版本" "$(q "$DB" "select version from schema_migrations where kind='versioned' order by applied_seq desc limit 1")" "332"
eq "克隆后仍无列" "$(q "$DB" "select count(*) from information_schema.columns where table_name='sys_role' and column_name='escalation_role_id'")" "0"

make_env "$DB" "$PORT" "$ENVF"
say "env 键数 = $(grep -c = "$ENVF")（含 JWT / 主密钥 / admin 口令，仅从既有实例的 /proc 抄）"

say ""
say "--- 2) 首次启动（应只应用 V0333）"
start_instance "$ENVF" "$LOG1" "$PIDF" "$PORT"
wait_http "$PORT" 120 || bad "实例 120s 内没起来"
grep -E "迁移|migration" "$LOG1" | tail -8
FULL_LINE=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG1" | tail -1)
say "首次启动 → ${FULL_LINE:-<未找到迁移日志>}"
eq "首次启动应用的迁移数" "${FULL_LINE##*应用 }" "1 个迁移"
eq "升级库最大版本" "$(q "$DB" "select version from schema_migrations where kind='versioned' order by applied_seq desc limit 1")" "333"
eq "sys_role.escalation_role_id 已建" "$(q "$DB" "select count(*) from information_schema.columns where table_name='sys_role' and column_name='escalation_role_id'")" "1"
eq "biz_flow_task 两列已建" "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_task' and column_name in ('escalated_from_role','escalate_reason')")" "2"
eq "自引用 CHECK 已建" "$(q "$DB" "select count(*) from pg_constraint where conname='ck_sys_role_escalation_self'")" "1"
eq "上级角色索引已建" "$(q "$DB" "select count(*) from pg_indexes where indexname='idx_sys_role_escalation'")" "1"
CHAIN=$(q "$DB" "select count(*) from sys_role where escalation_role_id is not null")
say "预置升级链覆盖角色数 = $CHAIN（模板库里存在的角色才有；找不到的按设计跳过）"
[ "$CHAIN" -ge 1 ] && ok "预置链非空（≥1）" || bad "预置链为空，检查 role_code 是否与迁移一致"
say "样例：$(q "$DB" "select r.role_code || ' → ' || coalesce(t.role_code,'<无>') from sys_role r left join sys_role t on t.id=r.escalation_role_id where r.escalation_role_id is not null order by r.role_code limit 6" | tr '\n' ' ')"

say ""
say "--- 3) 重启：指纹校验通过，应用 0 个迁移"
stop_instance "$PIDF"
start_instance "$ENVF" "$LOG2" "$PIDF" "$PORT"
wait_http "$PORT" 120 || bad "实例重启后 120s 内没起来"
RESTART_LINE=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG2" | tail -1)
say "重启 → ${RESTART_LINE:-<未找到迁移日志>}"
eq "重启应用的迁移数" "${RESTART_LINE##*应用 }" "0 个迁移"
stop_instance "$PIDF"

say ""
say "--- 4) down.sql 可回滚"
psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
  -f /opt/cloudpivot/backend/migrations/business/V0333__role_escalation_chain.down.sql || bad "down.sql 执行失败"
eq "down 后 escalation_role_id 消失" "$(q "$DB" "select count(*) from information_schema.columns where table_name='sys_role' and column_name='escalation_role_id'")" "0"
eq "down 后 biz_flow_task 两列消失" "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_task' and column_name in ('escalated_from_role','escalate_reason')")" "0"
eq "down 后 CHECK 消失" "$(q "$DB" "select count(*) from pg_constraint where conname='ck_sys_role_escalation_self'")" "0"
eq "down 后索引消失" "$(q "$DB" "select count(*) from pg_indexes where indexname='idx_sys_role_escalation'")" "0"

say ""
say "--- 5) up.sql 再放回去（回滚可逆）"
psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
  -f /opt/cloudpivot/backend/migrations/business/V0333__role_escalation_chain.up.sql || bad "up.sql 回放失败"
eq "回滚后重新 up：列都在" "$(q "$DB" "select count(*) from information_schema.columns where table_name='sys_role' and column_name='escalation_role_id'")" "1"
eq "回滚后重新 up：预置链条数一致" "$(q "$DB" "select count(*) from sys_role where escalation_role_id is not null")" "$CHAIN"

summary
