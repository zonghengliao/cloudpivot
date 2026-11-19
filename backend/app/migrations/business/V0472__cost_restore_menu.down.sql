-- ============================================================================
-- V0472 回滚：成本还原页菜单 + 授权（M5-30）
--
-- 顺序：先删本迁移授的授权行（按本页 menu_id）→ 再删本页行 → 最后回收祖先目录授权。
-- 护栏：本页下若出现**本迁移之外的子菜单**（有人挂了按钮 / 子页），拒绝删除 ——
--       那是别人的东西，不该替人连坐删掉。
-- 幂等：删不到行不报错（DELETE 语义），自检只在「形态不符」时抛异常。
--
-- 祖先目录（成本核算）的授权**只回收「因为本页才补上的」那一部分**：
--   判据 = 该角色在「成本核算」目录下除本页外**已没有任何其它已授菜单**
--   （否则那行是别人的基础设施：成本对象 / 工时台账 / 费用台账 / 分摊规则 / 成本核算 /
--     批次成本 / 标准成本 / 差异分析 / 在制品九页都靠它）。
--
-- 注意：本迁移只删菜单行，**不动** `business:erp:cost-object:list` 这个码本身
--       （它是 V0378 的，还被成本对象 / 批次成本 / 标准成本 / 差异分析 / 在制品等页
--       与成本还原的两条接口用着）。
-- ============================================================================

DO $$
DECLARE
    v_page_children integer;
BEGIN
    SELECT count(*) INTO v_page_children
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND parent_id = '20000000-0000-4000-8000-00000000d601'::uuid;
    IF v_page_children > 0 THEN
        RAISE EXCEPTION 'V0472 回滚拒绝：成本还原页下还挂着 % 条子菜单（不是本迁移建的），先人工确认', v_page_children;
    END IF;
END $$;

-- 1) 本页的授权行
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-4000-8000-00000000d601'::uuid;

-- 2) 页面行
DELETE FROM sys_menu
 WHERE id = '20000000-0000-4000-8000-00000000d601'::uuid;

-- 3) 祖先目录授权回收（只删「这个角色在该目录下已经一无所有」的那些行）
DELETE FROM sys_role_menu rm
 WHERE rm.menu_id = (SELECT id FROM sys_menu
                      WHERE deleted_at IS NULL AND menu_type = 1
                        AND path = '/business/erp/cost' LIMIT 1)
   AND NOT EXISTS (
        SELECT 1
          FROM sys_role_menu x
          JOIN sys_menu m ON m.id = x.menu_id AND m.deleted_at IS NULL
         WHERE x.role_id = rm.role_id
           AND m.path LIKE '/business/erp/cost/%'
   );
