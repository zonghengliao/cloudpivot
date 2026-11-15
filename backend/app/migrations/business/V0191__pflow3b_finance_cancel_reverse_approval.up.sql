-- ======================================================
-- V0191：财务链标准审批流（P-FLOW-3B）
--        ① `biz_payable` 加 `cancel_instance_id` / `cancel_reason`，放开 `PENDING_CANCEL`
--        ② `biz_voucher` 加 `pending_reverse_instance_id` / `reverse_reason`（红冲审批中标记）
--        ③ 角色种子：FINANCE_MANAGER / FINANCE_DIRECTOR
--        ④ 两个开箱即用的标准流程：
--             · `payable_cancel`  应付单作废审批（财务经理复核，无金额分级）
--             · `voucher_reverse` 凭证红冲审批（≤ 1 万财务经理；> 1 万财务经理+财务总监会签）
--        ⑤ 四个按钮权限码（+ 授给超管）
--
-- ── 为什么要这条迁移 ────────────────────────────────────
-- 应付作废与凭证红冲都是「改账」动作：一个让欠款消失，一个用红字冲掉已入账的凭证。
-- 内控上都必须有第二个人复核（制单人 ≠ 复核人）。本迁移把两条复核链写成**流程定义**，
-- 阈值（1 万）落在条件边上、审批人落在 ROLE 上 —— 客户要改「5 万以上才会签」，
-- 在「流程定义」里另存版本即可，不用改代码、不发版（与 V0182 同一套「模板 ≠ 写死」）。
--
-- ── 为什么凭证不用 `PENDING_REVERSE` 状态（本卡的关键取舍）──
-- 卡面自检 L1 写「凭证补 PENDING_REVERSE」，但卡面 §二 的「改设计」与硬红线 3 又明确：
-- 红冲的语义是**生成红字凭证、原凭证保持 POSTED**，状态机表达不了，用 pending 字段。
-- 落地取后者，依据是**报表口径**（不是口味问题）：
--   `ledger.rs` / `report.rs` / `income_statement.rs` / `cash_flow.rs` 的取数全部是
--   `WHERE v.status = 'POSTED' AND v.deleted_at IS NULL`（四大报表 + 总账 + 余额表）。
--   原凭证一旦改成 `PENDING_REVERSE`，它会在**所有报表里瞬间消失**（少记一笔已入账业务），
--   而审计要求是「已入账凭证不因审批流程而改变账务存在性」。
--   所以：凭证 `status` 恒为 `POSTED`，另用 `pending_reverse_instance_id`
--   （有值 = 正在审批红冲）+ `reverse_reason` 表达「审批中」，
--   审批通过后清空 pending、生成红字、回写 `reversed_by`；驳回只清 pending。
--   库侧另有 `ck_biz_voucher_pending_reverse` 保证「pending 只可能挂在已记账且未红冲的凭证上」。
--
-- ── 应付作废的状态机（真实流转，4 → 7 条边）────────────
--   OPEN ──发起作废审批──▶ PENDING_CANCEL ──审批通过──▶ CANCELLED
--                                    └──驳回 / 撤回──▶ OPEN
--   **PARTIAL_PAID / SETTLED 一律不许作废**（卡面决策）：作废一张已付过钱的应付单
--   必须同时冲回已付款，那是「付款冲销 + 红冲凭证」的活，不是作废单据能表达的。
--   所以边表里没有 `PARTIAL_PAID → PENDING_CANCEL`，入口处也再核一次 paid_amount = 0。
--
-- ── 审批人用 ROLE + 兜底超管（与 V0182 同口径）──────────
--   `empty_assignee = FALLBACK_ADMIN`：角色里 0 人时回退平台超管，**不是静默跳过**
--   （SKIP 会让「没人可派」变成「审批自动通过」，比卡住更坏）。
--   `self_approval = ALLOW`：FORBID 在**发起时**就 422（整单起不来），
--   而内控要的是「提交成功、但制单人这一票点不动」→ 待办照常可见留痕，
--   真正的否决在审批入口（`payable_bridge::guard_self_approval` /
--   `voucher_bridge::guard_self_approval`，403 + 65019），
--   并在**生成待办前**就把制单人/发起人排除（`flow::internal_control`，落 SKIPPED 留痕）。
--
-- ── 库侧硬约束（不是靠代码自觉）────────────────────────
-- · `ck_biz_payable_status` 加 `PENDING_CANCEL`；
-- · `ck_biz_payable_pending_cancel`：审批中 ⟺ 有实例；其它状态 ⟹ 没有实例；软删豁免。
--   前半句挡「审批中却没挂实例（永远等不到回调）」，后半句挡「已终态还挂着实例」
--   （会撞 `uk_biz_flow_instance_biz_active`，下一次发起永久 409 且无法自愈）。
-- · `ck_biz_voucher_pending_reverse`：pending 只许挂在 `POSTED` 且 `is_red = 0` 且
--   `reversed_by IS NULL` 的凭证上（已红冲的不能同时又「审批中」）。
--
-- ── 快照与 hash（不是手写的）───────────────────────────
-- 图按 `graph.rs::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
-- 审批人按 (kind,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--   payable_cancel  : 4f484777debbea1a0cc1a76db28274c4d9986c8cbe8d3936afa7e08a50adf7fc
--   voucher_reverse : 742f06d8fbecc6eb2fdfdfaed66571bfff9c1b3b02ff8dd0338f0dc7f3dd17b2
-- 末尾 DO 块自检「列 / 约束 / 角色 / 条数 / hash / 条件边 / 会签 / END 汇聚 / 权限码」，
-- 任一不符 → 迁移失败 → 启动失败（宁可起不来，也不要种出一张会被引擎跳过的图）。
--
-- ── 幂等与安全 ────────────────────────────────────────
-- · 加列 `IF NOT EXISTS`；约束先 `DROP ... IF EXISTS` 再建；索引 `IF NOT EXISTS`；
-- · 角色/菜单 `ON CONFLICT DO NOTHING`；流程四层 `WHERE NOT EXISTS` 守卫
--   （已有同名流程 / 已有同 biz_type 的默认流程 → 整条跳过，绝不覆盖客户配置）；
-- · 角色被客户以另一个 UUID 预建过 → 流程种子整体跳过（NOTICE 提示去流程定义里绑人）。
-- · 回滚：`V0191__pflow3b_finance_cancel_reverse_approval.down.sql`。
-- ======================================================

-- ---------- 1) 应付单：作废审批字段 + PENDING_CANCEL ----------
ALTER TABLE biz_payable
    ADD COLUMN IF NOT EXISTS cancel_instance_id uuid,
    ADD COLUMN IF NOT EXISTS cancel_reason text;

COMMENT ON COLUMN biz_payable.cancel_instance_id IS
    '作废审批的实例 id（P-FLOW-3B）：PENDING_CANCEL（审批中）必有值；通过/驳回后清空（实例留 biz_id 作审计）';
COMMENT ON COLUMN biz_payable.cancel_reason IS
    '作废原因（P-FLOW-3B）：提交审批时填写，审批通过后写进 remark 与状态日志';

CREATE INDEX IF NOT EXISTS idx_biz_payable_cancel_instance
    ON biz_payable (cancel_instance_id)
    WHERE cancel_instance_id IS NOT NULL;

ALTER TABLE biz_payable DROP CONSTRAINT IF EXISTS ck_biz_payable_status;
ALTER TABLE biz_payable
    ADD CONSTRAINT ck_biz_payable_status
    CHECK (status IN ('OPEN', 'PARTIAL_PAID', 'SETTLED', 'CANCELLED', 'PENDING_CANCEL'));

ALTER TABLE biz_payable DROP CONSTRAINT IF EXISTS ck_biz_payable_pending_cancel;
ALTER TABLE biz_payable
    ADD CONSTRAINT ck_biz_payable_pending_cancel
    CHECK (
        deleted_at IS NOT NULL
        OR (status = 'PENDING_CANCEL' AND cancel_instance_id IS NOT NULL)
        OR (status <> 'PENDING_CANCEL' AND cancel_instance_id IS NULL)
    );

-- ---------- 2) 凭证：红冲审批字段（状态不动） ----------
ALTER TABLE biz_voucher
    ADD COLUMN IF NOT EXISTS pending_reverse_instance_id uuid,
    ADD COLUMN IF NOT EXISTS reverse_reason text;

COMMENT ON COLUMN biz_voucher.pending_reverse_instance_id IS
    '红冲审批的实例 id（P-FLOW-3B）：有值 = 正在审批红冲（status 仍是 POSTED，报表口径不变）；'
    '审批通过后清空 + 生成红字，驳回只清空';
COMMENT ON COLUMN biz_voucher.reverse_reason IS
    '红冲原因（P-FLOW-3B）：提交审批时填写，审批通过后成为红字凭证的分录摘要';

CREATE INDEX IF NOT EXISTS idx_biz_voucher_pending_reverse
    ON biz_voucher (pending_reverse_instance_id)
    WHERE pending_reverse_instance_id IS NOT NULL;

ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_pending_reverse;
ALTER TABLE biz_voucher
    ADD CONSTRAINT ck_biz_voucher_pending_reverse
    CHECK (
        deleted_at IS NOT NULL
        OR pending_reverse_instance_id IS NULL
        OR (status = 'POSTED' AND is_red = 0 AND reversed_by IS NULL)
    );

-- ---------- 3) 角色种子（审批人来源 = ROLE） ----------
INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, remark, created_at, updated_at)
VALUES
    ('20000000-0000-4000-8000-0000000f0005'::uuid, 'FINANCE_MANAGER', '财务经理',
     '财务管理：应付作废复核 + 凭证红冲复核（含 ≤ 1 万单人会签档）', 1, 20,
     'P-FLOW-3B 预置审批角色：把用户绑到这个角色上即可参与财务复核，无需改流程', now(), now()),
    ('20000000-0000-4000-8000-0000000f0006'::uuid, 'FINANCE_DIRECTOR', '财务总监',
     '财务管理：凭证红冲 > 1 万会签（与财务经理共同签字）', 1, 21,
     'P-FLOW-3B 预置审批角色', now(), now())
ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 4) 流程定义：应付单作废审批 payable_cancel ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000c0182', 'payable_cancel', '应付单作废审批（标准）',
    'PAYABLE_CANCEL', '财务管理',
    '应付单作废必须先经财务经理复核：提交后单据进入 PENDING_CANCEL（不记账、不可改），'
    '审批通过才真正作废（欠款消失、进项凭证红冲），驳回或撤回则回到 OPEN。'
    '只有「未付且一分钱都没付」（OPEN + 已付 0）的应付单能提交作废；'
    '部分已付 / 已结清的单据请走付款冲销，不要作废单据。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000c0183', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'payable_cancel' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'PAYABLE_CANCEL' AND is_default AND deleted_at IS NULL
)
-- 角色必须是我们种的那一套（客户用别的 UUID 预建过 → 整条跳过，别种出悬空的 assignee）
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0005'::uuid AND deleted_at IS NULL
);

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000c0183', '20000000-0000-4000-8000-0000000c0182', 1, 'PUBLISHED', TRUE,
    '4f484777debbea1a0cc1a76db28274c4d9986c8cbe8d3936afa7e08a50adf7fc',
    $snapshot${"edges":[{"condition":null,"from":"n_fm","is_default":false,"key":"e_fm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_fm"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":560,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fm","max_remind_times":2,"name":"财务经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":280,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    3, 2, 'V0191 种子：应付单作废审批（财务经理单人复核）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0182')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0190'::uuid, '20000000-0000-4000-8000-0000000c0183'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 560::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0191'::uuid, '20000000-0000-4000-8000-0000000c0183'::uuid, 'n_fm'::varchar, '财务经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 280::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0192'::uuid, '20000000-0000-4000-8000-0000000c0183'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0183')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0200'::uuid, '20000000-0000-4000-8000-0000000c0183'::uuid, 'e_fm_end'::varchar, '20000000-0000-4000-8000-0000000c0191'::uuid, '20000000-0000-4000-8000-0000000c0190'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0201'::uuid, '20000000-0000-4000-8000-0000000c0183'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000c0192'::uuid, '20000000-0000-4000-8000-0000000c0191'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0183')
ON CONFLICT (version_id, edge_key) DO NOTHING;

INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0191'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c0183')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- 5) 流程定义：凭证红冲审批 voucher_reverse ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000d0182', 'voucher_reverse', '凭证红冲审批（标准）',
    'VOUCHER_REVERSE', '财务管理',
    '凭证红冲是按会计期间改账的动作，必须复核：单张凭证借贷合计 ≤ 1 万由财务经理审批，'
    '> 1 万走财务经理 + 财务总监会签。提交后凭证 status 仍为 POSTED（报表口径不变），'
    '只挂 pending_reverse_instance_id；审批通过才生成红字凭证并回写 reversed_by，'
    '驳回或撤回只清 pending、账一分不动。阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000d0183', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'voucher_reverse' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'VOUCHER_REVERSE' AND is_default AND deleted_at IS NULL
)
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0005'::uuid,
                  '20000000-0000-4000-8000-0000000f0006'::uuid)
       AND deleted_at IS NULL
);

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000d0183', '20000000-0000-4000-8000-0000000d0182', 1, 'PUBLISHED', TRUE,
    '742f06d8fbecc6eb2fdfdfaed66571bfff9c1b3b02ff8dd0338f0dc7f3dd17b2',
    $snapshot${"edges":[{"condition":{"cmp":"gt","field":"form.amount","op":"cmp","value":10000},"from":"c_amt","is_default":false,"key":"e_amt","label":"金额 > 10000（会签）","priority":0,"to":"n_cs"},{"condition":null,"from":"c_amt","is_default":true,"key":"e_amt_else","label":"金额 ≤ 10000","priority":10,"to":"n_fm"},{"condition":null,"from":"n_cs","is_default":false,"key":"e_cs_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_fm","is_default":false,"key":"e_fm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"c_amt"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_amt","max_remind_times":3,"name":"金额分级（> 1 万会签）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":760,"y":0},{"allow_robot":false,"approval_mode":"ALL","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0},{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0006","sort_order":1}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_cs","max_remind_times":2,"name":"财务经理+财务总监会签","node_type":"COUNTERSIGN","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":500,"y":-160},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fm","max_remind_times":2,"name":"财务经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":500,"y":140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    5, 5, 'V0191 种子：凭证红冲审批（≤ 1 万财务经理 / > 1 万双人会签）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0182')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0190'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 'c_amt'::varchar, '金额分级（> 1 万会签）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0191'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 760::int, 0::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0192'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 'n_cs'::varchar, '财务经理+财务总监会签'::varchar, 'COUNTERSIGN'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ALL'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 500::int, -160::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0193'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 'n_fm'::varchar, '财务经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 500::int, 140::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0194'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0183')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0200'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 'e_amt'::varchar, '20000000-0000-4000-8000-0000000d0190'::uuid, '20000000-0000-4000-8000-0000000d0192'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 10000}'::jsonb, '金额 > 10000（会签）'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0201'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 'e_amt_else'::varchar, '20000000-0000-4000-8000-0000000d0190'::uuid, '20000000-0000-4000-8000-0000000d0193'::uuid, NULL::jsonb, '金额 ≤ 10000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000d0202'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 'e_cs_end'::varchar, '20000000-0000-4000-8000-0000000d0192'::uuid, '20000000-0000-4000-8000-0000000d0191'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0203'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 'e_fm_end'::varchar, '20000000-0000-4000-8000-0000000d0193'::uuid, '20000000-0000-4000-8000-0000000d0191'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0204'::uuid, '20000000-0000-4000-8000-0000000d0183'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d0194'::uuid, '20000000-0000-4000-8000-0000000d0190'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0183')
ON CONFLICT (version_id, edge_key) DO NOTHING;

INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0193'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d0192'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d0192'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0006'::uuid, 1::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0183')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- 6) 权限码（菜单按钮；唯一来源就是 sys_menu.auth_code） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001028'::uuid,
     'business:erp:payable:submit-cancel', 3,
     'business:erp:payable:submit-cancel', NULL,
     '20000000-0000-0000-0000-000000001009'::uuid, NULL,
     4, 1, NULL, '提交应付单作废审批（发起 payable_cancel 流程）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001029'::uuid,
     'business:erp:payable:withdraw-cancel', 3,
     'business:erp:payable:withdraw-cancel', NULL,
     '20000000-0000-0000-0000-000000001009'::uuid, NULL,
     5, 1, NULL, '撤回应付单作废审批（发起人撤回）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001030'::uuid,
     'business:erp:voucher:submit-reverse', 3,
     'business:erp:voucher:submit-reverse', NULL,
     '20000000-0000-0000-0000-000000001004'::uuid, NULL,
     4, 1, NULL, '提交凭证红冲审批（发起 voucher_reverse 流程）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001031'::uuid,
     'business:erp:voucher:withdraw-reverse', 3,
     'business:erp:voucher:withdraw-reverse', NULL,
     '20000000-0000-0000-0000-000000001004'::uuid, NULL,
     5, 1, NULL, '撤回凭证红冲审批（发起人撤回）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001028'::uuid),
        ('20000000-0000-0000-0000-000000001029'::uuid),
        ('20000000-0000-0000-0000-000000001030'::uuid),
        ('20000000-0000-0000-0000-000000001031'::uuid)) AS m(id)
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
         WHERE table_name = 'biz_payable' AND column_name = 'cancel_instance_id'
    ) OR NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_payable' AND column_name = 'cancel_reason'
    ) THEN
        RAISE EXCEPTION 'V0191 自检失败：biz_payable 缺 cancel_instance_id / cancel_reason';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_voucher' AND column_name = 'pending_reverse_instance_id'
    ) OR NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_voucher' AND column_name = 'reverse_reason'
    ) THEN
        RAISE EXCEPTION 'V0191 自检失败：biz_voucher 缺 pending_reverse_instance_id / reverse_reason';
    END IF;
    IF pg_get_constraintdef((SELECT oid FROM pg_constraint WHERE conname = 'ck_biz_payable_status')) NOT LIKE '%PENDING_CANCEL%' THEN
        RAISE EXCEPTION 'V0191 自检失败：ck_biz_payable_status 没有放开 PENDING_CANCEL';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_payable_pending_cancel') THEN
        RAISE EXCEPTION 'V0191 自检失败：ck_biz_payable_pending_cancel 未建立';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_voucher_pending_reverse') THEN
        RAISE EXCEPTION 'V0191 自检失败：ck_biz_voucher_pending_reverse 未建立';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'FINANCE_MANAGER' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0191 自检失败：角色 FINANCE_MANAGER 未种';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'FINANCE_DIRECTOR' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0191 自检失败：角色 FINANCE_DIRECTOR 未种';
    END IF;
    IF EXISTS (SELECT 1 FROM sys_menu WHERE auth_code = 'business:erp:payable:submit-cancel' AND deleted_at IS NULL) = FALSE THEN
        RAISE EXCEPTION 'V0191 自检失败：权限码 business:erp:payable:submit-cancel 未种';
    END IF;
    IF EXISTS (SELECT 1 FROM sys_menu WHERE auth_code = 'business:erp:voucher:submit-reverse' AND deleted_at IS NULL) = FALSE THEN
        RAISE EXCEPTION 'V0191 自检失败：权限码 business:erp:voucher:submit-reverse 未种';
    END IF;

    -- 7.2 应付作废流程
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0182') THEN
        RAISE NOTICE 'V0191：payable_cancel 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    ELSE
        SELECT count(*) INTO m_rows FROM biz_flow_def d
         WHERE d.id = '20000000-0000-4000-8000-0000000c0182' AND d.flow_code = 'payable_cancel'
           AND d.biz_type = 'PAYABLE_CANCEL' AND d.status = 'PUBLISHED' AND d.is_default
           AND d.current_version_id = '20000000-0000-4000-8000-0000000c0183'
           AND d.deleted_at IS NULL;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：payable_cancel 定义行不满足 PUBLISHED/默认/当前版本';
        END IF;

        SELECT count(*) INTO v_bad FROM biz_flow_def_version v
         WHERE v.id = '20000000-0000-4000-8000-0000000c0183'
           AND v.def_id = '20000000-0000-4000-8000-0000000c0182'
           AND v.version_status = 'PUBLISHED' AND v.is_current
           AND v.graph_hash = '4f484777debbea1a0cc1a76db28274c4d9986c8cbe8d3936afa7e08a50adf7fc'
           AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
           AND v.node_count = 3 AND v.edge_count = 2;
        IF v_bad <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：payable_cancel 版本行（快照/hash/计数）不对';
        END IF;

        SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c0183';
        SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000c0183';
        IF n_rows <> 3 OR e_rows <> 2 THEN
            RAISE EXCEPTION 'V0191 自检失败：payable_cancel 节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
        END IF;

        -- 审批节点必须「有人可派 + 没人时回退超管」（不许静默自动通过）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000c0183' AND node_key = 'n_fm'
           AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
           AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：应付作废审批节点的来源/兜底策略不对';
        END IF;
        SELECT count(*) INTO a_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = '20000000-0000-4000-8000-0000000c0183' AND n.node_key = 'n_fm'
           AND a.ref_id = '20000000-0000-4000-8000-0000000f0005'::uuid;
        IF a_rows <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：应付作废审批人必须是 FINANCE_MANAGER（实际 % 行）', a_rows;
        END IF;
        -- 只有一条 END 入边，但 join_mode 仍写 OR（与其它种子一致，且加节点时不至于卡死）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000c0183' AND node_key = 'e' AND join_mode = 'OR';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：payable_cancel 的 END 节点必须 OR 汇聚';
        END IF;
    END IF;

    -- 7.3 凭证红冲流程
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0182') THEN
        RAISE NOTICE 'V0191：voucher_reverse 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    ELSE
        SELECT count(*) INTO m_rows FROM biz_flow_def d
         WHERE d.id = '20000000-0000-4000-8000-0000000d0182' AND d.flow_code = 'voucher_reverse'
           AND d.biz_type = 'VOUCHER_REVERSE' AND d.status = 'PUBLISHED' AND d.is_default
           AND d.current_version_id = '20000000-0000-4000-8000-0000000d0183'
           AND d.deleted_at IS NULL;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：voucher_reverse 定义行不满足 PUBLISHED/默认/当前版本';
        END IF;

        SELECT count(*) INTO v_bad FROM biz_flow_def_version v
         WHERE v.id = '20000000-0000-4000-8000-0000000d0183'
           AND v.def_id = '20000000-0000-4000-8000-0000000d0182'
           AND v.version_status = 'PUBLISHED' AND v.is_current
           AND v.graph_hash = '742f06d8fbecc6eb2fdfdfaed66571bfff9c1b3b02ff8dd0338f0dc7f3dd17b2'
           AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
           AND v.node_count = 5 AND v.edge_count = 5;
        IF v_bad <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：voucher_reverse 版本行（快照/hash/计数）不对';
        END IF;

        SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0183';
        SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0183';
        IF n_rows <> 5 OR e_rows <> 5 THEN
            RAISE EXCEPTION 'V0191 自检失败：voucher_reverse 节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
        END IF;

        -- 1 万阈值必须真的落在条件边上（不是代码里 if/else）
        SELECT count(*) INTO m_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000d0183'
           AND NOT is_default AND condition_expr IS NOT NULL
           AND condition_expr->>'field' = 'form.amount'
           AND condition_expr->>'cmp' = 'gt'
           AND (condition_expr->>'value')::numeric = 10000;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：10000 元会签阈值必须落在 gt 条件边上（实际 % 条）', m_rows;
        END IF;

        -- 会签节点：ALL + 两个角色（财务经理 + 财务总监）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000d0183' AND node_key = 'n_cs'
           AND node_type = 'COUNTERSIGN' AND approval_mode = 'ALL'
           AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：会签节点必须是 COUNTERSIGN + ALL + 兜底超管';
        END IF;
        SELECT count(*) INTO a_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = '20000000-0000-4000-8000-0000000d0183' AND n.node_key = 'n_cs';
        IF a_rows <> 2 THEN
            RAISE EXCEPTION 'V0191 自检失败：会签节点应有 2 个审批角色，实际 %', a_rows;
        END IF;

        -- END 有两条入边 ⇒ 必须 OR 汇聚（AND 会等人齐 → 审批单永久卡死）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000d0183' AND node_key = 'e' AND join_mode = 'OR';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0191 自检失败：voucher_reverse 的 END 节点必须 OR 汇聚';
        END IF;
    END IF;

    RAISE NOTICE 'V0191 自检通过：应付作废 + 凭证红冲两条标准流程已就绪';
END $$;
