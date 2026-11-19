-- ============================================================================
-- V0465__cost_standard_publish.up.sql
--
-- 标准成本卡「发布（审核后生效）」—— M5-10 / `t_a7d7a2d5`。
--
-- 本迁移做两件事：
--   ① `biz_cost_standard` 加**发布轴**三列 + 实例指针（DRAFT / PENDING / PUBLISHED）；
--   ② 种一条默认审批流程 `cost_standard_publish_approve`（biz_type = COST_STANDARD_PUBLISH），
--      节点 = 开始 → **财务经理**（FINANCE_MANAGER …f0005）→ 结束，**一级或签**。
--
-- ── 为什么加「发布轴」而不是复用 `status` ────────────────────────────────
--   `status`（1/0）的语义是「这一版参与期间取数」（V0374 的注释就是这么写的，
--   M5-08 / M5-09 的取数、重叠护栏、差异分析全指着它）。把「草稿 / 审批中」叠进去，
--   会让「审批中」在差异分析里被算成「已生效」—— 那正是 M5-08 / M5-09 交接时
--   点名要避免的事（见两张卡的 evidence README「给下游卡的接缝」）。
--   所以两条轴各管一件事：
--     · `status`          = 参与期间取数（M5-11+ 的 `effective` 只看它）
--     · `publish_state`   = 发布审批走到哪一步（本卡新增）
--   取数口径 = **两条都满足**（`status = 1 AND publish_state = 'PUBLISHED'`）。
--
-- ── 存量行为什么直接算「已发布」（DEFAULT 'PUBLISHED'）────────────────
--   升级前建的卡没有审批痕迹，也无法追溯「当时谁批的」。把它们降级成草稿 = 让
--   已经在算的标准突然失效（差异报表当天全员无依据）；而标记成已发布与升级前的
--   实际行为**逐字一致**（升级前就是建完即生效）。所以 DEFAULT 取 'PUBLISHED'，
--   新卡由 handler 显式写 'DRAFT'。
--
-- ── 为什么流程只有一级（财务经理）而不是像结算那样两级 ──────────────────
--   ① 发布标准卡**不动账、不出凭证**（ARCH-9 D-15：标准成本只用于差异分析），
--      风险等级低于结算（结算一次动三张凭证 + 一批完工入库，所以 V0381 是两级）；
--   ② 发布错了**可逆**：失效即退出取数、且不影响已落库的差异行，重发一版即可；
--   ③ 月初换版是定动作，两级会把它压到月结 5 个工作日里卡单。
--   制单与审批分离仍由桥侧 `guard_self_approval` 兜（`self_approval = 'ALLOW'` +
--   事务前 403 拦截，与本库其余 15 条审批线同款），不是靠「多一级」代替内控。
--
-- ── 幂等（up 连跑两遍结果不变）────────────────────────────────────────
--   列：`ADD COLUMN IF NOT EXISTS`；约束：先查 `pg_constraint` 再加；
--   流程：四重守卫（同名流程 / 同 biz_type 默认流 / 审批角色不齐 / 版本已种），
--   守卫跳过只 NOTICE 不抛异常（客户自建过就不动它）。
--
-- ── 回滚 ─────────────────────────────────────────────────────────────
--   见 `V0465__cost_standard_publish.down.sql`：有 COST_STANDARD_PUBLISH 实例 /
--   有卡还挂在审批中 → 拒绝回滚（删列会把「审批中」的卡变成无主单）。
-- ============================================================================

-- ---------- §0 canonical JSON（与 graph.rs::write_canonical 逐字同义，沿用 V0359/V0367/V0381）----------
CREATE OR REPLACE FUNCTION pg_temp.canonical_json(v jsonb) RETURNS text
LANGUAGE plpgsql IMMUTABLE AS $fn$
DECLARE
    out   text;
    k     text;
    item  jsonb;
    first boolean := true;
BEGIN
    CASE jsonb_typeof(v)
    WHEN 'object' THEN
        out := '{';
        -- COLLATE "C"：Rust 的 String Ord 是字节序（与 V0381 同一理由）
        FOR k, item IN SELECT e.key, e.value FROM jsonb_each(v) e ORDER BY e.key COLLATE "C" LOOP
            IF NOT first THEN out := out || ','; END IF;
            first := false;
            out := out || to_jsonb(k)::text || ':' || pg_temp.canonical_json(item);
        END LOOP;
        RETURN out || '}';
    WHEN 'array' THEN
        out := '[';
        FOR item IN SELECT a.value FROM jsonb_array_elements(v) a LOOP
            IF NOT first THEN out := out || ','; END IF;
            first := false;
            out := out || pg_temp.canonical_json(item);
        END LOOP;
        RETURN out || ']';
    ELSE
        RETURN v::text;
    END CASE;
