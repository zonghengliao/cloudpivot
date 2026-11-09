#!/usr/bin/env bash
# NEAR-FIX-4 干净环境演练：造一个「只有两张表」的最小库，铺事故现场，验判定侧认不认「结论过期」。
#
# 为什么要单独造库：生产 144 的库不能为了演练塞假 FAIL 行/假迁移行（那是往账簿里写伪证）。
# 两张表的 DDL 直接从生产库 pg_dump -s 搬过来（结构一致），数据全是造的。
#
# 四个场景（每个都要看输出里「数据源 / 判定」两行）：
#   A stale_fail   结论 17h 前 FAIL(blockers=1) + 之后落了一条「无关迁移」   → 期望 WARN「结论早于最后一次迁移，待重跑」
#   B current_fail 同上但没有「之后的迁移」（只有更早的秒/毫秒记账）          → 期望 ALERT「账已错」（旧口径不能被吞掉）
#   C stale_ok     结论 2h 前 OK + 之后落了一条迁移                          → 期望 INFO「已通过，但新迁移未复核」
#   D stale_old    结论 40h 前 FAIL + 之后落了一条迁移                       → 期望 ALERT「早于最后一次迁移且已 N 小时未重跑」
#
# 场景 B 是关键回归：库里同时存在一条**毫秒单位**的旧记账（144 的真实形态）。若不做单位归一，
# 毫秒值（1.79e12）永远大于结论的秒值 ⇒ 场景 B 会被误判成「过期」，真正的新错账就漏报了。
set -uo pipefail

DB="${DRILL_DB:-cp_drill_inv4}"
# 安全闸：这个脚本会 DROP/CREATE 库，库名必须形如 cp_drill_*（防止 DRILL_DB 手滑指向生产库）
case "$DB" in cp_drill_*) ;; *) echo "拒绝执行：DRILL_DB 必须形如 cp_drill_*（当前 $DB）"; exit 2 ;; esac
ALERT=/opt/cloudpivot/scripts/ops/alert_check.sh
DRILL_ENV=/tmp/drill-inv.env
OUT="${DRILL_OUT:-/tmp/drill-inv-result.txt}"

set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -X -q -A -t -h ${NIK_DATABASE_HOST} -p ${NIK_DATABASE_PORT:-5432} -U ${NIK_DATABASE_USER}"

say() { printf '\n### %s\n' "$*"; }

# ── 建库（cloudpivot 角色没有 CREATEDB，必须 postgres 建）─────────────────────
say "0) 建演练库 $DB（schema 从生产库 -s 搬，数据全造）"
su - postgres -c "psql -X -q -c 'DROP DATABASE IF EXISTS ${DB}'" >/dev/null 2>&1
su - postgres -c "createdb -O ${NIK_DATABASE_USER} ${DB}" || { echo "建库失败"; exit 1; }
pg_dump -h "$NIK_DATABASE_HOST" -p "${NIK_DATABASE_PORT:-5432}" -U "$NIK_DATABASE_USER" \
  -d "$NIK_DATABASE_NAME" -s -t public.schema_migrations -t public.sys_invariant_result \
  -f /tmp/drill_inv_schema.sql || { echo "pg_dump 取结构失败"; exit 1; }
$PSQL -d "$DB" -v ON_ERROR_STOP=1 -f /tmp/drill_inv_schema.sql >/dev/null || { echo "建表失败"; exit 1; }
echo "   ✅ $(basename /tmp/drill_inv_schema.sql) 导入完成：$($PSQL -d "$DB" -c "select string_agg(tablename,',' order by tablename) from pg_tables where schemaname='public'")"

# 演练库的 env 文件（alert_check 只挑 NIK_DATABASE_*）
cat >"$DRILL_ENV" <<ENV
NIK_DATABASE_HOST=${NIK_DATABASE_HOST}
NIK_DATABASE_PORT=${NIK_DATABASE_PORT:-5432}
NIK_DATABASE_NAME=${DB}
NIK_DATABASE_USER=${NIK_DATABASE_USER}
NIK_DATABASE_PASSWORD=${NIK_DATABASE_PASSWORD}
ENV
chmod 600 "$DRILL_ENV"

# ── 铺现场 ───────────────────────────────────────────────────────────────────
reset_fixture() {
  local variant="$1"
  $PSQL -d "$DB" -v ON_ERROR_STOP=1 >/dev/null <<SQL
TRUNCATE sys_invariant_result; TRUNCATE schema_migrations;
-- 更早的版本化迁移（秒）
INSERT INTO schema_migrations (kind,name,version,checksum,applied_via,applied_at,applied_seq,execution_ms,success,source)
VALUES ('versioned','V0331__purchase_requisition','331','c1','executed',
        (extract(epoch from now() - interval '30 hours'))::bigint, 1, 0, true,
        'migrations/business/V0331__purchase_requisition.up.sql');
-- 更早的**毫秒**记账（144 的历史形态：手工 psql 补记账写成了毫秒）
INSERT INTO schema_migrations (kind,name,version,checksum,applied_via,applied_at,applied_seq,execution_ms,success,source)
VALUES ('versioned','V0156__remove_erp_top_menu','156','c2','imported',
        (extract(epoch from now() - interval '40 hours'))::bigint * 1000, 2, 0, true,
        'migrations/business/V0156__remove_erp_top_menu.up.sql');
SQL
  case "$variant" in
    stale_fail|stale_ok|stale_old)
      # 「修账的无关迁移」落在结论之后
      $PSQL -d "$DB" -v ON_ERROR_STOP=1 >/dev/null <<SQL
