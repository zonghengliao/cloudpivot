-- V0199 反例演练（在 cloudpivot_test 上跑；每一条都必须被 DB 拦下）
\set ON_ERROR_STOP on

DO $$
DECLARE
    wh   uuid;
    sup  uuid;
    po   uuid;
    hit  int := 0;
    total int := 0;
BEGIN
    SELECT id INTO wh  FROM biz_warehouse WHERE deleted_at IS NULL LIMIT 1;
    SELECT id INTO sup FROM biz_supplier  WHERE deleted_at IS NULL LIMIT 1;
    SELECT id INTO po  FROM biz_purchase_order WHERE deleted_at IS NULL LIMIT 1;

    IF wh IS NULL OR sup IS NULL THEN
        RAISE EXCEPTION '库上没有仓库/供应商夹具，先造一条再来';
    END IF;

    -- ① 无源（source_order_id 为 NULL）却不打标记 → ck_biz_si_sourceless_pair
    total := total + 1;
    BEGIN
        INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id,
                                  in_date, status, total_qty, total_amount, total_tax, total_with_tax,
                                  is_sourceless)
        VALUES (gen_random_uuid(), 'V0199-NEG-1', 'PURCHASE', NULL, sup, wh,
                CURRENT_DATE, 'DRAFT', 1, 1, 0, 1, false);
        RAISE EXCEPTION '反例① 没被拦下（无源单 is_sourceless=false 竟然写进去了）';
    EXCEPTION WHEN check_violation THEN
        hit := hit + 1;
        RAISE NOTICE '反例① 被 ck_biz_si_sourceless_pair 拦下 ✅';
    END;

    -- ② 有源（source_order_id 非空）却标成无源 → ck_biz_si_sourceless_pair
    IF po IS NOT NULL THEN
        total := total + 1;
        BEGIN
            INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id,
                                      in_date, status, total_qty, total_amount, total_tax, total_with_tax,
                                      is_sourceless)
            VALUES (gen_random_uuid(), 'V0199-NEG-2', 'PURCHASE', po, sup, wh,
                    CURRENT_DATE, 'DRAFT', 1, 1, 0, 1, true);
            RAISE EXCEPTION '反例② 没被拦下（有源单标成无源竟然写进去了）';
        EXCEPTION WHEN check_violation THEN
            hit := hit + 1;
            RAISE NOTICE '反例② 被 ck_biz_si_sourceless_pair 拦下 ✅';
        END;
    END IF;

    -- ③ 非采购入库却带供应商 → ck_biz_si_supplier_scope（A3 落的，本卡复核）
    total := total + 1;
    BEGIN
        INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id,
                                  in_date, status, total_qty, total_amount, total_tax, total_with_tax,
                                  is_sourceless)
        VALUES (gen_random_uuid(), 'V0199-NEG-3', 'OTHER', NULL, sup, wh,
                CURRENT_DATE, 'DRAFT', 1, 1, 0, 1, true);
        RAISE EXCEPTION '反例③ 没被拦下（非采购入库带供应商竟然写进去了）';
    EXCEPTION WHEN check_violation THEN
        hit := hit + 1;
        RAISE NOTICE '反例③ 被 ck_biz_si_supplier_scope 拦下 ✅';
    END;

    -- ④ 采购入库却没有供应商 → ck_biz_si_supplier（V0145 既有）
    total := total + 1;
    BEGIN
        INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id,
                                  in_date, status, total_qty, total_amount, total_tax, total_with_tax,
                                  is_sourceless)
        VALUES (gen_random_uuid(), 'V0199-NEG-4', 'PURCHASE', NULL, NULL, wh,
                CURRENT_DATE, 'DRAFT', 1, 1, 0, 1, true);
        RAISE EXCEPTION '反例④ 没被拦下（采购入库无供应商竟然写进去了）';
    EXCEPTION WHEN check_violation THEN
        hit := hit + 1;
        RAISE NOTICE '反例④ 被 ck_biz_si_supplier 拦下 ✅';
    END;

    -- 正例：无源 + 标记 true + 供应商 → 必须通过（本次事务内不提交，回滚掉）
    INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id,
                              in_date, status, total_qty, total_amount, total_tax, total_with_tax,
                              is_sourceless)
    VALUES (gen_random_uuid(), 'V0199-POS-1', 'PURCHASE', NULL, sup, wh,
            CURRENT_DATE, 'DRAFT', 1, 1, 0, 1, true);
    RAISE NOTICE '正例：无源 + is_sourceless=true + 供应商 → 写入通过 ✅';

    RAISE NOTICE '反例演练结果：% / % 被拦下', hit, total;
    IF hit <> total THEN
        RAISE EXCEPTION '有反例没被拦下：% / %', hit, total;
    END IF;
END $$;

-- 清理正例（DO 块内插入会随块一起提交，这里显式删掉）
DELETE FROM biz_stock_in WHERE in_no = 'V0199-POS-1';
