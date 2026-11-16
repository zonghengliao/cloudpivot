-- ======================================================
-- V0435：销售报价单审批流（销售链 M3-04 · 报价单「发出」的前置条件）
--
--   ① `biz_quotation` 加 `approval_instance_id`（最近一轮审批实例弱引用）+ 部分索引
--   ② 种开箱即用的流程 `quotation_approve`（`biz_type = 'QUOTATION_APPROVE'`）：
--        START ──▶ 销售主管审批 ──▶ 条件（金额分级）
--                                    ├── form.amount < 100000  ──▶ END
--                                    └── 金额 ≥ 100000（兜底边）──▶ 财务经理复核 ──▶ END
--      金额分级写成**流程定义里的条件边**（`form.amount`），不是代码里的 if/else ——
--      客户改阈值 / 换审批岗位，在流程定义里另存版本即可，不用改代码、不用发版。
--   ③ 审批人全部走 `biz_flow_node_assignee`（`assignee_type = 'ROLE'`，运行时展开角色成员）：
--        SALES_SUPERVISOR（V0281 已种）/ FINANCE_MANAGER（V0191 已种，V0281 复用）。
--      **本迁移不种角色**：两个角色都在更早的迁移里，缺了说明这套库不是我们这个种子的形态，
--      此时整条流程跳过（NOTICE），绝不种出一张 assignee 指向不存在角色的图。
--
-- ── 号段（开工实测复核）──────────────────────────────────
--   开工时 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`：
--     V0432（报价单表 M3-01）/ V0433（报价单权限码 M3-02）/ V0434（转单权限码 M3-03）
--   ⇒ 本卡顺延取 **V0435**。
--
-- ── 口径：为什么审批不是状态（与 66044 收款退款同路线）────
--   报价单的状态域是「客户那边进行到哪一步」（草稿 / 已发出 / 已接受 / 已拒绝 / 已作废），
--   审批是「这张价能不能对外报」的**前置条件**。塞进同一个字段的话：
--     · `SENT` 既表示「客户收到了」又暗含「内部批过了」，报表拆不出来；
--     · 要动 `ck_biz_quotation_status`（DROP/ADD CHECK），风险面比收益大；
--     · 与收款退款（`RECEIPT_REFUND`）/ 凭证红冲（`VOUCHER_REVERSE`）的口径分叉。
--   代价（如实写明）：**审批在跑时 `status` 仍是 `DRAFT`**，前端不能只看 status 判断
--   「已提交待批」，要读 `GET /business/erp/quotation/{id}` 的 `approval` 段
--   （实例号 / 状态 / 在跑待办）或 `approval_instance_id` 列。
--
-- ── 判定与执行 ───────────────────────────────────────────
--   · 提交：`quotation_handler::submit_approval` → `engine::start`（`biz_id = 报价单 id`），
--     **同一事务**回写 `approval_instance_id`；
--   · 发出前置：`quotation_handler::change_status_with` 的 `DRAFT → SENT` 必须有
--     `biz_flow_instance(biz_type='QUOTATION_APPROVE', biz_id=报价单 id, status='APPROVED')`，
--     否则 409 + `66088`（`ERP_QUOTATION_APPROVAL_REQUIRED`）并给出提交入口；
--   · 审批通过回调：`quotation_approval_bridge::on_quotation_settled` 在回调那一步**直接发出**
--     （`DRAFT → SENT`，与人工发出同一条代码路径），不需要人再点一次；
--     驳回 / 撤回 / 终止 → 报价单留在 `DRAFT`（价格一分没对外，可改完再提）。
--
-- ── 快照与 hash（不是手写的）────────────────────────────
--   图按 `flow::graph::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
--   审批人按 (kind,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--     quotation_approve : 8633ba2dc797f5fbed626fef0e6d013a2bb9e2669b5ecc41c7b684b4cdb956b7
--   （由 `scripts/verify/m304_quotation_approval_e2e.sh` 用 `p22b_rows_hash.py` 的口径
--     在库里复算一遍作证：`drifted = 0`）
--   末尾 DO 块自检「条数 / hash / 条件边阈值 / 审批人角色 / 兜底策略 / END 汇聚 / START 直连」，
--   任一不符 → 迁移失败 → 启动失败（宁可起不来，也不要种出一张会被引擎跳过的图）。
--
-- ── 幂等与安全 ────────────────────────────────────────
--   · 加列 / 索引 `IF NOT EXISTS`；
--   · 流程四层 `WHERE NOT EXISTS` 守卫：已有 `flow_code='quotation_approve'`、或已有
--     同 `biz_type='QUOTATION_APPROVE'` 的默认流程（撞 `uk_biz_flow_def_default`）→ 整条跳过
--     （**绝不覆盖客户配置**）；
--   · 两个角色不存在（客户库用别的 UUID 预建过 / 删过）→ 也整条跳过（NOTICE）；
--   · 固定 uuid + `ON CONFLICT DO NOTHING`；
--   · 回滚：`V0435__quotation_approval_flow.down.sql`（只删本迁移种的四张表行 + 加的列）。
-- ======================================================

-- ---------- ① 报价单：审批实例弱引用 ----------
ALTER TABLE biz_quotation
    ADD COLUMN IF NOT EXISTS approval_instance_id uuid;

COMMENT ON COLUMN biz_quotation.approval_instance_id IS
    '最近一轮审批实例 id（M3-04）：提交审批时同一事务写入；作废时清空。审批不是状态，'
    '单据状态仍是 DRAFT/ SENT/...，是否待批要看该实例的 status';

CREATE INDEX IF NOT EXISTS idx_biz_quotation_approval
    ON biz_quotation (approval_instance_id)
    WHERE approval_instance_id IS NOT NULL;

-- ---------- ② 流程定义：报价单审批 quotation_approve ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, is_system)
SELECT
    '20000000-0000-4000-8000-0000000d0431'::uuid, 'quotation_approve', '报价单审批（标准·金额分级）',
    'QUOTATION_APPROVE', '销售管理',
    '销售报价单「发出」前必须走本流程：销售主管复核报价与折扣，价税合计 ≥ 10 万的报价再加一道财务经理复核。'
    '审批是「发出」的前置条件，不是报价单的一个状态 —— 通过后系统自动发出，驳回 / 撤回则留在草稿'
    '（可改完再提）。金额阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000d0432'::uuid, 1, TRUE, FALSE, TRUE
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'quotation_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'QUOTATION_APPROVE' AND is_default AND deleted_at IS NULL
)
-- 审批人角色必须是我们种的那一套（客户用别的 UUID 预建过 → 整条跳过，别种出悬空的 assignee）
AND (
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0007'::uuid,
                  '20000000-0000-4000-8000-0000000f0005'::uuid)
       AND deleted_at IS NULL
) = 2;

