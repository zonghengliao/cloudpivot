-- ======================================================
-- V0110：工作任务管理 · 依赖约束 / 索引 / 权限节点（批次 C+ · T4）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md §四（任务依赖，含「循环检测必须做」）、
--       §五（甘特图）、docs/云枢设计修正补丁-20261029.md 修正 10
--       （甘特拖拽改期必须重跑依赖校验 —— 本迁移给 T7 铺路，不含拖拽）
--       卡面 t_687d35b1「一、数据模型」「二、接口（权限码 business:task:dependency）」
-- 回滚：V0110__task_dependency_constraints.down.sql
--
-- ── 本迁移做四件事 ────────────────────────────────────────────
-- ① `biz_task_dependency` 补 `lag_days` 列（`FS+lag`：前置完成后还要等 N 天；
--    本版只存不算排程，T7 的改期校验会用到）；
-- ② 补约束 / 索引：反自反（V0100 已有，这里做**存在性自检**，缺了就补）、
--    `(task_id, depends_on_id, dep_type)` 唯一索引、（`depends_on_id`）反查索引；
-- ③ 建 2 个菜单节点：甘特页面（…0770）+ 依赖管理按钮（…0771）；
-- ④ 自检（失败即启动失败）：列 / 约束 / 索引 / 节点任一缺失就 RAISE。
--
-- ── 为什么不加 `id` 主键列 ────────────────────────────────────
-- 卡面「一、数据模型」写的是 `id, task_id, depends_on_id, …`，但 T1 落地的表
-- 用的是**业务主键** `PRIMARY KEY (task_id, depends_on_id)`（一条任务对之间
-- 只可能有一条 FS 依赖，代理主键没有增量价值）。给已有表再加 `id` 会出现
-- 「两个主键候选」的歧义；接口层因此把 `DELETE /dependencies/{dep_id}`
-- 的 `dep_id` 定义为**对端任务 id**（前置组 = 前置 id、后继组 = 后继 id），
-- 删除按 pair 双向匹配（见 handler::remove_dependency 的文档）。
-- 这是对卡面的一处**有意偏差**，已在交付说明里写清。
--
-- ── 关于「部分唯一索引防重」 ─────────────────────────────────
-- 本表**没有软删列**（依赖一旦删掉就是真删，不留墓碑：留墓碑会让
-- 「删了再加」命中唯一索引失败）。所以「部分」没有可用的谓词 ——
-- 用一条普通唯一索引 `uk_biz_task_dependency_triplet` 防重，
-- 它与主键 `(task_id, depends_on_id)` 的差别是**把 dep_type 纳入唯一性**：
-- 将来若放开 SS/FF/SF，同一对任务之间的不同类型依赖可以并存。
--
-- ── 菜单 UUID 段 ──────────────────────────────────────────────
-- 已占用：…0760–…0764（T1）、…0765–…0767（T2）、…0768–…0769（T3）、…0790（V0089）。
-- 本迁移取 **…0770–…0771**（全库 grep 过，此前未使用）。
--
-- ── 幂等 ──────────────────────────────────────────────────────
-- `ADD COLUMN IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` /
-- 固定 UUID + `ON CONFLICT (id) DO NOTHING`：重复执行不报错、不覆盖运维改过的标题。
-- ======================================================

-- ---------- ① lag_days 列 ----------
ALTER TABLE biz_task_dependency
    ADD COLUMN IF NOT EXISTS lag_days INTEGER NOT NULL DEFAULT 0;

-- 负滞后（「提前 N 天开始」）会让 FS 语义变成 SF，本版不支持 → 直接拦在库层。
ALTER TABLE biz_task_dependency DROP CONSTRAINT IF EXISTS ck_biz_task_dependency_lag;
ALTER TABLE biz_task_dependency
    ADD CONSTRAINT ck_biz_task_dependency_lag CHECK (lag_days >= 0);

COMMENT ON COLUMN biz_task_dependency.lag_days IS
    '滞后天数（FS+lag：前置完成后还要等 N 天）。本版只存不算排程，T7 改期校验使用';

-- ---------- ② 反自反约束（V0100 已建，这里只做「缺了就补」） ----------
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'biz_task_dependency'::regclass
           AND contype = 'c'
           AND pg_get_constraintdef(oid) LIKE '%task_id <> depends_on_id%'
    ) THEN
        ALTER TABLE biz_task_dependency
            ADD CONSTRAINT ck_biz_task_dependency_not_self CHECK (task_id <> depends_on_id);
        RAISE NOTICE 'V0110：补齐缺失的反自反约束 ck_biz_task_dependency_not_self';
    END IF;
END
$$;

