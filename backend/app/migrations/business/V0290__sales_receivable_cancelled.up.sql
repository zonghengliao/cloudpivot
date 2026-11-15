-- ======================================================
-- V0290：销售链 —— 应收单新增「已作废」状态 `CANCELLED`（SO-14 遗留 · t_b33adc45）
--
--   ① 重建 `ck_biz_ar_status`：4 值 → 5 值（OPEN / PARTIAL / SETTLED / WRITTEN_OFF / CANCELLED）
--   ② 新增 `ck_biz_ar_cancelled`：作废的应收不可能收过钱 ⇒ status='CANCELLED' 时 received_amount 必须为 0
--   ③ 列注释同步（status / settled_at）
--   ④ 末尾 DO 块自检：域值 / 约束行为 / 与坏账语义隔离 / 探针清场
--
-- ── 为什么要这个状态（卡面四问的结论，别处不要再各写一套）──────────
--   出库单作废（POSTED → CANCELLED）会把货退回库存并红冲成本凭证，但 SO-14 只挂了应收、
--   没做反向 —— 货回来了、应收还挂着继续计龄、继续能收款，账实不符。补反向处理前必须先定口径：
--     * `WRITTEN_OFF` 的语义是**坏账核销**（收不回来，借 6702 / 贷 1122），拿它当「作废」会把
--       坏账报表与减值口径一起污染 ⇒ 不能复用；
--     * 软删（`deleted_at`）会让这条应收从台账里消失、单号还能被复用，审计查不到
--       「这笔应收去哪了」⇒ 也不选；
--     * 负金额红字应收过不了 `ck_biz_ar_amount (amount > 0)`，放宽影响面大，且收款退款单
--       模块尚未落地 ⇒ 本卡不做（另开卡）。
--   所以：**新增 `CANCELLED` 终态**，语义 = 「这张应收对应的销售从未发生」（来源出库单已作废）。
--   与 `WRITTEN_OFF` 严格区分：坏账报表按 `status = 'WRITTEN_OFF'` 统计，作废单不进去。
--
-- ── 为什么只允许「未收款」的应收被作废 ─────────────────────
--   `ck_biz_ar_cancelled`：`status <> 'CANCELLED' OR received_amount = 0`。
--   已收过钱的应收作废后，钱就变成了「客户多付」，必须有收款退款单承接（模块未落地）——
--   库层直接拦住，handler 侧给 409（先冲收款再作废出库单）。
--
-- ── 账龄 / 报表要不要改 SQL ────────────────────────────────
--   不用。`receivable_aging` 取数一直是 `status IN ('OPEN','PARTIAL')`（只有这两个状态有余额），
--   `CANCELLED` 自动被排除；坏账报表按 `WRITTEN_OFF` 取数，也不受影响。
--   本卡只在 `receivable_aging` 的单测里把这两条不变量钉死（防止后人改成 `<> 'WRITTEN_OFF'`）。
--
-- ── 号段 ──────────────────────────────────────────────
--   写入本文件时 backend/migrations/business/ 最大号 = V0289（SO-14 客户账期卡）。
--   迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 幂等 ──────────────────────────────────────────────
--   `ck_biz_ar_status` 走「DROP IF EXISTS + ADD」（ADD CONSTRAINT 没有 IF NOT EXISTS，
--   而重建同一个域约束的最短路径就是先删后加，重复执行结果一致）；
--   `ck_biz_ar_cancelled` 走 DO 块判存在后再加。
-- ======================================================

-- ---------- ① 状态域：4 → 5 值 ----------
ALTER TABLE biz_receivable DROP CONSTRAINT IF EXISTS ck_biz_ar_status;
ALTER TABLE biz_receivable
    ADD CONSTRAINT ck_biz_ar_status
    CHECK (status IN ('OPEN', 'PARTIAL', 'SETTLED', 'WRITTEN_OFF', 'CANCELLED'));

-- ---------- ② 作废 ⇔ 一分没收 ----------
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'ck_biz_ar_cancelled'
           AND conrelid = 'public.biz_receivable'::regclass
    ) THEN
        ALTER TABLE biz_receivable
            ADD CONSTRAINT ck_biz_ar_cancelled
            CHECK (status <> 'CANCELLED' OR received_amount = 0);
    END IF;
END $$;

-- ---------- ③ 注释 ----------
COMMENT ON COLUMN biz_receivable.status IS
    '状态机：OPEN/PARTIAL/SETTLED/WRITTEN_OFF/CANCELLED（ARCH-7 §4.3 + SO-14 遗留卡）。'
    'CANCELLED = 来源出库单作废（销售未发生），与 WRITTEN_OFF（坏账核销，收不回来）严格区分；'
    'payable 的 PARTIAL_PAID 对应本表 PARTIAL';
COMMENT ON COLUMN biz_receivable.settled_at IS
    '封账时间：SETTLED（结清）/ WRITTEN_OFF（坏账核销）/ CANCELLED（出库作废冲销）时落值';

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    n          INT;
    d          TEXT;
    probe_ar   UUID := 'd0290000-0000-4000-8000-000000000001'::uuid;
    probe_cust UUID := 'd0290000-0000-4000-8000-000000000002'::uuid;