-- ---------- ③ 流程版本（快照 + hash） ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000d0432'::uuid, '20000000-0000-4000-8000-0000000d0431'::uuid, 1,
    'PUBLISHED', TRUE,
    '8633ba2dc797f5fbed626fef0e6d013a2bb9e2669b5ecc41c7b684b4cdb956b7',
    $snapshot${"edges":[{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":100000},"from":"c_amt","is_default":false,"key":"e_low","label":"金额 < 100000","priority":0,"to":"e"},{"condition":null,"from":"c_amt","is_default":true,"key":"e_low_else","label":"金额 ≥ 100000","priority":10,"to":"n_finance"},{"condition":null,"from":"n_finance","is_default":false,"key":"e_finance_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_sales","is_default":false,"key":"e_sales_amt","label":null,"priority":0,"to":"c_amt"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_sales"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_amt","max_remind_times":3,"name":"金额分级（≥ 10 万加财务复核）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":420,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":860,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_finance","max_remind_times":2,"name":"财务经理复核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":640,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0007","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_sales","max_remind_times":2,"name":"销售主管审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    5, 5, 'V0435 种子：报价单审批（发出前置 · 金额 ≥10 万加财务复核）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0431'::uuid)
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- ④ 节点（5） ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0441'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0442'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 'n_sales'::varchar, '销售主管审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0443'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 'c_amt'::varchar, '金额分级（≥ 10 万加财务复核）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 420::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0444'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 'n_finance'::varchar, '财务经理复核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 640::int, 0::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0445'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 860::int, 0::int, 5::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0432'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- ⑤ 边（5：两条串行 + 一条金额条件 + 一条兜底 + 收尾） ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0451'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d0441'::uuid, '20000000-0000-4000-8000-0000000d0442'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0452'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 'e_sales_amt'::varchar, '20000000-0000-4000-8000-0000000d0442'::uuid, '20000000-0000-4000-8000-0000000d0443'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0453'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 'e_low'::varchar, '20000000-0000-4000-8000-0000000d0443'::uuid, '20000000-0000-4000-8000-0000000d0445'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 100000}'::jsonb, '金额 < 100000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0454'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 'e_low_else'::varchar, '20000000-0000-4000-8000-0000000d0443'::uuid, '20000000-0000-4000-8000-0000000d0444'::uuid, NULL::jsonb, '金额 ≥ 100000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000d0455'::uuid, '20000000-0000-4000-8000-0000000d0432'::uuid, 'e_finance_end'::varchar, '20000000-0000-4000-8000-0000000d0444'::uuid, '20000000-0000-4000-8000-0000000d0445'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0432'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- ---------- ⑥ 审批人（ROLE → 销售主管 / 财务经理） ----------
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0442'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0007'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d0444'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0432'::uuid)
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ⑦ 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    m_rows int;
    v_rows int;
    n_rows int;
    e_rows int;
    a_rows int;
BEGIN
    -- 7.1 表结构（无论流程是否跳过都必须改到位）
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_quotation' AND column_name = 'approval_instance_id'
    ) THEN
        RAISE EXCEPTION 'V0435 自检失败：biz_quotation 缺 approval_instance_id';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE schemaname = 'public' AND tablename = 'biz_quotation'
           AND indexname = 'idx_biz_quotation_approval'
    ) THEN
        RAISE EXCEPTION 'V0435 自检失败：索引 idx_biz_quotation_approval 缺失';
    END IF;

    -- 7.2 流程（客户已有同名流程 / 角色 UUID 不符 → 守卫整条跳过）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0431'::uuid) THEN
        RAISE NOTICE 'V0435：quotation_approve 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
        RETURN;
    END IF;

    SELECT count(*) INTO m_rows FROM biz_flow_def d
     WHERE d.id = '20000000-0000-4000-8000-0000000d0431'::uuid AND d.flow_code = 'quotation_approve'
       AND d.biz_type = 'QUOTATION_APPROVE' AND d.status = 'PUBLISHED' AND d.is_default
       AND d.current_version_id = '20000000-0000-4000-8000-0000000d0432'::uuid AND d.current_version_no = 1
       AND d.deleted_at IS NULL;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0435 自检失败：quotation_approve 定义行不满足 PUBLISHED/默认/当前版本';
    END IF;

    SELECT count(*) INTO v_rows FROM biz_flow_def_version v
     WHERE v.id = '20000000-0000-4000-8000-0000000d0432'::uuid
       AND v.def_id = '20000000-0000-4000-8000-0000000d0431'::uuid
       AND v.version_status = 'PUBLISHED' AND v.is_current
       AND v.graph_hash = '8633ba2dc797f5fbed626fef0e6d013a2bb9e2669b5ecc41c7b684b4cdb956b7'
       AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
       AND v.node_count = 5 AND v.edge_count = 5;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0435 自检失败：quotation_approve 版本行（快照/hash/计数）不对';
    END IF;

    SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0432'::uuid;
    SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0432'::uuid;
    IF n_rows <> 5 OR e_rows <> 5 THEN
        RAISE EXCEPTION 'V0435 自检失败：节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
    END IF;

    -- 金额分级必须真的落在条件边上（不是代码里 if/else），阈值逐字是 100000
    SELECT count(*) INTO m_rows FROM biz_flow_edge
     WHERE version_id = '20000000-0000-4000-8000-0000000d0432'::uuid
       AND NOT is_default AND condition_expr IS NOT NULL
       AND condition_expr->>'field' = 'form.amount'
       AND (condition_expr->>'value')::numeric = 100000;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0435 自检失败：按 form.amount 分流的条件边应为 1 条（100000），实际 %', m_rows;
    END IF;
    -- 兜底边必须 is_default（否则金额 ≥ 阈值时无路可走 → 实例卡死）
    SELECT count(*) INTO m_rows FROM biz_flow_edge
     WHERE version_id = '20000000-0000-4000-8000-0000000d0432'::uuid AND is_default AND priority = 10;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0435 自检失败：else 边（is_default + priority=10）应为 1 条，实际 %', m_rows;
    END IF;

    -- 两个审批节点都必须「有人可派 + 没人时回退超管」（不许静默自动通过）
    SELECT count(*) INTO m_rows FROM biz_flow_node
     WHERE version_id = '20000000-0000-4000-8000-0000000d0432'::uuid
       AND node_key IN ('n_sales', 'n_finance')
       AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
       AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW' AND approval_mode = 'ANY';
    IF m_rows <> 2 THEN
        RAISE EXCEPTION 'V0435 自检失败：两个审批节点的审批人来源/兜底策略/会签口径不对（应 2 行，实际 %）', m_rows;
    END IF;

    -- 审批人必须逐节点绑到对应角色（销售主管 / 财务经理）
    SELECT count(*) INTO a_rows FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = '20000000-0000-4000-8000-0000000d0432'::uuid
       AND a.assignee_type = 'ROLE';
    IF a_rows <> 2 THEN
        RAISE EXCEPTION 'V0435 自检失败：审批人应有 2 行（每节点一个角色），实际 %', a_rows;
    END IF;
    SELECT count(*) INTO m_rows FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = '20000000-0000-4000-8000-0000000d0432'::uuid
       AND ((n.node_key = 'n_sales'   AND a.ref_id = '20000000-0000-4000-8000-0000000f0007'::uuid)
         OR (n.node_key = 'n_finance' AND a.ref_id = '20000000-0000-4000-8000-0000000f0005'::uuid));
    IF m_rows <> 2 THEN
        RAISE EXCEPTION 'V0435 自检失败：审批人角色绑定与岗位不符（应 2 行，实际 %）', m_rows;
    END IF;

    -- END 有多条入边 ⇒ 必须 OR 汇聚（AND 会等人齐 → 审批单永久卡死）
    SELECT count(*) INTO m_rows FROM biz_flow_node
     WHERE version_id = '20000000-0000-4000-8000-0000000d0432'::uuid AND node_key = 'e' AND join_mode = 'OR';
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0435 自检失败：quotation_approve 的 END 节点必须 OR 汇聚';
    END IF;
    -- 串行链：销售主管必须是**第一个**审批节点（START 的直接下游，两档都要过他）
    SELECT count(*) INTO m_rows FROM biz_flow_edge e
      JOIN biz_flow_node f ON f.id = e.from_node_id
      JOIN biz_flow_node t ON t.id = e.to_node_id
     WHERE e.version_id = '20000000-0000-4000-8000-0000000d0432'::uuid
       AND f.node_key = 's' AND t.node_key = 'n_sales';
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0435 自检失败：START 必须直连销售主管审批节点';
    END IF;

    RAISE NOTICE 'V0435 自检通过：报价单审批（发出前置 · 金额 ≥10 万加财务复核）标准流程已就绪';
END $$;
