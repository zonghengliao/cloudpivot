-- ============================================================================
-- ORGLANDING / 10：组织落位 —— 给 4 个在用账号补 sys_user.dept_id
-- ----------------------------------------------------------------------------
-- 上游决策：docs/architecture/P0-FIX-2B-生产组织落位与超管收敛-决策.md（D-1）
-- 卡片：t_db82c38e（【P0-FIX-2B-A】）
--
-- 目标态（照表落，不许自由发挥）：
--   admin       → HQ   （唯一保留的超管，落位是为了让「无部门账号」告警归零）
--   laoliu      → HQ   （平台实施 / 总管，业务上属总部职能）
--   laoliu_test → TECH （持 3 档业务角色；落 HQ 会让 3 档 = 全公司，摘权等于没摘）
--   cl_43870    → HQ   （0 次登录的残留号，按 N2 判据属「在用 HUMAN」；P0-FIX-3 随号删除）
-- 明确不动的：
--   henry.liao            保持 NULL（docs/待决策.md 铁律①未确认，归 C 卡）
--   verify-ai             保持 NULL（user_type='ROBOT'，AI 用户不进组织树）
--   qa.limited / qa.mutex / qa.viewer  保持原状（锁定夹具，qa.limited=TECH、qa.mutex=SALES 是夹具语义）
--   sys_dept.leader_id    一律不设（D-6：现有「人」里没有真实部门主管）
--   sys_user_role         一条不增删（摘权是 C 卡）
--   biz_task / biz_project 的 dept_id 不回填（D-3：846 行里 766 行已软删，回填 = 把今天的组织编造成历史）
--
-- 部门怎么定位：一律按 `dept_code`（HQ / TECH）解析 uuid，**不硬编码 uuid**
--   （V0247 已是这个口径：基础版的部门由运维用接口建，各环境 uuid 不同）。
--   目标库没有组织树 → RAISE NOTICE「跳过」+ RAISE EXCEPTION（调用方看到非 0 退出），
--   绝不猜、绝不造部门。
--
-- SOP（docs/architecture/数据修复与清场SOP.md）：同一事务里 ①先整行快照 ②硬断言
--   快照行数 = 目标行数 ③幂等 DML ④down 按快照还原。本脚本是一个事务（psql -1）。
--
-- 幂等：每条 UPDATE 都带 `dept_id IS DISTINCT FROM <目标>`，第二遍命中 0 行。
-- 安全阀（命中即 RAISE EXCEPTION，整脚本回滚）：
--   * 目标账号不存在或已软删；
--   * 部门树缺 HQ / TECH，或同名 dept_code 不唯一；
--   * 目标账号当前 dept_id 既不是 NULL 也不是目标值（说明有人已经做了别的决定）；
--   * 快照行数 ≠ 目标行数（SOP 红线：先改后补快照 = 违规）；
--   * 落位后仍有目标行没到位，或「不动名单」的 dept_id 被改，或 sys_user_role 行数变化。
-- ============================================================================

-- 整行快照表（down 精确写回用；形状照抄 SOP §1：pk 一律 text）
CREATE TABLE IF NOT EXISTS datafix_orglanding_snapshot (
    batch       text        NOT NULL,
    tbl         text        NOT NULL,
    pk          text        NOT NULL,
    row_data    jsonb       NOT NULL,
    captured_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (batch, tbl, pk)
);
COMMENT ON TABLE datafix_orglanding_snapshot IS
    'P0-FIX-2B 组织落位整行快照（batch=ORGLANDING-20261105）。10_org_landing.down.sql 按此精确写回；本批次收口后可整表 DROP。';

DO $$
DECLARE
    c_batch  CONSTANT text   := 'ORGLANDING-20261105';
    -- 目标表：username / dept_code 一一对应（要改名单先改这两行 + 上方注释）
    c_users  CONSTANT text[] := ARRAY['admin', 'laoliu', 'laoliu_test', 'cl_43870'];
    c_depts  CONSTANT text[] := ARRAY['HQ',    'HQ',     'TECH',        'HQ'];
    -- 不动名单（落位后必须仍无部门）：AI 用户 + 待确认身份 + 未落位的夹具
    c_untouched CONSTANT text[] := ARRAY['henry.liao', 'verify-ai', 'qa.viewer'];
    c_expect CONSTANT int := 4;

    v_hq        uuid;
    v_tech      uuid;
    v_n_hq      int;
    v_n_tech    int;
    v_uid       uuid;
    v_cur       uuid;
    v_tgt       uuid;
    v_targets   uuid[] := ARRAY[]::uuid[];
    v_cur_depts uuid[] := ARRAY[]::uuid[];
    v_tgt_cnt   int;
    v_snap      int;
    v_inserted  int;
    v_changed   int := 0;
    v_n         int;
    v_bad       text := '';
    v_ur_before int;
    v_ur_after  int;
    v_task_before int;
    v_task_after  int;
    v_proj_before int;
    v_proj_after  int;
    v_lead_before int;
    v_lead_after  int;
    i           int;
