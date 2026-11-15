\pset pager off
\echo '################ 1. sys_role：data_scope 列 + CHECK 约束 ################'
SELECT column_name, data_type, is_nullable, column_default
FROM information_schema.columns
WHERE table_name = 'sys_role' AND column_name = 'data_scope';
SELECT conname, pg_get_constraintdef(oid) AS definition
FROM pg_constraint
WHERE conrelid = 'sys_role'::regclass AND conname = 'ck_sys_role_data_scope';
\echo '--- 默认值验证：V0060 之前已存在的角色，data_scope 必须是 1（全部），不得有 NULL ---'
SELECT data_scope, count(*) AS roles FROM sys_role GROUP BY 1 ORDER BY 1;
SELECT count(*) AS null_or_out_of_range FROM sys_role WHERE data_scope IS NULL OR data_scope NOT IN (1,2,3,4,5);

\echo '################ 2. sys_field_mask：唯一索引 uk_sys_field_mask_rule ################'
SELECT indexname, indexdef FROM pg_indexes
WHERE tablename = 'sys_field_mask' ORDER BY indexname;
SELECT conname, pg_get_constraintdef(oid) AS definition
FROM pg_constraint
WHERE conrelid = 'sys_field_mask'::regclass AND contype = 'c'
ORDER BY conname;

\echo '################ 3. sys_role_mutex：唯一索引 uk_sys_role_mutex_pair ################'
SELECT indexname, indexdef FROM pg_indexes
WHERE tablename = 'sys_role_mutex' ORDER BY indexname;
SELECT conname, pg_get_constraintdef(oid) AS definition
FROM pg_constraint
WHERE conrelid = 'sys_role_mutex'::regclass AND contype = 'c'
ORDER BY conname;

\echo '################ 4. sys_export_task：mask_applied 列 ################'
SELECT column_name, data_type, is_nullable, column_default
FROM information_schema.columns
WHERE table_name = 'sys_export_task' AND column_name = 'mask_applied';

\echo '################ 5. sys_role_dept：表 + 2 索引 + 唯一约束 ################'
SELECT indexname, indexdef FROM pg_indexes
WHERE tablename = 'sys_role_dept' ORDER BY indexname;

\echo '################ 6. V0064 的三个索引（+ V0063 的 idx_sys_role_mutex_b）################'
SELECT tablename, indexname FROM pg_indexes
WHERE indexname IN ('idx_sys_user_dept_created','idx_sys_import_task_create_by',
                    'idx_sys_export_task_create_by','idx_sys_role_mutex_b')
ORDER BY indexname;

\echo '################ 7. 迁移记录：V0060–V0064 + V0067 全部 success = t ################'
SELECT version, kind, name, success
FROM schema_migrations
WHERE kind = 'versioned' AND version IN ('60','61','62','63','64','67')
ORDER BY version;
