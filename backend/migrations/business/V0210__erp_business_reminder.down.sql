-- ============================================================
-- V0210（回滚）：业务提醒任务包
--
-- 删除顺序按依赖：台账表 → 模板 → 定时任务行 → 系统参数 → 权限节点（先授权后节点）。
--
-- 口径：
--   · 已发出的**通知**（`sys_notice` / `sys_notice_receiver`）一行不动 ——
--     它们是沟通与审计事实，不由本迁移的代码路径产生，也不该被回滚删掉；
--   · `sys_job_log` 里这 4 个任务的历史执行日志同样保留（表结构本来就不建外键）；
--   · 回滚后提醒不再产生：代码里的内置任务仍在 `register_builtin` 注册，
--     但 `biz_reminder_log` 不存在时本轮扫描直接失败并记 ERROR（不 panic、不影响服务），
--     —— 这是**回滚迁移必须同时回滚代码**的信号，写在这里防止忘。
-- ============================================================

DROP TABLE IF EXISTS biz_reminder_log;

DELETE FROM sys_notice_template
 WHERE template_code IN ('ERP_REMINDER_PAYABLE_DUE', 'ERP_REMINDER_PO_OVERDUE',
                         'ERP_REMINDER_CONTRACT_EXPIRE', 'ERP_REMINDER_LOW_STOCK');

DELETE FROM sys_job
 WHERE job_name IN ('erp_payable_due_scan', 'erp_po_overdue_scan',
                    'erp_contract_expire_scan', 'erp_low_stock_scan');

DELETE FROM sys_config
 WHERE config_key IN ('erp.reminder.po_overdue.realert_days',
                      'erp.reminder.low_stock.realert_days');

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001214'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001214'::uuid;

DO $$
DECLARE
    n_tbl    integer;
    n_tpl    integer;
    n_job    integer;
    n_cfg    integer;
    n_menu   integer;
    n_grant  integer;
BEGIN
    SELECT count(*) INTO n_tbl FROM information_schema.tables
     WHERE table_name = 'biz_reminder_log';
    SELECT count(*) INTO n_tpl FROM sys_notice_template
     WHERE template_code LIKE 'ERP_REMINDER_%';
    SELECT count(*) INTO n_job FROM sys_job
     WHERE job_name IN ('erp_payable_due_scan', 'erp_po_overdue_scan',
                        'erp_contract_expire_scan', 'erp_low_stock_scan');
    SELECT count(*) INTO n_cfg FROM sys_config
     WHERE config_key LIKE 'erp.reminder.%';
    SELECT count(*) INTO n_menu FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001214'::uuid;
    SELECT count(*) INTO n_grant FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000001214'::uuid;

    IF n_tbl <> 0 OR n_tpl <> 0 OR n_job <> 0 OR n_cfg <> 0 OR n_menu <> 0 OR n_grant <> 0 THEN
        RAISE EXCEPTION 'V0210 回滚自检失败：残留 表=% 模板=% 任务=% 参数=% 节点=% 授权=%',
            n_tbl, n_tpl, n_job, n_cfg, n_menu, n_grant;
    END IF;

    RAISE NOTICE 'V0210 回滚自检通过：台账 / 模板 / 任务 / 参数 / 权限节点 零残留';
END $$;
