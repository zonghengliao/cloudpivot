#!/usr/bin/env bash
# ============================================================================
# P3-04 验收 ①：迁移 V0336 的三件事
#
#   ① 空库首启：全部迁移一次装起（含 V0336），建出 biz_flow_delegate + 2 索引 + 排除约束 + 3 个权限码菜单；
#   ② 重启：「应用 0 个迁移」（幂等，checksum 通过）；
#   ③ down 可回滚（表 / 菜单 / 授权全没了）→ 再 up 可重放（一键复原）；
#   ④ 库级护栏负例：自委托 / 区间非法 / 同委托人区间重叠（EXCLUDE）三条都必须被 PG 拒。
#
# 为什么要「删 schema_migrations 行再重启」来证明「升级库应用 1 个迁移」：
# 本卡开工时工作区里没有停在 V0335 的库（`cloudpivot_p303_e2e` 停在 334），
# 而「升级一个版本」与「删掉登记再重启」在迁移器眼里是**同一条路径**（缺哪条补哪条）。
# 这样断言比造模板库更硬：日志里能直接数出「应用 1 个迁移」。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

DB=${DB:-cloudpivot_p304_e2e}
PORT=${PORT:-18304}
ENVF=/root/p304/migrate.env     # 含密钥，**不落仓库**
LOG=/root/p304/10_upgrade_instance.log
PIDF="$RAW/upgrade.pid"
OUT="$RAW/10_upgrade.txt"
MIG_DIR=/opt/cloudpivot/backend/migrations/business
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-04 ① 迁移 V0336（$DB / :$PORT）================"

stop_instance "$PIDF"
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun $DB" || { bad "建空库失败"; summary; }
make_env "$DB" "$PORT" "$ENVF" || { bad "抄环境失败"; summary; }

say ""
say "--- 1) 空库首启：全部迁移一次装起 ---"
start_instance "$ENVF" "$LOG" "$PIDF" "$PORT" || { bad "实例没起来"; tail -20 "$LOG"; summary; }
wait_http "$PORT" 300 || { bad "实例 300s 内没起来"; tail -40 "$LOG"; summary; }
TOTAL=$(q "$DB" "select count(*) from schema_migrations where version='336'")
eq "V0336 已登记" "$TOTAL" "1"
MAXV=$(q "$DB" "select max(version::int) from schema_migrations")
eq "迁移最大号" "$MAXV" "336"
APP1=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
say "首启迁移日志：${APP1:-<未找到>}"

eq "表 biz_flow_delegate 存在" \
  "$(q "$DB" "select count(*) from information_schema.tables where table_name='biz_flow_delegate'")" "1"
eq "列数 = 11" \
  "$(q "$DB" "select count(*) from information_schema.columns where table_name='biz_flow_delegate'")" "11"
eq "两条局部索引都在" \
  "$(q "$DB" "select count(*) from pg_indexes where tablename='biz_flow_delegate' and indexname in ('idx_biz_flow_delegate_active','idx_biz_flow_delegate_delegate')")" "2"
eq "区间排除约束在（btree_gist 可用）" \
  "$(q "$DB" "select count(*) from pg_constraint where conname='ex_biz_flow_delegate_no_overlap'")" "1"
eq "三个权限码菜单都在" \
  "$(q "$DB" "select count(*) from sys_menu where auth_code in ('business:flow:delegate:list','business:flow:delegate:save','business:flow:delegate:delete') and deleted_at is null")" "3"
eq "委托页面菜单挂在审批流目录下" \
  "$(q "$DB" "select parent_id::text||'|'||path||'|'||component from sys_menu where id='20000000-0000-0000-0000-000000001240'::uuid")" \
  "20000000-0000-0000-0000-000000000860|/flow/delegate|business/flow/delegate/index"

