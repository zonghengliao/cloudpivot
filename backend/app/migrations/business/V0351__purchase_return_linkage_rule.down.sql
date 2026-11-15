-- ============================================================
-- V0351 回滚：删掉本迁移种的 1 条 PURCHASE_RETURN 联动规则
--          （只删这一个键，不碰运维后加 / 改过的其它规则）
--
-- 说明：`biz_flow_linkage_rule` 的「删除」在应用里是软删（`deleted_at`）；
--   迁移回滚是**数据复位**，这里直接物理删这一行 —— 回滚后退货确认回到
--   「不发明知会通知、也不报错」（账照落，凭证由专用路径出，不受影响）。
-- ============================================================

DELETE FROM biz_flow_linkage_rule
 WHERE (trigger_entity, trigger_status, action_code) IN (
     ('PURCHASE_RETURN', 'CONFIRMED', 'SEND_NOTICE')
 );

DO $$
DECLARE
    n_rows integer;
BEGIN
    SELECT count(*) INTO n_rows FROM biz_flow_linkage_rule
     WHERE (trigger_entity, trigger_status, action_code) IN (
         ('PURCHASE_RETURN', 'CONFIRMED', 'SEND_NOTICE')
     );
    IF n_rows <> 0 THEN
        RAISE EXCEPTION 'V0351 回滚自检失败：预置规则仍有 % 条未删除', n_rows;
    END IF;
    RAISE NOTICE 'V0351 回滚完成：1 条 PURCHASE_RETURN 联动规则已删除（退货确认回到「不发通知」）';
END $$;
