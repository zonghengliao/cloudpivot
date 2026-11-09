#!/usr/bin/env bash
# E2E-13 DB 层证据 + 勾稽（145 · cloudpivot_dev，只读）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
PSQL=(psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -P pager=off)
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

run() { echo; echo "== $1"; echo "   $2"; "${PSQL[@]}" -c "$2"; }

echo "########## E2E-13 平台功能域 · DB 证据（$(date '+%F %T %Z')）"

run "G1 导入任务状态机分布（E2E13 自有）" \
"select status, count(*) as tasks, sum(total_rows) as rows_total,
        sum(valid_rows) as rows_valid, sum(error_rows) as rows_error, sum(imported_rows) as rows_imported,
        count(*) filter (where valid_rows + error_rows > total_rows) as 自洽违例
   from sys_import_task where file_name ilike 'e2e13%' group by 1 order by 1"

run "G2 导入任务 × 错误明细（错误行必须逐条可查，行号在 Excel 真实区间内）" \
"select t.task_no, t.biz_type, t.status, t.total_rows, t.valid_rows, t.error_rows, t.imported_rows,
        (select count(*) from sys_import_task_error e where e.task_id = t.id) as 明细条数
   from sys_import_task t where t.file_name ilike 'e2e13%' order by t.created_at"

run "G3 勾稽·字典项落库 vs 导入声明（v 批 5 行、big 批 2000 行）" \
"select i.item_value like 'v%' as 小批, count(*) as 行数, count(distinct i.item_value) as 去重值,
        count(*) filter (where i.status = 0) as 停用
   from sys_dict_item i join sys_dict_type t on t.id = i.dict_type_id
  where t.dict_code like 'E2E13%' and i.deleted_at is null group by 1 order by 1 desc"

run "G4 勾稽·部分导入（PARTIAL）：错误编码一个都没落库，合法行全部落库" \
"select (select count(*) from biz_unit where unit_code like 'E2E13%' and deleted_at is null) as 已落库,
        (select count(*) from biz_unit where unit_code in ('E2E13U4','E2E13U5','E2E13U6','E2E13U8')) as 错误行落库数,
        (select count(*) from biz_unit c join biz_unit b on b.id = c.base_unit_id
          where c.unit_code = 'E2E13U2' and b.unit_code = 'E2E13U1' and c.rate = 12) as 基准单位解析正确"

run "G5 勾稽·导入任务行数与落库行数对得上（PARTIAL）" \
"select t.task_no, t.status, t.imported_rows,
        (select count(*) from biz_unit where unit_code like 'E2E13%' and deleted_at is null) as 实际库内
   from sys_import_task t
  where t.file_name ilike 'e2e13%' and t.biz_type = 'business.erp.unit' and t.status = 'PARTIAL'"

run "G6 导出任务 ↔ 产物（progress 100 / 行数对得上 / SHA 与路径齐备）" \
"select task_no, format, status, total_rows, exported_rows, progress, attempt_count,
        (file_path is not null) as 有产物路径, length(file_sha256) as sha长度,
        (expire_at is not null) as 有失效时间
   from sys_export_task where export_name ilike 'E2E13%' order by created_at"

run "G7 定时任务 ↔ 执行日志（手动执行一次必有日志）" \
"select j.job_name, j.status, (j.last_run_at is not null) as 有上次执行,
        (select count(*) from sys_job_log l where l.job_id = j.id) as 日志条数,
        (select max(l.status) from sys_job_log l where l.job_id = j.id) as 最近状态
   from sys_job j where j.job_name like 'E2E13%'"

run "G8 巡检运行记录（本次 MANUAL，计数自洽，失败项可复现）" \
"select id, run_at, trigger_type, status, total, passed, failed, blockers, warns,
        (total = passed + failed) as 计数自洽, jsonb_array_length(failures) as 失败项数
   from sys_invariant_result where trigger_type = 'MANUAL' order by run_at desc limit 3"

run "G9 参数配置留痕（值被删除后不应有生效行）" \
"select config_key, config_value, is_modifiable, deleted_at is not null as 已删
   from sys_config where config_key like 'e2e13.%' or config_key like 'public.e2e13.%' order by config_key"

run "G10 帮助中心版本链（doc.version 必须等于版本表最大版本）" \
"select d.slug, d.status, d.version,
        (select max(v.version) from sys_help_doc_version v where v.doc_id = d.id) as 版本表最大
   from sys_help_doc d where d.slug like 'e2e13%'"

run "G11 审计留痕（平台域写操作都在 operation_logs 里有记录）" \
"select request_method, request_path, count(*) as 次数, min(status_code) as 最小状态码
   from operation_logs
  where request_path like '/api/support/%' or request_path like '/api/business/help/%'
     or request_path like '/api/business/storage/%' or request_path like '/api/business/impexp/%'
  group by 1,2 order by 3 desc limit 12"

run "G12 审计日志表（audit_logs）覆盖本次关键动作" \
"select operation, count(*) from audit_logs
  where operation in ('CONFIG_UPDATE','CONFIG_CREATE','DICT_TYPE_DELETE','DICT_ITEM_DELETE',
                      'JOB_CREATE','JOB_STATUS','JOB_TRIGGER','STORAGE_CONFIG_CREATE',
                      'STORAGE_CONFIG_DELETE','FILE_UPLOAD')
  group by 1 order by 2 desc"
