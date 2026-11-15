-- ============================================================================
-- V0381__cost_flow_def.up.sql
--
-- 成本结算审批流种子（卡 COST-B3-2 / t_bef918b4）：
--   flow_code = `cost_settle_approve`，biz_type = `COST_SETTLE`，
--   节点 = 开始 → **财务经理**（FINANCE_MANAGER …f0005）→ **财务总监**
--          （FINANCE_DIRECTOR …f0006）→ 结束，**两级顺序审批**。
--
-- 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
--   §4.2「审批流怎么接」、§7.1 V0378 行（号段已顺延，见下）、§8.1 的 B3-2。
--
-- ── 号段为什么是 V0381 ────────────────────────────────────────────────
--   设计稿把本卡排在 **V0378**，但 V0378/V0379/V0380 已被前置卡 B1-7 占用
--   （成本菜单权限 / 号段字典 / 批号回填，整体顺延 3 位）。nik 迁移加载器对版本号
--   做跨目录全局唯一校验，重号 = 服务启动即失败（AGENTS 约定 6），故本卡取 V0381。
--
-- ── 为什么是「两级」 ──────────────────────────────────────────────────
--   结算一次动**三张凭证 + 一批完工入库**，并把 4101 制造费用清零（§4.2）：
--     · 一级（财务经理）够了 ≠ 有制衡 —— 谁归集谁结算谁记账，一个人闭环；
--     · 三级（再加总经理）在月结 5 个工作日里必然卡单。
--   所以「财务经理（业务口径：归集/分摊/约当产量）→ 财务总监（账务口径：完工入库
--   与 4001.01~03 三张凭证）」。
--
-- ── 节点为什么是「或签」而不是全员会签 ────────────────────────────────
--   `approval_mode = 'ANY'` 显式落库（不靠 NULL 兜底）：角色型审批节点上
--   `ALL` 把「岗位上任一人签字」误读成「岗位上所有人都要点头」—— 只要有一个
--   成员停用/离职未摘角色，单据就永久卡 PENDING 且无待办、界面无按钮可点。
--   这正是 V0359 → V0367（多版本安全版）在生产上修掉的那个 P0。
--   本卡因此把 ANY 写成硬值，并在自检里断言「**没有** APPROVAL+ROLE+ALL 节点」。
--   要多人共识必须显式建 COUNTERSIGN 节点，不是把 APPROVAL 改成 ALL。
--
-- ── 自审批 ────────────────────────────────────────────────────────────
--   节点 `self_approval = 'ALLOW'`（与 V0332 采购退货 / V0191 财务作废同款）：
--   自审拦截由**桥侧** `erp::separation::guard_self_approval` 在审批动作上做
--   （§4.2「自审批」行），同时 `self_approval::over_limit` 会在金额超阈值时
--   把发起人从候选人里剔除。**刻意不写 FORBID**：FORBID 是「派发阶段」整单失败
--   （`assignee.rs` filter 阶段），财务经理自己制单的月结批次会直接派发不出来，
--   既没有审批人兜底也没有界面按钮 —— 就是 AGENTS 约定 11 说的死单。
--
-- ── 四重守卫（范本 = V0332 §⑦）────────────────────────────────────────
--   ① 已有 `flow_code = 'cost_settle_approve'` 的活流程        → 整条跳过
--   ② 已有 `biz_type = 'COST_SETTLE'` 的**默认**流程           → 整条跳过
--      （`uk_biz_flow_def_default` 是唯一索引；硬插会撞索引，且「发起时挑哪条」
--        不可判定）
--   ③ 两个审批角色（…f0005 / …f0006）不齐                      → 整条跳过
--      （客户库若停用了财务总监角色，种一条「派不出去」的流程比不种更糟）
--   ④ **不重复插 / 版本递增**：本卡版本以 `change_note LIKE 'V0381 种子%'` 与
--      `graph_hash` 双标记；重复执行不插第二份；万一需要补插（半途失败后重跑），
--      版本号取 `COALESCE(MAX(version_no),0)+1`，不撞 `uk_biz_flow_def_version`。
--   守卫跳过**只 NOTICE 不抛异常**：客户已经自建过同名/同类型流程时，
--   升级不能失败（覆盖客户配置＝更严重的事故）。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
--   重复执行 up：def 被守卫①拦下 → version 被守卫④a/④b 拦下 → 节点/边/审批人
--   （按 `version_id` 定位，且那次 version 插入是 0 行）也 0 行 ⇒ **结果不变**。
--   `graph_snapshot` / `graph_hash` 不是手写的：先按 `graph.rs::snapshot_json()`
--   同一套规则（递归按键字典序 + 紧凑无空白 + sha256）离线算出，再在 §5 自检里
--   用库内 canonical 化**重算一遍**对账（对不上就整份迁移回滚）。
--
-- ── 回滚 ──────────────────────────────────────────────────────────────
--   见 `V0381__cost_flow_def.down.sql`：有同 biz_type 的审批实例 / 结算批次已挂
--   实例 / 本流程下还有别的版本 → 拒绝回滚（删版本会留下指向空气的审批单）。
-- ============================================================================

