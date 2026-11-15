-- ======================================================
-- V0115：任务管理 · 项目分组（批次 C+ · T7c）
--
-- 依据：卡面 t_b9f70dab「一、后端 / 权限码（V0115 种子）」+ 规则 1–6；
--       docs/批次C+-工作任务管理-核心设计.md（`biz_project` 是 T1 预留的「逻辑分组」）
--
-- 回滚：V0115__project_grouping.down.sql（逆序，零残留）
--
-- ── 本迁移做四件事，逐条说明「为什么必须动库」 ──────────────────
--
-- ① `biz_project_member` 表（**T1 漏建**）
--    卡面背景写的是「T1 建了 biz_project / biz_project_member / biz_task.project_id」，
--    但全仓 `grep -rn project_member` 在 V0100–V0114 与 src/ 里**零命中** —— 成员表从来没建过。
--    而卡面规则 6（「项目删除时若有成员 → 成员记录级联删」）与验收 6（「B 是项目成员」）
--    都以它为前提，所以本迁移补建。语义：**成员只是「参与这个项目」的标签**，
--    不构成任务可见性来源（可见性仍只走 T1 的 `scope ∪ 参与关系`）。
--
-- ② `biz_project.project_no` 的唯一索引（软删过滤）
--    卡面规则 3「`project.code` 唯一（软删过滤）」。库里对应的列是 `project_no`
--    （V0100 建表时的「项目编号」），**不新增列**：API 字段 `code` 与 DB 列
--    `project_no` 的映射写在本文件与 `business/project/dto.rs`，避免同一语义两列并存。
--    部分唯一索引 `WHERE deleted_at IS NULL`：软删的项目号可以被新项目复用，
--    但**未删**的项目号全局唯一（handler 侧仍先查一次给 400 中文文案，索引是最后一道闸）。
--    存量行 `project_no` 全为 NULL（V0100 注释「本卡不填」），PG 的部分唯一索引
--    允许多个 NULL，不会卡住升级。
--
-- ③ 三条列表索引
--    · `idx_biz_project_owner`：项目页「负责人」筛选 + 可见性谓词的 `owner_id` 分支；
--    · `idx_biz_project_status`：状态筛选（ACTIVE / ARCHIVED），部分索引排除已软删；
--    · `idx_biz_project_member_user`：可见性谓词的「我是成员」id 集合（`user_id` 在前）。
--
-- ④ 5 个权限节点（菜单 1 + 按钮 4）
--    `business:project:read` / `create` / `update` / `delete`
--    菜单节点 path `/project`（前端路由 `business/project/index`，手工注册在
--    `frontend/src/router/index.ts`），component 与 sys_menu.path 必须与前端逐字一致。
--
-- ── 为什么授权不在本文件 ────────────────────────────────────────
-- 版本化迁移（V\d+）先跑、种子（S__*.sql）后跑；空库上本文件执行时
-- `S__role_data.sql` 还没建出 SUPER_ADMIN 角色（V0103 / V0113 都踩过）。
-- 授权写在 `migrations/seed/S__project_permissions.sql`。
--
-- ── 菜单 UUID 段 ──────────────────────────────────────────────
-- …0760–…0774 已被 V0103 / V0108 / V0109 / V0110 / V0113 占用，…0790 亦已占用；
-- 本迁移取 **…0775–…0779**（`migrations/` 全库 grep 过，此前未使用）。
--
-- ── 幂等 ──────────────────────────────────────────────────────
-- `CREATE TABLE IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS` + 固定 UUID +
-- `ON CONFLICT (id) DO NOTHING`：重复执行不报错，也不覆盖运维改名后的菜单标题。
-- ======================================================

-- ---------- ① 项目成员表（T1 漏建，本卡补） ----------
CREATE TABLE IF NOT EXISTS biz_project_member (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    project_id  UUID NOT NULL REFERENCES biz_project(id) ON DELETE CASCADE, -- 项目（物理删除时级联）
    user_id     UUID NOT NULL REFERENCES sys_user(id), -- 成员（sys_user.id）
    role        VARCHAR(16) NOT NULL DEFAULT 'MEMBER'
        CHECK (role IN ('MEMBER', 'VIEWER')), -- MEMBER 成员 / VIEWER 只读成员
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(), -- 加入时间
    create_by   UUID, -- 操作人
    CONSTRAINT uk_biz_project_member UNIQUE (project_id, user_id)
);

COMMENT ON TABLE biz_project_member IS
    '项目成员：**只表示「参与这个项目」**，不构成任务可见性来源（可见性仍走 biz_task 的 scope ∪ 参与关系）';
COMMENT ON COLUMN biz_project_member.project_id IS '项目 ID（biz_project.id）';
COMMENT ON COLUMN biz_project_member.user_id IS '成员用户 ID（sys_user.id）';
COMMENT ON COLUMN biz_project_member.role IS '成员角色：MEMBER 成员 / VIEWER 只读（本卡两者权限等价，预留）';
COMMENT ON COLUMN biz_project_member.created_at IS '加入时间';
COMMENT ON COLUMN biz_project_member.create_by IS '操作人';

