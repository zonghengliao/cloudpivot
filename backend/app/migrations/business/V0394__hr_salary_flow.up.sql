-- ============================================================================
-- V0394__hr_salary_flow.up.sql
--
-- 薪资单审批流种子（卡 HR-38 / t_37006311）：
--   flow_code = `hr_salary`，biz_type = `HR_SALARY`，
--   节点 = 开始 → **人事经理**（ROLE HR_MANAGER）
--          → **财务会签**（COUNTERSIGN：财务经理 + 财务总监，全员签）
--          → 应发合计判定（≥ 50 万？）
--             ├─ 是 → **总经理** → 结束
--             └─ 否 → 结束
--
-- 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md`
--   §3.2 ⑨「审批流程（4 套：定义 + 版本 + 图 + hash）」的 `hr_salary` 行
--   （人事经理 → 财务经理（会签）→（应发 ≥ 50 万）总经理 → END，
--    理由：工资是真出钱；`≥50 万` 与预算审批同档），
--   §7.1 的 `hr_salary_flow` 行（四重守卫）、§8.1 的 HR-38 行、
--   §5.2 的 API #42 `POST /business/hr/salary/{id}/submit`（提交审批走本流程）。
--
-- ── 号段为什么是 V0394 而不是卡面写的 V0385 / 设计映射的 V0391 ────────────
--   ① 卡面/设计原始号 `V0385` 已被成本的灰度开关实占（`V0385__cost_module_switch`，
--      COST-B4-4）；HR-03 实施时曾把本卡顺延映射到 **V0391**，但 V0391 又被
--      `V0391__flow_cost_settle_is_system` 实占、V0392 被 `V0392__attendance_lock_guard`
--      （HR-23 考勤锁定守卫）实占，V0393 被 `V0393__hr_leave_flow`（HR-26）实占。
--   ② nik 迁移加载器对版本号做**跨目录全局唯一**校验，重号 = 服务启动即失败
--      （AGENTS 约定 6）。落地前实测 `ls backend/migrations/*/ | grep -oE 'V[0-9]{4}'
--      | sort -u | tail -3` = …/V0393/V0396/V0397，V0394/V0395 两格空闲。
--   ③ 故本卡取设计文档 §7.1「第三次号段修正」给的建议号 **V0394**（薪资单流程）
--      与 **V0395**（调薪/离职流程）。规律不变：先落地者定号，后落地者避让。
--
-- ── 节点为什么是 6 个 / 边 6 条（不是设计表里的线性 4 节点）────────────
--   「（应发 ≥ 50 万）总经理」是一个**金额分级**，必须有分叉：
--     s → n_hr → n_fin → c_amt → {e_big→n_gm→e | e_small(兜底)→e}
--   金额阈值属于**客户可配置的业务口径**（50 万与预算审批同档，客户可按自己
--   的授权手册调整），所以写成 `biz_flow_edge.condition_expr` 而不是代码里的
--   if/else —— 与 V0393 请假「≥3 天」、V0182 采购「金额分级」同款做法。
--
-- ── ★ 条件字段为什么是 `form.gross_total_num`（数值镜像）────────────────
--   `salary_sheet` 单头的 `gross_total` 是 `NUMERIC(20,4)`。提交审批时
--   `hr::salary_sheet_handler`（HR-39/HR-42 的 `/salary/{id}/submit`）会把单头
--   冻结进 `form_data`。若冻结成**字符串**（展示口径 `"9154.0300"`），条件求值器
--   （`flow::graph::compare` 的 `gte`）会 `Value::as_f64() == None` ⇒
--   `EvalError::TypeMismatch` ⇒ 引擎按「条件不成立」**静默走兜底边**（只有 WARN）
--   ⇒ 应发 80 万的工资单**绕过总经理审批且不报错**。这是 V0393 踩过的同一个坑
--   （`duration_days` 字符串 vs `duration_days_num` 数值），故本卡沿用「`_num` =
--   必须是 JSON number」的命名契约，条件边只认它。
--   **契约持有方**：`salary_handler` 提交审批时必须写
--   `form["gross_total_num"] = serde_json::Value::Number`（与列同值、4 位小数
--   以内，`Number::from_f64` 足够）；`tests/hr_38_salary_flow_seed.rs` 在
--   handler 落地后由 HR-39/HR-42 的用例侧对拍（本卡先在迁移侧钉死答案）。
--
-- ── 「财务经理（会签）」落地成 COUNTERSIGN + ALL + 两个角色 ────────────────
--   设计表只写了「财务经理（会签）」。会签的语义是「多人签字才算过」，单角色
--   会签会退化成「该角色**所有成员**都要点一遍」—— 只要有一个成员停用未摘角色，
--   工资单就永久 `PENDING_APPROVAL`（V0359/V0367 的 P0：没有待办、界面无按钮、
--   只能改库）。故按本仓既有口径（V0182「采购经理+总经理会签」、V0191
--   「财务经理+财务总监会签」）把它落成 **COUNTERSIGN + ALL**，候选人 = 两个
--   岗位角色（`FINANCE_MANAGER` …f0005 + `FINANCE_DIRECTOR` …f0006）：
--   岗位换人不用改流程定义，也不依赖某个角色有几个成员。
--   兜底与超时同样必配（`empty_assignee=FALLBACK_ADMIN` + `timeout_hours=48`
--   + `timeout_action=REMIND`）：R28 与 `scripts/verify/flow_countersign_invariants.sh`
--   断言①都要求 ALL 节点有兜底或超时，否则「一个人签不了 → 整单永久卡死」。
--
-- ── 节点为什么都是「或签」以外还要显式写 approval_mode ─────────────────
--   `APPROVAL` 的 `NULL` 语义虽然等价 ANY（P3-06），但本仓口径是**显式落库**：
--   V0359 之后 `NULL` 会被当成「没配过」再被冻结一次。`n_fin` 是 COUNTERSIGN，
--   按 R9 **必须**显式给 approval_mode（这里 ALL）。
--   本迁移自检断言：没有 `APPROVAL + ALL`（角色型全员会签 = 死锁），
--   且 `n_fin` 是 `COUNTERSIGN + ALL` 且绑了两个角色。
--
-- ── 自审批 / 免审 ─────────────────────────────────────────────────────
--   三个审批节点 `self_approval = 'ALLOW'`：拦截由**桥侧**在审批动作上做
--   （`flow::self_approval::over_limit`）。**刻意不写 FORBID**：FORBID 是派发阶段
--   整单失败，人事经理自己提的工资单会直接派发不出来 = AGENTS 约定 11 的死单。
--   `allow_auto_approve = FALSE`：工资单**不允许**免审直过 / 超时自动通过
--   （免审直过是「提交后失败无人兜底」的最高危分支）。节点 `timeout_action`
--   一律 `REMIND`。
--
-- ── 四重守卫（范本 = V0393 §1 / V0182 §⑦）────────────────────────────
--   ① 已有 `flow_code = 'hr_salary'` 的活流程               → 整条跳过
--   ② 已有 `biz_type = 'HR_SALARY'` 的**默认**流程          → 整条跳过
--      （`uk_biz_flow_def_default` 是唯一索引；硬插会撞索引）
--   ③ 审批角色（HR_MANAGER / FINANCE_MANAGER / FINANCE_DIRECTOR / GENERAL_MANAGER）
--      任一不存在或已软删                                    → 整条跳过
--      （种一个「派不出去」的流程比不种更糟）
--   ④ 不重复插 / 版本递增：`change_note LIKE 'V0394 种子%'` + `graph_hash` 双标记；
--      重跑取 `COALESCE(MAX(version_no),0)+1`，不撞 `uk_biz_flow_def_version`。
--   守卫跳过**只 NOTICE 不抛异常**：客户已自建同名 / 同类型流程时，升级不能失败
--   （覆盖客户配置 = 比种子没生效更严重的事故）。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
--   重复执行 up：def 被守卫①拦下 → version 被④a/④b 拦下 → 节点 / 边 / 审批人
--   （按 `version_id` 定位，且那次 version 插入是 0 行）也 0 行 ⇒ **结果不变**。
--   `graph_snapshot` / `graph_hash` 不是手写的：按 `graph.rs::snapshot_json()`
--   同一套规则（递归键升序 + 紧凑无空白 + sha256）离线算出，再在 §6 自检里用
--   库内 canonical 化**重算一遍**对账（对不上就整份迁移回滚）。
--   graph_hash = 0a296af0477bd307f7d7687b447d50f2f813da9eeef1bbe57711b1e8a09bd5d0
--
-- ── 回滚 ──────────────────────────────────────────────────────────────
--   见 `V0394__hr_salary_flow.down.sql`：有 HR_SALARY 审批实例 / 薪资单已挂实例 /
--   本流程下还有别的版本 → 拒绝回滚（删版本会留下指向空气的审批单）。
-- ============================================================================

-- ---------- §0 canonical JSON（与 graph.rs::write_canonical 逐字同义，沿用 V0359/V0367/V0381/V0393）----------
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
        RAISE EXCEPTION 'HR-38: 以下流程表未建成：%（请先执行 V0120/V0121 流程定义迁移）', v_missing;
    END IF;

    -- 1.2 本卡固定 def id 被**别的** flow_code 占用 ⇒ 结构性冲突，直接拦
    SELECT flow_code INTO v_code
      FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-0000000e1401'::uuid;
    IF v_code IS NOT NULL AND v_code <> 'hr_salary' THEN
        RAISE EXCEPTION 'HR-38: 固定流程 id …e1401 已被 flow_code=% 占用（不是本卡种的流程）；'
                        '本迁移不改已有流程定义行，请人工确认后重试', v_code;
    END IF;

    -- 1.3 客户自建的同名流程 / 同 biz_type 默认流 ⇒ 守卫会跳过（只提示，不拦）
    IF EXISTS (SELECT 1 FROM biz_flow_def
                WHERE flow_code = 'hr_salary' AND deleted_at IS NULL
                  AND id <> '20000000-0000-4000-8000-0000000e1401'::uuid) THEN
        RAISE NOTICE 'HR-38: 已存在 flow_code=hr_salary 的流程（非本卡 id）→ 守卫①跳过，保留客户配置';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_flow_def
                WHERE biz_type = 'HR_SALARY' AND is_default AND deleted_at IS NULL
                  AND id <> '20000000-0000-4000-8000-0000000e1401'::uuid) THEN
        RAISE NOTICE 'HR-38: 已存在 biz_type=HR_SALARY 的默认流程（非本卡 id）→ 守卫②跳过，保留客户配置';
    END IF;

    -- 1.4 审批角色（守卫③的输入，只提示）
    SELECT count(*) INTO v_roles
      FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0016'::uuid,  -- HR_MANAGER 人事经理
                  '20000000-0000-4000-8000-0000000f0005'::uuid,  -- FINANCE_MANAGER 财务经理
                  '20000000-0000-4000-8000-0000000f0006'::uuid,  -- FINANCE_DIRECTOR 财务总监
                  '20000000-0000-4000-8000-0000000f0003'::uuid)  -- GENERAL_MANAGER 总经理
       AND deleted_at IS NULL;
    IF v_roles <> 4 THEN
        RAISE NOTICE 'HR-38: 审批角色只找到 %/4（HR_MANAGER/FINANCE_MANAGER/FINANCE_DIRECTOR/GENERAL_MANAGER）'
                     ' → 守卫③跳过，不种流程', v_roles;
    END IF;
END $$;

-- ---------- §2 流程定义（守卫 ①②③）----------
-- `is_system = TRUE`：预置流程客户不能删（设计 §3.2 ⑨「四条都 is_system = TRUE」；
-- P2-6b 的 H6 不变量会判红）。
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve,
    is_system, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000e1401'::uuid,
    'hr_salary',
    '薪资单审批（标准·人事 → 财务会签 → 大额总经理）',
    'HR_SALARY',
    '人力资源',
    '工资单提交审批：人事经理确认人数与逐人金额，财务经理 + 财务总监会签确认资金与个税/社保口径，'
    '应发合计 ≥ 50 万再加一道总经理审批（工资是真出钱，与预算审批同档）。审批人绑角色'
    '（人事经理 / 财务经理 / 财务总监 / 总经理），岗位换人不用改代码；会签节点或签口径为全员签。'
    '驳回或撤回打回 DRAFT，意见追加到薪资单审批轨迹。',
    'PUBLISHED',
    '20000000-0000-4000-8000-0000000e1402'::uuid,
    1,
    TRUE,
    FALSE,
    TRUE,
    NULL
WHERE NOT EXISTS (
    -- 守卫①：同名流程已在（客户自建 / 上一次执行已种）
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_salary' AND deleted_at IS NULL
)
AND NOT EXISTS (
    -- 守卫②：同 biz_type 已有默认流程（唯一索引 uk_biz_flow_def_default）
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'HR_SALARY' AND is_default AND deleted_at IS NULL
)
AND (
    -- 守卫③：四个审批角色都在（缺一个就派不出人 ⇒ 不种）
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0016'::uuid,
                  '20000000-0000-4000-8000-0000000f0005'::uuid,
                  '20000000-0000-4000-8000-0000000f0006'::uuid,
                  '20000000-0000-4000-8000-0000000f0003'::uuid)
       AND deleted_at IS NULL
) = 4;

-- ---------- §3 版本快照（守卫④：不重复插 / 版本递增）----------
-- graph_hash = sha256(canonical JSON) = 0a296af0477bd307f7d7687b447d50f2f813da9eeef1bbe57711b1e8a09bd5d0
-- `is_current`：只有当该 def 还没有当前版本时才置 TRUE（绝不去抢客户现有版本）。
INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000e1402'::uuid,
    d.id,
    -- 守卫④「版本递增」：MAX+1（多版本安全，同 V0367 教训 —— 写死 1 会撞客户已有的 v1）
    COALESCE((SELECT MAX(x.version_no) FROM biz_flow_def_version x WHERE x.def_id = d.id), 0) + 1,
    'PUBLISHED',
    NOT EXISTS (SELECT 1 FROM biz_flow_def_version x WHERE x.def_id = d.id AND x.is_current),
    '0a296af0477bd307f7d7687b447d50f2f813da9eeef1bbe57711b1e8a09bd5d0',
    $snapshot${"edges":[{"condition":null,"from":"c_amt","is_default":true,"key":"e_small","label":"应发 < 50 万：财务会签后结束","priority":10,"to":"e"},{"condition":{"cmp":"gte","field":"form.gross_total_num","op":"cmp","value":500000},"from":"c_amt","is_default":false,"key":"e_big","label":"应发 ≥ 50 万：总经理审批","priority":0,"to":"n_gm"},{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_amt","label":null,"priority":0,"to":"c_amt"},{"condition":null,"from":"n_gm","is_default":false,"key":"e_gm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_hr","is_default":false,"key":"e_hr_fin","label":null,"priority":0,"to":"n_fin"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_hr"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_amt","max_remind_times":3,"name":"应发合计判定（≥50 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":660,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":1100,"y":0},{"allow_robot":false,"approval_mode":"ALL","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0},{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0006","sort_order":1}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"财务会签","node_type":"COUNTERSIGN","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":440,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm","max_remind_times":2,"name":"总经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":72,"x":880,"y":-140},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0016","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_hr","max_remind_times":2,"name":"人事经理审核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    6,
    6,
    'V0394 种子：薪资单审批（人事经理 → 财务会签 →（应发 ≥ 50 万）总经理）默认流程',
    now(),
    NULL
  FROM biz_flow_def d
 WHERE d.flow_code = 'hr_salary'
   AND d.biz_type = 'HR_SALARY'
   AND d.is_default
   AND d.deleted_at IS NULL
   -- 守卫④a：本卡版本已在 ⇒ 不重复插（幂等的核心）
   AND NOT EXISTS (
       SELECT 1 FROM biz_flow_def_version x
        WHERE x.def_id = d.id AND x.change_note LIKE 'V0394 种子%'
   )
   -- 守卫④b：同 hash 的版本已在（换了 change_note 也不重复发）
   AND NOT EXISTS (
       SELECT 1 FROM biz_flow_def_version x
        WHERE x.def_id = d.id
          AND x.graph_hash = '0a296af0477bd307f7d7687b447d50f2f813da9eeef1bbe57711b1e8a09bd5d0'
   )
ON CONFLICT DO NOTHING;

-- ---------- §4 节点（6 个）/ 边（6 条）/ 审批人绑定（4 条）----------
-- 三张表都按「本卡版本行存在」为条件：版本被守卫拦下时它们一起 0 行，
-- 不会留下「有节点没版本」的半成品。
INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e1411'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    -- 人事经理：绑角色 HR_MANAGER（…f0016），或签；48 小时窗口（要核对人数与逐人金额）。
    ('20000000-0000-4000-8000-0000000e1412'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'n_hr'::varchar, '人事经理审核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    -- 财务会签：COUNTERSIGN + ALL（财务经理 + 财务总监），必须显式 approval_mode（R9）；
    -- 兜底超管 + 48h 提醒（ALL 节点没有兜底/超时 = 一人失效即永久卡单，R28）。
    ('20000000-0000-4000-8000-0000000e1413'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'n_fin'::varchar, '财务会签'::varchar, 'COUNTERSIGN'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ALL'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 440::int, 0::int, 3::int, '{}'::jsonb),
    -- 金额判定：唯一一条条件边（应发 ≥ 50 万）+ 一条兜底边（< 50 万直接结束）。
    ('20000000-0000-4000-8000-0000000e1414'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'c_amt'::varchar, '应发合计判定（≥50 万）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 660::int, 0::int, 4::int, '{}'::jsonb),
    -- 总经理：绑角色 GENERAL_MANAGER（…f0003），或签；72 小时窗口（大额工资要等老板）。
    ('20000000-0000-4000-8000-0000000e1415'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'n_gm'::varchar, '总经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 72::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 880::int, -140::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000e1416'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 1100::int, 0::int, 6::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e1402'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e1421'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000e1411'::uuid, '20000000-0000-4000-8000-0000000e1412'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e1422'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'e_hr_fin'::varchar, '20000000-0000-4000-8000-0000000e1412'::uuid, '20000000-0000-4000-8000-0000000e1413'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e1423'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'e_fin_amt'::varchar, '20000000-0000-4000-8000-0000000e1413'::uuid, '20000000-0000-4000-8000-0000000e1414'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    -- ★ 大额条件边：只认**数值镜像** gross_total_num（字符串字段会让求值器 TypeMismatch → 静默走兜底，见文件头）
    ('20000000-0000-4000-8000-0000000e1424'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'e_big'::varchar, '20000000-0000-4000-8000-0000000e1414'::uuid, '20000000-0000-4000-8000-0000000e1415'::uuid, '{"op": "cmp", "field": "form.gross_total_num", "cmp": "gte", "value": 500000}'::jsonb, '应发 ≥ 50 万：总经理审批'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e1425'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'e_small'::varchar, '20000000-0000-4000-8000-0000000e1414'::uuid, '20000000-0000-4000-8000-0000000e1416'::uuid, NULL::jsonb, '应发 < 50 万：财务会签后结束'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000e1426'::uuid, '20000000-0000-4000-8000-0000000e1402'::uuid, 'e_gm_end'::varchar, '20000000-0000-4000-8000-0000000e1415'::uuid, '20000000-0000-4000-8000-0000000e1416'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e1402'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- 审批人 = 角色（不是「全员会签」、也不是写死某个人）：人换了不用改流程定义。
-- `n_fin` 落**两行**（财务经理 + 财务总监）：会签的候选人是两个岗位，不是一个角色的全部成员。
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e1412'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0016'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000e1413'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000e1413'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0006'::uuid, 1::int),
    ('20000000-0000-4000-8000-0000000e1415'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e1402'::uuid)
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
 WHERE v.id = '20000000-0000-4000-8000-0000000e1402'::uuid
   AND d.id = v.def_id
   AND d.deleted_at IS NULL
   AND (d.current_version_id IS NULL OR d.current_version_id = v.id);

-- ---------- §6 自检 ----------
DO $$
DECLARE
    v_rows      integer;
    v_def       uuid := '20000000-0000-4000-8000-0000000e1401'::uuid;
    v_ver       uuid := '20000000-0000-4000-8000-0000000e1402'::uuid;
    v_cur       uuid;
    v_hash      text := '0a296af0477bd307f7d7687b447d50f2f813da9eeef1bbe57711b1e8a09bd5d0';
    v_recalc    text;
    v_seq       text;
    v_cond      jsonb;
BEGIN
    -- 6.1 def 侧：存在即断言形态（被守卫跳过时不检查，只说明原因）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_salary' AND deleted_at IS NULL) THEN
        RAISE NOTICE 'HR-38: hr_salary 种子被守卫跳过（已有同名流程 / 已 HR_SALARY 默认流 / 审批角色缺失），本迁移不改任何流程定义';
        RETURN;
    END IF;

    SELECT current_version_id INTO v_cur
      FROM biz_flow_def
     WHERE id = v_def AND deleted_at IS NULL;
    IF v_cur IS NULL THEN
        -- 本卡的 def 被守卫跳过、库里那条是客户的 → 不校验（也不许改它）
        RAISE NOTICE 'HR-38: hr_salary 由客户自建（非本卡 id …e1401），跳过自检';
        RETURN;
    END IF;

    -- 只有当「当前版本仍是本卡种的那一版」才做形态自检。客户在流程编辑器里改过图 /
    -- 发过新版之后，本卡的行已经不是出厂形态了 —— 这时**只提示不报错**
    -- （覆盖或阻断客户配置比种子没生效更严重）。
    IF v_cur <> v_ver
       OR (SELECT change_note FROM biz_flow_def_version WHERE id = v_ver)
          IS DISTINCT FROM 'V0394 种子：薪资单审批（人事经理 → 财务会签 →（应发 ≥ 50 万）总经理）默认流程' THEN
        RAISE NOTICE 'HR-38: hr_salary 已被客户改过（当前版本 %），跳过形态自检；本迁移不改客户配置', v_cur;
        RETURN;
    END IF;

    SELECT count(*) INTO v_rows
      FROM biz_flow_def
     WHERE id = v_def
       AND status = 'PUBLISHED'
       AND is_default
       AND is_system
       AND allow_auto_approve = FALSE
       AND biz_type = 'HR_SALARY'
       AND flow_code = 'hr_salary'
       AND category = '人力资源'
       AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-38: 流程定义形态不对（应 PUBLISHED / is_default / is_system / allow_auto_approve=FALSE / biz_type=HR_SALARY / category=人力资源）';
    END IF;

    -- 6.2 def 指针指向的版本必须真实存在（「能发起但没有图」是死单的另一种形态）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = v_cur) THEN
        RAISE EXCEPTION 'HR-38: biz_flow_def.current_version_id=% 指向不存在的版本', v_cur;
    END IF;

    -- 6.3 版本侧：PUBLISHED + 唯一当前版 + 节点/边计数
    SELECT count(*) INTO v_rows
      FROM biz_flow_def_version
     WHERE id = v_ver
       AND version_status = 'PUBLISHED'
       AND is_current
       AND node_count = 6
       AND edge_count = 6
       AND published_at IS NOT NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-38: 版本行形态不对（应 PUBLISHED / is_current / 6 节点 / 6 边 / 有发布时间）';
    END IF;

    -- 6.4 快照 ↔ graph_hash 自洽（库内 canonical 重算，防止手写 hash 与快照分叉）
    SELECT encode(sha256(convert_to(pg_temp.canonical_json(graph_snapshot), 'UTF8')), 'hex')
      INTO v_recalc
      FROM biz_flow_def_version
     WHERE id = v_ver;
    IF v_recalc IS DISTINCT FROM v_hash THEN
        RAISE EXCEPTION 'HR-38: graph_hash 与快照重算不一致（库内 %，期望 %）', v_recalc, v_hash;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_def_version
     WHERE id = v_ver AND graph_hash = v_hash;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-38: 落库 graph_hash 不是期望值（列值 / 常量不一致）';
    END IF;

    -- 6.5 节点行 ↔ 快照一致（行数、类型序列）
    SELECT count(*) INTO v_rows FROM biz_flow_node WHERE version_id = v_ver;
    IF v_rows <> 6 THEN
        RAISE EXCEPTION 'HR-38: 节点数 = %（期望 6）', v_rows;
    END IF;
    SELECT string_agg(node_type, ',' ORDER BY sort_order) INTO v_seq
      FROM biz_flow_node WHERE version_id = v_ver;
    IF v_seq <> 'START,APPROVAL,COUNTERSIGN,CONDITION,APPROVAL,END' THEN
        RAISE EXCEPTION 'HR-38: 节点类型序列 = %（期望 START,APPROVAL,COUNTERSIGN,CONDITION,APPROVAL,END）', v_seq;
    END IF;

    -- 6.6 ★ 审批节点形态：2 个 APPROVAL 必须是「或签 + 兜底 + 超时只提醒」；
    --     任何 APPROVAL 都不许 ALL（角色型全员会签 = 一人停用即永久卡单，V0359/V0367 的 P0）。
    SELECT count(*) INTO v_rows
      FROM biz_flow_node
     WHERE version_id = v_ver
       AND node_type = 'APPROVAL'
       AND approval_mode = 'ANY'
       AND empty_assignee = 'FALLBACK_ADMIN'
       AND timeout_action = 'REMIND'
       AND timeout_hours IS NOT NULL;
    IF v_rows <> 2 THEN
        RAISE EXCEPTION 'HR-38: 「或签 + 兜底 + 超时只提醒」的 APPROVAL 节点数 = %（期望 2：人事经理 / 总经理）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_node
     WHERE version_id = v_ver
       AND node_type = 'APPROVAL'
       AND (approval_mode = 'ALL' OR approval_mode IS NULL);
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'HR-38: 有 % 个 APPROVAL 节点是全员会签 / approval_mode 未落库（必须是显式 ANY）', v_rows;
    END IF;

    -- 6.7 ★ 会签节点：恰好 1 个 COUNTERSIGN + ALL + 绑两个角色（财务经理 + 财务总监）
    --      且必须带兜底与超时（R28 / flow_countersign_invariants.sh 断言①）
    SELECT count(*) INTO v_rows
      FROM biz_flow_node
     WHERE version_id = v_ver
       AND node_type = 'COUNTERSIGN'
       AND node_key = 'n_fin'
       AND approval_mode = 'ALL'
       AND empty_assignee = 'FALLBACK_ADMIN'
       AND timeout_action = 'REMIND'
       AND timeout_hours IS NOT NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-38: 财务会签节点形态不对（应 COUNTERSIGN + ALL + FALLBACK_ADMIN + 有超时小时）';
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = v_ver
       AND n.node_key = 'n_fin'
       AND a.assignee_type = 'ROLE'
       AND a.ref_id IN ('20000000-0000-4000-8000-0000000f0005'::uuid,
                        '20000000-0000-4000-8000-0000000f0006'::uuid);
    IF v_rows <> 2 THEN
        RAISE EXCEPTION 'HR-38: 财务会签的审批人绑定 = % 行（期望 2：财务经理 …f0005 + 财务总监 …f0006）', v_rows;
    END IF;

    -- 6.8 ★ 审批人绑定：人事经理 / 总经理各一行，且不多不少（4 行 = HR + 财务×2 + 总经理）
    SELECT count(*) INTO v_rows
      FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = v_ver AND a.assignee_type = 'ROLE';
    IF v_rows <> 4 THEN
        RAISE EXCEPTION 'HR-38: 审批人绑定总行数 = %（期望 4：人事经理 1 + 财务 2 + 总经理 1）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = v_ver
       AND n.node_key = 'n_hr'
       AND a.ref_id = '20000000-0000-4000-8000-0000000f0016'::uuid;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-38: n_hr 未绑 HR_MANAGER（…f0016）';
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_node_assignee a
      JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = v_ver
       AND n.node_key = 'n_gm'
       AND a.ref_id = '20000000-0000-4000-8000-0000000f0003'::uuid;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-38: n_gm 未绑 GENERAL_MANAGER（…f0003）';
    END IF;

    -- 6.9 链路形状：s→n_hr→n_fin→c_amt→{e | n_gm→e}；条件节点恰 1 条兜底边
    SELECT count(*) INTO v_rows
      FROM biz_flow_edge e
      JOIN biz_flow_node f ON f.id = e.from_node_id
      JOIN biz_flow_node t ON t.id = e.to_node_id
     WHERE e.version_id = v_ver
       AND ((f.node_key = 's'     AND t.node_key = 'n_hr'  AND e.edge_key = 'e_start')
         OR (f.node_key = 'n_hr'  AND t.node_key = 'n_fin' AND e.edge_key = 'e_hr_fin')
         OR (f.node_key = 'n_fin' AND t.node_key = 'c_amt' AND e.edge_key = 'e_fin_amt')
         OR (f.node_key = 'c_amt' AND t.node_key = 'n_gm'  AND e.edge_key = 'e_big')
         OR (f.node_key = 'c_amt' AND t.node_key = 'e'     AND e.edge_key = 'e_small')
         OR (f.node_key = 'n_gm'  AND t.node_key = 'e'     AND e.edge_key = 'e_gm_end'));
    IF v_rows <> 6 THEN
        RAISE EXCEPTION 'HR-38: 骨架边数 = %（期望 6：s→n_hr→n_fin→c_amt→{e | n_gm→e}）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_edge e
      JOIN biz_flow_node t ON t.id = e.to_node_id
     WHERE e.version_id = v_ver AND t.node_key = 's';
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'HR-38: 开始节点有 % 条入边（起点头必须为 0）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_flow_edge
     WHERE version_id = v_ver AND is_default;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'HR-38: 兜底边 = % 条（期望恰好 1 条：应发 < 50 万 → 结束）', v_rows;
    END IF;

    -- 6.10 ★ 条件边：字段必须是**数值镜像** form.gross_total_num、比较符 gte、阈值 500000
    --      （字符串形式会让求值器 TypeMismatch → 静默走兜底，大额工资绕过总经理审批）
    SELECT condition_expr INTO v_cond
      FROM biz_flow_edge
     WHERE version_id = v_ver AND edge_key = 'e_big';
    IF v_cond IS NULL
       OR v_cond->>'field' IS DISTINCT FROM 'form.gross_total_num'
       OR v_cond->>'cmp'   IS DISTINCT FROM 'gte'
       OR (v_cond->>'value') IS DISTINCT FROM '500000' THEN
        RAISE EXCEPTION 'HR-38: 大额条件边形态不对（期望 form.gross_total_num gte 500000，实际 %）', v_cond;
    END IF;

    RAISE NOTICE 'HR-38 自检通过：hr_salary（PUBLISHED / 6 节点 / 6 边 / 人事经理 → 财务会签 →（≥50 万）总经理 / 快照 hash 自洽）';
END $$;
