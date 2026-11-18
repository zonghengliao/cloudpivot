-- ============================================================================
-- V0455 回滚：供应商准入 / 黑名单流程（M4-14）
--
--   顺序：先看数据（有单据就拒），再删运行期配置（菜单 / 授权 / 流程四层 / 取号行），
--         最后删表。**申请单是审计事实**（谁因为什么把谁拉黑、谁批的），
--         回滚会把这段历史连根拔掉 —— 所以有数据就拒绝，让人先导出再决定。
-- ============================================================================
DO $$
DECLARE
    n_rows INT;
BEGIN
    SELECT count(*) INTO n_rows FROM biz_supplier_access;
    IF n_rows > 0 THEN
        RAISE EXCEPTION 'V0455 回滚被拒绝：biz_supplier_access 还有 % 行（准入/黑名单是审计事实，先导出再回滚）', n_rows;
    END IF;
    -- 已经在跑的审批实例也会失去业务对象（死单），一并拒绝
    SELECT count(*) INTO n_rows FROM biz_flow_instance WHERE biz_type = 'SUPPLIER_ACCESS';
    IF n_rows > 0 THEN
        RAISE EXCEPTION 'V0455 回滚被拒绝：还有 % 个 SUPPLIER_ACCESS 审批实例（回滚会让它们变成没有业务对象的死单）', n_rows;
    END IF;
END $$;

-- 菜单授权 → 菜单行
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000004484'::uuid,
                   '20000000-0000-0000-0000-000000004485'::uuid,
                   '20000000-0000-0000-0000-000000004486'::uuid,
                   '20000000-0000-0000-0000-000000004487'::uuid,
                   '20000000-0000-0000-0000-000000004488'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000004484'::uuid,
              '20000000-0000-0000-0000-000000004485'::uuid,
              '20000000-0000-0000-0000-000000004486'::uuid,
              '20000000-0000-0000-0000-000000004487'::uuid,
              '20000000-0000-0000-0000-000000004488'::uuid);

-- 流程四层（节点 / 边 / 审批人靠 ON DELETE CASCADE；版本靠 def 的 CASCADE）
DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d05a2'::uuid;
DELETE FROM biz_flow_def         WHERE id = '20000000-0000-4000-8000-0000000d05a1'::uuid;

-- 取号行
DELETE FROM biz_seq WHERE seq_type = 'SAC' AND id = '20000000-0000-4000-8000-000000004489'::uuid;

-- 表
DROP TABLE IF EXISTS biz_supplier_access;
