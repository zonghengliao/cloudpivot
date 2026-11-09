-- ============================================================
-- V0360 down：撤掉本迁移种的 15 条预置规则（E2E-07 · 联动触发点覆盖率）
--
-- 回滚口径：按 `(trigger_entity, trigger_status, action_code)` 精确删本迁移插入的那 15 个键。
--
-- **例外：`TASK/DOING` 不在删除清单里** —— 那是 V0254 的基线键（`任务开工 → 通知负责人与创建人`），
-- 本迁移只是把它在演练清场后被硬删的那一行种回去。回滚 V0360 不该顺手拆掉 V0254 的能力，
-- 否则「回滚本卡」会变成「静默关掉任务通知」（V0254 的 down 才是删它的地方）。
--
-- 不写 BEGIN/COMMIT（迁移器自己在事务里执行）。重复执行幂等（DELETE 命中 0 行）。
-- ============================================================

DELETE FROM biz_flow_linkage_rule
 WHERE (trigger_entity, trigger_status, action_code) IN (
       ('CONTRACT',          'SIGNED',    'SEND_NOTICE'),
       ('CONTRACT',          'TERMINATED','SEND_NOTICE'),
       ('STOCK_OUT',         'POSTED',    'SEND_NOTICE'),
       ('STOCK_TRANSFER',    'DONE',      'SEND_NOTICE'),
       ('PURCHASE_ORDER',    'RECEIVED',  'SEND_NOTICE'),
       ('PAYABLE',           'SETTLED',   'SEND_NOTICE'),
       ('VOUCHER',           'POSTED',    'SEND_NOTICE'),
       ('ACCOUNTING_PERIOD', 'CLOSED',    'SEND_NOTICE'),
       ('ACCOUNTING_YEAR',   'CLOSED',    'SEND_NOTICE'),
       ('STOCK_TAKE',        'POSTED',    'SEND_NOTICE'),
       ('DEPRECIATION',      'POSTED',    'SEND_NOTICE'),
       ('EXPENSE_REPORT',    'PAID',      'SEND_NOTICE'),
       ('FLOW_INSTANCE',     'APPROVED',  'SEND_NOTICE'),
       ('FLOW_INSTANCE',     'REJECTED',  'SEND_NOTICE')
   );

DO $$
DECLARE
    n_left integer;
BEGIN
    SELECT count(*) INTO n_left FROM biz_flow_linkage_rule
     WHERE deleted_at IS NULL
       AND (trigger_entity, trigger_status, action_code) IN (
           ('CONTRACT',          'SIGNED',    'SEND_NOTICE'),
           ('CONTRACT',          'TERMINATED','SEND_NOTICE'),
           ('STOCK_OUT',         'POSTED',    'SEND_NOTICE'),
           ('STOCK_TRANSFER',    'DONE',      'SEND_NOTICE'),
           ('PURCHASE_ORDER',    'RECEIVED',  'SEND_NOTICE'),
           ('PAYABLE',           'SETTLED',   'SEND_NOTICE'),
           ('VOUCHER',           'POSTED',    'SEND_NOTICE'),
           ('ACCOUNTING_PERIOD', 'CLOSED',    'SEND_NOTICE'),
           ('ACCOUNTING_YEAR',   'CLOSED',    'SEND_NOTICE'),
           ('STOCK_TAKE',        'POSTED',    'SEND_NOTICE'),
           ('DEPRECIATION',      'POSTED',    'SEND_NOTICE'),
           ('EXPENSE_REPORT',    'PAID',      'SEND_NOTICE'),
           ('FLOW_INSTANCE',     'APPROVED',  'SEND_NOTICE'),
           ('FLOW_INSTANCE',     'REJECTED',  'SEND_NOTICE')
       );
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0360 down 自检失败：回滚后仍有 % 条存活规则', n_left;
    END IF;
    RAISE NOTICE 'V0360 down 完成：14 个预置键已删除（TASK/DOING 属 V0254，按口径保留）';
END $$;
