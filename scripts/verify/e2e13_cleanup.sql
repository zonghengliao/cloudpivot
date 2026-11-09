-- E2E-13 演练数据自清（只删 E2E13 前缀的自有数据；不 DROP / 不 TRUNCATE / 不碰他人数据）
-- 依据：sys_import_task.file_name / sys_export_task.export_name / 编码或名称为 E2E13* 的行

BEGIN;

-- 1) 导入任务 + 错误明细（先明细后主表；明细有 ON DELETE CASCADE，这里显式删便于核对行数）
DELETE FROM sys_import_task_error
 WHERE task_id IN (SELECT id FROM sys_import_task WHERE file_name LIKE 'e2e13%' OR file_name LIKE 'E2E13%');
DELETE FROM sys_import_task
 WHERE file_name LIKE 'e2e13%' OR file_name LIKE 'E2E13%';

-- 2) 导出任务（产物文件留在磁盘上由 impexp 清理任务按过期回收，不动别人的）
DELETE FROM sys_export_task
 WHERE export_name LIKE 'E2E13%' OR file_name LIKE 'E2E13%';

-- 3) 字典项 / 字典类型
DELETE FROM sys_dict_item
 WHERE dict_type_id IN (SELECT id FROM sys_dict_type WHERE dict_code LIKE 'E2E13%');
DELETE FROM sys_dict_type WHERE dict_code LIKE 'E2E13%';

-- 4) 计量单位（S3/S4 部分导入落的）
DELETE FROM biz_unit WHERE unit_code LIKE 'E2E13%';

-- 5) 参数配置（含 public. 前缀的公共参数）
DELETE FROM sys_config WHERE config_key LIKE 'e2e13.%' OR config_key LIKE 'public.e2e13.%';

-- 6) 定时任务 + 执行日志
DELETE FROM sys_job_log WHERE job_name LIKE 'E2E13%';
DELETE FROM sys_job WHERE job_name LIKE 'E2E13%';

-- 7) 存储后端（文件登记统一在 sys_file，见第 9 步）
DELETE FROM sys_storage_config WHERE config_code LIKE 'e2e13%';

-- 8) 帮助中心（版本 → 文档 → 分类）
DELETE FROM sys_help_doc_version
 WHERE doc_id IN (SELECT id FROM sys_help_doc WHERE slug LIKE 'e2e13%');
DELETE FROM sys_help_doc WHERE slug LIKE 'e2e13%';
DELETE FROM sys_help_category WHERE code LIKE 'e2e13%';

-- 9) 文件登记（support/file 与 storage 上传共用 sys_file）
DELETE FROM sys_file WHERE original_name LIKE 'E2E13%' OR storage_code LIKE 'e2e13%';

-- 10) 演练账号与角色
DELETE FROM sys_user_role
 WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'e2e13\_%' OR username = 'e2e13_online')
    OR role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'E2E13%');
DELETE FROM sys_user WHERE username LIKE 'e2e13\_%' OR username = 'e2e13_online';
DELETE FROM sys_role_menu WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'E2E13%');
DELETE FROM sys_role WHERE role_code LIKE 'E2E13%';

COMMIT;
