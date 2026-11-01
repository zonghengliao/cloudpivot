-- ======================================================
-- V0188：会计期间开账 / 结账 / 反结账 —— 权限节点（ERP-P1）
--
-- 依据：kanban 卡 t_436ef985 §一 / §五 硬红线 4
--   · 开账、结账、反结账是**三个动作**，各自一个权限码，不复用 `:list`：
--     「能看期间列表」≠「能把整个月封账」。混成一个码会让只读的会计
--     （看报表要 `period:list`）顺手拿到封账能力。
--   · 本迁移**只种权限节点**，不建表、不改 biz_accounting_period 结构：
--     期间表 V0137 已有 status / closed_at / closed_by 三列，够用。
--
-- 节点（menu_type = 3 = 按钮，挂「会计期间」页面 …0951 下）：
--   1019 business:erp:period:open    开账（建期间 / 批量开账）
--   1020 business:erp:period:close   结账（含 GET close-check 预检）
--   1021 business:erp:period:reopen  反结账（高危，默认只给超管 / 财务总监）
--
-- 授权口径（与 V0185 同一写法：角色不存在就 0 行，不报错）：
--   · open / close → SUPER_ADMIN / ADMIN / FINANCE / ACCOUNTANT
--   · reopen       → SUPER_ADMIN / FINANCE（反结账是撤销已封账的动作，只给财务总监）
--   `SUPER_ADMIN` 在框架里本来就短路全部校验，这里种上是为了权限管理页可见可授。
--
-- 幂等：`ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS`，重复执行不报错。
-- 回滚：`V0188__erp_period_close.down.sql`（只删这三个节点与其授权行，
--       期间数据 / 状态日志一行不动 —— 反结账留痕属于审计证据，不该被回滚抹掉）
-- ======================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001019'::uuid, 'business:erp:period:open', 3,
     'business:erp:period:open', NULL,
     '20000000-0000-0000-0000-000000000951'::uuid, NULL,
     2, 1, NULL, '开账', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001020'::uuid, 'business:erp:period:close', 3,
     'business:erp:period:close', NULL,
     '20000000-0000-0000-0000-000000000951'::uuid, NULL,
     3, 1, NULL, '结账', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001021'::uuid, 'business:erp:period:reopen', 3,
     'business:erp:period:reopen', NULL,
     '20000000-0000-0000-0000-000000000951'::uuid, NULL,
     4, 1, NULL, '反结账', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

DO $$
DECLARE
    n_open   integer;
    n_close  integer;
    n_reopen integer;
    n_roles  integer;
    n_grants integer;
    n_bad    integer;
BEGIN
    -- ---------- 节点自检 ----------
    SELECT count(*) INTO n_open
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001019'::uuid
       AND auth_code = 'business:erp:period:open'
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000000951'::uuid;
    SELECT count(*) INTO n_close
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001020'::uuid
       AND auth_code = 'business:erp:period:close'
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000000951'::uuid;
    SELECT count(*) INTO n_reopen
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001021'::uuid
       AND auth_code = 'business:erp:period:reopen'
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000000951'::uuid;
    IF n_open <> 1 OR n_close <> 1 OR n_reopen <> 1 THEN
        RAISE EXCEPTION
            'V0188 自检失败：期间三码节点没种全（open=% close=% reopen=%）',
            n_open, n_close, n_reopen;
    END IF;

    -- 三个码必须互不相同，且不得与已有节点撞码（一码一节点）
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE auth_code IN ('business:erp:period:open',
                         'business:erp:period:close',
                         'business:erp:period:reopen')
       AND deleted_at IS NULL;
    IF n_bad <> 3 THEN
        RAISE EXCEPTION 'V0188 自检失败：期间三码在 sys_menu 里出现 % 次（应为 3）', n_bad;
    END IF;

    -- ---------- 授权（空库首启时 sys_role 可能还没有行，跳过并提示）----------
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0188：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001019'::uuid),
                ('20000000-0000-0000-0000-000000001020'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE', 'ACCOUNTANT')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;

        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001021'::uuid
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'FINANCE')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm
                 WHERE rm.role_id = r.id
                   AND rm.menu_id = '20000000-0000-0000-0000-000000001021'::uuid
           );
        GET DIAGNOSTICS n_roles = ROW_COUNT;
        n_grants := n_grants + n_roles;

        RAISE NOTICE 'V0188：期间三码已种，授权新增 % 行', n_grants;
    END IF;

    RAISE NOTICE 'V0188 自检通过：开账/结账/反结账 3 个权限节点';
END $$;
