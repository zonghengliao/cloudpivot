-- ============================================================================
-- V0392__attendance_lock_guard.up.sql
--
-- 考勤「**锁定后写保护**」的库层兜底（卡 HR-23 / t_8aa9f8ed）。
--
-- 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md`
--   §4.2.3「`LOCKED` 之后**记录不许增删改**（handler 闸 + 巡检 A8）」
--   §4.2.3「锁定不校验审批（HR 自己锁），但要 `attendance:lock` 权限码 + 写 `biz_status_log`」
--
-- ── 为什么除了 handler 闸还要一道库层守卫 ──────────────────────────────────
--   写 `biz_attendance_record` 的路径**不止一条**：
--     · HR-20 台账单行修正（`attendance_record_model::UPDATE_SQL` 已带 `NOT EXISTS(… 'LOCKED')`）
--     · HR-21/22 Excel 导入 upsert（按 `period_code` 批量落盘）
--     · HR-27 请假 bridge（已批请假回写 `leave_paid_days / leave_unpaid_days`）
--     · 以后任何「修数据 / 补录 / 对账」脚本
--   每条路径各自记得判期间状态是不可能的（写代码的人换了、顺序改了、事务开漏了）。
--   把判据下沉到表上：**只要期间是 `LOCKED`，这一行就写不动**，与谁在写无关。
--   这与 HR-20 在 `UPDATE_SQL` 里做的「语句级守卫」是同一取向，本迁移把它补齐到
--   **INSERT / UPDATE（含软删 = UPDATE）** 两个动作上。
--
-- ── 拦什么、不拦什么（刻意留下的两个口子都有理由）────────────────────────
--   ① 拦：`INSERT`（导入补人、手工新增行）
--   ② 拦：`UPDATE`（改天数/小时、**软删 `deleted_at`**、改 `period_id` 挪行）
--   ③ **不拦 `DELETE`**（硬删）。理由：本表的业务删除是软删（`deleted_at`，被 ② 拦住），
--      硬 `DELETE` 只出现在演练清场、`V0383.down.sql` 收表、以及 `ON DELETE CASCADE`
--      （删期间连删记录）。拦了硬删会让**清场与回滚无法收回数据**（期间一旦锁过就删不掉，
--      演练库得手工 drop database），换来的是挡不住任何真实业务路径的「保护」——
--      业务代码里根本没有硬删。
--   ④ **不拦期间不存在** 的插入：那是外键（`period_id → biz_attendance_period`）的事，
--      库层已经能挡，重复拦只会把 FK 报错盖成一句含糊的「已锁定」。
--
-- ── 逃生门：`cloudpivot.attendance_lock_guard = 'off'` ─────────────────────
--   演练库重建、客户数据修复、`V0383.down.sql` 之类**必须**改历史数据的场合：
--     `SET LOCAL cloudpivot.attendance_lock_guard = 'off';`（事务级，跟着事务一起结束）
--   为什么必须是 `SET LOCAL`：`SET`（会话级）在连接池里会**留到下一个请求**，
--   等于把守卫永久关掉且没人知道。事务级设置回滚即恢复。
--   默认（未设置）= 守卫开启，所以「忘了设」的后果是多拦一次，不是漏拦。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────────
--   `CREATE OR REPLACE FUNCTION` + `DROP TRIGGER IF EXISTS` 再 `CREATE TRIGGER`：
--   重复执行结果不变（函数体被覆盖，触发器只有一个）。
--   `DROP FUNCTION` 在 down 里带 `IF EXISTS`。
--
-- ── 号段 ──────────────────────────────────────────────────────────────────
--   本仓 business 目录当前最大 = `V0391`（flow_cost_settle_is_system），故取 V0392；
--   重号会让服务启动即失败（AGENTS.md 约定 6），改号段前先 `ls backend/migrations/*/`。
--
--   ⚠ 与设计的偏差（给 HR-38 的提醒）：设计 §7.1 把 `V0391/V0392` 预留给
--   `hr_salary` / 调薪·离职**流程**（设计原号 V0385/V0386，后来整体顺延）。
--   本卡按「实际最大号 +1」取号（迁移号段规则优先于设计里的预测值），
--   所以 **HR-38 落地时请取 V0393 起**，不要再写 V0392 —— 否则启动即撞号。
-- ============================================================================

-- ① 守卫函数：BEFORE INSERT OR UPDATE 判「所属期间是否 LOCKED」。
--
--    `SECURITY INVOKER`（默认）：不提升权限，跟着调用者的角色走。
--    `search_path` 不动：本仓迁移与业务 SQL 都走 public，显式加 search_path 反而会让
--    `biz_attendance_period` 在其它 schema 下失效。
CREATE OR REPLACE FUNCTION fn_biz_att_record_lock_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    -- 逃生门：事务级关闭（见文件头 ④）。未设置 = 'on'。
    v_bypass    text;
    v_old_status text;
    v_old_code   text;
    v_new_status text;
    v_new_code   text;
BEGIN
    v_bypass := coalesce(current_setting('cloudpivot.attendance_lock_guard', true), 'on');
    IF v_bypass = 'off' THEN
        RETURN NEW;
    END IF;

    -- ② 目标期间（NEW 侧）：INSERT 与 UPDATE 都要看
    SELECT p.status, p.period_code INTO v_new_status, v_new_code
      FROM biz_attendance_period p
     WHERE p.id = NEW.period_id;

    IF v_new_status = 'LOCKED' THEN
        RAISE EXCEPTION 'cloudpivot.attendance_lock_guard: 考勤期间 % 已锁定（LOCKED），记录不许新增或修改',
            coalesce(v_new_code, NEW.period_id::text)
            USING ERRCODE = 'CP001',
                  HINT = '先由有 business:hr:attendance:lock 权限的人解锁（解锁必须给原因，会写 biz_status_log），再改考勤';
    END IF;

    -- ③ 源期间（OLD 侧）：允许改 period_id 挪行时，**挪出**锁定期间同样要拦，
    --    否则「先挪到草稿期、再在草稿期随便改」就是一条绕过锁定的路。
    IF TG_OP = 'UPDATE' AND OLD.period_id IS DISTINCT FROM NEW.period_id THEN
        SELECT p.status, p.period_code INTO v_old_status, v_old_code
          FROM biz_attendance_period p
         WHERE p.id = OLD.period_id;

        IF v_old_status = 'LOCKED' THEN
            RAISE EXCEPTION 'cloudpivot.attendance_lock_guard: 考勤期间 % 已锁定（LOCKED），记录不许迁出',
                coalesce(v_old_code, OLD.period_id::text)
                USING ERRCODE = 'CP001',
                      HINT = '锁定期间的记录既不能改也不能挪到别的期间；要重算请先解锁';
        END IF;
    END IF;

    RETURN NEW;
END;
$$;

COMMENT ON FUNCTION fn_biz_att_record_lock_guard() IS
    '考勤记录锁定守卫：所属期间 status=LOCKED 时拒绝 INSERT/UPDATE（含软删）。
     逃生门 = SET LOCAL cloudpivot.attendance_lock_guard=''off''；
     错误码 CP001 由 attendance_lock::map_write_error 翻成 409 + 68010。';

-- ④ 触发器挂在表上（BEFORE INSERT OR UPDATE：先拦，不落盘、不产生 WAL 里的脏行）
DROP TRIGGER IF EXISTS trg_biz_att_record_lock_guard ON biz_attendance_record;
CREATE TRIGGER trg_biz_att_record_lock_guard
    BEFORE INSERT OR UPDATE ON biz_attendance_record
    FOR EACH ROW
    EXECUTE FUNCTION fn_biz_att_record_lock_guard();

COMMENT ON TRIGGER trg_biz_att_record_lock_guard ON biz_attendance_record IS
    '考勤期间 LOCKED 后记录不许新增/修改/软删（设计 §4.2.3；卡 HR-23）。硬 DELETE 不拦，见 V0392 文件头 ③';

-- ⑤ 自检：守卫真挂上了，且函数体里真有 LOCKED 判据（防止有人用空函数顶掉）
DO $$
DECLARE
    v_trigger  integer;
    v_src      text;
BEGIN
    SELECT count(*) INTO v_trigger
      FROM pg_trigger t
      JOIN pg_class c ON c.oid = t.tgrelid
     WHERE c.relname = 'biz_attendance_record'
       AND t.tgname  = 'trg_biz_att_record_lock_guard'
       AND NOT t.tgisinternal;
    IF v_trigger <> 1 THEN
        RAISE EXCEPTION 'V0392 自检失败：biz_attendance_record 上的锁定守卫触发器应有且仅有 1 个，实际 %', v_trigger;
    END IF;

    SELECT pg_get_functiondef('fn_biz_att_record_lock_guard()'::regprocedure) INTO v_src;
    IF position('''LOCKED''' in v_src) = 0 THEN
        RAISE EXCEPTION 'V0392 自检失败：守卫函数体里没有 LOCKED 判据（被空函数顶掉了？）';
    END IF;
    IF position('CP001' in v_src) = 0 THEN
        RAISE EXCEPTION 'V0392 自检失败：守卫函数没有抛 CP001（attendance_lock::is_lock_violation 会认不出来）';
    END IF;
END;
$$;
