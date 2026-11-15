-- ======================================================
-- V0243：费用报销审批流种子 `expense`（ARCH-6 §4.4，金额三档）
--
-- 图（7 节点 / 8 边）：
--   START → 直属上级审批 → 金额分级①（< 1000 元）
--                            ├─ 金额 < 1000        → END
--                            └─ 金额 ≥ 1000 → 金额分级②（< 10000 元）
--                                              ├─ 金额 < 10000 → 财务经理审批 → END
--                                              └─ 金额 ≥ 10000 → 财务经理+总经理会签 → END
--
-- ── 为什么第一档是「直属上级」而不是「部门主管角色」────────────
-- 报销是**员工个人**的支出，最天然的审批人是「我的直接上级」（组织架构里唯一确定
-- 的关系）；`dept_id` 又是预算维度 —— 审批人与预算责任人必须是同一条链上的人，
-- 否则超支没人真正负责。这与 `po_approve`（用 DEPT_LEADER 角色）是一处**有意不同**。
--
-- ── 分级口径 ─────────────────────────────────────────────
-- 取 **含税金额**（`form.amount` = 报销单 `total_with_tax`），与预算占用、与
-- `po_approve` 的分档口径一致。边界：1000 与 10000 都归**高档**
-- （`< 1000` 免财务 / `< 10000` 财务单签 / `≥ 10000` 会签），宁可多一道不肯少一道。
--
-- ── 审批人与内控 ─────────────────────────────────────────
-- * 直属上级：`INITIATOR_SUPERIOR`（`superior_levels = 1`，沿 dept_path 上溯取 leader_id），
--   解析不到 → `FALLBACK_ADMIN`（**不用 SKIP**：SKIP 会把「没人可派」变成自动通过）；
-- * 财务经理：`ROLE = FINANCE_MANAGER`（V0191 已种，本迁移不重复种角色）；
-- * 会签：`ROLE = FINANCE_MANAGER` + `ROLE = GENERAL_MANAGER`（`approval_mode = ALL`）；
-- * `self_approval = ALLOW`：真正的否决在 `expense_bridge::guard_self_approval`
--   （403 + 65019）—— 发起时就 422 会让整单起不来；
-- * 驳回 / 撤回 → 报销单回 `DRAFT`（可改后重提），预算占用 RELEASED。
--
-- ── 快照与 hash（不是手写的）─────────────────────────────
-- 图按 `flow::graph::snapshot()` 的确定性排序序列化成 canonical JSON
-- （递归按键字典序、紧凑无空白），再 sha256：
--   expense : 196042b525bb64d95741902d415270652ac9afb1f6ecfcbc683540f6ec6d345b
-- 末尾 DO 块自检「条数 / hash / 角色存在 / is_current 唯一 / 图必须有开始结束」，
-- 任一不符 → 迁移失败。
--
-- ── 幂等与回滚 ───────────────────────────────────────────
-- 四层 `WHERE NOT EXISTS`（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符 → 整条跳过）；
-- down 见同名文件：**有审批实例跑在这个版本上就拒绝回滚**。
-- ======================================================

-- ---------- 1) 流程定义 ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id,
    is_system)
SELECT
    '20000000-0000-4000-8000-0000000d0240', 'expense', '费用报销审批（金额三档）', 'EXPENSE', '财务管理',
    '费用报销提交审批：直属上级必审，再按含税金额分级 —— < 1000 元只走上级；'
    '1000 ~ 10000 元加财务经理；≥ 10000 元财务经理 + 总经理会签。'
    '审批通过即入账（借费用科目 / 借进项税 / 贷 2241.01 员工垫付），'
    '驳回或撤回回草稿并把意见写进备注。阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000d0250', 1, TRUE, FALSE, NULL,
    -- `is_system = TRUE`：预置流程（客户在模板列表里能看到、不能删；漏写会吃 DEFAULT FALSE，
    -- P2-6b 的 H6 不变量会判红 —— V0233 就是为这个缺陷打的补丁，新种子别再犯）
    TRUE
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'expense' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE biz_type = 'EXPENSE' AND is_default AND deleted_at IS NULL
)
AND EXISTS (
    SELECT 1 FROM sys_role WHERE id = '20000000-0000-4000-8000-0000000f0005'::uuid AND deleted_at IS NULL
)
AND EXISTS (
    SELECT 1 FROM sys_role WHERE id = '20000000-0000-4000-8000-0000000f0003'::uuid AND deleted_at IS NULL
);