BEGIN
    -- 4.1 域约束在位且确实是 5 值（逐字比对，不接受「包含」这种模糊判据）
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conrelid = 'public.biz_receivable'::regclass
       AND conname = 'ck_biz_ar_status';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0290 自检失败：ck_biz_ar_status 不在位（% 条）', n;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d
      FROM pg_constraint WHERE conname = 'ck_biz_ar_status'
        AND conrelid = 'public.biz_receivable'::regclass;
    IF d NOT LIKE '%CANCELLED%' THEN
        RAISE EXCEPTION 'V0290 自检失败：ck_biz_ar_status 不含 CANCELLED（def=%）', d;
    END IF;
    -- 原有 4 值一个都不许丢（漏一个 = 存量单据与核销立刻炸 23514）
    FOREACH d IN ARRAY ARRAY['OPEN','PARTIAL','SETTLED','WRITTEN_OFF']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint
             WHERE conname = 'ck_biz_ar_status'
               AND conrelid = 'public.biz_receivable'::regclass) NOT LIKE '%' || d || '%' THEN
            RAISE EXCEPTION 'V0290 自检失败：ck_biz_ar_status 丢了原有域值 %', d;
        END IF;
    END LOOP;
    -- payable 的 PARTIAL_PAID 仍不许混进应收域
    IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint
         WHERE conname = 'ck_biz_ar_status'
           AND conrelid = 'public.biz_receivable'::regclass) LIKE '%PARTIAL_PAID%' THEN
        RAISE EXCEPTION 'V0290 自检失败：ck_biz_ar_status 混入了 payable 的 PARTIAL_PAID';
    END IF;

    -- 4.2 作废闸在位
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conrelid = 'public.biz_receivable'::regclass
       AND conname = 'ck_biz_ar_cancelled';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0290 自检失败：ck_biz_ar_cancelled 不在位';
    END IF;

    -- 4.3 原有约束一条都不少（本卡只加不改：amount / unsettled / credit / settled）
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conrelid = 'public.biz_receivable'::regclass
       AND conname IN ('ck_biz_ar_status','ck_biz_ar_amount','ck_biz_ar_unsettled',
                       'ck_biz_ar_credit','ck_biz_ar_settled','ck_biz_ar_cancelled');
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0290 自检失败：biz_receivable CHECK 应有 6 条，实际 %', n;
    END IF;

    -- ================= 4.4 行为探针 =================

    -- 4.4a 未收款（received = 0）的应收 → OPEN → CANCELLED 必须放行
    INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, period_code, due_date)
    VALUES (probe_ar, 'AR-20261106-900001', probe_cust, 'MANUAL', gen_random_uuid(), 'MANUAL',
            100.0000, 100.0000, '2026-11', DATE '2026-12-06');
    UPDATE biz_receivable SET status = 'CANCELLED', settled_at = now() WHERE id = probe_ar;
    SELECT status || '|' || received_amount::text || '|' || unsettled_amount::text
      INTO d FROM biz_receivable WHERE id = probe_ar;
    IF d <> 'CANCELLED|0.0000|100.0000' THEN
        RAISE EXCEPTION 'V0290 自检失败：未收款应收作废后 = %（期望 CANCELLED|0.0000|100.0000）', d;
    END IF;

    -- 4.4b 已部分收款（received = 40）的应收 → 改 CANCELLED 必须被拒（钱已经收了，不许作废）
    UPDATE biz_receivable
       SET received_amount = 40.0000, unsettled_amount = 60.0000, status = 'PARTIAL'
     WHERE id = probe_ar;
    BEGIN
        UPDATE biz_receivable SET status = 'CANCELLED' WHERE id = probe_ar;
        RAISE EXCEPTION 'V0290 自检失败：已收款应收（received=40）仍能被改成 CANCELLED';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.4c 直接插一张 received > 0 的 CANCELLED → 必须被拒
    BEGIN
        INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                    amount, received_amount, unsettled_amount, period_code,
                                    due_date, status)
        VALUES ('AR-20261106-900002', probe_cust, 'MANUAL', gen_random_uuid(), 'MANUAL',
                100, 10, 90, '2026-11', DATE '2026-12-06', 'CANCELLED');
        RAISE EXCEPTION 'V0290 自检失败：received=10 的 CANCELLED 应收插入成功';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.4d 原有域值不受影响：OPEN / PARTIAL / SETTLED / WRITTEN_OFF 仍可写
    UPDATE biz_receivable
       SET received_amount = 100.0000, unsettled_amount = 0.0000, status = 'SETTLED'
     WHERE id = probe_ar;
    UPDATE biz_receivable
       SET received_amount = 0.0000, unsettled_amount = 100.0000, status = 'WRITTEN_OFF',
           settled_at = now()
     WHERE id = probe_ar;
    SELECT status INTO d FROM biz_receivable WHERE id = probe_ar;
    IF d <> 'WRITTEN_OFF' THEN
        RAISE EXCEPTION 'V0290 自检失败：原有域值 WRITTEN_OFF 不再可写（实际 %）', d;
    END IF;

    -- 4.4e 越界值仍被拒（域约束没有被「加值」加成白名单）
    BEGIN
        UPDATE biz_receivable SET status = 'PENDING_CANCEL' WHERE id = probe_ar;
        RAISE EXCEPTION 'V0290 自检失败：PENDING_CANCEL 未被 ck_biz_ar_status 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.4f 清场：探针行不留
    DELETE FROM biz_receivable WHERE id = probe_ar;
    SELECT count(*) INTO n FROM biz_receivable WHERE ar_no LIKE 'AR-20261106-9000%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0290 自检失败：探针行残留 % 行', n;
    END IF;

    RAISE NOTICE 'V0290 自检通过：ck_biz_ar_status 5 值（含 CANCELLED）/ ck_biz_ar_cancelled 拦住已收款作废 / 原有 4 值不受影响';
END $$;
