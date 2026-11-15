-- ============================================================
-- V0353 回滚：图标改回 shopping-cart（**会重新引入空白图标 bug**）+ 物料管理退回基础数据
--
-- 说明（为什么这不是"对称回滚"）：
--
--   ① 图标：V0353 改的是 prod（144）上 path='/business/erp/purchase' 那一行，
--      它迁移前的值就是 'shopping-cart' —— 而这个名字在
--      tdesign-icons-vue-next 里**不存在**，前端渲染为空白。
--      所以本 down 只是把数据库恢复到迁移前的字面值，**功能上是把 bug 装回去**：
--      仅在确认 V0353 本身有问题时才执行，执行后侧栏「采购订单」前面会重新变空白。
--      dev / 新库的 '/business/erp/purchase-order' 本来 icon 就是 'cart'（V0170 已改），
--      V0353 在那边改 0 行，因此这里也**不**把它改回 'shopping-cart' ——
--      回滚不该顺手把一个正确的库改坏。
--
--   ② 物料管理：退回 parent=/business/erp/master-data、sort_order=10（迁移前 dev
--      的实测值）。生产库迁移前的值同样是基础数据/sort 10。
-- ============================================================

-- §1 图标：只回滚 prod 那条别名路径（迁移前字面值）
UPDATE sys_menu
   SET icon = 'shopping-cart',
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business/erp/purchase'
   AND icon IS DISTINCT FROM 'shopping-cart';

-- §2 物料管理退回基础数据
UPDATE sys_menu
   SET parent_id = (SELECT id FROM sys_menu
                     WHERE path = '/business/erp/master-data'
                       AND deleted_at IS NULL),
       sort_order = 10,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business/erp/material'
   AND (parent_id IS DISTINCT FROM (SELECT id FROM sys_menu
                                     WHERE path = '/business/erp/master-data'
                                       AND deleted_at IS NULL)
        OR sort_order IS DISTINCT FROM 10);

DO $$
DECLARE
    v_mat_parent text;
BEGIN
    SELECT p.path INTO v_mat_parent
      FROM sys_menu m
      LEFT JOIN sys_menu p ON p.id = m.parent_id
     WHERE m.path = '/business/erp/material'
       AND m.deleted_at IS NULL;

    IF v_mat_parent IS DISTINCT FROM '/business/erp/master-data' THEN
        RAISE EXCEPTION 'V0353 回滚自检失败：物料管理父节点为 % ，预期 /business/erp/master-data', v_mat_parent;
    END IF;

    RAISE NOTICE 'V0353 回滚完成：图标已改回 shopping-cart（⚠ 前端会空白）、物料管理已退回基础数据';
END $$;
