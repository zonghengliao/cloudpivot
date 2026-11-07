#!/usr/bin/env bash
# ============================================================================
# FIX-6 数据回填幂等验证：直接跑 V0327 的 up（两遍）/ down（两遍 + ROLLBACK）
#
# 跑在任一有本地 PG 的机器（本卡实测在 99）：
#   DB=<已应用 V0001–V0326 的库> PGPASSWORD=alun PGHOST=127.0.0.1 PGUSER=alun \
#     bash 15_backfill_idempotency.sh
#
# 只动 material_code LIKE 'FIX6-T%' 的夹具行 + 迁移本身的 DDL，绝不碰业务数据。
# 断言的是「0.13 → 13 只放大一次」与「down 之后能除回来」。
# ============================================================================
set -uo pipefail
: "${DB:?用法：DB=<库名> bash 15_backfill_idempotency.sh}"
export PGPASSWORD="${PGPASSWORD:-alun}"
export PGHOST="${PGHOST:-127.0.0.1}"
export PGUSER="${PGUSER:-alun}"
M="${M:-/opt/cloudpivot/backend/migrations/business}"
PSQL="psql -d $DB -X -v ON_ERROR_STOP=1 -q"
Q() { psql -d "$DB" -X -tAc "$1"; }

# 先确认能连上（连不上时下面的断言会「空 == 空」假绿，必须 fail-fast）
if ! Q "select 1" >/dev/null 2>&1; then
  echo "FATAL: 连不上库 $DB（检查 PGHOST/PGUSER/PGPASSWORD；本机 alun/alun）"
  exit 1
fi

echo "=== 0) 造 4 行「迁移前形态」的物料税率（假设本库尚未跑 V0327）"
$PSQL <<'SQL'
DELETE FROM biz_material WHERE material_code LIKE 'FIX6-T%';
INSERT INTO biz_material (material_code, material_name, unit_id, material_type, tax_rate)
VALUES
  ('FIX6-T1', '13% 小数形态',   (SELECT id FROM biz_unit LIMIT 1), 'RAW', 0.1300),
  ('FIX6-T2', '0.5% 小数形态',  (SELECT id FROM biz_unit LIMIT 1), 'RAW', 0.0050),
  ('FIX6-T3', '免税 0',         (SELECT id FROM biz_unit LIMIT 1), 'RAW', 0),
  ('FIX6-T4', '已是百分数 13',  (SELECT id FROM biz_unit LIMIT 1), 'RAW', 13);
SQL
Q "select '  before '||material_code||' '||tax_rate||' '||coalesce(tax_rate_mode,'NULL') from biz_material where material_code like 'FIX6-T%' order by 1"

echo "=== 1) 第一遍 up"
$PSQL -f "$M/V0327__tax_rate_percent_scale.up.sql" 2>&1 | grep -Ev '^(NOTICE|$)' | head -5
Q "select '  pass1  '||material_code||' '||tax_rate||' '||coalesce(tax_rate_mode,'NULL') from biz_material where material_code like 'FIX6-T%' order by 1"

echo "=== 2) 第二遍 up（幂等：值不得再 ×100）"
$PSQL -f "$M/V0327__tax_rate_percent_scale.up.sql" 2>&1 | grep -Ev '^(NOTICE|$)' | head -5
PASS1=$(Q "select string_agg(material_code||':'||tax_rate||':'||tax_rate_mode, ',' order by material_code) from biz_material where material_code like 'FIX6-T%'")
echo "  pass2  $PASS1"

echo "=== 3) down 双跑 + ROLLBACK（结构/数据应回到 pass2 状态）"
$PSQL <<SQL 2>&1 | grep -Ev '^(NOTICE|$)' | head -8
BEGIN;
\i $M/V0327__tax_rate_percent_scale.down.sql
\i $M/V0327__tax_rate_percent_scale.down.sql
SELECT '  down 后在事务内：' || material_code || ' ' || tax_rate FROM biz_material WHERE material_code LIKE 'FIX6-T%' ORDER BY 1;
ROLLBACK;
SQL
AFTER=$(Q "select string_agg(material_code||':'||tax_rate||':'||tax_rate_mode, ',' order by material_code) from biz_material where material_code like 'FIX6-T%'")
echo "  after rollback $AFTER"

echo "=== 4) 结论"
[ "$PASS1" = "$AFTER" ] && echo "  ✅ up 双跑幂等（0.13→13、0.005→0.5 都没被二次放大），down 双跑+ROLLBACK 复原" || { echo "  ❌ 状态漂移"; exit 1; }
for pair in "FIX6-T1:13.0000:DECIMAL" "FIX6-T2:0.5000:DECIMAL" "FIX6-T3:0.0000:PERCENT" "FIX6-T4:13.0000:PERCENT"; do
  code=${pair%%:*}; rest=${pair#*:}; val=${rest%%:*}; mode=${rest##*:}
  got=$(Q "select tax_rate||':'||tax_rate_mode from biz_material where material_code='$code'")
  if [ "$got" = "$val:$mode" ]; then echo "  ✅ $code = $got"; else echo "  ❌ $code = $got（期望 $val:$mode）"; exit 1; fi
done
$PSQL -c "DELETE FROM biz_material WHERE material_code LIKE 'FIX6-T%'" >/dev/null
echo "  夹具已清理"
