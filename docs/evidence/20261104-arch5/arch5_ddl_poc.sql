-- ============================================================
-- ARCH-5 工作流引擎设计 · DDL POC（设计级验证，不是迁移文件）
--
-- 目的：把《ARCH-5-工作流引擎设计.md》第 3 章的数据模型在**真实库副本**上跑一遍，
--       证明 ① 语法可执行 ② 扩展现有表不破坏存量约束 ③ 关键约束真的能拦脏数据。
-- 基线库：cloudpivot_p16md（102 张表，含 biz_flow_* 9 张 + biz_task 全套 + V0177 三列）
-- 落点：POC 库 arch5_poc（临时副本，验证后丢弃）
--
-- 号段：设计建议 V0220–V0227（当前仓内最大 V0215，留 4 个空号给并行卡）
--       POC 里故意不带版本前缀，因为它不是迁移文件。
-- ============================================================

\timing off
\set ON_ERROR_STOP on

-- ============================================================
-- 第一部分：扩展现有表（只加列 / 只换约束，不删列、不改语义）
-- ============================================================

-- ---------- 1.1 biz_flow_def：加 flow_type / closure_mode / archive_policy ----------
ALTER TABLE biz_flow_def
    ADD COLUMN IF NOT EXISTS flow_type      varchar(16) NOT NULL DEFAULT 'APPROVAL',
    ADD COLUMN IF NOT EXISTS closure_mode   varchar(16) NOT NULL DEFAULT 'ONE_WAY',
    ADD COLUMN IF NOT EXISTS archive_policy varchar(16) NOT NULL DEFAULT 'NONE',
    ADD COLUMN IF NOT EXISTS party_scope    varchar(16) NOT NULL DEFAULT 'INTERNAL',
    ADD COLUMN IF NOT EXISTS purpose        varchar(500);

ALTER TABLE biz_flow_def DROP CONSTRAINT IF EXISTS ck_biz_flow_def_flow_type;
ALTER TABLE biz_flow_def ADD CONSTRAINT ck_biz_flow_def_flow_type
    CHECK (flow_type IN ('APPROVAL', 'WORKFLOW', 'AUTOMATION'));

ALTER TABLE biz_flow_def DROP CONSTRAINT IF EXISTS ck_biz_flow_def_closure_mode;
ALTER TABLE biz_flow_def ADD CONSTRAINT ck_biz_flow_def_closure_mode
    CHECK (closure_mode IN ('ONE_WAY', 'VERIFY', 'REVISIT', 'ARCHIVE'));

ALTER TABLE biz_flow_def DROP CONSTRAINT IF EXISTS ck_biz_flow_def_archive_policy;
ALTER TABLE biz_flow_def ADD CONSTRAINT ck_biz_flow_def_archive_policy
    CHECK (archive_policy IN ('NONE', 'ON_FINISH', 'MANUAL'));

-- 维度 ⑥ 参与边界（L1/L2 恒为 INTERNAL；EXTERNAL/PUBLIC 在 L3 才放开，但字段从第一天就在）
ALTER TABLE biz_flow_def DROP CONSTRAINT IF EXISTS ck_biz_flow_def_party_scope;
ALTER TABLE biz_flow_def ADD CONSTRAINT ck_biz_flow_def_party_scope
    CHECK (party_scope IN ('INTERNAL', 'CROSS_DEPT', 'EXTERNAL', 'PUBLIC'));

-- 闭环类流程必须声明归档策略（董事会那一类：走完就得有材料包，不许事后想不起来打）
ALTER TABLE biz_flow_def DROP CONSTRAINT IF EXISTS ck_biz_flow_def_closure_archive;
ALTER TABLE biz_flow_def ADD CONSTRAINT ck_biz_flow_def_closure_archive
    CHECK (closure_mode <> 'ARCHIVE' OR archive_policy <> 'NONE');

COMMENT ON COLUMN biz_flow_def.party_scope IS
    '参与边界（维度 ⑥）：INTERNAL 内部 / CROSS_DEPT 跨部门 / EXTERNAL 含外部单位 / PUBLIC 公开；L1/L2 只允许 INTERNAL 发起';
COMMENT ON COLUMN biz_flow_def.flow_type IS
    '流程大类型：APPROVAL 审批（要你签字）/ WORKFLOW 作业（要你干活）/ AUTOMATION 自动化（无人节点，系统跑完就完）';

-- ---------- 1.2 biz_flow_node：node_type 扩到 15 值 + 协作/闭环/绑定列 ----------
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS biz_flow_node_node_type_check;
ALTER TABLE biz_flow_node ADD CONSTRAINT biz_flow_node_node_type_check CHECK (
    node_type IN (
        -- 存量 9 值（V0120 + V0178）
        'START','APPROVAL','COUNTERSIGN','CONDITION','PARALLEL','CC','END',
        'TASK_NODE','NOTIFY_NODE',
        -- ARCH-5 新增 6 值
        'FORM','CHECKLIST','EVIDENCE','WAIT','SUBPROCESS','ACTION'
    )
);

