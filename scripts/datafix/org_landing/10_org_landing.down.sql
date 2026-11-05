-- ============================================================================
-- ORGLANDING / 10 · 回滚：按快照把 4 个账号的 dept_id 写回落位前的值
-- ----------------------------------------------------------------------------
-- 数据来源：datafix_orglanding_snapshot（10_org_landing.up.sql 在 DML 之前用 to_jsonb(u) 存了整行）
--   ⇒ 不是「无条件置 NULL」，而是**写回快照里那一刻的值**（本次 4 行原值确实全 NULL，
--     但口径仍是快照值 —— 防「up 与 down 之间被人改过」这类事故）。
--
-- 幂等：只更新「当前 dept_id 与快照不一致」的 4 行；第二遍 0 行。
-- 安全阀：快照不是 4 行 → 拒绝回滚（说明 up 没干净跑过，或快照表被别的批次污染）。
-- 只碰 sys_user.dept_id / updated_at：不动角色、不动 sys_dept、不动业务单据。
-- ============================================================================
DO $$
DECLARE
    c_batch CONSTANT text := 'ORGLANDING-20261105';
    c_users CONSTANT text[] := ARRAY['admin', 'laoliu', 'laoliu_test', 'cl_43870'];
    v_snap int;
    v_n    int := 0;
    v_bad  text;
BEGIN
    IF to_regclass('public.datafix_orglanding_snapshot') IS NULL THEN
        RAISE EXCEPTION 'ORGLANDING/10 down: 没有 datafix_orglanding_snapshot 表 —— up 没跑过，无从回滚';
    END IF;

    SELECT count(*) INTO v_snap
      FROM datafix_orglanding_snapshot
     WHERE batch = c_batch AND tbl = 'sys_user';
    IF v_snap <> 4 THEN
        RAISE EXCEPTION 'ORGLANDING/10 down: 快照 % 行 ≠ 4 —— 拒绝回滚，请人工核对快照表', v_snap;
    END IF;

    UPDATE sys_user u
       SET dept_id    = (e.row_data->>'dept_id')::uuid,
           updated_at = (e.row_data->>'updated_at')::timestamptz
      FROM datafix_orglanding_snapshot e
     WHERE e.batch = c_batch
       AND e.tbl   = 'sys_user'
       AND e.pk    = u.id::text
       AND u.deleted_at IS NULL
       AND u.dept_id IS DISTINCT FROM (e.row_data->>'dept_id')::uuid;
    GET DIAGNOSTICS v_n = ROW_COUNT;

    -- 结果断言：4 行都必须回到快照值
    SELECT string_agg(u.username, ', ') INTO v_bad
      FROM sys_user u
      JOIN datafix_orglanding_snapshot e
        ON e.batch = c_batch AND e.tbl = 'sys_user' AND e.pk = u.id::text
     WHERE u.dept_id IS DISTINCT FROM (e.row_data->>'dept_id')::uuid;
    IF v_bad IS NOT NULL THEN
        RAISE EXCEPTION 'ORGLANDING/10 down: 还原后仍与快照不一致：% —— 整脚本回滚', v_bad;
    END IF;

    RAISE NOTICE 'ORGLANDING/10 down: 已还原 % 行 dept_id（第二遍跑会报 0；快照 % 行保留，供再 up 前核对）', v_n, v_snap;
END
$$;
