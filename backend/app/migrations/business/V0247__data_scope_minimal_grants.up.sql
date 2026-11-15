-- ======================================================
-- V0247：数据权限 · 按角色职责做最小授权（【P0-FIX-2】收口）
--
-- ## 病根（评审实测，本卡复核一致）
--
--   select count(*) from sys_role_dept;            -- 0 行
--   select count(*) from sys_role where data_scope <> 1;  -- 0 行（默认值 1 = 全部数据）
--
-- 库里「谁能看哪些行」只有两个载体：
--   sys_role.data_scope  SMALLINT 1 全部 / 2 本部门 / 3 本部门及以下 / 4 仅本人 / 5 自定义部门集合
--   sys_role_dept        (role_id, dept_id) 一行一个部门，**仅 data_scope = 5 生效**
--
-- 解析链（读一次请求，缓存 300 s）：
--   business/shared/principal.rs::RESOLVE_SQL（CUSTOM 分支 INNER JOIN sys_role_dept）
--     → principal::merge_scope_rows（多角色取并集；部门类解析不出部门 → 降级 SELF，S10/S11）
--     → shared/scope.rs::filter_for（生成 `AND …dept_id = ANY(?)` 片段）
--     → scope::paginate / filter_for 注入到列表与单条查询
-- 全库 data_scope 都是默认 1 ⇒ 每个人的片段都是 `AND 1 = 1` ⇒ 行级数据权限空转。
--
-- ## 本迁移做什么 / 不做什么
--
-- 做：按角色职责把 data_scope 落成有意义的取值，并给需要「跨部门集合」的两个财务角色
--     落 sys_role_dept（每一条都写理由，见下方逐段注释）。
-- 不做：不建部门、不建/改用户、不碰权限码与菜单、不动 SUPER_ADMIN 的 1。
--
-- ## 部门怎么定位
--
-- 用 dept_code（HQ / FIN）而不是硬编码 uuid：基础版的部门树由运维用接口建，
-- 各环境 uuid 不同。目标库**还没有**该部门时（基础版空库没有组织树）：
--   · 不插 sys_role_dept 行；
--   · 该角色保持原值（1）并 RAISE NOTICE —— 硬写 5 而没有部门行会让持有者
--     被 merge_scope_rows 静默降级成「仅本人」（S10），比不管更糟；
--   · 组织树建好后用 `PUT /api/business/permission/scope/role/{role_id}` 补配
--     （巡检 N1 会 WARN 提醒）。
--
-- ## 回滚
--
-- V0247__data_scope_minimal_grants.down.sql：把本迁移碰过的角色逐个还原成 1，
-- 并删除这两个角色的 sys_role_dept 行（= 迁移前基线：全 1 / 0 行）。
-- 注意：down 会一并清掉这两个角色后来在管理界面配的集合——它还原的是「基线」，
-- 不是「上一次的值」，与本仓其它种子类迁移同口径。
-- ======================================================

DO $$
DECLARE
    v_n     integer;
    v_rows  integer;
