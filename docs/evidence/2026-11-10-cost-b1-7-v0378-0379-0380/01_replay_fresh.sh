#!/usr/bin/env bash
# ============================================================================
# B1-7 空库全链重放：把全部迁移（含本卡 V0378/V0379/V0380）从零跑一遍
#
# 目的：证明三件新迁移在**空库首启**路径上不出错（客户装机 / CI 全新环境走的就是这条路），
#       并且种子结果与设计一致（菜单 411 条 / 号段 2 条 / 字典 7 项 / 13 张成本表）。
# 用法：bash 01_replay_fresh.sh
# 退出码：0 = 全部通过
# ============================================================================
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
MROOT="$ROOT/backend/migrations"
DBHOST="${DBHOST:-127.0.0.1}"; DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-alun}";      DBPASS="${DBPASS:-alun}"
DB="mig_costb17_fresh_$(date +%m%d%H%M%S)"
export PGPASSWORD="$DBPASS"
q() { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "${2:-$DB}" -tAc "$1" 2>&1; }

FAILS=0
ok(){ printf '  ✓ %s\n' "$1"; }
bad(){ printf '  ✗ %s\n' "$1"; FAILS=$((FAILS+1)); }
eq(){ if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }

cleanup(){ su - postgres -c "dropdb --force $DB" >/dev/null 2>&1 || q "DROP DATABASE IF EXISTS $DB" postgres >/dev/null 2>&1; }
trap cleanup EXIT

su - postgres -c "createdb -O $DBUSER $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }
echo "==> 重放库：$DB"

n=0
while IFS=$'\t' read -r ver f; do
    if ! PGOPTIONS='-c client_min_messages=warning' \
         psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
              --single-transaction -f "$f" >/dev/null 2>/tmp/b17_replay_err.log; then
        echo "FAIL: v$ver $(basename "$f")"
        grep -E "ERROR|FATAL" /tmp/b17_replay_err.log | head -5
        exit 2
    fi
    n=$((n + 1))
done < <(find "$MROOT" -name 'V*__*.up.sql' | awk -F'/V' '{split($NF,a,"__"); print a[1]+0 "\t" $0}' | sort -n -k1,1)
echo "  重放迁移数：$n 条"
eq "重放迁移数 300 条（B1-6 时 297，+3）" "$n" "300"

PD='20000000-0000-4000-8000-00000000'
eq "成本块菜单 26 条（6 页 + 20 按钮）" "$(q "select count(*) from sys_menu where id::text like '${PD}c%' and deleted_at is null")" "26"
eq "菜单总数 411 条" "$(q "select count(*) from sys_menu where deleted_at is null")" "411"
eq "号段 2 条（CB/CS）" "$(q "select count(*) from biz_seq where seq_type in ('COST_OBJECT','COST_PERIOD') and deleted_at is null")" "2"
eq "费用项目字典 7 项" "$(q "select count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id where t.dict_code='cost_expense_item' and i.deleted_at is null")" "7"
eq "13 张成本表齐" "$(q "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_lot','biz_cost_object','biz_cost_collect','biz_cost_labor','biz_cost_overhead','biz_cost_rate','biz_cost_alloc_rule','biz_cost_alloc_result','biz_cost_period','biz_cost_period_item','biz_cost_standard','biz_cost_standard_item','biz_cost_variance')")" "13"
eq "空库：biz_lot 0 行（回填无源数据，不报错）" "$(q "select count(*) from biz_lot")" "0"
eq "空库：角色授权 48 条（SUPER_ADMIN/ADMIN 未种 → 迁移只 NOTICE，不阻断）" "$(q "select count(*) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where m.id::text like '${PD}c%'")" "48"
eq "财务总监 / 财务经理 / 仓管三个已种角色拿到 23/22/3 条" \
   "$(q "select string_agg(c::text, '/' order by rc) from (select r.role_code rc, count(*) c from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code in ('FINANCE_MANAGER','FINANCE_DIRECTOR','WAREHOUSE_MANAGER') and m.id::text like '${PD}c%' group by 1) s")" \
   "23/22/3"

echo "=== 结果：$FAILS 条断言失败 ==="
[ "$FAILS" -eq 0 ] && exit 0 || exit 1
