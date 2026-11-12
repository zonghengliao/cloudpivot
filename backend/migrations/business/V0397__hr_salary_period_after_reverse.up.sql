-- ============================================================================
-- V0397__hr_salary_sheet_period_after_reverse.up.sql
--
-- 薪资单「红冲后可重算」：把期间唯一键 `uk_biz_sal_sheet_period` 的谓词
-- 从 `deleted_at IS NULL` 扩成 `deleted_at IS NULL AND status <> 'REVERSED'`
-- （卡 HR-40 / ARCH-9 §4.8.2 / §8.1）。
--
-- ── 为什么必须改（不改就是死路）──────────────────────────────────────────
--   红冲的存在意义是「这张单算错了，冲掉账、重算一张」。但 V0383 的唯一键
--   `(org_id, period_code) WHERE deleted_at IS NULL` 把**已红冲的单**也算成占位：
--     ① 红冲是终态（设计 §4.2.4：`REVERSED` 不回到 `CALCULATED`，因为「红字凭证 + 新数」
--        两套口径会说不清）；
--     ② 同期间再开新单撞唯一键 → 409；
--     ③ 界面上没有任何「删单」入口（设计 §5.2 的 46 条接口里没有 DELETE）。
--   ⇒ 客户 11 月工资算错、红冲完就**再也发不出 11 月工资**，只能让研发改库。
--     这正是 `AGENTS.md` 约定 11 的判据要禁止的形态（用户无法从界面继续操作）。
--
-- ── 与设计原文的关系（一次具名偏差，留档）────────────────────────────────
--   设计 §4.8.2 原文：「红冲后同期间**不能再开新单**（`uk_biz_salary_sheet_period`）；
--   重发要么把红冲单软删再开新单（审计上「删除」有痕），要么用**补差单**」。
--   本迁移选了**第三条更轻的路**：让 `REVERSED` 单不占期间位 —— 比「软删」保守
--   （红冲单及其红字凭证、红冲原因、状态轨迹**全部保留**，审计要比稿时它还在列表里），
--   比「补差单」直接（同期间就能重算，不用等下一个期间）。
--   补差单与软删两条路**仍然可用**，本迁移没有堵它们。
--   同款先例：成本期间 `uk_biz_cost_period` 早就排除了 `REVERSED`/`CANCELLED`
--   （`V0373`，见 `tests/cost_b1_3_alloc_and_period_ddl_migration.rs::v0373_period_unique_key_excludes_reversed_and_cancelled`）。
--
-- ── 允许多张 `REVERSED` 同期间吗（是的，且必须允许）──────────────────────
--   同一个月可能「算错 → 红冲 → 重算 → 又错 → 再红冲」。每次红冲都留一张 `REVERSED` 单
--   （各自带自己的两张红字凭证）。把 `REVERSED` 也纳入唯一键会让第二次红冲之后
--   连「重开」都做不了。故谓词里 `status <> 'REVERSED'`，不去重 `REVERSED`。
--
-- ── 幂等 ────────────────────────────────────────────────────────────────
--   `DROP INDEX IF EXISTS` + `CREATE UNIQUE INDEX IF NOT EXISTS`：重复执行等价于一次
--   （先删后建，谓词永远收敛到目标形态）。自检探针按 probe org 删净，零残留。
-- ============================================================================

-- ① 换索引：红冲过的单不再占期间位
DROP INDEX IF EXISTS uk_biz_sal_sheet_period;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sal_sheet_period
    ON biz_salary_sheet (org_id, period_code)
 WHERE deleted_at IS NULL AND status <> 'REVERSED';

-- ② 形态自检：索引谓词必须真的排除 REVERSED（删掉谓词 = 本卡功能静默失效）
DO $$
DECLARE
    v_def   text;
    v_org   uuid := '00000000-0000-4000-8000-0000000f0397';
    v_red   uuid;
    v_live  uuid;
    v_hit   boolean;
    v_n     integer;
BEGIN
    SELECT indexdef INTO v_def
      FROM pg_indexes
     WHERE indexname = 'uk_biz_sal_sheet_period';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0397 自检失败：uk_biz_sal_sheet_period 没建出来';
    END IF;
    IF position('REVERSED' in v_def) = 0 THEN
        RAISE EXCEPTION 'V0397 自检失败：唯一键谓词没排除 REVERSED（当前定义：%）', v_def;
    END IF;
    IF position('deleted_at' in v_def) = 0 THEN
        RAISE EXCEPTION 'V0397 自检失败：唯一键谓词丢了软删条件（当前定义：%）', v_def;
    END IF;

    -- 行为自检 A：一张 REVERSED 单不占位 —— 同期间再开一张必须成功
    INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end, status,
                                  reverse_reason)
    VALUES (v_org, 'SELFCHECK-0397-RED', '2099-01', DATE '2099-01-01', DATE '2099-01-31',
            'REVERSED', 'V0397 自检：红冲单不占期间位')
    RETURNING id INTO v_red;

    INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end)
    VALUES (v_org, 'SELFCHECK-0397-NEW', '2099-01', DATE '2099-01-01', DATE '2099-01-31')
    RETURNING id INTO v_live;

    -- 行为自检 B：有效单仍然只许一张 —— 第三张（未红冲）必须被拦
    v_hit := false;
    BEGIN
        INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end)
        VALUES (v_org, 'SELFCHECK-0397-DUP', '2099-01', DATE '2099-01-01', DATE '2099-01-31');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0397 自检失败：同期间写入第二张有效薪资单竟然成功（uk_biz_sal_sheet_period 放宽过头）';
    END IF;

    -- 行为自检 C：两张 REVERSED 同期间都要放行（「红冲 → 重开 → 再红冲」的真实路径）
    UPDATE biz_salary_sheet SET status = 'REVERSED', reverse_reason = 'V0397 自检：再红冲'
     WHERE id = v_live;
    INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end, status,
                                  reverse_reason)
    VALUES (v_org, 'SELFCHECK-0397-RED2', '2099-01', DATE '2099-01-01', DATE '2099-01-31',
            'REVERSED', 'V0397 自检：第二张红冲单')
    RETURNING id INTO v_live;
    IF v_live IS NULL THEN
        RAISE EXCEPTION 'V0397 自检失败：同期间第二张 REVERSED 单写不进去';
    END IF;

    -- 清理探针（按 probe org，重复执行零残留）
    DELETE FROM biz_salary_sheet WHERE org_id = v_org;
    SELECT count(*) INTO v_n FROM biz_salary_sheet WHERE org_id = v_org;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0397 自检失败：探针行未清理干净（残留 % 行）', v_n;
    END IF;

    RAISE NOTICE 'V0397 完成：uk_biz_sal_sheet_period 谓词 = deleted_at IS NULL AND status <> REVERSED；红冲单不占期间位（红冲后可重算），有效单仍唯一';
END $$;
