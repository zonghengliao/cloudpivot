-- ============================================================================
-- V0441__config_category_merge_m1m2（参数分类再归并 · 2026-11-17 / laoliu）
--
-- 【为什么还要再整理一次】
--   V0405 已经把 ERP 分类从 5 类合并成 4 类（erp_basic / erp_budget /
--   erp_control / erp_cost），但那是**当时**的快照。M1/M2 批次的迁移随后
--   又各自造了新分类，页面立刻回到「一个 tab 只有一项、英文编码裸奔」的状态：
--
--     V0411（M1 汇兑）      → category = 'erp_fx'
--     V0426（M2 慢动销）    → category = 'erp_inventory'
--     V0429（M2 水位检查）  → category = 'erp'
--
--   三项都只有 1 条配置，字典 config_category 里也没有它们的中文名
--   ⇒ 参数配置页又冒出 3 个英文 tab。
--
-- 【本迁移做的事】
--   A. 把这 3 个分类统一归并到 `erp_basic`（与在途/提醒/主数据同类：
--      都是「先跑起来需要的默认参数」，不是独立业务域）
--   B. 清掉字典里可能残留的这 3 个编码（如果谁手工加过）
--   C. 自检：ERP 只允许 4 类；不允许空分类；每个分类必须有字典中文名
--
-- 【为什么是归并而不是新建字典】
--   「水位重提醒天数」「慢动销天数」「汇兑损益科目」三条都是**单点默认值**，
--   凑不出一个页面；给它们各建一个 tab 只会让页面更碎。真要有独立设置页，
--   那是产品需求（另开卡），不是靠 category 造 tab 能实现的。
--
-- 【幂等】
--   全部带 `WHERE category IN (...)` 守卫：
--     · 已归并过的环境 → 0 行
--     · 未归并的环境   → 3 行
-- ============================================================================

-- ---------------------------------------------------------------------------
-- A. 归并 3 个 M1/M2 新造的分类到 erp_basic
-- ---------------------------------------------------------------------------
UPDATE sys_config
SET category = 'erp_basic', updated_at = now()
WHERE category IN ('erp', 'erp_fx', 'erp_inventory')
  AND deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- B. 清掉字典里这 3 个编码（如果被手工加过）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_dict_id uuid;
BEGIN
    SELECT id INTO v_dict_id
    FROM sys_dict_type
    WHERE dict_code = 'config_category' AND deleted_at IS NULL
    LIMIT 1;

    IF v_dict_id IS NULL THEN
        RETURN;
    END IF;

    DELETE FROM sys_dict_item
    WHERE dict_type_id = v_dict_id
      AND item_value IN ('erp', 'erp_fx', 'erp_inventory');
END $$;

-- ---------------------------------------------------------------------------
-- C. 自检
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_dict_id   uuid;
    v_empty     int;
    v_erp_kinds int;
    v_no_label  int;
    v_bad       text;
BEGIN
    -- ① 不允许空分类
    SELECT count(*) INTO v_empty
    FROM sys_config
    WHERE deleted_at IS NULL AND (category IS NULL OR category = '');
    IF v_empty > 0 THEN
        RAISE EXCEPTION 'V0441 自检失败：仍有 % 条配置没有分类', v_empty;
    END IF;

    -- ② ERP 只允许 4 类
    SELECT count(DISTINCT category) INTO v_erp_kinds
    FROM sys_config
    WHERE deleted_at IS NULL AND config_key LIKE 'erp.%';
    IF v_erp_kinds > 4 THEN
        RAISE EXCEPTION 'V0441 自检失败：ERP 仍有 % 个分类（期望 ≤4：erp_basic/erp_budget/erp_control/erp_cost）', v_erp_kinds;
    END IF;

    -- ③ 每个分类都必须在字典里有中文名
    SELECT id INTO v_dict_id
    FROM sys_dict_type WHERE dict_code = 'config_category' AND deleted_at IS NULL LIMIT 1;

    IF v_dict_id IS NOT NULL THEN
        SELECT count(*) INTO v_no_label
        FROM (SELECT DISTINCT category AS cat FROM sys_config WHERE deleted_at IS NULL) c
        WHERE NOT EXISTS (
            SELECT 1 FROM sys_dict_item i
            WHERE i.dict_type_id = v_dict_id AND i.item_value = c.cat
        );
        IF v_no_label > 0 THEN
            SELECT string_agg(c.cat, ', ') INTO v_bad
            FROM (SELECT DISTINCT category AS cat FROM sys_config WHERE deleted_at IS NULL) c
            WHERE NOT EXISTS (
                SELECT 1 FROM sys_dict_item i
                WHERE i.dict_type_id = v_dict_id AND i.item_value = c.cat
            );
            RAISE EXCEPTION 'V0441 自检失败：以下分类缺字典中文名 → %', v_bad;
        END IF;
    END IF;

    RAISE NOTICE 'V0441 自检通过：无空分类、ERP % 类、字典全覆盖', v_erp_kinds;
END $$;
