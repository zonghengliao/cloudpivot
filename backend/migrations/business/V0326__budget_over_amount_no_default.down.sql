-- ======================================================
-- V0326 down：把 `over_amount` 的默认值 0 加回去（回到 V0242 / V0227 建表时的样子）
--
-- 只恢复 default，不动数据：NULL 行仍然保持 NULL（V0325 的 down 才负责归零）。
-- 幂等：`SET DEFAULT 0` 重复执行无副作用。
-- ======================================================

ALTER TABLE biz_expense_report ALTER COLUMN over_amount SET DEFAULT 0;
ALTER TABLE biz_budget_usage   ALTER COLUMN over_amount SET DEFAULT 0;
