-- ======================================================
-- V0257 回滚：把 biz_reminder_log 的两个值域收回 P1-FIX-10 之前的形状
--   doc_type   : 8 值 → 5 值（去掉 VOUCHER / STOCK_IN / STOCK_TRANSFER）
--   remind_kind: 6 值 → 4 值（去掉 PERIOD_REMIND / YEAR_END_REMIND）
--
-- ⚠️ 安全闸：如果台账里已经有结账催办的行（或用了新 doc_type 的行），add constraint
--    会因既存行违反 CHECK 而直接报错 —— 这里显式先检查并给出人话原因，
--    **不静默 DELETE 数据**（台账是 append-only 的审计事实，删它比留着更危险）。
--    确需回滚时先自行决定这些行的去留（导出后手工删除），再重跑本脚本。
-- ======================================================

DO $$
DECLARE
    n_new_kind integer;
    n_new_doc  integer;
BEGIN
    SELECT count(*) INTO n_new_kind
      FROM biz_reminder_log
     WHERE remind_kind IN ('PERIOD_REMIND', 'YEAR_END_REMIND');
    SELECT count(*) INTO n_new_doc
      FROM biz_reminder_log
     WHERE doc_type IN ('VOUCHER', 'STOCK_IN', 'STOCK_TRANSFER');

    IF n_new_kind <> 0 OR n_new_doc <> 0 THEN
        RAISE EXCEPTION
            'V0257 拒绝回滚：台账已有 % 行结账催办（remind_kind 新值）、% 行新 doc_type，'
            '收回值域会让它们变成非法行。请先导出并手工处理这些行（本脚本不删数据）',
            n_new_kind, n_new_doc;
    END IF;

    RAISE NOTICE 'V0257 回滚：新值域下 0 行数据，可以安全收回';
END $$;

ALTER TABLE biz_reminder_log
    DROP CONSTRAINT IF EXISTS ck_biz_reminder_log_doc_type;

ALTER TABLE biz_reminder_log
    ADD CONSTRAINT ck_biz_reminder_log_doc_type
    CHECK (doc_type IN ('PAYABLE', 'PURCHASE_ORDER', 'CONTRACT', 'INVENTORY', 'EXPENSE_REPORT'));

ALTER TABLE biz_reminder_log
    DROP CONSTRAINT IF EXISTS ck_biz_reminder_log_kind;

ALTER TABLE biz_reminder_log
    ADD CONSTRAINT ck_biz_reminder_log_kind
    CHECK (remind_kind IN ('PAYABLE_DUE', 'PO_OVERDUE', 'CONTRACT_EXPIRE', 'LOW_STOCK'));

COMMENT ON COLUMN biz_reminder_log.doc_type IS
    '单据类型：PAYABLE 应付 / PURCHASE_ORDER 采购单 / CONTRACT 合同 / INVENTORY 库存行（物料×仓库）/ EXPENSE_REPORT 报销单';
COMMENT ON COLUMN biz_reminder_log.remind_kind IS
    '提醒类型：PAYABLE_DUE 应付到期 / PO_OVERDUE 采购超期 / CONTRACT_EXPIRE 合同到期 / LOW_STOCK 低库存';
COMMENT ON COLUMN biz_reminder_log.stage IS
    '档位：应付 D-7/D-3/D-1、合同 D-30/D-15/D-7/D-1、采购超期 OVERDUE-{天数}、低库存 LOW';
