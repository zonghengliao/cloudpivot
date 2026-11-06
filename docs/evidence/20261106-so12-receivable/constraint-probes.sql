-- ======================================================
-- SO-12 约束探针（独立于迁移自检的第二遍校验）
-- 目标库：已应用 V0287 的库（cloudpivot_so12 / cloudpivot_so12_scratch）
-- 用法：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d <db> -v ON_ERROR_STOP=1 -f constraint-probes.sql
-- 口径：库层只兜「单行可判定的不变量」；跨表/状态迁移由 SO-13 的 model 守。
-- ======================================================
\set ON_ERROR_STOP on
\timing off

DROP TABLE IF EXISTS probe_results;
CREATE TEMP TABLE probe_results (n int PRIMARY KEY, name text, expected text, actual text, ok boolean);

-- ---------- 1. 金额三连（ck_biz_ar_amount，卡面逐字）----------
DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P01', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 0, 0, '2026-11', DATE '2026-12-06');
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (1, 'amount = 0', '拒 ck_biz_ar_amount',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_amount');
END $$;

DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P02', gen_random_uuid(), gen_random_uuid(), 'MANUAL', -5, -5, '2026-11', DATE '2026-12-06');
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (2, 'amount = -5', '拒 ck_biz_ar_amount',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_amount');
END $$;

DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, received_amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P03', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 100, 100.0001, -0.0001, '2026-11', DATE '2026-12-06');
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (3, 'received_amount > amount（超收 0.0001）', '拒 ck_biz_ar_amount',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_amount');
END $$;

DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, received_amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P04', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 100, -0.0001, 100.0001, '2026-11', DATE '2026-12-06');
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (4, 'received_amount = -0.0001', '拒 ck_biz_ar_amount',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_amount');
END $$;

-- ---------- 2. 未收额恒等（ck_biz_ar_unsettled）----------
DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, received_amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P05', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 100, 40, 59.9999, '2026-11', DATE '2026-12-06');
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (5, 'unsettled_amount ≠ amount - received_amount', '拒 ck_biz_ar_unsettled',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_unsettled');
END $$;

-- ---------- 3. 状态域（ck_biz_ar_status，4 值；payable 的值不许混入）----------
DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, unsettled_amount, period_code, due_date, status)
    VALUES ('AR-PROBE-P06', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 100, 100, '2026-11', DATE '2026-12-06', 'PARTIAL_PAID');
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (6, 'status = PARTIAL_PAID（payable 的域值）', '拒 ck_biz_ar_status',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_status');
END $$;

DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, unsettled_amount, period_code, due_date, status)
    VALUES ('AR-PROBE-P07', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 100, 100, '2026-11', DATE '2026-12-06', 'CANCELLED');
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (7, 'status = CANCELLED（payable 的域值）', '拒 ck_biz_ar_status',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_status');
END $$;

-- ---------- 4. 结清口径（ck_biz_ar_settled）----------
DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, received_amount, unsettled_amount, period_code, due_date, status)
    VALUES ('AR-PROBE-P08', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 100, 40, 60, '2026-11', DATE '2026-12-06', 'SETTLED');
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (8, 'SETTLED 但未收 60', '拒 ck_biz_ar_settled',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_settled');
END $$;

-- ---------- 5. 账期非负（ck_biz_ar_credit）----------
DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, unsettled_amount, credit_days, period_code, due_date)
    VALUES ('AR-PROBE-P09', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 100, 100, -1, '2026-11', DATE '2026-12-06');
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (9, 'credit_days = -1', '拒 ck_biz_ar_credit',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_credit');
END $$;

-- ---------- 6. 正常路径：挂账 → 部分收款 → 结清 → 坏账 ----------
DO $$
DECLARE v_ok boolean := false; d text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, credit_days, period_code, due_date)
    VALUES ('AR-PROBE-P10', gen_random_uuid(), 'SALES_OUT', 'd0287aaa-0000-4000-8000-000000000010'::uuid,
            'OUT-20261106-000010', 1000.0000, 1000.0000, 30, '2026-11', DATE '2026-12-06');
    SELECT status || '/' || received_amount::text || '/' || unsettled_amount::text || '/' || currency_code
      INTO d FROM biz_receivable WHERE ar_no = 'AR-PROBE-P10';
    v_ok := (d = 'OPEN/0.0000/1000.0000/CNY');
  EXCEPTION WHEN OTHERS THEN
    d := SQLSTATE || ':' || SQLERRM;
  END;
  INSERT INTO probe_results VALUES (10, '正常挂账（默认 status/币种/已收 0）', 'OPEN/0.0000/1000.0000/CNY', d, v_ok);
END $$;

DO $$
DECLARE v_ok boolean := false; d text := '-';
BEGIN
  BEGIN
    UPDATE biz_receivable
       SET received_amount = 400.0000, unsettled_amount = 600.0000, status = 'PARTIAL', updated_at = now()
     WHERE ar_no = 'AR-PROBE-P10';
    SELECT status || '/' || received_amount::text || '/' || unsettled_amount::text
      INTO d FROM biz_receivable WHERE ar_no = 'AR-PROBE-P10';
    v_ok := (d = 'PARTIAL/400.0000/600.0000');
  EXCEPTION WHEN OTHERS THEN
    d := SQLSTATE || ':' || SQLERRM;
  END;
  INSERT INTO probe_results VALUES (11, '部分收款 400 → PARTIAL', 'PARTIAL/400.0000/600.0000', d, v_ok);
END $$;

DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    UPDATE biz_receivable SET received_amount = 1000.0001, unsettled_amount = -0.0001
     WHERE ar_no = 'AR-PROBE-P10';
  EXCEPTION WHEN check_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (12, 'UPDATE 超收（1000.0001 > 1000）', '拒 ck_biz_ar_amount',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'UPDATE 成功（漏洞）' END, v_ok AND v_diag = 'ck_biz_ar_amount');
END $$;

DO $$
DECLARE v_ok boolean := false; d text := '-';
BEGIN
  BEGIN
    UPDATE biz_receivable
       SET received_amount = 1000.0000, unsettled_amount = 0, status = 'SETTLED', settled_at = now()
     WHERE ar_no = 'AR-PROBE-P10';
    SELECT status || '/' || unsettled_amount::text || '/' || (settled_at IS NOT NULL)::text
      INTO d FROM biz_receivable WHERE ar_no = 'AR-PROBE-P10';
    v_ok := (d = 'SETTLED/0.0000/true');
  EXCEPTION WHEN OTHERS THEN
    d := SQLSTATE || ':' || SQLERRM;
  END;
  INSERT INTO probe_results VALUES (13, '全额收款 → SETTLED + settled_at', 'SETTLED/0.0000/true', d, v_ok);
END $$;

DO $$
DECLARE v_ok boolean := false; d text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, received_amount, unsettled_amount, period_code, due_date,
                                status, settled_at, remark)
    VALUES ('AR-PROBE-P14', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
            500.0000, 0, 500.0000, '2026-11', DATE '2026-12-06',
            'WRITTEN_OFF', now(), '客户破产，全额计提坏账');
    SELECT status || '/' || (settled_at IS NOT NULL)::text INTO d
      FROM biz_receivable WHERE ar_no = 'AR-PROBE-P14';
    v_ok := (d = 'WRITTEN_OFF/true');
  EXCEPTION WHEN OTHERS THEN
    d := SQLSTATE || ':' || SQLERRM;
  END;
  INSERT INTO probe_results VALUES (14, '坏账核销 WRITTEN_OFF（保留未收额）', 'WRITTEN_OFF/true', d, v_ok);
END $$;

-- ---------- 7. 唯一 / 幂等 / 软删复用 ----------
DO $$
DECLARE v_ok boolean := false; v_diag text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P10', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 1, 1, '2026-11', DATE '2026-12-06');
  EXCEPTION WHEN unique_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (15, '存活行 ar_no 重号', '拒 uk_biz_ar_no',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'uk_biz_ar_no');
END $$;

DO $$
DECLARE v_ok boolean := false; d text := '-';
BEGIN
  BEGIN
    UPDATE biz_receivable SET deleted_at = now() WHERE ar_no = 'AR-PROBE-P10';
    INSERT INTO biz_receivable (ar_no, customer_id, source_id, source_no, amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P10', gen_random_uuid(), gen_random_uuid(), 'MANUAL', 1, 1, '2026-11', DATE '2026-12-06');
    SELECT count(*)::text INTO d FROM biz_receivable WHERE ar_no = 'AR-PROBE-P10' AND deleted_at IS NULL;
    v_ok := (d = '1');
  EXCEPTION WHEN OTHERS THEN
    d := SQLSTATE || ':' || SQLERRM;
  END;
  INSERT INTO probe_results VALUES (16, '软删后同号复插', '存活行 = 1', d, v_ok);
END $$;

DO $$
DECLARE v_ok boolean := false; v_diag text := '-'; probe_src uuid := 'd0287aaa-0000-4000-8000-000000000017'::uuid;
BEGIN
  BEGIN
    -- 先自己造一张活的 SALES_OUT 应收，再试同源第二张（不依赖前面探针留下的行状态：
    -- P10 那行在 P16 被软删了，拿它当「活来源」会假绿）
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P17a', gen_random_uuid(), 'SALES_OUT', probe_src,
            'OUT-20261106-000017', 100, 100, '2026-11', DATE '2026-12-06');
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P17b', gen_random_uuid(), 'SALES_OUT', probe_src,
            'OUT-20261106-000017', 999, 999, '2026-11', DATE '2026-12-06');
  EXCEPTION WHEN unique_violation THEN GET STACKED DIAGNOSTICS v_diag = CONSTRAINT_NAME; v_ok := true;
  END;
  INSERT INTO probe_results VALUES (17, '同一 SALES_OUT 来源重复挂账', '拒 uk_biz_ar_source',
    CASE WHEN v_ok THEN '拒 ' || v_diag ELSE 'INSERT 成功（漏洞）' END, v_ok AND v_diag = 'uk_biz_ar_source');
END $$;

DO $$
DECLARE v_ok boolean := false; d text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P18a', gen_random_uuid(), 'MANUAL', 'd0287aaa-0000-4000-8000-000000000018'::uuid,
            'MANUAL', 10, 10, '2026-11', DATE '2026-12-06');
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P18b', gen_random_uuid(), 'MANUAL', 'd0287aaa-0000-4000-8000-000000000018'::uuid,
            'MANUAL', 20, 20, '2026-11', DATE '2026-12-06');
    SELECT count(*)::text INTO d FROM biz_receivable
     WHERE source_type = 'MANUAL' AND source_id = 'd0287aaa-0000-4000-8000-000000000018'::uuid
       AND deleted_at IS NULL;
    v_ok := (d = '2');
  EXCEPTION WHEN OTHERS THEN
    d := SQLSTATE || ':' || SQLERRM;
  END;
  INSERT INTO probe_results VALUES (18, 'MANUAL 同 source_id 两笔（例外生效）', '2 行', d, v_ok);
