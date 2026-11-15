-- ======================================================
-- V0120：审批流 · 定义侧 5 张表（批次 D · D1）
--
-- 依据：`docs/批次D-工作流审批-技术架构.md`
--       §0.1 D1（三个表示）/ D2（实例绑版本）/ D3+D4（会签模式 × 否决）/ D7（只有显式网关能分叉）
--       §6.1（完整 DDL，POC 已实跑）/ §6.8（回滚顺序）
-- 回滚：`V0120__flow_def_tables.down.sql`（逆序 DROP，零残留，脚本内含断言）
--
-- ── 为什么编号是 V0120 而不是文档里的 V0115 ────────────────────────
-- 文档 §6 排的是 V0115（定义侧）/ V0116（运行侧）/ V0117（索引）/ V0118（权限种子）/ V0119（定时任务）。
-- 但文档定稿之后，**V0115 已被 T7c「项目分组」占用**（`V0115__project_grouping.{up,down}.sql`
-- 已在 main 上，迁移器按版本号唯一）。为了让同批并行的 D2–D6 仍然拿到文档排给它们的
-- V0116–V0119，本卡（D1）整体后移到 **V0120（表）/ V0121（权限节点）**。
-- 排序安全性：运行侧表（V0116）引用本文件建的 `biz_flow_def_version`，本文件号更大 ⇒ 后执行，
-- 但运行侧的外键指向的是**已存在的表**（V0115 及以前），与本文件无关，因此两条迁移互不阻塞。
--
-- ── 为什么固定 5 张表一次建完 ────────────────────────────────────
-- 它们是同一张「定义图」的 5 个切面（def 是身份、version 是快照边界、node/edge 是图、
-- node_assignee 是审批人引用）。拆成 5 条迁移只会让同一个 schema 决策散落在 5 个文件里，
-- 回滚还要记清 DROP 顺序。
--
-- ── 四个刻意的建模决策 ─────────────────────────────────────────
-- ① **枚举一律 VARCHAR + CHECK**（不用 PG 原生 ENUM）：与 `biz_task` / `sys_announcement`
--    同一理由——原生 ENUM 加值要 DDL 且不能在事务里回滚。
-- ② **`biz_flow_node_assignee` 独立成表，绝不塞进 `node.config` JSONB**（架构 §0.2-C 三个理由）：
--    · 无法外键约束（`ref_id` 指向已删用户 / 角色，没有任何约束拦）；
--    · 无法反查「哪些流程引用了这个角色」（改角色前的影响面分析要 jsonb_path_query 全表扫）；
--    · 本栈驱动不支持数组参数绑定（`scope.rs` 注释里实测过 `Value::Array → unsupported feature`）。
-- ③ **节点/边的呈现坐标（`position_x/y`）存在行上**而不是只存 JSONB：编辑器整图提交时
--    坐标与语义一起落库，避免「图在 JSONB 里、坐标在别处」两份真相。
-- ④ **`graph_snapshot` 只在 PUBLISHED 时非空**（CHECK 强制），DRAFT 阶段以行为准（L1 → L2）。
--
-- ── 与架构 §6.1 DDL 的两处差异（都往「更严」的方向） ─────────────
-- ① `biz_flow_def_version` 增加 `is_current BOOLEAN` + 部分唯一索引 `uk_biz_flow_def_version_current`：
--    卡面要求「版本号自增、`is_current` 切换、`def.current_version_id` 更新」三件事同时发生。
--    `is_current` 让「每个 def 至多一个当前版本」成为库级事实（部分唯一索引），发布事务
--    同时维护它与 `def.current_version_id`，二者的**一致性**由发布事务 + 启动/发布自检保证。
-- ② `biz_flow_node` 增加 `ck_biz_flow_node_ratio`：`RATIO` 模式必须填 `approve_ratio`
--    （架构 §3.7 V10 的库侧兜底；应用层仍然先校验并给中文文案）。
--
-- ── 幂等 ────────────────────────────────────────────────────
-- `CREATE TABLE IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS`：重复执行不报错；
-- 末尾 DO 块做结构自检（缺表 / 缺索引 / 缺约束 / 约束内容不对 → 迁移失败 = 启动失败）。
-- ======================================================

