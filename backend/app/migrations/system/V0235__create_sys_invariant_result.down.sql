-- 回滚 V0235：先摘掉 sys_job 的可见行，再删结果表
DELETE FROM sys_job WHERE job_name = 'invariant_audit';
DROP TABLE IF EXISTS sys_invariant_result;
