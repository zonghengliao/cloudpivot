-- ======================================================
-- V0175：应付付款审批（批次 2 · P17）—— 金额分级 + 内控（制单人不能批自己）
--
--        ① `biz_payment` 加 `approval_instance_id` / `approval_status`
--        ② `biz_payable` 加 `payment_approval_instance_id`（在跑付款审批的弱引用）
--        ③ 种流程 `payable_pay`（PAYABLE_PAY）：START → 条件（金额）→ 三级审批 → END
--
-- ── 为什么要这条迁移 ────────────────────────────────────
-- P7 的付款是「点一下就出钱」：`POST /business/erp/payable/{id}/pay` 直接记付款、出凭证、
-- 改应付状态，**没有任何审批字段、没有任何分级**。付款是真出钱，内控要求
-- 「< 1 万主管 / 1–10 万财务经理 / ≥ 10 万财务总监+总经理会签」。
--
-- 本迁移把金额分级**做成流程定义里的条件节点**（而不是代码里的 if/else）：
--
--   START → 条件① amount < 10000  ──是──▶ 部门主管审批 ──▶ END
--                        │
--                        └──否──▶ 条件② amount < 100000 ──是──▶ 财务经理审批 ──▶ END
--                                              │
--                                              └──否──▶ 财务总监+总经理会签 ──▶ END
--
-- 阈值、审批人、超时**全在流程定义里**（`biz_flow_node` / `biz_flow_edge.condition_expr`）：
-- 老板要改「5 万以上才走财务经理」，在「流程定义」里另存一个版本即可，**不用改代码、不发版**。
-- 这正是本卡的核心价值。
--
-- ── 状态口径（`biz_payment.approval_status`）────────────
--   NONE      未配流程（降级直付，保持 P7 老行为）或 P17 之前的历史付款行
--   PENDING   审批中（**账面一分没动**：`biz_payable.paid_amount` 不变）
--   APPROVED  审批通过且已执行（应付回写 + 付款凭证，才是真的出钱）
--   REJECTED  驳回 / 撤回（账一分不动，可重新提交）
--   CANCELLED 因单据作废等外部原因终止（账一分不动）
--
-- ── 三条库侧硬约束（不是靠代码自觉）────────────────────
--   1. `ck_biz_payment_approval_status`：状态只许上面五个值（写错一个字就 23514，不当静默脏数据）；
--   2. `ck_biz_payment_pending_instance`：`PENDING ⟹ 必须挂审批实例`
--      —— 否则这笔付款永远等不到回调，钱被永久卡住且没人知道；
--   3. `uk_biz_payment_pending`（部分唯一索引，`approval_status='PENDING'`）：
--      **一个应付单同时只能有一笔在审批中**（硬红线 ⑦ 的库层兜底；并发双击也只会有 1 笔）。
--
-- ── 审批人为什么是 ADMIN + self_approval=ALLOW ─────────────
-- 本仓没有「财务总监 / 总经理 / 部门主管」这些业务角色种子（见 V0157 / V0174 同一取舍），
-- 审批人来源取 `ADMIN`（运行时展开全部超管），`empty_assignee=FALLBACK_ADMIN` 兜底。
-- 会签节点 `approval_mode='ALL'`：**有几个审批人就必须全部同意**才算通过
-- （两个超管 → 两个都批才过；单超管部署 → 一个批即过，不会永久卡死）。
-- 客户要按真实岗位分权，在「流程定义」里把节点的 `assignee_type` 改成 `ROLE` 并选角色即可，
-- 代码一行不用改。
--
-- `self_approval='ALLOW'` **故意不是** FORBID：FORBID 在**发起时**就 422（整单起不来），
-- 而内控要求是「提交成功、但**制单人点不动**这一票」。放行待办 + 在审批入口 403
-- （`payable_bridge::guard_self_approval`，报文：「不能审批自己提交的付款申请」）才既有留痕又不挡发起。
--
-- ── 快照与 hash（不是手写的）─────────────────────────────
-- 图按 `graph.rs::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)）序列化成 canonical JSON
-- （递归按键字典序、紧凑无空白），再 sha256：
--   graph_hash = 8341eac726949864d2a9fdb562a34154d8b7eb611a265fc6adc2c1c9e8e5c1c5
-- 末尾 DO 块自检「条数 / hash / 条件边 / 会签口径 / is_current 唯一」，任一不符 → 迁移失败 → 启动失败。
--
-- ── 幂等与安全 ────────────────────────────────────────
-- · 加列 `IF NOT EXISTS`；约束先 `DROP ... IF EXISTS` 再建；索引 `IF NOT EXISTS`。
-- · 四层 `WHERE NOT EXISTS` 守卫：已有 `flow_code='payable_pay'` 的未删流程，
--   或已有别的 `biz_type='PAYABLE_PAY'` 默认流程（撞 `uk_biz_flow_def_default`），整条跳过，绝不覆盖。
-- · 回滚：`V0175__erp_payable_payment_approval.down.sql`（只删固定 UUID 那一套 + 摘掉列/约束）。
-- ======================================================

-- ---------- 1) 付款记录：审批字段 ----------
ALTER TABLE biz_payment
    ADD COLUMN IF NOT EXISTS approval_instance_id uuid,
    ADD COLUMN IF NOT EXISTS approval_status varchar(24) NOT NULL DEFAULT 'NONE';

COMMENT ON COLUMN biz_payment.approval_instance_id IS
    '付款审批实例 id（biz_flow_instance）；PENDING 时必有值，终态保留作审计';
COMMENT ON COLUMN biz_payment.approval_status IS
    'NONE 未配流程/历史直付 | PENDING 审批中（账未动）| APPROVED 已执行 | REJECTED 驳回/撤回 | CANCELLED 单据作废终止';

-- 状态白名单（写错一个字就 23514，不留静默脏状态）
ALTER TABLE biz_payment DROP CONSTRAINT IF EXISTS ck_biz_payment_approval_status;
ALTER TABLE biz_payment
    ADD CONSTRAINT ck_biz_payment_approval_status
    CHECK (approval_status IN ('NONE', 'PENDING', 'APPROVED', 'REJECTED', 'CANCELLED'));

-- PENDING ⟹ 必须挂审批实例（否则这笔付款永远等不到回调，钱被永久卡住）
ALTER TABLE biz_payment DROP CONSTRAINT IF EXISTS ck_biz_payment_pending_instance;
ALTER TABLE biz_payment
    ADD CONSTRAINT ck_biz_payment_pending_instance
    CHECK (approval_status <> 'PENDING' OR approval_instance_id IS NOT NULL);

-- 一个应付单同时只能有一笔「审批中」的付款（硬红线 ⑦：审批中不能重复付款）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_payment_pending
    ON biz_payment (payable_id)
    WHERE approval_status = 'PENDING' AND deleted_at IS NULL;

-- 按实例反查付款（回调 / 孤儿回收都按它找）
CREATE INDEX IF NOT EXISTS idx_biz_payment_approval
    ON biz_payment (approval_instance_id)
    WHERE approval_instance_id IS NOT NULL;

-- ---------- 2) 应付单：在跑付款审批的弱引用 ----------
ALTER TABLE biz_payable
    ADD COLUMN IF NOT EXISTS payment_approval_instance_id uuid;

COMMENT ON COLUMN biz_payable.payment_approval_instance_id IS
    '该应付单当前在跑的付款审批实例（与 biz_payment.approval_status=PENDING 同生同灭，终态清空）';

CREATE INDEX IF NOT EXISTS idx_biz_payable_payment_approval
    ON biz_payable (payment_approval_instance_id)
    WHERE payment_approval_instance_id IS NOT NULL;

-- ---------- 3) 流程定义（PUBLISHED + 默认） ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-000000000717', 'payable_pay', '应付付款审批', 'PAYABLE_PAY', 'erp',
    '付款分级审批：发起付款 → 条件节点按金额分流（< 1 万部门主管 / 1–10 万财务经理 / '
    '≥ 10 万财务总监+总经理会签）→ 结束。审批通过才真正记账（应付回写 + 借 2202 / 贷 1002 凭证）；'
    '驳回、撤回或单据作废则一分钱不动。阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-000000000718', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'payable_pay' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'PAYABLE_PAY' AND is_default AND deleted_at IS NULL
);

-- ---------- 4) 版本行（PUBLISHED + is_current + 快照/hash） ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-000000000718', '20000000-0000-4000-8000-000000000717', 1, 'PUBLISHED', TRUE,
    '8341eac726949864d2a9fdb562a34154d8b7eb611a265fc6adc2c1c9e8e5c1c5',
    '{"schema_version": 1, "nodes": [{"key": "c_low", "name": "金额分级①（< 1 万）", "node_type": "CONDITION", "assignee_type": null, "approval_mode": null, "approve_ratio": null, "reject_veto": true, "join_mode": "OR", "fork_mode": "AND", "form_var_key": null, "superior_levels": 1, "empty_assignee": "SKIP", "self_approval": "SKIP", "allow_robot": false, "timeout_hours": null, "timeout_action": "REMIND", "remind_before_hours": null, "max_remind_times": 3, "cc_on_approved": false, "x": 200, "y": 0, "sort_order": 2, "config": {}, "assignees": []}, {"key": "c_mid", "name": "金额分级②（< 10 万）", "node_type": "CONDITION", "assignee_type": null, "approval_mode": null, "approve_ratio": null, "reject_veto": true, "join_mode": "OR", "fork_mode": "AND", "form_var_key": null, "superior_levels": 1, "empty_assignee": "SKIP", "self_approval": "SKIP", "allow_robot": false, "timeout_hours": null, "timeout_action": "REMIND", "remind_before_hours": null, "max_remind_times": 3, "cc_on_approved": false, "x": 400, "y": 140, "sort_order": 4, "config": {}, "assignees": []}, {"key": "e", "name": "结束", "node_type": "END", "assignee_type": null, "approval_mode": null, "approve_ratio": null, "reject_veto": true, "join_mode": "OR", "fork_mode": "AND", "form_var_key": null, "superior_levels": 1, "empty_assignee": "SKIP", "self_approval": "SKIP", "allow_robot": false, "timeout_hours": null, "timeout_action": "REMIND", "remind_before_hours": null, "max_remind_times": 3, "cc_on_approved": false, "x": 820, "y": 80, "sort_order": 7, "config": {}, "assignees": []}, {"key": "n_board", "name": "财务总监+总经理会签", "node_type": "COUNTERSIGN", "assignee_type": "ADMIN", "approval_mode": "ALL", "approve_ratio": null, "reject_veto": true, "join_mode": "AND", "fork_mode": "AND", "form_var_key": null, "superior_levels": 1, "empty_assignee": "FALLBACK_ADMIN", "self_approval": "ALLOW", "allow_robot": false, "timeout_hours": 48, "timeout_action": "REMIND", "remind_before_hours": null, "max_remind_times": 2, "cc_on_approved": false, "x": 600, "y": 280, "sort_order": 6, "config": {}, "assignees": []}, {"key": "n_dept", "name": "部门主管审批", "node_type": "APPROVAL", "assignee_type": "ADMIN", "approval_mode": null, "approve_ratio": null, "reject_veto": true, "join_mode": "AND", "fork_mode": "AND", "form_var_key": null, "superior_levels": 1, "empty_assignee": "FALLBACK_ADMIN", "self_approval": "ALLOW", "allow_robot": false, "timeout_hours": 24, "timeout_action": "REMIND", "remind_before_hours": null, "max_remind_times": 2, "cc_on_approved": false, "x": 400, "y": -140, "sort_order": 3, "config": {}, "assignees": []}, {"key": "n_fin", "name": "财务经理审批", "node_type": "APPROVAL", "assignee_type": "ADMIN", "approval_mode": null, "approve_ratio": null, "reject_veto": true, "join_mode": "AND", "fork_mode": "AND", "form_var_key": null, "superior_levels": 1, "empty_assignee": "FALLBACK_ADMIN", "self_approval": "ALLOW", "allow_robot": false, "timeout_hours": 24, "timeout_action": "REMIND", "remind_before_hours": null, "max_remind_times": 2, "cc_on_approved": false, "x": 600, "y": 0, "sort_order": 5, "config": {}, "assignees": []}, {"key": "s", "name": "开始", "node_type": "START", "assignee_type": null, "approval_mode": null, "approve_ratio": null, "reject_veto": true, "join_mode": "AND", "fork_mode": "AND", "form_var_key": null, "superior_levels": 1, "empty_assignee": "SKIP", "self_approval": "SKIP", "allow_robot": false, "timeout_hours": null, "timeout_action": "REMIND", "remind_before_hours": null, "max_remind_times": 3, "cc_on_approved": false, "x": 0, "y": 0, "sort_order": 1, "config": {}, "assignees": []}], "edges": [{"key": "e_low_else", "from": "c_low", "to": "c_mid", "condition": null, "label": "金额 ≥ 10000", "priority": 10, "is_default": true}, {"key": "e_low", "from": "c_low", "to": "n_dept", "condition": {"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10000}, "label": "金额 < 10000", "priority": 0, "is_default": false}, {"key": "e_mid_else", "from": "c_mid", "to": "n_board", "condition": null, "label": "金额 ≥ 100000", "priority": 10, "is_default": true}, {"key": "e_mid", "from": "c_mid", "to": "n_fin", "condition": {"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 100000}, "label": "10000 ≤ 金额 < 100000", "priority": 0, "is_default": false}, {"key": "e_board_end", "from": "n_board", "to": "e", "condition": null, "label": null, "priority": 0, "is_default": false}, {"key": "e_dept_end", "from": "n_dept", "to": "e", "condition": null, "label": null, "priority": 0, "is_default": false}, {"key": "e_fin_end", "from": "n_fin", "to": "e", "condition": null, "label": null, "priority": 0, "is_default": false}, {"key": "e_start", "from": "s", "to": "c_low", "condition": null, "label": null, "priority": 0, "is_default": false}]}'::jsonb,
    7, 8, 'V0175 种子：应付付款审批（金额三级分流）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-000000000717')
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- 5) 节点（7 个：START / 2×CONDITION / 2×APPROVAL / COUNTERSIGN / END） ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-000000070100'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'c_low'::varchar, '金额分级①（< 1 万）'::varchar, 'CONDITION'::varchar, NULL, 1::int, NULL, NULL, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 200::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-000000070101'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'c_mid'::varchar, '金额分级②（< 10 万）'::varchar, 'CONDITION'::varchar, NULL, 1::int, NULL, NULL, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 400::int, 140::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-000000070102'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL, 1::int, NULL, NULL, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 820::int, 80::int, 7::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-000000070103'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'n_board'::varchar, '财务总监+总经理会签'::varchar, 'COUNTERSIGN'::varchar, 'ADMIN'::varchar, 1::int, NULL, 'ALL'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 600::int, 280::int, 6::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-000000070104'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'n_dept'::varchar, '部门主管审批'::varchar, 'APPROVAL'::varchar, 'ADMIN'::varchar, 1::int, NULL, NULL, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 400::int, -140::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-000000070105'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'n_fin'::varchar, '财务经理审批'::varchar, 'APPROVAL'::varchar, 'ADMIN'::varchar, 1::int, NULL, NULL, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 600::int, 0::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-000000070106'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL, 1::int, NULL, NULL, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-000000000718')
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- 6) 连线（8 条：两条条件边 + 两条兜底边 + 三条汇到 END） ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-000000080100'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'e_low_else'::varchar, '20000000-0000-4000-8000-000000070100'::uuid, '20000000-0000-4000-8000-000000070101'::uuid, NULL, '金额 ≥ 10000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-000000080101'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'e_low'::varchar, '20000000-0000-4000-8000-000000070100'::uuid, '20000000-0000-4000-8000-000000070104'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10000}'::jsonb, '金额 < 10000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-000000080102'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'e_mid_else'::varchar, '20000000-0000-4000-8000-000000070101'::uuid, '20000000-0000-4000-8000-000000070103'::uuid, NULL, '金额 ≥ 100000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-000000080103'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'e_mid'::varchar, '20000000-0000-4000-8000-000000070101'::uuid, '20000000-0000-4000-8000-000000070105'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 100000}'::jsonb, '10000 ≤ 金额 < 100000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-000000080104'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'e_board_end'::varchar, '20000000-0000-4000-8000-000000070103'::uuid, '20000000-0000-4000-8000-000000070102'::uuid, NULL, NULL, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-000000080105'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'e_dept_end'::varchar, '20000000-0000-4000-8000-000000070104'::uuid, '20000000-0000-4000-8000-000000070102'::uuid, NULL, NULL, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-000000080106'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'e_fin_end'::varchar, '20000000-0000-4000-8000-000000070105'::uuid, '20000000-0000-4000-8000-000000070102'::uuid, NULL, NULL, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-000000080107'::uuid, '20000000-0000-4000-8000-000000000718'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-000000070106'::uuid, '20000000-0000-4000-8000-000000070100'::uuid, NULL, NULL, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-000000000718')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 注：审批人不在 `biz_flow_node_assignee` 里 —— `ADMIN` 由运行时展开（见文件头）。

-- ---------- 7) 结构自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    m_rows int;
    v_bad  int;
    n_rows int;
    e_rows int;
BEGIN
    -- 表结构（无论流程是否跳过都必须改到位）
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_payment' AND column_name = 'approval_status'
    ) OR NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_payment' AND column_name = 'approval_instance_id'
    ) THEN
        RAISE EXCEPTION 'V0175 自检失败：biz_payment 缺 approval_status / approval_instance_id';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_payable' AND column_name = 'payment_approval_instance_id'
    ) THEN
        RAISE EXCEPTION 'V0175 自检失败：biz_payable 缺 payment_approval_instance_id';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_payment_approval_status'
           AND conrelid = 'biz_payment'::regclass
    ) THEN
        RAISE EXCEPTION 'V0175 自检失败：ck_biz_payment_approval_status 未建立';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE tablename = 'biz_payment' AND indexname = 'uk_biz_payment_pending'
    ) THEN
        RAISE EXCEPTION 'V0175 自检失败：uk_biz_payment_pending（同一应付单只许一笔审批中）未建立';
    END IF;

    -- 客户已有同名流程 / 已有别的默认流程（守卫整条跳过）→ 只核对表结构
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-000000000717') THEN
        IF EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'payable_pay' AND deleted_at IS NULL) THEN
            RAISE NOTICE 'V0175：已存在 flow_code=payable_pay 的流程，种子跳过（未插入）';
        ELSE
            RAISE NOTICE 'V0175：已存在 biz_type=PAYABLE_PAY 的其它默认流程，种子跳过（未插入，避免撞 uk_biz_flow_def_default）';
        END IF;
        RETURN;
    END IF;

    SELECT count(*) INTO m_rows FROM biz_flow_def d
     WHERE d.id = '20000000-0000-4000-8000-000000000717' AND d.flow_code = 'payable_pay'
       AND d.biz_type = 'PAYABLE_PAY' AND d.status = 'PUBLISHED'
       AND d.is_default AND d.current_version_id = '20000000-0000-4000-8000-000000000718'
       AND d.deleted_at IS NULL;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0175 自检失败：种子定义行不满足 PUBLISHED/默认/当前版本 四条件';
    END IF;

    SELECT count(*) INTO v_bad FROM biz_flow_def_version v
     WHERE v.id = '20000000-0000-4000-8000-000000000718' AND v.def_id = '20000000-0000-4000-8000-000000000717'
       AND v.version_status = 'PUBLISHED' AND v.is_current
       AND v.graph_hash = '8341eac726949864d2a9fdb562a34154d8b7eb611a265fc6adc2c1c9e8e5c1c5'
       AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
       AND v.node_count = 7 AND v.edge_count = 8;
    IF v_bad <> 1 THEN
        RAISE EXCEPTION 'V0175 自检失败：版本行缺少 PUBLISHED 三件套（快照/hash/发布时间）或计数不对';
    END IF;

    SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-000000000718';
    SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-000000000718';
    IF n_rows <> 7 OR e_rows <> 8 THEN
        RAISE EXCEPTION 'V0175 自检失败：节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
    END IF;

    -- 金额分级必须**真的落在条件边上**（不是代码里 if/else）：两条条件边、两条兜底边
    SELECT count(*) INTO m_rows FROM biz_flow_edge
     WHERE version_id = '20000000-0000-4000-8000-000000000718'
       AND NOT is_default AND condition_expr IS NOT NULL
       AND condition_expr->>'field' = 'form.amount';
    IF m_rows <> 2 THEN
        RAISE EXCEPTION 'V0175 自检失败：按 form.amount 分流的条件边应为 2 条，实际 %', m_rows;
    END IF;
    SELECT count(*) INTO m_rows FROM biz_flow_edge
     WHERE version_id = '20000000-0000-4000-8000-000000000718' AND is_default AND condition_expr IS NULL;
    IF m_rows <> 2 THEN
        RAISE EXCEPTION 'V0175 自检失败：条件节点的兜底边应为 2 条（每条 CONDITION 恰一条），实际 %', m_rows;
    END IF;

    -- 会签节点：ALL 模式（两个都批才通过）+ 有人可派 + 自审放行（403 由审批入口兜）
    SELECT count(*) INTO m_rows FROM biz_flow_node n
     WHERE n.version_id = '20000000-0000-4000-8000-000000000718' AND n.node_key = 'n_board'
       AND n.node_type = 'COUNTERSIGN' AND n.approval_mode = 'ALL'
       AND n.assignee_type = 'ADMIN' AND n.empty_assignee = 'FALLBACK_ADMIN'
       AND n.self_approval = 'ALLOW';
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0175 自检失败：会签节点的模式/审批人来源/自审策略不符合口径（须 COUNTERSIGN + ALL + ADMIN + FALLBACK_ADMIN + ALLOW）';
    END IF;

    -- END 有三条入边 ⇒ 必须 OR 汇聚（AND 会等人齐 → 审批单永久卡死）
    SELECT count(*) INTO m_rows FROM biz_flow_node
     WHERE version_id = '20000000-0000-4000-8000-000000000718' AND node_key = 'e' AND join_mode = 'OR';
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0175 自检失败：END 节点必须 OR 汇聚（三条分支不会都到）';
    END IF;

    SELECT count(*) INTO m_rows FROM biz_flow_def_version
     WHERE def_id = '20000000-0000-4000-8000-000000000717' AND is_current;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0175 自检失败：is_current 版本数为 %（应恰为 1）', m_rows;
    END IF;
END $$;
