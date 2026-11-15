-- ============================================================================
-- V0397__hr_salary_period_after_reverse.down.sql
--
-- 回滚：把 `uk_biz_sal_sheet_period` 的谓词还原成 V0383 的 `deleted_at IS NULL`
-- （即「同期间只能有一张薪资单」，红冲单也占位）。
--
-- ── 有业务数据就拒绝回滚（照 V0347 的既有做法）──────────────────────────
--   本迁移之后，客户很可能已经「红冲 → 同期间重开一张」：
--   那期间就会同时存在 1 张 REVERSED + 1 张有效单。恢复严格唯一键会
--   `CREATE UNIQUE INDEX` 直接失败（PG 报 duplicate key，指不到业务含义），
--   所以先自查并给出**可操作的**报文：要么先把多的那几张处理掉（软删 / 改期间，
--   留档在 `biz_status_log`），要么别回滚。
--
-- ── 幂等 ────────────────────────────────────────────────────────────────
--   `DROP INDEX IF EXISTS` + `CREATE UNIQUE INDEX IF NOT EXISTS`；
--   重复执行等价于一次。没有数据改动（本迁移只换了索引谓词）。
-- ============================================================================

DO $$
DECLARE
    v_dup integer;
    v_cnt integer;
BEGIN
    IF to_regclass('biz_salary_sheet') IS NULL THEN
        RAISE NOTICE 'V0397.down：biz_salary_sheet 不存在（V0383 已被回滚？），无事可做';
        RETURN;
    END IF;

    SELECT count(*) INTO v_dup
      FROM (
            SELECT org_id, period_code
              FROM biz_salary_sheet
             WHERE deleted_at IS NULL
             GROUP BY org_id, period_code
            HAVING count(*) > 1
           ) t;
    IF v_dup > 0 THEN
        RAISE EXCEPTION
            'V0397.down 拒绝回滚：有 % 个「公司 + 期间」存在多张未删除的薪资单（红冲后重开的那几张）。'
            '恢复严格唯一键会直接失败。请先处理重复（软删旧单或改其期间，操作留档 biz_status_log），'
            '再重跑本回滚脚本', v_dup;
    END IF;

    DROP INDEX IF EXISTS uk_biz_sal_sheet_period;
    CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sal_sheet_period
        ON biz_salary_sheet (org_id, period_code)
     WHERE deleted_at IS NULL;

    SELECT count(*) INTO v_cnt FROM pg_indexes WHERE indexname = 'uk_biz_sal_sheet_period';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0397.down 失败：uk_biz_sal_sheet_period 恢复后没有恰好 1 条（实际 %）', v_cnt;
    END IF;

    RAISE NOTICE 'V0397.down 完成：uk_biz_sal_sheet_period 已还原为 V0383 的谓词（deleted_at IS NULL）';
END $$;
