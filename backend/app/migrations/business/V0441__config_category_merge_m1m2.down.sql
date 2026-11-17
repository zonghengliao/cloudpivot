-- ============================================================================
-- V0441__config_category_merge_m1m2.down（回滚 · 近似还原）
--
-- 【说明】
--   归并是信息有损的：无法判断某一项原本是 'erp' / 'erp_fx' / 'erp_inventory'。
--   回滚策略：
--     · 按 config_key 精确还原这 3 项（它们各自唯一，可以精确回填）
--     · 其余 erp_basic 项保持不动
-- ============================================================================

-- 按 config_key 还原 3 个原本独立的分类
UPDATE sys_config
SET category = 'erp', updated_at = now()
WHERE config_key = 'erp.stock_water.realert_days' AND deleted_at IS NULL;

UPDATE sys_config
SET category = 'erp_fx', updated_at = now()
WHERE config_key = 'erp.fx.gain_loss_account' AND deleted_at IS NULL;

UPDATE sys_config
SET category = 'erp_inventory', updated_at = now()
WHERE config_key = 'erp.inventory.slow-moving-days' AND deleted_at IS NULL;
