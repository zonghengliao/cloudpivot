-- ======================================================
-- V0231：资产处置审批流种子 `asset_disposal`（ARCH-6 §5.6.3）
--
-- 图（3 节点 / 2 边，不分金额档）：
--   START → 总经理审批 → END
--
-- ── 为什么第一站就是总经理、且不分金额档 ────────────────────
-- 处置资产 = 把公司资产变成现金或废品，是最容易发生舞弊的环节
-- （低价卖给关联方、假装报废后私用）。处置频次低（一年几次），
-- 多一道总经理签的成本可以接受；按金额分级反而给了「拆单绕过」的口子
-- （把 30 万的设备拆成 3 台 10 万的）。
--
-- ── 审批人与内控 ─────────────────────────────────────────
-- * 审批人来源 = `ROLE`（`GENERAL_MANAGER`，V0182/V0191 已种，本迁移不重复种角色）；
-- * `empty_assignee = FALLBACK_ADMIN`（**不用 SKIP**：SKIP 会把「没人可派」变成
--   「处置自动通过」，那是最不该自动通过的审批）；
-- * `self_approval = ALLOW`：真正的否决在 `asset_bridge::guard_self_approval`
--   （403 + 65019）——发起时就 422 会让整单起不来；
-- * 驳回 / 撤回 → 资产停在原状态（`IN_USE` / `IDLE`），一张凭证都不生成。
--
-- ── 快照与 hash（不是手写的）─────────────────────────────
-- 图按 `flow::graph::snapshot()` 的确定性排序序列化成 canonical JSON
-- （递归按键字典序、紧凑无空白），再 sha256：
--   asset_disposal : b4bef2fc227db8131f75792028217d170e961a529055db9a96d3ac11333139ac
-- 末尾 DO 块自检「条数 / hash / 角色存在 / is_current 唯一」，任一不符 → 迁移失败。
--
-- ── 幂等与回滚 ───────────────────────────────────────────
-- 四层 `WHERE NOT EXISTS`（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符 → 整条跳过）；
-- down 见同名文件：**有审批实例跑在这个版本上就拒绝回滚**。
-- ======================================================

-- ---------- 1) 流程定义 ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000c0230', 'asset_disposal', '资产处置审批（出售 / 报废 / 盘亏）', 'ASSET_DISPOSAL', '财务管理',
    '资产处置必须审批：出售 / 报废 / 盘亏三种处置都走这条流程，第一站就是总经理（不分金额档，'
    '防拆单绕过）。审批通过后自动生成处置凭证（出售 3 张 / 报废 2 张 / 盘亏 2 张）、'
    '把卡片置为「已处置」并写变动留痕；驳回或撤回则资产保持原状态，一张凭证都不生成。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000c0231', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'asset_disposal' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE biz_type = 'ASSET_DISPOSAL' AND is_default AND deleted_at IS NULL
)
AND EXISTS (
    SELECT 1 FROM sys_role WHERE id = '20000000-0000-4000-8000-0000000f0003'::uuid AND deleted_at IS NULL
);

-- ---------- 2) 版本 + 图快照 ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000c0231', '20000000-0000-4000-8000-0000000c0230', 1, 'PUBLISHED', TRUE,
    'b4bef2fc227db8131f75792028217d170e961a529055db9a96d3ac11333139ac',
    $snapshot${"edges":[{"condition":null,"from":"n_gm","is_default":false,"key":"e_gm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_gm"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":720,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm","max_remind_times":2,"name":"总经理审批（处置不分金额档）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":360,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$,
    3, 2, 'V0231 种子：资产处置审批（总经理一站定稿）默认流程',
    now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0230')
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- 3) 节点 ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0501'::uuid, '20000000-0000-4000-8000-0000000c0231'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0502'::uuid, '20000000-0000-4000-8000-0000000c0231'::uuid, 'n_gm'::varchar, '总经理审批（处置不分金额档）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 360::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0503'::uuid, '20000000-0000-4000-8000-0000000c0231'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 720::int, 0::int, 3::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0231')
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- 4) 边 ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0601'::uuid, '20000000-0000-4000-8000-0000000c0231'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000c0501'::uuid, '20000000-0000-4000-8000-0000000c0502'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0602'::uuid, '20000000-0000-4000-8000-0000000c0231'::uuid, 'e_gm_end'::varchar, '20000000-0000-4000-8000-0000000c0502'::uuid, '20000000-0000-4000-8000-0000000c0503'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0231')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- ---------- 5) 审批人 ----------
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0502'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0231')
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
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0230') THEN
        RAISE NOTICE 'V0231：asset_disposal 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
        RETURN;
    END IF;

    SELECT count(*) INTO v_def FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-0000000c0230' AND flow_code = 'asset_disposal' AND biz_type = 'ASSET_DISPOSAL'
       AND status = 'PUBLISHED' AND is_default AND current_version_id = '20000000-0000-4000-8000-0000000c0231'
       AND deleted_at IS NULL;
    IF v_def <> 1 THEN
        RAISE EXCEPTION 'V0231 自检失败：资产处置审批定义行不满足 PUBLISHED/默认/当前版本';
    END IF;

    SELECT count(*) INTO v_nodes FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c0231';
    IF v_nodes <> 3 THEN
        RAISE EXCEPTION 'V0231 自检失败：节点数 % （期望 3）', v_nodes;
    END IF;

    SELECT count(*) INTO v_edges FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000c0231';
    IF v_edges <> 2 THEN
        RAISE EXCEPTION 'V0231 自检失败：边数 % （期望 2）', v_edges;
    END IF;

    SELECT graph_hash INTO v_hash FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0231';
    IF v_hash <> 'b4bef2fc227db8131f75792028217d170e961a529055db9a96d3ac11333139ac' THEN
        RAISE EXCEPTION 'V0231 自检失败：graph_hash 与声明不一致';
    END IF;

    -- 版本行必须唯一 current（同一 def 只许一个在线版本）
    SELECT count(*) INTO v_bad FROM biz_flow_def_version
     WHERE def_id = '20000000-0000-4000-8000-0000000c0230' AND is_current;
    IF v_bad <> 1 THEN
        RAISE EXCEPTION 'V0231 自检失败：current 版本行有 % 个（期望 1）', v_bad;
    END IF;

    -- 审批人必须存在（角色被客户换了 UUID → 上面就会跳过，这里是二次兜底）
    SELECT count(*) INTO v_bad
      FROM biz_flow_node_assignee a
     WHERE a.node_id IN (SELECT id FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c0231')
       AND a.assignee_type = 'ROLE'
       AND NOT EXISTS (SELECT 1 FROM sys_role r WHERE r.id = a.ref_id AND r.deleted_at IS NULL);
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0231 自检失败：% 条审批人指向不存在的角色', v_bad;
    END IF;

    RAISE NOTICE 'V0231 OK：asset_disposal 已发布（% 节点 / % 边 / hash %）', v_nodes, v_edges, left(v_hash, 12);
END $$;
