-- SO-38 回滚：删掉额度台账 / 索引 / 已用列 / 任务种子行。
-- 顺序与前向相反；任务行先删（它引用的处理器随代码回滚一起消失）。
DELETE FROM sys_job WHERE job_name = 'erp_credit_recompute';

DROP INDEX IF EXISTS idx_biz_credit_log_customer;
DROP TABLE IF EXISTS biz_credit_log;

DROP INDEX IF EXISTS idx_biz_customer_credit_used;

ALTER TABLE biz_customer DROP CONSTRAINT IF EXISTS ck_biz_customer_credit_used;
ALTER TABLE biz_customer DROP COLUMN IF EXISTS credit_used;
