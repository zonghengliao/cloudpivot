-- ======================================================
-- V0100：工作任务管理 · 7 张表（批次 C+ · T1）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md
--       §十二 数据模型（7 张表）、§十三 实施拆分 T1 行
-- 回滚：V0100__create_task_tables.down.sql（逆序 DROP，无残留）
--
-- ── 为什么 7 张表一次建完，而不是 V0100–V0106 逐表 ─────────────
-- ① 这 7 张表是**同一张实体图**（biz_task 是中心，其余 6 张都靠外键指向它），
--    拆成 7 条迁移只会让「同一个 schema 决策」散落在 7 个文件里；回滚时还得
--    记清 DROP 顺序（子表先、父表后），反而更容易写错。
-- ② 本模块的 DDL 是**一次成型**的（设计文档已定稿，不像批次 C 那样边做边改），
--    不存在「先建表、后续卡再 ALTER」的情形。
-- ③ 索引单独一条（V0101）：索引与表分开是仓库既有惯例（V0080/V0081、V0082/V0083），
--    partial index / UNIQUE 的 EXPLAIN 证据也按索引文件逐条对应。
-- ④ 权限种子（V0103）与底座改动（V0102 = sys_user.user_type）各自独立成条 ——
--    它们能被单独回滚，且 user_type 属「底座改动」，混进业务建表迁移里会让
--    「业务回滚」误删底座列。
--
-- ── 与设计文档 §十二 的逐字对应 ────────────────────────────────
--   biz_task            任务主体（含 parent_id / need_review / approval_instance_id 预留）
--   biz_task_member     协作人 / 关注者
--   biz_task_comment    评论（mentions jsonb）
--   biz_task_attachment 附件（file_id → sys_file）
--   biz_task_dependency 依赖（FS 唯一支持，反自反 CHECK）
--   biz_task_worklog    工时（P1，本卡只建表不接口）
--   biz_project         项目（P1，本卡只建表不接口；biz_task.project_id 可空）
--
-- ── 三个刻意的建模决策（README 里也写了，评审时可对照） ──────────
-- 1. 枚举一律 `VARCHAR + CHECK`，不用 PG 原生 ENUM：原生 ENUM 加值要 DDL（且不能
--    在事务里回滚加值），本项目状态集合将来若扩（如 REVIEW → 多级验收）会卡住；
--    批次 C 的 `sys_announcement.status` 也是同样选择。
-- 2. `dept_id` 是**冗余快照**：创建时取负责人当时的部门。部门看板按它过滤，
--    比「JOIN sys_user 取 dept_id」快且不受「人换部门」影响（历史归属保留）。
--    负责人变更（T3 转派）时同步刷新。
-- 3. `task_no` 唯一索引建在**全表**（不含 `WHERE deleted_at IS NULL`）：
--    软删的任务号不允许被复用（否则「TSK-202610-000007」指向两个不同的东西，
--    审计和变更日志都会对不上）。编号生成见 model::next_task_no（advisory lock 串行化）。
-- ======================================================

-- ---------- 0) 项目（P1：本卡只建表，无接口） ----------
CREATE TABLE biz_project (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    project_no    VARCHAR(32), -- 项目编号（P1 启用，本卡不填）
    project_name  VARCHAR(100) NOT NULL, -- 项目名称
    description   TEXT, -- 项目说明
    owner_id      UUID REFERENCES sys_user(id), -- 项目负责人（sys_user.id）
    dept_id       UUID REFERENCES sys_dept(id), -- 归属部门（sys_dept.id）
    status        VARCHAR(16) NOT NULL DEFAULT 'ACTIVE'
        CHECK (status IN ('ACTIVE', 'ARCHIVED')), -- 状态：ACTIVE 进行中 / ARCHIVED 已归档
    start_date    DATE, -- 项目开始日
    end_date      DATE, -- 项目结束日
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(), -- 创建时间
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(), -- 更新时间
    create_by     UUID, -- 创建人
    update_by     UUID, -- 更新人
    deleted_at    TIMESTAMPTZ -- 软删时间（非空即已删除）
);