-- ---------- §0 canonical JSON（与 graph.rs::write_canonical 逐字同义，沿用 V0359/V0367）----------
-- 用途：§5 自检把库里的 `graph_snapshot` 重新 canonical 化再取 sha256，与
-- `graph_hash` 逐字节对账。没有这一步，种子 hash 写错要到「流程图打开时报 drift」
-- 才发现（`flow::maintain` 的每日自检），那时已经上线了。
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
        -- COLLATE "C"：Rust 的 String Ord 是字节序；库默认 collation 会把 '_' 当弱标点
        -- 排到字母后面（`allow_robot` vs `allowRobot` 会换序 → hash 漂移）。
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
        -- 标量：jsonb 的 text 形式与 serde_json 一致（数字 / 布尔 / null / 字符串转义）
        RETURN v::text;
    END CASE;
END $fn$;

-- ---------- §1 前置护栏（结构性错误必须拦，客户配置差异只 NOTICE）----------
DO $$
DECLARE
    v_missing text;
    v_code    text;
    v_roles   integer;
BEGIN
    -- 1.1 流程五表必须先在（V0120 / V0121）
    SELECT string_agg(t, ', ') INTO v_missing
      FROM (VALUES ('biz_flow_def'), ('biz_flow_def_version'), ('biz_flow_node'),
                   ('biz_flow_edge'), ('biz_flow_node_assignee')) AS x(t)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.tables
                        WHERE table_schema = current_schema() AND table_name = x.t);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'COST-B3-2: 以下流程表未建成：%（请先执行 V0120/V0121 流程定义迁移）', v_missing;
    END IF;

    -- 1.2 本卡固定 def id 被**别的** flow_code 占用 ⇒ 结构性冲突，直接拦
    SELECT flow_code INTO v_code
      FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-0000000b0381'::uuid;
    IF v_code IS NOT NULL AND v_code <> 'cost_settle_approve' THEN
        RAISE EXCEPTION 'COST-B3-2: 固定流程 id …b0381 已被 flow_code=% 占用（不是本卡种的流程）；'
                        '本迁移不改已有流程定义行，请人工确认后重试', v_code;
    END IF;

    -- 1.3 客户自建的同名流程 / 同 biz_type 默认流 ⇒ 守卫会跳过（只提示，不拦）
    IF EXISTS (SELECT 1 FROM biz_flow_def
                WHERE flow_code = 'cost_settle_approve' AND deleted_at IS NULL
                  AND id <> '20000000-0000-4000-8000-0000000b0381'::uuid) THEN
        RAISE NOTICE 'COST-B3-2: 已存在 flow_code=cost_settle_approve 的流程（非本卡 id）→ 守卫①跳过，保留客户配置';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_flow_def
                WHERE biz_type = 'COST_SETTLE' AND is_default AND deleted_at IS NULL
                  AND id <> '20000000-0000-4000-8000-0000000b0381'::uuid) THEN
        RAISE NOTICE 'COST-B3-2: 已存在 biz_type=COST_SETTLE 的默认流程（非本卡 id）→ 守卫②跳过，保留客户配置';
    END IF;

    -- 1.4 审批角色（守卫③的输入，只提示）
    SELECT count(*) INTO v_roles
      FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0005'::uuid,
                  '20000000-0000-4000-8000-0000000f0006'::uuid)
       AND deleted_at IS NULL;
    IF v_roles <> 2 THEN
        RAISE NOTICE 'COST-B3-2: 审批角色只找到 %/2（FINANCE_MANAGER / FINANCE_DIRECTOR）→ 守卫③跳过，不种流程', v_roles;
    END IF;
