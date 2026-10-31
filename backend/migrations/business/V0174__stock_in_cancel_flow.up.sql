-- ======================================================
-- V0174：收货单作废审批（批次 2 · P16）
--        ① `biz_stock_in` 加 `approval_instance_id`（作废审批实例弱引用）
--        ② `ck_biz_si_status` 放开到 4 态（新增 PENDING_CANCEL ＝「作废审批中」）
--        ③ 种子流程 `stock_in_cancel`（START → 作废复核 → END，PUBLISHED + 默认）
--
-- ── 为什么要这条迁移 ────────────────────────────────────
-- P9 把「作废」做成了**直接回滚六本账**（库存 / 库位账 / 流水 / 凭证 / 应付 / 序列号），
-- 但作废入口只校验「有没有填 reason」——**没有任何复核**。一次误点就是六本账同时反向，
-- 这是整个 ERP 里最危险的反向操作。本迁移给收货单接上与调拨单（P5 · V0157）同构的审批前置：
--
--   POSTED ──作废──▶ PENDING_CANCEL（只发起审批，不动账）
--   PENDING_CANCEL ──审批通过──▶ CANCELLED（此时才执行 P9 的六本账回滚）
--   PENDING_CANCEL ──驳回/撤回──▶ POSTED（账一点没动）
--
-- 所以 `biz_stock_in.status` 必须允许 `PENDING_CANCEL`：V0008 的 `ck_biz_si_status`
-- 只登记了 DRAFT / POSTED / CANCELLED，不放开的话新状态第一笔写入就撞 23514（500）。
--
-- ── 审批人为什么是 ADMIN + self_approval=FORBID ──────────
-- 卡面 §三-⑧「权限分离（新增要求）」：**制单人不能批自己的作废**（内控）。
-- 仓管主管在本仓没有对应 `sys_role`（种子角色只有 SUPER_ADMIN），用 `ROLE` + role_code 会在
-- 「角色里 0 人」时把节点跳过（审批自动通过，比卡住更坏），所以审批人来源取 `ADMIN`
-- （运行时展开全部超管），`empty_assignee=FALLBACK_ADMIN` 兜底。
-- `self_approval=FORBID` 与 V0157 的调拨流**故意不同**：调拨是 OWNER 业务（自审可接受），
-- 作废是**反向记账**（自审自批等于没有复核）。发起人本人被引擎硬拦（422 + 65019），
-- 报文会明确提示「加一个复核人账号」。管理员若确实要放宽，在「流程定义」里另存新版本
-- 把 self_approval 改成 ALLOW 即可 —— 改老单不受影响（发布后不可改）。
--
-- ── 数据来源（不是手写的）─────────────────────────────
-- 图与快照由 V0157 的**真实发布快照**派生：只改节点 key/name（n_approve → n_review、
-- 「超管审批」→「作废复核」）、`self_approval`（ALLOW → FORBID）、`timeout_hours`（48 → 24），
-- 图结构（3 节点 2 条边 START → APPROVAL → END）一字未变。因此快照与 hash 自洽：
--   graph_hash = sha256(canonical_json(graph_snapshot)) = 78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c
-- 末尾 DO 块自检「快照条数 / hash / 审批人策略 / is_current 唯一」，任一不符 → 迁移失败 → 启动失败
-- （宁可起不来，也不要种出一张会被引擎跳过的图）。
--
-- ── 幂等与安全 ────────────────────────────────────────
-- · 加列 `IF NOT EXISTS`；约束先 `DROP ... IF EXISTS` 再建（可重复执行）。
-- · 四层 `WHERE NOT EXISTS` 守卫：已存在 `flow_code='stock_in_cancel'` 的未删流程，
--   或已有别的 `biz_type='STOCK_IN_CANCEL'` 默认流程（撞 `uk_biz_flow_def_default`），整条跳过，绝不覆盖。
-- · 回滚：`V0174__stock_in_cancel_flow.down.sql`（只删固定 UUID 那一套 + 摘掉状态值）。
-- ======================================================

