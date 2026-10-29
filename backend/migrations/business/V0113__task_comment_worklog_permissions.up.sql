-- ======================================================
-- V0113：工作任务管理 · 评论 / 附件 / 工时补齐（批次 C+ · T6b）
--
-- 依据：卡面 t_3fe560e1「一/二/三」三段规则 + docs/批次C+-工作任务管理-核心设计.md
--       §八（通知分级）、§九（附件走 storage）
-- 回滚：V0113__task_comment_worklog_permissions.down.sql（逆序，零残留）
--
-- ── 本迁移做四件事，逐条说明「为什么必须动库」 ──────────────────
--
-- ① `biz_task_attachment.deleted_at`（软删列）
--    V0100 建附件表时只有绑定关系、没有软删列。卡面要求「删除 = 软删记录 +
--    存储对象标记删除」。不加列就只能硬删绑定行，而硬删会连带丢失
--    「谁在什么时候删了哪个附件」的审计线索（存储侧 sys_file.deleted_at 只是
--    文件维度，一个文件可以被多个任务引用）。
--
-- ② 两条列表索引（评论按时间正序 / 工时按日期）
--    评论列表的查询形状固定为 `task_id = ? AND deleted_at IS NULL ORDER BY created_at`；
--    部分索引把「已软删的行」直接排除在索引之外，列表翻页不会越翻越慢。
--    工时列表按 (task_id, work_date) 取，顺带覆盖 `SUM(hours)` 的聚合扫描。
--
-- ③ `biz_task_worklog` 的工时区间 CHECK（`0.25 ≤ hours ≤ 24`）
--    V0100 只有 `hours > 0`。卡面规则 2 的上限（一个工作日）如果只在 handler 里判，
--    任何一条直连 SQL / 将来的批量导入都能塞进 `hours = 1000`，而
--    `biz_task.actual_hours` 是**由它汇总**的 —— 一条脏数据会污染整个任务的实际工时。
--    用 `NOT VALID` 加约束：存量行不校验（生产库可能有历史脏值，加了 VALIDATED
--    会让迁移直接失败并卡住部署），新写入一律校验。
--
-- ④ 3 个权限按钮节点（…0772 / …0773 / …0774）
--    `business:task:comment:delete`（删别人的评论）
--    `business:task:attachment:delete`（删别人的附件）
--    `business:task:worklog`（给非负责人登记工时）
--    三个码都是**加项**：默认只授 SUPER_ADMIN，其余角色在角色页按需勾选
--    （与 V0109 的 assign/member 同口径 —— 全员授予等于没有隔离）。
--
-- ── 为什么授权不在本文件 ────────────────────────────────────────
-- 版本化迁移（V\d+）先跑、种子（S__*.sql）后跑；空库上本文件执行时
-- `S__role_data.sql` 还没建出 SUPER_ADMIN 角色（V0103/V0109 都踩过）。
-- 授权写在 `migrations/seed/S__task_permissions.sql` 第 4 段。
--
-- ── 菜单 UUID 段 ──────────────────────────────────────────────
-- …0760–…0771 已被 V0103 / V0108 / V0109 / V0110 占用，
-- 本迁移取 **…0772–…0774**（`migrations/` 全库 grep 过，此前未使用）。
--
-- ── 幂等 ──────────────────────────────────────────────────────
-- `ADD COLUMN IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS` + 固定 UUID +
-- `ON CONFLICT (id) DO NOTHING` + DO 块判约束存在：重复执行不报错，
-- 也不覆盖运维改名后的菜单标题。
-- ======================================================

-- ---------- ① 附件软删列 ----------
ALTER TABLE biz_task_attachment
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

COMMENT ON COLUMN biz_task_attachment.deleted_at IS
    '软删时间（T6b）：非空即已删除；列表不返回，物理对象由 storage::cleanup 在 sys_file 软删 7 天后回收';

-- 存量行全部为「未删除」，与列的语义一致（ADD COLUMN 默认 NULL = 未删），无需回填。
-- 唯一索引 `uk_biz_task_attachment (task_id, file_id)` 保持**全表**唯一：
-- 软删后重新上传同一个文件走「复活」路径（`model::bind_*_attachment` 把
-- deleted_at 清空），而不是插第二条 —— 一任务一文件只留一行，附件计数才可信。

-- ---------- ② 列表索引 ----------
-- 评论列表：`WHERE task_id = ? AND deleted_at IS NULL ORDER BY created_at ASC`
CREATE INDEX IF NOT EXISTS idx_biz_task_comment_task_time
    ON biz_task_comment (task_id, created_at)
    WHERE deleted_at IS NULL;

