-- ======================================================
-- V0295（SO-19 销售发票）约束探针 —— **外部独立校验**，与迁移末尾 DO 块互为第二遍
--
--   用法：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d <库> -v ON_ERROR_STOP=1 -f 本文件
--   输出：每条探针一行 `NOTICE: Pxx PASS …`，末尾一行计数；任一不符 → RAISE EXCEPTION 直接炸（非零退出）
--   清场：P29 把探针行全删，可重复执行（round1 / round2 同库连跑即自证幂等）
--
--   与迁移内自检的差别：这里**不复用**迁移里的判定逻辑，全部从 SQL 外部视角断言
--   （结构查 information_schema / pg_constraint / pg_indexes，行为靠「插坏数据必须被拒」）。
-- ======================================================
DO $$
DECLARE
    n        INT;
    d        TEXT;
    pass     INT := 0;
    cust     UUID := gen_random_uuid();
    inv      UUID := 'd0295eee-0000-4000-8000-000000000001'::uuid;
    ar       UUID := 'd0295eee-0000-4000-8000-000000000002'::uuid;
    inv2     UUID := 'd0295eee-0000-4000-8000-000000000003'::uuid;
BEGIN
    -- ============ A. 结构 ============
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_invoice';
    IF n <> 21 THEN RAISE EXCEPTION 'P01 FAIL: biz_invoice 列数 %（期望 21）', n; END IF;
    pass := pass + 1; RAISE NOTICE 'P01 PASS 结构 biz_invoice = 21 列（16 业务 + 5 审计）';

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_invoice'
       AND column_name IN ('id','invoice_no','invoice_type','invoice_code','customer_id',
                           'invoice_date','amount','tax_amount','total_with_tax','tax_rate',
                           'status','receivable_id','source_type','source_no','voucher_id','remark');
    IF n <> 16 THEN RAISE EXCEPTION 'P02 FAIL: 卡面 16 个业务字段只齐 %', n; END IF;
    pass := pass + 1; RAISE NOTICE 'P02 PASS 结构 卡面 16 个业务字段逐字在位';

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid='public.biz_invoice'::regclass AND contype='c';
    IF n <> 6 THEN RAISE EXCEPTION 'P03 FAIL: CHECK 条数 %（期望 6）', n; END IF;
    pass := pass + 1; RAISE NOTICE 'P03 PASS 结构 6 条 CHECK 在位';

    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname='public' AND tablename='biz_invoice';
    IF n <> 7 THEN RAISE EXCEPTION 'P04 FAIL: 索引条数 %（期望 7）', n; END IF;
    pass := pass + 1; RAISE NOTICE 'P04 PASS 结构 索引 = 7（pkey + 唯一 + 5）';

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_invoice'
       AND column_name IN ('amount','tax_amount','total_with_tax')
       AND numeric_precision=18 AND numeric_scale=4;
    IF n <> 3 THEN RAISE EXCEPTION 'P05 FAIL: 金额三列 NUMERIC(18,4) 只 % /3', n; END IF;
    pass := pass + 1; RAISE NOTICE 'P05 PASS 结构 金额三列 NUMERIC(18,4)';

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_invoice'
       AND column_name='tax_rate' AND numeric_precision=6 AND numeric_scale=4;
    IF n <> 1 THEN RAISE EXCEPTION 'P06 FAIL: tax_rate 不是 NUMERIC(6,4)'; END IF;
    pass := pass + 1; RAISE NOTICE 'P06 PASS 结构 tax_rate = NUMERIC(6,4)';

    SELECT confdeltype INTO d FROM pg_constraint WHERE conname='biz_invoice_receivable_id_fkey';
    IF d IS DISTINCT FROM 'r' THEN RAISE EXCEPTION 'P07 FAIL: receivable_id 删除动作 %（期望 r）', d; END IF;
    pass := pass + 1; RAISE NOTICE 'P07 PASS 外键 receivable_id → biz_receivable = RESTRICT';

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid='public.biz_invoice'::regclass AND contype='f';
    IF n <> 1 THEN RAISE EXCEPTION 'P08 FAIL: 外键数 %（customer_id 必须弱引用，只许 1 个 FK）', n; END IF;
    pass := pass + 1; RAISE NOTICE 'P08 PASS 外键 仅 1 个（customer_id 弱引用无外键）';

    -- ============ B. 金额恒等式（ck_biz_invoice_amount）============
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-P09', cust, DATE '2026-11-06', 1000, 130, 1131);
        RAISE EXCEPTION 'P09 FAIL: total ≠ amount + tax 被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P09 PASS 拒 ck_biz_invoice_amount（INSERT 1131 ≠ 1000+130）';
    END;

    INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
    VALUES ('IV-PROBE-P10', cust, DATE '2026-11-06', 500, 65, 565);
    BEGIN
        UPDATE biz_invoice SET total_with_tax = 600 WHERE invoice_no = 'IV-PROBE-P10';
        RAISE EXCEPTION 'P10 FAIL: UPDATE 破坏恒等式被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P10 PASS 拒 ck_biz_invoice_amount（UPDATE 565→600）';
    END;

    -- ============ C. 票种与金额符号（ck_biz_invoice_sign / ck_biz_invoice_type）============
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-P11', cust, DATE '2026-11-06', 0, 0, 0);
        RAISE EXCEPTION 'P11 FAIL: OUT 且 amount=0 被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P11 PASS 拒 ck_biz_invoice_sign（OUT amount=0）';
    END;

    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-P12', cust, DATE '2026-11-06', -1000, -130, -1130);
        RAISE EXCEPTION 'P12 FAIL: OUT 且 amount<0 被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P12 PASS 拒 ck_biz_invoice_sign（OUT amount=-1000，红冲须走 OUT_RED）';
    END;

    BEGIN
        INSERT INTO biz_invoice (invoice_no, invoice_type, customer_id, invoice_date,
                                 amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-P13', 'OUT_RED', cust, DATE '2026-11-06', 1000, 130, 1130);
        RAISE EXCEPTION 'P13 FAIL: OUT_RED 且 amount>0 被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P13 PASS 拒 ck_biz_invoice_sign（OUT_RED amount=+1000，红字必须取负）';
    END;

    BEGIN
        INSERT INTO biz_invoice (invoice_no, invoice_type, customer_id, invoice_date,
                                 amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-P14', 'IN', cust, DATE '2026-11-06', 1000, 130, 1130);
        RAISE EXCEPTION 'P14 FAIL: invoice_type=IN 被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P14 PASS 拒 ck_biz_invoice_type（IN 是进项票，本表只收销项）';
    END;

    -- ============ D. 状态域（ck_biz_invoice_status）============
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, status)
        VALUES ('IV-PROBE-P15', cust, DATE '2026-11-06', 1000, 130, 1130, 'OPEN');
        RAISE EXCEPTION 'P15 FAIL: status=OPEN（应收域值）被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P15 PASS 拒 ck_biz_invoice_status（OPEN 属应收域）';
    END;

    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, status)
        VALUES ('IV-PROBE-P16', cust, DATE '2026-11-06', 1000, 130, 1130, 'CONFIRMED');
        RAISE EXCEPTION 'P16 FAIL: status=CONFIRMED（收款域值）被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P16 PASS 拒 ck_biz_invoice_status（CONFIRMED 属收款域）';
    END;

    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, status)
        VALUES ('IV-PROBE-P17', cust, DATE '2026-11-06', 1000, 130, 1130, 'CANCELLED');
        RAISE EXCEPTION 'P17 FAIL: status=CANCELLED 被放行（发票作废用 VOID）';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P17 PASS 拒 ck_biz_invoice_status（发票作废 = VOID，不是 CANCELLED）';
    END;

    -- ============ E. 来源 / 税率 ============
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, source_type)
        VALUES ('IV-PROBE-P18', cust, DATE '2026-11-06', 1000, 130, 1130, 'SO');
        RAISE EXCEPTION 'P18 FAIL: source_type=SO（ARCH 稿面字面量）被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P18 PASS 拒 ck_biz_invoice_source（稿面 SO 归一为 SALES_ORDER）';
    END;

    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, tax_rate)
        VALUES ('IV-PROBE-P19', cust, DATE '2026-11-06', 1000, 130, 1130, -0.01);
        RAISE EXCEPTION 'P19 FAIL: tax_rate=-0.01 被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P19 PASS 拒 ck_biz_invoice_tax_rate（-0.01）';
    END;

    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, tax_rate)
        VALUES ('IV-PROBE-P20', cust, DATE '2026-11-06', 1000, 130, 1130, 1.5);
        RAISE EXCEPTION 'P20 FAIL: tax_rate=1.5 被放行';
    EXCEPTION WHEN check_violation THEN pass := pass + 1; RAISE NOTICE 'P20 PASS 拒 ck_biz_invoice_tax_rate（1.5 > 100%%）';
    END;

    -- ============ F. 默认值 / 唯一 / 精度 / 红字正常路径 ============
    INSERT INTO biz_invoice (id, invoice_no, customer_id, invoice_date,
                             amount, tax_amount, total_with_tax)
    VALUES (inv, 'IV-PROBE-P21', cust, DATE '2026-11-06', 1000.0000, 130.0000, 1130.0000);
    SELECT status || '|' || invoice_type || '|' || tax_rate::text INTO d FROM biz_invoice WHERE id = inv;
    IF d <> 'DRAFT|OUT|0.1300' THEN RAISE EXCEPTION 'P21 FAIL: 默认值 %', d; END IF;
    pass := pass + 1; RAISE NOTICE 'P21 PASS 默认值 新单 = DRAFT|OUT|0.1300';

    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-P21', cust, DATE '2026-11-06', 1000, 130, 1130);
        RAISE EXCEPTION 'P22 FAIL: 存活行重号被放行';
    EXCEPTION WHEN unique_violation THEN pass := pass + 1; RAISE NOTICE 'P22 PASS 拒 uk_biz_invoice_no（存活行重号）';
    END;

    UPDATE biz_invoice SET deleted_at = now() WHERE id = inv;
    INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
    VALUES ('IV-PROBE-P21', cust, DATE '2026-11-06', 1000, 130, 1130);
    SELECT count(*) INTO n FROM biz_invoice WHERE invoice_no='IV-PROBE-P21' AND deleted_at IS NULL;
    IF n <> 1 THEN RAISE EXCEPTION 'P23 FAIL: 软删后同号重插存活行 %', n; END IF;
    pass := pass + 1; RAISE NOTICE 'P23 PASS 软删后同号可复用（存活行 = 1）';

    INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
    VALUES ('IV-PROBE-P24', cust, DATE '2026-11-06', 0.0002, 0.0000, 0.0002);
    SELECT amount::text INTO d FROM biz_invoice WHERE invoice_no='IV-PROBE-P24';
    IF d <> '0.0002' THEN RAISE EXCEPTION 'P24 FAIL: 精度被截断 → %', d; END IF;
    pass := pass + 1; RAISE NOTICE 'P24 PASS 精度 0.0002 不被截断（NUMERIC 18,4）';

    INSERT INTO biz_invoice (invoice_no, invoice_type, customer_id, invoice_date,
                             amount, tax_amount, total_with_tax, status)
    VALUES ('IV-PROBE-P25', 'OUT_RED', cust, DATE '2026-11-06', -1000.0000, -130.0000, -1130.0000, 'POSTED');
    SELECT total_with_tax::text INTO d FROM biz_invoice WHERE invoice_no='IV-PROBE-P25';
    IF d <> '-1130.0000' THEN RAISE EXCEPTION 'P25 FAIL: 红字票合计 %', d; END IF;
    pass := pass + 1; RAISE NOTICE 'P25 PASS 红字票正常路径（OUT_RED -1000/-130 → -1130.0000，POSTED 合法）';

    -- ============ G. 外键与删除动作 ============
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, receivable_id)
        VALUES ('IV-PROBE-P26', cust, DATE '2026-11-06', 1000, 130, 1130, gen_random_uuid());
        RAISE EXCEPTION 'P26 FAIL: receivable_id 悬空被放行';
    EXCEPTION WHEN foreign_key_violation THEN pass := pass + 1; RAISE NOTICE 'P26 PASS 拒 悬空 receivable_id（23503）';
    END;

    INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, credit_days, period_code, due_date)
    VALUES (ar, 'AR-PROBE-V0295-9001', cust, 'MANUAL', gen_random_uuid(), 'MANUAL',
            1130.0000, 1130.0000, 0, '2026-11', DATE '2026-11-06');
    INSERT INTO biz_invoice (id, invoice_no, customer_id, invoice_date, amount, tax_amount,
                             total_with_tax, receivable_id)
    VALUES (inv2, 'IV-PROBE-P27', cust, DATE '2026-11-06', 1000, 130, 1130, ar);
    BEGIN
        DELETE FROM biz_receivable WHERE id = ar;
        RAISE EXCEPTION 'P27 FAIL: 硬删被发票引用的应收被放行';
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN
        pass := pass + 1; RAISE NOTICE 'P27 PASS 拒 硬删被引用的应收（RESTRICT / 23001）';
    END;

    -- ============ H. 取号规则 ============
    SELECT prefix || '|' || date_pattern || '|' || separator || '|' || step || '|' || status
      INTO d FROM biz_seq WHERE seq_type='IV' AND deleted_at IS NULL;
    IF d IS DISTINCT FROM 'IV|yyyyMMdd|-|1|1' THEN
        RAISE EXCEPTION 'P28 FAIL: IV 取号规则 = %', coalesce(d,'<缺失>');
    END IF;
    IF ('IV-' || to_char(DATE '2026-11-06','YYYYMMDD') || '-' || lpad('7',6,'0')) !~ '^IV-[0-9]{8}-[0-9]{6}$' THEN
        RAISE EXCEPTION 'P28 FAIL: 单号形态不符 IV-YYYYMMDD-NNNNNN';
    END IF;
    pass := pass + 1; RAISE NOTICE 'P28 PASS 取号规则 IV|yyyyMMdd|-|1|1 → IV-20261106-000007';

    -- ============ I. 清场 ============
    DELETE FROM biz_invoice WHERE invoice_no LIKE 'IV-PROBE-%';
    DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-PROBE-V0295-%';
    SELECT count(*) INTO n FROM biz_invoice WHERE invoice_no LIKE 'IV-PROBE-%';
    IF n <> 0 THEN RAISE EXCEPTION 'P29 FAIL: 发票探针残留 % 行', n; END IF;
    SELECT count(*) INTO n FROM biz_receivable WHERE ar_no LIKE 'AR-PROBE-V0295-%';
    IF n <> 0 THEN RAISE EXCEPTION 'P29 FAIL: 应收探针残留 % 行', n; END IF;
    pass := pass + 1; RAISE NOTICE 'P29 PASS 清场 探针行残留 = 0';

    RAISE NOTICE '===== 探针汇总：% / 29 PASS =====', pass;
END $$;
