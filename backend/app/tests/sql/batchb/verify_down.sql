\pset pager off
\echo '########## 回滚到 V0059 后的状态：以下每项都应为 0（不存在）##########'
SELECT
  (SELECT count(*) FROM information_schema.tables
     WHERE table_schema = current_schema()
       AND table_name IN ('sys_role_dept','sys_field_mask','sys_role_mutex'))           AS new_tables_left,
  (SELECT count(*) FROM information_schema.columns
     WHERE table_name = 'sys_role' AND column_name = 'data_scope')                      AS role_data_scope_left,
  (SELECT count(*) FROM pg_constraint
     WHERE conrelid = 'sys_role'::regclass AND conname = 'ck_sys_role_data_scope')      AS role_data_scope_check_left,
  (SELECT count(*) FROM information_schema.columns
     WHERE table_name = 'sys_export_task' AND column_name = 'mask_applied')             AS export_mask_applied_left,
  (SELECT count(*) FROM pg_indexes
     WHERE indexname IN ('uk_sys_field_mask_rule','uk_sys_role_mutex_pair',
                         'idx_sys_user_dept_created','idx_sys_import_task_create_by',
                         'idx_sys_export_task_create_by','idx_sys_role_mutex_b'))       AS batchb_indexes_left;

\echo '########## 迁移记录：V0060–V0064 / V0067 应一条不剩 ##########'
SELECT version, kind, name, success
FROM schema_migrations
WHERE kind = 'versioned' AND version IN ('60','61','62','63','64','67')
ORDER BY version;

\echo '########## 存量结构未被误伤：这些必须还在 ##########'
SELECT
  (SELECT count(*) FROM information_schema.tables
     WHERE table_schema = current_schema() AND table_name = 'sys_role')                 AS sys_role_exists,
  (SELECT count(*) FROM information_schema.tables
     WHERE table_schema = current_schema() AND table_name = 'sys_export_task')          AS sys_export_task_exists,
  (SELECT count(*) FROM information_schema.columns
     WHERE table_name = 'sys_export_task' AND column_name = 'heartbeat_at')             AS v0050_heartbeat_at_kept,
  (SELECT count(*) FROM information_schema.columns
     WHERE table_name = 'sys_role' AND column_name = 'role_code')                       AS sys_role_role_code_kept;
