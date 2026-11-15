-- V0040 回滚：删除对象存储配置与上传策略，撤销 sys_file 的两列扩展
--
-- 顺序说明：先删 sys_file 上的索引与约束，再删新增列（DROP COLUMN 会连带删掉
-- 该列上的索引，但显式删除更直观、也不依赖 PostgreSQL 的隐式行为），
-- 最后删两张新表（表上的索引随表一起消失）。
--
-- 注意：本回滚不恢复已写入的业务数据（软删列的物理回收、策略配置等），
-- 只保证结构可回到 V0039 的状态。

-- ---------- 3'. 撤销 sys_file 扩展 ----------
DROP INDEX IF EXISTS idx_sys_file_storage_code;
DROP INDEX IF EXISTS uk_sys_file_hash;
ALTER TABLE sys_file DROP CONSTRAINT IF EXISTS ck_sys_file_upload_mode;
ALTER TABLE sys_file DROP COLUMN IF EXISTS upload_mode;
ALTER TABLE sys_file DROP COLUMN IF EXISTS storage_code;

-- ---------- 2'. 上传策略 ----------
DROP INDEX IF EXISTS uk_sys_storage_policy_category;
DROP TABLE IF EXISTS sys_storage_policy;

-- ---------- 1'. 存储后端配置 ----------
DROP INDEX IF EXISTS idx_sys_storage_config_status;
DROP INDEX IF EXISTS uk_sys_storage_config_default;
DROP INDEX IF EXISTS uk_sys_storage_config_code;
DROP TABLE IF EXISTS sys_storage_config;
