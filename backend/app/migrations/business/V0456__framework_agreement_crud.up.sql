-- ============================================================================
-- V0456：框架协议维护链路（采购链 M4-11b）—— 按钮权限码 + 审批流程 fw_agreement
--
--   ① 5 行菜单（menu_type=3，全部挂在 V0452 的页面行 `…004480` 下）：
--        `…:list` / `…:create` / `…:update` / `…:delete` / `…:submit`
--      页面行 `business:erp:framework-agreement:page` 已由 V0452 种下，本迁移**不动它**
--      （页面显形码只给前端，AGENTS 约定 14；后端零引用）。
--   ② 流程种子 fw_agreement（biz_type = 'FWA_APPROVE'）：
--        START ──▶ 采购经理审批 ──▶ 财务经理复核 ──▶ END
--      框架协议是**价格与额度的长期承诺**（签下去一年内所有采购都按它结算），
--      比单张采购单的影响面大得多 —— 所以固定两道：采购经理（业务条款）+ 财务经理（价格口径）。
--      要给大额协议加总经理会签，在流程定义里另存版本加条件边即可（本迁移不写 if/else）。
--   ③ 角色授权：PURCHASE_MANAGER 拿全部 5 行（建 / 改 / 删 / 提 / 看都是采购的日常动作）；
--      FINANCE_MANAGER 只拿 `…:list`（要看协议条款与上限，但不该改单）；
--      两者原有的 `…:execution`（V0452 授予）保持不变。
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`）──
--   实测最大号 = V0455（供应商准入 / 黑名单 M4-14）⇒ 本迁移顺延取 **V0456**。
--   菜单 id 取 `…004490–004494`（V0455 用到 …004489，连号不交叠）；
--   流程 uuid 取 `…0d06a1`（定义）/ `…0d06a2`（版本）/ `0d06a3–0d06a6`（节点）/ `0d06b1–0d06b3`（边）。
--   grep 过：`00449[0-4]`、`0d06a`、`0d06b`、`fw_agreement`、`FWA_APPROVE` 全库无占用。
--
-- ── 本卡的范围 ────────────────────────────────────────────────────────────
--   做：5 个接口权限码 + 流程种子 + 角色授权（对应后端 6 条路由 / 前端 4 个写按钮）。
--   不做：改表结构（V0450 的 29 + 13 列够用）、改取号（`biz_seq(FWA)` 已由 V0450 种下）、
--         改执行量视图（V0452 / V0453 不动）。
--
-- ── 为什么这里**不**新增状态、不加列 ────────────────────────────────────
--   状态域仍是 V0450 定的 DRAFT / SUBMITTED / APPROVED 三值；「协议到期」仍是时间的函数
--   （`status='APPROVED' AND start_date <= 今日 <= end_date`，判据在 model::is_effective_on）。
--   本卡只让三个状态**有入口**（建 → 提 → 批），不引入第四个状态。
--
-- ── graph_hash（不是手写的）──────────────────────────────────────────────
--   图按 `flow::graph::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
--   审批人按 (kind,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--     fw_agreement : de47937e30aa0fa1b56b007b1f4e3b7774cd3dd848e5bde4f35eb82d67885629
--   复算口径：本迁移落行后 `flow::model::load_graph()` 重建图 → `graph.graph_hash()`
--   必须等于上面这串（真库用例 `seeded_flow_hash_matches_rows` 每次运行都复算一遍；
--   本节注释里的值是实测出来回填的 —— 本卡先用 64 个 0 落行，跑用例拿到重算值再改）。
--
-- ── 幂等与回滚 ───────────────────────────────────────────────────────────
--   · 菜单 `ON CONFLICT (id) DO UPDATE`；角色授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`；
--   · 流程四层（定义 / 版本 / 节点 / 边 / 审批人）每个 INSERT 都带 `WHERE NOT EXISTS` 守卫：
--     已有 `flow_code='fw_agreement'`、或已有同 biz_type 的默认流程 → **整条跳过**，
--     绝不覆盖客户配置；两个审批角色不在 → 也整条跳过（避免种出悬空审批人）；
--   · 回滚见 V0456__framework_agreement_crud.down.sql（有协议单就拒绝回滚，再删流程四层 + 菜单 + 授权）。
-- ============================================================================

-- ---------- ① 菜单：5 个接口码按钮行（挂在页面行 …004480 下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004490'::uuid, 'framework-agreement:list', 3,
     'business:erp:framework-agreement:list', NULL,
     '20000000-0000-0000-0000-000000004480'::uuid, NULL,
     20, 1, NULL, '查看台账', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004491'::uuid, 'framework-agreement:create', 3,
     'business:erp:framework-agreement:create', NULL,
     '20000000-0000-0000-0000-000000004480'::uuid, NULL,
     30, 1, NULL, '新建协议', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004492'::uuid, 'framework-agreement:update', 3,
     'business:erp:framework-agreement:update', NULL,
     '20000000-0000-0000-0000-000000004480'::uuid, NULL,
     40, 1, NULL, '修改协议', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004493'::uuid, 'framework-agreement:delete', 3,
     'business:erp:framework-agreement:delete', NULL,
     '20000000-0000-0000-0000-000000004480'::uuid, NULL,
     50, 1, NULL, '删除协议', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004494'::uuid, 'framework-agreement:submit', 3,
     'business:erp:framework-agreement:submit', NULL,
     '20000000-0000-0000-0000-000000004480'::uuid, NULL,
     60, 1, NULL, '提交审批', TRUE, 'business')
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

-- ---------- ② 流程定义：框架协议审批 fw_agreement ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, is_system)
SELECT
    '20000000-0000-4000-8000-0000000d06a1'::uuid, 'fw_agreement',
    '框架协议审批', 'FWA_APPROVE', '采购管理',
    '框架协议（价格与额度的长期承诺）提交后走本流程：采购经理审批 → 财务经理复核。'
    '通过即 APPROVED，协议在有效期内可被采购单引用（有效期的判据是时间函数，不是状态列）；'
    '驳回 / 撤回则打回草稿并把意见写进备注。要给大额协议加会签，请在本流程定义里另存版本。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000d06a2'::uuid, 1, TRUE, FALSE, TRUE
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'fw_agreement' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'FWA_APPROVE' AND is_default AND deleted_at IS NULL
)
-- 审批人角色必须是我们种的那一套（客户用别的 UUID 预建过 → 整条跳过，别种出悬空的 assignee）
AND (
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0002'::uuid,
                  '20000000-0000-4000-8000-0000000f0005'::uuid)
       AND deleted_at IS NULL
) = 2;

-- ---------- ③ 流程版本（快照 + hash） ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000d06a2'::uuid, '20000000-0000-4000-8000-0000000d06a1'::uuid, 1,
    'PUBLISHED', TRUE,
    'de47937e30aa0fa1b56b007b1f4e3b7774cd3dd848e5bde4f35eb82d67885629',
    $snapshot${"edges":[{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_pm","is_default":false,"key":"e_pm_fin","label":null,"priority":0,"to":"n_fin"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_pm"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":660,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"财务经理复核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":440,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0002","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_pm","max_remind_times":2,"name":"采购经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    4, 3,
    'M4-11b 首版：采购经理审批 → 财务经理复核 → 结束（框架协议是价格与额度的长期承诺）',
    now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d06a1'::uuid)
ON CONFLICT (id) DO NOTHING;

-- ---------- ④ 节点（4：开始 / 采购经理 / 财务经理 / 结束） ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d06a3'::uuid, '20000000-0000-4000-8000-0000000d06a2'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d06a4'::uuid, '20000000-0000-4000-8000-0000000d06a2'::uuid, 'n_pm'::varchar, '采购经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d06a5'::uuid, '20000000-0000-4000-8000-0000000d06a2'::uuid, 'n_fin'::varchar, '财务经理复核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 440::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d06a6'::uuid, '20000000-0000-4000-8000-0000000d06a2'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 660::int, 0::int, 4::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d06a2'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- ⑤ 边（3：开始 → 采购经理 → 财务经理 → 结束） ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d06b1'::uuid, '20000000-0000-4000-8000-0000000d06a2'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d06a3'::uuid, '20000000-0000-4000-8000-0000000d06a4'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d06b2'::uuid, '20000000-0000-4000-8000-0000000d06a2'::uuid, 'e_pm_fin'::varchar, '20000000-0000-4000-8000-0000000d06a4'::uuid, '20000000-0000-4000-8000-0000000d06a5'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d06b3'::uuid, '20000000-0000-4000-8000-0000000d06a2'::uuid, 'e_fin_end'::varchar, '20000000-0000-4000-8000-0000000d06a5'::uuid, '20000000-0000-4000-8000-0000000d06a6'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d06a2'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- ---------- ⑥ 审批人（ROLE → 采购经理 / 财务经理） ----------
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d06a4'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0002'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d06a5'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d06a2'::uuid)
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ⑦ 角色授权：采购经理全拿 5 行；财务经理只拿「查看台账」 ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  JOIN sys_menu m ON m.id IN ('20000000-0000-0000-0000-000000004490'::uuid,
                               '20000000-0000-0000-0000-000000004491'::uuid,
                               '20000000-0000-0000-0000-000000004492'::uuid,
                               '20000000-0000-0000-0000-000000004493'::uuid,
                               '20000000-0000-0000-0000-000000004494'::uuid)
 WHERE r.role_code = 'PURCHASE_MANAGER' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- 财务经理：协议条款与上限是他复核月度付款 / 成本归集的输入，所以要看台账；
-- 但**不给** create / update / delete / submit —— 改条款是采购的活（职责分离）。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  JOIN sys_menu m ON m.id = '20000000-0000-0000-0000-000000004490'::uuid
 WHERE r.role_code = 'FINANCE_MANAGER' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑧ 自检（任一条不符 → 迁移失败 → 启动失败） ----------
DO $$
DECLARE
    n_rows INT;
    v_id   UUID := '20000000-0000-4000-8000-0000000d06a2'::uuid;
    v_def  UUID := '20000000-0000-4000-8000-0000000d06a1'::uuid;
BEGIN
    -- 8.1 菜单 5 行：全部 menu_type=3、父级是 V0452 的页面行、标题 ≤ 12 字
    SELECT count(*) INTO n_rows FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000004490'::uuid,
                  '20000000-0000-0000-0000-000000004491'::uuid,
                  '20000000-0000-0000-0000-000000004492'::uuid,
                  '20000000-0000-0000-0000-000000004493'::uuid,
                  '20000000-0000-0000-0000-000000004494'::uuid)
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000004480'::uuid
       AND deleted_at IS NULL
       AND length(title) <= 12;
    IF n_rows <> 5 THEN
        RAISE EXCEPTION 'V0456 自检失败：按钮行应有 5 行且挂在页面行 …004480 下（孤儿 parent 会让 menu_invariants ① 直接红），实际 %', n_rows;
    END IF;

    -- 8.2 五个接口码逐条在位（少了哪个，对应按钮点了就是 403）
    SELECT count(*) INTO n_rows FROM sys_menu
     WHERE auth_code IN ('business:erp:framework-agreement:list',
                         'business:erp:framework-agreement:create',
                         'business:erp:framework-agreement:update',
                         'business:erp:framework-agreement:delete',
                         'business:erp:framework-agreement:submit')
       AND menu_type = 3 AND deleted_at IS NULL;
    IF n_rows <> 5 THEN
        RAISE EXCEPTION 'V0456 自检失败：5 个接口码只种下 % 个', n_rows;
    END IF;

    -- 8.3 同父 sort_order 必须唯一（重复 → 侧边栏顺序每次刷新都变）
    SELECT count(*) INTO n_rows FROM (
        SELECT sort_order FROM sys_menu
         WHERE parent_id = '20000000-0000-0000-0000-000000004480'::uuid AND deleted_at IS NULL
         GROUP BY sort_order HAVING count(*) > 1) dup;
    IF n_rows <> 0 THEN
        RAISE EXCEPTION 'V0456 自检失败：页面行下的 sort_order 有重复（% 组）', n_rows;
    END IF;

    -- 8.4 授权：采购经理 5 行、财务经理 1 行（N20：除超管外至少一个角色能用）
    SELECT count(*) INTO n_rows FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE r.role_code = 'PURCHASE_MANAGER'
       AND rm.menu_id IN (SELECT unnest(ARRAY[
             '20000000-0000-0000-0000-000000004490'::uuid,
             '20000000-0000-0000-0000-000000004491'::uuid,
             '20000000-0000-0000-0000-000000004492'::uuid,
             '20000000-0000-0000-0000-000000004493'::uuid,
             '20000000-0000-0000-0000-000000004494'::uuid]));
    IF n_rows <> 5 THEN
        RAISE EXCEPTION 'V0456 自检失败：PURCHASE_MANAGER 应拿到 5 个授权，实际 %', n_rows;
    END IF;
    SELECT count(*) INTO n_rows FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE r.role_code = 'FINANCE_MANAGER'
       AND rm.menu_id = '20000000-0000-0000-0000-000000004490'::uuid;
    -- 财务经理角色存在时必须授到；不存在（客户自定义角色集）则 0 行也合法
    IF n_rows > 1 THEN
        RAISE EXCEPTION 'V0456 自检失败：FINANCE_MANAGER 的台账授权出现 % 行（唯一约束失效）', n_rows;
    END IF;

    -- 8.5 流程四层：定义 / 版本 / 4 节点 / 3 边 / 2 审批人
    SELECT count(*) INTO n_rows FROM biz_flow_def
     WHERE id = v_def AND flow_code = 'fw_agreement' AND biz_type = 'FWA_APPROVE'
       AND status = 'PUBLISHED' AND is_default AND current_version_id = v_id AND deleted_at IS NULL;
    IF n_rows > 1 THEN
        RAISE EXCEPTION 'V0456 自检失败：fw_agreement 流程定义出现 % 行（应至多 1 行）', n_rows;
    END IF;
    IF n_rows = 1 THEN
        SELECT count(*) INTO n_rows FROM biz_flow_def_version
         WHERE id = v_id AND version_status = 'PUBLISHED' AND is_current
           AND node_count = 4 AND edge_count = 3
           AND graph_hash ~ '^[0-9a-f]{64}$'
           AND jsonb_array_length(graph_snapshot->'nodes') = 4
           AND jsonb_array_length(graph_snapshot->'edges') = 3;
        IF n_rows <> 1 THEN
            RAISE EXCEPTION 'V0456 自检失败：流程版本行不对（发布态 / 节点边计数 / hash 形状 / 快照条数）';
        END IF;
        SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = v_id;
        IF n_rows <> 4 THEN
            RAISE EXCEPTION 'V0456 自检失败：节点应有 4 个，实际 %', n_rows;
        END IF;
        SELECT count(*) INTO n_rows FROM biz_flow_edge WHERE version_id = v_id;
        IF n_rows <> 3 THEN
            RAISE EXCEPTION 'V0456 自检失败：边应有 3 条，实际 %', n_rows;
        END IF;
        SELECT count(*) INTO n_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = v_id AND a.assignee_type = 'ROLE';
        IF n_rows <> 2 THEN
            RAISE EXCEPTION 'V0456 自检失败：ROLE 审批人应有 2 行（采购经理 / 财务经理），实际 %', n_rows;
        END IF;
        -- 串行链路必须真的串起来：s → n_pm → n_fin → e（没有条件边，也没有兜底边）
        SELECT count(*) INTO n_rows FROM biz_flow_edge
         WHERE version_id = v_id AND condition_expr IS NULL AND NOT is_default;
        IF n_rows <> 3 THEN
            RAISE EXCEPTION 'V0456 自检失败：3 条边都应是「无条件、非兜底」的串行边，实际 %', n_rows;
        END IF;
    END IF;

    RAISE NOTICE 'V0456 自检通过：框架协议维护链路（5 个接口权限码 + FWA_APPROVE 流程 + 授权）已就绪';
END $$;
