-- ============================================================
-- V0206：状态机配置的「管理面 + 审批化」配套（【D3】流程配置化 · 阶段 3 出口）
--
-- 依据：`docs/状态机边配置化-D1.md` §4（写入三道门）/ §8（已知边界 1、3）
--       卡 t_217a95ad「1. HTTP 管理面」「2. 改配置走审批流」
--
-- # 这条迁移做三件事
--
--   ① 两个权限码（挂在「流程定义」菜单下的按钮级权限）
--        business:flow:state-edge:list    读「BASE 边 + 当前配置 + 合并后的有效边」
--        business:flow:state-edge:manage  改 / 删配置（本迁移只授 SUPER_ADMIN）
--      与 `backend/src/business/flow/edge_admin.rs` 的 `#[nik::permission]` 注解**逐字一致**：
--      注解里写什么、这里就种什么（`system/menu/tests.rs::n10` 会把漏种的码扫出来）。
--
--   ② 一条**系统预置流程** `flow_edge_config`（`biz_type = 'FLOW_EDGE_CFG'`）：
--      改状态机配置这件事本身要走审批 —— 提交 → 复核 → 通过才落 `biz_flow_state_edge`。
--         s ──e_start──▶ n_cfg（流程配置复核）──e_cfg_end──▶ e
--      · 节点审批人 = `ROLE`（`FLOW_CONFIG_APPROVER`，本迁移种的角色）+ `empty_assignee =
--        FALLBACK_ADMIN` ⇒ **开箱可跑**：没人绑角色时落到超管身上；客户把「复核岗」绑上人之后
--        自动按岗位分权，流程一行不用改（与 V0182 的种子同款口径）。
--      · `self_approval = 'ALLOW'`：本流程**不在** `flow::internal_control` 的受控 biz_type 清单里
--        （那份清单是 6 个账务业务：采购 / 收货 / 应付 / 凭证）。语义上「改配置」是管理员的内控动作，
--        单超管环境要能自助完成 —— 但**必须留一张审批单**（谁发起、谁复核、什么时候生效，
--        审批中心可查；驳回/撤回则配置一分不动）。要更强的双人复核，把本流程的审批人绑到
--        `FLOW_CONFIG_APPROVER` 上即可（绑了人就不会再走 FALLBACK_ADMIN）。
--      · `is_system = TRUE`：预置模板，前端标「标准」，不允许改图（改图 = 换流程，另开卡）。
--
--   ③ 结构 / 数据自检（失败即整笔回滚 ⇒ 启动失败）。
--
-- # 为什么这条迁移里没有新表
--
--   审批单本身就是「待变更内容」的载体：`engine::start` 的 `form_data` 落 `biz_flow_instance.form_data`，
--   里面就是 `{action, entity, from_status, to_status, enabled, requires_reason, label, auth_code, reason}`。
--   审批通过时回调读它 → 调 D1 的**唯一写入入口** `state_machine::upsert_edge_config` / `delete_edge_config`
--   （内核一行没动）。再加一张 `biz_flow_edge_change` 只会变成「审批单 + 影子表」两份真相。
--
--   `biz_id` 用「边」的确定性 UUID（`Uuid::new_v5(NAMESPACE_OID, entity:from>to)`）：
--   同一实体同一状态上的**重复提交**会被 `uk_biz_flow_instance_biz_active` 挡住（同一时刻
--   同一条边只能有一张在跑的审批单），不同边的审批互不影响。
--
-- # 迁移号
--
--   卡面没有指定号段；复核当前最大号是 V0205（`flow_linkage_rule`，D2），故取 **V0206**。
--   复核命令：`ls backend/migrations/business | grep -oP 'V\d+' | sort -u | tail -3`
--
-- # graph_hash 怎么来的（别手抄）
--
--   `flow_edge_config` 是**系统预置版本**，所以 `graph_hash` 必须与「从 node / edge / assignee 行
--   重建出来的快照」逐字节一致，否则 D4d 的每日自检（`flow::maintain`）会把它报成
--   `RowsEdited` 漂移。本文件的 hash 由
--     snapshot()（节点按 key、边按 from/to/key、审批人按 kind+ref_id 排序）
--     → canonical JSON（递归按键升序、无空白）
--     → sha256
--   算出；单测 `edge_admin::tests::seed_flow_hash_matches_rows` 拿库里的行重算一遍钉死。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT DO NOTHING`（不覆盖运维改过的标题/排序/图）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。空库首启时 `sys_role` 为空 → 授权是空操作
--   （bootstrap 之后重放本迁移即可补齐，与 V0192/V0197/V0202 同口径）。
--
-- # 回滚
--
--   `V0206__flow_edge_config_admin.down.sql`：删种子流程（节点/边/审批人随版本级联）+ 两个权限码
--   + 授权行；**不动** `biz_flow_state_edge`（那是 D1 的表，回滚本迁移不该丢配置）
--   也不动 `biz_status_log`（审计不在回滚范围）。
-- ============================================================

-- ---------- ① 两个权限码（按钮级，挂在「流程定义」0…0780 下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001210'::uuid, 'business:flow:state-edge:list', 3,
     'business:flow:state-edge:list', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     920, 1, NULL, '状态机配置查看（BASE 边 / 当前配置 / 有效边）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001211'::uuid, 'business:flow:state-edge:manage', 3,
     'business:flow:state-edge:manage', NULL, '20000000-0000-0000-0000-000000000780'::uuid, NULL,
     930, 1, NULL, '状态机配置变更（提交审批 / 删除配置，仅 SUPER_ADMIN）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 授权：两个码都只给 SUPER_ADMIN ----------
-- manage 只授超管（改状态机 = 改全公司审批路径）；list 也只授超管起步，
-- 由管理员按需授予流程管理员（读的是「哪条边被禁用了」，属于控制面信息）。
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001210'::uuid),
        ('20000000-0000-0000-0000-000000001211'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- ③ 审批角色（种子流程的审批人来源；0 人时走 FALLBACK_ADMIN） ----------
INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, remark,
                      created_at, updated_at)
VALUES
    ('20000000-0000-4000-8000-0000000f0015'::uuid, 'FLOW_CONFIG_APPROVER', '流程配置复核员',
     '状态机边配置（改配置本身走审批）的复核岗；0 人绑定 → FALLBACK_ADMIN',
     1, 15, 'V0206 种子：改状态机配置的复核岗', now(), now())
ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ④ 流程定义：flow_edge_config ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id,
    is_system)
SELECT
    '20000000-0000-4000-8000-0000000e0206', 'flow_edge_config',
    '状态机配置变更审批（改流程规则前先过复核）',
    'FLOW_EDGE_CFG', '系统',
    '改状态机边配置（禁用可选边 / 追加必填原因 / 改文案 / 追加权限码 / 删除配置）本身要提交一条审批：'
    '审批通过才真正写 biz_flow_state_edge，驳回或撤回配置一分不动。'
    '审批单的 form_data 里带着「待变更内容」，通过时由回调调 D1 的唯一写入入口落库。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000e0207', 1, TRUE, FALSE, NULL, TRUE
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'flow_edge_config' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'FLOW_EDGE_CFG' AND is_default AND deleted_at IS NULL
)
-- 角色必须是本迁移种的那一个（客户用别的 UUID 预建过同名角色 → 整条跳过，别种出悬空 assignee）
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0015'::uuid AND deleted_at IS NULL
);

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000e0207', '20000000-0000-4000-8000-0000000e0206', 1,
    'PUBLISHED', TRUE,
    'c0ad61937b734114f6865f9347b57ab9824f560605e522a204d22650baa30e08',
    $snapshot${
  "schema_version": 1,
  "nodes": [
    {
      "key": "e", "name": "结束", "node_type": "END",
      "assignee_type": null, "approval_mode": null, "approve_ratio": null,
      "reject_veto": true, "join_mode": "AND", "fork_mode": "AND",
      "form_var_key": null, "superior_levels": 1,
      "empty_assignee": "SKIP", "self_approval": "SKIP", "allow_robot": false,
      "timeout_hours": null, "timeout_action": "REMIND", "remind_before_hours": null,
      "max_remind_times": 3, "cc_on_approved": false,
      "x": 640, "y": 0, "sort_order": 3, "config": {}, "assignees": []
    },
    {
      "key": "n_cfg", "name": "流程配置复核", "node_type": "APPROVAL",
      "assignee_type": "ROLE", "approval_mode": null, "approve_ratio": null,
      "reject_veto": true, "join_mode": "AND", "fork_mode": "AND",
      "form_var_key": null, "superior_levels": 1,
      "empty_assignee": "FALLBACK_ADMIN", "self_approval": "ALLOW", "allow_robot": false,
      "timeout_hours": 48, "timeout_action": "REMIND", "remind_before_hours": null,
      "max_remind_times": 2, "cc_on_approved": false,
      "x": 320, "y": 0, "sort_order": 2, "config": {},
      "assignees": [
        {"assignee_type": "ROLE", "ref_id": "20000000-0000-4000-8000-0000000f0015", "sort_order": 0}
      ]
    },
    {
      "key": "s", "name": "开始", "node_type": "START",
      "assignee_type": null, "approval_mode": null, "approve_ratio": null,
      "reject_veto": true, "join_mode": "AND", "fork_mode": "AND",
      "form_var_key": null, "superior_levels": 1,
      "empty_assignee": "SKIP", "self_approval": "SKIP", "allow_robot": false,
      "timeout_hours": null, "timeout_action": "REMIND", "remind_before_hours": null,
      "max_remind_times": 3, "cc_on_approved": false,
      "x": 0, "y": 0, "sort_order": 1, "config": {}, "assignees": []
    }
  ],
  "edges": [
    {
      "key": "e_cfg_end", "from": "n_cfg", "to": "e",
      "condition": null, "label": null, "priority": 0, "is_default": false
    },
    {
      "key": "e_start", "from": "s", "to": "n_cfg",
      "condition": null, "label": null, "priority": 0, "is_default": false
    }
  ]
}$snapshot$::jsonb,
    3, 2, 'V0206 种子：状态机配置变更审批（一人复核）', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000e0206')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e0210'::uuid, '20000000-0000-4000-8000-0000000e0207'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000e0211'::uuid, '20000000-0000-4000-8000-0000000e0207'::uuid, 'n_cfg'::varchar, '流程配置复核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 320::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000e0212'::uuid, '20000000-0000-4000-8000-0000000e0207'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 640::int, 0::int, 3::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e0207')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e0220'::uuid, '20000000-0000-4000-8000-0000000e0207'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000e0210'::uuid, '20000000-0000-4000-8000-0000000e0211'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e0221'::uuid, '20000000-0000-4000-8000-0000000e0207'::uuid, 'e_cfg_end'::varchar, '20000000-0000-4000-8000-0000000e0211'::uuid, '20000000-0000-4000-8000-0000000e0212'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e0207')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 审批人：ROLE（0 人 → FALLBACK_ADMIN，开箱可跑）
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e0211'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0015'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e0207')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    n_missing int;
    v_missing text;
    n_rows    int;
    v_hash    text;
    v_status  text;
BEGIN
    -- ① 两个权限码必须在（我们自己插的 → 缺了就 EXCEPTION）
    SELECT count(*) INTO n_missing
      FROM (VALUES ('business:flow:state-edge:list'),
                   ('business:flow:state-edge:manage')) AS t(code)
     WHERE NOT EXISTS (
           SELECT 1 FROM sys_menu m
            WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
    IF n_missing <> 0 THEN
        SELECT string_agg(code, ', ') INTO v_missing
          FROM (VALUES ('business:flow:state-edge:list'),
                       ('business:flow:state-edge:manage')) AS t(code)
         WHERE NOT EXISTS (
               SELECT 1 FROM sys_menu m
                WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
        RAISE EXCEPTION 'V0206 自检失败：缺权限码 %', v_missing;
    END IF;

    -- ② 两行的形状（菜单类型 / 隐藏 / 父节点）与设计一致
    SELECT count(*) INTO n_missing
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001210'::uuid,
                  '20000000-0000-0000-0000-000000001211'::uuid)
       AND (menu_type <> 3
            OR is_visible <> FALSE
            OR parent_id <> '20000000-0000-0000-0000-000000000780'::uuid);
    IF n_missing <> 0 THEN
        RAISE EXCEPTION 'V0206 自检失败：% 行权限码的形状（menu_type / is_visible / parent_id）与设计不一致', n_missing;
    END IF;

    -- ③ 种子流程必须在且是 PUBLISHED（客户先建了同名流程时整条跳过 → 只 NOTICE）
    SELECT count(*) INTO n_rows
      FROM biz_flow_def d
      JOIN biz_flow_def_version v ON v.id = d.current_version_id
     WHERE d.flow_code = 'flow_edge_config'
       AND d.deleted_at IS NULL AND d.status = 'PUBLISHED'
       AND d.biz_type = 'FLOW_EDGE_CFG'
       AND v.graph_snapshot IS NOT NULL AND v.graph_hash IS NOT NULL
       AND v.node_count = 3 AND v.edge_count = 2;
    IF n_rows = 0 THEN
        RAISE NOTICE 'V0206：flow_edge_config 种子未落（库里已有同编码流程或被 biz_type 默认位占用）'
                     '—— 改配置走审批会退化为「没配流程 → 409 + 显式跳过」，请检查 biz_flow_def';
        RETURN;
    END IF;

    -- ④ hash 与快照都必须在（正确性由单测 seed_flow_hash_matches_rows 从行重算钉死）
    SELECT v.graph_hash, v.version_status INTO v_hash, v_status
      FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id
     WHERE d.flow_code = 'flow_edge_config' AND d.deleted_at IS NULL;
    IF v_hash IS NULL OR length(v_hash) <> 64 THEN
        RAISE EXCEPTION 'V0206 自检失败：flow_edge_config 的 graph_hash 非法（%）', v_hash;
    END IF;
    IF v_status <> 'PUBLISHED' THEN
        RAISE EXCEPTION 'V0206 自检失败：flow_edge_config 的当前版本不是 PUBLISHED（%）', v_status;
    END IF;

    RAISE NOTICE 'V0206 完成：2 个权限码 + flow_edge_config 种子流程（s → n_cfg → e，hash %）', left(v_hash, 12);
END $$;
