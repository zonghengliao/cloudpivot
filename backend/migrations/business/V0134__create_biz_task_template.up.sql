-- V0134：任务模板（E11 · 一、任务模板 + 三、模板管理菜单）
--
-- 设计口径（老大卡面 t_834dcc63）：
--   `biz_task_template`（name / title / description / priority / estimate_hours /
--   need_review / subtasks jsonb）。
--   本卡额外补了 `owner_id` / `project_id` 两列 —— 没有它们「一键建任务」生成出来的
--   任务就没有负责人与归属项目，而 `biz_task.owner_id` 是 `NOT NULL REFERENCES sys_user(id)`。
--   口径：`owner_id` 为空时用**模板创建人**兜底（应用层解析，见 `business/task/template.rs`）。
--
-- subtasks 形态（jsonb 数组，元素可省略 priority）：
--   [{"title": "拆解步骤一", "priority": "P2"}, {"title": "拆解步骤二"}]
--
-- 迁移号段：V0133 是本批最后一笔（公告中心菜单），本卡取 V0134（模板表）/ V0135（周期列）。

CREATE TABLE biz_task_template (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键（应用侧用 now_v7）
    name           VARCHAR(100) NOT NULL, -- 模板名（列表里给人看的短名，唯一）
    title          VARCHAR(200) NOT NULL, -- 生成任务的标题模板
    description    TEXT, -- 生成任务的描述（纯文本 / Markdown 源文）
    priority       VARCHAR(2) NOT NULL DEFAULT 'P2'
        CHECK (priority IN ('P0', 'P1', 'P2', 'P3')), -- 优先级四级（与 biz_task 同域）
    estimate_hours NUMERIC(6, 1) CHECK (estimate_hours >= 0), -- 预估工时（小时，可空）
    need_review    BOOLEAN NOT NULL DEFAULT FALSE, -- 生成任务是否需要验收
    subtasks       JSONB NOT NULL DEFAULT '[]'::jsonb, -- 子任务列表（jsonb 数组，元素 {title, priority?}）
    owner_id       UUID REFERENCES sys_user(id), -- 生成任务的负责人（空 = 模板创建人）
    project_id     UUID REFERENCES biz_project(id), -- 生成任务归属项目（可空）
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(), -- 创建时间
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(), -- 更新时间
    create_by      UUID, -- 创建人
    update_by      UUID, -- 最后操作人
    deleted_at     TIMESTAMPTZ -- 软删时间（非空即已删除）
);

COMMENT ON TABLE biz_task_template IS '任务模板：一键建任务（含子任务）的来源；周期任务（V0135）也复用本表';
COMMENT ON COLUMN biz_task_template.id IS '主键';
COMMENT ON COLUMN biz_task_template.name IS '模板名（≤100 字符，未软删范围内唯一）';
COMMENT ON COLUMN biz_task_template.title IS '生成任务的标题（≤200 字符，与 biz_task.title 同域）';
COMMENT ON COLUMN biz_task_template.description IS '生成任务的描述';
COMMENT ON COLUMN biz_task_template.priority IS '生成任务的优先级：P0 / P1 / P2 / P3';
COMMENT ON COLUMN biz_task_template.estimate_hours IS '生成任务的预估工时（小时，可空）';
COMMENT ON COLUMN biz_task_template.need_review IS '生成任务是否需要验收';
COMMENT ON COLUMN biz_task_template.subtasks IS '子任务数组（jsonb）：元素 {"title": "...", "priority": "P2"?}，最多 20 条';
COMMENT ON COLUMN biz_task_template.owner_id IS '生成任务的负责人（空 = 用模板创建人兜底）';
COMMENT ON COLUMN biz_task_template.project_id IS '生成任务归属项目（可空）';
COMMENT ON COLUMN biz_task_template.deleted_at IS '软删时间（非空即已删除）';

-- 模板名唯一（软删范围内）
CREATE UNIQUE INDEX uk_biz_task_template_name
    ON biz_task_template (name)
 WHERE deleted_at IS NULL;

-- 列表默认按更新时间倒序
CREATE INDEX idx_biz_task_template_updated
    ON biz_task_template (updated_at DESC)
 WHERE deleted_at IS NULL;

-- ---------- 菜单：任务模板（挂在「工作任务」目录 0760 下，排序第四） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component, sort_order, status, icon, title, is_frame, is_cache, is_visible, hidden_expression, module_code, created_at, updated_at)
VALUES (
    '20000000-0000-0000-0000-000000000902', '任务模板', 2, 'business:task:read', '/task/templates',
    '20000000-0000-0000-0000-000000000760', 'business/task/templates',
    40, 1, 'file-copy', '任务模板', false, false, true, '', 'business', now(), now()
)
ON CONFLICT (id) DO UPDATE
  SET name = EXCLUDED.name, menu_type = EXCLUDED.menu_type, auth_code = EXCLUDED.auth_code,
      path = EXCLUDED.path, parent_id = EXCLUDED.parent_id, component = EXCLUDED.component,
      sort_order = EXCLUDED.sort_order, status = EXCLUDED.status, icon = EXCLUDED.icon,
      title = EXCLUDED.title, is_visible = EXCLUDED.is_visible, updated_at = now();

-- 授权给所有未删除角色（与 V0130 步骤⑤ 同口径：模板管理不是权限边界，入口权限沿用 business:task:read）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND m.id = '20000000-0000-0000-0000-000000000902'
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- 自检 ----------
DO $$
DECLARE
    n_table integer;
    n_menu  integer;
BEGIN
    SELECT count(*) INTO n_table
      FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name = 'biz_task_template';
    IF n_table <> 1 THEN
        RAISE EXCEPTION 'V0134 自检失败：biz_task_template 未创建';
    END IF;

    SELECT count(*) INTO n_menu
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000000902' AND parent_id = '20000000-0000-0000-0000-000000000760';
    IF n_menu <> 1 THEN
        RAISE EXCEPTION 'V0134 自检失败：任务模板菜单未挂在「工作任务」目录下';
    END IF;

    RAISE NOTICE 'V0134 自检通过：模板表 + 菜单已就位';
END $$;
