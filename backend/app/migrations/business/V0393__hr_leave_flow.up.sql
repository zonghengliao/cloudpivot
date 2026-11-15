-- ============================================================================
-- V0393__hr_leave_flow.up.sql
--
-- 请假审批流种子（卡 HR-26 / t_10899fe4）：
--   flow_code = `hr_leave`，biz_type = `HR_LEAVE`，
--   节点 = 开始 → **直属上级**（INITIATOR_SUPERIOR，发起人本级部门负责人）
--          → 请假天数判定（≥3 天？）
--             ├─ 是 → **人事经理**（HR_MANAGER …f0016）→ 结束
--             └─ 否 → 结束（直属上级终审）
--
-- 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md`
--   §3.2 ⑨「审批流程（4 套：定义 + 版本 + 图 + hash）」的 `hr_leave` 行
--   （直属上级 →（≥3 天）人事经理 → END，理由：长假要人事复核年假余额 / 产假证明）、
--   §7.1 的 `hr_leave_flow` 行（四重守卫）、§8.1 的 HR-26 行、
--   §4 的「自审批」行（员工不能批自己的请假单，`INITIATOR_SUPERIOR` 解析不到走
--   `FALLBACK_ADMIN`，**不用 `SKIP`** —— SKIP 会把「没人可派」变成自动通过，V0243 写死过）。
--
-- ── 号段为什么是 V0393 而不是卡面写的 V0384 / 设计映射的 V0390 ────────────
--   ① 设计号 V0384 已被 HR 系列自己的 `V0384__hr_accounts` 实占（HR-02）；
--   ② 设计文档 §7.1 第二次号段修正把本卡映射到 **V0390**，但 V0390 又被成本核算的
--      菜单分组迁移（`V0390__cost_menu_group`）实占，V0391 / V0392 亦然
--      （成本流程 / HR-23 考勤锁定守卫）。
--   nik 迁移加载器对版本号做**跨目录全局唯一**校验，重号 = 服务启动即失败
--   （AGENTS 约定 6 / 本仓 V0359 事故的镜像）。故本卡取当前最大号 +1 = **V0393**。
--   规律：先落地者定号，后落地者避让。批 5 的 V0393/V0394（HR-35/HR-45）顺延为
--   V0395/V0396 —— 已在本文件落地时同步回填设计文档 §7.1 / §8.1。
--
-- ── 为什么「≥3 天」写成条件边，而不是代码里的 if/else ────────────────────
--   阈值属于**客户可配置的业务口径**（年假余额 / 产假证明的复核成本与假期长度挂钩，
--   每个客户的人力制度不同）。写成 `biz_flow_edge.condition_expr` 之后，HR 在
--   「审批流程」编辑器里就能改阈值，不需要改代码、不需要发版 —— 与 V0331 采购申请
--   「预算内 / 超预算」两档同款做法。
--
-- ── ★ 条件字段为什么是 `form.duration_days_num`（而不是 `form.duration_days`）──
--   `leave_handler::leave_form_data` 冻结的 `duration_days` 是 **JSON 字符串**
--   （`"1.5"`，小数口径与出参一致，前端不做浮点）。而条件求值器
--   （`flow::graph::compare`）对 `gt/gte/lt/lte` 一律 `Value::as_f64()`：
--   左值是字符串 ⇒ `EvalError::TypeMismatch` ⇒ 引擎按「条件不成立」**静默走兜底边**
--   （只有 WARN 日志）—— 于是「≥3 天要人事复核」永远不成立，长假被直属上级一签直过，
--   **这是静默绕过复核的错**，不是「配错了会报错」。
--   故本卡在 `leave_form_data` 里补一个**数值镜像** `duration_days_num`（JSON number，
--   与 `duration_days` 同源同值），条件边只认它。两处的一致性由两边的单测钉死：
--     · `hr::leave_handler::tests::form_data_freezes_human_readable_fields`（数值镜像必须是 number）
--     · `tests/hr_26_leave_flow_seed.rs::condition_field_is_a_number_produced_by_the_handler`
--       （种子里引用的字段名必须在 handler 的 form_data 里是 number）
--
-- ── 节点为什么是「或签」（approval_mode = ANY）而不是全员会签 ─────────────
--   `ANY` 显式落库（不靠 NULL 兜底）。角色型审批节点上 `ALL` 把「岗位上任一人签字」
--   误读成「岗位上所有人都要点头」—— 只要有一个成员停用 / 离职未摘角色，单据就永久
--   `PENDING` 且无待办、界面无按钮可点（V0359 → V0367 修掉的 P0）。
--   `n_sup` 的候选虽然恒为 1 人（发起人本级部门负责人），也写 `ANY`：一旦
--   `FALLBACK_ADMIN` 兜出多个超管，`ALL` 会变成「全体超管会签」死锁（V0355 的口径）。
--   本迁移自检断言「**没有** 任何 APPROVAL 节点的 approval_mode 为 ALL / NULL」。
--
-- ── 自审批 ────────────────────────────────────────────────────────────
--   两个审批节点都是 `self_approval = 'ALLOW'`（与 V0332 / V0381 同款）：
--   拦截由**桥侧**（`hr::leave_bridge`，HR-27）在审批动作上做
--   （`flow::self_approval::over_limit` 在金额 / 阈值超限时把发起人从候选人里剔除）。
--   **刻意不写 FORBID**：FORBID 是「派发阶段」整单失败，员工自己提的单会直接派发不出来，
--   既没有审批人兜底也没有界面按钮 —— 就是 AGENTS 约定 11 说的死单。
--
-- ── 免审 ──────────────────────────────────────────────────────────────
--   `allow_auto_approve = FALSE`：请假单**不允许**免审直过。AGENTS 约定 11 的最高危分支
--   就是免审直过（没有审批人兜底，提交后任何失败都只能改库），本流程从源头不给这个口子。
--
-- ── 四重守卫（范本 = V0332 §⑦ / V0381）────────────────────────────────
--   ① 已有 `flow_code = 'hr_leave'` 的活流程                  → 整条跳过
--   ② 已有 `biz_type = 'HR_LEAVE'` 的**默认**流程             → 整条跳过
--      （`uk_biz_flow_def_default` 是唯一索引；硬插会撞索引，且「发起时挑哪条」不可判定）
--   ③ 审批角色（HR_MANAGER …f0016）不存在 / 已软删             → 整条跳过
--      （种子若引用一个不存在的角色，种出来的流程「派不出去」，比不种更糟）
--   ④ **不重复插 / 版本递增**：以 `change_note LIKE 'V0393 种子%'` 与 `graph_hash`
--      双标记；重复执行不插第二份；半途失败后重跑取 `COALESCE(MAX(version_no),0)+1`，
--      不撞 `uk_biz_flow_def_version`。
--   守卫跳过**只 NOTICE 不抛异常**：客户已自建同名 / 同类型流程时，升级不能失败
--   （覆盖客户配置＝比种子没生效更严重的事故）。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
--   重复执行 up：def 被守卫①拦下 → version 被④a/④b 拦下 → 节点 / 边 / 审批人
--   （按 `version_id` 定位，且那次 version 插入是 0 行）也 0 行 ⇒ **结果不变**。
--   `graph_snapshot` / `graph_hash` 不是手写的：按 `graph.rs::snapshot_json()` 同一套
--   规则（递归按键字典序 + 紧凑无空白 + sha256）离线算出，再在 §6 自检里用库内
--   canonical 化**重算一遍**对账（对不上就整份迁移回滚）。
--
-- ── 回滚 ──────────────────────────────────────────────────────────────
--   见 `V0393__hr_leave_flow.down.sql`：有 HR_LEAVE 审批实例 / 请假单已挂实例 /
--   本流程下还有别的版本 → 拒绝回滚（删版本会留下指向空气的审批单）。
-- ============================================================================

-- ---------- §0 canonical JSON（与 graph.rs::write_canonical 逐字同义，沿用 V0359/V0367/V0381）----------
-- 用途：§6 自检把库里的 `graph_snapshot` 重新 canonical 化再取 sha256，与
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
        RAISE EXCEPTION 'HR-26: 以下流程表未建成：%（请先执行 V0120/V0121 流程定义迁移）', v_missing;
    END IF;

    -- 1.2 本卡固定 def id 被**别的** flow_code 占用 ⇒ 结构性冲突，直接拦
    SELECT flow_code INTO v_code
      FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-0000000e0393'::uuid;
    IF v_code IS NOT NULL AND v_code <> 'hr_leave' THEN
        RAISE EXCEPTION 'HR-26: 固定流程 id …e0393 已被 flow_code=% 占用（不是本卡种的流程）；'
                        '本迁移不改已有流程定义行，请人工确认后重试', v_code;
    END IF;

    -- 1.3 客户自建的同名流程 / 同 biz_type 默认流 ⇒ 守卫会跳过（只提示，不拦）
    IF EXISTS (SELECT 1 FROM biz_flow_def
                WHERE flow_code = 'hr_leave' AND deleted_at IS NULL
                  AND id <> '20000000-0000-4000-8000-0000000e0393'::uuid) THEN
        RAISE NOTICE 'HR-26: 已存在 flow_code=hr_leave 的流程（非本卡 id）→ 守卫①跳过，保留客户配置';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_flow_def
                WHERE biz_type = 'HR_LEAVE' AND is_default AND deleted_at IS NULL
                  AND id <> '20000000-0000-4000-8000-0000000e0393'::uuid) THEN
        RAISE NOTICE 'HR-26: 已存在 biz_type=HR_LEAVE 的默认流程（非本卡 id）→ 守卫②跳过，保留客户配置';
    END IF;

    -- 1.4 审批角色（守卫③的输入，只提示）
    SELECT count(*) INTO v_roles
      FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0016'::uuid
       AND deleted_at IS NULL;
    IF v_roles <> 1 THEN
        RAISE NOTICE 'HR-26: 审批角色 HR_MANAGER（…f0016）不存在或已软删 → 守卫③跳过，不种流程';
    END IF;
END $$;

-- ---------- §2 流程定义（守卫 ①②③）----------
-- `is_system = TRUE`：预置流程客户不能删（设计 §⑨「四条都 is_system = TRUE」；
-- P2-6b 的 H6 不变量会判红）。
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve,
    is_system, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000e0393'::uuid,
    'hr_leave',
    '请假审批（标准·直属上级 → 长假人事复核）',
    'HR_LEAVE',
    '人力资源',
    '请假单提交审批：直属上级确认请假事由与排班影响；请假 ≥ 3 天再加一道人事经理复核'
    '（年假余额、产假 / 婚假证明、无薪假口径）。审批人绑角色 / 岗位（直属上级取发起人本级'
    '部门的负责人，长假复核绑 HR_MANAGER），或签（任一人签字即可），换人不用改代码。'
    '驳回或撤回打回 DRAFT，意见追加到请假单审批轨迹。',
    'PUBLISHED',
    '20000000-0000-4000-8000-0000000e0394'::uuid,
    1,
    TRUE,
    FALSE,
    TRUE,
    NULL
WHERE NOT EXISTS (
    -- 守卫①：同名流程已在（客户自建 / 上一次执行已种）
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_leave' AND deleted_at IS NULL
)
AND NOT EXISTS (
    -- 守卫②：同 biz_type 已有默认流程（唯一索引 uk_biz_flow_def_default）
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'HR_LEAVE' AND is_default AND deleted_at IS NULL
)
AND (
    -- 守卫③：审批角色 HR_MANAGER 在
    SELECT count(*) FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0016'::uuid
       AND deleted_at IS NULL
) = 1;

-- ---------- §3 版本快照（守卫④：不重复插 / 版本递增）----------
-- `graph_hash` = sha256(canonical JSON) = 4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97
-- 本卡的值由 `graph.rs::snapshot_json()` 同规则离线算出，§6 自检用库内 canonical 化重算对账。
-- `is_current`：只有当该 def 还没有当前版本时才置 TRUE（绝不去抢客户现有版本）。
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000e0394'::uuid,
    d.id,
    -- 守卫④「版本递增」：MAX+1（多版本安全，同 V0367 教训 —— 写死 1 会撞客户已有的 v1）
    COALESCE((SELECT MAX(x.version_no) FROM biz_flow_def_version x WHERE x.def_id = d.id), 0) + 1,
    'PUBLISHED',
    NOT EXISTS (SELECT 1 FROM biz_flow_def_version x WHERE x.def_id = d.id AND x.is_current),
    '4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97',
    $snapshot${
      "edges": [
        {"condition": null, "from": "c_days", "is_default": true, "key": "e_short", "label": "请假 < 3 天：直属上级终审", "priority": 10, "to": "e"},
        {"condition": {"cmp": "gte", "field": "form.duration_days_num", "op": "cmp", "value": 3}, "from": "c_days", "is_default": false, "key": "e_long", "label": "请假 ≥ 3 天：人事经理复核", "priority": 0, "to": "n_hr"},
        {"condition": null, "from": "n_hr", "is_default": false, "key": "e_hr_end", "label": null, "priority": 0, "to": "e"},
        {"condition": null, "from": "n_sup", "is_default": false, "key": "e_sup_days", "label": null, "priority": 0, "to": "c_days"},
        {"condition": null, "from": "s", "is_default": false, "key": "e_start", "label": null, "priority": 0, "to": "n_sup"}
      ],
      "nodes": [
        {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "OR", "key": "c_days", "max_remind_times": 3, "name": "请假天数判定（≥3 天）", "node_type": "CONDITION", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 3, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 440, "y": 0},
        {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "OR", "key": "e", "max_remind_times": 3, "name": "结束", "node_type": "END", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 5, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 880, "y": 0},
        {"allow_robot": false, "approval_mode": "ANY", "approve_ratio": null, "assignee_type": "ROLE", "assignees": [{"assignee_type": "ROLE", "ref_id": "20000000-0000-4000-8000-0000000f0016", "sort_order": 0}], "cc_on_approved": false, "config": {}, "empty_assignee": "FALLBACK_ADMIN", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "n_hr", "max_remind_times": 2, "name": "人事经理复核", "node_type": "APPROVAL", "reject_veto": true, "remind_before_hours": null, "self_approval": "ALLOW", "sort_order": 4, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": 48, "x": 660, "y": 0},
        {"allow_robot": false, "approval_mode": "ANY", "approve_ratio": null, "assignee_type": "INITIATOR_SUPERIOR", "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "FALLBACK_ADMIN", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "n_sup", "max_remind_times": 2, "name": "直属上级审批", "node_type": "APPROVAL", "reject_veto": true, "remind_before_hours": null, "self_approval": "ALLOW", "sort_order": 2, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": 24, "x": 220, "y": 0},
        {"allow_robot": false, "approval_mode": null, "approve_ratio": null, "assignee_type": null, "assignees": [], "cc_on_approved": false, "config": {}, "empty_assignee": "SKIP", "fork_mode": "AND", "form_var_key": null, "join_mode": "AND", "key": "s", "max_remind_times": 3, "name": "开始", "node_type": "START", "reject_veto": true, "remind_before_hours": null, "self_approval": "SKIP", "sort_order": 1, "superior_levels": 1, "timeout_action": "REMIND", "timeout_hours": null, "x": 0, "y": 0}
      ],
      "schema_version": 1
    }$snapshot$::jsonb,
    5,
    5,
    'V0393 种子：请假审批（直属上级 →（≥3 天）人事经理）默认流程',
    now(),
    NULL
  FROM biz_flow_def d
 WHERE d.flow_code = 'hr_leave'
   AND d.biz_type = 'HR_LEAVE'
   AND d.is_default
   AND d.deleted_at IS NULL
   -- 守卫④a：本卡版本已在 ⇒ 不重复插（幂等的核心）
   AND NOT EXISTS (
       SELECT 1 FROM biz_flow_def_version x
        WHERE x.def_id = d.id AND x.change_note LIKE 'V0393 种子%'
   )
   -- 守卫④b：同 hash 的版本已在（换了 change_note 也不重复发）
   AND NOT EXISTS (
       SELECT 1 FROM biz_flow_def_version x
        WHERE x.def_id = d.id
          AND x.graph_hash = '4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97'
   )
ON CONFLICT DO NOTHING;

-- ---------- §4 节点（5 个）/ 边（5 条）/ 审批人绑定（1 条）----------
-- 三张表都按「本卡版本行存在」为条件：版本被守卫拦下时它们一起 0 行，
-- 不会留下「有节点没版本」的半成品。
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e0395'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    -- 直属上级：候选 = 发起人本级部门的 leader_id（一人）。或签 ANY + 角色空了兜底给管理员，
    -- 24 小时未处理只提醒（REMIND），**不自动通过**（请假不许被超时静默放行）。
    ('20000000-0000-4000-8000-0000000e0396'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 'n_sup'::varchar, '直属上级审批'::varchar, 'APPROVAL'::varchar, 'INITIATOR_SUPERIOR'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    -- 天数判定：唯一一条条件边（≥3 天）+ 一条兜底边（<3 天）。
    ('20000000-0000-4000-8000-0000000e0397'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 'c_days'::varchar, '请假天数判定（≥3 天）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 440::int, 0::int, 3::int, '{}'::jsonb),
    -- 人事经理复核：绑角色 HR_MANAGER（…f0016），或签；48 小时复核窗口（长假要查余额/证明）。
    ('20000000-0000-4000-8000-0000000e0398'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 'n_hr'::varchar, '人事经理复核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 660::int, 0::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000e0399'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 880::int, 0::int, 5::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e0394'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e03a1'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000e0395'::uuid, '20000000-0000-4000-8000-0000000e0396'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e03a2'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 'e_sup_days'::varchar, '20000000-0000-4000-8000-0000000e0396'::uuid, '20000000-0000-4000-8000-0000000e0397'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    -- ★ 长假条件边：只认**数值镜像** duration_days_num（字符串字段会让求值器 TypeMismatch → 静默走兜底，见文件头）
    ('20000000-0000-4000-8000-0000000e03a3'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 'e_long'::varchar, '20000000-0000-4000-8000-0000000e0397'::uuid, '20000000-0000-4000-8000-0000000e0398'::uuid, '{"op": "cmp", "field": "form.duration_days_num", "cmp": "gte", "value": 3}'::jsonb, '请假 ≥ 3 天：人事经理复核'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e03a4'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 'e_short'::varchar, '20000000-0000-4000-8000-0000000e0397'::uuid, '20000000-0000-4000-8000-0000000e0399'::uuid, NULL::jsonb, '请假 < 3 天：直属上级终审'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000e03a5'::uuid, '20000000-0000-4000-8000-0000000e0394'::uuid, 'e_hr_end'::varchar, '20000000-0000-4000-8000-0000000e0398'::uuid, '20000000-0000-4000-8000-0000000e0399'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e0394'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 审批人 = 角色（不是「全员会签」、也不是写死某个人）：人换了不用改流程定义。
-- `n_sup` 不落行：INITIATOR_SUPERIOR 是**不可枚举**来源（随发起人变化），
-- 落一行部门/用户反而会把「谁的上级」写死成一个人的上级（V0355 的教训）。
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e0398'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0016'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e0394'::uuid)
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
 WHERE v.id = '20000000-0000-4000-8000-0000000e0394'::uuid
   AND d.id = v.def_id
   AND d.deleted_at IS NULL
   AND (d.current_version_id IS NULL OR d.current_version_id = v.id);

-- ---------- §6 自检 ----------
DO $$
DECLARE
    v_rows      integer;
    v_def       uuid := '20000000-0000-4000-8000-0000000e0393'::uuid;
    v_ver       uuid := '20000000-0000-4000-8000-0000000e0394'::uuid;
    v_cur       uuid;
    v_hash      text := '4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97';
    v_recalc    text;
    v_seq       text;
    v_cond      jsonb;
BEGIN
    -- 6.1 def 侧：存在即断言形态（被守卫跳过时不检查，只说明原因）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_leave' AND deleted_at IS NULL) THEN
        RAISE NOTICE 'HR-26: hr_leave 种子被守卫跳过（已有同名流程 / 已 HR_LEAVE 默认流 / HR_MANAGER 角色缺失），本迁移不改任何流程定义';
        RETURN;
    END IF;

    SELECT current_version_id INTO v_cur
      FROM biz_flow_def
     WHERE id = v_def AND deleted_at IS NULL;
    IF v_cur IS NULL THEN
        -- 本卡的 def 被守卫跳过、库里那条是客户的 → 不校验（也不许改它）
        RAISE NOTICE 'HR-26: hr_leave 由客户自建（非本卡 id …e0393），跳过自检';
        RETURN;
    END IF;

    -- 只有当「当前版本仍是本卡种的那一版」才做形态自检。客户在流程编辑器里改过图 /
    -- 发过新版之后，本卡的行已经不是出厂形态了 —— 这时**只提示不报错**
    -- （覆盖或阻断客户配置比种子没生效更严重）。
    IF v_cur <> v_ver
       OR (SELECT change_note FROM biz_flow_def_version WHERE id = v_ver)
          IS DISTINCT FROM 'V0393 种子：请假审批（直属上级 →（≥3 天）人事经理）默认流程' THEN
        RAISE NOTICE 'HR-26: hr_leave 已被客户改过（当前版本 %），跳过形态自检；本迁移不改客户配置', v_cur;
        RETURN;
    END IF;

    SELECT count(*) INTO v_rows
      FROM biz_flow_def
     WHERE id = v_def
       AND status = 'PUBLISHED'
       AND is_default
       AND is_system
       AND allow_auto_approve = FALSE
       AND biz_type = 'HR_LEAVE'
       AND flow_code = 'hr_leave'
       AND category = '人力资源'
       AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-26: 流程定义形态不对（应 PUBLISHED / is_default / is_system / allow_auto_approve=FALSE / biz_type=HR_LEAVE / category=人力资源）';
    END IF;

    -- 6.2 def 指针指向的版本必须真实存在（「能发起但没有图」是死单的另一种形态）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = v_cur) THEN
        RAISE EXCEPTION 'HR-26: biz_flow_def.current_version_id=% 指向不存在的版本', v_cur;
    END IF;

    -- 6.3 版本侧：PUBLISHED + 唯一当前版 + 节点/边计数
    SELECT count(*) INTO v_rows
      FROM biz_flow_def_version
     WHERE id = v_ver
       AND version_status = 'PUBLISHED'
       AND is_current
       AND node_count = 5
       AND edge_count = 5
       AND published_at IS NOT NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-26: 版本行形态不对（应 PUBLISHED / is_current / 5 节点 / 5 边 / 有发布时间）';
    END IF;

    -- 6.4 快照 ↔ graph_hash 自洽（库内 canonical 重算，防止手写 hash 与快照分叉）
    SELECT encode(sha256(convert_to(pg_temp.canonical_json(graph_snapshot), 'UTF8')), 'hex')
      INTO v_recalc
      FROM biz_flow_def_version
     WHERE id = v_ver;
    IF v_recalc IS DISTINCT FROM v_hash THEN
        RAISE EXCEPTION 'HR-26: graph_hash 与快照重算不一致（库内 %，期望 %）', v_recalc, v_hash;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_def_version
     WHERE id = v_ver AND graph_hash = v_hash;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-26: 落库 graph_hash 不是期望值（列值 / 常量不一致）';
    END IF;

    -- 6.5 节点行 ↔ 快照一致（行数、类型序列）
    SELECT count(*) INTO v_rows FROM biz_flow_node WHERE version_id = v_ver;
    IF v_rows <> 5 THEN
        RAISE EXCEPTION 'HR-26: 节点数 = %（期望 5）', v_rows;
    END IF;
    SELECT string_agg(node_type, ',' ORDER BY sort_order) INTO v_seq
      FROM biz_flow_node WHERE version_id = v_ver;
    IF v_seq <> 'START,APPROVAL,CONDITION,APPROVAL,END' THEN
        RAISE EXCEPTION 'HR-26: 节点类型序列 = %（期望 START,APPROVAL,CONDITION,APPROVAL,END）', v_seq;
    END IF;

    -- 6.6 ★ 审批节点：恰好 2 个，且都「有兜底 + 或签」；一个 ALL / NULL 都不许有
    --      （ALL 出现即是「全员会签」事故，V0359/V0367 的 P0）。
    SELECT count(*) INTO v_rows
      FROM biz_flow_node
     WHERE version_id = v_ver
       AND node_type = 'APPROVAL'
       AND approval_mode = 'ANY'
       AND empty_assignee = 'FALLBACK_ADMIN'
       AND timeout_action = 'REMIND'
       AND timeout_hours IS NOT NULL;
    IF v_rows <> 2 THEN
        RAISE EXCEPTION 'HR-26: 「或签 + 兜底 + 超时只提醒」的审批节点数 = %（期望 2）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_node
     WHERE version_id = v_ver
       AND node_type IN ('APPROVAL', 'COUNTERSIGN')
       AND (approval_mode = 'ALL' OR approval_mode IS NULL);
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'HR-26: 有 % 个审批节点是全员会签 / approval_mode 未落库（必须是 ANY）', v_rows;
    END IF;

    -- 6.7 ★ 审批人来源：直属上级（INITIATOR_SUPERIOR，levels=1，不落枚举行）+ 人事经理（绑角色）
    SELECT count(*) INTO v_rows
      FROM biz_flow_node
     WHERE version_id = v_ver
       AND node_key = 'n_sup'
       AND node_type = 'APPROVAL'
       AND assignee_type = 'INITIATOR_SUPERIOR'
       AND superior_levels = 1;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-26: n_sup 必须是 INITIATOR_SUPERIOR(superior_levels=1) —— 直属上级随发起人变化，不能写死';
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = v_ver
       AND n.node_key = 'n_hr'
       AND a.assignee_type = 'ROLE'
       AND a.ref_id = '20000000-0000-4000-8000-0000000f0016'::uuid;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-26: 审批人绑定行数 = %（期望 n_hr→HR_MANAGER …f0016 恰好 1 条）', v_rows;
    END IF;

    -- 6.8 链路形状：s→n_sup→c_days→{e | n_hr→e}；条件节点恰 1 条兜底边
    SELECT count(*) INTO v_rows
      FROM biz_flow_edge e
      JOIN biz_flow_node f ON f.id = e.from_node_id
      JOIN biz_flow_node t ON t.id = e.to_node_id
     WHERE e.version_id = v_ver
       AND ((f.node_key = 's'      AND t.node_key = 'n_sup'  AND e.edge_key = 'e_start')
         OR (f.node_key = 'n_sup'  AND t.node_key = 'c_days' AND e.edge_key = 'e_sup_days')
         OR (f.node_key = 'c_days' AND t.node_key = 'n_hr'   AND e.edge_key = 'e_long')
         OR (f.node_key = 'c_days' AND t.node_key = 'e'      AND e.edge_key = 'e_short')
         OR (f.node_key = 'n_hr'   AND t.node_key = 'e'      AND e.edge_key = 'e_hr_end'));
    IF v_rows <> 5 THEN
        RAISE EXCEPTION 'HR-26: 骨架边数 = %（期望 5：s→n_sup→c_days→{e | n_hr→e}）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_edge e
      JOIN biz_flow_node t ON t.id = e.to_node_id
     WHERE e.version_id = v_ver AND t.node_key = 's';
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'HR-26: 开始节点有 % 条入边（起点头必须为 0）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_edge
     WHERE version_id = v_ver AND is_default;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-26: 兜底边 = % 条（期望恰好 1 条：请假 < 3 天 → 结束）', v_rows;
    END IF;

    -- 6.9 ★ 条件边：字段必须是**数值镜像** form.duration_days_num、比较符 gte、阈值 3
    --      （字符串形式会让求值器 TypeMismatch → 静默走兜底，长假绕过人事复核）
    SELECT condition_expr INTO v_cond
      FROM biz_flow_edge
     WHERE version_id = v_ver AND edge_key = 'e_long';
    IF v_cond IS NULL
       OR v_cond->>'field' IS DISTINCT FROM 'form.duration_days_num'
       OR v_cond->>'cmp'   IS DISTINCT FROM 'gte'
       OR (v_cond->>'value') IS DISTINCT FROM '3' THEN
        RAISE EXCEPTION 'HR-26: 长假条件边形态不对（期望 form.duration_days_num gte 3，实际 %）', v_cond;
    END IF;

    RAISE NOTICE 'HR-26 自检通过：hr_leave（PUBLISHED / 5 节点 / 5 边 / 直属上级 +（≥3 天）人事经理 / 或签 / 快照 hash 自洽）';
END $$;
