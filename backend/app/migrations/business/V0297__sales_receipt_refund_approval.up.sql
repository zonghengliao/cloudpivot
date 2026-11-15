-- ======================================================
-- V0297：收款退款单审批流（SO-14 遗留 · D10 · 与 PAYABLE_PAY 同级）
--
--   ① 种开箱即用的流程 `receipt_refund`（`biz_type = 'RECEIPT_REFUND'`）：
--        START ──▶ 财务经理审批 ──▶ END
--      审批人走 `biz_flow_node_assignee`（`assignee_type = 'ROLE'` → `FINANCE_MANAGER`），
--      运行时展开角色成员；角色没人 / 被内控排除干净 → `empty_assignee = 'FALLBACK_ADMIN'`
--      回退超管（**不是 SKIP** —— SKIP 会把「没人可派」偷换成「自动通过」）。
--   ② 本迁移**不动任何表结构、不动退款单状态域**：
--        `biz_receipt_refund.status` 仍是 DRAFT/CONFIRMED/POSTED/CANCELLED，
--        审批是「过账的前置条件」而不是一个状态（口径见卡 t_b9840c8d 路线 ②）。
--        因此 `ck_biz_receipt_refund_status` 一个字都不改，没有 DROP/ADD CHECK。
--   ②' 会签口径 `approval_mode = 'ANY'`（**不是 ALL**）：财务经理岗位可能有多人，
--      任一人复核即可；写 ALL 会让「多一个超管兜底」变成「所有人必须都批」，
--      单节点流程会一直卡在 PENDING（实测：共用库里两个超管 → 一票之后仍是 PENDING）。
--   ③ 不新增权限码：提交审批入口 `POST /business/erp/receipt-refund/{id}/submit-approval`
--      复用 `business:erp:receipt-refund:status`（V0295 已种，菜单已承载）。
--      与 SO-06 的 `POST /sales-order/{id}/submit-approval` 同一口径 —— 提交审批
--      是「推进单据状态」的一个动作，不另开码（否则 perm_menu_gate 又要多一行菜单）。
--
-- ── 号段（开工实测复核）──────────────────────────────────
--   开工时 `ls backend/migrations/business | sort -V | tail`：
--     V0294 收款退款单（t_573bfe5b）/ V0295 退款权限码 / V0296 收款单页落地（SO-18 改号）
--   ⇒ 本卡顺延取 **V0297**。
--   （SO-18 的 `V0294__sales_receipt_menu_page` 与退款卡的 V0294 撞号，已在 V0296 里
--     改号并写明原因：alun-db 对同 kind 重复 version 是**启动即失败**，不是告警。）
--
-- ── 口径：为什么不是 PENDING_APPROVE 状态 ────────────────
--   路线 ① 要在 `ck_biz_receipt_refund_status` 里加一个值（DROP/ADD CHECK），
--   然后 `CONFIRMED → PENDING_APPROVE → POSTED`。本卡选路线 ②，理由：
--   · 资金单据的状态域是「钱有没有动」的台账（草稿 / 已确认 / 已过账 / 已作废），
--     审批是「这笔钱能不能动」的**前置条件**，两者混在一个字段里，
--     报表与状态日志都得再拆一次；
--   · 与既有的凭证红冲审批（`VOUCHER_REVERSE`）同思路：被审对象的状态一个都不变，
--     审批通过才执行那个「不可逆动作」；
--   · 不改已应用的 CHECK，少一次 `DROP CONSTRAINT` 的风险面。
--   代价：退款单在审批中时状态仍是 `CONFIRMED`，前端要靠
--   `GET /business/erp/receipt-refund/{id}` 返回的 `approval` 段（实例号 / 状态 /
--   在跑待办）来判断「已提交待批」，**不能只看 status**。
--
-- ── 判定与执行 ───────────────────────────────────────────
--   · 提交：`receipt_refund::submit_approval` → `engine::start`（`biz_id = 退款单 id`）；
--   · 过账前置：`receipt_refund::change_status` 的 `CONFIRMED → POSTED` 必须有
--     `biz_flow_instance(biz_type='RECEIPT_REFUND', biz_id=退款单 id, status='APPROVED')`，
--     否则 409 + `66044`（`ERP_RECEIPT_REFUND_APPROVAL_REQUIRED`）并给出提交入口；
--   · 审批通过回调：`receipt_refund_bridge::on_refund_settled` 在回调那一步**直接过账**
--     （`post_refund_voucher`），不需要人再点一次；驳回 / 撤回 / 终止 → 退款单留在
--     `CONFIRMED`（账一分没动，可以改完再提）。
--
-- ── 快照与 hash（不是手写的）────────────────────────────
--   图按 `flow::graph::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
--   审批人按 (kind,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--     receipt_refund : 6a592107a9e7e6bc34a47bc4098926f648eb15e4fb3a5b45c1f93354dfc2d1b2
--   末尾 DO 块自检「条数 / hash / 审批人角色 / 兜底策略 / END 汇聚 / START 直连」，
--   任一不符 → 迁移失败 → 启动失败（宁可起不来，也不要种一张会被引擎跳过的图）。
--
-- ── 幂等与安全 ────────────────────────────────────────
--   · 三层 `WHERE NOT EXISTS` 守卫：已有 `flow_code='receipt_refund'`、或已有别的
--     `biz_type='RECEIPT_REFUND'` 默认流程（撞 `uk_biz_flow_def_default`）→ 整条跳过；
--   · 审批人角色不存在（客户库没有 FINANCE_MANAGER）→ 也整条跳过（NOTICE），
--     绝不种出一张 assignee 指向不存在角色的图；
--   · 固定 uuid + `ON CONFLICT DO NOTHING`；
--   · 回滚：`V0297__sales_receipt_refund_approval.down.sql`（只删本迁移种的四张表行）。
-- ======================================================

-- ---------- ① 流程定义 ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, is_system)
SELECT
    '20000000-0000-4000-8000-0000000d0701'::uuid, 'receipt_refund', '收款退款审批（财务复核）', 'RECEIPT_REFUND', '销售管理',
    '收款退款单过账前必须走本流程：财务经理复核后退款才记账（借 1122 应收账款 / 贷 1002 银行存款）。'
    '审批是过账的前置条件，不是退款单的一个状态 —— 驳回 / 撤回后单据仍是「已确认」，可改完再提。'
    '审批人与兜底策略都在流程定义里改，不需要发版。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000d0702'::uuid, 1, TRUE, FALSE, TRUE
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'receipt_refund' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'RECEIPT_REFUND' AND is_default AND deleted_at IS NULL
)
-- 审批人角色必须存在（否则种出来的 assignee 指向不存在的角色 → 永远没人可派）
AND EXISTS (
    SELECT 1 FROM sys_role WHERE id = '20000000-0000-4000-8000-0000000f0005'::uuid AND deleted_at IS NULL
);

-- ---------- ② 流程版本（快照 + hash） ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000d0702'::uuid, '20000000-0000-4000-8000-0000000d0701'::uuid, 1, 'PUBLISHED', TRUE,
    '6a592107a9e7e6bc34a47bc4098926f648eb15e4fb3a5b45c1f93354dfc2d1b2',
    $snapshot${"edges":[{"condition":null,"from":"n_finance","is_default":false,"key":"e_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_finance"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":640,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_finance","max_remind_times":2,"name":"财务经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    3, 2, 'V0297 种子：收款退款审批（财务经理复核，过账前置）', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0701'::uuid)
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- ③ 节点（3） ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0711'::uuid, '20000000-0000-4000-8000-0000000d0702'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0712'::uuid, '20000000-0000-4000-8000-0000000d0702'::uuid, 'n_finance'::varchar, '财务经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0713'::uuid, '20000000-0000-4000-8000-0000000d0702'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 640::int, 0::int, 3::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0702'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- ④ 边（2） ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0721'::uuid, '20000000-0000-4000-8000-0000000d0702'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d0711'::uuid, '20000000-0000-4000-8000-0000000d0712'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0722'::uuid, '20000000-0000-4000-8000-0000000d0702'::uuid, 'e_end'::varchar, '20000000-0000-4000-8000-0000000d0712'::uuid, '20000000-0000-4000-8000-0000000d0713'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0702'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- ---------- ⑤ 审批人（ROLE → 财务经理） ----------
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0712'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0702'::uuid)
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ⑥ 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    m_rows int;
    v_rows int;
    n_rows int;
    e_rows int;
    a_rows int;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0701'::uuid) THEN
        RAISE NOTICE 'V0297：receipt_refund 种子跳过（已有同名流程 / 同 biz_type 默认流程 / FINANCE_MANAGER 角色不存在）';
        RETURN;
    END IF;

    SELECT count(*) INTO m_rows FROM biz_flow_def d
     WHERE d.id = '20000000-0000-4000-8000-0000000d0701'::uuid AND d.flow_code = 'receipt_refund'
       AND d.biz_type = 'RECEIPT_REFUND' AND d.status = 'PUBLISHED' AND d.is_default
       AND d.current_version_id = '20000000-0000-4000-8000-0000000d0702'::uuid AND d.current_version_no = 1
       AND d.deleted_at IS NULL;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0297 自检失败：receipt_refund 定义行不满足 PUBLISHED/默认/当前版本';
    END IF;

    SELECT count(*) INTO v_rows FROM biz_flow_def_version v
     WHERE v.id = '20000000-0000-4000-8000-0000000d0702'::uuid AND v.def_id = '20000000-0000-4000-8000-0000000d0701'::uuid
       AND v.version_status = 'PUBLISHED' AND v.is_current
       AND v.graph_hash = '6a592107a9e7e6bc34a47bc4098926f648eb15e4fb3a5b45c1f93354dfc2d1b2'
       AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
       AND v.node_count = 3 AND v.edge_count = 2;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0297 自检失败：receipt_refund 版本行（快照/hash/计数）不对';
    END IF;

    SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0702'::uuid;
    SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0702'::uuid;
    IF n_rows <> 3 OR e_rows <> 2 THEN
        RAISE EXCEPTION 'V0297 自检失败：节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
    END IF;

    -- 审批节点必须「有人可派 + 没人时回退超管」，且不允许自动通过
    SELECT count(*) INTO m_rows FROM biz_flow_node
     WHERE version_id = '20000000-0000-4000-8000-0000000d0702'::uuid AND node_key = 'n_finance'
       AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
       AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW'
       AND approval_mode = 'ANY';
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0297 自检失败：审批节点的审批人来源/兜底策略/会签口径不对（应为 ROLE + FALLBACK_ADMIN + ALLOW + ANY）';
    END IF;

    -- 审批人必须绑到财务经理角色（且只有一行）
    SELECT count(*) INTO a_rows FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = '20000000-0000-4000-8000-0000000d0702'::uuid
       AND a.assignee_type = 'ROLE' AND a.ref_id = '20000000-0000-4000-8000-0000000f0005'::uuid;
    IF a_rows <> 1 THEN
        RAISE EXCEPTION 'V0297 自检失败：审批人应绑定财务经理角色 1 行，实际 %', a_rows;
    END IF;

    -- END 只有一条入边，仍按团队口径写 OR 汇聚（改图加支路时不用再回头改）
    SELECT count(*) INTO m_rows FROM biz_flow_node
     WHERE version_id = '20000000-0000-4000-8000-0000000d0702'::uuid AND node_key = 'e' AND join_mode = 'OR';
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0297 自检失败：END 节点必须 OR 汇聚';
    END IF;

    -- START 必须直连审批节点（中间不许有条件节点空转）
    SELECT count(*) INTO m_rows FROM biz_flow_edge e
      JOIN biz_flow_node f ON f.id = e.from_node_id
      JOIN biz_flow_node t ON t.id = e.to_node_id
     WHERE e.version_id = '20000000-0000-4000-8000-0000000d0702'::uuid
       AND f.node_key = 's' AND t.node_key = 'n_finance';
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0297 自检失败：START 必须直连财务经理审批节点';
    END IF;

    RAISE NOTICE 'V0297 自检通过：收款退款审批（财务经理复核 · 过账前置）标准流程已就绪';
END $$;
