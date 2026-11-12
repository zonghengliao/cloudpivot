#!/usr/bin/env bash
# ============================================================================
# HR-38 真库用例：hr_38_salary_adjust_resign_flow_seed_db
#
# 演练库建在 145（172.18.10.145），跑完 DROP。
#   ① 建库 → 按版本号全局升序回放**全部**迁移（含 V0394/V0395）
#   ② 断言三套流程在位（提交审批取流程的那条 SQL 能选中）
#   ③ ERP_DB_URL 指向演练库，跑 cargo test --test ..._db
#   ④ DROP 演练库并复查 pg_database = 0
# ============================================================================
set -uo pipefail

ROOT=/opt/cloudpivot
BACKEND=$ROOT/backend
MROOT=$BACKEND/migrations
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${DB:-hr38_drill}

q()  { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
adm(){ psql -h "$DBHOST" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
qf() { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1; }

FAILS=0
ok(){  printf '  ✓ %s\n' "$1"; }
bad(){ printf '  ✗ %s\n' "$1"; FAILS=$((FAILS+1)); }
eq(){ if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }

cleanup(){ adm "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1; }
trap cleanup EXIT

echo "==> 建演练库 $DB @ $DBHOST（先 DROP 残留）"
cleanup
adm "CREATE DATABASE $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }

echo "==> 回放全部迁移（按版本号全局升序）"
n=0; failed=0
while IFS=$' \t' read -r ver f; do
    if ! qf "$f" >/dev/null 2>/tmp/hr38_db_mig_err.log; then
        echo "FAIL: 迁移 v$ver $(basename "$f")"; grep -E "ERROR|FATAL" /tmp/hr38_db_mig_err.log | head -3; failed=1; break
    fi
    n=$((n+1))
done < <(ls "$MROOT"/*/*.up.sql | sed -E 's#.*/V([0-9]{4})__.*#\1 &#' | sort -n)
[ "$failed" = 1 ] && exit 2
echo "    迁移 $n 条完成"

echo "==> 三套流程在位（迁移自检之外的独立复核）"
eq "hr_salary 定义" "$(q "SELECT count(*) FROM biz_flow_def WHERE flow_code='hr_salary' AND status='PUBLISHED' AND is_default")" "1"
eq "hr_salary_adjust 定义" "$(q "SELECT count(*) FROM biz_flow_def WHERE flow_code='hr_salary_adjust' AND status='PUBLISHED' AND is_default")" "1"
eq "hr_employee_resign 定义" "$(q "SELECT count(*) FROM biz_flow_def WHERE flow_code='hr_employee_resign' AND status='PUBLISHED' AND is_default")" "1"
eq "四个审批角色在" "$(q "SELECT count(*) FROM sys_role WHERE id IN ('20000000-0000-4000-8000-0000000f0016','20000000-0000-4000-8000-0000000f0005','20000000-0000-4000-8000-0000000f0006','20000000-0000-4000-8000-0000000f0003') AND deleted_at IS NULL")" "4"
eq "库：$(printf '%s' "$DB") 表数" "$(q "SELECT count(*) > 100 FROM information_schema.tables WHERE table_schema='public'")" "t"

echo "==> 构建并运行真库用例（ERP_DB_URL → $DB）"
cd "$BACKEND" || exit 2
export ERP_DB_URL="postgres://$DBUSER:${PGPASSWORD}@$DBHOST:5432/$DB"
timeout 1800 cargo test --test hr_38_salary_adjust_resign_flow_seed_db -- --test-threads=1 2>&1 | tee /tmp/hr38_db_tests.log | tail -25
RC=${PIPESTATUS[0]}
if [ "$RC" = 0 ]; then ok "cargo test 退出码 0"; else bad "cargo test 退出码 $RC"; fi
grep -q "SKIP: 未设置 ERP_DB_URL" /tmp/hr38_db_tests.log && bad "用例被 SKIP（ERP_DB_URL 没传进去）"
grep -E "^test result:" /tmp/hr38_db_tests.log | sed 's/^/    /'

echo "==> 清理"
cleanup
eq "演练库已 DROP" "$(adm "SELECT count(*) FROM pg_database WHERE datname='$DB'")" "0"

echo
if [ "$FAILS" = 0 ]; then echo "ALL PASS（0 失败）"; exit 0; else echo "FAILED: $FAILS 项"; exit 1; fi