-- ---------- ③ 索引 ----------
-- 反查「谁依赖我」：前置任务完成 / 取消时要找所有后继（T4 的通知与甘特依赖边）。
CREATE INDEX IF NOT EXISTS idx_biz_task_dependency_on
    ON biz_task_dependency (depends_on_id);

-- 防重（把 dep_type 纳入唯一性，见文件头「部分唯一索引」的说明）。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_task_dependency_triplet
    ON biz_task_dependency (task_id, depends_on_id, dep_type);

-- ---------- ④ 菜单节点（甘特页面 + 依赖管理按钮） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    -- 甘特页面：挂在「工作任务」下（menu_type=2），路由 /task/gantt 与前端
    -- router/index.ts 的 'task/gantt' 逐字对应；权限码复用 business:task:read。
    ('20000000-0000-0000-0000-000000000770'::uuid, '甘特图', 2, 'business:task:read',
     '/task/gantt', '20000000-0000-0000-0000-000000000760'::uuid, 'business/task/gantt',
     70, 1, 'chart', '甘特图（只读排期）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000771'::uuid, 'business:task:dependency', 3,
     'business:task:dependency',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     70, 1, NULL, '管理任务依赖（加 / 删前置，创建人 / 负责人 / 分派人之外的管理员）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑤ 自检（任一不满足就让迁移失败 = 启动失败） ----------
DO $$
DECLARE
    has_lag     int;
    has_check   int;
    has_idx_on  int;
    has_uk      int;
    page_gantt  uuid := '20000000-0000-0000-0000-000000000770'::uuid;
    btn_dep     uuid := '20000000-0000-0000-0000-000000000771'::uuid;
    found       int;
    wrong       int;
BEGIN
    SELECT count(*) INTO has_lag
      FROM information_schema.columns
     WHERE table_name = 'biz_task_dependency' AND column_name = 'lag_days';
    IF has_lag <> 1 THEN
        RAISE EXCEPTION 'V0110 自检失败：biz_task_dependency.lag_days 不存在';
    END IF;

    SELECT count(*) INTO has_check
      FROM pg_constraint
     WHERE conrelid = 'biz_task_dependency'::regclass
       AND contype = 'c'
       AND pg_get_constraintdef(oid) LIKE '%task_id <> depends_on_id%';
    IF has_check < 1 THEN
        RAISE EXCEPTION 'V0110 自检失败：反自反约束（task_id <> depends_on_id）缺失';
    END IF;

    SELECT count(*) INTO has_idx_on
      FROM pg_indexes
     WHERE tablename = 'biz_task_dependency' AND indexname = 'idx_biz_task_dependency_on';
    IF has_idx_on <> 1 THEN
        RAISE EXCEPTION 'V0110 自检失败：idx_biz_task_dependency_on（反查「谁依赖我」）缺失';
    END IF;

    SELECT count(*) INTO has_uk
      FROM pg_indexes
     WHERE tablename = 'biz_task_dependency' AND indexname = 'uk_biz_task_dependency_triplet';
    IF has_uk <> 1 THEN
        RAISE EXCEPTION 'V0110 自检失败：uk_biz_task_dependency_triplet（防重复依赖）缺失';
    END IF;

    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id IN (page_gantt, btn_dep) AND deleted_at IS NULL;
    IF found <> 2 THEN
        RAISE EXCEPTION 'V0110 自检失败：甘特页 / 依赖权限节点只存在 % 个（应为 2）', found;
    END IF;

    -- 甘特页必须是菜单（menu_type=2）且挂在「工作任务」下；
    -- 依赖按钮必须是按钮（menu_type=3）且 auth_code 逐字正确（拼错 = 前端勾了不生效）
    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id = page_gantt
       AND (menu_type <> 2
            OR parent_id IS DISTINCT FROM '20000000-0000-0000-0000-000000000760'::uuid
            OR path IS DISTINCT FROM '/task/gantt'
            OR component IS DISTINCT FROM 'business/task/gantt'
            OR auth_code <> 'business:task:read'
            OR status <> 1);
    IF wrong > 0 THEN
        RAISE EXCEPTION 'V0110 自检失败：甘特页节点的类型 / 父节点 / path / component / auth_code 不对';
    END IF;

    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id = btn_dep
       AND (menu_type <> 3
            OR parent_id IS DISTINCT FROM '20000000-0000-0000-0000-000000000760'::uuid
            OR auth_code <> 'business:task:dependency'
            OR status <> 1);
    IF wrong > 0 THEN
        RAISE EXCEPTION 'V0110 自检失败：依赖权限节点的类型 / 父节点 / auth_code 不对';
    END IF;
END
$$;
