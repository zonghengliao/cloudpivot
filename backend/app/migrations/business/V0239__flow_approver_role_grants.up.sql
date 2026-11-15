-- ============================================================================
-- V0239__flow_approver_role_grants.up.sql
--
-- 【t_a6e8aca3】审批岗位角色没有 `business:flow:approve` → 审批链永远推不动
--
-- # 一、现象（真机，库 cloudpivot_manual2）
--   预算上报后走 `budget_approve`，节点 n_dept 指派 DEPT_LEADER、n_fin 指派 FINANCE_MANAGER。
--   夹具用户登录成功，但 `POST /api/business/flow/task/{id}/approve` 一律：
--     403 {"code":403,"msg":"无权限访问，需要权限: business:flow:approve"}
--   影响面不止预算：合同（contract_approve）、采购（po_approve）、收货（stock_in_post）、
--   盘点（stock_take_post）、应付作废（payable_cancel）、资产处置（asset_disposal）、
--   状态机配置复核（flow_edge_config）—— 凡走「部门主管 / 采购经理 / 财务经理 / 总经理 /
--   财务总监 / 仓管主管 / 流程配置复核人」节点的审批在该环境全部 PENDING 到底。
--
-- # 二、根因：不是「漏给某个角色一行」，而是**授权与建角色的执行顺序**
--   1) …0800–…0804 这 5 个实例侧权限点由 V0124 种下，V0124 文件头（第 33–37 行）写明
--      「只种权限点，不替任何角色授权」；
--   2) 唯一给角色授过实例侧菜单的是 V0128（…861 发起审批 / …862 审批中心
--      「授权给所有未删除的角色」，第 46–58 行）—— 但它按 `SELECT … FROM sys_role` 展开，
--      执行时**全库还没有任何业务角色**：种子 S__role_data.sql 只种 SUPER_ADMIN，
--      且种子的执行时机在所有版本化迁移之后（nik-db `execution_rank`：versioned=1, seed=3）。
--      空库上这三条 INSERT 影响 0 行；
--   3) 业务角色是后来由 V0182（DEPT_LEADER / PURCHASE_MANAGER / GENERAL_MANAGER /
--      WAREHOUSE_MANAGER）、V0191（FINANCE_MANAGER / FINANCE_DIRECTOR）、
--      V0194 前后（FLOW_CONFIG_APPROVER）种出来的，**没有任何一条迁移回头补授权**。
--   ⇒ 结果：所有审批岗位角色手里一条菜单授权都没有（approve / instance:read /
--      instance:create 全缺，连「审批中心」菜单都不显示）。超管靠 is_super_admin 短路，
--      验收时看不出来 —— 与 V0124 文件头记的「超管有短路，所以缺口在验收里是隐形的」同一个坑。
--
--   现场复算（cloudpivot_manual2，应用本迁移前）：
--     SELECT count(*) FROM sys_role_menu WHERE menu_id='…0803';  -- 0
--     SELECT count(*) FROM sys_role_menu WHERE menu_id='…0862';  -- 0
--
-- # 三、修法（只做授权，不改权限点、不改流程定义）
--   ① business:flow:approve（…0803）→ **所有会被指派为审批人的角色**：
--      a. 数据驱动：扫 biz_flow_node_assignee（node_type ∈ APPROVAL/COUNTERSIGN，
--         版本 is_current + PUBLISHED，流程定义 PUBLISHED 且未软删）；
--      b. 并上 7 个标准审批岗位的 role_code 兜底 —— 覆盖「先建角色、流程还没接上」的空窗期，
--         也让 `sys_role` 与流程定义分属两条迁移时不会漏。
--      （本仓种过的审批岗位就这 7 个：DEPT_LEADER / PURCHASE_MANAGER / GENERAL_MANAGER /
--        WAREHOUSE_MANAGER / FINANCE_MANAGER / FINANCE_DIRECTOR / FLOW_CONFIG_APPROVER）
--   ② instance:create / instance:read / instance:withdraw（…0800/…0801/…0802）与
--      「审批流」目录 + 发起审批 + 审批中心（…0860/…0861/…0862）→ **所有未软删角色**：
--      与 V0128 第 45 行的原意逐条一致（普通员工要能发起、能看自己的单），这次按
--      「当前存在的角色」补。withdraw 的行级限制在 handler（只许发起人本人），不做角色隔离。
--   ③ business:flow:admin（…0804）→ 只给 SUPER_ADMIN（与 S__workflow_permissions.sql
--      的「写四码只授超管」同口径）。超管本来短路，这一行是给「另建一个流程管理员角色」
--      留的显式入口，默认不放给其它角色。
--
--   不改：business:flow:read / create / update / delete / publish（…0780–…0785）——
--   S__workflow_permissions.sql 已授且幂等，本文件不重复。
--
-- # 四、幂等 / 自检
--   全部 `INSERT … WHERE NOT EXISTS`：重复执行 0 行；自检对「结果」断言而非「改了 N 行」。
--   自检 ①② 是**修完之后必须成立**的不变量（修不好就是本文件逻辑错，直接炸掉升级）：
--     ① 每个被指派为审批人的**在用**角色都有 approve；
--     ② 每个未软删角色都有 instance:read。
--   自检 ④ 只 NOTICE（不拦升级）：流程定义里记着、但角色已被删除的悬空指派 ——
--     那是客户的配置错误，不该把升级挡死，但值得在迁移日志里点名。
--
-- # 五、回滚
--   V0239__flow_approver_role_grants.down.sql：删掉本文件涉及的 8 个菜单节点上的**全部**授权。
--   ⚠ 若升级前有人手工给某个角色勾过这些权限点（例如演示库现场修补），回滚会一并删掉它们 ——
--   授权行没有"来源"列，无法区分。回滚只用于开发/演练环境；生产先导出
--   `SELECT role_id, menu_id FROM sys_role_menu WHERE menu_id IN (…)`。
--
-- # 六、以后新增种子流程怎么办
--   新增「ROLE 指派」的审批节点时，同一条迁移里给该角色补 approve 授权（或复用本文件的口径），
--   否则 `system::menu::permission_audit_tests::n19_*` 会红（"审批节点的角色必须能审"）。
-- ============================================================================