INSERT INTO schema_migrations (kind,name,version,checksum,applied_via,applied_at,applied_seq,execution_ms,success,source)
VALUES ('versioned','V0366__seed_fix_flow_is_system','366','c3','executed',
        (extract(epoch from now() - interval '2 hours'))::bigint, 3, 0, true,
        'migrations/business/V0366__seed_fix_flow_is_system.up.sql');
SQL
      ;;
  esac
  case "$variant" in
    stale_fail|current_fail)
      # 事故同形：17 小时前的 SCHEDULED FAIL（blockers=1 H6 / failed=2）
      $PSQL -d "$DB" -v ON_ERROR_STOP=1 >/dev/null <<SQL
INSERT INTO sys_invariant_result (id,run_at,trigger_type,status,total,passed,failed,blockers,warns,duration_ms,database_name,script_path,error)
VALUES (gen_random_uuid(), now() - interval '17 hours', 'SCHEDULED','FAIL',79,77,2,1,1,3200,
        'cloudpivot','/opt/cloudpivot/scripts/verify/erp_invariants.py',NULL);
SQL
      ;;
    stale_ok)
      $PSQL -d "$DB" -v ON_ERROR_STOP=1 >/dev/null <<SQL
INSERT INTO sys_invariant_result (id,run_at,trigger_type,status,total,passed,failed,blockers,warns,duration_ms,database_name,script_path,error)
VALUES (gen_random_uuid(), now() - interval '3 hours', 'SCHEDULED','OK',80,80,0,0,0,3300,
        'cloudpivot','/opt/cloudpivot/scripts/verify/erp_invariants.py',NULL);
SQL
      ;;
    stale_old)
      $PSQL -d "$DB" -v ON_ERROR_STOP=1 >/dev/null <<SQL
INSERT INTO sys_invariant_result (id,run_at,trigger_type,status,total,passed,failed,blockers,warns,duration_ms,database_name,script_path,error)
VALUES (gen_random_uuid(), now() - interval '40 hours', 'SCHEDULED','FAIL',79,77,2,1,1,3200,
        'cloudpivot','/opt/cloudpivot/scripts/verify/erp_invariants.py',NULL);
SQL
      ;;
  esac
  # 铺完把「库里看到的事实」原样打出来（归一前后对比）
  $PSQL -d "$DB" -c "select '结论: ' || status || ' blockers=' || blockers || ' run_at=' || to_char(run_at,'MM-DD HH24:MI') || ' (' || round(extract(epoch from (now()-run_at))/3600) || 'h 前)' from sys_invariant_result order by run_at desc limit 1"
  $PSQL -d "$DB" -c "select '迁移: ' || name || ' applied_at=' || applied_at || ' → ' || to_char(to_timestamp(case when applied_at > 100000000000 then applied_at/1000 else applied_at end),'MM-DD HH24:MI') || case when applied_at > 100000000000 then ' (毫秒记账)' else ' (秒)' end from schema_migrations order by applied_seq"
}

run_alert() {
  NIK_ALERT_ENV_FILE="$DRILL_ENV" \
  NIK_ALERT_LOG=/tmp/drill-inv-alert.log \
  NIK_ALERT_STATE_DIR=/tmp/drill-inv-state \
  NIK_ALERT_KANBAN=off \
  NIK_ALERT_BASE_URL=http://127.0.0.1:9 \
  bash "$ALERT" --only invariant --dry-run 2>&1 | sed 's/^/   /'
}

for variant in stale_fail current_fail stale_ok stale_old; do
  case "$variant" in
    stale_fail)   say "A) stale_fail —— 结论 17h 前 FAIL，之后落了一条无关迁移（本次误报的同形现场）";;
    current_fail) say "B) current_fail —— 同样的 FAIL 结论，但迁移都更早（含一条毫秒记账）";;
    stale_ok)     say "C) stale_ok —— 结论 3h 前 OK，之后落了迁移";;
    stale_old)    say "D) stale_old —— 结论 40h 前 FAIL，之后落了迁移（>36h 真没人重跑）";;
  esac
  reset_fixture "$variant"
  run_alert
  say "   ↳ 判定行（level — title）"
  printf '   %s\n' "$(NIK_ALERT_ENV_FILE="$DRILL_ENV" NIK_ALERT_LOG=/tmp/drill-inv-alert.log \
      NIK_ALERT_STATE_DIR=/tmp/drill-inv-state NIK_ALERT_KANBAN=off NIK_ALERT_BASE_URL=http://127.0.0.1:9 \
      bash "$ALERT" --only invariant --dry-run 2>&1 | grep -E 'invariant: (OK|INFO|WARN|ALERT)' | tail -1)"
done

{
  echo
  echo "=== 演练库结构 / 现场（最后一轮 = stale_old）==="
  $PSQL -d "$DB" -c "select status, blockers, failed, to_char(run_at,'YYYY-MM-DD HH24:MI:SS') as run_at from sys_invariant_result"
  $PSQL -d "$DB" -c "select name, applied_at, case when applied_at > 100000000000 then 'ms' else 's' end as unit from schema_migrations order by applied_seq"
} | tee "$OUT"
