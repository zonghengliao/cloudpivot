-- ============================================================================
-- 员工自助（EMPLOYEE）角色 + 报销自助提报授权 —— 可重复种子（S__ = repeatable）
--
-- 依据：kanban 卡 t_92234f6b「演示种子缺口：5 个账号无部门（N2 WARN）+ 普通员工无
--       expense:submit（员工自助提报 403）」现象②。
--
-- 实测背景（145 / 全新库 cloudpivot_rev_demo，NIK_SEED_DEMO_USERS=true）：
--   `business:erp:expense:submit` 只授给了 DEPT_LEADER / FINANCE_MANAGER / FINANCE_DIRECTOR，
--   普通员工 demo_sales（SALES_CLERK）提交自己的报销单 → 403。
--   于是演示库上「员工提报 → 部门经理审批」这条链**只能由财务代提**才跑得通，
--   而部门经理本人（demo_lead）提报又会撞职责分离 65018。这不是回归，
--   是权限矩阵里**根本没有「员工」这一档**。
--
-- ── 为什么新建角色，而不是把报销码塞给 SALES_CLERK ───────────────────────
--   ① 「销售员」不是「员工」的同义词：本仓还有采购员 / 仓管 / 未来的任何岗位，
--      把报销码挂到 SALES_CLERK 上，采购员、仓管自己提报照样 403 —— 缺口只被挪了个位置；
--   ② 角色矩阵（V0258 / V0267）的语义按**岗位**切分，`EMPLOYEE` 是横切的**身份**角色：
--      「谁在这家公司上班」与「他做什么岗位」是两件事，一个人可以同时是
--      销售员（SALES_CLERK）和员工（EMPLOYEE）；
--   ③ 演示账号 `demo_sales / demo_purchase / demo_wh` 因此同时持岗位角色 + EMPLOYEE
--      （见 `backend/src/system/setup/model.rs` 的 DEMO_USERS）。
--
-- ── 为什么不改权限码 / 前端不用动 ────────────────────────────────────────
--   整包用的都是**既有** 8 个 `business:erp:expense*` 码 + `business:flow:instance:read`，
--   一个一个都在 `expense_handler.rs` / `flow/handler.rs` 的 `#[nik::permission]` 里存在
--   （AGENTS.md 约定 7：权限码与注解码一一对应，绝不给「点了必然 403」的哑弹建行）。
--   前端 `views/business/erp/expense/index.vue` 的 PERM 表一行没变 ⇒ 按钮显隐零改动。
--
-- ── 为什么放在 S__ 种子而不是版本化迁移 ─────────────────────────────────
--   · 版本化迁移一号一段位，本卡只是**数据**（角色 + 授权），不该吃掉 V0332 这种号段；
--   · 种子是 repeatable：老库（已应用完 331 条迁移的 145 / 144）下次启动会自动补上，
--     不需要再追加一条「只补数据」的空迁移；
--   · 幂等靠 `ON CONFLICT (role_code) WHERE deleted_at IS NULL` + `NOT EXISTS`，
--     重放 = 0 行插入。
--
-- ── 授权范围（13 行菜单：2 个目录闭包 + 3 个页面 + 8 个按钮）─────────────
--   财务管理(目录 e02) ⇐ 祖先闭包，缺了非超管侧边栏整棵子树被静默丢掉（V0258 第五节）
--   ├─ 费用报销(目录 1500)
--   │  ├─ 我的报销(页 1501)         business:erp:expense:list        进页面 / 看自己的单
--   │  │  ├─ 新建                    business:erp:expense:create
--   │  │  ├─ 编辑（草稿）            business:erp:expense:update
--   │  │  ├─ 删除（草稿）            business:erp:expense:delete
--   │  │  ├─ 提交审批                business:erp:expense:submit       ← 本卡的核心缺口
--   │  │  └─ 详情                    business:erp:expense:detail
--   │  └─ 费用类型(页 1503)         business:erp:expense-type:list   建单下拉取数（只读页）
--  审批流(目录 0860) → 审批中心(页 0862)  business:flow:instance:read   详情抽屉看审批进度
--  单据附件（V0212 的 3 个隐藏按钮，无独立页面）
--   ├─ business:attachment:upload    上传自己单据的发票影像 / 费用凭证   ← t_1fafa2ae 补
--   └─ business:attachment:list      看 / 下载自己单据的附件（含提交前的附件闸 check）
--
--   **不授**：expense:pay（付款）/ expense:cancel（红冲作废）/ expense-type:create|update|delete
--   —— 员工的边界是不碰钱、不碰账：付款与作废是 FINANCE_MANAGER 的事，多授一行就等于
--   「让员工自己给自己付钱」。
--   **也不授** `business:attachment:delete`（删除**他人**上传的附件）：行级规则是
--   「上传人可删自己的」，上传人本人那一条由 handler 放行，不需要这个升权码。
--
-- ── 为什么要补 attachment:upload / list（t_1fafa2ae · E2E-06 实测）────────
--   `biz_attachment_rule` 里 `EXPENSE_REPORT / required_at=SUBMIT / min_count=1`：提交报销
--   必须已有 ≥1 个发票影像。而这两条权限此前只授给 6 个管理岗（V0212 / V0258 / V0267），
--   员工调 `POST /business/attachment/upload` 一律 403 ⇒ **自助报销断在第一步**，
--   只能找财务代传发票才是「一条能跑通的流程」。补上之后：
--     · upload：员工自己传发票（单据读码仍把门，看不见的单传不进去 —— ATT-04）；
--     · list  ：详情抽屉的附件面板 / 提交前的 `/attachment/check` 闸都要它
--               （写接口挂 upload，读接口挂 list；只给 upload 会出现「传得上去看不见」）。
--   两者都**不越过单据可见性**：附件可见性 = 单据可见性（ATT-02），员工只看得见
--   自己部门能读的单据（本角色 data_scope=4）。
--
--   `sys_role.data_scope = 4`（仅本人）：报销单列表按 `dept_id` 做行级范围
--   （`expense_model::REPORT_TARGET` 只声明了部门列、没有归属人列），于是「仅本人」在
--   `scope::filter_for` ③ 回落成「本部门」—— 这是既有实现的既有口径，本卡不碰
--   （改它等于改业务规则）。**生产默认不受影响**：本角色 0 人绑定 = 零行为变化，
--   演示账号的绑定仍由 `NIK_SEED_DEMO_USERS` 开关控制（默认关）。
-- ============================================================================

