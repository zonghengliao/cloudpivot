-- ============================================================
-- V0323 回滚：删掉本迁移种的 3 条 FIXED_ASSET 联动规则
--          （只删这 3 个键，不碰运维后加 / 改过的其它规则）
--
-- 说明：`biz_flow_linkage_rule` 的「删除」在应用里是软删（`deleted_at`）；
--   迁移回滚是**结构/数据复位**，这里直接物理删这 3 行 —— 与 V0205 的初始态
--   （空表 = 引擎不接管任何联动）对齐，回滚后资产状态变更回到「一条通知都不发」。
--   运维如果改过这 3 条的参数，回滚仍然删掉它们（迁移回滚的语义是
--   「这批预置数据不存在」，不是「恢复到我改之前」）。
-- ============================================================

DELETE FROM biz_flow_linkage_rule
 WHERE trigger_entity = 'FIXED_ASSET'
   AND (trigger_entity, trigger_status, action_code) IN (
       ('FIXED_ASSET', 'IDLE', 'SEND_NOTICE'),
       ('FIXED_ASSET', 'IN_USE', 'SEND_NOTICE'),
       ('FIXED_ASSET', 'DISPOSED', 'SEND_NOTICE')
   );

DO $$
DECLARE
    n_rows integer;
BEGIN
    SELECT count(*) INTO n_rows FROM biz_flow_linkage_rule
     WHERE trigger_entity = 'FIXED_ASSET'
       AND (trigger_entity, trigger_status, action_code) IN (
           ('FIXED_ASSET', 'IDLE', 'SEND_NOTICE'),
           ('FIXED_ASSET', 'IN_USE', 'SEND_NOTICE'),
           ('FIXED_ASSET', 'DISPOSED', 'SEND_NOTICE')
       );
    IF n_rows <> 0 THEN
        RAISE EXCEPTION 'V0323 回滚自检失败：预置规则仍有 % 条未删除', n_rows;
    END IF;
    RAISE NOTICE 'V0323 回滚完成：3 条 FIXED_ASSET 联动规则已删除（资产状态变更回到「不发通知、也不报错」）';
END $$;