BEGIN
    -- ---------- 0) 组织树前置：按 dept_code 解析，缺就不猜 ----------
    SELECT count(*) INTO v_n_hq FROM sys_dept WHERE dept_code = 'HQ' AND deleted_at IS NULL;
    SELECT count(*) INTO v_n_tech FROM sys_dept WHERE dept_code = 'TECH' AND deleted_at IS NULL;

    IF v_n_hq <> 1 OR v_n_tech <> 1 THEN
        RAISE NOTICE 'ORGLANDING: 目标库组织树不满足前置条件（HQ % 个 / TECH % 个，各需恰好 1 个）—— 跳过，不猜部门、不造部门',
            v_n_hq, v_n_tech;
        RAISE EXCEPTION 'ORGLANDING: 跳过组织落位（部门树缺失或 dept_code 不唯一，HQ=% TECH=%）', v_n_hq, v_n_tech;
    END IF;

    SELECT id INTO v_hq   FROM sys_dept WHERE dept_code = 'HQ'   AND deleted_at IS NULL;
    SELECT id INTO v_tech FROM sys_dept WHERE dept_code = 'TECH' AND deleted_at IS NULL;

    -- ---------- 1) 解析四行目标 + 安全阀（当前值既非 NULL 也非目标 → 停手）----------
    FOR i IN 1..array_length(c_users, 1) LOOP
        SELECT u.id, u.dept_id INTO v_uid, v_cur
          FROM sys_user u
         WHERE u.username = c_users[i] AND u.deleted_at IS NULL;

        IF v_uid IS NULL THEN
            RAISE EXCEPTION 'ORGLANDING: 找不到在用账号 %（不存在或已软删）—— 停下人工核对，不自动落位', c_users[i];
        END IF;

        v_tgt := CASE c_depts[i] WHEN 'HQ' THEN v_hq WHEN 'TECH' THEN v_tech END;
        IF v_tgt IS NULL THEN
            RAISE EXCEPTION 'ORGLANDING: 账号 % 的目标 dept_code=% 不在本脚本支持的取值（HQ/TECH）', c_users[i], c_depts[i];
        END IF;

        IF v_cur IS NOT NULL AND v_cur <> v_tgt THEN
            RAISE EXCEPTION 'ORGLANDING: 账号 % 当前 dept_id=%（非 NULL 且不是目标 %）—— 有人已经做过别的决定，停下人工核对',
                c_users[i], v_cur, c_depts[i];
        END IF;

        v_targets   := v_targets   || v_tgt;
        v_cur_depts := v_cur_depts || v_cur;
    END LOOP;

    -- ---------- 2) 先记「不该变」的三个计数（落位后复核）----------
    SELECT count(*) INTO v_ur_before   FROM sys_user_role;
    SELECT count(*) INTO v_task_before FROM biz_task     WHERE dept_id IS NOT NULL;
    SELECT count(*) INTO v_proj_before FROM biz_project  WHERE dept_id IS NOT NULL;
    SELECT count(*) INTO v_lead_before FROM sys_dept     WHERE leader_id IS NOT NULL;

    -- ---------- 3) 整行快照（与下面 DML 完全相同的 WHERE，先于任何 DML）----------
    INSERT INTO datafix_orglanding_snapshot (batch, tbl, pk, row_data)
    SELECT c_batch, 'sys_user', u.id::text, to_jsonb(u)
      FROM sys_user u
     WHERE u.username = ANY (c_users)
       AND u.deleted_at IS NULL
    ON CONFLICT (batch, tbl, pk) DO NOTHING;
    GET DIAGNOSTICS v_inserted = ROW_COUNT;

    -- ---------- 4) 硬断言：快照行数 = 目标行数（SOP 红线）----------
    -- 判据是**表里这一批次的总行数**，不是本次插入的行数：第二次跑 apply 时
    -- ON CONFLICT DO NOTHING 插 0 行是正确的（快照已经在了），不能因此报错。
    SELECT count(*) INTO v_tgt_cnt
      FROM sys_user u
     WHERE u.username = ANY (c_users) AND u.deleted_at IS NULL;
    SELECT count(*) INTO v_snap
      FROM datafix_orglanding_snapshot
     WHERE batch = c_batch AND tbl = 'sys_user';

    IF v_tgt_cnt <> c_expect THEN
        RAISE EXCEPTION 'ORGLANDING: 目标行数 % ≠ 预期 %（账号名单与库不一致）—— 停止修数', v_tgt_cnt, c_expect;
    END IF;
    IF v_snap <> v_tgt_cnt THEN
        RAISE EXCEPTION 'ORGLANDING: 快照 % 行 ≠ 目标 % 行 —— 停止修数（先改后补快照 = 违规）', v_snap, v_tgt_cnt;
    END IF;
    RAISE NOTICE 'ORGLANDING: 快照就绪 % 行（batch=%，本次新插入 % 行），与目标行数一致',
        v_snap, c_batch, v_inserted;

    -- ---------- 5) 幂等 DML：逐账号落位 ----------
    FOR i IN 1..array_length(c_users, 1) LOOP
        UPDATE sys_user
           SET dept_id = v_targets[i],
               updated_at = now()
         WHERE username = c_users[i]
           AND deleted_at IS NULL
           AND dept_id IS DISTINCT FROM v_targets[i];
        GET DIAGNOSTICS v_n = ROW_COUNT;
        v_changed := v_changed + v_n;
        RAISE NOTICE 'ORGLANDING: % → %：变更 % 行（0 = 已经是目标态，幂等）', c_users[i], c_depts[i], v_n;
    END LOOP;
    RAISE NOTICE 'ORGLANDING: 本次共变更 % 行', v_changed;

    -- ---------- 6) 落位结果断言：4 行必须都到位 ----------
    FOR i IN 1..array_length(c_users, 1) LOOP
        IF NOT EXISTS (SELECT 1 FROM sys_user
                        WHERE username = c_users[i] AND deleted_at IS NULL
                          AND dept_id = v_targets[i]) THEN
            v_bad := v_bad || c_users[i] || '(' || c_depts[i] || ') ';
        END IF;
    END LOOP;
    IF v_bad <> '' THEN
        RAISE EXCEPTION 'ORGLANDING: 落位未生效：% —— 整脚本回滚', v_bad;
    END IF;

    -- ---------- 7) 不动名单 / 禁改项复核（任何一条变了就整体回滚）----------
    SELECT string_agg(u.username || '(dept=' || coalesce(u.dept_id::text, 'NULL') || ')', ', ')
      INTO v_bad
      FROM sys_user u
     WHERE u.username = ANY (c_untouched)
       AND u.deleted_at IS NULL
       AND u.dept_id IS NOT NULL;
    IF v_bad IS NOT NULL THEN
        RAISE EXCEPTION 'ORGLANDING: 不动名单被改了部门：% —— 整脚本回滚', v_bad;
    END IF;

    SELECT count(*) INTO v_ur_after   FROM sys_user_role;
    SELECT count(*) INTO v_task_after FROM biz_task    WHERE dept_id IS NOT NULL;
    SELECT count(*) INTO v_proj_after FROM biz_project WHERE dept_id IS NOT NULL;
    SELECT count(*) INTO v_lead_after FROM sys_dept    WHERE leader_id IS NOT NULL;

    IF v_ur_after <> v_ur_before THEN
        RAISE EXCEPTION 'ORGLANDING: sys_user_role 行数 % → % —— 本卡禁止动角色，整脚本回滚', v_ur_before, v_ur_after;
    END IF;
    IF v_task_after <> v_task_before OR v_proj_after <> v_proj_before THEN
        RAISE EXCEPTION 'ORGLANDING: biz_task/biz_project 的 dept_id 非空行数变了（%/% → %/%）—— D-3 禁止回填历史，整脚本回滚',
            v_task_before, v_proj_before, v_task_after, v_proj_after;
    END IF;
    IF v_lead_after <> v_lead_before THEN
        RAISE EXCEPTION 'ORGLANDING: sys_dept.leader_id 非空行数 % → % —— D-6 禁止设主管，整脚本回滚', v_lead_before, v_lead_after;
    END IF;

    RAISE NOTICE 'ORGLANDING: 收口复核通过 —— sys_user_role % 行未变、biz_task/biz_project dept_id 非空 %/% 行未变、sys_dept.leader_id 非空 % 行未变',
        v_ur_after, v_task_after, v_proj_after, v_lead_after;
    RAISE NOTICE 'ORGLANDING: 完成（batch=%）：admin/laoliu/cl_43870 → HQ，laoliu_test → TECH；henry.liao / verify-ai / qa.* 未动', c_batch;
END
$$;
