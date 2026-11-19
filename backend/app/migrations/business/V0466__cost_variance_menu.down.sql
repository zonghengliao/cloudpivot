-- ============================================================================
-- V0466 回滚：差异分析页菜单 + 授权（M5-19）
--
-- 顺序：先删本迁移授的授权行（按本页 menu_id）→ 再删本页行 → 最后回收祖先目录授权。
-- 护栏：本页下若出现**本迁移之外的子菜单**（有人挂了按钮 / 子页），拒绝删除 ——
--       那是别人的东西，不该替人连坐删掉。
-- 幂等：删不到行不报错（DELETE 语义），自检只在「形态不符」时抛异常。
--
-- 祖先目录（成本核算）的授权**只回收「因为本页才补上的」那一部分**：
--   判据 = 该角色在「成本核算」目录下除本页外**已没有任何其它已授菜单**（否则那行是别人的基础设施）。
--   「财务管理」这一代更是既有页面广泛依赖，**不动**（本迁移只可能给它补过 0~N 行，
--   而缺它的角色在回溯后仍会因别的成本页被同一段上溯逻辑补上 —— 由后续迁移负责，不在回滚里连坐删）。
--
-- 注意：本迁移只删菜单行，**不动** `business:erp:cost-method:list` 这个码本身
--       （它是 V0460 的，还被成本方法 / 标准成本两页与 M5-11 的报表接口用着）。
-- ============================================================================

DO $$
DECLARE
    v_page_children integer;
BEGIN
    SELECT count(*) INTO v_page_children
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND parent_id = '20000000-0000-4000-8000-00000000d401'::uuid;
    IF v_page_children > 0 THEN
        RAISE EXCEPTION 'V0466 回滚拒绝：差异分析页下还挂着 % 条子菜单（不是本迁移建的），先人工确认', v_page_children;
    END IF;
END $$;

-- 1) 本页的授权行
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-4000-8000-00000000d401'::uuid;

-- 2) 页面行
DELETE FROM sys_menu
 WHERE id = '20000000-0000-4000-8000-00000000d401'::uuid;

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
