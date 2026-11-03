-- ======================================================
-- V0208：年结催办 —— 权限节点（YEAR-END）
--
-- 依据：kanban 卡 t_f530126c §3（年结催办接口）
--   `POST /business/erp/year-end/remind`，
--   权限码 `business:erp:year-end:remind`。
--
-- 为什么**另立一个码**，不复用 `yearend:close`：
--   「能催人」≠「能结转整年」。只读的财务助理可以催人，但不该拿到封年的能力；
--   反过来有年结权的人也不一定愿意被催办消息打扰。与 V0207（期间催办）同一套理由
--   —— 能看 ≠ 能封 ≠ 能撤封 ≠ 能催。
--
-- ⚠️ 命名说明：V0190 种的三个年结码是 `business:erp:yearend:*`（连写），
--   而本卡**点名要求** `business:erp:year-end:remind`（带连字符）。这里照卡面执行，
--   并把「一码一节点」写进下面的自检：前端 `YEAR_END_PERMISSIONS.remind`、
--   后端 `year_end_remind::PERM_REMIND`、本迁移三处必须是同一个串，拼错任何一处都会 403。
--
-- 本迁移**只种权限节点**：
--   · 不建表 —— 24 小时防重复复用 `sys_notice`（`biz_type = 'year_end_remind'`，
--     与期间催办的 `period_remind` 分开，两边 24h 防重互不干扰）；
--   · 不改 `biz_status_log` / `biz_voucher` 任何结构。
--
-- 节点（menu_type = 3 = 按钮，挂「年度结转」页面 …1025 下）：
--   1213 business:erp:year-end:remind  催办（把年结待办发给处理人）
--
-- 授权口径（与 V0188 / V0207 同一写法：角色不存在就 0 行，不报错）：
--   SUPER_ADMIN / ADMIN / FINANCE / ACCOUNTANT —— 能年结的人都该能催人；
--   `SUPER_ADMIN` 在框架里本来就短路全部校验，这里种上是为了权限管理页可见可授。
--
-- 幂等：`ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS`，重复执行不报错。
-- 回滚：`V0208__erp_year_end_remind.down.sql`（只删该节点与其授权行；
--       已经发出去的年结催办通知属于审计/沟通记录，一行不动）
-- ======================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001213'::uuid, 'business:erp:year-end:remind', 3,
     'business:erp:year-end:remind', NULL,
     '20000000-0000-0000-0000-000000001025'::uuid, NULL,
     30, 1, NULL, '催办', FALSE, 'business')
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
     WHERE id = '20000000-0000-0000-0000-000000001213'::uuid
       AND auth_code = 'business:erp:year-end:remind'
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000001025'::uuid;
    IF n_node <> 1 THEN
        RAISE EXCEPTION 'V0208 自检失败：年结催办权限节点没种上（% 行）', n_node;
    END IF;

    -- 一码一节点：不得与已有节点撞码
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE auth_code = 'business:erp:year-end:remind'
       AND deleted_at IS NULL;
    IF n_bad <> 1 THEN
        RAISE EXCEPTION 'V0208 自检失败：年结催办码在 sys_menu 里出现 % 次（应为 1）', n_bad;
    END IF;

    -- ---------- 授权（空库首启时 sys_role 可能还没有行，跳过并提示）----------
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0208：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001213'::uuid
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE', 'ACCOUNTANT')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm
                 WHERE rm.role_id = r.id
                   AND rm.menu_id = '20000000-0000-0000-0000-000000001213'::uuid
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;
        RAISE NOTICE 'V0208：年结催办权限已种，授权新增 % 行', n_grants;
    END IF;

    RAISE NOTICE 'V0208 自检通过：年结催办 1 个权限节点';
END $$;
