-- ============================================================================
-- V0395__hr_adjust_resign_flow.up.sql
--
-- 调薪 / 离职 两套审批流种子（卡 HR-38 / t_37006311）：
--
--   ① flow_code = `hr_salary_adjust`，biz_type = `HR_SALARY_ADJUST`
--      开始 → **直属上级**（INITIATOR_SUPERIOR）→ **人事经理**（HR_MANAGER）
--           → **财务经理**（FINANCE_MANAGER）→ 结束
--      5 节点 / 4 边，graph_hash = a413971a5a5de2bdb11798bfbfa140d927573e541cf4fd7e4f99a99dd6a99171
--
--   ② flow_code = `hr_employee_resign`，biz_type = `HR_EMPLOYEE_RESIGN`
--      开始 → **直属上级** → **人事经理** → 关键岗位判定？
--             ├─ 是 → **总经理**（GENERAL_MANAGER）→ 结束
--             └─ 否 → 结束（人事经理终审）
--      6 节点 / 6 边，graph_hash = ec42328b209441d5ea6e403f8bfaef9b8d5dc13f5a83ec1319a36d9864558496
--
-- 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md`
--   §3.2 ⑨「审批流程（4 套）」的 `hr_salary_adjust` / `hr_employee_resign` 两行
--   （调薪：直属上级 → 人事经理 → 财务经理 → END，理由：调薪直接改后续每月的钱，三签；
--    离职：直属上级 → 人事经理 →（关键岗位）总经理 → END，理由：离职牵动账号权限与交接），
--   §4.1.3「离职流程」（② 提交审批 → ③ 审批通过同一事务处置账号与未决请假单），
--   §7.1 的 `hr_adjust_resign_flow` 行（四重守卫）、§8.1 的 HR-38 行。
--
-- ── 号段 ─────────────────────────────────────────────────────────────
--   卡面设计号 `V0386`、设计映射 V0392 均已被实占（`V0386__hr_seq_dict` HR-03、
--   `V0392__attendance_lock_guard` HR-23）。落地前实测最大号为 V0397，
--   V0394（本卡薪资单流程）+ **V0395**（本文件）两格空闲；nik 迁移加载器对版本号
--   做**跨目录全局唯一**校验，重号 = 服务启动即失败（AGENTS 约定 6 / §7.1 号段表）。
--
-- ── 两套流程为什么写在**同一个迁移**里 ─────────────────────────────────
--   设计 §7.1 把它们并成一格（`hr_adjust_resign_flow`，原 `V0386`），两者都是
--   「员工生命周期」变动单、共用同一批角色、同一批守卫、同一次自检。分两个文件反而
--   要复制一遍 canonical JSON 函数与三重守卫。一个事务里种两套 = 要么都成、要么都不成。
--
-- ── 调薪为什么是**三签**且没有金额分级 ────────────────────────────────
--   调薪改的是「以后每个月发多少钱」，一次签字会长期放大（不像单张工资单只错一个月），
--   故直属上级（业务确认）→ 人事经理（薪酬体系与合规）→ 财务经理（成本与预算）三道都要过。
--   设计没给调薪的金额阈值，本卡**不自造阈值**（客户若要「超过某金额加总经理」，
--   在流程编辑器里加节点即可 —— 条件是配置，不是代码）。
--
-- ── 离职为什么有「关键岗位」分叉 ─────────────────────────────────────
--   「关键岗位」判定是客户口径（岗位 / 职级 / 是否管钱管账），故写成条件边
--   `form.is_key_position` 而不是代码里的 if/else：HR 在流程编辑器里能改走向，
--   不需要改代码、不需要发版（与 V0393「≥3 天」、V0184 采购「金额分级」同款）。
--
-- ── ★ 条件字段的类型契约（沿用 V0393 的「数值镜像」教训）────────────────
--   离职条件边只认 `form.is_key_position`，且它必须是 **JSON boolean**：
--     · `graph::compare` 的 `eq` 走 `json_eq`，bool 与 bool 直接比 —— 合法；
--     · 若提交审批时冻结成字符串 `"true"`，`json_eq("true", true)` 为 **false** ⇒
--       关键岗位离职**静默跳过总经理审批**（只有 WARN 日志，不报错）。
--   **契约持有方**：离职单提交审批（后续卡）必须在 `form_data` 里写
--   `form["is_key_position"] = serde_json::Value::Bool(...)`；
--   薪资单流程的同款契约见 `V0394` 文件头（`gross_total_num` 必须是 JSON number）。
--
-- ── 这两套流程的「单据」是什么（避免后续卡去找不存在的单表）──────────
--   `shared/doc_kind.rs::FlowBizType::doc_kind()` 已定稿：调薪与离职**没有独立的单表**
--   （`biz_employee_salary` 是薪资标准的行版本台账；离职只是员工状态的一次流转），
--   审批实例的 `biz_id` 都是 **`biz_employee.id`**（`doc_kind = EMPLOYEE`）。
--   故本迁移只种流程定义/版本/图，不建表、不碰任何业务表；`down` 的安全阀也只能靠
--   `biz_flow_instance.biz_type` 判定（没有 `approval_instance_id` 列可查）。
--
-- ── 审批人来源 / 或签 / 兜底 ──────────────────────────────────────────
--   · 直属上级 = `INITIATOR_SUPERIOR`（superior_levels=1）：**不落枚举行** ——
--     它是不可枚举来源，落一行等于把「谁的上级」写死成某个人的上级（V0355 的教训）。
--   · 其余节点绑**角色**（人事经理 / 财务经理 / 总经理），不是写死某个人：
--     岗位换人不用改流程定义。
--   · 所有审批节点 `approval_mode = ANY` 显式落库（`APPROVAL` 的 NULL 虽等价 ANY，
--     但 V0359 之后 NULL 会被当成「没配过」再冻一次）；**没有任何 ALL** ——
--     角色型全员会签 = 一人停用即永久卡单（V0359/V0367 的 P0，
--     `await_flow_countersign_invariants` 断言②）。
--   · 所有审批节点 `empty_assignee = FALLBACK_ADMIN`（SKIP 会把「没人可派」变成
--     自动通过，等于静默放行）+ `timeout_action = REMIND` + 有超时小时。
--   · `self_approval = 'ALLOW'`：拦截走桥侧（`flow::self_approval::over_limit`），
--     刻意不写 FORBID（派发阶段整单失败 = 死单，AGENTS 约定 11）。
--   · `allow_auto_approve = FALSE`：调薪 / 离职都不许免审直过。
--   · 两套都 `is_system = TRUE`：预置流程客户不能删（P2-6b 的 H6 不变量会判红）。
--
-- ── 四重守卫（两套各一套，范本 = V0393 / V0182）──────────────────────
--   ① 已有同名 flow_code 的活流程                → 该套跳过
--   ② 已有同 biz_type 的**默认**流程             → 该套跳过（唯一索引会撞）
--   ③ 该套要绑的角色缺失 / 已软删                 → 该套跳过（种了也派不出去）
--   ④ 重复插 / 版本递增：`change_note LIKE 'V0395 种子%'` + `graph_hash` 双标记；
--      重跑取 `COALESCE(MAX(version_no),0)+1`。
--   守卫跳过**只 NOTICE 不抛异常**（覆盖客户配置比种子没生效更严重）。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
--   重复执行 up：两套 def 都被守卫拦下 → version 被④拦下 → 节点/边/审批人也 0 行
--   ⇒ 结果不变。快照与 hash 按 `graph.rs::snapshot_json()` 同规则离线算出，
--   §6 自检用库内 canonical 化重算对账（对不上整份迁移回滚）。
--
-- ── 回滚 ──────────────────────────────────────────────────────────────
--   见 `V0395__hr_adjust_resign_flow.down.sql`：有 HR_SALARY_ADJUST /
--   HR_EMPLOYEE_RESIGN 审批实例、离职单已挂实例、本卡流程下还有别的版本 → 拒绝回滚。
-- ============================================================================

-- ---------- §0 canonical JSON（与 graph.rs::write_canonical 逐字同义，沿用 V0359/V0367/V0381/V0393）----------
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

    -- 1.2 两个固定 def id 被**别的** flow_code 占用 ⇒ 结构性冲突，直接拦
    SELECT flow_code INTO v_code FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-0000000e1501'::uuid;
    IF v_code IS NOT NULL AND v_code <> 'hr_salary_adjust' THEN
        RAISE EXCEPTION 'HR-38: 固定流程 id …e1501 已被 flow_code=% 占用（不是本卡种的流程）', v_code;
    END IF;
    SELECT flow_code INTO v_code FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-0000000e1531'::uuid;
    IF v_code IS NOT NULL AND v_code <> 'hr_employee_resign' THEN
        RAISE EXCEPTION 'HR-38: 固定流程 id …e1531 已被 flow_code=% 占用（不是本卡种的流程）', v_code;
    END IF;

    -- 1.3 客户自建的同名流程 / 同 biz_type 默认流 ⇒ 守卫会跳过（只提示，不拦）
    IF EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_salary_adjust' AND deleted_at IS NULL
                AND id <> '20000000-0000-4000-8000-0000000e1501'::uuid) THEN
        RAISE NOTICE 'HR-38: 已存在 flow_code=hr_salary_adjust 的流程（非本卡 id）→ 守卫①跳过';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_flow_def WHERE biz_type = 'HR_SALARY_ADJUST' AND is_default AND deleted_at IS NULL
                AND id <> '20000000-0000-4000-8000-0000000e1501'::uuid) THEN
        RAISE NOTICE 'HR-38: 已存在 biz_type=HR_SALARY_ADJUST 的默认流程 → 守卫②跳过';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_employee_resign' AND deleted_at IS NULL
                AND id <> '20000000-0000-4000-8000-0000000e1531'::uuid) THEN
        RAISE NOTICE 'HR-38: 已存在 flow_code=hr_employee_resign 的流程（非本卡 id）→ 守卫①跳过';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_flow_def WHERE biz_type = 'HR_EMPLOYEE_RESIGN' AND is_default AND deleted_at IS NULL
                AND id <> '20000000-0000-4000-8000-0000000e1531'::uuid) THEN
        RAISE NOTICE 'HR-38: 已存在 biz_type=HR_EMPLOYEE_RESIGN 的默认流程 → 守卫②跳过';
    END IF;

    -- 1.4 审批角色（守卫③的输入，只提示）
    SELECT count(*) INTO v_roles
      FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0016'::uuid,  -- HR_MANAGER
                  '20000000-0000-4000-8000-0000000f0005'::uuid,  -- FINANCE_MANAGER
                  '20000000-0000-4000-8000-0000000f0003'::uuid)  -- GENERAL_MANAGER
       AND deleted_at IS NULL;
    IF v_roles <> 3 THEN
        RAISE NOTICE 'HR-38: 审批角色只找到 %/3（HR_MANAGER/FINANCE_MANAGER/GENERAL_MANAGER）→ 相应流程被守卫③跳过', v_roles;
    END IF;
END $$;

-- ============================================================================
-- A. hr_salary_adjust：直属上级 → 人事经理 → 财务经理（三签）
-- ============================================================================

INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve,
    is_system, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000e1501'::uuid,
    'hr_salary_adjust',
    '调薪审批（标准·上级 → 人事 → 财务三签）',
    'HR_SALARY_ADJUST',
    '人力资源',
    '调薪单提交审批：直属上级确认调薪理由与业绩依据，人事经理核薪酬体系与合规（岗位职级带宽、'
    '同岗公平性），财务经理核成本与预算影响。调薪改的是后续每个月的工资，故三道都要过。'
    '审批人绑角色（人事经理 / 财务经理），直属上级取发起人本级部门负责人；或签，换人不用改代码。'
    '驳回或撤回打回 DRAFT，意见追加到调薪单审批轨迹。',
    'PUBLISHED',
    '20000000-0000-4000-8000-0000000e1502'::uuid,
    1,
    TRUE,
    FALSE,
    TRUE,
    NULL
WHERE NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_salary_adjust' AND deleted_at IS NULL)
AND NOT EXISTS (SELECT 1 FROM biz_flow_def
                 WHERE biz_type = 'HR_SALARY_ADJUST' AND is_default AND deleted_at IS NULL)
AND (
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0016'::uuid,
                  '20000000-0000-4000-8000-0000000f0005'::uuid)
       AND deleted_at IS NULL
) = 2;

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000e1502'::uuid,
    d.id,
    COALESCE((SELECT MAX(x.version_no) FROM biz_flow_def_version x WHERE x.def_id = d.id), 0) + 1,
    'PUBLISHED',
    NOT EXISTS (SELECT 1 FROM biz_flow_def_version x WHERE x.def_id = d.id AND x.is_current),
    'a413971a5a5de2bdb11798bfbfa140d927573e541cf4fd7e4f99a99dd6a99171',
    $snapshot${"edges":[{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_hr","is_default":false,"key":"e_hr_fin","label":null,"priority":0,"to":"n_fin"},{"condition":null,"from":"n_sup","is_default":false,"key":"e_sup_hr","label":null,"priority":0,"to":"n_hr"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_sup"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":880,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"财务经理审核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":660,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0016","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_hr","max_remind_times":2,"name":"人事经理审核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":440,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"INITIATOR_SUPERIOR","assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_sup","max_remind_times":2,"name":"直属上级审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    5,
    4,
    'V0395 种子：调薪审批（直属上级 → 人事经理 → 财务经理）默认流程',
    now(),
    NULL
  FROM biz_flow_def d
 WHERE d.flow_code = 'hr_salary_adjust'
   AND d.biz_type = 'HR_SALARY_ADJUST'
   AND d.is_default
   AND d.deleted_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM biz_flow_def_version x
                    WHERE x.def_id = d.id AND x.change_note LIKE 'V0395 种子%')
   AND NOT EXISTS (SELECT 1 FROM biz_flow_def_version x
                    WHERE x.def_id = d.id
                      AND x.graph_hash = 'a413971a5a5de2bdb11798bfbfa140d927573e541cf4fd7e4f99a99dd6a99171')
ON CONFLICT DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e1511'::uuid, '20000000-0000-4000-8000-0000000e1502'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    -- 直属上级：INITIATOR_SUPERIOR + levels=1（随发起人变化，**不落枚举行**）
    ('20000000-0000-4000-8000-0000000e1512'::uuid, '20000000-0000-4000-8000-0000000e1502'::uuid, 'n_sup'::varchar, '直属上级审批'::varchar, 'APPROVAL'::varchar, 'INITIATOR_SUPERIOR'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    -- 人事经理：绑角色 HR_MANAGER（…f0016），或签；48 小时核薪酬体系与合规
    ('20000000-0000-4000-8000-0000000e1513'::uuid, '20000000-0000-4000-8000-0000000e1502'::uuid, 'n_hr'::varchar, '人事经理审核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 440::int, 0::int, 3::int, '{}'::jsonb),
    -- 财务经理：绑角色 FINANCE_MANAGER（…f0005），或签；48 小时核成本与预算
    ('20000000-0000-4000-8000-0000000e1514'::uuid, '20000000-0000-4000-8000-0000000e1502'::uuid, 'n_fin'::varchar, '财务经理审核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 660::int, 0::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000e1515'::uuid, '20000000-0000-4000-8000-0000000e1502'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 880::int, 0::int, 5::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e1502'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e1521'::uuid, '20000000-0000-4000-8000-0000000e1502'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000e1511'::uuid, '20000000-0000-4000-8000-0000000e1512'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e1522'::uuid, '20000000-0000-4000-8000-0000000e1502'::uuid, 'e_sup_hr'::varchar, '20000000-0000-4000-8000-0000000e1512'::uuid, '20000000-0000-4000-8000-0000000e1513'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e1523'::uuid, '20000000-0000-4000-8000-0000000e1502'::uuid, 'e_hr_fin'::varchar, '20000000-0000-4000-8000-0000000e1513'::uuid, '20000000-0000-4000-8000-0000000e1514'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e1524'::uuid, '20000000-0000-4000-8000-0000000e1502'::uuid, 'e_fin_end'::varchar, '20000000-0000-4000-8000-0000000e1514'::uuid, '20000000-0000-4000-8000-0000000e1515'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e1502'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- `n_sup` 不落枚举行（INITIATOR_SUPERIOR 不可枚举）；只绑人事经理与财务经理两个角色。
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e1513'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0016'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000e1514'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e1502'::uuid)
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ============================================================================
-- B. hr_employee_resign：直属上级 → 人事经理 →（关键岗位）总经理
-- ============================================================================

INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve,
    is_system, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000e1531'::uuid,
    'hr_employee_resign',
    '离职审批（标准·上级 → 人事 → 关键岗位总经理）',
    'HR_EMPLOYEE_RESIGN',
    '人力资源',
    '离职单提交审批：直属上级确认交接安排与最后工作日，人事经理核离职类型、补偿与账号处置，'
    '**关键岗位**（管钱管账、核心技术、部门负责人等）再由总经理审批 —— 关键岗位离职牵动'
    '账号权限、客户关系与交接风险。审批人绑角色（人事经理 / 总经理），直属上级取发起人本级'
    '部门负责人；或签，换人不用改代码。通过后由桥侧同一事务处置员工状态、账号停用与在途'
    '请假单（设计 §4.1.3）。驳回或撤回打回 DRAFT。',
    'PUBLISHED',
    '20000000-0000-4000-8000-0000000e1532'::uuid,
    1,
    TRUE,
    FALSE,
    TRUE,
    NULL
WHERE NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_employee_resign' AND deleted_at IS NULL)
AND NOT EXISTS (SELECT 1 FROM biz_flow_def
                 WHERE biz_type = 'HR_EMPLOYEE_RESIGN' AND is_default AND deleted_at IS NULL)
AND (
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0016'::uuid,
                  '20000000-0000-4000-8000-0000000f0003'::uuid)
       AND deleted_at IS NULL
) = 2;

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000e1532'::uuid,
    d.id,
    COALESCE((SELECT MAX(x.version_no) FROM biz_flow_def_version x WHERE x.def_id = d.id), 0) + 1,
    'PUBLISHED',
    NOT EXISTS (SELECT 1 FROM biz_flow_def_version x WHERE x.def_id = d.id AND x.is_current),
    'ec42328b209441d5ea6e403f8bfaef9b8d5dc13f5a83ec1319a36d9864558496',
    $snapshot${"edges":[{"condition":null,"from":"c_key","is_default":true,"key":"e_plain","label":"非关键岗位：人事经理终审","priority":10,"to":"e"},{"condition":{"cmp":"eq","field":"form.is_key_position","op":"cmp","value":true},"from":"c_key","is_default":false,"key":"e_key","label":"关键岗位：总经理审批","priority":0,"to":"n_gm"},{"condition":null,"from":"n_gm","is_default":false,"key":"e_gm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_hr","is_default":false,"key":"e_hr_key","label":null,"priority":0,"to":"c_key"},{"condition":null,"from":"n_sup","is_default":false,"key":"e_sup_hr","label":null,"priority":0,"to":"n_hr"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_sup"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_key","max_remind_times":3,"name":"关键岗位判定","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":660,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":1100,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm","max_remind_times":2,"name":"总经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":72,"x":880,"y":-140},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0016","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_hr","max_remind_times":2,"name":"人事经理审核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":440,"y":0},{"allow_robot":false,"approval_mode":"ANY","approve_ratio":null,"assignee_type":"INITIATOR_SUPERIOR","assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_sup","max_remind_times":2,"name":"直属上级审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb,
    6,
    6,
    'V0395 种子：离职审批（直属上级 → 人事经理 →（关键岗位）总经理）默认流程',
    now(),
    NULL
  FROM biz_flow_def d
 WHERE d.flow_code = 'hr_employee_resign'
   AND d.biz_type = 'HR_EMPLOYEE_RESIGN'
   AND d.is_default
   AND d.deleted_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM biz_flow_def_version x
                    WHERE x.def_id = d.id AND x.change_note LIKE 'V0395 种子%')
   AND NOT EXISTS (SELECT 1 FROM biz_flow_def_version x
                    WHERE x.def_id = d.id
                      AND x.graph_hash = 'ec42328b209441d5ea6e403f8bfaef9b8d5dc13f5a83ec1319a36d9864558496')
ON CONFLICT DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e1541'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    -- 直属上级：INITIATOR_SUPERIOR + levels=1，24 小时确认交接与最后工作日
    ('20000000-0000-4000-8000-0000000e1542'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'n_sup'::varchar, '直属上级审批'::varchar, 'APPROVAL'::varchar, 'INITIATOR_SUPERIOR'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    -- 人事经理：绑角色 HR_MANAGER，或签；48 小时核离职类型 / 补偿 / 账号处置
    ('20000000-0000-4000-8000-0000000e1543'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'n_hr'::varchar, '人事经理审核'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 440::int, 0::int, 3::int, '{}'::jsonb),
    -- 关键岗位判定：唯一条件边（关键岗位 → 总经理）+ 兜底边（非关键岗位 → 结束）
    ('20000000-0000-4000-8000-0000000e1544'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'c_key'::varchar, '关键岗位判定'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 660::int, 0::int, 4::int, '{}'::jsonb),
    -- 总经理：绑角色 GENERAL_MANAGER，或签；72 小时窗口
    ('20000000-0000-4000-8000-0000000e1545'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'n_gm'::varchar, '总经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, 'ANY'::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 72::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 880::int, -140::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000e1546'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 1100::int, 0::int, 6::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e1532'::uuid)
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e1551'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000e1541'::uuid, '20000000-0000-4000-8000-0000000e1542'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e1552'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'e_sup_hr'::varchar, '20000000-0000-4000-8000-0000000e1542'::uuid, '20000000-0000-4000-8000-0000000e1543'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e1553'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'e_hr_key'::varchar, '20000000-0000-4000-8000-0000000e1543'::uuid, '20000000-0000-4000-8000-0000000e1544'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    -- ★ 关键岗位条件边：字段必须是**布尔**（字符串会让 eq 恒为 false → 静默跳过总经理审批）
    ('20000000-0000-4000-8000-0000000e1554'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'e_key'::varchar, '20000000-0000-4000-8000-0000000e1544'::uuid, '20000000-0000-4000-8000-0000000e1545'::uuid, '{"op": "cmp", "field": "form.is_key_position", "cmp": "eq", "value": true}'::jsonb, '关键岗位：总经理审批'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e1555'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'e_plain'::varchar, '20000000-0000-4000-8000-0000000e1544'::uuid, '20000000-0000-4000-8000-0000000e1546'::uuid, NULL::jsonb, '非关键岗位：人事经理终审'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000e1556'::uuid, '20000000-0000-4000-8000-0000000e1532'::uuid, 'e_gm_end'::varchar, '20000000-0000-4000-8000-0000000e1545'::uuid, '20000000-0000-4000-8000-0000000e1546'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e1532'::uuid)
ON CONFLICT (version_id, edge_key) DO NOTHING;

-- `n_sup` 不落枚举行；只绑人事经理与总经理两个角色。
INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e1543'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0016'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000e1545'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e1532'::uuid)
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- §5 def 的「当前版本」指针同步（两套各一条）----------
UPDATE biz_flow_def d
   SET current_version_id = v.id,
       current_version_no = v.version_no,
       updated_at         = now()
  FROM biz_flow_def_version v
 WHERE v.id = '20000000-0000-4000-8000-0000000e1502'::uuid
   AND d.id = v.def_id
   AND d.deleted_at IS NULL
   AND (d.current_version_id IS NULL OR d.current_version_id = v.id);

UPDATE biz_flow_def d
   SET current_version_id = v.id,
       current_version_no = v.version_no,
       updated_at         = now()
  FROM biz_flow_def_version v
 WHERE v.id = '20000000-0000-4000-8000-0000000e1532'::uuid
   AND d.id = v.def_id
   AND d.deleted_at IS NULL
   AND (d.current_version_id IS NULL OR d.current_version_id = v.id);

-- ---------- §6 自检（两套逐个断言；被守卫对应跳过的不校验）----------
DO $$
DECLARE
    v_rows   integer;
    v_seq    text;
    v_cur    uuid;
    v_cond   jsonb;
    v_recalc text;
BEGIN
    -- ========== 6.A hr_salary_adjust（5 节点 / 4 边）==========
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_salary_adjust' AND deleted_at IS NULL) THEN
        RAISE NOTICE 'HR-38: hr_salary_adjust 种子被守卫跳过（已有同名流程 / 已 HR_SALARY_ADJUST 默认流 / 角色缺失）';
    ELSE
        SELECT current_version_id INTO v_cur FROM biz_flow_def
         WHERE id = '20000000-0000-4000-8000-0000000e1501'::uuid AND deleted_at IS NULL;
        IF v_cur IS NULL THEN
            RAISE NOTICE 'HR-38: hr_salary_adjust 由客户自建（非本卡 id …e1501），跳过自检';
        ELSIF v_cur <> '20000000-0000-4000-8000-0000000e1502'::uuid
           OR (SELECT change_note FROM biz_flow_def_version
                WHERE id = '20000000-0000-4000-8000-0000000e1502'::uuid)
              IS DISTINCT FROM 'V0395 种子：调薪审批（直属上级 → 人事经理 → 财务经理）默认流程' THEN
            RAISE NOTICE 'HR-38: hr_salary_adjust 已被客户改过（当前版本 %），跳过形态自检', v_cur;
        ELSE
            SELECT count(*) INTO v_rows FROM biz_flow_def
             WHERE id = '20000000-0000-4000-8000-0000000e1501'::uuid
               AND status = 'PUBLISHED' AND is_default AND is_system
               AND allow_auto_approve = FALSE
               AND biz_type = 'HR_SALARY_ADJUST' AND flow_code = 'hr_salary_adjust'
               AND category = '人力资源' AND deleted_at IS NULL;
            IF v_rows <> 1 THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 定义形态不对（应 PUBLISHED / is_default / is_system / 免审 FALSE / category=人力资源）';
            END IF;

            IF NOT EXISTS (SELECT 1 FROM biz_flow_def_version
                            WHERE id = '20000000-0000-4000-8000-0000000e1502'::uuid) THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 的 current_version_id 指向不存在的版本';
            END IF;

            SELECT count(*) INTO v_rows FROM biz_flow_def_version
             WHERE id = '20000000-0000-4000-8000-0000000e1502'::uuid
               AND version_status = 'PUBLISHED' AND is_current
               AND node_count = 5 AND edge_count = 4 AND published_at IS NOT NULL;
            IF v_rows <> 1 THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 版本行形态不对（应 PUBLISHED / is_current / 5 节点 / 4 边）';
            END IF;

            SELECT encode(sha256(convert_to(pg_temp.canonical_json(graph_snapshot), 'UTF8')), 'hex')
              INTO v_recalc FROM biz_flow_def_version
             WHERE id = '20000000-0000-4000-8000-0000000e1502'::uuid;
            IF v_recalc IS DISTINCT FROM 'a413971a5a5de2bdb11798bfbfa140d927573e541cf4fd7e4f99a99dd6a99171' THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust graph_hash 与快照重算不一致（库内 %）', v_recalc;
            END IF;

            SELECT string_agg(node_type, ',' ORDER BY sort_order) INTO v_seq
              FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000e1502'::uuid;
            IF v_seq IS DISTINCT FROM 'START,APPROVAL,APPROVAL,APPROVAL,END' THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 节点类型序列 = %（期望 START,APPROVAL,APPROVAL,APPROVAL,END）', v_seq;
            END IF;

            SELECT count(*) INTO v_rows FROM biz_flow_node
             WHERE version_id = '20000000-0000-4000-8000-0000000e1502'::uuid
               AND node_type = 'APPROVAL' AND approval_mode = 'ANY'
               AND empty_assignee = 'FALLBACK_ADMIN' AND timeout_action = 'REMIND'
               AND timeout_hours IS NOT NULL;
            IF v_rows <> 3 THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 的「或签 + 兜底 + 超时只提醒」审批节点 = %（期望 3）', v_rows;
            END IF;

            SELECT count(*) INTO v_rows FROM biz_flow_node
             WHERE version_id = '20000000-0000-4000-8000-0000000e1502'::uuid
               AND node_type IN ('APPROVAL', 'COUNTERSIGN')
               AND (approval_mode = 'ALL' OR approval_mode IS NULL);
            IF v_rows <> 0 THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 有 % 个审批节点是全员会签 / 未落 approval_mode', v_rows;
            END IF;

            -- 直属上级不落枚举行；另两个角色各一行 ⇒ 恰好 2 行
            SELECT count(*) INTO v_rows
              FROM biz_flow_node_assignee a
              JOIN biz_flow_node n ON n.id = a.node_id
             WHERE n.version_id = '20000000-0000-4000-8000-0000000e1502'::uuid;
            IF v_rows <> 2 THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 审批人绑定 = % 行（期望 2：人事经理 + 财务经理）', v_rows;
            END IF;
            SELECT count(*) INTO v_rows
              FROM biz_flow_node_assignee a
              JOIN biz_flow_node n ON n.id = a.node_id
             WHERE n.version_id = '20000000-0000-4000-8000-0000000e1502'::uuid
               AND n.node_key = 'n_sup';
            IF v_rows <> 0 THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 的 n_sup 不许落枚举行（INITIATOR_SUPERIOR 不可枚举）';
            END IF;

            SELECT count(*) INTO v_rows
              FROM biz_flow_edge e
              JOIN biz_flow_node f ON f.id = e.from_node_id
              JOIN biz_flow_node t ON t.id = e.to_node_id
             WHERE e.version_id = '20000000-0000-4000-8000-0000000e1502'::uuid
               AND ((f.node_key = 's'     AND t.node_key = 'n_sup' AND e.edge_key = 'e_start')
                 OR (f.node_key = 'n_sup' AND t.node_key = 'n_hr'  AND e.edge_key = 'e_sup_hr')
                 OR (f.node_key = 'n_hr'  AND t.node_key = 'n_fin' AND e.edge_key = 'e_hr_fin')
                 OR (f.node_key = 'n_fin' AND t.node_key = 'e'     AND e.edge_key = 'e_fin_end'));
            IF v_rows <> 4 THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 骨架边数 = %（期望 4：s→n_sup→n_hr→n_fin→e）', v_rows;
            END IF;
            SELECT count(*) INTO v_rows
              FROM biz_flow_edge e JOIN biz_flow_node t ON t.id = e.to_node_id
             WHERE e.version_id = '20000000-0000-4000-8000-0000000e1502'::uuid AND t.node_key = 's';
            IF v_rows <> 0 THEN
                RAISE EXCEPTION 'HR-38: hr_salary_adjust 开始节点有 % 条入边', v_rows;
            END IF;

            RAISE NOTICE 'HR-38 自检通过：hr_salary_adjust（PUBLISHED / 5 节点 / 4 边 / 直属上级 → 人事经理 → 财务经理 / 快照 hash 自洽）';
        END IF;
    END IF;

    -- ========== 6.B hr_employee_resign（6 节点 / 6 边）==========
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_employee_resign' AND deleted_at IS NULL) THEN
        RAISE NOTICE 'HR-38: hr_employee_resign 种子被守卫跳过（已有同名流程 / 已 HR_EMPLOYEE_RESIGN 默认流 / 角色缺失）';
    ELSE
        SELECT current_version_id INTO v_cur FROM biz_flow_def
         WHERE id = '20000000-0000-4000-8000-0000000e1531'::uuid AND deleted_at IS NULL;
        IF v_cur IS NULL THEN
            RAISE NOTICE 'HR-38: hr_employee_resign 由客户自建（非本卡 id …e1531），跳过自检';
        ELSIF v_cur <> '20000000-0000-4000-8000-0000000e1532'::uuid
           OR (SELECT change_note FROM biz_flow_def_version
                WHERE id = '20000000-0000-4000-8000-0000000e1532'::uuid)
              IS DISTINCT FROM 'V0395 种子：离职审批（直属上级 → 人事经理 →（关键岗位）总经理）默认流程' THEN
            RAISE NOTICE 'HR-38: hr_employee_resign 已被客户改过（当前版本 %），跳过形态自检', v_cur;
        ELSE
            SELECT count(*) INTO v_rows FROM biz_flow_def
             WHERE id = '20000000-0000-4000-8000-0000000e1531'::uuid
               AND status = 'PUBLISHED' AND is_default AND is_system
               AND allow_auto_approve = FALSE
               AND biz_type = 'HR_EMPLOYEE_RESIGN' AND flow_code = 'hr_employee_resign'
               AND category = '人力资源' AND deleted_at IS NULL;
            IF v_rows <> 1 THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign 定义形态不对（应 PUBLISHED / is_default / is_system / 免审 FALSE / category=人力资源）';
            END IF;

            IF NOT EXISTS (SELECT 1 FROM biz_flow_def_version
                            WHERE id = '20000000-0000-4000-8000-0000000e1532'::uuid) THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign 的 current_version_id 指向不存在的版本';
            END IF;

            SELECT count(*) INTO v_rows FROM biz_flow_def_version
             WHERE id = '20000000-0000-4000-8000-0000000e1532'::uuid
               AND version_status = 'PUBLISHED' AND is_current
               AND node_count = 6 AND edge_count = 6 AND published_at IS NOT NULL;
            IF v_rows <> 1 THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign 版本行形态不对（应 PUBLISHED / is_current / 6 节点 / 6 边）';
            END IF;

            SELECT encode(sha256(convert_to(pg_temp.canonical_json(graph_snapshot), 'UTF8')), 'hex')
              INTO v_recalc FROM biz_flow_def_version
             WHERE id = '20000000-0000-4000-8000-0000000e1532'::uuid;
            IF v_recalc IS DISTINCT FROM 'ec42328b209441d5ea6e403f8bfaef9b8d5dc13f5a83ec1319a36d9864558496' THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign graph_hash 与快照重算不一致（库内 %）', v_recalc;
            END IF;

            SELECT string_agg(node_type, ',' ORDER BY sort_order) INTO v_seq
              FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000e1532'::uuid;
            IF v_seq IS DISTINCT FROM 'START,APPROVAL,APPROVAL,CONDITION,APPROVAL,END' THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign 节点类型序列 = %（期望 START,APPROVAL,APPROVAL,CONDITION,APPROVAL,END）', v_seq;
            END IF;

            SELECT count(*) INTO v_rows FROM biz_flow_node
             WHERE version_id = '20000000-0000-4000-8000-0000000e1532'::uuid
               AND node_type = 'APPROVAL' AND approval_mode = 'ANY'
               AND empty_assignee = 'FALLBACK_ADMIN' AND timeout_action = 'REMIND'
               AND timeout_hours IS NOT NULL;
            IF v_rows <> 3 THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign 的「或签 + 兜底 + 超时只提醒」审批节点 = %（期望 3）', v_rows;
            END IF;

            SELECT count(*) INTO v_rows FROM biz_flow_node
             WHERE version_id = '20000000-0000-4000-8000-0000000e1532'::uuid
               AND node_type IN ('APPROVAL', 'COUNTERSIGN')
               AND (approval_mode = 'ALL' OR approval_mode IS NULL);
            IF v_rows <> 0 THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign 有 % 个审批节点是全员会签 / 未落 approval_mode', v_rows;
            END IF;

            -- 直属上级不落枚举行 ⇒ 恰好 2 行（人事经理 + 总经理）
            SELECT count(*) INTO v_rows
              FROM biz_flow_node_assignee a
              JOIN biz_flow_node n ON n.id = a.node_id
             WHERE n.version_id = '20000000-0000-4000-8000-0000000e1532'::uuid;
            IF v_rows <> 2 THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign 审批人绑定 = % 行（期望 2：人事经理 + 总经理）', v_rows;
            END IF;

            SELECT count(*) INTO v_rows
              FROM biz_flow_edge e
              JOIN biz_flow_node f ON f.id = e.from_node_id
              JOIN biz_flow_node t ON t.id = e.to_node_id
             WHERE e.version_id = '20000000-0000-4000-8000-0000000e1532'::uuid
               AND ((f.node_key = 's'     AND t.node_key = 'n_sup' AND e.edge_key = 'e_start')
                 OR (f.node_key = 'n_sup' AND t.node_key = 'n_hr'  AND e.edge_key = 'e_sup_hr')
                 OR (f.node_key = 'n_hr'  AND t.node_key = 'c_key' AND e.edge_key = 'e_hr_key')
                 OR (f.node_key = 'c_key' AND t.node_key = 'n_gm'  AND e.edge_key = 'e_key')
                 OR (f.node_key = 'c_key' AND t.node_key = 'e'     AND e.edge_key = 'e_plain')
                 OR (f.node_key = 'n_gm'  AND t.node_key = 'e'     AND e.edge_key = 'e_gm_end'));
            IF v_rows <> 6 THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign 骨架边数 = %（期望 6：s→n_sup→n_hr→c_key→{e | n_gm→e}）', v_rows;
            END IF;

            SELECT count(*) INTO v_rows FROM biz_flow_edge
             WHERE version_id = '20000000-0000-4000-8000-0000000e1532'::uuid AND is_default;
            IF v_rows <> 1 THEN
                RAISE EXCEPTION 'HR-38: hr_employee_resign 兜底边 = % 条（期望恰好 1 条：非关键岗位 → 结束）', v_rows;
            END IF;

            -- 条件边：布尔字段 + eq + true（字符串会让 eq 恒 false → 静默跳过总经理审批）
            SELECT condition_expr INTO v_cond FROM biz_flow_edge
             WHERE version_id = '20000000-0000-4000-8000-0000000e1532'::uuid AND edge_key = 'e_key';
            IF v_cond IS NULL
               OR v_cond->>'field' IS DISTINCT FROM 'form.is_key_position'
               OR v_cond->>'cmp'   IS DISTINCT FROM 'eq'
               OR (v_cond->>'value') IS DISTINCT FROM 'true' THEN
                RAISE EXCEPTION 'HR-38: 关键岗位条件边形态不对（期望 form.is_key_position eq true，实际 %）', v_cond;
            END IF;

            RAISE NOTICE 'HR-38 自检通过：hr_employee_resign（PUBLISHED / 6 节点 / 6 边 / 直属上级 → 人事经理 →（关键岗位）总经理 / 快照 hash 自洽）';
        END IF;
    END IF;
END $$;
