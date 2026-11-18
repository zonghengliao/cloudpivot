-- ============================================================================
-- V0455：供应商准入 / 黑名单流程（采购链 M4-14）—— 申请单 + 审批流 supplier_access
--
--   ① biz_supplier_access        准入 / 黑名单 / 解除 三种申请单（一张表，access_type 区分）
--   ② biz_seq：SAC（SAC-yyyyMMdd-000001，按 CST 天滚动）
--   ③ 流程种子 supplier_access（biz_type = 'SUPPLIER_ACCESS'）：
--        START ──▶ 采购经理审批 ──▶ 条件（access_type eq 'BLACKLIST'）
--                                    ├── 是 ──▶ 财务经理复核 ──▶ END
--                                    └── 兜底（准入 / 解除）──▶ END
--      黑名单要停掉一家正在供货的供应商（影响在手订单与应付），所以加一道财务复核；
--      准入与解除是「恢复」方向，采购经理一票即可。分级写成**流程定义里的条件边**，
--      不是代码里的 if/else —— 客户要改谁审，在流程定义里另存版本即可。
--   ④ 5 行菜单：1 个页面（menu_type=2，显形码 *:page）+ 4 个按钮（menu_type=3，接口码）
--   ⑤ 角色授权：PURCHASE_MANAGER 拿全部 5 行（申请 / 提交是采购的日常动作）
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`）──
--   实测最大号 = V0454（供应商评估表结构 M4-12）⇒ 本卡顺延取 **V0455**。
--   流程 uuid 取 `…0d05a1`（定义）/ `…0d05a2`（版本）/ `…0d05a3–0d05a7`（节点）
--   / `…0d05b1–0d05b5`（边）；菜单取 `…004484–004488`（V0454 的 biz_seq 行到 …4482，
--   与既有 4471–4481 段连号但不交叠）；biz_seq 行 id 取 `…004489`。
--   grep 过：`0d05`、`00448[4-9]`、SAC 取号键、`biz_supplier_access` 表名、`sa_` 约束前缀全库无占用。
--
-- ── 本卡的范围（卡面「本卡只做这一件事，不夹带」）────────────────────────────
--   做：申请单表 + 取号 + 审批流程 + 提交/回调落地 + 接口（4 个权限码）+ 菜单 + 授权。
--   不做：评估打分（M4-13 已交）、前端页面（M4-16）、供应商主数据建档改造。
--
-- ── 为什么落点选 biz_supplier.status（1 启用 / 0 停用）而不是新加一列 ──────────
--   1) 「黑名单」的业务事实就是「这家供应商不能下单」，而 `biz_supplier.status` 本来就是
--      这个语义（V0137 起就在，主数据编辑页也在维护它）。再造一个 `blacklist_flag` 等于
--      让「能不能下单」有两个真相：两边不一致时没有任何入口会去对齐。
--   2) 本卡**不改** biz_supplier 的任何字节（不加列、不改默认值）—— 存量 181 家供应商
--      与全部既有夹具的行为一个字节都不变，这是「只追加、不侵入」的硬要求。
--   3) 历史与理由不丢：谁在什么时候因为什么把谁拉黑 / 准入 / 解除，全在
--      `biz_supplier_access`（单号 + 申请理由 + 评估等级快照 + 审批人快照 + 结论时间）。
--   4) 唯一的强制点在采购订单建单（`purchase_handler::resolve_header` 统一校验
--      `status = 1`）：停用的供应商（含被拉黑的）下单 → 409 + 66099。
--      主数据仍可直接改 status（现状不变）；准入/黑名单流程提供的是**有审批留痕的**启停。
--
-- ── 三种申请单的起点与落地（代码侧纯函数 `supplier_access_model::check_start`，单测钉住）──
--   | 类型    | 允许的起点                                   | 通过后 supplier.status |
--   |---------|----------------------------------------------|------------------------|
--   | ADMIT   | 未在黑名单中，且**从未**通过准入（首次准入） | 1（启用）              |
--   | BLACKLIST | 未在黑名单中                               | 0（停用）              |
--   | RELEASE | 正在黑名单中                                 | 1（启用）              |
--   「是否在黑名单」= 最近一张**已通过**的黑名单 / 解除单谁更晚（`decided_at` 比较），
--   不是读缓存列 —— 回写列一旦被动过就与单据链分叉（与 V0449 / M4-09 同一条理由）。
--
-- ── 状态域只有三个值 ────────────────────────────────────────────────────
--   DRAFT（拟稿，可改可删）→ SUBMITTED（审批中，必有实例）→ APPROVED（已生效）
--   驳回 / 撤回由回调打回 DRAFT（不是造「已驳回」状态：打回后要做的事和草稿完全一样）。
--   **提交与通过都不改主数据**：准不准、拉不拉黑一律等审批结论 —— 只有 APPROVED 才落
--   `biz_supplier.status`，所以「驳回」「撤回」「终止」没有任何需要回滚的副作用。
--
-- ── 幂等与安全 ─────────────────────────────────────────────────────────
--   · 建表 / 索引 `IF NOT EXISTS`；biz_seq 与流程四层 `WHERE NOT EXISTS` 守卫
--     （已有 `flow_code='supplier_access'`、或已有同 biz_type 的默认流程 → 整条跳过，
--      **绝不覆盖客户配置**；两个审批角色不在 → 也整条跳过，避免种出悬空审批人）；
--   · 固定 uuid + `ON CONFLICT DO NOTHING`；菜单 `ON CONFLICT (id) DO UPDATE`；
--   · 角色授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`（角色不存在时自然插 0 行）；
--   · 末尾 DO 块自检：表 / 索引 / 意图写入的坏数据逐条被拒 / 取号行 / 流程四层与
--     hash 形状 / 菜单 5 行与「页面码只给前端」/ 同父 sort 唯一 / 授权 ≥1 个非超管；
--   · 回滚：`V0455__supplier_access.down.sql`（有数据就拒绝，再删流程四层 + 菜单 + 授权
--     + 表 + 取号行）。
--
-- ── graph_hash（不是手写的）────────────────────────────────────────────────
--   图按 `flow::graph::snapshot()` 的确定性排序（节点按 key、边按 (from,to,key)、
--   审批人按 (kind,ref_id)）序列化成 canonical JSON（递归按键字典序、紧凑无空白），再 sha256：
--     supplier_access : 9e1d2b9cc5296f9b20e679602a18b5d04f0354b376aaea983992f5e20d0f7cea
--   复算口径：先本迁移落行 → `flow::model::load_graph()` 重建图 → `graph.graph_hash()`
--   必须等于上面这串（本卡的真库用例 `seeded_flow_hash_matches_rows` 每次运行都复算一遍）。
-- ============================================================================

