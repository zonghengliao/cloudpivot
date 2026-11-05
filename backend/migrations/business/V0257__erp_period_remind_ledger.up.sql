-- ======================================================
-- V0257：结账催办（期间 / 年结）写业务提醒台账 biz_reminder_log（P1-FIX-10）
--
-- ── 为什么要改 ────────────────────────────────────────
-- 复核「结账催办形同虚设」这条缺陷时实测到的两件事要分开看：
--
--   ① 催办本身**是同步的**，不依赖定时任务：`POST /business/erp/period/{id}/remind`
--      直接走 `notice::dispatch` 发站内信，生产 `sys_notice` 里 `biz_type='period_remind'`
--      已有真实行（实测 7 条）。这条不是缺陷。
--   ② 但 `biz_reminder_log` **永远是 0 行** —— V0207 的头注释写得很明白：
--      「不建表 —— 24 小时防重复用的是 sys_notice」。后果是：
--        · 运维 / 审计按台账追「这批人哪天被催过」查不到任何东西；
--        · `notice_expire_cleanup` 按 `expire_at` **硬删**过期通知（级联删接收行），
--          24h 防重判据随之消失 —— 通知一被清掉，同一张单就能被重复催；
--        · 与另外 4 类提醒（应付 / 采购 / 合同 / 低库存）的记账口径不一致：
--          同是「业务提醒」，只有结账催办不进台账。
--
--   本迁移只做「让台账能装下结账催办」这一件事：
--   把 `ck_biz_reminder_log_doc_type` 放宽 3 个值（VOUCHER / STOCK_IN / STOCK_TRANSFER ——
--   结账挡路明细的 5 类单据里，这 3 个原来不在值域内），
--   把 `ck_biz_reminder_log_kind` 放宽 2 个值（PERIOD_REMIND / YEAR_END_REMIND）。
--   写台账的代码在 `period_remind::dispatch_all`（期间与年结共用的唯一发送循环）。
--
-- ── 为什么值域要跟 DocKind 对齐而不是另立一张表 ──────────
-- `biz_reminder_log.doc_type` 的取值域由 `shared/doc_kind.rs::REMINDER_DOC_KINDS`
-- 与 `docs/architecture/snapshot-drift/10_checks.sql` 的 `REMINDER_DOC` 登记表双向守着
-- （`doc_kind_tests::doc_kind_registries_match_doc_kind_table`）。
-- 另立一张「结账催办台账」会引出第 5 类同构表与第 5 套防重判据，与本仓
-- 「一个事实一张表、一条判据一个实现」的口径相悖。
--
-- ── 幂等 ──────────────────────────────────────────────
--   · 约束重建用 `DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT`：重放无副作用；
--   · 自检只读，重复执行结论一致。
-- 回滚：`V0257__erp_period_remind_ledger.down.sql`
--       （新值域若已被使用，down 会**拒绝执行并说明原因**，不静默删数据）
-- ======================================================

-- ---------- ① 放宽 doc_type：加 3 类结账挡路单据 ----------
ALTER TABLE biz_reminder_log
    DROP CONSTRAINT IF EXISTS ck_biz_reminder_log_doc_type;

ALTER TABLE biz_reminder_log
    ADD CONSTRAINT ck_biz_reminder_log_doc_type
    CHECK (doc_type IN ('PAYABLE', 'PURCHASE_ORDER', 'CONTRACT', 'INVENTORY', 'EXPENSE_REPORT',
                        'VOUCHER', 'STOCK_IN', 'STOCK_TRANSFER'));

COMMENT ON COLUMN biz_reminder_log.doc_type IS
    '提醒对象类型：PAYABLE 应付 / PURCHASE_ORDER 采购单 / CONTRACT 合同 / INVENTORY 库存行 / '
    'EXPENSE_REPORT 报销单 / VOUCHER 凭证 / STOCK_IN 收货单 / STOCK_TRANSFER 调拨单。'
    '取值域 = shared/doc_kind.rs::REMINDER_DOC_KINDS（8 值，与 snapshot-drift 登记表双向校验）';

-- ---------- ② 放宽 remind_kind：加期间 / 年结催办 ----------
ALTER TABLE biz_reminder_log
    DROP CONSTRAINT IF EXISTS ck_biz_reminder_log_kind;

