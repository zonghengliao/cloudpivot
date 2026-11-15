-- ============================================================================
-- V0319 down：把菜单结构回滚成「本迁移执行前 145 dev 库的实际状态」
--
-- 语义同 V0170 down：down 让 up 的净效果归零，**不追求更好看**。
-- 注意：生产库跑过 up 时是 0 行受影响（本来就对），所以 down 在生产上会退回
-- 到「dev 历史坏结构」—— 这正是 down 的语义，只在演练/回滚场景执行。
--
-- 幂等：每条都带「当前值 = up 写入的基线值」守卫，重复跑为 0 行。
-- 不 DELETE、不碰 sys_role_menu。
-- ============================================================================

-- ---------- ① /system/* 4 条退回旧 parent ----------
UPDATE sys_menu
   SET parent_id = NULL,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path IN ('/system/data-scope', '/system/field-mask', '/system/role-mutex')
   AND parent_id = '10000000-0000-0000-0000-000000000001'::uuid;

UPDATE sys_menu
   SET parent_id = '20000000-0000-0000-0000-000000000790'::uuid,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/system/change-log'
   AND parent_id = '10000000-0000-0000-0000-000000000001'::uuid;

-- ---------- ② 帮助中心退回「内容」目录 + 排序 74 ----------
UPDATE sys_menu
   SET parent_id = '20000000-0000-0000-0000-000000000790'::uuid,
       sort_order = 74,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/help'
   AND parent_id IS NULL
   AND sort_order = 78;

-- ---------- ③④ 排序退回 ----------
UPDATE sys_menu
   SET sort_order = 70,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business'
   AND sort_order = 75;

UPDATE sys_menu
   SET sort_order = 10,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/flow/def'
   AND sort_order = 68;

-- ---------- ⑤ 名称退回 ----------
UPDATE sys_menu
   SET name = '应付单',
       title = '应付单',
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business/erp/payable'
   AND name = '应付账款'
   AND title = '应付账款';

UPDATE sys_menu
   SET name = '凭证查询',
       title = '凭证查询',
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business/erp/voucher'
   AND name = '记账凭证'
   AND title = '记账凭证';

-- ---------- ⑥ 4 个明细页退回「隐藏」 ----------
-- up 的净效果是 visible false→true（生产真实变更 4 行；145 dev 迁移前是 true，
-- 被上一版迁移误设为 false 后由本 up 重新打开）。down 反向设回 false：
--   · 在生产：完全复原（回到迁移前的 false）；
--   · 在 dev：down 会把这 4 个隐藏，重跑 up 即再次打开 ⇒ down→up 往返后结果一致
--     （已实测往返，见卡内汇报）。
UPDATE sys_menu
   SET is_visible = false,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path IN ('/business/erp/inventory-location', '/business/erp/inventory-txn',
                '/business/erp/purchase-order-item', '/business/erp/seq')
   AND is_visible IS DISTINCT FROM false;
