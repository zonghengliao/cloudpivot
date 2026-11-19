-- ============================================================================
-- V0470 down：回滚在制品「工序完工率」表
--
-- # 为什么有数据就拒绝回滚
--   biz_cost_object_operation 的每一行都是「某批某道工序当时报了多少进度」的录入凭据；
--   而对象级 `biz_cost_object.completion_pct` 已经是它的加权汇总。表一删，对象级完工率
--   就变成**没有任何依据的孤立数字**（约当产量表还照算，界面上看不出是错的）。
--   所以：有活行就点名条数并拒绝执行；清掉数据后可重放（down 幂等）。
--
-- # 为什么护栏与 DROP 写在**同一个 DO 块**里
--   分两段写（DO 守卫 + 后面的 DROP TABLE）只在「整文件在一个事务里」时才安全。
--   把 DROP 放进 DO 块（`EXECUTE`）后：护栏 `RAISE EXCEPTION` 一响，DROP 那行根本
--   执行不到 —— 不管调用方是迁移器（有事务）还是人手 `psql -f`（无事务、不带 ON_ERROR_STOP）。
-- ============================================================================

DO $$
DECLARE
    v_n   BIGINT;
    v_has BOOLEAN;
BEGIN
    SELECT to_regclass('public.biz_cost_object_operation') IS NOT NULL INTO v_has;
    IF NOT v_has THEN
        RAISE NOTICE 'V0470 down：biz_cost_object_operation 已不存在（重复回滚），跳过护栏';
        RETURN;
    END IF;

    EXECUTE 'SELECT count(*) FROM biz_cost_object_operation WHERE deleted_at IS NULL' INTO v_n;
    IF v_n > 0 THEN
        RAISE EXCEPTION
            'V0470 down 拒绝回滚：biz_cost_object_operation 还有 % 行工序完工率（它们是对象级完工率的唯一依据）。'
            '先确认这些批次不再需要工序进度留档（或软删数据）后再执行回滚。', v_n;
    END IF;

    EXECUTE 'DROP TABLE IF EXISTS biz_cost_object_operation';

    IF to_regclass('public.biz_cost_object_operation') IS NOT NULL THEN
        RAISE EXCEPTION 'V0470 down 失败：biz_cost_object_operation 仍存在';
    END IF;
    RAISE NOTICE 'V0470 down 完成：biz_cost_object_operation 已删除';
END $$;
