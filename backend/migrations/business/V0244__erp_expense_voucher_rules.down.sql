-- ======================================================
-- V0244 down：撤掉报销凭证规则 + 两处 CHECK 收回到 V0232/V0210 的形态 + 删 sys_config
--
-- CHECK 收回是**有损**的：若库里已有 `EXPENSE_REPORT` 的附件 / 提醒行，ADD CONSTRAINT
-- 会直接失败（这是想要的：宁可回滚失败，也不要静默丢掉「这条附件属于哪张单」的信息）。
-- ======================================================

-- ---------- ① 凭证规则 ----------
DELETE FROM biz_voucher_rule
 WHERE rule_code IN ('EXPENSE_REPORT_DEFAULT','EXPENSE_PAY_DEFAULT');

-- ---------- ② 附件类型 7 → 6 ----------
ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
ALTER TABLE biz_doc_attachment
    ADD CONSTRAINT ck_biz_doc_attachment_type
    CHECK (doc_type IN ('CONTRACT','PURCHASE_ORDER','STOCK_IN','PAYABLE','PAYMENT','FIXED_ASSET'));

-- ---------- ③ 提醒类型 5 → 4 ----------
ALTER TABLE biz_reminder_log DROP CONSTRAINT IF EXISTS ck_biz_reminder_log_doc_type;
ALTER TABLE biz_reminder_log
    ADD CONSTRAINT ck_biz_reminder_log_doc_type
    CHECK (doc_type IN ('PAYABLE','PURCHASE_ORDER','CONTRACT','INVENTORY'));

-- ---------- ④ sys_config：不动（`erp.expense.pay_remind_days` 由 V0226 种，本迁移只读） ----------