-- ---------- ① 角色 EMPLOYEE（员工自助）：不存在才建 ----------
-- ⚠ 必须是 `VALUES + ON CONFLICT … DO NOTHING` 这种形状：`role_code_gate.py` 只从
--   `INSERT INTO sys_role … VALUES …` 解析「本文件种出来的角色」并并入白名单。
INSERT INTO sys_role (id, role_code, role_name, description, data_scope,
                      status, sort_order, remark, created_at, updated_at)
VALUES
    ('20000000-0000-4000-8000-0000000f0009'::uuid, 'EMPLOYEE', '员工自助',
     '身份角色（不是岗位）：任何在职员工都可以自助提报自己的费用报销（建单/改草稿/提交/撤回/看自己的单），不碰付款与红冲作废',
     4, 1, 90,
     't_92234f6b 种：员工自助。与岗位角色（SALES_CLERK / PURCHASE_MANAGER / WAREHOUSE_MANAGER）叠加使用',
     now(), now())
ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ② 授权（13 行，含祖先目录闭包）----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES
        -- 祖先目录闭包（缺一行 ⇒ 非超管侧边栏整棵子树消失）
        ('20000000-0000-0000-0000-000000000e02'::uuid),  -- 财务管理（目录）
        ('20000000-0000-0000-0000-000000001500'::uuid),  -- 费用报销（目录）
        ('20000000-0000-0000-0000-000000000860'::uuid),  -- 审批流（目录）
        -- 页面
        ('20000000-0000-0000-0000-000000001501'::uuid),  -- 我的报销   business:erp:expense:list
        ('20000000-0000-0000-0000-000000001503'::uuid),  -- 费用类型   business:erp:expense-type:list
        ('20000000-0000-0000-0000-000000000862'::uuid),  -- 审批中心   business:flow:instance:read
        -- 「我的报销」页的按钮
        ('20000000-0000-0000-0000-000000001511'::uuid),  -- business:erp:expense:create
        ('20000000-0000-0000-0000-000000001512'::uuid),  -- business:erp:expense:update
        ('20000000-0000-0000-0000-000000001513'::uuid),  -- business:erp:expense:delete
        ('20000000-0000-0000-0000-000000001514'::uuid),  -- business:erp:expense:submit  ← 本卡核心
        ('20000000-0000-0000-0000-000000001516'::uuid),  -- business:erp:expense:detail
        -- 单据附件（隐藏按钮，挂在「通用底座」根上，不影响侧边栏）
        ('20000000-0000-0000-0000-000000001221'::uuid),  -- business:attachment:upload  ← t_1fafa2ae
        ('20000000-0000-0000-0000-000000001222'::uuid)   -- business:attachment:list    ← t_1fafa2ae
     ) AS m(id)
 WHERE r.role_code = 'EMPLOYEE'
   AND r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ③ 自检（任一不符 → 启动失败；宁可炸在启动期，也别让「员工点提交 403」上线）----------
