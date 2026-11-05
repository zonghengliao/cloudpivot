-- ============================================================
-- V0254 回滚：删掉 3 条预置联动规则（只删本迁移种的那 3 个键，不碰运维后加的规则）
--
-- 说明：`biz_flow_linkage_rule` 的「删除」在应用里是软删（`deleted_at`）；
--   迁移回滚是**结构/数据复位**，这里直接物理删这 3 行 —— 与 V0205 的初始态
--   （空表 = 引擎不接管任何联动）对齐，回滚后业务行为与「从未配过规则」逐字相同。
--   如果运维已经把这 3 条改成别的参数，回滚仍然删掉它们（迁移回滚的语义是
--   「这批预置数据不存在」，不是「恢复到我改之前」）。
-- ============================================================

DELETE FROM biz_flow_linkage_rule
 WHERE (trigger_entity, trigger_status, action_code) IN (
     ('STOCK_IN', 'POSTED', 'GENERATE_VOUCHER'),
     ('STOCK_IN', 'POSTED', 'SEND_NOTICE'),
     ('TASK',     'DOING', 'SEND_NOTICE')
   );

DO $$
DECLARE
    n_rows integer;
BEGIN
    SELECT count(*) INTO n_rows FROM biz_flow_linkage_rule
     WHERE (trigger_entity, trigger_status, action_code) IN (
         ('STOCK_IN', 'POSTED', 'GENERATE_VOUCHER'),
         ('STOCK_IN', 'POSTED', 'SEND_NOTICE'),
         ('TASK',     'DOING', 'SEND_NOTICE')
     );
    IF n_rows <> 0 THEN
        RAISE EXCEPTION 'V0254 回滚自检失败：预置规则仍有 % 条未删除', n_rows;
    END IF;
    RAISE NOTICE 'V0254 回滚完成：3 条预置联动规则已删除（引擎回到「读不到规则 = 不接管」）';
END $$;
