-- ============================================================================
-- V0403__hr_manager_approval_center_page（闸⑦ 孤儿按钮收口 · 第二对 · t_c49010bf）
--
-- 【问题】同一道闸 `scripts/verify/orphan_button_gate.py` 在「V0400 已应用」的库上
-- 报出**另一对**孤儿（V0402 修掉 SALES_CLERK 那两对之后才露出来）：
--
--     · HR_MANAGER  business:flow:approve  不可达父页 /flow/task（business:flow:instance:read）
--
--   根因链：
--     1. V0400（HR-48 / t_fb2e95f4）给 HR_MANAGER 补了 `business:flow:approve`
--        （菜单 …0803，`menu_type=3` 按钮）；这是必要的修复 —— 否则 HR 三套流程的
--        `n_hr` 节点永远 403；
--     2. 但 …0803 这个按钮节点挂在「审批中心」页面（…0862，
--        `auth_code = business:flow:instance:read`，`path = /flow/task`）下；
--     3. HR_MANAGER **没有** `business:flow:instance:read` ⇒ 按钮的父页在它的菜单树里
--        不存在 ⇒ 现象是「审批权限给了，界面上却找不到审批中心」——
--        API 能批（权限码有），页面进不去（页面码没有）。
--
--   为什么 V0400 当时没被这道闸拦住：V0400 的验证口径是 `permission_audit_tests::n19`
--   （「审批节点的 ROLE 审批人是否持有 approve 码」）与真库审批 E2E，**没有重跑闸⑦**；
--   而闸⑦ 只有在「V0400 已 apply」的库上才会报这对。本卡补上这一对。
--
-- # 修法：① 补授父页（含目录）
--
--   与库里既有 9 个审批角色**逐条对齐**（DEPT_LEADER / EMPLOYEE / FINANCE_DIRECTOR /
--   FINANCE_MANAGER / FLOW_CONFIG_APPROVER / GENERAL_MANAGER / PURCHASE_MANAGER /
--   SALES_SUPERVISOR / WAREHOUSE_MANAGER 都是「目录 …0860 + 页面 …0862」成对持有）：
--     · …0860  `审批流` 目录（`auth_code` 为空）—— 不授目录，侧边栏整棵子树不出现；
--     · …0862  `审批中心` 页面（`business:flow:instance:read`）—— 闸⑦ 认的「带码祖先」。
--   两块缺一不可：少了 862 闸⑦ 照样红；少了 860 侧边栏看不见（页面码有、入口没有）。
--
--   不选「把 …0803 改挂到 /flow/def 下」的理由与 V0402 同：按钮与页面是**同一件事**
--   （审批动作发生在审批中心），挪父页会让权限树语义错位；且 HR_MANAGER 本来就该看审批中心。
--
-- # 口径
--   · 角色 / 菜单按业务键现查（`role_code` / `id`），不写死角色 UUID；
--   · `NOT EXISTS` 判重 ⇒ 幂等（重跑 0 行）；
--   · 只加授权，不动 `sys_menu` 行；
--   · 末尾自检：HR_MANAGER 必须**同时**持有 860 与 862，否则迁移失败。
--
-- # 号段
--   写入本文件时 `backend/migrations/business/` 最大号 = V0402（本卡同日新增），
--   本文件顺延取 **V0403**。
--
-- # 回滚
--   `V0403__hr_manager_approval_center_page.down.sql` 只删这两对
--   （HR_MANAGER × 860 / 862），删完闸⑦ 回到 V0403 之前的状态（红）。
-- ============================================================================

-- ---------- 前置护栏：角色 / 目录 / 页面 / 按钮必须真实存在 ----------
DO $$
DECLARE
    v_role_total int;
    v_role_alive int;
    v_dir        int;
    v_page       int;
    v_btn        int;
BEGIN
    SELECT count(*), count(*) FILTER (WHERE deleted_at IS NULL)
      INTO v_role_total, v_role_alive
      FROM sys_role
     WHERE role_code = 'HR_MANAGER';

    IF v_role_total = 0 THEN
        RAISE EXCEPTION 'V0403 拒绝执行：找不到 HR_MANAGER 角色（HR 迁移应当已种）';
    END IF;
    IF v_role_total > 1 THEN
        RAISE EXCEPTION 'V0403 拒绝执行：HR_MANAGER 有 % 行（role_code 无唯一约束，先人工收口）',
            v_role_total;
    END IF;

    SELECT count(*) INTO v_dir
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000860'::uuid
       AND menu_type = 1
       AND coalesce(auth_code, '') = ''
       AND deleted_at IS NULL;
    IF v_dir <> 1 THEN
        RAISE EXCEPTION 'V0403 拒绝执行：审批流目录（…0860）不在位或形态不对';
    END IF;

    SELECT count(*) INTO v_page
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000862'::uuid
       AND menu_type = 2
       AND auth_code = 'business:flow:instance:read'
       AND deleted_at IS NULL;
    IF v_page <> 1 THEN
        RAISE EXCEPTION 'V0403 拒绝执行：审批中心页面（…0862 / business:flow:instance:read）不在位';
    END IF;

    SELECT count(*) INTO v_btn
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000803'::uuid
       AND menu_type = 3
       AND auth_code = 'business:flow:approve'
       AND parent_id = '20000000-0000-0000-0000-000000000862'::uuid
       AND deleted_at IS NULL;
    IF v_btn <> 1 THEN
        RAISE EXCEPTION 'V0403 拒绝执行：审批按钮（…0803）不在审批中心页面下（闸⑦ 的判据前提变了）';
    END IF;

    IF v_role_alive = 0 THEN
        RAISE NOTICE 'V0403：HR_MANAGER 已软删 → 不补授权（闸⑦ 对该角色无意义）';
    ELSE
        RAISE NOTICE 'V0403：护栏通过（角色 / 目录 / 页面 / 按钮 各 1 行）';
    END IF;
END $$;

-- ---------- 授权：HR_MANAGER → 审批流目录 + 审批中心页面 ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  JOIN sys_menu m
    ON m.id IN (
           '20000000-0000-0000-0000-000000000860'::uuid,   -- 审批流（目录）
           '20000000-0000-0000-0000-000000000862'::uuid    -- 审批中心（页面）
       )
   AND m.deleted_at IS NULL
 WHERE r.role_code = 'HR_MANAGER'
   AND r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu rm
            WHERE rm.role_id = r.id
              AND rm.menu_id = m.id
       );

-- ---------- 自检：两行授权必须都在位 ----------
DO $$
DECLARE
    v_have int;
    v_alive int;
BEGIN
    SELECT count(*) INTO v_alive FROM sys_role WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL;
    IF v_alive = 0 THEN
        RAISE NOTICE 'V0403：HR_MANAGER 已软删，跳过自检';
        RETURN;
    END IF;

    SELECT count(*) INTO v_have
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
     WHERE r.role_code = 'HR_MANAGER'
       AND r.deleted_at IS NULL
       AND rm.menu_id IN (
               '20000000-0000-0000-0000-000000000860'::uuid,
               '20000000-0000-0000-0000-000000000862'::uuid
           );
    IF v_have <> 2 THEN
        RAISE EXCEPTION 'V0403 自检失败：HR_MANAGER 的审批流目录 + 审批中心页面授权应为 2 行（实际 %）',
            v_have;
    END IF;

    RAISE NOTICE 'V0403：HR_MANAGER 已可进入审批中心（目录 + 页面各 1 行），business:flow:approve 不再是孤儿按钮';
END $$;
