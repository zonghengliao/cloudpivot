-- ======================================================
-- V0109：工作任务管理 · 分派 / 协作人权限节点（批次 C+ · T3）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md §八（分派）、§九（通知）
--       docs/云枢设计修正补丁-20261029.md 修正 8（通知分级）、修正 11（AI 分派可授予）
--       卡面 t_30c7737d「一、接口」的 2 个权限码
-- 回滚：V0109__task_assign_permissions.down.sql（先撤授权，再删菜单）
--
-- ── 本迁移做两件「schema / 数据字典」的事 ─────────────────────
--
-- ① 建 2 个权限按钮节点（…0768 / …0769）；
-- ② 扩 `biz_task_comment.comment_type` 的 CHECK 值域，加入 `ASSIGN`
--    （转派要写一条 ASSIGN 评论，V0108 的三值 CHECK 会把它拒掉 —— 实测 500）。
-- **不改**任何表结构列 / 索引，也不删任何数据。
--
-- ── 授权单独分两处（这是本卡最容易做错的一处）──────────────────────
--
-- ① `business:task:assign` / `business:task:member` **不能**像 read/create 那样
--    全员授予（V0103 + S__task_permissions 的口径是「人人可用」）。理由：
--    分派权的真实口径是「行级关系（创建人 / 负责人 / 分派人）∪ 权限码」；
--    如果两个码也人手一个，「无关用户来分派 → 403」这条验收**永远不可能成立**
--    —— 权限码一授予就退化成「无隔离」。
-- ② 因此本迁移只建节点，**默认只授给 SUPER_ADMIN**（超管本来就短路全部权限，
--    这里授一遍是为了角色树勾选状态正确，不是为了让超管能用）。
-- ③ 其余角色（含将来给 AI 的「AI 团队主管」）由管理员在角色页按需勾选
--    ——「默认不能、可按角色授予」（修正 11）就是这么落地的。
--
-- ── 时序：授权为什么必须在种子迁移里 ─────────────────────────
-- 版本化迁移（V\\d+）先跑、种子（S__*.sql）后跑。空库上 V0109 执行时
-- `S__role_data.sql` 还没建出 SUPER_ADMIN 角色（V0103 踩过：那时
-- `INSERT … SELECT FROM sys_role` 命中 0 行）。所以：
--   · 版本化迁移 = 建 2 个菜单节点 + 扩评论类型 CHECK（都不依赖任何角色）；
--   · 种子迁移 `S__task_permissions.sql` 第 3 段 = 把 2 个码授给 SUPER_ADMIN。
-- 老库升级与全新安装两种场景都正确。
--
-- ── 菜单 UUID 段 ──────────────────────────────────────────────
-- …0760–…0764 被 V0103（首屏 + 4 个 CRUD 按钮）占用，
-- …0765–…0767 被 V0108（状态 / 进度 / 子任务）占用，
-- 本迁移取 **…0768–…0769**（全库 grep 过，此前未使用）。
--
-- ── 幂等 ──────────────────────────────────────────────────────
-- 固定 UUID + `ON CONFLICT (id) DO NOTHING`：重复执行不报错、不覆盖运维改名后的标题。
-- ======================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000768'::uuid, 'business:task:assign', 3,
     'business:task:assign',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     50, 1, NULL, '分派 / 转派任务（创建人 / 负责人 / 分派人，或持有本权限的管理员）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000769'::uuid, 'business:task:member', 3,
     'business:task:member',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     60, 1, NULL, '管理任务协作人（加 / 移除 / 列表）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② `biz_task_comment.comment_type` 扩值：加 `ASSIGN` ----------
--
-- 卡面要求「转派写一条 `ASSIGN` 类型评论」，而 V0108 建的 CHECK 只有
-- `PLAIN / STATUS_CHANGE / PROGRESS_AUTO` —— 不扩就会在事务里撞
-- `ck_biz_task_comment_type`（实测：500 + 10004，整条分派回滚）。
--
-- 做法：**先删后加**（PG 不支持给 CHECK 追加枚举值），用 `IF EXISTS` 保证可重放；
-- 加了之后立刻自检「约束文本里必须有 ASSIGN」，防止有人把范围改回去。
ALTER TABLE biz_task_comment DROP CONSTRAINT IF EXISTS ck_biz_task_comment_type;
ALTER TABLE biz_task_comment
    ADD CONSTRAINT ck_biz_task_comment_type
    CHECK (comment_type IN ('PLAIN', 'STATUS_CHANGE', 'PROGRESS_AUTO', 'ASSIGN'));

-- ---------- ③ 自检：约束必须包含 ASSIGN ----------
DO $$
DECLARE
    def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO def
      FROM pg_constraint
     WHERE conrelid = 'biz_task_comment'::regclass
       AND conname = 'ck_biz_task_comment_type';

    IF def IS NULL OR position('ASSIGN' in def) = 0 THEN
        RAISE EXCEPTION 'V0109 自检失败：ck_biz_task_comment_type 未包含 ASSIGN（实际：%）', def;
    END IF;
END
$$;

-- ---------- 自检：2 个节点必须在，且挂在「工作任务」页面下（失败即启动失败） ----------
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000000760'::uuid;
    btn_assign uuid := '20000000-0000-0000-0000-000000000768'::uuid;
    btn_member uuid := '20000000-0000-0000-0000-000000000769'::uuid;
    found      int;
    wrong      int;
BEGIN
    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id IN (btn_assign, btn_member)
       AND deleted_at IS NULL;

    IF found <> 2 THEN
        RAISE EXCEPTION 'V0109 自检失败：business:task:assign / member 节点只存在 % 个（应为 2）', found;
    END IF;

    -- 父节点必须是工作任务页面，且 auth_code 逐字正确（权限码拼错 = 前端勾了不生效）
    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id IN (btn_assign, btn_member)
       AND (parent_id IS DISTINCT FROM page_menu
            OR auth_code NOT IN ('business:task:assign', 'business:task:member')
            OR menu_type <> 3
            OR status <> 1);

    IF wrong > 0 THEN
        RAISE EXCEPTION 'V0109 自检失败：% 个节点的父节点 / auth_code / 类型不对', wrong;
    END IF;
END
$$;
