-- ======================================================
-- V0121：审批流 · 定义侧权限节点（批次 D · D1）
--
-- 依据：卡面 t_a92e9d5e「5. V0118 权限种子：`business:flow:read/create/update/delete/publish` + 菜单节点」
--       + `docs/批次D-工作流审批-技术架构.md` §10.2（权限码清单）/ §6.4（菜单 UUID 段）
-- 回滚：`V0121__flow_def_permissions.down.sql`（连授权一起清，零残留）
--
-- ── 为什么编号是 V0121（不是文档的 V0118） ──────────────────────
-- 与 V0120 同一原因：V0115 已被 T7c「项目分组」占用，本卡整体后移，
-- 让 D2–D6 继续使用文档排给它们的 V0116–V0119。
--
-- ── 码位口径（与文档 §10.2 的差异，必须先对齐） ─────────────────
-- 文档给的是 **10 个码**、定义侧叫 `business:flow:def:read/write/publish`；
-- 卡面（D1 的验收契约）明确要求 **5 个码**：`business:flow:read/create/update/delete/publish`。
-- 本卡按**卡面**落地，理由：
--   · D1 只交付定义侧，实例侧（instance:read / approve / withdraw / admin / stats:read）在 D2/D3；
--   · 5 个码与 `business:project:*` / `business:task:*` 的既有命名段一致（read/create/update/delete/publish），
--     前端角色树与运维心智不用为流程单独记一套 `def:` 前缀。
-- D2/D3 落地实例侧时**追加**码（`business:flow:instance:read` / `business:flow:approve` …），不改本文件的 5 个。
--
-- 码 → 接口映射（`business/flow/handler.rs` 的 `#[alun::permission]` 注解逐字使用）：
--   read    → GET  /business/flow/def/page、/def/{id}、/def/{id}/versions、/version/{vid}
--   create  → POST /business/flow/def
--   update  → PUT  /business/flow/def/{id}、PUT /version/{vid}/graph、POST /version/{vid}/validate、
--             POST /business/flow/def/{id}/draft（另存为新草稿）
--   delete  → DELETE /business/flow/def/{id}
--   publish → POST /business/flow/def/{id}/publish、POST /business/flow/def/{id}/offline
--
-- ── 菜单 UUID 段 ──────────────────────────────────────────────
-- 全库枚举（`grep -rhoE '20000000-0000-0000-0000-[0-9]{12}' migrations/ | sort -u`）确认
-- …0780–…0789 未被占用（…0775–…0779 归属 T7c 项目分组，…0790 是文档结构根）。本卡取 …0780–…0785。
--
-- ── is_visible = FALSE（页面节点） ─────────────────────────────
-- 卡片只要求「菜单节点存在」；流程编辑器页面在 D6 才建。**指向不存在前端路由的可见菜单 = 坏链接**，
-- 所以页面节点先登记为隐藏（`is_visible = FALSE`），D6 建好 `business/flow/def/index` 后
-- 用一条 UPDATE 把它放出来（D6 卡内执行，不在本迁移里预置「假可见」）。
--
-- ── 幂等 ────────────────────────────────────────────────────
-- 固定 UUID + `ON CONFLICT (id) DO NOTHING`：重复执行不报错，也不覆盖运维改名后的标题。
-- ======================================================

-- ---------- ① 页面节点 + 5 个按钮节点 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    -- 页面：流程定义管理（前端页面 D6 交付，故 is_visible=FALSE，见文件头）
    ('20000000-0000-0000-0000-000000000780'::uuid, '流程定义', 2, 'business:flow:read',
     '/flow/def', NULL, 'business/flow/def/index',
     68, 1, 'flow', '审批流程定义（编排 / 发布 / 版本）', FALSE, 'business'),
    -- 按钮：查看
    ('20000000-0000-0000-0000-000000000781'::uuid, 'business:flow:read', 3,
     'business:flow:read', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     10, 1, NULL, '查看流程定义 / 版本 / 图', FALSE, 'business'),
    -- 按钮：新建
    ('20000000-0000-0000-0000-000000000782'::uuid, 'business:flow:create', 3,
     'business:flow:create', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     20, 1, NULL, '新建流程（自动建 v1 草稿）', FALSE, 'business'),
    -- 按钮：编辑
    ('20000000-0000-0000-0000-000000000783'::uuid, 'business:flow:update', 3,
     'business:flow:update', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     30, 1, NULL, '编辑流程元信息 / 保存整图 / 另存新草稿', FALSE, 'business'),
    -- 按钮：删除
    ('20000000-0000-0000-0000-000000000784'::uuid, 'business:flow:delete', 3,
     'business:flow:delete', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     40, 1, NULL, '删除流程（软删；有在跑审批单时拒绝）', FALSE, 'business'),
    -- 按钮：发布 / 停用
    ('20000000-0000-0000-0000-000000000785'::uuid, 'business:flow:publish', 3,
     'business:flow:publish', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     50, 1, NULL, '发布（跑 12 条校验）/ 停用流程', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000000780'::uuid;
    found     int;
    wrong     int;
BEGIN
    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id IN (page_menu,
                  '20000000-0000-0000-0000-000000000781'::uuid,
                  '20000000-0000-0000-0000-000000000782'::uuid,
                  '20000000-0000-0000-0000-000000000783'::uuid,
                  '20000000-0000-0000-0000-000000000784'::uuid,
                  '20000000-0000-0000-0000-000000000785'::uuid)
       AND deleted_at IS NULL;
    IF found <> 6 THEN
        RAISE EXCEPTION 'V0121 自检失败：流程定义权限节点只存在 % 个（应为 6）', found;
    END IF;

    -- 页面节点：类型 2、auth_code = business:flow:read、component 与前端逐字一致
    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id = page_menu
       AND (menu_type <> 2
            OR auth_code <> 'business:flow:read'
            OR component <> 'business/flow/def/index'
            OR path <> '/flow/def'
            OR module_code <> 'business');
    IF wrong > 0 THEN
        RAISE EXCEPTION 'V0121 自检失败：页面节点的类型 / auth_code / component / path 不对';
    END IF;

    -- 5 个按钮：类型 3、父节点是页面、auth_code 逐字正确
    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000781'::uuid,
                  '20000000-0000-0000-0000-000000000782'::uuid,
                  '20000000-0000-0000-0000-000000000783'::uuid,
                  '20000000-0000-0000-0000-000000000784'::uuid,
                  '20000000-0000-0000-0000-000000000785'::uuid)
       AND (menu_type <> 3 OR parent_id IS DISTINCT FROM page_menu OR status <> 1);
    IF wrong > 0 THEN
        RAISE EXCEPTION 'V0121 自检失败：% 个按钮节点的类型 / 父节点 / 状态不对', wrong;
    END IF;

    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE auth_code IN ('business:flow:read','business:flow:create','business:flow:update',
                         'business:flow:delete','business:flow:publish')
       AND id IN ('20000000-0000-0000-0000-000000000781'::uuid,
                  '20000000-0000-0000-0000-000000000782'::uuid,
                  '20000000-0000-0000-0000-000000000783'::uuid,
                  '20000000-0000-0000-0000-000000000784'::uuid,
                  '20000000-0000-0000-0000-000000000785'::uuid);
    IF wrong <> 5 THEN
        RAISE EXCEPTION 'V0121 自检失败：5 个权限码只对上 % 个', wrong;
    END IF;

    RAISE NOTICE 'V0121 自检通过：1 个页面节点 + 5 个权限按钮（business:flow:read/create/update/delete/publish）';
END
$$;