COMMENT ON TABLE biz_project IS '项目表（P1）：任务的可选分组容器，biz_task.project_id 可空';
COMMENT ON COLUMN biz_project.project_no IS '项目编号（P1 启用）';
COMMENT ON COLUMN biz_project.project_name IS '项目名称';
COMMENT ON COLUMN biz_project.description IS '项目说明';
COMMENT ON COLUMN biz_project.owner_id IS '项目负责人（sys_user.id）';
COMMENT ON COLUMN biz_project.dept_id IS '归属部门（sys_dept.id）';
COMMENT ON COLUMN biz_project.status IS '状态：ACTIVE 进行中 / ARCHIVED 已归档';
COMMENT ON COLUMN biz_project.start_date IS '项目开始日';
COMMENT ON COLUMN biz_project.end_date IS '项目结束日';

-- ---------- 1) 任务主体 ----------
CREATE TABLE biz_task (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键（应用侧用 now_v7，保留默认值兜底）
    task_no               VARCHAR(32) NOT NULL, -- 任务编号 TSK-YYYYMM-NNNNNN（月内递增，软删不复用）
    title                 VARCHAR(200) NOT NULL, -- 标题
    description           TEXT, -- 描述（纯文本 / Markdown 源文，渲染属 T6）
    status                VARCHAR(16) NOT NULL DEFAULT 'TODO'
        CHECK (status IN ('TODO', 'DOING', 'REVIEW', 'DONE', 'CANCELLED')), -- 5 态状态机
    priority              VARCHAR(2) NOT NULL DEFAULT 'P2'
        CHECK (priority IN ('P0', 'P1', 'P2', 'P3')), -- 优先级四级（老大 v2 拍板）
    progress              SMALLINT NOT NULL DEFAULT 0
        CHECK (progress BETWEEN 0 AND 100), -- 进度 0~100（自动/手动规则属 T2）
    need_review           BOOLEAN NOT NULL DEFAULT FALSE, -- 是否需要验收（v2 保留的验收开关）
    owner_id              UUID NOT NULL REFERENCES sys_user(id), -- 负责人
    creator_id            UUID NOT NULL REFERENCES sys_user(id), -- 创建人
    assigner_id           UUID REFERENCES sys_user(id), -- 分派人（自建任务为空）
    dept_id               UUID REFERENCES sys_dept(id), -- 归属部门快照（创建时负责人所属部门）
    parent_id             UUID REFERENCES biz_task(id), -- 父任务（深度限 2 层，应用层校验）
    project_id            UUID REFERENCES biz_project(id), -- 所属项目（可空）
    start_date            DATE, -- 计划开始日
    due_date              DATE, -- 截止日（逾期扫描按它）
    done_at               TIMESTAMPTZ, -- 完成时间（进入 DONE 时写入，T2 负责）
    estimate_hours        NUMERIC(6, 1) CHECK (estimate_hours >= 0), -- 预估工时
    actual_hours          NUMERIC(6, 1) CHECK (actual_hours >= 0), -- 实际工时（T4 工时接口回填）
    source                VARCHAR(16) NOT NULL DEFAULT 'SELF'
        CHECK (source IN ('SELF', 'ASSIGNED', 'TRANSFERRED')), -- 来源：自建 / 被分派 / 被转派
    cancel_reason         VARCHAR(500), -- 取消原因（任意 → CANCELLED 必填，T2 校验）
    reject_reason         VARCHAR(500), -- 打回原因（REVIEW → DOING 必填，T2 校验）
    approval_instance_id  UUID, -- 审批流实例（批次 D 预留，本批恒为 NULL）
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(), -- 创建时间
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(), -- 更新时间
    create_by             UUID, -- 创建人（审计列，与 creator_id 的区别：前者是操作人，后者是业务归属）
    update_by             UUID, -- 最后操作人
    deleted_at            TIMESTAMPTZ, -- 软删时间（非空即已删除）
    -- 反自反：任务不能把自己当父任务（设计文档 §十二「parent_id 限 2 层」的第一道闸）
    CONSTRAINT ck_biz_task_parent_not_self CHECK (parent_id IS NULL OR parent_id <> id)
);

