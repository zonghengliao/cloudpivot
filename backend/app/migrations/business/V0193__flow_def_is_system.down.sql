-- 回滚 V0193：删掉 is_system 列。
-- category 归一不回退（见 up 文件头：回退成 erp/task 内部码没有任何业务收益，
-- 且会让模板库分组重新出现给用户看的分组名）。
ALTER TABLE biz_flow_def DROP COLUMN IF EXISTS is_system;
