#!/usr/bin/env bash
# ============================================================================
# HR-41 真库用例：business::hr::self_service_db_tests
#
# 演练库建在 **145（172.18.10.145）**，跑完 DROP（99 上不许建测试库）。
#   ① 建库 → 按版本号全局升序回放**全部**迁移
#   ② 种最小夹具：一个部门 + 一个操作人账号（迁移只建表不种业务数据；
#      self_service_db_tests::dept_id() 要读部门，建单要 operator）
#   ③ HR_DB_URL / ERP_DB_URL 指向演练库，跑那 5 条真库用例
#   ④ 残留复核 + DROP 演练库并复查 pg_database = 0
#
# 用法（口令从 145 的 /etc/cloudpivot/cloudpivot-dev.env 读，不硬编码）：
#   export PGPASSWORD=$(ssh root@172.18.10.145 \
#     'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')
#   bash 01_db_tests.sh
#
# 可选环境变量：BACKEND / CARGO_TARGET_DIR / DB / DBHOST / DBUSER
# ============================================================================
set -uo pipefail

BACKEND=${BACKEND:-/opt/cloudpivot/backend}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/tmp/hr41-target}
MROOT=$BACKEND/migrations
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${DB:-hr41_drill}

if [ -z "${PGPASSWORD:-}" ]; then
    echo "FAIL: 未设置 PGPASSWORD（从 145 的 /etc/cloudpivot/cloudpivot-dev.env 读 NIK_DATABASE_PASSWORD）"
    exit 2
fi
export PGPASSWORD

q()   { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
adm() { psql -h "$DBHOST" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
qf()  { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1; }

FAILS=0
ok()  { printf '  ✓ %s\n' "$1"; }
bad() { printf '  ✗ %s\n' "$1"; FAILS=$((FAILS+1)); }
eq()  { if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }

cleanup(){ adm "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1; }
trap cleanup EXIT

echo "==> 建演练库 $DB @ $DBHOST（先 DROP 残留）"
cleanup
adm "CREATE DATABASE $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }

echo "==> 回放全部迁移（按版本号全局升序）"
n=0; failed=0
while IFS=$' \t' read -r ver f; do
    if ! qf "$f" >/dev/null 2>/root/tmp/hr41_db_mig_err.log; then
        echo "FAIL: 迁移 v$ver $(basename "$f")"
        grep -E "ERROR|FATAL" /root/tmp/hr41_db_mig_err.log | head -3
        failed=1; break
    fi
    n=$((n+1))
done < <(ls "$MROOT"/*/*.up.sql | sed -E 's#.*/V([0-9]{4})__.*#\1 &#' | sort -n)
[ "$failed" = 1 ] && exit 2
echo "    迁移 $n 条完成"

echo "==> 前置复核（本卡依赖的三样东西必须在位）"
eq "17 个薪资项种子（V0396）" \
   "$(q "SELECT count(*) FROM biz_salary_item WHERE deleted_at IS NULL")" "17"
eq "biz_salary_detail 表（V0383）" \
   "$(q "SELECT count(*) FROM information_schema.tables WHERE table_name='biz_salary_detail'")" "1"
eq "biz_attendance_record 有 dept_id 列（数据权限登记用）" \
   "$(q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_attendance_record' AND column_name='dept_id'")" "1"

echo "==> 种最小夹具（部门 + 操作人）"
q "INSERT INTO sys_dept (id, dept_name, dept_code, parent_id, sort_order, status)
   SELECT '30000000-0000-4000-8000-000000000041'::uuid, 'HR41测试部', 'HR41', NULL, 941, 1
    WHERE NOT EXISTS (SELECT 1 FROM sys_dept WHERE deleted_at IS NULL)" >/dev/null
q "INSERT INTO sys_user (id, username, password, real_name, status)
   SELECT '40000000-0000-4000-8000-000000000041'::uuid, 'hr41_tester', 'x', 'HR41测试员', 1
    WHERE NOT EXISTS (SELECT 1 FROM sys_user WHERE deleted_at IS NULL)" >/dev/null
eq "部门就位" "$(q "SELECT count(*) > 0 FROM sys_dept WHERE deleted_at IS NULL")" "t"
eq "操作人就位" "$(q "SELECT count(*) > 0 FROM sys_user WHERE deleted_at IS NULL")" "t"

echo "==> 构建并运行真库用例（HR_DB_URL/ERP_DB_URL → $DB）"
cd "$BACKEND" || exit 2
export HR_DB_URL="postgres://$DBUSER:${PGPASSWORD}@$DBHOST:5432/$DB"
export ERP_DB_URL="$HR_DB_URL"
timeout 2400 cargo test --lib business::hr::self_service_db_tests -- --test-threads=1 2>&1 \
    | tee /root/tmp/hr41_db_tests.log | tail -30
RC=${PIPESTATUS[0]}
if [ "$RC" = 0 ]; then ok "cargo test 退出码 0"; else bad "cargo test 退出码 $RC"; fi
grep -q "SKIP: 未设置 HR_DB_URL" /root/tmp/hr41_db_tests.log && bad "用例被 SKIP（库地址没传进去）"
grep -q "SKIP: HR_DB_URL" /root/tmp/hr41_db_tests.log && bad "用例被 SKIP（连不上库）"
eq "真库用例执行数" \
   "$(grep -cE '^test business::hr::self_service_db_tests::db::' /root/tmp/hr41_db_tests.log)" "5"
grep -E "^test result:" /root/tmp/hr41_db_tests.log | sed 's/^/    /'

echo "==> 残留复核（用例自清场：员工 / 账号 / 考勤 / 请假 / 薪资单都要删干净）"
eq "没有本卡夹具的员工" "$(q "SELECT count(*) FROM biz_employee WHERE emp_no LIKE 'HR41%'")" "0"
eq "没有本卡夹具的账号" "$(q "SELECT count(*) FROM sys_user WHERE username LIKE 'hr41\\_%'")" "0"
eq "没有本卡夹具的薪资单" "$(q "SELECT count(*) FROM biz_salary_sheet WHERE remark LIKE 'HR41%'")" "0"
eq "没有本卡夹具的考勤记录" "$(q "SELECT count(*) FROM biz_attendance_record WHERE employee_no LIKE 'HR41%'")" "0"
eq "没有本卡夹具的请假单" "$(q "SELECT count(*) FROM biz_leave_request WHERE employee_no LIKE 'HR41%'")" "0"

echo "==> 清理"
cleanup
eq "演练库已 DROP" "$(adm "SELECT count(*) FROM pg_database WHERE datname='$DB'")" "0"

echo
if [ "$FAILS" = 0 ]; then echo "ALL PASS（0 失败）"; exit 0; else echo "FAILED: $FAILS 项"; exit 1; fi
