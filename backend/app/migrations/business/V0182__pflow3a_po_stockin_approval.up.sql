-- ======================================================
-- V0182：入库链标准审批流（P-FLOW-3A）
--        ① `biz_purchase_order` 加 `approval_instance_id`（提交审批的实例弱引用）
--        ② `biz_stock_in` 放开 `PENDING_POST`（过账审批中）+ 审批实例绑死约束扩到两种「审批中」
--        ③ 角色种子：DEPT_LEADER / PURCHASE_MANAGER / GENERAL_MANAGER / WAREHOUSE_MANAGER
--        ④ 两个开箱即用的标准流程：
--             · `po_approve`     采购订单审批（金额三档：< 1 万部门主管 / 1–10 万采购经理 / ≥ 10 万两人会签）
--             · `stock_in_post`  收货过账审批（< 1000 免审直过 + 通知仓管主管 / ≥ 1000 仓管主管审批）
--        ⑤ 两个按钮权限码（+ 授给超管）
--
-- ── 为什么要这条迁移 ────────────────────────────────────
-- 卡面《P-FLOW-3A》：「预置标准审批流（开箱即用），并认可『模板 ≠ 写死』三原则」。
-- 采购订单提交、收货过账是入库链上两个真正会改变经营数据的动作：
--   · 采购订单提交后就会进入收货安排 —— 没人复核等于「谁下单谁说了算」；
--   · 收货过账一次动六本账（库存 / 库位账 / 流水 / 序列号 / 凭证 / 应付）。
-- 本迁移把**金额分级写成流程定义里的条件节点**（不是代码里的 if/else），
-- 阈值、审批人、超时全在 `biz_flow_node` / `biz_flow_edge.condition_expr` /
-- `biz_flow_node_assignee` 里 —— 客户要改成「5 万以上才走采购经理」，
-- 在「流程定义」里另存一个版本即可，**不用改代码、不发版**。这正是本卡的核心价值。
--
-- ── 为什么审批人用 ROLE 而不是 ADMIN（与 V0157 / V0174 / V0175 的取舍相反）──
-- 那三条种子的注释写着「本仓没有部门主管 / 财务总监这些业务角色种子，所以用 ADMIN」。
-- 本卡正好**把角色种进来了**（见 ③），于是改成卡面要求的形态：
--   · 审批人来源 = `ROLE`（运行时展开该角色下的所有用户）；
--   · `empty_assignee = FALLBACK_ADMIN` —— **角色里 0 人时回退给平台超管，而不是静默跳过**。
--     这后半句是关键：`SKIP` 会让「没人可派」变成「审批自动通过」，比卡住更坏
--     （V0174 文件头就是这个担心的原话）。FALLBACK_ADMIN 下开箱即用（默认只有超管），
--     客户把人绑到角色上之后自动按岗位分权，**流程一行不用改**。
--   · `self_approval = ALLOW` **故意不是 FORBID**：FORBID 在**发起时**就 422（整单起不来），
--     而内控要的是「提交成功、但**制单人这一票**点不动」→ 待办照常可见留痕，
--     真正的否决在审批入口（`purchase_bridge::guard_self_approval` /
--     `stockin_bridge::guard_self_approval`，403 + 65019）。
--
-- ── 状态口径 ────────────────────────────────────────────
-- 收货单（`biz_stock_in.status`）：
--   DRAFT ──提交过账审批──▶ PENDING_POST ──审批通过──▶ POSTED（此时才动六本账）
--                                  └──驳回 / 撤回──▶ DRAFT（账一分没动）
--                                  └──审批中作废──▶ CANCELLED（撤审批单，从未入过账所以不冲账）
-- 采购单（`biz_purchase_order.status`）：
--   DRAFT ──提交审批──▶ SUBMITTED ──审批通过──▶ APPROVED（此后才正式收货）
--                              └──驳回 / 撤回──▶ DRAFT（意见追加到 remark）
--
-- ── 库侧硬约束（不是靠代码自觉）────────────────────────
-- `ck_biz_si_status` 从 4 值扩到 5 值（加 `PENDING_POST`）；
-- `ck_biz_si_pending_cancel` 从「只有 PENDING_CANCEL 才许挂实例」扩成
-- 「两种审批中状态 ⟺ 有实例；其它状态 ⟹ 没有实例；软删行豁免」——
-- 前半句挡「审批中却没挂实例（永远等不到回调）」，后半句挡
-- 「已终态却还挂着实例（下次发起撞 uk_biz_flow_instance_biz_active）」，
-- 软删豁免是因为「审批中删单」是合法反向路径（先终止审批再软删，同一事务）。
--
-- ── 快照与 hash（不是手写的）───────────────────────────
-- 图按 `graph.rs::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
-- 审批人按 (kind,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--   po_approve    : 8f65819ea25e1145d441d9b947b89b21bd4bfdc5075ac321367ea106a06366de
--   stock_in_post : 035d3c246b89330681f67c353277dc53313c7a6521041e45841bd078b2563b3d
-- 末尾 DO 块自检「条数 / hash / 条件边 / 免审通知节点 / END 汇聚 / is_current 唯一 / 角色存在」，
-- 任一不符 → 迁移失败 → 启动失败（宁可起不来，也不要种出一张会被引擎跳过的图）。
--
-- ── 幂等与安全 ────────────────────────────────────────
-- · 加列 `IF NOT EXISTS`；约束先 `DROP ... IF EXISTS` 再建；索引 `IF NOT EXISTS`。
-- · 角色/菜单 `ON CONFLICT DO NOTHING`；流程四层 `WHERE NOT EXISTS` 守卫（已有同名流程，
--   或已有同 biz_type 的其它默认流程 → 整条跳过，绝不覆盖客户配置）。
-- · 角色被客户「以另一个 UUID 预先建过」时，流程种子整体跳过（NOTICE 提示去流程定义里绑人），
--   宁可不种，也不种出一张 assignee 指向不存在角色的图。
-- · 回滚：`V0182__pflow3a_po_stockin_approval.down.sql`。
-- ======================================================

-- ---------- 1) 采购订单：提交审批的实例弱引用 ----------
ALTER TABLE biz_purchase_order
    ADD COLUMN IF NOT EXISTS approval_instance_id uuid;

COMMENT ON COLUMN biz_purchase_order.approval_instance_id IS
    '提交审批的实例 id（P-FLOW-3A）：SUBMITTED（审批中）必有值；驳回回 DRAFT 时清空；APPROVED 后保留作审计';

CREATE INDEX IF NOT EXISTS idx_biz_po_approval
    ON biz_purchase_order (approval_instance_id)
    WHERE approval_instance_id IS NOT NULL;

-- ---------- 2) 收货单：放开 PENDING_POST ----------
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_status;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_status
    CHECK (status IN ('DRAFT', 'POSTED', 'PENDING_CANCEL', 'PENDING_POST', 'CANCELLED'));

ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_pending_cancel;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_pending_cancel
    CHECK (
        deleted_at IS NOT NULL
        OR (status IN ('PENDING_CANCEL', 'PENDING_POST') AND approval_instance_id IS NOT NULL)
        OR (status NOT IN ('PENDING_CANCEL', 'PENDING_POST') AND approval_instance_id IS NULL)
    );

-- ---------- 3) 角色种子（审批人来源 = ROLE） ----------
INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, remark, created_at, updated_at)
VALUES
    ('20000000-0000-4000-8000-0000000f0001'::uuid, 'DEPT_LEADER', '部门主管',
     '采购订单审批·第一档（< 1 万）：本部门的采购复核人', 1, 10,
     'P-FLOW-3A 预置审批角色：把用户绑到这个角色上即可参与采购审批，无需改流程', now(), now()),
    ('20000000-0000-4000-8000-0000000f0002'::uuid, 'PURCHASE_MANAGER', '采购经理',
     '采购订单审批·第二档（1–10 万）与第三档会签（≥ 10 万）', 1, 11,
     'P-FLOW-3A 预置审批角色', now(), now()),
    ('20000000-0000-4000-8000-0000000f0003'::uuid, 'GENERAL_MANAGER', '总经理',
     '采购订单审批·第三档会签（≥ 10 万）：与采购经理共同签字', 1, 12,
     'P-FLOW-3A 预置审批角色', now(), now()),
    ('20000000-0000-4000-8000-0000000f0004'::uuid, 'WAREHOUSE_MANAGER', '仓管主管',
     '收货过账审批（≥ 1000）+ 小额免审知会', 1, 13,
     'P-FLOW-3A 预置审批角色', now(), now())
ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 4) 流程定义：采购订单审批 po_approve ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000a0182', 'po_approve', '采购订单审批（标准·金额分级）',
    'PO_APPROVE', '采购管理',
    '采购订单提交审批：按含税金额三档分流（< 1 万部门主管 / 1–10 万采购经理 / '
    '≥ 10 万采购经理+总经理会签）。审批通过才变 APPROVED（此后才正式收货），'
    '驳回或撤回则打回草稿并把意见追加到备注。阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000a0183', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'po_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'PO_APPROVE' AND is_default AND deleted_at IS NULL
)
-- 角色必须是我们种的那一套（客户用别的 UUID 预建过 → 整条跳过，别种出悬空的 assignee）
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0001'::uuid,
                  '20000000-0000-4000-8000-0000000f0002'::uuid,
                  '20000000-0000-4000-8000-0000000f0003'::uuid)
       AND deleted_at IS NULL
);

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000a0183', '20000000-0000-4000-8000-0000000a0182', 1, 'PUBLISHED', TRUE,
    '8f65819ea25e1145d441d9b947b89b21bd4bfdc5075ac321367ea106a06366de',
    $snapshot${"edges":[{"condition":null,"from":"c_low","is_default":true,"key":"e_low_else","label":"金额 ≥ 10000","priority":10,"to":"c_mid"},{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":10000},"from":"c_low","is_default":false,"key":"e_low","label":"金额 < 10000","priority":0,"to":"n_dept"},{"condition":null,"from":"c_mid","is_default":true,"key":"e_mid_else","label":"金额 ≥ 100000","priority":10,"to":"n_board"},{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":100000},"from":"c_mid","is_default":false,"key":"e_mid","label":"10000 ≤ 金额 < 100000","priority":0,"to":"n_pm"},{"condition":null,"from":"n_board","is_default":false,"key":"e_board_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_dept","is_default":false,"key":"e_dept_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_pm","is_default":false,"key":"e_pm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"c_low"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_low","max_remind_times":3,"name":"金额分级①（< 1 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":200,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_mid","max_remind_times":3,"name":"金额分级②（< 10 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":400,"y":140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":7,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":860,"y":80},{"allow_robot":false,"approval_mode":"ALL","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0002","sort_order":0},{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":1}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_board","max_remind_times":2,"name":"采购经理+总经理会签","node_type":"COUNTERSIGN","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":620,"y":280},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0001","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_dept","max_remind_times":2,"name":"部门主管审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":400,"y":-160},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0002","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_pm","max_remind_times":2,"name":"采购经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":620,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    7, 8, 'V0182 种子：采购订单审批（金额三档分流）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000a0182')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000a0190'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'c_low'::varchar, '金额分级①（< 1 万）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 200::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000a0191'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'c_mid'::varchar, '金额分级②（< 10 万）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 400::int, 140::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000a0192'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 860::int, 80::int, 7::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000a0193'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'n_board'::varchar, '采购经理+总经理会签'::varchar, 'COUNTERSIGN'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ALL'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 620::int, 280::int, 6::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000a0194'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'n_dept'::varchar, '部门主管审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 400::int, -160::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000a0195'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'n_pm'::varchar, '采购经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 620::int, 0::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000a0196'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000a0183')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000a0200'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'e_low_else'::varchar, '20000000-0000-4000-8000-0000000a0190'::uuid, '20000000-0000-4000-8000-0000000a0191'::uuid, NULL::jsonb, '金额 ≥ 10000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000a0201'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'e_low'::varchar, '20000000-0000-4000-8000-0000000a0190'::uuid, '20000000-0000-4000-8000-0000000a0194'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10000}'::jsonb, '金额 < 10000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000a0202'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'e_mid_else'::varchar, '20000000-0000-4000-8000-0000000a0191'::uuid, '20000000-0000-4000-8000-0000000a0193'::uuid, NULL::jsonb, '金额 ≥ 100000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000a0203'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'e_mid'::varchar, '20000000-0000-4000-8000-0000000a0191'::uuid, '20000000-0000-4000-8000-0000000a0195'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 100000}'::jsonb, '10000 ≤ 金额 < 100000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000a0204'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'e_board_end'::varchar, '20000000-0000-4000-8000-0000000a0193'::uuid, '20000000-0000-4000-8000-0000000a0192'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000a0205'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'e_dept_end'::varchar, '20000000-0000-4000-8000-0000000a0194'::uuid, '20000000-0000-4000-8000-0000000a0192'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000a0206'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'e_pm_end'::varchar, '20000000-0000-4000-8000-0000000a0195'::uuid, '20000000-0000-4000-8000-0000000a0192'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000a0207'::uuid, '20000000-0000-4000-8000-0000000a0183'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000a0196'::uuid, '20000000-0000-4000-8000-0000000a0190'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000a0183')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 审批人：全部 ROLE（客户只绑人，不改流程）
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000a0194'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0001'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000a0195'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0002'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000a0193'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0002'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000a0193'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 1::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000a0183')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- 5) 流程定义：收货过账审批 stock_in_post ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000b0182', 'stock_in_post', '收货过账审批（标准·金额分级）',
    'STOCK_IN_POST', '仓库管理',
    '收货单提交过账：含税金额 < 1000 免审直接过账（并发通知知会仓管主管），'
    '≥ 1000 走仓管主管审批；审批通过才执行过账（库存 / 库位账 / 流水 / 序列号 / 凭证 / 应付 '
    '六本账同一事务落库），驳回或撤回则打回草稿，账一分没动。阈值与审批人都在本流程定义里改。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000b0183', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'stock_in_post' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'STOCK_IN_POST' AND is_default AND deleted_at IS NULL
)
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0004'::uuid AND deleted_at IS NULL
);

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000b0183', '20000000-0000-4000-8000-0000000b0182', 1, 'PUBLISHED', TRUE,
    '035d3c246b89330681f67c353277dc53313c7a6521041e45841bd078b2563b3d',
    $snapshot${"edges":[{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":1000},"from":"c_small","is_default":false,"key":"e_small","label":"金额 < 1000（免审）","priority":0,"to":"n_notify"},{"condition":null,"from":"c_small","is_default":true,"key":"e_small_else","label":"金额 ≥ 1000","priority":10,"to":"n_wh"},{"condition":null,"from":"n_notify","is_default":false,"key":"e_notify_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_wh","is_default":false,"key":"e_wh_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"c_small"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_small","max_remind_times":3,"name":"金额分级（< 1000 免审）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":700,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{"notify":{"inline":{"content":"单号 {{instance.biz_no}} 的含税金额未达审批阈值，已按免审规则直接过账，请知悉。","title":"免审知会：{{instance.title}}"},"targets":[{"role_codes":["WAREHOUSE_MANAGER"],"type":"ROLE"}],"when":"ON_ENTER"}},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_notify","max_remind_times":3,"name":"通知仓管主管（免审知会）","node_type":"NOTIFY_NODE","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":460,"y":-140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0004","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_wh","max_remind_times":2,"name":"仓管主管审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":460,"y":140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    5, 5, 'V0182 种子：收货过账审批（小额免审 + 仓管主管审批）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000b0182')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000b0190'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 'c_small'::varchar, '金额分级（< 1000 免审）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000b0191'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 700::int, 0::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000b0192'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 'n_notify'::varchar, '通知仓管主管（免审知会）'::varchar, 'NOTIFY_NODE'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 460::int, -140::int, 3::int, '{"notify": {"when": "ON_ENTER", "targets": [{"type": "ROLE", "role_codes": ["WAREHOUSE_MANAGER"]}], "inline": {"title": "免审知会：{{instance.title}}", "content": "单号 {{instance.biz_no}} 的含税金额未达审批阈值，已按免审规则直接过账，请知悉。"}}}'::jsonb),
    ('20000000-0000-4000-8000-0000000b0193'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 'n_wh'::varchar, '仓管主管审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 460::int, 140::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000b0194'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000b0183')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000b0200'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 'e_small'::varchar, '20000000-0000-4000-8000-0000000b0190'::uuid, '20000000-0000-4000-8000-0000000b0192'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 1000}'::jsonb, '金额 < 1000（免审）'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000b0201'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 'e_small_else'::varchar, '20000000-0000-4000-8000-0000000b0190'::uuid, '20000000-0000-4000-8000-0000000b0193'::uuid, NULL::jsonb, '金额 ≥ 1000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000b0202'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 'e_notify_end'::varchar, '20000000-0000-4000-8000-0000000b0192'::uuid, '20000000-0000-4000-8000-0000000b0191'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000b0203'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 'e_wh_end'::varchar, '20000000-0000-4000-8000-0000000b0193'::uuid, '20000000-0000-4000-8000-0000000b0191'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000b0204'::uuid, '20000000-0000-4000-8000-0000000b0183'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000b0194'::uuid, '20000000-0000-4000-8000-0000000b0190'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000b0183')
ON CONFLICT (version_id, edge_key) DO NOTHING;

INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000b0193'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0004'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000b0183')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- 6) 权限码（菜单按钮；唯一来源就是 sys_menu.auth_code） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000787'::uuid,
     'business:erp:purchase-order:submit-approval', 3,
     'business:erp:purchase-order:submit-approval', NULL,
     '20000000-0000-0000-0000-000000000953'::uuid, NULL,
     6, 1, NULL, '提交采购订单审批（发起 po_approve 流程）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000788'::uuid,
     'business:erp:stock-in:submit-post-approval', 3,
     'business:erp:stock-in:submit-post-approval', NULL,
     '20000000-0000-0000-0000-000000000998'::uuid, NULL,
     6, 1, NULL, '提交收货过账审批（发起 stock_in_post 流程）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000000787'::uuid),
        ('20000000-0000-0000-0000-000000000788'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- 7) 结构与种子自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    m_rows int;
    v_bad  int;
    n_rows int;
    e_rows int;
    a_rows int;
BEGIN
    -- 7.1 表结构（无论流程是否跳过都必须改到位）
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_purchase_order' AND column_name = 'approval_instance_id'
    ) THEN
        RAISE EXCEPTION 'V0182 自检失败：biz_purchase_order 缺 approval_instance_id';
    END IF;
    IF pg_get_constraintdef((SELECT oid FROM pg_constraint WHERE conname = 'ck_biz_si_status')) NOT LIKE '%PENDING_POST%' THEN
        RAISE EXCEPTION 'V0182 自检失败：ck_biz_si_status 没有放开 PENDING_POST';
    END IF;
    IF pg_get_constraintdef((SELECT oid FROM pg_constraint WHERE conname = 'ck_biz_si_pending_cancel')) NOT LIKE '%PENDING_POST%' THEN
        RAISE EXCEPTION 'V0182 自检失败：ck_biz_si_pending_cancel 没有覆盖 PENDING_POST';
    END IF;
    IF EXISTS (SELECT 1 FROM sys_menu WHERE auth_code = 'business:erp:purchase-order:submit-approval' AND deleted_at IS NULL) = FALSE THEN
        RAISE EXCEPTION 'V0182 自检失败：权限码 business:erp:purchase-order:submit-approval 未种';
    END IF;
    IF EXISTS (SELECT 1 FROM sys_menu WHERE auth_code = 'business:erp:stock-in:submit-post-approval' AND deleted_at IS NULL) = FALSE THEN
        RAISE EXCEPTION 'V0182 自检失败：权限码 business:erp:stock-in:submit-post-approval 未种';
    END IF;

    -- 7.2 采购审批流程（客户已有同名流程 / 角色被别的 UUID 占过 → 守卫整条跳过）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000a0182') THEN
        RAISE NOTICE 'V0182：po_approve 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    ELSE
        SELECT count(*) INTO m_rows FROM biz_flow_def d
         WHERE d.id = '20000000-0000-4000-8000-0000000a0182' AND d.flow_code = 'po_approve'
           AND d.biz_type = 'PO_APPROVE' AND d.status = 'PUBLISHED' AND d.is_default
           AND d.current_version_id = '20000000-0000-4000-8000-0000000a0183'
           AND d.deleted_at IS NULL;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：po_approve 定义行不满足 PUBLISHED/默认/当前版本';
        END IF;

        SELECT count(*) INTO v_bad FROM biz_flow_def_version v
         WHERE v.id = '20000000-0000-4000-8000-0000000a0183'
           AND v.def_id = '20000000-0000-4000-8000-0000000a0182'
           AND v.version_status = 'PUBLISHED' AND v.is_current
           AND v.graph_hash = '8f65819ea25e1145d441d9b947b89b21bd4bfdc5075ac321367ea106a06366de'
           AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
           AND v.node_count = 7 AND v.edge_count = 8;
        IF v_bad <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：po_approve 版本行（快照/hash/计数）不对';
        END IF;

        SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000a0183';
        SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000a0183';
        IF n_rows <> 7 OR e_rows <> 8 THEN
            RAISE EXCEPTION 'V0182 自检失败：po_approve 节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
        END IF;

        -- 金额分级必须真的落在条件边上（不是代码里 if/else）
        SELECT count(*) INTO m_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000a0183'
           AND NOT is_default AND condition_expr IS NOT NULL
           AND condition_expr->>'field' = 'form.amount';
        IF m_rows <> 2 THEN
            RAISE EXCEPTION 'V0182 自检失败：按 form.amount 分流的条件边应为 2 条，实际 %', m_rows;
        END IF;

        -- 三个审批节点都必须「有人可派 + 没人时回退超管」（不许静默自动通过）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000a0183'
           AND node_key IN ('n_dept', 'n_pm', 'n_board')
           AND assignee_type = 'ROLE' AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW';
        IF m_rows <> 3 THEN
            RAISE EXCEPTION 'V0182 自检失败：采购审批三个节点的审批人来源/兜底策略不对（应为 3 行，实际 %）', m_rows;
        END IF;

        -- 会签节点：ALL + 两个角色（采购经理 + 总经理）
        SELECT count(*) INTO a_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = '20000000-0000-4000-8000-0000000a0183' AND n.node_key = 'n_board';
        IF a_rows <> 2 THEN
            RAISE EXCEPTION 'V0182 自检失败：会签节点应有 2 个审批角色，实际 %', a_rows;
        END IF;
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000a0183' AND node_key = 'n_board'
           AND node_type = 'COUNTERSIGN' AND approval_mode = 'ALL';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：会签节点必须是 COUNTERSIGN + ALL';
        END IF;

        -- END 有三条入边 ⇒ 必须 OR 汇聚（AND 会等人齐 → 审批单永久卡死）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000a0183' AND node_key = 'e' AND join_mode = 'OR';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：po_approve 的 END 节点必须 OR 汇聚';
        END IF;
    END IF;

    -- 7.3 收货过账流程
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000b0182') THEN
        RAISE NOTICE 'V0182：stock_in_post 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    ELSE
        SELECT count(*) INTO m_rows FROM biz_flow_def d
         WHERE d.id = '20000000-0000-4000-8000-0000000b0182' AND d.flow_code = 'stock_in_post'
           AND d.biz_type = 'STOCK_IN_POST' AND d.status = 'PUBLISHED' AND d.is_default
           AND d.current_version_id = '20000000-0000-4000-8000-0000000b0183'
           AND d.deleted_at IS NULL;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：stock_in_post 定义行不满足 PUBLISHED/默认/当前版本';
        END IF;

        SELECT count(*) INTO v_bad FROM biz_flow_def_version v
         WHERE v.id = '20000000-0000-4000-8000-0000000b0183'
           AND v.def_id = '20000000-0000-4000-8000-0000000b0182'
           AND v.version_status = 'PUBLISHED' AND v.is_current
           AND v.graph_hash = '035d3c246b89330681f67c353277dc53313c7a6521041e45841bd078b2563b3d'
           AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
           AND v.node_count = 5 AND v.edge_count = 5;
        IF v_bad <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：stock_in_post 版本行（快照/hash/计数）不对';
        END IF;

        SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000b0183';
        SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000b0183';
        IF n_rows <> 5 OR e_rows <> 5 THEN
            RAISE EXCEPTION 'V0182 自检失败：stock_in_post 节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
        END IF;

        -- 小额免审：条件边 + 通知节点（NOTIFY_NODE 的 config.notify 必须成型）
        SELECT count(*) INTO m_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000b0183'
           AND NOT is_default AND condition_expr->>'field' = 'form.amount'
           AND (condition_expr->>'value')::numeric = 1000;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：1000 元免审阈值必须落在条件边上（实际 % 条）', m_rows;
        END IF;
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000b0183' AND node_key = 'n_notify'
           AND node_type = 'NOTIFY_NODE'
           AND config #>> '{notify,when}' = 'ON_ENTER'
           AND config #>> '{notify,targets,0,type}' = 'ROLE'
           AND config #>> '{notify,targets,0,role_codes,0}' = 'WAREHOUSE_MANAGER'
           AND config #> '{notify,inline,title}' IS NOT NULL;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：免审知会通知节点配置不成型（NOTIFY_NODE + ON_ENTER + ROLE WAREHOUSE_MANAGER + inline）';
        END IF;
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000b0183' AND node_key = 'n_wh'
           AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
           AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：仓管主管审批节点的来源/兜底策略不对';
        END IF;
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000b0183' AND node_key = 'e' AND join_mode = 'OR';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0182 自检失败：stock_in_post 的 END 节点必须 OR 汇聚';
        END IF;
    END IF;

    RAISE NOTICE 'V0182 自检通过：采购审批 + 收货过账审批两个标准流程已就绪';
END $$;
