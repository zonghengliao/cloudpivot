-- ======================================================
-- V0244：报销的 2 条凭证规则 + 2 处 CHECK 放宽 + 1 条 sys_config（ARCH-6 §4.5.5 / §6.5）
--
-- ── ① 凭证规则（2 条）────────────────────────────────────
--   EXPENSE_REPORT_DEFAULT  EXPENSE_REPORT   借 5602.09（兜底） / 贷 2241.01 / 税 2221.01
--   EXPENSE_PAY_DEFAULT     EXPENSE_PAYMENT  借 2241.01 / 贷 1002 / 无税
--
--   报销入账的借方是**多个不同科目**（差旅 / 招待 / 办公…按明细聚合），一条 rule
--   表达不了 ⇒ 实际过账走 `voucher_multi::post_multi_debit`（FIN-2 已建的多借方入口，
--   `create_by_rule` 一行未改）。这两条 rule 的作用是：① 记录「贷方 = 2241.01、
--   税行 = 2221.01」这对唯一口径；② 付款凭证按 rule 的借贷两科目生成；
--   ③ 巡检 `biz_type_domain.py` 要求 `biz_voucher_rule.biz_type` ∈ VoucherSourceType 域。
--
-- ── ② CHECK 放宽（只放宽，不改存量）──────────────────────
--   ck_biz_doc_attachment_type   6 → 7（+ EXPENSE_REPORT，发票影像）
--   ck_biz_reminder_log_doc_type 4 → 5（+ EXPENSE_REPORT，待付款提醒）
--   与 `doc_kind.rs::ATTACHMENT_DOC_KINDS / REMINDER_DOC_KINDS` **逐字一致**。
--   V0232 的注释明确写「`EXPENSE_REPORT` 由 FIN-3 一起加」——本迁移就是那次。
--
-- ── ③ sys_config ────────────────────────────────────────
--   erp.expense.pay_remind_days = 7（待付款提醒阈值；开关注释里说明来源）。
--   ⚠️ 待付款扫描任务（`erp_expense_pay_scan`）与孤儿对账（`erp_expense_reconcile`）
--   属 ARCH-6 §7.2 的 P1-5 卡，本卡不种 sys_job（本卡不带提醒执行体，种了就是摆设行）。
--
-- ── 幂等 ────────────────────────────────────────────────
-- 规则 `WHERE NOT EXISTS`；CHECK 先 DROP IF EXISTS 再 ADD（可重放）；
-- sys_config `ON CONFLICT (config_key) DO NOTHING`。
-- ======================================================

-- ---------- ① 凭证规则 ----------
INSERT INTO biz_voucher_rule (
    rule_code, biz_type, debit_account_id, credit_account_id, tax_account_id, is_active, remark)
SELECT v.rule_code, v.biz_type,
       (SELECT id FROM biz_account WHERE account_code = v.debit_code AND deleted_at IS NULL),
       (SELECT id FROM biz_account WHERE account_code = v.credit_code AND deleted_at IS NULL),
       CASE WHEN v.tax_code IS NULL THEN NULL
            ELSE (SELECT id FROM biz_account WHERE account_code = v.tax_code AND deleted_at IS NULL)
       END,
       1, v.remark
  FROM (VALUES
        ('EXPENSE_REPORT_DEFAULT', 'EXPENSE_REPORT',  '5602.09', '2241.01', '2221.01',
         '报销入账：借 费用科目（按明细多行）/ 借 进项税 / 贷 其他应付款-员工垫付'),
        ('EXPENSE_PAY_DEFAULT',    'EXPENSE_PAYMENT', '2241.01', '1002',    NULL,
         '报销付款：借 其他应付款-员工垫付 / 贷 银行存款')
       ) AS v(rule_code, biz_type, debit_code, credit_code, tax_code, remark)
 WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule r
                    WHERE r.rule_code = v.rule_code AND r.deleted_at IS NULL);

