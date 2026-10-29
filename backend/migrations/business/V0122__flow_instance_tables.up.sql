-- ======================================================
-- V0122：审批流 · 运行侧 4 张表（批次 D · D2）
--
-- 依据：卡面 t_3715c446「1. 迁移 V0122（4 张运行侧表）」
--       + `docs/批次D-工作流审批-技术架构.md` §6.2（完整 DDL，POC 已实跑）
--       + §4.1（实例状态机 5 态）/ §4.2（节点状态机 6 态）/ §4.3（8 种审批人 + 三层去重 + 冻结）
--       + §4.6（退回 round）/ §4.9（超时）
-- 回滚：`V0122__flow_instance_tables.down.sql`（逆序 DROP，含零残留断言）
--
-- ── 为什么编号是 V0122（不是文档的 V0116） ────────────────────────
-- 与 V0120/V0121 同一原因：V0115 已被 T7c 占用，D1 拿走 V0120/V0121，D2 顺延到 V0122/V0123。
--
-- ── 四个刻意的建模决策（与文档 §6.2 逐字一致，勿改） ──────────────
-- ① `instance.version_id` **非空** + `ON DELETE RESTRICT`：实例绑版本（架构 D2）。
--    有实例引用的版本不能删；引擎只读 `version.graph_snapshot`，永不跟随 `def.current_version_id`。
-- ② `instance` 冗余 `flow_code` / `flow_name` / `version_no`：展示不需要 JOIN 定义表
--    —— 这正是坑 2「版本漂移的最隐蔽子坑」（展示时顺手 JOIN 后再顺手取 current_version_id）。
-- ③ `instance_no` 全表唯一（含软删行）：编号不复用，审计可回溯（与 `biz_task.task_no` 同款）。
-- ④ `uk_biz_flow_instance_biz_active` 部分唯一索引：同一业务单据同时只能有一个在跑实例
--    —— 双击 / 重试 / 并发提交的库级兜底。
--
-- ── 三层去重（D14）在库层的落点 ──────────────────────────────────
-- 第 2 层 = `uk_biz_flow_task_pending (instance_node_id, assignee_id) WHERE status = 'PENDING'`。
-- 应用层把 23505 当**幂等成功**（不是 500），见 `flow/engine.rs` 的 `is_pending_dup`。
-- 第 3 层 = 计票一律 `COUNT(*)` 现算，`approved_count` / `rejected_count` 只是展示冗余。
--
-- ── 编号生成工具 ────────────────────────────────────────────────
-- `instance_no` 形如 `FLW-20261029-XXXXXXXX`：日期段 + UUIDv7 **末 8 位**（随机段）。
-- 不用前缀：v7 高位就是时间戳，同一毫秒连发会撞号（skill cailu-dev §9.12-3 实测踩过）。
-- ======================================================

-- ---------- 6) 实例 ----------
CREATE TABLE biz_flow_instance (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_no       VARCHAR(32) NOT NULL,
    def_id            UUID NOT NULL REFERENCES biz_flow_def(id) ON DELETE RESTRICT,
    version_id        UUID NOT NULL REFERENCES biz_flow_def_version(id) ON DELETE RESTRICT,
    version_no        INT NOT NULL,
    flow_code         VARCHAR(64) NOT NULL,
    flow_name         VARCHAR(100) NOT NULL,
    biz_type          VARCHAR(32) NOT NULL,
    biz_id            UUID,
    biz_no            VARCHAR(32),
    title             VARCHAR(200) NOT NULL,
    status            VARCHAR(16) NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING','APPROVED','REJECTED','CANCELLED','WITHDRAWN')),
    initiator_id      UUID NOT NULL REFERENCES sys_user(id),
    initiator_name    VARCHAR(50),
    initiator_dept_id UUID,
    form_data         JSONB NOT NULL DEFAULT '{}'::jsonb,
    result_comment    VARCHAR(1000),
    auto_passed_nodes INT NOT NULL DEFAULT 0 CHECK (auto_passed_nodes >= 0),
    round_total       INT NOT NULL DEFAULT 1 CHECK (round_total >= 1),
    prev_instance_id  UUID REFERENCES biz_flow_instance(id),
    revision          INT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    started_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at          TIMESTAMPTZ,
    due_at            TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,
    -- 等值写法同时拦住「PENDING 却有结束时间」与「终态却没有结束时间」两种脏值（文档 §6.2）
    CONSTRAINT ck_biz_flow_instance_end
        CHECK ((status = 'PENDING') = (ended_at IS NULL))
);