-- ---------- ① biz_supplier_access（准入 / 黑名单 / 解除 申请单） ----------
CREATE TABLE IF NOT EXISTS biz_supplier_access (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    access_no            VARCHAR(32)   NOT NULL,                            -- 申请单号：SAC-yyyyMMdd-000001
    supplier_id          UUID          NOT NULL,                            -- 供应商（弱引用 biz_supplier.id）
    supplier_code        VARCHAR(40)   NOT NULL,                            -- 供应商编码快照（主档改码后仍能对出当时是谁）
    supplier_name        VARCHAR(200)  NOT NULL,                            -- 供应商名快照（审计/历史类，约定 13）
    access_type          VARCHAR(16)   NOT NULL,                            -- ADMIT 准入 / BLACKLIST 黑名单 / RELEASE 解除
    status               VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',            -- DRAFT / SUBMITTED / APPROVED
    reason               TEXT,                                              -- 申请理由（提交后必填 ≥ 5 字）
    grade_snapshot       VARCHAR(2),                                        -- 评估等级快照（**仅黑名单**：拉黑依据，A/B/C/D）
    apply_by             UUID,                                              -- 申请人（弱引用 sys_user.id）
    apply_by_name        VARCHAR(64),                                       -- 申请人姓名快照
    approval_instance_id UUID,                                              -- 审批实例（提交时写；状态日志/审批中心回查用）
    decided_by           UUID,                                              -- 结论人（弱引用 sys_user.id）
    decided_by_name      VARCHAR(64),                                       -- 结论人姓名快照
    decided_at           TIMESTAMPTZ,                                       -- 结论时间（APPROVED 必填）
    remark               TEXT,                                              -- 备注（驳回意见 / 补充说明）
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：创建时间
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：更新时间
    create_by            UUID,                                              -- 审计：创建人
    update_by            UUID,                                              -- 审计：更新人
    deleted_at           TIMESTAMPTZ,                                       -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_sa_no_fmt
        CHECK (access_no ~ '^SAC-[0-9]{8}-[0-9]{6}$'),
    CONSTRAINT ck_biz_sa_type
        CHECK (access_type IN ('ADMIT', 'BLACKLIST', 'RELEASE')),
    CONSTRAINT ck_biz_sa_status
        CHECK (status IN ('DRAFT', 'SUBMITTED', 'APPROVED')),
    -- 等级快照只对黑名单有意义（准入/解除写它就是录错；也是「不造摆设列」的库层保证）
    CONSTRAINT ck_biz_sa_grade_only_blacklist
        CHECK (access_type = 'BLACKLIST' OR grade_snapshot IS NULL),
    CONSTRAINT ck_biz_sa_grade_domain
        CHECK (grade_snapshot IS NULL OR grade_snapshot IN ('A', 'B', 'C', 'D')),
    -- 提交后必须有理由（≥ 5 字）：拉黑一家供应商却写不出理由的单据没有审计价值
    CONSTRAINT ck_biz_sa_reason_on_submit
        CHECK (status = 'DRAFT' OR length(btrim(coalesce(reason, ''))) >= 5),
    -- 审批中 / 已通过必须有审批实例（否则就是「没人能推进它」的死单）
    CONSTRAINT ck_biz_sa_instance_on_submit
        CHECK (status = 'DRAFT' OR approval_instance_id IS NOT NULL),
    -- 结论两列与 APPROVED 同生共死：通过必须有结论人与时间，未通过不许提前写死结论
    CONSTRAINT ck_biz_sa_decided
        CHECK (status <> 'APPROVED' OR (decided_at IS NOT NULL AND decided_by IS NOT NULL)),
    CONSTRAINT ck_biz_sa_not_decided_early
        CHECK (status = 'APPROVED' OR (decided_at IS NULL AND decided_by IS NULL))
);