DO $$
DECLARE
    v_role_id    uuid;
    v_scope      smallint;
    v_codes      text[] := ARRAY[
                     'business:erp:expense:list', 'business:erp:expense:create',
                     'business:erp:expense:update', 'business:erp:expense:delete',
                     'business:erp:expense:submit', 'business:erp:expense:detail',
                     'business:erp:expense-type:list', 'business:flow:instance:read',
                     'business:attachment:upload', 'business:attachment:list'];
    v_want_menus uuid[] := ARRAY[
                     '20000000-0000-0000-0000-000000000e02'::uuid,
                     '20000000-0000-0000-0000-000000001500'::uuid,
                     '20000000-0000-0000-0000-000000000860'::uuid,
                     '20000000-0000-0000-0000-000000001501'::uuid,
                     '20000000-0000-0000-0000-000000001503'::uuid,
                     '20000000-0000-0000-0000-000000000862'::uuid,
                     '20000000-0000-0000-0000-000000001511'::uuid,
                     '20000000-0000-0000-0000-000000001512'::uuid,
                     '20000000-0000-0000-0000-000000001513'::uuid,
                     '20000000-0000-0000-0000-000000001514'::uuid,
                     '20000000-0000-0000-0000-000000001516'::uuid,
                     '20000000-0000-0000-0000-000000001221'::uuid,
                     '20000000-0000-0000-0000-000000001222'::uuid];
    v_n          int;
    v_str        text;
BEGIN
    -- 3.1 角色在库且启用
    SELECT id, data_scope INTO v_role_id, v_scope
      FROM sys_role WHERE role_code = 'EMPLOYEE' AND deleted_at IS NULL;
    IF v_role_id IS NULL THEN
        RAISE EXCEPTION 'S__employee_self_service_permissions 自检失败：角色 EMPLOYEE 不存在（① 插入被跳过？）';
    END IF;
    IF v_scope <> 4 THEN
        RAISE EXCEPTION 'S__employee_self_service_permissions 自检失败：EMPLOYEE 的 data_scope = %（期望 4 仅本人）', v_scope;
    END IF;

    -- 3.2 10 个权限码必须都有菜单行承载（否则授了也用不上 / 授权行写给了空气）
    SELECT string_agg(want, ', ') INTO v_str
      FROM unnest(v_codes) AS want
     WHERE NOT EXISTS (SELECT 1 FROM sys_menu m WHERE m.auth_code = want AND m.deleted_at IS NULL);
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'S__employee_self_service_permissions 自检失败：以下权限码在 sys_menu 里没有承载 → %', v_str;
    END IF;

    -- 3.3 13 行菜单全部真持有（空跑 = 报警，不静默）
    SELECT count(*) INTO v_n
      FROM sys_role_menu rm
     WHERE rm.role_id = v_role_id AND rm.menu_id = ANY (v_want_menus);
    IF v_n <> array_length(v_want_menus, 1) THEN
        SELECT string_agg(want::text, ', ') INTO v_str
          FROM unnest(v_want_menus) AS want
         WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                            WHERE rm.role_id = v_role_id AND rm.menu_id = want);
        RAISE EXCEPTION 'S__employee_self_service_permissions 自检失败：EMPLOYEE 只持有 %/13 行授权，缺 → %',
            v_n, v_str;
    END IF;

    -- 3.4 无孤儿按钮：每个被授的按钮码，其父页的码必须也授给了同一角色
    SELECT string_agg(child.auth_code, ', ') INTO v_str
      FROM sys_menu child
      JOIN sys_menu parent ON parent.id = child.parent_id AND parent.deleted_at IS NULL
      JOIN sys_role_menu rm  ON rm.menu_id = child.id AND rm.role_id = v_role_id
     WHERE child.deleted_at IS NULL
       AND child.menu_type = 3
       AND coalesce(parent.auth_code, '') <> ''
       AND NOT EXISTS (SELECT 1 FROM sys_role_menu prm
                        WHERE prm.role_id = v_role_id AND prm.menu_id = parent.id);
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'S__employee_self_service_permissions 自检失败：孤儿按钮（父页未授权）→ %', v_str;
    END IF;

    -- 3.5 反向：不许把「碰钱 / 碰账 / 删他人附件」的码捎带进来
    --     （多授一行 = 员工能自己付款 / 能删别人的凭证影像）
    SELECT string_agg(m.auth_code, ', ') INTO v_str
      FROM sys_role_menu rm
      JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
     WHERE rm.role_id = v_role_id
       AND m.auth_code IN ('business:erp:expense:pay', 'business:erp:expense:cancel',
                           'business:erp:expense-type:create', 'business:erp:expense-type:update',
                           'business:erp:expense-type:delete',
                           -- 附件「删他人上传的」是升权码；上传人删自己的由 handler 行级放行
                           'business:attachment:delete');
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'S__employee_self_service_permissions 自检失败：EMPLOYEE 被过度授权（越权码）→ %', v_str;
    END IF;

    RAISE NOTICE 'S__employee_self_service_permissions 自检通过：EMPLOYEE 持有 13 行菜单 / 10 个权限码（含 expense:submit 与 attachment:upload/list），data_scope=4';
END $$;