-- ---------- ① 审批角色 → business:flow:approve（…0803） ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT r.id, '20000000-0000-0000-0000-000000000803'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND (
        -- a. 流程定义里被指派为审批人的角色（当前生效版本）
        EXISTS (
            SELECT 1
              FROM biz_flow_node_assignee a
              JOIN biz_flow_node n          ON n.id = a.node_id
              JOIN biz_flow_def_version v   ON v.id = n.version_id
              JOIN biz_flow_def d           ON d.id = v.def_id
             WHERE a.assignee_type = 'ROLE'
               AND a.ref_id = r.id
               AND n.node_type IN ('APPROVAL', 'COUNTERSIGN')
               AND v.is_current
               AND v.version_status = 'PUBLISHED'
               AND d.deleted_at IS NULL
               AND d.status = 'PUBLISHED'
        )
        -- b. 标准审批岗位兜底（角色已建、流程尚未接入时也要能勾到权限点）
        OR r.role_code IN ('DEPT_LEADER', 'PURCHASE_MANAGER', 'GENERAL_MANAGER',
                          'WAREHOUSE_MANAGER', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR',
                          'FLOW_CONFIG_APPROVER')
       )
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu rm
            WHERE rm.role_id = r.id
              AND rm.menu_id = '20000000-0000-0000-0000-000000000803'::uuid
       );

-- ---------- ② 实例侧「人人可用」的四个码 + 三个页面节点 → 所有未软删角色 ----------
-- …0800 instance:create / …0801 instance:read / …0802 instance:withdraw
-- …0860 审批流（目录） / …0861 发起审批 / …0862 审批中心
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
 CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000000800'::uuid),
        ('20000000-0000-0000-0000-000000000801'::uuid),
        ('20000000-0000-0000-0000-000000000802'::uuid),
        ('20000000-0000-0000-0000-000000000860'::uuid),
        ('20000000-0000-0000-0000-000000000861'::uuid),
        ('20000000-0000-0000-0000-000000000862'::uuid)
     ) AS m(id)
 WHERE r.deleted_at IS NULL
   AND EXISTS (SELECT 1 FROM sys_menu pm WHERE pm.id = m.id AND pm.deleted_at IS NULL AND pm.status = 1)
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu rm
            WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );

-- ---------- ③ business:flow:admin（…0804）→ 只给 SUPER_ADMIN ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, '20000000-0000-0000-0000-000000000804'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu rm
            WHERE rm.role_id = r.id
              AND rm.menu_id = '20000000-0000-0000-0000-000000000804'::uuid
       );

-- ---------- ④ 自检（对结果断言，全部 fail-closed） ----------
DO $$
DECLARE
    men_approve uuid := '20000000-0000-0000-0000-000000000803'::uuid;
    men_read    uuid := '20000000-0000-0000-0000-000000000801'::uuid;
    n_bad_point int;
    n_lacking   int;
    n_no_read   int;
    n_granted   int;
    n_dangling  int;
    dangle_list text;
