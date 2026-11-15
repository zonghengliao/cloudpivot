-- ============================================================================
-- V0331 回滚：采购申请闭环（P2-02）
--
-- 顺序：审批流（assignee → edge → node → version → def）→ 角色授权 → 按钮 / 页面
--       → 取号规则 → 两张表（明细先删）。
-- 安全：**业务已经用过这张表就拒绝回滚**（数据会一起消失，宁可失败也不静默删业务数据）。
-- ============================================================================

DO $$
DECLARE
    v_rows integer;
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.tables
                WHERE table_schema = current_schema() AND table_name = 'biz_purchase_requisition') THEN
        EXECUTE 'SELECT count(*) FROM biz_purchase_requisition WHERE deleted_at IS NULL' INTO v_rows;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0331 拒绝回滚：biz_purchase_requisition 还有 % 张存活申请单（先删业务数据或改为前滚修复）', v_rows;
        END IF;
    END IF;
END $$;

-- ---------- ① 审批流 ----------
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node
                    WHERE version_id = '20000000-0000-4000-8000-0000000d0332'::uuid);
DELETE FROM biz_flow_edge  WHERE version_id = '20000000-0000-4000-8000-0000000d0332'::uuid;
DELETE FROM biz_flow_node  WHERE version_id = '20000000-0000-4000-8000-0000000d0332'::uuid;
UPDATE biz_flow_def SET current_version_id = NULL, updated_at = now()
 WHERE id = '20000000-0000-4000-8000-0000000d0331'::uuid;
DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0332'::uuid;
DELETE FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0331'::uuid;

-- ---------- ② 角色授权 + 菜单 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000003331'::uuid,
                   '20000000-0000-0000-0000-000000003332'::uuid,
                   '20000000-0000-0000-0000-000000003333'::uuid,
                   '20000000-0000-0000-0000-000000003334'::uuid);
DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000003332'::uuid,
              '20000000-0000-0000-0000-000000003333'::uuid,
              '20000000-0000-0000-0000-000000003334'::uuid,
              '20000000-0000-0000-0000-000000003331'::uuid);

-- ---------- ③ 取号规则 ----------
DELETE FROM biz_seq WHERE seq_type = 'PRQ';

-- ---------- ④ 两张表（明细先删） ----------
DROP TABLE IF EXISTS biz_purchase_requisition_item;
DROP TABLE IF EXISTS biz_purchase_requisition;
