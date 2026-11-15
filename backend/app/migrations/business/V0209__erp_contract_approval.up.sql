-- ======================================================
-- V0209：合同审批流（ARCH-3 §6 R-P1-1 · 断链 DC-02）
--
-- ① `biz_contract` 加 `approval_instance_id`（提交审批的实例弱引用）
-- ② `ck_biz_contract_status` 从 5 值扩到 6 值（加 `PENDING_APPROVAL` 审批中）
-- ③ `ck_biz_contract_approval`：审批中 ⟺ 挂实例（其它状态不许挂 / 软删行豁免）
-- ④ 预置标准流程 `contract_approve`（金额条件分支：< 10 万单级 / ≥ 10 万两级审批）
-- ⑤ 存量行兼容：已在用的合同不受影响（status 值语义没变，只是多了一个中间态）
--
-- ── 为什么要有这条迁移 ────────────────────────────────
-- R-P1-1 实测的问题：`biz_contract` 没有 `approval_instance_id`，合同在列表里点一次
-- 「登记签订」就从 DRAFT 变 SIGNED —— 那是「可被采购单引用、可用于付款」的已签状态，
-- **全程没有任何人审批**。采购单自己有 `po_approve` 复核，可它挂的合同是一人点一下
-- 就生效的，于是「给合同付钱」这件事只审了单、没审合同，内控在最上游漏了一整环。
--
-- 修法（四步，本迁移负责前两步）：
--   DRAFT ──提交审批──▶ PENDING_APPROVAL ──审批通过──▶ SIGNED ──生效──▶ ACTIVE
--                                   └──驳回/撤回──▶ DRAFT（实例清空，可改后重提）
-- 状态机边表由 `shared/state_machine.rs::CONTRACT_EDGES` 从 5 条扩到 7 条
-- （去掉 `DRAFT → SIGNED`，加 `DRAFT → PENDING_APPROVAL` / `PENDING_APPROVAL → SIGNED` /
--  `PENDING_APPROVAL → DRAFT`）；业务回调在 `business/erp/contract_bridge.rs`。
--
-- ── 金额分级为什么写成流程定义里的条件边（不是代码 if/else）──
-- 与 V0182 的 `po_approve` 同一个理由：「阈值、审批人、超时」全在
-- `biz_flow_node` / `biz_flow_edge.condition_expr` / `biz_flow_node_assignee` 里。
-- 客户要改成「20 万以上才两级」，在「流程定义」里另存一个版本即可，**不用改代码、不发版**。
--
-- ── 审批人为什么用 ROLE 而不是 ADMIN ─────────────────
--   · 审批人来源 = `ROLE`（运行时展开该角色下的所有用户）：
--     `PURCHASE_MANAGER`（采购经理）/ `GENERAL_MANAGER`（总经理），角色由 V0182 种好；
--   · `empty_assignee = FALLBACK_ADMIN` —— **角色里 0 人时回退给平台超管，而不是静默跳过**。
--     `SKIP` 会让「没人可派」变成「审批自动通过」，比卡住更坏（V0174 文件头的原话）。
--   · `self_approval = ALLOW` **故意不是 FORBID**：FORBID 在**发起时**就 422（整单起不来），
--     而内控要的是「提交成功、但**制单人这一票**点不动」→ 待办照常可见留痕，
--     真正的否决在审批入口（`contract_bridge::guard_self_approval`，403 + 65019）
--     与待办生成前（`flow::internal_control`，落 SKIPPED 留痕）。
--
-- ── 库侧硬约束（不是靠代码自觉）────────────────────────
-- `ck_biz_contract_approval` 挡两类坏数据：
--   · 「审批中却没挂实例」→ 永远等不到回调，合同卡死在 PENDING_APPROVAL；
--   · 「其它状态却还挂着实例」→ 下次提交会撞 `uk_biz_flow_instance_biz_active`（409 无法自愈）。
-- 软删行豁免：「审批中删单」是合法反向路径（先终止审批再软删/或直接软删，同一事务）。
-- 注意审批通过后 `SIGNED` **保留**实例 id 作审计（详情显示「审批通过 FLW-xxx」），
-- 所以约束只要求「PENDING_APPROVAL ⟺ 非空」，不对 SIGNED 之后的保留做限制。
--
-- ── 快照与 hash（不是手写的）───────────────────────────
-- 图按 `graph.rs::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
-- 审批人按 (kind,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--   contract_approve : 95270c466037cbb4f83a68e42eb02d7476ae520ca0f96da84edd1e47f7b75768
-- 末尾 DO 块自检「条数 / hash / 条件边 / END 汇聚 / is_current 唯一 / 角色存在」，
-- 任一不符 → 迁移失败 → 启动失败（宁可起不来，也不要种出一张会被引擎跳过的图）。
--
-- ── 幂等与安全 ────────────────────────────────────────
-- · 加列 `IF NOT EXISTS`；约束先 `DROP ... IF EXISTS` 再建；索引 `IF NOT EXISTS`。
-- · 流程四层 `WHERE NOT EXISTS` 守卫（已有同名流程 / 已有同 biz_type 的默认流程 →
--   整条跳过，绝不覆盖客户配置）；角色 UUID 不符也整条跳过（别种出悬空的 assignee）。
-- · 回滚：`V0209__erp_contract_approval.down.sql`。
-- ======================================================

-- ---------- ① 合同：提交审批的实例弱引用 ----------
ALTER TABLE biz_contract
    ADD COLUMN IF NOT EXISTS approval_instance_id uuid;

COMMENT ON COLUMN biz_contract.approval_instance_id IS
    '提交审批的实例 id（R-P1-1）：PENDING_APPROVAL（审批中）必有值；驳回/撤回时清空；SIGNED 之后保留作审计';

CREATE INDEX IF NOT EXISTS idx_biz_contract_approval
    ON biz_contract (approval_instance_id)
    WHERE approval_instance_id IS NOT NULL;

-- ---------- ② 状态白名单：加 PENDING_APPROVAL ----------
ALTER TABLE biz_contract DROP CONSTRAINT IF EXISTS ck_biz_contract_status;
ALTER TABLE biz_contract
    ADD CONSTRAINT ck_biz_contract_status
    CHECK (status IN ('DRAFT', 'PENDING_APPROVAL', 'SIGNED', 'ACTIVE', 'COMPLETED', 'TERMINATED'));

COMMENT ON COLUMN biz_contract.status IS
    '状态机：DRAFT→PENDING_APPROVAL→SIGNED→ACTIVE→COMPLETED/TERMINATED，另有 PENDING_APPROVAL→DRAFT（驳回/撤回）与 SIGNED→TERMINATED；边表在应用层（state_machine::CONTRACT_EDGES，7 条）';

-- ---------- ③ 审批中 ⟺ 挂实例 ----------
ALTER TABLE biz_contract DROP CONSTRAINT IF EXISTS ck_biz_contract_approval;
ALTER TABLE biz_contract
    ADD CONSTRAINT ck_biz_contract_approval
    CHECK (
        deleted_at IS NOT NULL
        OR (status = 'PENDING_APPROVAL' AND approval_instance_id IS NOT NULL)
        OR (status <> 'PENDING_APPROVAL')
    );

-- ---------- ④ 流程定义：合同审批 contract_approve ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000c0209', 'contract_approve', '合同审批（标准·金额分级）',
    'CONTRACT_APPROVE', '合同管理',
    '合同提交审批：按合同金额两档分流（< 10 万采购经理单级审批 / ≥ 10 万采购经理 + 总经理两级审批）。'
    '审批通过才落「已签订（SIGNED）」（此后才可被采购单引用、才可生效），驳回或撤回则回草稿并把意见写进状态日志。'
    '阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000c020a', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'contract_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'CONTRACT_APPROVE' AND is_default AND deleted_at IS NULL
)
-- 角色必须是我们种的那一套（客户用别的 UUID 预建过 → 整条跳过，别种出悬空的 assignee）
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0002'::uuid AND deleted_at IS NULL
)
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0003'::uuid AND deleted_at IS NULL
);

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000c020a', '20000000-0000-4000-8000-0000000c0209', 1, 'PUBLISHED', TRUE,
    '95270c466037cbb4f83a68e42eb02d7476ae520ca0f96da84edd1e47f7b75768',
    $snapshot${"edges":[{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":100000},"from":"c_amt","is_default":false,"key":"e_low","label":"金额 < 100000（单级审批）","priority":0,"to":"n_pm"},{"condition":null,"from":"c_amt","is_default":true,"key":"e_low_else","label":"金额 ≥ 100000（两级审批）","priority":10,"to":"n_pm_hi"},{"condition":null,"from":"n_gm","is_default":false,"key":"e_gm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_pm","is_default":false,"key":"e_pm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_pm_hi","is_default":false,"key":"e_hi_seq","label":null,"priority":0,"to":"n_gm"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"c_amt"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_amt","max_remind_times":3,"name":"金额分级（< 10 万单级）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":200,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":860,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm","max_remind_times":2,"name":"总经理审批（≥ 10 万·第二级）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":640,"y":140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0002","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_pm","max_remind_times":2,"name":"采购经理审批（< 10 万）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":420,"y":-140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0002","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_pm_hi","max_remind_times":2,"name":"采购经理审批（≥ 10 万·第一级）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":420,"y":140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    6, 6, 'V0209 种子：合同审批（金额两档分流）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0209')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0210'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0211'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'c_amt'::varchar, '金额分级（< 10 万单级）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 200::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0212'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'n_pm'::varchar, '采购经理审批（< 10 万）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 420::int, -140::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0213'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'n_pm_hi'::varchar, '采购经理审批（≥ 10 万·第一级）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 420::int, 140::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0214'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'n_gm'::varchar, '总经理审批（≥ 10 万·第二级）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 640::int, 140::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c0215'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 860::int, 0::int, 6::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c020a')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0220'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000c0210'::uuid, '20000000-0000-4000-8000-0000000c0211'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0221'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'e_low'::varchar, '20000000-0000-4000-8000-0000000c0211'::uuid, '20000000-0000-4000-8000-0000000c0212'::uuid, '{"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 100000}'::jsonb, '金额 < 100000（单级审批）'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0222'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'e_low_else'::varchar, '20000000-0000-4000-8000-0000000c0211'::uuid, '20000000-0000-4000-8000-0000000c0213'::uuid, NULL::jsonb, '金额 ≥ 100000（两级审批）'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000c0223'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'e_pm_end'::varchar, '20000000-0000-4000-8000-0000000c0212'::uuid, '20000000-0000-4000-8000-0000000c0215'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0224'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'e_hi_seq'::varchar, '20000000-0000-4000-8000-0000000c0213'::uuid, '20000000-0000-4000-8000-0000000c0214'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c0225'::uuid, '20000000-0000-4000-8000-0000000c020a'::uuid, 'e_gm_end'::varchar, '20000000-0000-4000-8000-0000000c0214'::uuid, '20000000-0000-4000-8000-0000000c0215'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c020a')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 审批人：全部 ROLE（客户只绑人，不改流程）
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c0212'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0002'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c0213'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0002'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c0214'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c020a')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ⑤ 结构与种子自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    m_rows int;
    v_bad  int;
    n_rows int;
    e_rows int;
    a_rows int;
BEGIN
    -- 5.1 表结构（无论流程是否跳过都必须改到位）
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_contract' AND column_name = 'approval_instance_id'
    ) THEN
        RAISE EXCEPTION 'V0209 自检失败：biz_contract 缺 approval_instance_id';
    END IF;
    IF pg_get_constraintdef((SELECT oid FROM pg_constraint WHERE conname = 'ck_biz_contract_status'))
       NOT LIKE '%PENDING_APPROVAL%' THEN
        RAISE EXCEPTION 'V0209 自检失败：ck_biz_contract_status 没有放开 PENDING_APPROVAL';
    END IF;
    IF pg_get_constraintdef((SELECT oid FROM pg_constraint WHERE conname = 'ck_biz_contract_approval'))
       NOT LIKE '%PENDING_APPROVAL%' THEN
        RAISE EXCEPTION 'V0209 自检失败：ck_biz_contract_approval 没有覆盖 PENDING_APPROVAL';
    END IF;
    -- 存量行不允许被本迁移改坏（R-P1-1 只加列/加值，不动既有数据）
    SELECT count(*) INTO m_rows FROM biz_contract WHERE status NOT IN
        ('DRAFT','PENDING_APPROVAL','SIGNED','ACTIVE','COMPLETED','TERMINATED');
    IF m_rows <> 0 THEN
        RAISE EXCEPTION 'V0209 自检失败：biz_contract 有 % 行状态不在 6 值白名单里', m_rows;
    END IF;

    -- 5.2 合同审批流程（客户已有同名流程 / 角色被别的 UUID 占过 → 守卫整条跳过）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c0209') THEN
        RAISE NOTICE 'V0209：contract_approve 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    ELSE
        SELECT count(*) INTO m_rows FROM biz_flow_def d
         WHERE d.id = '20000000-0000-4000-8000-0000000c0209' AND d.flow_code = 'contract_approve'
           AND d.biz_type = 'CONTRACT_APPROVE' AND d.status = 'PUBLISHED' AND d.is_default
           AND d.current_version_id = '20000000-0000-4000-8000-0000000c020a'
           AND d.deleted_at IS NULL;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0209 自检失败：contract_approve 定义行不满足 PUBLISHED/默认/当前版本';
        END IF;

        SELECT count(*) INTO v_bad FROM biz_flow_def_version v
         WHERE v.id = '20000000-0000-4000-8000-0000000c020a'
           AND v.def_id = '20000000-0000-4000-8000-0000000c0209'
           AND v.version_status = 'PUBLISHED' AND v.is_current
           AND v.graph_hash = '95270c466037cbb4f83a68e42eb02d7476ae520ca0f96da84edd1e47f7b75768'
           AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
           AND v.node_count = 6 AND v.edge_count = 6;
        IF v_bad <> 1 THEN
            RAISE EXCEPTION 'V0209 自检失败：contract_approve 版本行（快照/hash/计数）不对';
        END IF;

        SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c020a';
        SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000c020a';
        IF n_rows <> 6 OR e_rows <> 6 THEN
            RAISE EXCEPTION 'V0209 自检失败：contract_approve 节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
        END IF;

        -- 金额分级必须真的落在条件边上（不是代码里 if/else）
        SELECT count(*) INTO m_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000c020a'
           AND NOT is_default AND condition_expr IS NOT NULL
           AND condition_expr->>'field' = 'form.amount'
           AND (condition_expr->>'value')::numeric = 100000;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0209 自检失败：10 万分级阈值必须落在条件边上（实际 % 条）', m_rows;
        END IF;
        -- 必须恰有一条兜底边（否则 ≥ 10 万的合同会走进单级审批）
        SELECT count(*) INTO m_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000c020a' AND is_default;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0209 自检失败：条件节点必须有且仅有一条兜底边（实际 % 条）', m_rows;
        END IF;

        -- 三个审批节点都必须「有人可派 + 没人时回退超管」（不许静默自动通过）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000c020a'
           AND node_key IN ('n_pm', 'n_pm_hi', 'n_gm')
           AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
           AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW';
        IF m_rows <> 3 THEN
            RAISE EXCEPTION 'V0209 自检失败：合同审批三个节点的审批人来源/兜底策略不对（应为 3 行，实际 %）', m_rows;
        END IF;

        -- 两级审批必须是**串行两层**（n_pm_hi → n_gm），不是会签
        SELECT count(*) INTO a_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000c020a'
           AND edge_key = 'e_hi_seq';
        IF a_rows <> 1 THEN
            RAISE EXCEPTION 'V0209 自检失败：≥ 10 万必须两级串行（缺 e_hi_seq 边）';
        END IF;
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000c020a' AND node_type = 'COUNTERSIGN';
        IF m_rows <> 0 THEN
            RAISE EXCEPTION 'V0209 自检失败：两级审批是串行的，不该有 COUNTERSIGN 会签节点';
        END IF;

        -- 审批人角色必须是采购经理 / 总经理（不是固定人、不是 ADMIN）
        SELECT count(*) INTO a_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = '20000000-0000-4000-8000-0000000c020a'
           AND a.assignee_type = 'ROLE'
           AND a.ref_id IN ('20000000-0000-4000-8000-0000000f0002'::uuid,
                            '20000000-0000-4000-8000-0000000f0003'::uuid);
        IF a_rows <> 3 THEN
            RAISE EXCEPTION 'V0209 自检失败：审批角色引用应为 3 行（采购经理×2 + 总经理），实际 %', a_rows;
        END IF;

        -- END 有两条入边 ⇒ 必须 OR 汇聚（AND 会等人齐 → 审批单永久卡死）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000c020a' AND node_key = 'e' AND join_mode = 'OR';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0209 自检失败：contract_approve 的 END 节点必须 OR 汇聚';
        END IF;
    END IF;

    RAISE NOTICE 'V0209 自检通过：合同审批流（金额两档分级）已就绪；状态白名单已扩到 6 值';
END $$;
