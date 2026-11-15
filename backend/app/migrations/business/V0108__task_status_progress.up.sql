-- ======================================================
-- V0108：工作任务管理 · 状态机与进度的 schema + 权限节点（批次 C+ · T2）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md §二（状态机）/ §三（进度模型）
--       docs/云枢设计修正补丁-20261029.md 修正 8（通知分级）
--       卡面 t_5c19c0a9「三、接口」的 3 个新权限码
-- 回滚：V0108__task_status_progress.down.sql（逆序：先撤授权/菜单，再删列）
--
-- ── 本迁移做三件事，逐条说明「为什么必须改 schema」 ─────────────────
--
-- ① `biz_task.status_semantic`（**P1 扩展位**，本次恒为 NULL）
--    详细架构 D3 的「两层状态机」= 自定义列（`biz_task_status_def`，P1）映射到 5 个
--    语义状态。本版不做自定义列（v2 已批准），但**先把落点建好**：
--    将来加自定义列时只需回填这一列 + 在应用层做映射，不必再动 `biz_task` 的
--    主状态列（动它要重建 CHECK 与全部索引，风险大得多）。
--    本次**不填值**：留 NULL 表示「语义 = status 本身」，读路径遇到 NULL 直接
--    用 `status`，不引入第二份真相。
--
-- ② `biz_task_comment.comment_type`（`PLAIN` / `STATUS_CHANGE` / `PROGRESS_AUTO`）
--    卡面要求「任何流转都写 `biz_task_comment`，type = STATUS_CHANGE」，
--    而 V0100 建的评论表**没有类型列**（T1 的评论接口属 T3）。
--    不加列就只能把类型塞进正文（`[状态变更] …`），前端时间线要靠正则去猜，
--    且「进度自动汇总」与「人写的评论」无法区分 —— 所以加这一列。
--    默认 `PLAIN`：T3 的普通评论不用改代码就能落库。
--
-- ③ 3 个权限按钮节点（`…0765`–`…0767`）
--    `business:task:status`（改状态）/ `business:task:progress`（改进度）/
--    `business:task:subtask`（建子任务）。**授权**放在种子迁移
--    `migrations/seed/S__task_permissions.sql`（与 V0103 同款时序理由：
--    版本化迁移先跑、那时全新库里连 SUPER_ADMIN 角色都还没建，
--    `INSERT … SELECT FROM sys_role` 会命中 0 行）。
--
-- ── 菜单 UUID 段 ──────────────────────────────────────────────
-- …0760–…0764 已被 V0103（任务管理首屏 + 4 个 CRUD 按钮）占用，
-- 本迁移取 **…0765–…0767**（全库 grep 过，此前未使用）。
--
-- ── 幂等 ──────────────────────────────────────────────────────
-- `ADD COLUMN IF NOT EXISTS` + 固定 UUID/名 + `ON CONFLICT (id) DO NOTHING`：
-- 重复执行不报错、不覆盖运维改名后的标题。
-- ======================================================

-- ---------- ① 状态语义扩展位（P1 用，本次不填值） ----------
ALTER TABLE biz_task
    ADD COLUMN IF NOT EXISTS status_semantic VARCHAR(16);

COMMENT ON COLUMN biz_task.status_semantic IS
    'P1 扩展位：自定义状态列（biz_task_status_def）映射到的语义状态；NULL = 语义即 status 本身（T2 不填值）';

-- CHECK 只约束「填了就必须是 5 态之一」，NULL 放行（本次全表都是 NULL）。
-- 用 DO 块是因为 PG 没有 `ADD CONSTRAINT IF NOT EXISTS`：重复执行时先判存在。
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conrelid = 'biz_task'::regclass
           AND conname = 'ck_biz_task_status_semantic'
    ) THEN
        ALTER TABLE biz_task
            ADD CONSTRAINT ck_biz_task_status_semantic
            CHECK (status_semantic IS NULL
                   OR status_semantic IN ('TODO', 'DOING', 'REVIEW', 'DONE', 'CANCELLED'));
    END IF;
