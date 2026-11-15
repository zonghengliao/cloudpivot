-- ======================================================
-- V0281 回滚：销售订单审批流（SO-06）
--
-- 顺序（外键在前，被引用在后）：
--   ① 流程版本（节点 / 边 / 审批人引用随外键 CASCADE 一起走）→ 流程定义
--   ② `biz_sales_order.approval_instance_id` 索引 + 列
--   ③ 角色 `SALES_SUPERVISOR`（**只在没有用户绑定、且没有流程还引用它**时删；否则 NOTICE 保留）
--
-- ⚠️ ② 的前提是库里**没有**还挂着实例的销售订单：有就整条回滚失败并报错，
--    而不是把那列静默清空（「这张单走过哪张审批单」是审计事实，不许迁移自己抹掉）。
-- ⚠️ 财务经理 / 总经理两个角色不归本迁移管（V0191 / V0182 种的），不删。
-- ======================================================

-- ---------- ① 流程定义 ----------
DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-0000000d0282'::uuid
   AND def_id = '20000000-0000-4000-8000-0000000d0281'::uuid;

DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-0000000d0281'::uuid
   AND flow_code = 'so_approve';

-- ---------- ② 销售订单列 ----------
DO $$
DECLARE
    alive int;
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_sales_order' AND column_name = 'approval_instance_id'
    ) THEN
        SELECT count(*) INTO alive
          FROM biz_sales_order
         WHERE approval_instance_id IS NOT NULL;
        IF alive > 0 THEN
            RAISE EXCEPTION 'V0281 down 拒绝执行：库里还有 % 张销售订单挂着审批实例（approval_instance_id 非空），请先备份审计数据再回滚', alive;
        END IF;
    END IF;
END $$;

DROP INDEX IF EXISTS idx_biz_sales_order_approval;
ALTER TABLE biz_sales_order DROP COLUMN IF EXISTS approval_instance_id;

-- ---------- ③ 角色（有人绑 / 有流程引用就保留） ----------
DO $$
DECLARE
    role_id uuid := '20000000-0000-4000-8000-0000000f0007'::uuid;
    in_use  int;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM sys_role WHERE id = role_id AND role_code = 'SALES_SUPERVISOR'
    ) THEN
        RAISE NOTICE 'V0281 down：角色 SALES_SUPERVISOR 不存在（或 UUID 不是本迁移种的），跳过';
        RETURN;
    END IF;
    SELECT count(*) INTO in_use FROM sys_user_role WHERE role_id = role_id;
    IF in_use > 0 THEN
        RAISE NOTICE 'V0281 down：角色 SALES_SUPERVISOR 仍有 % 个用户绑定，保留不删', in_use;
    ELSIF EXISTS (SELECT 1 FROM biz_flow_node_assignee WHERE ref_id = role_id) THEN
        RAISE NOTICE 'V0281 down：角色 SALES_SUPERVISOR 仍被流程引用，保留不删';
    ELSE
        DELETE FROM sys_role WHERE id = role_id AND role_code = 'SALES_SUPERVISOR';
    END IF;
END $$;