CREATE UNIQUE INDEX uk_biz_flow_instance_no ON biz_flow_instance (instance_no);
CREATE UNIQUE INDEX uk_biz_flow_instance_biz_active ON biz_flow_instance (biz_type, biz_id)
    WHERE status = 'PENDING' AND biz_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX idx_biz_flow_instance_initiator ON biz_flow_instance (initiator_id, started_at DESC);
CREATE INDEX idx_biz_flow_instance_def ON biz_flow_instance (def_id, status, started_at DESC);
CREATE INDEX idx_biz_flow_instance_biz ON biz_flow_instance (biz_type, biz_id);

COMMENT ON TABLE biz_flow_instance IS '审批实例（一次发起 = 一张单；绑 version_id，引擎只读该版本的 graph_snapshot）';
COMMENT ON COLUMN biz_flow_instance.version_id IS '实例绑定的版本（架构 D2：永不跟随 def.current_version_id）';
COMMENT ON COLUMN biz_flow_instance.flow_code IS '流程编码冗余（展示不 JOIN 定义表 → 防版本漂移子坑）';
COMMENT ON COLUMN biz_flow_instance.auto_passed_nodes IS '自审 / 空审批人自动跳过的节点数（审计口径，坑 3 最危险组合的唯一痕迹）';
COMMENT ON COLUMN biz_flow_instance.round_total IS '当前最大退回轮次（打回率 = round_total - 1）';
COMMENT ON COLUMN biz_flow_instance.revision IS 'CAS 版本号（并发三件套之二）';

-- ---------- 7) 实例节点 ----------
CREATE TABLE biz_flow_instance_node (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id    UUID NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    node_id        UUID REFERENCES biz_flow_node(id) ON DELETE SET NULL,
    node_key       VARCHAR(64) NOT NULL,
    node_name      VARCHAR(100) NOT NULL,
    node_type      VARCHAR(16) NOT NULL,
    round          INT NOT NULL DEFAULT 1 CHECK (round >= 1),
    status         VARCHAR(16) NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING','ACTIVE','APPROVED','REJECTED','SKIPPED','CANCELLED')),
    approval_mode  VARCHAR(8),
    approve_ratio  NUMERIC(4,3),
    reject_veto    BOOLEAN,
    approver_count INT NOT NULL DEFAULT 0 CHECK (approver_count >= 0),
    approved_count INT NOT NULL DEFAULT 0 CHECK (approved_count >= 0),
    rejected_count INT NOT NULL DEFAULT 0 CHECK (rejected_count >= 0),
    taken_edge_key VARCHAR(64),
    is_auto_passed BOOLEAN NOT NULL DEFAULT FALSE,
    activated_at   TIMESTAMPTZ,
    ended_at       TIMESTAMPTZ,
    duration_ms    BIGINT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uk_biz_flow_instance_node UNIQUE (instance_id, node_key, round)
);

CREATE INDEX idx_biz_flow_instance_node_inst ON biz_flow_instance_node (instance_id, round);
CREATE INDEX idx_biz_flow_instance_node_stat ON biz_flow_instance_node (node_key, status, duration_ms)
    WHERE ended_at IS NOT NULL;

COMMENT ON TABLE biz_flow_instance_node IS '实例节点（按「到达才建行」懒创建；退回 = round + 1 的新行，第 1 轮历史保留）';
COMMENT ON COLUMN biz_flow_instance_node.round IS '退回轮次：同一节点每个轮次一行，判定时必须带 round 条件（坑 4）';
COMMENT ON COLUMN biz_flow_instance_node.approved_count IS '展示冗余，权威值是 biz_flow_task 的 COUNT(*) 现算（禁止 +=）';
COMMENT ON COLUMN biz_flow_instance_node.is_auto_passed IS '本节点因自审跳过 / 空审批人自动通过';

