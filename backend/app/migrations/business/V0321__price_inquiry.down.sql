-- V0321 回滚：询比价（P2-01）
-- 顺序：先撤授权 → 再删按钮与页面节点 → 再删取号规则 → 最后删表（子表先删）。
BEGIN;

-- ① 授权
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000003321'::uuid,
                   '20000000-0000-0000-0000-000000003322'::uuid,
                   '20000000-0000-0000-0000-000000003323'::uuid,
                   '20000000-0000-0000-0000-000000003324'::uuid,
                   '20000000-0000-0000-0000-000000003325'::uuid);

-- ② 菜单（按钮先删，再删页面节点）
DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000003322'::uuid,
              '20000000-0000-0000-0000-000000003323'::uuid,
              '20000000-0000-0000-0000-000000003324'::uuid,
              '20000000-0000-0000-0000-000000003325'::uuid);
DELETE FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000003321'::uuid;

-- ③ 取号规则
DELETE FROM biz_seq WHERE seq_type = 'PIQ';

-- ④ 表（子表先删）
DROP TABLE IF EXISTS biz_price_quote;
DROP TABLE IF EXISTS biz_price_inquiry_item;
DROP TABLE IF EXISTS biz_price_inquiry;

COMMIT;