ALTER TABLE biz_flow_node
    -- 有序多签：串行 / 并行（复用 node_assignee.sort_order 作签序）
    ADD COLUMN IF NOT EXISTS sign_mode        varchar(16) NOT NULL DEFAULT 'PARALLEL',
    -- 主责 + 协办：审批节点也可有「协办人」（只知情不表决）
    ADD COLUMN IF NOT EXISTS co_assignee      boolean     NOT NULL DEFAULT false,
    -- 角色认领：派给角色池，谁先认领谁做
    ADD COLUMN IF NOT EXISTS claim_mode       varchar(16) NOT NULL DEFAULT 'NONE',
    -- 升级链：ESCALATE 时向上追几级
    ADD COLUMN IF NOT EXISTS escalate_levels  smallint    NOT NULL DEFAULT 0,
    -- 闭环三方
    ADD COLUMN IF NOT EXISTS closure_role     varchar(16) NOT NULL DEFAULT 'NONE',
    -- 作业形态绑定（FORM / CHECKLIST / SUBPROCESS 三类需要指向定义）
    ADD COLUMN IF NOT EXISTS form_def_id      uuid,
    ADD COLUMN IF NOT EXISTS checklist_def_id uuid,
    ADD COLUMN IF NOT EXISTS subprocess_def_id uuid;

ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_sign_mode;
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_sign_mode
    CHECK (sign_mode IN ('PARALLEL', 'SEQUENTIAL'));

ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_claim_mode;
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_claim_mode
    CHECK (claim_mode IN ('NONE', 'ROLE_CLAIM'));

ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_closure_role;
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_closure_role
    CHECK (closure_role IN ('NONE', 'RECTIFY', 'VERIFIER'));

ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_escalate;
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_escalate
    CHECK (escalate_levels BETWEEN 0 AND 10
           AND (timeout_action <> 'ESCALATE' OR escalate_levels >= 1));

-- 作业形态绑定：节点类型 ⇔ 绑定列，错配一律拦在库层
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_work_binding;
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_work_binding CHECK (
      (node_type <> 'FORM'       OR form_def_id      IS NOT NULL)
  AND (node_type <> 'CHECKLIST'  OR checklist_def_id IS NOT NULL)
  AND (node_type <> 'SUBPROCESS' OR subprocess_def_id IS NOT NULL)
  AND (node_type =  'FORM'       OR form_def_id      IS NULL)
  AND (node_type =  'CHECKLIST'  OR checklist_def_id IS NULL)
  AND (node_type =  'SUBPROCESS' OR subprocess_def_id IS NULL)
);

-- 每个版本恰一个验证节点（闭环 VERIFY 的库级依据；部分唯一索引 = 「至多一个」，应用层补「至少一个」）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_flow_node_verifier
    ON biz_flow_node (version_id) WHERE closure_role = 'VERIFIER';

COMMENT ON COLUMN biz_flow_node.sign_mode IS
    '会签签序：PARALLEL 同时签（存量行为）/ SEQUENTIAL 按 node_assignee.sort_order 串行签（有序多签）';
COMMENT ON COLUMN biz_flow_node.closure_role IS
    '闭环三方：NONE 无 / RECTIFY 整改人 / VERIFIER 验证人（每版本至多一个，见 uk_biz_flow_node_verifier）';

-- ---------- 1.3 biz_flow_task：主责 / 协办 / 签序 ----------
ALTER TABLE biz_flow_task
    ADD COLUMN IF NOT EXISTS task_role varchar(16) NOT NULL DEFAULT 'PRIMARY',
    ADD COLUMN IF NOT EXISTS seq_no    smallint;

ALTER TABLE biz_flow_task DROP CONSTRAINT IF EXISTS ck_biz_flow_task_role;
ALTER TABLE biz_flow_task ADD CONSTRAINT ck_biz_flow_task_role
    CHECK (task_role IN ('PRIMARY', 'CO'));

-- 注：**不建任何** (instance_node_id, assignee_id) 或 (instance_node_id, task_role) 唯一索引。
-- 理由（实测存量语义）：
--   · 存量 uk_biz_flow_task_pending 已经保证「同一节点同一人只有一条 PENDING」；
--   · 历史行必须允许多条（加签 is_added_sign=true / 转办留 TRANSFERRED 旧行）；
--   · 会签 ALL/RATIO 下**每个签名人都是 PRIMARY**，「一个节点只能有一个主责」是错的。
-- 因此 task_role 只作「是否计入表决」的标记，不做唯一性约束 —— 约束加错比不加更难拆。

COMMENT ON COLUMN biz_flow_task.task_role IS
    'PRIMARY 有表决权 / CO 协办（知情 + 可加意见，不参与 ALL/RATIO 计票）';