COMMENT ON TABLE biz_supplier_access IS
    '供应商准入 / 黑名单 / 解除 申请单（审批通过才落 biz_supplier.status；历史与理由以本表为准）';
COMMENT ON COLUMN biz_supplier_access.access_type IS
    'ADMIT 准入（首次进入合格名录）/ BLACKLIST 黑名单 / RELEASE 解除黑名单；起点与落地见迁移头注释';
COMMENT ON COLUMN biz_supplier_access.status IS
    'DRAFT 拟稿 → SUBMITTED 审批中 → APPROVED 已生效；驳回/撤回/终止由回调打回 DRAFT';
COMMENT ON COLUMN biz_supplier_access.grade_snapshot IS
    '拉黑时依据的评估等级快照（M4-13 的 A/B/C/D）；准入/解除单恒为 NULL（ck_biz_sa_grade_only_blacklist）';

-- 单号存活唯一
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sa_no
    ON biz_supplier_access (access_no) WHERE deleted_at IS NULL;
-- 同一供应商同时只能有一张**未结**申请（草稿或审批中）：两张互相打架的单据必然有一张是错的
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sa_supplier_open
    ON biz_supplier_access (supplier_id)
 WHERE status IN ('DRAFT', 'SUBMITTED') AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_sa_status
    ON biz_supplier_access (status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_biz_sa_supplier
    ON biz_supplier_access (supplier_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_biz_sa_instance
    ON biz_supplier_access (approval_instance_id);

-- ---------- ② 取号：SAC ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key,
                     current_no, step, separator, status, remark)
SELECT '20000000-0000-4000-8000-000000004489'::uuid,
       'SAC', '供应商准入/黑名单申请单号', 'SAC', 'yyyyMMdd', '', 0, 1, '-', 1,
       '供应商准入/黑名单申请单号：SAC-yyyyMMdd-000001，按天（CST）滚动'
WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'SAC' AND deleted_at IS NULL);

-- ---------- ③ 流程定义：供应商准入 / 黑名单 supplier_access ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, is_system)
SELECT
    '20000000-0000-4000-8000-0000000d05a1'::uuid, 'supplier_access',
    '供应商准入 / 黑名单审批', 'SUPPLIER_ACCESS', '采购管理',
    '供应商准入、拉黑、解除黑名单三种申请单提交后走本流程：采购经理审批；'
    '拉黑（BLACKLIST）再加一道财务经理复核 —— 停掉一家正在供货的供应商会影响在手订单与应付。'
    '通过后按单据类型落 biz_supplier.status（准入/解除 → 启用，拉黑 → 停用），'
    '驳回 / 撤回则打回草稿并把意见写进备注。改审批人或加会签请在本流程定义里另存版本。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000d05a2'::uuid, 1, TRUE, FALSE, TRUE
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'supplier_access' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'SUPPLIER_ACCESS' AND is_default AND deleted_at IS NULL
)
-- 审批人角色必须是我们种的那一套（客户用别的 UUID 预建过 → 整条跳过，别种出悬空的 assignee）
AND (
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0002'::uuid,
                  '20000000-0000-4000-8000-0000000f0005'::uuid)
       AND deleted_at IS NULL
) = 2;