BEGIN
    -- ---------- ① 部门类角色 → 3（本部门及以下） ----------
    -- DEPT_LEADER        部门主管：职责就是管本部门这棵子树（含下级部门），不含平级部门。
    -- PURCHASE_MANAGER   采购经理：采购口的单据/任务只应看到自己部门（及其下级采购组）。
    -- WAREHOUSE_MANAGER  仓管主管：仓库口的单据/任务同理，库间互不可见。
    -- 为什么不是 2（本部门）：三个角色都有「带下级」的组织形态（采购组 / 分仓 / 子部门），
    -- 用 2 会让下级部门的数据凭空消失（静默少数据，比多看见更难查）。
    -- 为什么不是 5（自定义集合）：集合需要逐部门维护，组织调整就得逐角色改；2/3 跟着
    -- 用户的 dept_id 自动走，是「按职责」而不是「按某次配置」。
    UPDATE sys_role
       SET data_scope = 3,
           updated_at = CURRENT_TIMESTAMP
     WHERE role_code IN ('DEPT_LEADER', 'PURCHASE_MANAGER', 'WAREHOUSE_MANAGER')
       AND deleted_at IS NULL;
    GET DIAGNOSTICS v_n = ROW_COUNT;
    RAISE NOTICE 'V0247 ① 部门类角色（DEPT_LEADER / PURCHASE_MANAGER / WAREHOUSE_MANAGER）→ data_scope=3：% 行', v_n;

    -- ---------- ② 财务两个角色 → 5（自定义部门集合） ----------
    -- 先落 sys_role_dept（幂等：唯一键 (role_id, dept_id)），再决定是否切 5。
    --
    -- FINANCE_MANAGER（财务经理）→ {财务部 FIN}
    --   理由：财务经理处理的是「财务部归口」的预算/费用/任务；跨部门单据（应付、采购）
    --   本来就是通过审批流与共享台账流转的，不需要靠「默认可见全公司」来干活。
    --   最小授权的取向：先给到本部门，确实不够再按需加集合（而不是反过来）。
    --
    -- FINANCE_DIRECTOR（财务总监）→ {财务部 FIN, 云枢总部 HQ}
    --   理由：总监多一层「复核总部各部门归口费用/预算」的职责，总部（HQ）是各部门的父节点；
    --   注意 CUSTOM 只匹配集合里的部门本身、**不展开子树**（scope.rs::any_filter 是 = ANY），
    --   所以这里显式列出两个部门而不是「给 HQ 就自动含下级」。
    INSERT INTO sys_role_dept (role_id, dept_id)
    SELECT r.id, d.id
      FROM sys_role r, sys_dept d
     WHERE r.role_code = 'FINANCE_MANAGER'
       AND d.dept_code = 'FIN'
       AND r.deleted_at IS NULL AND d.deleted_at IS NULL
    ON CONFLICT (role_id, dept_id) DO NOTHING;
    GET DIAGNOSTICS v_rows = ROW_COUNT;
    RAISE NOTICE 'V0247 ②a FINANCE_MANAGER → sys_role_dept(财务部 FIN)：新增 % 行', v_rows;

    INSERT INTO sys_role_dept (role_id, dept_id)
    SELECT r.id, d.id
      FROM sys_role r, sys_dept d
     WHERE r.role_code = 'FINANCE_DIRECTOR'
       AND d.dept_code IN ('FIN', 'HQ')
       AND r.deleted_at IS NULL AND d.deleted_at IS NULL
    ON CONFLICT (role_id, dept_id) DO NOTHING;
    GET DIAGNOSTICS v_rows = ROW_COUNT;
    RAISE NOTICE 'V0247 ②b FINANCE_DIRECTOR → sys_role_dept(财务部 FIN + 总部 HQ)：新增 % 行', v_rows;

    -- 只有「真的拿到了部门行」的角色才切 5（否则会静默降级成仅本人，见文件头）
    UPDATE sys_role r
       SET data_scope = 5,
           updated_at = CURRENT_TIMESTAMP
     WHERE r.role_code IN ('FINANCE_MANAGER', 'FINANCE_DIRECTOR')
       AND r.deleted_at IS NULL
       AND EXISTS (SELECT 1 FROM sys_role_dept rd WHERE rd.role_id = r.id);
    GET DIAGNOSTICS v_n = ROW_COUNT;
    RAISE NOTICE 'V0247 ②c 财务角色 → data_scope=5：% 行', v_n;

    FOR v_rows IN
        SELECT count(*) FROM sys_role r
         WHERE r.role_code IN ('FINANCE_MANAGER', 'FINANCE_DIRECTOR')
           AND r.deleted_at IS NULL AND r.data_scope = 1
    LOOP
        IF v_rows > 0 THEN
            RAISE NOTICE 'V0247 ②d 本库缺少 dept_code FIN/HQ（基础版空库），% 个财务角色保持 data_scope=1；组织树建好后用 API 补配', v_rows;
        END IF;
    END LOOP;

    -- ---------- ③ 明确**不动**的角色（写下来是决定，不是疏漏） ----------
    -- SUPER_ADMIN：data_scope=1 与「超管短路」是两件事——scope.rs::filter_for 里
    --   `principal.is_super()` 先恒真返回（S9），所以哪怕把超管配成 4 也看得见全部；
    --   把它显式留在 1 是为了让「管理界面显示的范围」和「实际行为」不打架。
    -- GENERAL_MANAGER（总经理）：职责本身就是全公司经营数据，1 是授权而不是缺省。
    -- FLOW_CONFIG_APPROVER（流程配置复核员）：只管流程定义与节点，不持单据数据；
    --   流程表（biz_flow_*）本来就不参与行级范围（见 docs 里「哪些表不受控」）。
    -- QA_LIMITED / QA_VIEWER / MUTEX_A / MUTEX_B：验收夹具角色，QA 用例依赖其权限码集合，
    --   改范围会让既有用例假红；它们不在业务岗位序列里。

    -- ---------- ④ 自检（跑完立刻验，不留「看起来改了」的错觉） ----------
    IF EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'SUPER_ADMIN'
                AND deleted_at IS NULL AND data_scope <> 1) THEN
        RAISE EXCEPTION 'V0247 自检失败：SUPER_ADMIN 的 data_scope 必须保持 1（不许把管理员限死）';
    END IF;

    IF EXISTS (SELECT 1 FROM sys_role r
                WHERE r.deleted_at IS NULL AND r.data_scope = 5
                  AND NOT EXISTS (SELECT 1 FROM sys_role_dept rd WHERE rd.role_id = r.id)) THEN
        RAISE EXCEPTION 'V0247 自检失败：存在 data_scope=5 但 sys_role_dept 无行的角色（持有者会被静默降级为仅本人）';
    END IF;

    -- 有组织树却没有**任何**部门级授权 = 行级数据权限又空转了：这种情况本迁移必须拦住
    IF EXISTS (SELECT 1 FROM sys_dept WHERE deleted_at IS NULL) THEN
        IF NOT EXISTS (
            SELECT 1 FROM sys_role r
             WHERE r.deleted_at IS NULL AND r.status = 1
               AND (r.data_scope IN (2, 3)
                    OR (r.data_scope = 5
                        AND EXISTS (SELECT 1 FROM sys_role_dept rd WHERE rd.role_id = r.id)))
        ) THEN
            RAISE EXCEPTION 'V0247 自检失败：本库已有组织树但没有任何角色的数据范围是部门级（行级数据权限仍是空转）';
        END IF;
    END IF;

    RAISE NOTICE 'V0247 自检通过：SUPER_ADMIN=1 / 5 必有集合 / 有组织树即有部门级授权';
END $$;
