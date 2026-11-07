-- ======================================================
-- V0325 down：把「未编制预算」标记与可空 over_amount 撤回旧结构
--
-- 顺序很重要：
--   ① 先把 `over_amount` 的 NULL 归零（旧结构是 `NOT NULL DEFAULT 0`，不归零加不上 NOT NULL）；
--   ② 再 `SET NOT NULL`（只对「原来就是 NOT NULL」的两张表做）；
--   ③ 删约束 → 删列。
--
-- 归一有信息损失：回滚后分不清「提交时没超支」和「超支 0 元」。这与旧代码的判定口径一致
-- （旧代码超支额恒为 Decimal，不超支写 0），所以业务上无损。
-- 幂等：`SET DEFAULT` / `DROP COLUMN IF EXISTS` / 约束判存在，重复执行不报错。
-- ======================================================

UPDATE biz_expense_report SET over_amount = 0 WHERE over_amount IS NULL;
UPDATE biz_budget_usage   SET over_amount = 0 WHERE over_amount IS NULL;

ALTER TABLE biz_expense_report ALTER COLUMN over_amount SET DEFAULT 0;
ALTER TABLE biz_budget_usage   ALTER COLUMN over_amount SET DEFAULT 0;
ALTER TABLE biz_expense_report ALTER COLUMN over_amount SET NOT NULL;
ALTER TABLE biz_budget_usage   ALTER COLUMN over_amount SET NOT NULL;

ALTER TABLE biz_expense_report DROP CONSTRAINT IF EXISTS ck_biz_exp_report_missing;

ALTER TABLE biz_expense_report DROP COLUMN IF EXISTS missing_budget;
