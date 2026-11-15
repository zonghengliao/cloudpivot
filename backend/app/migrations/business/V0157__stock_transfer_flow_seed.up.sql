-- ======================================================
-- V0157：种子流程 `stock_transfer`（库存调拨审批默认流程）
--        START(s) → 超管审批(n_approve) → END(e)
--
-- ── 为什么需要这条种子 ─────────────────────────────────
-- 调拨单的状态机（W3a/W3b）里，`DRAFT → PENDING` 时只做一件事：
-- 查 `biz_type='STOCK_TRANSFER'`（或 `flow_code='stock_transfer'`）的**已发布默认流程**，
-- 有就 `engine::start` 发起审批单并回写 `biz_stock_transfer.approval_instance_id`，
-- 没有就静默降级（允许人工 PENDING → APPROVED）。
--
-- 生产库里 `biz_flow_def` 只有 `task_accept` / `EXPENSE` / `OTHER` 三个流程，**没有调拨流**，
-- 于是：提交审批 → `approval_instance_id = NULL` → 审批人待办 0 条 → 单子永远停在
-- 「审批中」、只能撤回。用户看到的现象就是「调拨单卡住了、无法流转」。
--
-- 这条迁移把默认流程直接种成 **PUBLISHED + is_default**，做到「全新库 / 存量库一键可用」。
--
-- ── 数据来源（不是手写的）─────────────────────────────
-- 图与快照来自一次**真实发布**（隔离库 cloudpivot_p5 + 隔离实例 18096，2026-10-31）：
--   `POST /business/flow/def`（flow_code=stock_transfer / biz_type=STOCK_TRANSFER / is_default=true）
--   → `PUT /business/flow/version/<vid>/graph`（START → APPROVAL → END）
--   → `POST /business/flow/def/<id>/publish`
-- 然后把 `biz_flow_def / biz_flow_def_version / biz_flow_node / biz_flow_edge` 四张表按行导出，
-- 只改 id / 时间戳 / 操作人（换成固定 UUID、now()、NULL），图内容一字未动。
-- 因此 `graph_snapshot`（引擎唯一的图来源）与 `graph_hash` 必然自洽：
--   graph_hash = sha256(canonical_json(graph_snapshot)) = e1947064684e326c4ad4248e354d0215f1a3f3a7f668fb467aefe854adbd90da
--
-- ── 审批人为什么是 ADMIN（超管）而不是某个角色 ──────────
-- 卡面写「超管 or 仓管主管」。仓管主管在本仓没有对应 `sys_role`（种子角色只有
-- SUPER_ADMIN / DEV_MONITOR_VIEWER / TEST_UADMIN），用 `ROLE` + role_code 会在
-- 「角色不存在 / 角色里 0 人」时空审批人 → 节点被跳过 → 审批**自动通过**（比卡住更坏）。
-- `ADMIN`（`flow/assignee.rs` 的 8+2 种来源之一）由运行时展开「所有 SUPER_ADMIN 用户」，
-- 保证任何环境都有人收得到待办；另配 `empty_assignee=FALLBACK_ADMIN` 兜底。
-- 管理员想要「仓管主管审」时，在「流程定义」里另存新版本、把节点审批人改成角色即可，
-- 在跑的单子不受影响（发布后不可改）。
--
-- `self_approval=ALLOW`：超管自己提的单子也要能看到自己的待办并点通过，
-- 否则种子流程会把「只有超管一个账号」的环境卡死。
--
-- ── 幂等与安全 ────────────────────────────────────────
-- 四层 `WHERE NOT EXISTS` / `EXISTS` 守卫：已存在 `flow_code='stock_transfer'` 的未删流程
-- （客户自己建过）就整条跳过，绝不覆盖；每一步都以上一步落地为前提，不会留半张图。
-- 末尾 DO 块自检：PUBLISHED 三件套、`is_current` 唯一性、节点/边数量、审批人来源。
-- 回滚：`V0157__stock_transfer_flow_seed.down.sql`（只删固定 UUID 的那一套，零残留断言）。
--
-- ── 乱序补迁 ──────────────────────────────────────────
-- 存量库里 `schema_migrations` 已到 V0156，本迁移是顺号，不会触发「乱序补迁」WARN；
-- dev 实例（`cargo-watch -w migrations`）会在文件落地后自动重启并应用本迁移。
-- ======================================================

