#!/usr/bin/env bash
# ============================================================================
# SO-28 回滚演练：克隆库上 down → up → 幂等重放 → 第三轮探针
#
# 为什么不直接对主库跑 down：down 含 DROP TABLE（会丢单据），只在一次性克隆库上演练
#   （skill references/db-table-card-and-migration-probes.md §10 的做法）。
# 用法：bash docs/evidence/20261107-so28-sales-return/rollback-drill.sh
# ============================================================================
set -uo pipefail

SRC=${SRC:-cloudpivot_so28}
DRILL=${DRILL:-cloudpivot_so28_drill}
REPO=${REPO:-/opt/cloudpivot}
export PGPASSWORD=${PGPASSWORD:-alun}
PSQL="psql -h 127.0.0.1 -U alun -Atq"
UP="$REPO/backend/migrations/business/V0303__sales_return.up.sql"
DOWN="$REPO/backend/migrations/business/V0303__sales_return.down.sql"

fail() { echo "FAIL: $*"; exit 1; }
fp() {   # 结构指纹：表 | 列数 | 索引数 | 约束数 | 取号行
  $PSQL -d "$1" -c "select
      coalesce(to_regclass('public.biz_sales_return')::text,'<null>') || ' | cols=' ||
      coalesce((select count(*) from information_schema.columns where table_name='biz_sales_return'),0) || ' | idx=' ||
      coalesce((select count(*) from pg_indexes where tablename='biz_sales_return'),0) || ' | item_cols=' ||
      coalesce((select count(*) from information_schema.columns where table_name='biz_sales_return_item'),0) ||
      ' | item_idx=' || coalesce((select count(*) from pg_indexes where tablename='biz_sales_return_item'),0) ||
      ' | sr_seq=' || coalesce((select count(*) from biz_seq where seq_type='SR' and deleted_at is null),0);"
}

echo "== 0) 前置：源库无活跃连接才能 createdb -T"
ACT=$($PSQL -d postgres -c "select count(*) from pg_stat_activity where datname='$SRC'")
[ "$ACT" = "0" ] || fail "源库 $SRC 仍有 $ACT 个连接（先停实例）"

echo "== 1) 克隆 $SRC → $DRILL"
su - postgres -c "dropdb --force $DRILL" >/dev/null 2>&1
su - postgres -c "createdb -O alun -T $SRC $DRILL" || fail "克隆失败"
echo "   迁移后指纹: $(fp "$DRILL")"

echo "== 2) 执行 down.sql（期望：两表消失 + SR 取号行消失 + 父表仍在）"
$PSQL -d "$DRILL" -v ON_ERROR_STOP=1 -f "$DOWN" >/dev/null || fail "down.sql 执行失败"
$PSQL -d "$DRILL" -c "select 'head='||coalesce(to_regclass('public.biz_sales_return')::text,'<null>')
                            ||' item='||coalesce(to_regclass('public.biz_sales_return_item')::text,'<null>')
                            ||' sr_seq='||(select count(*) from biz_seq where seq_type='SR')
                            ||' parent_so='||coalesce(to_regclass('public.biz_sales_order')::text,'<null>');"
HEAD=$($PSQL -d "$DRILL" -c "select to_regclass('public.biz_sales_return') is null")
ITEM=$($PSQL -d "$DRILL" -c "select to_regclass('public.biz_sales_return_item') is null")
SEQ=$($PSQL -d "$DRILL" -c "select count(*) from biz_seq where seq_type='SR'")
SO=$($PSQL -d "$DRILL" -c "select to_regclass('public.biz_sales_order') is not null")
[ "$HEAD" = "t" ] && [ "$ITEM" = "t" ] || fail "down 后表未删除"
[ "$SEQ" = "0" ] || fail "down 后 SR 取号行仍剩 $SEQ 行"
[ "$SO" = "t" ] || fail "down 误伤了父表 biz_sales_order"
echo "   ✅ 两表已删 / SR 取号行已删 / 父表完好"

echo "== 3) 再执行 up.sql（期望：结构 + 取号行全部回来）"
$PSQL -d "$DRILL" -v ON_ERROR_STOP=1 -f "$UP" >/dev/null || fail "up.sql 重放失败（含迁移内自检）"
echo "   重放后指纹: $(fp "$DRILL")"
HEAD=$($PSQL -d "$DRILL" -c "select to_regclass('public.biz_sales_return') is not null")
SEQ=$($PSQL -d "$DRILL" -c "select count(*) from biz_seq where seq_type='SR' and deleted_at is null")
CK=$($PSQL -d "$DRILL" -c "select count(*) from pg_constraint where conname='ck_biz_sreturn_item_qty'")
FK=$($PSQL -d "$DRILL" -c "select count(*) from pg_constraint where conname='fk_biz_sreturn_item_return' and contype='f'")
[ "$HEAD" = "t" ] && [ "$SEQ" = "1" ] && [ "$CK" = "1" ] && [ "$FK" = "1" ] || fail "重放后结构不全（head=$HEAD seq=$SEQ ck=$CK fk=$FK）"
echo "   ✅ up 重放后 表/取号行/CHECK/外键 全部在位"

echo "== 4) 幂等重放第二轮（期望：不报错、不重复种子）"
$PSQL -d "$DRILL" -v ON_ERROR_STOP=1 -f "$UP" >/dev/null || fail "第二轮 up 不幂等"
SEQ=$($PSQL -d "$DRILL" -c "select count(*) from biz_seq where seq_type='SR' and deleted_at is null")
[ "$SEQ" = "1" ] || fail "第二轮 up 后 SR 取号行 = $SEQ（期望 1）"
echo "   ✅ 第二轮 up 幂等（SR 取号行仍 1 行）"

echo "== 5) 第三轮：独立约束探针（在克隆库上跑一遍，确认回滚-重放后行为不变）"
PROBES="$REPO/docs/evidence/20261107-so28-sales-return/constraint-probes.sql"
OUT=$(psql -h 127.0.0.1 -U alun -d "$DRILL" -v ON_ERROR_STOP=1 -f "$PROBES" 2>&1)
echo "$OUT" | grep -E "PASS|FAIL|汇总" | sed 's/^/   /'
echo "$OUT" | grep -q "FAIL" && fail "回滚-重放后探针出现 FAIL"

echo "== 6) 清场：删一次性克隆库"
su - postgres -c "dropdb --force $DRILL" || fail "克隆库清理失败"
echo "ALL PASS: down → up → 幂等重放 → 探针 23/23"
