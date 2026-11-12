#!/usr/bin/env bash
# V0398 收口自检的「幂等 + 造错必红」实测（145 演练库）
set -uo pipefail
ROOT=/opt/cloudpivot
UP=$ROOT/backend/migrations/business/V0398__hr_selfcheck.up.sql
DOWN=$ROOT/backend/migrations/business/V0398__hr_selfcheck.down.sql
export PGPASSWORD="$(cat ${HR45_PW_FILE:?请指向本机保存 145 口令的文件})"
CONN="postgres://cloudpivot:$PGPASSWORD@172.18.10.145:5432/hr45_drill3"

echo "=== ① 幂等：连跑两遍 up.sql ==="
for i in 1 2; do
    if psql "$CONN" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$UP" 2>&1 | grep -E "NOTICE|ERROR" | sed 's/^/  /'; then :; fi
    echo "  第 $i 遍 RC=$?"
done

echo
echo "=== ② down.sql 可执行（无状态可回滚）==="
psql "$CONN" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$DOWN" 2>&1 | sed 's/^/  /'; echo "  RC=$?"

echo
echo "=== ③ 造错必红：逐项破坏一个种子，再跑 up.sql 应 RAISE 并点名 ==="
run_break() {  # $1=名字 $2=破坏 SQL
    local out rc
    out="$(psql "$CONN" -X -q -v ON_ERROR_STOP=1 <<SQL 2>&1
BEGIN;
$2
\i $UP
ROLLBACK;
SQL
)"
    rc=$?
    echo "── $1"
    if [ "$rc" -eq 0 ]; then
        echo "  ✗ 未报红（自检没牙）RC=0"
        return 1
    fi
    printf '%s\n' "$out" | grep -E "ERROR|收口自检失败" | head -3 | sed 's/^/  /'
    echo "  ✓ 报红 RC=$rc"
}

run_break "软删薪资项 NET（期望点名 薪资项 NET + 系统项 is_system）" \
  "UPDATE biz_salary_item SET deleted_at = now() WHERE item_code = 'NET';"
run_break "软删菜单目录（期望点名 菜单 目录 / 菜单合计）" \
  "UPDATE sys_menu SET deleted_at = now() WHERE id = '20000000-0000-4000-8000-00000000e000';"
run_break "软删科目 5602.10（期望点名 科目 5602.10）" \
  "UPDATE biz_account SET deleted_at = now() WHERE account_code = '5602.10';"
run_break "停用字典项 hr_leave_type 一项（期望点名 字典 hr_leave_type）" \
  "UPDATE sys_dict_item SET status = 0
    WHERE dict_type_id = '20000000-0000-4000-8000-00000000d104'
      AND item_value = 'OTHER';"
run_break "把流程 hr_salary 改成非默认（期望点名 流程 biz_type=HR_SALARY）" \
  "UPDATE biz_flow_def SET is_default = FALSE WHERE flow_code = 'hr_salary';"
run_break "停用一条脱敏规则（期望点名 脱敏 biz_employee.id_card）" \
  "UPDATE sys_field_mask SET status = 0 WHERE table_name='biz_employee' AND column_name='id_card';"
run_break "删掉一条 sys_config（期望点名 配置）" \
  "UPDATE sys_config SET deleted_at = now() WHERE config_key = 'hr.salary.iit_mode';"

run_break "软删一条按钮权限点（期望点名 按钮权限点 29/30 + 合计 35/36）" \
  "UPDATE sys_menu SET deleted_at = now()
    WHERE deleted_at IS NULL AND menu_type = 3 AND auth_code = 'business:hr:salary:post';"
run_break "改 EMP 号段位数（期望点名 号段 EMP 口径）" \
  "UPDATE biz_seq SET pad_width = 5 WHERE seq_type = 'EMP' AND deleted_at IS NULL;"
run_break "给 5602.10 挂一个子科目让父级口径分叉（期望点名 科目 5602.10 末级）" \
  "INSERT INTO biz_account (account_code, account_name, parent_id, level, is_leaf,
                            account_category, balance_direction, status)
   SELECT '5602.10.01', '工资子目（自检夹具）', id, level + 1, 1,
          account_category, balance_direction, status
     FROM biz_account WHERE account_code = '5602.10' AND deleted_at IS NULL;"

echo
echo "=== ④ 破坏后回滚零残留复核 ==="
psql "$CONN" -X -tA <<'SQL'
SELECT 'biz_salary_item 未删=' || count(*) FROM biz_salary_item WHERE deleted_at IS NULL;
SELECT 'sys_menu 未删=' || count(*) FROM sys_menu WHERE deleted_at IS NULL;
SELECT 'biz_account(5602.10) 未删=' || count(*) FROM biz_account WHERE account_code='5602.10' AND deleted_at IS NULL;
SELECT 'sys_config(hr.*) 未删=' || count(*) FROM sys_config WHERE config_key LIKE 'hr.%' AND deleted_at IS NULL;
SQL