-- ---------- 1) 流程定义（PUBLISHED + 默认） ----------
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-00000000d157', 'stock_transfer', '库存调拨审批', 'STOCK_TRANSFER', 'erp',
    '库存调拨单提交审批时的默认流程：提交（发起）→ 超管审批 → 结束。'
    '管理员可在「流程定义」里另存新版本调整审批人（改老单不受影响）。',
    'PUBLISHED', '20000000-0000-4000-8000-00000000e157', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'stock_transfer' AND deleted_at IS NULL
)
-- 同 biz_type 至多一个默认流程（`uk_biz_flow_def_default (biz_type) WHERE is_default AND deleted_at IS NULL`）：
-- 客户若已把「另一个编码」的调拨流程设成默认，本迁移整条跳过 —— 抢默认会在 INSERT 上撞 23505，
-- 让整批迁移失败（启动失败），而那属于「客户自己配好了」，不该由种子去争。
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'STOCK_TRANSFER' AND is_default AND deleted_at IS NULL
);

-- ---------- 2) 版本行（PUBLISHED + is_current + 快照/hash） ----------
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-00000000e157', '20000000-0000-4000-8000-00000000d157', 1, 'PUBLISHED', TRUE,
    'e1947064684e326c4ad4248e354d0215f1a3f3a7f668fb467aefe854adbd90da',
    '{"edges": [{"condition": null, "from": "n_approve", "is_default": false, "key": "e2", "label": null, "priority": 0, "to": "e"}, {"condition": null, "from": "s", "is_default": false, "key": "e1", "label": null, "priority": 0, "to": "n_approve"}], "nodes": [{"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "e", "max_remind_times": 3, "name": "结束", "node_type": "END", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 3, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 480, "y": 0}, {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": "ADMIN", "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "FALLBACK_ADMIN", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "n_approve", "max_remind_times": 2, "name": "超管审批", "node_type": "APPROVAL", "reject_veto": true, "remind_before_hours": null, "self_approval": "ALLOW", "sort_order": 2, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": 48, "x": 240, "y": 0}, {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "s", "max_remind_times": 3, "name": "开始", "node_type": "START", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 1, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 0, "y": 0}], "schema_version": 1}'::jsonb,
    3, 2, 'V0157 种子：库存调拨审批默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-00000000d157')
ON CONFLICT (def_id, version_no) DO NOTHING;

-- ---------- 3) 节点（3 个：START / APPROVAL / END） ----------
-- 列顺序与 V0127 完全一致（`SELECT * FROM (VALUES …)` 依赖位置），
-- 值逐列取自一次真实发布的 `biz_flow_node` 导出。
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-00000000a157'::uuid, '20000000-0000-4000-8000-00000000e157'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-00000000a158'::uuid, '20000000-0000-4000-8000-00000000e157'::uuid, 'n_approve'::varchar, '超管审批'::varchar, 'APPROVAL'::varchar, 'ADMIN'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 240::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-00000000a159'::uuid, '20000000-0000-4000-8000-00000000e157'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 480::int, 0::int, 3::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-00000000e157')
ON CONFLICT (version_id, node_key) DO NOTHING;