-- ---------- 8) 待办 / 审批记录 ----------
CREATE TABLE biz_flow_task (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id         UUID NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    instance_node_id    UUID NOT NULL REFERENCES biz_flow_instance_node(id) ON DELETE CASCADE,
    node_key            VARCHAR(64) NOT NULL,
    round               INT NOT NULL DEFAULT 1 CHECK (round >= 1),
    task_kind           VARCHAR(16) NOT NULL DEFAULT 'APPROVAL'
        CHECK (task_kind IN ('APPROVAL','RESUBMIT')),
    assignee_id         UUID NOT NULL REFERENCES sys_user(id),
    assignee_name       VARCHAR(50),
    assignee_source     VARCHAR(24) NOT NULL
        CHECK (assignee_source IN ('USER','ROLE','INITIATOR','INITIATOR_SUPERIOR','DEPT_LEADER',
                                   'TASK_OWNER','TASK_CREATOR','TASK_ASSIGNER','FORM_VAR','ADMIN')),
    assignee_source_ref UUID,
    status              VARCHAR(16) NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING','APPROVED','REJECTED','CANCELLED','SKIPPED','TRANSFERRED')),
    is_added_sign       BOOLEAN NOT NULL DEFAULT FALSE,
    is_self_approval    BOOLEAN NOT NULL DEFAULT FALSE,
    is_auto             BOOLEAN NOT NULL DEFAULT FALSE,
    comment             VARCHAR(1000),
    attachments         JSONB NOT NULL DEFAULT '[]'::jsonb,
    transferred_from    UUID REFERENCES biz_flow_task(id) ON DELETE SET NULL,
    transferred_to      UUID,
    due_at              TIMESTAMPTZ,
    remind_count        SMALLINT NOT NULL DEFAULT 0,
    last_remind_at      TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at        TIMESTAMPTZ,
    CONSTRAINT ck_biz_flow_task_done CHECK ((status = 'PENDING') = (completed_at IS NULL))
);

-- 三层去重的第 2 层（D14）：同一节点同一人只能有一条 PENDING 待办。
-- 应用层把 23505 当幂等成功（不是 500）—— 见 engine.rs 的 `is_pending_dup`。
CREATE UNIQUE INDEX uk_biz_flow_task_pending ON biz_flow_task (instance_node_id, assignee_id)
    WHERE status = 'PENDING';
CREATE INDEX idx_biz_flow_task_mine ON biz_flow_task (assignee_id, status, created_at DESC);
CREATE INDEX idx_biz_flow_task_inst ON biz_flow_task (instance_id, created_at);
CREATE INDEX idx_biz_flow_task_due ON biz_flow_task (due_at)
    WHERE status = 'PENDING' AND due_at IS NOT NULL;

COMMENT ON TABLE biz_flow_task IS '待办 / 审批记录（审批人冻结成行，架构 D13：角色改了也不影响在跑的单）';
COMMENT ON COLUMN biz_flow_task.assignee_source IS '8 种来源标记（D18：隐式来源过滤 ROBOT，显式 USER 不受限）';
COMMENT ON COLUMN biz_flow_task.assignee_source_ref IS '来源角色 / 部门 id（回答「为什么是他审我」）';
COMMENT ON COLUMN biz_flow_task.status IS 'PENDING/APPROVED/REJECTED/CANCELLED/SKIPPED/TRANSFERRED；SKIPPED = 自审跳过留痕（不静默丢弃）';
COMMENT ON COLUMN biz_flow_task.transferred_from IS '转办来源行（旧行留 TRANSFERRED，历史不丢）';
COMMENT ON COLUMN biz_flow_task.is_auto IS '系统代点（超时自动通过/驳回、空审批人跳过）；D10 要求 allow_auto_approve 全局闸';

-- ---------- 9) 抄送 ----------
CREATE TABLE biz_flow_cc (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id   UUID NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    node_key      VARCHAR(64) NOT NULL,
    round         INT NOT NULL DEFAULT 1 CHECK (round >= 1),
    receiver_id   UUID NOT NULL REFERENCES sys_user(id),
    receiver_name VARCHAR(50),
    source_type   VARCHAR(24) NOT NULL,
    source_ref    UUID,
    is_read       BOOLEAN NOT NULL DEFAULT FALSE,
    read_at       TIMESTAMPTZ,
    notice_id     UUID,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uk_biz_flow_cc UNIQUE (instance_id, node_key, round, receiver_id)
);

CREATE INDEX idx_biz_flow_cc_mine ON biz_flow_cc (receiver_id, created_at DESC);

COMMENT ON TABLE biz_flow_cc IS '抄送（架构 D22：非阻塞，不进 biz_flow_task —— 否则抄送人会占用待办唯一槽位）';
COMMENT ON COLUMN biz_flow_cc.source_type IS '抄送来源（与 assignee_source 同取值域）';