-- ---------- ② 附件类型 6 → 7（+ EXPENSE_REPORT） ----------
ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
ALTER TABLE biz_doc_attachment
    ADD CONSTRAINT ck_biz_doc_attachment_type
    CHECK (doc_type IN ('CONTRACT','PURCHASE_ORDER','STOCK_IN','PAYABLE','PAYMENT',
                        'FIXED_ASSET','EXPENSE_REPORT'));

-- ---------- ③ 提醒类型 4 → 5（+ EXPENSE_REPORT） ----------
ALTER TABLE biz_reminder_log DROP CONSTRAINT IF EXISTS ck_biz_reminder_log_doc_type;
ALTER TABLE biz_reminder_log
    ADD CONSTRAINT ck_biz_reminder_log_doc_type
    CHECK (doc_type IN ('PAYABLE','PURCHASE_ORDER','CONTRACT','INVENTORY','EXPENSE_REPORT'));

-- ---------- ④ sys_config：待付款提醒天数（**V0226 已种，本迁移不重复种**） ----------
-- V0226 §④ 已经写入 `erp.expense.pay_remind_days`（默认 '3'，备注写明「FIN-3 使用」）。
-- 本卡只做**存在性自检**（见 ⑤.5）：不插，也不改它的值 —— 阈值是运维口径，
-- 换值走「系统配置」页面，不靠迁移改数据。


-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    n int;
    v_missing text;
BEGIN
    -- 5.1 两条规则齐全
    SELECT count(*) INTO n FROM biz_voucher_rule
     WHERE rule_code IN ('EXPENSE_REPORT_DEFAULT','EXPENSE_PAY_DEFAULT')
       AND is_active = 1 AND deleted_at IS NULL;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0244 自检失败：报销凭证规则只种了 % 条（期望 2）', n;
    END IF;

    -- 5.2 两条规则的科目都必须解析到（科目缺失 → 规则是空壳）
    SELECT string_agg(v.rule_code, ',') INTO v_missing
      FROM (VALUES ('EXPENSE_REPORT_DEFAULT'),('EXPENSE_PAY_DEFAULT')) AS v(rule_code)
     WHERE EXISTS (SELECT 1 FROM biz_voucher_rule r
                    WHERE r.rule_code = v.rule_code
                      AND (r.debit_account_id IS NULL OR r.credit_account_id IS NULL));
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0244 自检失败：规则 % 的科目没解析到（5602.09 / 2241.01 / 1002 是否齐全）', v_missing;
    END IF;

    -- 5.3 报销入账规则必须有税行（2221.01），付款规则必须没有
    IF NOT EXISTS (SELECT 1 FROM biz_voucher_rule
                    WHERE rule_code = 'EXPENSE_REPORT_DEFAULT' AND tax_account_id IS NOT NULL) THEN
        RAISE EXCEPTION 'V0244 自检失败：EXPENSE_REPORT_DEFAULT 缺税行科目 2221.01';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_voucher_rule
                WHERE rule_code = 'EXPENSE_PAY_DEFAULT' AND tax_account_id IS NOT NULL) THEN
        RAISE EXCEPTION 'V0244 自检失败：EXPENSE_PAY_DEFAULT 不该有税行（付款不产生税额）';
    END IF;

    -- 5.4 两处 CHECK 必须真的放宽了（直接读约束定义，不试插 —— 试插会撞别的约束）
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_doc_attachment_type'
           AND pg_get_constraintdef(oid) LIKE '%EXPENSE_REPORT%') THEN
        RAISE EXCEPTION 'V0244 自检失败：ck_biz_doc_attachment_type 未放宽到 EXPENSE_REPORT';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_reminder_log_doc_type'
           AND pg_get_constraintdef(oid) LIKE '%EXPENSE_REPORT%') THEN
        RAISE EXCEPTION 'V0244 自检失败：ck_biz_reminder_log_doc_type 未放宽到 EXPENSE_REPORT';
    END IF;

    -- 5.5 sys_config 已种
    IF NOT EXISTS (SELECT 1 FROM sys_config WHERE config_key = 'erp.expense.pay_remind_days') THEN
        RAISE EXCEPTION 'V0244 自检失败：erp.expense.pay_remind_days 未种入';
    END IF;
END $$;
