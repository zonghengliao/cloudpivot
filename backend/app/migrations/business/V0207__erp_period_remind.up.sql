-- ======================================================
-- V0207：结账催办 —— 权限节点（PERIOD-CLOSE）
--
-- 依据：kanban 卡 t_1d3a6fce §3（催办接口）
--   `POST /business/erp/period/{id}/remind`，
--   权限码 `business:erp:period:remind`。
--
-- 为什么**另立一个码**，不复用 `period:close`：
--   「能给处理人发催办」≠「有资格替他把单过账」。只读的财务助理可以催人，
--   但不该拿到封账能力；反过来有结账权的人也不一定愿意被催办消息打扰。
--   与 V0188 的三码同一套理由（能看 ≠ 能封 ≠ 能撤封）。
--
-- 本迁移**只种权限节点**：
--   · 不建表 —— 24 小时防重复用的是 `sys_notice`（`biz_type = 'period_remind'`），
--     通知系统已有的幂等键 + 最近记录查询足够，不另造台账；
--   · 不改 `biz_accounting_period` 结构。
--
-- 节点（menu_type = 3 = 按钮，挂「会计期间」页面 …0951 下）：
--   1212 business:erp:period:remind  催办（把结账待办发给处理人）
--        （原计划 …1022，实测已被 V0195 的「期初余额」页面占用，故顺延到 1212）
--
-- 授权口径（与 V0188 同一写法：角色不存在就 0 行，不报错）：
--   SUPER_ADMIN / ADMIN / FINANCE / ACCOUNTANT —— 能结账的人都该能催人；
--   `SUPER_ADMIN` 在框架里本来就短路全部校验，这里种上是为了权限管理页可见可授。
--
-- 幂等：`ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS`，重复执行不报错。
-- 回滚：`V0207__erp_period_remind.down.sql`（只删该节点与其授权行；
--       已经发出去的催办通知属于审计/沟通记录，一行不动）
-- ======================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001212'::uuid, 'business:erp:period:remind', 3,
     'business:erp:period:remind', NULL,
     '20000000-0000-0000-0000-000000000951'::uuid, NULL,
     5, 1, NULL, '催办', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

DO $$
DECLARE
    n_node   integer;
    n_bad    integer;
    n_roles  integer;
    n_grants integer;
BEGIN
    -- ---------- 节点自检 ----------
    SELECT count(*) INTO n_node
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001212'::uuid
       AND auth_code = 'business:erp:period:remind'
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000000951'::uuid;
    IF n_node <> 1 THEN
        RAISE EXCEPTION 'V0207 自检失败：催办权限节点没种上（% 行）', n_node;
    END IF;

    -- 一码一节点：不得与已有节点撞码
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE auth_code = 'business:erp:period:remind'
       AND deleted_at IS NULL;
    IF n_bad <> 1 THEN
        RAISE EXCEPTION 'V0207 自检失败：催办码在 sys_menu 里出现 % 次（应为 1）', n_bad;
    END IF;

    -- ---------- 授权（空库首启时 sys_role 可能还没有行，跳过并提示）----------
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0207：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001212'::uuid
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE', 'ACCOUNTANT')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm
                 WHERE rm.role_id = r.id
                   AND rm.menu_id = '20000000-0000-0000-0000-000000001212'::uuid
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;
        RAISE NOTICE 'V0207：催办权限已种，授权新增 % 行', n_grants;
    END IF;

    RAISE NOTICE 'V0207 自检通过：结账催办 1 个权限节点';
END $$;
