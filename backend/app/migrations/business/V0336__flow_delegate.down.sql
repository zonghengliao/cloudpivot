-- ======================================================
-- V0336 down：回滚审批委托（P3-04）
--
-- 顺序：菜单授权 → 菜单 → 排除约束（随表删）→ 索引 → 表。
-- 说明：
--   · `biz_flow_delegate` 只有**配置**（没有业务数据挂在上面：待办仍是 `biz_flow_task` 行，
--     派单规则写在 `assigned_by_rule` / `comment`），所以删表不会丢任何审批结论。
--   · 回滚后**已经派出去的委托待办不受影响**（解析期已冻结成行），只是新单不再替换成受托人。
--   · `btree_gist` 扩展**不删**：它可能被别的对象（未来其它区间约束）用着，
--     且扩展是库级资源，不该由单张表的迁移持有。
-- ======================================================

-- 授权行（role_menu 有 FK 指向 sys_menu，必须先删）
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001240'::uuid,
                   '20000000-0000-0000-0000-000000001241'::uuid,
                   '20000000-0000-0000-0000-000000001242'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001240'::uuid,
              '20000000-0000-0000-0000-000000001241'::uuid,
              '20000000-0000-0000-0000-000000001242'::uuid);

ALTER TABLE biz_flow_delegate DROP CONSTRAINT IF EXISTS ex_biz_flow_delegate_no_overlap;
DROP INDEX IF EXISTS idx_biz_flow_delegate_delegate;
DROP INDEX IF EXISTS idx_biz_flow_delegate_active;
DROP TABLE IF EXISTS biz_flow_delegate;