ALTER TABLE biz_reminder_log
    ADD CONSTRAINT ck_biz_reminder_log_kind
    CHECK (remind_kind IN ('PAYABLE_DUE', 'PO_OVERDUE', 'CONTRACT_EXPIRE', 'LOW_STOCK',
                           'PERIOD_REMIND', 'YEAR_END_REMIND'));

COMMENT ON COLUMN biz_reminder_log.remind_kind IS
    '提醒类型：PAYABLE_DUE 应付到期 / PO_OVERDUE 采购超期 / CONTRACT_EXPIRE 合同到期 / '
    'LOW_STOCK 低库存（以上 4 类由定时扫描写）/ PERIOD_REMIND 月结账催办 / '
    'YEAR_END_REMIND 年结账催办（以上 2 类由用户点「催办」同步写）';

COMMENT ON COLUMN biz_reminder_log.stage IS
    '档位：应付 D-7/D-3/D-1、合同 D-30/D-15/D-7/D-1、采购超期 OVERDUE-{天数}、低库存 LOW；'
    '结账催办写的是期间码（2036-03）或年度（2036）';

-- ---------- ③ 自检：两个 CHECK 都装下了新值，旧值一个没丢 ----------
DO $$
DECLARE
    def_doc  text;
    def_kind text;
    n_bad    integer;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO def_doc
      FROM pg_constraint WHERE conname = 'ck_biz_reminder_log_doc_type';
    SELECT pg_get_constraintdef(oid) INTO def_kind
      FROM pg_constraint WHERE conname = 'ck_biz_reminder_log_kind';

    IF def_doc IS NULL OR def_kind IS NULL THEN
        RAISE EXCEPTION 'V0257 自检失败：biz_reminder_log 的 CHECK 约束不存在（doc=% / kind=%）',
            def_doc, def_kind;
    END IF;

    -- 新值必须都在，旧值一个都不能少（放宽而不是替换）
    IF def_doc NOT LIKE '%''PAYABLE''%' OR def_doc NOT LIKE '%''PURCHASE_ORDER''%'
       OR def_doc NOT LIKE '%''CONTRACT''%' OR def_doc NOT LIKE '%''INVENTORY''%'
       OR def_doc NOT LIKE '%''EXPENSE_REPORT''%' OR def_doc NOT LIKE '%''VOUCHER''%'
       OR def_doc NOT LIKE '%''STOCK_IN''%' OR def_doc NOT LIKE '%''STOCK_TRANSFER''%' THEN
        RAISE EXCEPTION 'V0257 自检失败：doc_type 值域不完整（应为 8 值）：%', def_doc;
    END IF;

    IF def_kind NOT LIKE '%''PAYABLE_DUE''%' OR def_kind NOT LIKE '%''PO_OVERDUE''%'
       OR def_kind NOT LIKE '%''CONTRACT_EXPIRE''%' OR def_kind NOT LIKE '%''LOW_STOCK''%'
       OR def_kind NOT LIKE '%''PERIOD_REMIND''%' OR def_kind NOT LIKE '%''YEAR_END_REMIND''%' THEN
        RAISE EXCEPTION 'V0257 自检失败：remind_kind 值域不完整（应为 6 值）：%', def_kind;
    END IF;

    -- 现有台账行必须仍然合法（放宽不会让老行变非法，这里再确认一遍）
    SELECT count(*) INTO n_bad
      FROM biz_reminder_log
     WHERE remind_kind NOT IN ('PAYABLE_DUE', 'PO_OVERDUE', 'CONTRACT_EXPIRE', 'LOW_STOCK',
                               'PERIOD_REMIND', 'YEAR_END_REMIND')
        OR doc_type NOT IN ('PAYABLE', 'PURCHASE_ORDER', 'CONTRACT', 'INVENTORY', 'EXPENSE_REPORT',
                            'VOUCHER', 'STOCK_IN', 'STOCK_TRANSFER');
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0257 自检失败：有 % 行既有台账落在新值域外', n_bad;
    END IF;

    RAISE NOTICE 'V0257 自检通过：doc_type 8 值 / remind_kind 6 值';
END $$;