-- ---------- 4) 连线（2 条：s → n_approve → e） ----------
INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-00000000b157'::uuid, '20000000-0000-4000-8000-00000000e157'::uuid, 'e1'::varchar, '20000000-0000-4000-8000-00000000a157'::uuid, '20000000-0000-4000-8000-00000000a158'::uuid, 'null'::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-00000000b158'::uuid, '20000000-0000-4000-8000-00000000e157'::uuid, 'e2'::varchar, '20000000-0000-4000-8000-00000000a158'::uuid, '20000000-0000-4000-8000-00000000a159'::uuid, 'null'::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-00000000e157')
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 注：审批人不在 `biz_flow_node_assignee` 里 —— `ADMIN` 由运行时展开（见文件头），
-- 可枚举来源（USER / DEPT_LEADER）才需要行。

-- ---------- 5) 结构自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    m_rows int;
    v_bad  int;
    n_rows int;
    e_rows int;
BEGIN
    -- 若客户已有同名流程（守卫整条跳过），本套固定 UUID 不存在，跳过自检
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-00000000d157') THEN
        IF EXISTS (SELECT 1 FROM biz_flow_def
                    WHERE flow_code = 'stock_transfer' AND deleted_at IS NULL) THEN
            RAISE NOTICE 'V0157：已存在 flow_code=stock_transfer 的流程，种子跳过（未插入）';
        ELSE
            RAISE NOTICE 'V0157：已存在 biz_type=STOCK_TRANSFER 的其它默认流程，种子跳过（未插入，避免撞 uk_biz_flow_def_default）';
        END IF;
        RETURN;
    END IF;

    SELECT count(*) INTO m_rows FROM biz_flow_def d
     WHERE d.id = '20000000-0000-4000-8000-00000000d157' AND d.flow_code = 'stock_transfer'
       AND d.biz_type = 'STOCK_TRANSFER' AND d.status = 'PUBLISHED'
       AND d.is_default AND d.current_version_id = '20000000-0000-4000-8000-00000000e157'
       AND d.deleted_at IS NULL;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0157 自检失败：种子定义行不满足 PUBLISHED/默认/当前版本 四条件';
    END IF;

    SELECT count(*) INTO v_bad FROM biz_flow_def_version v
     WHERE v.id = '20000000-0000-4000-8000-00000000e157' AND v.def_id = '20000000-0000-4000-8000-00000000d157'
       AND v.version_status = 'PUBLISHED' AND v.is_current
       AND v.graph_hash = 'e1947064684e326c4ad4248e354d0215f1a3f3a7f668fb467aefe854adbd90da'
       AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
       AND v.node_count = 3 AND v.edge_count = 2;
    IF v_bad <> 1 THEN
        RAISE EXCEPTION 'V0157 自检失败：版本行缺少 PUBLISHED 三件套（快照/hash/发布时间）或计数不对';
    END IF;

    SELECT count(*) INTO n_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-00000000e157';
    SELECT count(*) INTO e_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-00000000e157';
    IF n_rows <> 3 OR e_rows <> 2 THEN
        RAISE EXCEPTION 'V0157 自检失败：节点/边数量不对（nodes=% edges=%）', n_rows, e_rows;
    END IF;

    -- 审批节点必须有人可派：ADMIN 来源 + 空审批人不许是「静默跳过」
    SELECT count(*) INTO m_rows FROM biz_flow_node n
     WHERE n.version_id = '20000000-0000-4000-8000-00000000e157' AND n.node_key = 'n_approve'
       AND n.node_type = 'APPROVAL' AND n.assignee_type = 'ADMIN'
       AND n.empty_assignee = 'FALLBACK_ADMIN' AND n.self_approval = 'ALLOW';
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0157 自检失败：审批节点的审批人来源/空审批人策略不符合种子口径';
    END IF;

    -- 每个 def 至多一个 current 版本（部分唯一索引的语义复核）
    SELECT count(*) INTO m_rows FROM biz_flow_def_version
     WHERE def_id = '20000000-0000-4000-8000-00000000d157' AND is_current;
    IF m_rows <> 1 THEN
        RAISE EXCEPTION 'V0157 自检失败：is_current 版本数为 %（应恰为 1）', m_rows;
    END IF;
END $$;
