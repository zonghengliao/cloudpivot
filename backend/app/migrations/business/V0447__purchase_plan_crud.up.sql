-- ======================================================
-- V0447：采购计划 CRUD + 审批（采购链 M4-02）
--
--   ① 种开箱即用的流程 `pp_approve`（`biz_type = 'PP_APPROVE'`）：
--        START ──▶ 采购经理审批 ──▶ 条件（计划金额分级）
--                                    ├── form.total_planned_amount < 500000 ──▶ END
--                                    └── 金额 ≥ 500000（兜底边）───────────▶ 财务经理复核 ──▶ END
--      分级写成**流程定义里的条件边**，不是代码里的 if/else ——
--      客户改阈值 / 换审批岗位，在流程定义里另存版本即可，不用改代码、不用发版。
--   ② 6 行菜单：1 个页面（`menu_type=2`）+ 5 个按钮（`menu_type=3`）。
--      页面行挂**显形码** `business:erp:purchase-plan:page`（只给前端判侧栏与路由，
--      后端零引用，见 AGENTS 约定 14）；5 个按钮行挂接口码 {list,create,update,delete,submit}。
--   ③ 角色授权：PURCHASE_MANAGER 全给（编制 / 提交是采购的日常动作）；
--      FINANCE_MANAGER 与 DEPT_LEADER 只给页面 + 全量列表（大额复核要看单）。
--
-- ── 号段（开工实测复核）──────────────────────────────────
--   开工时 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`：
--     V0444（销售预测数量）/ V0445（返利应付科目）/ V0446（采购计划表结构 M4-01）
--   ⇒ 本卡顺延取 **V0447**。
--   流程 uuid 取 `…0d0481`（定义）/ `…0d0482`（版本）/ `…0d0483–0d0487`（节点）
--   / `…0d0491–0d0495`（边）；菜单取 `…004471–004476`（与既有段无交叠）。
--
-- ── 与表结构（V0446）的分工 ──────────────────────────────
--   V0446 只建表：`biz_purchase_plan`（单头 22 列）+ `biz_purchase_plan_item`（13 列）。
--   本迁移**不改那两张表一个字节**，只补「流程 / 菜单 / 授权」三样运行期配置 ——
--   建单 / 改单 / 删单 / 提交审批的业务入口在 `purchase_plan_handler`。
--
-- ── 快照与 hash（不是手写的）────────────────────────────
--   图按 `flow::graph::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
--   审批人按 (kind,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--     pp_approve : 405b90103fc3c19448f01eb060bd77a1a553e9099aca6370f8d7b11dbd26bb1d
--   复算口径：与 V0435 同一套脚本（把 V0435 的快照重算一遍必须等于它自己写着的
--   `8633ba2d…` —— 这条自校验先跑通，再拿同一函数算本迁移的 hash）。
--   末尾 DO 块自检「条数 / hash / 条件边阈值 / 审批人角色 / 兜底策略 / END 汇聚 / START 直连 /
--   菜单 6 行与页面码只给前端 / 角色授权 ≥1 个非超管」，任一不符 → 迁移失败 → 启动失败。
--
-- ── 幂等与安全 ────────────────────────────────────────
--   · 流程四层 `WHERE NOT EXISTS` 守卫：已有 `flow_code='pp_approve'`、或已有同
--     `biz_type='PP_APPROVE'` 的默认流程（撞 `uk_biz_flow_def_default`）→ 整条跳过
--     （**绝不覆盖客户配置**）；两个审批角色不在（客户库用别的 UUID 预建过）→ 也整条跳过；
--   · 固定 uuid + `ON CONFLICT DO NOTHING`；菜单 `ON CONFLICT (id) DO UPDATE`；
--   · 角色授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`（角色不存在时自然插 0 行）；
--   · 回滚：`V0447__purchase_plan_crud.down.sql`（只删本迁移种的四张表行 + 菜单 + 授权）。
-- ======================================================

-- ---------- ① 流程定义：采购计划审批 pp_approve ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, is_system)
SELECT
    '20000000-0000-4000-8000-0000000d0481'::uuid, 'pp_approve', '采购计划审批（标准·金额分级）',
    'PP_APPROVE', '采购管理',
    '年度 / 月度采购计划提交后走本流程：采购经理复核计划量与预估金额；'
    '计划金额 ≥ 50 万的计划再加一道财务经理复核。审批通过计划才变 APPROVED（此后才能转采购申请），'
    '驳回 / 撤回则打回草稿并把意见追加到备注。阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000d0482'::uuid, 1, TRUE, FALSE, TRUE
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'pp_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'PP_APPROVE' AND is_default AND deleted_at IS NULL
)
-- 审批人角色必须是我们种的那一套（客户用别的 UUID 预建过 → 整条跳过，别种出悬空的 assignee）
AND (
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0002'::uuid,
                  '20000000-0000-4000-8000-0000000f0005'::uuid)
       AND deleted_at IS NULL
) = 2;

-- ---------- ② 流程版本（快照 + hash） ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000d0482'::uuid, '20000000-0000-4000-8000-0000000d0481'::uuid, 1,
    'PUBLISHED', TRUE,
    '405b90103fc3c19448f01eb060bd77a1a553e9099aca6370f8d7b11dbd26bb1d',
    $snapshot${"edges":[{"condition":{"cmp":"lt","field":"form.total_planned_amount","op":"cmp","value":500000},"from":"c_amt","is_default":false,"key":"e_low","label":"金额 < 500000","priority":0,"to":"e"},{"condition":null,"from":"c_amt","is_default":true,"key":"e_low_else","label":"金额 ≥ 500000","priority":10,"to":"n_fin"},{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_pm","is_default":false,"key":"e_pm_amt","label":null,"priority":0,"to":"c_amt"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_pm"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_amt","max_remind_times":3,"name":"金额分级（≥ 50 万加财务复核）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":420,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":860,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"财务经理复核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":640,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0002","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_pm","max_remind_times":2,"name":"采购经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$,
    5, 5, 'V0447 种子：采购计划审批（编制 → 采购经理 / ≥50 万加财务经理复核）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0481'::uuid)
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- ③ 节点（5） ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0483'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0484'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 'n_pm'::varchar, '采购经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0485'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 'c_amt'::varchar, '金额分级（≥ 50 万加财务复核）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 420::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0486'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 'n_fin'::varchar, '财务经理复核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 640::int, 0::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0487'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 860::int, 0::int, 5::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0482'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- ④ 边（5：两条串行 + 一条金额条件 + 一条兜底 + 收尾） ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0491'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d0483'::uuid, '20000000-0000-4000-8000-0000000d0484'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0492'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 'e_pm_amt'::varchar, '20000000-0000-4000-8000-0000000d0484'::uuid, '20000000-0000-4000-8000-0000000d0485'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0493'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 'e_low'::varchar, '20000000-0000-4000-8000-0000000d0485'::uuid, '20000000-0000-4000-8000-0000000d0487'::uuid, '{"op": "cmp", "field": "form.total_planned_amount", "cmp": "lt", "value": 500000}'::jsonb, '金额 < 500000'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0494'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 'e_low_else'::varchar, '20000000-0000-4000-8000-0000000d0485'::uuid, '20000000-0000-4000-8000-0000000d0486'::uuid, NULL::jsonb, '金额 ≥ 500000'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000d0495'::uuid, '20000000-0000-4000-8000-0000000d0482'::uuid, 'e_fin_end'::varchar, '20000000-0000-4000-8000-0000000d0486'::uuid, '20000000-0000-4000-8000-0000000d0487'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0482'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- ---------- ⑤ 审批人（ROLE → 采购经理 / 财务经理） ----------
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0484'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0002'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d0486'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0482'::uuid)
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ⑥ 菜单：页面节点 + 5 个按钮 ----------
-- 页面行（menu_type=2）挂的码是**显形码** `…:page`：只给前端判侧栏显隐与路由 meta.permission，
-- 后端**零引用**（AGENTS 约定 14 / permission_audit_tests 的 N14 会把「页面码出现在后端源码」判红）。
-- 5 个接口码各挂一条按钮行（menu_type=3），与 handler 的 #[nik::permission] 注解逐个对应。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004471'::uuid, '采购计划', 2,
     'business:erp:purchase-plan:page', '/business/erp/purchase-plan',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/purchase-plan/index',
     38, 1, 'calendar', '采购计划', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004472'::uuid, '查看采购计划', 3,
     'business:erp:purchase-plan:list', NULL, '20000000-0000-0000-0000-000000004471'::uuid,
     NULL, 10, 1, NULL, '查看全量', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004473'::uuid, '新建采购计划', 3,
     'business:erp:purchase-plan:create', NULL, '20000000-0000-0000-0000-000000004471'::uuid,
     NULL, 20, 1, NULL, '新建计划', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004474'::uuid, '编辑采购计划', 3,
     'business:erp:purchase-plan:update', NULL, '20000000-0000-0000-0000-000000004471'::uuid,
     NULL, 30, 1, NULL, '编辑计划', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004475'::uuid, '删除采购计划', 3,
     'business:erp:purchase-plan:delete', NULL, '20000000-0000-0000-0000-000000004471'::uuid,
     NULL, 40, 1, NULL, '删除计划', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004476'::uuid, '提交计划审批', 3,
     'business:erp:purchase-plan:submit', NULL, '20000000-0000-0000-0000-000000004471'::uuid,
     NULL, 50, 1, NULL, '提交审批', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

-- ---------- ⑦ 角色授权（本迁移替人做的决定，逐条说明） ----------
--   · PURCHASE_MANAGER（采购经理）：6 个码全给 —— 编制 / 改 / 删 / 提交计划就是它的日常动作；
--   · FINANCE_MANAGER（财务经理）：page + list —— ≥50 万的计划要它复核，得能看单；
--     编制与提交不是财务的动作（审批权限由流程引擎按角色展开，不靠这里的菜单码）；
--   · DEPT_LEADER（部门主管）：page + list —— 只能看，提计划由采购统一编制。
--   · 其余角色不给：SUPER_ADMIN 由框架短路，不需要显式授权。
--   角色不存在时自然插入 0 行（空库首启：角色由历史迁移种，本迁移先跑也能过）。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.menu_id
  FROM (VALUES
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000004471'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000004472'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000004473'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000004474'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000004475'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000004476'::uuid),
        ('FINANCE_MANAGER',  '20000000-0000-0000-0000-000000004471'::uuid),
        ('FINANCE_MANAGER',  '20000000-0000-0000-0000-000000004472'::uuid),
        ('DEPT_LEADER',      '20000000-0000-0000-0000-000000004471'::uuid),
        ('DEPT_LEADER',      '20000000-0000-0000-0000-000000004472'::uuid)
       ) AS m(role_code, menu_id)
  JOIN sys_role r ON r.role_code = m.role_code AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑧ 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    m_rows int;
    v_rows int;
    n_rows int;
    e_rows int;
    a_rows int;
BEGIN
    -- 8.1 流程（客户已有同名流程 / 角色 UUID 不符 → 守卫整条跳过，仅提示）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0481'::uuid) THEN
        RAISE NOTICE 'V0447：pp_approve 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    ELSE
        SELECT count(*) INTO m_rows FROM biz_flow_def d
         WHERE d.id = '20000000-0000-4000-8000-0000000d0481'::uuid AND d.flow_code = 'pp_approve'
           AND d.biz_type = 'PP_APPROVE' AND d.status = 'PUBLISHED' AND d.is_default
           AND d.current_version_id = '20000000-0000-4000-8000-0000000d0482'::uuid AND d.current_version_no = 1
           AND d.deleted_at IS NULL;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0447 自检失败：pp_approve 定义行不满足 PUBLISHED/默认/当前版本';
        END IF;

        SELECT count(*) INTO v_rows FROM biz_flow_def_version v
         WHERE v.id = '20000000-0000-4000-8000-0000000d0482'::uuid
           AND v.def_id = '20000000-0000-4000-8000-0000000d0481'::uuid
           AND v.version_status = 'PUBLISHED' AND v.is_current
           AND v.graph_hash = '405b90103fc3c19448f01eb060bd77a1a553e9099aca6370f8d7b11dbd26bb1d'
           AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
           AND v.node_count = 5 AND v.edge_count = 5;
        IF v_rows <> 1 THEN
            RAISE EXCEPTION 'V0447 自检失败：pp_approve 版本行（快照/hash/计数）不对';
        END IF;

        SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0482'::uuid;
        SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0482'::uuid;
        IF n_rows <> 5 OR e_rows <> 5 THEN
            RAISE EXCEPTION 'V0447 自检失败：节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
        END IF;

        -- 金额分级必须真的落在条件边上（不是代码里 if/else），阈值逐字是 500000 且字段是计划金额合计
        SELECT count(*) INTO m_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000d0482'::uuid
           AND NOT is_default AND condition_expr IS NOT NULL
           AND condition_expr->>'field' = 'form.total_planned_amount'
           AND (condition_expr->>'value')::numeric = 500000;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0447 自检失败：按 form.total_planned_amount 分流的条件边应为 1 条（500000），实际 %', m_rows;
        END IF;
        -- 兜底边必须 is_default（否则金额 ≥ 阈值时无路可走 → 实例卡死）
        SELECT count(*) INTO m_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000d0482'::uuid AND is_default AND priority = 10;
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0447 自检失败：else 边（is_default + priority=10）应为 1 条，实际 %', m_rows;
        END IF;

        -- 两个审批节点都必须「有人可派 + 没人时回退超管」（不许静默自动通过）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000d0482'::uuid
           AND node_key IN ('n_pm', 'n_fin')
           AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
           AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW' AND approval_mode = 'ANY';
        IF m_rows <> 2 THEN
            RAISE EXCEPTION 'V0447 自检失败：两个审批节点的审批人来源/兜底策略/会签口径不对（应 2 行，实际 %）', m_rows;
        END IF;

        -- 审批人必须逐节点绑到对应角色（采购经理 / 财务经理）
        SELECT count(*) INTO a_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = '20000000-0000-4000-8000-0000000d0482'::uuid
           AND a.assignee_type = 'ROLE';
        IF a_rows <> 2 THEN
            RAISE EXCEPTION 'V0447 自检失败：审批人应有 2 行（每节点一个角色），实际 %', a_rows;
        END IF;
        SELECT count(*) INTO m_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = '20000000-0000-4000-8000-0000000d0482'::uuid
           AND ((n.node_key = 'n_pm'  AND a.ref_id = '20000000-0000-4000-8000-0000000f0002'::uuid)
             OR (n.node_key = 'n_fin' AND a.ref_id = '20000000-0000-4000-8000-0000000f0005'::uuid));
        IF m_rows <> 2 THEN
            RAISE EXCEPTION 'V0447 自检失败：审批人角色绑定与岗位不符（应 2 行，实际 %）', m_rows;
        END IF;

        -- END 有多条入边 ⇒ 必须 OR 汇聚（AND 会等人齐 → 审批单永久卡死）
        SELECT count(*) INTO m_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000d0482'::uuid AND node_key = 'e' AND join_mode = 'OR';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0447 自检失败：pp_approve 的 END 节点必须 OR 汇聚';
        END IF;
        -- 串行链：采购经理必须是**第一个**审批节点（START 的直接下游）
        SELECT count(*) INTO m_rows FROM biz_flow_edge e
          JOIN biz_flow_node f ON f.id = e.from_node_id
          JOIN biz_flow_node t ON t.id = e.to_node_id
         WHERE e.version_id = '20000000-0000-4000-8000-0000000d0482'::uuid
           AND f.node_key = 's' AND t.node_key = 'n_pm';
        IF m_rows <> 1 THEN
            RAISE EXCEPTION 'V0447 自检失败：START 必须直连采购经理审批节点';
        END IF;
    END IF;

    -- 8.2 菜单（无论流程是否跳过都必须落位）
    SELECT count(*) INTO m_rows FROM sys_menu
     WHERE id IN (SELECT unnest(ARRAY[
             '20000000-0000-0000-0000-000000004471'::uuid,
             '20000000-0000-0000-0000-000000004472'::uuid,
             '20000000-0000-0000-0000-000000004473'::uuid,
             '20000000-0000-0000-0000-000000004474'::uuid,
             '20000000-0000-0000-0000-000000004475'::uuid,
             '20000000-0000-0000-0000-000000004476'::uuid]))
       AND deleted_at IS NULL;
    IF m_rows <> 6 THEN
        RAISE EXCEPTION 'V0447 自检失败：采购计划菜单应有 6 行（1 页面 + 5 按钮），实际 %', m_rows;
    END IF;

    -- 页面行：显形码 + 父级是采购管理目录 + 标题不长于 12 字
    SELECT count(*) INTO m_rows FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004471'::uuid
       AND menu_type = 2 AND auth_code = 'business:erp:purchase-plan:page'
       AND parent_id = '20000000-0000-0000-0000-000000000946'::uuid
       AND path = '/business/erp/purchase-plan' AND deleted_at IS NULL
       AND length(title) <= 12;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0447 自检失败：采购计划页面行（显形码 / 父级 / path / 标题长度）不对';
    END IF;

    -- 5 个接口码必须在按钮行上、且父级是页面行（页面码 ≠ 接口码，约定 14）
    SELECT count(*) INTO m_rows FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000004471'::uuid
       AND menu_type = 3 AND deleted_at IS NULL
       AND auth_code IN ('business:erp:purchase-plan:list',
                         'business:erp:purchase-plan:create',
                         'business:erp:purchase-plan:update',
                         'business:erp:purchase-plan:delete',
                         'business:erp:purchase-plan:submit');
    IF m_rows <> 5 THEN
        RAISE EXCEPTION 'V0447 自检失败：接口码按钮应有 5 行，实际 %', m_rows;
    END IF;

    -- 同父 sort_order 必须唯一（否则侧栏顺序每次刷新都变）
    SELECT count(*) INTO m_rows FROM (
        SELECT sort_order FROM sys_menu
         WHERE parent_id = '20000000-0000-0000-0000-000000004471'::uuid AND deleted_at IS NULL
         GROUP BY sort_order HAVING count(*) > 1) dup;
    IF m_rows <> 0 THEN
        RAISE EXCEPTION 'V0447 自检失败：按钮行 sort_order 有重复（% 组）', m_rows;
    END IF;

    -- 8.3 授权：PURCHASE_MANAGER 必须拿到全部 6 行（N20：除超管外至少要有一个角色能用）
    SELECT count(*) INTO m_rows FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE r.role_code = 'PURCHASE_MANAGER'
       AND rm.menu_id IN (SELECT unnest(ARRAY[
             '20000000-0000-0000-0000-000000004471'::uuid,
             '20000000-0000-0000-0000-000000004472'::uuid,
             '20000000-0000-0000-0000-000000004473'::uuid,
             '20000000-0000-0000-0000-000000004474'::uuid,
             '20000000-0000-0000-0000-000000004475'::uuid,
             '20000000-0000-0000-0000-000000004476'::uuid]));
    IF m_rows <> 6 THEN
        RAISE EXCEPTION 'V0447 自检失败：PURCHASE_MANAGER 应拿到 6 个菜单授权，实际 %', m_rows;
    END IF;

    RAISE NOTICE 'V0447 自检通过：采购计划（CRUD + 审批流 pp_approve + 6 行菜单 + 授权）已就绪';
END $$;
