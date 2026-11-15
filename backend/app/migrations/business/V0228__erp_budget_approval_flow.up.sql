-- ======================================================
-- V0228：预算审批流种子 `budget_approve`（ARCH-6 §3.4.2 / §3.4.3 · FIN-1）
--
-- 一个流程接两种单据，靠 `form.change_type` 条件分流（设计 §3.4.3 的原话：
-- 「流程定义是运维资产，客户升级后要维护两套图；两者差别只是调整要多一道更严的签」）：
--
-- ```text
-- START → 编制/调整分流 ─┬─ ORIGINAL → 部门主管审批 → 金额分级 ─┬─ < 50 万 → 财务经理
--                        │                                     └─ ≥ 50 万 → 财务经理+总经理会签
--                        └─ 兜底（ADJUST 及非 ORIGINAL）→ 财务复核（调整前置）→ 金额分级
--                                                     ─┬─ < 50 万 → 总经理
--                                                      └─ ≥ 50 万 → 总经理+财务总监会签
-- 全部 → END
-- ```
--
-- ── 为什么调整要更严（先到财务）────────────────────────────
-- 调整预算 = 改自己部门的额度。内控要求「不能自己改自己的预算」——
-- `DEPT_LEADER` 恰恰是被调整方的上级，让同一批人既编又调等于没控制。
-- 所以调整路径**第一站就是财务**，且终审一定到 `GENERAL_MANAGER` 以上。
--
-- ── 为什么兜底边指向「调整」而不是「编制」──────────────────
-- 条件节点必须恰有一条 default 边。缺字段 / 脏数据时走**更严**的一侧（财务前置 + 总经理）
-- 比走编制安全：审批人多走一道不会漏风控，少走一道就是「自批自调」。
--
-- ── 审批人与内控 ─────────────────────────────────────────
-- * 审批人来源 = `ROLE`（运行时展开角色成员）：DEPT_LEADER / FINANCE_MANAGER /
--   GENERAL_MANAGER / FINANCE_DIRECTOR（V0182 / V0191 已种，本迁移不重复种角色）；
-- * `empty_assignee = FALLBACK_ADMIN`，**不用 SKIP** —— SKIP 会把「没人可派」
--   变成「预算自动通过」；
-- * `self_approval = ALLOW`：真正的否决在 `budget_bridge::guard_self_approval`
--   （403 + 65019），发起时 422 会让整单起不来；
-- * `biz_budget` 没有「制单人不能批自己」之外的内控要求，与 po_approve 同款。
--
-- ── 快照与 hash（不是手写的）─────────────────────────────
-- 图按 `flow::graph::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
-- 审批人按 (type,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--   budget_approve : 19b84d031306f9dbb2140690245b153b9f287de2a89d95aacaf2f6cbcf762871
-- 末尾 DO 块自检「条数 / hash / 条件节点各有一条 default 边 / 角色存在 / is_current 唯一」，
-- 任一不符 → 迁移失败 → 启动失败（宁可起不来，也不要种出一张会被引擎跳过的图）。
--
-- ── 幂等与回滚 ───────────────────────────────────────────
-- · 四层 `WHERE NOT EXISTS` 守卫（已有同名流程 / 已有同 biz_type 的默认流程 / 角色 UUID 不符
--   → 整条跳过，绝不覆盖客户配置）；重复执行零变更。
-- · 回滚见 `V0228__erp_budget_approval_flow.down.sql`：**有审批单跑在这个版本上就拒绝回滚**。
-- ======================================================

-- ---------- 1) 流程定义 ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000c0228', 'budget_approve', '预算审批（编制 / 调整双分支）', 'BUDGET_APPROVE', '财务管理',
    '预算编制与调整共用的审批流程：编制走「部门主管 → 财务（≥50 万加总经理会签）」，'
    '调整走「财务复核 → 总经理（≥50 万加财务总监会签）」。分支条件是 form.change_type，'
    '阈值与审批人都在本流程定义里改，不需要改代码。审批通过后该版本生效 '
    '（同槽旧版本自动置为「已被取代」），驳回或撤回则回草稿（额度一分没生效）。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000c0229', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'budget_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE biz_type = 'BUDGET_APPROVE' AND is_default AND deleted_at IS NULL
)
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0001'::uuid,
                  '20000000-0000-4000-8000-0000000f0003'::uuid,
                  '20000000-0000-4000-8000-0000000f0005'::uuid,
                  '20000000-0000-4000-8000-0000000f0006'::uuid)
       AND deleted_at IS NULL
);

-- ---------- 2) 版本 + 图快照 ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000c0229', '20000000-0000-4000-8000-0000000c0228', 1, 'PUBLISHED', TRUE,
    '19b84d031306f9dbb2140690245b153b9f287de2a89d95aacaf2f6cbcf762871',
    $snapshot${"edges":[{"condition":{"cmp":"lt","field":"form.total_amount","op":"cmp","value":500000},"from":"c_adj_amt","is_default":false,"key":"e_adj_low","label":"年度额度 < 500000","priority":0,"to":"n_gm"},{"condition":null,"from":"c_adj_amt","is_default":true,"key":"e_adj_high","label":"年度额度 ≥ 500000","priority":10,"to":"n_gm_fd"},{"condition":{"cmp":"eq","field":"form.change_type","op":"cmp","value":"ORIGINAL"},"from":"c_change","is_default":false,"key":"e_change_original","label":"编制（ORIGINAL）","priority":0,"to":"n_dept"},{"condition":null,"from":"c_change","is_default":true,"key":"e_change_else","label":"调整（ADJUST，兜底走更严的一侧）","priority":10,"to":"n_fin_adj"},{"condition":{"cmp":"lt","field":"form.total_amount","op":"cmp","value":500000},"from":"c_orig_amt","is_default":false,"key":"e_orig_low","label":"年度额度 < 500000","priority":0,"to":"n_fin"},{"condition":null,"from":"c_orig_amt","is_default":true,"key":"e_orig_high","label":"年度额度 ≥ 500000","priority":10,"to":"n_fin_gm"},{"condition":null,"from":"n_dept","is_default":false,"key":"e_dept_amt","label":null,"priority":0,"to":"c_orig_amt"},{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_fin_adj","is_default":false,"key":"e_adj_amt","label":null,"priority":0,"to":"c_adj_amt"},{"condition":null,"from":"n_fin_gm","is_default":false,"key":"e_fin_gm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_gm","is_default":false,"key":"e_gm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_gm_fd","is_default":false,"key":"e_gm_fd_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"c_change"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":"form.total_amount","join_mode":"OR","key":"c_adj_amt","max_remind_times":3,"name":"金额分级（调整 < 50 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":8,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":620,"y":160},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":"form.change_type","join_mode":"OR","key":"c_change","max_remind_times":3,"name":"编制 / 调整分流","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":200,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":"form.total_amount","join_mode":"OR","key":"c_orig_amt","max_remind_times":3,"name":"金额分级（编制 < 50 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":620,"y":-160},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":11,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":1060,"y":80},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0001","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_dept","max_remind_times":2,"name":"部门主管审批（编制）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":400,"y":-160},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"财务经理审批（编制）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":840,"y":-260},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin_adj","max_remind_times":2,"name":"财务复核（调整前置）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":7,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":400,"y":160},{"allow_robot":false,"approval_mode":"ALL","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":1},{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin_gm","max_remind_times":2,"name":"财务经理+总经理会签（编制）","node_type":"COUNTERSIGN","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":840,"y":-60},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm","max_remind_times":2,"name":"总经理审批（调整）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":9,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":840,"y":160},{"allow_robot":false,"approval_mode":"ALL","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0},{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0006","sort_order":1}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm_fd","max_remind_times":2,"name":"总经理+财务总监会签（调整）","node_type":"COUNTERSIGN","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":10,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":840,"y":360},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    11, 13, 'V0228 种子：预算审批（编制 / 调整双分支，50 万阈值）默认流程',
    now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0228')
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- 3) 节点 ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0308'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'c_adj_amt'::varchar, '金额分级（调整 < 50 万）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, 'form.total_amount'::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 620::int, 160::int, 8::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0302'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'c_change'::varchar, '编制 / 调整分流'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, 'form.change_type'::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 200::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0304'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'c_orig_amt'::varchar, '金额分级（编制 < 50 万）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, 'form.total_amount'::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 620::int, -160::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0311'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 1060::int, 80::int, 11::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0303'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'n_dept'::varchar, '部门主管审批（编制）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 400::int, -160::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0305'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'n_fin'::varchar, '财务经理审批（编制）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 840::int, -260::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0307'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'n_fin_adj'::varchar, '财务复核（调整前置）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 400::int, 160::int, 7::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0306'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'n_fin_gm'::varchar, '财务经理+总经理会签（编制）'::varchar, 'COUNTERSIGN'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ALL'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 840::int, -60::int, 6::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0309'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'n_gm'::varchar, '总经理审批（调整）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 840::int, 160::int, 9::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0310'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'n_gm_fd'::varchar, '总经理+财务总监会签（调整）'::varchar, 'COUNTERSIGN'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ALL'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 840::int, 360::int, 10::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0301'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0229')
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- 4) 边 ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0403'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_adj_low'::varchar, '20000000-0000-4000-8000-0000000c0308'::uuid, '20000000-0000-4000-8000-0000000c0309'::uuid, '{"cmp": "lt", "field": "form.total_amount", "op": "cmp", "value": 500000}'::jsonb, '年度额度 < 500000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0402'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_adj_high'::varchar, '20000000-0000-4000-8000-0000000c0308'::uuid, '20000000-0000-4000-8000-0000000c0310'::uuid, NULL::jsonb, '年度额度 ≥ 500000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000c0405'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_change_original'::varchar, '20000000-0000-4000-8000-0000000c0302'::uuid, '20000000-0000-4000-8000-0000000c0303'::uuid, '{"cmp": "eq", "field": "form.change_type", "op": "cmp", "value": "ORIGINAL"}'::jsonb, '编制（ORIGINAL）'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0404'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_change_else'::varchar, '20000000-0000-4000-8000-0000000c0302'::uuid, '20000000-0000-4000-8000-0000000c0307'::uuid, NULL::jsonb, '调整（ADJUST，兜底走更严的一侧）'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000c0412'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_orig_low'::varchar, '20000000-0000-4000-8000-0000000c0304'::uuid, '20000000-0000-4000-8000-0000000c0305'::uuid, '{"cmp": "lt", "field": "form.total_amount", "op": "cmp", "value": 500000}'::jsonb, '年度额度 < 500000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0411'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_orig_high'::varchar, '20000000-0000-4000-8000-0000000c0304'::uuid, '20000000-0000-4000-8000-0000000c0306'::uuid, NULL::jsonb, '年度额度 ≥ 500000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000c0406'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_dept_amt'::varchar, '20000000-0000-4000-8000-0000000c0303'::uuid, '20000000-0000-4000-8000-0000000c0304'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0407'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_fin_end'::varchar, '20000000-0000-4000-8000-0000000c0305'::uuid, '20000000-0000-4000-8000-0000000c0311'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0401'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_adj_amt'::varchar, '20000000-0000-4000-8000-0000000c0307'::uuid, '20000000-0000-4000-8000-0000000c0308'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0408'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_fin_gm_end'::varchar, '20000000-0000-4000-8000-0000000c0306'::uuid, '20000000-0000-4000-8000-0000000c0311'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0409'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_gm_end'::varchar, '20000000-0000-4000-8000-0000000c0309'::uuid, '20000000-0000-4000-8000-0000000c0311'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0410'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_gm_fd_end'::varchar, '20000000-0000-4000-8000-0000000c0310'::uuid, '20000000-0000-4000-8000-0000000c0311'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0413'::uuid, '20000000-0000-4000-8000-0000000c0229'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000c0301'::uuid, '20000000-0000-4000-8000-0000000c0302'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0229')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- ---------- 5) 审批人 ----------
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0303'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0001'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c0305'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c0307'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c0306'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 1::int),
    ('20000000-0000-4000-8000-0000000c0306'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c0309'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c0310'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c0310'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0006'::uuid, 1::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0229')
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
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0228') THEN
        RAISE NOTICE 'V0228：budget_approve 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
        RETURN;
    END IF;

    SELECT count(*) INTO v_def FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-0000000c0228' AND flow_code = 'budget_approve' AND biz_type = 'BUDGET_APPROVE'
       AND status = 'PUBLISHED' AND is_default AND current_version_id = '20000000-0000-4000-8000-0000000c0229'
       AND deleted_at IS NULL;
    IF v_def <> 1 THEN
        RAISE EXCEPTION 'V0228 自检失败：预算审批定义行不满足 PUBLISHED/默认/当前版本';
    END IF;

    SELECT count(*) INTO v_nodes FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c0229';
    IF v_nodes <> 11 THEN
        RAISE EXCEPTION 'V0228 自检失败：节点数 % （期望 11）', v_nodes;
    END IF;

    SELECT count(*) INTO v_edges FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000c0229';
    IF v_edges <> 13 THEN
        RAISE EXCEPTION 'V0228 自检失败：边数 % （期望 13）', v_edges;
    END IF;

    SELECT graph_hash INTO v_hash FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0229';
    IF v_hash <> '19b84d031306f9dbb2140690245b153b9f287de2a89d95aacaf2f6cbcf762871' THEN
        RAISE EXCEPTION 'V0228 自检失败：graph_hash 与声明不一致';
    END IF;

    -- 每个条件节点必须**恰有一条** default 边（否则引擎无从兜底）
    SELECT count(*) INTO v_bad
      FROM biz_flow_node n
     WHERE n.version_id = '20000000-0000-4000-8000-0000000c0229' AND n.node_type = 'CONDITION'
       AND (SELECT count(*) FROM biz_flow_edge e
             WHERE e.version_id = n.version_id AND e.from_node_id = n.id AND e.is_default) <> 1;
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0228 自检失败：% 个条件节点的 default 边数量不为 1', v_bad;
    END IF;

    -- 审批人必须存在（角色被客户换了 UUID → 上面就会跳过，这里是二次兜底）
    SELECT count(*) INTO v_bad
      FROM biz_flow_node_assignee a
     WHERE a.node_id IN (SELECT id FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c0229')
       AND a.assignee_type = 'ROLE'
       AND NOT EXISTS (SELECT 1 FROM sys_role r WHERE r.id = a.ref_id AND r.deleted_at IS NULL);
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0228 自检失败：% 条审批人指向不存在的角色', v_bad;
    END IF;

    RAISE NOTICE 'V0228 OK：budget_approve 已发布（% 节点 / % 边 / hash %）', v_nodes, v_edges, left(v_hash, 12);
END $$;
