-- ======================================================
-- V0292（SO-16 收款单）约束探针 —— **外部独立校验**，与迁移末尾 DO 块互为第二遍
--
--   用法：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d <库> -v ON_ERROR_STOP=1 -f 本文件
--   输出：每个探针一行 `NOTICE: Pxx PASS …`；任一不符 → RAISE EXCEPTION 直接炸（非零退出）
--   清场：最后 4.10n / P27 把探针行全删，可重复执行（round1 / round2 同库连跑即自证幂等）
--
--   与迁移内自检的差别：这里**不复用**迁移里的判定逻辑，全部从 SQL 外部视角断言
--   （结构查 information_schema / pg_constraint / pg_indexes，行为靠「插坏数据必须被拒」）。
-- ======================================================
DO $$
DECLARE
    n        INT;
    d        TEXT;
    sample   TEXT;
    ar       UUID := 'd0292eee-0000-4000-8000-000000000001'::uuid;
    rc       UUID := 'd0292eee-0000-4000-8000-000000000002'::uuid;
    rc2      UUID := 'd0292eee-0000-4000-8000-000000000003'::uuid;
    cust     UUID := gen_random_uuid();
BEGIN
    -- ============ A. 结构 ============
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_receipt';
    IF n <> 16 THEN RAISE EXCEPTION 'P01 FAIL: biz_receipt 列数 %', n; END IF;
    RAISE NOTICE 'P01 PASS 结构 biz_receipt = 16 列';

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_receipt_allocation';
    IF n <> 7 THEN RAISE EXCEPTION 'P02 FAIL: biz_receipt_allocation 列数 %', n; END IF;
    RAISE NOTICE 'P02 PASS 结构 biz_receipt_allocation = 7 列';

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid IN ('public.biz_receipt'::regclass,'public.biz_receipt_allocation'::regclass)
       AND contype='c';
    IF n <> 5 THEN RAISE EXCEPTION 'P03 FAIL: CHECK 条数 %', n; END IF;
    RAISE NOTICE 'P03 PASS 结构 5 条 CHECK 在位';

    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname='public' AND tablename IN ('biz_receipt','biz_receipt_allocation');
    IF n <> 9 THEN RAISE EXCEPTION 'P04 FAIL: 索引条数 %（期望 9）', n; END IF;
    RAISE NOTICE 'P04 PASS 结构 索引 = 9（6 + 3）';

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public'
       AND ((table_name='biz_receipt' AND column_name IN ('amount','allocated_amount'))
         OR (table_name='biz_receipt_allocation' AND column_name='amount'))
       AND numeric_precision=18 AND numeric_scale=4;
    IF n <> 3 THEN RAISE EXCEPTION 'P05 FAIL: NUMERIC(18,4) 只 % /3', n; END IF;
    RAISE NOTICE 'P05 PASS 结构 三列金额 NUMERIC(18,4)';

    SELECT confdeltype INTO d FROM pg_constraint WHERE conname='biz_receipt_allocation_receipt_id_fkey';
    IF d IS DISTINCT FROM 'c' THEN RAISE EXCEPTION 'P06 FAIL: receipt_id 删除动作 %', d; END IF;
    SELECT confdeltype INTO d FROM pg_constraint WHERE conname='biz_receipt_allocation_receivable_id_fkey';
    IF d IS DISTINCT FROM 'r' THEN RAISE EXCEPTION 'P07 FAIL: receivable_id 删除动作 %', d; END IF;
    RAISE NOTICE 'P06 PASS 外键 receipt_id=CASCADE / receivable_id=RESTRICT';

    -- ============ B. 头表金额三连（ck_biz_receipt_amount）============
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
        VALUES ('RC-20261106-800001', cust, DATE '2026-11-06', 0);
        RAISE EXCEPTION 'P08 FAIL: amount=0 被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P08 PASS 拒 ck_biz_receipt_amount (amount=0)';
    END;

    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
        VALUES ('RC-20261106-800002', cust, DATE '2026-11-06', -5);
        RAISE EXCEPTION 'P09 FAIL: amount=-5 被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P09 PASS 拒 ck_biz_receipt_amount (amount=-5)';
    END;

    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, allocated_amount)
        VALUES ('RC-20261106-800003', cust, DATE '2026-11-06', 100, 100.0001);
        RAISE EXCEPTION 'P10 FAIL: allocated>amount 被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P10 PASS 拒 ck_biz_receipt_amount (allocated 100.0001 > amount 100)';
    END;

    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, allocated_amount)
        VALUES ('RC-20261106-800004', cust, DATE '2026-11-06', 100, -0.0001);
        RAISE EXCEPTION 'P11 FAIL: allocated=-0.0001 被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P11 PASS 拒 ck_biz_receipt_amount (allocated=-0.0001)';
    END;

    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
        VALUES ('RC-20261106-800005', cust, DATE '2026-11-06', NULL);
        RAISE EXCEPTION 'P12 FAIL: amount NULL 被放行（amount 必须 NOT NULL 且无默认值）';
    EXCEPTION WHEN not_null_violation THEN RAISE NOTICE 'P12 PASS 拒 NOT NULL (amount 缺省省略)';
    END;

    -- ============ C. 域值 ============
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, status)
        VALUES ('RC-20261106-800006', cust, DATE '2026-11-06', 100, 'OPEN');
        RAISE EXCEPTION 'P13 FAIL: status=OPEN（应收域值）被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P13 PASS 拒 ck_biz_receipt_status (status=OPEN ← 应收域值)';
    END;

    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, status)
        VALUES ('RC-20261106-800007', cust, DATE '2026-11-06', 100, 'PARTIAL_PAID');
        RAISE EXCEPTION 'P14 FAIL: status=PARTIAL_PAID（应付域值）被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P14 PASS 拒 ck_biz_receipt_status (PARTIAL_PAID ← 应付域值)';
    END;

    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, method)
        VALUES ('RC-20261106-800008', cust, DATE '2026-11-06', 100, 'ALIPAY');
        RAISE EXCEPTION 'P15 FAIL: method=ALIPAY 被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P15 PASS 拒 ck_biz_receipt_method (ALIPAY)';
    END;

    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, allocated_amount, status)
        VALUES ('RC-20261106-800009', cust, DATE '2026-11-06', 100, 60, 'CANCELLED');
        RAISE EXCEPTION 'P16 FAIL: CANCELLED 且已核销 60 被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P16 PASS 拒 ck_biz_receipt_cancelled (CANCELLED + allocated 60)';
    END;

    -- ============ D. 正常路径 / 默认值 / 唯一 / 精度 ============
    INSERT INTO biz_receipt (id, receipt_no, customer_id, receipt_date, amount, method, bank_account)
    VALUES (rc, 'RC-20261106-800101', cust, DATE '2026-11-06', 1000.0000, 'BANK_TRANSFER', '6222-0000-1111-2222');
    SELECT status||'|'||allocated_amount::text||'|'||amount::text INTO d FROM biz_receipt WHERE id=rc;
    IF d <> 'DRAFT|0.0000|1000.0000' THEN RAISE EXCEPTION 'P17 FAIL: 默认值落库 %', d; END IF;
    RAISE NOTICE 'P17 PASS 正常收款单 默认 DRAFT / allocated 0 / amount 1000.0000';

    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
        VALUES ('RC-20261106-800101', cust, DATE '2026-11-06', 10);
        RAISE EXCEPTION 'P18 FAIL: 存活行重号被放行';
    EXCEPTION WHEN unique_violation THEN RAISE NOTICE 'P18 PASS 拒 uk_biz_receipt_no (存活行重号)';
    END;

    INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
    VALUES ('RC-20261106-800102', cust, DATE '2026-11-06', 10);
    UPDATE biz_receipt SET deleted_at = now() WHERE receipt_no='RC-20261106-800102';
    INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
    VALUES ('RC-20261106-800102', cust, DATE '2026-11-06', 20);
    SELECT count(*) INTO n FROM biz_receipt WHERE receipt_no='RC-20261106-800102' AND deleted_at IS NULL;
    IF n <> 1 THEN RAISE EXCEPTION 'P19 FAIL: 软删后同号复插存活行 %', n; END IF;
    RAISE NOTICE 'P19 PASS 软删后同号可复用（存活行 1）';

    INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, status, method)
    VALUES ('RC-20261106-800103', cust, DATE '2026-11-06', 0.0002, 'CANCELLED', NULL);
    SELECT amount::text||'|'||status||'|'||coalesce(method,'NULL') INTO d
      FROM biz_receipt WHERE receipt_no='RC-20261106-800103';
    IF d <> '0.0002|CANCELLED|NULL' THEN RAISE EXCEPTION 'P20 FAIL: 精度/域值 %', d; END IF;
    RAISE NOTICE 'P20 PASS 精度 0.0002 不截断 / CANCELLED+allocated 0 合法 / method NULL 合法';

    BEGIN
        UPDATE biz_receipt SET allocated_amount = 1000.0001 WHERE id=rc;
        RAISE EXCEPTION 'P21 FAIL: UPDATE 超核销被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P21 PASS 拒 UPDATE 超核销 (1000.0001 > 1000)';
    END;
    UPDATE biz_receipt SET allocated_amount = 600.0000, status='CONFIRMED' WHERE id=rc;
    RAISE NOTICE 'P21b PASS 正常核销 600 / status CONFIRMED 落库成功';

    -- ============ E. 核销明细 ============
    INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no,
                                amount, received_amount, unsettled_amount, credit_days,
                                period_code, due_date)
    VALUES (ar, 'AR-20261106-800101', cust, 'MANUAL', gen_random_uuid(), 'MANUAL',
            1000.0000, 0, 1000.0000, 30, '2026-11', DATE '2026-12-06');
    INSERT INTO biz_receipt (id, receipt_no, customer_id, receipt_date, amount)
    VALUES (rc2, 'RC-20261106-800104', cust, DATE '2026-11-06', 100);

    BEGIN
        INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount) VALUES (rc2, ar, 0);
        RAISE EXCEPTION 'P22 FAIL: 核销 amount=0 被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P22 PASS 拒 ck_biz_rcpt_alloc_amount (0)';
    END;

    BEGIN
        INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount) VALUES (rc2, ar, -1);
        RAISE EXCEPTION 'P23 FAIL: 核销 amount=-1 被放行';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE 'P23 PASS 拒 ck_biz_rcpt_alloc_amount (-1)';
    END;

    INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount, remark)
    VALUES (rc2, ar, 300.0000, '探针核销');
    RAISE NOTICE 'P24 PASS 正常核销 300 落库';

    BEGIN
        INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount) VALUES (rc2, ar, 100);
        RAISE EXCEPTION 'P25 FAIL: 同一 (receipt, receivable) 第二行被放行';
    EXCEPTION WHEN unique_violation THEN RAISE NOTICE 'P25 PASS 拒 uk_biz_rcpt_alloc_pair (重复核销同一笔应收)';
    END;

    BEGIN
        INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount)
        VALUES (rc2, gen_random_uuid(), 10);
        RAISE EXCEPTION 'P26 FAIL: 核销到不存在的应收被放行';
    EXCEPTION WHEN foreign_key_violation THEN RAISE NOTICE 'P26 PASS 拒 外键 (receivable 不存在)';
    END;

    BEGIN
        INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount)
        VALUES (gen_random_uuid(), ar, 10);
        RAISE EXCEPTION 'P27 FAIL: 核销到不存在的收款单被放行';
    EXCEPTION WHEN foreign_key_violation THEN RAISE NOTICE 'P27 PASS 拒 外键 (receipt 不存在)';
    END;

    -- ============ F. 删除动作 ============
    BEGIN
        DELETE FROM biz_receivable WHERE id = ar;
        RAISE EXCEPTION 'P28 FAIL: 被核销的应收被硬删（应为 RESTRICT）';
    EXCEPTION WHEN restrict_violation OR foreign_key_violation
        THEN RAISE NOTICE 'P28 PASS 拒 硬删被核销应收 (RESTRICT / SQLSTATE 23001)';
    END;

    DELETE FROM biz_receipt WHERE id = rc2;
    SELECT count(*) INTO n FROM biz_receipt_allocation WHERE receipt_id = rc2;
    IF n <> 0 THEN RAISE EXCEPTION 'P29 FAIL: 删收款头后核销明细残留 %', n; END IF;
    RAISE NOTICE 'P29 PASS 删收款头 → 核销明细级联删除 (CASCADE)';

    -- ============ G. 取号规则 ============
    SELECT prefix||'|'||date_pattern||'|'||separator||'|'||step||'|'||status INTO d
      FROM biz_seq WHERE seq_type='RC' AND deleted_at IS NULL;
    IF d IS NULL OR d <> 'RC|yyyyMMdd|-|1|1' THEN RAISE EXCEPTION 'P30 FAIL: RC 取号规则 %', d; END IF;
    sample := 'RC-' || to_char(DATE '2026-11-06','YYYYMMDD') || '-' || lpad('1',6,'0');
    IF sample !~ '^RC-[0-9]{8}-[0-9]{6}$' THEN RAISE EXCEPTION 'P31 FAIL: 样例 %', sample; END IF;
    RAISE NOTICE 'P30 PASS 取号规则 RC|yyyyMMdd|-|1|1 → %', sample;

    -- ============ H. 清场 ============
    DELETE FROM biz_receipt WHERE receipt_no LIKE 'RC-20261106-8%';
    DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-20261106-8%';
    SELECT count(*) INTO n FROM biz_receipt WHERE receipt_no LIKE 'RC-20261106-8%';
    IF n <> 0 THEN RAISE EXCEPTION 'P31 FAIL: 收款探针残留 %', n; END IF;
    SELECT count(*) INTO n FROM biz_receipt_allocation;
    IF n <> 0 THEN RAISE EXCEPTION 'P32 FAIL: 核销探针残留 %', n; END IF;
    SELECT count(*) INTO n FROM biz_receivable WHERE ar_no LIKE 'AR-20261106-8%';
    IF n <> 0 THEN RAISE EXCEPTION 'P33 FAIL: 应收探针残留 %', n; END IF;
    RAISE NOTICE 'P31 PASS 探针清场 0 行残留（可重复执行）';
    RAISE NOTICE '===== V0292 约束探针全部通过 =====';
END $$;
