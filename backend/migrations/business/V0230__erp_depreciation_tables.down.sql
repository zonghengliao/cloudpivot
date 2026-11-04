-- ======================================================
-- V0230 down：有折旧批次就拒绝回滚（批次 = 已计提折旧，删了就是丢账）
-- ======================================================
DO $$
DECLARE
    n int;
BEGIN
    SELECT count(*) INTO n FROM biz_depreciation_batch;
    IF n > 0 THEN
        RAISE EXCEPTION 'V0230 回滚被拒绝：库里还有 % 条折旧批次（含已红冲），折旧是账务事实，不能随表一起删', n;
    END IF;
END $$;

DELETE FROM sys_role_menu WHERE menu_id IN (
    '20000000-0000-0000-0000-000000001403'::uuid,
    '20000000-0000-0000-0000-000000001431'::uuid,
    '20000000-0000-0000-0000-000000001432'::uuid
);
DELETE FROM sys_menu WHERE id IN (
    '20000000-0000-0000-0000-000000001403'::uuid,
    '20000000-0000-0000-0000-000000001431'::uuid,
    '20000000-0000-0000-0000-000000001432'::uuid
);
DELETE FROM sys_job WHERE job_name = 'erp_depreciation_run';

DROP TABLE IF EXISTS biz_depreciation;
DROP TABLE IF EXISTS biz_depreciation_batch;