-- ---------- 1) 单据表：审批实例弱引用 ----------
ALTER TABLE biz_stock_in
    ADD COLUMN IF NOT EXISTS approval_instance_id uuid;

COMMENT ON COLUMN biz_stock_in.approval_instance_id IS
    '作废审批实例 id；为空 = 未发起或未配流程（与 biz_stock_in.status = PENDING_CANCEL 同时出现）';

-- 便于「审批中的单子」按实例反查（作废回调 / 孤儿实例回收都按它找）
CREATE INDEX IF NOT EXISTS idx_biz_si_approval
    ON biz_stock_in (approval_instance_id)
    WHERE approval_instance_id IS NOT NULL;

-- ---------- 2) 状态约束：放开 PENDING_CANCEL ----------
-- 原约束（V0008）：status IN ('DRAFT','POSTED','CANCELLED')
-- 新约束：多一个 'PENDING_CANCEL'（作废审批中；此时单据锁定，不许改数量/金额/明细）
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_status;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_status
    CHECK (status IN ('DRAFT', 'POSTED', 'PENDING_CANCEL', 'CANCELLED'));

-- 状态与审批实例必须同生同灭：PENDING_CANCEL ⟺ 有实例；其它状态 ⟹ 没有实例。
-- 这条 CAS 之外的硬约束把「审批中却没挂实例（永远等不到回调）」和
-- 「已终态却还挂着实例（下次发起撞 uk_biz_flow_instance_biz_active）」都挡在库层。
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_pending_cancel;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_pending_cancel
    CHECK (
        (status = 'PENDING_CANCEL' AND approval_instance_id IS NOT NULL)
        OR (status <> 'PENDING_CANCEL' AND approval_instance_id IS NULL)
    );

-- ---------- 3) 流程定义（PUBLISHED + 默认） ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-00000000d174', 'stock_in_cancel', '收货单作废审批', 'STOCK_IN_CANCEL', 'erp',
    '收货入库单作废的复核流程：发起作废 → 作废复核（超管，禁止自审） → 结束。'
    '审批通过才执行六本账（库存/库位账/流水/凭证/应付/序列号）回滚；驳回或撤回则单据回到已过账，账不动。',
    'PUBLISHED', '20000000-0000-4000-8000-00000000e174', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'stock_in_cancel' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'STOCK_IN_CANCEL' AND is_default AND deleted_at IS NULL
);

