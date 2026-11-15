-- ======================================================
-- V0325：FIX-5 ——「未编制预算」与「超预算」分开表达
--
-- 问题（P2）：`BudgetVerdict::evaluate` 里 `missing_budget=true` 与 `over=true` 同时成立，
-- 于是「该槽压根没编预算」的单据落成 `over_budget=1 / over_amount=全额`，报销单列表页
-- 一片「超预算」，业务无法区分「真超支」和「没编预算」。
--
-- 本迁移只做两件事（都是纯结构变更，不改历史判定的语义）：
--   ① `biz_expense_report.missing_budget`（SMALLINT 0/1）—— 新增，冻结「提交时该单有没有
--      受控槽未编制预算」这个事实（与 `over_budget` 互斥：真超支才 over）。
--   ② `over_amount` 允许 `NULL` —— 「超支额」是**超支才有值**的量：没超支（含未编预算）
--      写 `NULL` 而不是 `0`，前端与报表才分得清「0 元超支」和「没有超支」。
--      两张表同改：`biz_expense_report`（单据）+ `biz_budget_usage`（占用快照）。
--
-- ── 幂等 ───────────────────────────────────────────────
-- `ADD COLUMN IF NOT EXISTS`；`DROP NOT NULL` 天然幂等；约束用 `pg_constraint` 判存在。
-- 重复执行只会刷一次 `COMMENT`（同值），不产生任何数据变更。
--
-- ── 回滚 ───────────────────────────────────────────────
-- down 把 `over_amount` 的 NULL 归零后再 `SET NOT NULL`（否则加不上约束），再删列/约束。
-- 归一有信息损失（分不清「当时没超支」与「超支 0 元」），但这是回滚到旧结构必须付的代价，
-- 与旧代码的判定口径（0 = 没超支）一致，业务上无损。
-- ======================================================

-- ---------- ① 报销单：新增 missing_budget ----------
ALTER TABLE biz_expense_report
    ADD COLUMN IF NOT EXISTS missing_budget SMALLINT NOT NULL DEFAULT 0;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_exp_report_missing'
           AND conrelid = 'biz_expense_report'::regclass
    ) THEN
        ALTER TABLE biz_expense_report
            ADD CONSTRAINT ck_biz_exp_report_missing CHECK (missing_budget IN (0, 1));
    END IF;
END
$$;

COMMENT ON COLUMN biz_expense_report.missing_budget IS
    '提交时是否有受控槽「未编制预算」（0/1）；与 over_budget 互斥：真超支才置 over_budget=1';

-- ---------- ② over_amount 改为可空（超支才有值） ----------
ALTER TABLE biz_expense_report ALTER COLUMN over_amount DROP NOT NULL;
ALTER TABLE biz_budget_usage   ALTER COLUMN over_amount DROP NOT NULL;

COMMENT ON COLUMN biz_expense_report.over_amount IS
    '提交时点超支额；NULL = 当时没超支（含未编预算），不是 0 元超支';
COMMENT ON COLUMN biz_budget_usage.over_amount IS
    '占用写入时点的超支额快照；NULL = 当时没超支（含未编预算）';

-- ---------- 自检：列在、约束在、两张表的 over_amount 可空 ----------
DO $$
DECLARE
    missing_column BOOLEAN;
    missing_check  BOOLEAN;
    report_notnull BOOLEAN;
    usage_notnull  BOOLEAN;
BEGIN
    SELECT NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_expense_report' AND column_name = 'missing_budget'
    ) INTO missing_column;
    IF missing_column THEN
        RAISE EXCEPTION 'V0325 自检失败：biz_expense_report.missing_budget 未创建';
    END IF;

    SELECT NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_exp_report_missing'
           AND conrelid = 'biz_expense_report'::regclass
    ) INTO missing_check;
    IF missing_check THEN
        RAISE EXCEPTION 'V0325 自检失败：ck_biz_exp_report_missing 未创建';
    END IF;

    SELECT is_nullable = 'NO' INTO report_notnull
      FROM information_schema.columns
     WHERE table_name = 'biz_expense_report' AND column_name = 'over_amount';
    SELECT is_nullable = 'NO' INTO usage_notnull
      FROM information_schema.columns
     WHERE table_name = 'biz_budget_usage' AND column_name = 'over_amount';
    IF report_notnull OR usage_notnull THEN
        RAISE EXCEPTION 'V0325 自检失败：over_amount 仍为 NOT NULL（report=% usage=%）',
            report_notnull, usage_notnull;
    END IF;
END
$$;
