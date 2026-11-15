-- ======================================================
-- V0281：销售订单审批流（销售链 SO-06）
--
--   ① `biz_sales_order` 加 `approval_instance_id`（提交审批的实例弱引用）+ 部分索引
--   ② 角色种子：`SALES_SUPERVISOR`（销售主管）；`FINANCE_MANAGER` / `GENERAL_MANAGER`
--      复用 V0191 / V0182 已种的（**不重复种**，只在缺失时补）
--   ③ 开箱即用的标准流程 `so_approve`（biz_type = `SO_APPROVE`）：
--        · 金额 < 1 万          ：销售主管
--        · 1 万 ≤ 金额 < 10 万  ：销售主管 → 财务经理
--        · 金额 ≥ 10 万         ：销售主管 → 财务经理 → 总经理
--      三档写成**流程定义里的条件边**（`form.amount`），不是代码里的 if/else ——
--      客户改阈值 / 换审批岗位，在流程定义里另存版本即可，不用改代码、不用发版。
--   ④ 审批人全部走 `biz_flow_node_assignee`（`assignee_type = 'ROLE'`，运行时展开角色成员）
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 写入本文件时 `backend/migrations/business/` 最大号 = V0280
-- （V0280 是另一张卡的库存/盘点菜单修正，已合入 main），本卡顺延取 **V0281**。
-- 卡面写的 `V0262__sales_order_flow` 早在客户档案菜单卡（V0262/V0263）就用掉了，
-- 而 V0264（销售订单两表）/ V0265（销售订单权限码）也已被 SO-04 / SO-05 占用 ——
-- 已应用迁移不可改、指纹校验会炸，**只能顺延**。
--
-- ── 为什么 flow_code 是小写 `so_approve`，而卡面写 `SO_APPROVE` ──
-- `biz_flow_def` 的 `ck_biz_flow_def_code` 要求 `^[a-z][a-z0-9_]{1,63}$`（首字符小写，
-- 全大写写不进去）。本仓既有口径就是「`biz_type` 大写 + `flow_code` 小写」：
-- `PO_APPROVE`/`po_approve`、`STOCK_IN_POST`/`stock_in_post` …… 本卡沿用：
--   · `biz_type  = 'SO_APPROVE'`（卡面的编码，业务类型）
--   · `flow_code = 'so_approve'`（流程编码，用户名可见）
-- 两处都在 `sales_approval_bridge::{BIZ_TYPE, FLOW_CODE}` 里钉死，单测逐字对拍。
--
-- ── 状态口径 ────────────────────────────────────────────
--   DRAFT ──提交审批──▶ SUBMITTED ──审批通过──▶ APPROVED（此后才能出库）
--                              └──驳回 / 撤回──▶ DRAFT（清实例与提交痕迹，意见追加 remark）
--                              └──审批中作废──▶ CANCELLED（同一事务撤审批单）
-- `SUBMITTED → DRAFT` 这条边由本卡**新增**到 `state_machine::SALES_ORDER_EDGES`
-- （10 → 11 条），只有审批回调 `sales_approval_bridge::on_so_settled` 会写它；
-- 人工 `PATCH /status` 推 `APPROVED` / `DRAFT` 一律 400（指向审批中心）。
--
-- ── 库侧硬约束 ──────────────────────────────────────────
-- `ck_si_...` 那一套不适用（销售订单没有「审批中」状态值，`SUBMITTED` 本身就是审批中）；
-- 单据与实例的对应靠**代码 + 部分索引**：`approval_instance_id` 在 `SUBMITTED` 时由
-- `sales_order_handler::start_approval` 同一事务写入，回草稿时清空。
-- 唯一性由 `flow::biz_instance_running(BIZ_TYPE, so_id)` + `uk_biz_flow_instance_biz_active`
-- 双保险（同一张单不允许两个在跑的审批）。
--
-- ── 快照与 hash（不是手写的）───────────────────────────
-- 图按 `flow::graph::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
-- 审批人按 (kind,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--   so_approve : da3c2eeda2593b0e3a0b362dd31ec2c79505ba3efe0053028896d05e5b2fa9a2
-- 末尾 DO 块自检「条数 / hash / 条件边 / 审批人 / END 汇聚 / is_current 唯一 / 角色存在」，
-- 任一不符 → 迁移失败 → 启动失败（宁可起不来，也不要种出一张会被引擎跳过的图）。
--
-- ── 幂等与安全 ────────────────────────────────────────
-- · 加列 / 索引 `IF NOT EXISTS`；角色 `ON CONFLICT (role_code) WHERE deleted_at IS NULL`；
-- · 流程四层 `WHERE NOT EXISTS` 守卫：已有 `flow_code='so_approve'`，或已有
--   同 `biz_type='SO_APPROVE'` 的默认流程 → 整条跳过（**绝不覆盖客户配置**）；
-- · 角色被客户「以另一个 UUID 预先建过」时，流程种子整体跳过（NOTICE 提示去流程定义里绑人），
--   宁可不种，也不种出一张 assignee 指向不存在角色的图；
-- · 回滚：`V0281__sales_order_flow.down.sql`。
-- ======================================================

-- ---------- ① 销售订单：审批实例弱引用 ----------
ALTER TABLE biz_sales_order
    ADD COLUMN IF NOT EXISTS approval_instance_id uuid;

COMMENT ON COLUMN biz_sales_order.approval_instance_id IS
    '提交审批的实例 id（SO-06）：SUBMITTED（审批中）必有值；驳回 / 撤回回 DRAFT 时清空；APPROVED 后保留作审计';

CREATE INDEX IF NOT EXISTS idx_biz_sales_order_approval
    ON biz_sales_order (approval_instance_id)
    WHERE approval_instance_id IS NOT NULL;

-- ---------- ② 角色种子（审批人来源 = ROLE） ----------
-- 销售主管是本卡新增岗位；财务经理 / 总经理复用既有种子（V0191 / V0182），缺失才补。
INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, remark, created_at, updated_at)
VALUES
    ('20000000-0000-4000-8000-0000000f0007'::uuid, 'SALES_SUPERVISOR', '销售主管',
     '销售订单审批·第一档（< 1 万）：销售链的第一复核人（三档都要过他）', 1, 14,
     'SO-06 预置审批角色：把用户绑到这个角色上即可参与销售订单审批，无需改流程', now(), now()),
    ('20000000-0000-4000-8000-0000000f0005'::uuid, 'FINANCE_MANAGER', '财务经理',
     '销售订单审批·第二档（1–10 万）：大额销售单的财务复核（V0191 已种，此处仅缺失时补）', 1, 11,
     'V0191 预置审批角色（SO-06 复用）', now(), now()),
    ('20000000-0000-4000-8000-0000000f0003'::uuid, 'GENERAL_MANAGER', '总经理',
     '销售订单审批·第三档（≥ 10 万）：特大额销售单的终审（V0182 已种，此处仅缺失时补）', 1, 12,
     'V0182 预置审批角色（SO-06 复用）', now(), now())
ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ③ 流程定义：销售订单审批 so_approve ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000d0281', 'so_approve', '销售订单审批（标准·金额分级）',
    'SO_APPROVE', '销售管理',
    '销售订单提交审批：按价税合计三档串行复核（< 1 万销售主管 / 1–10 万销售主管→财务经理 / '
    '≥ 10 万销售主管→财务经理→总经理）。审批通过才变 APPROVED（此后才能出库），'
    '驳回或撤回则打回草稿并把意见追加到备注。阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000d0282', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'so_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'SO_APPROVE' AND is_default AND deleted_at IS NULL
)
-- 角色必须是我们种的那一套（客户用别的 UUID 预建过 → 整条跳过，别种出悬空的 assignee）
AND (
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0007'::uuid,
                  '20000000-0000-4000-8000-0000000f0005'::uuid,
                  '20000000-0000-4000-8000-0000000f0003'::uuid)
       AND deleted_at IS NULL
) = 3;

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000d0282', '20000000-0000-4000-8000-0000000d0281', 1,
    'PUBLISHED', TRUE,
    'da3c2eeda2593b0e3a0b362dd31ec2c79505ba3efe0053028896d05e5b2fa9a2',
    $snapshot${"edges":[{"condition":null,"from":"n_finance","is_default":false,"key":"e_finance_mid","label":null,"priority":0,"to":"c_mid"},{"condition":null,"from":"n_gm","is_default":false,"key":"e_gm_end","label":null,"priority":0,"to":"e"},{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":10000},"from":"c_low","is_default":false,"key":"e_low","label":"金额 < 10000","priority":0,"to":"e"},{"condition":null,"from":"c_low","is_default":true,"key":"e_low_else","label":"金额 ≥ 10000","priority":10,"to":"n_finance"},{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":100000},"from":"c_mid","is_default":false,"key":"e_mid","label":"10000 ≤ 金额 < 100000","priority":0,"to":"e"},{"condition":null,"from":"c_mid","is_default":true,"key":"e_mid_else","label":"金额 ≥ 100000","priority":10,"to":"n_gm"},{"condition":null,"from":"n_sales","is_default":false,"key":"e_sales_low","label":null,"priority":0,"to":"c_low"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_sales"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_low","max_remind_times":3,"name":"金额分级①（< 1 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":420,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_mid","max_remind_times":3,"name":"金额分级②（< 10 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":860,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":7,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":1300,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_finance","max_remind_times":2,"name":"财务经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":640,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm","max_remind_times":2,"name":"总经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":1080,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0007","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_sales","max_remind_times":2,"name":"销售主管审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    7, 8, 'V0281 种子：销售订单审批（金额三档串行复核）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0281')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0301'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0302'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'n_sales'::varchar, '销售主管审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0303'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'c_low'::varchar, '金额分级①（< 1 万）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 420::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0304'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'n_finance'::varchar, '财务经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 640::int, 0::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0305'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'c_mid'::varchar, '金额分级②（< 10 万）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 860::int, 0::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0306'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'n_gm'::varchar, '总经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 1080::int, 0::int, 6::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0307'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 1300::int, 0::int, 7::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0282')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0401'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d0301'::uuid, '20000000-0000-4000-8000-0000000d0302'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0402'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'e_sales_low'::varchar, '20000000-0000-4000-8000-0000000d0302'::uuid, '20000000-0000-4000-8000-0000000d0303'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0403'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'e_low'::varchar, '20000000-0000-4000-8000-0000000d0303'::uuid, '20000000-0000-4000-8000-0000000d0307'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10000}'::jsonb, '金额 < 10000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0404'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'e_low_else'::varchar, '20000000-0000-4000-8000-0000000d0303'::uuid, '20000000-0000-4000-8000-0000000d0304'::uuid, NULL::jsonb, '金额 ≥ 10000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000d0405'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'e_finance_mid'::varchar, '20000000-0000-4000-8000-0000000d0304'::uuid, '20000000-0000-4000-8000-0000000d0305'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0406'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'e_mid'::varchar, '20000000-0000-4000-8000-0000000d0305'::uuid, '20000000-0000-4000-8000-0000000d0307'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 100000}'::jsonb, '10000 ≤ 金额 < 100000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0407'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'e_mid_else'::varchar, '20000000-0000-4000-8000-0000000d0305'::uuid, '20000000-0000-4000-8000-0000000d0306'::uuid, NULL::jsonb, '金额 ≥ 100000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000d0408'::uuid, '20000000-0000-4000-8000-0000000d0282'::uuid, 'e_gm_end'::varchar, '20000000-0000-4000-8000-0000000d0306'::uuid, '20000000-0000-4000-8000-0000000d0307'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0282')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 审批人：全部 ROLE（客户只绑人，不改流程）
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0302'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0007'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d0304'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d0306'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0282')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ④ 结构与种子自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    m_rows int;
    v_bad  int;
    n_rows int;
    e_rows int;
    a_rows int;
BEGIN
    -- 4.1 表结构（无论流程是否跳过都必须改到位）
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_sales_order' AND column_name = 'approval_instance_id'
    ) THEN
        RAISE EXCEPTION 'V0281 自检失败：biz_sales_order 缺 approval_instance_id';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE schemaname = 'public' AND tablename = 'biz_sales_order'
           AND indexname = 'idx_biz_sales_order_approval'
    ) THEN
        RAISE EXCEPTION 'V0281 自检失败：索引 idx_biz_sales_order_approval 缺失';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'SALES_SUPERVISOR' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0281 自检失败：角色 SALES_SUPERVISOR 未种';
    END IF;

    -- 4.2 销售订单审批流程（客户已有同名流程 / 角色被别的 UUID 占过 → 守卫整条跳过）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0281') THEN
        RAISE NOTICE 'V0281：so_approve 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    ELSE
        SELECT count(*) INTO m_rows FROM biz_flow_def d
         WHERE d.id = '20000000-0000-4000-8000-0000000d0281' AND d.flow_code = 'so_approve'
           AND d.biz_type = 'SO_APPROVE' AND d.status = 'PUBLISHED' AND d.is_default
           AND d.current_version_id = '20000000-0000-4000-8000-0000000d0282'
           AND d.deleted_at IS NULL;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0281 自检失败：so_approve 定义行不满足 PUBLISHED/默认/当前版本';
        END IF;

        SELECT count(*) INTO v_bad FROM biz_flow_def_version v
         WHERE v.id = '20000000-0000-4000-8000-0000000d0282'
           AND v.def_id = '20000000-0000-4000-8000-0000000d0281'
           AND v.version_status = 'PUBLISHED' AND v.is_current
           AND v.graph_hash = 'da3c2eeda2593b0e3a0b362dd31ec2c79505ba3efe0053028896d05e5b2fa9a2'
           AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
           AND v.node_count = 7 AND v.edge_count = 8;
        IF v_bad <> 1 THEN
            RAISE EXCEPTION 'V0281 自检失败：so_approve 版本行（快照/hash/计数）不对';
        END IF;

        SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0282';
        SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0282';
        IF n_rows <> 7 OR e_rows <> 8 THEN
            RAISE EXCEPTION 'V0281 自检失败：so_approve 节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
        END IF;

        -- 金额分级必须真的落在条件边上（不是代码里 if/else），阈值逐字是 10000 / 100000
        SELECT count(*) INTO m_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000d0282'
           AND NOT is_default AND condition_expr IS NOT NULL
           AND condition_expr->>'field' = 'form.amount'
           AND (condition_expr->>'value')::numeric IN (10000, 100000);
        IF m_rows <> 2 THEN
            RAISE EXCEPTION 'V0281 自检失败：按 form.amount 分流的条件边应为 2 条（10000 / 100000），实际 %', m_rows;
        END IF;
        -- 两条 else 边必须 is_default（否则金额 ≥ 阈值时无路可走 → 实例卡死）
        SELECT count(*) INTO m_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000d0282' AND is_default AND priority = 10;
        IF m_rows <> 2 THEN
            RAISE EXCEPTION 'V0281 自检失败：else 边（is_default + priority=10）应为 2 条，实际 %', m_rows;
        END IF;

        -- 三个审批节点都必须「有人可派 + 没人时回退超管」（不许静默自动通过）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000d0282'
           AND node_key IN ('n_sales', 'n_finance', 'n_gm')
           AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
           AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW';
        IF m_rows <> 3 THEN
            RAISE EXCEPTION 'V0281 自检失败：三个审批节点的审批人来源/兜底策略不对（应为 3 行，实际 %）', m_rows;
        END IF;

        -- 审批人必须逐节点绑到对应角色（销售主管 / 财务经理 / 总经理）
        SELECT count(*) INTO a_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = '20000000-0000-4000-8000-0000000d0282'
           AND a.assignee_type = 'ROLE';
        IF a_rows <> 3 THEN
            RAISE EXCEPTION 'V0281 自检失败：审批人应有 3 行（每节点一个角色），实际 %', a_rows;
        END IF;
        SELECT count(*) INTO m_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = '20000000-0000-4000-8000-0000000d0282'
           AND ((n.node_key = 'n_sales'   AND a.ref_id = '20000000-0000-4000-8000-0000000f0007'::uuid)
             OR (n.node_key = 'n_finance' AND a.ref_id = '20000000-0000-4000-8000-0000000f0005'::uuid)
             OR (n.node_key = 'n_gm'      AND a.ref_id = '20000000-0000-4000-8000-0000000f0003'::uuid));
        IF m_rows <> 3 THEN
            RAISE EXCEPTION 'V0281 自检失败：审批人角色绑定与三档岗位不符（应 3 行，实际 %）', m_rows;
        END IF;

        -- END 有三条入边 ⇒ 必须 OR 汇聚（AND 会等人齐 → 审批单永久卡死）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000d0282' AND node_key = 'e' AND join_mode = 'OR';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0281 自检失败：so_approve 的 END 节点必须 OR 汇聚';
        END IF;
        -- 串行链：销售主管必须是**第一个**审批节点（START 的直接下游，三档都要过他）
        SELECT count(*) INTO m_rows FROM biz_flow_edge e
          JOIN biz_flow_node f ON f.id = e.from_node_id
          JOIN biz_flow_node t ON t.id = e.to_node_id
         WHERE e.version_id = '20000000-0000-4000-8000-0000000d0282'
           AND f.node_key = 's' AND t.node_key = 'n_sales';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0281 自检失败：START 必须直连销售主管审批节点';
        END IF;
    END IF;

    RAISE NOTICE 'V0281 自检通过：销售订单审批（三档金额分流）标准流程已就绪';
END $$;