END $$;

-- ---------- §2 流程定义（守卫 ①②③）----------
-- 免审（`allow_auto_approve`）恒为 FALSE：结算动库存与凭证，没有免审直过这种口径
-- （设计 §4.2 末行）。真正的「免审直过」分支一旦存在，就必须自带 AGENTS 约定 11
-- 要求的全套补偿逻辑，本流程选择从源头不给这个口子。
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000b0381'::uuid,
    'cost_settle_approve',
    '成本结算审批（标准·两级）',
    'COST_SETTLE',
    '财务管理',
    '成本结算批次提交审批：财务经理确认归集与分摊口径（料/工/费三路来源、约当产量、'
    '完工与在制拆分），财务总监确认完工入库数量与三张结转凭证（借 4001.01~03）。'
    '审批通过才允许结算（生成 PRODUCTION 入库单 + 凭证）；驳回或撤回打回 DRAFT，'
    '意见追加到结算批次 remark。审批人绑角色（FINANCE_MANAGER → FINANCE_DIRECTOR），'
    '两级、或签（角色内任一人签字即可），换人不用改代码。',
    'PUBLISHED',
    '20000000-0000-4000-8000-0000000b0382'::uuid,
    1,
    TRUE,
    FALSE,
    NULL
WHERE NOT EXISTS (
    -- 守卫①：同名流程已在（客户自建 / 上一次执行已种）
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'cost_settle_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    -- 守卫②：同 biz_type 已有默认流程（唯一索引 uk_biz_flow_def_default）
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'COST_SETTLE' AND is_default AND deleted_at IS NULL
)
AND (
    -- 守卫③：两个审批角色都在
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0005'::uuid,
                  '20000000-0000-4000-8000-0000000f0006'::uuid)
       AND deleted_at IS NULL
) = 2;

