-- ============================================================================
-- V0396 回滚：删掉 17 个薪资项种子（ARCH-9-2 §7.3「有业务数据就拒绝回滚」）
--
-- 卡：t_1c4a7b35（HR-35【迁移 V0387】）
--
-- # 三条拒绝条件（都是「删了会留下算不出来的账」）
--   ① `biz_employee_salary` 有未删行引用这 17 个编码
--      ⇒ 员工薪资标准变成「指向不存在的项」，下次试算静默少几项（`item_code` 是弱引用、不建 FK，
--        删数据不会报错，只会少算钱）；
--   ② `biz_salary_detail_item` 有行引用这些编码
--      ⇒ 历史工资单的明细项快照失去定义（`item_id` 是 `ON DELETE SET NULL`，删了只剩编码快照，
--        「这一项当时按什么公式算的」就查不到了）；
--   ③ `biz_salary_sheet` 里有 `POSTED` / `REVERSED` 单
--      ⇒ 工资凭证已经落账，回滚种子等于把账上的科目口径抽掉（与 `V0347.down.sql` 同口径）。
--
-- # 只删「本迁移该负责的行」
--   `org_id = 全零哨兵 AND item_code IN (17) AND deleted_at IS NULL`。
--   客户自己建的同编码行（多组织落地后不同 org）不动；已软删的行也不动（本来就是软删状态）。
--
-- # 幂等
--   第二遍：三条拒绝条件仍会先检查（若已清干净就不拦），DELETE 命中 0 行，末尾自检通过。
-- ============================================================================
DO $$
DECLARE
    v_org      CONSTANT uuid := '00000000-0000-0000-0000-000000000000';
    v_codes    CONSTANT text[] := ARRAY['BASE','POST','MEAL','PERF','OT_WD','OT_WE','OT_HO','ATT_BONUS',
                                        'ATT_DEDUCT','SI_PERSONAL','HF_PERSONAL','IIT','OTHER_WITHHOLD',
                                        'SI_COMPANY','HF_COMPANY','GROSS','NET'];
    v_n        BIGINT;
    v_bad      TEXT;
BEGIN
    IF to_regclass('public.biz_salary_item') IS NULL THEN
        RAISE NOTICE 'V0396 回滚：biz_salary_item 不存在（表已被 V0383.down 删掉？）—— 无事可做';
        RETURN;
    END IF;

    -- ① 员工薪资标准引用
    IF to_regclass('public.biz_employee_salary') IS NOT NULL THEN
        SELECT count(*) INTO v_n
          FROM biz_employee_salary
         WHERE deleted_at IS NULL AND item_code = ANY (v_codes);
        IF v_n > 0 THEN
            RAISE EXCEPTION 'V0396 拒绝回滚：biz_employee_salary 有 % 行未删记录引用这 17 个薪资项编码。'
                '删掉种子后这些薪资标准会指向不存在的项（弱引用，删数据不报错、下次试算静默少算钱）。'
                '请先在「员工档案-薪资标准」里清理这些项（或改为客户自建项），再回滚', v_n;
        END IF;
    END IF;

    -- ② 历史工资单明细项引用
    IF to_regclass('public.biz_salary_detail_item') IS NOT NULL THEN
        SELECT count(*) INTO v_n
          FROM biz_salary_detail_item
         WHERE item_code = ANY (v_codes);
        IF v_n > 0 THEN
            RAISE EXCEPTION 'V0396 拒绝回滚：biz_salary_detail_item 有 % 行历史明细引用了这些薪资项。'
                '删掉种子后历史工资单只剩编码快照、失去项定义（公式解释层查不到）。'
                '请先确认这些工资单可以作废/删除，再回滚', v_n;
        END IF;
    END IF;

    -- ③ 已过账 / 已红冲的薪资单
    IF to_regclass('public.biz_salary_sheet') IS NOT NULL THEN
        SELECT string_agg(format('%s(%s)', s.sheet_no, s.status), ' / ') INTO v_bad
          FROM biz_salary_sheet s
         WHERE s.deleted_at IS NULL AND s.status IN ('POSTED', 'REVERSED');
        IF v_bad IS NOT NULL THEN
            RAISE EXCEPTION 'V0396 拒绝回滚：存在已过账/已红冲的薪资单 —— %。'
                '工资凭证已经落账，回滚薪资项会让账上的科目口径抽掉。'
                '请先红冲/作废这些薪资单（走业务流程），再回滚', v_bad;
        END IF;
    END IF;

    -- 删种子（只删哨兵组织下、未软删的这 17 个编码）
    DELETE FROM biz_salary_item
     WHERE org_id = v_org
       AND deleted_at IS NULL
       AND item_code = ANY (v_codes);

    -- 自检：这 17 个编码在哨兵组织下不该再有未删行
    SELECT count(*) INTO v_n
      FROM biz_salary_item
     WHERE org_id = v_org AND deleted_at IS NULL AND item_code = ANY (v_codes);
    IF v_n > 0 THEN
        RAISE EXCEPTION 'V0396 回滚失败：仍有 % 行薪资项未删（应为 0）', v_n;
    END IF;

    RAISE NOTICE 'V0396 回滚完成：17 个薪资项种子已删除（组织哨兵 %）', v_org;
END
$$;
