#!/usr/bin/env bash
# ============================================================================
# HR-39 真库用例：business::hr::salary_bridge_db_tests
#
# 演练库建在 **145（172.18.10.145）**，跑完 DROP（99 上不许建测试库）。
#   ① 建库 → 按版本号全局升序回放**全部**迁移（含 V0394 薪资流程 / V0384 工资科目）
#   ② 种最小夹具：一个部门 + 一个操作人账号（提交审批要 initiator_id，
#      迁移回放只建表不种业务数据）
#   ③ HR_DB_URL / ERP_DB_URL 指向演练库，跑 lib 单测里那 7 条真库用例
#   ④ DROP 演练库并复查 pg_database = 0
#
# 用法（口令从 145 的 /etc/cloudpivot/cloudpivot-dev.env 读，不硬编码）：
#   export PGPASSWORD=$(ssh root@172.18.10.145 \
#     'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')
#   bash 01_db_tests.sh
#
# 可选环境变量：
#   BACKEND     被测代码目录（默认 /root/hr39-wt/backend：共享工作区里有邻居在途改动，
#               用 worktree 保证「编得过的就是我这一版」）
#   CARGO_TARGET_DIR  编译产物目录（默认 /root/hr39-target）
#   DB / DBHOST / DBUSER  演练库名与连接（默认 hr39_drill / 172.18.10.145 / cloudpivot）
# ============================================================================
set -uo pipefail

BACKEND=${BACKEND:-/root/hr39-wt/backend}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/hr39-target}
MROOT=$BACKEND/migrations
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${DB:-hr39_drill}

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
    if ! qf "$f" >/dev/null 2>/tmp/hr39_db_mig_err.log; then
        echo "FAIL: 迁移 v$ver $(basename "$f")"
        grep -E "ERROR|FATAL" /tmp/hr39_db_mig_err.log | head -3
        failed=1; break
    fi
    n=$((n+1))
done < <(ls "$MROOT"/*/*.up.sql | sed -E 's#.*/V([0-9]{4})__.*#\1 &#' | sort -n)
[ "$failed" = 1 ] && exit 2
echo "    迁移 $n 条完成"

echo "==> 前置复核（本卡依赖的三样东西必须在位）"
eq "9 个工资末级科目（V0384）" \
   "$(q "SELECT count(*) FROM biz_account WHERE account_code IN ('2211.01','2211.02','2211.03','2221.03','2241.03','2241.04','5602.10','5602.11','5602.12') AND is_leaf=1 AND status=1 AND deleted_at IS NULL")" "9"
eq "带 DEPT 辅助核算的科目（5602.1x / 2211.0x）" \
   "$(q "SELECT count(*) FROM biz_account WHERE account_code IN ('5602.10','5602.11','5602.12','2211.01','2211.02','2211.03') AND upper(coalesce(aux_dimensions,'')) LIKE '%DEPT%'")" "6"
eq "17 个薪资项种子（V0396）" \
   "$(q "SELECT count(*) FROM biz_salary_item WHERE deleted_at IS NULL")" "17"
eq "hr_salary 流程 PUBLISHED/is_default（V0394）" \
   "$(q "SELECT count(*) FROM biz_flow_def WHERE flow_code='hr_salary' AND status='PUBLISHED' AND is_default")" "1"
eq "大额条件边 = form.gross_total_num gte 500000" \
   "$(q "SELECT (condition_expr->>'field')||'|'||(condition_expr->>'cmp')||'|'||(condition_expr->>'value') FROM biz_flow_edge WHERE condition_expr->>'field' = 'form.gross_total_num' LIMIT 1")" \
   'form.gross_total_num|gte|500000'
eq "工资发放凭证规则（V0384）" \
   "$(q "SELECT count(*) FROM biz_voucher_rule WHERE rule_code IN ('PAYROLL_ACCRUAL_DEFAULT','PAYROLL_PAYMENT_DEFAULT') AND is_active=1 AND deleted_at IS NULL")" "2"

echo "==> 种最小夹具（部门 + 操作人）"
q "INSERT INTO sys_dept (id, dept_name, dept_code, parent_id, sort_order, status)
   SELECT '30000000-0000-4000-8000-000000000039'::uuid, 'HR39测试部', 'HR39', NULL, 900, 1
    WHERE NOT EXISTS (SELECT 1 FROM sys_dept WHERE deleted_at IS NULL)" >/dev/null
q "INSERT INTO sys_user (id, username, password, real_name, status)
   SELECT '40000000-0000-4000-8000-000000000039'::uuid, 'hr39_tester', 'x', 'HR39测试员', 1
    WHERE NOT EXISTS (SELECT 1 FROM sys_user WHERE deleted_at IS NULL)" >/dev/null
# 审批节点的派发靠「角色下有人在职成员」：把人绑到薪资流程用到的 4 个角色上
# （不绑就一定 65018 fail-closed —— 那是引擎的正确行为，不是本卡的 bug）
q "INSERT INTO sys_user_role (user_id, role_id)
   SELECT '40000000-0000-4000-8000-000000000039'::uuid, r.id FROM sys_role r
    WHERE r.id IN ('20000000-0000-4000-8000-0000000f0016',
                   '20000000-0000-4000-8000-0000000f0005',
                   '20000000-0000-4000-8000-0000000f0006',
                   '20000000-0000-4000-8000-0000000f0003')
   ON CONFLICT (user_id, role_id) DO NOTHING" >/dev/null
eq "部门就位" "$(q "SELECT count(*) > 0 FROM sys_dept WHERE deleted_at IS NULL")" "t"
eq "操作人就位" "$(q "SELECT count(*) > 0 FROM sys_user WHERE deleted_at IS NULL")" "t"
eq "4 个审批角色都有人在职成员" \
   "$(q "SELECT count(DISTINCT role_id) FROM sys_user_role WHERE user_id = '40000000-0000-4000-8000-000000000039'::uuid")" "4"

echo "==> 构建并运行真库用例（HR_DB_URL/ERP_DB_URL → $DB）"
cd "$BACKEND" || exit 2
export HR_DB_URL="postgres://$DBUSER:$PGPASSWORD@$DBHOST:5432/$DB"
export ERP_DB_URL="$HR_DB_URL"
timeout 1800 cargo test --lib business::hr::salary_bridge_db_tests -- --test-threads=1 2>&1 \
    | tee /tmp/hr39_db_tests.log | tail -25
RC=${PIPESTATUS[0]}
if [ "$RC" = 0 ]; then ok "cargo test 退出码 0"; else bad "cargo test 退出码 $RC"; fi
grep -q "SKIP: 未设置 HR_DB_URL" /tmp/hr39_db_tests.log && bad "用例被 SKIP（库地址没传进去）"
grep -q "SKIP: 库里没有 sys_user" /tmp/hr39_db_tests.log && bad "审批回调用例被 SKIP（没种操作人）"
eq "真库用例执行数" "$(grep -cE '^test business::hr::salary_bridge_db_tests' /tmp/hr39_db_tests.log)" "7"
grep -E "^test result:" /tmp/hr39_db_tests.log | sed 's/^/    /'

echo "==> 残留复核（用例自清场：凭证 / 薪资单 / 员工都要删干净）"
eq "演练库里没有本卡夹具的凭证" \
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
