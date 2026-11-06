#!/usr/bin/env bash
# ==============================================================================
# so39_rollback_drill.sh —— V0272（币种 + 汇率表）回滚演练 + 幂等重放
#
# 做法（照 references/db-table-card-and-migration-probes.md §10）：
#   ① 从「已迁到最新」的库克隆一个一次性库（秒级，不重建全链）
#   ② 在克隆库上跑 down.sql  → 断言两张表 + 唯一索引全部消失
#   ③ 再跑 up.sql            → 断言表 / 约束 / 种子 / 索引全部回来
#   ④ 第三轮依赖库自检      → 外部探针 28 项必须仍全 PASS（down/up 没留下半截状态）
#   ⑤ 再跑一遍 up.sql        → 幂等（DDL 全 skipping、种子 0 行、自检仍通过）
#   ⑥ 删掉一次性库
#
# 用法：bash scripts/verify/so39_rollback_drill.sh [源库名]
#       默认源库 cloudpivot_so39_clean（须零连接；先停实例）
# ==============================================================================
set -uo pipefail

SRC_DB=${1:-cloudpivot_so39_clean}
DRILL_DB="${SRC_DB}_drill"
PGHOST=${PGHOST:-127.0.0.1}
PGPORT=${PGPORT:-5432}
DB_USER=${DB_USER:-alun}
DB_PASS=${DB_PASS:-alun}
REPO=${REPO:-/opt/cloudpivot}
UP="$REPO/backend/migrations/business/V0272__sales_currency.up.sql"
DOWN="$REPO/backend/migrations/business/V0272__sales_currency.down.sql"
PROBES="$REPO/docs/evidence/20261107-so39-currency/constraint-probes.sql"

fail=0
q() { PGPASSWORD="$DB_PASS" psql -h "$PGHOST" -p "$PGPORT" -U "$DB_USER" -d "$1" -Atc "$2"; }
ok()   { echo "  [OK]   $1"; }
bad()  { echo "  [FAIL] $1"; fail=1; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $3"; else bad "$1 = $2（期望 $3）"; fi; }

echo "==> 0) 前置：源库 $SRC_DB 必须存在且零连接"
if [ "$(q postgres "select count(*) from pg_database where datname='$SRC_DB'")" != "1" ]; then
  echo "源库不存在：$SRC_DB" >&2; exit 1
fi
conns=$(q postgres "select count(*) from pg_stat_activity where datname='$SRC_DB'")
if [ "$conns" != "0" ]; then
  echo "源库仍有 $conns 个连接，createdb -T 会失败（先停实例）" >&2; exit 1
fi
echo "    源库零连接 ✔"

echo "==> 1) 克隆 $SRC_DB → $DRILL_DB"
su - postgres -c "dropdb --if-exists --force $DRILL_DB" >/dev/null 2>&1
su - postgres -c "createdb -O $DB_USER -T $SRC_DB $DRILL_DB" || { echo "createdb -T 失败" >&2; exit 1; }

echo "==> 2) 迁移前基线（V0272 已在源库应用 ⇒ 克隆里也有）"
check "biz_currency 存在" "$(q "$DRILL_DB" "select coalesce(to_regclass('public.biz_currency')::text,'<null>')")" "biz_currency"
check "biz_exchange_rate 存在" "$(q "$DRILL_DB" "select coalesce(to_regclass('public.biz_exchange_rate')::text,'<null>')")" "biz_exchange_rate"
check "币种种子行数" "$(q "$DRILL_DB" "select count(*) from biz_currency")" "4"

echo "==> 3) 执行 down.sql（逆序删）"
if PGPASSWORD="$DB_PASS" psql -h "$PGHOST" -p "$PGPORT" -U "$DB_USER" -d "$DRILL_DB" \
      -v ON_ERROR_STOP=1 -1 -f "$DOWN" > /tmp/so39_down.log 2>&1; then
  ok "down.sql 执行成功（$(grep -c 'NOTICE' /tmp/so39_down.log) 条 NOTICE）"
  grep 'SO-39 down' /tmp/so39_down.log | sed 's/^/         /'
else
  bad "down.sql 执行失败（见 /tmp/so39_down.log）"; sed 's/^/         /' /tmp/so39_down.log | tail -5
fi
check "biz_currency 已消失" "$(q "$DRILL_DB" "select coalesce(to_regclass('public.biz_currency')::text,'<null>')")" "<null>"
check "biz_exchange_rate 已消失" "$(q "$DRILL_DB" "select coalesce(to_regclass('public.biz_exchange_rate')::text,'<null>')")" "<null>"
check "本位币唯一索引已随表删除" "$(q "$DRILL_DB" "select count(*) from pg_indexes where indexname in ('uk_biz_currency_single_base','uk_biz_exchange_rate')")" "0"

echo "==> 4) 再执行 up.sql（回滚可逆）"
if PGPASSWORD="$DB_PASS" psql -h "$PGHOST" -p "$PGPORT" -U "$DB_USER" -d "$DRILL_DB" \
      -v ON_ERROR_STOP=1 -1 -f "$UP" > /tmp/so39_up.log 2>&1; then
  ok "up.sql 重放成功"
  grep 'SO-39 自检通过' /tmp/so39_up.log | sed 's/^.*NOTICE: /         NOTICE /'
else
  bad "up.sql 重放失败（见 /tmp/so39_up.log）"; sed 's/^/         /' /tmp/so39_up.log | tail -8
fi
check "币种种子回来" "$(q "$DRILL_DB" "select count(*) from biz_currency")" "4"
check "本位币 = CNY" "$(q "$DRILL_DB" "select code from biz_currency where is_base")" "CNY"
check "本位币行数" "$(q "$DRILL_DB" "select count(*) from biz_currency where is_base")" "1"

echo "==> 5) 第三轮：down→up 之后外部探针必须仍全 PASS"
if PGPASSWORD="$DB_PASS" psql -h "$PGHOST" -p "$PGPORT" -U "$DB_USER" -d "$DRILL_DB" \
      -v ON_ERROR_STOP=1 -f "$PROBES" > /tmp/so39_probes3.log 2>&1; then
  ok "外部探针 28 项全 PASS（$(grep -o '[0-9]* / [0-9]* PASS' /tmp/so39_probes3.log | head -1)）"
else
  bad "第三轮外部探针失败（见 /tmp/so39_probes3.log）"; tail -5 /tmp/so39_probes3.log | sed 's/^/         /'
fi

echo "==> 6) 幂等：up.sql 再跑一遍（DDL 全 skipping / 种子 0 行）"
if PGPASSWORD="$DB_PASS" psql -h "$PGHOST" -p "$PGPORT" -U "$DB_USER" -d "$DRILL_DB" \
      -v ON_ERROR_STOP=1 -1 -f "$UP" > /tmp/so39_up2.log 2>&1; then
  ok "第二次重放成功（幂等）"
  check "种子插入 0 行（ON CONFLICT DO NOTHING）" \
        "$(q "$DRILL_DB" "select count(*) from biz_currency")" "4"
else
  bad "第二次重放失败（见 /tmp/so39_up2.log）"; tail -5 /tmp/so39_up2.log | sed 's/^/         /'
fi

echo "==> 7) 清场"
su - postgres -c "dropdb --if-exists --force $DRILL_DB" >/dev/null 2>&1 && echo "    已删除 $DRILL_DB"

echo ""
if [ "$fail" = "0" ]; then
  echo "===== 回滚演练 PASS（down 可逆 / 重放幂等 / 约束在 down-up 后仍全生效）====="
  exit 0
else
  echo "===== 回滚演练 FAIL ====="
  exit 1
fi
