#!/usr/bin/env bash
# E2E-13 自清 + 残留核对（145 · cloudpivot_dev）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
SCRIPT="${1:-/tmp/e2e13_cleanup.sql}"

echo "===== 清理前残留"
PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -c "
select 'import_task', count(*) from sys_import_task where file_name ilike 'e2e13%'
union all select 'import_error', count(*) from sys_import_task_error e join sys_import_task t on t.id=e.task_id where t.file_name ilike 'e2e13%'
union all select 'export_task', count(*) from sys_export_task where export_name ilike 'E2E13%'
union all select 'dict_item', count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id where t.dict_code like 'E2E13%'
union all select 'dict_type', count(*) from sys_dict_type where dict_code like 'E2E13%'
union all select 'biz_unit', count(*) from biz_unit where unit_code like 'E2E13%'
union all select 'sys_config', count(*) from sys_config where config_key like 'e2e13.%' or config_key like 'public.e2e13.%'
union all select 'sys_job', count(*) from sys_job where job_name like 'E2E13%'
union all select 'sys_job_log', count(*) from sys_job_log where job_name like 'E2E13%'
union all select 'storage_config', count(*) from sys_storage_config where config_code like 'e2e13%'
union all select 'storage_file', count(*) from sys_file where storage_code like 'e2e13%'
union all select 'help_doc', count(*) from sys_help_doc where slug like 'e2e13%'
union all select 'help_category', count(*) from sys_help_category where code like 'e2e13%'
union all select 'sys_file', count(*) from sys_file where original_name like 'E2E13%'
union all select 'sys_user', count(*) from sys_user where username like 'e2e13\_%' or username='e2e13_online'
union all select 'sys_role', count(*) from sys_role where role_code like 'E2E13%'
"

echo
echo "===== 执行清理"
PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -v ON_ERROR_STOP=1 -f "$SCRIPT" 2>&1 | tail -3

echo
echo "===== 清理后残留（全 0 才算干净）"
PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -c "
select 'import_task', count(*) from sys_import_task where file_name ilike 'e2e13%'
union all select 'import_error', count(*) from sys_import_task_error e join sys_import_task t on t.id=e.task_id where t.file_name ilike 'e2e13%'
union all select 'export_task', count(*) from sys_export_task where export_name ilike 'E2E13%'
union all select 'dict_item', count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id where t.dict_code like 'E2E13%'
union all select 'dict_type', count(*) from sys_dict_type where dict_code like 'E2E13%'
union all select 'biz_unit', count(*) from biz_unit where unit_code like 'E2E13%'
union all select 'sys_config', count(*) from sys_config where config_key like 'e2e13.%' or config_key like 'public.e2e13.%'
union all select 'sys_job', count(*) from sys_job where job_name like 'E2E13%'
union all select 'sys_job_log', count(*) from sys_job_log where job_name like 'E2E13%'
union all select 'storage_config', count(*) from sys_storage_config where config_code like 'e2e13%'
union all select 'storage_file', count(*) from sys_file where storage_code like 'e2e13%'
union all select 'help_doc', count(*) from sys_help_doc where slug like 'e2e13%'
union all select 'help_category', count(*) from sys_help_category where code like 'e2e13%'
union all select 'sys_file', count(*) from sys_file where original_name like 'E2E13%'
union all select 'sys_user', count(*) from sys_user where username like 'e2e13\_%' or username='e2e13_online'
union all select 'sys_role', count(*) from sys_role where role_code like 'E2E13%'
"
echo
echo "（按设计保留：operation_logs / audit_logs 的本次操作留痕、sys_invariant_result 的巡检历史）"
