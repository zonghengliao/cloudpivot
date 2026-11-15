-- ============================================================================
-- V0240__erp_budget_mapping_material_type.down.sql
--
-- 回滚 V0240：ck_biz_bsm_ref 回到「DEFAULT 免 ref_id / 其余必须 ref_id」，
-- uk_biz_bsm_target 回到 (map_type, ref_id, dept)。
--
-- ⚠ 回滚前提：表里不能有 MATERIAL_TYPE 行 —— 它们的 ref_id 恒为 NULL，
--   会被旧 ck 直接判为非法。这里先 RAISE 拦住，**不静默删用户的配置**。
--   人工处理方式（演练/开发环境）：
--     DELETE FROM biz_budget_subject_map WHERE map_type = 'MATERIAL_TYPE';
-- ============================================================================

DO $$
DECLARE
    mt bigint;
BEGIN
    SELECT count(*) INTO mt
      FROM biz_budget_subject_map
     WHERE map_type = 'MATERIAL_TYPE';
    IF mt > 0 THEN
        RAISE EXCEPTION
            'V0240 回滚中止：表里还有 % 条 MATERIAL_TYPE 映射（ref_id 为 NULL，旧约束不接受）。'
            '先备份并手工删除或改型后再回滚', mt;
    END IF;
END $$;

ALTER TABLE biz_budget_subject_map DROP CONSTRAINT IF EXISTS ck_biz_bsm_ref;
ALTER TABLE biz_budget_subject_map
    ADD CONSTRAINT ck_biz_bsm_ref CHECK (
        (map_type = 'DEFAULT' AND ref_id IS NULL)
     OR (map_type <> 'DEFAULT' AND ref_id IS NOT NULL)
    );

DROP INDEX IF EXISTS uk_biz_bsm_target;
CREATE UNIQUE INDEX uk_biz_bsm_target
    ON biz_budget_subject_map (
        map_type,
        ref_id,
        COALESCE(dept_id, '00000000-0000-0000-0000-000000000000'::uuid)
    )
    WHERE deleted_at IS NULL;

COMMENT ON COLUMN biz_budget_subject_map.ref_id IS NULL;
COMMENT ON COLUMN biz_budget_subject_map.ref_code IS NULL;
COMMENT ON INDEX uk_biz_bsm_target IS NULL;