BEGIN
    -- 权限点本身必须仍在（被别的迁移退休掉了，本文件就白授）
    SELECT count(*) INTO n_bad_point
      FROM sys_menu
     WHERE id IN (men_approve, men_read)
       AND (deleted_at IS NOT NULL OR status <> 1 OR auth_code IS NULL OR auth_code = '');
    IF n_bad_point > 0 THEN
        RAISE EXCEPTION 'V0239 自检失败：% 个流程实例侧权限点已失效（软删 / 停用 / 无码）', n_bad_point;
    END IF;

    -- ① 每个「会被指派为审批人」的在用角色都必须有 business:flow:approve
    SELECT count(*) INTO n_lacking
      FROM (
            SELECT DISTINCT a.ref_id AS role_id
              FROM biz_flow_node_assignee a
              JOIN biz_flow_node n        ON n.id = a.node_id
              JOIN biz_flow_def_version v ON v.id = n.version_id
              JOIN biz_flow_def d         ON d.id = v.def_id
             WHERE a.assignee_type = 'ROLE'
               AND n.node_type IN ('APPROVAL', 'COUNTERSIGN')
               AND v.is_current
               AND v.version_status = 'PUBLISHED'
               AND d.deleted_at IS NULL
               AND d.status = 'PUBLISHED'
           ) refs
      JOIN sys_role r ON r.id = refs.role_id AND r.deleted_at IS NULL
     WHERE NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = r.id AND rm.menu_id = men_approve
           );
    IF n_lacking > 0 THEN
        RAISE EXCEPTION
            'V0239 自检失败：还有 % 个被审批节点指派的在用角色缺 business:flow:approve（该节点永远点不动）',
            n_lacking;
    END IF;

    -- ② 每个未软删角色都必须有 business:flow:instance:read（否则进不了审批中心）
    SELECT count(*) INTO n_no_read
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = r.id AND rm.menu_id = men_read
           );
    IF n_no_read > 0 THEN
        RAISE EXCEPTION
            'V0239 自检失败：还有 % 个未软删角色缺 business:flow:instance:read（审批中心不可见）',
            n_no_read;
    END IF;

    -- ③ 防空转：本文件点名的 8 个节点上必须真的存在授权行
    SELECT count(*) INTO n_granted
      FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000000800'::uuid,
                       '20000000-0000-0000-0000-000000000801'::uuid,
                       '20000000-0000-0000-0000-000000000802'::uuid,
                       men_approve,
                       '20000000-0000-0000-0000-000000000804'::uuid,
                       '20000000-0000-0000-0000-000000000860'::uuid,
                       '20000000-0000-0000-0000-000000000861'::uuid,
                       '20000000-0000-0000-0000-000000000862'::uuid);
    IF n_granted = 0 THEN
        RAISE EXCEPTION 'V0239 自检失败：8 个实例侧节点上一条授权都没有（授权语句空转）';
    END IF;

    -- ④ 只告警：审批节点指派指向的角色已不存在 / 已软删（客户配置问题，不拦升级）
    SELECT count(*), string_agg(DISTINCT a.ref_id::text, ', ')
      INTO n_dangling, dangle_list
      FROM biz_flow_node_assignee a
      JOIN biz_flow_node n        ON n.id = a.node_id
      JOIN biz_flow_def_version v ON v.id = n.version_id
      JOIN biz_flow_def d         ON d.id = v.def_id
     WHERE a.assignee_type = 'ROLE'
       AND n.node_type IN ('APPROVAL', 'COUNTERSIGN')
       AND v.is_current
       AND v.version_status = 'PUBLISHED'
       AND d.deleted_at IS NULL
       AND d.status = 'PUBLISHED'
       AND NOT EXISTS (
               SELECT 1 FROM sys_role r
                WHERE r.id = a.ref_id AND r.deleted_at IS NULL
           );
    IF n_dangling > 0 THEN
        RAISE WARNING
            'V0239 告警：% 处审批节点指派指向已删除/已软删的角色（%），这些节点路由时会缺审批人，请检查流程定义',
            n_dangling, dangle_list;
    END IF;

    RAISE NOTICE 'V0239 自检通过：审批角色 approve 授权 % 行；8 个实例侧节点合计授权 % 行',
        (SELECT count(*) FROM sys_role_menu WHERE menu_id = men_approve), n_granted;
END
$$;