END $$;

-- ---------- 8. 精度：NUMERIC(18,4) 不许静默截断 ----------
DO $$
DECLARE v_ok boolean := false; d text := '-';
BEGIN
  BEGIN
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, received_amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-PROBE-P19', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
            0.0002, 0.0001, 0.0001, '2026-11', DATE '2026-12-06');
    SELECT amount::text || '/' || received_amount::text || '/' || unsettled_amount::text
      INTO d FROM biz_receivable WHERE ar_no = 'AR-PROBE-P19';
    v_ok := (d = '0.0002/0.0001/0.0001');
  EXCEPTION WHEN OTHERS THEN
    d := SQLSTATE || ':' || SQLERRM;
  END;
  INSERT INTO probe_results VALUES (19, '四位小数精度（0.0002/0.0001）', '0.0002/0.0001/0.0001', d, v_ok);
END $$;

-- ---------- 9. 结构与取号规则（与迁移自检相互独立的重算）----------
DO $$
DECLARE d text := '-'; v_ok boolean := false; n int;
BEGIN
  SELECT count(*)::text INTO d FROM information_schema.columns
   WHERE table_schema = 'public' AND table_name = 'biz_receivable';
  SELECT count(*) INTO n FROM pg_constraint
   WHERE conrelid = 'public.biz_receivable'::regclass AND contype = 'c';
  SELECT d || ' 列 / ' || n || ' 条 CHECK' INTO d FROM (SELECT 1) t;
  v_ok := (split_part(d, ' 列', 1) = '24' AND n = 5);
  INSERT INTO probe_results VALUES (20, '列数 24 + 5 条 CHECK', '24 列 / 5 条 CHECK', d, v_ok);
