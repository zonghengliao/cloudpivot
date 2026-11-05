-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 0：给 henry.liao 补 dept_id（→ HQ）
-- ----------------------------------------------------------------------------
-- 授权：卡面评论 `laoliu` 2026-11-05 16:45（用户原话「henry.liao 是我建的自己的测试号。可以调整」）
--   ⇒ 决策 §2.3 的「客户/演示号」分支：落位 HQ + 摘 SUPER_ADMIN + 保留岗位角色。
--
-- 为什么这一步必须做（不是可选项）：
--   1. `erp_invariants` 的 N2 判据 = 「在用 HUMAN 账号必须 dept_id NOT NULL」（WARN）。
--      henry.liao 是 status=1 的 HUMAN，A 卡当时按「身份未确认」留 NULL —— 那是生产上
--      唯一一条 WARN。本卡验收 C5 要求 BLOCKER 0 / WARN 0，C4 要求 N2 升 BLOCKER 后仍绿，
--      所以他的 dept_id 必须落地，否则这两条按定义不可达。
--   2. 摘 SUPER_ADMIN 之后他只剩 4 个业务角色，其中 GENERAL_MANAGER 的 data_scope=1（全部）
--      仍把生效范围顶在 All —— 但「无部门」会让 N2 继续点名，且一旦哪天有人把
--      GENERAL_MANAGER 从他身上摘掉，DEPT_LEADER/PURCHASE_MANAGER/WAREHOUSE_MANAGER 三个
--      3 档角色 + 无部门 ⇒ `merge_scope_rows` 降级 SelfOnly（S10/S11）。先落位把这个坑填掉。
--   3. 落位**不改变任何生效范围**：他今天就是 All（靠 GENERAL_MANAGER 的 data_scope=1），
--      落 HQ 之后还是 All。影响面在 apply 前后各跑一次 `org_landing_impact.py` 对平。
--
-- 为什么是 HQ 而不是 FIN/SALES/TECH：他是「老板自己的测试号」，没有真实业务归属；
--   决策 §2.3 对「客户/演示号」给的落点就是 HQ（根节点）。落 TECH 会让他那三个 3 档角色
--   只看技术部，反而是凭空编造了一个业务归属。
--
-- 幂等：已 = HQ ⇒ 打 NOTICE 直接返回（0 行变更）；当前值既非 NULL 又非 HQ ⇒ 抛异常停手
--       （说明有人已经做了别的决定，交人判断）。
-- 快照：整行写进 datafix_superconv_snapshot（batch=SUPERCONV-20261105-LAND），down 精确写回。
-- ============================================================================
\set ON_ERROR_STOP on

CREATE TABLE IF NOT EXISTS datafix_superconv_snapshot (
  batch       text        NOT NULL,
  tbl         text        NOT NULL,
  pk          text        NOT NULL,
  row_data    jsonb       NOT NULL,
  captured_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (batch, tbl, pk)
);

DO $$
DECLARE
  v_batch text := 'SUPERCONV-20261105-LAND';
  v_uid   uuid;
  v_cur   uuid;
  v_st    int2;
  v_hq    uuid;
  v_n     int;
  v_snap  int;
  v_upd   int;
BEGIN
  SELECT u.id, u.dept_id, u.status INTO v_uid, v_cur, v_st
    FROM sys_user u WHERE u.username = 'henry.liao' AND u.deleted_at IS NULL;
  IF v_uid IS NULL THEN
    RAISE EXCEPTION '前置失败：找不到账号 henry.liao';
  END IF;
  IF v_st <> 1 THEN
    RAISE EXCEPTION '前置失败：henry.liao status=%（期望 1=启用）；停用账号不落位', v_st;
  END IF;

  SELECT count(*) INTO v_n FROM sys_dept WHERE dept_code = 'HQ' AND deleted_at IS NULL;
  IF v_n <> 1 THEN
    RAISE EXCEPTION '前置失败：dept_code=HQ 的部门期望 1 个，实际 % 个（不猜、不造部门）', v_n;
  END IF;
  SELECT id INTO v_hq FROM sys_dept WHERE dept_code = 'HQ' AND deleted_at IS NULL;

  IF v_cur = v_hq THEN
    RAISE NOTICE 'henry.liao 已在 HQ，本脚本无变更（幂等重放）';
    RETURN;
  END IF;
  IF v_cur IS NOT NULL THEN
    RAISE EXCEPTION '前置失败：henry.liao 当前 dept_id=% 既不是 NULL 也不是 HQ，停手交人判断', v_cur;
  END IF;

  -- 快照（先于 DML；SOP §1 红线）
  INSERT INTO datafix_superconv_snapshot (batch, tbl, pk, row_data)
  SELECT v_batch, 'sys_user', u.id::text, to_jsonb(u) FROM sys_user u WHERE u.id = v_uid
  ON CONFLICT DO NOTHING;
  SELECT count(*) INTO v_snap FROM datafix_superconv_snapshot
   WHERE batch = v_batch AND tbl = 'sys_user';
  IF v_snap <> 1 THEN
    RAISE EXCEPTION '快照 % 行 ≠ 目标 1 行，停止修数', v_snap;
  END IF;

  UPDATE sys_user SET dept_id = v_hq, updated_at = now()
   WHERE id = v_uid AND dept_id IS DISTINCT FROM v_hq;
  GET DIAGNOSTICS v_upd = ROW_COUNT;
  IF v_upd <> 1 THEN
    RAISE EXCEPTION '落位命中 % 行（期望 1），本事务回滚', v_upd;
  END IF;

  -- 收口：目标到位即收口（「别的行没被顺带改」由 apply.sh 的前后行数对平负责，
  -- 不在这里用 updated_at 判 —— 生产上别的运维动作也会碰 updated_at，会误报）
  IF NOT EXISTS (SELECT 1 FROM sys_user WHERE id = v_uid AND dept_id = v_hq) THEN
    RAISE EXCEPTION '收口失败：henry.liao 的 dept_id 未落到 HQ';
  END IF;

  RAISE NOTICE '阶段 0 完成：henry.liao → HQ（dept_id=%），快照 1 行', v_hq;
END $$;