-- ---------- 4) 版本行（PUBLISHED + is_current + 快照/hash） ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-00000000e174', '20000000-0000-4000-8000-00000000d174', 1, 'PUBLISHED', TRUE,
    '78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c',
    '{"edges": [{"condition": null, "from": "n_review", "is_default": false, "key": "e2", "label": null, "priority": 0, "to": "e"}, {"condition": null, "from": "s", "is_default": false, "key": "e1", "label": null, "priority": 0, "to": "n_review"}], "nodes": [{"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "e", "max_remind_times": 3, "name": "结束", "node_type": "END", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 3, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 480, "y": 0}, {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": "ADMIN", "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "FALLBACK_ADMIN", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "n_review", "max_remind_times": 2, "name": "作废复核", "node_type": "APPROVAL", "reject_veto": true, "remind_before_hours": null, "self_approval": "FORBID", "sort_order": 2, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": 24, "x": 240, "y": 0}, {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "s", "max_remind_times": 3, "name": "开始", "node_type": "START", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 1, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 0, "y": 0}], "schema_version": 1}'::jsonb,
    3, 2, 'V0174 种子：收货单作废审批默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-00000000d174')
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- 5) 节点（3 个：START / APPROVAL / END） ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-00000000a174'::uuid, '20000000-0000-4000-8000-00000000e174'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-00000000a175'::uuid, '20000000-0000-4000-8000-00000000e174'::uuid, 'n_review'::varchar, '作废复核'::varchar, 'APPROVAL'::varchar, 'ADMIN'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'FORBID'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 240::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-00000000a176'::uuid, '20000000-0000-4000-8000-00000000e174'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 480::int, 0::int, 3::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-00000000e174')
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- 6) 连线（2 条：s → n_review → e） ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-00000000b174'::uuid, '20000000-0000-4000-8000-00000000e174'::uuid, 'e1'::varchar, '20000000-0000-4000-8000-00000000a174'::uuid, '20000000-0000-4000-8000-00000000a175'::uuid, 'null'::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-00000000b175'::uuid, '20000000-0000-4000-8000-00000000e174'::uuid, 'e2'::varchar, '20000000-0000-4000-8000-00000000a175'::uuid, '20000000-0000-4000-8000-00000000a176'::uuid, 'null'::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-00000000e174')
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
    -- 客户已有同名流程 / 已有别的默认流程（守卫整条跳过）→ 只需确认表结构改到位
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-00000000d174') THEN
        IF EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'stock_in_cancel' AND deleted_at IS NULL) THEN
            RAISE NOTICE 'V0174：已存在 flow_code=stock_in_cancel 的流程，种子跳过（未插入）';
        ELSE
            RAISE NOTICE 'V0174：已存在 biz_type=STOCK_IN_CANCEL 的其它默认流程，种子跳过（未插入，避免撞 uk_biz_flow_def_default）';
        END IF;
        RETURN;
    END IF;

    SELECT count(*) INTO m_rows FROM biz_flow_def d
     WHERE d.id = '20000000-0000-4000-8000-00000000d174' AND d.flow_code = 'stock_in_cancel'
       AND d.biz_type = 'STOCK_IN_CANCEL' AND d.status = 'PUBLISHED'
       AND d.is_default AND d.current_version_id = '20000000-0000-4000-8000-00000000e174'
       AND d.deleted_at IS NULL;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0174 自检失败：种子定义行不满足 PUBLISHED/默认/当前版本 四条件';
    END IF;

    SELECT count(*) INTO v_bad FROM biz_flow_def_version v
     WHERE v.id = '20000000-0000-4000-8000-00000000e174' AND v.def_id = '20000000-0000-4000-8000-00000000d174'
       AND v.version_status = 'PUBLISHED' AND v.is_current
       AND v.graph_hash = '78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c'
       AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
       AND v.node_count = 3 AND v.edge_count = 2;
    IF v_bad <> 1 THEN
        RAISE EXCEPTION 'V0174 自检失败：版本行缺少 PUBLISHED 三件套（快照/hash/发布时间）或计数不对';
    END IF;

    SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-00000000e174';
    SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-00000000e174';
    IF n_rows <> 3 OR e_rows <> 2 THEN
        RAISE EXCEPTION 'V0174 自检失败：节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
    END IF;

    -- 审批节点必须有人可派，且**不许自审**（内控要求：审批人 ≠ 发起人）
    SELECT count(*) INTO m_rows FROM biz_flow_node n
     WHERE n.version_id = '20000000-0000-4000-8000-00000000e174' AND n.node_key = 'n_review'
       AND n.node_type = 'APPROVAL' AND n.assignee_type = 'ADMIN'
       AND n.empty_assignee = 'FALLBACK_ADMIN' AND n.self_approval = 'FORBID';
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0174 自检失败：作废复核节点的审批人来源/空审批人/自审策略不符合内控口径（须 ADMIN + FALLBACK_ADMIN + FORBID）';
    END IF;

    SELECT count(*) INTO m_rows FROM biz_flow_def_version
     WHERE def_id = '20000000-0000-4000-8000-00000000d174' AND is_current;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0174 自检失败：is_current 版本数为 %（应恰为 1）', m_rows;
    END IF;

    -- 约束必须已放开到 4 态（否则新状态第一笔写入就 23514）
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_si_status'
           AND conrelid = 'biz_stock_in'::regclass
           AND pg_get_constraintdef(oid) LIKE '%PENDING_CANCEL%'
    ) THEN
        RAISE EXCEPTION 'V0174 自检失败：ck_biz_si_status 没有放开 PENDING_CANCEL';
    END IF;
END $$;
