-- ======================================================
-- V0182 回滚：入库链标准审批流（P-FLOW-3A）
--
-- 顺序（外键在前，被引用在后）：
--   ① 两个流程定义（版本 / 节点 / 边 / 审批人引用随外键 CASCADE 一起走）
--   ② 菜单 + 角色授权
--   ③ 角色（**只在没有用户绑定、且没有流程还引用它**时删；否则 NOTICE 保留）
--   ④ `biz_purchase_order.approval_instance_id` 列 + 索引
--   ⑤ `biz_stock_in` 的两条 CHECK 收回（PENDING_POST 不再合法）
--
-- ⚠️ ⑤ 的前提是库里**没有** `PENDING_POST` 的收货单：有就整条回滚失败并报错，
--    而不是把那些行静默改成别的状态（状态是经营事实，不许迁移自己改）。
-- ======================================================

-- ---------- ① 流程定义 ----------
DELETE FROM biz_flow_def_version
 WHERE id IN ('20000000-0000-4000-8000-0000000a0183'::uuid,
              '20000000-0000-4000-8000-0000000b0183'::uuid);

DELETE FROM biz_flow_def
 WHERE id IN ('20000000-0000-4000-8000-0000000a0182'::uuid,
              '20000000-0000-4000-8000-0000000b0182'::uuid)
   AND flow_code IN ('po_approve', 'stock_in_post');

-- ---------- ② 菜单与授权 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000000787'::uuid,
                   '20000000-0000-0000-0000-000000000788'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000000787'::uuid,
              '20000000-0000-0000-0000-000000000788'::uuid)
   AND auth_code IN ('business:erp:purchase-order:submit-approval',
                     'business:erp:stock-in:submit-post-approval');

-- ---------- ③ 角色（有人绑 / 有流程引用就保留） ----------
DO $$
DECLARE
    role_rec record;
    in_use   int;
BEGIN
    FOR role_rec IN
        SELECT * FROM (VALUES
            ('20000000-0000-4000-8000-0000000f0001'::uuid, 'DEPT_LEADER'),
            ('20000000-0000-4000-8000-0000000f0002'::uuid, 'PURCHASE_MANAGER'),
            ('20000000-0000-4000-8000-0000000f0003'::uuid, 'GENERAL_MANAGER'),
            ('20000000-0000-4000-8000-0000000f0004'::uuid, 'WAREHOUSE_MANAGER')
        ) AS v(id, code)
    LOOP
        SELECT count(*) INTO in_use FROM sys_user_role WHERE role_id = role_rec.id;
        IF in_use > 0 THEN
            RAISE NOTICE 'V0182 down：角色 % 仍有 % 个用户绑定，保留不删', role_rec.code, in_use;
        ELSIF EXISTS (SELECT 1 FROM biz_flow_node_assignee WHERE ref_id = role_rec.id) THEN
            RAISE NOTICE 'V0182 down：角色 % 仍被流程引用，保留不删', role_rec.code;
        ELSE
            DELETE FROM sys_role WHERE id = role_rec.id AND role_code = role_rec.code;
        END IF;
    END LOOP;
END $$;

-- ---------- ④ 采购订单列 ----------
DROP INDEX IF EXISTS idx_biz_po_approval;
ALTER TABLE biz_purchase_order DROP COLUMN IF EXISTS approval_instance_id;

-- ---------- ⑤ 收货单 CHECK 收回 ----------
DO $$
DECLARE
    alive int;
BEGIN
    SELECT count(*) INTO alive FROM biz_stock_in WHERE status = 'PENDING_POST';
    IF alive > 0 THEN
        RAISE EXCEPTION 'V0182 down 拒绝执行：库里还有 % 张 PENDING_POST（过账审批中）的收货单，请先让它们走完审批或作废再回滚', alive;
    END IF;
END $$;

ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_pending_cancel;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_pending_cancel
    CHECK (
        (status = 'PENDING_CANCEL' AND approval_instance_id IS NOT NULL)
        OR (status <> 'PENDING_CANCEL' AND approval_instance_id IS NULL)
    );

ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_status;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_status
    CHECK (status IN ('DRAFT', 'POSTED', 'PENDING_CANCEL', 'CANCELLED'));

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code IN ('po_approve', 'stock_in_post')) THEN
        RAISE EXCEPTION 'V0182 down 自检失败：流程定义未删净';
    END IF;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_purchase_order' AND column_name = 'approval_instance_id'
    ) THEN
        RAISE EXCEPTION 'V0182 down 自检失败：approval_instance_id 列未删';
    END IF;
    IF pg_get_constraintdef((SELECT oid FROM pg_constraint WHERE conname = 'ck_biz_si_status')) LIKE '%PENDING_POST%' THEN
        RAISE EXCEPTION 'V0182 down 自检失败：ck_biz_si_status 仍允许 PENDING_POST';
    END IF;
    RAISE NOTICE 'V0182 down 自检通过：列 / 约束 / 流程 / 菜单已回滚';
END $$;
