-- ============================================================================
-- V0442__perm_gate_orphans_and_n20.down（FIX-权限闸 回滚）
--
-- 只回退本迁移做过的事，一行都不多删：
--   ① 按**确定性 id** 删本迁移插入的授权行：
--        · grant 行   id = md5('v0442.'  || role_id || ':' || menu_id)
--        · closure 行 id = md5('v0442c.' || role_id || ':' || dir_id)
--      角色侧**不按名单枚举**（up 的 A1/A2 两段是数据驱动的「谁持有按钮码就给谁补父页码」，
--      名单式回滚会漏角色）：枚举「全部角色 × 本迁移涉及的菜单行」，用同一套 id 公式重算，
--      只删真有这些 id 的行 —— 迁移前就存在的授权 id 不同（随机 uuid / 别的公式），天然不动。
--   ② 把 `business:notice:mine` 的按钮节点 …414 放回「消息通知」页（…410，原 sort_order = 40）。
--
-- 幂等：再跑一遍 `DELETE 0` + `UPDATE 0`。
-- 反向影响：**不是关功能** —— 菜单行、权限码、接口全部还在；回滚后回到迁移前的形态：
--   · 4 对孤儿按钮重新出现在 orphan_button_gate（监控/验收角色 × notice:mine、
--     财务总监 × receivable:aging、验收角色 × role-mutex:conflict）；
--   · N20 六个报价/返利码回到「只授给超管或谁都没授」（除超管外全员 403）；
--   · 因本迁移补授的三个页面入口（应收台账 / 角色互斥）从相应角色侧栏消失（码还在，超管不受影响）。
-- ============================================================================

DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.sys_role_menu') IS NULL OR to_regclass('public.sys_menu') IS NULL THEN
        RAISE NOTICE 'V0442 down：sys_menu / sys_role_menu 不存在，无事可做';
        RETURN;
    END IF;

    -- ① grant 行：全部角色 × 本迁移涉及的 8 个码的承载节点（id 公式命中即本迁移插的）
    DELETE FROM sys_role_menu
     WHERE id IN (
            SELECT md5('v0442.' || r.id || ':' || m.id)::uuid
              FROM sys_role r
             CROSS JOIN sys_menu m
             WHERE m.deleted_at IS NULL
               AND m.auth_code IN (
                    'business:erp:receivable:list',
                    'system:role-mutex:list',
                    'business:erp:quotation:list',
                    'business:erp:quotation:create',
                    'business:erp:quotation:status',
                    'business:erp:rebate-policy:list',
                    'business:erp:rebate-policy:create',
                    'business:erp:rebate-settlement:list'
               )
       );
    GET DIAGNOSTICS n = ROW_COUNT;
    RAISE NOTICE 'V0442 down：删除 grant 授权行 % 行', n;

    -- ② 祖先闭包行：全部角色 × 本迁移涉及的两个目录（系统管理 / 销售管理）
    DELETE FROM sys_role_menu
     WHERE id IN (
            SELECT md5('v0442c.' || r.id || ':' || d.dir_id)::uuid
              FROM sys_role r
             CROSS JOIN (VALUES
                            ('10000000-0000-0000-0000-000000000001'::uuid),
                            ('20000000-0000-0000-0000-000000002701'::uuid)
                        ) AS d(dir_id)
       );
    GET DIAGNOSTICS n = ROW_COUNT;
    RAISE NOTICE 'V0442 down：删除祖先闭包授权行 % 行', n;
END $$;

-- ③ 父页归位回退：…414「我的通知 / 未读数 / 已读」放回「消息通知」页（…410，原 sort_order = 40）
UPDATE sys_menu m
   SET parent_id = '20000000-0000-0000-0000-000000000410'::uuid,
       sort_order = 40,
       updated_at = now()
 WHERE m.id = '20000000-0000-0000-0000-000000000414'::uuid
   AND m.parent_id = '23000000-0000-0000-0000-000000000010'::uuid;
