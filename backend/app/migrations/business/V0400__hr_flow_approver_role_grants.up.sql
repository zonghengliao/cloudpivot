-- ============================================================================
-- V0400__hr_flow_approver_role_grants（HR-48 全量回归收口）
--
-- 【t_fb2e95f4】HR 的 3 条种子流程把节点指派给 ROLE = HR_MANAGER，但 HR_MANAGER
-- 没有 `business:flow:approve` ⇒ 这三个节点**永远点不动**（审批人一律 403，
-- 单据永久卡在审批中）。全量回归的闸 `system::menu::permission_audit_tests::n19` 实测：
--
--     hr_employee_resign / n_hr → 角色 HR_MANAGER
--     hr_salary        / n_hr → 角色 HR_MANAGER
--     hr_salary_adjust / n_hr → 角色 HR_MANAGER
--
-- # 为什么会漏
--   V0239（flow_approver_role_grants）种的是「**当时**所有 PUBLISHED 流程里被指派为
--   审批人的角色」，它的口径完全正确 —— 但它跑在 V0394/V0395（HR 的三套流程）**之前**，
--   所以后加的流程不会被它覆盖。V0239 文件头「六、以后新增种子流程怎么办」写明：
--   新增 ROLE 指派的审批节点时，同一条迁移里给该角色补 approve 授权。
--   HR-38 只种了流程、没补授权 ⇒ 本迁移补上。
--
-- # 口径
--   与 V0239 ① 逐字同源，只把范围收到**本批 HR 的三套流程**：
--     · 角色来源不写死 role_code，而是「在 HR 流程的 APPROVAL/COUNTERSIGN 节点上被
--       ROLE 指派 + 当前发布版本」→ 以后 HR 再加审批节点，重跑本迁移即可覆盖；
--     · 每条 INSERT 都带 NOT EXISTS ⇒ 幂等（重跑 0 行）；
--     · 只授 business:flow:approve（…0803）一个权限点，不动其它 8 个节点（V0239 down
--       会按节点整片清理，本迁移的 down 也只删自己那一条，互不误伤）。
--
-- # 回滚
--   down 只删 (HR_MANAGER, …0803) 这一对，并断言「HR 流程的 ROLE 审批人都有 approve」
--   在回滚后**确实**不再成立（说明删的是该删的）。不动 sys_menu 行本身。
-- ============================================================================

-- ---------- 前置护栏：角色与权限点必须真实存在 ----------
DO $$
DECLARE
    v_role  int;
    v_menu  int;
BEGIN
    SELECT count(*) INTO v_role FROM sys_role
     WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL;
    IF v_role <> 1 THEN
        RAISE EXCEPTION 'V0400 拒绝执行：HR_MANAGER 角色必须恰好 1 行（实际 %）', v_role;
    END IF;

    SELECT count(*) INTO v_menu FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000803'::uuid
       AND auth_code = 'business:flow:approve'
       AND deleted_at IS NULL;
    IF v_menu <> 1 THEN
        RAISE EXCEPTION 'V0400 拒绝执行：business:flow:approve（…0803）菜单行不在位';
    END IF;
END $$;

-- ---------- 授权：HR 流程里被 ROLE 指派的审批角色 → business:flow:approve ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT r.id, '20000000-0000-0000-0000-000000000803'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND EXISTS (
           SELECT 1
             FROM biz_flow_node_assignee a
             JOIN biz_flow_node n        ON n.id = a.node_id
             JOIN biz_flow_def_version v ON v.id = n.version_id
             JOIN biz_flow_def d         ON d.id = v.def_id
            WHERE a.assignee_type = 'ROLE'
              AND a.ref_id = r.id
              AND n.node_type IN ('APPROVAL', 'COUNTERSIGN')
              AND v.is_current
              AND v.version_status = 'PUBLISHED'
              AND d.deleted_at IS NULL
              AND d.status = 'PUBLISHED'
              -- 本批 HR 的三套流程（V0393 / V0394 / V0395）
              AND d.flow_code IN ('hr_leave', 'hr_salary', 'hr_salary_adjust',
                                  'hr_employee_resign')
       )
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu rm
            WHERE rm.role_id = r.id
              AND rm.menu_id = '20000000-0000-0000-0000-000000000803'::uuid
       );

-- ---------- 自检：HR 流程的 ROLE 审批人必须都能审（本迁移的核心断言） ----------
DO $$
DECLARE
    v_missing text;
BEGIN
    SELECT string_agg(DISTINCT d.flow_code || ' / ' || r.role_code, ', ')
      INTO v_missing
      FROM biz_flow_node_assignee a
      JOIN biz_flow_node n        ON n.id = a.node_id
      JOIN biz_flow_def_version v ON v.id = n.version_id
      JOIN biz_flow_def d         ON d.id = v.def_id
      JOIN sys_role r             ON r.id = a.ref_id AND r.deleted_at IS NULL
     WHERE a.assignee_type = 'ROLE'
       AND n.node_type IN ('APPROVAL', 'COUNTERSIGN')
       AND v.is_current
       AND v.version_status = 'PUBLISHED'
       AND d.deleted_at IS NULL
       AND d.status = 'PUBLISHED'
       AND d.flow_code IN ('hr_leave', 'hr_salary', 'hr_salary_adjust', 'hr_employee_resign')
       AND NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = r.id
                  AND rm.menu_id = '20000000-0000-0000-0000-000000000803'::uuid
           );
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0400 自检失败：以下 HR 审批节点的角色仍无 business:flow:approve —— %', v_missing;
    END IF;
    RAISE NOTICE 'V0400 自检通过：HR 三套流程（+hr_leave）的 ROLE 审批人全部可审';
END $$;