-- ---------- 1) 流程定义（稳定身份） ----------
CREATE TABLE IF NOT EXISTS biz_flow_def (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    flow_code          VARCHAR(64) NOT NULL,
    flow_name          VARCHAR(100) NOT NULL,
    biz_type           VARCHAR(32) NOT NULL,
    category           VARCHAR(32),
    description        TEXT,
    status             VARCHAR(16) NOT NULL DEFAULT 'DRAFT'
        CHECK (status IN ('DRAFT', 'PUBLISHED', 'DISABLED')),
    current_version_id UUID,
    current_version_no INT NOT NULL DEFAULT 0 CHECK (current_version_no >= 0),
    is_default         BOOLEAN NOT NULL DEFAULT FALSE,
    allow_auto_approve BOOLEAN NOT NULL DEFAULT FALSE,
    owner_dept_id      UUID,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    create_by          UUID,
    update_by          UUID,
    deleted_at         TIMESTAMPTZ,
    CONSTRAINT ck_biz_flow_def_code CHECK (flow_code ~ '^[a-z][a-z0-9_]{1,63}$'),
    -- PUBLISHED 必须有指向版本：把「发布事务的正确顺序」约束住 ——
    -- 不可能出现「PUBLISHED 但 current_version_id 为空」的半成品（POC 写 seed 时实测拦到过）。
    CONSTRAINT ck_biz_flow_def_published_has_version
        CHECK (status <> 'PUBLISHED' OR current_version_id IS NOT NULL)
);

COMMENT ON TABLE biz_flow_def IS
    '审批流程定义（稳定身份）：flow_code 是用户可见编码、biz_type 决定「哪类单据能发起」';
COMMENT ON COLUMN biz_flow_def.current_version_id IS
    '当前发布版本（只决定「新实例用哪个版本」；在跑实例绑的是自己的 version_id，见架构 D2）';
COMMENT ON COLUMN biz_flow_def.allow_auto_approve IS
    '流程级「允许超时自动通过」总闸：节点 timeout_action=AUTO_APPROVE 必须同时打开它（架构 D10）';
COMMENT ON COLUMN biz_flow_def.deleted_at IS
    '软删时间。`flow_code` 的部分唯一索引只覆盖未删行 ⇒ 删掉的编码可以复用';

-- 编码唯一（软删过滤）：删掉的流程不占用编码
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_flow_def_code
    ON biz_flow_def (flow_code) WHERE deleted_at IS NULL;

-- 同一 biz_type 至多一个默认流程（否则「发起时挑哪个」不可判定）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_flow_def_default
    ON biz_flow_def (biz_type) WHERE is_default AND deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_biz_flow_def_status
    ON biz_flow_def (status, biz_type);

-- ---------- 2) 版本快照（PUBLISHED 后不可变） ----------
CREATE TABLE IF NOT EXISTS biz_flow_def_version (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    def_id         UUID NOT NULL REFERENCES biz_flow_def(id) ON DELETE CASCADE,
    version_no     INT NOT NULL CHECK (version_no >= 1),
    version_status VARCHAR(16) NOT NULL DEFAULT 'DRAFT'
        CHECK (version_status IN ('DRAFT', 'PUBLISHED', 'ARCHIVED')),
    is_current     BOOLEAN NOT NULL DEFAULT FALSE,
    graph_hash     CHAR(64),
    graph_snapshot JSONB,
    node_count     INT NOT NULL DEFAULT 0 CHECK (node_count >= 0),
    edge_count     INT NOT NULL DEFAULT 0 CHECK (edge_count >= 0),
    change_note    VARCHAR(500),
    published_at   TIMESTAMPTZ,
    published_by   UUID,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    create_by      UUID,
    update_by      UUID,
    CONSTRAINT uk_biz_flow_def_version UNIQUE (def_id, version_no),
    -- PUBLISHED 必须同时有快照 + hash + 发布时间：三者缺一都意味着「能发起但没有可执行的图」
    CONSTRAINT ck_biz_flow_def_version_published
        CHECK (version_status <> 'PUBLISHED'
               OR (graph_hash IS NOT NULL AND graph_snapshot IS NOT NULL
                   AND published_at IS NOT NULL)),
    -- 只有 PUBLISHED 才是 current（ARCHIVED / DRAFT 不许挂 is_current）
    CONSTRAINT ck_biz_flow_def_version_current_published
        CHECK (NOT is_current OR version_status = 'PUBLISHED')
);