COMMENT ON TABLE biz_task IS '任务主体：工作任务管理的核心实体（5 态状态机 + 进度 + 验收开关）';
COMMENT ON COLUMN biz_task.id IS '主键';
COMMENT ON COLUMN biz_task.task_no IS '任务编号 TSK-YYYYMM-NNNNNN，月内递增，软删后不复用（全表唯一）';
COMMENT ON COLUMN biz_task.title IS '任务标题';
COMMENT ON COLUMN biz_task.description IS '任务描述（Markdown 源文，渲染与净化属前端 T6）';
COMMENT ON COLUMN biz_task.status IS '状态：TODO 待开始 / DOING 进行中 / REVIEW 待验收 / DONE 已完成 / CANCELLED 已取消';
COMMENT ON COLUMN biz_task.priority IS '优先级：P0 最高 / P1 高 / P2 中（默认）/ P3 低';
COMMENT ON COLUMN biz_task.progress IS '进度百分比 0~100（有子任务时由子任务汇总，规则属 T2）';
COMMENT ON COLUMN biz_task.need_review IS '是否需要验收：true 时 DOING 只能进 REVIEW，false 可直接 DONE';
COMMENT ON COLUMN biz_task.owner_id IS '负责人（sys_user.id）';
COMMENT ON COLUMN biz_task.creator_id IS '创建人（sys_user.id）';
COMMENT ON COLUMN biz_task.assigner_id IS '分派人（sys_user.id）：自建任务为空，被分派/转派时记录分派人';
COMMENT ON COLUMN biz_task.dept_id IS '归属部门快照：创建时负责人所属部门（部门看板按此过滤）';
COMMENT ON COLUMN biz_task.parent_id IS '父任务（深度限 2 层：父任务自身不能再有 parent_id）';
COMMENT ON COLUMN biz_task.project_id IS '所属项目（P1，可空）';
COMMENT ON COLUMN biz_task.start_date IS '计划开始日';
COMMENT ON COLUMN biz_task.due_date IS '截止日（逾期扫描走部分索引 idx_biz_task_due_open）';
COMMENT ON COLUMN biz_task.done_at IS '完成时间（T2 进入 DONE 时写入，重开时清空）';
COMMENT ON COLUMN biz_task.estimate_hours IS '预估工时（小时，可空）';
COMMENT ON COLUMN biz_task.actual_hours IS '实际工时（小时，由工时记录汇总，P1）';
COMMENT ON COLUMN biz_task.source IS '来源：SELF 自建 / ASSIGNED 被分派 / TRANSFERRED 被转派';
COMMENT ON COLUMN biz_task.cancel_reason IS '取消原因（任意状态 → CANCELLED 必填，T2 校验）';
COMMENT ON COLUMN biz_task.reject_reason IS '打回原因（REVIEW → DOING 必填，T2 校验）';
COMMENT ON COLUMN biz_task.approval_instance_id IS '审批流实例 ID（批次 D 预留，本批恒为 NULL）';

-- ---------- 2) 协作人 / 关注者 ----------
CREATE TABLE biz_task_member (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    task_id     UUID NOT NULL REFERENCES biz_task(id) ON DELETE CASCADE, -- 任务（任务物理删除时级联）
    user_id     UUID NOT NULL REFERENCES sys_user(id), -- 成员（sys_user.id）
    role        VARCHAR(16) NOT NULL DEFAULT 'COLLABORATOR'
        CHECK (role IN ('COLLABORATOR', 'FOLLOWER')), -- 协作人 / 关注者
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(), -- 加入时间
    create_by   UUID, -- 操作人
    CONSTRAINT uk_biz_task_member UNIQUE (task_id, user_id)
);

COMMENT ON TABLE biz_task_member IS '任务协作人 / 关注者：可见性谓词的第 4 个 OR 分支（跨部门协作靠它）';
COMMENT ON COLUMN biz_task_member.task_id IS '任务 ID（biz_task.id，级联删除）';
COMMENT ON COLUMN biz_task_member.user_id IS '成员用户 ID（sys_user.id）';
COMMENT ON COLUMN biz_task_member.role IS '角色：COLLABORATOR 协作人 / FOLLOWER 关注者';
COMMENT ON COLUMN biz_task_member.created_at IS '加入时间';

-- ---------- 3) 评论 ----------
CREATE TABLE biz_task_comment (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    task_id            UUID NOT NULL REFERENCES biz_task(id) ON DELETE CASCADE, -- 任务
    user_id            UUID NOT NULL REFERENCES sys_user(id), -- 评论人
    content            TEXT NOT NULL, -- 评论正文（纯文本，@ 通过 mentions 表达）
    mentions           JSONB NOT NULL DEFAULT '[]'::jsonb, -- 被 @ 的用户 ID 数组（[uuid, ...]）
    parent_comment_id  UUID REFERENCES biz_task_comment(id), -- 回复的目标评论（顶层为空）
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(), -- 创建时间
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(), -- 更新时间
    create_by          UUID, -- 创建人
    update_by          UUID, -- 更新人
    deleted_at         TIMESTAMPTZ -- 软删时间
);