-- ---------- ④ 流程版本（快照 + hash） ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000d05a2'::uuid, '20000000-0000-4000-8000-0000000d05a1'::uuid, 1,
    'PUBLISHED', TRUE,
    '9e1d2b9cc5296f9b20e679602a18b5d04f0354b376aaea983992f5e20d0f7cea',
    $snapshot${"edges":[{"condition":null,"from":"c_type","is_default":true,"key":"e_type_else","label":"准入 / 解除（兜底）","priority":10,"to":"e"},{"condition":{"cmp":"eq","field":"form.access_type","op":"cmp","value":"BLACKLIST"},"from":"c_type","is_default":false,"key":"e_type_blk","label":"拉黑（BLACKLIST）","priority":0,"to":"n_fin"},{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_pm","is_default":false,"key":"e_pm_type","label":null,"priority":0,"to":"c_type"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_pm"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":"form.access_type","join_mode":"OR","key":"c_type","max_remind_times":3,"name":"类型分流（拉黑加财务复核）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":420,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":860,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"财务经理复核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":640,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0002","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_pm","max_remind_times":2,"name":"采购经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    5, 5,
    'M4-14 首版：采购经理审批 → 条件（拉黑加财务经理复核）→ 结束',
    now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d05a1'::uuid)
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑤ 节点（5：开始 / 采购经理 / 条件 / 财务经理 / 结束） ----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d05a3'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d05a4'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 'n_pm'::varchar, '采购经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d05a5'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 'c_type'::varchar, '类型分流（拉黑加财务复核）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, 'form.access_type'::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 420::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d05a6'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 'n_fin'::varchar, '财务经理复核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 640::int, 0::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d05a7'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 860::int, 0::int, 5::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d05a2'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- ⑥ 边（5：串行 2 + 一条类型条件 + 一条兜底 + 收尾） ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d05b1'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d05a3'::uuid, '20000000-0000-4000-8000-0000000d05a4'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d05b2'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 'e_pm_type'::varchar, '20000000-0000-4000-8000-0000000d05a4'::uuid, '20000000-0000-4000-8000-0000000d05a5'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d05b3'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 'e_type_blk'::varchar, '20000000-0000-4000-8000-0000000d05a5'::uuid, '20000000-0000-4000-8000-0000000d05a6'::uuid, '{"op": "cmp", "field": "form.access_type", "cmp": "eq", "value": "BLACKLIST"}'::jsonb, '拉黑（BLACKLIST）'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d05b4'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 'e_type_else'::varchar, '20000000-0000-4000-8000-0000000d05a5'::uuid, '20000000-0000-4000-8000-0000000d05a7'::uuid, NULL::jsonb, '准入 / 解除（兜底）'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000d05b5'::uuid, '20000000-0000-4000-8000-0000000d05a2'::uuid, 'e_fin_end'::varchar, '20000000-0000-4000-8000-0000000d05a6'::uuid, '20000000-0000-4000-8000-0000000d05a7'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d05a2'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- ---------- ⑦ 审批人（ROLE → 采购经理 / 财务经理） ----------
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d05a4'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0002'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d05a6'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d05a2'::uuid)
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ⑧ 菜单：页面行 + 4 个按钮 ----------
-- 页面行（menu_type=2）挂**显形码** `…:page`：只给前端判侧栏显隐与路由 meta.permission，
-- 后端**零引用**（AGENTS 约定 14；已在 permission_audit_tests 的 ROUTE_LEVEL_ONLY 台账登记）。
-- 4 个接口码各挂一条按钮行（menu_type=3），与 handler 的 #[nik::permission] 注解逐个对应。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004484'::uuid, 'supplier-access', 2,
     'business:erp:supplier-access:page', '/business/erp/supplier-access',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/supplier-access/index',
     42, 1, 'usergroup', '供应商准入', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004485'::uuid, 'supplier-access:list', 3,
     'business:erp:supplier-access:list', NULL,
     '20000000-0000-0000-0000-000000004484'::uuid, NULL,
     10, 1, NULL, '查看列表', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004486'::uuid, 'supplier-access:create', 3,
     'business:erp:supplier-access:create', NULL,
     '20000000-0000-0000-0000-000000004484'::uuid, NULL,
     20, 1, NULL, '新建申请', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004487'::uuid, 'supplier-access:update', 3,
     'business:erp:supplier-access:update', NULL,
     '20000000-0000-0000-0000-000000004484'::uuid, NULL,
     30, 1, NULL, '修改草稿', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004488'::uuid, 'supplier-access:submit', 3,
     'business:erp:supplier-access:submit', NULL,
     '20000000-0000-0000-0000-000000004484'::uuid, NULL,
     40, 1, NULL, '提交审批', TRUE, 'business')
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

-- ---------- ⑨ 角色授权：采购经理拿全部 5 行 ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  JOIN sys_menu m ON m.id IN (
        '20000000-0000-0000-0000-000000004484'::uuid,
        '20000000-0000-0000-0000-000000004485'::uuid,
        '20000000-0000-0000-0000-000000004486'::uuid,
        '20000000-0000-0000-0000-000000004487'::uuid,
        '20000000-0000-0000-0000-000000004488'::uuid)
 WHERE r.role_code = 'PURCHASE_MANAGER' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑩ 自检（任一条不符 → 迁移失败 → 启动失败） ----------
DO $$
DECLARE
    n_rows INT;
    v_id   UUID := '20000000-0000-4000-8000-0000000d05a2'::uuid;
BEGIN
    -- 10.1 表在
    IF to_regclass('public.biz_supplier_access') IS NULL THEN
        RAISE EXCEPTION 'V0455 自检失败：biz_supplier_access 表不存在';
    END IF;

    -- 10.2 四个索引在（单号 / 未结唯一 / 状态 / 供应商）
    SELECT count(*) INTO n_rows FROM pg_indexes
     WHERE tablename = 'biz_supplier_access'
       AND indexname IN ('uk_biz_sa_no', 'uk_biz_sa_supplier_open', 'idx_biz_sa_status', 'idx_biz_sa_supplier');
    IF n_rows <> 4 THEN
        RAISE EXCEPTION 'V0455 自检失败：biz_supplier_access 索引应有 4 个，实际 %', n_rows;
    END IF;

    -- 10.3 取号行在，且 shape 对
    SELECT count(*) INTO n_rows FROM biz_seq
     WHERE seq_type = 'SAC' AND prefix = 'SAC' AND date_pattern = 'yyyyMMdd'
       AND separator = '-' AND deleted_at IS NULL;
    IF n_rows <> 1 THEN
        RAISE EXCEPTION 'V0455 自检失败：biz_seq(SAC) 应有且只有 1 行，实际 %', n_rows;
    END IF;

    -- 10.4 流程四层在（定义 / 版本 / 5 节点 / 5 边 / 2 审批人）
    SELECT count(*) INTO n_rows FROM biz_flow_def
     WHERE flow_code = 'supplier_access' AND biz_type = 'SUPPLIER_ACCESS'
       AND status = 'PUBLISHED' AND is_default AND current_version_id = v_id AND deleted_at IS NULL;
    IF n_rows <> 1 THEN
        RAISE EXCEPTION 'V0455 自检失败：supplier_access 流程定义不对（应有 1 行已发布默认流程）';
    END IF;
    SELECT count(*) INTO n_rows FROM biz_flow_def_version
     WHERE id = v_id AND version_status = 'PUBLISHED' AND is_current
       AND node_count = 5 AND edge_count = 5
       AND graph_hash ~ '^[0-9a-f]{64}$';
    IF n_rows <> 1 THEN
        RAISE EXCEPTION 'V0455 自检失败：流程版本行不对（发布态 / 节点边计数 / hash 形状）';
    END IF;
    SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = v_id;
    IF n_rows <> 5 THEN
        RAISE EXCEPTION 'V0455 自检失败：节点应有 5 个，实际 %', n_rows;
    END IF;
    SELECT count(*) INTO n_rows FROM biz_flow_edge WHERE version_id = v_id;
    IF n_rows <> 5 THEN
        RAISE EXCEPTION 'V0455 自检失败：边应有 5 条，实际 %', n_rows;
    END IF;
    -- 条件边必须真的落在「拉黑」上（分级在流程定义里，不在代码里）
    SELECT count(*) INTO n_rows FROM biz_flow_edge
     WHERE version_id = v_id AND edge_key = 'e_type_blk'
       AND condition_expr->>'field' = 'form.access_type'
       AND condition_expr->>'cmp' = 'eq'
       AND condition_expr->>'value' = 'BLACKLIST'
       AND NOT is_default;
    IF n_rows <> 1 THEN
        RAISE EXCEPTION 'V0455 自检失败：拉黑条件边（form.access_type = BLACKLIST）不对';
    END IF;
    SELECT count(*) INTO n_rows FROM biz_flow_edge
     WHERE version_id = v_id AND is_default AND condition_expr IS NULL;
    IF n_rows <> 1 THEN
        RAISE EXCEPTION 'V0455 自检失败：兜底边应有 1 条且不带条件，实际 %', n_rows;
    END IF;
    SELECT count(*) INTO n_rows FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = v_id AND a.assignee_type = 'ROLE';
    IF n_rows <> 2 THEN
        RAISE EXCEPTION 'V0455 自检失败：ROLE 审批人应有 2 行（采购经理 / 财务经理），实际 %', n_rows;
    END IF;

    -- 10.5 菜单 5 行：1 页面 + 4 按钮；页面行父级是采购管理目录、标题 ≤ 12 字
    SELECT count(*) INTO n_rows FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000004484'::uuid,
                  '20000000-0000-0000-0000-000000004485'::uuid,
                  '20000000-0000-0000-0000-000000004486'::uuid,
                  '20000000-0000-0000-0000-000000004487'::uuid,
                  '20000000-0000-0000-0000-000000004488'::uuid)
       AND deleted_at IS NULL;
    IF n_rows <> 5 THEN
        RAISE EXCEPTION 'V0455 自检失败：供应商准入菜单应有 5 行（1 页面 + 4 按钮），实际 %', n_rows;
    END IF;
    SELECT count(*) INTO n_rows FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004484'::uuid
       AND menu_type = 2 AND auth_code = 'business:erp:supplier-access:page'
       AND parent_id = '20000000-0000-0000-0000-000000000946'::uuid
       AND path = '/business/erp/supplier-access' AND deleted_at IS NULL
       AND length(title) <= 12;
    IF n_rows <> 1 THEN
        RAISE EXCEPTION 'V0455 自检失败：页面行（显形码 / 父级 / path / 标题长度）不对';
    END IF;
    SELECT count(*) INTO n_rows FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000004484'::uuid
       AND menu_type = 3 AND deleted_at IS NULL
       AND auth_code IN ('business:erp:supplier-access:list',
                         'business:erp:supplier-access:create',
                         'business:erp:supplier-access:update',
                         'business:erp:supplier-access:submit');
    IF n_rows <> 4 THEN
        RAISE EXCEPTION 'V0455 自检失败：接口码按钮应有 4 行，实际 %', n_rows;
    END IF;
    -- 同父 sort_order 必须唯一（否则侧栏顺序每次刷新都变）
    SELECT count(*) INTO n_rows FROM (
        SELECT sort_order FROM sys_menu
         WHERE parent_id = '20000000-0000-0000-0000-000000004484'::uuid AND deleted_at IS NULL
         GROUP BY sort_order HAVING count(*) > 1) dup;
    IF n_rows <> 0 THEN
        RAISE EXCEPTION 'V0455 自检失败：按钮行 sort_order 有重复（% 组）', n_rows;
    END IF;

    -- 10.6 授权：PURCHASE_MANAGER 必须拿到全部 5 行（N20：除超管外至少一个角色能用）
    SELECT count(*) INTO n_rows FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE r.role_code = 'PURCHASE_MANAGER'
       AND rm.menu_id IN (SELECT unnest(ARRAY[
             '20000000-0000-0000-0000-000000004484'::uuid,
             '20000000-0000-0000-0000-000000004485'::uuid,
             '20000000-0000-0000-0000-000000004486'::uuid,
             '20000000-0000-0000-0000-000000004487'::uuid,
             '20000000-0000-0000-0000-000000004488'::uuid]));
    IF n_rows <> 5 THEN
        RAISE EXCEPTION 'V0455 自检失败：PURCHASE_MANAGER 应拿到 5 个菜单授权，实际 %', n_rows;
    END IF;

    RAISE NOTICE 'V0455 自检通过：供应商准入/黑名单（申请单 + SUPPLIER_ACCESS 流程 + 5 行菜单 + 授权）已就绪';
END $$;