COMMENT ON TABLE biz_flow_def_version IS
    '流程版本：DRAFT 可编辑（行是真相），PUBLISHED 冻结（graph_snapshot 是真相），ARCHIVED 仅历史实例引用';
COMMENT ON COLUMN biz_flow_def_version.graph_snapshot IS
    'L2 发布快照（canonical JSON）：运行时引擎只读它，永不跟随 def.current_version_id（架构 D2）';
COMMENT ON COLUMN biz_flow_def_version.graph_hash IS
    '快照 canonical JSON 的 sha256（64 位小写十六进制）：同图重复发布必须得到同一个值（可自检）';

-- `uk_biz_flow_def_version`（UNIQUE 约束）已经生成唯一索引，**不要**再建同名索引（POC 踩过重复索引）
CREATE INDEX IF NOT EXISTS idx_biz_flow_def_version_def
    ON biz_flow_def_version (def_id, version_no DESC);

-- 每个 def 至多一个当前版本（部分唯一索引；发布事务负责切换）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_flow_def_version_current
    ON biz_flow_def_version (def_id) WHERE is_current;

-- ---------- 3) 节点（编辑期行） ----------
CREATE TABLE IF NOT EXISTS biz_flow_node (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    version_id          UUID NOT NULL REFERENCES biz_flow_def_version(id) ON DELETE CASCADE,
    node_key            VARCHAR(64) NOT NULL,
    node_name           VARCHAR(100) NOT NULL,
    node_type           VARCHAR(16) NOT NULL
        CHECK (node_type IN ('START','APPROVAL','COUNTERSIGN','CONDITION','PARALLEL','CC','END')),
    assignee_type       VARCHAR(24)
        CHECK (assignee_type IN ('USER','ROLE','INITIATOR','INITIATOR_SUPERIOR','DEPT_LEADER',
                                 'TASK_OWNER','TASK_CREATOR','TASK_ASSIGNER','FORM_VAR','ADMIN')),
    superior_levels     SMALLINT NOT NULL DEFAULT 1 CHECK (superior_levels BETWEEN 1 AND 10),
    form_var_key        VARCHAR(64),
    approval_mode       VARCHAR(8) CHECK (approval_mode IN ('ALL','RATIO','ANY')),
    approve_ratio       NUMERIC(4,3) CHECK (approve_ratio > 0 AND approve_ratio <= 1),
    reject_veto         BOOLEAN NOT NULL DEFAULT TRUE,
    empty_assignee      VARCHAR(16) NOT NULL DEFAULT 'SKIP'
        CHECK (empty_assignee IN ('SKIP','ERROR','FALLBACK_ADMIN')),
    self_approval       VARCHAR(8) NOT NULL DEFAULT 'SKIP'
        CHECK (self_approval IN ('ALLOW','SKIP','FORBID')),
    allow_robot         BOOLEAN NOT NULL DEFAULT FALSE,
    join_mode           VARCHAR(8) NOT NULL DEFAULT 'AND' CHECK (join_mode IN ('AND','OR')),
    fork_mode           VARCHAR(8) NOT NULL DEFAULT 'AND' CHECK (fork_mode IN ('AND','OR')),
    timeout_hours       INT CHECK (timeout_hours IS NULL OR timeout_hours BETWEEN 1 AND 8760),
    timeout_action      VARCHAR(16) NOT NULL DEFAULT 'REMIND'
        CHECK (timeout_action IN ('REMIND','AUTO_APPROVE','AUTO_REJECT','ESCALATE')),
    remind_before_hours INT CHECK (remind_before_hours IS NULL OR remind_before_hours >= 0),
    max_remind_times    SMALLINT NOT NULL DEFAULT 3 CHECK (max_remind_times BETWEEN 0 AND 20),
    cc_on_approved      BOOLEAN NOT NULL DEFAULT FALSE,
    position_x          INT NOT NULL DEFAULT 0,
    position_y          INT NOT NULL DEFAULT 0,
    sort_order          INT NOT NULL DEFAULT 0,
    config              JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uk_biz_flow_node UNIQUE (version_id, node_key),
    CONSTRAINT ck_biz_flow_node_key CHECK (node_key ~ '^[A-Za-z][A-Za-z0-9_]{0,63}$'),
    -- 架构 §3.7 V10 的库侧兜底：RATIO 必须带阈值（应用层同样校验并给中文文案）
    CONSTRAINT ck_biz_flow_node_ratio
        CHECK (approval_mode IS NULL OR approval_mode <> 'RATIO' OR approve_ratio IS NOT NULL)
);