-- ---------- §3 版本快照（守卫④：不重复插 / 版本递增）----------
-- `graph_hash` = sha256(canonical JSON)。本卡的值由 `graph.rs::snapshot_json()`
-- 同规则离线算出（8a4fbd64…4171），§7 自检会用库内 canonical 化重算对账。
-- `is_current`：只有当该 def 还没有当前版本时才置 TRUE（绝不去抢客户现有版本）。
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000b0382'::uuid,
    d.id,
    -- 守卫④「版本递增」：MAX+1（多版本安全，同 V0367 教训 —— 写死 1 会撞客户已有的 v1）
    COALESCE((SELECT MAX(x.version_no) FROM biz_flow_def_version x WHERE x.def_id = d.id), 0) + 1,
    'PUBLISHED',
    NOT EXISTS (SELECT 1 FROM biz_flow_def_version x WHERE x.def_id = d.id AND x.is_current),
    '8a4fbd64cf5fb5cfc3b133ef2e4ff6103cd14a9afbabfae2b6f63f5845eb4171',
    $snapshot${
      "edges": [
        {"condition": null, "from": "n_fd", "is_default": false, "key": "e_fd_end", "label": null, "priority": 0, "to": "e"},
        {"condition": null, "from": "n_fm", "is_default": false, "key": "e_fm_fd", "label": null, "priority": 0, "to": "n_fd"},
        {"condition": null, "from": "s", "is_default": false, "key": "e_start", "label": null, "priority": 0, "to": "n_fm"}
      ],
      "nodes": [
        {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "OR", "key": "e", "max_remind_times": 3, "name": "结束", "node_type": "END", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 4, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 660, "y": 0},
        {"allow_robot": false, "approval_mode": "ANY", "approve_ratio": null, "assignee_type": "ROLE", "assignees": [{"assignee_type": "ROLE", "ref_id": "20000000-0000-4000-8000-0000000f0006", "sort_order": 0}], "cc_on_approved": false, "config": {}, "empty_assignee": "FALLBACK_ADMIN", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "n_fd", "max_remind_times": 2, "name": "财务总监审批", "node_type": "APPROVAL", "reject_veto": true, "remind_before_hours": null, "self_approval": "ALLOW", "sort_order": 3, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": 24, "x": 440, "y": 0},
        {"allow_robot": false, "approval_mode": "ANY", "approve_ratio": null, "assignee_type": "ROLE", "assignees": [{"assignee_type": "ROLE", "ref_id": "20000000-0000-4000-8000-0000000f0005", "sort_order": 0}], "cc_on_approved": false, "config": {}, "empty_assignee": "FALLBACK_ADMIN", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "n_fm", "max_remind_times": 2, "name": "财务经理审批", "node_type": "APPROVAL", "reject_veto": true, "remind_before_hours": null, "self_approval": "ALLOW", "sort_order": 2, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": 24, "x": 220, "y": 0},
        {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "s", "max_remind_times": 3, "name": "开始", "node_type": "START", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 1, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 0, "y": 0}
      ],
      "schema_version": 1
    }$snapshot$::jsonb,
    4,
    3,
    'V0381 种子：成本结算审批（财务经理 → 财务总监 两级）默认流程',
    now(),
    NULL
  FROM biz_flow_def d
 WHERE d.flow_code = 'cost_settle_approve'
   AND d.biz_type = 'COST_SETTLE'
   AND d.is_default
   AND d.deleted_at IS NULL
   -- 守卫④a：本卡版本已在 ⇒ 不重复插（幂等的核心）
   AND NOT EXISTS (
       SELECT 1 FROM biz_flow_def_version x
        WHERE x.def_id = d.id AND x.change_note LIKE 'V0381 种子%'
   )
   -- 守卫④b：同 hash 的版本已在（换了 change_note 也不重复发）
   AND NOT EXISTS (
       SELECT 1 FROM biz_flow_def_version x
        WHERE x.def_id = d.id
          AND x.graph_hash = '8a4fbd64cf5fb5cfc3b133ef2e4ff6103cd14a9afbabfae2b6f63f5845eb4171'
   )
ON CONFLICT DO NOTHING;

-- ---------- §4 节点（4 个）/ 边（3 条）/ 审批人绑定（2 条）----------
-- 三张表都按「本卡版本行存在」为条件：版本被守卫拦下时它们一起 0 行，
-- 不会留下「有节点没版本」的半成品。
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000b0391'::uuid, '20000000-0000-4000-8000-0000000b0382'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    -- 财务经理：或签（ANY），角色内任一人签字即过；角色空了兜底给管理员（FALLBACK_ADMIN），
    -- 24 小时未处理只提醒（REMIND），**不自动通过**（结算不许被超时静默放行）。
    ('20000000-0000-4000-8000-0000000b0392'::uuid, '20000000-0000-4000-8000-0000000b0382'::uuid, 'n_fm'::varchar, '财务经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000b0393'::uuid, '20000000-0000-4000-8000-0000000b0382'::uuid, 'n_fd'::varchar, '财务总监审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 440::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000b0394'::uuid, '20000000-0000-4000-8000-0000000b0382'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 660::int, 0::int, 4::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000b0382'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000b0395'::uuid, '20000000-0000-4000-8000-0000000b0382'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000b0391'::uuid, '20000000-0000-4000-8000-0000000b0392'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000b0396'::uuid, '20000000-0000-4000-8000-0000000b0382'::uuid, 'e_fm_fd'::varchar, '20000000-0000-4000-8000-0000000b0392'::uuid, '20000000-0000-4000-8000-0000000b0393'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000b0397'::uuid, '20000000-0000-4000-8000-0000000b0382'::uuid, 'e_fd_end'::varchar, '20000000-0000-4000-8000-0000000b0393'::uuid, '20000000-0000-4000-8000-0000000b0394'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000b0382'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 审批人 = 角色（不是「全员会签」、也不是写死某个人）：人换了不用改流程定义。
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000b0392'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000b0393'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0006'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000b0382'::uuid)
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- §5 def 的「当前版本」指针同步 ----------
-- 正常路径：§2 插入 def 时就写了 current_version_id/current_version_no=1，这里 UPDATE 0 行。
-- 兜底路径（半途失败后重跑、版本号被递增）：把指针与版本行对齐，避免「PUBLISHED 但指向
-- 不存在的版本」这种「能发起但没有图」的半成品。
UPDATE biz_flow_def d
   SET current_version_id = v.id,
       current_version_no = v.version_no,
       updated_at         = now()
  FROM biz_flow_def_version v
 WHERE v.id = '20000000-0000-4000-8000-0000000b0382'::uuid
   AND d.id = v.def_id
   AND d.deleted_at IS NULL
   AND (d.current_version_id IS NULL OR d.current_version_id = v.id);

-- ---------- §6 自检 ----------
DO $$
DECLARE
    v_missing   text;
    v_rows      integer;
    v_def       uuid := '20000000-0000-4000-8000-0000000b0381'::uuid;
    v_ver       uuid := '20000000-0000-4000-8000-0000000b0382'::uuid;
    v_cur       uuid;
    v_hash      text := '8a4fbd64cf5fb5cfc3b133ef2e4ff6103cd14a9afbabfae2b6f63f5845eb4171';
    v_recalc    text;
    v_name      text;
BEGIN
    -- 6.1 def 侧：存在即断言形态（被守卫跳过时不检查，只说明原因）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'cost_settle_approve' AND deleted_at IS NULL) THEN
        RAISE NOTICE 'COST-B3-2: cost_settle_approve 种子被守卫跳过（已有同名流程 / 已 COST_SETTLE 默认流 / 审批角色不齐），本迁移不改任何流程定义';
        RETURN;
    END IF;

    SELECT current_version_id INTO v_cur
      FROM biz_flow_def
     WHERE id = v_def AND deleted_at IS NULL;
    IF v_cur IS NULL THEN
        -- 本卡的 def 被守卫跳过、库里那条是客户的 → 不校验（也不许改它）
        RAISE NOTICE 'COST-B3-2: cost_settle_approve 由客户自建（非本卡 id …b0381），跳过自检';
        RETURN;
    END IF;

    -- 6.1 只有当「当前版本仍是本卡种的那一版」才做形态自检。
    --    客户在流程编辑器里改过图 / 发过新版之后，本卡的行已经不是出厂形态了 ——
    --    这时**只提示不报错**（覆盖或阻断客户配置比种子没生效更严重）。
    IF v_cur <> v_ver
       OR (SELECT change_note FROM biz_flow_def_version WHERE id = v_ver)
          IS DISTINCT FROM 'V0381 种子：成本结算审批（财务经理 → 财务总监 两级）默认流程' THEN
        RAISE NOTICE 'COST-B3-2: cost_settle_approve 已被客户改过（当前版本 %），跳过形态自检；本迁移不改客户配置', v_cur;
        RETURN;
    END IF;

    SELECT count(*) INTO v_rows
      FROM biz_flow_def
     WHERE id = v_def
       AND status = 'PUBLISHED'
       AND is_default
       AND allow_auto_approve = FALSE
       AND biz_type = 'COST_SETTLE'
       AND category = '财务管理'
       AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'COST-B3-2: 流程定义形态不对（应 PUBLISHED / is_default / allow_auto_approve=FALSE / biz_type=COST_SETTLE）';
    END IF;

    -- 6.2 def 指针指向的版本必须真实存在（「能发起但没有图」是死单的另一种形态）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = v_cur) THEN
        RAISE EXCEPTION 'COST-B3-2: biz_flow_def.current_version_id=% 指向不存在的版本', v_cur;
    END IF;

    -- 6.3 版本侧：PUBLISHED + 唯一当前版 + 节点/边计数
    SELECT count(*) INTO v_rows
      FROM biz_flow_def_version
     WHERE id = v_ver
       AND version_status = 'PUBLISHED'
       AND is_current
       AND node_count = 4
       AND edge_count = 3
       AND published_at IS NOT NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'COST-B3-2: 版本行形态不对（应 PUBLISHED / is_current / 4 节点 / 3 边 / 有发布时间）';
    END IF;

    -- 6.4 快照 ↔ graph_hash 自洽（库内 canonical 重算，防止手写 hash 与快照分叉）
    SELECT encode(sha256(convert_to(pg_temp.canonical_json(graph_snapshot), 'UTF8')), 'hex')
      INTO v_recalc
      FROM biz_flow_def_version
     WHERE id = v_ver;
    IF v_recalc IS DISTINCT FROM v_hash THEN
        RAISE EXCEPTION 'COST-B3-2: graph_hash 与快照重算不一致（库内 %，期望 %）', v_recalc, v_hash;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_def_version
     WHERE id = v_ver AND graph_hash = v_hash;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'COST-B3-2: 落库 graph_hash 不是期望值（列值 / 常量不一致）';
    END IF;

    -- 6.5 节点行 ↔ 快照一致（行数、类型序列）
    SELECT count(*) INTO v_rows FROM biz_flow_node WHERE version_id = v_ver;
    IF v_rows <> 4 THEN
        RAISE EXCEPTION 'COST-B3-2: 节点数 = %（期望 4）', v_rows;
    END IF;
    SELECT string_agg(node_type, ',' ORDER BY sort_order) INTO v_name
      FROM biz_flow_node WHERE version_id = v_ver;
    IF v_name <> 'START,APPROVAL,APPROVAL,END' THEN
        RAISE EXCEPTION 'COST-B3-2: 节点类型序列 = %（期望 START,APPROVAL,APPROVAL,END）', v_name;
    END IF;

    -- 6.6 ★ 两级审批：恰好 2 个 APPROVAL 节点，且都是「绑角色 + 或签 + 有兜底」
    --      ALL 出现即是「全员会签」事故（V0359/V0367 的 P0），这里必须一条都不许有。
    SELECT count(*) INTO v_rows
      FROM biz_flow_node
     WHERE version_id = v_ver
       AND node_type = 'APPROVAL'
       AND assignee_type = 'ROLE'
       AND approval_mode = 'ANY'
       AND empty_assignee = 'FALLBACK_ADMIN';
    IF v_rows <> 2 THEN
        RAISE EXCEPTION 'COST-B3-2: 「绑角色 + 或签 + 兜底」的审批节点数 = %（期望 2）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_node
     WHERE version_id = v_ver
       AND node_type IN ('APPROVAL', 'COUNTERSIGN')
       AND (approval_mode = 'ALL' OR approval_mode IS NULL);
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'COST-B3-2: 有 % 个节点是全员会签 / approval_mode 未落库（角色型审批必须是 ANY）', v_rows;
    END IF;

    -- 6.7 审批人绑定：n_fm → FINANCE_MANAGER，n_fd → FINANCE_DIRECTOR，各一条
    SELECT count(*) INTO v_rows
      FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = v_ver
       AND a.assignee_type = 'ROLE'
       AND ((n.node_key = 'n_fm' AND a.ref_id = '20000000-0000-4000-8000-0000000f0005'::uuid)
         OR (n.node_key = 'n_fd' AND a.ref_id = '20000000-0000-4000-8000-0000000f0006'::uuid));
    IF v_rows <> 2 THEN
        RAISE EXCEPTION 'COST-B3-2: 审批人绑定行数 = %（期望 n_fm→FINANCE_MANAGER、n_fd→FINANCE_DIRECTOR）', v_rows;
    END IF;

    -- 6.8 链路形状：s→n_fm→n_fd→e，且起终点度数正确
    SELECT count(*) INTO v_rows
      FROM biz_flow_edge e
      JOIN biz_flow_node f ON f.id = e.from_node_id
      JOIN biz_flow_node t ON t.id = e.to_node_id
     WHERE e.version_id = v_ver
       AND ((f.node_key = 's' AND t.node_key = 'n_fm' AND e.edge_key = 'e_start')
         OR (f.node_key = 'n_fm' AND t.node_key = 'n_fd' AND e.edge_key = 'e_fm_fd')
         OR (f.node_key = 'n_fd' AND t.node_key = 'e' AND e.edge_key = 'e_fd_end'));
    IF v_rows <> 3 THEN
        RAISE EXCEPTION 'COST-B3-2: 骨架边数 = %（期望 3：s→n_fm→n_fd→e）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_edge e
      JOIN biz_flow_node t ON t.id = e.to_node_id
     WHERE e.version_id = v_ver AND t.node_key = 's';
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'COST-B3-2: 开始节点有 % 条入边（起点头必须为 0）', v_rows;
    END IF;

    RAISE NOTICE 'COST-B3-2 自检通过：cost_settle_approve（PUBLISHED / 4 节点 / 3 边 / 两级或签 / 快照 hash 自洽）';
END $$;
