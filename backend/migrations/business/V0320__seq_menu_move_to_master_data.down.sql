-- ============================================================================
-- V0320 down：把单号序列退回「财务管理」（本迁移执行前的状态）
--
-- 语义同 V0170 / V0319 down：down 让 up 的净效果归零，不追求更好看。
-- 退回值取 V0154 写入的历史值：parent = …0e02（财务管理）、sort_order = 50。
--
-- 幂等：带「当前值 = up 写入的目标值」守卫，重复跑为 0 行。
-- 只改 parent_id + sort_order，不 DELETE、不碰 sys_role_menu。
-- ============================================================================

-- ---------- ① 退回财务管理 / sort 50 ----------
UPDATE sys_menu
   SET parent_id  = '20000000-0000-0000-0000-000000000e02'::uuid,  -- 财务管理
       sort_order = 50,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business/erp/seq'
   AND menu_type = 2
   AND parent_id = '20000000-0000-0000-0000-000000000e03'::uuid
   AND sort_order = 30;

-- ---------- ② 自检：必须已退回财务管理 ----------
DO $$
DECLARE
    bad int;
BEGIN
    SELECT count(*) INTO bad
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND path = '/business/erp/seq'
       AND menu_type = 2
       AND (parent_id IS DISTINCT FROM '20000000-0000-0000-0000-000000000e02'::uuid
            OR sort_order <> 50);
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0320 down 自检失败：% 条单号序列未退回财务管理', bad;
    END IF;

    RAISE NOTICE 'V0320 down 自检通过：单号序列已退回「财务管理」sort=50';
END $$;
