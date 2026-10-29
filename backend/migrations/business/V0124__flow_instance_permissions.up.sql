-- ======================================================
-- V0124：审批流 · 实例侧权限节点（批次 D · D2）
--
-- 依据：卡面 t_3715c446「4. 实例侧 API（12–23）」的 12 个接口 + `docs/批次D-工作流审批-技术架构.md` §10.2。
--       D1 的 `V0121` 文件头明确预留：「D2/D3 落地实例侧时**追加**码（business:flow:instance:read /
--       business:flow:approve …），不改本文件的 5 个」。本文件就是那次追加。
--
-- ── 为什么必须有这条迁移（而不是「代码里写了码就行」） ─────────────
-- `#[alun::permission(permission = "...")]` 的码值必须在 `sys_menu.auth_code` 里**种过**，
-- 否则非超管角色在角色树上根本勾不到它 → 接口对所有人都 403（skill cailu-dev §9.13-6：
-- 「A7 报的后端缺路由，常常是权限点没种」）。超管有短路、验收能过，所以这个缺口
-- 在验收里是隐形的，只能在真实授权时炸。
--
-- ── 码 → 接口映射（`business/flow/handler.rs` 的注解逐字使用） ────
--   instance:create   → POST /business/flow/instance（发起）
--   instance:read     → GET  /business/flow/instance/page、/instance/{id}
--                       GET  /business/flow/task/todo、/task/done、/task/cc
--   instance:withdraw → POST /business/flow/instance/{id}/withdraw（只允许发起人本人，行级判定在 handler）
--   approve           → POST /business/flow/task/{id}/approve|reject|transfer|add-sign、/task/batch
--   admin             → 退回「任意节点」（return_to = node_key）这个管理动作的附加校验
--                       （中间件仍挂 `approve`，管理动作在 handler 里再查一次 `business:flow:admin`，
--                        与 §9.2「流程管理员不能代替审批人投票」一致）
--
-- ── 与 D1 的 5 个码的关系 ──────────────────────────────────────
-- 不改、不删、不覆盖 V0121 的 `business:flow:read/create/update/delete/publish`；
-- 本文件**只做加项**。同一个码不会被两条迁移同时插入（auth_code 唯一性由 UUID 主键兜底，
-- 且本文件的 5 个码与 V0121 的 5 个码字面不同）。
--
-- ── 菜单 UUID 段 ──────────────────────────────────────────────
-- 全库枚举（`grep -rhoE '20000000-0000-0000-0000-[0-9]{12}' migrations/ | sort -u`）
-- 已用到 …0790；本文件取 **…0800–…0804**（未占用，留 …0805–…0809 给 D3–D5）。
--
-- ── 授权（sys_role_menu）刻意的「不做」 ─────────────────────────
-- 与 V0121 同口径：本迁移只种**权限点**，不替任何角色授权。
-- 理由：① 种子迁移 `S__role_data.sql` 执行时机在版本化迁移之后，空库这里还没有角色；
-- ② 「谁能审单」是上线时的组织决策，不该被迁移写死；③ 超管有权限短路，不影响本卡验收。
-- 前端页面（待办 / 我的审批）在 D6 交付，角色勾选同批做。
--
-- 回滚：`V0124__flow_instance_permissions.down.sql`（连授权一起清，零残留）
-- ======================================================

-- ---------- ① 5 个按钮节点（父节点 = D1 建的流程定义页面 …0780） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000800'::uuid, 'business:flow:instance:create', 3,
     'business:flow:instance:create', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     30, 1, NULL, '发起审批（实例绑版本，引擎只读 graph_snapshot）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000801'::uuid, 'business:flow:instance:read', 3,
     'business:flow:instance:read', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     40, 1, NULL, '查看审批单 / 我的待办 / 我的已办 / 抄送我的', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000802'::uuid, 'business:flow:instance:withdraw', 3,
     'business:flow:instance:withdraw', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     50, 1, NULL, '撤回自己发起的审批（零人已审才允许）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000803'::uuid, 'business:flow:approve', 3,
     'business:flow:approve', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     60, 1, NULL, '审批（同意 / 驳回 / 转办 / 加签 / 批量）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000804'::uuid, 'business:flow:admin', 3,
     'business:flow:admin', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     70, 1, NULL, '流程管理员（退回任意节点 / 终止）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 自检：5 个码逐字就位、父节点正确 ----------
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000000780'::uuid;
    found     int;
    wrong     int;
BEGIN
    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000800'::uuid,
                  '20000000-0000-0000-0000-000000000801'::uuid,
                  '20000000-0000-0000-0000-000000000802'::uuid,
                  '20000000-0000-0000-0000-000000000803'::uuid,
                  '20000000-0000-0000-0000-000000000804'::uuid);
    IF found <> 5 THEN
        RAISE EXCEPTION 'V0124 自检失败：实例侧权限节点只存在 % 个（应为 5）', found;
    END IF;

    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000800'::uuid,
                  '20000000-0000-0000-0000-000000000801'::uuid,
                  '20000000-0000-0000-0000-000000000802'::uuid,
                  '20000000-0000-0000-0000-000000000803'::uuid,
                  '20000000-0000-0000-0000-000000000804'::uuid)
       AND (menu_type <> 3
            OR parent_id IS DISTINCT FROM page_menu
            OR status <> 1
            OR module_code <> 'business'
            OR is_visible);
    IF wrong > 0 THEN
        RAISE EXCEPTION 'V0124 自检失败：% 个节点的类型 / 父节点 / 状态 / 可见性不对', wrong;
    END IF;

    -- 码值与会话权限表逐字一致（拼错就是「勾不到」，是隐形故障）
    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000800'::uuid,
                  '20000000-0000-0000-0000-000000000801'::uuid,
                  '20000000-0000-0000-0000-000000000802'::uuid,
                  '20000000-0000-0000-0000-000000000803'::uuid,
                  '20000000-0000-0000-0000-000000000804'::uuid)
       AND auth_code IN ('business:flow:instance:create','business:flow:instance:read',
                         'business:flow:instance:withdraw','business:flow:approve',
                         'business:flow:admin');
    IF wrong <> 5 THEN
        RAISE EXCEPTION 'V0124 自检失败：5 个权限码只对上 % 个', wrong;
    END IF;

    RAISE NOTICE 'V0124 自检通过：5 个实例侧权限节点（instance:create/read/withdraw + approve + admin）';
END
$$;
