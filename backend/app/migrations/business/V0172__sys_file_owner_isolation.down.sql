-- V0172 down：删掉 `sys_file.is_shared`（文件中心下载退回「全局共享」旧行为）
--
-- 说明：本列只承载读路径的授权标记，不属于业务数据；回滚后代码侧的
-- `can_read_file` 仍会读本列 —— 因此**回滚必须与代码一起回滚**（部署口径），
-- 单独跑 down 会让新二进制的下载接口 500。
--
-- 幂等：`DROP COLUMN IF EXISTS`。

ALTER TABLE sys_file DROP COLUMN IF EXISTS is_shared;