END $$;

DO $$
DECLARE d text := '-'; v_ok boolean := false; n int;
BEGIN
  SELECT count(*) INTO n FROM pg_indexes WHERE schemaname = 'public' AND tablename = 'biz_receivable';
  SELECT prefix || '|' || date_pattern || '|' || separator || '|' || step || '|' || status
    INTO d FROM biz_seq WHERE seq_type = 'AR' AND deleted_at IS NULL;
  v_ok := (n = 8 AND d = 'AR|yyyyMMdd|-|1|1');
  INSERT INTO probe_results VALUES (21, '索引 8 个 + AR 取号规则',
    '8 索引 / AR|yyyyMMdd|-|1|1', n || ' 索引 / ' || coalesce(d, '缺失'), v_ok);
END $$;

DO $$
DECLARE d text := '-'; v_ok boolean := false;
BEGIN
  -- 与 payable 逐列同型（独立于迁移自检的第二遍）
  WITH map(ar_col, pay_col) AS (
      VALUES ('id','id'), ('ar_no','payable_no'), ('customer_id','supplier_id'),
             ('source_type','source_type'), ('source_id','source_id'), ('source_no','source_no'),
             ('invoice_no','invoice_no'), ('invoice_date','invoice_date'),
             ('currency_code','currency_code'), ('amount','amount'),
             ('received_amount','paid_amount'), ('unsettled_amount','unpaid_amount'),
             ('credit_days','credit_days'), ('due_date','due_date'),
             ('period_code','period_code'), ('status','status'),
             ('settled_at','settled_at'), ('voucher_id','voucher_id'),
             ('remark','remark'), ('created_at','created_at'), ('updated_at','updated_at'),
             ('create_by','create_by'), ('update_by','update_by'), ('deleted_at','deleted_at')
  )
  SELECT count(*) FILTER (WHERE a.data_type = p.data_type
                             AND coalesce(a.character_maximum_length,0) = coalesce(p.character_maximum_length,0)
                             AND coalesce(a.numeric_precision,0) = coalesce(p.numeric_precision,0)
                             AND coalesce(a.numeric_scale,0) = coalesce(p.numeric_scale,0)
                             AND a.is_nullable = p.is_nullable)::text
         || ' 列 / 24 列'
    INTO d
    FROM map m
    JOIN information_schema.columns a
      ON a.table_schema='public' AND a.table_name='biz_receivable' AND a.column_name=m.ar_col
    JOIN information_schema.columns p
      ON p.table_schema='public' AND p.table_name='biz_payable' AND p.column_name=m.pay_col;
  v_ok := (d = '24 列 / 24 列');
  INSERT INTO probe_results VALUES (22, '与 biz_payable 逐列同型（类型/长度/精度/可空）',
    '24 列 / 24 列', coalesce(d, '无法比较'), v_ok);
END $$;

-- ---------- 10. 收尾：探针零残留 ----------
DO $$
DECLARE n int; v_ok boolean := false;
BEGIN
  DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-PROBE-%';
  SELECT count(*) INTO n FROM biz_receivable WHERE ar_no LIKE 'AR-PROBE-%';
  v_ok := (n = 0);
  INSERT INTO probe_results VALUES (23, '探针清场', '0 行', n || ' 行', v_ok);
END $$;

\echo ''
\echo '===== 探针结果（expected vs actual）====='
SELECT n, CASE WHEN ok THEN 'PASS' ELSE 'FAIL' END AS rc, name, expected, actual FROM probe_results ORDER BY n;
\echo ''

DO $$
DECLARE n_fail int; n_total int;
BEGIN
  SELECT count(*), count(*) FILTER (WHERE NOT ok) INTO n_total, n_fail FROM probe_results;
  IF n_fail <> 0 THEN
    RAISE EXCEPTION 'SO-12 探针失败：% / % 条不通过', n_fail, n_total;
  END IF;
  RAISE NOTICE 'SO-12 探针全部通过：% / % 条', n_total - n_fail, n_total;
END $$;
