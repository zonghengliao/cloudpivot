-- ============================================================================
-- SO-28（t_6b51a48b）外部独立约束探针 —— biz_sales_return / biz_sales_return_item
--
-- 与迁移内自检**不共用代码**：这里从「卡面验收点」反着问一遍库能不能拒绝坏数据。
-- 用法（库需已应用 V0303）：
--   PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so28 -a \
--     -f docs/evidence/20261107-so28-sales-return/constraint-probes.sql
-- 行为：全部探针结果落到 temp 表，末尾汇总打印 `x / N PASS`；有任何 not ok → RAISE EXCEPTION
--       （psql 退出码非 0 = 真信号）。探针自己造前提、统一清场（return_no LIKE '__SO28PROBE%'）。
-- ============================================================================

CREATE TEMP TABLE IF NOT EXISTS probe_results (
    seq    INT,
    code   TEXT,
    ok     BOOLEAN,
    detail TEXT
);
DELETE FROM probe_results;

DO $$
DECLARE
    h1  UUID := 'd0303aaa-0000-4000-8000-000000000001'::uuid;   -- 探针主单头
    h2  UUID := 'd0303aaa-0000-4000-8000-000000000002'::uuid;   -- 唯一性探针单头
    n   INT;
    d   TEXT;
BEGIN
    ------------------------------------------------------------------ S：结构
    INSERT INTO probe_results VALUES (1, 'S1_tables_exist',
        (to_regclass('public.biz_sales_return') IS NOT NULL
         AND to_regclass('public.biz_sales_return_item') IS NOT NULL),
        'biz_sales_return / biz_sales_return_item 都存在');

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_sales_return';
    INSERT INTO probe_results VALUES (2, 'S2_head_columns',
        n = 22, format('主表列数 = %s（期望 22）', n));

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_sales_return_item';
    INSERT INTO probe_results VALUES (3, 'S3_item_columns',
        n = 14, format('明细列数 = %s（期望 14）', n));

    -- 金额/税率列与订单行逐列同型（拷贝不丢精度）
    SELECT string_agg(format('%s:%s(%s,%s) vs 订单行 %s(%s,%s)',
                             a.column_name, a.data_type, a.numeric_precision, a.numeric_scale,
                             b.data_type, b.numeric_precision, b.numeric_scale), '; ')
      INTO d
      FROM information_schema.columns a
      JOIN information_schema.columns b
        ON b.table_schema='public' AND b.table_name='biz_sales_order_item'
       AND (a.column_name, b.column_name) IN (('price','price'),('amount','amount'),
                                              ('tax_rate','tax_rate'),('tax_amount','tax_amount'))
     WHERE a.table_schema='public' AND a.table_name='biz_sales_return_item'
       AND (a.data_type<>b.data_type
            OR coalesce(a.numeric_precision,0)<>coalesce(b.numeric_precision,0)
            OR coalesce(a.numeric_scale,0)<>coalesce(b.numeric_scale,0));
    INSERT INTO probe_results VALUES (4, 'S4_amount_types_match_order_item',
        d IS NULL, coalesce('不同型：'||d, 'price 18,6 / amount 18,4 / tax_rate 7,4 / tax_amount 18,4 与订单行一致'));

    -- 外键口径：单头 0 条；明细恰好 1 条且 CASCADE
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid='public.biz_sales_return'::regclass AND contype='f';
    INSERT INTO probe_results VALUES (5, 'S5_head_zero_fk',
        n = 0, format('单头外键条数 = %s（ADR-06：单据链/主数据一律弱引用）', n));

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid='public.biz_sales_return_item'::regclass AND contype='f';
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname='fk_biz_sreturn_item_return' AND contype='f';
    INSERT INTO probe_results VALUES (6, 'S6_item_single_cascade_fk',
        n = 1 AND d LIKE '%REFERENCES biz_sales_return(id)%' AND d LIKE '%ON DELETE CASCADE%',
        format('明细外键 %s 条；定义=%s', n, coalesce(d,'<缺失>')));

    -- 全仓迁移零 TRIGGER（业务判定在 Rust 侧）
    SELECT count(*) INTO n FROM pg_trigger t
      JOIN pg_class c ON c.oid = t.tgrelid
     WHERE NOT t.tgisinternal
       AND c.relname IN ('biz_sales_return','biz_sales_return_item');
    INSERT INTO probe_results VALUES (7, 'S7_no_trigger', n = 0,
        format('两张表上的用户触发器 = %s', n));

    -- 取号规则行
    SELECT prefix||'|'||date_pattern||'|'||separator||'|'||step||'|'||status INTO d
      FROM biz_seq WHERE seq_type='SR' AND deleted_at IS NULL;
    INSERT INTO probe_results VALUES (8, 'S8_seq_rule_sr',
        d = 'SR|yyyyMMdd|-|1|1', format('biz_seq(SR) = %s（期望 SR|yyyyMMdd|-|1|1）', coalesce(d,'<缺失>')));

    -- 卡面点名的约束名在位
    SELECT count(*) INTO n FROM pg_constraint WHERE conname='ck_biz_sreturn_item_qty';
    INSERT INTO probe_results VALUES (9, 'S9_ck_name_from_card',
        n = 1, format('ck_biz_sreturn_item_qty 存在 = %s', n = 1));

    ------------------------------------------------------------------ B：行为
    -- B1 默认值：只给必填列 → status=DRAFT、三金额=0
    INSERT INTO biz_sales_return (id, return_no, customer_id, return_date, return_type, warehouse_id)
    VALUES (h1, '__SO28PROBE_H1__', gen_random_uuid(), DATE '2026-11-07', 'QUALITY', gen_random_uuid());
    SELECT status||'|'||total_amount::text||'|'||total_tax::text||'|'||total_with_tax::text INTO d
      FROM biz_sales_return WHERE id = h1;
    INSERT INTO probe_results VALUES (10, 'B1_defaults',
        d = 'DRAFT|0.0000|0.0000|0.0000', format('默认落库 = %s', d));

    -- B2 状态域：6 个合法值全可落；非法值被拒
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id, status)
        VALUES ('__SO28PROBE_BADSTATUS__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid(), 'RETURNED');
        INSERT INTO probe_results VALUES (11, 'B2_status_reject_illegal', FALSE, 'status=RETURNED 竟然插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO probe_results VALUES (11, 'B2_status_reject_illegal', TRUE, 'status=RETURNED → 23514 拒绝');
    END;

    -- B3 退货类型域：非法值（隔壁发票域的 OUT_RED）被拒
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id)
        VALUES ('__SO28PROBE_BADTYPE__', gen_random_uuid(), DATE '2026-11-07', 'OUT_RED', gen_random_uuid());
        INSERT INTO probe_results VALUES (12, 'B3_type_reject_illegal', FALSE, 'return_type=OUT_RED 竟然插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO probe_results VALUES (12, 'B3_type_reject_illegal', TRUE, 'return_type=OUT_RED → 23514 拒绝');
    END;

    -- B4 价税恒等：不等式被拒
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id,
                                      total_amount, total_tax, total_with_tax)
        VALUES ('__SO28PROBE_BADSUM__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid(),
                50, 40, 100);
        INSERT INTO probe_results VALUES (13, 'B4_amount_identity_reject', FALSE, 'amount+tax≠total 竟然插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO probe_results VALUES (13, 'B4_amount_identity_reject', TRUE, 'amount+tax≠total → 23514 拒绝');
    END;

    -- B5 负金额被拒
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id,
                                      total_amount, total_tax, total_with_tax)
        VALUES ('__SO28PROBE_NEG__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid(),
                -1, -0.13, -1.13);
        INSERT INTO probe_results VALUES (14, 'B5_amount_negative_reject', FALSE, '负金额竟然插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO probe_results VALUES (14, 'B5_amount_negative_reject', TRUE, '负金额 → 23514 拒绝');
    END;

    -- B6 税率越界（150）被拒
    BEGIN
        INSERT INTO biz_sales_return_item (return_id, material_id, qty, tax_rate)
        VALUES (h1, gen_random_uuid(), 1, 150);
        INSERT INTO probe_results VALUES (15, 'B6_tax_rate_reject', FALSE, 'tax_rate=150 竟然插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO probe_results VALUES (15, 'B6_tax_rate_reject', TRUE, 'tax_rate=150 → 23514 拒绝');
    END;

    -- B7 qty 边界：0 / 负数被拒，正数通过
    BEGIN
        INSERT INTO biz_sales_return_item (return_id, material_id, qty) VALUES (h1, gen_random_uuid(), 0);
        INSERT INTO probe_results VALUES (16, 'B7_qty_zero_reject', FALSE, 'qty=0 竟然插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO probe_results VALUES (16, 'B7_qty_zero_reject', TRUE, 'qty=0 → 23514 拒绝');
    END;
    BEGIN
        INSERT INTO biz_sales_return_item (return_id, material_id, qty) VALUES (h1, gen_random_uuid(), -3);
        INSERT INTO probe_results VALUES (17, 'B8_qty_negative_reject', FALSE, 'qty=-3 竟然插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO probe_results VALUES (17, 'B8_qty_negative_reject', TRUE, 'qty=-3 → 23514 拒绝');
    END;
    BEGIN
        INSERT INTO biz_sales_return_item (return_id, material_id, qty, price, amount, tax_rate, tax_amount, serial_nos)
        VALUES (h1, gen_random_uuid(), 3, 10.5, 31.5, 13, 4.095, ARRAY['SN-A','SN-B','SN-C']);
        INSERT INTO probe_results VALUES (18, 'B9_qty_positive_accept', TRUE, 'qty=3 + serial_nos 三元素插入成功');
    EXCEPTION WHEN others THEN
        INSERT INTO probe_results VALUES (18, 'B9_qty_positive_accept', FALSE, '合法明细插入失败：'||SQLERRM);
    END;

    -- B10 明细挂空单头被外键拦
    BEGIN
        INSERT INTO biz_sales_return_item (return_id, material_id, qty)
        VALUES ('00000000-0000-4000-8000-0000aaaaaaaa'::uuid, gen_random_uuid(), 1);
        INSERT INTO probe_results VALUES (19, 'B10_fk_reject_orphan', FALSE, '孤儿明细竟然插入成功');
    EXCEPTION WHEN foreign_key_violation THEN
        INSERT INTO probe_results VALUES (19, 'B10_fk_reject_orphan', TRUE, '孤儿明细 → 23503 拒绝');
    END;

    -- B11 单号唯一：存活行重号被拒
    INSERT INTO biz_sales_return (id, return_no, customer_id, return_date, return_type, warehouse_id)
    VALUES (h2, '__SO28PROBE_UK__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid());
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id)
        VALUES ('__SO28PROBE_UK__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid());
        INSERT INTO probe_results VALUES (20, 'B11_no_unique_reject', FALSE, '存活行重号竟然插入成功');
    EXCEPTION WHEN unique_violation THEN
        INSERT INTO probe_results VALUES (20, 'B11_no_unique_reject', TRUE, '存活行重号 → 23505 拒绝');
    END;

    -- B12 软删后同号可复用（部分唯一索引语义）
    UPDATE biz_sales_return SET deleted_at = now() WHERE id = h2;
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id)
        VALUES ('__SO28PROBE_UK__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid());
        INSERT INTO probe_results VALUES (21, 'B12_softdelete_no_reuse', TRUE, '软删后同号重插成功');
    EXCEPTION WHEN unique_violation THEN
        INSERT INTO probe_results VALUES (21, 'B12_softdelete_no_reuse', FALSE, '软删后同号仍被拒（部分索引写错？）');
    END;

    -- B13 级联：物理删单头 → 明细一起消失
    SELECT count(*) INTO n FROM biz_sales_return_item WHERE return_id = h1;
    DELETE FROM biz_sales_return WHERE id = h1;
    SELECT count(*) INTO d FROM biz_sales_return_item WHERE return_id = h1;
    INSERT INTO probe_results VALUES (22, 'B13_cascade_delete',
        (n > 0 AND d = '0'), format('删前明细 %s 行 → 删后 %s 行（期望 0）', n, d));

    -- B14 清场：探针行全部消失
    DELETE FROM biz_sales_return WHERE return_no LIKE '__SO28PROBE%';
    SELECT (SELECT count(*) FROM biz_sales_return WHERE return_no LIKE '__SO28PROBE%')
         + (SELECT count(*) FROM biz_sales_return_item i
              JOIN biz_sales_return h ON h.id = i.return_id
             WHERE h.return_no LIKE '__SO28PROBE%')
      INTO n;
    INSERT INTO probe_results VALUES (23, 'B14_residue_zero', n = 0,
        format('探针残留 = %s 行', n));
END $$;

\echo ''
\echo '================ SO-28 约束探针结果 ================'
SELECT code, CASE WHEN ok THEN 'PASS' ELSE 'FAIL' END AS verdict, detail
  FROM probe_results ORDER BY seq;
\echo '===================================================='

DO $$
DECLARE total INT; passed INT;
BEGIN
    SELECT count(*), count(*) FILTER (WHERE ok) INTO total, passed FROM probe_results;
    RAISE NOTICE 'SO-28 探针汇总: % / % PASS', passed, total;
    IF passed <> total THEN
        RAISE EXCEPTION 'SO-28 探针失败: % / % PASS（见上面 FAIL 行）', passed, total;
    END IF;
END $$;
