-- ======================================================
-- V0127：种子流程 `task_accept`（任务验收默认流程）
--        START(s) → 创建人验收(n_creator) → END(e)
--
-- ── 为什么需要这条种子 ─────────────────────────────────
-- 批次 D 的任务验收接线（`flow/task_bridge.rs::prepare_accept`）只在
-- 「`biz_flow_def` 里存在 `biz_type='TASK_ACCEPT'` 的**已发布默认流程**」时才自动发起审批单；
-- 没有流程就静默走原两级验收。空库上线后管理员什么都不配 ⇒ 验收审批永远不生效。
-- 这条迁移把默认流程直接种成 **PUBLISHED + is_default**，做到「全新库一键可用」。
--
-- ── 数据来源（不是手写的）─────────────────────────────
-- 图与快照来自一次**真实发布**：空库实例上走
-- `POST /business/flow/def` → `PUT /business/flow/version/<vid>/graph` → `POST /def/<id>/publish`，
-- 然后把 `biz_flow_def / _version / biz_flow_node / _edge / _node_assignee` 五张表按行导出。
-- 因此 `graph_snapshot`（引擎唯一的图来源）与 `graph_hash` 必然自洽：
--   graph_hash = sha256(canonical_json(graph_snapshot)) = f5e20abd1098ed8b6fa84d713d5c5456db4e5743cbd4cdb7605cd13d3761c05f
--
-- ── 审批人为什么是 TASK_CREATOR ────────────────────────
-- 「任务验收」= 交活的人提，派活的人（任务创建人）验。TASK_CREATOR 由运行时按
-- `biz_task.creator_id` 解析（`flow/assignee.rs`，仅 `biz_type=TASK_ACCEPT` 支持），
-- 落 `biz_flow_node_assignee` 行没有意义，故本迁移**没有**审批人行。
-- `self_approval=ALLOW`：自己给自己建的任务也能一键自验，避免种子流程把人卡死；
-- `empty_assignee=SKIP`：创建人账号被删时跳过该节点而不是报错。
--
-- ── 幂等与安全 ────────────────────────────────────────
-- 四层 `WHERE NOT EXISTS` 守卫：已存在 `flow_code='task_accept'` 的未删流程（客户自己建过）
-- 就整条跳过，绝不覆盖；每一步都以上一步落地为前提，不会留半张图。
-- 末尾 DO 块自检：节点/边数量、`is_current` 唯一性、PUBLISHED 三件套（hash/快照/时间）。
-- 回滚：`V0127__flow_seed_task_accept.down.sql`（只删固定 UUID 的那一套，零残留断言）。
-- ======================================================

-- ---------- 1) 流程定义（PUBLISHED + 默认） ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-00000000d127', 'task_accept', '任务验收', 'TASK_ACCEPT', 'task',
    '任务提交验收时的默认审批流程：提交验收（发起）→ 创建人验收 → 结束。'
    '管理员可在「流程定义」里另存新版本调整（改老单不受影响）。',
    'PUBLISHED', '20000000-0000-4000-8000-00000000e127', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'task_accept' AND deleted_at IS NULL
);

