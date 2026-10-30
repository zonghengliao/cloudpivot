#!/usr/bin/env bash
set -a
. /etc/cloudpivot/cloudpivot-test.env
set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_test -tA"

echo "== 1) 迁移记录（V0043/V0044/V0045/V0049）"
$PSQL -c "SELECT 'V' || version || ' ' || name || ' success=' || success || ' ms=' || coalesce(execution_ms, 0) FROM schema_migrations WHERE version IN ('43','44','45','49') ORDER BY version::int"
echo
echo "== 2) 通知三表 + 模板表"
$PSQL -c "SELECT tablename FROM pg_tables WHERE tablename LIKE 'sys_notice%' ORDER BY 1"
echo
echo "== 3) 索引（含未读部分索引）"
$PSQL -c "SELECT indexname || ' :: ' || indexdef FROM pg_indexes WHERE tablename IN ('sys_notice','sys_notice_receiver','sys_notice_delivery','sys_notice_template') ORDER BY tablename, indexname"
echo
echo "== 4) CHECK 约束"
$PSQL -c "SELECT conrelid::regclass::text || ' :: ' || conname FROM pg_constraint WHERE contype = 'c' AND conrelid::regclass::text LIKE 'sys_notice%' ORDER BY 1"
echo
echo "== 5) 权限点（V0045 种子 9 个 + 菜单载体）"
$PSQL -c "SELECT auth_code || ' | ' || title || ' | menu_type=' || menu_type FROM sys_menu WHERE auth_code LIKE 'business:notice%' ORDER BY auth_code"
echo
echo "== 6) 授权范围（SUPER_ADMIN 全部；其它角色仅 business:notice:mine）"
$PSQL -c "SELECT r.role_code || ' → ' || count(*) || ' 个通知权限点' FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id WHERE rm.menu_id IN (SELECT id FROM sys_menu WHERE auth_code LIKE 'business:notice%') GROUP BY r.role_code ORDER BY r.role_code"
echo
echo "== 7) 内置模板（V0044 种子）"
$PSQL -c "SELECT template_code || ' | ' || template_name || ' | channels=' || channels FROM sys_notice_template ORDER BY template_code"
echo
echo "== 8) 通知与接收人现状（造数据后）"
$PSQL -c "SELECT 'sys_notice=' || (SELECT count(*) FROM sys_notice) || '，sys_notice_receiver=' || (SELECT count(*) FROM sys_notice_receiver) || '，sys_notice_delivery=' || (SELECT count(*) FROM sys_notice_delivery)"
echo
echo "== 9) 投递记录状态分布（未配置通道一律 SKIPPED）"
$PSQL -c "SELECT channel || ' | ' || status || ' | ' || count(*) FROM sys_notice_delivery GROUP BY channel, status ORDER BY channel, status"
