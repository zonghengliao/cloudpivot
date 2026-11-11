#!/usr/bin/env bash
# ============================================================================
# B3-1 真库演练库：空库全链重放（301 条迁移）+ 供 cargo test --lib cost_period_db 使用
#
# 为什么要单独一个库：`cost_period_db_tests` 会 DELETE 2031-01…2031-06 的批次行做清场，
# 在多张卡共用的验证库里跑会踩别人的夹具。本脚本**只操作自己建的库**
# （默认 costb31_drill），跑完保留（下游 B3-3/B3-4 可直接复用；README 里已登记为 leak）。
#
# 用法：
#   bash 01_replay_fresh.sh              # 建库 + 全链重放
#   bash 01_replay_fresh.sh --drop       # 先删后建（重来一遍）
# 退出码：0 = 重放成功
# ============================================================================
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
MROOT="$ROOT/backend/migrations"
DBHOST="${DBHOST:-127.0.0.1}"; DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-alun}";      DBPASS="${DBPASS:-alun}"
DB="${DB:-costb31_drill}"
export PGPASSWORD="$DBPASS"
q() { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "${2:-$DB}" -tAc "$1" 2>&1; }

if [ "${1:-}" = "--drop" ]; then
    su - postgres -c "dropdb --force $DB" >/dev/null 2>&1 || true
fi
if [ "$(q "SELECT 1" postgres)" != "1" ]; then echo "FAIL: 连不上 PG"; exit 2; fi
if [ "$(q "SELECT count(*) FROM pg_database WHERE datname='$DB'" postgres)" != "0" ]; then
    echo "==> 库 $DB 已存在（要重建请加 --drop）"
else
    su - postgres -c "createdb -O $DBUSER $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }
fi
echo "==> 演练库：$DB"

n=0
ERRLOG="$(mktemp)"
while IFS=$'\t' read -r ver f; do
    if ! PGOPTIONS='-c client_min_messages=warning' \
         psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
              --single-transaction -f "$f" >/dev/null 2>"$ERRLOG"; then
        echo "FAIL: v$ver $(basename "$f")"
        grep -E "ERROR|FATAL" "$ERRLOG" | head -5
        exit 2
    fi
    n=$((n + 1))
done < <(find "$MROOT" -name 'V*__*.up.sql' | awk -F'/V' '{split($NF,a,"__"); print a[1]+0 "\t" $0}' | sort -n -k1,1)
rm -f "$ERRLOG"
echo "  重放迁移数：$n 条"

FAILS=0
ok(){ printf '  ✓ %s\n' "$1"; }
bad(){ printf '  ✗ %s\n' "$1"; FAILS=$((FAILS+1)); }
eq(){ if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }

echo "==> 本卡依赖的结构自检"
eq "biz_cost_period 在"          "$(q "SELECT to_regclass('public.biz_cost_period') IS NOT NULL")" "t"
eq "biz_cost_period_item 在"     "$(q "SELECT to_regclass('public.biz_cost_period_item') IS NOT NULL")" "t"
eq "唯一键排除 REVERSED/CANCELLED" \
   "$(q "SELECT count(*) FROM pg_indexes WHERE indexname='uk_biz_cost_period' AND indexdef LIKE '%REVERSED%' AND indexdef LIKE '%CANCELLED%'")" "1"
eq "uk_biz_cost_period_no 在"    "$(q "SELECT count(*) FROM pg_indexes WHERE indexname='uk_biz_cost_period_no'")" "1"
eq "状态 CHECK 七值"             "$(q "SELECT count(*) FROM pg_constraint WHERE conname='ck_biz_cost_period_status'")" "1"
eq "成本核算页面菜单（list 权限码）" \
   "$(q "SELECT count(*) FROM sys_menu WHERE auth_code='business:erp:cost-period:list' AND deleted_at IS NULL")" "1"
eq "成本核算按钮菜单 4 个"        \
   "$(q "SELECT count(*) FROM sys_menu WHERE auth_code IN ('business:erp:cost-period:calc','business:erp:cost-period:submit','business:erp:cost-period:settle','business:erp:cost-period:reverse') AND deleted_at IS NULL")" "4"
eq "结算审批流种子在（biz_type=COST_SETTLE）" \
   "$(q "SELECT count(*) FROM biz_flow_def WHERE biz_type='COST_SETTLE' AND deleted_at IS NULL")" "1"

echo "==> ERP_DB_URL=postgres://$DBUSER:***@$DBHOST:$DBPORT/$DB"
if [ "$FAILS" = "0" ]; then echo "RESULT: PASS"; else echo "RESULT: FAIL ($FAILS)"; exit 1; fi