COMMENT ON TABLE biz_task_comment IS '任务评论（P0，接口属 T3）：mentions 用 jsonb 存被 @ 的用户 ID';
COMMENT ON COLUMN biz_task_comment.task_id IS '任务 ID（biz_task.id，级联删除）';
COMMENT ON COLUMN biz_task_comment.user_id IS '评论人（sys_user.id）';
COMMENT ON COLUMN biz_task_comment.content IS '评论正文（纯文本）';
COMMENT ON COLUMN biz_task_comment.mentions IS '被 @ 的用户 ID 数组（jsonb），通知扇出按它取人';
COMMENT ON COLUMN biz_task_comment.parent_comment_id IS '回复的上级评论 ID（顶层评论为空）';

-- ---------- 4) 附件（→ sys_file） ----------
CREATE TABLE biz_task_attachment (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    task_id     UUID NOT NULL REFERENCES biz_task(id) ON DELETE CASCADE, -- 任务
    file_id     UUID NOT NULL REFERENCES sys_file(id), -- 文件（support/storage 的 sys_file）
    sort_order  INT NOT NULL DEFAULT 0, -- 展示顺序
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(), -- 绑定时间
    create_by   UUID, -- 操作人
    CONSTRAINT uk_biz_task_attachment UNIQUE (task_id, file_id)
);

COMMENT ON TABLE biz_task_attachment IS '任务附件：只存 task ↔ sys_file 的绑定关系，文件本体在对象存储';
COMMENT ON COLUMN biz_task_attachment.task_id IS '任务 ID（biz_task.id，级联删除）';
COMMENT ON COLUMN biz_task_attachment.file_id IS '文件 ID（sys_file.id）';
COMMENT ON COLUMN biz_task_attachment.sort_order IS '展示顺序（升序）';

-- ---------- 5) 依赖（v2 新增，FS） ----------
CREATE TABLE biz_task_dependency (
    task_id        UUID NOT NULL REFERENCES biz_task(id) ON DELETE CASCADE, -- 后置任务（被阻塞方）
    depends_on_id  UUID NOT NULL REFERENCES biz_task(id) ON DELETE CASCADE, -- 前置任务（阻塞方）
    dep_type       VARCHAR(4) NOT NULL DEFAULT 'FS'
        CHECK (dep_type IN ('FS', 'SS', 'FF', 'SF')), -- 类型：本版只支持 FS，其余为枚举预留
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(), -- 创建时间
    create_by      UUID, -- 操作人
    CONSTRAINT pk_biz_task_dependency PRIMARY KEY (task_id, depends_on_id),
    -- 反自反（设计文档 §四）：任务不能依赖自己
    CONSTRAINT ck_biz_task_dependency_not_self CHECK (task_id <> depends_on_id)
);

COMMENT ON TABLE biz_task_dependency IS '任务依赖（P0，接口与循环检测属 T4）：本版唯一支持 FS（Finish-to-Start）';
COMMENT ON COLUMN biz_task_dependency.task_id IS '后置任务（被阻塞方，biz_task.id）';
COMMENT ON COLUMN biz_task_dependency.depends_on_id IS '前置任务（阻塞方，biz_task.id）';
COMMENT ON COLUMN biz_task_dependency.dep_type IS '依赖类型：FS 唯一支持；SS/FF/SF 为枚举预留（不出现在 UI）';

-- ---------- 6) 工时（P1：只建表） ----------
CREATE TABLE biz_task_worklog (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    task_id      UUID NOT NULL REFERENCES biz_task(id) ON DELETE CASCADE, -- 任务
    user_id      UUID NOT NULL REFERENCES sys_user(id), -- 填报人
    work_date    DATE NOT NULL, -- 工时日期
    hours        NUMERIC(5, 1) NOT NULL CHECK (hours > 0), -- 工时（小时，> 0）
    description  TEXT, -- 工作说明
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(), -- 创建时间
    create_by    UUID -- 创建人
);

COMMENT ON TABLE biz_task_worklog IS '任务工时记录（P1：本卡只建表，接口与统计属后续卡）';
COMMENT ON COLUMN biz_task_worklog.task_id IS '任务 ID（biz_task.id，级联删除）';
COMMENT ON COLUMN biz_task_worklog.user_id IS '填报人（sys_user.id）';
COMMENT ON COLUMN biz_task_worklog.work_date IS '工时日期';
COMMENT ON COLUMN biz_task_worklog.hours IS '工时（小时，必须 > 0）';
COMMENT ON COLUMN biz_task_worklog.description IS '工作说明';
