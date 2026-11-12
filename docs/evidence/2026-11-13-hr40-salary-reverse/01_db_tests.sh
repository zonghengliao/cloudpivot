#!/usr/bin/env bash
# ============================================================================
# HR-40 真库用例：business::hr::salary_bridge_db_tests（含 4 条红冲用例）
#
# 演练库建在 **145（172.18.10.145）**，跑完 DROP（99 上不许建测试库）。
#   ① 建库 → 按版本号全局升序回放**全部**迁移（含本卡的 V0397：
#        `uk_biz_sal_sheet_period` 排除 REVERSED，红冲后可重算）
#   ② 种最小夹具：一个部门 + 一个操作人账号
#   ③ HR_DB_URL / ERP_DB_URL 指向演练库，跑 lib 单测里的 11 条真库用例
#        （HR-39 的 7 条 + HR-40 的 4 条：两张红字 / 状态闸 / 原因必填 / 红冲后可重算）
#   ④ 残留复核 + DROP 演练库并复查 pg_database = 0
#
# 用法（口令从 145 的 /etc/cloudpivot/cloudpivot-dev.env 读，不硬编码）：
#   export PGPASSWORD=$(ssh root@172.18.10.145 \
#     'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')
#   bash 01_db_tests.sh
#
# 可选环境变量：
#   BACKEND            被测代码目录（默认 /opt/cloudpivot/backend 共享工作区；
#                      若邻居在途改动编不过，指到自己的 worktree）
#   CARGO_TARGET_DIR   编译产物目录（默认沿用 backend/target）
#   DB / DBHOST / DBUSER  演练库名与连接（默认 hr40_drill / 172.18.10.145 / cloudpivot）
# ============================================================================
set -uo pipefail

BACKEND=${BACKEND:-/opt/cloudpivot/backend}
MROOT=$BACKEND/migrations
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${DB:-hr40_drill}

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

echo "==> 回放全部迁移（按版本号全局升序；V0397 自带「红冲单不占期间位」自检）"
n=0; failed=0
while IFS=$' \t' read -r ver f; do
    if ! qf "$f" >/dev/null 2>/tmp/hr40_db_mig_err.log; then
        echo "FAIL: 迁移 v$ver $(basename "$f")"
        grep -E "ERROR|FATAL" /tmp/hr40_db_mig_err.log | head -3
        failed=1; break
    fi
    n=$((n+1))
done < <(ls "$MROOT"/*/*.up.sql | sed -E 's#.*/V([0-9]{4,})__.*#\1 &#' | sort -n)
[ "$failed" = 1 ] && exit 2
echo "    迁移 $n 条完成"

echo "==> 前置复核（本卡依赖的东西必须在位）"
eq "V0397 唯一键谓词排除 REVERSED" \
   "$(q "SELECT position('REVERSED' in indexdef) > 0 FROM pg_indexes WHERE indexname='uk_biz_sal_sheet_period'")" "t"
eq "V0397 唯一键谓词保留软删条件" \
   "$(q "SELECT position('deleted_at' in indexdef) > 0 FROM pg_indexes WHERE indexname='uk_biz_sal_sheet_period'")" "t"
eq "9 个工资末级科目（V0384）" \
   "$(q "SELECT count(*) FROM biz_account WHERE account_code IN ('2211.01','2211.02','2211.03','2221.03','2241.03','2241.04','5602.10','5602.11','5602.12') AND is_leaf=1 AND status=1 AND deleted_at IS NULL")" "9"
eq "17 个薪资项种子（V0396）" \
   "$(q "SELECT count(*) FROM biz_salary_item WHERE deleted_at IS NULL")" "17"
eq "红冲权限码在菜单种子（V0387）" \
   "$(q "SELECT count(*) FROM sys_menu WHERE auth_code = 'business:hr:salary:reverse' AND deleted_at IS NULL")" "1"

echo "==> 种最小夹具（部门 + 操作人）"
q "INSERT INTO sys_dept (id, dept_name, dept_code, parent_id, sort_order, status)
   SELECT '30000000-0000-4000-8000-000000000040'::uuid, 'HR40测试部', 'HR40', NULL, 901, 1
    WHERE NOT EXISTS (SELECT 1 FROM sys_dept WHERE deleted_at IS NULL)" >/dev/null
q "INSERT INTO sys_user (id, username, password, real_name, status)
   SELECT '40000000-0000-4000-8000-000000000040'::uuid, 'hr40_tester', 'x', 'HR40测试员', 1
    WHERE NOT EXISTS (SELECT 1 FROM sys_user WHERE deleted_at IS NULL)" >/dev/null
eq "部门就位" "$(q "SELECT count(*) > 0 FROM sys_dept WHERE deleted_at IS NULL")" "t"
eq "操作人就位" "$(q "SELECT count(*) > 0 FROM sys_user WHERE deleted_at IS NULL")" "t"

echo "==> 构建并运行真库用例（HR_DB_URL/ERP_DB_URL → $DB）"
cd "$BACKEND" || exit 2
export HR_DB_URL="postgres://$DBUSER:***@$DBHOST:5432/$DB"
export ERP_DB_URL="$HR_DB_URL"
timeout 1800 cargo test --lib business::hr::salary_bridge_db_tests -- --test-threads=1 2>&1 \
    | tee /tmp/hr40_db_tests.log | tail -25
RC=${PIPESTATUS[0]}
if [ "$RC" = 0 ]; then ok "cargo test 退出码 0"; else bad "cargo test 退出码 $RC"; fi
grep -q "SKIP: 未设置 HR_DB_URL" /tmp/hr40_db_tests.log && bad "用例被 SKIP（库地址没传进去）"
eq "真库用例执行数（HR-39 七条 + HR-40 四条）" \
   "$(grep -cE '^test business::hr::salary_bridge_db_tests' /tmp/hr40_db_tests.log)" "11"
grep -E "^test result:" /tmp/hr40_db_tests.log | sed 's/^/    /'

echo "==> 残留复核（用例自清场：凭证 / 薪资单 / 员工都要删干净）"
eq "演练库里没有工资凭证（含红字）" \
   "$(q "SELECT count(*) FROM biz_voucher WHERE source_type IN ('PAYROLL_ACCRUAL','PAYROLL_PAYMENT')")" "0"
eq "演练库里没有本卡夹具的薪资单" \
   "$(q "SELECT count(*) FROM biz_salary_sheet WHERE remark = 'HR39 真库用例'")" "0"
eq "演练库里没有本卡夹具的员工" \
   "$(q "SELECT count(*) FROM biz_employee WHERE emp_no LIKE 'HR39%'")" "0"

echo "==> 清理"
cleanup
eq "演练库已 DROP" "$(adm "SELECT count(*) FROM pg_database WHERE datname='$DB'")" "0"

echo
if [ "$FAILS" = 0 ]; then echo "ALL PASS（0 失败）"; exit 0; else echo "FAILED: $FAILS 项"; exit 1; fi