-- ---------- 1.4 biz_flow_instance：SUBPROCESS 节点需要父子关系 ----------
ALTER TABLE biz_flow_instance
    ADD COLUMN IF NOT EXISTS parent_instance_id uuid REFERENCES biz_flow_instance(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS parent_node_key    varchar(64);

ALTER TABLE biz_flow_instance DROP CONSTRAINT IF EXISTS ck_biz_flow_instance_parent;
ALTER TABLE biz_flow_instance ADD CONSTRAINT ck_biz_flow_instance_parent CHECK (
    (parent_instance_id IS NULL AND parent_node_key IS NULL)
    OR (parent_instance_id IS NOT NULL AND parent_node_key IS NOT NULL)
);
-- 自引用：实例不能把自己当父实例
ALTER TABLE biz_flow_instance DROP CONSTRAINT IF EXISTS ck_biz_flow_instance_parent_not_self;
ALTER TABLE biz_flow_instance ADD CONSTRAINT ck_biz_flow_instance_parent_not_self
    CHECK (parent_instance_id IS NULL OR parent_instance_id <> id);

CREATE INDEX IF NOT EXISTS idx_biz_flow_instance_parent
    ON biz_flow_instance (parent_instance_id) WHERE parent_instance_id IS NOT NULL;

COMMENT ON COLUMN biz_flow_instance.parent_instance_id IS
    '父实例（SUBPROCESS 节点产生）：NULL = 顶层实例；层级深度 ≤3 由应用层校验（V0221）';

-- ============================================================
-- 第二部分：新增 11 张表
-- ============================================================

-- ---------- 2.1 表单定义（一行 = 一个不可变版本） ----------
CREATE TABLE IF NOT EXISTS biz_flow_form_def (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    form_code    varchar(64)  NOT NULL,
    form_name    varchar(100) NOT NULL,
    version_no   integer      NOT NULL DEFAULT 1,
    status       varchar(16)  NOT NULL DEFAULT 'DRAFT',
    is_current   boolean      NOT NULL DEFAULT false,
    layout       varchar(16)  NOT NULL DEFAULT 'TWO_COL',
    category     varchar(32),
    schema       jsonb        NOT NULL,
    schema_hash  char(64),
    field_count  integer      NOT NULL DEFAULT 0,
    remark       varchar(500),
    published_at timestamptz,
    published_by uuid,
    created_at   timestamptz  NOT NULL DEFAULT now(),
    updated_at   timestamptz  NOT NULL DEFAULT now(),
    create_by    uuid,
    update_by    uuid,
    deleted_at   timestamptz,
    CONSTRAINT ck_biz_flow_form_def_code CHECK (form_code ~ '^[a-z][a-z0-9_]{1,63}$'),
    CONSTRAINT ck_biz_flow_form_def_ver  CHECK (version_no >= 1),
    CONSTRAINT ck_biz_flow_form_def_status CHECK (status IN ('DRAFT','PUBLISHED','ARCHIVED')),
    CONSTRAINT ck_biz_flow_form_def_layout CHECK (layout IN ('SINGLE','TWO_COL','GRID')),
    -- schema 必须是对象且带非空 fields 数组（空表单没有存在的理由）
    CONSTRAINT ck_biz_flow_form_def_schema CHECK (
        jsonb_typeof(schema) = 'object'
        AND jsonb_typeof(schema -> 'fields') = 'array'
        AND jsonb_array_length(schema -> 'fields') >= 1
        AND jsonb_array_length(schema -> 'fields') <= 200
    ),
    CONSTRAINT ck_biz_flow_form_def_published CHECK (
        status <> 'PUBLISHED'
        OR (schema_hash IS NOT NULL AND published_at IS NOT NULL)
    ),
    CONSTRAINT ck_biz_flow_form_def_current CHECK (NOT is_current OR status = 'PUBLISHED'),
    CONSTRAINT uk_biz_flow_form_def_ver UNIQUE (form_code, version_no)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_flow_form_def_current
    ON biz_flow_form_def (form_code) WHERE is_current AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_flow_form_def_list
    ON biz_flow_form_def (category, status, updated_at DESC) WHERE deleted_at IS NULL;

COMMENT ON TABLE biz_flow_form_def IS
    '表单定义：一行 = 一个不可变版本（改表单 = 新增一行 + 切 is_current，绝不原地改已发布行）';

-- ---------- 2.2 表单实例数据（一行 = 某实例某节点某轮的一份填写） ----------
CREATE TABLE IF NOT EXISTS biz_flow_form_data (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id      uuid NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    instance_node_id uuid NOT NULL REFERENCES biz_flow_instance_node(id) ON DELETE CASCADE,
    node_key         varchar(64) NOT NULL,
    round            integer     NOT NULL DEFAULT 1,
    form_def_id      uuid        NOT NULL REFERENCES biz_flow_form_def(id) ON DELETE RESTRICT,
    schema_hash      char(64)    NOT NULL,
    data             jsonb       NOT NULL DEFAULT '{}'::jsonb,
    field_errors     jsonb       NOT NULL DEFAULT '[]'::jsonb,
    status           varchar(16) NOT NULL DEFAULT 'DRAFT',
    submitted_by     uuid,
    submitted_at     timestamptz,
    revision         integer     NOT NULL DEFAULT 0,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_biz_flow_form_data_round CHECK (round >= 1),
    CONSTRAINT ck_biz_flow_form_data_status CHECK (status IN ('DRAFT','SUBMITTED')),
    CONSTRAINT ck_biz_flow_form_data_submitted
        CHECK ((status = 'DRAFT') OR (submitted_by IS NOT NULL AND submitted_at IS NOT NULL)),
    CONSTRAINT uk_biz_flow_form_data UNIQUE (instance_node_id, round)
);

CREATE INDEX IF NOT EXISTS idx_biz_flow_form_data_inst
    ON biz_flow_form_data (instance_id, node_key);
CREATE INDEX IF NOT EXISTS idx_biz_flow_form_data_def
    ON biz_flow_form_data (form_def_id);
-- 报表按表单字段统计：GIN(jsonb_path_ops) 是所有 jsonb 过滤的通用入口
CREATE INDEX IF NOT EXISTS idx_biz_flow_form_data_gin
    ON biz_flow_form_data USING gin (data jsonb_path_ops);

COMMENT ON TABLE biz_flow_form_data IS
    '作业表单数据：一行 = 某实例某节点某轮的一份填写（含 DRAFT 草稿）；schema_hash 是提交时的定义指纹';

-- ---------- 2.3 检查清单定义（版本化） ----------
CREATE TABLE IF NOT EXISTS biz_flow_checklist_def (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    checklist_code  varchar(64)  NOT NULL,
    checklist_name  varchar(100) NOT NULL,
    version_no      integer      NOT NULL DEFAULT 1,
    status          varchar(16)  NOT NULL DEFAULT 'DRAFT',
    is_current      boolean      NOT NULL DEFAULT false,
    category        varchar(32),
    fail_action     varchar(16)  NOT NULL DEFAULT 'STAY',
    item_count      integer      NOT NULL DEFAULT 0,
    remark          varchar(500),
    published_at    timestamptz,
    published_by    uuid,
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now(),
    create_by       uuid,
    update_by       uuid,
    deleted_at      timestamptz,
    CONSTRAINT ck_biz_flow_checklist_def_code CHECK (checklist_code ~ '^[a-z][a-z0-9_]{1,63}$'),
    CONSTRAINT ck_biz_flow_checklist_def_ver CHECK (version_no >= 1),
    CONSTRAINT ck_biz_flow_checklist_def_status CHECK (status IN ('DRAFT','PUBLISHED','ARCHIVED')),
    CONSTRAINT ck_biz_flow_checklist_def_fail CHECK (fail_action IN ('STAY','TO_RECTIFY','TO_APPROVAL','REJECT')),
    CONSTRAINT ck_biz_flow_checklist_def_published CHECK (status <> 'PUBLISHED' OR published_at IS NOT NULL),
    CONSTRAINT uk_biz_flow_checklist_def_ver UNIQUE (checklist_code, version_no)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_flow_checklist_def_current
    ON biz_flow_checklist_def (checklist_code) WHERE is_current AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_flow_checklist_def_list
    ON biz_flow_checklist_def (category, status, updated_at DESC) WHERE deleted_at IS NULL;

COMMENT ON TABLE biz_flow_checklist_def IS
    '检查清单定义（版本化）：fail_action 决定「查出不合格后流程往哪走」，是判定与分支的配置入口';

-- ---------- 2.4 检查项（一等公民） ----------
CREATE TABLE IF NOT EXISTS biz_flow_checklist_item (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    checklist_def_id    uuid NOT NULL REFERENCES biz_flow_checklist_def(id) ON DELETE CASCADE,
    item_key            varchar(64)  NOT NULL,
    seq                 integer      NOT NULL DEFAULT 0,
    item_text           varchar(500) NOT NULL,
    group_name          varchar(64),
    standard_text       text,
    standard_file_id    uuid REFERENCES sys_file(id) ON DELETE SET NULL,
    is_required         boolean      NOT NULL DEFAULT true,
    input_type          varchar(16)  NOT NULL DEFAULT 'BOOLEAN',
    options             jsonb        NOT NULL DEFAULT '[]'::jsonb,
    unit                varchar(16),
    min_value           numeric(20,6),
    max_value           numeric(20,6),
    pass_rule           varchar(16)  NOT NULL DEFAULT 'NONE',
    pass_regex          varchar(200),
    evidence_min_photos smallint     NOT NULL DEFAULT 0,
    allow_skip          boolean      NOT NULL DEFAULT false,
    skip_auth_code      varchar(64),
    weight              numeric(6,3) NOT NULL DEFAULT 1,
    created_at          timestamptz  NOT NULL DEFAULT now(),
    CONSTRAINT ck_biz_flow_checklist_item_key CHECK (item_key ~ '^[A-Za-z][A-Za-z0-9_]{0,63}$'),
    CONSTRAINT ck_biz_flow_checklist_item_text CHECK (length(btrim(item_text)) > 0),
    CONSTRAINT ck_biz_flow_checklist_item_input
        CHECK (input_type IN ('BOOLEAN','TEXT','NUMBER','ENUM','DATE','PHOTO')),
    CONSTRAINT ck_biz_flow_checklist_item_rule
        CHECK (pass_rule IN ('NONE','RANGE','EQ','IN_SET','REGEX')),
    CONSTRAINT ck_biz_flow_checklist_item_range
        CHECK (min_value IS NULL OR max_value IS NULL OR min_value <= max_value),
    -- 判合格必须有判据：RANGE 要边界、REGEX 要正则、IN_SET 要选项
    CONSTRAINT ck_biz_flow_checklist_item_judgeable CHECK (
        (pass_rule <> 'RANGE'  OR min_value IS NOT NULL OR max_value IS NOT NULL)
        AND (pass_rule <> 'REGEX' OR pass_regex IS NOT NULL)
        AND (pass_rule <> 'IN_SET' OR jsonb_array_length(options) >= 1)
    ),
    -- 跳过必须有人授权：allow_skip 为真且给了权限码
    CONSTRAINT ck_biz_flow_checklist_item_skip
        CHECK (NOT allow_skip OR skip_auth_code IS NOT NULL),
    CONSTRAINT ck_biz_flow_checklist_item_photos CHECK (evidence_min_photos BETWEEN 0 AND 20),
    CONSTRAINT uk_biz_flow_checklist_item UNIQUE (checklist_def_id, item_key)
);

CREATE INDEX IF NOT EXISTS idx_biz_flow_checklist_item_def
    ON biz_flow_checklist_item (checklist_def_id, seq);

COMMENT ON TABLE biz_flow_checklist_item IS
    '检查项（一等公民，不是任务备注）：有标准 / 必做开关 / 输入类型 / 合格范围 / 跳过权限 / 权重';

-- ---------- 2.5 检查执行结果 ----------
CREATE TABLE IF NOT EXISTS biz_flow_checklist_result (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id      uuid NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    instance_node_id uuid NOT NULL REFERENCES biz_flow_instance_node(id) ON DELETE CASCADE,
    node_key         varchar(64) NOT NULL,
    round            integer     NOT NULL DEFAULT 1,
    checklist_def_id uuid        NOT NULL REFERENCES biz_flow_checklist_def(id) ON DELETE RESTRICT,
    item_id          uuid        NOT NULL REFERENCES biz_flow_checklist_item(id) ON DELETE RESTRICT,
    item_key         varchar(64) NOT NULL,
    seq              integer     NOT NULL DEFAULT 0,
    is_required      boolean     NOT NULL DEFAULT true,
    result           varchar(16) NOT NULL,
    input_value      jsonb,
    in_range         boolean,
    note             varchar(500),
    skipped_reason   varchar(500),
    photo_count      smallint    NOT NULL DEFAULT 0,
    checked_by       uuid,
    checked_at       timestamptz,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_biz_flow_checklist_result_round CHECK (round >= 1),
    CONSTRAINT ck_biz_flow_checklist_result_value
        CHECK (result IN ('PASS','FAIL','SKIPPED','NA')),
    -- 跳过必须有原因；SKIPPED 只有 allow_skip 的项才允许（应用层校验，库层保留下限）
    CONSTRAINT ck_biz_flow_checklist_result_skip
        CHECK (result <> 'SKIPPED' OR (skipped_reason IS NOT NULL AND length(btrim(skipped_reason)) > 0)),
    CONSTRAINT uk_biz_flow_checklist_result UNIQUE (instance_node_id, round, item_id)
);

CREATE INDEX IF NOT EXISTS idx_biz_flow_checklist_result_inst
    ON biz_flow_checklist_result (instance_id, node_key, round);
CREATE INDEX IF NOT EXISTS idx_biz_flow_checklist_result_fail
    ON biz_flow_checklist_result (item_id, result) WHERE result = 'FAIL';

COMMENT ON TABLE biz_flow_checklist_result IS
    '检查执行结果：一行 = 一项一轮的判定；防漏靠 uk_…(instance_node_id, round, item_id) + 完成闸数条数';

-- ---------- 2.6 节点证据要求（定义侧） ----------
CREATE TABLE IF NOT EXISTS biz_flow_evidence_req (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    node_id        uuid NOT NULL REFERENCES biz_flow_node(id) ON DELETE CASCADE,
    req_key        varchar(64)  NOT NULL,
    req_name       varchar(100) NOT NULL,
    evidence_type  varchar(16)  NOT NULL DEFAULT 'FILE',
    min_count      smallint     NOT NULL DEFAULT 1,
    max_count      smallint     NOT NULL DEFAULT 10,
    mime_allow     varchar(200),
    max_size_mb    integer      NOT NULL DEFAULT 20,
    require_sign   boolean      NOT NULL DEFAULT false,
    signer_rule    varchar(24)  NOT NULL DEFAULT 'SAME_AS_ASSIGNEE',
    signer_refs    jsonb        NOT NULL DEFAULT '[]'::jsonb,
    capture_geo    boolean      NOT NULL DEFAULT false,
    is_required    boolean      NOT NULL DEFAULT true,
    sort_order     integer      NOT NULL DEFAULT 0,
    created_at     timestamptz  NOT NULL DEFAULT now(),
    CONSTRAINT ck_biz_flow_evidence_req_key CHECK (req_key ~ '^[A-Za-z][A-Za-z0-9_]{0,63}$'),
    CONSTRAINT ck_biz_flow_evidence_req_type
        CHECK (evidence_type IN ('FILE','PHOTO','VIDEO','AUDIO','SCAN','SIGN')),
    CONSTRAINT ck_biz_flow_evidence_req_count
        CHECK (min_count >= 1 AND max_count >= min_count AND max_count <= 50),
    CONSTRAINT ck_biz_flow_evidence_req_signer
        CHECK (signer_rule IN ('SAME_AS_ASSIGNEE','INITIATOR','INITIATOR_SUPERIOR','USER_LIST','ANY')),
    -- 要人签就必须有可解析的签署人来源；SIGN 类型本身就是「要签」
    CONSTRAINT ck_biz_flow_evidence_req_sign CHECK (
        evidence_type <> 'SIGN' OR require_sign
    ),
    CONSTRAINT ck_biz_flow_evidence_req_signer_list CHECK (
        NOT require_sign OR signer_rule <> 'USER_LIST' OR jsonb_array_length(signer_refs) >= 1
    ),
    CONSTRAINT uk_biz_flow_evidence_req UNIQUE (node_id, req_key)
);

CREATE INDEX IF NOT EXISTS idx_biz_flow_evidence_req_node
    ON biz_flow_evidence_req (node_id, sort_order);

COMMENT ON TABLE biz_flow_evidence_req IS
    '节点证据要求：类型 / 数量 / 大小 / 是否要签 / 谁签 —— EVIDENCE 节点没有它就不许发布（R22）';

-- ---------- 2.7 证据记录（运行侧） ----------
CREATE TABLE IF NOT EXISTS biz_flow_evidence (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id      uuid NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    instance_node_id uuid NOT NULL REFERENCES biz_flow_instance_node(id) ON DELETE CASCADE,
    node_key         varchar(64) NOT NULL,
    round            integer     NOT NULL DEFAULT 1,
    req_id           uuid REFERENCES biz_flow_evidence_req(id) ON DELETE SET NULL,
    req_key          varchar(64),
    evidence_type    varchar(16) NOT NULL DEFAULT 'FILE',
    file_id          uuid REFERENCES sys_file(id) ON DELETE RESTRICT,
    file_hash        char(64),
    caption          varchar(200),
    captured_at      timestamptz,
    captured_geo     varchar(64),
    signed_by        uuid,
    signed_at        timestamptz,
    signature_data   jsonb,
    status           varchar(16) NOT NULL DEFAULT 'VALID',
    revoked_reason   varchar(500),
    uploaded_by      uuid,
    created_at       timestamptz NOT NULL DEFAULT now(),
    deleted_at       timestamptz,
    CONSTRAINT ck_biz_flow_evidence_round CHECK (round >= 1),
    CONSTRAINT ck_biz_flow_evidence_type
        CHECK (evidence_type IN ('FILE','PHOTO','VIDEO','AUDIO','SCAN','SIGN')),
    CONSTRAINT ck_biz_flow_evidence_status CHECK (status IN ('VALID','REVOKED')),
    CONSTRAINT ck_biz_flow_evidence_file
        CHECK (evidence_type = 'SIGN' OR file_id IS NOT NULL),
    CONSTRAINT ck_biz_flow_evidence_revoked
        CHECK (status <> 'REVOKED' OR revoked_reason IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_biz_flow_evidence_inst
    ON biz_flow_evidence (instance_id, node_key, round) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_flow_evidence_req
    ON biz_flow_evidence (req_id) WHERE deleted_at IS NULL;
-- 同一文件不许在同一节点同一轮挂两次（重复上传的唯一拦点）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_flow_evidence_file
    ON biz_flow_evidence (instance_node_id, round, file_id)
    WHERE file_id IS NOT NULL AND deleted_at IS NULL;

COMMENT ON TABLE biz_flow_evidence IS
    '证据记录：一行 = 一份材料（或一次签署）；req_id 为空的额外证据允许存在，但不计入 req 的达标数量';

-- ---------- 2.8 归档包 ----------
CREATE TABLE IF NOT EXISTS biz_flow_archive_pack (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id    uuid NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    pack_no        varchar(32) NOT NULL,
    status         varchar(16) NOT NULL DEFAULT 'PENDING',
    format         varchar(16) NOT NULL DEFAULT 'ZIP',
    manifest       jsonb       NOT NULL DEFAULT '{}'::jsonb,
    zip_file_id    uuid REFERENCES sys_file(id) ON DELETE SET NULL,
    pdf_file_id    uuid REFERENCES sys_file(id) ON DELETE SET NULL,
    item_count     integer     NOT NULL DEFAULT 0,
    total_bytes    bigint      NOT NULL DEFAULT 0,
    sha256         char(64),
    retention_until date,
    download_count integer     NOT NULL DEFAULT 0,
    built_at       timestamptz,
    build_ms       integer,
    error_msg      varchar(1000),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    created_by     uuid,
    CONSTRAINT ck_biz_flow_archive_pack_status
        CHECK (status IN ('PENDING','BUILDING','READY','FAILED','EXPIRED')),
    CONSTRAINT ck_biz_flow_archive_pack_format CHECK (format IN ('ZIP','PDF','BOTH')),
    -- READY 必须真的有产物 + 指纹（否则「下载得到一个空包」是最坏的审计事故）
    CONSTRAINT ck_biz_flow_archive_pack_ready CHECK (
        status <> 'READY'
        OR ((format <> 'PDF' AND zip_file_id IS NOT NULL)
            AND (format <> 'ZIP' AND pdf_file_id IS NOT NULL)
            AND sha256 IS NOT NULL AND built_at IS NOT NULL)
    ),
    CONSTRAINT uk_biz_flow_archive_pack_no UNIQUE (pack_no)
);

CREATE INDEX IF NOT EXISTS idx_biz_flow_archive_pack_inst
    ON biz_flow_archive_pack (instance_id, created_at DESC);

COMMENT ON TABLE biz_flow_archive_pack IS
    '归档包：一行 = 一次打包任务；manifest 记录「打了什么」（材料清单 + 时间线 + 签署记录），sha256 供事后校验';

-- ---------- 2.9 触发器 ----------
CREATE TABLE IF NOT EXISTS biz_flow_trigger (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    def_id             uuid NOT NULL REFERENCES biz_flow_def(id) ON DELETE CASCADE,
    trigger_name       varchar(100) NOT NULL,
    trigger_kind       varchar(16)  NOT NULL,
    enabled            boolean      NOT NULL DEFAULT true,
    priority           integer      NOT NULL DEFAULT 0,
    -- 幂等策略
    dedupe_strategy    varchar(16)  NOT NULL DEFAULT 'NONE',
    dedupe_expr        varchar(300),
    idem_window_sec    integer,
    -- CRON
    cron_expr          varchar(100),
    cron_timezone      varchar(64) DEFAULT 'Asia/Shanghai',
    misfire_policy     varchar(16)  NOT NULL DEFAULT 'SKIP',
    catch_up_limit     smallint     NOT NULL DEFAULT 1,
    -- EVENT / CONDITION / DOC
    event_code         varchar(64),
    entity             varchar(32),
    match_expr         jsonb,
    where_expr         jsonb,
    scan_cron          varchar(100),
    -- 实例化参数
    title_template     varchar(200) NOT NULL DEFAULT '{{def.flow_name}}',
    param_mapping      jsonb        NOT NULL DEFAULT '{}'::jsonb,
    initiator_strategy varchar(16)  NOT NULL DEFAULT 'ROLE',
    initiator_ref      uuid,
    -- 运行态
    last_fire_at       timestamptz,
    next_fire_at       timestamptz,
    fire_count         integer      NOT NULL DEFAULT 0,
    fail_count         integer      NOT NULL DEFAULT 0,
    last_error         varchar(1000),
    created_at         timestamptz  NOT NULL DEFAULT now(),
    updated_at         timestamptz  NOT NULL DEFAULT now(),
    create_by          uuid,
    update_by          uuid,
    deleted_at         timestamptz,
    CONSTRAINT ck_biz_flow_trigger_kind
        CHECK (trigger_kind IN ('MANUAL','DOC','CRON','EVENT','CONDITION')),
    CONSTRAINT ck_biz_flow_trigger_dedupe
        CHECK (dedupe_strategy IN ('NONE','NATURAL_KEY','EXPR','WINDOW')),
    CONSTRAINT ck_biz_flow_trigger_misfire
        CHECK (misfire_policy IN ('SKIP','CATCH_UP','ONCE')),
    CONSTRAINT ck_biz_flow_trigger_initiator
        CHECK (initiator_strategy IN ('ROLE','USER','SYSTEM','EVENT_OWNER')),
    -- CRON 必须有 cron；EVENT/CONDITION 必须有事件码或条件；DOC 必须有实体
    CONSTRAINT ck_biz_flow_trigger_shape CHECK (
        (trigger_kind <> 'CRON'      OR cron_expr IS NOT NULL)
        AND (trigger_kind <> 'EVENT' OR event_code IS NOT NULL)
        AND (trigger_kind <> 'CONDITION' OR (where_expr IS NOT NULL AND entity IS NOT NULL))
        AND (trigger_kind <> 'DOC'   OR (entity IS NOT NULL AND event_code IS NOT NULL))
    ),
    -- 幂等策略与表达式必须配套（声明了策略却没键 = 假幂等）
    CONSTRAINT ck_biz_flow_trigger_dedupe_expr CHECK (
        (dedupe_strategy <> 'EXPR'   OR dedupe_expr IS NOT NULL)
        AND (dedupe_strategy <> 'WINDOW' OR (dedupe_expr IS NOT NULL AND idem_window_sec IS NOT NULL))
    ),
    -- 禁止 SQL 走私（与 biz_flow_linkage_rule.ck_flow_linkage_no_sql_key 同款）
    CONSTRAINT ck_biz_flow_trigger_no_sql CHECK (
        NOT (match_expr ?| ARRAY['sql','sql_text','statement','query','script','shell','cmd']
             OR where_expr ?| ARRAY['sql','sql_text','statement','query','script','shell','cmd']
             OR param_mapping ?| ARRAY['sql','sql_text','statement','query','script','shell','cmd'])
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_flow_trigger_code
    ON biz_flow_trigger (def_id, trigger_kind, coalesce(event_code, ''), coalesce(cron_expr, ''))
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_flow_trigger_due
    ON biz_flow_trigger (next_fire_at) WHERE enabled AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_flow_trigger_event
    ON biz_flow_trigger (entity, event_code) WHERE enabled AND deleted_at IS NULL;

COMMENT ON TABLE biz_flow_trigger IS
    '触发器：CRON 定时 / EVENT 事件 / CONDITION 条件扫描 / DOC 单据驱动（MANUAL 行不落本表，发起中心直接调 create_instance）';

-- ---------- 2.10 触发台账（幂等第二层） ----------
CREATE TABLE IF NOT EXISTS biz_flow_trigger_fire (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    trigger_id   uuid NOT NULL REFERENCES biz_flow_trigger(id) ON DELETE CASCADE,
    dedupe_key   varchar(200) NOT NULL,
    fired_at     timestamptz NOT NULL DEFAULT now(),
    instance_id  uuid REFERENCES biz_flow_instance(id) ON DELETE SET NULL,
    status       varchar(16) NOT NULL DEFAULT 'FIRED',
    error_msg    varchar(1000),
    CONSTRAINT ck_biz_flow_trigger_fire_status
        CHECK (status IN ('FIRED','SKIPPED_DUP','FAILED')),
    CONSTRAINT uk_biz_flow_trigger_fire UNIQUE (trigger_id, dedupe_key)
);

CREATE INDEX IF NOT EXISTS idx_biz_flow_trigger_fire_recent
    ON biz_flow_trigger_fire (trigger_id, fired_at DESC);

COMMENT ON TABLE biz_flow_trigger_fire IS
    '触发台账：uk(trigger_id, dedupe_key) 是「同一事件不触发两次」的库级唯一依据（23505 当幂等成功）';

-- ---------- 2.11 谁能发起（按角色 / 部门 / 用户） ----------
CREATE TABLE IF NOT EXISTS biz_flow_def_initiator (
    def_id     uuid NOT NULL REFERENCES biz_flow_def(id) ON DELETE CASCADE,
    party_type varchar(16) NOT NULL,
    ref_id     uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (def_id, party_type, ref_id),
    CONSTRAINT ck_biz_flow_def_initiator_party CHECK (party_type IN ('ROLE','DEPT','USER'))
);

CREATE INDEX IF NOT EXISTS idx_biz_flow_def_initiator_ref
    ON biz_flow_def_initiator (party_type, ref_id);

COMMENT ON TABLE biz_flow_def_initiator IS
    '谁能发起这个流程（空集 = 所有有 business:flow:instance:create 的人）；独立表理由同 biz_flow_node_assignee：可外键、可反查影响面';

-- ============================================================
-- 第三部分：自检（失败即 POC 失败）
-- ============================================================
DO $$
DECLARE
    n_tbl  int;
    n_col  int;
    n_idx  int;
BEGIN
    SELECT count(*) INTO n_tbl
      FROM information_schema.tables
     WHERE table_schema = current_schema()
       AND table_name IN ('biz_flow_form_def','biz_flow_form_data','biz_flow_checklist_def',
                          'biz_flow_checklist_item','biz_flow_checklist_result',
                          'biz_flow_evidence_req','biz_flow_evidence','biz_flow_archive_pack',
                          'biz_flow_trigger','biz_flow_trigger_fire','biz_flow_def_initiator');
    IF n_tbl <> 11 THEN
        RAISE EXCEPTION 'ARCH-5 POC：新表只建了 % 张（应 11 张）', n_tbl;
    END IF;

    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_flow_node'
       AND column_name IN ('sign_mode','co_assignee','claim_mode','escalate_levels',
                           'closure_role','form_def_id','checklist_def_id','subprocess_def_id');
    IF n_col <> 8 THEN
        RAISE EXCEPTION 'ARCH-5 POC：biz_flow_node 新列只加了 % 个（应 8 个）', n_col;
    END IF;

    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND ((table_name = 'biz_flow_def'
             AND column_name IN ('flow_type','closure_mode','archive_policy','party_scope','purpose'))
         OR (table_name = 'biz_flow_instance'
             AND column_name IN ('parent_instance_id','parent_node_key'))
         OR (table_name = 'biz_flow_task'
             AND column_name IN ('task_role','seq_no')));
    IF n_col <> 9 THEN
        RAISE EXCEPTION 'ARCH-5 POC：def/instance/task 新列只加了 % 个（应 9 个）', n_col;
    END IF;

    SELECT count(*) INTO n_idx
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND indexname IN ('uk_biz_flow_form_def_current','uk_biz_flow_checklist_def_current',
                         'uk_biz_flow_evidence_file','uk_biz_flow_trigger_fire',
                         'uk_biz_flow_node_verifier','idx_biz_flow_form_data_gin');
    IF n_idx <> 6 THEN
        RAISE EXCEPTION 'ARCH-5 POC：关键索引只建了 % 条（应 6 条）', n_idx;
    END IF;

    RAISE NOTICE 'ARCH-5 POC 自检通过：11 张新表 + node 8 列 + def/instance/task 9 列 + 6 条关键索引';
END
$$;