-- ---------- ② 项目编号唯一（软删过滤） ----------
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_project_no
    ON biz_project (project_no)
    WHERE deleted_at IS NULL;

COMMENT ON INDEX uk_biz_project_no IS
    'T7c：项目编号（API 的 `code`）未删唯一；已软删项目的编号可被新项目复用';

-- ---------- ③ 列表索引 ----------
CREATE INDEX IF NOT EXISTS idx_biz_project_owner
    ON biz_project (owner_id)
    WHERE deleted_at IS NULL;

COMMENT ON INDEX idx_biz_project_owner IS
    'T7c：项目页负责人筛选 + 可见性谓词的 owner_id 分支（部分索引，排除已软删）';

CREATE INDEX IF NOT EXISTS idx_biz_project_status
    ON biz_project (status, created_at DESC)
    WHERE deleted_at IS NULL;

COMMENT ON INDEX idx_biz_project_status IS
    'T7c：项目页状态筛选 + 默认排序（创建时间倒序）';

CREATE INDEX IF NOT EXISTS idx_biz_project_member_user
    ON biz_project_member (user_id, project_id);

COMMENT ON INDEX idx_biz_project_member_user IS
    'T7c：可见性谓词取「我参与的项目」id 集合（user_id 在前，避免全表扫）';

-- ---------- ④ 5 个权限节点（1 菜单 + 4 按钮） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000775'::uuid, '项目', 2, 'business:project:read',
     '/project', NULL, 'business/project/index',
     65, 1, 'folder', '项目', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000776'::uuid, 'business:project:read', 3,
     'business:project:read',
     NULL, '20000000-0000-0000-0000-000000000775'::uuid, NULL,
     10, 1, NULL, '查看项目（分页 / 详情 / 统计 / 项目下任务）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000777'::uuid, 'business:project:create', 3,
     'business:project:create',
     NULL, '20000000-0000-0000-0000-000000000775'::uuid, NULL,
     20, 1, NULL, '新建项目', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000778'::uuid, 'business:project:update', 3,
     'business:project:update',
     NULL, '20000000-0000-0000-0000-000000000775'::uuid, NULL,
     30, 1, NULL, '编辑项目 / 维护项目成员（负责人与创建人不需要此权限）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000779'::uuid, 'business:project:delete', 3,
     'business:project:delete',
     NULL, '20000000-0000-0000-0000-000000000775'::uuid, NULL,
     40, 1, NULL, '删除项目（负责人与创建人不需要此权限；删除不会删任务）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑤ 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000000775'::uuid;
    found     int;
    wrong     int;
BEGIN
    -- ① 成员表
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.tables
         WHERE table_name = 'biz_project_member' AND table_type = 'BASE TABLE'
    ) THEN
        RAISE EXCEPTION 'V0115 自检失败：biz_project_member 不存在';
    END IF;

    -- ② 唯一索引
    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE tablename = 'biz_project' AND indexname = 'uk_biz_project_no'
    ) THEN
        RAISE EXCEPTION 'V0115 自检失败：uk_biz_project_no 不存在';
    END IF;

    -- ③ 三条索引
    IF (SELECT count(*) FROM pg_indexes
         WHERE indexname IN ('idx_biz_project_owner',
                             'idx_biz_project_status',
                             'idx_biz_project_member_user')) <> 3 THEN
        RAISE EXCEPTION 'V0115 自检失败：列表索引不齐（应为 3 条）';
    END IF;

    -- ④ 5 个节点在、父子关系与 auth_code 逐字正确
    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id IN (page_menu,
                  '20000000-0000-0000-0000-000000000776'::uuid,
                  '20000000-0000-0000-0000-000000000777'::uuid,
                  '20000000-0000-0000-0000-000000000778'::uuid,
                  '20000000-0000-0000-0000-000000000779'::uuid)
       AND deleted_at IS NULL;
    IF found <> 5 THEN
        RAISE EXCEPTION 'V0115 自检失败：5 个权限节点只存在 % 个', found;
    END IF;

    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000776'::uuid,
                  '20000000-0000-0000-0000-000000000777'::uuid,
                  '20000000-0000-0000-0000-000000000778'::uuid,
                  '20000000-0000-0000-0000-000000000779'::uuid)
       AND (parent_id IS DISTINCT FROM page_menu
            OR auth_code NOT IN ('business:project:read',
                                 'business:project:create',
                                 'business:project:update',
                                 'business:project:delete')
            OR menu_type <> 3
            OR status <> 1);
    IF wrong > 0 THEN
        RAISE EXCEPTION 'V0115 自检失败：% 个按钮节点的父节点 / auth_code / 类型不对', wrong;
    END IF;

    -- 菜单节点的 path / component 必须与前端路由逐字一致（漏了菜单点不开页面）
    IF NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = page_menu
           AND path = '/project'
           AND component = 'business/project/index'
           AND auth_code = 'business:project:read'
           AND menu_type = 2
    ) THEN
        RAISE EXCEPTION 'V0115 自检失败：项目菜单节点的 path / component / auth_code 不对';
    END IF;

    RAISE NOTICE 'V0115 自检通过：成员表 + 唯一索引 + 3 条索引 + 5 个权限节点';
END
$$;
