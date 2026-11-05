-- ======================================================
-- V0245 down：4 个 ERP 提醒任务回到 V0210 的「停用」形态
--
-- 回滚后提醒**并不会停**：`business::erp::reminder::register_builtin` 的内置注册仍在，
-- cron 照跑，只是又变回「跑完只有 tracing 日志、库里查不到记录」的状态
-- （即 P0-FIX-1 之前的现场）。真正的开关从来不是 `sys_job.status`，而是
-- 「有没有代码把这个 job_name 注册进调度器」。
--
-- remark 逐字还原成 V0210 种子里的原文，保证 up → down → up 的往返可复核。
-- ======================================================

UPDATE sys_job AS j
   SET status = 0,
       remark = v.remark,
       updated_at = now()
  FROM (VALUES
        ('erp_payable_due_scan',
         '应付到期提醒（内置任务已注册，本行仅供查看/手动执行）：每天 07:10 扫描「7/3/1 天后到期」的未付应付单并通知采购员 + 财务'),
        ('erp_po_overdue_scan',
         '采购超期提醒（内置任务已注册，本行仅供查看/手动执行）：每天 07:20 扫描「超过期望到货日仍未收完」的采购单并通知制单人 + 采购经理'),
        ('erp_contract_expire_scan',
         '合同到期提醒（内置任务已注册，本行仅供查看/手动执行）：每天 07:30 扫描「30/15/7/1 天后到期」的生效合同并通知制单人 + 采购经理'),
        ('erp_low_stock_scan',
         '低库存提醒（内置任务已注册，本行仅供查看/手动执行）：每天 07:40 扫描「当前库存 < 安全库存」的物料×仓库并通知仓管主管')
       ) AS v(job_name, remark)
 WHERE j.job_name = v.job_name
   AND j.deleted_at IS NULL;