END $fn$;

-- ---------- §1 前置护栏 ----------
DO $$
DECLARE
    v_missing text;
    v_roles   integer;
BEGIN
    -- 1.1 标准成本卡表必须在（V0374）
    IF NOT EXISTS (SELECT 1 FROM information_schema.tables
                    WHERE table_schema = current_schema() AND table_name = 'biz_cost_standard') THEN
        RAISE EXCEPTION 'M5-10: biz_cost_standard 不存在（请先执行 V0374 标准成本三表迁移）';
    END IF;
    -- 1.2 流程五表必须在（V0120 / V0121）
    SELECT string_agg(t, ', ') INTO v_missing
      FROM (VALUES ('biz_flow_def'), ('biz_flow_def_version'), ('biz_flow_node'),
                   ('biz_flow_edge'), ('biz_flow_node_assignee')) AS x(t)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.tables
                        WHERE table_schema = current_schema() AND table_name = x.t);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'M5-10: 以下流程表未建成：%（请先执行 V0120/V0121 流程定义迁移）', v_missing;
    END IF;
    -- 1.3 审批角色（守卫③的输入，只提示不拦）
    SELECT count(*) INTO v_roles
      FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0005'::uuid AND deleted_at IS NULL;
    IF v_roles <> 1 THEN
        RAISE NOTICE 'M5-10: 审批角色 FINANCE_MANAGER 不存在 → 守卫③跳过，不种发布审批流程（发布接口会 409 提示去发布流程）';
    END IF;
END $$;

-- ---------- §2 发布轴三列 + 实例指针 ----------
ALTER TABLE biz_cost_standard
    ADD COLUMN IF NOT EXISTS publish_state       VARCHAR(16) NOT NULL DEFAULT 'PUBLISHED',
    ADD COLUMN IF NOT EXISTS publish_instance_id UUID,
    ADD COLUMN IF NOT EXISTS published_at        TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS published_by        UUID;

COMMENT ON COLUMN biz_cost_standard.publish_state IS
    '发布状态：DRAFT（草稿，待提交）/ PENDING（审批中）/ PUBLISHED（已发布，可参与取数）。与 status 是两条正交的轴 —— status=参与期间取数，本列=审批走到哪一步；取数要求两者同时满足（M5-10）';
-- 审批中心 / 待办的「单据号」：本表**没有单号列**（V0374 的卡头只有物料 + 版本号），
-- `erp::doc_reader_impl::ErpDoc::CostStandardPublish` 的 DocSpec 用列表达式
-- `('STD-v' || version_no::text)` 拼一个可读标识（`doc_spec_identifiers_exist_in_migrations`
-- 会在这里逐字核对，改表达式必须同步改那一处）。
COMMENT ON COLUMN biz_cost_standard.publish_instance_id IS
    '发布审批实例（PENDING 必填；PUBLISHED 保留最近一次的实例 id 作审计；驳回 / 撤回清空，与采购退货 V0332 同口径）';
