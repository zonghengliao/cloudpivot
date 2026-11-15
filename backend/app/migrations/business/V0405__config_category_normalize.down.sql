-- ============================================================================
-- V0405__config_category_normalize.down（回滚 · 近似还原）
--
-- 【说明：为什么是「近似」】
--   本迁移把 `erp` 与 `erp_master_data` **合并**成 `erp_basic`，这个合并是**信息有损**的
--   —— 回滚时无法判断某一行原本属于 `erp` 还是 `erp_master_data`。
--   同理 `erp.cost.enabled` 与 `auth.idle_timeout_minutes` 的原始 description 是空串，
--   回滚会清回空串（可接受，因为原值就是空）。
--
--   因此 down 的策略是：
--     · `erp_basic`   → 全部还原为 `erp`（erp_master_data 的那一项留在 erp，不影响业务，
--                       因为 category 只影响页面分组）
--     · `erp_control` → 还原为 `erp_internal_control`
--     · 删掉本迁移新增的字典项，恢复被删的旧项
--     · 清空两项的 description，category 归空
--
--   如果将来真的需要精确回滚，请从迁移前的库备份恢复（deploy 脚本每次都会 dump）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 分类还原
-- ---------------------------------------------------------------------------
UPDATE sys_config
SET category = 'erp', updated_at = now()
WHERE category = 'erp_basic' AND deleted_at IS NULL;

UPDATE sys_config
SET category = 'erp_internal_control', updated_at = now()
WHERE category = 'erp_control' AND deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- ② 两项游离项回到原状（category 空、description 空）
-- ---------------------------------------------------------------------------
UPDATE sys_config
SET category = '', description = '', updated_at = now()
WHERE config_key IN ('erp.cost.enabled', 'auth.idle_timeout_minutes')
  AND deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- ③ 字典还原：删掉新增的，补回被删的旧编码
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

    -- 删掉本迁移新增的分类翻译
    DELETE FROM sys_dict_item
    WHERE dict_type_id = v_dict_id
      AND item_value IN ('erp_basic', 'erp_budget', 'erp_control', 'announcement', 'hr', 'changelog');

    -- 补回原本存在、被本迁移删掉的旧编码
    IF NOT EXISTS (SELECT 1 FROM sys_dict_item WHERE dict_type_id = v_dict_id AND item_value = 'erp_internal_control') THEN
        INSERT INTO sys_dict_item (id, dict_type_id, item_value, item_label, sort_order, status, created_at, updated_at)
        VALUES (gen_random_uuid(), v_dict_id, 'erp_internal_control', 'ERP 内控', 30, 1, now(), now());
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_dict_item WHERE dict_type_id = v_dict_id AND item_value = 'erp_master_data') THEN
        INSERT INTO sys_dict_item (id, dict_type_id, item_value, item_label, sort_order, status, created_at, updated_at)
        VALUES (gen_random_uuid(), v_dict_id, 'erp_master_data', 'ERP 主数据', 40, 1, now(), now());
    END IF;
END $$;
