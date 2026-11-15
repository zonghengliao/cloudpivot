-- ======================================================
-- V0289 回滚：去掉 biz_customer.credit_days（SO-14）
--
-- ⚠ 会丢掉已录的账期天数（应收挂账的 due_date 是按它算出来的，历史应收单自己的
--    `credit_days` / `due_date` 列不受影响 —— 挂账时已经把值快照进 biz_receivable）。
-- ======================================================

ALTER TABLE biz_customer DROP CONSTRAINT IF EXISTS ck_biz_customer_credit_days;
ALTER TABLE biz_customer DROP COLUMN IF EXISTS credit_days;
