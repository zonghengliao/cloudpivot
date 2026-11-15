-- 回滚 V0104：删掉「我参与」id 集合的创建人分支索引
DROP INDEX IF EXISTS idx_biz_task_creator;