-- ---------- 2) 版本行（PUBLISHED + is_current + 快照/hash） ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-00000000e127', '20000000-0000-4000-8000-00000000d127', 1, 'PUBLISHED', TRUE,
    'f5e20abd1098ed8b6fa84d713d5c5456db4e5743cbd4cdb7605cd13d3761c05f',
    '{"edges": [{"condition": null, "from": "n_creator", "is_default": false, "key": "e2", "label": null, "priority": 0, "to": "e"}, {"condition": null, "from": "s", "is_default": false, "key": "e1", "label": null, "priority": 0, "to": "n_creator"}], "nodes": [{"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "e", "max_remind_times": 3, "name": "结束", "node_type": "END", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 0, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 480, "y": 0}, {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": "TASK_CREATOR", "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "n_creator", "max_remind_times": 2, "name": "创建人验收", "node_type": "APPROVAL", "reject_veto": true, "remind_before_hours": null, "self_approval": "ALLOW", "sort_order": 0, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": 48, "x": 240, "y": 0}, {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "s", "max_remind_times": 3, "name": "开始", "node_type": "START", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 0, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 0, "y": 0}], "schema_version": 1}'::jsonb,
    3, 2, 'V0127 种子：任务验收默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-00000000d127')
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- 3) 节点（3 个：START / APPROVAL / END） ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-00000000a111'::uuid, '20000000-0000-4000-8000-00000000e127'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 0::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-00000000a112'::uuid, '20000000-0000-4000-8000-00000000e127'::uuid, 'n_creator'::varchar, '创建人验收'::varchar, 'APPROVAL'::varchar, 'TASK_CREATOR'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 240::int, 0::int, 0::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-00000000a113'::uuid, '20000000-0000-4000-8000-00000000e127'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 480::int, 0::int, 0::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-00000000e127')
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- 4) 连线（2 条） ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-00000000b121'::uuid, '20000000-0000-4000-8000-00000000e127'::uuid, 'e1'::varchar, '20000000-0000-4000-8000-00000000a111'::uuid, '20000000-0000-4000-8000-00000000a112'::uuid, 'null'::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-00000000b122'::uuid, '20000000-0000-4000-8000-00000000e127'::uuid, 'e2'::varchar, '20000000-0000-4000-8000-00000000a112'::uuid, '20000000-0000-4000-8000-00000000a113'::uuid, 'null'::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-00000000e127')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 注：审批人不在种子行里（TASK_CREATOR 由运行时按任务创建人解析，见文件头）。

-- ---------- 5) 结构自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    m_rows int;
    v_bad  int;
    n_rows int;
    e_rows int;
BEGIN
    -- 若客户已有同名流程（守卫整条跳过），本套固定 UUID 不存在，跳过自检
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-00000000d127') THEN
        RAISE NOTICE 'V0127：已存在 flow_code=task_accept 的流程，种子跳过（未插入）';
        RETURN;
    END IF;

    SELECT count(*) INTO m_rows FROM biz_flow_def d
     WHERE d.id = '20000000-0000-4000-8000-00000000d127' AND d.flow_code = 'task_accept'
       AND d.biz_type = 'TASK_ACCEPT' AND d.status = 'PUBLISHED'
       AND d.is_default AND d.current_version_id = '20000000-0000-4000-8000-00000000e127'
       AND d.deleted_at IS NULL;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0127 自检失败：种子定义行不满足 PUBLISHED/默认/当前版本 四条件';
    END IF;

    SELECT count(*) INTO v_bad FROM biz_flow_def_version v
     WHERE v.id = '20000000-0000-4000-8000-00000000e127' AND v.def_id = '20000000-0000-4000-8000-00000000d127'
       AND v.version_status = 'PUBLISHED' AND v.is_current
       AND v.graph_hash = 'f5e20abd1098ed8b6fa84d713d5c5456db4e5743cbd4cdb7605cd13d3761c05f'
       AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL;
    IF v_bad <> 1 THEN
        RAISE EXCEPTION 'V0127 自检失败：版本行缺少 PUBLISHED 三件套（快照/hash/发布时间）';
    END IF;

    SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-00000000e127';
    SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-00000000e127';
    IF n_rows <> 3 OR e_rows <> 2 THEN
        RAISE EXCEPTION 'V0127 自检失败：节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
    END IF;

    -- 每个 def 至多一个 current 版本（部分唯一索引的语义复核）
    SELECT count(*) INTO m_rows FROM biz_flow_def_version
     WHERE def_id = '20000000-0000-4000-8000-00000000d127' AND is_current;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0127 自检失败：is_current 版本数为 %（应恰为 1）', m_rows;
    END IF;
END $$;