-- ---------- 2) 版本 + 图快照 ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000d0250', '20000000-0000-4000-8000-0000000d0240', 1, 'PUBLISHED', TRUE,
    '196042b525bb64d95741902d415270652ac9afb1f6ecfcbc683540f6ec6d345b',
    $snapshot${"edges":[{"condition":null,"from":"c_low","is_default":true,"key":"e_low_else","label":"金额 ≥ 1000","priority":10,"to":"c_mid"},{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":1000},"from":"c_low","is_default":false,"key":"e_low_end","label":"金额 < 1000","priority":0,"to":"e"},{"condition":null,"from":"c_mid","is_default":true,"key":"e_mid_else","label":"金额 ≥ 10000","priority":10,"to":"n_board"},{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":10000},"from":"c_mid","is_default":false,"key":"e_mid_fin","label":"1000 ≤ 金额 < 10000","priority":0,"to":"n_fin"},{"condition":null,"from":"n_board","is_default":false,"key":"e_board_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_sup","is_default":false,"key":"e_sup_cond","label":null,"priority":0,"to":"c_low"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_sup"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_low","max_remind_times":3,"name":"金额分级①（< 1000 元）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":440,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_mid","max_remind_times":3,"name":"金额分级②（< 10000 元）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":660,"y":150},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":7,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":1120,"y":0},{"allow_robot":false,"approval_mode":"ALL","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":1},{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_board","max_remind_times":2,"name":"财务经理 + 总经理会签","node_type":"COUNTERSIGN","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":880,"y":300},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"财务经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":880,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"INITIATOR_SUPERIOR","assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_sup","max_remind_times":2,"name":"直属上级审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$,
    7, 8, 'V0243 种子：费用报销审批（直属上级 + 金额三档分流）默认流程',
    now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0240')
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- 3) 节点 ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0261'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0262'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'n_sup'::varchar, '直属上级审批'::varchar, 'APPROVAL'::varchar, 'INITIATOR_SUPERIOR'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0263'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'c_low'::varchar, '金额分级①（< 1000 元）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 440::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0264'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'c_mid'::varchar, '金额分级②（< 10000 元）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 660::int, 150::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0265'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'n_fin'::varchar, '财务经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 880::int, 0::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0266'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'n_board'::varchar, '财务经理 + 总经理会签'::varchar, 'COUNTERSIGN'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ALL'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 880::int, 300::int, 6::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0267'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 1120::int, 0::int, 7::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0250')
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- 4) 边 ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0271'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d0261'::uuid, '20000000-0000-4000-8000-0000000d0262'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0272'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'e_sup_cond'::varchar, '20000000-0000-4000-8000-0000000d0262'::uuid, '20000000-0000-4000-8000-0000000d0263'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0273'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'e_low_end'::varchar, '20000000-0000-4000-8000-0000000d0263'::uuid, '20000000-0000-4000-8000-0000000d0267'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 1000}'::jsonb, '金额 < 1000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0274'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'e_low_else'::varchar, '20000000-0000-4000-8000-0000000d0263'::uuid, '20000000-0000-4000-8000-0000000d0264'::uuid, NULL::jsonb, '金额 ≥ 1000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000d0275'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'e_mid_fin'::varchar, '20000000-0000-4000-8000-0000000d0264'::uuid, '20000000-0000-4000-8000-0000000d0265'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10000}'::jsonb, '1000 ≤ 金额 < 10000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0276'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'e_mid_else'::varchar, '20000000-0000-4000-8000-0000000d0264'::uuid, '20000000-0000-4000-8000-0000000d0266'::uuid, NULL::jsonb, '金额 ≥ 10000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000d0277'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'e_fin_end'::varchar, '20000000-0000-4000-8000-0000000d0265'::uuid, '20000000-0000-4000-8000-0000000d0267'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0278'::uuid, '20000000-0000-4000-8000-0000000d0250'::uuid, 'e_board_end'::varchar, '20000000-0000-4000-8000-0000000d0266'::uuid, '20000000-0000-4000-8000-0000000d0267'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0250')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- ---------- 5) 审批人（只有 ROLE 类落行；INITIATOR_SUPERIOR 运行时算） ----------
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0265'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d0266'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d0266'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 1::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0250')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- 6) 自检 ----------
DO $$
DECLARE
    v_def   int;
    v_nodes int;
    v_edges int;
    v_hash  text;
    v_bad   int;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0240') THEN
        RAISE NOTICE 'V0243：expense 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
        RETURN;
    END IF;

    SELECT count(*) INTO v_def FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-0000000d0240' AND flow_code = 'expense' AND biz_type = 'EXPENSE'
       AND status = 'PUBLISHED' AND is_default AND current_version_id = '20000000-0000-4000-8000-0000000d0250'
       AND deleted_at IS NULL;
    IF v_def <> 1 THEN
        RAISE EXCEPTION 'V0243 自检失败：费用报销审批定义行不满足 PUBLISHED/默认/当前版本';
    END IF;

    SELECT count(*) INTO v_nodes FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0250';
    IF v_nodes <> 7 THEN
        RAISE EXCEPTION 'V0243 自检失败：节点数 % （期望 7）', v_nodes;
    END IF;

    SELECT count(*) INTO v_edges FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0250';
    IF v_edges <> 8 THEN
        RAISE EXCEPTION 'V0243 自检失败：边数 % （期望 8）', v_edges;
    END IF;

    SELECT graph_hash INTO v_hash FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0250';
    IF v_hash <> '196042b525bb64d95741902d415270652ac9afb1f6ecfcbc683540f6ec6d345b' THEN
        RAISE EXCEPTION 'V0243 自检失败：graph_hash 与声明不一致';
    END IF;

    SELECT count(*) INTO v_bad FROM biz_flow_def_version
     WHERE def_id = '20000000-0000-4000-8000-0000000d0240' AND is_current;
    IF v_bad <> 1 THEN
        RAISE EXCEPTION 'V0243 自检失败：current 版本行有 % 个（期望 1）', v_bad;
    END IF;

    -- 图必须恰好一个 START + 一个 END（引擎启动的前置）
    SELECT count(*) INTO v_bad FROM biz_flow_node
     WHERE version_id = '20000000-0000-4000-8000-0000000d0250' AND node_type = 'START';
    IF v_bad <> 1 THEN
        RAISE EXCEPTION 'V0243 自检失败：START 节点 % 个（期望 1）', v_bad;
    END IF;
    SELECT count(*) INTO v_bad FROM biz_flow_node
     WHERE version_id = '20000000-0000-4000-8000-0000000d0250' AND node_type = 'END';
    IF v_bad <> 1 THEN
        RAISE EXCEPTION 'V0243 自检失败：END 节点 % 个（期望 1）', v_bad;
    END IF;

    -- 金额分级的两条「条件边」必须在（否则三档退化成同一档，且没人会发现）
    SELECT count(*) INTO v_bad FROM biz_flow_edge
     WHERE version_id = '20000000-0000-4000-8000-0000000d0250' AND condition_expr IS NOT NULL;
    IF v_bad <> 2 THEN
        RAISE EXCEPTION 'V0243 自检失败：条件边 % 条（期望 2：< 1000 / < 10000）', v_bad;
    END IF;

    -- 审批人必须存在（角色被客户换了 UUID → 上面就会跳过，这里是二次兜底）
    SELECT count(*) INTO v_bad
      FROM biz_flow_node_assignee a
     WHERE a.node_id IN (SELECT id FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0250')
       AND a.assignee_type = 'ROLE'
       AND NOT EXISTS (SELECT 1 FROM sys_role r WHERE r.id = a.ref_id AND r.deleted_at IS NULL);
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0243 自检失败：% 条审批人指向不存在的角色', v_bad;
    END IF;
END $$;