COMMENT ON TABLE biz_flow_node IS
    '编辑期节点（L1）：只写 DRAFT 版本；PUBLISHED 版本的行由应用层强制只读（架构 §3.6.3）';
COMMENT ON COLUMN biz_flow_node.approval_mode IS
    '会签模式 ALL/RATIO/ANY（只对 COUNTERSIGN 生效）—— 与 reject_veto 是**正交两维**（架构 D3/D4）';
COMMENT ON COLUMN biz_flow_node.reject_veto IS
    '一票否决开关（与 approval_mode 正交：「过 60% 且财务没否决」= RATIO + veto）';
COMMENT ON COLUMN biz_flow_node.config IS
    '节点扩展配置（表单提示、按钮文案等）。**审批人 id 列表不放这里**，见 biz_flow_node_assignee';

CREATE INDEX IF NOT EXISTS idx_biz_flow_node_version
    ON biz_flow_node (version_id, node_type);

-- ---------- 4) 节点审批人引用（独立表，见文件头 ②） ----------
CREATE TABLE IF NOT EXISTS biz_flow_node_assignee (
    node_id       UUID NOT NULL REFERENCES biz_flow_node(id) ON DELETE CASCADE,
    assignee_type VARCHAR(24) NOT NULL CHECK (assignee_type IN ('USER','ROLE','DEPT_LEADER')),
    ref_id        UUID NOT NULL,
    sort_order    INT NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (node_id, assignee_type, ref_id)
);

COMMENT ON TABLE biz_flow_node_assignee IS
    '节点审批人的**可枚举引用**（USER/ROLE/DEPT_LEADER）。INITIATOR_SUPERIOR / TASK_OWNER / FORM_VAR / ADMIN / INITIATOR 是运行时才能算出来的，落行没有意义（架构 §6.1 注）';
COMMENT ON COLUMN biz_flow_node_assignee.ref_id IS
    '指向 sys_user.id / sys_role.id / sys_dept.id（按 assignee_type 区分）。无外键：三种目标表任一，靠应用层 + 运行时解析兜底';

-- 影响面反查：「角色 X 被哪些流程引用」必须走这条索引（架构 §0.2-C 理由 ②）
CREATE INDEX IF NOT EXISTS idx_biz_flow_node_assignee_ref
    ON biz_flow_node_assignee (assignee_type, ref_id);

-- ---------- 5) 连线（条件在边上） ----------
CREATE TABLE IF NOT EXISTS biz_flow_edge (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    version_id      UUID NOT NULL REFERENCES biz_flow_def_version(id) ON DELETE CASCADE,
    edge_key        VARCHAR(64) NOT NULL,
    from_node_id    UUID NOT NULL REFERENCES biz_flow_node(id) ON DELETE CASCADE,
    to_node_id      UUID NOT NULL REFERENCES biz_flow_node(id) ON DELETE CASCADE,
    condition_expr  JSONB,
    condition_label VARCHAR(100),
    priority        INT NOT NULL DEFAULT 0,
    is_default      BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uk_biz_flow_edge UNIQUE (version_id, edge_key),
    -- 自环没有语义（发布校验会以「环」报错，但库层先挡一道，避免脏数据进图）
    CONSTRAINT ck_biz_flow_edge_not_self CHECK (from_node_id <> to_node_id),
    -- default 边不许带条件：带了条件的「兜底」不是兜底（架构 §3.2 硬约束 2）
    CONSTRAINT ck_biz_flow_edge_default_no_cond
        CHECK (NOT is_default OR condition_expr IS NULL)
);

COMMENT ON TABLE biz_flow_edge IS
    '编辑期连线：from/to 是 node 主键（不是 node_key），因此节点改 key 不会改历史连线；API 层收发 key';
