-- ============================================================================
-- V0461 down：回滚分批法「完工批次结转」表
--
-- # 为什么有数据就拒绝回滚
--   biz_cost_batch_finish 的每一行都是「这批货转出了多少钱」的唯一凭据。表一删，
--   M5-05 的在制余额（累计归集 − 已结转）会**静默变成全额在制**，界面上看不出是错的。
--   所以：有活行就点名条数并拒绝执行；清掉数据后可重放（down 幂等）。
--
-- # 为什么护栏与 DROP 写在**同一个 DO 块**里
--   分两段写（DO 守卫 + 后面的 DROP TABLE）只在「整文件在一个事务里」时才安全。
--   直接把 DROP 放进 DO 块（`EXECUTE`）后：护栏 `RAISE EXCEPTION` 一响，DROP 那行
--   根本执行不到 —— 不管调用方是迁移器（有事务）还是人手 `psql -f`（无事务、
--   不带 ON_ERROR_STOP）。反证见卡内证据：造 1 行数据后跑 down，表**还在**。
-- ============================================================================

DO $$
DECLARE
    v_n     BIGINT;
    v_has   BOOLEAN;
BEGIN
    SELECT to_regclass('public.biz_cost_batch_finish') IS NOT NULL INTO v_has;
    IF NOT v_has THEN
        RAISE NOTICE 'V0461 down：biz_cost_batch_finish 已不存在（重复回滚），跳过护栏';
        RETURN;
    END IF;

    EXECUTE 'SELECT count(*) FROM biz_cost_batch_finish WHERE deleted_at IS NULL' INTO v_n;
    IF v_n > 0 THEN
        RAISE EXCEPTION
            'V0461 down 拒绝回滚：biz_cost_batch_finish 还有 % 行完工结转记录（它们是「这批货转出了多少钱」的唯一凭据）。'
            '先确认这些批次不需要保留结转历史（或软删 / 迁走数据）后再执行回滚。', v_n;
    END IF;

    EXECUTE 'DROP TABLE IF EXISTS biz_cost_batch_finish';

    IF to_regclass('public.biz_cost_batch_finish') IS NOT NULL THEN
        RAISE EXCEPTION 'V0461 down 失败：biz_cost_batch_finish 仍存在';
    END IF;
    RAISE NOTICE 'V0461 down 完成：biz_cost_batch_finish 已删除';
END $$;