COMMENT ON COLUMN biz_cost_standard.published_at IS '最近一次审批通过（发布）的时刻';
COMMENT ON COLUMN biz_cost_standard.published_by IS '最近一次发布时的审批人';

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_cost_std_publish'
                      AND conrelid = 'biz_cost_standard'::regclass) THEN
        ALTER TABLE biz_cost_standard
            ADD CONSTRAINT ck_biz_cost_std_publish
            CHECK (publish_state IN ('DRAFT', 'PENDING', 'PUBLISHED'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_cost_std_publish_inst'
                      AND conrelid = 'biz_cost_standard'::regclass) THEN
        -- 审批中必须有实例：否则详情页显示「审批中」却点不进任何审批单（死单形态）
        ALTER TABLE biz_cost_standard
            ADD CONSTRAINT ck_biz_cost_std_publish_inst
            CHECK (publish_state <> 'PENDING' OR publish_instance_id IS NOT NULL);
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_biz_cost_std_publish_state
    ON biz_cost_standard (publish_state) WHERE deleted_at IS NULL;

-- ---------- §3 流程定义（守卫 ①②③）----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000d0465'::uuid,
    'cost_standard_publish_approve',
    '标准成本发布审批（标准·一级）',
    'COST_STANDARD_PUBLISH',
    '财务管理',
    '标准成本卡提交发布审批：财务经理确认这一版的标准 BOM、标准工时与工资率口径，'
    '通过后该版才参与差异分析取数（status=1 且 publish_state=PUBLISHED）。'
    '不通过或撤回打回草稿（可改后重提）。审批人绑角色（FINANCE_MANAGER），'
    '一级、或签（角色内任一人签字即可），换人不用改代码。',
    'PUBLISHED',
    '20000000-0000-4000-8000-0000000d0466'::uuid,
    1,
    TRUE,
    FALSE,
    NULL
WHERE NOT EXISTS (
    -- 守卫①：同名流程已在（客户自建 / 上一次执行已种）
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'cost_standard_publish_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    -- 守卫②：同 biz_type 已有默认流程（唯一索引 uk_biz_flow_def_default）
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'COST_STANDARD_PUBLISH' AND is_default AND deleted_at IS NULL
)
AND (
    -- 守卫③：审批角色在
    SELECT count(*) FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0005'::uuid AND deleted_at IS NULL
) = 1;

-- ---------- §4 版本快照（守卫④：不重复插 / 版本递增）----------
-- graph_hash = sha256(canonical JSON)，按 graph.rs::snapshot_json() 同规则离线算出，
-- §7 自检会用库内 canonical 化重算对账（对不上整份迁移回滚）。
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000d0466'::uuid,
    d.id,
    COALESCE((SELECT MAX(x.version_no) FROM biz_flow_def_version x WHERE x.def_id = d.id), 0) + 1,
    'PUBLISHED',
    NOT EXISTS (SELECT 1 FROM biz_flow_def_version x WHERE x.def_id = d.id AND x.is_current),
    'd1f9e769e7e91ca920832928f2e47aca3477805ad7b39ebca68ee1a7e1581f1e',
    $snapshot${
      "edges": [
        {"condition": null, "from": "s", "is_default": false, "key": "e_start", "label": null, "priority": 0, "to": "n_fm"},
        {"condition": null, "from": "n_fm", "is_default": false, "key": "e_fm_end", "label": null, "priority": 0, "to": "e"}
      ],
      "nodes": [
        {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "OR", "key": "s", "max_remind_times": 3, "name": "开始", "node_type": "START", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 1, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 0, "y": 0},
        {"allow_robot": false, "approval_mode": "ANY", "approve_ratio": null, "assignee_type": "ROLE", "assignees": [{"assignee_type": "ROLE", "ref_id": "20000000-0000-4000-8000-0000000f0005", "sort_order": 0}], "cc_on_approved": false, "config": {}, "empty_assignee": "FALLBACK_ADMIN", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "n_fm", "max_remind_times": 2, "name": "财务经理审批", "node_type": "APPROVAL", "reject_veto": true, "remind_before_hours": null, "self_approval": "ALLOW", "sort_order": 2, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": 24, "x": 220, "y": 0},
        {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "OR", "key": "e", "max_remind_times": 3, "name": "结束", "node_type": "END", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 3, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 440, "y": 0}
      ],
      "schema_version": 1
    }$snapshot$::jsonb,
    3,
    2,
    'V0465 种子：标准成本发布审批（财务经理 一级）默认流程',
    now(),
    NULL
  FROM biz_flow_def d
 WHERE d.flow_code = 'cost_standard_publish_approve'
   AND d.biz_type = 'COST_STANDARD_PUBLISH'
   AND d.is_default
   AND d.deleted_at IS NULL
   AND NOT EXISTS (
       SELECT 1 FROM biz_flow_def_version x
        WHERE x.def_id = d.id AND x.change_note LIKE 'V0465 种子%'
   )
   AND NOT EXISTS (
       SELECT 1 FROM biz_flow_def_version x
        WHERE x.def_id = d.id
          AND x.graph_hash = 'd1f9e769e7e91ca920832928f2e47aca3477805ad7b39ebca68ee1a7e1581f1e'
   )
ON CONFLICT DO NOTHING;

-- ---------- §5 节点（3 个）/ 边（2 条）/ 审批人绑定（1 条）----------
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0471'::uuid, '20000000-0000-4000-8000-0000000d0466'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0472'::uuid, '20000000-0000-4000-8000-0000000d0466'::uuid, 'n_fm'::varchar, '财务经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0473'::uuid, '20000000-0000-4000-8000-0000000d0466'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 440::int, 0::int, 3::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0466'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0474'::uuid, '20000000-0000-4000-8000-0000000d0466'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d0471'::uuid, '20000000-0000-4000-8000-0000000d0472'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0475'::uuid, '20000000-0000-4000-8000-0000000d0466'::uuid, 'e_fm_end'::varchar, '20000000-0000-4000-8000-0000000d0472'::uuid, '20000000-0000-4000-8000-0000000d0473'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0466'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0472'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0466'::uuid)
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- §6 def 的「当前版本」指针同步（半途失败后重跑兜底）----------
UPDATE biz_flow_def d
   SET current_version_id = v.id,
       current_version_no = v.version_no,
       updated_at         = now()
  FROM biz_flow_def_version v
 WHERE v.id = '20000000-0000-4000-8000-0000000d0466'::uuid
   AND d.id = v.def_id
   AND d.deleted_at IS NULL
   AND (d.current_version_id IS NULL OR d.current_version_id = v.id);

-- ---------- §7 自检 ----------
DO $$
DECLARE
    v_def    uuid := '20000000-0000-4000-8000-0000000d0465'::uuid;
    v_ver    uuid := '20000000-0000-4000-8000-0000000d0466'::uuid;
    v_hash   text := 'd1f9e769e7e91ca920832928f2e47aca3477805ad7b39ebca68ee1a7e1581f1e';
    v_cur    uuid;
    v_recalc text;
    v_nodes  integer;
    v_edges  integer;
    v_cnt    integer;
BEGIN
    -- 7.1 列与约束（无条件检查：本迁移的核心交付）
    SELECT count(*) INTO v_cnt
      FROM information_schema.columns
     WHERE table_schema = current_schema() AND table_name = 'biz_cost_standard'
       AND column_name IN ('publish_state', 'publish_instance_id', 'published_at', 'published_by');
    IF v_cnt <> 4 THEN
        RAISE EXCEPTION 'M5-10: biz_cost_standard 的发布轴四列只建成 %/4', v_cnt;
    END IF;
    -- 存量行必须是「已发布」（升级不能让在算的标准突然失效）
    SELECT count(*) INTO v_cnt FROM biz_cost_standard WHERE publish_state NOT IN ('DRAFT', 'PENDING', 'PUBLISHED');
    IF v_cnt > 0 THEN
        RAISE EXCEPTION 'M5-10: biz_cost_standard 有 % 行的 publish_state 落在三值域之外', v_cnt;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_cost_std_publish'
                      AND conrelid = 'biz_cost_standard'::regclass) THEN
        RAISE EXCEPTION 'M5-10: ck_biz_cost_std_publish 未建立';
    END IF;

    -- 7.2 流程（被守卫跳过时只说明原因，不校验也不改客户配置）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'cost_standard_publish_approve' AND deleted_at IS NULL) THEN
        RAISE NOTICE 'M5-10: cost_standard_publish_approve 种子被守卫跳过（已有同名流程 / 已有默认流 / 审批角色不齐），本迁移不改任何流程定义';
        RETURN;
    END IF;
    SELECT current_version_id INTO v_cur
      FROM biz_flow_def WHERE id = v_def AND deleted_at IS NULL;
    IF v_cur IS NULL THEN
        RAISE NOTICE 'M5-10: cost_standard_publish_approve 由客户自建（非本卡 id …d0465），跳过自检';
        RETURN;
    END IF;

    SELECT count(*), (SELECT count(*) FROM biz_flow_edge WHERE version_id = v_ver)
      INTO v_nodes, v_edges
      FROM biz_flow_node WHERE version_id = v_ver;
    IF v_nodes <> 3 OR v_edges <> 2 THEN
        RAISE EXCEPTION 'M5-10: 发布流程图为 % 节点 / % 边，应为 3 / 2', v_nodes, v_edges;
    END IF;
    -- 或签必须显式落库（角色型节点写 ALL 会让「上任一人签字」被误读成「全员点头」→ 死单）
    IF EXISTS (SELECT 1 FROM biz_flow_node
                WHERE version_id = v_ver AND node_type = 'APPROVAL'
                  AND (approval_mode IS DISTINCT FROM 'ANY')) THEN
        RAISE EXCEPTION 'M5-10: 发布流程的审批节点必须是 ANY（或签），不允许 ALL';
    END IF;

    -- 7.3 graph_hash 与库内 canonical 化重算逐字节对账（种子 hash 写错 = 上线后流程图 drift）
    SELECT pg_temp.canonical_json(graph_snapshot) INTO v_recalc
      FROM biz_flow_def_version WHERE id = v_ver;
    IF v_recalc IS NULL THEN
        RAISE EXCEPTION 'M5-10: 版本行 …d0466 不存在（节点/边已种但版本缺失 = 能发起但没有图）';
    END IF;
    IF encode(sha256(convert_to(v_recalc, 'UTF8')), 'hex') <> v_hash THEN
        RAISE EXCEPTION 'M5-10: graph_hash 对不上（库内重算 %），种子快照被改过',
            encode(sha256(convert_to(v_recalc, 'UTF8')), 'hex');
    END IF;
END $$;