COMMENT ON COLUMN biz_flow_edge.condition_expr IS
    '条件分支的 JSON DSL（架构 §3.4：op=and/or/not/cmp，字段白名单，深度 ≤ 8、节点 ≤ 64）';
COMMENT ON COLUMN biz_flow_edge.is_default IS
    'CONDITION 节点的兜底分支：每条出边恰有一条 is_default=true，且它不许带条件（架构 §3.2 硬约束 2）';

CREATE INDEX IF NOT EXISTS idx_biz_flow_edge_from ON biz_flow_edge (version_id, from_node_id);
CREATE INDEX IF NOT EXISTS idx_biz_flow_edge_to   ON biz_flow_edge (version_id, to_node_id);

-- ---------- 6) 结构自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    missing_tbl  int;
    missing_idx  int;
    missing_ck   int;
    missing_fk   int;
BEGIN
    -- ① 5 张表
    SELECT 5 - count(*) INTO missing_tbl
      FROM information_schema.tables
     WHERE table_schema = current_schema()
       AND table_name IN ('biz_flow_def','biz_flow_def_version','biz_flow_node',
                          'biz_flow_node_assignee','biz_flow_edge');
    IF missing_tbl > 0 THEN
        RAISE EXCEPTION 'V0120 自检失败：缺少 % 张 biz_flow_* 定义侧表', missing_tbl;
    END IF;

    -- ② 4 条索引（软删唯一 / 默认唯一 / 当前版本唯一 / 审批人反查）
    SELECT 4 - count(*) INTO missing_idx
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND indexname IN ('uk_biz_flow_def_code','uk_biz_flow_def_default',
                         'uk_biz_flow_def_version_current','idx_biz_flow_node_assignee_ref');
    IF missing_idx > 0 THEN
        RAISE EXCEPTION 'V0120 自检失败：缺少 % 条关键索引', missing_idx;
    END IF;

    -- ③ 卡面点名的 CHECK 约束（node.type / approval_mode / fork_mode / join_mode 枚举 +
    --    RATIO 阈值兜底 + key 格式 + 节点唯一 + 边的自环 / default 无条件）
    SELECT 11 - count(*) INTO missing_ck
      FROM pg_constraint
     WHERE conname IN ('biz_flow_node_node_type_check','biz_flow_node_assignee_type_check',
                       'biz_flow_node_approval_mode_check','biz_flow_node_approve_ratio_check',
                       'biz_flow_node_fork_mode_check','biz_flow_node_join_mode_check',
                       'ck_biz_flow_node_key','ck_biz_flow_node_ratio','uk_biz_flow_node',
                       'ck_biz_flow_edge_not_self','ck_biz_flow_edge_default_no_cond');
    IF missing_ck > 0 THEN
        RAISE EXCEPTION 'V0120 自检失败：枚举 / 阈值 / 唯一 / 边的 CHECK 约束缺 % 条', missing_ck;
    END IF;

    -- ③b `reject_veto` 必须是 NOT NULL BOOLEAN（卡面点名「reject_veto 布尔」）
    SELECT 1 - count(*) INTO missing_ck
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_flow_node'
       AND column_name = 'reject_veto'
       AND is_nullable = 'NO'
       AND data_type = 'boolean';
    IF missing_ck > 0 THEN
        RAISE EXCEPTION 'V0120 自检失败：biz_flow_node.reject_veto 不是 NOT NULL BOOLEAN';
    END IF;

    -- ④ 4 条外键（version→def / node→version / edge→version / assignee→node）
    SELECT 4 - count(DISTINCT conrelid::regclass::text) INTO missing_fk
      FROM pg_constraint
     WHERE contype = 'f'
       AND conrelid::regclass::text IN ('biz_flow_def_version','biz_flow_node',
                                        'biz_flow_edge','biz_flow_node_assignee');
    IF missing_fk > 0 THEN
        RAISE EXCEPTION 'V0120 自检失败：定义侧外键覆盖缺 % 张表', missing_fk;
    END IF;

    RAISE NOTICE 'V0120 自检通过：5 张表 + 4 条关键索引 + node 约束 + 4 条外键';
END
$$;