END
$$;

-- ---------- ② 评论类型（状态变更 / 进度自动汇总 / 普通评论） ----------
ALTER TABLE biz_task_comment
    ADD COLUMN IF NOT EXISTS comment_type VARCHAR(16) NOT NULL DEFAULT 'PLAIN';

COMMENT ON COLUMN biz_task_comment.comment_type IS
    '评论类型：PLAIN 普通评论（T3）/ STATUS_CHANGE 状态流转 / PROGRESS_AUTO 进度自动汇总';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conrelid = 'biz_task_comment'::regclass
           AND conname = 'ck_biz_task_comment_type'
    ) THEN
        ALTER TABLE biz_task_comment
            ADD CONSTRAINT ck_biz_task_comment_type
            CHECK (comment_type IN ('PLAIN', 'STATUS_CHANGE', 'PROGRESS_AUTO'));
    END IF;
END
$$;

-- ---------- ③ 权限按钮节点（授权在 S__task_permissions.sql） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000765'::uuid, 'business:task:status', 3,
     'business:task:status',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     50, 1, NULL, '改任务状态（白名单流转 + 打回原因）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000766'::uuid, 'business:task:progress', 3,
     'business:task:progress',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     60, 1, NULL, '改任务进度（有子任务时由汇总决定）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000767'::uuid, 'business:task:subtask', 3,
     'business:task:subtask',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     70, 1, NULL, '建子任务（快捷入口，继承父任务项目 / 优先级）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 自检（失败即启动失败） ----------
-- 1) 两个新列必须在，且 comment_type 的默认值必须是 PLAIN；
-- 2) 3 个权限节点必须在，且父节点是「工作任务」页面（…0760）；
-- 3) status_semantic 本次必须**全表为 NULL**（填了值说明有人提前用了 P1 的语义）。
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000000760'::uuid;
    found     int;
    filled    int;
    default_type text;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_task' AND column_name = 'status_semantic'
    ) THEN
        RAISE EXCEPTION 'V0108 自检失败：biz_task.status_semantic 不存在';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_task_comment' AND column_name = 'comment_type'
    ) THEN
        RAISE EXCEPTION 'V0108 自检失败：biz_task_comment.comment_type 不存在';
    END IF;

    SELECT column_default INTO default_type
      FROM information_schema.columns
     WHERE table_name = 'biz_task_comment' AND column_name = 'comment_type';
    IF default_type IS NULL OR default_type NOT LIKE '''PLAIN''%' THEN
        RAISE EXCEPTION 'V0108 自检失败：comment_type 默认值应为 ''PLAIN''，实际 %', default_type;
    END IF;

    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000765'::uuid,
                  '20000000-0000-0000-0000-000000000766'::uuid,
                  '20000000-0000-0000-0000-000000000767'::uuid)
       AND deleted_at IS NULL;
    IF found <> 3 THEN
        RAISE EXCEPTION 'V0108 自检失败：T2 权限节点只存在 % 个（应为 3）', found;
    END IF;

    IF EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id IN ('20000000-0000-0000-0000-000000000765'::uuid,
                      '20000000-0000-0000-0000-000000000766'::uuid,
                      '20000000-0000-0000-0000-000000000767'::uuid)
           AND (parent_id IS DISTINCT FROM page_menu OR menu_type <> 3)
    ) THEN
        RAISE EXCEPTION 'V0108 自检失败：T2 权限节点必须是「工作任务」页面（%）下的按钮节点（menu_type=3）', page_menu;
    END IF;

    SELECT count(*) INTO filled FROM biz_task WHERE status_semantic IS NOT NULL;
    IF filled > 0 THEN
        RAISE EXCEPTION 'V0108 自检失败：status_semantic 本次应全表为 NULL（实际 % 行有值）', filled;
    END IF;
END
$$;