say ""
say "--- 2) 库级护栏负例（三条都必须被 PG 拒） ---"
DELEGATOR="11111111-1111-4111-8111-111111111111"
DELEGATE="22222222-2222-4222-8222-222222222222"
NEG1=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 -tAc \
  "insert into biz_flow_delegate (delegator_id, delegate_id, start_at, end_at) values ('$DELEGATOR','$DELEGATOR',now(),now()+interval '1 day')" 2>&1 | grep -c "ck_biz_flow_delegate_self")
eq "自委托被 ck_biz_flow_delegate_self 拒" "$NEG1" "1"
NEG2=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 -tAc \
  "insert into biz_flow_delegate (delegator_id, delegate_id, start_at, end_at) values ('$DELEGATOR','$DELEGATE',now()+interval '1 day',now())" 2>&1 | grep -c "ck_biz_flow_delegate_period")
eq "区间非法被 ck_biz_flow_delegate_period 拒" "$NEG2" "1"
q "$DB" "insert into biz_flow_delegate (id, delegator_id, delegate_id, start_at, end_at) values ('33333333-3333-4333-8333-333333333333','$DELEGATOR','$DELEGATE',now(),now()+interval '2 days')" >/dev/null
NEG3=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 -tAc \
  "insert into biz_flow_delegate (delegator_id, delegate_id, start_at, end_at) values ('$DELEGATOR','$DELEGATE',now()+interval '1 day',now()+interval '3 days')" 2>&1 | grep -c "ex_biz_flow_delegate_no_overlap")
eq "同委托人重叠区间被 EXCLUDE 拒" "$NEG3" "1"

say ""
say "--- 3) 重启：应用 0 个迁移（幂等） ---"
stop_instance "$PIDF"
: >"$LOG"
start_instance "$ENVF" "$LOG" "$PIDF" "$PORT" || { bad "重启失败"; tail -20 "$LOG"; summary; }
wait_http "$PORT" 180 || { bad "重启后没起来"; tail -30 "$LOG"; summary; }
APP2=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
eq "重启日志" "${APP2:-<未找到>}" "应用 0 个迁移"

say ""
say "--- 4) down 可回滚 ---"
stop_instance "$PIDF"
psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$MIG_DIR/V0336__flow_delegate.down.sql" \
  || { bad "down 执行失败"; summary; }
eq "down 后表已删" \
  "$(q "$DB" "select count(*) from information_schema.tables where table_name='biz_flow_delegate'")" "0"
eq "down 后 3 个菜单已删" \
  "$(q "$DB" "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001240'::uuid,'20000000-0000-0000-0000-000000001241'::uuid,'20000000-0000-0000-0000-000000001242'::uuid)")" "0"
eq "down 后授权行已删" \
  "$(q "$DB" "select count(*) from sys_role_menu where menu_id in ('20000000-0000-0000-0000-000000001240'::uuid,'20000000-0000-0000-0000-000000001241'::uuid,'20000000-0000-0000-0000-000000001242'::uuid)")" "0"

say ""
say "--- 5) 删登记行再启动 = 「升级库应用 1 个迁移」 ---"
q "$DB" "delete from schema_migrations where version='336'" >/dev/null
: >"$LOG"
start_instance "$ENVF" "$LOG" "$PIDF" "$PORT" || { bad "重放失败"; tail -20 "$LOG"; summary; }
wait_http "$PORT" 180 || { bad "重放后没起来"; tail -30 "$LOG"; summary; }
APP3=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
eq "升级库应用迁移数" "${APP3:-<未找到>}" "应用 1 个迁移"
eq "up 重放后表回来了" \
  "$(q "$DB" "select count(*) from information_schema.tables where table_name='biz_flow_delegate'")" "1"
eq "up 重放后排除约束也在" \
  "$(q "$DB" "select count(*) from pg_constraint where conname='ex_biz_flow_delegate_no_overlap'")" "1"

stop_instance "$PIDF"
say ""
say "（隔离实例已停；库 $DB 保留给 20_e2e_scenarios.sh 复用）"
summary