COMMENT ON INDEX idx_biz_task_comment_task_time IS
    'T6b 评论列表：按任务取未删评论并保证时间正序（部分索引，已软删行不进索引）';

-- 工时列表 / 汇总：`WHERE task_id = ? ORDER BY work_date`
CREATE INDEX IF NOT EXISTS idx_biz_task_worklog_task_date
    ON biz_task_worklog (task_id, work_date);

COMMENT ON INDEX idx_biz_task_worklog_task_date IS
    'T6b 工时列表与 SUM(hours) 汇总：按任务取记录、按日期排序';

-- ---------- ③ 工时区间约束（新写入才校验） ----------
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conrelid = 'biz_task_worklog'::regclass
           AND conname = 'ck_biz_task_worklog_hours_range'
    ) THEN
        ALTER TABLE biz_task_worklog
            ADD CONSTRAINT ck_biz_task_worklog_hours_range
            CHECK (hours >= 0.25 AND hours <= 24) NOT VALID;
    END IF;
END
$$;

COMMENT ON CONSTRAINT ck_biz_task_worklog_hours_range ON biz_task_worklog IS
    'T6b：单条工时 0.25–24 小时（一个工作日）。NOT VALID：只约束新写入，存量脏值不阻断部署';

-- ---------- ④ 3 个权限按钮节点 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000772'::uuid, 'business:task:comment:delete', 3,
     'business:task:comment:delete',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     70, 1, NULL, '删除他人评论（作者本人不需要此权限；系统生成的评论一律不可删）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000773'::uuid, 'business:task:attachment:delete', 3,
     'business:task:attachment:delete',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     80, 1, NULL, '删除他人上传的任务附件（上传者本人不需要此权限）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000774'::uuid, 'business:task:worklog', 3,
     'business:task:worklog',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     90, 1, NULL, '为非负责人 / 非协作人登记工时（负责人与协作人不需要此权限）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑤ 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000000760'::uuid;
    btn_comment uuid := '20000000-0000-0000-0000-000000000772'::uuid;
    btn_att     uuid := '20000000-0000-0000-0000-000000000773'::uuid;
    btn_worklog uuid := '20000000-0000-0000-0000-000000000774'::uuid;
    found       int;
    wrong       int;
    def         text;
BEGIN
    -- ① 附件软删列
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_task_attachment' AND column_name = 'deleted_at'
    ) THEN
        RAISE EXCEPTION 'V0113 自检失败：biz_task_attachment.deleted_at 不存在';
    END IF;

    -- ② 两条索引
    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE tablename = 'biz_task_comment' AND indexname = 'idx_biz_task_comment_task_time'
    ) THEN
        RAISE EXCEPTION 'V0113 自检失败：idx_biz_task_comment_task_time 不存在';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE tablename = 'biz_task_worklog' AND indexname = 'idx_biz_task_worklog_task_date'
    ) THEN
        RAISE EXCEPTION 'V0113 自检失败：idx_biz_task_worklog_task_date 不存在';
    END IF;

    -- ③ 工时区间约束必须在（文本里要能看到 24）
    SELECT pg_get_constraintdef(oid) INTO def
      FROM pg_constraint
     WHERE conrelid = 'biz_task_worklog'::regclass
       AND conname = 'ck_biz_task_worklog_hours_range';
    IF def IS NULL THEN
        RAISE EXCEPTION 'V0113 自检失败：ck_biz_task_worklog_hours_range 不存在';
    END IF;
    IF position('24' in def) = 0 OR position('0.25' in def) = 0 THEN
        RAISE EXCEPTION 'V0113 自检失败：工时区间约束内容不对（实际：%）', def;
    END IF;

    -- ④ 3 个节点必须在、父节点必须是「工作任务」页面、auth_code 逐字正确
    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id IN (btn_comment, btn_att, btn_worklog) AND deleted_at IS NULL;
    IF found <> 3 THEN
        RAISE EXCEPTION 'V0113 自检失败：3 个权限节点只存在 % 个', found;
    END IF;

    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id IN (btn_comment, btn_att, btn_worklog)
       AND (parent_id IS DISTINCT FROM page_menu
            OR auth_code NOT IN ('business:task:comment:delete',
                                 'business:task:attachment:delete',
                                 'business:task:worklog')
            OR menu_type <> 3
            OR status <> 1);
    IF wrong > 0 THEN
        RAISE EXCEPTION 'V0113 自检失败：% 个节点的父节点 / auth_code / 类型不对', wrong;
    END IF;

    RAISE NOTICE 'V0113 自检通过：附件软删列 + 2 条索引 + 工时区间约束 + 3 个权限节点';
END
$$;
