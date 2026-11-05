-- ======================================================
-- V0245：4 个 ERP 提醒任务从「停用」改为「启用」（P0-FIX-1）
--
-- ── 为什么改 ──────────────────────────────────────────
-- V0210 当年把这 4 行种成 `status = 0`，理由写在迁移注释里：
--   「这 4 个任务已由代码内置注册（register_builtin），种子里再写 1 会让同一 cron
--     注册两次（内置一次 + 动态一次），属于白跑，故默认停用」。
-- 这个理由**有两点站不住**：
--
--   ① 框架注册表是 `HashMap<String, ScheduledJob>`（`vendor/nik-plugin/src/scheduler.rs`
--      的 `SchedulerPlugin::insert`），同名 `insert` 是**覆盖**不是追加。
--      `Scheduler::reload()` 的顺序是「先 register_builtin() → 再 load_db_jobs() 逐条
--      register_job()」，所以 `status = 1` 时**动态那条覆盖内置那条**，一天只跑一次。
--      覆盖语义有回归用例守着：`backend/src/task/mod.rs::tests::
--      dynamic_registration_overwrites_builtin_of_same_name`。
--   ② 内置注册的那一轮**不写 `sys_job_log`**（`task::run_builtin` 只打 tracing 日志），
--      于是「提醒到底跑没跑过」在库里查不到任何证据：`sys_job_log` 0 行、
--      `sys_job.last_run_at` 为 NULL、「定时任务」页显示停用。业务方以为系统会提醒，
--      评审只能靠「4 行全 status=0 + 日志 0 行」推断它从没响过（P0-FIX-1 的现场）。
--
-- 改成 `status = 1` 之后，这 4 个提醒由 `sys_job` 驱动（`register_job` → `run_job`）：
--   · 每轮落一条 `sys_job_log`（成功/失败 + 摘要 `scanned/sent/skipped_*`）；
--   · 回写 `last_run_at` / `next_run_at`，「定时任务」页一眼能看出最近一次跑没跑；
--   · 「立即执行」与 cron 走同一条 `run_handler` 分支（验收 / 补跑同一入口）。
-- 内置注册**保留不动**，继续当兜底：任务行被误删 / 被停用 / 迁移没跑时，提醒照跑。
--
-- ── cron 语义（顺手把话说清楚，逻辑没改）──────────────
-- 调度器用 `chrono::Utc` 取时间、按 UTC 字段匹配 cron（`SchedulerPlugin::new` 的
-- clock = `Utc::now`），所以 `0 10 7 * * *` 是 **07:10 UTC = 15:10 北京时间**。
-- V0210 的 remark 写成「每天 07:10」，与真实触发时刻差了 8 小时 —— 本迁移把 remark
-- 改成真实时刻，**cron 值一个字没动**（改 cron 会连带改掉全系统的触发时刻口径，
-- 属于框架层时区议题，另案处理；若产品确认要「早上 7:10」触发，把 4 条 cron 的小时
-- 段减 8 写成 `0 10 23 * * *` 即可，本卡不动）。
--
-- ── 幂等 ──────────────────────────────────────────────
-- 纯 UPDATE，按 `job_name` 定位未删除行：重放无副作用；行不存在（老库没种过）时
-- 自检会把它报出来而不是静默放过。
-- ======================================================

-- ---------- ① 4 行改为启用 + remark 写真实触发时刻 ----------
UPDATE sys_job AS j
   SET status = 1,
       remark = v.remark,
       updated_at = now()
  FROM (VALUES
        ('erp_payable_due_scan',
         '应付到期提醒（启用：每天 15:10 北京时间 = 07:10 UTC 扫描「7/3/1 天后到期」的未付应付单，通知采购员 + 财务；每轮落 sys_job_log）'),
        ('erp_po_overdue_scan',
         '采购超期提醒（启用：每天 15:20 北京时间 = 07:20 UTC 扫描「超过期望到货日仍未收完」的采购单，通知制单人 + 采购经理；每轮落 sys_job_log）'),
        ('erp_contract_expire_scan',
         '合同到期提醒（启用：每天 15:30 北京时间 = 07:30 UTC 扫描「30/15/7/1 天后到期」的生效合同，通知制单人 + 采购经理；每轮落 sys_job_log）'),
        ('erp_low_stock_scan',
         '低库存提醒（启用：每天 15:40 北京时间 = 07:40 UTC 扫描「当前库存 < 安全库存」的物料×仓库，通知仓管主管；每轮落 sys_job_log）')
       ) AS v(job_name, remark)
 WHERE j.job_name = v.job_name
   AND j.deleted_at IS NULL;

-- ---------- ② 自检：4 行都在、都启用、cron/调用目标仍是 V0210 的原值 ----------
DO $$
DECLARE
    n_all      integer;
    n_enabled  integer;
    n_bad_cron integer;
BEGIN
    SELECT count(*) INTO n_all
      FROM sys_job
     WHERE deleted_at IS NULL
       AND job_name IN ('erp_payable_due_scan', 'erp_po_overdue_scan',
                        'erp_contract_expire_scan', 'erp_low_stock_scan');
    IF n_all <> 4 THEN
        RAISE EXCEPTION 'V0245 自检失败：4 个 ERP 提醒任务应有 4 行未删除记录，实际 %', n_all;
    END IF;

    SELECT count(*) INTO n_enabled
      FROM sys_job
     WHERE deleted_at IS NULL
       AND status = 1
       AND job_name IN ('erp_payable_due_scan', 'erp_po_overdue_scan',
                        'erp_contract_expire_scan', 'erp_low_stock_scan');
    IF n_enabled <> 4 THEN
        RAISE EXCEPTION 'V0245 自检失败：应 4 行启用（status = 1），实际 %', n_enabled;
    END IF;

    -- 任务名 ↔ cron ↔ 调用目标 三方逐字一致（V0210 已种，本迁移只动 status/remark）
    SELECT count(*) INTO n_bad_cron
      FROM sys_job
     WHERE deleted_at IS NULL
       AND NOT (job_name, job_group, cron_expr, invoke_target) IN (
             ('erp_payable_due_scan',     'ERP', '0 10 7 * * *', 'erp_payable_due_scan'),
             ('erp_po_overdue_scan',      'ERP', '0 20 7 * * *', 'erp_po_overdue_scan'),
             ('erp_contract_expire_scan', 'ERP', '0 30 7 * * *', 'erp_contract_expire_scan'),
             ('erp_low_stock_scan',       'ERP', '0 40 7 * * *', 'erp_low_stock_scan')
           )
       AND job_name IN ('erp_payable_due_scan', 'erp_po_overdue_scan',
                        'erp_contract_expire_scan', 'erp_low_stock_scan');
    IF n_bad_cron <> 0 THEN
        RAISE EXCEPTION 'V0245 自检失败：% 行的 分组/cron/调用目标 与 V0210 种子不一致（本迁移不应改动它们）', n_bad_cron;
    END IF;
END $$;
